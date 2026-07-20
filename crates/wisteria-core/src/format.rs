//! Optional transcript cleanup (Stage B). The raw ASR output is passed to a small local LLM
//! served by [Ollama](https://ollama.com) (default model `qwen3:0.6b`) with a strict cleanup
//! prompt: remove fillers/false starts/stutters, resolve self-corrections, fix punctuation and
//! capitalization — while preserving every meaningful word.
//!
//! The formatter must **never block or break a paste**: it runs with a timeout and, on any
//! error (server down, model missing, timeout, empty output), the caller falls back to the raw
//! transcript. When `format = "off"` the stage is skipped entirely.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::{Config, FormatLevel};

/// Base cleanup instructions. An intensity line is appended per request.
///
/// IMPORTANT: this prompt is deliberately **example-free**. Small models (e.g. `qwen3:0.6b`)
/// tend to copy any concrete `Input → Output` example verbatim into their answer, so rules are
/// described abstractly. Keep it that way when editing.
const BASE_PROMPT: &str = r#"You are a speech-to-text transcript cleanup engine.

Your only task is to clean raw speech transcription while preserving
EVERY meaningful word, detail, and intention from the speaker.

You are NOT a summarizer.
You are NOT a writing assistant.
You must NOT shorten, simplify, paraphrase, or improve the speaker's ideas.

Your output must contain the same meaningful information as the input.

CLEANUP RULES:

1. Remove meaningless speech fillers:
   "um", "uh", "hmm", "er", "ah".

2. Remove words such as "like", "you know", "I mean", "basically",
   "actually", and "literally" ONLY when they are clearly meaningless
   verbal fillers.

   If they contribute to the meaning, KEEP them.

3. Remove accidental immediate repetitions and stutters.

   Input:
   "I I think we should should probably start today."

   Output:
   "I think we should probably start today."

   IMPORTANT:
   Delete ONLY the duplicated word.
   Do NOT delete surrounding words.

4. Resolve explicit self-corrections by keeping the speaker's final choice.

   Input:
   "Schedule it for Monday, no wait, Tuesday."

   Output:
   "Schedule it for Tuesday."

5. Remove a false start ONLY when it is clearly abandoned and replaced
   by a complete thought.

   Input:
   "I wanted to, um, I was thinking, actually, can you send the report?"

   Output:
   "Can you send the report?"

6. When uncertain whether something is a false start, KEEP IT.

7. Fix punctuation, capitalization, spacing, and sentence boundaries.

8. Add paragraph breaks when the speaker clearly changes topics.

9. Preserve the speaker's natural tone.
   Casual speech must remain casual.

10. Preserve ALL meaningful:
    - words
    - details
    - instructions
    - names
    - numbers
    - dates and times
    - URLs
    - email addresses
    - commands
    - code
    - filenames
    - technical terminology
    - product and company names

11. Apply spoken punctuation commands when clearly dictated.

    Input:
    "Hello John comma how are you question mark"

    Output:
    "Hello John, how are you?"

12. NEVER:
    - summarize
    - paraphrase
    - shorten for conciseness
    - rewrite for style
    - add information
    - infer missing information
    - remove meaningful information

13. If the transcript is already clean, return it essentially unchanged,
    correcting only punctuation or capitalization if necessary.

CRITICAL PRESERVATION RULE:

Make the MINIMUM number of edits necessary to clean the transcript.

When deciding whether to delete something:
- If it is definitely a filler, repetition, stutter, abandoned false start,
  or explicitly corrected information, remove it.
- If you are uncertain, KEEP IT.

OUTPUT RULE:

Return ONLY the cleaned transcript.
Do not explain your changes.
Do not add labels.
Do not add quotation marks."#;

/// A configured, reachable transcript formatter backed by an Ollama model.
pub struct Formatter {
    client: reqwest::blocking::Client,
    url: String,
    model: String,
    level: FormatLevel,
}

impl Formatter {
    /// Build a formatter from config, verifying the Ollama server is reachable and the model is
    /// available. Returns `None` (cleanup disabled) when `format = "off"` or the server/model is
    /// unavailable — the pipeline then uses raw transcripts.
    pub fn new(config: &Config) -> Option<Formatter> {
        if config.format == FormatLevel::Off {
            info!("formatter disabled (format = off)");
            return None;
        }
        let url = config.formatter_url.trim_end_matches('/').to_string();
        let model = config.formatter_model.clone();

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(config.formatter_timeout_ms))
            .build()
            .ok()?;

