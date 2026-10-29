# Snorkle

An iPad-first web browser: Tauri v2 hosts the trusted browser chrome, untrusted pages run in system
`WKWebView`s that are created and driven from Rust, and a pure-Rust core makes every decision
(tabs, navigation policy, HTTPS-first, address-bar display, storage, content blocking).
Security first, speed second.

**Status:** an initial shell with tabs, address bar, navigation, suspension/discard and private
mode, plus a security test suite and fuzzers. It has **not yet been built for, or run on, an iPad**:
this repository was developed on Linux without Xcode. The iOS code type-checks for
`aarch64-apple-ios` but has never been linked or executed. Read `docs/OPEN_QUESTIONS.md` first.

## Docs

- `docs/ARCHITECTURE.md`: components, trust boundaries, threat model.
- `docs/SECURITY_REVIEW.md`: review-required code, page-to-native channel register, sign-off log.
- `docs/OPEN_QUESTIONS.md`: unverified assumptions and decisions that are yours.
- `docs/PERFORMANCE.md`: memory/tab budgets and the hardware measurement plan (no results yet).
- `docs/DEPENDENCIES.md`: pinned dependencies and audit results.

## Layout

```
crates/snorkle-core     platform-independent core (#![forbid(unsafe_code)])
crates/snorkle-content  iOS content WKWebView host (objc2), stub elsewhere
src-tauri               Tauri app: chrome window, commands, capabilities, plist/entitlements
ui                      chrome UI (HTML/CSS/JS, no build step, no packages)
fuzz                    cargo-fuzz targets (separate workspace, nightly)
tests/device            on-device probes (IPC isolation, rAF/ProMotion)
scripts                 check-ios.sh (iOS type-check without Xcode), check-deps.sh
```

## Test (any OS)

```sh
cargo test --workspace          # unit, URL-spoofing, browser invariants, IPC isolation, config, tripwires
scripts/check-ios.sh            # clippy -D warnings for aarch64-apple-ios (no SDK; stub C compiler)
scripts/check-deps.sh           # cargo deny + cargo audit + graph checks
cd fuzz && cargo +nightly fuzz run url_display     # also: input, nav_policy, browser_events
```

On Linux, the Tauri test build needs the WebKitGTK development packages
(`libwebkit2gtk-4.1-dev libgtk-3-dev`).

## Build for iPad (macOS, not yet done)

1. Xcode for the iPadOS 26/27 SDK; `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`.
2. `cargo install tauri-cli --version =2.12.4 --locked`
3. `cd src-tauri && cargo tauri icon icons/icon.png && cargo tauri ios init`
4. Copy the keys from `src-tauri/ios/Entitlements.plist` into
   `gen/apple/snorkle_iOS/snorkle_iOS.entitlements`. Remove `com.apple.developer.web-browser` until
   Apple grants the managed entitlement, or signing fails.
5. Set the bundle identifier and development team (`tauri.conf.json`, currently placeholders).
6. `cargo tauri ios dev` (device), then run `tests/device/ipc-probe.html` from a remote origin and
   `tests/device/raf-probe.html`, and record the results in `docs/PERFORMANCE.md`.

Minimum OS: iPadOS 26.0 (deployment target and runtime gate). Older patch levels get a warning
banner; the table lives in `crates/snorkle-core/src/os.rs` and must be updated with each Apple
security release.

## Non-goals (for now)

Extensions, an embedded AI agent, sync, alternative engines, platforms other than iPadOS.
