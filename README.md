# commit-generator 🤖

A lightweight, fast Rust CLI tool that reads your `git diff` and uses OpenAI to automatically generate meaningful Conventional Commit messages.

---

## ✨ Features

- **Automated Commit Messages**: Analyzes git changes and infers concise Conventional Commit messages (`feat:`, `fix:`, `refactor:`, `chore:`, etc.).
- **Auto-Commit Option (`-c` / `--commit`)**: Generates the commit message and automatically executes `git commit` in one step.
- **Smart Diff Inspection**:
  - Prioritizes **staged changes** (`git diff --staged`).
  - Automatically falls back to **unstaged modifications** if nothing is staged.
  - Detects untracked files and alerts you to stage them.
- **Fast & Self-Contained**: Built with pure Rust TLS (`rustls`). No OpenSSL runtime dependencies.
- **Customizable LLMs**: Defaults to the fast and inexpensive `gpt-4o-mini`. Easily switch models with `-m` or environment variables.
- **Any OpenAI-Compatible API**: Works with Groq, Ollama, OpenRouter, vLLM, or any local/custom endpoint via `OPENAI_BASE_URL`.

---

## 🚀 Installation

### Option 1: Download Prebuilt Binaries
Download the pre-compiled binary for your operating system from the **[Releases](../../releases)** page:
- **Linux (`musl`)**: Single static binary that works on **Fedora, Arch, Debian, Ubuntu**, and Alpine.
- **Windows**: `commit-generator.exe` (.zip)
- **macOS**: Native builds for both Apple Silicon (M1/M2/M3/M4) and Intel.

Unpack the archive and place the binary into a directory in your `PATH` (such as `~/.local/bin` or `/usr/local/bin`).

### Option 2: Build from Source (using Cargo)
Ensure you have Rust and Cargo installed:
```bash
git clone https://github.com/fsakhiy/lazygit.git
cd lazygit
cargo install --path .
```

---

## ⚙️ Configuration

Set your OpenAI API key as an environment variable:

```bash
export OPENAI_API_KEY="sk-your-openai-api-key-here"
```

To make it permanent, add the export line above to your `~/.bashrc`, `~/.zshrc`, or shell profile.

### Optional Environment Variables

| Variable | Description | Default |
|---|---|---|
| `OPENAI_API_KEY` | Your OpenAI API key (**required**) | None |
| `OPENAI_MODEL` | Default OpenAI model to use | `gpt-4o-mini` |
| `OPENAI_BASE_URL` | Custom API base URL (for Ollama, Groq, etc.) | `https://api.openai.com/v1` |

---

## 💻 Usage

### 1. Generate a commit message
Stage your changes and run:
```bash
git add <files>
commit-generator
```
Output:
```text
feat: add auto-commit flag and multi-platform release workflow
```

### 2. Generate and commit directly (`-c` / `--commit`)
Generate the message and automatically run `git commit` in one go:
```bash
commit-generator -c
```
Output:
```text
Generated commit message:
----------------------------------------
feat: add auto-commit flag and multi-platform release workflow
----------------------------------------
Committing changes...
[main a1b2c3d] feat: add auto-commit flag and multi-platform release workflow
 2 files changed, 45 insertions(+)
Successfully committed!
```

### 3. CLI Options & Flags

```text
USAGE:
    commit-generator [OPTIONS]

OPTIONS:
    -c, --commit           Automatically commit changes with the generated message
    -s, --staged           Inspect staged changes only (do not fall back to unstaged)
    -m, --model <MODEL>    Specify OpenAI model (e.g. gpt-4o, gpt-3.5-turbo)
    -h, --help             Print help information
    -v, --version          Print version information
```

### 4. Git Alias / Workflow Tip

You can create a custom git alias so you can run `git cg`:
```bash
git config --global alias.cg '!commit-generator -c'
```
Now, in any repository, simply run:
```bash
git add .
git cg
```

---

## 📦 How to Create a New Release

This repository is equipped with an automated GitHub Actions workflow (`.github/workflows/release.yml`). Whenever you push a version tag, GitHub will automatically compile binaries for all platforms and publish a release.

### Step-by-Step Release Guide:

1. **Ensure your changes are committed and pushed to `main`**:
   ```bash
   git add .
   git commit -m "chore: bump version to 0.1.0"
   git push origin main
   ```

2. **Create a version tag**:
   Version tags follow the `v*` format (e.g. `v0.1.0`, `v0.1.1`, `v1.0.0`):
   ```bash
   git tag v0.1.0
   ```

3. **Push the tag to GitHub**:
   ```bash
   git push origin v0.1.0
   ```

4. **Watch the automated release**:
   - Navigate to the **Actions** tab on your GitHub repository.
   - GitHub Actions will spin up Ubuntu, Windows, and macOS runners.
   - Once the build completes (~2–3 minutes), the release is published automatically on your repository's **Releases** tab with:
     - `commit-generator-x86_64-linux.tar.gz` (Universal static Linux binary)
     - `commit-generator-x86_64-windows.zip` (Windows executable)
     - `commit-generator-aarch64-macos.tar.gz` (Apple Silicon Mac)
     - `commit-generator-x86_64-macos.tar.gz` (Intel Mac)
     - Auto-generated release notes and changelog.

> [!NOTE]
> If you ever need to delete a tag and recreate it:
> ```bash
> git tag -d v0.1.0
> git push --delete origin v0.1.0
> ```

---

## 📄 License

This project is open-source and available under the [MIT License](LICENSE). You are free to use, modify, redistribute, and commercially use this software.
