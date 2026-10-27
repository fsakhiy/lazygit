use eframe::egui;
use lazycommit::{
    collect_all_activity, detect_git_author, find_git_repos, format_activity_prompt,
    generate_commit_message, generate_daily_report,
};
use std::{
    env,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

#[derive(Clone)]
struct ProjectState {
    files: Vec<FileChange>,
    file_diffs: Vec<(String, String)>,
    diff: String,
    staged_diff: String,
    has_untracked: bool,
}

#[derive(Clone)]
struct FileChange {
    path: String,
    status: String,
    untracked: bool,
}

struct WorkspaceTab {
    id: u64,
    project: PathBuf,
    state: Option<ProjectState>,
    selected_file: Option<String>,
    message: String,
    status: String,
    busy: bool,
}

enum WorkerResult {
    Loaded(u64, Result<ProjectState, String>),
    Generated(u64, Result<String, String>),
    Committed(u64, Result<String, String>),
    ReportLoaded(Result<String, String>),
    ReportGenerated(Result<String, String>),
}

struct LazyCommitApp {
    tabs: Vec<WorkspaceTab>,
    active_tab: Option<u64>,
    next_tab_id: u64,
    tx: Sender<WorkerResult>,
    rx: Receiver<WorkerResult>,

    // Daily report state
    show_report: bool,
    report_use_tabs: bool,
    report_folder: Option<PathBuf>,
    report_since: String,
    report_author: String,
    report_all_authors: bool,
    report_output: String,
    report_status: String,
    report_busy: bool,
    report_copied: bool,
}

impl Default for LazyCommitApp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tabs: Vec::new(),
            active_tab: None,
            next_tab_id: 1,
            tx,
            rx,

            show_report: false,
            report_use_tabs: true,
            report_folder: None,
            report_since: "midnight".into(),
            report_author: detect_git_author(None).unwrap_or_default(),
            report_all_authors: false,
            report_output: String::new(),
            report_status: "Ready".into(),
            report_busy: false,
            report_copied: false,
        }
    }
}

impl LazyCommitApp {
    fn with_project(project: Option<PathBuf>) -> Self {
        let mut app = Self::default();
        if let Some(project) = project {
            app.open_project(project);
        }
        app
    }

    fn open_project(&mut self, project: PathBuf) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(WorkspaceTab {
            id,
            project,
            state: None,
            selected_file: None,
            message: String::new(),
            status: "Loading changes…".into(),
            busy: false,
        });
        self.active_tab = Some(id);
        self.refresh_tab(id);
    }

    fn close_tab(&mut self, id: u64) {
        self.tabs.retain(|tab| tab.id != id);
        if self.active_tab == Some(id) {
            self.active_tab = self.tabs.last().map(|tab| tab.id);
        }
    }
}

