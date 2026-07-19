//! Warm microphone capture. The input stream stays open ("warm") so there is no device
//! startup latency on push-to-talk; PTT only gates whether incoming samples are buffered.
//! Captured audio is downmixed to mono and linearly resampled to [`crate::TARGET_SAMPLE_RATE`].

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use tracing::{error, warn};

use crate::TARGET_SAMPLE_RATE;

/// Recordings shorter than this are treated as accidental taps and discarded.
const MIN_RECORDING_SECS: f32 = 0.3;

/// Shared capture state, written from the cpal callback thread and read on `stop`.
struct Inner {
    recording: bool,
    /// Mono samples at the device's native rate, accumulated while `recording`.
    buf: Vec<f32>,
}

/// A warm capture stream over the default input device.
pub struct Recorder {
    // The stream must stay alive for capture to continue; dropping it stops the device.
    _stream: cpal::Stream,
    inner: Arc<Mutex<Inner>>,
    source_rate: u32,
}

impl Recorder {
    /// Open the default input device and keep the stream warm.
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device found"))?;
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let supported = device
            .default_input_config()
            .context("querying default input config")?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let source_rate = config.sample_rate.0;
        tracing::info!(device = %name, %source_rate, channels, ?sample_format, "input device");

        let inner = Arc::new(Mutex::new(Inner {
            recording: false,
            buf: Vec::new(),
        }));

        let err_fn = |err| error!(%err, "audio input stream error");

        // Downmix `channels`-interleaved frames to mono and buffer them while recording.
        macro_rules! build {
            ($sample:ty, $to_f32:expr) => {{
                let inner = Arc::clone(&inner);
                device.build_input_stream(
                    &config,
                    move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                        let mut guard = match inner.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        if !guard.recording {
                            return;
                        }
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame.iter().copied().map($to_f32).sum();
                            guard.buf.push(sum / channels as f32);
                        }
                    },
                    err_fn,
                    None,
                )
            }};
        }

        let stream = match sample_format {
            SampleFormat::F32 => build!(f32, |s| s),
            SampleFormat::I16 => build!(i16, |s| s as f32 / 32768.0),
            SampleFormat::U16 => build!(u16, |s| (s as f32 - 32768.0) / 32768.0),
            other => bail!("unsupported input sample format: {other:?}"),
        }
        .context("building input stream")?;

        stream.play().context("starting input stream")?;

        Ok(Self {
            _stream: stream,
            inner,
            source_rate,
        })
    }

    /// Begin buffering samples for a new recording.
    pub fn start(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.buf.clear();
            guard.recording = true;
        }
    }

    /// Stop buffering and return the recording as [`crate::TARGET_SAMPLE_RATE`] mono `f32`.
    /// Recordings shorter than [`MIN_RECORDING_SECS`] return empty (accidental taps).
    pub fn stop(&self) -> Vec<f32> {
        let mono = {
            let mut guard = match self.inner.lock() {
                Ok(g) => g,
                Err(_) => return Vec::new(),
            };
            guard.recording = false;
            std::mem::take(&mut guard.buf)
        };

        let secs = mono.len() as f32 / self.source_rate as f32;
        if secs < MIN_RECORDING_SECS {
            warn!(secs, "recording too short, ignoring");
            return Vec::new();
        }
        resample_linear(&mono, self.source_rate, TARGET_SAMPLE_RATE)
    }
}

/// Linear-interpolation resample of mono `input` from `from` Hz to `to` Hz.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_is_identity_when_rates_match() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downsample_halves_length() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_linear(&input, 32_000, 16_000);
        assert!((out.len() as i32 - 50).abs() <= 1);
    }
}
