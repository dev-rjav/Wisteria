//! Global push-to-talk listener. A background thread watches for the configured key and emits
//! press/release transitions. Per-OS keycode quirks stay isolated in this module.

use anyhow::Result;
use crossbeam_channel::Receiver;

/// Push-to-talk transitions emitted by the listener thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttEvent {
    /// The PTT key went down — start recording.
    Pressed,
    /// The PTT key came up — finalize the recording.
    Released,
}

/// Spawn a background listener for `ptt_key` (an `rdev::Key` variant name such as
/// `"ControlRight"`); returns a receiver of debounced press/release events. Implemented in M2.3.
pub fn spawn(_ptt_key: &str) -> Result<Receiver<PttEvent>> {
    todo!("M2.3: rdev::listen thread, parse ptt_key, debounce auto-repeat, send PttEvent")
}
