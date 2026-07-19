# Wisteria 🌸

**Open-source, local-first voice dictation.** Hold a key, speak, release — clean, formatted text
appears at your cursor in any app. Windows · Linux · macOS.

Your voice never leaves your machine: a local speech-to-text model (Parakeet / Whisper) does the
transcription, and a tiny local LLM strips the "um"s, fixes punctuation, and formats the result —
all in a few hundred milliseconds.

> Black & lavender. A quiet night garden instead of a cloud.

## Status

🌱 Early — research and scaffolding phase. See [project.md](project.md) for vision, brand, and
roadmap; [research.md](research.md) for how Wispr Flow (our closed-source reference) works; and
[models-research.md](models-research.md) for the model landscape.

## Planned pipeline

```
global hotkey ▶ warm mic ▶ Silero VAD ▶ local ASR ▶ small local LLM cleanup ▶ paste at cursor
```

## License

TBD (leaning MIT).
