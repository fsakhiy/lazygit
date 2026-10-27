use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, Rect, Stroke, Vec2};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

impl DiffFileStatus {
    pub fn badge_letter(&self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "U",
        }
    }

    pub fn badge_color(&self) -> Color32 {
        match self {
            Self::Modified => Color32::from_rgb(33, 115, 220),
            Self::Added => Color32::from_rgb(34, 150, 75),
            Self::Deleted => Color32::from_rgb(215, 60, 65),
            Self::Renamed => Color32::from_rgb(145, 75, 205),
            Self::Untracked => Color32::from_rgb(215, 140, 30),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
    NoNewline,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line_no: Option<usize>,
    pub new_line_no: Option<usize>,
    pub text: String,
    pub intra_highlights: Vec<(usize, usize)>, // (char_start, char_end) in Unicode chars
}

#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub header_raw: String,
    pub context_name: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
    pub raw_patch: String,
}

#[derive(Clone, Debug)]
pub struct DiffFile {
    pub path: String,
    pub old_path: String,
    pub status: DiffFileStatus,
    pub is_staged: bool,
    pub hunks: Vec<DiffHunk>,
}

pub enum DiffAction {
    StageFile(String),
    UnstageFile(String),
    DiscardFile(String, bool), // (path, is_untracked)
    StageHunk(String),        // patch string
    UnstageHunk(String),      // patch string
    DiscardHunk(String),      // patch string
    ToggleCollapse(String),   // path
}

// ---------------- Tokenization and Intra-Line LCS ----------------

#[derive(Clone, Debug)]
struct Token<'a> {
    text: &'a str,
    char_start: usize,
    char_end: usize,
}

fn tokenize(s: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, usize, char)> = s
        .char_indices()
        .enumerate()
        .map(|(char_idx, (byte_idx, c))| (char_idx, byte_idx, c))
        .collect();

    if chars.is_empty() {
        return tokens;
    }

    let mut i = 0;
    while i < chars.len() {
        let (start_char, start_byte, c) = chars[i];
        let is_word = c.is_alphanumeric() || c == '_';
        let is_space = c.is_whitespace();

        let mut j = i + 1;
        while j < chars.len() {
            let (_, _, next_c) = chars[j];
            let next_is_word = next_c.is_alphanumeric() || next_c == '_';
            let next_is_space = next_c.is_whitespace();

            if is_word && next_is_word {
                j += 1;
            } else if is_space && next_is_space {
                j += 1;
            } else if !is_word && !is_space && !next_is_word && !next_is_space && next_c == c {
                j += 1;
            } else {
                break;
            }
        }

        let end_char = if j < chars.len() { chars[j].0 } else { chars.len() };
        let end_byte = if j < chars.len() { chars[j].1 } else { s.len() };

        tokens.push(Token {
            text: &s[start_byte..end_byte],
            char_start: start_char,
            char_end: end_char,
        });
        i = j;
    }
    tokens
}

