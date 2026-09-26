#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Mutex},
    thread,
    time::Duration,
};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WindowEvent,
};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table, Value};

const SOCKET_NAME: &str = "pip.sock";
const REMOTE_SOCKET_NAME: &str = "remote.sock";
const LAUNCH_WITH_EVENT_COMMAND: &str = "__launch-with-event";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PipEvent {
    source: String,
    event: String,
}

#[derive(Debug, Deserialize)]
struct SocketEvent {
    source: String,
    event: String,
    token: Option<String>,
}

#[derive(Default)]
struct PipState {
    initial_event: Mutex<Option<PipEvent>>,
    ghost_mode: Mutex<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct Integrations {
    codex_previous_notify: Option<Vec<String>>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).is_some_and(|arg| arg == "configure") {
        let target = args.get(2).map(String::as_str).unwrap_or("all");
        if let Err(error) = configure(target) {
            eprintln!("Pip-chan configuration failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if args.get(1).is_some_and(|arg| arg == "remote-token") {
        match ensure_remote_token() {
            Ok(token) => println!("{token}"),
            Err(error) => {
                eprintln!("Pip-chan remote token failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if args
        .get(1)
        .is_some_and(|arg| arg == LAUNCH_WITH_EVENT_COMMAND)
    {
        run_app(Some(event_from_args(&args)));
        return;
    }

    if args
        .get(1)
        .is_some_and(|arg| arg == "minimize" || arg == "show")
    {
        let event = PipEvent {
            source: "pip-chan".to_string(),
            event: args[1].clone(),
        };
        if let Err(error) = send_to_running_app(&event) {
            eprintln!("Pip-chan is not running: {error}");
            std::process::exit(1);
        }
        return;
    }

    if args.get(1).is_some_and(|arg| arg == "signal") {
        let event = event_from_args(&args);
        let payload = signal_payload(&args, &event);
        if send_to_running_app(&event).is_err() {
            let _ = launch_with_event(&event);
        }
        forward_previous_codex_notify(&event, &payload);
        return;
    }

    run_app(None);
}

fn run_app(startup_event: Option<PipEvent>) {
    let app = tauri::Builder::default()
        .manage(PipState {
            initial_event: Mutex::new(startup_event.clone()),
            ghost_mode: Mutex::new(false),
        })
        .plugin(tauri_plugin_single_instance::init(
            move |app, args, _cwd| {
                if args
                    .get(1)
                    .is_some_and(|arg| arg == "signal" || arg == LAUNCH_WITH_EVENT_COMMAND)
                {
                    dispatch_event(app, event_from_args(&args));
                } else {
                    show_window(app);
                }
            },
        ))
        .setup(move |app| {
            start_socket_server(app.handle().clone(), socket_path(), None)?;
            start_socket_server(
                app.handle().clone(),
                remote_socket_path(),
                Some(ensure_remote_token().map_err(tauri::Error::Io)?),
            )?;
            install_tray(app)?;

            if let Some(event) = startup_event.as_ref() {
                dispatch_event(app.handle(), event.clone());
            }
            if let Some(window) = app.get_webview_window("main") {
                restore_position(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Moved(_) = event {
                save_position(window);
            }
        })
        .invoke_handler(tauri::generate_handler![
            hide_window,
            initial_event,
            ghost_mode,
            reset_position,
            toggle_ghost_mode
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Pip-chan");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_window(app);
        }
    });
}

#[tauri::command]
fn initial_event(state: tauri::State<'_, PipState>) -> Option<PipEvent> {
    state
        .initial_event
        .lock()
        .ok()
        .and_then(|event| event.clone())
}

#[tauri::command]
fn ghost_mode(state: tauri::State<'_, PipState>) -> bool {
    state
        .ghost_mode
        .lock()
        .map(|enabled| *enabled)
        .unwrap_or(false)
}

#[tauri::command]
fn hide_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|error| error.to_string())
}

#[tauri::command]
fn reset_position(window: tauri::WebviewWindow) -> Result<(), String> {
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "No display is available".to_string())?;
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let x = (size.width as f64 / scale - 355.0).max(0.0) as i32;
    let y = (size.height as f64 / scale - 570.0).max(0.0) as i32;
    window
        .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
            (x as f64 * scale) as i32,
            (y as f64 * scale) as i32,
        )))
        .map_err(|error| error.to_string())
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show Pip-chan").build(app)?;
    let ghost = MenuItemBuilder::with_id("ghost", "Toggle ghost mode").build(app)?;
    let minimize = MenuItemBuilder::with_id("minimize", "Minimize Pip-chan").build(app)?;
    let reset = MenuItemBuilder::with_id("reset", "Reset position").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Pip-chan").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show, &ghost, &minimize, &reset, &quit])
        .build()?;
    let app_handle = app.handle().clone();
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id("pip-chan")
        .icon(icon)
        .tooltip("Pip-chan")
        .menu(&menu)
        .on_menu_event(move |_tray, event| match event.id().as_ref() {
            "show" => show_window(&app_handle),
            "ghost" => toggle_ghost_mode(app_handle.clone()),
            "minimize" => minimize_window(&app_handle),
            "reset" => {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = reset_position(window);
                }
            }
            "quit" => app_handle.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn emit_event(app: &tauri::AppHandle, event: PipEvent) {
    let _ = app.emit("pip:event", event);
}

fn dispatch_event(app: &tauri::AppHandle, event: PipEvent) {
    if event.source == "pip-chan" {
        match event.event.as_str() {
            "minimize" => {
                minimize_window(app);
                return;
            }
            "show" => {
                show_window(app);
                return;
            }
            _ => {}
        }
    }
    emit_event(app, event);
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn minimize_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.minimize();
    }
}

#[tauri::command]
fn toggle_ghost_mode(app: tauri::AppHandle) {
    let state = app.state::<PipState>();
    let enabled = match state.ghost_mode.lock() {
        Ok(mut enabled) => {
            *enabled = !*enabled;
            *enabled
        }
        Err(_) => return,
    };
    let _ = app.emit("pip:ghost", enabled);
}

fn start_socket_server(
    app: tauri::AppHandle,
    socket: PathBuf,
    expected_token: Option<String>,
) -> tauri::Result<()> {
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent).map_err(tauri::Error::Io)?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    if socket.exists() {
        let _ = fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket).map_err(tauri::Error::Io)?;
    let _ = fs::set_permissions(&socket, fs::Permissions::from_mode(0o600));

    thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            let mut input = String::new();
            let stream = match connection.try_clone() {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            if stream.take(4097).read_to_string(&mut input).is_ok() && input.len() <= 4096 {
                if let Ok(event) = serde_json::from_str::<SocketEvent>(&input) {
                    let authorized = expected_token.as_ref().map_or(true, |expected| {
                        event
                            .token
                            .as_deref()
                            .is_some_and(|actual| tokens_match(expected, actual))
                    });
                    if authorized {
                        dispatch_event(
                            &app,
                            PipEvent {
                                source: event.source,
                                event: event.event,
                            },
                        );
                    }
                }
            }
        }
    });
    Ok(())
}

