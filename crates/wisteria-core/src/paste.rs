//! Deliver text to the focused application by simulating a paste: save the user's current
//! clipboard, set the transcript, synthesize the platform paste shortcut, then restore the
//! original clipboard shortly after. If keystroke synthesis fails, the text is left on the
//! clipboard as a fallback (logged), so a transcript is never silently lost.

use anyhow::Result;

/// Paste `text` at the cursor, preserving the user's existing clipboard. Implemented in M2.5.
pub fn paste_text(_text: &str) -> Result<()> {
    todo!("M2.5: arboard save/set + enigo Ctrl/Cmd+V + restore original clipboard")
}