fn git(root: &PathBuf, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("Could not run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn load_project(root: &PathBuf) -> Result<ProjectState, String> {
    let inside = git(root, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return Err("Choose a directory inside a Git repository.".into());
    }
    let status = git(root, &["status", "--porcelain", "--untracked-files=all"])?;
    let files: Vec<FileChange> = status
        .lines()
        .filter_map(|line| {
            let status = line.get(..2)?.to_owned();
            let path = line.get(3..)?.to_owned();
            Some(FileChange {
                untracked: status == "??",
                status,
                path,
            })
        })
        .collect();
    let staged_diff = git(root, &["diff", "--cached", "--"])?;
    let unstaged_diff = git(root, &["diff", "--"])?;
    let file_diffs = files
        .iter()
        .map(|file| {
            let args = if staged_diff.trim().is_empty() {
                vec!["diff", "--", file.path.as_str()]
            } else {
                vec!["diff", "--cached", "--", file.path.as_str()]
            };
            (file.path.clone(), git(root, &args).unwrap_or_default())
        })
        .collect();
    let diff = if staged_diff.trim().is_empty() {
        unstaged_diff.clone()
    } else {
        staged_diff.clone()
    };
    let has_untracked = files.iter().any(|file| file.untracked);
    Ok(ProjectState {
        files,
        file_diffs,
        diff,
        staged_diff,
        has_untracked,
    })
}

fn generate_message(diff: String) -> Result<String, String> {
    let key = env::var("OPENAI_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        return Err("Set OPENAI_API_KEY in your environment before generating.".into());
    }
    if diff.trim().is_empty() {
        return Err("No tracked changes to generate a message from. Stage new files first.".into());
    }
    let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-6-luna".into());
    let base_url =
        env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let temperature = env::var("OPENAI_TEMPERATURE")
        .ok()
        .and_then(|v| v.parse().ok());
    generate_commit_message(&diff, model, &key, &base_url, temperature)
}

impl LazyCommitApp {
    fn refresh_tab(&mut self, id: u64) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let root = tab.project.clone();
        let tx = self.tx.clone();
        tab.busy = true;
        tab.status = "Refreshing changes…".into();
        thread::spawn(move || {
            let _ = tx.send(WorkerResult::Loaded(id, load_project(&root)));
        });
    }

    fn poll(&mut self) {
        while let Ok(result) = self.rx.try_recv() {
            match result {
                WorkerResult::Loaded(id, result) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                        tab.busy = false;
                        match result {
                            Ok(state) => {
                                tab.status = format!("{} changed file(s)", state.files.len());
                                tab.state = Some(state);
                            }
                            Err(error) => tab.status = error,
                        }
                    }
                }
                WorkerResult::Generated(id, result) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                        tab.busy = false;
                        match result {
                            Ok(message) => {
                                tab.message = message;
                                tab.status =
                                    "Message generated. Review or edit it before committing."
                                        .into();
                            }
                            Err(error) => tab.status = error,
                        }
                    }
                }
                WorkerResult::Committed(id, result) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                        tab.busy = false;
                        match result {
                            Ok(output) => {
                                tab.status = if output.trim().is_empty() {
                                    "Commit created.".into()
                                } else {
                                    output.trim().to_owned()
                                };
                                tab.message.clear();
                                self.refresh_tab(id);
                            }
                            Err(error) => tab.status = error,
                        }
                    }
                }
                WorkerResult::ReportLoaded(result) => {
                    self.report_busy = false;
                    match result {
                        Ok(text) => {
                            self.report_output = text;
                            self.report_status = "Raw git commits loaded.".into();
                        }
                        Err(err) => {
                            self.report_status = err;
                        }
                    }
                }
                WorkerResult::ReportGenerated(result) => {
                    self.report_busy = false;
                    match result {
                        Ok(report) => {
                            self.report_output = report;
                            self.report_status = "Daily report generated! Ready to copy.".into();
                        }
                        Err(err) => {
                            self.report_status = err;
                        }
                    }
                }
            }
        }
    }

    fn fetch_report_activity(&mut self, call_ai: bool) {
        let repos = if self.report_use_tabs {
            let tab_repos: Vec<PathBuf> = self.tabs.iter().map(|t| t.project.clone()).collect();
            if tab_repos.is_empty() {
                if let Some(folder) = &self.report_folder {
                    find_git_repos(&[folder.clone()])
                } else {
                    find_git_repos(&[std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))])
                }
            } else {
                tab_repos
            }
        } else if let Some(folder) = &self.report_folder {
            find_git_repos(&[folder.clone()])
        } else {
            find_git_repos(&[std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))])
        };

        if repos.is_empty() {
            self.report_status = "No Git repositories found to report on.".into();
            return;
        }

        let since = self.report_since.clone();
        let author = if self.report_all_authors {
            None
        } else if !self.report_author.trim().is_empty() {
            Some(self.report_author.clone())
        } else {
            None
        };

        let tx = self.tx.clone();
        self.report_busy = true;
        self.report_copied = false;
        self.report_status = if call_ai {
            "Collecting commits and generating AI report…".into()
        } else {
            "Collecting commit history…".into()
        };

        thread::spawn(move || {
            let activities = collect_all_activity(&repos, &since, author.as_deref());
            if activities.is_empty() {
                let msg = format!(
                    "No commits found since '{}' across {} repository(ies).",
                    since,
                    repos.len()
                );
                if call_ai {
                    let _ = tx.send(WorkerResult::ReportGenerated(Err(msg)));
                } else {
                    let _ = tx.send(WorkerResult::ReportLoaded(Ok(msg)));
                }
                return;
            }

            let prompt = format_activity_prompt(&activities, &since);
            if !call_ai {
                let _ = tx.send(WorkerResult::ReportLoaded(Ok(prompt)));
                return;
            }

            let key = env::var("OPENAI_API_KEY").unwrap_or_default();
            if key.trim().is_empty() {
                let _ = tx.send(WorkerResult::ReportGenerated(Err(
                    "Set OPENAI_API_KEY in your environment to generate an AI report, or click 'Preview Commits' for raw logs.".into(),
                )));
                return;
            }
            let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-6-luna".into());
            let base_url = env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            let temperature = env::var("OPENAI_TEMPERATURE").ok().and_then(|v| v.parse().ok());

            let res = generate_daily_report(&prompt, model, &key, &base_url, temperature);
            let _ = tx.send(WorkerResult::ReportGenerated(res));
        });
    }

    fn generate(&mut self, id: u64) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let Some(state) = &tab.state else { return };
        let diff = state.diff.clone();
        let tx = self.tx.clone();
        tab.busy = true;
        tab.status = "Generating commit message…".into();
        thread::spawn(move || {
            let _ = tx.send(WorkerResult::Generated(id, generate_message(diff)));
        });
    }

    fn commit(&mut self, id: u64) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let Some(state) = tab.state.as_ref() else {
            return;
        };
        let root = tab.project.clone();
        let message = tab.message.trim().to_owned();
        if message.is_empty() {
            tab.status = "Generate or enter a commit message first.".into();
            return;
        }
        let staged = !state.staged_diff.trim().is_empty();
        let tx = self.tx.clone();
        tab.busy = true;
        tab.status = "Creating commit…".into();
        thread::spawn(move || {
            let mut command = Command::new("git");
            command.arg("-C").arg(root).arg("commit");
            if !staged {
                command.arg("-a");
            }
            let output = command
                .args(["-m", &message])
                .output()
                .map_err(|e| format!("Could not run git commit: {e}"))
                .and_then(|out| {
                    if out.status.success() {
                        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
                    } else {
                        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
                    }
                });
            let _ = tx.send(WorkerResult::Committed(id, output));
        });
    }
}

