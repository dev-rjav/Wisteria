//! Wisteria desktop app (Tauri v2). Wraps the dictation engine in the workspace UI.
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
    /// Directory holding per-dictation WAV recordings (for history replay / re-transcription).
    recordings_dir: PathBuf,
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

// ---------- dictionary import/export ----------

/// Write the current dictionary to `path` as a plain word list (one entry per line, with a header
/// comment). Round-trips with [`import_dictionary`].
#[tauri::command]
fn export_dictionary(state: State<AppState>, path: String) -> Result<(), String> {
    let config = Config::load_or_create(&state.config_path).map_err(|e| e.to_string())?;
    let mut out = String::from("# Wisteria dictionary — one word or phrase per line. Lines starting with # are ignored.\n");
    for word in &config.dictionary {
        out.push_str(word);
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| format!("writing {path}: {e}"))
}

/// Read a word list from `path` (one entry per line; blank lines and `#` comments ignored) and
/// return the parsed words. Merging with the existing dictionary is done on the frontend so it uses
/// the same save/dedupe path as manual edits.
#[tauri::command]
fn import_dictionary(path: String) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let words = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();
    Ok(words)
}

// ---------- issue reports (temporary testing-phase feature) ----------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportForm {
    name: String,
    email: String,
    kind: String,
    severity: String,
    title: String,
    description: String,
    steps: String,
}

/// The issue-report ingestion endpoint: a Google Apps Script web app (`doPost`) that appends each
/// report as a row in a Google Sheet. It's a public client endpoint (it ships inside every binary
/// anyway and the deployment is set to "Anyone"), so it's baked in directly rather than treated as
/// a build secret — that's what makes reporting work in the distributed builds. A build can still
/// override it via the `WISTERIA_REPORT_URI` env var (e.g. to point at a staging sheet).
const DEFAULT_REPORT_URI: &str = "https://script.google.com/macros/s/AKfycbwz-ur0mPb-tZ154p6z4Wms547FySbL8RwUCxIIy05FAMv8ejnuxZtKkoo4G4OjXgYF/exec";

/// Ship a tester's issue report to the Apps Script → Google Sheet endpoint as JSON (the script
/// reads `JSON.parse(e.postData.contents)` and the keys below map 1:1 to its columns).
#[tauri::command]
fn submit_report(report: ReportForm) -> Result<(), String> {
    let baked = option_env!("WISTERIA_REPORT_URI").unwrap_or("").trim();
    let endpoint = if baked.is_empty() { DEFAULT_REPORT_URI } else { baked };
    if report.name.trim().is_empty()
        || report.title.trim().is_empty()
        || report.description.trim().is_empty()
    {
        return Err("Please fill in your name, a title, and a description.".into());
    }
    let payload = serde_json::json!({
        "name": report.name.trim(),
        "email": report.email.trim(),
        "type": report.kind,
        "severity": report.severity,
        "title": report.title.trim(),
        "description": report.description.trim(),
        "steps": report.steps.trim(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
    });
    let endpoint = endpoint.to_string();
    // Run the blocking HTTP on a dedicated thread: `reqwest::blocking` panics if it's called from
    // within Tauri's async runtime (a command may run there), so isolate it on its own thread.
    let worker = std::thread::spawn(move || -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(&endpoint)
            .json(&payload)
            .send()
            .map_err(|e| format!("could not send: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("server returned {}", resp.status()));
        }
        Ok(())
    });
    match worker.join() {
        Ok(result) => result,
        Err(_) => Err("the report request crashed".to_string()),
    }
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

/// The `data[].id` list from an OpenAI-compatible `GET /models` response (OpenRouter, OpenAI,
/// Groq, …). Extra fields are ignored.
#[derive(serde::Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelObj>,
}

#[derive(serde::Deserialize)]
struct OpenAiModelObj {
    id: String,
}

/// List the models a BYOK cloud provider exposes, so the user can pick one instead of typing an id.
/// Hits `GET {base_url}/models` with the (optional) Bearer key. Doubles as a connectivity/key test:
/// a bad URL or key surfaces here as an error string. Runs the blocking HTTP off-thread (reqwest's
/// blocking client must not be created inside Tauri's async runtime).
#[tauri::command]
fn list_api_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("Enter the service's base URL first.".into());
    }
    let key = api_key.trim().to_string();
    let worker = std::thread::spawn(move || -> Result<Vec<String>, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.get(format!("{base}/models"));
        if !key.is_empty() {
            req = req.bearer_auth(&key);
        }
        let resp = req
            .send()
            .map_err(|e| format!("could not reach the service: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "the service returned {} (check the base URL and API key)",
                resp.status()
            ));
        }
        let parsed: OpenAiModelsResponse = resp
            .json()
            .map_err(|e| format!("unexpected response (is this an OpenAI-compatible API?): {e}"))?;
        let mut ids: Vec<String> = parsed
            .data
            .into_iter()
            .map(|m| m.id)
            .filter(|s| !s.trim().is_empty())
            .collect();
        ids.sort();
        if ids.is_empty() {
            return Err("connected, but the service listed no models.".into());
        }
        Ok(ids)
    });
    worker
        .join()
        .map_err(|_| "the request crashed".to_string())?
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