fn send_to_running_app(event: &PipEvent) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.write_all(serde_json::to_string(event)?.as_bytes())
}

fn launch_with_event(event: &PipEvent) -> io::Result<()> {
    Command::new(std::env::current_exe()?)
        .arg(LAUNCH_WITH_EVENT_COMMAND)
        .arg("--source")
        .arg(&event.source)
        .arg("--event")
        .arg(&event.event)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn event_from_args(args: &[String]) -> PipEvent {
    let source = option_value(args, "--source").unwrap_or_else(|| "test".to_string());
    let event = option_value(args, "--event").unwrap_or_else(|| "ready".to_string());
    PipEvent { source, event }
}

fn option_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn signal_payload(args: &[String], event: &PipEvent) -> String {
    if event.source == "codex" && event.event == "ready" {
        return trailing_argument(args, "--event").unwrap_or_default();
    }
    read_stdin_with_timeout()
}

fn trailing_argument(args: &[String], option: &str) -> Option<String> {
    let value_index = args.iter().position(|argument| argument == option)? + 1;
    args.get(value_index + 1).cloned()
}

fn read_stdin_with_timeout() -> String {
    if io::stdin().is_terminal() {
        return String::new();
    }
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut payload = String::new();
        let _ = io::stdin().take(1_048_577).read_to_string(&mut payload);
        let _ = sender.send(payload);
    });
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn pip_dir() -> PathBuf {
    home_dir().join(".pip-chan")
}

