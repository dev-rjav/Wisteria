//! Model artifact management: ensure the model files the pipeline needs are downloaded to the
//! app-data models directory and verified. Artifacts are never committed (gitignored) and are
//! fetched on first run with progress logs.
//!
//! ASR default: Parakeet TDT 0.6B v3 int8 (ONNX). The canonical bundle is the tarball published
//! by Handy (referenced by transcribe-rs' own README); it extracts to the exact file layout
//! `transcribe_rs::onnx::parakeet::ParakeetModel::load` expects.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::config::Config;

/// Subdirectory (under the models dir) holding the default Parakeet int8 model.
const PARAKEET_DIR: &str = "parakeet-tdt-0.6b-v3-int8";
/// Canonical download for the Parakeet int8 bundle (same source transcribe-rs documents).
const PARAKEET_URL: &str = "https://blob.handy.computer/parakeet-v3-int8.tar.gz";
/// Files `ParakeetModel::load` requires inside the model directory.
const PARAKEET_FILES: [&str; 4] = [
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
];

/// Resolved on-disk locations of the model artifacts the pipeline needs.
#[derive(Debug, Clone)]
pub struct ModelPaths {
    /// Directory containing the Parakeet ONNX model files (encoder / decoder_joint / preprocessor / vocab).
    pub asr_dir: PathBuf,
}

/// The models directory (`<app-data>/models`), created if missing.
pub fn models_dir() -> Result<PathBuf> {
    let dir = Config::app_data_dir()?.join("models");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Ensure every model named by `config` is present locally, downloading any that are missing.
///
/// Only the default Parakeet model is wired for MVP; a non-default `config.model` is accepted but
/// still resolves to the Parakeet bundle (model selection lands with the Whisper engine later).
pub fn ensure_models(config: &Config) -> Result<ModelPaths> {
    if config.model != "parakeet-tdt-0.6b-v3-int8" {
        warn!(
            model = %config.model,
            "only the default Parakeet model is supported in Phase 1; using it"
        );
    }
    let asr_dir = models_dir()?.join(PARAKEET_DIR);
    ensure_parakeet(&asr_dir)?;
    Ok(ModelPaths { asr_dir })
}

/// Ensure the Parakeet model files exist in `dir`, downloading + extracting the bundle if not.
fn ensure_parakeet(dir: &Path) -> Result<()> {
    if PARAKEET_FILES.iter().all(|f| dir.join(f).exists()) {
        info!(dir = %dir.display(), "Parakeet model already present");
        return Ok(());
    }
    info!(url = PARAKEET_URL, "downloading Parakeet model (first run)");
    fs::create_dir_all(dir).with_context(|| format!("creating model dir {}", dir.display()))?;

    let archive = download(PARAKEET_URL)?;
    info!(sha256 = %hex_sha256(&archive), bytes = archive.len(), "downloaded archive");
    extract_tar_gz(&archive, dir).context("extracting Parakeet bundle")?;

    let missing: Vec<&str> = PARAKEET_FILES
        .iter()
        .copied()
        .filter(|f| !dir.join(f).exists())
        .collect();
    if !missing.is_empty() {
        bail!(
            "Parakeet bundle extracted but missing expected files: {:?} (in {})",
            missing,
            dir.display()
        );
    }
    info!(dir = %dir.display(), "Parakeet model ready");
    Ok(())
}

/// Download `url` fully into memory (blocking), logging progress as it streams.
fn download(url: &str) -> Result<Vec<u8>> {
    let mut resp = reqwest::blocking::Client::builder()
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("bad status from {url}"))?;

    let total = resp.content_length();
    let mut buf = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut chunk = [0u8; 64 * 1024];
    let mut last_logged_pct = 0u64;
    loop {
        let n = resp.read(&mut chunk).context("reading response body")?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(total) = total {
            let pct = (buf.len() as u64 * 100 / total).min(100);
            if pct >= last_logged_pct + 10 {
                last_logged_pct = pct;
                info!(percent = pct, "downloading…");
            }
        }
    }
    Ok(buf)
}

/// Extract a `.tar.gz` byte buffer into `dest`, flattening any single wrapping top-level
/// directory so the required files land directly in `dest` regardless of archive layout.
fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // Keep only the final component so wrapper dirs (e.g. `parakeet-v3-int8/vocab.txt`)
        // collapse into `dest`. We only need the flat set of files.
        let Some(name) = path.file_name() else { continue };
        if !PARAKEET_FILES.iter().any(|f| Path::new(f) == Path::new(name)) {
            continue;
        }
        let out_path = dest.join(name);
        let mut out = fs::File::create(&out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("writing {}", out_path.display()))?;
        out.flush().ok();
        info!(file = %out_path.display(), "extracted");
    }
    Ok(())
}

/// Hex-encoded SHA-256 of `bytes` (logged so a known-good hash can be pinned later).
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}
