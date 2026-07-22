# Model Landscape Research — Transcription & Formatting (2026)

> Which free/open models Wisteria can use for its two model stages:
> **Stage A** — speech → raw text (ASR/STT), and
> **Stage B** — raw text → clean, formatted text (small fast LLM).
> Everything below is open-weight and locally runnable. Compiled 2026-07-19.

---

## Stage A — Open speech-to-text models

### The contenders

| Model | Size | Languages | License | Speed | Notes |
|---|---|---|---|---|---|
| **NVIDIA Parakeet TDT 0.6B v3** | 0.6B | 25 (European) | CC-BY-4.0 | RTFx >2000 (fastest class) | FastConformer-TDT. The dictation-app favorite (Handy, OpenWhispr use it). Near-instant on CPU/GPU |
| **NVIDIA Parakeet TDT 1.1B** | 1.1B | en | CC-BY-4.0 | RTFx >2000 | Highest throughput for high-volume English |
| **Whisper large-v3 / v3-turbo** | 1.55B / 0.8B | 99+ | MIT | Moderate; turbo ~6× faster than v3 | Still the multilingual gold standard. Batch (30 s windows), needs chunking tricks for streaming |
| **Distil-Whisper** | ~0.7B | en | MIT | ~6× Whisper | Good latency/accuracy middle ground |
| **Moonshine v2** (Useful Sensors) | 27 MB – ~400 MB | en (+few) | MIT | Designed for streaming | "Ergodic Streaming Encoder" — true streaming, runs on Raspberry Pi. Smallest footprint of anything usable |
| **IBM Granite Speech 4.1 2B** | 2B | multi | Apache 2.0 | Moderate | **#1 on Open ASR Leaderboard (5.33% mean WER)** — open models now beat proprietary on accuracy |
| **NVIDIA Canary-Qwen 2.5B** | 2.5B | en | CC-BY-4.0 | Slower | Top-tier English accuracy; overkill for realtime dictation |
| **Qwen3-ASR** (Jan 2026) | — | 52 + dialects | open-weight | Good | Language ID + timestamps built in; strong on Asian languages |
| **Voxtral Mini** (Mistral) | 3B | multi | Apache 2.0 | Moderate | ASR + audio understanding in one model (can follow instructions about the audio) |
| **Kyutai STT** | 1B / 2.6B | en/fr | CC-BY-4.0 | True streaming, ~0.5 s delay | Built for live word-by-word output |
| **Vosk** | tiny | 20+ | Apache 2.0 | Fast | Legacy accuracy; only for ultra-low-end fallback |

### Runtimes (how we actually run them)

- **whisper.cpp** — C/C++, GGML; Metal (Mac), Vulkan, CUDA, plain CPU. The cross-platform workhorse for Whisper-family models.
- **faster-whisper** — CTranslate2; ~4× faster than reference on NVIDIA GPUs (Python, heavier to ship).
- **sherpa-onnx** — ONNX Runtime wrapper that runs **Parakeet, Moonshine, Whisper, Zipformer + Silero VAD** with C/C++/Rust bindings on all three OSes. Best single-runtime story for us.
- **transcribe-rs** — the Rust crate Handy uses (whisper.cpp + Parakeet backends). Proven in exactly our use case.
- **VAD**: **Silero VAD** (ONNX, <1 ms per 30 ms slice, MIT-friendly) to gate silence and prevent Whisper hallucinations.

### Recommendation for Wisteria

Model-pluggable from day one (a `ModelBackend` trait), with these defaults:

1. **Default (English/European, realtime feel)** → **Parakeet TDT 0.6B v3** via sherpa-onnx/transcribe-rs. Transcribes a sentence in tens of milliseconds; this is how we hit instant, cloud-app-like latency locally.
2. **Multilingual mode** → **Whisper large-v3-turbo** via whisper.cpp (99 languages, MIT).
3. **Low-end hardware mode** → **Moonshine base** (tiny, streaming, CPU-only fine).
4. **Max-accuracy mode (non-realtime, e.g. voice notes)** → **Granite Speech 4.1 2B** or Canary.

---

## Stage B — Small fast LLM for cleanup & formatting

The job: strip fillers ("um", "uh", false starts), fix punctuation/capitalization, apply light
formatting (lists, casing for the target app), optionally obey spoken corrections ("scratch that").
Latency target <200 ms for a typical utterance (~30–60 words), so the model must be tiny and the
prompt short.

