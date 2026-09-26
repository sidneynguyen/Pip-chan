#!/usr/bin/env bash
set -euo pipefail

APP_SOURCE="${1:-src-tauri/target/release/bundle/macos/Pip-chan.app}"
APP_DESTINATION="$HOME/Applications/Pip-chan.app"
APP_EXECUTABLE="$APP_DESTINATION/Contents/MacOS/pip-chan"

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