fn socket_path() -> PathBuf {
    pip_dir().join(SOCKET_NAME)
}

fn remote_socket_path() -> PathBuf {
    pip_dir().join(REMOTE_SOCKET_NAME)
}

fn remote_token_path() -> PathBuf {
    pip_dir().join("remote-token")
}

fn ensure_remote_token() -> io::Result<String> {
    let path = remote_token_path();
    if let Ok(token) = fs::read_to_string(&path) {
        let token = token.trim().to_string();
        if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            fs::set_permissions(pip_dir(), fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            return Ok(token);
        }
    }

    fs::create_dir_all(pip_dir())?;
    fs::set_permissions(pip_dir(), fs::Permissions::from_mode(0o700))?;
    let mut random = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    output.write_all(token.as_bytes())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

fn tokens_match(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .bytes()
        .zip(actual.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn integration_path() -> PathBuf {
    pip_dir().join("integrations.json")
}

fn position_path() -> PathBuf {
    pip_dir().join("position.json")
}

fn save_position(window: &tauri::Window) {
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let data =
        json!({ "x": position.x, "y": position.y, "width": size.width, "height": size.height });
    let _ = fs::create_dir_all(pip_dir());
    let _ = fs::write(position_path(), data.to_string());
}

fn restore_position(window: &tauri::WebviewWindow) {
    let Ok(contents) = fs::read_to_string(position_path()) else {
        return;
    };
    let Ok(position) = serde_json::from_str::<JsonValue>(&contents) else {
        return;
    };
    let (Some(x), Some(y)) = (
        position.get("x").and_then(JsonValue::as_i64),
        position.get("y").and_then(JsonValue::as_i64),
    ) else {
        return;
    };
    let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
        x as i32, y as i32,
    )));
}

fn configure(target: &str) -> Result<(), String> {
    match target {
        "codex" => configure_codex(),
        "claude" => configure_claude(),
        "all" => {
            configure_codex()?;
            configure_claude()
        }
        _ => Err("Use `configure codex`, `configure claude`, or `configure all`.".to_string()),
    }
}

fn executable() -> Result<String, String> {
    std::env::current_exe()
        .map_err(|error| error.to_string())
        .map(|path| path.to_string_lossy().to_string())
}

fn pip_command(source: &str, event: &str) -> Result<Vec<String>, String> {
    Ok(vec![
        executable()?,
        "signal".to_string(),
        "--source".to_string(),
        source.to_string(),
        "--event".to_string(),
        event.to_string(),
    ])
}

fn configure_codex() -> Result<(), String> {
    let config = home_dir().join(".codex/config.toml");
    let existing = fs::read_to_string(&config).unwrap_or_default();
    let mut document = existing
        .parse::<DocumentMut>()
        .map_err(|error| error.to_string())?;
    let command = pip_command("codex", "ready")?;
    let existing_notify = document
        .get("notify")
        .and_then(Item::as_value)
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });

    if let Some(notify) = existing_notify
        .as_ref()
        .filter(|notify| *notify != &command)
        .cloned()
    {
        let mut integrations = read_integrations();
        integrations.codex_previous_notify = Some(notify);
        write_integrations(&integrations)?;
    }

    if existing_notify.as_ref() != Some(&command) {
        let mut array = Array::new();
        for argument in command {
            array.push(argument);
        }
        document["notify"] = Item::Value(Value::Array(array));
    }

    add_codex_hook(
        &mut document,
        "UserPromptSubmit",
        shell_command(&pip_command("codex", "thinking")?),
    )?;
    add_codex_hook(
        &mut document,
        "PermissionRequest",
        shell_command(&pip_command("codex", "attention")?),
    )?;

    backup_then_write(&config, &existing, &document.to_string())?;
    println!("Connected Codex to Pip-chan. Open `/hooks` in Codex to review and trust the Pip-chan hooks.");
    Ok(())
}

