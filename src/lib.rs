pub mod report;

pub use report::{
    collect_all_activity, collect_repo_commits, detect_git_author, find_git_repos,
    format_activity_prompt, RepoActivity,
};

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
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

/// Generic chat completion helper for OpenAI-compatible APIs.
fn send_chat_completion(
    messages: Vec<ChatMessage>,
    model: String,
    api_key: &str,
    base_url: &str,
    temperature: Option<f32>,
    timeout_secs: u64,
) -> Result<String, String> {
    let request_body = ChatCompletionRequest {
        model,
        messages,
        temperature,
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let response = client
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&request_body)
        .send()
        .map_err(|e| format!("Failed to send request to OpenAI API: {e}"))?;

    let status = response.status();
    let text = response
        .text()
        .map_err(|e| format!("Failed to read API response: {e}"))?;
    if !status.is_success() {
        if let Ok(err_resp) = serde_json::from_str::<ChatCompletionResponse>(&text)
            && let Some(err) = err_resp.error
        {
            return Err(format!("OpenAI API error ({status}): {}", err.message));
        }
        return Err(format!(
            "OpenAI API request failed with status {status}:\n{text}"
        ));
    }

    let parsed: ChatCompletionResponse =
        serde_json::from_str(&text).map_err(|e| format!("Failed to parse OpenAI response: {e}"))?;
    parsed
        .choices
        .as_ref()
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.message.content.as_ref())
        .map(|content| content.trim().to_owned())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "Received empty response from OpenAI.".to_owned())
}

/// Keep input within the shared approximate context-size limit, truncating only at UTF-8 boundaries.
pub fn prepare_diff(diff: &str) -> (String, bool) {
    const MAX_CHARS: usize = 800_000;
    if diff.chars().count() > MAX_CHARS {
        let mut truncated: String = diff.chars().take(MAX_CHARS).collect();
        truncated.push_str("\n\n[...diff truncated due to size limit...]");
        (truncated, true)
    } else {
        (diff.to_owned(), false)
    }
}

/// Shared OpenAI-compatible chat-completions flow for the CLI and native GUI to generate commit messages.
pub fn generate_commit_message(
    diff: &str,
    model: String,
    api_key: &str,
    base_url: &str,
    temperature: Option<f32>,
) -> Result<String, String> {
    let (prepared_diff, _) = prepare_diff(diff);
    let system_prompt = "\
You are an expert developer assistant specialized in creating git commit messages.
Generate a concise, high-quality git commit message summarizing the provided git diff.
Follow the Conventional Commits specification (e.g. feat:, fix:, chore:, refactor:, docs:).
Rules:
- Provide only the commit message.
- Do NOT wrap the message in markdown code blocks or quotes.
- Keep the summary line under 72 characters if possible.
- If multiple distinct changes are made, you may include an optional short bulleted body after an empty line.";

    send_chat_completion(
        vec![
            ChatMessage {
                role: "system".to_owned(),
                content: system_prompt.to_owned(),
            },
            ChatMessage {
                role: "user".to_owned(),
                content: format!(
                    "Generate a commit message for the following git diff:\n\n{}",
                    prepared_diff
                ),
            },
        ],
        model,
        api_key,
        base_url,
        temperature,
        45,
    )
}

/// Summarizes committed git history across repositories into a daily end-of-day report.
pub fn generate_daily_report(
    activity: &str,
    model: String,
    api_key: &str,
    base_url: &str,
    temperature: Option<f32>,
) -> Result<String, String> {
    let (prepared_activity, was_truncated) = prepare_diff(activity);
    let system_prompt = "\
You are an engineering assistant helping a software developer prepare a Daily End-of-Day (EOD) Report for their manager/boss.
Review the provided git commit history across projects and write a polished, professional report.

Format Guidelines:
# Daily Engineering Report

## Executive Summary
- 2 to 3 concise bullet points highlighting key deliverables, breakthroughs, or progress achieved.

## Work Completed (Grouped by Project)
### [Project Name]
- Bullet points describing features added, bugs resolved, tests created, or refactoring completed.
- Focus on business and technical value (what was achieved and why), grounded strictly in the commit log.
- Do not invent uncommitted tasks or mention raw git commit SHAs unless necessary.

## Next Steps / Blockers (Optional)
- Brief bullet points on what's next or any blockers if inferable, or omit if not applicable.

Tone: Professional, clear, concise, and easy to scan for management.";

    let user_prompt = if was_truncated {
        format!(
            "Generate my Daily EOD Report from today's git commits across my repositories (note: commit history was truncated due to length):\n\n{}",
            prepared_activity
        )
    } else {
        format!(
            "Generate my Daily EOD Report from today's git commits across my repositories:\n\n{}",
            prepared_activity
        )
    };

    send_chat_completion(
        vec![
            ChatMessage {
                role: "system".to_owned(),
                content: system_prompt.to_owned(),
            },
            ChatMessage {
                role: "user".to_owned(),
                content: user_prompt,
            },
        ],
        model,
        api_key,
        base_url,
        temperature,
        60,
    )
}
