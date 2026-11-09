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
21. **Downloads** (supported since 2026-11-03, ARCHITECTURE §5h, channel D1; see 95–100).

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
55. **[verify] Find in page** (rewritten 2026-11-02: highlights instead of WebKit's selecting
    `findString:`). Assumed: `CSS.highlights` set from the isolated client world paints in the
    page (WebKit keeps one highlight registry per document; the JS map is per world); Highlight
    API needs Safari 17.2+, so iPadOS 26 has it. Without it the script still counts and scrolls,
    but nothing is highlighted. Limits: a match split across elements (`<b>`, links) is not
    found; text in closed shadow roots and iframes is not searched; matches are found again
    when the page replaced their text, otherwise kept, so text added after the search is missed
    until the query changes. The highlight colour is fixed (orange-yellow; light gray in black
    and white).
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
65. **[verify] Dark mode: videos shown inverted** (reported on the nytimes.com homepage,
    2026-11-02). Not reproduced (no device; nytimes.com is unreachable from this environment).
    Working assumption: WebKit does not reliably apply a filter on the `<video>` element itself to
    a playing video (its picture is a separate compositing layer, hosted from the GPU process),
    while the root's `invert` still reaches it. The counter-filter now goes on the video's frame
    (the nearest same-size ancestor, `data-snorkle-media-box`) and media inside it get none, which
    is right whichever of the two filters WebKit drops. Other possible causes this also covers:
    a video inside an open shadow root (the style cannot match it, but its host can be the
    frame). Not covered: a video with no same-size ancestor (letterboxed in a larger box; it
    keeps the filter on itself), videos in closed or nested shadow roots, CSS background images
    (inverted when hide media is off), WebGL canvases. Check on the device: NYT homepage loops
    playing, paused and hovered; a YouTube embed; a page with a letterboxed video. In Safari's
    Web Inspector, the frame element should carry `data-snorkle-media-box`. Cost: the detector
    rescans videos (and every element for open shadow roots) at most every 0.5 s while the page
    changes.
66. **[verify] Hide media and loading** (2026-11-02). Hidden `img` and `video` are forced to
    `opacity: 1`, so lazy-loading placeholders at opacity 0 show a gray box. Risk: an image kept
    at opacity 0 on purpose (crossfade carousels, hover-swap product images) now shows as a gray
    box over or under its neighbour while hidden; revealed media keep the page's opacity. An
    image pointed at before it has loaded pulses (registered custom property `--snorkle-pulse`
    animated under the `!important` filter; checked in Chromium, not in WebKit) and a
    `loading="lazy"` one is switched to `eager`. Lazy loaders that keep the real URL in
    `data-src` until the image scrolls into view are not triggered by pointing; containers kept
    at opacity 0 still hide their images.
67. **[verify] Page scale** (2026-11-02; reworked 2026-11-03). Reported: `pageZoom` scaled text
    rather than giving the page more room. Now the web view is laid out at pane size / scale and
    drawn with a scale transform (bounds, center and transform; autoresizing only at 100%).
    Check: at 80% a site that served a large layout to the narrow window gets its wider one;
    taps, text selection (the menu uses pane width / `innerWidth`, so it should line up), find,
    pull to refresh and the fit-width check after a resize all behave; text stays sharp enough
    (WebKit draws at the view's own scale, then the compositor scales it). Popups open at 100%.
    No keyboard shortcuts yet. Sites that size their content to the window cancel the effect by
    design: Google Docs at its "Fit" zoom makes the page as wide as the window, so a wider
    viewport just gets a larger page (reported 2026-11-03); a fixed Docs zoom (e.g. 100%) avoids
    it.
68. **[verify] Google Calendar and Drive freezing** (reported 2026-11-03, with ad blocking off).
    Not reproduced (no device). Most likely cause found: the in-app console's page hook (on by
    default in debug builds) ran `JSON.stringify` on every argument of every `console.*` call
    before truncating, and posted every call to native, so an app logging large objects or
    logging in a loop stalled its own main thread. The hook now uses a bounded formatter and
    stops after 100 messages per 10 s per frame. Check first: does the freeze still happen with
    Console → Record off? If it does, the hook was not the cause; next suspects are the dark
    mode detector (video scan; the full shadow-root search now runs only at load) and hide
    media's pointer tracker.