fn add_codex_hook(document: &mut DocumentMut, event: &str, command: String) -> Result<(), String> {
    if !document.contains_key("hooks") {
        document["hooks"] = Item::Table(Table::new());
    }
    let hooks = document["hooks"]
        .as_table_mut()
        .ok_or("Codex config `hooks` must be a table.")?;
    if !hooks.contains_key(event) {
        hooks[event] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let entries = hooks[event]
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("Codex config `hooks.{event}` must be an array of tables."))?;
    for entry in entries.iter_mut() {
        let Some(commands) = entry
            .get_mut("hooks")
            .and_then(Item::as_array_of_tables_mut)
        else {
            continue;
        };
        for hook in commands.iter_mut() {
            let matches = hook
                .get("command")
                .and_then(Item::as_value)
                .and_then(Value::as_str)
                == Some(command.as_str());
            if matches {
                hook["timeout"] = value(5);
                return Ok(());
            }
        }
    }

    let mut hook = Table::new();
    hook["type"] = value("command");
    hook["command"] = value(command);
    hook["timeout"] = value(5);
    let mut hook_entries = ArrayOfTables::new();
    hook_entries.push(hook);

    let mut entry = Table::new();
    entry["hooks"] = Item::ArrayOfTables(hook_entries);
    entries.push(entry);
    Ok(())
}

fn configure_claude() -> Result<(), String> {
    let config = home_dir().join(".claude/settings.json");
    let existing = fs::read_to_string(&config).unwrap_or_else(|_| "{}".to_string());
    let mut root =
        serde_json::from_str::<JsonValue>(&existing).map_err(|error| error.to_string())?;
    let root_object = root
        .as_object_mut()
        .ok_or("Claude settings must be a JSON object.")?;
    let hooks = root_object
        .entry("hooks")
        .or_insert_with(|| JsonValue::Object(Map::new()))
        .as_object_mut()
        .ok_or("Claude settings `hooks` must be a JSON object.")?;

    add_claude_hook(
        hooks,
        "UserPromptSubmit",
        None,
        shell_command(&pip_command("claude", "thinking")?),
    );
    add_claude_hook(
        hooks,
        "Stop",
        None,
        shell_command(&pip_command("claude", "ready")?),
    );
    add_claude_hook(
        hooks,
        "Notification",
        Some("permission_prompt"),
        shell_command(&pip_command("claude", "attention")?),
    );

    let next = serde_json::to_string_pretty(&root).map_err(|error| error.to_string())? + "\n";
    backup_then_write(&config, &existing, &next)?;
    println!("Connected Claude Code to Pip-chan.");
    Ok(())
}