fn compute_token_lcs(tokens1: &[Token], tokens2: &[Token]) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let n = tokens1.len();
    let m = tokens2.len();

    // Guard against huge token sizes
    if n == 0 || m == 0 || n * m > 10_000 {
        return (Vec::new(), Vec::new());
    }

    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            if tokens1[i].text == tokens2[j].text {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i][j + 1].max(dp[i + 1][j]);
            }
        }
    }

    let mut i = n;
    let mut j = m;
    let mut matched1 = vec![false; n];
    let mut matched2 = vec![false; m];

    while i > 0 && j > 0 {
        if tokens1[i - 1].text == tokens2[j - 1].text {
            matched1[i - 1] = true;
            matched2[j - 1] = true;
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    let mut ranges1 = Vec::new();
    for (k, matched) in matched1.iter().enumerate() {
        if !matched {
            ranges1.push((tokens1[k].char_start, tokens1[k].char_end));
        }
    }

    let mut ranges2 = Vec::new();
    for (k, matched) in matched2.iter().enumerate() {
        if !matched {
            ranges2.push((tokens2[k].char_start, tokens2[k].char_end));
        }
    }

    (merge_ranges(&ranges1), merge_ranges(&ranges2))
}

fn merge_ranges(ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut merged = Vec::new();
    let mut curr = ranges[0];
    for &r in &ranges[1..] {
        if r.0 == curr.1 {
            curr.1 = r.1;
        } else {
            merged.push(curr);
            curr = r;
        }
    }
    merged.push(curr);
    merged
}

// ---------------- Unified Diff Parser ----------------

pub fn parse_unified_diff(
    diff_text: &str,
    default_path: Option<&str>,
    is_staged: bool,
    default_status: Option<DiffFileStatus>,
) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let lines: Vec<&str> = diff_text.lines().collect();
    if lines.is_empty() {
        return files;
    }

    let mut line_idx = 0;
    while line_idx < lines.len() {
        let cur = lines[line_idx];

        // Check if this starts a file
        if cur.starts_with("diff --git ")
            || cur.starts_with("diff --no-index ")
            || (cur.starts_with("--- ") && line_idx + 1 < lines.len() && lines[line_idx + 1].starts_with("+++ "))
        {
            let mut file_path = default_path.unwrap_or("unknown").to_string();
            let mut old_path = file_path.clone();
            let mut status = default_status.unwrap_or(DiffFileStatus::Modified);

            let _header_start = line_idx;
            let mut header_lines = Vec::new();

            while line_idx < lines.len() && !lines[line_idx].starts_with("@@ ") {
                let l = lines[line_idx];
                header_lines.push(l);

                if l.starts_with("diff --git ") {
                    let parts: Vec<&str> = l.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let a_path = parts[2].trim_start_matches("a/");
                        let b_path = parts[3].trim_start_matches("b/");
                        old_path = a_path.to_string();
                        file_path = b_path.to_string();
                    }
                } else if l.starts_with("diff --no-index ") {
                    let parts: Vec<&str> = l.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let p = parts[3].trim();
                        if p != "/dev/null" {
                            file_path = p.to_string();
                            old_path = p.to_string();
                        }
                    }
                    status = DiffFileStatus::Untracked;
                } else if l.starts_with("new file mode") {
                    status = DiffFileStatus::Added;
                } else if l.starts_with("deleted file mode") {
                    status = DiffFileStatus::Deleted;
                } else if l.starts_with("similarity index") || l.starts_with("rename from") {
                    status = DiffFileStatus::Renamed;
                } else if l.starts_with("+++ b/") {
                    file_path = l["+++ b/".len()..].trim().to_string();
                } else if l.starts_with("--- a/") && file_path == "unknown" {
                    file_path = l["--- a/".len()..].trim().to_string();
                }

                line_idx += 1;
            }

            // Minimal patch header for git apply
            let patch_header = format!(
                "diff --git a/{old_path} b/{file_path}\n--- a/{old_path}\n+++ b/{file_path}\n"
            );

            // Now parse hunks for this file
            let mut hunks = Vec::new();
            while line_idx < lines.len() {
                let hl = lines[line_idx];
                if hl.starts_with("diff --git ") || hl.starts_with("diff --no-index ") {
                    break;
                }
                if !hl.starts_with("@@ ") {
                    line_idx += 1;
                    continue;
                }

                // Hunk header: @@ -old_start,old_count +new_start,new_count @@ context
                let header_raw = hl.to_string();
                let (old_start, old_count, new_start, new_count, context_name) = parse_hunk_header(hl);
                line_idx += 1;

                let mut hunk_lines = Vec::new();
                let mut raw_hunk_lines = Vec::new();
                let mut cur_old = old_start;
                let mut cur_new = new_start;

                while line_idx < lines.len() {
                    let dl = lines[line_idx];
                    if dl.starts_with("@@ ")
                        || dl.starts_with("diff --git ")
                        || dl.starts_with("diff --no-index ")
                    {
                        break;
                    }

                    raw_hunk_lines.push(dl);

                    if let Some(rest) = dl.strip_prefix('+') {
                        hunk_lines.push(DiffLine {
                            kind: DiffLineKind::Addition,
                            old_line_no: None,
                            new_line_no: Some(cur_new),
                            text: rest.replace('\t', "    "),
                            intra_highlights: Vec::new(),
                        });
                        cur_new += 1;
                    } else if let Some(rest) = dl.strip_prefix('-') {
                        hunk_lines.push(DiffLine {
                            kind: DiffLineKind::Deletion,
                            old_line_no: Some(cur_old),
                            new_line_no: None,
                            text: rest.replace('\t', "    "),
                            intra_highlights: Vec::new(),
                        });
                        cur_old += 1;
                    } else if let Some(rest) = dl.strip_prefix(' ') {
                        hunk_lines.push(DiffLine {
                            kind: DiffLineKind::Context,
                            old_line_no: Some(cur_old),
                            new_line_no: Some(cur_new),
                            text: rest.replace('\t', "    "),
                            intra_highlights: Vec::new(),
                        });
                        cur_old += 1;
                        cur_new += 1;
                    } else if dl.starts_with('\\') {
                        hunk_lines.push(DiffLine {
                            kind: DiffLineKind::NoNewline,
                            old_line_no: None,
                            new_line_no: None,
                            text: dl.to_string(),
                            intra_highlights: Vec::new(),
                        });
                    } else {
                        // Sometimes empty line without leading space
                        hunk_lines.push(DiffLine {
                            kind: DiffLineKind::Context,
                            old_line_no: Some(cur_old),
                            new_line_no: Some(cur_new),
                            text: dl.replace('\t', "    "),
                            intra_highlights: Vec::new(),
                        });
                        cur_old += 1;
                        cur_new += 1;
                    }

                    line_idx += 1;
                }

                // Compute intra-line highlights for consecutive deletion/addition pairs
                compute_hunk_intra_highlights(&mut hunk_lines);

                let raw_patch = format!(
                    "{patch_header}{header_raw}\n{}\n",
                    raw_hunk_lines.join("\n")
                );

                hunks.push(DiffHunk {
                    header_raw,
                    context_name,
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    lines: hunk_lines,
                    raw_patch,
                });
            }

            files.push(DiffFile {
                path: file_path,
                old_path,
                status,
                is_staged,
                hunks,
            });
        } else {
            line_idx += 1;
        }
    }

    files
}

