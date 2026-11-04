# Snorkle

An iPad-first web browser that also runs on macOS: Tauri v2 hosts the trusted browser chrome,
untrusted pages run in system `WKWebView`s that are created and driven from Rust, and a pure-Rust
core makes every decision (tabs, navigation policy, HTTPS-first, address-bar display, storage,
content blocking). Security first, speed second.

**Status:** an initial shell with tabs, address bar, navigation, suspension/discard and private
mode, plus a security test suite and fuzzers. It has **not yet been built for, or run on, an iPad**:
this repository was developed on Linux without Xcode. The iOS code type-checks for
`aarch64-apple-ios` but has never been linked or executed. The macOS host (added 2026-11-03)
type-checks for `aarch64-apple-darwin` and has not been built or run on a Mac either. Read
`docs/OPEN_QUESTIONS.md` first (macOS: items 76–90).

## Docs

- `docs/ARCHITECTURE.md`: components, trust boundaries, threat model.
- `docs/SECURITY_REVIEW.md`: review-required code, page-to-native channel register, sign-off log.
- `docs/OPEN_QUESTIONS.md`: unverified assumptions and decisions that are yours.
- `docs/PERFORMANCE.md`: memory/tab budgets and the hardware measurement plan (no results yet).
- `docs/DEPENDENCIES.md`: pinned dependencies and audit results.

## Layout

```
crates/snorkle-core     platform-independent core (#![forbid(unsafe_code)])
crates/snorkle-content  content WKWebView hosts (objc2): src/ios (UIKit), src/macos (AppKit),
                        src/webkit (WebKit code both share), stub elsewhere
src-tauri               Tauri app: chrome window, commands, capabilities, plist/entitlements
ui                      chrome UI (HTML/CSS/JS, no build step, no packages)
fuzz                    cargo-fuzz targets (separate workspace, nightly)
tests/device            on-device probes (IPC isolation, rAF/ProMotion)
scripts                 check-ios.sh / check-macos.sh (type-checks without Xcode), check-deps.sh
```

## Test (any OS)

```sh
cargo test --workspace          # unit, URL-spoofing, browser invariants, IPC isolation, config, tripwires
scripts/check-ios.sh            # clippy -D warnings for aarch64-apple-ios (no SDK; stub C compiler)
scripts/check-macos.sh          # the same for aarch64-apple-darwin
scripts/check-deps.sh           # cargo deny + cargo audit + graph checks
scripts/update-filter-lists.sh  # refresh bundled EasyList/EasyPrivacy, record versions + hashes
cd fuzz && cargo +nightly fuzz run url_display     # also: input, nav_policy, browser_events
```

On Linux, the Tauri test build needs the WebKitGTK development packages
(`libwebkit2gtk-4.1-dev libgtk-3-dev`).

## Run on macOS

`npm ci` installs the pinned Tauri CLI (dev tool only; the chrome UI has no JS packages). Needs
Xcode's command-line tools and Rust 1.90+; Safari 26 or later (Safari 27 runs on macOS 15, 26 and
27). The app's minimum macOS is 15.0.

- `npm run tauri dev`: **the browser on macOS**, with the same chrome, pages and features as on the
  iPad. The window has no title bar: the close, minimise and zoom buttons sit in a strip at the top
  of the docked sidebar (hidden while the sidebar is hidden or flies out), and that strip drags the
  window (double-click zooms). Inspect the chrome and the pages from Safari → Develop → *this Mac*
  (Safari → Settings → Advanced → "Show features for web developers"); the terminal prints
  `[snorkle] policy …` / `effect …` lines for every navigation decision.
- `npm run tauri build`: an app bundle in `target/release/bundle/macos/`. Set the bundle
  identifier (`tauri.conf.json`, a placeholder) and, to sign, `APPLE_SIGNING_IDENTITY`.
  Unsigned and ad-hoc-signed builds keep AI keys in the login keychain instead of the
  data-protection keychain (`webkit/keychain.rs`), and macOS may ask to allow access after each
  rebuild.
- Default browser: the bundle declares `http`/`https` (`src-tauri/Info.plist`); choose Snorkle
  under System Settings → Desktop & Dock → Default web browser. Links open in new tabs.
- What differs from the iPad: no pull to refresh; page scale reflows the page like Safari's zoom
  (`pageZoom`); no memory warnings (only the live-view budget and the idle timeout); keyboard
  shortcuts come from a key-event monitor instead of UIKit key commands. See `docs/ARCHITECTURE.md`
  §1a.

## Run on iPadOS

- **In-app console** (Menu → Console): the same browser log plus page console output and errors,
  on the device itself. Recording is on by default in debug builds; see
  `docs/ARCHITECTURE.md` §5d.
