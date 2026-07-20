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
use tauri::{AppHandle, Emitter, Manager, State};
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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = Config::app_data_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config_path = data_dir.join("config.toml");
            let gui_state_path = data_dir.join("gui-state.json");
            let phase = Arc::new(Mutex::new(Phase::Warming.tag().to_string()));

            let config = Config::load_or_create(&config_path).unwrap_or_default();

            // Engine event sink → webview + phase cache.
            let handle = app.handle().clone();
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

            app.manage(AppState {
                engine: Mutex::new(Some(engine)),
                config_path,
                gui_state_path,
                phase,
            });
            info!("Wisteria GUI started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_input_devices,
            list_formatter_models,
            list_transcription_models,
            pull_model,
            engine_status,
            engine_set_enabled,
            get_gui_state,
            save_gui_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wisteria");
}
