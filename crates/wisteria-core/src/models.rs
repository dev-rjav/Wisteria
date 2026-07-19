//! Model artifact management: ensure the model files the pipeline needs are downloaded to the
//! app-data models directory and verified. Artifacts are never committed (gitignored) and are
//! fetched on first run with progress logs.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;

/// Resolved on-disk locations of the model artifacts the pipeline needs.
#[derive(Debug, Clone)]
pub struct ModelPaths {
    /// Directory containing the Parakeet ONNX model files (encoder/decoder/tokens).
    pub asr_dir: PathBuf,
}

/// The models directory (`<app-data>/models`), created if missing.
pub fn models_dir() -> Result<PathBuf> {
    let dir = Config::app_data_dir()?.join("models");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Ensure every model named by `config` is present locally, downloading any that are missing and
/// verifying integrity. Implemented in M2.1.
pub fn ensure_models(_config: &Config) -> Result<ModelPaths> {
    todo!("M2.1: download + SHA-verify Parakeet TDT 0.6B v3 int8 into models_dir()")
}
