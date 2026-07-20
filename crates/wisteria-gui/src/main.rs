//! Wisteria desktop app (Tauri v2). Wraps the dictation engine in the Flow App workspace UI.
//!
//! The Rust side owns the [`Engine`] and exposes commands the frontend calls to read/write every
//! setting, enumerate mic devices, manage Ollama formatting models (list installed + curated
//! downloadable, and pull them), and toggle dictation. Engine progress is pushed to the webview
//! as `engine-event`; model-download progress as `model-pull`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ollama;

use std::path::PathBuf;
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
    /// Last known engine phase tag, so the UI can query it on load.
    phase: Arc<Mutex<String>>,
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
    wisteria_core::format::default_prompt().to_string()
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
    std::thread::spawn(move || {
        ollama::pull(&url, &name, |progress| {
            let _ = app.emit("model-pull", progress);
        });
    });
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

// ---------- GUI-only state (dictionary, snippets, scratchpad, style, transforms) ----------

#[tauri::command]
fn get_gui_state(state: State<AppState>) -> serde_json::Value {
    std::fs::read_to_string(&state.gui_state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

#[tauri::command]
fn save_gui_state(state: State<AppState>, data: serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    std::fs::write(&state.gui_state_path, text).map_err(|e| e.to_string())
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
            let phase = Arc::new(Mutex::new(Phase::Warming.tag().to_string()));

            let config = Config::load_or_create(&config_path).unwrap_or_default();

            // Register state FIRST so frontend commands resolve immediately. Engine::start below
            // loads the ASR model (several seconds); the webview calls get_config/engine_status as
            // soon as it loads. If state weren't managed yet those calls would error and — before
            // the frontend was hardened — the whole UI came up blank. Managing early is the real fix.
            app.manage(AppState {
                engine: Mutex::new(None),
                config_path,
                gui_state_path,
                phase: Arc::clone(&phase),
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
            list_input_devices,
            list_formatter_models,
            list_transcription_models,
            pull_model,
            engine_status,
            engine_set_enabled,
            get_gui_state,
            save_gui_state,
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