// ---------- in-app updates ----------

/// A pending update the user can choose to install (surfaced to the webview).
#[derive(Serialize)]
struct UpdateInfo {
    /// The version being offered (e.g. "0.1.5").
    version: String,
    /// The currently-running version, for a "0.1.4 → 0.1.5" prompt.
    current: String,
    /// Release notes / changelog body, if the manifest carries one.
    notes: Option<String>,
}

/// Ask the update server whether a newer signed build exists. Returns `None` when up to date.
/// Never errors the UI on a transient network failure beyond the string — the caller treats any
/// error as "couldn't check right now".
#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            current: update.current_version.clone(),
            notes: update.body.clone(),
        })),
        None => Ok(None),
    }
}

/// Download and install the pending update, then relaunch into the new version. Progress is pushed
/// to the webview as `update-progress` events. We re-`check()` here (rather than caching the
/// `Update` between commands) to keep the command stateless and robust across a slow user prompt.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No update is available.")?;

    let mut downloaded: usize = 0;
    let progress_app = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk;
                let _ = progress_app.emit(
                    "update-progress",
                    serde_json::json!({ "downloaded": downloaded, "total": total }),
                );
            },
            {
                let app = app.clone();
                move || {
                    let _ = app.emit("update-progress", serde_json::json!({ "done": true }));
                }
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    // Relaunch into the freshly-installed binary. `restart` never returns.
    app.restart();
}

// ---------- persistence helpers ----------

