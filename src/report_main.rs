use lazycommit::{
    collect_all_activity, detect_git_author, find_git_repos, format_activity_prompt,
    generate_daily_report,
};
use std::env;
use std::path::PathBuf;
use std::process::exit;

fn print_help() {
    println!(
        "lazyreport: Generate end-of-day daily reports across git repositories using OpenAI\n\n\
USAGE:\n    \
lazyreport [OPTIONS] [PATHS...]\n\n\
ARGS:\n    \
<PATHS...>             Directories or repositories to scan (default: current directory)\n\n\
OPTIONS:\n    \
--since <TIME>         Git time window for commits (default: 'midnight')\n    \
--author <AUTHOR>      Filter commits by author (default: git config user.email or user.name)\n    \
--all-authors          Include commits from all authors in the repositories\n    \
--raw                  Print raw aggregated git commits without calling OpenAI\n    \
-m, --model <MODEL>    Specify OpenAI model (default: env OPENAI_MODEL or 'gpt-6-luna')\n    \
-t, --temperature <T>  Sampling temperature (optional, default: model default)\n    \
-h, --help             Print help information\n    \
-v, --version          Print version information\n\n\
ENVIRONMENT VARIABLES:\n    \
OPENAI_API_KEY         Your OpenAI API key (required unless --raw is used)\n    \
OPENAI_MODEL           Default model to use (optional, default: 'gpt-6-luna')\n    \
OPENAI_TEMPERATURE     Sampling temperature (optional, default: model default)\n    \
OPENAI_BASE_URL        Custom API base URL (optional, default: 'https://api.openai.com/v1')"
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut model_arg: Option<String> = None;
    let mut temp_arg: Option<f32> = None;
    let mut since_arg: Option<String> = None;
    let mut author_arg: Option<String> = None;
    let mut all_authors = false;
    let mut raw_output = false;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "-v" | "--version" => {
                println!("lazyreport {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--raw" => {
                raw_output = true;
            }
            "--all-authors" => {
                all_authors = true;
            }
            "--since" => {
                if i + 1 < args.len() {
                    since_arg = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: Missing value for --since option");
                    exit(1);
                }
            }
            opt if opt.starts_with("--since=") => {
                since_arg = Some(opt.trim_start_matches("--since=").to_string());
            }
            "--author" => {
                if i + 1 < args.len() {
                    author_arg = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: Missing value for --author option");
                    exit(1);
                }
            }
            opt if opt.starts_with("--author=") => {
                author_arg = Some(opt.trim_start_matches("--author=").to_string());
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
            opt if opt.starts_with("--temperature=") => {
                let val = opt.trim_start_matches("--temperature=");
                match val.parse::<f32>() {
                    Ok(t) => temp_arg = Some(t),
                    Err(_) => {
                        eprintln!("Error: Invalid float value for --temperature");
                        exit(1);
                    }
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
            opt if opt.starts_with("--model=") => {
                model_arg = Some(opt.trim_start_matches("--model=").to_string());
            }
            arg if arg.starts_with('-') => {
                eprintln!(
                    "Error: Unknown option '{}'. Run with --help for usage.",
                    arg
                );
                exit(1);
            }
            positional => {
                paths.push(PathBuf::from(positional));
            }
        }
        i += 1;
    }

    let since = since_arg.unwrap_or_else(|| "midnight".to_string());

    let author_filter = if all_authors {
        None
    } else if let Some(author) = author_arg {
        Some(author)
    } else {
        detect_git_author(None)
    };

    if paths.is_empty() {
        paths.push(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    }

    eprintln!(
        "Scanning for Git repositories in {} location(s)...",
        paths.len()
    );
    let repos = find_git_repos(&paths);
    if repos.is_empty() {
        eprintln!("Error: No Git repositories found in the specified path(s).");
        exit(1);
    }

    eprintln!(
        "Found {} repository(ies). Collecting committed changes since '{}'{}...",
        repos.len(),
        since,
        author_filter
            .as_ref()
            .map(|a| format!(" for author '{}'", a))
            .unwrap_or_default()
    );

    let activities = collect_all_activity(&repos, &since, author_filter.as_deref());

    if activities.is_empty() {
        eprintln!(
            "No commits found since '{}' across the {} repository(ies) scanned.",
            since,
            repos.len()
        );
        return;
    }

    let total_commits: usize = activities.iter().map(|a| a.commit_count).sum();
    eprintln!(
        "Found {} commit(s) across {} project(s) with activity.",
        total_commits,
        activities.len()
    );

    let activity_text = format_activity_prompt(&activities, &since);

    if raw_output {
        println!("{}", activity_text);
        return;
    }

    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(val) if !val.trim().is_empty() => val,
        _ => {
            eprintln!(
                "Error: OPENAI_API_KEY environment variable is not set.\n\
                 Please set it using:\n    \
                 export OPENAI_API_KEY=\"your-openai-api-key\"\n\n\
                 Or run with '--raw' to output raw git commit history without AI."
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

    let temperature = temp_arg.or_else(|| {
        env::var("OPENAI_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
    });

    eprintln!("Generating daily report using model '{}'...", model);
    match generate_daily_report(&activity_text, model, &api_key, &base_url, temperature) {
        Ok(report) => {
            println!("{}", report);
        }
        Err(error) => {
            eprintln!("Error: {}", error);
            exit(1);
        }
    }
}
