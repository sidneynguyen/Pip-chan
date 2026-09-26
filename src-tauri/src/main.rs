#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};
use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WindowEvent,
};
use toml_edit::{Array, DocumentMut, Item, Value};

const SOCKET_NAME: &str = "pip.sock";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PipEvent {
    source: String,
    event: String,
}

#[derive(Default)]
struct PipState {
    initial_event: Mutex<Option<PipEvent>>,
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

    let initial = if args.get(1).is_some_and(|arg| arg == "signal") {
        let event = event_from_args(&args);
        let payload = read_stdin();
        if send_to_running_app(&event).is_ok() {
            forward_previous_codex_notify(&event, &payload);
            return;
        }
        Some((event, payload))
    } else {
        None
    };

    run_app(initial);
}

fn run_app(initial: Option<(PipEvent, String)>) {
    let startup_event = initial.as_ref().map(|(event, _)| event.clone());

    tauri::Builder::default()
        .manage(PipState {
            initial_event: Mutex::new(startup_event),
        })
        .plugin(tauri_plugin_single_instance::init(move |app, args, _cwd| {
            if args.get(1).is_some_and(|arg| arg == "signal") {
                emit_event(app, event_from_args(&args));
            } else if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            start_socket_server(app.handle().clone())?;
            install_tray(app)?;

            if let Some((event, payload)) = initial.as_ref() {
                emit_event(app.handle(), event.clone());
                forward_previous_codex_notify(event, payload);
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
        .invoke_handler(tauri::generate_handler![initial_event, reset_position])
        .run(tauri::generate_context!())
        .expect("failed to run Pip-chan");
}

#[tauri::command]
fn initial_event(state: tauri::State<'_, PipState>) -> Option<PipEvent> {
    state.initial_event.lock().ok().and_then(|event| event.clone())
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
    let x = (size.width as f64 / scale - 375.0).max(0.0) as i32;
    let y = (size.height as f64 / scale - 600.0).max(0.0) as i32;
    window
        .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
            (x as f64 * scale) as i32,
            (y as f64 * scale) as i32,
        )))
        .map_err(|error| error.to_string())
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show Pip-chan").build(app)?;
    let reset = MenuItemBuilder::with_id("reset", "Reset position").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Pip-chan").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&show, &reset, &quit]).build()?;
    let app_handle = app.handle().clone();

    TrayIconBuilder::with_id("pip-chan")
        .tooltip("Pip-chan")
        .menu(&menu)
        .on_menu_event(move |_tray, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
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

fn start_socket_server(app: tauri::AppHandle) -> tauri::Result<()> {
    let socket = socket_path();
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent).map_err(tauri::Error::Io)?;
    }
    if socket.exists() {
        let _ = fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket).map_err(tauri::Error::Io)?;
    let _ = fs::set_permissions(&socket, fs::Permissions::from_mode(0o600));

    thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            let mut input = String::new();
            if connection
                .try_clone()
                .and_then(|mut stream| stream.read_to_string(&mut input))
                .is_ok()
            {
                if let Ok(event) = serde_json::from_str::<PipEvent>(&input) {
                    emit_event(&app, event);
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

fn read_stdin() -> String {
    if io::stdin().is_terminal() {
        return String::new();
    }
    let mut payload = String::new();
    let _ = io::stdin().read_to_string(&mut payload);
    payload
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

fn integration_path() -> PathBuf {
    pip_dir().join("integrations.json")
}

fn position_path() -> PathBuf {
    pip_dir().join("position.json")
}

fn save_position(window: &tauri::Window) {
    let Ok(position) = window.outer_position() else { return };
    let Ok(size) = window.outer_size() else { return };
    let data = json!({ "x": position.x, "y": position.y, "width": size.width, "height": size.height });
    let _ = fs::create_dir_all(pip_dir());
    let _ = fs::write(position_path(), data.to_string());
}

fn restore_position(window: &tauri::WebviewWindow) {
    let Ok(contents) = fs::read_to_string(position_path()) else { return };
    let Ok(position) = serde_json::from_str::<JsonValue>(&contents) else { return };
    let (Some(x), Some(y)) = (
        position.get("x").and_then(JsonValue::as_i64),
        position.get("y").and_then(JsonValue::as_i64),
    ) else { return };
    let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x as i32, y as i32)));
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
        executable()?, "signal".to_string(), "--source".to_string(), source.to_string(),
        "--event".to_string(), event.to_string(),
    ])
}

