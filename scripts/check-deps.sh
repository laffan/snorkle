#!/bin/sh
# Dependency hygiene: advisories, licences, sources, bans, and engine checks.
set -eu
cargo deny check
cargo audit
# No second copy of the objc2 stack or wry in the iOS or macOS graph (duplicate
# Objective-C class registrations would crash at runtime).
for t in aarch64-apple-ios aarch64-apple-darwin; do
  if cargo tree --target "$t" -e normal -p snorkle -d | grep -E '^(objc2|block2|wry) '; then
    echo "duplicate objc2/wry versions in the $t graph" >&2; exit 1
  fi
done
# No embedded JavaScript engine or alternative web engine on iOS or macOS.
for t in aarch64-apple-ios aarch64-apple-darwin; do
  for c in javascriptcore-rs boa_engine v8 rusty_v8 servo; do
    if [ -n "$(cargo tree --target "$t" -e normal -p snorkle -i "$c" 2>/dev/null)" ]; then
      echo "forbidden engine crate $c in the $t graph" >&2; exit 1
    fi
  done
done
# Chrome UI must have no JS packages.
test ! -e ui/package.json
echo "dependency checks passed"
