//! Wisteria headless daemon (Phase 1). The console is the UI: it loads config, warms the
//! models, and runs the push-to-talk loop, logging each stage's latency.
//!
//! M1 wires up config loading + logging; the capture→ASR→paste loop is filled in during M2.

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use wisteria_core::Config;

fn main() -> Result<()> {
    init_logging();

    let config_path = Config::default_path()?;
    let config = Config::load_or_create(&config_path)?;

    info!("Wisteria daemon starting");
    info!(path = %config_path.display(), "config loaded");
    info!(
        ptt_key = %config.ptt_key,
        model = %config.model,
        language = %config.language,
        format = ?config.format,
        "configuration"
    );

    // M2 wires the pipeline here:
    //   let models = wisteria_core::models::ensure_models(&config)?;
    //   let mut asr = wisteria_core::asr::Asr::load(&models.asr_dir)?;
    //   let recorder = wisteria_core::audio::Recorder::new()?;
    //   let ptt = wisteria_core::hotkey::spawn(&config.ptt_key)?;
    //   loop { match ptt.recv()? { Pressed => recorder.start(), Released => { … paste } } }
    info!("pipeline wiring lands in M2 — see plan.md. Nothing else to do yet; exiting.");

    Ok(())
}

/// Initialize tracing. Verbosity is controlled by `RUST_LOG` (default: `info`).
fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();
}
