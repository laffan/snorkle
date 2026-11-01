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
scripts/update-filter-lists.sh  # refresh bundled EasyList/EasyPrivacy, record versions + hashes
cd fuzz && cargo +nightly fuzz run url_display     # also: input, nav_policy, browser_events
```

On Linux, the Tauri test build needs the WebKitGTK development packages
(`libwebkit2gtk-4.1-dev libgtk-3-dev`).

## Run (macOS)

`npm ci` installs the pinned Tauri CLI (dev tool only; the chrome UI has no JS packages).

- `npm run tauri dev`: **desktop preview**. Only the browser chrome runs; there is no content
  host on macOS, so pages never render (a banner says so). The Web Inspector opens
  automatically in debug builds, and the terminal prints `[snorkle] policy …` / `effect …` lines
  for every navigation decision.
- **In-app console** (Menu → Console): the same browser log plus page console output and errors,
  on the device itself. Recording is on by default in debug builds; see
  `docs/ARCHITECTURE.md` §5d.
- `npm run tauri ios dev`: the real browser, in the iPad Simulator or on a device. First run:
  1. Xcode with the iPadOS 26/27 SDK; `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`.
  2. `npm run tauri icon src-tauri/icons/icon.png && npm run tauri ios init`
  3. Copy the keys from `src-tauri/ios/Entitlements.plist` into
     `src-tauri/gen/apple/snorkle_iOS/snorkle_iOS.entitlements`. Leave out
     `com.apple.developer.web-browser` until Apple grants it, or device signing fails.
  4. Set the bundle identifier and development team (`tauri.conf.json`, currently placeholders).
  5. Pick an iPad simulator when prompted. Inspect the chrome **and** the page views from
     Safari → Develop → Simulator (both are inspectable in debug builds).
  6. Physical iPad: `npm run ios:build` builds a debug IPA and installs it with
     `ios-deploy --bundle` (`brew install ios-deploy`). `npm run ios:build -- --release` for a
     release build; any other arguments go to ios-deploy (e.g. `--justlaunch`, `--id <udid>`).
     Needs a development team set (`bundle.iOS.developmentTeam` or `APPLE_DEVELOPMENT_TEAM`).
  7. Run `tests/device/ipc-probe.html` from a remote origin and `tests/device/raf-probe.html`,
     and record the results in `docs/PERFORMANCE.md`.

### Default browser

The app already declares the `http`/`https` URL schemes (`Info.ios.plist`) and handles links
tapped in other apps (each opens in a new tab). To appear in **Settings → Apps → Default
Browser App**, the app also needs Apple's managed entitlement `com.apple.developer.web-browser`:

1. Request it with Apple's "Default browser entitlement request" form (developer account
   holder), for the app's real bundle identifier.
2. Once granted, enable it for the App ID in the developer portal, keep the key in
   `src-tauri/ios/Entitlements.plist`, and copy it into the generated
   `gen/apple/snorkle_iOS/snorkle_iOS.entitlements` (step 3 above).
3. Install the signed build, then choose Snorkle under Settings → Apps → Default Browser App.

Until then, builds must leave the key out, or signing fails.

Minimum OS: iPadOS 26.0 (deployment target and runtime gate). Older patch levels get a warning
banner; the table lives in `crates/snorkle-core/src/os.rs` and must be updated with each Apple
security release.

## Non-goals (for now)

Extensions, an embedded AI agent, sync, alternative engines, platforms other than iPadOS.
