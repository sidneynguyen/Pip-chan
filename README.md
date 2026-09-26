# Pip-chan

Pip-chan is a small, draggable desktop companion for Codex CLI and Claude Code CLI. One overlay serves every local session. When an agent is ready, she says: “Baka! I’m waiting.”

## What is included

- A transparent, always-on-top Tauri overlay with a menu-bar control.
- Idle, thinking, and waiting images that follow agent state signals.
- One local Unix socket at `~/.pip-chan/pip.sock`; no network listener.
- A singleton app: repeated `signal` invocations reach the running overlay.
- `configure codex` installs user-level Codex hooks and a `notify` command.
- `configure claude` merges Claude state hooks into global settings.
- Config files are backed up as `*.pip-chan.bak` before their first change.
- Pip-chan stores only its window position and an existing Codex notify command that it forwards. It does not retain prompts, code, or terminal output.

## Development

Install Rust, Node.js 20 or newer, and the platform dependencies for Tauri. Then:

```sh
npm install
npm run tauri dev
```

To send a test event to a running release build:

```sh
src-tauri/target/release/bundle/macos/Pip-chan.app/Contents/MacOS/pip-chan \
  signal --source test --event thinking

src-tauri/target/release/bundle/macos/Pip-chan.app/Contents/MacOS/pip-chan \
  signal --source test --event ready

src-tauri/target/release/bundle/macos/Pip-chan.app/Contents/MacOS/pip-chan \
  signal --source test --event idle
```

## Configure integrations

Run these against the built Pip-chan executable:

```sh
src-tauri/target/release/bundle/macos/Pip-chan.app/Contents/MacOS/pip-chan configure codex
src-tauri/target/release/bundle/macos/Pip-chan.app/Contents/MacOS/pip-chan configure claude
```

Codex uses `UserPromptSubmit` and `PermissionRequest` hooks for thinking and approval events. It uses its user-level `notify` setting for ready events when a turn completes. Pip-chan forwards a pre-existing notify command after sending its own event. After configuration, open `/hooks` in Codex and trust the Pip-chan hooks. Claude Code gets a `UserPromptSubmit` hook for thinking events, a `Stop` hook for ready events, and a `Notification` hook for delayed permission requests.

## Build and install on macOS

```sh
npm install
npm run tauri build
./scripts/install.sh
```

The installer replaces `Pip-chan.app` in `~/Applications`, configures both CLIs, and does not require administrator access. Launch Pip-chan from `~/Applications` after installation. It changes only user-level files under `~/Applications`, `~/.pip-chan`, `~/.codex`, and `~/.claude`.

## Linux later

The socket protocol and Tauri UI are portable. Linux needs WebKitGTK and desktop integration packages to build. X11 supports the intended overlay behavior most consistently; Wayland compositors may refuse always-on-top placement.

## Asset

`src/assets/pip-idle.png`, `src/assets/pip-thinking.png`, and `src/assets/pip-ready.png` were generated with OpenAI image generation for this project. The ready state uses a brighter glow and motion treatment in the overlay.
