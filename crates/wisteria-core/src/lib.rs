//! Wisteria core: the headless dictation pipeline.
//!
//! Hold a push-to-talk key → mic captures audio → local ASR transcribes → (optional) local LLM
//! cleans it up → text is pasted at the cursor. 100% offline by default.
//!
//! Each stage lives in its own module and is composed by the `wisteria-cli` daemon:
//!
//! - [`config`] — user config (`config.toml` in the app data dir).
//! - [`models`] — ensure model artifacts are downloaded + verified.
//! - [`audio`] — warm `cpal` input stream → 16 kHz mono `f32` recordings.
//! - [`hotkey`] — global push-to-talk listener (`rdev`) → press/release events.
//! - [`asr`]  — warm Parakeet engine → transcription.
//! - [`paste`] — clipboard-save → set text → synthetic paste → restore.

pub mod asr;
pub mod audio;
pub mod config;
pub mod dictionary;
pub mod engine;
pub mod format;
pub mod hotkey;
pub mod models;
pub mod paste;
pub mod snippets;

pub use config::Config;

/// Target sample rate for the ASR engine. All captured audio is resampled to this.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
