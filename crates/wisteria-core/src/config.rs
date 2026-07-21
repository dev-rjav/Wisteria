//! User configuration, persisted as `config.toml` in the platform app-data directory
//! (`%LOCALAPPDATA%/wisteria` on Windows, `~/.config/wisteria` on Linux,
//! `~/Library/Application Support/wisteria` on macOS).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How aggressively the (optional) LLM formatter cleans up the raw transcript.
/// `Off` skips the formatter stage entirely — the default until the M3 stage lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatLevel {
    #[default]
    Off,
    Light,
    Medium,
    High,
}

/// Writing voice the formatter rewrites the transcript into (the app's **Style** page). `Concise`
/// is the neutral default — it keeps the speaker's own wording and only cleans up. The others
/// actively rewrite the tone/structure while still preserving the speaker's meaning and facts (they
/// never invent content). Only meaningful when [`Config::format`] is not [`FormatLevel::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WritingStyle {
    /// Faithful cleanup: keep the speaker's wording and tone, minimal edits.
    #[default]
    Concise,
    /// Polished, formal, business-ready prose.
    Professional,
    /// Relaxed and conversational.
    Casual,
    /// Thorough, structured, and explanatory.
    Detailed,
}

/// Per-behavior formatter toggles, surfaced in the app's **Transforms** page. Each flag, when
/// turned **off**, appends an explicit negative override to the formatter prompt that suppresses
/// the corresponding built-in rule. When every flag is on (the default) the base prompt runs
/// unmodified. Only meaningful when [`Config::format`] is not [`FormatLevel::Off`] — at `Off` the
/// LLM stage is skipped entirely, so transforms don't apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transforms {
    /// Add/repair punctuation and sentence boundaries.
    pub auto_punctuation: bool,
    /// Drop meaningless fillers/hesitations (um, uh, like, you know).
    pub remove_fillers: bool,
    /// Fix capitalization of sentence starts, names, and acronyms.
    pub smart_capitalization: bool,
    /// Normalize dictated emails, URLs, numbers, dates, and times.
    pub email_formatting: bool,
}

impl Default for Transforms {
    fn default() -> Self {
        // All on = the base prompt's full cleanup behavior.
        Transforms {
            auto_punctuation: true,
            remove_fillers: true,
            smart_capitalization: true,
            email_formatting: true,
        }
    }
}

/// A voice text-expansion snippet: say the keyword + `trigger` (e.g. "insert address") and the
/// `expansion` is pasted verbatim in its place. Triggered only when the spoken words after the
/// keyword match a `trigger`; otherwise the text is left untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// The spoken phrase said after the keyword (e.g. "work email").
    pub trigger: String,
    /// The exact text pasted in place of "<keyword> <trigger>".
    pub expansion: String,
}

/// Top-level user configuration. Missing fields fall back to [`Default`] on load, so upgrades
/// that add fields don't break existing config files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Push-to-talk key or `+`-separated combo (e.g. `"ControlRight"`, `"Win+Alt"`).
    pub ptt_key: String,
    /// ASR model identifier (see [`crate::models`]).
    pub model: String,
    /// Spoken language hint, or `"auto"`.
    pub language: String,
    /// Input device name (case-insensitive substring match). Empty = auto-select, preferring a
    /// real microphone over loopback/"Stereo Mix" devices.
    pub input_device: String,
    /// Formatter intensity. `Off` skips the LLM cleanup stage entirely.
    pub format: FormatLevel,
    /// Base URL of the Ollama server used for transcript cleanup.
    pub formatter_url: String,
    /// Ollama model tag used to clean transcripts (e.g. `"qwen3:0.6b"`).
    pub formatter_model: String,
    /// Max time (ms) to wait for the formatter before falling back to the raw transcript.
    pub formatter_timeout_ms: u64,
    /// Custom formatter system prompt. Empty = use the built-in default
    /// ([`crate::format::default_prompt`]). Editable from the app's Settings.
    pub formatter_prompt: String,
    /// Per-behavior formatter toggles (the app's Transforms page).
    pub transforms: Transforms,
    /// Writing voice the formatter rewrites into (the app's Style page).
    pub style: WritingStyle,
    /// Custom vocabulary (names, jargon, brands) the pipeline should spell exactly (the app's
    /// Dictionary page). Each entry is a canonical word or short phrase, cased as it should appear.
    pub dictionary: Vec<String>,
    /// Voice text-expansion snippets (the app's Snippets page).
    pub snippets: Vec<Snippet>,
    /// The spoken keyword that precedes a snippet trigger (default `"snippet"`). Saying this word
    /// followed by a snippet's trigger expands it; on its own it's left as ordinary text.
    pub snippet_keyword: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ptt_key: "F8".to_string(),
            model: "parakeet-tdt-0.6b-v3-int8".to_string(),
            language: "auto".to_string(),
            input_device: String::new(),
            format: FormatLevel::Medium,
            formatter_url: "http://127.0.0.1:11434".to_string(),
            formatter_model: "qwen3:1.7b".to_string(),
            formatter_timeout_ms: 20000,
            formatter_prompt: String::new(),
            transforms: Transforms::default(),
            style: WritingStyle::default(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
            snippet_keyword: "snippet".to_string(),
        }
    }
}

impl Config {
    /// The Wisteria app-data directory, creating it if needed.
    pub fn app_data_dir() -> Result<PathBuf> {
        let dir = dirs::data_local_dir()
            .context("could not resolve a local data directory for this platform")?
            .join("wisteria");
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating app data dir {}", dir.display()))?;
        Ok(dir)
    }

    /// Default config file path (`<app-data>/config.toml`).
    pub fn default_path() -> Result<PathBuf> {
        Ok(Self::app_data_dir()?.join("config.toml"))
    }

    /// Load config from `path`. If the file does not exist, a default config is written there
    /// and returned, so first run leaves a documented, editable file on disk.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if path.exists() {
            let text = fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let config: Config = toml::from_str(&text)
                .with_context(|| format!("parsing config {}", path.display()))?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save(path)?;
            Ok(config)
        }
    }

    /// Convenience: load (or create) the config at [`Config::default_path`].
    pub fn load() -> Result<Config> {
        let path = Self::default_path()?;
        Self::load_or_create(&path)
    }

    /// Serialize and write the config to `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing config")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        fs::write(path, text).with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.ptt_key, config.ptt_key);
        assert_eq!(parsed.format, config.format);
        assert_eq!(parsed.formatter_model, config.formatter_model);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Only one field present; the rest must default (thanks to `#[serde(default)]`).
        let parsed: Config = toml::from_str(r#"model = "custom-model""#).unwrap();
        assert_eq!(parsed.model, "custom-model");
        assert_eq!(parsed.ptt_key, Config::default().ptt_key);
    }
}