        match model_available(&client, &url, &model) {
            Ok(true) => {
                info!(url = %url, model = %model, level = ?config.format, "formatter ready");
                Some(Formatter {
                    client,
                    url,
                    model,
                    level: config.format,
                })
            }
            Ok(false) => {
                warn!(
                    model = %model,
                    "formatter model not found in Ollama; cleanup disabled (run `ollama pull {model}`)"
                );
                None
            }
            Err(e) => {
                warn!(url = %url, %e, "Ollama unreachable; transcript cleanup disabled");
                None
            }
        }
    }

    /// Clean `raw`, falling back to `raw` on any error, timeout, or empty result. Never panics.
    pub fn clean(&self, raw: &str) -> String {
        if raw.trim().is_empty() {
            return raw.to_string();
        }
        match self.request(raw) {
            Ok(cleaned) => {
                let cleaned = capitalize_first(&strip_reasoning(&cleaned));
                if cleaned.is_empty() {
                    warn!("formatter returned empty output; using raw transcript");
                    raw.to_string()
                } else {
                    cleaned
                }
            }
            Err(e) => {
                warn!(%e, "formatter request failed; using raw transcript");
                raw.to_string()
            }
        }
    }

    /// Issue the `/api/chat` request and return the assistant message content.
    fn request(&self, raw: &str) -> reqwest::Result<String> {
        let system = format!("{BASE_PROMPT}\n\nINTENSITY: {}", intensity_line(self.level));
        let body = ChatRequest {
            model: &self.model,
            stream: false,
            // Qwen3 is a hybrid "thinking" model; disable reasoning so we get only the transcript.
            think: false,
            messages: vec![
                ChatMessage { role: "system", content: &system },
                ChatMessage { role: "user", content: raw },
            ],
            options: ChatOptions { temperature: 0.2 },
        };
        let resp: ChatResponse = self
            .client
            .post(format!("{}/api/chat", self.url))
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        Ok(resp.message.content)
    }
}

/// Query `/api/tags` and report whether `model` is installed.
fn model_available(client: &reqwest::blocking::Client, url: &str, model: &str) -> reqwest::Result<bool> {
    let tags: TagsResponse = client
        .get(format!("{url}/api/tags"))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(tags.models.iter().any(|m| m.name == model))
}

/// Per-level guidance appended to the base prompt.
fn intensity_line(level: FormatLevel) -> &'static str {
    match level {
        FormatLevel::Off => "None.",
        FormatLevel::Light => {
            "Light — only remove obvious fillers (um/uh) and fix punctuation and capitalization. \
             Do NOT remove false starts or repetitions unless unmistakable."
        }
        FormatLevel::Medium => {
            "Medium — apply all cleanup rules with balanced, conservative judgment."
        }
        FormatLevel::High => {
            "High — apply all cleanup rules thoroughly: remove fillers, stutters, and clearly \
             abandoned false starts, while still preserving every meaningful word."
        }
    }
}

/// Remove any `<think>…</think>` reasoning block (defensive; `think:false` should prevent it) and
/// surrounding whitespace/quotes.
fn strip_reasoning(text: &str) -> String {
    let mut s = text;
    // Drop everything up to and including a closing think tag, if present.
    if let Some(idx) = s.to_lowercase().rfind("</think>") {
        s = &s[idx + "</think>".len()..];
    }
    let trimmed = s.trim();
    // Strip a single pair of wrapping quotes if the model added them.
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(trimmed);
    unquoted.trim().to_string()
}

/// Deterministically uppercase the first non-whitespace character if it is a letter — a safety
/// net since the small model capitalizes inconsistently. If the transcript starts with a digit
/// or symbol (e.g. "42 items"), it is left untouched.
fn capitalize_first(text: &str) -> String {
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        if !c.is_alphabetic() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..i]);
        out.extend(c.to_uppercase());
        out.push_str(&text[i + c.len_utf8()..]);
        return out;
    }
    text.to_string()
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    stream: bool,
    think: bool,
    messages: Vec<ChatMessage<'a>>,
    options: ChatOptions,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatOptions {
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_think_block_and_quotes() {
        assert_eq!(
            strip_reasoning("<think>hmm the user said hi</think>\n\"Hello there.\""),
            "Hello there."
        );
    }

    #[test]
    fn passes_clean_text_through() {
        assert_eq!(strip_reasoning("  Send the report.  "), "Send the report.");
    }

    #[test]
    fn intensity_differs_per_level() {
        assert_ne!(intensity_line(FormatLevel::Light), intensity_line(FormatLevel::High));
    }

    #[test]
    fn capitalizes_first_letter_only() {
        assert_eq!(capitalize_first("hello there."), "Hello there.");
        assert_eq!(capitalize_first("send the report."), "Send the report.");
        // Leading digit/symbol: leave untouched (don't capitalize a later word).
        assert_eq!(capitalize_first("42 items were shipped"), "42 items were shipped");
        assert_eq!(capitalize_first(""), "");
    }

    #[test]
    fn base_prompt_has_no_parrotable_examples() {
        // Small models copy concrete `Input:/Output:` examples verbatim — keep the prompt free of them.
        assert!(!BASE_PROMPT.contains("Input:"));
        assert!(!BASE_PROMPT.contains("Output:"));
    }
}
