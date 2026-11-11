# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`lazycommit` is a small Rust command-line tool that reads changes from the current Git repository and asks an OpenAI-compatible chat-completions API to write a concise Conventional Commit message. Keep the tool focused on making that workflow quick, predictable, and safe. The repository ships that CLI plus two companion binaries: `lazyreport` (cross-project daily EOD reports) and `lazycommit-gui` (native multi-workspace desktop app).

## Common commands

```bash
cargo build                    # Build all binaries
cargo build --release          # Optimized build
cargo test                     # Run the unit tests (in src/lib.rs and src/report.rs)
cargo fmt                      # Format the code
cargo clippy --all-targets     # Lint (keep warnings clean)

# Run a single test by name filter
cargo test test_build_daily_report_prompts_truncated

# Run the binaries
cargo run --bin lazycommit -- --help
cargo run --bin lazyreport -- --raw .
cargo run --bin lazycommit-gui          # Desktop app; pass a directory to open it directly
```

## Project layout

- `src/main.rs` — CLI argument handling, Git diff collection, prompt construction, API request/response handling, and optional commit execution for `lazycommit`.
- `src/report_main.rs` — CLI argument handling, repository discovery, activity collection, and OpenAI request dispatching for cross-project daily reports via `lazyreport`.
- `src/gui_main.rs` and `src/gui.rs` — the native multi-workspace desktop application (`lazycommit-gui`) with diff inspection and the Daily EOD Report view. `gui_main.rs` validates the optional project-directory argument (must resolve to a Git work tree via `git rev-parse --show-toplevel`); `gui.rs` holds the egui/eframe app, per-tab project state, staging operations, hunk actions, and session persistence (`~/.config/lazycommit/session.json`).
- `src/report.rs` — Git repository discovery (max depth 3, skipping hidden dirs, `node_modules`, `target`, `vendor`, `dist`, `build`, `.cargo`, `venv`, `__pycache__`), commit log collection, and prompt formatting across repositories.
- `src/lib.rs` — shared OpenAI chat completions client logic (`send_chat_completion`), diff truncation (`prepare_diff`), commit-message generation (`generate_commit_message`), and report prompt/generation routines (`build_daily_report_prompts`, `generate_daily_report`).
- `src/diff_view.rs` — the Sublime Merge-style diff viewer used by the GUI (two-column line-number gutter, intra-line token highlighting, hunk actions).
- `Cargo.toml` defines the package and its dependencies; `Cargo.lock` pins dependency versions for builds.
- `README.md` documents installation, configuration, and usage. Keep it consistent with the behavior and defaults in `src/main.rs` and `src/report_main.rs`.
- `.github/workflows/release.yml` — on `v*` tag push, builds and publishes static Linux (musl), Windows, and macOS (Intel + Apple Silicon) binaries.

## Configuration (all three binaries)

- `OPENAI_API_KEY` — required (except `lazyreport --raw`, which skips the API).
- `OPENAI_MODEL` — default model `gpt-6-luna` (overridable with `-m`/`--model`).
- `OPENAI_TEMPERATURE` — sampling temperature (overridable with `-t`/`--temperature`).
- `OPENAI_BASE_URL` — custom OpenAI-compatible endpoint (default `https://api.openai.com/v1`).

`lazyreport` extras: `--since <TIME>` (default `midnight`), `--author <AUTHOR>` (default: `git config user.email`, falling back to `user.name`), `--all-authors`, `--raw` (print raw aggregated commits without calling the API).

## Behavior to preserve

- Prefer staged changes. If none exist, use unstaged tracked-file changes unless `--staged` was supplied.
- Do not silently include untracked files; tell the user to stage them first.
- Print the generated message by default. Run `git commit` only when the user passes `--commit`.
- When committing unstaged changes, preserve the existing `git commit -a` behavior; staged changes should be committed as staged.
- Support OpenAI-compatible endpoints through `OPENAI_BASE_URL`, and honor CLI/environment configuration for model and temperature.
- Keep prompts focused on a concise Conventional Commit message, with no surrounding Markdown or quotation marks.
- Daily reports are a single unified, consolidated, buzzword-free technical bullet list (no executive summary, no per-project sections); do not regress to the old management-oriented report format.
- Large diffs may be truncated before sending. Keep the limit safe for UTF-8 and describe token estimates as approximate; tokenization varies by content and model context limits.

## Implementation guidance

- Keep the code and dependencies simple; this is a lightweight CLI, not a general Git client or framework.
- Make failures actionable and write diagnostics to stderr; reserve stdout for the generated commit message and normal success output.
- Avoid logging API keys, authorization headers, or other secrets.
- Treat Git data and API responses as potentially large or malformed, and handle errors without panicking where practical.
- If changing flags, defaults, environment variables, or user-visible behavior, update `print_help()` and the relevant README sections together.
- Unit tests live inline in `src/lib.rs` and `src/report.rs` (`#[cfg(test)]` modules) rather than in dedicated test files. For changes, use focused checks appropriate to the scope, such as formatting or compiling the Rust crate; add tests when introducing behavior that benefits from isolated coverage.
- When committing, end the commit message with: `Co-Authored-By: Claude Code <noreply@anthropic.com>`.
