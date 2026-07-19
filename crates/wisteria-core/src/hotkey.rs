//! Global push-to-talk listener. A background thread watches for the configured key and emits
//! debounced press/release transitions. Per-OS keycode quirks live in `rdev`; the name→`Key`
//! mapping stays isolated here.

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use rdev::{listen, Event, EventType, Key};
use tracing::{error, info, warn};

/// Push-to-talk transitions emitted by the listener thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttEvent {
    /// The PTT key went down — start recording.
    Pressed,
    /// The PTT key came up — finalize the recording.
    Released,
}

/// Spawn a background listener for `ptt_key` (an `rdev::Key` variant name such as
/// `"ControlRight"`); returns a receiver of debounced press/release events. Auto-repeat
/// `KeyPress` events while the key is held are collapsed into a single [`PttEvent::Pressed`].
pub fn spawn(ptt_key: &str) -> Result<Receiver<PttEvent>> {
    let target = parse_key(ptt_key);
    info!(key = ?target, "push-to-talk listener armed");

    let (tx, rx): (Sender<PttEvent>, Receiver<PttEvent>) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name("wisteria-hotkey".into())
        .spawn(move || {
            let mut held = false;
            let callback = move |event: Event| match event.event_type {
                EventType::KeyPress(k) if k == target && !held => {
                    held = true;
                    let _ = tx.send(PttEvent::Pressed);
                }
                EventType::KeyRelease(k) if k == target && held => {
                    held = false;
                    let _ = tx.send(PttEvent::Released);
                }
                _ => {}
            };
            if let Err(e) = listen(callback) {
                error!(?e, "hotkey listener stopped (input events unavailable)");
            }
        })?;

    Ok(rx)
}

/// Map a config key name to an `rdev::Key`. Covers the keys that make sensible push-to-talk
/// bindings; unknown names fall back to Right Ctrl with a warning.
fn parse_key(name: &str) -> Key {
    match name.trim() {
        "ControlRight" | "RightCtrl" | "RCtrl" => Key::ControlRight,
        "ControlLeft" | "LeftCtrl" | "LCtrl" => Key::ControlLeft,
        "Alt" => Key::Alt,
        "AltGr" => Key::AltGr,
        "ShiftRight" | "RightShift" => Key::ShiftRight,
        "ShiftLeft" | "LeftShift" => Key::ShiftLeft,
        "MetaRight" | "MetaLeft" => Key::MetaLeft,
        "Space" => Key::Space,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        other => {
            warn!(key = other, "unknown ptt_key; falling back to ControlRight");
            Key::ControlRight
        }
    }
}
