pub mod diff_view;
pub mod report;

pub use report::{
    RepoActivity, collect_all_activity, collect_repo_commits, detect_git_author, find_git_repos,
    format_activity_prompt,
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

/// Builds the system and user prompts for daily report generation.
pub fn build_daily_report_prompts(activity: &str) -> (String, String) {
    let (prepared_activity, was_truncated) = prepare_diff(activity);
    let system_prompt = "\
You are a technical assistant generating a concise, factual daily engineering report from git commit logs across one or more projects.
Write a short, highly technical report synthesizing actual changes into a single unified list. Avoid buzzwords, executive summaries, marketing speak, and corporate fluff.

Format Guidelines:
# Daily Report

- Bullet points of technical changes categorized with tags:
  - **feat**: New functionality or capabilities
  - **fix**: Bug fixes and issue resolutions
  - **sec**: Security updates or vulnerability patches
  - **refactor** / **perf** / **chore**: Internal code improvements, optimizations, or maintenance
  - **test**: Test suite additions or updates

Merge & Consolidation Rules:
- One unified output: Do NOT split into separate project headers or sections. Synthesize all selected projects into a single consolidated list.
- Merge related changes: If multiple commits or multiple repositories work on the same feature, API, or bug fix (e.g., frontend and backend changes for team management), combine them into a single cohesive bullet point rather than multiple fragmented entries.
- Contextual references: Mention the relevant project(s) or component(s) concisely where helpful (e.g., 'across dashboard and gateway', or '(gateway)').
- Keep bullets compact, single-line, direct, and factual. Focus on technical specifics (e.g. components, APIs, database migrations, bug causes).
- Do NOT include an executive summary, high-level narrative overview, conclusion, or conversational greeting.
- Strictly avoid corporate buzzwords (e.g., 'streamlined', 'leveraged', 'synergy', 'game-changing', 'elevated').
- Do NOT speculate or invent changes, blockers, or next steps not present in the commit log.";

    let user_prompt = if was_truncated {
        format!(
            "Generate a unified, concise technical daily report merging related changes across today's git commits from all projects (note: commit history was truncated due to length):\n\n{}",
            prepared_activity
        )
    } else {
        format!(
            "Generate a unified, concise technical daily report merging related changes across today's git commits from all projects:\n\n{}",
            prepared_activity
        )
    };

    (system_prompt.to_string(), user_prompt)
}

/// Summarizes committed git history across repositories into a daily end-of-day report.
pub fn generate_daily_report(
    activity: &str,
    model: String,
    api_key: &str,
    base_url: &str,
    temperature: Option<f32>,
) -> Result<String, String> {
    let (system_prompt, user_prompt) = build_daily_report_prompts(activity);

    send_chat_completion(
        vec![
            ChatMessage {
                role: "system".to_owned(),
                content: system_prompt,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_daily_report_prompts_content() {
        let sample_activity = "=== COMMIT 123 ===\nfeat: add login endpoint";
        let (sys, user) = build_daily_report_prompts(sample_activity);

        // Verify focus on technical concise changes without executive summary or buzzwords
        assert!(!sys.contains("Executive Summary"));
        assert!(sys.contains("**feat**"));
        assert!(sys.contains("**fix**"));
        assert!(sys.contains("**sec**"));
        assert!(sys.contains("buzzwords"));
        assert!(sys.contains("Merge related changes"));
        assert!(sys.contains("One unified output"));
        assert!(user.contains("concise technical daily report"));
        assert!(user.contains("feat: add login endpoint"));
    }

    #[test]
    fn test_build_daily_report_prompts_truncated() {
        // Construct activity longer than MAX_CHARS (800_000)
        let long_activity = "a".repeat(850_000);
        let (_sys, user) = build_daily_report_prompts(&long_activity);
        assert!(user.contains("truncated due to length"));
    }
}
