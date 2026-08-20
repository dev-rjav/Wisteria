//! Warm microphone capture. The input stream stays open ("warm") so there is no device
//! startup latency on push-to-talk; PTT only gates whether incoming samples are buffered.
//! Captured audio is downmixed to mono and linearly resampled to [`crate::TARGET_SAMPLE_RATE`].

use std::path::Path;
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

/// Write 16 kHz mono `f32` `samples` (values in [-1, 1]) to `path` as a 16-bit PCM WAV file, so a
/// dictation's audio can be replayed from — or re-transcribed out of — history later. Deliberately
/// dependency-free (a hand-rolled 44-byte header) since we only ever read back what we wrote.
pub fn write_wav_16k_mono(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let sample_rate = TARGET_SAMPLE_RATE;
    let data_len = (samples.len() as u32) * 2; // 16-bit samples
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, buf)
}

/// Read back a WAV written by [`write_wav_16k_mono`] into 16 kHz mono `f32` samples, for
/// re-transcription. Scans for the `data` chunk (rather than assuming a fixed header length) and
/// interprets it as little-endian 16-bit PCM.
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body = pos + 8;
        if id == b"data" {
            let end = (body + size).min(bytes.len());
            let mut out = Vec::with_capacity((end - body) / 2);
            let mut i = body;
            while i + 1 < end {
                out.push(i16::from_le_bytes([bytes[i], bytes[i + 1]]) as f32 / 32768.0);
                i += 2;
            }
            return Ok(out);
        }
        // Chunks are word-aligned: skip the body plus a pad byte if the size is odd.
        pos = body + size + (size & 1);
    }
    bail!("WAV has no data chunk")
}

/// Names of all available input devices (for a settings picker). Best-effort: returns an empty
/// list if the host cannot be queried.
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
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

    #[test]
    fn wav_round_trips_through_16bit_pcm() {
        let samples = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.25];
        let path = std::env::temp_dir().join(format!("wisteria-wav-test-{}.wav", std::process::id()));
        write_wav_16k_mono(&path, &samples).unwrap();
        let back = read_wav_16k_mono(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(back.len(), samples.len());
        // 16-bit quantization error is well under 1/32768.
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }
}
