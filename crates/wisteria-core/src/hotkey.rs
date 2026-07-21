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

use std::time::{Duration, Instant};

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
pub struct ChordState {
    targets: Vec<Key>,
    /// Bitmask of held target keys (bit `i` ↔ `targets[i]`).
    held: u32,
    /// Mask value meaning "all targets held".
    all: u32,
    active: bool,
}

impl ChordState {
    pub fn new(targets: Vec<Key>) -> Self {
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
    pub fn update(&mut self, event_type: &EventType) -> (Option<PttEvent>, bool) {
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

/// Longest press→release counted as a "tap". A shorter press is a tap (part of a double-tap-to-lock
/// gesture); a longer one is a deliberate hold-to-talk.
const TAP_MAX: Duration = Duration::from_millis(350);
/// After a lone tap, how long to wait for a second tap before giving up (and discarding the tap).
/// Also the maximum gap between the two taps of a lock gesture.
const DOUBLE_WINDOW: Duration = Duration::from_millis(400);

/// What the engine should do in response to a PTT transition (or a lock-window timeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttAction {
    /// Nothing.
    None,
    /// Begin capturing audio (key just went down from idle).
    Start,
    /// Stop capturing and run the ASR → format → paste pipeline.
    Finish,
    /// Stop capturing and throw the audio away (a lone quick tap that wasn't a double-tap).
    Discard,
    /// A double-tap completed — recording is now locked hands-free (audio is already capturing).
    Lock,
}

/// Layers hold-to-talk **and** double-tap-to-lock on top of raw [`PttEvent`] transitions.
///
/// - **Hold**: press and hold to record, release to finish (unchanged classic PTT).
/// - **Double-tap to lock**: two quick taps lock recording hands-free; a single tap afterwards
///   stops and processes. A lone quick tap that isn't followed by a second is discarded.
///
/// Time is injected ([`Instant`]) so the logic is unit-testable. The caller drives it with
/// [`on_event`](LockMachine::on_event) for each transition and, while [`deadline`](LockMachine::deadline)
/// is `Some`, calls [`on_timeout`](LockMachine::on_timeout) once that instant passes.
pub struct LockMachine {
    state: LockState,
}

enum LockState {
    /// Not recording.
    Idle,
    /// Recording; first key press is held.
    Held { since: Instant },
    /// One quick tap done; still recording, waiting for a second tap (or the window to lapse).
    ArmedTap { since: Instant },
    /// A second press is down after a tap; its release decides lock vs. finish.
    HeldAfterTap { since: Instant },
    /// Hands-free recording after a successful double-tap.
    Locked,
    /// The stop-tap's press was consumed in locked mode; ignore its release.
    Stopping,
}

impl Default for LockMachine {
    fn default() -> Self {
        LockMachine { state: LockState::Idle }
    }
}

impl LockMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The instant at which [`on_timeout`](LockMachine::on_timeout) should be called, if any (only
    /// while waiting for a possible second tap).
    pub fn deadline(&self) -> Option<Instant> {
        match self.state {
            LockState::ArmedTap { since } => Some(since + DOUBLE_WINDOW),
            _ => None,
        }
    }

    /// Feed a press/release transition; returns the action the engine should take.
    pub fn on_event(&mut self, ev: PttEvent, now: Instant) -> PttAction {
        match (&self.state, ev) {
            (LockState::Idle, PttEvent::Pressed) => {
                self.state = LockState::Held { since: now };
                PttAction::Start
            }
            (LockState::Held { since }, PttEvent::Released) => {
                if now.duration_since(*since) < TAP_MAX {
                    // A quick tap — keep recording and wait to see if a second tap arrives.
                    self.state = LockState::ArmedTap { since: now };
                    PttAction::None
                } else {
                    self.state = LockState::Idle;
                    PttAction::Finish
                }
            }
            (LockState::ArmedTap { .. }, PttEvent::Pressed) => {
                self.state = LockState::HeldAfterTap { since: now };
                PttAction::None
            }
            (LockState::HeldAfterTap { since }, PttEvent::Released) => {
                if now.duration_since(*since) < TAP_MAX {
                    // Second quick tap → lock hands-free (already recording since the first press).
                    self.state = LockState::Locked;
                    PttAction::Lock
                } else {
                    // Held the second press → treat as a normal hold and finish.
                    self.state = LockState::Idle;
                    PttAction::Finish
                }
            }
            (LockState::Locked, PttEvent::Pressed) => {
                // Any press while locked stops and processes; its release is then ignored.
                self.state = LockState::Stopping;
                PttAction::Finish
            }
            (LockState::Stopping, PttEvent::Released) => {
                self.state = LockState::Idle;
                PttAction::None
            }
            _ => PttAction::None,
        }
    }

    /// Call once [`deadline`](LockMachine::deadline) has passed: a lone tap that never got a second
    /// tap is abandoned and its audio discarded.
    pub fn on_timeout(&mut self, _now: Instant) -> PttAction {
        match self.state {
            LockState::ArmedTap { .. } => {
                self.state = LockState::Idle;
                PttAction::Discard
            }
            _ => PttAction::None,
        }
    }

    /// Reset to idle (engine disabled/shutting down). Returns `true` if we were mid-recording, so
    /// the caller can stop and drop the recorder.
    pub fn reset(&mut self) -> bool {
        let recording = !matches!(self.state, LockState::Idle | LockState::Stopping);
        self.state = LockState::Idle;
        recording
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
pub fn parse_combo(spec: &str) -> Vec<Key> {
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

    // ----- LockMachine (hold-to-talk + double-tap-to-lock) -----

    // Convenience: an instant `ms` after a base, so tests can express timing precisely.
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn hold_to_talk_starts_and_finishes() {
        let mut m = LockMachine::new();
        let t = Instant::now();
        assert_eq!(m.on_event(PttEvent::Pressed, t), PttAction::Start);
        // Held well past the tap threshold → a normal hold that finishes on release.
        assert_eq!(m.on_event(PttEvent::Released, at(t, 1000)), PttAction::Finish);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn double_tap_locks_then_a_tap_stops() {
        let mut m = LockMachine::new();
        let t = Instant::now();
        // First quick tap: start recording, then a short release (no action yet, awaiting 2nd tap).
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 0)), PttAction::Start);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 80)), PttAction::None);
        assert!(m.deadline().is_some());
        // Second quick tap within the window → lock.
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 200)), PttAction::None);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 280)), PttAction::Lock);
        assert_eq!(m.deadline(), None);
        // A later single press stops + processes; its release is ignored.
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 5000)), PttAction::Finish);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 5090)), PttAction::None);
    }

    #[test]
    fn lone_tap_is_discarded_after_the_window() {
        let mut m = LockMachine::new();
        let t = Instant::now();
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 0)), PttAction::Start);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 90)), PttAction::None);
        // No second tap arrives; the window lapses → discard.
        assert_eq!(m.on_timeout(at(t, 500)), PttAction::Discard);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn hold_after_a_stray_tap_finishes_normally() {
        let mut m = LockMachine::new();
        let t = Instant::now();
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 0)), PttAction::Start);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 80)), PttAction::None);
        // Second press is held long (not a tap) → treated as a normal hold, finishes on release.
        assert_eq!(m.on_event(PttEvent::Pressed, at(t, 200)), PttAction::None);
        assert_eq!(m.on_event(PttEvent::Released, at(t, 1500)), PttAction::Finish);
    }

    #[test]
    fn reset_reports_recording_state() {
        let mut m = LockMachine::new();
        let t = Instant::now();
        assert!(!m.reset()); // idle
        m.on_event(PttEvent::Pressed, t);
        assert!(m.reset()); // was recording
        assert!(!m.reset()); // back to idle
    }
}
