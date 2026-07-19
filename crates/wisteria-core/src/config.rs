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
    /// Formatter intensity. `Off` until the M3 formatter stage is wired.
    pub format: FormatLevel,
    /// Localhost port the bundled `llama-server` listens on (formatter stage, M3).
    pub llama_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ptt_key: "Win+Alt".to_string(),
            model: "parakeet-tdt-0.6b-v3-int8".to_string(),
            language: "auto".to_string(),
            format: FormatLevel::Off,
            llama_port: 8080,
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
        assert_eq!(parsed.format, FormatLevel::Off);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Only one field present; the rest must default (thanks to `#[serde(default)]`).
        let parsed: Config = toml::from_str(r#"model = "custom-model""#).unwrap();
        assert_eq!(parsed.model, "custom-model");
        assert_eq!(parsed.ptt_key, Config::default().ptt_key);
    }
}
