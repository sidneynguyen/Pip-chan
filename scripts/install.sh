#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 [--remote SSH_HOST] [--remote-port PORT] [APP_SOURCE]"
}

APP_SOURCE="src-tauri/target/release/bundle/macos/Pip-chan.app"
APP_SOURCE_SET=false
REMOTE_HOST=""
REMOTE_PORT=47821

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --remote)
      if [[ $# -lt 2 || "$2" == -* ]]; then
        echo "The --remote option needs an SSH host." >&2
        exit 1
      fi
      REMOTE_HOST="$2"
      shift 2
      ;;
    --remote-port)
      if [[ $# -lt 2 ]]; then
        echo "The --remote-port option needs a port." >&2
        exit 1
      fi
      REMOTE_PORT="$2"
      shift 2
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [[ "$APP_SOURCE_SET" == true ]]; then
        echo "Pass only one Pip-chan.app source path." >&2
        exit 1
      fi
      APP_SOURCE="$1"
      APP_SOURCE_SET=true
      shift
      ;;
  esac
done

if [[ ! "$REMOTE_PORT" =~ ^[0-9]+$ ]] || (( REMOTE_PORT < 1024 || REMOTE_PORT > 65535 )); then
  echo "Use a remote port from 1024 through 65535." >&2
  exit 1
fi

APP_DESTINATION="$HOME/Applications/Pip-chan.app"
APP_EXECUTABLE="$APP_DESTINATION/Contents/MacOS/pip-chan"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REMOTE_HELPER="$SCRIPT_DIR/pip-chan-remote.py"

if [[ ! -d "$APP_SOURCE" ]]; then
  echo "Build the release bundle first, or pass the path to Pip-chan.app." >&2
  exit 1
fi
mkdir -p "$HOME/Applications"
if [[ -e "$APP_DESTINATION" ]]; then
  rm -rf -- "$APP_DESTINATION"
fi
cp -R "$APP_SOURCE" "$APP_DESTINATION"
"$APP_EXECUTABLE" configure all
echo "Pip-chan is installed. Launch it from ~/Applications, then start a Codex or Claude Code session to try her out."

if [[ -z "$REMOTE_HOST" ]]; then
  exit 0
fi
if [[ ! -f "$REMOTE_HELPER" ]]; then
  echo "The remote helper is missing: $REMOTE_HELPER" >&2
  exit 1
fi

ssh "$REMOTE_HOST" 'command -v python3 >/dev/null'
ssh "$REMOTE_HOST" 'set -eu
umask 077
mkdir -p "$HOME/.local/bin"
temporary="$(mktemp "$HOME/.local/bin/.pip-chan-signal.XXXXXX")"
cat > "$temporary"
chmod 700 "$temporary"
mv "$temporary" "$HOME/.local/bin/pip-chan-signal"
' < "$REMOTE_HELPER"

PIP_TOKEN="$("$APP_EXECUTABLE" remote-token)"
printf '%s\n' "$PIP_TOKEN" |
  ssh "$REMOTE_HOST" "\"\$HOME/.local/bin/pip-chan-signal\" configure all --port $REMOTE_PORT --token-stdin"

echo "Remote Pip-chan hooks are installed on $REMOTE_HOST."
echo "Connect with: $SCRIPT_DIR/pip-chan-ssh --port $REMOTE_PORT $REMOTE_HOST"
echo "Then open /hooks in Codex and trust the new Pip-chan hooks."