fn parse_hunk_header(header: &str) -> (usize, usize, usize, usize, String) {
    // @@ -o,c +n,c @@ context
    let mut old_start: usize = 1;
    let mut old_count: usize = 1;
    let mut new_start: usize = 1;
    let mut new_count: usize = 1;
    let mut context_name = String::new();

    if let Some(first_at) = header.find("@@") {
        if let Some(second_rel) = header[first_at + 2..].find("@@") {
            let second_at = first_at + 2 + second_rel;
            let middle = header[first_at + 2..second_at].trim();
            context_name = header[second_at + 2..].trim().to_string();

            for part in middle.split_whitespace() {
                if let Some(old_spec) = part.strip_prefix('-') {
                    if let Some((s, c)) = old_spec.split_once(',') {
                        old_start = s.parse().unwrap_or(1);
                        old_count = c.parse().unwrap_or(1);
                    } else {
                        old_start = old_spec.parse().unwrap_or(1);
                        old_count = 1;
                    }
                } else if let Some(new_spec) = part.strip_prefix('+') {
                    if let Some((s, c)) = new_spec.split_once(',') {
                        new_start = s.parse().unwrap_or(1);
                        new_count = c.parse().unwrap_or(1);
                    } else {
                        new_start = new_spec.parse().unwrap_or(1);
                        new_count = 1;
                    }
                }
            }
        }
    }

    if context_name.is_empty() {
        context_name = if new_count <= 1 {
            format!("Line {new_start}")
        } else {
            format!("Lines {new_start}..{}", new_start + new_count.saturating_sub(1))
        };
    }

    (old_start, old_count, new_start, new_count, context_name)
}

fn compute_hunk_intra_highlights(lines: &mut [DiffLine]) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == DiffLineKind::Deletion {
            let del_start = i;
            while i < lines.len() && lines[i].kind == DiffLineKind::Deletion {
                i += 1;
            }
            let del_end = i;

            let add_start = i;
            while i < lines.len() && lines[i].kind == DiffLineKind::Addition {
                i += 1;
            }
            let add_end = i;

            let del_count = del_end - del_start;
            let add_count = add_end - add_start;

            if del_count > 0 && add_count > 0 {
                let pairs = del_count.min(add_count);
                for p in 0..pairs {
                    let del_idx = del_start + p;
                    let add_idx = add_start + p;

                    let t1 = tokenize(&lines[del_idx].text);
                    let t2 = tokenize(&lines[add_idx].text);
                    let (h1, h2) = compute_token_lcs(&t1, &t2);
                    lines[del_idx].intra_highlights = h1;
                    lines[add_idx].intra_highlights = h2;
                }
            }
        } else {
            i += 1;
        }
    }
}

// ---------------- Sublime Merge Styling & UI Rendering ----------------

pub struct DiffTheme {
    pub bg_color: Color32,
    pub gutter_bg: Color32,
    pub gutter_border: Color32,
    pub gutter_text_context: Color32,
    pub gutter_text_del: Color32,
    pub gutter_text_add: Color32,

    pub line_bg_del: Color32,
    pub line_hl_del: Color32,
    pub line_text_del: Color32,

