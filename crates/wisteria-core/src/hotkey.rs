//! Global push-to-talk listener. A background thread watches for the configured key (or key
//! combination) and emits debounced press/release transitions.
//!
//! On Windows and macOS the listener uses `rdev::grab`, which **consumes** the push-to-talk
//! key so it never triggers the key's normal OS action (no Start menu, no menu-bar activation,
//! no focus stealing before we paste). On Linux, `grab` requires uinput/root, so we fall back
//! to `rdev::listen` (observe-only, key not suppressed) — a tracked limitation.
//!
//! `ptt_key` may be a single key (`"F8"`) or a `+`-separated chord (`"Win+Alt"`). For a chord,
//! [`PttEvent::Pressed`] fires once every key is held and [`PttEvent::Released`] as soon as any
//! is released. Because the listener consumes every target key, prefer a **dedicated** key
//! (function key, etc.): binding a shared modifier would disable its normal use (Alt+Tab, the
//! Start menu…) while Wisteria runs.

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use rdev::{Event, EventType, Key};
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
    warn_if_modifier(&targets);

    let (tx, rx): (Sender<PttEvent>, Receiver<PttEvent>) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name("wisteria-hotkey".into())
        .spawn(move || run_listener(targets, tx))?;

    Ok(rx)
}

/// Tracks how many of the target keys are currently held and whether the chord is active,
/// deciding for each event whether to emit a transition and whether to consume the key.
struct ChordState {
    targets: Vec<Key>,
    /// Bitmask of held target keys (bit `i` ↔ `targets[i]`).
    held: u32,
    /// Mask value meaning "all targets held".
    all: u32,
    active: bool,
}

impl ChordState {
    fn new(targets: Vec<Key>) -> Self {
        let n = targets.len().min(32);
        let all = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
        ChordState {
            targets,
            held: 0,
            all,
            active: false,
        }
    }

    /// Update state for `event_type`; returns `(transition, is_target)`. `is_target` means the
    /// key belongs to the PTT binding and should be consumed by the caller.
    fn update(&mut self, event_type: &EventType) -> (Option<PttEvent>, bool) {
        match *event_type {
            EventType::KeyPress(k) => {
                let mut is_target = false;
                for (i, t) in self.targets.iter().enumerate() {
                    if *t == k {
                        self.held |= 1 << i;
                        is_target = true;
                    }
                }
                let transition = if !self.active && self.held & self.all == self.all {
                    self.active = true;
                    Some(PttEvent::Pressed)
                } else {
                    None
                };
                (transition, is_target)
            }
            EventType::KeyRelease(k) => {
                let mut is_target = false;
                for (i, t) in self.targets.iter().enumerate() {
                    if *t == k {
                        self.held &= !(1 << i);
                        is_target = true;
                    }
                }
                let transition = if self.active && self.held & self.all != self.all {
                    self.active = false;
                    Some(PttEvent::Released)
                } else {
                    None
                };
                (transition, is_target)
            }
            _ => (None, false),
        }
    }
}

/// Windows/macOS: grab (consume) the PTT key so it never performs its normal OS action.
#[cfg(not(target_os = "linux"))]
fn run_listener(targets: Vec<Key>, tx: Sender<PttEvent>) {
    use std::cell::RefCell;

    let state = RefCell::new(ChordState::new(targets));
    let callback = move |event: Event| -> Option<Event> {
        let (transition, is_target) = state.borrow_mut().update(&event.event_type);
        if let Some(t) = transition {
            let _ = tx.send(t);
        }
        // Consume target keys (return None); pass everything else through untouched.
        if is_target {
            None
        } else {
            Some(event)
        }
    };
    if let Err(e) = rdev::grab(callback) {
        error!(?e, "hotkey grab failed (input events unavailable)");
    }
}

/// Linux: `grab` needs uinput/root, so observe only — the key is NOT suppressed.
#[cfg(target_os = "linux")]
fn run_listener(targets: Vec<Key>, tx: Sender<PttEvent>) {
    warn!("Linux: push-to-talk key is observed but not suppressed (grab needs uinput/root)");
    let mut state = ChordState::new(targets);
    let callback = move |event: Event| {
        if let (Some(t), _) = state.update(&event.event_type) {
            let _ = tx.send(t);
        }
    };
    if let Err(e) = rdev::listen(callback) {
        error!(?e, "hotkey listener stopped (input events unavailable)");
    }
}

/// Warn if the binding uses a shared modifier, since consuming it disables normal use.
fn warn_if_modifier(targets: &[Key]) {
    const MODIFIERS: [Key; 8] = [
        Key::MetaLeft,
        Key::MetaRight,
        Key::Alt,
        Key::AltGr,
        Key::ControlLeft,
        Key::ControlRight,
        Key::ShiftLeft,
        Key::ShiftRight,
    ];
    if targets.iter().any(|k| MODIFIERS.contains(k)) {
        warn!(
            "ptt_key binds a modifier key; while Wisteria runs it will be consumed and its \
             normal function (Alt+Tab, Start menu, …) disabled. A dedicated key like F8 is safer."
        );
    }
}

/// Parse a `+`-separated key combination. Empty/invalid specs fall back to F8.
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
        warn!(spec, "no valid keys in ptt_key; falling back to F8");
        vec![Key::F8]
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
    fn parses_single_and_combo() {
        assert_eq!(parse_combo("F8"), vec![Key::F8]);
        assert_eq!(parse_combo("Win+Alt"), vec![Key::MetaLeft, Key::Alt]);
    }

    #[test]
    fn invalid_spec_falls_back_to_f8() {
        assert_eq!(parse_combo("nonsense"), vec![Key::F8]);
    }

    #[test]
    fn chord_fires_on_full_press_and_releases_on_partial() {
        let mut s = ChordState::new(vec![Key::MetaLeft, Key::Alt]);
        let press = EventType::KeyPress;
        let release = EventType::KeyRelease;
        assert_eq!(s.update(&press(Key::MetaLeft)), (None, true));
        assert_eq!(s.update(&press(Key::Alt)), (Some(PttEvent::Pressed), true));
        // Auto-repeat of an already-held key must not re-fire.
        assert_eq!(s.update(&press(Key::Alt)), (None, true));
        assert_eq!(s.update(&release(Key::Alt)), (Some(PttEvent::Released), true));
        // Non-target key is neither a transition nor consumed.
        assert_eq!(s.update(&press(Key::KeyA)), (None, false));
    }
}
