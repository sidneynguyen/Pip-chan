#!/usr/bin/env python3

import argparse
import json
import os
import shlex
import shutil
import socket
import sys
import tempfile
from pathlib import Path

DEFAULT_PORT = 47821
CONNECT_TIMEOUT = 0.25
STATE_HOOKS = (
    ("UserPromptSubmit", None, "thinking"),
    ("PermissionRequest", None, "attention"),
    ("PostToolUse", None, "thinking"),
    ("PreCompact", None, "thinking"),
    ("PostCompact", "manual", "idle"),
    ("PostCompact", "auto", "thinking"),
    ("SessionEnd", None, "idle"),
)


def pip_dir():
    return Path.home() / ".pip-chan"


def connection_path():
    return pip_dir() / "remote.json"


def load_json(path, default):
    if not path.exists():
        return default
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def render_json(value):
    return json.dumps(value, indent=2) + "\n"


def write_json(path, value, backup=True):
    next_contents = render_json(value)
    previous = path.read_text(encoding="utf-8") if path.exists() else None
    if previous == next_contents:
        return False

    path.parent.mkdir(parents=True, exist_ok=True)
    path.parent.chmod(0o700)
    backup_path = path.with_name(path.name + ".pip-chan.bak")
    if backup and path.exists() and not backup_path.exists():
        shutil.copy2(path, backup_path)

    descriptor, temporary_name = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(next_contents)
        os.chmod(temporary_name, 0o600)
        os.replace(temporary_name, path)
    finally:
        if os.path.exists(temporary_name):
            os.unlink(temporary_name)
    return True


def hook_arguments(source, event, hook_result_json=False):
    arguments = [
        "signal",
        "--source",
        source,
        "--event",
        event,
    ]
    if hook_result_json:
        arguments.append("--hook-result-json")
    return arguments


def codex_handler(script, source, event, hook_result_json=False):
    arguments = [str(script)] + hook_arguments(source, event, hook_result_json)
    return {
        "type": "command",
        "command": " ".join(shlex.quote(argument) for argument in arguments),
        "timeout": 5,
    }


def claude_handler(script, source, event, hook_result_json=False):
    return {
        "type": "command",
        "command": str(script),
        "args": hook_arguments(source, event, hook_result_json),
        "timeout": 5,
    }


def add_hook(root, event, handler, matcher=None):
    hooks = root.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("The `hooks` value must be a JSON object.")
    entries = hooks.setdefault(event, [])
    if not isinstance(entries, list):
        raise ValueError("The `hooks.{}` value must be a JSON array.".format(event))

    matching_entry = None
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        if entry.get("matcher") == matcher:
            handlers = entry.get("hooks")
            if not isinstance(handlers, list):
                raise ValueError("A `hooks.{}` entry has an invalid `hooks` value.".format(event))
            if handler in handlers:
                return
            if matching_entry is None:
                matching_entry = entry

    if matching_entry is not None:
        matching_entry["hooks"].append(handler)
        return

    entry = {"hooks": [handler]}
    if matcher is not None:
        entry["matcher"] = matcher
    entries.append(entry)


def same_handler(left, right):
    return left.get("command") == right.get("command") and left.get("args") == right.get("args")


def remove_hook(root, event, handler, matcher=None):
    hooks = root.get("hooks")
    if not isinstance(hooks, dict) or not isinstance(hooks.get(event), list):
        return
    remaining_entries = []
    for entry in hooks[event]:
        if isinstance(entry, dict) and entry.get("matcher") == matcher:
            handlers = entry.get("hooks")
            if isinstance(handlers, list):
                entry["hooks"] = [
                    existing
                    for existing in handlers
                    if not (isinstance(existing, dict) and same_handler(existing, handler))
                ]
                if not entry["hooks"]:
                    continue
        remaining_entries.append(entry)
    if remaining_entries:
        hooks[event] = remaining_entries
    else:
        del hooks[event]


def add_state_hooks(root, handler):
    for event, matcher, state in STATE_HOOKS:
        add_hook(root, event, handler(state), matcher)