    pub line_bg_add: Color32,
    pub line_hl_add: Color32,
    pub line_text_add: Color32,

    pub line_text_context: Color32,

    pub file_header_bg: Color32,
    pub file_header_border: Color32,
    pub file_text_dir: Color32,
    pub file_text_name: Color32,

    pub hunk_header_bg: Color32,
    pub hunk_header_border: Color32,
    pub hunk_text_context: Color32,

    pub btn_bg: Color32,
    pub btn_border: Color32,
    pub btn_text: Color32,
}

impl DiffTheme {
    pub fn dark() -> Self {
        Self {
            bg_color: Color32::from_rgb(18, 20, 26),
            gutter_bg: Color32::from_rgb(18, 20, 26),
            gutter_border: Color32::from_rgb(38, 44, 58),
            gutter_text_context: Color32::from_rgb(100, 110, 128),
            gutter_text_del: Color32::from_rgb(195, 110, 118),
            gutter_text_add: Color32::from_rgb(115, 195, 125),

            line_bg_del: Color32::from_rgb(52, 24, 28),
            line_hl_del: Color32::from_rgb(95, 32, 38),
            line_text_del: Color32::from_rgb(255, 185, 185),

            line_bg_add: Color32::from_rgb(22, 48, 30),
            line_hl_add: Color32::from_rgb(32, 88, 48),
            line_text_add: Color32::from_rgb(180, 240, 195),

            line_text_context: Color32::from_rgb(215, 222, 235),

            file_header_bg: Color32::from_rgb(26, 30, 42),
            file_header_border: Color32::from_rgb(45, 52, 70),
            file_text_dir: Color32::from_rgb(130, 145, 168),
            file_text_name: Color32::from_rgb(240, 245, 255),

            hunk_header_bg: Color32::from_rgb(23, 27, 37),
            hunk_header_border: Color32::from_rgb(38, 45, 62),
            hunk_text_context: Color32::from_rgb(125, 185, 240),

            btn_bg: Color32::from_rgb(36, 42, 56),
            btn_border: Color32::from_rgb(58, 68, 88),
            btn_text: Color32::from_rgb(220, 228, 242),
        }
    }

    pub fn light() -> Self {
        Self {
            bg_color: Color32::from_rgb(255, 255, 255),
            gutter_bg: Color32::from_rgb(248, 249, 251),
            gutter_border: Color32::from_rgb(222, 226, 234),
            gutter_text_context: Color32::from_rgb(150, 158, 172),
            gutter_text_del: Color32::from_rgb(205, 85, 85),
            gutter_text_add: Color32::from_rgb(45, 155, 65),

            line_bg_del: Color32::from_rgb(253, 232, 232),
            line_hl_del: Color32::from_rgb(246, 196, 196),
            line_text_del: Color32::from_rgb(165, 30, 30),

            line_bg_add: Color32::from_rgb(230, 249, 230),
            line_hl_add: Color32::from_rgb(192, 238, 192),
            line_text_add: Color32::from_rgb(25, 125, 45),

            line_text_context: Color32::from_rgb(38, 42, 52),

            file_header_bg: Color32::from_rgb(235, 239, 246),
            file_header_border: Color32::from_rgb(210, 218, 230),
            file_text_dir: Color32::from_rgb(115, 125, 140),
            file_text_name: Color32::from_rgb(25, 30, 42),

            hunk_header_bg: Color32::from_rgb(242, 245, 250),
            hunk_header_border: Color32::from_rgb(220, 226, 236),
            hunk_text_context: Color32::from_rgb(30, 95, 180),

            btn_bg: Color32::from_rgb(225, 230, 238),
            btn_border: Color32::from_rgb(195, 202, 215),
            btn_text: Color32::from_rgb(45, 52, 65),
        }
    }
}

pub fn render_sublime_button(
    ui: &mut egui::Ui,
    theme: &DiffTheme,
    text: &str,
    hover_tooltip: &str,
) -> egui::Response {
    let btn = egui::Button::new(
        egui::RichText::new(text)
            .size(11.0)
            .color(theme.btn_text),
    )
    .fill(theme.btn_bg)
    .stroke(Stroke::new(1.0, theme.btn_border))
    .corner_radius(CornerRadius::same(4))
    .min_size(Vec2::new(0.0, 21.0));

    let mut resp = ui.add(btn);
    if !hover_tooltip.is_empty() {
        resp = resp.on_hover_text(hover_tooltip);
    }
    resp
}

