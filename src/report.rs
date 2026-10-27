use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct RepoActivity {
    pub name: String,
    pub path: PathBuf,
    pub raw_log: String,
    pub commit_count: usize,
}

fn run_git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run 'git {}' in {}: {}", args.join(" "), dir.display(), e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git command failed in {}: {}", dir.display(), err.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Detects the author email or name from git config to default to commits authored by the user.
pub fn detect_git_author(dir: Option<&Path>) -> Option<String> {
    let check = |args: &[&str]| -> Option<String> {
        let mut cmd = Command::new("git");
        if let Some(d) = dir {
            cmd.arg("-C").arg(d);
        }
        cmd.args(args);
        cmd.output().ok().and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            None
        })
    };

    check(&["config", "user.email"]).or_else(|| check(&["config", "user.name"]))
}

/// Recursively discovers git repositories under given root directories up to a max depth.
pub fn find_git_repos(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let default_roots = if roots.is_empty() {
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    } else {
        roots.to_vec()
    };

    for root in default_roots {
        scan_dir(&root, 0, 3, &mut repos);
    }

    repos.sort();
    repos.dedup();
    repos
}

fn scan_dir(dir: &Path, depth: usize, max_depth: usize, repos: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }

    // If this directory itself contains a .git folder or file, consider it a repo and stop descending.
    let git_marker = dir.join(".git");
    if git_marker.exists() {
        if let Ok(canonical) = dir.canonicalize() {
            repos.push(canonical);
        } else {
            repos.push(dir.to_path_buf());
        }
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };

        // Skip hidden directories (except we checked .git above) and heavy dependency/build folders
        if file_name.starts_with('.')
            || file_name == "node_modules"
            || file_name == "target"
            || file_name == "vendor"
            || file_name == "dist"
            || file_name == "build"
            || file_name == ".cargo"
            || file_name == "venv"
            || file_name == ".venv"
            || file_name == "__pycache__"
        {
            continue;
        }

        scan_dir(&path, depth + 1, max_depth, repos);
    }
}

/// Collects committed changes from a git repository for a given time window and author.
pub fn collect_repo_commits(
    repo_path: &Path,
    since: &str,
    author: Option<&str>,
) -> Result<Option<RepoActivity>, String> {
    // Check if repo has any commits at all (avoid fatal error on empty repositories)
    let has_head = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);

    if !has_head {
        return Ok(None);
    }

    let mut args = vec![
        "log",
        "--branches",
        "HEAD",
        "--no-merges",
    ];

    let since_arg = format!("--since={since}");
    args.push(&since_arg);

    let author_arg;
    if let Some(auth) = author.filter(|s| !s.trim().is_empty()) {
        author_arg = format!("--author={auth}");
        args.push(&author_arg);
    }

    // Format commits clearly with commit hash, date, subject, body, and diffstat
    args.push("--pretty=format:=== COMMIT %h ===%nDate: %ad%nAuthor: %an <%ae>%nSubject: %s%n%b");
    args.push("--stat");

    let log_output = match run_git_in(repo_path, &args) {
        Ok(out) => out,
        Err(e) => {
            // If git log fails, return error
            return Err(e);
        }
    };

    let trimmed = log_output.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let commit_count = trimmed.matches("=== COMMIT ").count();
    let name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed-repo")
        .to_string();

    Ok(Some(RepoActivity {
        name,
        path: repo_path.to_path_buf(),
        raw_log: trimmed.to_string(),
        commit_count,
    }))
}

/// Collects commit activity across multiple repositories.
pub fn collect_all_activity(
    repos: &[PathBuf],
    since: &str,
    author: Option<&str>,
) -> Vec<RepoActivity> {
    let mut activities = Vec::new();
    for repo in repos {
        if let Ok(Some(activity)) = collect_repo_commits(repo, since, author) {
            activities.push(activity);
        }
    }
    activities
}

/// Formats the raw collected commit activities into a prompt-ready overview.
pub fn format_activity_prompt(activities: &[RepoActivity], since: &str) -> String {
    if activities.is_empty() {
        return format!("No committed changes found since '{since}'.");
    }

    let mut text = String::new();
    text.push_str(&format!(
        "Git Commit History across {} project(s) (window: '{}'):\n\n",
        activities.len(),
        since
    ));

    for act in activities {
        text.push_str(&format!(
            "========================================\n\
             PROJECT: {}\n\
             PATH: {}\n\
             COMMITS: {}\n\
             ========================================\n\
             {}\n\n",
            act.name,
            act.path.display(),
            act.commit_count,
            act.raw_log
        ));
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_activity_prompt_empty() {
        let prompt = format_activity_prompt(&[], "midnight");
        assert_eq!(prompt, "No committed changes found since 'midnight'.");
    }

    #[test]
    fn test_format_activity_prompt_with_activity() {
        let acts = vec![RepoActivity {
            name: "test-repo".to_string(),
            path: PathBuf::from("/path/to/test-repo"),
            raw_log: "=== COMMIT 123456 ===\nSubject: feat: add daily report".to_string(),
            commit_count: 1,
        }];
        let prompt = format_activity_prompt(&acts, "midnight");
        assert!(prompt.contains("test-repo"));
        assert!(prompt.contains("feat: add daily report"));
        assert!(prompt.contains("COMMITS: 1"));
    }

    #[test]
    fn test_find_git_repos_in_current_repo() {
        let cur = std::env::current_dir().unwrap();
        let repos = find_git_repos(&[cur.clone()]);
        assert!(!repos.is_empty());
        assert!(repos.iter().any(|r| r == &cur || r.ends_with("commit-generator")));
    }
}
