//! The dictation engine: a self-contained, restartable pipeline that the GUI (or any embedder)
//! drives. It owns the mic recorder, ASR, and formatter on a dedicated worker thread, plus a
//! global push-to-talk listener thread. Lifecycle and config changes are driven by commands, and
//! progress is reported through an [`EventSink`] callback.
//!
//! Design notes:
//! - The push-to-talk key is **grabbed** (consumed) only while the engine is *enabled*; when
//!   disabled the key passes through to the OS. Target keys can change live without restarting
//!   the global hook.
//! - The worker thread owns all non-`Send`-friendly resources (the cpal stream lives and dies on
//!   that thread), so nothing audio-related crosses threads.
//! - Nothing here ever panics the host app: setup/runtime errors are reported as
//!   [`EngineEvent::Error`] and the worker keeps running where it can.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use tracing::{error, info};
#[cfg(target_os = "linux")]
use tracing::warn;

use crate::asr::Asr;
use crate::audio::Recorder;
use crate::config::Config;
use crate::format::Formatter;
use crate::hotkey::{ChordState, PttEvent};
use crate::{models, paste};

/// Live state read by the global-hotkey callback. Guarded by a mutex; updated when settings change.
struct HotState {
    chord: ChordState,
    enabled: bool,
}

/// High-level pipeline state, surfaced to the UI as the dock's mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Engine warming up (loading models).
    Warming,
    /// Ready and idle, waiting for the push-to-talk key.
    Idle,
    /// Recording the user's speech.
    Listening,
    /// Transcribing + formatting.
    Processing,
    /// Engine disabled by the user.
    Disabled,
}

impl Phase {
    /// Lowercase tag used by the frontend (matches the design's dock modes).
    pub fn tag(self) -> &'static str {
        match self {
            Phase::Warming => "warming",
            Phase::Idle => "idle",
            Phase::Listening => "listening",
            Phase::Processing => "processing",
            Phase::Disabled => "disabled",
        }
    }
}

/// Events emitted by the engine for the UI to render.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Pipeline phase changed.
    Phase(Phase),
    /// A dictation completed. `ms` is total capture→paste latency.
    Transcript {
        raw: String,
        clean: String,
        ms: u128,
        words: usize,
    },
    /// A non-fatal error occurred (shown to the user, engine keeps running).
    Error(String),
}

/// Callback the engine calls (from the worker thread) to report events.
pub type EventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Commands sent to the worker thread.
enum Cmd {
    SetEnabled(bool),
    Reload(Box<Config>),
    Shutdown,
}

/// Handle to a running engine. Dropping it shuts the engine down.
pub struct Engine {
    cmd_tx: Sender<Cmd>,
    hot: Arc<Mutex<HotState>>,
    enabled: Arc<AtomicBool>,
    config: Config,
    worker: Option<JoinHandle<()>>,
}

impl Engine {
    /// Start the engine with `config`, reporting progress through `sink`. Returns immediately;
    /// model loading happens on the worker thread (watch for [`Phase::Warming`]→[`Phase::Idle`]).
    pub fn start(config: Config, sink: EventSink) -> Engine {
        let targets = crate::hotkey::parse_combo(&config.ptt_key);
        let hot = Arc::new(Mutex::new(HotState {
            chord: ChordState::new(targets),
            enabled: true,
        }));
        let enabled = Arc::new(AtomicBool::new(true));

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<Cmd>();
        let (ptt_tx, ptt_rx) = crossbeam_channel::unbounded::<PttEvent>();

        spawn_hotkey_thread(Arc::clone(&hot), Arc::clone(&enabled), ptt_tx);

        let worker_cfg = config.clone();
        let worker = std::thread::Builder::new()
            .name("wisteria-engine".into())
            .spawn(move || worker_loop(worker_cfg, cmd_rx, ptt_rx, sink))
            .expect("spawn engine worker");

        Engine {
            cmd_tx,
            hot,
            enabled,
            config,
            worker: Some(worker),
        }
    }

