#!/usr/bin/env bash
#
# Wisteria — one-command Linux installer.
#
#   curl -fsSL https://raw.githubusercontent.com/dev-rjav/Wisteria/main/install.sh | bash
#
# Downloads the prebuilt Wisteria AppImage (one portable file that runs on virtually any distro),
# installs it to ~/.local/bin, adds an application-menu launcher, and offers to set up Ollama for
# the optional local AI formatter. No compiling, no root required (except the optional Ollama
# install, which uses its own official installer).
#
# Uninstall:  curl -fsSL https://raw.githubusercontent.com/dev-rjav/Wisteria/main/install.sh | bash -s -- --uninstall
#
set -euo pipefail

# ---- config -----------------------------------------------------------------
REPO="dev-rjav/Wisteria"
APPIMAGE_NAME="Wisteria-x86_64.AppImage"           # stable asset name published by CI
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${APPIMAGE_NAME}"
ICON_URL="https://raw.githubusercontent.com/${REPO}/main/crates/wisteria-gui/icons/128x128.png"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/128x128/apps"
TARGET="$BIN_DIR/Wisteria.AppImage"
DESKTOP="$APP_DIR/wisteria.desktop"
ICON="$ICON_DIR/wisteria.png"

# ---- pretty output ----------------------------------------------------------
if [ -t 1 ]; then B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; N=$'\033[0m'; else B=; G=; Y=; R=; N=; fi
info()  { printf '%s\n' "${B}==>${N} $*"; }
ok()    { printf '%s\n' "${G}✓${N} $*"; }
warn()  { printf '%s\n' "${Y}!${N} $*" >&2; }
die()   { printf '%s\n' "${R}✗ $*${N}" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1; }

# ---- uninstall --------------------------------------------------------------
uninstall() {
  info "Uninstalling Wisteria (your settings/history in ~/.local/share/WisteriaData are kept)"
  rm -f "$TARGET" "$DESKTOP" "$ICON"
  need update-desktop-database && update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
  ok "Removed the app, launcher, and icon."
  info "To also delete your data:  rm -rf ~/.local/share/WisteriaData"
  info "To remove Ollama (if you installed it), see https://github.com/ollama/ollama"
  exit 0
}

[ "${1:-}" = "--uninstall" ] && uninstall

# ---- preflight --------------------------------------------------------------
info "Installing Wisteria for Linux"

case "$(uname -s)" in Linux) ;; *) die "This installer is for Linux. On Windows use the .exe installer." ;; esac
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ;;
  *) die "Unsupported architecture '$ARCH'. Prebuilt Wisteria is x86_64 only for now." ;;
esac

if need curl; then DL='curl -fSL# -o'; elif need wget; then DL='wget -O'; else die "Need 'curl' or 'wget' installed."; fi

# ---- download ---------------------------------------------------------------
mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
info "Downloading the latest AppImage..."
# shellcheck disable=SC2086
$DL "$TARGET.tmp" "$DOWNLOAD_URL" || die "Download failed. No Linux release yet? See https://github.com/${REPO}/releases"
chmod +x "$TARGET.tmp"
mv -f "$TARGET.tmp" "$TARGET"
ok "Installed to $TARGET"

# Best-effort icon (non-fatal).
# shellcheck disable=SC2086
$DL "$ICON" "$ICON_URL" >/dev/null 2>&1 && ok "Installed icon" || warn "Could not fetch icon (cosmetic only)."

# ---- desktop launcher -------------------------------------------------------
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=Wisteria
Comment=Local-first voice dictation
Exec=$TARGET
Icon=wisteria
Terminal=false
Categories=Utility;AudioVideo;Accessibility;
StartupWMClass=Wisteria
EOF
need update-desktop-database && update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
ok "Added application-menu launcher"

# ---- FUSE hint (AppImages need libfuse2 on some distros) --------------------
if ! ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2'; then
  warn "AppImages need FUSE. If Wisteria won't start, install it:"
  warn "  Debian/Ubuntu: sudo apt install libfuse2      Fedora: sudo dnf install fuse-libs      Arch: sudo pacman -S fuse2"
  warn "  (or run it extracted:  $TARGET --appimage-extract-and-run )"
fi

# ---- PATH hint --------------------------------------------------------------
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH. Add this to your shell rc:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

# ---- optional Ollama (formatter) --------------------------------------------
install_ollama() {
  info "Installing Ollama (official installer)..."
  if need curl; then curl -fsSL https://ollama.com/install.sh | sh; else warn "curl required for Ollama install; skipping."; return; fi
  ok "Ollama installed. Pick and download a formatter model later from inside Wisteria."
}

if need ollama; then
  ok "Ollama already installed — the AI formatter is available."
elif [ "${WISTERIA_INSTALL_OLLAMA:-}" = "1" ]; then
  install_ollama
elif [ -e /dev/tty ]; then
  printf '\n%s\n' "${B}Optional:${N} Wisteria can use a small local AI model (via Ollama) to clean up dictation"
  printf '%s\n' "(remove filler words, fix punctuation). It's optional and runs on your machine."
  printf '%s' "Install Ollama now? [y/N] "
  read -r reply </dev/tty || reply=n
  case "$reply" in [yY]*) install_ollama ;; *) info "Skipped. Install later with:  curl -fsSL https://ollama.com/install.sh | sh" ;; esac
else
  info "Skipping Ollama (non-interactive). To include it, re-run with WISTERIA_INSTALL_OLLAMA=1."
fi

# ---- done -------------------------------------------------------------------
printf '\n'
ok "Wisteria is installed."
info "Launch it from your application menu, or run:  ${B}$TARGET${N}"
info "On first run it downloads its speech-to-text model automatically."
