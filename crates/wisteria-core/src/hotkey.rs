//! Global push-to-talk listener. A background thread watches for the configured key (or key
//! combination) and emits debounced press/release transitions. Per-OS keycode quirks live in
//! `rdev`; the name→`Key` mapping stays isolated here.
//!
//! `ptt_key` may be a single key (`"ControlRight"`) or a `+`-separated chord (`"Win+Alt"`).
//! For a chord, [`PttEvent::Pressed`] fires once every key is held and [`PttEvent::Released`]
//! fires as soon as any of them is let go.

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use rdev::{listen, Event, EventType, Key};
use tracing::{error, info, warn};

/// Push-to-talk transitions emitted by the listener thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttEvent {
    /// The PTT key/combo went down — start recording.
    Pressed,
    /// The PTT key/combo was released — finalize the recording.
    Released,
}

/// Spawn a background listener for `ptt_key` (a single `rdev::Key` name or a `+`-separated
/// chord such as `"Win+Alt"`); returns a receiver of debounced press/release events.
pub fn spawn(ptt_key: &str) -> Result<Receiver<PttEvent>> {
    let targets = parse_combo(ptt_key);
    info!(combo = ?targets, spec = ptt_key, "push-to-talk listener armed");

    let (tx, rx): (Sender<PttEvent>, Receiver<PttEvent>) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name("wisteria-hotkey".into())
        .spawn(move || {
            // Per-target "is currently held" flags; the chord is active while all are true.
            let mut held = vec![false; targets.len()];
            let mut active = false;

            let callback = move |event: Event| match event.event_type {
                EventType::KeyPress(k) => {
                    for (i, t) in targets.iter().enumerate() {
                        if *t == k {
                            held[i] = true;
                        }
                    }
                    if !active && held.iter().all(|&h| h) {
                        active = true;
                        let _ = tx.send(PttEvent::Pressed);
                    }
                }
                EventType::KeyRelease(k) => {
                    let mut touched = false;
                    for (i, t) in targets.iter().enumerate() {
                        if *t == k {
                            held[i] = false;
                            touched = true;
                        }
                    }
                    if active && touched && !held.iter().all(|&h| h) {
                        active = false;
                        let _ = tx.send(PttEvent::Released);
                    }
                }
                _ => {}
            };
            if let Err(e) = listen(callback) {
                error!(?e, "hotkey listener stopped (input events unavailable)");
            }
        })?;

    Ok(rx)
}

/// Parse a `+`-separated key combination. Empty/invalid specs fall back to Right Ctrl.
fn parse_combo(spec: &str) -> Vec<Key> {
    let keys: Vec<Key> = spec
        .split('+')
        .filter_map(|tok| {
            let tok = tok.trim();
            if tok.is_empty() {
                return None;
            }
            match parse_key(tok) {
                Some(k) => Some(k),
                None => {
                    warn!(token = tok, "unknown key in ptt_key; ignoring");
                    None
                }
            }
        })
        .collect();

    if keys.is_empty() {
        warn!(spec, "no valid keys in ptt_key; falling back to ControlRight");
        vec![Key::ControlRight]
    } else {
        keys
    }
}

/// Map a single key name (case-insensitive, with common aliases) to an `rdev::Key`.
fn parse_key(name: &str) -> Option<Key> {
    let key = match name.to_ascii_lowercase().as_str() {
        "win" | "super" | "meta" | "cmd" | "command" | "metaleft" | "winleft" => Key::MetaLeft,
        "metaright" | "winright" => Key::MetaRight,
        "alt" | "altleft" | "option" => Key::Alt,
        "altgr" | "altright" => Key::AltGr,
        "ctrl" | "control" | "controlleft" | "leftctrl" | "lctrl" => Key::ControlLeft,
        "controlright" | "rightctrl" | "rctrl" => Key::ControlRight,
        "shift" | "shiftleft" | "leftshift" => Key::ShiftLeft,
        "shiftright" | "rightshift" => Key::ShiftRight,
        "space" => Key::Space,
        "capslock" => Key::CapsLock,
        "tab" => Key::Tab,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_win_alt_combo() {
        assert_eq!(parse_combo("Win+Alt"), vec![Key::MetaLeft, Key::Alt]);
    }

    #[test]
    fn single_key_still_works() {
        assert_eq!(parse_combo("ControlRight"), vec![Key::ControlRight]);
    }

    #[test]
    fn invalid_spec_falls_back() {
        assert_eq!(parse_combo("nonsense"), vec![Key::ControlRight]);
    }
}
