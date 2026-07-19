//! Warm microphone capture. The input stream stays open ("warm") so there is no device
//! startup latency on push-to-talk; PTT only gates whether incoming samples are buffered.
//! Captured audio is resampled to [`crate::TARGET_SAMPLE_RATE`] mono `f32`.

use anyhow::Result;

/// A warm capture stream over the default input device.
pub struct Recorder;

impl Recorder {
    /// Open the default input device and keep the stream warm. Implemented in M2.2.
    pub fn new() -> Result<Self> {
        todo!("M2.2: open cpal input stream into a ring buffer, keep warm")
    }

    /// Begin buffering samples for a new recording.
    pub fn start(&self) {
        todo!("M2.2: mark recording active")
    }

    /// Stop buffering and return the recording as 16 kHz mono `f32`.
    /// Recordings shorter than ~300 ms are returned empty (treated as accidental taps).
    pub fn stop(&self) -> Vec<f32> {
        todo!("M2.2: drain ring buffer, downsample to 16 kHz mono")
    }
}
