//! Warm microphone capture. The input stream stays open ("warm") so there is no device
//! startup latency on push-to-talk; PTT only gates whether incoming samples are buffered.
//! Captured audio is downmixed to mono and linearly resampled to [`crate::TARGET_SAMPLE_RATE`].

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use tracing::{error, info, warn};

use crate::TARGET_SAMPLE_RATE;

/// Recordings shorter than this are treated as accidental taps and discarded.
const MIN_RECORDING_SECS: f32 = 0.3;

/// Substrings identifying loopback / "listen to what's playing" endpoints that are NOT mics.
const LOOPBACK_HINTS: [&str; 6] = [
    "stereo mix",
    "what u hear",
    "what you hear",
    "loopback",
    "wave out",
    "rec. playback",
];

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
    /// Open an input device and keep the stream warm. `device_query` is a case-insensitive
    /// substring of the desired device name; empty auto-selects, preferring a real microphone
    /// over loopback/"Stereo Mix" devices.
    pub fn new(device_query: &str) -> Result<Self> {
        let host = cpal::default_host();
        let device = select_input_device(&host, device_query)?;
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

/// True if `name` looks like a system-audio loopback endpoint rather than a microphone.
fn is_loopback(name: &str) -> bool {
    let n = name.to_lowercase();
    LOOPBACK_HINTS.iter().any(|h| n.contains(h))
}

/// Choose an input device: honor `query` (case-insensitive substring) if provided; otherwise
/// use the OS default, but if that default is a loopback endpoint (e.g. "Stereo Mix"), prefer a
/// real microphone. All available devices are logged so the user can set `input_device`.
fn select_input_device(host: &cpal::Host, query: &str) -> Result<cpal::Device> {
    let devices: Vec<cpal::Device> = host
        .input_devices()
        .context("enumerating input devices")?
        .collect();
    for d in &devices {
        if let Ok(n) = d.name() {
            info!(device = %n, loopback = is_loopback(&n), "available input device");
        }
    }

    let query = query.trim();
    if !query.is_empty() {
        let q = query.to_lowercase();
        if let Some(d) = devices
            .iter()
            .find(|d| d.name().map(|n| n.to_lowercase().contains(&q)).unwrap_or(false))
        {
            info!(query, device = %d.name().unwrap_or_default(), "selected input device by config");
            return Ok(d.clone());
        }
        warn!(query, "no input device matched input_device; auto-selecting");
    }

    // Prefer the OS default unless it's a loopback device.
    if let Some(default) = host.default_input_device() {
        let name = default.name().unwrap_or_default();
        if !is_loopback(&name) {
            return Ok(default);
        }
        warn!(device = %name, "default input is loopback, not a mic; searching for a microphone");
    }

    // Pick a non-loopback device, favoring names that mention "microphone"/"mic".
    let mic = devices
        .iter()
        .filter(|d| d.name().map(|n| !is_loopback(&n)).unwrap_or(false))
        .max_by_key(|d| {
            let n = d.name().unwrap_or_default().to_lowercase();
            u8::from(n.contains("microphone") || n.contains("mic"))
        });
    if let Some(d) = mic {
        info!(device = %d.name().unwrap_or_default(), "auto-selected microphone");
        return Ok(d.clone());
    }

    // Last resort: OS default (even if loopback) or the first enumerated device.
    host.default_input_device()
        .or_else(|| devices.into_iter().next())
        .ok_or_else(|| anyhow!("no input device found"))
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
