//! Ollama model management for the GUI: list installed models and stream-pull new ones.
//!
//! Talks to the local Ollama HTTP API directly (no dependency on the `ollama` binary being on
//! PATH). Used to populate the formatting-model picker with what's installed vs. downloadable.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A curated, `ollama pull`-able model for transcript formatting, with guidance from our local
/// benchmark (see research/model-benchmark/comparison.md).
pub struct CuratedModel {
    pub name: &'static str,
    pub size: &'static str,
    pub note: &'static str,
    pub recommended: bool,
}

/// Formatting models we suggest, smallest→largest. `installed` is resolved at runtime.
pub const CURATED: &[CuratedModel] = &[
    CuratedModel { name: "qwen3:0.6b", size: "0.5 GB", note: "Fastest; light cleanup, may drop words", recommended: false },
    CuratedModel { name: "qwen3:1.7b", size: "1.4 GB", note: "Fast; good everyday cleanup", recommended: false },
    CuratedModel { name: "qwen3:4b", size: "2.5 GB", note: "Best balance of speed and quality", recommended: true },
    CuratedModel { name: "llama3.2:1b", size: "1.3 GB", note: "Small Llama; decent punctuation", recommended: false },
    CuratedModel { name: "llama3.2:3b", size: "2.0 GB", note: "Strong general cleanup", recommended: false },
    CuratedModel { name: "gemma3:1b", size: "0.8 GB", note: "Tiny Gemma; fast", recommended: false },
    CuratedModel { name: "gemma3:4b", size: "3.3 GB", note: "High quality; heavier", recommended: false },
];

/// One entry in the formatting-model picker.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub installed: bool,
    pub size: String,
    pub note: String,
    pub recommended: bool,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: u64,
}

/// Progress update while pulling a model.
#[derive(Debug, Clone, Serialize)]
pub struct PullProgress {
    pub model: String,
    pub status: String,
    pub percent: f64,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct PullLine {
    #[serde(default)]
    status: String,
    #[serde(default)]
    completed: u64,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    error: Option<String>,
}

fn client(timeout: Duration) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder().timeout(timeout).build()
}

/// Names of models currently installed in Ollama. Empty (with `Ok`) is a reachable-but-empty
/// server; `Err` means Ollama is unreachable.
pub fn installed(url: &str) -> reqwest::Result<Vec<TagInfo>> {
    let url = url.trim_end_matches('/');
    let resp: TagsResponse = client(Duration::from_secs(4))?
        .get(format!("{url}/api/tags"))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(resp
        .models
        .into_iter()
        .map(|m| TagInfo {
            name: m.name,
            size: human_size(m.size),
        })
        .collect())
}

/// An installed model's name + human-readable size.
pub struct TagInfo {
    pub name: String,
    pub size: String,
}

/// Build the picker list: every installed model (marked installed) unioned with the curated
/// downloadable set (marked not-installed). Reachability is signalled by the `Ok`/`Err` of the
/// tags call by the caller; here we just merge.
pub fn model_list(installed: &[TagInfo]) -> Vec<ModelEntry> {
    let mut entries: Vec<ModelEntry> = Vec::new();

    // Installed first.
    for t in installed {
        let curated = CURATED.iter().find(|c| c.name == t.name);
        entries.push(ModelEntry {
            name: t.name.clone(),
            installed: true,
            size: if t.size.is_empty() {
                curated.map(|c| c.size.to_string()).unwrap_or_default()
            } else {
                t.size.clone()
            },
            note: curated.map(|c| c.note.to_string()).unwrap_or_default(),
            recommended: curated.map(|c| c.recommended).unwrap_or(false),
        });
    }

    // Then curated models not already installed.
    for c in CURATED {
        if installed.iter().any(|t| t.name == c.name) {
            continue;
        }
        entries.push(ModelEntry {
            name: c.name.to_string(),
            installed: false,
            size: c.size.to_string(),
            note: c.note.to_string(),
            recommended: c.recommended,
        });
    }

    entries
}

/// Pull `model` from Ollama, invoking `on_progress` for each streamed status line. Blocks until
/// the pull finishes, errors, or `cancel` is set. Run on a background thread. When cancelled, the
/// streaming connection is dropped (which aborts the transfer) and a final `cancelled` progress
/// with `done: true` is emitted.
pub fn pull(url: &str, model: &str, cancel: Arc<AtomicBool>, mut on_progress: impl FnMut(PullProgress)) {
    let url = url.trim_end_matches('/');
    let body = serde_json::json!({ "name": model, "stream": true });

    // No overall timeout: a large pull can take minutes. Read-streaming below reports progress.
    let client = match reqwest::blocking::Client::builder().build() {
        Ok(c) => c,
        Err(e) => {
            on_progress(err(model, format!("http client: {e}")));
            return;
        }
    };

    let resp = match client.post(format!("{url}/api/pull")).json(&body).send() {
        Ok(r) => r,
        Err(e) => {
            on_progress(err(model, format!("connect: {e}")));
            return;
        }
    };
    if let Err(e) = resp.error_for_status_ref() {
        on_progress(err(model, format!("server: {e}")));
        return;
    }

    let reader = BufReader::new(resp);
    for line in reader.lines() {
        // Stop and drop the connection if the user cancelled.
        if cancel.load(Ordering::SeqCst) {
            on_progress(cancelled(model));
            return;
        }
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                on_progress(err(model, format!("stream: {e}")));
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let parsed: PullLine = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(e) = parsed.error {
            on_progress(err(model, e));
            return;
        }
        let percent = if parsed.total > 0 {
            (parsed.completed as f64 / parsed.total as f64) * 100.0
        } else {
            0.0
        };
        let done = parsed.status == "success";
        on_progress(PullProgress {
            model: model.to_string(),
            status: parsed.status,
            percent,
            done,
            error: None,
        });
        if done {
            return;
        }
    }
    // Stream ended without an explicit "success": treat as complete.
    on_progress(PullProgress {
        model: model.to_string(),
        status: "success".into(),
        percent: 100.0,
        done: true,
        error: None,
    });
}

fn err(model: &str, msg: String) -> PullProgress {
    PullProgress {
        model: model.to_string(),
        status: "error".into(),
        percent: 0.0,
        done: true,
        error: Some(msg),
    }
}

fn cancelled(model: &str) -> PullProgress {
    PullProgress {
        model: model.to_string(),
        status: "cancelled".into(),
        percent: 0.0,
        done: true,
        error: Some("cancelled".into()),
    }
}

/// Bytes → e.g. "1.4 GB".
fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    let gb = bytes as f64 / 1_000_000_000.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    }
}