fn read_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            // Strip a UTF-8 BOM (EF BB BF) that Windows tools like PowerShell 5.1 prepend.
            // serde_json treats the BOM as an invalid token and rejects the whole file, so
            // history/config silently vanishes on affected machines.
            let s = s.strip_prefix('\u{feff}').unwrap_or(&s);
            serde_json::from_str(s).ok()
        })
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
#[allow(clippy::too_many_arguments)]
fn append_history(
    state: State<AppState>,
    time: String,
    text: String,
    raw: String,
    audio: Option<String>,
    words: u64,
    ts: i64,
) -> Result<(), String> {
    const CAP: usize = 500;
    let mut v = load_history_value(&state.history_path, &state.gui_state_path);
    let obj = v.as_object_mut().ok_or("history document is not an object")?;

    let hist = obj
        .entry("history")
        .or_insert_with(|| serde_json::json!([]));
    if let Some(arr) = hist.as_array_mut() {
        // `ts` is epoch milliseconds; it lets the UI group entries by day (Today/Yesterday/date).
        // `time` stays as the human display string for the row. `raw` is the pre-formatting
        // transcript (so history can reformat it) and `audio` the recording filename (for replay /
        // re-transcription); both are absent on legacy rows.
        arr.insert(0, serde_json::json!({ "time": time, "text": text, "raw": raw, "audio": audio, "ts": ts }));
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

/// Update an existing history row (identified by its `ts`) after a reformat or re-transcription.
/// Overwrites the displayed `text`, and the stored `raw` when the transcript itself changed. Leaves
/// the all-time counters untouched — editing a dictation isn't a new dictation.
#[tauri::command]
fn update_history(state: State<AppState>, ts: i64, text: String, raw: Option<String>) -> Result<(), String> {
    let mut v = load_history_value(&state.history_path, &state.gui_state_path);
    let obj = v.as_object_mut().ok_or("history document is not an object")?;
    if let Some(arr) = obj.get_mut("history").and_then(|h| h.as_array_mut()) {
        for item in arr.iter_mut() {
            if item.get("ts").and_then(|t| t.as_i64()) == Some(ts) {
                if let Some(o) = item.as_object_mut() {
                    o.insert("text".into(), serde_json::json!(text));
                    if let Some(r) = &raw {
                        o.insert("raw".into(), serde_json::json!(r));
                    }
                }
                break;
            }
        }
    }
    let text_out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    atomic_write(&state.history_path, &text_out).map_err(|e| e.to_string())
}

/// Result of re-transcribing a stored recording: the fresh raw transcript plus its formatted form.
#[derive(Serialize)]
struct Retranscribed {
    raw: String,
    clean: String,
}

/// A grab of the engine's command handle, released as soon as we have it so the (possibly slow)
/// ASR/LLM round-trip below never holds the engine lock that status polls also need.
fn engine_commander(state: &State<AppState>) -> Result<wisteria_core::engine::EngineCommander, String> {
    state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .map(|e| e.commander())
        .ok_or_else(|| "The dictation engine is still starting — try again in a moment.".to_string())
}

/// Re-run the current formatter (and dictionary/snippet passes) over a stored raw transcript.
#[tauri::command]
fn reformat_transcript(state: State<AppState>, raw: String) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("This dictation has no raw transcript to reformat.".into());
    }
    engine_commander(&state)?.reformat(raw)
}

/// Re-transcribe a stored recording with the warm ASR model, then format the fresh transcript.
#[tauri::command]
fn retranscribe(state: State<AppState>, audio: String) -> Result<Retranscribed, String> {
    let name = safe_recording_name(&audio)?;
    let path = state.recordings_dir.join(name);
    let samples = wisteria_core::audio::read_wav_16k_mono(&path)
        .map_err(|e| format!("couldn't read the recording: {e}"))?;
    let commander = engine_commander(&state)?;
    let raw = commander.transcribe(samples)?;
    let clean = commander.reformat(raw.clone())?;
    Ok(Retranscribed { raw, clean })
}

/// Return a stored recording's WAV bytes so the frontend can play it back.
#[tauri::command]
fn read_recording(state: State<AppState>, audio: String) -> Result<Vec<u8>, String> {
    let name = safe_recording_name(&audio)?;
    std::fs::read(state.recordings_dir.join(name)).map_err(|e| e.to_string())
}

/// Validate an untrusted recording filename from the frontend: it must be a bare filename (no path
/// separators or `..`), so a crafted value can never escape the recordings directory.
fn safe_recording_name(audio: &str) -> Result<String, String> {
    let name = Path::new(audio)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid recording name")?;
    if name != audio || name.is_empty() {
        return Err("invalid recording name".into());
    }
    Ok(name.to_string())
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

/// Clearance (logical px) between the dock's bottom edge and the bottom of the monitor work area,
/// so the pill sits *just above* the taskbar rather than touching it.
const DOCK_BOTTOM_GAP: f64 = 14.0;

/// Place the frameless dock centered horizontally and just above the taskbar, for a given logical
/// window size. Uses the *physical work area* of whichever monitor the dock currently sits on
/// (falling back to the primary), then sets a physical position. This is deliberately independent
/// of the webview's `screen.avail*` values, which WebView2 reports unreliably across DPI scaling
/// and multi-monitor setups — the root cause of the dock landing at a random spot on other
/// machines. `work_area` already excludes the taskbar at any scale, so the same math is correct on
/// 100%, 125%, 150%, … displays.
fn place_dock(app: &AppHandle, width_logical: f64, height_logical: f64) {
    let Some(dock) = app.get_webview_window("dock") else {
        return;
    };
    let monitor = dock
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| dock.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let wa = monitor.work_area();
        let pw = (width_logical * scale).round() as i32;
        let ph = (height_logical * scale).round() as i32;
        let gap = (DOCK_BOTTOM_GAP * scale).round() as i32;
        let x = wa.position.x + (wa.size.width as i32 - pw) / 2;
        let y = wa.position.y + wa.size.height as i32 - ph - gap;
        let _ = dock.set_position(tauri::PhysicalPosition::new(x, y));
    }
    let _ = dock.set_always_on_top(true);
}

