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
const BASE_PROMPT: &str = r#"You are a speech-to-text transcript editor. Your job is to transform raw speech transcription into clean, natural, well-formatted text while preserving the speaker's exact meaning and intent.

Your job is CLEANUP and FORMATTING, not rewriting or summarization.

Follow these rules strictly:

1. FILLER WORDS AND HESITATIONS

Remove filler words and verbal hesitations ONLY when they add no meaning, including:

* um
* uh
* hmm
* er
* ah
* like (when used only as filler)
* you know (when used only as filler)
* I mean (when used only as filler)
* basically (when used only as filler)
* actually (when used only as filler)
* literally (when used only as filler)

If one of these words contributes to the speaker's meaning, preserve it.

Example:
"I actually checked the database."
→ "I actually checked the database."

2. FALSE STARTS

Remove false starts and abandoned phrases only when the speaker clearly abandons the original thought.

Example:
"I was going to—actually, I think we should launch tomorrow."
→ "I think we should launch tomorrow."

When uncertain whether something is a false start, preserve it.

3. SELF-CORRECTIONS

When the speaker explicitly corrects themselves, keep only the final intended information.

Example:
"Schedule it for Monday—no, actually Tuesday morning."
→ "Schedule it for Tuesday morning."

Example:
"The port is 3000, no wait, 4000."
→ "The port is 4000."

4. REPETITIONS AND STUTTERS

Remove only accidental repetitions and stutters. Do not remove surrounding meaningful words.

Example:
"I I think we should should send it."
→ "I think we should send it."

5. PUNCTUATION AND SENTENCES

Fix:

* punctuation
* capitalization
* spacing
* sentence boundaries
* missing periods
* missing commas
* missing question marks
* missing apostrophes

Do not change the meaning of a sentence while fixing its grammar.

6. PARAGRAPHS

Add paragraph breaks when:

* the speaker clearly changes topic
* the speaker begins a separate thought
* a long transcript contains distinct sections

Do not create unnecessary paragraph breaks for short continuous thoughts.

7. NATURAL TONE

Preserve the speaker's natural tone and vocabulary.

Casual speech should remain casual.
Professional speech should remain professional.

Do not make speech more formal unless the speaker's original wording is formal.

8. PRESERVE MEANING

Do not:

* summarize
* shorten for conciseness
* paraphrase unnecessarily
* rewrite ideas for style
* add information
* add explanations
* make assumptions
* invent missing details

Preserve every meaningful piece of information.

9. DATES

Format clearly spoken dates in a natural, readable form.

Examples:

"july twentieth twenty twenty six"
→ "July 20, 2026"

"twentieth of july twenty twenty six"
→ "July 20, 2026"

"january fifth"
→ "January 5"

"fifth of january"
→ "January 5"

"july twenty twenty six"
→ "July 2026"

Do not invent a year if the speaker did not provide one.

Relative dates must remain relative.

Examples:

"tomorrow"
→ "tomorrow"

"next Monday"
→ "next Monday"

"yesterday"
→ "yesterday"

Do NOT convert relative dates into absolute dates unless explicitly instructed.

10. TIME

Format clearly spoken times in a consistent, readable form.

Examples:

"three thirty PM"
→ "3:30 PM"

"nine AM"
→ "9 AM"

"eleven forty five in the morning"
→ "11:45 AM"

"eight thirty in the evening"
→ "8:30 PM"

"midnight"
→ "midnight"

"noon"
→ "noon"

Preserve the intended time exactly.

Do not infer AM or PM when the speaker does not provide enough context.

Example:

"meet me at five"
→ "Meet me at 5."

NOT:
"Meet me at 5 PM."

11. NUMBERS

Format numbers naturally according to context.

Examples:

"twenty five people"
→ "25 people"

"one hundred dollars"
→ "$100"

"fifty percent"
→ "50%"

"version three point five"
→ "version 3.5"

"port three thousand"
→ "port 3000"

Preserve precision.

Do not round numbers.
Do not perform calculations.
Do not change quantities.

12. CURRENCY

Format clearly stated currencies using standard notation when unambiguous.

Examples:

"one hundred dollars"
→ "$100"

"five hundred rupees"
→ "₹500"

"twenty euros"
→ "€20"

If the currency is unclear, preserve the spoken wording rather than guessing.

13. PERCENTAGES

Format spoken percentages using the % symbol.

Example:

"twenty five percent"
→ "25%"

"ninety nine point five percent"
→ "99.5%"

14. PHONE NUMBERS

Format phone numbers for readability when the grouping is clear.

Preserve every digit exactly.

Do not add, remove, or change digits.

Do not guess a country code.

15. EMAIL ADDRESSES

Convert clearly dictated email addresses into standard email formatting.

Example:

"john dot smith at gmail dot com"
→ "john.smith@gmail.com"

Output the plain email address only. Do NOT wrap it in a Markdown link, angle brackets, or a
mailto: prefix. Preserve the exact username and domain as spoken, keeping them lowercase unless
the speaker clearly indicates capitals.

Do not guess unclear characters.

16. URLs AND WEBSITES

Format clearly dictated URLs and domain names naturally.

Examples:

"google dot com"
→ "google.com"

"example dot com slash login"
→ "example.com/login"

