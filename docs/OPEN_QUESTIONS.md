# Open questions and unverified assumptions

Each item is marked:
- **[verify]**: an Apple or Tauri behaviour I could not confirm from official docs or source. Check it on a device or with the SDK.
- **[decide]**: a product or security decision that is yours.
- **[gap]**: known missing functionality.

What I could check: Tauri 2.12.5, tauri-runtime-wry 2.12.3, wry 0.57.1 and objc2-web-kit 0.3.2
**source code** (vendored by cargo); Apple's "Preparing your app to be the default web browser"
page (via its JSON endpoint); the Tauri v2 capability reference. What I could not check:
webkit.org was unreachable from this environment (network policy), Apple's JS-rendered
API pages, and anything that needs macOS, Xcode or an iPad. **Nothing has been built for, or run on,
iOS.** The Rust compiles and clippy-checks for `aarch64-apple-ios` against the objc2 bindings
(`scripts/check-ios.sh`), with a stub C compiler and no SDK.

## Build and platform

1. **[verify] Hand-written `WKWebView` binding.** `crates/snorkle-content/src/ios/bindings.rs`
   declares selectors and return types by hand, because objc2-web-kit 0.3.2 binds `WKWebView`
   from the macOS SDK only. A wrong signature is undefined behaviour. Check every entry against
   `WKWebView.h` in the iOS 26 SDK. The same applies to delegate method signatures in
   `delegates.rs`, which are registered by raw selector.
2. **[verify] OS patch table.** As of 2026-10-29 I found iPadOS 27.0 (released 2026-09-14) and
   26.6.1 (2026-08-17) as the newest security releases. I could not find October 2026 releases.
   `os::LATEST_KNOWN_PATCH` must be updated for every Apple security release; consider
   automating it as a release checklist item.
3. **[verify] Memory budget and maximum live tabs.** 2/3/4/6/8 live views by RAM tier and a
   10-minute background TTL are starting targets, not measurements. See `PERFORMANCE.md` for the
   hardware test plan.
4. **[verify] ProMotion frame rate.** Not measured. Whether `requestAnimationFrame` in a
   third-party WKWebView runs above 60 Hz on ProMotion iPads is unknown. `tests/device/raf-probe.html`
   is the harness.
5. **[verify] Tauri iOS integration points.** Assumed from source reading, not observed:
   (a) `setup` runs on the main thread after the `main` window exists;
   (b) `UiKitWindowHandle.ui_view` is the root view wry adds the chrome WKWebView to, so our
   containers added later sit above it in z-order;
   (c) sync Tauri commands run on the main thread on iOS;
   (d) `emit_to("main", …)` reaches `event.listen` in the chrome.
6. **[verify] WebKit commit semantics.** The address-bar guarantee relies on these:
   `didCommitNavigation` is main-frame only and `WKWebView.URL` read there is the committed
   document URL; KVO URL changes outside a provisional navigation are same-document; and
   `decidePolicyForNavigationAction` is (or is not) called for `javascript:` URLs. The policy
   allows page-initiated `javascript:` either way.
7. **[verify] `WKNavigationType` and user gestures.** Script-simulated clicks may report
   `.linkActivated`. This only affects whether an external-app request is prompted or silently
   blocked; a prompt is always required.
8. **[verify] Entitlements wiring.** Tauri 2.12.5 has no config key for iOS entitlements. After
   `cargo tauri ios init`, the keys in `src-tauri/ios/Entitlements.plist` must be copied into
   `gen/apple/snorkle_iOS/snorkle_iOS.entitlements` (or provided via a custom XcodeGen template,
   `bundle.iOS.template`). Also verify that `NSFileProtectionComplete` covers the Application
   Support directory Tauri resolves for `app_data_dir`.
9. **[verify] Private-mode wipe.** One non-persistent `WKWebsiteDataStore` per private session
   (shared by private tabs) is wiped with `removeDataOfTypes:` and released when the last private
   tab closes. I could not verify that WebKit retains nothing else, such as HSTS state, the memory
   cache in the network process, or the back/forward cache. **[decide]** Safari-style one store per
   private *tab* would be stricter but breaks logins across private tabs.
10. **[verify] Upgrade-related WebKit APIs.** `upgradeKnownHostsToHTTPS`,
    `inactiveSchedulingPolicy`, `elementFullscreenEnabled` and `setInspectable` are guarded with
    `respondsToSelector:`. Their exact iOS availability was not confirmed.
