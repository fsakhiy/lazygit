mod gui;

use std::{env, path::PathBuf, process::Command};

fn main() -> eframe::Result {
    let mut args = env::args_os().skip(1);
    let Some(arg) = args.next() else {
        return gui::run(None);
    };
    if args.next().is_some() {
        eprintln!("Usage: lazycommit-gui [PROJECT_DIRECTORY]");
        std::process::exit(2);
    }

    match arg.to_str() {
        Some("-h" | "--help") => {
            println!(
                "lazycommit-gui: native desktop app for reviewing diffs and generating commit messages\n\n\
USAGE:\n    \
lazycommit-gui [PROJECT_DIRECTORY]\n\n\
ARGS:\n    \
PROJECT_DIRECTORY    Optional path to a Git repository to open on launch\n\n\
OPTIONS:\n    \
-h, --help           Print help information\n    \
-v, --version        Print version information"
            );
            return Ok(());
        }
        Some("-v" | "--version") => {
            println!("lazycommit-gui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    let supplied_path = PathBuf::from(arg);
    let path = match supplied_path.canonicalize() {
        Ok(path) if path.is_dir() => path,
        Ok(_) => {
            eprintln!("Error: project path must be a directory.");
            std::process::exit(2);
        }
        Err(error) => {
            eprintln!("Error: cannot open project directory: {error}");
            std::process::exit(2);
        }
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    let root = match output {
        Ok(output) if output.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        }
        Ok(output) => {
            let reason = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "Error: {} is not inside a Git repository: {}",
                path.display(),
                reason.trim()
            );
            std::process::exit(2);
        }
        Err(error) => {
            eprintln!("Error: could not run git: {error}");
            std::process::exit(1);
        }
    };

    gui::run(Some(root))
}
