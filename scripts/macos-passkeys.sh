#!/bin/sh
# Builds the macOS app signed with Apple's passkey entitlement for browsers
# (com.apple.developer.web-browser.public-key-credential), so pages can use
# passkeys and security keys for every site. Without it WebKit refuses
# them (Google: passkey_first_auth_factor_error). See README → Passkeys.
#
#   npm run macos:passkeys              tauri build with the entitlement
#   npm run macos:passkeys -- <args>    extra arguments go to `tauri build`
#
# Needs, on a Mac:
# - Apple's grant for com.laffan.snorkle (the Account Holder of an
#   organization developer account asks with the "macOS Browsers Passkeys"
#   form), enabled for the App ID;
# - a Developer ID provisioning profile for the App ID that carries it, at
#   src-tauri/macos/embedded.provisionprofile (not committed), or the path
#   in SNORKLE_PROVISION_PROFILE;
# - APPLE_SIGNING_IDENTITY: the "Developer ID Application: …" identity.
#
# The entitlement is restricted: an app signed with it whose profile does
# not carry it is killed at launch. So the ordinary build
# (`npm run tauri build`, src-tauri/macos/Entitlements.plist) never has it,
# and this script checks the profile before building. It writes the signing
# entitlements (the ordinary ones, plus the app and team identifiers from
# the profile and the passkey key) and a config overlay under
# src-tauri/gen/passkeys/, and embeds the profile in the bundle
# (Contents/embedded.provisionprofile).
# SECURITY-REVIEW: what this writes is what the app is signed with.
set -eu
cd "$(dirname "$0")/.."

key=com.apple.developer.web-browser.public-key-credential
bundle_id=com.laffan.snorkle
profile="${SNORKLE_PROVISION_PROFILE:-src-tauri/macos/embedded.provisionprofile}"
pb=/usr/libexec/PlistBuddy

[ "$(uname)" = Darwin ] || { echo "Run this on a Mac." >&2; exit 1; }
[ -f "$profile" ] || { echo "No provisioning profile at $profile (README → Passkeys)." >&2; exit 1; }
[ -n "${APPLE_SIGNING_IDENTITY:-}" ] || { echo "Set APPLE_SIGNING_IDENTITY to your Developer ID Application identity." >&2; exit 1; }

decoded="$(mktemp)"
trap 'rm -f "$decoded"' EXIT
security cms -D -i "$profile" > "$decoded"
read_key() { "$pb" -c "Print :$1" "$decoded" 2>/dev/null || true; }

if [ "$(read_key "Entitlements:$key")" != "true" ]; then
  echo "The profile does not carry $key: Apple has not granted it for $bundle_id, or it is not enabled for the App ID." >&2
  exit 1
fi
team="$(read_key Entitlements:com.apple.developer.team-identifier)"
app_id="$(read_key Entitlements:com.apple.application-identifier)"
if [ -z "$team" ] || [ "$app_id" != "$team.$bundle_id" ]; then
  echo "The profile is for '$app_id', not '$team.$bundle_id'." >&2
  exit 1
fi

gen=src-tauri/gen/passkeys
mkdir -p "$gen"
cp "$profile" "$gen/embedded.provisionprofile"
cp src-tauri/macos/Entitlements.plist "$gen/Entitlements.plist"
"$pb" -c "Add :com.apple.application-identifier string $app_id" \
      -c "Add :com.apple.developer.team-identifier string $team" \
      -c "Add :$key bool true" "$gen/Entitlements.plist"
# Paths in the overlay are relative to src-tauri, as in tauri.conf.json.
cat > "$gen/tauri.conf.json" <<'JSON'
{
  "bundle": {
    "macOS": {
      "entitlements": "gen/passkeys/Entitlements.plist",
      "files": { "embedded.provisionprofile": "gen/passkeys/embedded.provisionprofile" }
    }
  }
}
JSON

npx tauri build --config "$gen/tauri.conf.json" "$@"

app="$(find src-tauri/target/release/bundle/macos target/release/bundle/macos -maxdepth 1 -name '*.app' 2>/dev/null | head -n 1)"
if [ -n "$app" ]; then
  if codesign -d --entitlements - --xml "$app" 2>/dev/null | grep -q "$key"; then
    echo "Signed with $key: $app"
  else
    echo "Warning: $app is not signed with $key." >&2
  fi
fi
