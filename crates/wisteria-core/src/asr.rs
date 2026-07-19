//! Warm ASR engine wrapper. The engine (Parakeet TDT via `transcribe-rs`/ONNX Runtime) is
//! loaded once at startup and kept resident so transcription latency is just inference.

use std::path::Path;

use anyhow::Result;

/// A loaded, warm speech-to-text engine.
pub struct Asr;

impl Asr {
    /// Load the model from `model_dir` and keep it warm. Implemented in M2.4.
    pub fn load(_model_dir: &Path) -> Result<Self> {
        todo!("M2.4: ParakeetEngine::new + load_model(model_dir), kept warm")
    }

    /// Transcribe 16 kHz mono `f32` `samples` to text.
    pub fn transcribe(&mut self, _samples: &[f32]) -> Result<String> {
        todo!("M2.4: engine.transcribe_samples(samples) -> text")
    }
}