/// Position the frameless dock at startup, sized to its tiny idle pill (see SIZES.idle in dock.js).
fn position_dock(app: &AppHandle) {
    place_dock(app, 96.0, 34.0);
}

/// Re-place the dock for the logical size the webview just resized itself to. Called from dock.js
/// after every state change so the pill stays centered and taskbar-anchored as it grows/shrinks —
/// on the correct monitor and at the correct DPI, without trusting `screen.avail*`.
#[tauri::command]
fn place_dock_cmd(app: AppHandle, width: f64, height: f64) {
    place_dock(&app, width, height);
}

/// Shrink + center the main window so it always fits the current monitor's work area, even on
/// small or high-DPI laptop screens where the 1280×820 default would overflow off-screen (which is
/// what makes the layout look "disturbed" on other machines). We cap at ~94% of the available
/// logical work area and re-center using physical coordinates so it's correct at any scale.
fn fit_main_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let monitor = win
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let scale = monitor.scale_factor();
    let wa = monitor.work_area();
    let avail_w = wa.size.width as f64 / scale;
    let avail_h = wa.size.height as f64 / scale;
    // Preferred default, but never larger than the work area (with a small margin).
    let w = 1280.0_f64.min(avail_w * 0.94);
    let h = 820.0_f64.min(avail_h * 0.94);
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    let pw = (w * scale).round() as i32;
    let ph = (h * scale).round() as i32;
    let x = wa.position.x + (wa.size.width as i32 - pw) / 2;
    let y = wa.position.y + (wa.size.height as i32 - ph) / 2;
    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            fit_main_window(&handle);
            let data_dir = Config::app_data_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config_path = data_dir.join("config.toml");
            let gui_state_path = data_dir.join("gui-state.json");
            let history_path = data_dir.join("history.json");
            let recordings_dir = data_dir.join("recordings");
            let _ = std::fs::create_dir_all(&recordings_dir);
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
                recordings_dir,
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
                    EngineEvent::Transcript { raw, clean, ms, words, audio } => serde_json::json!({
                        "kind": "transcript", "raw": raw, "clean": clean, "ms": ms, "words": words, "audio": audio
                    }),
                    EngineEvent::Error(m) => serde_json::json!({ "kind": "error", "message": m }),
                };
                let _ = handle.emit("engine-event", payload);
            });

            let engine = Engine::start(config, sink);
            app.state::<AppState>().engine.lock().unwrap().replace(engine);
            info!("Wisteria GUI started");

            // Silently check for a newer signed build in the background; if one exists, tell the
            // webview so it can offer a non-blocking "Update & restart" prompt. A missing endpoint
            // or offline machine just does nothing (local-first: updates are opt-in, never forced).
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri_plugin_updater::UpdaterExt;
                if let Ok(updater) = update_handle.updater() {
                    if let Ok(Some(update)) = updater.check().await {
                        let _ = update_handle.emit(
                            "update-available",
                            serde_json::json!({
                                "version": update.version,
                                "current": update.current_version,
                                "notes": update.body,
                            }),
                        );
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            default_formatter_prompt,
            effective_prompt,
            export_dictionary,
            import_dictionary,
            submit_report,
            place_dock_cmd,
            list_api_models,
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
            update_history,
            reformat_transcript,
            retranscribe,
            read_recording,
            check_for_update,
            install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Wisteria")
        .run(|_app, event| {
            // Keep running when windows are hidden (implicit exit, `code = None`), so closing the
            // last window just parks Wisteria in the tray. But a deliberate `app.exit(0)` from the
            // tray "Quit" carries a code — let that one through, or the app can never be quit except
            // via Task Manager.
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