### The contenders

| Model | Params | Quantized size (Q4) | License | Notes |
|---|---|---|---|---|
| **Qwen3-0.6B** | 0.6B | ~400 MB | Apache 2.0 | Shockingly capable for rewriting tasks; thinking mode OFF for speed |
| **Qwen3-1.7B** | 1.7B | ~1.1 GB | Apache 2.0 | Sweet spot: quality cleanup, still fast on CPU |
| **Gemma 3 1B** | 1B | ~720 MB | Gemma license | Very light; runs on 4 GB RAM machines |
| **Gemma 3 270M** | 0.27B | ~200 MB | Gemma license | Fine-tune target: a purpose-tuned 270M formatter could be our endgame |
| **SmolLM3-3B** | 3B | ~1.9 GB | Apache 2.0 | Beats Llama-3.2-3B/Qwen2.5-3B; good "quality" tier |
| **SmolLM2-1.7B** | 1.7B | ~1.1 GB | Apache 2.0 | Solid, fully open (data + recipes public) |
| **Llama 3.2 1B/3B** | 1B/3B | 0.7–1.9 GB | Llama license | Fine but license is less clean than Apache |
| **Phi-4-mini** | 3.8B | ~2.3 GB | MIT | Strong instruction following; heavier |

### Recommendation for Wisteria

- **Default**: **Qwen3-1.7B** (Q4) via **llama.cpp** — Apache 2.0, fast on CPU, good at
  "rewrite this exactly, changing only X" instructions.
- **Low-end tier**: **Qwen3-0.6B** or **Gemma 3 1B**.
- **Later**: fine-tune **Gemma 3 270M** (or Qwen3-0.6B) on speech-cleanup pairs — a dedicated
  ~200 MB formatter model would beat generic models at this one job and run in <50 ms. This is
  our equivalent of the proprietary "token-level formatting control" in commercial apps.
- Keep the cleanup prompt to a few lines (conceptually just: *"Remove filler words,
  fix punctuation and capitalisation, keep the meaning"*) + inject: target-app type, user
  dictionary words, and the 3 cleanup intensity levels (Light/Medium/High).
- **BYOK escape hatch**: optional user-supplied API key (Groq/OpenAI/Anthropic/Gemini) for people
  who want cloud-grade polish — but never required, never default.

---

## Pipeline fit (both stages together)

```
hotkey ▶ mic (16 kHz mono, warm) ▶ Silero VAD gate ▶ Parakeet/Whisper (streaming chunks)
      ▶ raw transcript ▶ Qwen3-1.7B cleanup (app-aware prompt) ▶ paste at cursor
```

Total local latency budget on a mid-range machine: VAD ~0 ms + ASR 50–300 ms + LLM 100–250 ms +
paste ~50 ms → **subjectively instant, no cloud, no audio ever leaving the device** — our core
differentiator vs cloud dictation apps.

---

## Sources

- [Northflank — Best open source STT 2026 (benchmarks)](https://northflank.com/blog/best-open-source-speech-to-text-stt-model-in-2026-benchmarks)
- [Gladia — Best open-source STT models 2026](https://www.gladia.io/blog/best-open-source-speech-to-text-models)
- [Resonant — Moonshine vs Parakeet vs Whisper (local, 2026)](https://www.onresonant.com/resources/local-stt-models-2026)
- [Presenc — Best open-weight STT 2026 (Open ASR Leaderboard)](https://presenc.ai/research/best-open-weight-speech-to-text-models-2026)
- [AssemblyAI — Top 8 open source STT options 2026](https://www.assemblyai.com/blog/top-open-source-stt-options-for-voice-applications)
- [Modelslab — Moonshine vs Whisper realtime benchmark](https://modelslab.com/blog/audio-generation/moonshine-vs-whisper-asr-real-time-speech-2026)
- [PromptQuorum — Phi-4 Mini vs Gemma 3 vs SmolLM on-device](https://www.promptquorum.com/power-local-llm/mobile-llm-models-phi4-gemma-smollm)
- [BentoML — Best open-source small language models 2026](https://www.bentoml.com/blog/the-best-open-source-small-language-models)
- [Klymentiev — Best local LLM by hardware 2026](https://klymentiev.com/blog/best-local-llm)
- [PocketLLM — Best local LLM models 2026](https://pocketllm.app/blog/best-local-llm-models-2026/)
