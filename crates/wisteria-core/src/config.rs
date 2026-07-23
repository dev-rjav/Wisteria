//! User configuration, persisted as `config.toml` in the platform app-data directory
//! (`%LOCALAPPDATA%/WisteriaData` on Windows, `~/.local/share/WisteriaData` on Linux,
//! `~/Library/Application Support/WisteriaData` on macOS). See [`Config::app_data_dir`] for why the
//! folder is intentionally separate from the installer's program-files directory.

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

/// A user-configured cloud formatter service, reached over the **OpenAI-compatible** Chat
/// Completions protocol (`POST {base_url}/chat/completions` with a `Bearer` key). This is the
/// universal denominator supported by OpenRouter, OpenAI, Groq, Together, DeepInfra, LM Studio,
/// and Ollama's own `/v1` endpoint — so "any other service" that speaks it can be linked as a
/// formatter backend. BYOK and always optional: local Ollama stays the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiProvider {
    /// Stable identifier (used by [`Config::formatter_backend`] to select this provider). Generated
    /// by the GUI when the provider is added; never shown to the user.
    pub id: String,
    /// Human-readable label shown in Settings (e.g. "OpenRouter", "My Groq key").
    pub name: String,
    /// OpenAI-compatible base URL, without a trailing `/chat/completions`
    /// (e.g. `https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// Secret API key sent as `Authorization: Bearer <key>`. Stored in `config.toml` (local-only).
    #[serde(default)]
    pub api_key: String,
    /// The model id selected for this provider (e.g. `openai/gpt-4o-mini`). Provider-specific.
    #[serde(default)]
    pub model: String,
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
    /// Which formatter backend is active. Empty or `"local"` = the local Ollama server above;
    /// otherwise the [`ApiProvider::id`] of one of [`Config::api_providers`] (a BYOK cloud service).
    #[serde(default)]
    pub formatter_backend: String,
    /// User-configured OpenAI-compatible cloud formatter services (BYOK). Empty by default —
    /// local-first is preserved; a cloud provider is only ever used when explicitly selected.
    #[serde(default)]
    pub api_providers: Vec<ApiProvider>,
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
    /// Enable "Ask AI" mode: a keyword-prefixed dictation is sent to the LLM as a request and the
    /// generated answer is pasted (instead of the usual transcript cleanup). Off by default.
    pub ask_ai_enabled: bool,
    /// The spoken keyword that opens an Ask-AI request (default `"assistant"`).
    pub ask_ai_keyword: String,
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
            formatter_backend: String::new(),
            api_providers: Vec::new(),
            formatter_timeout_ms: 20000,
            formatter_prompt: String::new(),
            transforms: Transforms::default(),
            style: WritingStyle::default(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
            snippet_keyword: "snippet".to_string(),
            ask_ai_enabled: false,
            ask_ai_keyword: "assistant".to_string(),
        }
    }
}

/// One-time, best-effort migration of user data out of the legacy `wisteria` folder (which, on
/// Windows, collided case-insensitively with the installer's program-files dir) into the current
/// data dir. Runs only when the new dir has no `config.toml` yet and the legacy dir does; it never
/// overwrites existing data, and any failure is ignored (the app falls back to defaults and
/// re-downloads models on first run).
fn migrate_legacy_data(root: &Path, new_dir: &Path) {
    let legacy = root.join("wisteria");
    if legacy.as_path() == new_dir {
        return;
    }
    if new_dir.join("config.toml").exists() || !legacy.join("config.toml").exists() {
        return;
    }
    for name in ["config.toml", "history.json", "gui-state.json"] {
        let src = legacy.join(name);
        if src.exists() {
            let _ = fs::copy(&src, new_dir.join(name));
        }
    }
    // Models are large; a rename within the same volume is instant. If it fails (cross-device or
    // in use), the model simply re-downloads on first run.
    let (src_models, dst_models) = (legacy.join("models"), new_dir.join("models"));
    if src_models.is_dir() && !dst_models.exists() {
        let _ = fs::rename(&src_models, &dst_models);
    }
}