pub fn render_status_badge(ui: &mut egui::Ui, status: DiffFileStatus) {
    let badge_color = status.badge_color();
    let badge_letter = status.badge_letter();

    let (rect, _) = ui.allocate_exact_size(Vec2::new(16.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(3), badge_color);

    let font_id = FontId::proportional(10.5);
    let galley = ui.painter().layout_no_wrap(badge_letter.to_string(), font_id, Color32::WHITE);
    let text_pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(text_pos, galley, Color32::WHITE);
}

pub fn render_diff_view(
    ui: &mut egui::Ui,
    theme: &DiffTheme,
    files: &[DiffFile],
    collapsed_files: &HashSet<String>,
    wrap_lines: bool,
    actions: &mut Vec<DiffAction>,
) {
    if files.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.label(
                egui::RichText::new("Working directory clean. No changes detected.")
                    .size(13.0)
                    .color(theme.gutter_text_context),
            );
        });
        return;
    }

    // 1. Calculate global line number digits across ALL files and hunks
    let global_max_line = files
        .iter()
        .flat_map(|f| &f.hunks)
        .flat_map(|h| &h.lines)
        .filter_map(|l| l.old_line_no.max(l.new_line_no))
        .max()
        .unwrap_or(1);
    let digits = global_max_line.to_string().len().max(3);
    let col_width = (digits as f32 * 7.5 + 10.0).max(32.0);
    let gutter_width = col_width * 2.0 + 10.0;

    let font_id = FontId::monospace(12.0);
    let char_width = 7.3;
    let row_min_height = 20.0;

    // 2. Calculate uniform content width across ALL files and hunks
    let avail_w = ui.available_width();
    let content_width = if wrap_lines {
        avail_w.max(300.0)
    } else {
        let max_code_chars = files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(40);
        let longest_line_width = gutter_width + (max_code_chars as f32 * char_width) + 80.0;
        longest_line_width.max(avail_w)
    };

    ui.set_min_width(content_width);

    for (file_idx, file) in files.iter().enumerate() {
        let is_collapsed = collapsed_files.contains(&file.path);

        if file_idx > 0 {
            ui.add_space(12.0);
        }

        // --- FILE HEADER BAR ---
        egui::Frame::new()
            .fill(theme.file_header_bg)
            .stroke(Stroke::new(1.0, theme.file_header_border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                let inner_width = (content_width - 20.0).max(100.0);
                ui.set_min_width(inner_width);
                ui.allocate_ui_with_layout(
                    Vec2::new(inner_width, 24.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        // Caret toggle
                        let caret = if is_collapsed { "▶" } else { "▼" };
                        if ui.button(egui::RichText::new(caret).size(10.0).color(theme.btn_text)).clicked() {
                            actions.push(DiffAction::ToggleCollapse(file.path.clone()));
                        }

                        // Status badge
                        render_status_badge(ui, file.status);

                        // Path
                        let path_str = &file.path;
                        let (dir_part, base_part) = match path_str.rfind('/') {
                            Some(pos) => (&path_str[..=pos], &path_str[pos + 1..]),
                            None => ("", path_str.as_str()),
                        };

                        if !dir_part.is_empty() {
                            ui.label(
                                egui::RichText::new(dir_part)
                                    .size(11.5)
                                    .color(theme.file_text_dir),
                            );
                        }
                        ui.label(
                            egui::RichText::new(base_part)
                                .size(11.5)
                                .strong()
                                .color(theme.file_text_name),
                        );

                        // Right-aligned file actions
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if file.is_staged {
                                if render_sublime_button(ui, theme, "Unstage", "Unstage this file back to working tree").clicked() {
                                    actions.push(DiffAction::UnstageFile(file.path.clone()));
                                }
                            } else if file.status == DiffFileStatus::Untracked {
                                if render_sublime_button(ui, theme, "Stage", "Stage untracked file").clicked() {
                                    actions.push(DiffAction::StageFile(file.path.clone()));
                                }
                                if render_sublime_button(ui, theme, "Delete", "Delete untracked file from disk").clicked() {
                                    actions.push(DiffAction::DiscardFile(file.path.clone(), true));
                                }
                            } else {
                                if render_sublime_button(ui, theme, "Stage", "Stage modified file").clicked() {
                                    actions.push(DiffAction::StageFile(file.path.clone()));
                                }
                                if render_sublime_button(ui, theme, "Discard", "Discard working changes for this file").clicked() {
                                    actions.push(DiffAction::DiscardFile(file.path.clone(), false));
                                }
                            }
                        });
                    },
                );
            });

        if is_collapsed {
            continue;
        }

        ui.add_space(3.0);

        // Render each hunk
        for hunk in &file.hunks {
            ui.add_space(4.0);

            // --- HUNK HEADER BAR ---
            egui::Frame::new()
                .fill(theme.hunk_header_bg)
                .stroke(Stroke::new(1.0, theme.hunk_header_border))
                .inner_margin(Margin::symmetric(10, 4))
                .corner_radius(CornerRadius::same(4))
                .show(ui, |ui| {
                    let inner_width = (content_width - 20.0).max(100.0);
                    ui.set_min_width(inner_width);
                    ui.allocate_ui_with_layout(
                        Vec2::new(inner_width, 22.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(&hunk.context_name)
                                    .size(11.0)
                                    .strong()
                                    .color(theme.hunk_text_context),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if file.is_staged {
                                    if render_sublime_button(ui, theme, "Unstage Hunk", "Unstage this specific hunk from git index").clicked() {
                                        actions.push(DiffAction::UnstageHunk(hunk.raw_patch.clone()));
                                    }
                                } else {
                                    if render_sublime_button(ui, theme, "Stage Hunk", "Stage this specific hunk into git index").clicked() {
                                        actions.push(DiffAction::StageHunk(hunk.raw_patch.clone()));
                                    }
                                    if render_sublime_button(ui, theme, "Discard Hunk", "Discard changes in this specific hunk").clicked() {
                                        actions.push(DiffAction::DiscardHunk(hunk.raw_patch.clone()));
                                    }
                                }
                            });
                        },
                    );
                });

            // --- HUNK CODE LINES ---
            for line in &hunk.lines {
                let (row_bg, text_color) = match line.kind {
                    DiffLineKind::Addition => (theme.line_bg_add, theme.line_text_add),
                    DiffLineKind::Deletion => (theme.line_bg_del, theme.line_text_del),
                    DiffLineKind::Context => (theme.bg_color, theme.line_text_context),
                    DiffLineKind::NoNewline => (theme.bg_color, theme.gutter_text_context),
                };

                let (galley, line_height) = if line.text.is_empty() {
                    (None, row_min_height)
                } else if wrap_lines {
                    let wrap_width = (content_width - gutter_width - 24.0).max(60.0);
                    let g = ui.painter().layout(line.text.clone(), font_id.clone(), text_color, wrap_width);
                    let h = (g.size().y + 5.0).max(row_min_height);
                    (Some(g), h)
                } else {
                    let g = ui.painter().layout_no_wrap(line.text.clone(), font_id.clone(), text_color);
                    (Some(g), row_min_height)
                };

                let (rect, _) = ui.allocate_exact_size(Vec2::new(content_width, line_height), egui::Sense::hover());

                // 1. Full-width row background
                ui.painter().rect_filled(rect, CornerRadius::ZERO, row_bg);

                // 2. Gutter background & border
                let gutter_rect = Rect::from_min_max(
                    rect.min,
                    egui::pos2(rect.min.x + gutter_width, rect.max.y),
                );
                ui.painter().line_segment(
                    [gutter_rect.right_top(), gutter_rect.right_bottom()],
                    Stroke::new(1.0, theme.gutter_border),
                );

                // 3. Old line number (Col 1)
                let num_y = rect.min.y + 2.5;
                if let Some(old_no) = line.old_line_no {
                    let old_str = old_no.to_string();
                    let num_color = if line.kind == DiffLineKind::Deletion {
                        theme.gutter_text_del
                    } else {
                        theme.gutter_text_context
                    };
                    let g_old = ui.painter().layout_no_wrap(old_str, font_id.clone(), num_color);
                    let x_pos = gutter_rect.left() + col_width - g_old.size().x;
                    ui.painter().galley(egui::pos2(x_pos, num_y), g_old, num_color);
                }

                // 4. New line number (Col 2)
                if let Some(new_no) = line.new_line_no {
                    let new_str = new_no.to_string();
                    let num_color = if line.kind == DiffLineKind::Addition {
                        theme.gutter_text_add
                    } else {
                        theme.gutter_text_context
                    };
                    let g_new = ui.painter().layout_no_wrap(new_str, font_id.clone(), num_color);
                    let x_pos = gutter_rect.left() + (col_width * 2.0) - g_new.size().x;
                    ui.painter().galley(egui::pos2(x_pos, num_y), g_new, num_color);
                }

                // 5. Code text & Intra-line Highlights
                let code_x = gutter_rect.right() + 8.0;
                let code_pos = egui::pos2(code_x, rect.min.y + 2.5);

                if let Some(ref g) = galley {
                    // Paint intra-line highlight rects behind text
                    let hl_bg = match line.kind {
                        DiffLineKind::Addition => Some(theme.line_hl_add),
                        DiffLineKind::Deletion => Some(theme.line_hl_del),
                        _ => None,
                    };

                    if let Some(hl_color) = hl_bg {
                        for &(char_start, char_end) in &line.intra_highlights {
                            if char_start < char_end {
                                let r_start = g.pos_from_cursor(egui::text::CCursor::new(char_start));
                                let r_end = g.pos_from_cursor(egui::text::CCursor::new(char_end));

                                if (r_end.min.y - r_start.min.y).abs() < 4.0 {
                                    let y_top = (code_pos.y + r_start.min.y - 1.0).max(rect.min.y + 1.0);
                                    let y_bot = (code_pos.y + r_start.max.y + 1.0).min(rect.max.y - 1.0);
                                    let hl_rect = Rect::from_min_max(
                                        egui::pos2(code_x + r_start.min.x, y_top),
                                        egui::pos2(code_x + r_end.min.x, y_bot),
                                    );
                                    ui.painter().rect_filled(hl_rect, CornerRadius::same(2), hl_color);
                                } else {
                                    let y1_top = (code_pos.y + r_start.min.y - 1.0).max(rect.min.y + 1.0);
                                    let y1_bot = (code_pos.y + r_start.max.y + 1.0).min(rect.max.y - 1.0);
                                    let hl_rect1 = Rect::from_min_max(
                                        egui::pos2(code_x + r_start.min.x, y1_top),
                                        egui::pos2(code_x + g.size().x, y1_bot),
                                    );
                                    let y2_top = (code_pos.y + r_end.min.y - 1.0).max(rect.min.y + 1.0);
                                    let y2_bot = (code_pos.y + r_end.max.y + 1.0).min(rect.max.y - 1.0);
                                    let hl_rect2 = Rect::from_min_max(
                                        egui::pos2(code_x, y2_top),
                                        egui::pos2(code_x + r_end.min.x, y2_bot),
                                    );
                                    ui.painter().rect_filled(hl_rect1, CornerRadius::same(2), hl_color);
                                    ui.painter().rect_filled(hl_rect2, CornerRadius::same(2), hl_color);
                                }
                            }
                        }
                    }

                    ui.painter().galley(code_pos, g.clone(), text_color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sublime_merge_sample_diff() {
        let diff = r#"diff --git a/app/application/use_cases/agents_v2.py b/app/application/use_cases/agents_v2.py
index 1234567..89abcdef 100644
--- a/app/application/use_cases/agents_v2.py
+++ b/app/application/use_cases/agents_v2.py
@@ -1,7 +1,7 @@
 from typing import AsyncGenerator
 from app.domain.models.request import AgentRequest
 from app.application.interfaces.i_llm_client import ILLMClient
-from app.registry.workspaces import WORKSPACE_REGISTRY
+# from app.registry.workspaces import WORKSPACE_REGISTRY
 
 class RouteAgentRequestUseCaseV2:
     def __init__(self, llm_client: ILLMClient):
@@ -12,8 +12,11 @@ execute:
         raise PermissionError(f"Access denied for model {request.model}")
 
         # Resolve workspace config if needed
-        workspace_config = WORKSPACE_REGISTRY.get(request.workspace, WORKSPACE_REGISTRY.get('default'))
-        # workspace_config = WORKSPACE_REGISTRY.get(request.workspace, WORKSPACE_REGISTRY.get('default'))
+        workspace_config = WORKSPACE_REGISTRY.get(request.workspace, WORKSPACE_REGISTRY.get('default'))
+        # workspace_config = WORKSPACE_REGISTRY.get(request.workspace, WORKSPACE_REGISTRY.get('default'))
 
         return self.llm_client.stream_chat_completion(request)
"#;
        let files = parse_unified_diff(diff, None, false, None);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "app/application/use_cases/agents_v2.py");
        assert_eq!(f.status, DiffFileStatus::Modified);
        assert_eq!(f.hunks.len(), 2);

        // Check Hunk 1
        assert_eq!(f.hunks[0].old_start, 1);
        assert_eq!(f.hunks[0].new_start, 1);
        assert_eq!(f.hunks[0].lines[3].kind, DiffLineKind::Deletion);
        assert_eq!(f.hunks[0].lines[3].old_line_no, Some(4));
        assert_eq!(f.hunks[0].lines[3].new_line_no, None);
        assert_eq!(f.hunks[0].lines[4].kind, DiffLineKind::Addition);
        assert_eq!(f.hunks[0].lines[4].old_line_no, None);
        assert_eq!(f.hunks[0].lines[4].new_line_no, Some(4));

        // Check intra-line highlight on Hunk 1
        assert!(!f.hunks[0].lines[4].intra_highlights.is_empty());

        // Check Hunk 2
        assert_eq!(f.hunks[1].context_name, "execute:");
        assert_eq!(f.hunks[1].old_start, 12);
        assert_eq!(f.hunks[1].new_start, 12);
    }

    #[test]
    fn test_parse_untracked_diff() {
        let diff = r#"diff --no-index /dev/null new_file.txt
--- /dev/null
+++ new_file.txt
@@ -0,0 +1,3 @@
+hello
+world
+foo
"#;
        let files = parse_unified_diff(diff, Some("new_file.txt"), false, Some(DiffFileStatus::Untracked));
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, DiffFileStatus::Untracked);
        assert_eq!(f.path, "new_file.txt");
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.hunks[0].lines.len(), 3);
        assert_eq!(f.hunks[0].lines[0].old_line_no, None);
        assert_eq!(f.hunks[0].lines[0].new_line_no, Some(1));
        assert_eq!(f.hunks[0].lines[1].new_line_no, Some(2));
        assert_eq!(f.hunks[0].lines[2].new_line_no, Some(3));
    }

    #[test]
    fn test_parse_multi_file_diff() {
        let diff = r#"diff --git a/file1.txt b/file1.txt
index 1111111..2222222 100644
--- a/file1.txt
+++ b/file1.txt
@@ -1,2 +1,2 @@
-a
+b
 c
diff --git a/file2.txt b/file2.txt
index 3333333..4444444 100644
--- a/file2.txt
+++ b/file2.txt
@@ -1 +1,2 @@
+x
 y
"#;
        let files = parse_unified_diff(diff, None, false, None);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "file1.txt");
        assert_eq!(files[1].path, "file2.txt");
    }

    #[test]
    fn test_parse_large_real_diff() {
        let output = std::process::Command::new("git")
            .args(["diff"])
            .output()
            .expect("git diff failed");
        let text = String::from_utf8_lossy(&output.stdout);
        let files = parse_unified_diff(&text, None, false, None);
        assert!(!files.is_empty());
        for f in &files {
            assert!(!f.path.is_empty());
            for h in &f.hunks {
                assert!(!h.lines.is_empty());
            }
        }
    }


    #[test]
    fn test_hunk_patch_application() {
        let path_buf = std::env::temp_dir().join(format!("lazycommit_test_hunk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path_buf);
        std::fs::create_dir_all(&path_buf).unwrap();
        let path = &path_buf;
        
        let run = |args: &[&str]| {
            let res = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap();
            assert!(res.status.success(), "git command failed: {:?} => {}", args, String::from_utf8_lossy(&res.stderr));
            String::from_utf8_lossy(&res.stdout).into_owned()
        };

        run(&["init"]);
        run(&["config", "user.name", "test"]);
        run(&["config", "user.email", "test@test.com"]);

        let file_path = path.join("demo.txt");
        let initial_text = (0..15).map(|_| "line1\n").collect::<String>() + "line2\n";
        std::fs::write(&file_path, &initial_text).unwrap();
        run(&["add", "demo.txt"]);
        run(&["commit", "-m", "initial"]);

        let modified_text = "line1_mod\n".to_string() + &(0..14).map(|_| "line1\n").collect::<String>() + "line2_mod\n";
        std::fs::write(&file_path, &modified_text).unwrap();

        let diff_str = run(&["diff", "demo.txt"]);
        let files = parse_unified_diff(&diff_str, Some("demo.txt"), false, None);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks.len(), 2);

        // Apply only the first hunk
        let hunk1_patch = &files[0].hunks[0].raw_patch;
        
        use std::io::Write;
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["apply", "--cached", "--recount", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        child.stdin.take().unwrap().write_all(hunk1_patch.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git apply failed: {}", String::from_utf8_lossy(&out.stderr));

        let staged = run(&["diff", "--cached"]);
        assert!(staged.contains("line1_mod"));
        assert!(!staged.contains("line2_mod"));

        let _ = std::fs::remove_dir_all(&path_buf);
    }
}
