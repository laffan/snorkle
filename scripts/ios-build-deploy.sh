#!/bin/sh
# Builds the iOS app for a physical device and installs it with ios-deploy.
#
#   npm run ios:build                 debug build (Web Inspector enabled), install
#   npm run ios:build -- --release    release build, install
#   npm run ios:build -- [--release] <extra ios-deploy args>   e.g. --justlaunch, --id <udid>
#   npm run ios:build -- --without-web-browser   sign without the default-browser
#                                     entitlement (until Apple has granted it)
#
# Before building, the keys of src-tauri/ios/Entitlements.plist are copied
# into the generated Xcode project (scripts/ios-entitlements.sh); after
# building, the script reports whether the signed app carries
# com.apple.developer.web-browser, which iPadOS requires before it lists
# Snorkle under Settings → Apps → Default Apps → Browser App.
#
# Requires: `npm run tauri ios init` done once, a development team set
# (tauri.conf.json bundle.iOS.developmentTeam or APPLE_DEVELOPMENT_TEAM),
# and ios-deploy (`brew install ios-deploy`).
set -eu
cd "$(dirname "$0")/.."

command -v ios-deploy >/dev/null 2>&1 || { echo "ios-deploy not found (brew install ios-deploy)" >&2; exit 1; }
[ -d src-tauri/gen/apple ] || { echo "run 'npm run tauri ios init' first" >&2; exit 1; }

profile="--debug"
deploy_args=""
ent_args=""
for a in "$@"; do
  case "$a" in
    --release) profile="" ;;
    --without-web-browser) ent_args="--without-web-browser" ;;
    *) deploy_args="$deploy_args $a" ;;
  esac
done

identifier=$(sed -n 's/.*"identifier": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -n 1)
case "$identifier" in
  com.example.*)
    echo "warning: bundle identifier is the placeholder '$identifier'. Apple grants the" >&2
    echo "         default-browser entitlement per bundle identifier; set yours in tauri.conf.json." >&2 ;;
esac

# shellcheck disable=SC2086
sh scripts/ios-entitlements.sh $ent_args

# Output location per tauri-cli 2.12.4 (src/mobile/ios/build.rs):
# <gen/apple>/build/<arch>/<productName>.ipa
out=src-tauri/gen/apple/build/arm64
rm -f "$out"/*.ipa

if ! npx --no-install tauri ios build $profile --target aarch64 --export-method debugging; then
  if [ -z "$ent_args" ]; then
    echo "" >&2
    echo "Build failed. If signing complained about com.apple.developer.web-browser, the" >&2
    echo "provisioning profile does not carry it: Apple has not granted the default-browser" >&2
    echo "entitlement for '$identifier', or it is not enabled for the App ID (developer portal →" >&2
    echo "Identifiers → the App ID → Additional Capabilities). To install without it:" >&2
    echo "  npm run ios:build -- --without-web-browser" >&2
  fi
  exit 1
fi

ipa=$(ls -t "$out"/*.ipa 2>/dev/null | head -n 1)
[ -n "$ipa" ] || { echo "no .ipa found in $out" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unzip -q "$ipa" -d "$tmp"
app=$(ls -d "$tmp"/Payload/*.app | head -n 1)
[ -n "$app" ] || { echo "no .app inside $ipa" >&2; exit 1; }

# Check the installed artifact, not just the source plist: an old generated
# project or an Xcode override must not silently lose the input declaration.
indirect=$(/usr/libexec/PlistBuddy -c 'Print :UIApplicationSupportsIndirectInputEvents' "$app/Info.plist" 2>/dev/null || true)
if [ "$indirect" != true ]; then
  echo "The built app does not enable native trackpad input (UIApplicationSupportsIndirectInputEvents)." >&2
  echo "Check that Xcode is using the Info.plist merged from src-tauri/Info.ios.plist, then rebuild." >&2
  exit 1
fi
echo "Trackpad input: native indirect input enabled in the built app."

# What the app is actually signed with, and what its profile allows.
ents=$(codesign -d --entitlements - --xml "$app" 2>/dev/null || codesign -d --entitlements :- "$app" 2>/dev/null || true)
prof=$(security cms -D -i "$app/embedded.mobileprovision" 2>/dev/null || true)
key='<key>com.apple.developer.web-browser</key>'
if printf '%s' "$ents" | grep -q "$key"; then
  echo "Default browser: the app is signed with com.apple.developer.web-browser. After installing,"
  echo "choose Snorkle under Settings → Apps → Default Apps → Browser App."
elif printf '%s' "$prof" | grep -q "$key"; then
  echo "warning: the provisioning profile allows com.apple.developer.web-browser but the app was" >&2
  echo "         signed without it; iPadOS will not list Snorkle as a browser. Check" >&2
  echo "         src-tauri/gen/apple/*_iOS/*_iOS.entitlements (scripts/ios-entitlements.sh)." >&2
else
  echo "Default browser: not available in this build (no com.apple.developer.web-browser in the" >&2
  echo "signed app or its provisioning profile), so iPadOS will not list Snorkle under" >&2
  echo "Settings → Apps → Default Apps → Browser App. See README.md → Default browser." >&2
fi

echo "Installing $(basename "$app") from $ipa"
# shellcheck disable=SC2086
ios-deploy --bundle "$app" $deploy_args