impl eframe::App for LazyCommitApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();
        self.poll();
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(24, 27, 33);
        visuals.window_fill = egui::Color32::from_rgb(30, 34, 41);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(35, 39, 47);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(43, 48, 57);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(54, 63, 76);
        visuals.selection.bg_fill = egui::Color32::from_rgb(44, 86, 112);
        ctx.set_visuals(visuals);

        let available_size = root_ui.available_size();
        egui::Frame::central_panel(root_ui.style()).show(root_ui, |ui| {
            ui.set_min_size(available_size - egui::Vec2::splat(16.0));
            ui.vertical(|ui| {
                let mut open_project = None;
                let mut close_tab = None;
                let mut activate_tab = None;
                let mut refresh_tab = None;
                let active_busy = self
                    .active_tab
                    .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
                    .is_some_and(|tab| tab.busy);

                ui.horizontal(|ui| {
                    ui.heading("lazycommit");
                    ui.separator();
                    if ui.button("＋ Open project").clicked() {
                        open_project = rfd::FileDialog::new().pick_folder();
                    }
                    if ui.button("📋 Daily Report").on_hover_text("Generate end-of-day daily report across projects").clicked() {
                        self.show_report = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!active_busy, egui::Button::new("Refresh"))
                            .clicked()
                        {
                            refresh_tab = self.active_tab;
                        }
                    });
                });

                egui::ScrollArea::horizontal()
                    .id_salt("workspace-tabs")
                    .max_height(36.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for tab in &self.tabs {
                                let title = tab.project.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or_else(|| tab.project.to_str().unwrap_or("project"));
                                let selected = self.active_tab == Some(tab.id);
                                if ui.selectable_label(selected, title)
                                    .on_hover_text(tab.project.display().to_string())
                                    .clicked()
                                {
                                    activate_tab = Some(tab.id);
                                }
                                if ui.small_button("×").on_hover_text("Close workspace").clicked() {
                                    close_tab = Some(tab.id);
                                }
                                ui.separator();
                            }
                        });
                    });

                if let Some(id) = activate_tab {
                    self.active_tab = Some(id);
                }
                if let Some(id) = close_tab {
                    self.close_tab(id);
                }
                if let Some(path) = open_project {
                    self.open_project(path);
                }
                if let Some(id) = refresh_tab {
                    self.refresh_tab(id);
                }

                let Some(active_id) = self.active_tab else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(100.0);
                        ui.heading("Open a Git workspace");
                        ui.label("Each workspace has its own changes, preview, and commit message.");
                        if ui.button("＋ Open project").clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_folder()
                        {
                            self.open_project(path);
                        }
                    });
                    return;
                };
                let Some(tab_index) = self.tabs.iter().position(|tab| tab.id == active_id) else {
                    return;
                };

                let (project, state, mut selected, mut message, status, busy) = {
                    let tab = &self.tabs[tab_index];
                    (
                        tab.project.clone(),
                        tab.state.clone(),
                        tab.selected_file.clone(),
                        tab.message.clone(),
                        tab.status.clone(),
                        tab.busy,
                    )
                };
                let diff = state.as_ref().map(|s| s.diff.clone()).unwrap_or_default();
                let files = state.as_ref().map(|s| s.files.clone()).unwrap_or_default();
                let file_diffs = state.as_ref().map(|s| s.file_diffs.clone()).unwrap_or_default();
                let has_untracked = state.as_ref().is_some_and(|s| s.has_untracked);

                ui.add_space(6.0);
                let preview_height = (ui.available_height() - 175.0).max(130.0);
                let mut generate_clicked = false;
                let mut commit_clicked = false;
                ui.horizontal(|ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(250.0);
                        ui.set_max_width(300.0);
                        ui.set_min_height(preview_height);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.heading("Changes");
                                ui.label(egui::RichText::new(files.len().to_string()).weak());
                            });
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .id_salt(("changed-files", active_id))
                                .max_height(preview_height - 55.0)
                                .show(ui, |ui| {
                                    for file in &files {
                                        let (badge, tint) = if file.untracked {
                                            ("?", egui::Color32::from_rgb(230, 182, 91))
                                        } else if file.status.contains('A') {
                                            ("A", egui::Color32::from_rgb(126, 201, 146))
                                        } else if file.status.contains('D') {
                                            ("D", egui::Color32::from_rgb(232, 123, 123))
                                        } else {
                                            ("M", egui::Color32::from_rgb(110, 177, 226))
                                        };
                                        let response = ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(badge).color(tint).strong());
                                            ui.selectable_label(
                                                selected.as_deref() == Some(&file.path),
                                                &file.path,
                                            )
                                        });
                                        if response.inner.clicked() {
                                            selected = Some(file.path.clone());
                                        }
                                    }
                                    if has_untracked {
                                        ui.add_space(8.0);
                                        ui.label(egui::RichText::new(
                                            "Untracked files are preview only until staged.",
                                        ).weak());
                                    }
                                });
                        });
                    });

                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(preview_height);
                        ui.vertical(|ui| {
                            let selected_entry = selected.as_ref()
                                .and_then(|path| files.iter().find(|f| &f.path == path));
                            let is_untracked = selected_entry.is_some_and(|file| file.untracked);
                            ui.horizontal(|ui| {
                                ui.heading(selected.as_deref().unwrap_or("Diff preview"));
                                if is_untracked {
                                    ui.label(egui::RichText::new("UNTRACKED · preview only")
                                        .color(egui::Color32::from_rgb(230, 182, 91)));
                                }
                            });
                            ui.separator();
                            let untracked_preview = if is_untracked {
                                selected.as_ref().map(|path| {
                                    match std::fs::read(project.join(path)) {
                                        Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| {
                                            "[Binary file: preview unavailable]".into()
                                        }),
                                        Err(error) => format!("[Could not read file: {error}]"),
                                    }
                                })
                            } else { None };
                            let selected_diff = selected.as_ref().and_then(|file| {
                                file_diffs.iter().find(|(name, _)| name == file)
                                    .map(|(_, diff)| diff)
                            });
                            let shown = if let Some(preview) = untracked_preview.as_ref() {
                                preview.as_str()
                            } else {
                                selected_diff.map(String::as_str).unwrap_or_else(|| {
                                    if diff.is_empty() { "Select a changed file to inspect it." }
                                    else { &diff }
                                })
                            };
                            egui::ScrollArea::both()
                                .id_salt(("diff-preview", active_id))
                                .max_height(preview_height - 52.0)
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        for line in shown.lines() {
                                            let color = if line.starts_with("+++") || line.starts_with("---") {
                                                egui::Color32::from_rgb(150, 158, 170)
                                            } else if line.starts_with('+') {
                                                egui::Color32::from_rgb(145, 218, 163)
                                            } else if line.starts_with('-') {
                                                egui::Color32::from_rgb(240, 139, 139)
                                            } else if line.starts_with('@') {
                                                egui::Color32::from_rgb(119, 190, 230)
                                            } else {
                                                egui::Color32::from_rgb(205, 210, 218)
                                            };
                                            ui.label(egui::RichText::new(line).monospace().color(color));
                                        }
                                    });
                                });
                        });
                    });
                });
                self.tabs[tab_index].selected_file = selected;

                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Commit message");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled(
                                !busy && !message.trim().is_empty(),
                                egui::Button::new("Commit").fill(egui::Color32::from_rgb(48, 125, 91)),
                            ).clicked() {
                                commit_clicked = true;
                            }
                            if ui.add_enabled(
                                !busy && !diff.is_empty(),
                                egui::Button::new("✦ Generate message"),
                            ).clicked() {
                                generate_clicked = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.add(egui::TextEdit::multiline(&mut message)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3)
                        .hint_text("Generated message appears here. You can edit it before committing."));
                });
                self.tabs[tab_index].message = message;
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(&status);
                    if busy { ui.spinner(); }
                });

                if generate_clicked { self.generate(active_id); }
                if commit_clicked { self.commit(active_id); }
            });
        });

        if self.show_report {
            let mut show_report = self.show_report;
            let mut generate_report_clicked = false;
            let mut preview_report_clicked = false;
            let mut choose_folder_clicked = false;

            egui::Window::new("📋 Daily End-of-Day Report")
                .open(&mut show_report)
                .resizable(true)
                .default_size([820.0, 580.0])
                .show(&ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Target:").strong());
                            let tab_count = self.tabs.len();
                            ui.radio_value(
                                &mut self.report_use_tabs,
                                true,
                                format!("Open tabs ({tab_count})"),
                            );
                            ui.radio_value(&mut self.report_use_tabs, false, "Scan folder");

                            if !self.report_use_tabs {
                                if ui.button("📁 Choose folder…").clicked() {
                                    choose_folder_clicked = true;
                                }
                                if let Some(folder) = &self.report_folder {
                                    let name = folder
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("folder");
                                    ui.label(egui::RichText::new(name).weak())
                                        .on_hover_text(folder.display().to_string());
                                }
                            }
                        });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Since:");
                            ui.add(egui::TextEdit::singleline(&mut self.report_since).desired_width(85.0))
                                .on_hover_text("Git time window, e.g. 'midnight', 'yesterday', '24 hours ago'");

                            ui.label("Author:");
                            ui.add_enabled(
                                !self.report_all_authors,
                                egui::TextEdit::singleline(&mut self.report_author).desired_width(160.0),
                            ).on_hover_text("Filter by author email or name");

                            ui.checkbox(&mut self.report_all_authors, "All authors");

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let can_copy = !self.report_output.trim().is_empty();
                                let copy_label = if self.report_copied { "✓ Copied" } else { "📋 Copy" };
                                if ui.add_enabled(can_copy, egui::Button::new(copy_label)).clicked() {
                                    ctx.copy_text(self.report_output.clone());
                                    self.report_copied = true;
                                }

                                if ui.add_enabled(
                                    !self.report_busy,
                                    egui::Button::new("✦ Generate AI Report").fill(egui::Color32::from_rgb(48, 125, 91)),
                                ).clicked() {
                                    generate_report_clicked = true;
                                }

                                if ui.add_enabled(!self.report_busy, egui::Button::new("Preview Commits")).clicked() {
                                    preview_report_clicked = true;
                                }
                            });
                        });

                        ui.separator();

                        let scroll_height = (ui.available_height() - 40.0).max(180.0);
                        egui::ScrollArea::both()
                            .id_salt("daily-report-output-scroll")
                            .max_height(scroll_height)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.report_output)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(18)
                                        .font(egui::TextStyle::Monospace)
                                        .hint_text("Click '✦ Generate AI Report' to summarize today's committed work across your repositories for your boss, or 'Preview Commits' to view raw git logs."),
                                );
                            });

                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(&self.report_status);
                            if self.report_busy {
                                ui.spinner();
                            }
                        });
                    });
                });

            if choose_folder_clicked {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.report_folder = Some(folder);
                }
            }
            if preview_report_clicked {
                self.fetch_report_activity(false);
            }
            if generate_report_clicked {
                self.fetch_report_activity(true);
            }

            self.show_report = show_report;
        }
    }
}

pub fn run(initial_project: Option<PathBuf>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1050.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "lazycommit",
        options,
        Box::new(move |_cc| Ok(Box::new(LazyCommitApp::with_project(initial_project)))),
    )
}
