#!/usr/bin/env bash
# Build the Linux bundles (.deb, and optionally AppImage) for Wisteria inside a clean Ubuntu
# container. Intended to be run FROM the container (see docker run invocation in the docs);
# the repo is mounted at /src and artifacts are copied to /src/dist-linux.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
  build-essential curl wget file pkg-config ca-certificates \
  libwebkit2gtk-4.1-dev libssl-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  libasound2-dev libxdo-dev libxtst-dev libxi-dev

# Rust toolchain.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
. "$HOME/.cargo/env"

# Tauri CLI v2.
cargo install tauri-cli --version '^2' --locked

# Build into a container-local target dir (fast; avoids touching the host's Windows target/).
export CARGO_TARGET_DIR=/tmp/target
# AppImage tooling needs FUSE; extract-and-run avoids that inside containers/CI.
export APPIMAGE_EXTRACT_AND_RUN=1
export NO_STRIP=1

cd /src/crates/wisteria-gui

BUNDLES="${1:-deb}"   # e.g. "deb" or "deb,appimage"
cargo tauri build --bundles "$BUNDLES"

mkdir -p /src/dist-linux
find /tmp/target/release/bundle -maxdepth 2 -type f \( -name '*.deb' -o -name '*.AppImage' -o -name '*.rpm' \) -exec cp -v {} /src/dist-linux/ \;
echo "=== Artifacts in dist-linux ==="
ls -la /src/dist-linux/
