<#
.SYNOPSIS
    Build the single-file Windows installer for Wisteria.

.DESCRIPTION
    Produces a standalone NSIS GUI installer (.exe) that installs the prebuilt Wisteria app.
    The end user needs NO Rust, no MSVC, and no build tools — only this machine (the build
    machine) does. The installer:
      * ships the compiled wisteria-gui.exe + frontend,
      * auto-installs the WebView2 runtime if it is missing (download bootstrapper),
      * installs per-user (no admin / UAC prompt),
      * after install, offers to install Ollama for the optional local AI formatter model
        (see crates/wisteria-gui/installer/hooks.nsh).

    Parakeet (speech-to-text) and the formatter model are downloaded on first run / from inside
    the app, so the installer stays small (~10 MB).

.PREREQUISITES
    * Rust (stable) + the MSVC toolchain (VS Build Tools "Desktop development with C++").
    * Tauri CLI v2:  cargo install tauri-cli --version "^2"
    * Internet access on the FIRST build (Tauri downloads the NSIS toolchain once).

.EXAMPLE
    pwsh ./scripts/build-windows-installer.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# Repo root = parent of this script's folder.
$repoRoot = Split-Path -Parent $PSScriptRoot
$guiDir   = Join-Path $repoRoot 'crates/wisteria-gui'

Write-Host 'Building Wisteria Windows installer (NSIS)...' -ForegroundColor Cyan
Push-Location $guiDir
try {
    cargo tauri build --bundles nsis
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

$out = Join-Path $repoRoot 'target/release/bundle/nsis'
$exe = Get-ChildItem -Path $out -Filter '*-setup.exe' -ErrorAction SilentlyContinue |
       Sort-Object LastWriteTime -Descending | Select-Object -First 1

if ($null -eq $exe) {
    throw "Build reported success but no installer was found in $out"
}

$sizeMb = [Math]::Round($exe.Length / 1MB, 1)
Write-Host ''
Write-Host "Installer ready: $($exe.FullName)" -ForegroundColor Green
Write-Host "Size: $sizeMb MB" -ForegroundColor Green
