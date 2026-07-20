//! Dev tool for tuning the formatter prompt. Reads transcripts from stdin (one per line) and
//! prints the cleaned output using the real `Formatter`. Model via `FMT_MODEL` env (default
//! qwen3:1.7b), level via `FMT_LEVEL` (off/light/medium/high, default medium).
//!
//! Usage (PowerShell):
//!   "the port is 3000 no actually 4000" | cargo run -q --example fmt -p wisteria-core

use std::io::Read;

use wisteria_core::config::{Config, FormatLevel};
use wisteria_core::format::Formatter;

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();

    let cfg = Config {
        formatter_model: std::env::var("FMT_MODEL").unwrap_or_else(|_| "qwen3:1.7b".into()),
        format: match std::env::var("FMT_LEVEL").unwrap_or_default().as_str() {
            "off" => FormatLevel::Off,
            "light" => FormatLevel::Light,
            "high" => FormatLevel::High,
            _ => FormatLevel::Medium,
        },
        ..Config::default()
    };

    let formatter = Formatter::new(&cfg).expect("formatter unavailable (is Ollama running?)");
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        println!("IN : {line}");
        println!("OUT: {}", formatter.clean(line));
        println!();
    }
}