- `npm run tauri ios dev`: the real browser, in the iPad Simulator or on a device. First run:
  1. Xcode with the iPadOS 26/27 SDK; `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`.
  2. `npm run tauri icon src-tauri/icons/icon.png && npm run tauri ios init`
  3. `sh scripts/ios-entitlements.sh` copies the keys from `src-tauri/ios/Entitlements.plist`
     into `src-tauri/gen/apple/snorkle_iOS/snorkle_iOS.entitlements` (Tauri never does; `ios init`
     leaves it empty). Until Apple grants `com.apple.developer.web-browser`, add
     `--without-web-browser`, or device signing fails. `npm run ios:build` runs it for you.
  4. Set the bundle identifier and development team (`tauri.conf.json`, currently placeholders).
  5. Pick an iPad simulator when prompted. Inspect the chrome **and** the page views from
     Safari → Develop → Simulator (both are inspectable in debug builds).
  6. Physical iPad: `npm run ios:build` syncs the entitlements (step 3), builds a debug IPA,
     reports whether the signed app carries the default-browser entitlement, and installs it with
     `ios-deploy --bundle` (`brew install ios-deploy`). `npm run ios:build -- --release` for a
     release build, `-- --without-web-browser` to sign without that entitlement; any other
     arguments go to ios-deploy (e.g. `--justlaunch`, `--id <udid>`). Needs a development team set
     (`bundle.iOS.developmentTeam` or `APPLE_DEVELOPMENT_TEAM`).
  7. Run `tests/device/ipc-probe.html` from a remote origin and `tests/device/raf-probe.html`,
     and record the results in `docs/PERFORMANCE.md`.

### Default browser

The app already declares the `http`/`https` URL schemes (`Info.ios.plist`), opens an address
field at launch and handles links tapped in other apps (each opens in a new tab). iPadOS lists a
browser under **Settings → Apps → Default Apps → Browser App** only if the installed app is
*signed with* Apple's managed entitlement `com.apple.developer.web-browser`. Nothing in the code
can substitute for it. Three things must all hold:

1. **Apple granted it** for the app's real bundle identifier: the developer account holder
   submits Apple's "Default browser entitlement request" form
   (developer.apple.com/contact/request/default-browser-entitlement). The identifier in
   `tauri.conf.json` is still the placeholder `com.example.snorkle`, which cannot be granted.
2. **It is enabled for the App ID**: developer portal → Certificates, Identifiers & Profiles →
   Identifiers → the App ID → Additional Capabilities → Default Web Browser, so new provisioning
   profiles carry it.
3. **The build is signed with it**: `npm run ios:build` copies the key into the generated Xcode
   project (`scripts/ios-entitlements.sh`; before 2026-11-04 this was a manual copy that
   `tauri ios init` undoes), then prints whether the signed app and its profile contain it.

If signing fails with "provisioning profile doesn't include the
com.apple.developer.web-browser entitlement", 1 or 2 is missing; `npm run ios:build --
--without-web-browser` installs without it (no default-browser option, no Service Workers).

### Passkeys (both platforms)

Passkeys and security keys fail in Snorkle today (Google reports `passkey_first_auth_factor_error`).
This is Apple's rule, not a bug in the app: WebKit handles WebAuthn in `WKWebView` by itself, but
for a third-party browser it only accepts sites that list the app's own App ID, unless the app
holds a managed entitlement.

- **macOS:** `com.apple.developer.web-browser.public-key-credential` (macOS 13.3+). The developer
  account's *Account Holder* requests it with Apple's "macOS Browsers Passkeys" form; Apple reviews
  it against its browser criteria (http/https declared, an address field at launch, pages shown as
  they are), which Snorkle is built to meet. Once granted: enable it for the App ID, add the key to
  `src-tauri/macos/Entitlements.plist` (and to the expected keys in
  `src-tauri/tests/config_hardening.rs`), and build a signed bundle whose provisioning profile
  carries the capability (`npm run tauri build`; `tauri dev` runs an unsigned binary, where
  passkeys can never work). The person may then be asked once whether Snorkle may use their
  passkeys.
- **iPadOS:** Apple documents the same entitlement for macOS and Mac Catalyst only. On iPadOS the
  path is assumed to be the default-browser entitlement above (`com.apple.developer.web-browser`);
  unverified (OPEN_QUESTIONS 101).

Until then, use the site's other sign-in method (on Google: "Try another way").

Minimum OS: iPadOS 26.0 (deployment target and runtime gate). Older patch levels get a warning
banner; the table lives in `crates/snorkle-core/src/os.rs` and must be updated with each Apple
security release.

## Non-goals (for now)

Extensions, an embedded AI agent, sync, alternative engines, platforms other than iPadOS and
macOS. On Linux only the chrome runs (a desktop preview for UI work; pages never render).
