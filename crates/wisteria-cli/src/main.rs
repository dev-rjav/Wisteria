//! Wisteria headless daemon (Phase 1). The console is the UI: it loads config, warms the
//! models, and runs the push-to-talk loop, logging each stage's latency.
//!
//! Hold the configured push-to-talk key → speak → release → the transcript is pasted into the
//! focused app. Nothing leaves the machine.

use std::time::Instant;

use anyhow::Result;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use wisteria_core::asr::Asr;
use wisteria_core::audio::Recorder;
use wisteria_core::hotkey::{self, PttEvent};
use wisteria_core::{models, paste, Config};

fn main() -> Result<()> {
    init_logging();

    let config_path = Config::default_path()?;
    let config = Config::load_or_create(&config_path)?;
    info!(path = %config_path.display(), "config loaded");
    info!(
        ptt_key = %config.ptt_key,
        model = %config.model,
        language = %config.language,
        format = ?config.format,
        "configuration"
    );

    // Ensure model artifacts are present (downloads on first run).
    let models = models::ensure_models(&config)?;

    // Warm the ASR engine once so per-utterance latency is just inference.
    let warm = Instant::now();
    let mut asr = Asr::load(&models.asr_dir, &config.language)?;
    info!(ms = warm.elapsed().as_millis(), "ASR engine warm");

    // Keep the microphone stream open ("warm").
    let recorder = Recorder::new()?;

    // Global push-to-talk listener.
    let ptt = hotkey::spawn(&config.ptt_key)?;

    info!(key = %config.ptt_key, "ready — hold the push-to-talk key and speak");

    for event in ptt.iter() {
        match event {
            PttEvent::Pressed => {
                recorder.start();
                info!("recording…");
            }
            PttEvent::Released => {
                let cap = Instant::now();
                let samples = recorder.stop();
                if samples.is_empty() {
                    continue;
                }
                let capture_ms = cap.elapsed().as_millis();

                let asr_start = Instant::now();
                let text = match asr.transcribe(&samples) {
                    Ok(text) => text,
                    Err(e) => {
                        error!(%e, "transcription failed");
                        continue;
                    }
                };
                let asr_ms = asr_start.elapsed().as_millis();

                if text.is_empty() {
                    info!(capture_ms, asr_ms, "no speech detected");
                    continue;
                }

                let paste_start = Instant::now();
                if let Err(e) = paste::paste_text(&text) {
                    error!(%e, "paste failed");
                }
                info!(
                    capture_ms,
                    asr_ms,
                    paste_ms = paste_start.elapsed().as_millis(),
                    text = %text,
                    "transcript pasted"
                );
            }
        }
    }

    Ok(())
}

/// Initialize tracing. Verbosity is controlled by `RUST_LOG` (default: `info`).
fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();
    info!("Wisteria daemon starting");
}