fn add_claude_hook(
    hooks: &mut Map<String, JsonValue>,
    event: &str,
    matcher: Option<&str>,
    command: String,
) {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| JsonValue::Array(Vec::new()))
        .as_array_mut()
        .expect("Claude hook event must be an array");
    for entry in entries.iter_mut() {
        if !entry
            .get("matcher")
            .and_then(JsonValue::as_str)
            .eq(&matcher)
        {
            continue;
        }
        let Some(commands) = entry.get_mut("hooks").and_then(JsonValue::as_array_mut) else {
            continue;
        };
        for hook in commands.iter_mut() {
            let matches = hook.get("command").and_then(JsonValue::as_str) == Some(command.as_str());
            if matches {
                if let Some(hook) = hook.as_object_mut() {
                    hook.insert("timeout".to_string(), JsonValue::from(5));
                }
                return;
            }
        }
    }
    let mut entry = Map::new();
    if let Some(matcher) = matcher {
        entry.insert(
            "matcher".to_string(),
            JsonValue::String(matcher.to_string()),
        );
    }
    entry.insert(
        "hooks".to_string(),
        JsonValue::Array(vec![
            json!({ "type": "command", "command": command, "timeout": 5 }),
        ]),
    );
    entries.push(JsonValue::Object(entry));
}

fn shell_command(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn backup_then_write(path: &Path, previous: &str, next: &str) -> Result<(), String> {
    if previous == next {
        return Ok(());
    }
    let parent = path.parent().ok_or("Configuration path has no parent.")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    if path.exists() {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config");
        fs::copy(
            path,
            path.with_file_name(format!("{filename}.pip-chan.bak")),
        )
        .map_err(|error| error.to_string())?;
    }
    fs::write(path, next).map_err(|error| error.to_string())
}

fn read_integrations() -> Integrations {
    fs::read_to_string(integration_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn write_integrations(integrations: &Integrations) -> Result<(), String> {
    fs::create_dir_all(pip_dir()).map_err(|error| error.to_string())?;
    fs::write(
        integration_path(),
        serde_json::to_string_pretty(integrations).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn forward_previous_codex_notify(event: &PipEvent, payload: &str) {
    if !should_forward_previous_codex_notify(event, payload) {
        return;
    }
    let Some(command) = read_integrations().codex_previous_notify else {
        return;
    };
    let Some((program, arguments)) = command.split_first() else {
        return;
    };
    let _ = Command::new(program)
        .args(arguments)
        .arg(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn should_forward_previous_codex_notify(event: &PipEvent, payload: &str) -> bool {
    event.source == "codex" && event.event == "ready" && !payload.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_hook_is_added_once() {
        let mut document = "".parse::<DocumentMut>().unwrap();
        let command = "'pip-chan' 'signal' '--event' 'thinking'".to_string();

        add_codex_hook(&mut document, "UserPromptSubmit", command.clone()).unwrap();
        add_codex_hook(&mut document, "UserPromptSubmit", command).unwrap();

        let entries = document["hooks"]["UserPromptSubmit"]
            .as_array_of_tables()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries.get(0).unwrap();
        let hook = entry["hooks"].as_array_of_tables().unwrap().get(0).unwrap();
        assert_eq!(
            hook["timeout"].as_value().and_then(Value::as_integer),
            Some(5)
        );
    }

    #[test]
    fn only_ready_codex_payloads_are_forwarded() {
        let ready = PipEvent {
            source: "codex".to_string(),
            event: "ready".to_string(),
        };
        let thinking = PipEvent {
            source: "codex".to_string(),
            event: "thinking".to_string(),
        };

        assert!(should_forward_previous_codex_notify(&ready, "{}"));
        assert!(!should_forward_previous_codex_notify(&thinking, "{}"));
        assert!(!should_forward_previous_codex_notify(&ready, ""));
    }

    #[test]
    fn remote_tokens_must_match() {
        assert!(tokens_match("secret", "secret"));
        assert!(!tokens_match("secret", "other!"));
        assert!(!tokens_match("secret", "short"));
    }

    #[test]
    fn codex_notify_payload_comes_from_the_trailing_argument() {
        let args = vec![
            "pip-chan".to_string(),
            "signal".to_string(),
            "--source".to_string(),
            "codex".to_string(),
            "--event".to_string(),
            "ready".to_string(),
            "{\"type\":\"agent-turn-complete\"}".to_string(),
        ];
        let event = event_from_args(&args);

        assert_eq!(
            signal_payload(&args, &event),
            "{\"type\":\"agent-turn-complete\"}"
        );
    }
}