    /// Enable or disable dictation. When disabled the push-to-talk key passes through to the OS.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
        if let Ok(mut h) = self.hot.lock() {
            h.enabled = on;
        }
        let _ = self.cmd_tx.send(Cmd::SetEnabled(on));
    }

    /// Whether dictation is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Apply a new config. Rebuilds only the parts that changed (device/model/formatter) and
    /// updates the hotkey binding live.
    pub fn reload(&mut self, config: Config) {
        // Update the hotkey binding immediately (doesn't need the worker).
        if config.ptt_key != self.config.ptt_key {
            let targets = crate::hotkey::parse_combo(&config.ptt_key);
            if let Ok(mut h) = self.hot.lock() {
                h.chord = ChordState::new(targets);
            }
        }
        self.config = config.clone();
        let _ = self.cmd_tx.send(Cmd::Reload(Box::new(config)));
    }

    /// The config the engine was last told to use.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Spawn the global push-to-talk listener. Consumes the target key only while enabled; otherwise
/// passes it through. On Linux, `grab` needs uinput/root so we fall back to observe-only `listen`.
#[cfg(not(target_os = "linux"))]
fn spawn_hotkey_thread(hot: Arc<Mutex<HotState>>, _enabled: Arc<AtomicBool>, ptt_tx: Sender<PttEvent>) {
    std::thread::Builder::new()
        .name("wisteria-hotkey".into())
        .spawn(move || {
            let callback = move |event: rdev::Event| -> Option<rdev::Event> {
                let mut guard = match hot.lock() {
                    Ok(g) => g,
                    Err(_) => return Some(event),
                };
                if !guard.enabled {
                    return Some(event);
                }
                let (transition, is_target) = guard.chord.update(&event.event_type);
                if let Some(t) = transition {
                    let _ = ptt_tx.send(t);
                }
                if is_target {
                    None
                } else {
                    Some(event)
                }
            };
            if let Err(e) = rdev::grab(callback) {
                error!(?e, "hotkey grab failed (input events unavailable)");
            }
        })
        .expect("spawn hotkey thread");
}

#[cfg(target_os = "linux")]
fn spawn_hotkey_thread(hot: Arc<Mutex<HotState>>, _enabled: Arc<AtomicBool>, ptt_tx: Sender<PttEvent>) {
    warn!("Linux: push-to-talk key is observed but not suppressed (grab needs uinput/root)");
    std::thread::Builder::new()
        .name("wisteria-hotkey".into())
        .spawn(move || {
            let callback = move |event: rdev::Event| {
                let mut guard = match hot.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if !guard.enabled {
                    return;
                }
                if let (Some(t), _) = guard.chord.update(&event.event_type) {
                    let _ = ptt_tx.send(t);
                }
            };
            if let Err(e) = rdev::listen(callback) {
                error!(?e, "hotkey listener stopped (input events unavailable)");
            }
        })
        .expect("spawn hotkey thread");
}

/// Resources the worker owns and rebuilds on config changes.
struct Pipeline {
    recorder: Option<Recorder>,
    asr: Option<Asr>,
    formatter: Option<Formatter>,
}

/// Build (or rebuild) the pipeline from `config`, emitting warming/error events.
fn build_pipeline(config: &Config, sink: &EventSink) -> Pipeline {
    sink(EngineEvent::Phase(Phase::Warming));

    let recorder = match Recorder::new(&config.input_device) {
        Ok(r) => Some(r),
        Err(e) => {
            sink(EngineEvent::Error(format!("microphone: {e}")));
            None
        }
    };

    let asr = match models::ensure_models(config).and_then(|m| Asr::load(&m.asr_dir, &config.language)) {
        Ok(a) => {
            info!("ASR engine warm");
            Some(a)
        }
        Err(e) => {
            sink(EngineEvent::Error(format!("ASR model: {e}")));
            None
        }
    };

    let formatter = Formatter::new(config);

    Pipeline {
        recorder,
        asr,
        formatter,
    }
}

