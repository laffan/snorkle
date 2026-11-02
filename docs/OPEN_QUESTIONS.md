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
    saves data, needs a reload to toggle). (2026-11-02: what each launch starts with is now a
    setting, Settings → General; see 64.)
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
47. **[verify] Detect Headers.** The sample is collected in the client world (computed styles of
    up to 8,000 elements, once per press); check the cost on very large pages. Rules are keyed by
    registrable domain and apply to its subdomains; a site whose layouts differ by section may
    need re-detecting. **[decide]** Pressing the button sends up to 200 short text snippets from
    the page to the AI provider.
48. **[verify] Floating URL pill** (2026-11-01). Pages now run the full window height and the pill,
    its ⋯ popup and toasts are cut out of them (layer mask + `pointInside:withEvent:` on the
    containers, `ios/keys.rs`). Check on device: the pill, popup and toasts are tappable and the
    page receives taps right next to them; scrolling a page under the pill feels normal (WebKit
    does not know about the hole, so the page's last lines sit under the pill unless scrolled);
    the mask follows the sidebar while it is dragged; text selection handles and the keyboard
    accessory near the bottom edge. With a pointer, the pill is translucent until hovered, so the
    chrome's background (not the page) shows through it. **[decide]** Whether pages should get a
    bottom inset (`scrollView.contentInset`) so their end can scroll clear of the pill.
49. **[verify] ⌘-click** relies on `WKNavigationAction.modifierFlags` (iOS 18.4+, `UIKeyModifierFlags`),
    read with `msg_send` after `respondsToSelector:`. Check that WebKit sets it for trackpad and
    mouse clicks with ⌘ held, that links with `target=_blank` arrive with no target frame as
    assumed, and whether a hidden background view loads before it is shown
    (`inactiveSchedulingPolicy = .suspend` may defer it until the tab is opened). New background
    tabs are inserted right after the current tab, so several open in reverse order. ⌘-click on
    pins, bookmarks and history entries in the chrome is not handled.
50. **[verify] Content not reflowing after a panel closes.** Reported on device: with the headers
    view turned off the page kept its narrower width. The chrome reports the new inset correctly
    (checked in a desktop browser with a mocked bridge), so the cause is assumed to be WebKit
    keeping the zoom scale it used while the area was narrower (desktop content mode). The host
    now sets the web view's frame explicitly and, 0.35 s after the area widens, zooms any page
    whose content is narrower than the view to fill it. Unverified on device; if it persists,
    log `scrollView.zoomScale`, `minimumZoomScale` and `contentSize` before and after.
51. **[decide] Page chat** (2026-11-01) sends the page's visible text (≤ 48,000 chars), title, URL and
    selection to the chosen provider with every message, also in private tabs and on any site
    (including signed-in pages such as mail). The panel says so; nothing is sent until the user
    sends a message. Pages can attempt prompt injection through their text; the model is told the
    page is data, and replies are only displayed, but a reply could still be misleading.
    Anthropic prompt caching is not used, so long pages cost their full input price each turn,
    and the estimate equals list price × reported tokens.
52. **[verify] Hide media on hover** (2026-11-01). The fade animates `filter` between
    `grayscale(1) contrast(0) brightness(1.57)` and `grayscale(1) contrast(1) brightness(1)` with
    `transition: … !important`, which overrides the page's own transitions on media elements.
    The pointer tracker uses `pointermove` (not touch) and `elementsFromPoint`; check its cost on
    image-heavy pages and that iPad trackpad hover produces pointer events in WKWebView.
53. **[verify] Detect Headers sections.** The prompt asks for section labels at level 1 and
    stories at 2–3; the panel groups whenever there are at least two top-level entries with
    deeper ones under them, which also applies to ordinary pages with several h1s. Rules stored
    before this change keep their old levels until Detect Headers is pressed again.
54. **[verify] Shortcuts in the main menu** (2026-11-01). All shortcuts are now menu key commands
    (`ios/menu.rs`), added to tao's app-delegate class at runtime like ⌘W before, to fix ⌘T doing
    nothing with the sidebar hidden (assumed cause: no first responder, so neither the chrome's
    JavaScript nor the content containers saw the key). Check: the menu bar shows File → New Tab /
    Add Bookmark / Close Tab, View → sidebar items, Edit → Find on Page (the system Find menu is
    replaced) and a Tabs menu; ⌘⇧\ matches with input `\` plus Shift on US and other layouts;
    the chrome's keydown and the menu do not both act (de-duplicated within 60 ms); system
    shortcuts are not shadowed. **[decide]** "Shift+D" was implemented as ⌘⇧D, since a bare Shift+D
    would fire while typing a capital D.
    2026-11-01: crashed on launch on iPad. Probable cause (not confirmed with a crash log): two
    menu commands (⌘S and ⌘\) with the same action and property list, which UIKit rejects with an
    exception while building the menu. Property lists are now unique per entry (tested), each
    target menu is looked up before it is edited, and the build runs inside
    `objc2::exception::catch`: on an exception the menu bar keeps the system's menus, the shortcuts
    fall back to the content containers' key commands, and `[snorkle] menu: …` is logged.
55. **[verify] Find in page.** `findString:withConfiguration:` on iOS: whether it highlights the
    match visibly (it selects it), whether it moves focus out of the find field (the chrome
    refocuses it after each step), and that the count (visible text, case-insensitive, collapsed
    whitespace) agrees with WebKit's matches; "2 of 7" is tracked by counting steps, so it can
    drift when they disagree. Closing the bar clears the selection.
56. **[verify] Sidebar pop-over.** Closing it when the pointer leaves relies on the chrome's web
    view getting `pointerleave`/`mouseout` when the pointer moves from a hole onto a page view.
    If it does not, the pop-over stays until a tap outside or Escape.
57. **[verify] Round 4 (2026-11-01).**
    (a) `focus_chrome` (`becomeFirstResponder` on Tauri's WKWebView) followed by `input.focus()`
    gives the palette keyboard focus after ⌘T from a page; WebKit may still ignore programmatic
    focus without a user gesture.
    (b) The share sheet and dictionary are presented from the window's root view controller as
    popovers anchored to the root view; check they appear over the page views and dismiss cleanly.
    (c) Selection menu placement assumes the page's visual viewport width maps onto the pane width
    (`k = pane width / vw`); check at zoomed-in and desktop-scaled pages, in split view, and that it
    does not fight the system's own selection callout on touch.
    (d) ⌘I now toggles images and takes priority over pages (italic in web editors); ⌘D likewise.
    (e) Rebinding rebuilds the menu bar; check that UIKit picks up `setNeedsRebuild` promptly and
    that punctuation keys other than `\` match on non-US layouts.
    **[decide]** "Google This" always uses Google (the address bar's default is DuckDuckGo).
    Settings → AI now has Authentication and Uses (Site script helper, Ask Selection, Header View
    Parsing), each with its own service and model; earlier per-provider model choices are used
    as the defaults.
58. **[verify] Round 5 (2026-11-01).**
    (a) Animated sidebar: content frames animate with `UIView.animateWithDuration(0.25)` while the
    CSS slide runs; the layer mask is set for the final frame, so a narrowing page may clip early
    during the animation, and WKWebView may relayout once at the end rather than continuously. The
    URL pill moves without animation.
    (b) Dark mode: the detector runs at document end, on load and after 1.5 s, so a light page may
    flash light before it is inverted; pages that change theme later are not re-checked. CSS
    background images are inverted with the page (only `img`, `picture`, `video`, `canvas`,
    embeds and `iframe` are inverted back). `:has()` (Safari 15.4+) is used to combine with the
    other modes. Check that `overrideUserInterfaceStyle` on WKWebView switches
    `prefers-color-scheme` for already-loaded pages.
    (c) **[decide]** The chat archive keeps up to 60 chats with up to 48,000 characters of page
    text each, in the app container (data protection as for other files), until deleted; there is
    no "delete all" yet. Private-tab chats are never stored.
59. **[verify] Sidebar and shortcuts (2026-11-02).** Reported on device: shortcuts stopped working;
    the sidebar snapped open from the pop-over; pinning the pop-over replayed the opening. The
    snap was a CSS rule (half opacity with a pointer) replacing the sidebar's `transition`, so
    only opacity animated; one transition list now covers slide, reshape and opacity. Pinning
    replayed because moving from the pop-over onto its toggle tab (outside the sidebar) counted
    as leaving; the tab and sidebar are now one hover region, and the pop-over opens only after
    160 ms on the tab. The shortcut failure was not reproduced (the chrome's handling works in a
    desktop browser; the native path did not change); the in-app console now logs whether the
    menu-bar shortcuts were installed (`menu: …`) and each shortcut the native side receives
    (`shortcut …`), to find out on device. Also reported: with the chat open, hovering the
    toggle "flies out an entire new tab"; not reproduced, assumed to be the pop-over opening
    on a passing pointer.
60. **[verify] Sidebar rewrite (2026-11-02).** Attached and flyout modes as specified by the owner
    (⌘\ rules: attached hidden → show; attached shown → hide; flyout hidden or shown → attached,
    shown). The page cut-outs now glide with the slide; this relies on Core Animation
    interpolating the mask path, which only works while the holes keep their shape (count and
    corners). A toast or popup appearing mid-slide changes the set and the mask jumps to the end
    state. If the URL pill would overlap the flyout (narrow but not "narrow" windows), the two
    holes merge into one rectangle. Hover-to-show depends on the chrome receiving pointer events
    at the window edge (see 56).
61. **[verify] Dark mode load dim.** The page stays at 20% brightness from document start until the
    detector runs at document end; pages that take long to reach DOMContentLoaded stay dim that
    long. Media are dimmed until hovered; media under overlays (video players) are not undimmed by
    pointing unless hide media's pointer tracker is running.
62. **[verify] Shortcuts moved to the app delegate (2026-11-02).** Reported: ⌘\, ⌘⇧\ and ⌘T dead
    since the shortcut editor (round 4), and recording stuck at "Press keys…". Round 4 made the
    menu bar rebuild with the shortcuts at launch and added ⌘I, which the system's Format menu
    also uses; the exact failure was not observed (no device here). Shortcuts are now responder
    key commands of the app delegate, and recording uses native key commands. Check: every
    shortcut works with a page, the chrome, or nothing focused; recording finishes; the
    console shows `keys: shortcuts offered by the app delegate`. Whether responder key
    commands win over a menu-bar item with the same keys (⌘I and Italic in text fields) is
    unverified. ⌘S was removed (⌘\ remains).
63. **[verify] View-mode exemptions are saved with the session** (`SessionTab.exempt`), restored
    before the restored views are created; not for private tabs.
64. **[verify] Round 7 (2026-11-02).**
    (a) **Launch view modes.** Settings → General sets what black and white, hide media and dark
    start as at every launch (`view-modes.json`); changing one there also applies it now. The
    moon button stores dark too (owner request: dark persists); the palette and images buttons
    stay session-only. The Keyboard and ad-blocking explainer rows were removed; the EasyList /
    EasyPrivacy credit is kept as a line under General, since CC BY-SA 3.0 requires attribution.
    (b) **Pins ignoring single clicks** (reported: frequent, worse with nytimes.com and Google
    Drive, a double click works, later a single click works again). Cause found in the chrome:
    with a mouse or trackpad, a press started a drag after 120 ms even without movement, and
    every ended drag swallowed the click. A click held a little longer did nothing; reproduced in
    a desktop browser (a 400 ms press on a pin did not open it). The busy-page link is assumed,
    not observed: while the app's main thread is busy with a heavy page, the pointerup reaches
    the chrome late, so short presses also crossed 120 ms. Now mouse drags start only after
    the pointer moves 8 px, and a drag that never moved lets its click through (touch keeps the
    280 ms long-press). Tabs and bookmarks used the same code and get the same fix. Check on
    device with the trackpad while a heavy page loads.
    (c) **"Disable for this tab" for hide media lost after relaunch** (reported). Not reproduced:
    exemptions are saved with the session since round 6 (63), and the core test round-trips
    them. If the report predates that build, it is fixed there. Two weaknesses were fixed: the
    session write waited for 400 ms without any storage job, so a steady stream of navigations
    and history entries could postpone it indefinitely (it is now written at most 400 ms after
    the first change); and a pending write is flushed when the app enters the background. The
    in-app console logs at launch `session: N tabs restored; view modes …; off per tab: …`,
    which shows whether the exemption came back.
