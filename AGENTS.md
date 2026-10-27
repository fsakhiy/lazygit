# AGENTS.md

## Project goal

`lazycommit` is a small Rust command-line tool that reads changes from the current Git repository and asks an OpenAI-compatible chat-completions API to write a concise Conventional Commit message. Keep the tool focused on making that workflow quick, predictable, and safe.

## Project layout

- `src/main.rs` contains the CLI argument handling, Git diff collection, prompt construction, API request/response handling, and optional commit execution for `lazycommit`.
- `src/report_main.rs` contains the CLI argument handling, repository discovery, activity collection, and OpenAI request dispatching for cross-project daily reports via `lazyreport`.
- `src/gui_main.rs` and `src/gui.rs` implement the native multi-workspace desktop application (`lazycommit-gui`) with diff inspection and the Daily EOD Report view.
- `src/report.rs` provides git repository discovery, commit log collection, and prompt formatting across repositories.
- `src/lib.rs` provides shared OpenAI chat completions client logic, diff truncation, and report generation routines.
- `Cargo.toml` defines the package and its dependencies; `Cargo.lock` pins dependency versions for builds.
- `README.md` documents installation, configuration, and usage. Keep it consistent with the behavior and defaults in `src/main.rs` and `src/report_main.rs`.

## Behavior to preserve

- Prefer staged changes. If none exist, use unstaged tracked-file changes unless `--staged` was supplied.
- Do not silently include untracked files; tell the user to stage them first.
- Print the generated message by default. Run `git commit` only when the user passes `--commit`.
- When committing unstaged changes, preserve the existing `git commit -a` behavior; staged changes should be committed as staged.
- Support OpenAI-compatible endpoints through `OPENAI_BASE_URL`, and honor CLI/environment configuration for model and temperature.
- Keep prompts focused on a concise Conventional Commit message, with no surrounding Markdown or quotation marks.
- Large diffs may be truncated before sending. Keep the limit safe for UTF-8 and describe token estimates as approximate; tokenization varies by content and model context limits.

## Implementation guidance

- Keep the code and dependencies simple; this is a lightweight CLI, not a general Git client or framework.
- Make failures actionable and write diagnostics to stderr; reserve stdout for the generated commit message and normal success output.
- Avoid logging API keys, authorization headers, or other secrets.
- Treat Git data and API responses as potentially large or malformed, and handle errors without panicking where practical.
- If changing flags, defaults, environment variables, or user-visible behavior, update `print_help()` and the relevant README sections together.
- There are currently no dedicated test files. For changes, use focused checks appropriate to the scope, such as formatting or compiling the Rust crate; add tests when introducing behavior that benefits from isolated coverage.