11. **[verify] ATS in web content.** I set `NSAllowsArbitraryLoadsInWebContent` so HTTP can load
    after explicit consent, on the assumption that ATS would otherwise block it inside WKWebView.
    Not confirmed for iOS 26/27, and App Review may ask for a justification.

## Security design

12. **[decide] IDN policy.** It is UTS #39 "highly restrictive" plus hand-curated
    Cyrillic/Greek/Armenian lookalike sets and a forbidden-character list. It has not been tested
    against Chromium's IDN spoof corpus, and it does not catch pure-ASCII lookalikes (`rn`/`m`,
    `paypa1`). Options: port Chromium's skeleton-vs-top-domains check, or keep the stricter
    punycode fallback.
13. **[verify] PSL freshness.** The Public Suffix List is compiled in (`psl =2.1.245`). New
    private suffixes added after that release are misread as registrable domains until a
    dependency bump. **[decide]** Choose an update cadence.
14. **[verify] Popups with `window.opener`** (decided 2026-10-30, to fix Google sign-in on
    Pinterest): popups are real web views built from WebKit's configuration and appear as a new
    tab; `window.close()` returns to the opener, and the opener is protected from discard while
    its popup is in front. To check on device: the Google → Pinterest flow completes; WebKit
    applies `noopener` to `target=_blank` links; popups get the opener's rule lists and user
    scripts (they share its content controller).
    2026-10-30: sized popups are shown as a sheet over the still-visible opener, because a hidden
    opener is suspended (`inactiveSchedulingPolicy`) and cannot receive the sign-in result. The
    bundled filter lists were checked against Google sign-in URLs (script, button iframe, popup,
    sign-in page) on pinterest.com: no blocking or hiding rule matches. Still unverified: whether
    WebKit calls `createWebView` at all for Pinterest's button (its popup blocker drops
    `window.open` calls it does not count as user-initiated, without telling the app), and
    whether `window.opener` survives (a 2024 Apple forum report says it is null in WKWebView
    popups from iOS 17.5 on). Use the in-app console to see which step fails.
15. **[decide][gap] JavaScript dialogs** (`alert`/`confirm`/`prompt`) are suppressed, and the chrome
    shows a note naming the origin. A trusted dialog UI (native `UIAlertController` titled with the
    origin, rate-limited, only for the active tab) is the obvious next step. It is native UI code
    and needs a review.
16. **[decide] Safe Browsing.** `fraudulentWebsiteWarningEnabled = true` (WebKit's default). Apple
    proxies the lookups to a Safe Browsing provider. You asked for no telemetry by default. Is this
    acceptable as a security feature, or should it be off or opt-in?
17. **[decide] Camera/microphone usage strings** are in `Info.ios.plist` so `getUserMedia` can
    prompt. Remove them if you prefer no capture support.
18. **[decide] Element fullscreen and link previews are disabled** to prevent fullscreen phishing
    and unpoliced preview loads. This costs some UX (e.g. custom video players' fullscreen
    buttons).
19. **[verify] User agent.** Resolved 2026-10-29 at your request after Gmail reported "This
    browser is not supported": content views now send Safari's iPad desktop UA
    (`CONTENT_VIEW_SPEC.safari_desktop_user_agent`, version taken from the running OS) and request
    desktop content mode. Sites cannot tell Snorkle from Safari. To verify on device: Gmail and
    Google sign-in work; `navigator.userAgent` in the page inspector matches Safari's; and in narrow
    Split View, Safari would switch to mobile layout but Snorkle stays desktop (known difference).
20. **[verify] Tauri's local-origin rule.** Tauri treats `https://tauri.localhost` as local. This
    does not affect content views (they are not Tauri webviews), and policy blocks those hosts
    anyway. It matters only if a remote page were ever loaded into the chrome webview, which no
    code path does.
21. **[gap] Downloads** are cancelled. Supporting them adds a file-system channel (`WKDownload`)
    and needs a review. The tripwire test flags `WKDownload`.

## Product and functionality gaps

22. **[decide] Default search engine** is DuckDuckGo; no search suggestions, since suggestions
    would send keystrokes off-device.
23. **[verify] Content blocking** (decided 2026-10-29: Brave's adblock-rust converter, EasyList +
    EasyPrivacy bundled, per-site allow toggle, no scriptlets). Still to check on a device:
    (a) how long WebKit takes to compile the 127k rules / 3 lists on first launch, and the
    memory it uses (after that the compiled lists are reused via the cache key);
    (b) that `WKContentRuleListStore` completion handlers run on the main thread (asserted;
    a violation aborts);
    (c) that `if-top-url` with `ignore-previous-rules` disables blocking for subdomains of an
    allowed site, as the regex intends;
    (d) page-load cost of about 2,000 generic `css-display-none` rules;
    (e) how many sites break. The converter drops rules WebKit can't express (scriptlets, some
    `$` options), so blocking is weaker than uBlock Origin. YouTube-style video ads are not
    blocked.
    **[decide]** Lists refresh only with app updates (`scripts/update-filter-lists.sh`). They
    expire in about 4 days upstream, so ship regularly or add an opt-in updater (an app-initiated
    network request, which needs a review).