69. **[verify] ⌘T after returning from another app** (reported 2026-11-03). Assumed cause: iPadOS
    left no first responder, so key commands were not delivered until a tap. On
    `UIApplicationDidBecomeActiveNotification` the host finds the first responder (public API:
    an action sent to nil, recorded by a method added to `UIResponder`) and, if there is none,
    makes the chrome's web view first responder; the console logs which (`keys: app active
    again …`). The chrome also refocuses an open palette or find bar on `visibilitychange`, and
    asks for focus once more if the first request came too early. If ⌘T still fails, the log
    line says whether a responder existed.
70. **[verify] Offline copies** (2026-11-03). Assumed: `loadData:` with the
    `application/x-webarchive` MIME type and the page's URL as base URL shows the archive, its
    navigation action carries that base URL (the core allows it as its own load), and
    `webView:decidePolicyForNavigationAction:preferences:decisionHandler:` (now implemented; WebKit
    calls it instead of the variant without preferences) can turn JavaScript off for it. The
    archive is WebKit's serialisation of the live page, so script-built content should be in
    it; pages that need scripts to show anything stay blank. Not covered: bookmarks never opened
    since this change (a copy is made on visit), private tabs, and copies of pages behind a
    sign-in are only as fresh as the last visit.
71. **[verify] Image menu** (2026-11-03). Trackpad right-click and two-finger click should fire
    `contextmenu` in WKWebView; long press uses touch events (0.5 s, under 10 pt of movement), and
    `-webkit-touch-callout: none` on images removes iOS's own callout (so a linked image's link
    callout is gone too). Images behind a sign-in fail (no cookies), and plain-http images fail
    under ATS. "Save Image" in the share sheet needs `NSPhotoLibraryAddUsageDescription` (added).
72. **[verify] Appearance** (2026-11-03). The window's interface style also reaches content views
    that have no override, so pages see `prefers-color-scheme` follow the app's setting (dark
    pages mode still forces dark on its views).
73. **[verify] Edge tab near the pointer** (2026-11-03). A `UIHoverGestureRecognizer` on the
    window's root view (recognising alongside every other recognizer) reports the pointer within
    160 pt of the left edge, at most every 33 ms. Assumed: it receives hover over page views and
    the chrome alike and does not disturb WebKit's own hover (`:hover`, menus on pages). The tab
    is hidden only with `(any-hover: hover)`; with a trackpad attached but used by touch, the
    hidden sidebar is reached with ⌘\ or by pointing.
74. **[verify] Split top and bottom** (2026-11-03). ⌘D / ⌘⇧D split with an empty tab, or change
    an existing split's layout. Check the panes, the start surface over the empty half, the
    selection and image menus' placement (pane geometry in `paneBox`) and dragging out of a pair
    in rows, and dragging the divider (8 pt gap, one update per frame; each update re-places
    the visible pages natively). Bookmark Page moved to ⌥⌘D; a saved custom binding that used ⌘D or ⌘⇧D now clashes,
    and Settings → Keyboard then refuses the set until one is changed.
75. **[decide] Opening a pin or bookmark** (2026-11-03) now returns to its tab as left; Return to
    Original URL in its menu goes back to the saved address. Before, every click went back to it
    (and reloaded sites that redirect, e.g. nytimes.com → www.nytimes.com).

## macOS (2026-11-03)

The macOS host (`crates/snorkle-content/src/macos/`, ARCHITECTURE §1a) type-checks and
clippy-lints for `aarch64-apple-darwin` against the objc2 AppKit/WebKit bindings
(`scripts/check-macos.sh`, stub C compiler, no SDK). **It has not been linked or run on a Mac.**
The WebKit side is the iPadOS code, now shared (`webkit/`), so items 6–7, 9–10, 14, 23, 35, 44–45,
55 and 70 apply on macOS too.

76. **[verify] First launch.** Assumed from wry 0.57.1 and tao 0.37.2 source: the window's content
    view is wry's `WryWebViewParent` holding the chrome's `WKWebView`; `WebviewWindow::ns_window`
    returns that window during `setup`; a view added to the content view afterwards is above the
    chrome (hit-tested first, drawn on top). If pages do not appear, or clicks on the sidebar do
    nothing, check these first.
77. **[verify] Masks and hit-testing.** All views in the hierarchy are unflipped, on the
    assumption that a layer-backed unflipped view's layer then uses the view's coordinates (no
    `geometryFlipped`), so the mask path is built bottom-up. If cut-outs appear mirrored
    vertically (the pill's hole at the top of the page), that assumption is wrong. Check: the
    pill, its ⋯ popup, toasts, the find bar and the sidebar's edge tab are drawn and clickable over
    a page, the page is clickable right next to them, and the cut-outs glide with the sidebar.
78. **[verify] Hover and cursor in holes.** Mouse moves are delivered by tracking areas, which
    know nothing of the holes, so a page under the pill may still see `:hover` and set the cursor
    while the pointer is over the pill. Clicks and scrolling are hit-tested and are correct.
79. **[verify] Shortcuts.** The key monitor consumes bound ⌘ combinations before the menu bar, so
    ⌘W closes a tab instead of Tauri's default "Close Window", and ⌘F/⌘G/⌘D/⌘I/⌘T go to the
    browser while a page has focus. Keys are read with `charactersByApplyingModifiers:` (no
    modifiers), so ⌘⇧\ reads as `\` plus Shift; check on a non-US layout. Held keys do not repeat
    a shortcut. **[decide]** No ⌘R (reload), ⌘L (address bar) or ⌘[ / ⌘] (back, forward) yet: the
    shortcut list is shared with the iPad.
80. **[verify] Title bar.** `titleBarStyle: Overlay` with `trafficLightPosition` (16, 20) is meant
    to centre the buttons in the sidebar's 40 pt strip; the exact offset depends on the macOS
    version's title-bar height and may need adjusting. The buttons are hidden whenever the chrome
    reports no docked sidebar (hidden, flyout, narrow window, full screen keeps its own). Check that
    the strip drags the window and a double-click zooms it (Tauri's drag script, permissions
    `core:window:allow-start-dragging` and `allow-internal-toggle-maximize`). **[decide]** With the
    sidebar hidden there is nowhere to drag the window.
81. **[verify] Engine version.** The gate and the user agent use `CFBundleShortVersionString` of
    `/Applications/Safari.app` (falling back to the OS version). Check that it reads e.g. `27.2` on
    macOS 15 and 26 with Safari 27 installed, and that Google sign-in accepts the UA.
    `os::MACOS_SAFARI_LATEST` (27.2, found 2026-11-03) must be updated with each Safari security
    release, like `LATEST_KNOWN_PATCH` (item 2; Safari 27.2's existence suggests iPadOS 27.2 is
    out too, which that table does not list yet).
82. **[verify] Page scale** on macOS is `WKWebView.pageZoom`, assumed to reflow in a wider viewport
    (as Safari's ⌘−) rather than scale the bitmap. Pinch zoom on the trackpad is turned on
    (`allowsMagnification`, off by default on macOS), as the iPad has it.
83. **[verify] Keychain.** Unsigned and ad-hoc-signed builds get `errSecMissingEntitlement` (-34018)
    from the data-protection keychain and fall back to the login keychain, where any app the user
    approves can read the key, and rebuilt binaries may trigger an access prompt. Signed builds
    with an application identifier should use the data-protection keychain; check which one is
    used (Keychain Access shows login-keychain items).
84. **[gap] No memory pressure handling** on macOS (no memory-warning notification is observed;
    a `DispatchSource` memory-pressure source would be the equivalent). The live-view budget is by
    physical RAM as on the iPad.
85. **[gap] No pull to refresh** on macOS (no `UIRefreshControl`); back/forward swipes on the
    trackpad work (`allowsBackForwardNavigationGestures`).
86. **[verify] Session on quit.** `RunEvent::Exit` writes a pending session synchronously (up to
    2 s). Resigning active (switching apps) does what entering the background does on the iPad
    (idle views are discarded by the TTL tick, the session is written).
87. **[verify] Image menu and system UI.** Share uses `NSSharingServicePicker` anchored above the
    share button; Download asks for a location with a save-panel sheet; Copy writes an `NSImage` to
    the pasteboard; Define uses Look Up (`showDefinitionForAttributedString:atPoint:`) at the
    selection's bottom-left. Check placement in split view and with page scale.
88. **[verify] Favicons.** The hidden web view sits outside the root view's bounds (in the window)
    so WebKit renders it; its background is white (transparency needs a private API), so icons
    with transparency get a white backdrop. A white snapshot is assumed to encode below the
    220-byte "blank" threshold.
89. **[decide] Minimum macOS 15.0** (`bundle.macOS.minimumSystemVersion`); browsing additionally
    needs Safari 26 or later. The app is not sandboxed (the WebContent processes are, by WebKit).
    Data at rest relies on FileVault (no per-file data protection on macOS).
90. **[verify] Default browser.** Declaring the `http`/`https` URL schemes should list Snorkle under
    System Settings → Desktop & Dock → Default web browser; no document types are declared, so
    `.html` files do not open in Snorkle (file URLs are refused by the core anyway).


## Round 9 (2026-11-03, both platforms)

91. **[decide] Saved page text** is read from every page a non-private tab loads (1.5 s after the
    load finishes), including signed-in pages such as mail and banking, and kept on disk for the
    100 most recently visited pages until they leave history. It is the same data the page chat
    reads (C17), but without a user action each time. Options if this is too broad: skip pages
    with password fields, skip sites the user lists, or keep text in memory only.
92. **[verify] Saved text quality.** `innerText` 1.5 s after load: single-page apps that render
    later, infinite scrolls and pages behind overlays may save little or the wrong text. Each
    visit replaces the page's previous text. Text over 48,000 characters is cut.
93. **[verify] ⌘⇧T** opens the page in a new tab at the end of the list (not where the tab was),
    without its back/forward history; closing a tab and then removing its page from history makes
    ⌘⇧T skip it. The closed list is lost when the app quits.
94. **[verify] History selection.** Shift-click on a row extends from the last row clicked in
    Select mode; on iPad, Shift needs a hardware keyboard (each tap otherwise toggles one row). The
    × replaces the time on hover and is always shown on touch-only devices.


## Downloads (2026-11-03)

95. **[verify] Download triggers.** (2026-11-04: downloads from a frame inside the page now
    wait for the user's Allow / Block in a prompt above the URL bar; WebKit's decision handler is
    held, and unanswered requests are blocked after 60 s.) Assumed: WebKit asks `decidePolicyForNavigationResponse` for
    attachment and non-renderable main-frame responses, honours `.download`, then calls
    `webView:navigationResponse:didBecomeDownload:`; links with `download` arrive with
    `shouldPerformDownload` and `.linkActivated`. Blob and data downloads made by script
    (`a.click()` on a generated link) depend on WebKit reporting them that way. Downloads from
    subframes (some sites use a hidden iframe) ask the user (decided 2026-11-04).
96. **[verify] Progress.** `WKDownload.progress` is assumed to count bytes
    (`completedUnitCount`/`totalUnitCount`); servers without a length show an indeterminate bar.
97. **Limits** (decided 2026-11-04). At most 5 downloads per tab per minute, 20 at once, and
    500 MB for all downloaded files together (`storage::MAX_DOWNLOADS_TOTAL_BYTES`): new downloads
    are refused, one announcing a size that does not fit is refused, and a running one is stopped
    when the total passes the cap. `downloads/` is excluded from device backups
    (`NSURLIsExcludedFromBackupKey`; iCloud/Finder backups on iPadOS, Time Machine on macOS;
    **[verify]** that Time Machine honours it).
98. **[verify] Export.** iPadOS: the Files export picker copies the file (the staged copy stays
    until deleted). macOS: a save-panel sheet, then `NSFileManager` copy (keeps the quarantine mark;
    an existing file at the destination is replaced after the panel's own confirmation).
99. **[verify] Popups.** A `target=_blank` download link opens a new tab that stays empty while the
    file downloads; Safari closes such a tab. Resuming failed downloads is not supported (WebKit's
    resume data is discarded); a download interrupted by quitting is deleted at the next launch.
100. **Private downloads** (confirmed 2026-11-04) are listed for the session and deleted (with
    their files) when the last private tab closes, unless saved or shared before.

101. **[blocked] Passkeys** (reported 2026-11-04 on macOS: Google sign-in fails with
     `passkey_first_auth_factor_error`). Cause: WebKit accepts WebAuthn requests from a third-party
     browser only for relying parties tied to the app's App ID, unless the app has Apple's managed
     entitlement `com.apple.developer.web-browser.public-key-credential` (macOS 13.3+, Mac Catalyst;
     request form "macOS Browsers Passkeys", Account Holder only). Not fixable in code. To verify
     once granted: whether WebKit asks for the person's consent itself or the app must call
     `ASAuthorizationWebBrowserPublicKeyCredentialManager.requestAuthorizationForPublicKeyCredentials`
     (AuthenticationServices; not linked yet); and, on iPadOS, whether
     `com.apple.developer.web-browser` enables passkeys in `WKWebView` (assumed, unverified).


## Round 10 (2026-11-04)

102. **[blocked] Default browser on iPadOS** (reported 2026-11-04: Snorkle is not listed under
     Settings → Apps → Default Apps → Browser App). The list includes only apps signed with
     `com.apple.developer.web-browser`, which Apple grants per bundle identifier. Found: the
     bundle identifier is still the placeholder `com.example.snorkle`, and the entitlement reached
     the signed app only if someone copied it by hand into the generated Xcode project after every
     `tauri ios init` (Tauri 2.12 merges `Info.ios.plist` but never entitlements;
     `tauri-cli` 2.12.4 `mobile/ios/build.rs`). Fixed in code: `npm run ios:build` now copies it and
     reports whether the signed app and its provisioning profile carry it. Not fixable in code: the
     grant itself (Account Holder; "Default browser entitlement request" form) and enabling it for
     the App ID. **[verify]** on device: the build script's report, then the Settings list.
103. **[verify] Entitlement sync script.** `scripts/ios-entitlements.sh` uses PlistBuddy
     (`Delete`, then `Merge`) and `plutil -lint`; written without a Mac, not yet run. The build
     check reads `codesign -d --entitlements - --xml` (older Xcode: `:-`) and
     `security cms -D -i embedded.mobileprovision`.
104. **[decide] Blocked list scope.** Matching ignores the query (so a search for "reddit.com" is
     not blocked) and frames inside pages (embedded videos still play). Private tabs are blocked
     too. The list is a file in the app's container, so it is a commitment device, not a lock.
105. **[verify] Passage typing on iPad.** The chrome accepts only one-character `insertText` and
     `deleteContentBackward` input events, and reverts anything else (paste, drop, predictive
     text, dictation, the double-space full stop). Assumed from WebKit's Input Events support; check
     on device with the software keyboard and a hardware keyboard.
106. **[verify] Attached URL bar.** On iPadOS the bar's height includes the home-indicator inset
     (`env(safe-area-inset-bottom)`); with the software keyboard up, pages and the bar behave as
     they did with the pill (the keyboard is over both).
107. **[decide] Tab favicons and privacy.** Each site visited in a non-private tab now has its
     `favicon.ico` fetched once (from the sandboxed favicon view, no cookies) and the host is
     stored in `icons-v3/` until Settings → Cache is cleared; before, only pinned sites were. The
     site already saw the visit, so the extra request reveals little, but the stored icons are a
     list of hosts visited that history clearing does not remove.
108. **[verify] ⌘T sometimes does nothing** (reported 2026-11-04). Not reproduced from the code.
     Now logged per route (Menu → Console): if a press that did nothing has no
     `shortcut new_tab` line, the key never reached Snorkle's key command (most likely the page,
     or the chrome's web view with nothing focused, consumed it); if it has one, the palette
     failed to open.
109. **[verify] Trackpad scrolls go sideways or Back on iPadOS** (reported 2026-11-04; Snorkle only,
     not Safari; touch scrolling is fine; recent). Not explained by the code: no injected script
     handles wheel or scroll in a way that redirects it, view modes off changes nothing, and the
     page-scale transform is a uniform scale. Candidates added in the last updates: the root
     view's hover recognizer (edge tab, 2026-11-03), the page-scale transform (2026-11-03), pull to
     refresh (2026-11-01). Debug builds now log each pan (scroll probe): a sideways gesture or a
     held Shift means the input is wrong before WebKit; a vertical gesture with a sideways page
     move or a Back navigation points at WebKit or our view setup. Settings → General → Swipe
     between pages turns WebKit's back swipe off for diagnosis. Updated owner report 2026-11-04:
     neither that setting nor Pull to refresh fixes scrolling. It also fails in the chrome's
     Settings modal. Portrait works; returning to landscape breaks it again. A horizontal trackpad
     gesture was logged as a vertical pan (x 0, y nonzero), with no modifier keys held. This points
     to an orientation mismatch before the individual web views, not page scrolling code or a
     held Shift key. The owner tested the explicit `UIApplicationSupportsIndirectInputEvents`
     opt-in (`9cb5dd3`): **no improvement**. Horizontal gestures still report vertical pans.
     Next candidate: declare a single-window `UIApplicationSceneManifest` with `TaoSceneDelegate`.
     In pinned tao 0.37.2, the manifest makes startup wait for scene connection, and the window is
     attached to that scene before its root controller is installed. Without it, startup takes the
     legacy application-delegate path. This may resolve a scene/window coordinate mismatch but is
     **not yet verified on an iPad**. `ios:build` checks the scene declaration in the actual IPA.
     Debug scroll logs now include live scene orientation and pan vectors in window coordinates.
     See `tests/device/trackpad.md`, including lifecycle and external-link checks. If this also fails,
     compare those vectors and inspect the root hover recognizer before considering an explicit
     orientation-aware indirect-input correction; swapping page offsets alone would miss Settings,
     nested scroll areas, inertia and history gestures.


## Sidebar UI and favicons (2026-11-04)

- **[verify]** On iPad and Mac, confirm the image-only hidden WebKit snapshot
  shows declared SVG/PNG/ICO favicons. Check claude.ai, laurieanderson.com and
  springer.com after the icons-v4 migration; this cloud environment cannot reach
  those sites. Snapshot format/transparent rendering is only type-checked here.
- **[verify]** Test Cmd-click on the fixed sidebar divider with Magic Keyboard,
  plus ordinary dragging, and domain close-all in normal/private tab groups.
- **[verify]** Send a chat with each configured provider using Default and a
  non-default effort. Anthropic capability parsing uses the provider SDK schema;
  no API credentials are configured here. OpenAI uses explicit known model
  capabilities; unrecognized models retain Default until their support is known.

## Dark loading and navigation follow-up (2026-11-06)

- Verify black native page canvases and the document-start dark-mode cover on
  physical iPad/macOS during reload, back/forward restoration and app resume.
  The cover is removed by the existing dark detector; no timer exposes an
  unchecked page. This cloud environment cannot verify compositor timing.
- Verify lazy-loaded media remains hidden until deliberate hover on affected
  sites. Media selectors now outrank ordinary site rules and DOM insertions
  refresh pointer-reveal state; custom shadow-root players still need site testing.
- Check New Tab centering with either sidebar docked, result priority, history
  opening a fresh tab, bookmark movement, and deselection while a text read is pending.

Header View follow-up: device-check black hidden media on both inverted light
sites and sites with their own dark theme, including lazy placeholders and
hover reveal. Run Detect Headers again to generate page-specific snippets;
verify provider output on a long article and a sectioned home page. Existing
site selector rules alone do not trigger paid summary requests on navigation.

URL bar / refresh follow-up: verify two tabs retain independent attachment
settings after restarting; verify Cmd+R and the button interrupt a slow load
and reload from origin on iPad/macOS. Media brightness now halves RGB before
counter-inversion, visually equivalent to a 50% black overlay with unchanged
alpha. Test hover reveal for video compositing on device. The NASA Mars article
URL reported in session is blocked by this environment's network policy; local
DOM checks cover white article panels inside dark roots, fully dark pages,
and backgrounds that change after load. Live NASA verification is pending.

## Start page, tab switching and Define (2026-11-08)

- **[verify]** ⌘⇧] / ⌘⇧[ on iPad (UIKeyCommand input `]` / `[` plus Shift, as ⌘⇧\ matches `\`)
  and on macOS (key monitor, `charactersByApplyingModifiers` gives `]`), with a page, the chrome
  and nothing focused, on a US and a non-US layout. Users who had bound either combination to
  another shortcut get the clash error and the defaults.
- **[verify]** Launch on device: the start page shows with its input focused (on iPad without a
  hardware keyboard this raises the software keyboard), no restored page loads until chosen, and
  resuming from the background adds no tab. If iOS terminates the app in the background, the next
  open is a launch and does add one.
- **[verify]** A cold launch from a link in another app opens only that link (no start page) when
  iOS delivers the URL before setup finishes; if it arrives later, an empty start tab remains.
- **[decide]** Choosing another tab, pin or bookmark from the start page closes the empty tab
  (Chrome's behaviour); ⌘T always opens another empty tab, also from an empty one.
- **[decide]** "A new tab in the AI chat sidebar" was implemented as a new chat in the chat panel
  (the panel has one conversation per browser tab and an archive, not tabs of its own), bound to the
  start page's tab and kept when that tab then navigates.
- **[verify]** Define with each provider: reply format and cost; checked here only against a mock.

## Dark mode: sections kept dark (2026-11-08)

- **[verify]** On device (WebKit), on sites with dark headers, footers, hero bands and code blocks:
  they keep their dark colour and their text stays readable; white cards inside them turn dark.
  Checked here only in Chromium on a test page.
- **[verify]** Cost of the scan (≤ 4,000 `getComputedStyle` calls per pass, at most every 0.5 s on
  pages that keep changing) and of many filtered elements (each is a compositing layer) on long
  pages, on an iPad.
- **[decide]** A dark section that contains a `position: fixed` element is left inverted (bright),
  since filtering it would re-anchor the fixed element to the section. Backgrounds drawn with
  images or gradients are not considered, only background colours.
- Double inversion (`invert` + `hue-rotate(180deg)` twice) is not exact for saturated colours: a
  pure red image shows slightly desaturated, the same as images were already shown on inverted
  pages.

## Links to apps (2026-11-09)

- **[verify]** On iPad with the Google Docs and Sheets apps installed: following a link to a doc or
  sheet loads it in Snorkle (default "Browser"); ⋯ → Open in App opens it in the app; with
  "Automatic" it jumps as before. Also check a site without an app (toast, nothing opens).
- **[decide]** `_WKNavigationActionPolicyAllowWithoutTryingAppLink` is undocumented. It is an enum
  value passed to a public delegate method, not a private selector, and Chrome for iOS relies on it
  (WebKit bug 216624), but App Review could object; without it the only public way to keep a page in
  the browser is to cancel and reload it, which is not equivalent. Current WebKit handles it (it
  skips the app and goes on to load); re-check on each iPadOS release in case it is removed.
- **[decide]** No prompt before the jump: iOS cannot say whether an app claims an address without
  opening it, so the app is offered from the ⋯ menu instead of asked about on each link.

## Sidebar rows, ⌘-click Back, Save for Later (2026-11-09)

- **[verify]** Save for Later on iPad and Mac: the thumbnail matches what the tab showed (view
  modes included); saving a tab that is not on screen (from its sidebar row while another tab is
  shown) may give no thumbnail, since WebKit may not draw a hidden view (then the site's letter is
  shown). Thumbnail size and JPEG quality (320 pt, 0.6) are guesses to tune.
- **[decide]** Bookmark and Recover both remove the page from Save for Later (as bookmarking a tab
  moves it into the bookmark); clicking a thumbnail also recovers it.
- **[decide]** Back from a ⌘-clicked tab loads the opener's page as a new entry (WebKit cannot
  prepend history to a new web view): afterwards Back returns to the link's page, not Forward. The
  alternative, copying the opener's `interactionState`, would also bring its older history and load
  the opener's page first.
- **[verify]** Hover-only row buttons with a Magic Keyboard trackpad on iPad (pointer hover) and on
  touch alone (active row only).

## Links sent to Snorkle (2026-11-09)

- **[verify]** On iPad and Mac (installed build): `snorkle-https://example.com`,
  `snorkle://open-url?url=…` and `snorkle://example.com` from Safari, Notes and Shortcuts each open
  one new tab, cold and warm launch. Tauri delivers custom-scheme URLs through the same
  `RunEvent::Opened` as http(s) links (assumed from the macOS `application:openURLs:` and iOS
  `openURLContexts` paths; not run here). On iPadOS the generated Xcode project must carry the
  plist's `CFBundleURLTypes` (as for http/https).
