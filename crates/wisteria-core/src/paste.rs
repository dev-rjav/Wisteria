//! Deliver text to the focused application by simulating a paste: save the user's current
//! clipboard, set the transcript, synthesize the platform paste shortcut, then restore the
//! original clipboard shortly after. If keystroke synthesis fails, the text is left on the
//! clipboard as a fallback (logged), so a transcript is never silently lost.

use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use tracing::warn;

/// How long to wait after the paste keystroke before restoring the original clipboard, giving
/// the target app time to read the pasted text.
const RESTORE_DELAY: Duration = Duration::from_millis(150);

/// The modifier used with `V` to paste on this platform (⌘ on macOS, Ctrl elsewhere).
#[cfg(target_os = "macos")]
const PASTE_MODIFIER: Key = Key::Meta;
#[cfg(not(target_os = "macos"))]
const PASTE_MODIFIER: Key = Key::Control;

/// The "V" key of the paste chord. This must be a real virtual key, NOT a Unicode character:
/// on Windows `Key::Unicode('v')` uses `KEYEVENTF_UNICODE` and on macOS it sets the event's
/// Unicode string — both **bypass the held modifier**, so the target receives a literal "v"
/// character and the Ctrl/⌘+V shortcut never fires (the symptom: only "v" gets typed). Sending
/// the platform virtual-key code respects the modifier. On Linux the X11 keysym path already
/// honors modifiers, so `Key::Unicode` is fine there.
#[cfg(target_os = "windows")]
const PASTE_V: Key = Key::Other(0x56); // VK_V
#[cfg(target_os = "macos")]
const PASTE_V: Key = Key::Other(0x09); // kVK_ANSI_V
#[cfg(target_os = "linux")]
const PASTE_V: Key = Key::Unicode('v');

/// Paste `text` at the cursor, preserving the user's existing clipboard.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let mut clipboard = Clipboard::new().context("opening system clipboard")?;
    let previous = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .context("writing transcript to clipboard")?;

    match synth_paste() {
        Ok(()) => {
            // Give the foreground app a moment to consume the clipboard, then restore.
            std::thread::sleep(RESTORE_DELAY);
            if let Some(prev) = previous {
                if let Err(e) = clipboard.set_text(prev) {
                    warn!(%e, "could not restore previous clipboard contents");
                }
            }
        }
        Err(e) => {
            // Leave the transcript on the clipboard so the user can paste manually.
            warn!(%e, "keystroke synthesis failed; transcript left on clipboard for manual paste");
        }
    }
    Ok(())
}

/// Synthesize the platform paste chord (Ctrl/⌘ + V).
fn synth_paste() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow::anyhow!("initializing input synthesis: {e}"))?;
    enigo
        .key(PASTE_MODIFIER, Direction::Press)
        .map_err(|e| anyhow::anyhow!("press modifier: {e}"))?;
    // Let the target register the modifier before the V keystroke arrives.
    std::thread::sleep(Duration::from_millis(20));
    let v_result = enigo.key(PASTE_V, Direction::Click);
    // Always release the modifier, even if the 'v' click failed.
    let release = enigo.key(PASTE_MODIFIER, Direction::Release);
    v_result.map_err(|e| anyhow::anyhow!("press v: {e}"))?;
    release.map_err(|e| anyhow::anyhow!("release modifier: {e}"))?;
    Ok(())
}
