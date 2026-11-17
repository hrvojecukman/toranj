#!/bin/sh
# Builds the Claude Code hook and places it where Tauri expects a sidecar
# (src-tauri/binaries/toranj-hook-<target-triple>), so it ships inside the app.
# Usage: scripts/build-hook.sh [release|debug]
set -e
cd "$(dirname "$0")/.."
command -v cargo >/dev/null 2>&1 || PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
profile="${1:-release}"
triple="$(rustc -vV | sed -n 's/^host: //p')"
if [ "$profile" = "release" ]; then
  cargo build -p toranj-hook --release
  out="${CARGO_TARGET_DIR:-target}/release/toranj-hook"
else
  cargo build -p toranj-hook
  out="${CARGO_TARGET_DIR:-target}/debug/toranj-hook"
fi
mkdir -p src-tauri/binaries
cp "$out" "src-tauri/binaries/toranj-hook-$triple"