fn configure_codex() -> Result<(), String> {
    let config = home_dir().join(".codex/config.toml");
    let existing = fs::read_to_string(&config).unwrap_or_default();
    let mut document = existing.parse::<DocumentMut>().map_err(|error| error.to_string())?;
    let command = pip_command("codex", "ready")?;
    let existing_notify = document
        .get("notify")
        .and_then(Item::as_value)
        .and_then(Value::as_array)
        .map(|array| array.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>());

    if existing_notify.as_ref().is_some_and(|notify| notify == &command) {
        println!("Codex is already connected to Pip-chan.");
        return Ok(());
    }
    if let Some(notify) = existing_notify {
        let mut integrations = read_integrations();
        integrations.codex_previous_notify = Some(notify);
        write_integrations(&integrations)?;
    }

    let mut array = Array::new();
    for argument in command { array.push(argument); }
    document["notify"] = Item::Value(Value::Array(array));
    backup_then_write(&config, &existing, &document.to_string())?;
    println!("Connected Codex to Pip-chan.");
    Ok(())
}

fn configure_claude() -> Result<(), String> {
    let config = home_dir().join(".claude/settings.json");
    let existing = fs::read_to_string(&config).unwrap_or_else(|_| "{}".to_string());
    let mut root = serde_json::from_str::<JsonValue>(&existing).map_err(|error| error.to_string())?;
    let root_object = root.as_object_mut().ok_or("Claude settings must be a JSON object.")?;
    let hooks = root_object.entry("hooks").or_insert_with(|| JsonValue::Object(Map::new()))
        .as_object_mut().ok_or("Claude settings `hooks` must be a JSON object.")?;

    add_claude_hook(hooks, "Stop", None, shell_command(&pip_command("claude", "ready")?));
    add_claude_hook(hooks, "Notification", Some("permission_prompt"), shell_command(&pip_command("claude", "attention")?));

    let next = serde_json::to_string_pretty(&root).map_err(|error| error.to_string())? + "\n";
    backup_then_write(&config, &existing, &next)?;
    println!("Connected Claude Code to Pip-chan.");
    Ok(())
}

fn add_claude_hook(hooks: &mut Map<String, JsonValue>, event: &str, matcher: Option<&str>, command: String) {
    let entries = hooks.entry(event.to_string()).or_insert_with(|| JsonValue::Array(Vec::new()))
        .as_array_mut().expect("Claude hook event must be an array");
    let exists = entries.iter().any(|entry| {
        entry.get("matcher").and_then(JsonValue::as_str).eq(&matcher)
            && entry.get("hooks").and_then(JsonValue::as_array).is_some_and(|commands| {
                commands.iter().any(|value| value.get("command").and_then(JsonValue::as_str) == Some(command.as_str()))
            })
    });
    if exists { return; }
    let mut entry = Map::new();
    if let Some(matcher) = matcher { entry.insert("matcher".to_string(), JsonValue::String(matcher.to_string())); }
    entry.insert("hooks".to_string(), JsonValue::Array(vec![json!({ "type": "command", "command": command })]));
    entries.push(JsonValue::Object(entry));
}

fn shell_command(arguments: &[String]) -> String {
    arguments.iter().map(|argument| format!("'{}'", argument.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ")
}

fn backup_then_write(path: &Path, previous: &str, next: &str) -> Result<(), String> {
    if previous == next { return Ok(()); }
    let parent = path.parent().ok_or("Configuration path has no parent.")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    if path.exists() {
        let filename = path.file_name().and_then(|name| name.to_str()).unwrap_or("config");
        fs::copy(path, path.with_file_name(format!("{filename}.pip-chan.bak"))).map_err(|error| error.to_string())?;
    }
    fs::write(path, next).map_err(|error| error.to_string())
}

fn read_integrations() -> Integrations {
    fs::read_to_string(integration_path()).ok().and_then(|contents| serde_json::from_str(&contents).ok()).unwrap_or_default()
}

fn write_integrations(integrations: &Integrations) -> Result<(), String> {
    fs::create_dir_all(pip_dir()).map_err(|error| error.to_string())?;
    fs::write(integration_path(), serde_json::to_string_pretty(integrations).map_err(|error| error.to_string())?).map_err(|error| error.to_string())
}

fn forward_previous_codex_notify(event: &PipEvent, payload: &str) {
    if event.source != "codex" || payload.is_empty() { return; }
    let Some(command) = read_integrations().codex_previous_notify else { return };
    let Some((program, arguments)) = command.split_first() else { return };
    let Ok(mut child) = Command::new(program).args(arguments).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return };
    if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(payload.as_bytes()); }
}
