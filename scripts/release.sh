#!/bin/sh
# Builds a signed and notarized Toranj (app + dmg) and, with --publish,
# creates a GitHub release for the version in tauri.conf.json.
#
# Signing setup lives outside the repo, in a shell file that exports:
#   APPLE_SIGNING_IDENTITY  "Developer ID Application: <name> (<team id>)"
#   APPLE_API_ISSUER        App Store Connect API issuer id
#   APPLE_API_KEY           API key id
#   APPLE_API_KEY_PATH      path to the AuthKey_<id>.p8 file
# Default location: ~/.toranj-signing/env (override with TORANJ_SIGNING_ENV).
#
# Usage: scripts/release.sh [--publish]
set -e
cd "$(dirname "$0")/.."
env_file="${TORANJ_SIGNING_ENV:-$HOME/.toranj-signing/env}"
[ -f "$env_file" ] || { echo "Missing $env_file (see the comment at the top of this script)." >&2; exit 1; }
. "$env_file"
for v in APPLE_SIGNING_IDENTITY APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH; do
  eval "[ -n \"\$$v\" ]" || { echo "$v is not set in $env_file" >&2; exit 1; }
done

version="$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)"
arch="$(uname -m | sed 's/arm64/aarch64/')"
echo "Building Toranj $version ($arch)…"

sh scripts/build-hook.sh release
npm run tauri build -- --bundles app,dmg  # signs the app and notarizes it

target="${CARGO_TARGET_DIR:-target}/release/bundle"
dmg="$target/dmg/Toranj_${version}_${arch}.dmg"
echo "Notarizing $dmg…"
xcrun notarytool submit "$dmg" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple "$dmg"
spctl -a -t open --context context:primary-signature "$dmg"
spctl -a "$target/macos/Toranj.app"
shasum -a 256 "$dmg"

if [ "$1" = "--publish" ]; then
  gh release create "v$version" "$dmg" --title "Toranj $version" --generate-notes
fi
