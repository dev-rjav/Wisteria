//! Wisteria desktop app (Tauri v2). Wraps the dictation engine in the Flow App workspace UI.
//!
//! The Rust side owns the [`Engine`] and exposes commands the frontend calls to read/write every
//! setting, enumerate mic devices, manage Ollama formatting models (list installed + curated
//! downloadable, and pull them), and toggle dictation. Engine progress is pushed to the webview
//! as `engine-event`; model-download progress as `model-pull`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ollama;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tracing::info;

use wisteria_core::config::Config;
use wisteria_core::engine::{Engine, EngineEvent, EventSink, Phase};

/// Shared app state managed by Tauri.
struct AppState {
    engine: Mutex<Option<Engine>>,
    config_path: PathBuf,
    gui_state_path: PathBuf,
    /// Dedicated, append-only dictation history + all-time counters (separate from gui-state so a
    /// settings save can never clobber it).
    history_path: PathBuf,
    /// Last known engine phase tag, so the UI can query it on load.
    phase: Arc<Mutex<String>>,
    /// Cancel flags for in-flight model pulls, keyed by model name.
    pulls: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

// ---------- config ----------

#[tauri::command]
fn get_config(state: State<AppState>) -> Result<Config, String> {
    Config::load_or_create(&state.config_path).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_config(state: State<AppState>, config: Config) -> Result<(), String> {
    config.save(&state.config_path).map_err(|e| e.to_string())?;
    if let Some(engine) = state.engine.lock().unwrap().as_mut() {
        engine.reload(config);
    }
    Ok(())
}

/// The built-in default formatter prompt, so the Settings editor can show and reset to it.
#[tauri::command]
fn default_formatter_prompt() -> String {
    wisteria_core::format::default_prompt()
}

/// The exact system prompt the model would receive for `config` (composed from the transform
/// toggles, intensity, custom prompt, and guard). Powers the live "effective prompt" preview so the
/// user can verify a toggle actually changes what's sent. Pure function of the passed config — takes
/// the frontend's current (possibly just-toggled) config so the preview updates instantly.
#[tauri::command]
fn effective_prompt(config: Config) -> String {
    wisteria_core::format::effective_system_prompt(&config)
}

// ---------- devices ----------

#[tauri::command]
fn list_input_devices() -> Vec<String> {
    wisteria_core::audio::input_device_names()
}

// ---------- models ----------

#[derive(Serialize)]
struct FormatterModels {
    reachable: bool,
    selected: String,
    models: Vec<ollama::ModelEntry>,
}

#[tauri::command]
fn list_formatter_models(state: State<AppState>) -> FormatterModels {
    let config = Config::load_or_create(&state.config_path).unwrap_or_default();
    match ollama::installed(&config.formatter_url) {
        Ok(tags) => FormatterModels {
            reachable: true,
            selected: config.formatter_model,
            models: ollama::model_list(&tags),
        },
        Err(_) => FormatterModels {
            reachable: false,
            selected: config.formatter_model,
            // Still show the curated list so the user knows what's downloadable.
            models: ollama::model_list(&[]),
        },
    }
}

#[derive(Serialize)]
struct TranscriptionModel {
    name: String,
    label: String,
    installed: bool,
    note: String,
}

#[tauri::command]
fn list_transcription_models(state: State<AppState>) -> Vec<TranscriptionModel> {
    let config = Config::load_or_create(&state.config_path).unwrap_or_default();
    // Only Parakeet is wired for now; it's auto-downloaded on first run.
    let parakeet_present = wisteria_core::models::models_dir()
        .map(|d| d.join("parakeet-tdt-0.6b-v3-int8").join("vocab.txt").exists())
        .unwrap_or(false);
    vec![
        TranscriptionModel {
            name: "parakeet-tdt-0.6b-v3-int8".into(),
            label: "Parakeet TDT 0.6B v3".into(),
            installed: parakeet_present || config.model == "parakeet-tdt-0.6b-v3-int8",
            note: "Fast local ASR, 25 languages. Auto-downloads on first use.".into(),
        },
        TranscriptionModel {
            name: "whisper-large-v3-turbo".into(),
            label: "Whisper Large v3 Turbo".into(),
            installed: false,
            note: "Multilingual Whisper (coming soon).".into(),
        },
    ]
}

#[tauri::command]
fn pull_model(app: AppHandle, state: State<AppState>, name: String) {
    let url = Config::load_or_create(&state.config_path)
        .unwrap_or_default()
        .formatter_url;
    // Register a fresh cancel flag for this pull (replacing any stale one for the same model).
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .pulls
        .lock()
        .unwrap()
        .insert(name.clone(), Arc::clone(&cancel));

    std::thread::spawn(move || {
        let cancel_for_pull = Arc::clone(&cancel);
        ollama::pull(&url, &name, cancel_for_pull, |progress| {
            let _ = app.emit("model-pull", progress);
        });
        // Clean up the registry entry once the pull ends (success, error, or cancel).
        if let Some(state) = app.try_state::<AppState>() {
            state.pulls.lock().unwrap().remove(&name);
        }
    });
}

/// Cancel an in-flight model download by setting its cancel flag; the pull thread drops the
/// connection and emits a final `cancelled` progress.
#[tauri::command]
fn cancel_pull(state: State<AppState>, name: String) {
    if let Some(flag) = state.pulls.lock().unwrap().get(&name) {
        flag.store(true, Ordering::SeqCst);
    }
}

// ---------- engine ----------

#[derive(Serialize)]
struct EngineStatus {
    enabled: bool,
    phase: String,
}

#[tauri::command]
fn engine_status(state: State<AppState>) -> EngineStatus {
    let enabled = state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .map(|e| e.is_enabled())
        .unwrap_or(false);
    let phase = state.phase.lock().unwrap().clone();
    EngineStatus { enabled, phase }
}

#[tauri::command]
fn engine_set_enabled(state: State<AppState>, on: bool) {
    if let Some(engine) = state.engine.lock().unwrap().as_mut() {
        engine.set_enabled(on);
    }
}

// ---------- persistence helpers ----------

fn read_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Write `text` to `path` atomically (temp file + rename) so a crash or an OS shutdown mid-write
/// can never leave a truncated/corrupt file — the old file stays until the new one is complete.
fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Load the history document, preferring history.json and falling back to legacy fields still in
/// gui-state.json (pre-split installs).
fn load_history_value(history_path: &Path, gui_state_path: &Path) -> serde_json::Value {
    if let Some(v) = read_json(history_path) {
        return v;
    }
    let legacy = read_json(gui_state_path).unwrap_or_else(|| serde_json::json!({}));
    serde_json::json!({
        "history": legacy.get("history").cloned().unwrap_or_else(|| serde_json::json!([])),
        "totalWords": legacy.get("totalWords").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "totalDictations": legacy.get("totalDictations").cloned().unwrap_or_else(|| serde_json::json!(0)),
    })
}

/// One-time migration: if history.json doesn't exist yet but gui-state.json carries legacy history,
/// write it out so a later settings-only save of gui-state can't drop it.
fn migrate_history(history_path: &Path, gui_state_path: &Path) {
    if history_path.exists() {
        return;
    }
    let legacy = match read_json(gui_state_path) {
        Some(v) if v.get("history").is_some() => v,
        _ => return,
    };
    let v = serde_json::json!({
        "history": legacy.get("history").cloned().unwrap_or_else(|| serde_json::json!([])),
        "totalWords": legacy.get("totalWords").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "totalDictations": legacy.get("totalDictations").cloned().unwrap_or_else(|| serde_json::json!(0)),
    });
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = atomic_write(history_path, &s);
    }
}

// ---------- GUI-only state (dictionary, snippets, scratchpad, style, transforms) ----------

#[tauri::command]
fn get_gui_state(state: State<AppState>) -> serde_json::Value {
    read_json(&state.gui_state_path).unwrap_or_else(|| serde_json::json!({}))
}

#[tauri::command]
fn save_gui_state(state: State<AppState>, data: serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    atomic_write(&state.gui_state_path, &text).map_err(|e| e.to_string())
}

// ---------- dictation history (append-only, Rust-owned) ----------

#[tauri::command]
fn get_history(state: State<AppState>) -> serde_json::Value {
    load_history_value(&state.history_path, &state.gui_state_path)
}

/// Append one dictation to history and bump the all-time counters, then persist atomically. Doing
/// the read-modify-write in Rust means the frontend can never overwrite the whole history with a
/// stale in-memory copy (the bug that lost history across restarts).
#[tauri::command]
fn append_history(state: State<AppState>, time: String, text: String, words: u64) -> Result<(), String> {
    const CAP: usize = 500;
    let mut v = load_history_value(&state.history_path, &state.gui_state_path);
    let obj = v.as_object_mut().ok_or("history document is not an object")?;

    let hist = obj
        .entry("history")
        .or_insert_with(|| serde_json::json!([]));
    if let Some(arr) = hist.as_array_mut() {
        arr.insert(0, serde_json::json!({ "time": time, "text": text }));
        if arr.len() > CAP {
            arr.truncate(CAP);
        }
    }

    let total_words = obj.get("totalWords").and_then(|x| x.as_u64()).unwrap_or(0) + words;
    let total_dict = obj.get("totalDictations").and_then(|x| x.as_u64()).unwrap_or(0) + 1;
    obj.insert("totalWords".into(), serde_json::json!(total_words));
    obj.insert("totalDictations".into(), serde_json::json!(total_dict));

    let text_out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    atomic_write(&state.history_path, &text_out).map_err(|e| e.to_string())
}

/// Show and focus the main window (from the tray).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Toggle dictation on/off (from the tray).
fn toggle_engine(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut guard = state.engine.lock().unwrap();
    if let Some(e) = guard.as_mut() {
        let now = !e.is_enabled();
        e.set_enabled(now);
    }
}

/// Position the frameless dock at the bottom-center of the primary monitor.
fn position_dock(app: &AppHandle) {
    if let Some(dock) = app.get_webview_window("dock") {
        if let Ok(Some(monitor)) = dock.primary_monitor() {
            let scale = monitor.scale_factor();
            let size = monitor.size();
            let lw = size.width as f64 / scale;
            let lh = size.height as f64 / scale;
            // Matches the dock's tiny idle window (see SIZES.idle in dock.js). This uses the full
            // monitor height, so leave room for the taskbar (~48px) plus a gap; dock.js refines
            // the position using the work area once the webview loads.
            let (win_w, win_h) = (96.0, 34.0);
            let x = (lw - win_w) / 2.0;
            let y = lh - win_h - 100.0;
            let _ = dock.set_position(tauri::LogicalPosition::new(x, y));
        }
        let _ = dock.set_always_on_top(true);
    }
}

/// Build the system-tray icon + menu so Wisteria keeps running in the background.
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Wisteria", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Enable / disable dictation", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wisteria", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &quit])?;