- **[decide]** Scheme names: `snorkle`, `snorkle-http`, `snorkle-https`. Another app could claim the
  same names; iOS then picks one of the apps, undefined which.


## Define inline, tab order and labels, Edit, Copy, Save for Later archives (2026-11-09)

- **[verify]** Save for Later archives on iPad and Mac (nothing here was run on Apple hardware;
  the capture scripts were run in Chromium against a local test page, the archive format against
  Python's `plistlib`). Assumed from WebKit's source (`WebLoaderStrategy::scheduleLoad` →
  `DocumentLoader::scheduleArchiveLoad`; `disallowWebArchive` lets `loadData:` archives through in
  the main frame): a web archive loaded with `loadData:` and scripts on runs the page's scripts,
  and WebKit answers their requests (scripts, styles, fonts, images, `fetch`, `XMLHttpRequest`)
  from the archive when it holds the URL, else from the network. Check on a real article
  (interactive piece, a Next.js site, a page with web fonts) in airplane mode.
- **[verify]** `callAsyncJavaScript` in the default client world: `fetch` there uses the page's
  origin and cookies (same-origin resources) and `performance.getEntriesByType('resource')` sees
  the page's loads. Cross-origin resources without CORS headers cannot be read by the script; they
  come from WebKit's own archive of the page (`createWebArchiveData`), which holds what the
  document references, not what scripts fetched. A cross-origin data request without CORS is
  therefore missing offline.
- **[verify]** POST requests, WebSockets, service workers and media larger than 15 MB are not
  archived; pages that need them degrade offline (as in thedailyprophet). Pages whose HTML could
  not be fetched again are saved as WebKit's copy of the live document and shown with
  JavaScript off.
- **[verify]** Back and forward between pages of a saved copy are left to WebKit (links open pages
  from the archive as new entries). Going back to an archive entry WebKit no longer holds in its
  page cache may reload it from the network, i.e. fail offline. A session restored after a
  relaunch reopens a saved copy's tab as the live page.
- **[decide]** Clicking a saved page's thumbnail opens its saved copy (it stays in the list);
  Recover opens the live page and removes it, as before. "+" opens the saved copy with scripts off
  (so script-driven links are plain links) and link taps choose pages on the same host (at most 30
  pages per saved page); Add loads each chosen page live in that tab, saves it into the same
  archive and shows the saved copy again. Pages a site loads only through scripts (single-page
  app routes without real links) cannot be chosen this way.
- **[decide]** Limits: 15 MB per resource, 80 MB per saved page (all its pages), 1 GB for all
  archives (the oldest lose their archive first and keep address, title and thumbnail), 1,500
  resources, 90 s per capture.
- **[verify]** Emoji in the Edit dialog on iPad: the dialog is no longer inside the sidebar (whose
  flyout could hide when the system emoji picker appeared, taking the field with it; assumed cause,
  not reproduced), and a grid of common emoji fills the field without the system picker.
- **[verify]** Define's spelling check depends on the model following the `SPELLING:` instruction;
  a model that defines a misspelling directly still gives a definition (no question is asked).
- **[decide]** Dragging a tab onto the top or bottom 30% of another row moves it there; the middle
  pairs them in split view, as before. A split half dropped between rows leaves its pair. Same-site
  groups are still drawn at their first member's position, so order inside a group follows the
  drag but a group does not split.
- **[verify]** The Copy button sits beside Share inside the address field; with a narrow address
  field (narrow windows), check the address is not hidden by the two buttons.
