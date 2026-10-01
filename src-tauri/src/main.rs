#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::{Local, NaiveDateTime, TimeDelta, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    menu::{CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, LogicalSize, Manager, PhysicalPosition, WindowEvent,
};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table, Value};

const SOCKET_NAME: &str = "pip.sock";
const REMOTE_SOCKET_NAME: &str = "remote.sock";
const LAUNCH_WITH_EVENT_COMMAND: &str = "__launch-with-event";
const BASE_WINDOW_WIDTH: f64 = 240.0;
const BASE_WINDOW_HEIGHT: f64 = 336.0;
const DEFAULT_SIZE_PERCENT: u32 = 130;
const MIN_SIZE_PERCENT: u32 = 50;
const MAX_SIZE_PERCENT: u32 = 200;
const SIZE_STEP_PERCENT: u32 = 10;
const WATER_REMINDER_TIMES: [(u32, u32); 2] = [(11, 0), (14, 0)];
const EYE_REMINDER_MINUTE: u32 = 55;
const REMINDER_CHECK_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PipEvent {
    source: String,
    event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SocketEvent {
    source: String,
    event: String,
    session: Option<String>,
    token: Option<String>,
}

#[derive(Default)]
struct PipState {
    initial_event: Mutex<Option<PipEvent>>,
    ghost_mode: Mutex<bool>,
    size_percent: Mutex<u32>,
    reminder_settings: Mutex<ReminderSettings>,
    pending_reminders: Mutex<Vec<PendingReminder>>,
}

struct VisibilityMenuItem(MenuItem<tauri::Wry>);

struct ReminderMenuItems(Vec<(Reminder, CheckMenuItem<tauri::Wry>)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reminder {
    Water,
    Eyes,
}

impl Reminder {
    const ALL: [Reminder; 2] = [Reminder::Water, Reminder::Eyes];

    fn key(self) -> &'static str {
        match self {
            Reminder::Water => "water",
            Reminder::Eyes => "eyes",
        }
    }

    fn menu_id(self) -> String {
        format!("reminder_{}", self.key())
    }

    fn menu_label(self) -> &'static str {
        match self {
            Reminder::Water => "Water reminders at 11 AM and 2 PM",
            Reminder::Eyes => "Eye rest reminders every hour at :55",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Reminder::Water => "Drink some water, baka!",
            Reminder::Eyes => "Stop staring, baka!",
        }
    }

    fn max_age(self) -> TimeDelta {
        match self {
            Reminder::Water => TimeDelta::hours(2),
            Reminder::Eyes => TimeDelta::minutes(15),
        }
    }

    fn latest_time(self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        match self {
            Reminder::Water => {
                let today = now.date();
                [today.pred_opt(), Some(today)]
                    .into_iter()
                    .flatten()
                    .flat_map(|date| {
                        WATER_REMINDER_TIMES
                            .into_iter()
                            .filter_map(move |(hour, minute)| date.and_hms_opt(hour, minute, 0))
                    })
                    .filter(|time| *time <= now)
                    .max()
            }
            Reminder::Eyes => {
                let this_hour = now.date().and_hms_opt(now.hour(), EYE_REMINDER_MINUTE, 0)?;
                Some(if this_hour <= now {
                    this_hour
                } else {
                    this_hour - TimeDelta::hours(1)
                })
            }
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ReminderSettings {
    #[serde(default)]
    disabled: BTreeSet<String>,
    #[serde(default)]
    dismissed: BTreeMap<String, NaiveDateTime>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PendingReminder {
    kind: &'static str,
    message: &'static str,
    time: NaiveDateTime,
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

    if args.get(1).is_some_and(|arg| arg == "signal") {
        let mut event = event_from_args(&args);
        let payload = signal_payload(&args, &event);
        event.session = session_from_payload(&payload);
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
            size_percent: Mutex::new(load_size_percent()),
            reminder_settings: Mutex::new(load_reminder_settings()),
            pending_reminders: Mutex::new(Vec::new()),
        })
        .plugin(tauri_plugin_single_instance::init(
            move |app, args, _cwd| {
                if args
                    .get(1)
                    .is_some_and(|arg| arg == "signal" || arg == LAUNCH_WITH_EVENT_COMMAND)
                {
                    emit_event(app, event_from_args(&args));
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
            start_reminders(app.handle().clone());

            if let Some(event) = startup_event.as_ref() {
                emit_event(app.handle(), event.clone());
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = resize_window(&window, current_size_percent(app.handle()));
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
            toggle_ghost_mode,
            pending_reminders,
            dismiss_reminder
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
    window.hide().map_err(|error| error.to_string())?;
    update_visibility_menu_item(window.app_handle(), false);
    Ok(())
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
    let window_size = window
        .outer_size()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale);
    let x = (size.width as f64 / scale - window_size.width - 55.0).max(0.0) as i32;
    let y = (size.height as f64 / scale - window_size.height - 102.0).max(0.0) as i32;
    window
        .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
            (x as f64 * scale) as i32,
            (y as f64 * scale) as i32,
        )))
        .map_err(|error| error.to_string())
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let visibility =
        MenuItemBuilder::with_id("visibility", visibility_menu_label(true)).build(app)?;
    let ghost = MenuItemBuilder::with_id("ghost", "Toggle ghost mode").build(app)?;
    let size_increase = MenuItemBuilder::with_id("size_increase", "Increase size").build(app)?;
    let size_decrease = MenuItemBuilder::with_id("size_decrease", "Decrease size").build(app)?;
    let size_reset = MenuItemBuilder::with_id("size_reset", "Reset size").build(app)?;
    let reset = MenuItemBuilder::with_id("reset", "Reset position").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Pip-chan").build(app)?;
    let mut reminders = Vec::with_capacity(Reminder::ALL.len());
    for reminder in Reminder::ALL {
        let item = CheckMenuItemBuilder::with_id(reminder.menu_id(), reminder.menu_label())
            .checked(reminder_enabled(app.handle(), reminder))
            .build(app)?;
        reminders.push((reminder, item));
    }

    let mut builder = MenuBuilder::new(app).items(&[
        &visibility,
        &ghost,
        &size_increase,
        &size_decrease,
        &size_reset,
        &reset,
    ]);
    for (_, item) in &reminders {
        builder = builder.item(item);
    }
    let menu = builder.item(&quit).build()?;
    app.manage(VisibilityMenuItem(visibility));
    app.manage(ReminderMenuItems(reminders));
    let app_handle = app.handle().clone();
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

    TrayIconBuilder::with_id("pip-chan")
        .icon(icon)
        .tooltip("Pip-chan")
        .menu(&menu)
        .on_menu_event(move |_tray, event| match event.id().as_ref() {
            "visibility" => toggle_window_visibility(&app_handle),
            "ghost" => toggle_ghost_mode(app_handle.clone()),
            "reset" => {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = reset_position(window);
                }
            }
            "size_increase" => set_size_percent(
                &app_handle,
                current_size_percent(&app_handle) + SIZE_STEP_PERCENT,
            ),
            "size_decrease" => set_size_percent(
                &app_handle,
                current_size_percent(&app_handle).saturating_sub(SIZE_STEP_PERCENT),
            ),
            "size_reset" => set_size_percent(&app_handle, DEFAULT_SIZE_PERCENT),
            "quit" => app_handle.exit(0),
            id => {
                if let Some(reminder) = Reminder::ALL.into_iter().find(|r| r.menu_id() == id) {
                    toggle_reminder(&app_handle, reminder);
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn emit_event(app: &tauri::AppHandle, event: PipEvent) {
    let _ = app.emit("pip:event", event);
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        update_visibility_menu_item(app, true);
    }
}

fn toggle_window_visibility(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = hide_window(window);
    } else {
        show_window(app);
    }
}

fn visibility_menu_label(visible: bool) -> &'static str {
    if visible {
        "Hide Pip-chan"
    } else {
        "Show Pip-chan"
    }
}

fn update_visibility_menu_item(app: &tauri::AppHandle, visible: bool) {
    if let Some(item) = app.try_state::<VisibilityMenuItem>() {
        let _ = item.0.set_text(visibility_menu_label(visible));
    }
}

fn current_size_percent(app: &tauri::AppHandle) -> u32 {
    app.state::<PipState>()
        .size_percent
        .lock()
        .map(|percent| *percent)
        .unwrap_or(DEFAULT_SIZE_PERCENT)
}

fn set_size_percent(app: &tauri::AppHandle, percent: u32) {
    let percent = percent.clamp(MIN_SIZE_PERCENT, MAX_SIZE_PERCENT);
    let state = app.state::<PipState>();
    let Ok(mut current) = state.size_percent.lock() else {
        return;
    };
    if *current == percent {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if resize_window_keeping_feet_in_place(&window, percent).is_ok() {
        *current = percent;
        save_size_percent(percent);
    }
}

fn window_size(percent: u32) -> LogicalSize<f64> {
    let scale = percent as f64 / 100.0;
    let round_to_multiple_of_8 = |length: f64| (length / 8.0).round() * 8.0;
    LogicalSize::new(
        round_to_multiple_of_8(BASE_WINDOW_WIDTH * scale),
        round_to_multiple_of_8(BASE_WINDOW_HEIGHT * scale),
    )
}

fn resize_window(window: &tauri::WebviewWindow, percent: u32) -> tauri::Result<()> {
    window.set_size(window_size(percent))?;
    window.set_zoom(percent as f64 / 100.0)
}

fn resize_window_keeping_feet_in_place(
    window: &tauri::WebviewWindow,
    percent: u32,
) -> tauri::Result<()> {
    let old_position = window.outer_position()?;
    let old_size = window.outer_size()?;
    resize_window(window, percent)?;
    let new_size = window_size(percent).to_physical::<i32>(window.scale_factor()?);
    window.set_position(PhysicalPosition::new(
        old_position.x + (old_size.width as i32 - new_size.width) / 2,
        old_position.y + old_size.height as i32 - new_size.height,
    ))
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

fn reminder_enabled(app: &tauri::AppHandle, reminder: Reminder) -> bool {
    app.state::<PipState>()
        .reminder_settings
        .lock()
        .map(|settings| !settings.disabled.contains(reminder.key()))
        .unwrap_or(true)
}

fn toggle_reminder(app: &tauri::AppHandle, reminder: Reminder) {
    let enabled = {
        let state = app.state::<PipState>();
        let Ok(mut settings) = state.reminder_settings.lock() else {
            return;
        };
        let enabled = settings.disabled.remove(reminder.key());
        if !enabled {
            settings.disabled.insert(reminder.key().to_string());
        }
        save_reminder_settings(&settings);
        enabled
    };
    refresh_reminders(app);
    if let Some(items) = app.try_state::<ReminderMenuItems>() {
        for (each, item) in &items.0 {
            if *each == reminder {
                let _ = item.set_checked(enabled);
            }
        }
    }
}

#[tauri::command]
fn pending_reminders(state: tauri::State<'_, PipState>) -> Vec<PendingReminder> {
    state
        .pending_reminders
        .lock()
        .map(|pending| pending.clone())
        .unwrap_or_default()
}

#[tauri::command]
fn dismiss_reminder(app: tauri::AppHandle, kind: String, time: NaiveDateTime) {
    if !Reminder::ALL.iter().any(|reminder| reminder.key() == kind) {
        return;
    }
    if let Ok(mut settings) = app.state::<PipState>().reminder_settings.lock() {
        let dismissed = settings.dismissed.entry(kind).or_insert(time);
        *dismissed = (*dismissed).max(time);
        save_reminder_settings(&settings);
    }
    refresh_reminders(&app);
}

fn start_reminders(app: tauri::AppHandle) {
    thread::spawn(move || loop {
        refresh_reminders(&app);
        thread::sleep(REMINDER_CHECK_INTERVAL);
    });
}

fn refresh_reminders(app: &tauri::AppHandle) {
    let state = app.state::<PipState>();
    let (Ok(settings), Ok(mut pending)) = (
        state.reminder_settings.lock(),
        state.pending_reminders.lock(),
    ) else {
        return;
    };
    let due = due_reminders(Local::now().naive_local(), &settings, &pending);
    if *pending != due {
        *pending = due;
        let _ = app.emit("pip:reminders", &*pending);
    }
}

fn due_reminders(
    now: NaiveDateTime,
    settings: &ReminderSettings,
    published: &[PendingReminder],
) -> Vec<PendingReminder> {
    let mut due: Vec<_> = Reminder::ALL
        .into_iter()
        .filter(|reminder| !settings.disabled.contains(reminder.key()))
        .filter_map(|reminder| {
            let time = reminder.latest_time(now)?;
            let dismissed = settings
                .dismissed
                .get(reminder.key())
                .is_some_and(|dismissed| *dismissed >= time);
            let fresh = now - time < reminder.max_age();
            let already_published = published
                .iter()
                .any(|pending| pending.kind == reminder.key() && pending.time == time);
            (!dismissed && (fresh || already_published)).then_some(PendingReminder {
                kind: reminder.key(),
                message: reminder.message(),
                time,
            })
        })
        .collect();
    due.sort_by_key(|reminder| reminder.time);
    due
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
                        emit_event(
                            &app,
                            PipEvent {
                                source: event.source,
                                event: event.event,
                                session: event.session,
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
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(LAUNCH_WITH_EVENT_COMMAND)
        .arg("--source")
        .arg(&event.source)
        .arg("--event")
        .arg(&event.event);
    if let Some(session) = &event.session {
        command.arg("--session").arg(session);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn event_from_args(args: &[String]) -> PipEvent {
    let source = option_value(args, "--source").unwrap_or_else(|| "test".to_string());
    let event = option_value(args, "--event").unwrap_or_else(|| "ready".to_string());
    let session = option_value(args, "--session");
    PipEvent {
        source,
        event,
        session,
    }
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

fn session_from_payload(payload: &str) -> Option<String> {
    const SESSION_KEYS: [&str; 2] = ["session_id", "thread-id"];
    if let Ok(JsonValue::Object(fields)) = serde_json::from_str::<JsonValue>(payload) {
        return SESSION_KEYS
            .iter()
            .find_map(|key| fields.get(*key).and_then(JsonValue::as_str))
            .map(str::to_string);
    }
    SESSION_KEYS
        .iter()
        .find_map(|key| string_field_prefix(payload, key))
}

fn string_field_prefix(payload: &str, key: &str) -> Option<String> {
    let after_key = &payload[payload.find(&format!("\"{key}\""))? + key.len() + 2..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let value = after_colon.strip_prefix('"')?;
    Some(value[..value.find('"')?].to_string())
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

fn size_path() -> PathBuf {
    pip_dir().join("size.json")
}

fn load_size_percent() -> u32 {
    fs::read_to_string(size_path())
        .ok()
        .and_then(|contents| serde_json::from_str::<JsonValue>(&contents).ok())
        .and_then(|size| size.get("percent").and_then(JsonValue::as_u64))
        .map_or(DEFAULT_SIZE_PERCENT, |percent| {
            percent.clamp(MIN_SIZE_PERCENT.into(), MAX_SIZE_PERCENT.into()) as u32
        })
}

fn save_size_percent(percent: u32) {
    let _ = fs::create_dir_all(pip_dir());
    let _ = fs::write(size_path(), json!({ "percent": percent }).to_string());
}

fn reminders_path() -> PathBuf {
    pip_dir().join("reminders.json")
}

fn load_reminder_settings() -> ReminderSettings {
    fs::read_to_string(reminders_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_reminder_settings(settings: &ReminderSettings) {
    let _ = fs::create_dir_all(pip_dir());
    if let Ok(contents) = serde_json::to_string(settings) {
        let _ = fs::write(reminders_path(), contents);
    }
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

const STATE_HOOKS: [(&str, Option<&str>, &str); 7] = [
    ("UserPromptSubmit", None, "thinking"),
    ("PermissionRequest", None, "attention"),
    ("PostToolUse", None, "thinking"),
    ("PreCompact", None, "thinking"),
    ("PostCompact", Some("manual"), "idle"),
    ("PostCompact", Some("auto"), "thinking"),
    ("SessionEnd", None, "idle"),
];

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

    for (event, matcher, state) in STATE_HOOKS {
        add_codex_hook(
            &mut document,
            event,
            matcher,
            shell_command(&pip_command("codex", state)?),
        )?;
    }

    backup_then_write(&config, &existing, &document.to_string())?;
    println!("Connected Codex to Pip-chan. Open `/hooks` in Codex to review and trust the Pip-chan hooks.");
    Ok(())
}

fn add_codex_hook(
    document: &mut DocumentMut,
    event: &str,
    matcher: Option<&str>,
    command: String,
) -> Result<(), String> {
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
        let entry_matcher = entry
            .get("matcher")
            .and_then(Item::as_value)
            .and_then(Value::as_str);
        if entry_matcher != matcher {
            continue;
        }
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
    if let Some(matcher) = matcher {
        entry["matcher"] = value(matcher);
    }
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

    for (event, matcher, state) in STATE_HOOKS {
        add_claude_hook(
            hooks,
            event,
            matcher,
            shell_command(&pip_command("claude", state)?),
        );
    }
    add_claude_hook(
        hooks,
        "Stop",
        None,
        shell_command(&pip_command("claude", "ready")?),
    );
    remove_claude_hook(
        hooks,
        "Notification",
        Some("permission_prompt"),
        &shell_command(&pip_command("claude", "attention")?),
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

fn remove_claude_hook(
    hooks: &mut Map<String, JsonValue>,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) {
    let Some(entries) = hooks.get_mut(event).and_then(JsonValue::as_array_mut) else {
        return;
    };
    for entry in entries.iter_mut() {
        if entry.get("matcher").and_then(JsonValue::as_str) != matcher {
            continue;
        }
        if let Some(commands) = entry.get_mut("hooks").and_then(JsonValue::as_array_mut) {
            commands
                .retain(|hook| hook.get("command").and_then(JsonValue::as_str) != Some(command));
        }
    }
    entries.retain(|entry| {
        entry
            .get("hooks")
            .and_then(JsonValue::as_array)
            .is_none_or(|commands| !commands.is_empty())
    });
    if entries.is_empty() {
        hooks.remove(event);
    }
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

        add_codex_hook(&mut document, "UserPromptSubmit", None, command.clone()).unwrap();
        add_codex_hook(&mut document, "UserPromptSubmit", None, command).unwrap();

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
    fn codex_hooks_are_kept_apart_by_matcher() {
        let mut document = "".parse::<DocumentMut>().unwrap();
        let idle = "'pip-chan' 'signal' '--event' 'idle'".to_string();
        let thinking = "'pip-chan' 'signal' '--event' 'thinking'".to_string();

        add_codex_hook(&mut document, "PostCompact", Some("manual"), idle.clone()).unwrap();
        add_codex_hook(&mut document, "PostCompact", Some("auto"), thinking).unwrap();
        add_codex_hook(&mut document, "PostCompact", Some("manual"), idle).unwrap();

        let entries = document["hooks"]["PostCompact"]
            .as_array_of_tables()
            .unwrap();
        let matchers = entries
            .iter()
            .map(|entry| entry["matcher"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(matchers, ["manual", "auto"]);
    }

    #[test]
    fn claude_hook_removal_keeps_other_hooks() {
        let mut hooks = json!({
            "Notification": [
                {
                    "matcher": "permission_prompt",
                    "hooks": [
                        { "type": "command", "command": "pip" },
                        { "type": "command", "command": "other" }
                    ]
                },
                { "matcher": "idle_prompt", "hooks": [{ "type": "command", "command": "pip" }] }
            ],
            "Stop": [{ "hooks": [{ "type": "command", "command": "pip" }] }]
        })
        .as_object()
        .unwrap()
        .clone();

        remove_claude_hook(&mut hooks, "Notification", Some("permission_prompt"), "pip");
        assert_eq!(
            hooks["Notification"],
            json!([
                { "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "other" }] },
                { "matcher": "idle_prompt", "hooks": [{ "type": "command", "command": "pip" }] }
            ])
        );

        remove_claude_hook(&mut hooks, "Stop", None, "pip");
        assert!(!hooks.contains_key("Stop"));
    }

    #[test]
    fn sessions_come_from_hook_and_notify_payloads() {
        assert_eq!(
            session_from_payload(r#"{"session_id":"abc","hook_event_name":"Stop"}"#),
            Some("abc".to_string())
        );
        assert_eq!(
            session_from_payload(r#"{"type":"agent-turn-complete","thread-id":"thr"}"#),
            Some("thr".to_string())
        );
        assert_eq!(
            session_from_payload(r#"{"session_id": "abc", "tool_response": "trunc"#),
            Some("abc".to_string())
        );
        assert_eq!(session_from_payload(""), None);
        assert_eq!(session_from_payload(r#"{"session_id":7}"#), None);
    }

    #[test]
    fn only_ready_codex_payloads_are_forwarded() {
        let ready = PipEvent {
            source: "codex".to_string(),
            event: "ready".to_string(),
            session: None,
        };
        let thinking = PipEvent {
            source: "codex".to_string(),
            event: "thinking".to_string(),
            session: None,
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

    fn day(day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    fn at(hour: u32, minute: u32) -> NaiveDateTime {
        day(29, hour, minute)
    }

    fn settings(disabled: &[&str], dismissed: &[(&str, NaiveDateTime)]) -> ReminderSettings {
        ReminderSettings {
            disabled: disabled.iter().map(|kind| kind.to_string()).collect(),
            dismissed: dismissed
                .iter()
                .map(|(kind, time)| (kind.to_string(), *time))
                .collect(),
        }
    }

    fn due(
        now: NaiveDateTime,
        settings: &ReminderSettings,
        published: &[PendingReminder],
    ) -> Vec<(&'static str, NaiveDateTime)> {
        due_reminders(now, settings, published)
            .into_iter()
            .map(|reminder| (reminder.kind, reminder.time))
            .collect()
    }

    #[test]
    fn reminders_are_pending_after_a_late_start() {
        assert_eq!(
            due(at(11, 5), &settings(&[], &[]), &[]),
            [("eyes", at(10, 55)), ("water", at(11, 0))]
        );
        assert_eq!(
            due(day(30, 0, 5), &settings(&[], &[]), &[]),
            [("eyes", day(29, 23, 55))]
        );
    }

    #[test]
    fn unpublished_reminders_expire_after_their_max_age() {
        let water_only = settings(&["eyes"], &[]);
        let eyes_only = settings(&["water"], &[]);

        assert_eq!(due(at(12, 59), &water_only, &[]), [("water", at(11, 0))]);
        assert_eq!(due(at(13, 0), &water_only, &[]), []);
        assert_eq!(due(at(10, 9), &eyes_only, &[]), [("eyes", at(9, 55))]);
        assert_eq!(due(at(10, 10), &eyes_only, &[]), []);
    }

    #[test]
    fn published_reminders_stay_until_dismissed() {
        let eyes_only = settings(&["water"], &[]);
        let published = due_reminders(at(10, 55), &eyes_only, &[]);

        assert_eq!(
            due(at(11, 30), &eyes_only, &published),
            [("eyes", at(10, 55))]
        );
        assert_eq!(
            due(at(11, 55), &eyes_only, &published),
            [("eyes", at(11, 55))]
        );
    }

    #[test]
    fn dismissed_reminders_return_at_their_next_time() {
        let dismissed = settings(&["eyes"], &[("water", at(11, 0))]);

        assert_eq!(due(at(11, 5), &dismissed, &[]), []);
        assert_eq!(due(at(14, 0), &dismissed, &[]), [("water", at(14, 0))]);
    }

    #[test]
    fn reminder_settings_read_older_files_and_store_local_times() {
        let older: ReminderSettings = serde_json::from_str(r#"{"disabled":["eyes"]}"#).unwrap();
        assert!(older.disabled.contains("eyes"));
        assert!(older.dismissed.is_empty());

        let stored = serde_json::to_string(&settings(&[], &[("water", at(11, 0))])).unwrap();
        assert_eq!(
            stored,
            r#"{"disabled":[],"dismissed":{"water":"2026-09-29T11:00:00"}}"#
        );
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
