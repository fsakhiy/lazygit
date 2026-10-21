use serde::{Deserialize, Serialize};
use std::env;
use std::process::{Command, exit};

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageResponse,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Option<Vec<ChatChoice>>,
    error: Option<ApiErrorResponse>,
}

#[derive(Deserialize)]
struct ApiErrorResponse {
    message: String,
}

fn print_help() {
    println!(
        "commit-generator: Generate git commit messages using OpenAI API\n\n\
USAGE:\n    \
commit-generator [OPTIONS]\n\n\
OPTIONS:\n    \
-m, --model <MODEL>    Specify OpenAI model (default: env OPENAI_MODEL or 'gpt-4o-mini')\n    \
-s, --staged           Inspect staged changes only\n    \
-h, --help             Print help information\n    \
-v, --version          Print version information\n\n\
ENVIRONMENT VARIABLES:\n    \
OPENAI_API_KEY         Your OpenAI API key (required)\n    \
OPENAI_MODEL           Default model to use (optional, default: 'gpt-4o-mini')\n    \
OPENAI_BASE_URL        Custom API base URL (optional, default: 'https://api.openai.com/v1')"
    );
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

fn get_git_diff(staged_only: bool) -> Result<String, String> {
    // Check if in git repository
    run_git(&["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| "Current directory is not inside a git repository.".to_string())?;

    // Try staged diff first
    let staged_diff = run_git(&["diff", "--cached"])?;
    if !staged_diff.trim().is_empty() {
        return Ok(staged_diff);
    }

    if staged_only {
        return Err("No staged changes found. Use 'git add' to stage changes or omit --staged.".to_string());
    }

    // Fall back to unstaged diff
    let unstaged_diff = run_git(&["diff"])?;
    if !unstaged_diff.trim().is_empty() {
        eprintln!("Note: No staged changes found, using unstaged diff.");
        return Ok(unstaged_diff);
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
    let mut staged_only = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "-v" | "--version" => {
                println!("commit-generator {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-s" | "--staged" => {
                staged_only = true;
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
            unknown => {
                eprintln!("Error: Unknown option '{}'. Run with --help for usage.", unknown);
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
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    let base_url = env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
        .trim_end_matches('/')
        .to_string();

    let diff = match get_git_diff(staged_only) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {}", e);
            exit(1);
        }
    };

    // Truncate diff if excessively large (~30,000 characters)
    let max_len = 30_000;
    let prepared_diff = if diff.len() > max_len {
        eprintln!("Warning: Git diff is very large; truncating to 30,000 characters.");
        let mut truncated = diff[..max_len].to_string();
        truncated.push_str("\n\n[...diff truncated due to size limit...]");
        truncated
    } else {
        diff
    };

    let system_prompt = "\
You are an expert developer assistant specialized in creating git commit messages.
Generate a concise, high-quality git commit message summarizing the provided git diff.
Follow the Conventional Commits specification (e.g. feat:, fix:, chore:, refactor:, docs:).
Rules:
- Provide only the commit message.
- Do NOT wrap the message in markdown code blocks or quotes.
- Keep the summary line under 72 characters if possible.
- If multiple distinct changes are made, you may include an optional short bulleted body after an empty line.";

    let user_prompt = format!("Generate a commit message for the following git diff:\n\n{}", prepared_diff);

    let request_body = ChatCompletionRequest {
        model,
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_prompt,
            },
        ],
        temperature: 0.2,
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to create HTTP client: {}", e);
            exit(1);
        }
    };

    let endpoint = format!("{}/chat/completions", base_url);

    let response = client
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&request_body)
        .send();

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: Failed to send request to OpenAI API: {}", e);
            exit(1);
        }
    };

    let status = response.status();
    let text = match response.text() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: Failed to read API response: {}", e);
            exit(1);
        }
    };

    if !status.is_success() {
        if let Ok(err_resp) = serde_json::from_str::<ChatCompletionResponse>(&text) {
            if let Some(err) = err_resp.error {
                eprintln!("Error from OpenAI API ({}): {}", status, err.message);
                exit(1);
            }
        }
        eprintln!("Error: OpenAI API request failed with status {}:\n{}", status, text);
        exit(1);
    }

    let parsed: ChatCompletionResponse = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Failed to parse OpenAI response: {}", e);
            exit(1);
        }
    };

    let message = parsed
        .choices
        .as_ref()
        .and_then(|c| c.first())
        .and_then(|choice| choice.message.content.as_ref());

    match message {
        Some(content) => {
            println!("{}", content.trim());
        }
        None => {
            eprintln!("Error: Received empty response from OpenAI.");
            exit(1);
        }
    }
}
