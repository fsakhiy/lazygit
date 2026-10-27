use eframe::egui;
use lazycommit::{
    collect_all_activity, detect_git_author, find_git_repos, format_activity_prompt,
    generate_commit_message, generate_daily_report,
};
use std::{
    env,
    path::{Path, PathBuf},
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
    branch: String,
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

pub struct LazyCommitApp {
    tabs: Vec<WorkspaceTab>,
    active_tab: Option<u64>,
    next_tab_id: u64,
    tx: Sender<WorkerResult>,
    rx: Receiver<WorkerResult>,

    // Daily report modal state
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
    pub fn with_project(project: Option<PathBuf>) -> Self {
        let mut app = Self::default();
        if let Some(project) = project {
            app.open_project(project);
        }
        app
    }

    fn open_project(&mut self, project: PathBuf) {
        // If repository is already open in a tab, simply switch to it
        if let Some(existing) = self.tabs.iter().find(|t| t.project == project) {
            self.active_tab = Some(existing.id);
            return;
        }

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

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
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

fn load_project(root: &Path) -> Result<ProjectState, String> {
    let inside = git(root, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return Err("Choose a directory inside a Git repository.".into());
    }

    let branch = git(root, &["branch", "--show-current"])
        .or_else(|_| git(root, &["rev-parse", "--abbrev-ref", "HEAD"]))
        .unwrap_or_else(|_| "main".to_string())
        .trim()
        .to_string();

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
        branch: if branch.is_empty() { "HEAD".into() } else { branch },
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
                                let count = state.files.len();
                                tab.status = format!("{count} changed file(s)");
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
                                tab.status = "Commit message generated! Review or edit before committing.".into();
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
                                    "Commit created successfully.".into()
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
        tab.status = "Generating commit message with AI…".into();
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
            tab.status = "Enter or generate a commit message first.".into();
            return;
        }
        let staged = !state.staged_diff.trim().is_empty();
        let tx = self.tx.clone();
        tab.busy = true;
        tab.status = "Executing git commit…".into();
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

        // Refined modern dark theme styling
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(17, 19, 24);
        visuals.window_fill = egui::Color32::from_rgb(23, 26, 33);
        visuals.faint_bg_color = egui::Color32::from_rgb(29, 33, 42);
        visuals.extreme_bg_color = egui::Color32::from_rgb(13, 15, 19);

        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(26, 30, 39);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 48, 62));
        visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(31, 36, 47);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 56, 73));
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(44, 52, 68);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(72, 85, 110));
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);

        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(52, 62, 82);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);

        visuals.selection.bg_fill = egui::Color32::from_rgb(36, 72, 120);
        visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 115, 185));
        visuals.window_corner_radius = egui::CornerRadius::same(10);
        visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 56, 73));

        ctx.set_visuals(visuals);

        let available_size = root_ui.available_size();
        egui::Frame::central_panel(root_ui.style()).show(root_ui, |ui| {
            ui.set_min_size(available_size - egui::Vec2::splat(12.0));
            ui.vertical(|ui| {
                let mut open_project = None;
                let mut close_tab = None;
                let mut activate_tab = None;
                let mut refresh_tab = None;
                let active_busy = self
                    .active_tab
                    .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
                    .is_some_and(|tab| tab.busy);

                // --- TOP NAVBAR ---
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(22, 25, 33))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .corner_radius(egui::CornerRadius::same(8))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 58)))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Logo and App Name
                            ui.label(
                                egui::RichText::new("⚡ lazycommit")
                                    .size(16.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(240, 243, 248)),
                            );

                            egui::Frame::new()
                                .fill(egui::Color32::from_rgb(34, 40, 53))
                                .inner_margin(egui::Margin::symmetric(6, 2))
                                .corner_radius(egui::CornerRadius::same(4))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                            .size(10.0)
                                            .color(egui::Color32::from_rgb(140, 150, 170)),
                                    );
                                });

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(8.0);

                            // Header Actions
                            if ui
                                .button(egui::RichText::new("📁 Open Repository").strong())
                                .on_hover_text("Open a Git repository as a workspace tab")
                                .clicked()
                            {
                                open_project = rfd::FileDialog::new().pick_folder();
                            }

                            let report_btn = egui::Button::new(
                                egui::RichText::new("📋 Daily EOD Report")
                                    .strong()
                                    .color(egui::Color32::from_rgb(210, 225, 255)),
                            )
                            .fill(egui::Color32::from_rgb(32, 54, 88));

                            if ui
                                .add(report_btn)
                                .on_hover_text("Generate daily report of committed changes across your repositories")
                                .clicked()
                            {
                                self.show_report = true;
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let refresh_btn = egui::Button::new(if active_busy {
                                    "⏳ Refreshing…"
                                } else {
                                    "🔄 Refresh"
                                });
                                if ui.add_enabled(!active_busy && self.active_tab.is_some(), refresh_btn).clicked() {
                                    refresh_tab = self.active_tab;
                                }
                            });
                        });
                    });

                ui.add_space(6.0);

                // --- WORKSPACE TABS STRIP ---
                if !self.tabs.is_empty() {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(20, 23, 30))
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .corner_radius(egui::CornerRadius::same(6))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(34, 39, 52)))
                        .show(ui, |ui| {
                            egui::ScrollArea::horizontal()
                                .id_salt("workspace-tabs")
                                .max_height(32.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        for tab in &self.tabs {
                                            let is_selected = self.active_tab == Some(tab.id);
                                            let title = tab
                                                .project
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("project");

                                            let file_count = tab
                                                .state
                                                .as_ref()
                                                .map(|s| s.files.len())
                                                .unwrap_or(0);

                                            let tab_bg = if is_selected {
                                                egui::Color32::from_rgb(38, 46, 62)
                                            } else {
                                                egui::Color32::from_rgb(26, 30, 39)
                                            };
                                            let tab_stroke = if is_selected {
                                                egui::Stroke::new(1.0, egui::Color32::from_rgb(65, 82, 115))
                                            } else {
                                                egui::Stroke::new(1.0, egui::Color32::from_rgb(36, 42, 54))
                                            };

                                            egui::Frame::new()
                                                .fill(tab_bg)
                                                .stroke(tab_stroke)
                                                .inner_margin(egui::Margin::symmetric(10, 4))
                                                .corner_radius(egui::CornerRadius::same(6))
                                                .show(ui, |ui| {
                                                    ui.horizontal(|ui| {
                                                        let label_color = if is_selected {
                                                            egui::Color32::from_rgb(240, 245, 255)
                                                        } else {
                                                            egui::Color32::from_rgb(160, 170, 188)
                                                        };

                                                        let tab_click = ui
                                                            .label(
                                                                egui::RichText::new(format!("📦 {title}"))
                                                                    .color(label_color)
                                                                    .strong(),
                                                            )
                                                            .on_hover_text(tab.project.display().to_string());

                                                        if tab_click.clicked() {
                                                            activate_tab = Some(tab.id);
                                                        }

                                                        if file_count > 0 {
                                                            egui::Frame::new()
                                                                .fill(if is_selected {
                                                                    egui::Color32::from_rgb(52, 65, 92)
                                                                } else {
                                                                    egui::Color32::from_rgb(36, 42, 56)
                                                                })
                                                                .inner_margin(egui::Margin::symmetric(5, 1))
                                                                .corner_radius(egui::CornerRadius::same(8))
                                                                .show(ui, |ui| {
                                                                    ui.label(
                                                                        egui::RichText::new(file_count.to_string())
                                                                            .size(10.0)
                                                                            .color(egui::Color32::from_rgb(175, 190, 215)),
                                                                    );
                                                                });
                                                        }

                                                        ui.add_space(2.0);
                                                        if ui
                                                            .small_button("×")
                                                            .on_hover_text("Close workspace tab")
                                                            .clicked()
                                                        {
                                                            close_tab = Some(tab.id);
                                                        }
                                                    });
                                                });
                                        }
                                    });
                                });
                        });
                    ui.add_space(6.0);
                }

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

                // --- EMPTY STATE IF NO ACTIVE WORKSPACE ---
                let Some(active_id) = self.active_tab else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(70.0);
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(23, 26, 34))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 58)))
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(36, 28))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("⚡").size(32.0));
                                ui.add_space(8.0);
                                ui.heading(
                                    egui::RichText::new("No Git Workspace Open")
                                        .size(18.0)
                                        .color(egui::Color32::from_rgb(240, 245, 255)),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Open any Git repository to inspect changes, generate Conventional Commit messages,\n\
                                         or use the Daily Report tool to summarize completed work across multiple projects.",
                                    )
                                    .color(egui::Color32::from_rgb(145, 155, 175)),
                                );
                                ui.add_space(16.0);

                                ui.horizontal(|ui| {
                                    let open_btn = egui::Button::new(
                                        egui::RichText::new("＋ Open Repository")
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(45, 88, 150));

                                    if ui.add(open_btn).clicked()
                                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                                    {
                                        self.open_project(path);
                                    }

                                    let rep_btn = egui::Button::new(
                                        egui::RichText::new("📋 Daily EOD Report")
                                            .strong()
                                            .color(egui::Color32::from_rgb(210, 225, 255)),
                                    )
                                    .fill(egui::Color32::from_rgb(34, 46, 68));

                                    if ui.add(rep_btn).clicked() {
                                        self.show_report = true;
                                    }
                                });
                            });
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
                let branch_name = state
                    .as_ref()
                    .map(|s| s.branch.clone())
                    .unwrap_or_else(|| "HEAD".into());

                // Auto-select first file if none selected
                if selected.is_none() && !files.is_empty() {
                    selected = Some(files[0].path.clone());
                }

                // --- REPOSITORY SUMMARY BAR ---
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(20, 23, 30))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .corner_radius(egui::CornerRadius::same(6))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(34, 39, 52)))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(project.display().to_string())
                                    .monospace()
                                    .color(egui::Color32::from_rgb(140, 150, 170)),
                            );

                            ui.separator();
                            ui.label(
                                egui::RichText::new(format!("🌿 {branch_name}"))
                                    .color(egui::Color32::from_rgb(110, 185, 235))
                                    .strong(),
                            );

                            ui.separator();
                            let staged_count = files.iter().filter(|f| !f.untracked && !f.status.starts_with(' ')).count();
                            let unstaged_count = files.iter().filter(|f| !f.untracked && f.status.starts_with(' ')).count();
                            let untracked_count = files.iter().filter(|f| f.untracked).count();

                            ui.label(
                                egui::RichText::new(format!(
                                    "{} total ({staged_count} staged, {unstaged_count} modified, {untracked_count} untracked)",
                                    files.len()
                                ))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(150, 160, 180)),
                            );
                        });
                    });

                ui.add_space(6.0);

                // --- MAIN TWO-COLUMN SPLIT: FILE LIST + DIFF PREVIEW ---
                let available_height = ui.available_height();
                let commit_area_height = 160.0;
                let preview_height = (available_height - commit_area_height - 35.0).max(160.0);

                let mut generate_clicked = false;
                let mut commit_clicked = false;

                ui.horizontal(|ui| {
                    // LEFT PANE: Changed Files
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(22, 25, 33))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 58)))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(10, 10))
                        .show(ui, |ui| {
                            ui.set_min_width(260.0);
                            ui.set_max_width(320.0);
                            ui.set_min_height(preview_height);
                            ui.set_max_height(preview_height);

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Changed Files")
                                            .strong()
                                            .color(egui::Color32::from_rgb(220, 226, 238)),
                                    );
                                    egui::Frame::new()
                                        .fill(egui::Color32::from_rgb(34, 40, 54))
                                        .inner_margin(egui::Margin::symmetric(6, 1))
                                        .corner_radius(egui::CornerRadius::same(6))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(files.len().to_string())
                                                    .size(11.0)
                                                    .color(egui::Color32::from_rgb(160, 175, 200)),
                                            );
                                        });
                                });

                                ui.add_space(4.0);
                                ui.separator();
                                ui.add_space(4.0);

                                if files.is_empty() {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(30.0);
                                        ui.label(
                                            egui::RichText::new("✓ Clean working tree")
                                                .color(egui::Color32::from_rgb(120, 190, 140)),
                                        );
                                        ui.label(
                                            egui::RichText::new("No uncommitted changes.")
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(130, 140, 155)),
                                        );
                                    });
                                } else {
                                    egui::ScrollArea::vertical()
                                        .id_salt(("changed-files", active_id))
                                        .max_height(preview_height - 65.0)
                                        .show(ui, |ui| {
                                            for file in &files {
                                                let is_active = selected.as_deref() == Some(&file.path);
                                                let (badge, badge_color, badge_bg) = if file.untracked {
                                                    (
                                                        "?",
                                                        egui::Color32::from_rgb(245, 158, 11),
                                                        egui::Color32::from_rgba_premultiplied(55, 40, 15, 180),
                                                    )
                                                } else if file.status.contains('A') {
                                                    (
                                                        "A",
                                                        egui::Color32::from_rgb(52, 211, 153),
                                                        egui::Color32::from_rgba_premultiplied(20, 55, 35, 180),
                                                    )
                                                } else if file.status.contains('D') {
                                                    (
                                                        "D",
                                                        egui::Color32::from_rgb(248, 113, 113),
                                                        egui::Color32::from_rgba_premultiplied(65, 25, 25, 180),
                                                    )
                                                } else {
                                                    (
                                                        "M",
                                                        egui::Color32::from_rgb(96, 165, 250),
                                                        egui::Color32::from_rgba_premultiplied(25, 45, 75, 180),
                                                    )
                                                };

                                                let row_bg = if is_active {
                                                    egui::Color32::from_rgb(36, 44, 60)
                                                } else {
                                                    egui::Color32::TRANSPARENT
                                                };

                                                let file_row = egui::Frame::new()
                                                    .fill(row_bg)
                                                    .inner_margin(egui::Margin::symmetric(6, 4))
                                                    .corner_radius(egui::CornerRadius::same(4))
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            // Status Badge
                                                            egui::Frame::new()
                                                                .fill(badge_bg)
                                                                .corner_radius(egui::CornerRadius::same(4))
                                                                .inner_margin(egui::Margin::symmetric(5, 1))
                                                                .show(ui, |ui| {
                                                                    ui.label(
                                                                        egui::RichText::new(badge)
                                                                            .strong()
                                                                            .size(11.0)
                                                                            .color(badge_color),
                                                                    );
                                                                });

                                                            // File Path with basename in bold
                                                            let path_str = &file.path;
                                                            let (dir_part, base_part) = match path_str.rfind('/') {
                                                                Some(pos) => (&path_str[..=pos], &path_str[pos + 1..]),
                                                                None => ("", path_str.as_str()),
                                                            };

                                                            let text_color = if is_active {
                                                                egui::Color32::WHITE
                                                            } else {
                                                                egui::Color32::from_rgb(205, 212, 225)
                                                            };

                                                            if !dir_part.is_empty() {
                                                                ui.label(
                                                                    egui::RichText::new(dir_part)
                                                                        .size(11.0)
                                                                        .color(egui::Color32::from_rgb(130, 140, 160)),
                                                                );
                                                            }
                                                            ui.label(
                                                                egui::RichText::new(base_part)
                                                                    .color(text_color)
                                                                    .strong(),
                                                            );
                                                        });
                                                    });

                                                if file_row.response.interact(egui::Sense::click()).clicked() {
                                                    selected = Some(file.path.clone());
                                                }
                                            }

                                            if has_untracked {
                                                ui.add_space(8.0);
                                                egui::Frame::new()
                                                    .fill(egui::Color32::from_rgba_premultiplied(50, 38, 15, 140))
                                                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 75, 30)))
                                                    .inner_margin(egui::Margin::symmetric(8, 6))
                                                    .corner_radius(egui::CornerRadius::same(6))
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            egui::RichText::new("⚠️ Untracked files are preview only until staged with 'git add'.")
                                                                .size(11.0)
                                                                .color(egui::Color32::from_rgb(240, 190, 80)),
                                                        );
                                                    });
                                            }
                                        });
                                }
                            });
                        });

                    // RIGHT PANE: Diff Preview
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(18, 20, 26))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 58)))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.set_min_height(preview_height);
                            ui.set_max_height(preview_height);

                            ui.vertical(|ui| {
                                let selected_entry = selected
                                    .as_ref()
                                    .and_then(|path| files.iter().find(|f| &f.path == path));
                                let is_untracked =
                                    selected_entry.is_some_and(|file| file.untracked);

                                ui.horizontal(|ui| {
                                    let title = selected
                                        .as_deref()
                                        .unwrap_or("Diff Preview (All Changes)");
                                    ui.label(
                                        egui::RichText::new(title)
                                            .strong()
                                            .color(egui::Color32::from_rgb(225, 232, 245)),
                                    );

                                    if is_untracked {
                                        egui::Frame::new()
                                            .fill(egui::Color32::from_rgb(55, 42, 18))
                                            .corner_radius(egui::CornerRadius::same(4))
                                            .inner_margin(egui::Margin::symmetric(6, 2))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    egui::RichText::new("UNTRACKED FILE")
                                                        .size(10.0)
                                                        .color(egui::Color32::from_rgb(245, 175, 40)),
                                                );
                                            });
                                    }
                                });

                                ui.add_space(4.0);
                                ui.separator();
                                ui.add_space(4.0);

                                let untracked_preview = if is_untracked {
                                    selected.as_ref().map(|path| match std::fs::read(project.join(path)) {
                                        Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| {
                                            "[Binary file: preview unavailable]".into()
                                        }),
                                        Err(error) => format!("[Could not read file: {error}]"),
                                    })
                                } else {
                                    None
                                };

                                let selected_diff = selected.as_ref().and_then(|file| {
                                    file_diffs
                                        .iter()
                                        .find(|(name, _)| name == file)
                                        .map(|(_, diff)| diff)
                                });

                                let shown = if let Some(preview) = untracked_preview.as_ref() {
                                    preview.as_str()
                                } else {
                                    selected_diff.map(String::as_str).unwrap_or_else(|| {
                                        if diff.is_empty() {
                                            "Select a changed file from the left to inspect its diff."
                                        } else {
                                            &diff
                                        }
                                    })
                                };

                                egui::ScrollArea::both()
                                    .id_salt(("diff-preview", active_id))
                                    .max_height(preview_height - 65.0)
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            for line in shown.lines() {
                                                let (text_color, bg_color) = if line.starts_with("+++")
                                                    || line.starts_with("---")
                                                {
                                                    (
                                                        egui::Color32::from_rgb(180, 188, 200),
                                                        Some(egui::Color32::from_rgba_premultiplied(40, 45, 55, 120)),
                                                    )
                                                } else if line.starts_with('+') {
                                                    (
                                                        egui::Color32::from_rgb(145, 225, 165),
                                                        Some(egui::Color32::from_rgba_premultiplied(25, 65, 38, 90)),
                                                    )
                                                } else if line.starts_with('-') {
                                                    (
                                                        egui::Color32::from_rgb(245, 140, 140),
                                                        Some(egui::Color32::from_rgba_premultiplied(75, 25, 25, 90)),
                                                    )
                                                } else if line.starts_with('@') {
                                                    (
                                                        egui::Color32::from_rgb(125, 195, 240),
                                                        Some(egui::Color32::from_rgba_premultiplied(28, 45, 70, 120)),
                                                    )
                                                } else {
                                                    (egui::Color32::from_rgb(205, 212, 222), None)
                                                };

                                                let frame = egui::Frame::new()
                                                    .fill(bg_color.unwrap_or(egui::Color32::TRANSPARENT))
                                                    .inner_margin(egui::Margin::symmetric(4, 1))
                                                    .corner_radius(egui::CornerRadius::same(2));

                                                frame.show(ui, |ui| {
                                                    ui.label(
                                                        egui::RichText::new(line)
                                                            .monospace()
                                                            .size(12.0)
                                                            .color(text_color),
                                                    );
                                                });
                                            }
                                        });
                                    });
                            });
                        });
                });

                self.tabs[tab_index].selected_file = selected;

                ui.add_space(6.0);

                // --- COMMIT MESSAGE PANEL ---
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(22, 25, 33))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 44, 58)))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Commit Message")
                                    .strong()
                                    .color(egui::Color32::from_rgb(230, 235, 248)),
                            );

                            let first_line_len = message.lines().next().map(|l| l.chars().count()).unwrap_or(0);
                            let count_color = if first_line_len == 0 {
                                egui::Color32::from_rgb(120, 130, 145)
                            } else if first_line_len <= 72 {
                                egui::Color32::from_rgb(110, 190, 140)
                            } else {
                                egui::Color32::from_rgb(240, 170, 70)
                            };

                            ui.label(
                                egui::RichText::new(format!("{first_line_len}/72 chars"))
                                    .size(11.0)
                                    .color(count_color),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let can_commit = !busy && !message.trim().is_empty();
                                let commit_btn = egui::Button::new(
                                    egui::RichText::new("✓ Commit Changes")
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(35, 120, 75));

                                if ui.add_enabled(can_commit, commit_btn).clicked() {
                                    commit_clicked = true;
                                }

                                let can_generate = !busy && !diff.is_empty();
                                let generate_btn = egui::Button::new(
                                    egui::RichText::new("✦ Generate with AI")
                                        .strong()
                                        .color(egui::Color32::from_rgb(235, 230, 255)),
                                )
                                .fill(egui::Color32::from_rgb(65, 45, 115));

                                if ui.add_enabled(can_generate, generate_btn).clicked() {
                                    generate_clicked = true;
                                }
                            });
                        });

                        ui.add_space(4.0);
                        ui.add(
                            egui::TextEdit::multiline(&mut message)
                                .desired_width(f32::INFINITY)
                                .desired_rows(3)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("Click '✦ Generate with AI' or type your Conventional Commit message here (e.g. feat: add authentication)…"),
                        );
                    });

                self.tabs[tab_index].message = message;

                // --- FOOTER / STATUS BAR ---
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(17, 19, 24))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let status_dot = if busy {
                                egui::RichText::new("🟡").size(10.0)
                            } else if status.contains("error") || status.contains("Error") || status.contains("failed") {
                                egui::RichText::new("🔴").size(10.0)
                            } else {
                                egui::RichText::new("🟢").size(10.0)
                            };
                            ui.label(status_dot);
                            ui.label(
                                egui::RichText::new(&status)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(170, 180, 200)),
                            );

                            if busy {
                                ui.spinner();
                            }
                        });
                    });

                if generate_clicked {
                    self.generate(active_id);
                }
                if commit_clicked {
                    self.commit(active_id);
                }
            });
        });

        // --- DAILY REPORT MODAL WINDOW ---
        if self.show_report {
            let mut show_report = self.show_report;
            let mut generate_report_clicked = false;
            let mut preview_report_clicked = false;
            let mut choose_folder_clicked = false;

            egui::Window::new("📋 Daily End-of-Day (EOD) Report")
                .open(&mut show_report)
                .resizable(true)
                .default_size([840.0, 600.0])
                .show(&ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(
                                "Synthesize committed work across your repositories into a boss-ready end-of-day summary.",
                            )
                            .size(12.0)
                            .color(egui::Color32::from_rgb(150, 160, 180)),
                        );

                        ui.add_space(8.0);

                        // Scope & Parameters Card
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(25, 29, 38))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 48, 64)))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("Target Scope:").strong());
                                    let tab_count = self.tabs.len();
                                    ui.radio_value(
                                        &mut self.report_use_tabs,
                                        true,
                                        format!("Open Tabs ({tab_count})"),
                                    );
                                    ui.radio_value(&mut self.report_use_tabs, false, "Scan Folder");

                                    if !self.report_use_tabs {
                                        if ui.button("📁 Browse…").clicked() {
                                            choose_folder_clicked = true;
                                        }
                                        if let Some(folder) = &self.report_folder {
                                            let name = folder
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("folder");
                                            ui.label(
                                                egui::RichText::new(name)
                                                    .color(egui::Color32::from_rgb(130, 180, 230))
                                                    .strong(),
                                            )
                                            .on_hover_text(folder.display().to_string());
                                        } else {
                                            ui.label(
                                                egui::RichText::new("(Current directory)")
                                                    .weak()
                                                    .size(11.0),
                                            );
                                        }
                                    }
                                });

                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.label("Time Window:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.report_since)
                                            .desired_width(90.0),
                                    );

                                    // Quick preset buttons for convenience
                                    if ui.small_button("Today").on_hover_text("Since midnight").clicked() {
                                        self.report_since = "midnight".into();
                                    }
                                    if ui.small_button("Yesterday").on_hover_text("Since yesterday").clicked() {
                                        self.report_since = "yesterday".into();
                                    }
                                    if ui.small_button("3 Days").on_hover_text("Past 3 days").clicked() {
                                        self.report_since = "3 days ago".into();
                                    }

                                    ui.add_space(8.0);
                                    ui.separator();
                                    ui.add_space(8.0);

                                    ui.label("Author:");
                                    ui.add_enabled(
                                        !self.report_all_authors,
                                        egui::TextEdit::singleline(&mut self.report_author)
                                            .desired_width(150.0),
                                    );
                                    ui.checkbox(&mut self.report_all_authors, "All authors");
                                });
                            });

                        ui.add_space(8.0);

                        // Actions toolbar
                        ui.horizontal(|ui| {
                            let gen_btn = egui::Button::new(
                                egui::RichText::new("✦ Generate AI Report")
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(45, 95, 160));

                            if ui.add_enabled(!self.report_busy, gen_btn).clicked() {
                                generate_report_clicked = true;
                            }

                            if ui
                                .add_enabled(!self.report_busy, egui::Button::new("Preview Raw Commits"))
                                .on_hover_text("Display raw git commits without calling OpenAI")
                                .clicked()
                            {
                                preview_report_clicked = true;
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let has_content = !self.report_output.trim().is_empty();
                                let copy_btn_text = if self.report_copied {
                                    "✓ Copied to Clipboard"
                                } else {
                                    "📋 Copy to Clipboard"
                                };
                                let copy_btn = egui::Button::new(
                                    egui::RichText::new(copy_btn_text)
                                        .strong()
                                        .color(if self.report_copied {
                                            egui::Color32::from_rgb(130, 230, 160)
                                        } else {
                                            egui::Color32::from_rgb(220, 230, 250)
                                        }),
                                )
                                .fill(if self.report_copied {
                                    egui::Color32::from_rgb(25, 60, 40)
                                } else {
                                    egui::Color32::from_rgb(38, 46, 62)
                                });

                                if ui.add_enabled(has_content, copy_btn).clicked() {
                                    ctx.copy_text(self.report_output.clone());
                                    self.report_copied = true;
                                }
                            });
                        });

                        ui.add_space(8.0);

                        // Output editor / preview
                        let scroll_height = (ui.available_height() - 40.0).max(220.0);
                        egui::ScrollArea::both()
                            .id_salt("daily-report-output-scroll")
                            .max_height(scroll_height)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.report_output)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(20)
                                        .font(egui::TextStyle::Monospace)
                                        .hint_text(
                                            "Click '✦ Generate AI Report' to produce a polished executive report,\n\
                                             or click 'Preview Raw Commits' to see aggregated git logs across repositories.",
                                        ),
                                );
                            });

                        ui.add_space(4.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            let status_dot = if self.report_busy {
                                egui::RichText::new("🟡").size(10.0)
                            } else if self.report_status.contains("Error")
                                || self.report_status.contains("failed")
                                || self.report_status.contains("No commits")
                            {
                                egui::RichText::new("🟠").size(10.0)
                            } else {
                                egui::RichText::new("🟢").size(10.0)
                            };
                            ui.label(status_dot);
                            ui.label(
                                egui::RichText::new(&self.report_status)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(170, 180, 200)),
                            );
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
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([800.0, 560.0])
            .with_title("lazycommit"),
        ..Default::default()
    };
    eframe::run_native(
        "lazycommit",
        options,
        Box::new(move |_cc| Ok(Box::new(LazyCommitApp::with_project(initial_project)))),
    )
}
