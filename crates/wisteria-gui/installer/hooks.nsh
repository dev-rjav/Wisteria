; Wisteria — NSIS installer hooks.
;
; Post-install step: offer to install Ollama, the local engine that runs Wisteria's OPTIONAL
; AI formatter model (it cleans up dictation — removes filler words, fixes punctuation and
; capitalization, all on-device). Wisteria works fully without it (transcription-only), so this
; is a yes/no prompt, and a "no" (or any failure) never blocks the Wisteria install.
;
; The formatter MODEL is not downloaded here — the user picks and pulls it later from inside the
; app (Settings → formatting model), which already talks to the local Ollama server.

!macro NSIS_HOOK_POSTINSTALL
  ; Skip entirely if Ollama is already present in any of its default locations.
  ${If} ${FileExists} "$LOCALAPPDATA\Programs\Ollama\ollama.exe"
  ${OrIf} ${FileExists} "$PROGRAMFILES64\Ollama\ollama.exe"
  ${OrIf} ${FileExists} "$PROGRAMFILES\Ollama\ollama.exe"
    DetailPrint "Ollama is already installed — skipping the formatter setup."
  ${Else}
    MessageBox MB_YESNO|MB_ICONQUESTION \
"Wisteria works out of the box using only on-device speech-to-text.$\r$\n$\r$\nOptionally, it can run a small local AI model (via Ollama) that polishes your dictation: removing filler words and fixing punctuation and capitalization. It runs 100% on your machine.$\r$\n$\r$\nInstall Ollama now?$\r$\n$\r$\n(Recommended. You can choose and download the model afterwards from inside Wisteria. Wisteria still works without it.)" \
      /SD IDNO IDYES wisteria_get_ollama IDNO wisteria_ollama_done

    wisteria_get_ollama:
      DetailPrint "Downloading the Ollama installer (this can take a few minutes)..."
      StrCpy $1 "$TEMP\OllamaSetup.exe"
      ; Download with PowerShell (present on every Win10/11). $$ escapes '$' so it reaches
      ; PowerShell verbatim; $1 is expanded by NSIS to the temp path.
      nsExec::ExecToLog "powershell -NoProfile -ExecutionPolicy Bypass -Command $\"$$ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing -Uri 'https://ollama.com/download/OllamaSetup.exe' -OutFile '$1'$\""
      Pop $0
      ${If} $0 != 0
        MessageBox MB_OK|MB_ICONEXCLAMATION "Ollama could not be downloaded automatically. You can install it anytime from https://ollama.com — Wisteria will still work without it."
        Goto wisteria_ollama_done
      ${EndIf}
      ${IfNot} ${FileExists} "$1"
        MessageBox MB_OK|MB_ICONEXCLAMATION "The Ollama download did not complete. You can install it anytime from https://ollama.com — Wisteria will still work without it."
        Goto wisteria_ollama_done
      ${EndIf}
      DetailPrint "Launching the Ollama installer..."
      ; Run Ollama's own installer (robust across its versions). It is a quick per-user install.
      ExecWait '"$1"' $0
      DetailPrint "Ollama installer exited with code $0."
      Delete "$1"

    wisteria_ollama_done:
  ${EndIf}
!macroend