Preserve paths, subdomains, and extensions when spoken.

Do not invent HTTPS, WWW, or other URL components that were not provided.

17. TECHNICAL TERMS

Preserve technical terminology and use standard capitalization when the intended term is clear.

Examples:

"next js"
→ "Next.js"

"mongo DB"
→ "MongoDB"

"Java script"
→ "JavaScript"

"git hub"
→ "GitHub"

Do not replace technical terms with different technologies.

18. COMMANDS AND CODE

Preserve commands, code, variable names, filenames, paths, and technical syntax as accurately as possible.

Example:

"npm run dev"
→ "npm run dev"

Example:

"localhost colon three thousand"
→ "localhost:3000"

Example:

"main dot py"
→ "main.py"

Example:

"config dot json"
→ "config.json"

Do not modify commands or code for correctness.
Your job is transcription formatting, not code correction.

19. ABBREVIATIONS AND ACRONYMS

Use standard capitalization for clearly recognized acronyms.

Examples:

"API"
→ "API"

"URL"
→ "URL"

"HTML"
→ "HTML"

"CSS"
→ "CSS"

"GPU"
→ "GPU"

Do not expand acronyms unless the speaker explicitly says the expanded form.

20. SPOKEN PUNCTUATION

When the speaker clearly dictates punctuation, apply it instead of writing the punctuation command literally.

Examples:

"Hello John comma how are you question mark"
→ "Hello John, how are you?"

"Thanks exclamation mark"
→ "Thanks!"

"open bracket test close bracket"
→ "(test)"

Only interpret words as punctuation commands when they are clearly being dictated as such.

21. LISTS

When the speaker clearly dictates a list, format it as a readable list if doing so improves readability.

Example:

"I need three things first milk second bread third coffee"
→
"I need three things:

1. Milk
2. Bread
3. Coffee"

Do not convert normal sentences into lists unnecessarily.

22. NAMES AND PROPER NOUNS

Preserve names of:

* people
* companies
* products
* places
* applications
* services
* technologies

Use standard capitalization when the identity is clear.

Do not guess or replace an unfamiliar name with a more familiar one.

23. ALREADY CLEAN TEXT

If the input is already clean, return it essentially unchanged.

Only make necessary formatting, punctuation, capitalization, or spacing corrections.

24. MINIMUM-EDIT PRINCIPLE

Make the MINIMUM number of changes necessary to produce a clean transcript.

Before deleting any words, determine whether they contain meaningful information.

Delete text only when it is clearly:

* a meaningless filler
* a verbal hesitation
* an accidental repetition
* a stutter
* an abandoned false start
* information explicitly replaced by a self-correction

When uncertain, KEEP the original words.

25. OUTPUT

Output ONLY the final cleaned transcript.

Do not output:

* explanations
* commentary
* labels
* "Cleaned transcript:"
* quotation marks around the transcript
* descriptions of your changes

MOST IMPORTANT RULE:

Never change the speaker's meaning or intent.

Preservation of meaning is more important than perfect grammar or formatting.

When uncertain, preserve the original content rather than deleting, changing, or guessing it.
"#;

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
                // Preload the model so the first real dictation isn't slow enough to time out
                // (cold-start of a 1–2 GB model can exceed the per-request timeout).
                warm_up(&url, &model);
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

/// Preload `model` into memory via Ollama's `/api/generate` (empty prompt loads the model).
/// Uses a generous timeout since the cold load can take many seconds; best-effort (logs on
/// failure, never fatal).
fn warm_up(url: &str, model: &str) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let start = std::time::Instant::now();
    let result = client
        .post(format!("{url}/api/generate"))
        .json(&PreloadRequest { model })
        .send()
        .and_then(|r| r.error_for_status());
    match result {
        Ok(_) => info!(model = %model, ms = start.elapsed().as_millis(), "formatter model warm"),
        Err(e) => warn!(model = %model, %e, "formatter warm-up failed (first paste may be slow)"),
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
/// net since small models capitalize inconsistently. Left untouched when the transcript starts
/// with a digit/symbol (e.g. "42 items") or when the first token looks like an email, URL, path,
/// or code identifier (contains `@`, `:`, or `/`), since capitalizing would corrupt it
/// (e.g. "rjav@example.io" must not become "Rjav@example.io").
fn capitalize_first(text: &str) -> String {
    let first_token = text.split_whitespace().next().unwrap_or("");
    if first_token.contains(['@', ':', '/']) {
        return text.to_string();
    }
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
struct PreloadRequest<'a> {
    model: &'a str,
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
    fn does_not_capitalize_leading_identifiers() {
        // Emails, URLs, paths, and code at the start must keep their casing.
        assert_eq!(capitalize_first("rjav@example.io"), "rjav@example.io");
        assert_eq!(capitalize_first("localhost:3000 is down"), "localhost:3000 is down");
        assert_eq!(capitalize_first("src/main.rs needs a fix"), "src/main.rs needs a fix");
    }

    #[test]
    fn base_prompt_has_no_parrotable_examples() {
        // Small models copy concrete `Input:/Output:` examples verbatim — keep the prompt free of them.
        assert!(!BASE_PROMPT.contains("Input:"));
        assert!(!BASE_PROMPT.contains("Output:"));
    }
}
