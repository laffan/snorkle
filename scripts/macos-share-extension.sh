#!/bin/sh
# Builds Snorkle's macOS share extension ("Snorkle" in the Share menu; owner
# request 2026-11-25) from src-tauri/share/common and src-tauri/share/macos
# into src-tauri/gen/share-extension/SnorkleShare.appex. `npm run tauri build`
# runs this before bundling (src-tauri/tauri.macos.conf.json,
# build.beforeBundleCommand) and copies the result into the app as
# Contents/PlugIns/SnorkleShare.appex (bundle.macOS.files). `tauri dev` does
# not bundle, so it has no share extension.
#
# Tauri signs the app around the extension without re-signing it (no
# --deep), so it is signed here: with APPLE_SIGNING_IDENTITY when that is set
# (hardened runtime, as Tauri signs the app), else ad hoc, which a Mac runs
# for local builds. macOS only loads sandboxed app extensions
# (ShareExtension.entitlements). Needs Xcode or its command-line tools
# (swiftc and the macOS SDK); builds for Apple silicon and Intel.
# SECURITY-REVIEW: what this signs the extension with.
set -eu
cd "$(dirname "$0")/.."

[ "$(uname)" = Darwin ] || { echo "The share extension builds on macOS only." >&2; exit 1; }
conf=src-tauri/tauri.conf.json
version=$(sed -n 's/^  "version": *"\([^"]*\)".*/\1/p' "$conf" | head -n 1)
identifier=$(sed -n 's/^  "identifier": *"\([^"]*\)".*/\1/p' "$conf" | head -n 1)
[ -n "$version" ] && [ -n "$identifier" ] || { echo "No version or identifier in $conf." >&2; exit 1; }

src=src-tauri/share
out=src-tauri/gen/share-extension/SnorkleShare.appex
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sdk=$(xcrun --sdk macosx --show-sdk-path)

rm -rf "$out"
mkdir -p "$out/Contents/MacOS"
for arch in arm64 x86_64; do
  # An extension has no main: NSExtensionMain (Foundation) is its entry point.
  xcrun --sdk macosx swiftc -O -swift-version 5 -target "$arch-apple-macos15.0" -sdk "$sdk" \
    -module-name SnorkleShare -parse-as-library -application-extension \
    -Xlinker -application_extension -Xlinker -e -Xlinker _NSExtensionMain \
    "$src/common/Links.swift" "$src/macos/ShareViewController.swift" \
    -o "$tmp/SnorkleShare-$arch"
done
lipo -create "$tmp/SnorkleShare-arm64" "$tmp/SnorkleShare-x86_64" -output "$out/Contents/MacOS/SnorkleShare"
sed -e "s/__VERSION__/$version/g" -e "s/__IDENTIFIER__/$identifier/g" "$src/macos/Info.plist" > "$out/Contents/Info.plist"
plutil -lint -s "$out/Contents/Info.plist"

ent="$src/macos/ShareExtension.entitlements"
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  codesign --force --sign "$APPLE_SIGNING_IDENTITY" --options runtime --entitlements "$ent" "$out"
else
  codesign --force --sign - --entitlements "$ent" "$out"
fi
codesign --verify --strict "$out"
echo "Share extension: $out ($identifier.share $version, signed ${APPLE_SIGNING_IDENTITY:-ad hoc})"