def prepare_codex(script):
    path = Path.home() / ".codex" / "hooks.json"
    root = load_json(path, {})
    if not isinstance(root, dict):
        raise ValueError("Codex hooks must be a JSON object.")
    add_state_hooks(root, lambda state: codex_handler(script, "codex", state))
    add_hook(root, "Stop", codex_handler(script, "codex", "ready", True))
    return path, root


def prepare_claude(script):
    path = Path.home() / ".claude" / "settings.json"
    root = load_json(path, {})
    if not isinstance(root, dict):
        raise ValueError("Claude settings must be a JSON object.")
    add_state_hooks(root, lambda state: claude_handler(script, "claude", state))
    add_hook(root, "Stop", claude_handler(script, "claude", "ready", True))
    remove_hook(
        root,
        "Notification",
        claude_handler(script, "claude", "attention"),
        "permission_prompt",
    )
    return path, root


def configure(target, port, token):
    script = Path(__file__).resolve()
    updates = []
    if target in ("codex", "all"):
        updates.append(("Codex",) + prepare_codex(script))
    if target in ("claude", "all"):
        updates.append(("Claude Code",) + prepare_claude(script))

    connection = {
        "host": "127.0.0.1",
        "port": port,
        "token": token,
    }
    write_json(connection_path(), connection, backup=False)
    for name, path, value in updates:
        changed = write_json(path, value)
        action = "Updated" if changed else "Checked"
        print("{} {}.".format(action, name))


def read_connection():
    value = load_json(connection_path(), {})
    if not isinstance(value, dict):
        return None
    host = value.get("host")
    port = value.get("port")
    token = value.get("token")
    if not isinstance(host, str) or not isinstance(port, int) or not isinstance(token, str):
        return None
    return host, port, token


def read_session():
    if sys.stdin.isatty():
        return None
    try:
        payload = json.loads(sys.stdin.buffer.read())
    except ValueError:
        return None
    session = payload.get("session_id") if isinstance(payload, dict) else None
    return session if isinstance(session, str) else None


def send_signal(source, event, session=None):
    connection = read_connection()
    if connection is None:
        return False
    host, port, token = connection
    message = {
        "source": source,
        "event": event,
        "token": token,
    }
    if session is not None:
        message["session"] = session
    payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
    try:
        with socket.create_connection((host, port), CONNECT_TIMEOUT) as connection_socket:
            connection_socket.sendall(payload)
        return True
    except OSError:
        return False


def positive_port(value):
    port = int(value)
    if port < 1024 or port > 65535:
        raise argparse.ArgumentTypeError("Use a port from 1024 through 65535.")
    return port


def parse_arguments():
    parser = argparse.ArgumentParser(description="Send remote agent states to Pip-chan.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    configure_parser = subparsers.add_parser("configure")
    configure_parser.add_argument("target", choices=("codex", "claude", "all"))
    configure_parser.add_argument("--port", type=positive_port, default=DEFAULT_PORT)
    configure_parser.add_argument("--token-stdin", action="store_true", required=True)

    signal_parser = subparsers.add_parser("signal")
    signal_parser.add_argument("--source", required=True)
    signal_parser.add_argument(
        "--event",
        choices=("idle", "thinking", "ready", "attention"),
        required=True,
    )
    signal_parser.add_argument("--hook-result-json", action="store_true")
    signal_parser.add_argument("--strict", action="store_true")

    test_parser = subparsers.add_parser("test")
    test_parser.add_argument(
        "--event",
        choices=("idle", "thinking", "ready", "attention"),
        default="thinking",
    )
    return parser.parse_args()


def main():
    arguments = parse_arguments()
    if arguments.command == "configure":
        token = sys.stdin.readline().strip()
        if len(token) < 32:
            raise ValueError("The Pip-chan remote token is invalid.")
        configure(arguments.target, arguments.port, token)
        return

    session = read_session()
    if arguments.command == "test":
        if not send_signal("test", arguments.event):
            print("Pip-chan did not receive the remote test event.", file=sys.stderr)
            raise SystemExit(1)
        print("Sent the {} test event to Pip-chan.".format(arguments.event))
        return

    sent = send_signal(arguments.source, arguments.event, session)
    if arguments.hook_result_json:
        print("{}")
    if arguments.strict and not sent:
        raise SystemExit(1)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print("Pip-chan remote setup failed: {}".format(error), file=sys.stderr)
        raise SystemExit(1)