    TrayIconBuilder::with_id("wisteria-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Wisteria — hold your hotkey to dictate")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "toggle" => toggle_engine(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        // Single-instance MUST be the first plugin registered. When the user launches Wisteria
        // again while it's already running (it lives in the tray after the window is closed), this
        // fires in the *existing* process — we just re-show its window — and the second process
        // exits. That prevents a duplicate dock, tray icon, and engine from spawning.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // Closing a window hides it (app keeps running in the tray); it never quits the app.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            setup_tray(&handle)?;
            position_dock(&handle);
            let data_dir = Config::app_data_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config_path = data_dir.join("config.toml");
            let gui_state_path = data_dir.join("gui-state.json");
            let history_path = data_dir.join("history.json");
            let phase = Arc::new(Mutex::new(Phase::Warming.tag().to_string()));

            // Split any legacy history out of gui-state.json into its own file before the frontend
            // starts saving settings (which no longer carry history).
            migrate_history(&history_path, &gui_state_path);

            let config = Config::load_or_create(&config_path).unwrap_or_default();

            // Register state FIRST so frontend commands resolve immediately. Engine::start below
            // loads the ASR model (several seconds); the webview calls get_config/engine_status as
            // soon as it loads. If state weren't managed yet those calls would error and — before
            // the frontend was hardened — the whole UI came up blank. Managing early is the real fix.
            app.manage(AppState {
                engine: Mutex::new(None),
                config_path,
                gui_state_path,
                history_path,
                phase: Arc::clone(&phase),
                pulls: Mutex::new(HashMap::new()),
            });

            // Engine event sink → webview (global emit reaches both windows) + phase cache.
            let phase_for_sink = Arc::clone(&phase);
            let sink: EventSink = Arc::new(move |ev: EngineEvent| {
                let payload = match &ev {
                    EngineEvent::Phase(p) => {
                        *phase_for_sink.lock().unwrap() = p.tag().to_string();
                        serde_json::json!({ "kind": "phase", "phase": p.tag() })
                    }
                    EngineEvent::Transcript { raw, clean, ms, words } => serde_json::json!({
                        "kind": "transcript", "raw": raw, "clean": clean, "ms": ms, "words": words
                    }),
                    EngineEvent::Error(m) => serde_json::json!({ "kind": "error", "message": m }),
                };
                let _ = handle.emit("engine-event", payload);
            });

            let engine = Engine::start(config, sink);
            app.state::<AppState>().engine.lock().unwrap().replace(engine);
            info!("Wisteria GUI started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            default_formatter_prompt,
            effective_prompt,
            list_input_devices,
            list_formatter_models,
            list_transcription_models,
            pull_model,
            cancel_pull,
            engine_status,
            engine_set_enabled,
            get_gui_state,
            save_gui_state,
            get_history,
            append_history,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Wisteria")
        .run(|_app, event| {
            // Keep running when windows are hidden; only the tray "Quit" exits (via app.exit).
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