24. **[decide] Bundle identifier** `com.example.snorkle` and the development team are placeholders.
25. **[gap] Multiple windows / Stage Manager scenes.** Only one scene is supported; Tauri's
    `SceneRequested` is not handled.
26. **[gap] Background media.** A tab playing audio can be discarded by the budget. WKWebView's
    `mediaPlaybackState` (iOS 15+) could exempt it.
27. **[gap] Storage.** JSON files are capped at 5,000 history entries. Move to SQLite if history
    search becomes a feature. `interactionState` is kept in memory only, so discarded tabs lose
    back/forward history across app restarts.
28. **[gap]** Accessibility audit, hardware-keyboard shortcuts, find-in-page, reader mode, printing.
29. **[gap] Desktop dev build** renders the chrome only; no web content off iOS (non-goal).
30. **[verify] IPC test scope.** `ipc_isolation.rs` exercises Tauri's ACL on MockRuntime, not the
    iOS IPC transport. The on-device probe (`tests/device/ipc-probe.html`) covers the content views.
    Run it on hardware before each release.
31. **[decide] Per-site ad-blocking exceptions in private tabs** are disabled, because the
    setting is persisted. A memory-only exception would need separate rule lists for the private
    profile.
32. **[verify] Sidebar on device.** Docked at ≥ 700 pt window width; below that it slides over
    the page (pages are hidden while it is open). The 700 pt threshold, safe-area handling in
    Stage Manager, and the moment the native page view moves (instantly, while the sidebar
    animates) need checking on hardware.
33. **[decide] User scripts run in an isolated world.** Pages can't see or interfere with them,
    but scripts can't read page JavaScript variables either (only the DOM). Many Greasemonkey
    scripts expect page access (`unsafeWindow`). Running in the page world would be more
    compatible and more exposed; that change needs a review.
34. **[decide] User scripts also run in private tabs.** They persist nothing about browsing, but
    a script could itself send data somewhere. Currently they apply in private tabs.
35. **[verify] User scripts on device.** Check: a `<style>` added at document start in the
    client world applies before first paint; `removeAllUserScripts` + re-adding takes effect on
    the next load; scripts do not run in iframes.
36. **[verify] Black-and-white mode.** `html{filter:grayscale(1)}` should not break fixed
    positioning (the CSS spec exempts the root element) and should cost little GPU on long
    pages. Video and canvas get the filter too. Pages can remove the style element (cosmetic
    only). It is now on by default (2026-10-30) and colour is a session-only toggle: check that
    the first page after launch never paints in colour.
    **Hidden images** (2026-10-30) use a second constant style at document start. To check: no
    image flashes before the style applies; iframes (main-frame-only scripts) still show their
    images; `<img>` files are still downloaded (only CSS background images are skipped).
    **[decide]** Block image loads too, with a WebKit content rule (`resource-type: image`,
    saves data, needs a reload to toggle), and whether either mode should be remembered across
    launches.
37. **[verify] Split view.** Tap-to-focus uses a gesture recognizer that fires simultaneously with
    WebKit's own and does not cancel touches. Confirm links and scrolling are unaffected. The
    50/50 split has no draggable divider yet. The memory budget counts both panes as protected;
    split is refused when the budget is below 2 live views. Several pairs can exist (2026-10-30);
    only the one on screen is protected. **[gap]** Pairs are not saved in the session, and
    private and normal tabs cannot be paired (the sidebar lists them separately).
38. **[verify] Sidebar resizing** reports the new content inset once per animation frame, so the
    page re-lays out continuously while dragging. Check smoothness on heavy pages; if it is
    choppy, move the content view only on release and show a placeholder edge while dragging.
