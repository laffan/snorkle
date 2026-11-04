#!/bin/sh
# Copies the keys of src-tauri/ios/Entitlements.plist into the entitlements
# file of the Xcode project that `tauri ios init` generated
# (src-tauri/gen/apple/<name>_iOS/<name>_iOS.entitlements). Tauri 2.12 merges
# Info.ios.plist into the app on every build but never touches entitlements,
# and `tauri ios init` writes that file empty, so without this step the app
# is signed without com.apple.developer.web-browser and iPadOS never lists it
# under Settings → Apps → Default Apps → Browser App.
#
#   sh scripts/ios-entitlements.sh                        all keys
#   sh scripts/ios-entitlements.sh --without-web-browser  leave out the
#       default-browser entitlement (signing fails with it until Apple has
#       granted it for the bundle identifier and it is enabled for the App ID)
#
# Keys already in the generated file that the source does not name (added
# by Xcode or a plugin) are kept. Run on macOS (PlistBuddy).
# SECURITY-REVIEW: what this writes is what the app is signed with.
set -eu
cd "$(dirname "$0")/.."

without_browser=0
for a in "$@"; do
  case "$a" in
    --without-web-browser) without_browser=1 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

src=src-tauri/ios/Entitlements.plist
pb=/usr/libexec/PlistBuddy
[ -x "$pb" ] || { echo "PlistBuddy not found (run on macOS)" >&2; exit 1; }
dst=$(ls src-tauri/gen/apple/*_iOS/*_iOS.entitlements 2>/dev/null | head -n 1 || true)
[ -n "$dst" ] || { echo "no generated entitlements file; run 'npm run tauri ios init' first" >&2; exit 1; }
plutil -lint -s "$dst" >/dev/null 2>&1 || printf '%s\n' \
  '<?xml version="1.0" encoding="UTF-8"?>' \
  '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
  '<plist version="1.0"><dict/></plist>' >"$dst"

# The source is a flat dictionary (no nested <key>s), so every <key> is a
# top-level entitlement.
keys=$(sed -n 's:^[[:space:]]*<key>\([^<]*\)</key>.*:\1:p' "$src")
for k in $keys; do
  "$pb" -c "Delete :$k" "$dst" >/dev/null 2>&1 || true
done
"$pb" -c "Merge $src" "$dst" >/dev/null
if [ "$without_browser" = 1 ]; then
  "$pb" -c "Delete :com.apple.developer.web-browser" "$dst" >/dev/null 2>&1 || true
fi

echo "Entitlements in $dst:"
for k in $keys; do
  if "$pb" -c "Print :$k" "$dst" >/dev/null 2>&1; then echo "  $k"; fi
done