/// The worker: builds the pipeline, then handles PTT events and control commands until shutdown.
fn worker_loop(
    mut config: Config,
    cmd_rx: Receiver<Cmd>,
    ptt_rx: Receiver<PttEvent>,
    sink: EventSink,
) {
    let mut pipe = build_pipeline(&config, &sink);
    let mut enabled = true;
    sink(EngineEvent::Phase(if pipe.asr.is_some() {
        Phase::Idle
    } else {
        Phase::Warming
    }));

    loop {
        crossbeam_channel::select! {
            recv(cmd_rx) -> cmd => match cmd {
                Ok(Cmd::Shutdown) | Err(_) => break,
                Ok(Cmd::SetEnabled(on)) => {
                    enabled = on;
                    sink(EngineEvent::Phase(if on { Phase::Idle } else { Phase::Disabled }));
                }
                Ok(Cmd::Reload(new)) => {
                    let new = *new;
                    let rebuild_audio = new.input_device != config.input_device;
                    let rebuild_asr = new.model != config.model || new.language != config.language;
                    let rebuild_fmt = new.format != config.format
                        || new.formatter_model != config.formatter_model
                        || new.formatter_url != config.formatter_url
                        || new.formatter_timeout_ms != config.formatter_timeout_ms;
                    if rebuild_audio {
                        pipe.recorder = match Recorder::new(&new.input_device) {
                            Ok(r) => Some(r),
                            Err(e) => { sink(EngineEvent::Error(format!("microphone: {e}"))); None }
                        };
                    }
                    if rebuild_asr {
                        sink(EngineEvent::Phase(Phase::Warming));
                        pipe.asr = match models::ensure_models(&new).and_then(|m| Asr::load(&m.asr_dir, &new.language)) {
                            Ok(a) => Some(a),
                            Err(e) => { sink(EngineEvent::Error(format!("ASR model: {e}"))); None }
                        };
                    }
                    if rebuild_fmt {
                        pipe.formatter = Formatter::new(&new);
                    }
                    config = new;
                    sink(EngineEvent::Phase(if enabled { Phase::Idle } else { Phase::Disabled }));
                }
            },
            recv(ptt_rx) -> ev => match ev {
                Ok(PttEvent::Pressed) => {
                    if enabled {
                        if let Some(r) = &pipe.recorder { r.start(); }
                        sink(EngineEvent::Phase(Phase::Listening));
                    }
                }
                Ok(PttEvent::Released) => {
                    if enabled {
                        handle_utterance(&mut pipe, &sink);
                        sink(EngineEvent::Phase(Phase::Idle));
                    }
                }
                Err(_) => break,
            },
        }
    }
    info!("engine worker stopped");
}

/// Run one capture→ASR→format→paste cycle, emitting a transcript or error.
fn handle_utterance(pipe: &mut Pipeline, sink: &EventSink) {
    let start = Instant::now();
    let samples = match &pipe.recorder {
        Some(r) => r.stop(),
        None => return,
    };
    if samples.is_empty() {
        return; // too short / no mic
    }
    sink(EngineEvent::Phase(Phase::Processing));

    let asr = match &mut pipe.asr {
        Some(a) => a,
        None => {
            sink(EngineEvent::Error("no ASR model loaded".into()));
            return;
        }
    };
    let raw = match asr.transcribe(&samples) {
        Ok(t) => t,
        Err(e) => {
            sink(EngineEvent::Error(format!("transcription: {e}")));
            return;
        }
    };
    if raw.is_empty() {
        return;
    }

    let clean = match &pipe.formatter {
        Some(f) => f.clean(&raw),
        None => raw.clone(),
    };

    if let Err(e) = paste::paste_text(&clean) {
        sink(EngineEvent::Error(format!("paste: {e}")));
    }

    let words = clean.split_whitespace().count();
    sink(EngineEvent::Transcript {
        raw,
        clean,
        ms: start.elapsed().as_millis(),
        words,
    });
}
