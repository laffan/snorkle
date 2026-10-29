#!/bin/sh
# Builds the iOS app for a physical device and installs it with ios-deploy.
#
#   npm run ios:build                 debug build (Web Inspector enabled), install
#   npm run ios:build -- --release    release build, install
#   npm run ios:build -- [--release] <extra ios-deploy args>   e.g. --justlaunch, --id <udid>
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
for a in "$@"; do
  case "$a" in
    --release) profile="" ;;
    *) deploy_args="$deploy_args $a" ;;
  esac
done

# Output location per tauri-cli 2.12.4 (src/mobile/ios/build.rs):
# <gen/apple>/build/<arch>/<productName>.ipa
out=src-tauri/gen/apple/build/arm64
rm -f "$out"/*.ipa

npx --no-install tauri ios build $profile --target aarch64 --export-method debugging

ipa=$(ls -t "$out"/*.ipa 2>/dev/null | head -n 1)
[ -n "$ipa" ] || { echo "no .ipa found in $out" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unzip -q "$ipa" -d "$tmp"
app=$(ls -d "$tmp"/Payload/*.app | head -n 1)
[ -n "$app" ] || { echo "no .app inside $ipa" >&2; exit 1; }

echo "Installing $(basename "$app") from $ipa"
# shellcheck disable=SC2086
ios-deploy --bundle "$app" $deploy_args