impl Config {
    /// The Wisteria app-data directory (settings, history, downloaded models), creating it if
    /// needed. Windows: `%LOCALAPPDATA%/WisteriaData`, Linux: `~/.local/share/WisteriaData`,
    /// macOS: `~/Library/Application Support/WisteriaData`.
    ///
    /// The folder name is deliberately **not** `wisteria`: on Windows the installer places the app
    /// under `%LOCALAPPDATA%/Wisteria`, and because the filesystem is case-insensitive a data dir
    /// named `wisteria` resolves to the *same folder* as the install dir — so uninstalling the app
    /// would delete the user's history, and updates couldn't cleanly replace program files. Keeping
    /// data in a separate, non-colliding folder means uninstall/update never touches user data.
    pub fn app_data_dir() -> Result<PathBuf> {
        let root = dirs::data_local_dir()
            .context("could not resolve a local data directory for this platform")?;
        let dir = root.join("WisteriaData");
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating app data dir {}", dir.display()))?;
        migrate_legacy_data(&root, &dir);
        Ok(dir)
    }

    /// The active cloud formatter provider, if one is selected (and still exists). Returns `None`
    /// when the backend is local Ollama — the default — so callers fall back to the Ollama path.
    pub fn active_provider(&self) -> Option<&ApiProvider> {
        let id = self.formatter_backend.trim();
        if id.is_empty() || id == "local" {
            return None;
        }
        self.api_providers.iter().find(|p| p.id == id)
    }

    /// The local Ollama model the pipeline is actively using, as `(url, model)`, or `None`.
    /// Returns `None` when a cloud provider is selected (Ollama is untouched) or when neither the
    /// formatter nor Ask AI needs an LLM (`format = off` and Ask AI disabled). Used to decide when
    /// to unload a model from Ollama for memory efficiency on a settings change.
    pub fn active_ollama_model(&self) -> Option<(&str, &str)> {
        let uses_llm = self.format != FormatLevel::Off || self.ask_ai_enabled;
        if !uses_llm || self.active_provider().is_some() {
            return None;
        }
        Some((self.formatter_url.as_str(), self.formatter_model.as_str()))
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
        // New BYOK fields default cleanly on an old config file.
        assert_eq!(parsed.formatter_backend, "");
        assert!(parsed.api_providers.is_empty());
    }

    #[test]
    fn active_provider_resolves_only_when_selected() {
        let mut cfg = Config::default();
        cfg.api_providers.push(ApiProvider {
            id: "p-1".into(),
            name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "sk-x".into(),
            model: "openai/gpt-4o-mini".into(),
        });
        // Default (empty) backend = local Ollama.
        assert!(cfg.active_provider().is_none());
        cfg.formatter_backend = "local".into();
        assert!(cfg.active_provider().is_none());
        // Selecting the provider id resolves it.
        cfg.formatter_backend = "p-1".into();
        assert_eq!(cfg.active_provider().unwrap().model, "openai/gpt-4o-mini");
        // A dangling id (provider removed) falls back to local.
        cfg.formatter_backend = "p-gone".into();
        assert!(cfg.active_provider().is_none());
    }

    #[test]
    fn active_ollama_model_reflects_backend_and_llm_use() {
        let mut cfg = Config::default(); // format=Medium, local backend
        assert_eq!(cfg.active_ollama_model(), Some(("http://127.0.0.1:11434", "qwen3:1.7b")));
        // Off + Ask AI off => nothing loaded.
        cfg.format = FormatLevel::Off;
        assert_eq!(cfg.active_ollama_model(), None);
        // Off but Ask AI on still uses the local model.
        cfg.ask_ai_enabled = true;
        assert!(cfg.active_ollama_model().is_some());
        // Selecting a cloud provider => Ollama untouched.
        cfg.format = FormatLevel::Medium;
        cfg.api_providers.push(ApiProvider {
            id: "p-1".into(), name: "OpenRouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(), api_key: "k".into(),
            model: "openai/gpt-4o-mini".into(),
        });
        cfg.formatter_backend = "p-1".into();
        assert_eq!(cfg.active_ollama_model(), None);
    }

    #[test]
    fn providers_round_trip_through_toml() {
        let mut cfg = Config::default();
        cfg.formatter_backend = "p-1".into();
        cfg.api_providers.push(ApiProvider {
            id: "p-1".into(),
            name: "Groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key: "gsk".into(),
            model: "llama-3.1-8b-instant".into(),
        });
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.formatter_backend, "p-1");
        assert_eq!(parsed.api_providers.len(), 1);
        assert_eq!(parsed.active_provider().unwrap().name, "Groq");
    }
}
