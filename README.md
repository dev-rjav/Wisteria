# Wisteria 🌸

**Open-source, local-first voice dictation.** Hold a key, speak, release — clean, formatted text
appears at your cursor in any app. Windows · Linux · macOS.

Your voice never leaves your machine: a local speech-to-text model (Parakeet / Whisper) does the
transcription, and a tiny local LLM strips the "um"s, fixes punctuation, and formats the result —
all in a few hundred milliseconds. No cloud, no account, no telemetry.

> Black & lavender. A quiet night garden instead of a cloud.

---

## Download

**Windows 10/11 (64-bit):**
[⬇ Download the Wisteria installer](https://github.com/dev-rjav/Wisteria/releases/latest/download/Wisteria-Setup-Windows-x64.exe)

Just download and run it — no extra steps, no build tools required. The installer sets up the app,
adds the WebView2 runtime if needed, and offers to install the optional local formatter (Ollama).
On first launch, Wisteria downloads its speech-to-text model automatically.

Prefer to browse versions and release notes? See the
[Releases page](https://github.com/dev-rjav/Wisteria/releases). macOS and Linux builds are on the
roadmap.

---

## Why Wisteria

Most polished dictation tools stream your voice to proprietary models on rented GPUs. Wisteria
runs the whole pipeline **on your machine**:

```
global hotkey ▶ warm mic ▶ VAD ▶ local ASR model ▶ local small-LLM cleanup ▶ paste at cursor
```

- **100% offline by default.** Nothing is uploaded; there is no account and no telemetry.
- **Cross-platform, first-class.** Windows, Linux, and macOS are all supported — OS-specific code
  is isolated in per-platform modules.
- **Open models, swappable.** Pick your ASR and formatter models like picking a theme.
- **Optional BYOK cloud.** You can point the formatter at your own API key for cloud-grade polish,
  but it is never required and never the default.

---

## How it works

Two local model stages (see [models-research.md](models-research.md) for the full landscape and
our default picks):

- **Stage A — Transcription (ASR):** pluggable local speech-to-text. Default
  **Parakeet TDT 0.6B v3** (realtime English/European), with **Whisper large-v3-turbo** for
  99-language mode and **Moonshine** for low-end hardware.
- **Stage B — Formatting (LLM):** a very small, fast local model (default **Qwen3** family, run via
  [Ollama](https://ollama.com)) that removes fillers and false starts, fixes
  punctuation/capitalization, and shapes the text for the target app. Every stage falls back to the
  raw transcript if the formatter is unavailable — dictation never blocks on it.

The formatter is entirely optional: with cleanup intensity set to **Off**, the LLM stage is skipped
and the raw transcript is pasted directly.

---

## Features

- **Push-to-talk** global hotkey (hold to talk), configurable per OS (default **F8**).
- **Hands-free lock** — double-tap the hotkey to keep recording without holding; tap again to
  finish.
- **Warm mic** capture (`cpal`), auto-selecting a real input device and skipping system-loopback
  endpoints.
- **Paste-at-cursor** delivery via simulated Ctrl/⌘+V with clipboard save/restore.
- **Cleanup intensity** — Off / Light / Medium / High, plus per-behavior **Transforms**
  (auto-punctuation, filler removal, smart capitalization, email/number formatting) that physically
  compose the formatter prompt.
- **Writing styles** — Concise (faithful cleanup) / Professional / Casual / Detailed.
- **Personal dictionary** — teach it your names, jargon, and brands; a conservative deterministic
  matcher (Soundex + Jaro-Winkler) fixes sound-alikes even when the LLM is off, with import/export.
- **Voice snippets** — say a keyword + trigger phrase to expand verbatim text.
- **Ask AI mode** (opt-in) — open a dictation with a keyword and the spoken request is answered by
  the local LLM and the answer is pasted instead of a transcript.
- **History** window with per-entry copy and real day grouping.
- **The Dock** — a small, frameless, always-on-top overlay at the bottom of the screen that renders
  a flowing lavender **wave** with your voice (idle → listening → processing → done).

---

## Repository layout

A Cargo workspace of three crates plus the frontend assets:

```
Wisteria/
├── Cargo.toml                     # Workspace manifest (shared deps, release profile)
├── README.md                      # This file
├── models-research.md             # ASR + formatter model landscape and our default picks
└── crates/
    ├── wisteria-core/             # Headless dictation pipeline (the library)
    │   └── src/
    │       ├── lib.rs             # Crate root; composes the stages, TARGET_SAMPLE_RATE
    │       ├── config.rs          # User config (config.toml): hotkey, models, transforms, styles…
    │       ├── models.rs          # Download + verify model artifacts (gitignored, fetched on first run)
    │       ├── audio.rs           # Warm cpal capture → 16 kHz mono f32
    │       ├── hotkey.rs          # Global push-to-talk listener (rdev) + double-tap lock machine
    │       ├── asr.rs             # Warm Parakeet/Whisper engine (transcribe-rs / ONNX Runtime)
    │       ├── format.rs          # Optional LLM cleanup + Ask AI (Ollama), prompt composition
    │       ├── dictionary.rs      # Deterministic custom-vocabulary matcher (Soundex + Jaro-Winkler)
    │       ├── snippets.rs        # Keyword-gated voice text expansion
    │       ├── paste.rs           # Clipboard save → set → synthetic paste → restore
    │       └── engine.rs          # Restartable pipeline that the CLI/GUI drive on worker threads
    ├── wisteria-cli/              # Headless daemon — the console is the UI (Phase 1)
    │   └── src/main.rs
    └── wisteria-gui/              # Tauri v2 desktop app (workspace window + Dock overlay)
        ├── src/main.rs            # Tauri commands, tray, single-instance, window lifecycle
        ├── src/ollama.rs          # Local Ollama HTTP client (list + stream-pull formatter models)
        ├── tauri.conf.json        # Window/bundle config (main window + transparent Dock)
        ├── capabilities/          # Tauri capability allowlist
        └── dist/                  # Frontend (index.html, dock.html, app.js, dock.js, wave.js, styles.css)
```

### The core stages at a glance

| Module | Responsibility |
|---|---|
| `config` | Persist/load `config.toml` in the platform app-data dir; forward-compatible defaults |
| `models` | Ensure ASR artifacts are downloaded and verified (never committed) |
| `audio` | Keep the mic stream warm; downmix + resample to 16 kHz mono; drop <300 ms clips |
| `hotkey` | Consume the PTT key globally (grab on Win/macOS, listen on Linux); PTT + hands-free lock |
| `asr` | Load the ASR engine once and keep it resident so latency is just inference |
| `format` | Compose the cleanup prompt from transforms/style/dictionary; call Ollama; Ask AI |
| `dictionary` | Conservative, always-on correction of custom vocabulary |
| `snippets` | Expand `<keyword> <trigger>` spoken phrases to verbatim text |
| `paste` | Deliver text without losing the user's clipboard |
| `engine` | Own the recorder, ASR, and formatter on worker threads; hot-reload on config change |

Config lives at `%LOCALAPPDATA%/wisteria/config.toml` (Windows), `~/.config/wisteria/config.toml`
(Linux), or `~/Library/Application Support/wisteria/config.toml` (macOS). Model weights are
downloaded on first run and are **never** committed.

---

## Build & run

**Prerequisites**

- [Rust](https://rustup.rs/) (stable). On Windows, the MSVC toolchain + VS Build Tools (C++) are
  required for linking.
- [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS (WebView2 on
  Windows; `webkit2gtk` etc. on Linux).
- [Ollama](https://ollama.com) running locally **if** you want the LLM cleanup / Ask AI stages
  (e.g. `ollama pull qwen3:0.6b`). Skip it to run transcription-only.

**Headless daemon (CLI)**

```bash
cargo run -p wisteria-cli
```

The console is the UI: it loads config, downloads and warms the models, then arms the push-to-talk
loop. Hold the hotkey, speak, release — the transcript is pasted into the focused app.

**Desktop app (GUI)**

```bash
# with the Tauri CLI installed (cargo install tauri-cli --version '^2')
cargo tauri dev --config crates/wisteria-gui/tauri.conf.json

# or run the binary directly
cargo run -p wisteria-gui
```

---

## Building the Windows installer

> Just want to use Wisteria? Grab the prebuilt installer from [Download](#download) above — you
> don't need any of this. This section is for building the installer yourself.

Wisteria ships as a **single standalone GUI installer** (`.exe`) built with Tauri's NSIS bundler.
End users need **no** Rust, MSVC, or build tools — only the build machine does. The installer:

- ships the prebuilt app and **auto-installs the WebView2 runtime** if it's missing;
- installs **per-user** (no admin / UAC prompt);
- after install, **prompts to install Ollama** for the optional local AI formatter model — decline
  and Wisteria still runs transcription-only (see
  [`crates/wisteria-gui/installer/hooks.nsh`](crates/wisteria-gui/installer/hooks.nsh)).

Parakeet (speech-to-text) and the formatter model are fetched on first run / from inside the app,
so the installer stays small (~10 MB).

**Build it** (on a Windows machine with the prerequisites above + `cargo install tauri-cli --version '^2'`):

```powershell
pwsh ./scripts/build-windows-installer.ps1
# → target/release/bundle/nsis/Wisteria_<version>_x64-setup.exe
```

or directly:

```powershell
cd crates/wisteria-gui
cargo tauri build --bundles nsis
```

> macOS and Linux installers (`.dmg`, `.AppImage`/`.deb`) come from the same `cargo tauri build`
> on those OSes and are tracked for a later phase.

---

## Configuration

Everything is editable in the GUI, and persisted to `config.toml`. Highlights:

- `ptt_key` — push-to-talk key or `+`-separated combo (default `F8`).
- `model` — ASR model identifier.
- `format` — cleanup intensity (`off` / `light` / `medium` / `high`).
- `style` — writing voice (`concise` / `professional` / `casual` / `detailed`).
- `transforms` — per-behavior toggles (all on by default).
- `formatter_url` / `formatter_model` / `formatter_timeout` — the local Ollama endpoint + model.
- `dictionary` — custom vocabulary words.
- `snippets` + `snippet_keyword` — voice text expansions.
- `ask_ai_enabled` + `ask_ai_keyword` — opt-in Ask AI mode.

---

## Roadmap

- **Phase 0** — research, name/brand, repo scaffold ✅
- **Phase 1** — MVP: hotkey → record → transcribe → paste ✅
- **Phase 2** — Dock overlay with wave animation; hands-free mode; cancel ✅
- **Phase 3** — Formatter LLM stage + cleanup intensity + transforms + styles ✅
- **Phase 4** — History, dictionary + auto-learn, model manager ✅ (auto-learn in progress)
- **Phase 5** — Context awareness (active-app detection → tone), richer command mode

---

## Contributing

Cross-platform is non-negotiable — no feature is merged if it only works on one OS unless it's
explicitly stubbed with a tracked TODO for the others. Local-first is non-negotiable — cloud (BYOK)
is always optional, never required, never default. Commits follow
[Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `chore:`,
`refactor:`) and stay small and atomic.

## License

Dual-licensed under **MIT OR Apache-2.0**, at your option.
