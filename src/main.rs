use lazycommit::{generate_commit_message, prepare_diff};
use std::env;
use std::process::{Command, exit};

fn print_help() {
    println!(
        "lazycommit: Generate git commit messages using OpenAI API\n\n\
USAGE:\n    \
lazycommit [OPTIONS]\n\n\
OPTIONS:\n    \
-m, --model <MODEL>    Specify OpenAI model (default: env OPENAI_MODEL or 'gpt-6-luna')\n    \
-t, --temperature <T>  Sampling temperature (optional, default: model default)\n    \
-s, --staged           Inspect staged changes only\n    \
-c, --commit           Automatically commit changes with the generated message\n    \
-h, --help             Print help information\n    \
-v, --version          Print version information\n\n\
ENVIRONMENT VARIABLES:\n    \
OPENAI_API_KEY         Your OpenAI API key (required)\n    \
OPENAI_MODEL           Default model to use (optional, default: 'gpt-6-luna')\n    \
OPENAI_TEMPERATURE     Sampling temperature (optional, default: model default)\n    \
OPENAI_BASE_URL        Custom API base URL (optional, default: 'https://api.openai.com/v1')"
    );
}

struct GitDiffResult {
    diff: String,
    is_staged: bool,
}

fn run_git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run 'git {}': {}", args.join(" "), e))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git command failed: {}", err.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn get_git_diff(staged_only: bool) -> Result<GitDiffResult, String> {
    // Check if in git repository
    run_git(&["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| "Current directory is not inside a git repository.".to_string())?;

    // Try staged diff first
    let staged_diff = run_git(&["diff", "--cached"])?;
    if !staged_diff.trim().is_empty() {
        return Ok(GitDiffResult {
            diff: staged_diff,
            is_staged: true,
        });
    }

    if staged_only {
        return Err(
            "No staged changes found. Use 'git add' to stage changes or omit --staged.".to_string(),
        );
    }

    // Fall back to unstaged diff
    let unstaged_diff = run_git(&["diff"])?;
    if !unstaged_diff.trim().is_empty() {
        eprintln!("Note: No staged changes found, using unstaged diff.");
        return Ok(GitDiffResult {
            diff: unstaged_diff,
            is_staged: false,
        });
    }

    // Check if there are untracked files
    let untracked = run_git(&["status", "--porcelain"])?;
    if !untracked.trim().is_empty() {
        return Err("No changes detected in tracked files. Untracked files exist; run 'git add <files>' first.".to_string());
    }

    Err("No git changes detected in the working tree.".to_string())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut model_arg: Option<String> = None;
    let mut temp_arg: Option<f32> = None;
    let mut staged_only = false;
    let mut auto_commit = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "-v" | "--version" => {
                println!("lazycommit {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-s" | "--staged" => {
                staged_only = true;
            }
            "-c" | "--commit" => {
                auto_commit = true;
            }
            "-t" | "--temperature" => {
                if i + 1 < args.len() {
                    match args[i + 1].parse::<f32>() {
                        Ok(t) => temp_arg = Some(t),
                        Err(_) => {
                            eprintln!("Error: Invalid float value for --temperature");
                            exit(1);
                        }
                    }
                    i += 1;
                } else {
                    eprintln!("Error: Missing value for --temperature option");
                    exit(1);
                }
            }
            "-m" | "--model" => {
                if i + 1 < args.len() {
                    model_arg = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: Missing value for --model option");
                    exit(1);
                }
            }
            "report" | "--report" => {
                eprintln!(
                    "Tip: Daily reports across repositories are generated using the 'lazyreport' command.\n\
                     Run 'lazyreport --help' or 'lazyreport [PATH]'."
                );
                exit(1);
            }
            unknown => {
                eprintln!(
                    "Error: Unknown option '{}'. Run with --help for usage.",
                    unknown
                );
                exit(1);
            }
        }
        i += 1;
    }

    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(val) if !val.trim().is_empty() => val,
        _ => {
            eprintln!(
                "Error: OPENAI_API_KEY environment variable is not set.\n\
                 Please set it using:\n    \
                 export OPENAI_API_KEY=\"your-openai-api-key\""
            );
            exit(1);
        }
    };

    let model = model_arg
        .or_else(|| env::var("OPENAI_MODEL").ok())
        .unwrap_or_else(|| "gpt-6-luna".to_string());

    let base_url = env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
        .trim_end_matches('/')
        .to_string();

    let diff_result = match get_git_diff(staged_only) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {}", e);
            exit(1);
        }
    };

    // Roughly 4 characters per token; tokenization varies by content and model.
    let (_, was_truncated) = prepare_diff(&diff_result.diff);
    if was_truncated {
        eprintln!(
            "Warning: Git diff is very large; truncating to 800,000 characters (~200k tokens)."
        );
    }

    let temperature = temp_arg.or_else(|| {
        env::var("OPENAI_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
    });
    let message =
        match generate_commit_message(&diff_result.diff, model, &api_key, &base_url, temperature) {
            Ok(message) => message,
            Err(error) => {
                eprintln!("Error: {error}");
                exit(1);
            }
        };

    if auto_commit {
        println!(
            "Generated commit message:\n----------------------------------------\n{}\n----------------------------------------",
            message
        );
        println!("Committing changes...");

        let mut git_cmd = Command::new("git");
        git_cmd.arg("commit");
        if !diff_result.is_staged {
            git_cmd.arg("-a");
        }
        git_cmd.args(["-m", &message]);

        let status = git_cmd.status();
        match status {
            Ok(s) if s.success() => {
                println!("Successfully committed!");
            }
            Ok(s) => {
                eprintln!("git commit failed with exit code: {:?}", s.code());
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Error executing 'git commit': {}", e);
                exit(1);
            }
        }
    } else {
        println!("{}", message);
    }
}