39. **[verify] Drag and drop on touch.** Uses long-press (280 ms) then pointer events, with
    `touchmove.preventDefault()` once dragging. Verify there is no conflict with iPadOS
    long-press menus or text selection in the chrome webview. Private tabs cannot be dropped
    into bookmarks. Tabs themselves cannot be reordered yet. Dragging a tab out of a split pair
    onto the page area relies on the chrome webview still receiving the pointer while it is
    over a native content view (touch sequences stay with the view they began in); verify.
    Tab close buttons show on hover (`any-hover: hover`) and on the active tab otherwise; check
    a first tap on a tab still opens it on iPadOS with a trackpad attached.
40. **[decide] Website data** (Settings, 2026-10-30): separate Delete buttons for cache, cookies,
    site data, and "logins" (cookies + site data, i.e. signed out everywhere), for the default and
    current private stores. Passwords are not the app's to delete (iOS Passwords app); HTTP
    authentication credentials WebKit may keep are not cleared. Open pages keep what they have in
    memory until reloaded.
41. **[verify] Keyboard shortcuts.** ⌘T/⌘S/⌘1–5 use `UIKeyCommand`s on content containers with
    `wantsPriorityOverSystemBehavior`, so they take precedence over page shortcuts (e.g. ⌘S in web
    editors). Check that UIKit offers them while a page has focus, and that the chrome's own
    handler fires when the address bar or sidebar has focus.
    ⌘W (2026-11-01) closes the current tab: a content key command while a page has focus, and
    otherwise the main menu's Close command, which the app replaces with "Close Tab ⌘W" by adding
    `buildMenuWithBuilder:` / `snorkleCloseTab:` to Tauri's app-delegate class at runtime
    (`ios/menu.rs`). Check on device that the class did not already define them (else the menu
    keeps "Close Window") and that the menu bar shows "Close Tab".
42. **[verify] Favicon snapshots.** Check: an off-screen web view inside the window renders, so
    `takeSnapshot` returns pixels; the "blank icon" heuristic (PNG < 220 bytes) suits real sites;
    sites without `/favicon.ico` (icon only in `<link rel=icon>`) fall back to letter tiles.
    Fetching `<link rel=icon>` would need JS or HTML parsing in the hidden view, so it is not done.
    2026-10-30: icons showed as broken images on device. The cause was not identified (no device
    here); the snapshot is now redrawn into a standard-range 8-bit sRGB bitmap before PNG encoding,
    storage accepts only 8-bit RGB/RGBA PNGs up to 512 px, the old icon cache is deleted, and the
    chrome falls back to the letter tile if an image still fails. The console logs each fetch
    ("favicon <host>: 128×128, depth 8, …") and any display failure.
43. **[decide] ⌘1–5 order** is pins, bookmarks, then tabs. Since 2026-10-30 each pin and bookmark
    owns one tab (not listed with the other tabs) that opening it returns to, wherever that tab
    has navigated since. **[gap]** There is no way to close a pin's tab without unpinning.
44. **[verify] Console capture on device.** Check: the page-world hook is installed before page
    scripts run (document start) in iframes too, e.g. Google's sign-in button frame; removing
    the handler and scripts when Record is turned off takes effect on the next load;
    `WKScriptMessage.webView` matches the mounted web view (used to attribute messages to tabs;
    popups share their opener's content controller); the clipboard copy works in the chrome
    webview. **[decide]** Release builds record nothing until the user turns Record on; pages can
    detect the hook while it is on.
45. **[verify] View modes per tab and headers view.** Check: a popup shares its opener's content
    controller, so installing per-tab scripts for one rewrites the other's (exemptions are copied
    to popups to keep them consistent); the client-world handler is not visible to page scripts
    (`window.webkit.messageHandlers.snorkleHeadings` undefined in the page world); the extractor
    keeps up on single-page apps; heading indices can shift between report and click on pages
    that change quickly.
46. **[verify] AI connections on device.** Check: Keychain add/read/delete work under the app's
    entitlements and `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`; `NSURLSession`'s completion
    runs off the main thread as assumed; the OpenAI model filter (`snorkle_core::ai::is_text_model`)
    still matches the account's chat models; the Responses API reply shape (`output[].content[]`
    `output_text`). Anthropic requests send `anthropic-version: 2026-10-21` and, for Claude Fable
    5.1 / Opus 5.5 / Opus 5 / Sonnet 5.5, the server-side refusal fallback (`fallbacks: "default"`,
    beta `server-side-fallback-2026-07-01`). **[decide]** Keys are device-only (no iCloud
    Keychain sync); the domain, script and prompt are sent to the provider under the user's own
    account terms.
