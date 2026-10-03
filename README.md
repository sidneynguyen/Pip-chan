# Pip-chan

Pip-chan is a small, draggable desktop companion for Codex CLI and Claude Code CLI. One overlay serves every local session. When an agent is ready, she says: “Baka! I’m waiting.”

## What is included

- A transparent, always-on-top Tauri overlay with a menu-bar control.
- Idle, thinking, and waiting images that follow agent state signals.
- One local Unix socket at `~/.pip-chan/pip.sock`; no network listener.
- One token-protected remote socket at `~/.pip-chan/remote.sock` for SSH forwarding.
- A singleton app: repeated `signal` invocations reach the running overlay.
- Signal commands exit immediately when the overlay is closed. Hooks never launch the GUI.
- A tray command toggles 20% ghost mode across all agent states. In ghost mode, clicks go through Pip-chan to the window behind her. She is solid and clickable while a ready bubble shows, and she fades again when it closes. Click Pip-chan in the Dock to make her solid again.
- Hover controls provide quick ghost-mode and hide actions.
- Tray commands show or hide the overlay and change its size.
- Water reminders appear at :25 on every even hour. Eye rest reminders appear every hour at :55. If Pip-chan starts or the Mac wakes late, a water reminder still appears for 30 minutes and an eye rest reminder for 15 minutes. Each reminder stays until you click Pip-chan, and the dismissal is saved. In ghost mode, Pip-chan is solid while a reminder shows. A hidden overlay does not open for a reminder. A tray item turns each kind on or off.
- `configure codex` installs user-level Codex hooks and a `notify` command.
- `configure claude` merges Claude state hooks into global settings.
- Config files are backed up as `*.pip-chan.bak` before their first change.
- Pip-chan stores its window position and size, the reminder settings, a remote authentication token, and an existing Codex notify command that it forwards. It does not retain prompts, code, or terminal output.

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

Both CLIs get the same state hooks:

| Hook | Pip-chan state |
|---|---|
| `UserPromptSubmit`, `PostToolUse`, `PreCompact` | thinking |
| `PostCompact` with the `auto` matcher | thinking |
| `PermissionRequest` | waiting for approval |
| `PostCompact` with the `manual` matcher, `SessionEnd` | idle |

Claude Code also gets a `Stop` hook for ready events. Codex uses its user-level `notify` setting for ready events when a turn completes. Pip-chan forwards a pre-existing notify command after sending its own event. After configuration, open `/hooks` in Codex and trust the Pip-chan hooks.

Pip-chan tracks each session separately by the `session_id` in the hook input. She shows the thinking state while any session works. A ready bubble stays until its own session starts again, or for 10 seconds.

## Build and install on macOS

```sh
npm install
npm run tauri build
./scripts/install.sh
```

The installer replaces `Pip-chan.app` in `~/Applications`, configures both CLIs, and does not require administrator access. Launch Pip-chan from `~/Applications` after installation. It changes only user-level files under `~/Applications`, `~/.pip-chan`, `~/.codex`, and `~/.claude`.

## App icon

Put a square PNG or SVG source at `app-icon.png` in the repository root. Then generate the platform icon files:

```sh
npm run tauri icon app-icon.png
```

The command writes the generated icons under `src-tauri/icons`, including the macOS `icon.icns` file. Build the app again after icon generation.

## Remote agents over SSH

Install Pip-chan on your Mac and install the remote helper and user-level hooks:

```sh
./scripts/install.sh --remote my-server
```

The `--remote` option copies `pip-chan-signal` to `~/.local/bin` on the remote server. It updates `~/.codex/hooks.json` and `~/.claude/settings.json` on that server. It creates `*.pip-chan.bak` files before it changes existing hook files. It does not install the app, images, Node.js packages, or Rust packages on the server.

Start Pip-chan on your Mac. Then connect through the SSH helper:

```sh
./scripts/pip-chan-ssh my-server
tmux attach
```

The SSH helper forwards remote loopback port `47821` to the token-protected Pip-chan socket on your Mac. Codex and Claude Code in that SSH session or an attached tmux session send state events through the tunnel. Hook failures do not stop the agent when the tunnel is unavailable.

Open `/hooks` in remote Codex and trust the Pip-chan hooks after installation. Test the tunnel from the remote server:

```sh
~/.local/bin/pip-chan-signal test --event thinking
```

Use another port if `47821` is not available:

```sh
./scripts/install.sh --remote my-server --remote-port 49152
./scripts/pip-chan-ssh --port 49152 my-server
```

SSH must permit remote TCP forwarding. The remote listener binds only to `127.0.0.1`. The helper sends only the state source, event, and authentication token. It discards hook input such as prompts and terminal output.

## Linux later

The socket protocol and Tauri UI are portable. Linux needs WebKitGTK and desktop integration packages to build. X11 supports the intended overlay behavior most consistently; Wayland compositors may refuse always-on-top placement.

## Asset

`src/assets/pip-idle.png`, `src/assets/pip-thinking.png`, and `src/assets/pip-ready.png` were generated with OpenAI image generation for this project. The ready state uses a brighter glow and motion treatment in the overlay.
