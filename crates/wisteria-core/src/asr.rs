//! Warm ASR engine wrapper. The engine (Parakeet TDT via `transcribe-rs`/ONNX Runtime) is
//! loaded once at startup and kept resident so per-utterance latency is just inference.

use std::path::Path;

use anyhow::{anyhow, Result};
use transcribe_rs::onnx::parakeet::ParakeetModel;
use transcribe_rs::onnx::Quantization;
use transcribe_rs::{SpeechModel, TranscribeOptions};

/// A loaded, warm speech-to-text engine.
pub struct Asr {
    model: ParakeetModel,
    /// Optional BCP-47 language hint; `None` = let the model decide.
    language: Option<String>,
}

impl Asr {
    /// Load the Parakeet int8 model from `model_dir` and keep it warm. `language` is a hint
    /// (`"auto"` or empty → no hint).
    pub fn load(model_dir: &Path, language: &str) -> Result<Self> {
        let model = ParakeetModel::load(model_dir, &Quantization::Int8)
            .map_err(|e| anyhow!("loading Parakeet model from {}: {e}", model_dir.display()))?;
        let language = match language.trim() {
            "" | "auto" => None,
            l => Some(l.to_string()),
        };
        Ok(Self { model, language })
    }

    /// Transcribe 16 kHz mono `f32` `samples` (values in [-1, 1]) to trimmed text.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let options = TranscribeOptions {
            language: self.language.clone(),
            ..Default::default()
        };
        let result = self
            .model
            .transcribe(samples, &options)
            .map_err(|e| anyhow!("transcription failed: {e}"))?;
        Ok(result.text.trim().to_string())
    }
}
