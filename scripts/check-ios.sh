#!/bin/sh
# Type-checks (and clippy-lints) the full app for aarch64-apple-ios on a
# machine without Xcode, e.g. Linux CI. Nothing is linked: C/ObjC build
# steps (objc2-exception-helper) are replaced by a stub compiler that emits
# empty objects. This proves the Rust compiles against the objc2 bindings;
# it does NOT prove the bindings match the iOS SDK. A real build on macOS
# (`cargo tauri ios build`) is still required.
set -eu
stub="$(mktemp -d)"
trap 'rm -rf "$stub"' EXIT
mkdir -p "$stub/bin" "$stub/sdk"
cat > "$stub/cc" <<'CC'
#!/bin/sh
prev=""
for a in "$@"; do
  case "$prev" in -o) : > "$a";; esac
  case "$a" in -o?*) : > "${a#-o}";; -Fo?*) : > "${a#-Fo}";; esac
  prev="$a"
done
exit 0
CC
printf '#!/bin/sh\ncase "$*" in *show-sdk-version*) echo 26.0;; *) echo %s;; esac\n' "$stub/sdk" > "$stub/bin/xcrun"
chmod +x "$stub/cc" "$stub/bin/xcrun"
rustup target add aarch64-apple-ios >/dev/null
PATH="$stub/bin:$PATH" SDKROOT="$stub/sdk" CC_aarch64_apple_ios="$stub/cc" AR_aarch64_apple_ios=ar \
  cargo clippy --target aarch64-apple-ios -p snorkle -p snorkle-content -p snorkle-core -- -D warnings
