# Snorkle: architecture and threat model

Status: initial shell, 2026-10-29. Targets iPadOS 27 (current) and 26 (prior); since 2026-11-03
also macOS 15 and later with Safari 26 or later (§1a).
Priorities: security first, speed second.

## 1. Components

```
┌───────────────────────────── UIWindow (one UIScene) ─────────────────────────────┐
│ root UIView (Tauri/tao)                                                         │
│  ├─ WKWebView #0  "main"  ← Tauri + wry. Trusted chrome UI (ui/, tauri://).     │
│  │     Has the Tauri IPC bridge. Capability "chrome" only.                      │
│  └─ container UIView per live tab (clipsToBounds, placed BELOW the toolbar)     │
│        └─ WKWebView  ← created by snorkle-content via objc2. Untrusted pages.   │
│              No IPC bridge, no message handlers, no schemes. User scripts only  │
│              in the isolated client world (see §5c).                            │
└─────────────────────────────────────────────────────────────────────────────────┘
          ▲ commands (ACL-checked)              ▲ WebKit delegate callbacks (metadata only)
          │                                     │
   src-tauri (glue) ──── locks ────► snorkle-core::Browser (all decisions)
          │                                     │
          └── storage thread (JSON files) ◄─────┘ effects: RecordHistory/PersistSession
```

| Crate / dir | Responsibility | Trust |
|---|---|---|
| `crates/snorkle-core` | Tab/session model, suspension and discard, navigation policy, HTTPS-first, address-bar display (eTLD+1, IDN policy), input parsing, history/bookmark/session storage, content-blocking list parsing and WebKit rule generation, OS-version policy. Pure Rust, `#![forbid(unsafe_code)]`, no platform dependencies. | Trusted. All decisions live here. |
| `crates/snorkle-content` | Creates, lays out and destroys the per-tab `WKWebView`s; maps profiles to `WKWebsiteDataStore`s; implements `WKNavigationDelegate`/`WKUIDelegate`/KVO and forwards WebKit metadata to the core; installs the compiled content rule list. `src/webkit` holds the WebKit code both Apple hosts share (delegates, configuration, injection, rule lists, HTTPS requests, Keychain); `src/ios` (UIKit) and `src/macos` (AppKit) hold the views, input and system UI. A stub elsewhere. | Trusted code that handles untrusted data. |
| `src-tauri` | Tauri app: one window, one webview (`main`) with the chrome; Tauri commands; routes core effects to the content host (main thread), the storage thread and the chrome. | Trusted. |
| `ui/` | Chrome UI: tab strip, toolbar, address bar, interstitials, start page, bookmarks/history panel. Plain HTML/CSS/JS, no build step, no npm packages. | Trusted, local, bundled. |
| Web content | Rendering, JS, networking, TLS validation, same-origin policy, mixed-content blocking, process isolation. All done by system WebKit. | Untrusted. |

### Why two kinds of web view (approved change)

In Tauri 2.12.5, `Window::add_child` is compiled only for `desktop`, and wry 0.57.1 documents
`build_as_child` as "Android/iOS: Unsupported". So on iPadOS a Tauri window holds exactly one
Tauri webview. The chrome uses that one. Content views are plain `WKWebView`s created from Rust
via `objc2`/`objc2-web-kit`. You approved this deviation from "WKWebView via wry" on 2026-10-29.
It also gives the content side full delegate access (frame info, provisional failures,
popups, permission prompts, process termination). wry's iOS surface does not offer this.

`objc2-web-kit` 0.3.2 binds `WKWebView` from the macOS SDK only, so
`crates/snorkle-content/src/ios/bindings.rs` declares the UIKit `WKWebView` by hand, the same way
wry does. All objc2 crate versions are pinned to the versions wry resolves, so only one copy is
linked. Two copies would register duplicate Objective-C classes.

## 1a. macOS (2026-11-03)

The same design on AppKit. The Tauri window's content view holds wry's chrome `WKWebView`; the
host adds a `ContentRoot` view above it (`macos/views.rs`) that draws nothing and is never hit
itself, so clicks outside page containers reach the chrome. Each page sits in a `Container` that
clips to its pane, is masked with the chrome's holes (`CAShapeLayer`, even-odd, as on iPadOS) and
returns no hit inside a hole, so the URL pill, its popups, toasts and the sidebar tab take the
clicks there. Views are unflipped (bottom-left origin); rectangles from the chrome (top-left) are
converted at the boundary (`views::Tl`). Containers are reordered with `sortSubviewsUsingFunction`
rather than removed and re-added, which would take a web view out of its window.

Platform mapping:

| iPadOS | macOS |
|---|---|
| Hand-written UIKit `WKWebView` binding | `objc2-web-kit`'s own (AppKit) binding |
| App delegate key commands (`ios/menu.rs`), container key commands as fallback | A local key-down monitor (`macos/keys.rs`): sees every key event of the app first, consumes only bound ⌘ combinations (⌘C, ⌘V, ⌘Q and the menu bar keep working; Tauri's default menu supplies Edit) |
| Tap recognizer on containers (split focus), hover recognizer (edge tab) | A local mouse monitor: a click whose hit view is inside a container focuses that pane; pointer moves within 160 pt of the left edge are reported; a tracking area reports leaving the window |
| `UIRefreshControl` | none (no pull to refresh) |
| Page scale by view transform | `WKWebView.pageZoom`, which on macOS reflows the page in a wider viewport, as Safari's zoom does |
| Share sheet, Files export, `UIReferenceLibraryViewController` | `NSSharingServicePicker`, `NSSavePanel` (window sheet), `showDefinitionForAttributedString:atPoint:` (Look Up) |
| Memory warning, did-enter-background | No memory warning; on resigning active the core ticks and the session is written; on quit (`RunEvent::Exit`) the session is written synchronously (`Shared::flush_blocking`) |
| OS version gate (`os::check`) | Safari's version (`/Applications/Safari.app`), since WebKit updates with Safari on older macOS too (`os::check_macos_safari`, minimum 26.0, newest known 27.2); it is also the version in the user agent |
| `overrideUserInterfaceStyle` | `NSAppearance` (window; content views in dark pages mode) |
| Keychain, `WhenUnlockedThisDeviceOnly` | The data-protection keychain with the same class when the build is signed with an application identifier; otherwise the login keychain (`webkit/keychain.rs`) |

**Window.** No title bar (`titleBarStyle: Overlay`, hidden title): pages run the full height. The
close, minimise and zoom buttons sit in a 40 pt strip at the top of the docked sidebar
(`#titlebar`, `trafficLightPosition` 16, 20) and are hidden while the sidebar is not docked
(`left` inset 0), so they never float over a page. The strip is a `data-tauri-drag-region`: it
drags the window and a double-click zooms it, through Tauri's own drag script and two window
permissions granted to the chrome (`core:window:allow-start-dragging`,
`core:window:allow-internal-toggle-maximize`). With the sidebar hidden there is no drag area;
⌘\ shows it. The chrome detects macOS by `navigator.maxTouchPoints == 0` with a Macintosh user
agent (an iPad reports touch points, also in desktop mode).

**Data at rest.** macOS has no `NSFileProtectionComplete`; the profile and website data are in the
user's Library (`~/Library/Application Support/<identifier>` and WebKit's stores), protected by
the user account and FileVault. The app is not sandboxed; WebKit sandboxes its own content
processes as for Safari.

## 2. Trust boundaries

| # | Boundary | Direction | Mechanism | Enforced by |
|---|---|---|---|---|
| B1 | Web content → app process | page → native | **None by design.** Content `WKWebView`s get no `WKScriptMessageHandler` or `WKURLSchemeHandler`, and are not Tauri webviews, so no Tauri IPC script, invoke key or `ipc:` scheme exists in them. The only data that crosses is WebKit-generated metadata in delegate callbacks (URL, frame, navigation type, NSError, title via KVO). The only injected code is the user's own scripts and the view-mode styles (black and white, hidden images), in the isolated client world with no way to send messages back (§5c). Exception, only while console recording is on: a console hook in the page world posts page console output to one message handler (§5d, SECURITY_REVIEW C13). Exception, only when the user sends a page-chat message: the result of one constant script (title, visible text, selection) is read back (§5g, C17). Exception, only when the user saves a page for later or adds pages to a saved one: constant capture code reads back the page's HTML, the URLs it loaded and those resources (§5b, C22). | `CONTENT_VIEW_SPEC` (compile-time assert in `ios/mod.rs`), `tests/content_view_spec.rs`, `src-tauri/tests/channel_tripwire.rs`, on-device `tests/device/ipc-probe.html` |
| B2 | Chrome UI → Rust commands | chrome → native | Tauri IPC with app ACL manifest (`build.rs`): every app command and core plugin command is checked against `capabilities/chrome.json`, which grants every app command in `command_list.rs` and `event:listen/unlisten` to webview label `main`, local origin only. No `remote` URLs, no window-label grants, `dynamic-acl` feature compiled out. | Tauri runtime authority; `src-tauri/tests/ipc_isolation.rs`, `config_hardening.rs` |
| B3 | Rust → chrome | native → chrome | `emit_to("main", "state" \| "toast")`. Payload is core state. Page-derived strings (titles, hosts) are sanitised in Rust (`text::sanitize_page_text`: bidi and control characters removed, length capped) and rendered with `textContent` only. | `text.rs` tests, `channel_tripwire.rs` (no `innerHTML` and similar) |
| B4 | Content pixels ↔ chrome pixels | visual | Content containers sit in their own UIViews inside the content area (between the status bar, the sidebar and any docked panel; down to the bottom of the window), `clipsToBounds`. The URL pill, its popup and toasts float over the content area as **holes**: every container is masked there and does not take touches there, so the chrome below shows through and a page can never draw over or under them (§5f). Element fullscreen is disabled. Interstitials, prompts and errors are drawn by the chrome while the content view is hidden, never inside page content. Other chrome overlays hide content (`set_chrome_layout(overlay)`). | `ios/mod.rs` (`apply_mask`), `ios/keys.rs` (`pointInside`), `snorkle_content::sanitize_holes`, `Browser::sync_visibility`, `browser_invariants.rs` |
| B5 | App ↔ disk | native → storage | JSON files in the app container, written atomically off the main thread. Only http(s) URLs are stored, and only from non-private tabs. Data protection `NSFileProtectionComplete` (entitlement). | `storage.rs` tests, `browser_invariants.rs::private_tabs_never_reach_storage`, fuzz `browser_events` |
| B6 | App ↔ network | content → internet | WebKit for pages. The app itself makes no telemetry or update requests; its own requests are favicon fetches for pins (sandboxed web view) and, only when the user has connected an AI account, HTTPS calls to `api.openai.com` / `api.anthropic.com` (`snorkle_core::ai::is_allowed_url`, ephemeral `NSURLSession`): site-script drafts, Detect Headers, and page chat (which sends the page's text, §5g). ATS stays on for the app; `NSAllowsArbitraryLoadsInWebContent` is set only so HTTP can load after explicit user consent. TLS: system validation, `PerformDefaultHandling` for every auth challenge, deprecated TLS refused. | `delegates.rs`, `config_hardening.rs` |
| B7 | Profiles | site data | Default profile → `WKWebsiteDataStore(forIdentifier:)` with a fixed UUID. Private → one non-persistent store per private session, wiped and released when the last private tab closes. The chrome's own webview uses the default store, which is separate from both. | `ios/mod.rs::store_for`, `release_private_store` |

## 3. Threat model

Attacker capabilities considered:
- **A1, malicious website**: arbitrary HTML/JS/CSS in any frame, redirects, popups, crafted URLs.
- **A2, network attacker**: on-path on Wi-Fi; can block, delay or downgrade traffic and present
  invalid certificates.
- **A3, a compromised WebContent process** (WebKit renderer exploit).
- **A4, local attacker with a locked device** (theft).

Out of scope for now: a compromised UI process (WebKit or OS exploit beyond A3), malicious
filter lists (none shipped yet), extensions (non-goal), sync (non-goal).

| Threat | Attacker | Mitigation | Residual risk |
|---|---|---|---|
| Page calls app commands (classic Tauri/Electron bridge abuse) | A1 | B1: no bridge in content views at all. B2: even if a remote page were ever loaded in the Tauri webview, the ACL rejects every app and plugin command from non-local origins (tested). | A renderer exploit (A3) is contained by WebKit's process sandbox. It still cannot reach Tauri IPC, which lives only in the chrome webview's WebContent process. |
| Page navigates to internal schemes (`tauri:`, `ipc:`, `asset:`) or `file:` | A1 | Policy blocks them in every frame and for every source, plus internal hostnames. Content views have no handlers for those schemes. | – |
| Address-bar spoofing: lookalike IDN | A1 | Unicode is shown only for single-script labels (UTS #39 highly restrictive), with no forbidden characters and no whole-script Latin lookalikes outside native TLDs; otherwise punycode plus a warning marker. | Policy not yet checked against Chromium's corpus (OPEN_QUESTIONS #12). Pure-ASCII lookalikes (`rn` vs `m`) are not detected. |
| Spoofing: long subdomains, userinfo, path tricks | A1 | Registrable domain (PSL incl. private suffixes) is emphasised and never elided; subdomains elide from the left; userinfo is never shown, and page links with credentials are blocked; path is not shown when collapsed. | PSL baked in at build time can go stale (OPEN_QUESTIONS #13). |
| Spoofing: showing URL B while rendering A | A1 | Only committed main-frame URLs are displayed. KVO URL changes are accepted only if same-origin and not provisional. A failed provisional load keeps the old URL. | Relies on WebKit's commit semantics (OPEN_QUESTIONS #6). |
| Spoofing: page draws fake chrome or interstitials | A1 | Content is clipped below the toolbar. Element fullscreen is off. All browser UI is chrome-drawn with content hidden. JS dialogs are suppressed (no page-styled modal). | Video fullscreen uses the system player (no URL shown, system UI). |
| Spoofing via title or dialog text (bidi, newlines) | A1 | `sanitize_page_text`; per-label bidi isolates in the UI. | – |
| Silent HTTP downgrade / SSL stripping | A2 | HTTPS-first: every main-frame `http:` is upgraded (typed, links, redirects, popups, restores). On failure or an https→http redirect loop the chrome interstitial requires an explicit "Continue to HTTP"; the exception is per host, per profile, in memory only. WebKit `upgradeKnownHostsToHTTPS` on. | First visit to an HTTP-only site needs user consent (by design). Subframe and subresource http follows WebKit mixed-content rules. |
| Invalid certificates | A2 | No trust override path exists in code. Certificate errors on explicit https are not bypassable. | – |
| Popups / tabnabbing | A1 | `javaScriptCanOpenWindowsAutomatically = false`. `createWebView` runs the navigation policy, then returns a popup web view built from WebKit's configuration, shown as a tab. WebKit applies `noopener` to `target=_blank` links. The address bar shows each tab's own committed URL. | A popup can navigate its opener (needed for OAuth). That is visible in the address bar, but could still fool an inattentive user. |
| Opening other apps (`tel:`, `itms-apps:` …) | A1 | Main frame only, with a gesture-classified navigation, and always behind a chrome confirmation prompt. | Gesture detection relies on `WKNavigationType` (OPEN_QUESTIONS #7). |
| Drive-by downloads | A1 | Since 2026-11-03 downloads are supported (§5h): main frame (frames inside the page only after the user allows it), `policy::may_download` (http/https without credentials, the page's own `blob:`/`data:`), at most 5 per tab per minute and 20 at once. Files land in the app's staging folder under an app-chosen, cleaned name; nothing is opened or run; they leave only when the user saves or shares them. macOS marks them quarantined. | A page can still start a download without a click (an attachment response); the rate limit, the 500 MB cap and the visible list bound it. |
| Memory-pressure DoS (crash loops) | A1 | Live-view budget, background TTL, discard on memory warning, bounded auto-reload after WebContent termination (2 per 60 s, then a "page keeps crashing" notice). | Budgets unvalidated on hardware (OPEN_QUESTIONS #3). |
| Private-mode leakage | A1/A4 | Separate non-persistent store, never written to history/session/bookmarks (enforced in core and fuzzed), store wiped on last private tab close, exceptions cleared. | WebKit-internal caches outside the data store, if any (OPEN_QUESTIONS #9). |
| Data at rest | A4 | `NSFileProtectionComplete`. No credentials stored by the app; credentials only via iOS Keychain/AutoFill (§6). | – |
| Outdated WebKit | all | Deployment target 26.0, a runtime check that refuses to browse below it, and a banner when below the newest known patch level. | Patch table must be updated each release (OPEN_QUESTIONS #2). |
| Supply chain | – | Exact-pinned crates plus `Cargo.lock`; `cargo deny` (sources, licences, bans) and `cargo audit`; zero JS packages; no Tauri plugins. | Tauri itself is the bulk of the graph (235 crates on iOS, 44 for the core). |

## 4. Navigation pipeline

1. **Typed or bookmark:** `input::interpret` → `policy::decide(Main, Typed)`. Then: `Load`, `UpgradeToHttps`,
   `External` (prompt) or `Block` (notice). A blocked URL never reaches WebKit.
2. **Every WebKit navigation action** (`decidePolicyForNavigationAction`) → `Browser::decide_navigation`.
   Exception: a link activation with ⌘ held (WebKit's `modifierFlags`, iOS 18.4+) is cancelled and
   opened in a background tab instead (`Browser::on_background_tab_request`: http/https only,
   `policy::decide`, the opener's profile; it loads hidden within the live-view budget).
   Our own loads are recognised by URL and keep their original source. Anything else is
   classified from `WKNavigationType`; `.other` (redirects, script) gets the most restrictive
   page source.
3. **Response:** non-renderable MIME types are cancelled (no downloads).
4. **Provisional start → commit → finish/fail:** drives the address bar, HTTPS-first fallback,
   history and session.
5. **Popups:** `createWebView` → `open_popup` → policy → a web view built from WebKit's
   configuration, in the opener's profile. A `window.open` that asks for a window size
   (sign-in and payment buttons) becomes a **sheet**: centred over the dimmed opener, which stays
   on screen so its scripts keep running (hidden views are suspended), with the sheet's URL in
   the address bar, a "Pop-up" marker and a Close Pop-up button. Sheets are not listed as tabs or
   saved in the session; choosing the opener brings its sheet back to the front, and closing the
   opener closes its sheets. Other popups (target=_blank) open as tabs.

The policy runs synchronously on the main thread. It is a pure function with no I/O. Effects
are applied asynchronously on the main queue, so WebKit callbacks never re-enter UIKit code.

## 5. Tabs, memory and performance

- A new tab creates no web view until it navigates. Restored sessions are lazy: only the
  active tab gets a view.
- States are Active, then Background (live, hidden; WebKit `inactiveSchedulingPolicy = .suspend`),
  then Discarded (no view; `interactionState` kept in memory so back/forward and scroll survive).
- Budget is by physical RAM: <4 GB: 2 live views, <6 GB: 3, <8 GB: 4, <16 GB: 6, otherwise 8.
  Background views idle for 10 min are discarded. A memory warning leaves only the active view.
  These are **targets, not measured values** (see `docs/PERFORMANCE.md`).
- Heavy work stays off the main thread: blocklist parsing and rule JSON on a background thread;
  WebKit compiles the rule list in its own process; disk writes on a storage thread with
  session-write coalescing; chrome state updates coalesce progress to 5% steps.

## 5a. Content blocking

- **Lists:** EasyList and EasyPrivacy, bundled (`src-tauri/resources/filters`). They are
  converted by Brave's `adblock` crate (`snorkle_core::blocking`) into WebKit content-rule-list
  JSON: about 127k rules, split into 3 lists of at most 50k rules. Every list repeats all 1.4k
  exception rules, because WebKit applies `ignore-previous-rules` only within its own list.
  Conversion takes about 0.5 s (release build, laptop) and runs on a background thread.
- **Caching:** WebKit compiles the lists and stores them as `snorkle.cb.<key>.<i>.of.<n>`, where
  the key hashes the converter version, the lists and the allowlist. Later launches look up the
  compiled lists instead of converting again; lists for other keys are deleted
  (`snorkle-content/src/webkit/rules.rs`).
- **Enforcement:** WebKit blocks network loads and hides elements with `css-display-none`,
  inside its own processes, with no per-request work in the app. Blocking injects no scripts, so
  scriptlet filters are unsupported.
- **Per-site toggle:** stored by registrable domain (`adblock-allow.json`). It becomes one
  trailing `ignore-previous-rules` rule with `if-top-url`, then the lists are rebuilt and the tab
  reloaded. Disabled in private tabs, because the setting is persisted.
- **Security:** a rule list can only block, hide or upgrade content, never run code. Allowlist
  entries are validated to `[a-z0-9-.]` domains before they go into a regex. Rule lists are not a
  page-to-native channel.

## 5c. User scripts, view modes, split view

- **User scripts and styles** (`snorkle_core::userscripts`, `snorkle-content/src/webkit/injection.rs`)
  are the one owner-approved exception to "no scripts in content". They are installed as
  main-frame `WKUserScript`s in WebKit's isolated client world and guarded by
  `location.hostname`. There is no native bridge. See SECURITY_REVIEW I1/I2.
- **View modes** (`snorkle_core::viewmodes`): desaturate, hide media, dark and headers. Each is on or
  off app-wide; a long press on its button offers "Disable for this tab", which exempts that
  tab (popups inherit their opener's exemptions, since they share its content controller). The
  host compiles the injected scripts once for the app-wide modes and once per exempted tab. The
  **headers** mode runs a constant extractor in the client world that reports the page's h1–h6
  (or the site's detected rules) to a client-world message handler (C14); the chrome shows them,
  nested by level, in a panel docked right of the page, and a click scrolls to the heading and
  briefly highlights it on the page. When the outline has several top-level entries with
  headings under them (what Detect Headers produces: section labels at level 1, stories at 2),
  the panel shows them as foldable sections. Detect Headers sends each candidate's enclosing
  section element and whether it is uppercase, and asks the model for section labels first. The style modes use the same
  mechanism with constant styles, plus one `evaluateJavaScript` to update already-loaded pages.
  **Dark** (off by default; moon button; remembered across launches) makes content web views prefer a dark appearance
  (`overrideUserInterfaceStyle`), so sites with their own dark theme use it; a constant detector
  then checks the page's background (root, body, else the canvas) and, if it is light, marks the
  root so a constant style inverts the page with hues kept (`invert(1) hue-rotate(180deg)`).
  Sections kept dark (2026-11-08): on an inverted page the detector also marks each element whose
  own opaque background (alpha ≥ 0.5, at least 24×16 pt) the inversion would show bright (shown
  luminance > 0.6: a dark header, footer or code block), `data-snorkle-flip`, and the style inverts
  it back, so it keeps its original dark colour with readable text. Inside one, a light element (a
  white card) would now show white and is flipped again; at most two levels. Media whose nearest
  flipped ancestor shows the page as it is get the dark-page media filters. Elements containing a
  `position: fixed` element are not flipped (a filter would make them the fixed element's
  containing block), nor are media or video frames. Each pass looks at ≤ 4,000 elements and
  runs with the light check (document end, load, 1.5 s, and DOM changes at most every 0.5 s);
  marks are decided before the root is marked, so nothing transitions from the inverted state.
  Until the detector has run, the page is dimmed to 20% brightness so white pages do not flash
  while loading. Images, video and embeds are never shown inverted and are dimmed to 60% until
  pointed at (with hide media on, its gray boxes stay, and what it reveals is shown un-inverted). With black and white also on, the root filter includes `grayscale`.
  Video gets its counter-filter on its frame rather than on itself: the detector marks the
  nearest ancestor with the same box as the video (`data-snorkle-media-box`, in the document,
  crossing open shadow roots to their host), and media inside a frame get no filter of their own.
  A filter on a playing `<video>` is not reliably applied by WebKit while the root's inversion
  still is, which showed videos inverted (2026-11-02; see OPEN_QUESTIONS 65).
  Pages are black and white with media hidden by default: images, video, canvas, embedded
  objects and video-player iframes keep their boxes but are drawn as gray rectangles; pointing
  at one fades it in (0.3 s) in black and white. Media under an overlay (video players, images
  behind links) never get `:hover`, so a pointer tracker marks whatever media are under the
  pointer (`elementsFromPoint`, plus video boxes by geometry). CSS background images are
  dropped, and videos are paused when they start unless pointed at. The hide-media style runs in every frame and re-attaches itself if
  a page removes it. The buttons show what pages look like (2026-11-03): the palette is struck
  through while black and white is in effect, the picture while media are hidden, and dark mode
  shows a moon when on and a sun when off; a ring dot marks a mode turned off for this tab. Each launch starts from the stored launch modes (`viewmodes::LaunchModes`,
  `view-modes.json`), set in Settings → General (defaults: black and white and hide media on,
  dark off). The palette and images buttons change the current session only; the moon button
  also stores the new dark setting, so dark mode is remembered. "Disable for this tab" choices
  are saved with the session (`SessionTab.exempt`). Hidden `<img>` and `<video>` sources still download (hover needs them); CSS
  background images do not. Hidden images and videos are drawn opaque, so lazy-loading
  placeholders kept at `opacity: 0` still show their gray box; an image pointed at before it has
  loaded is marked `data-snorkle-loading` (a lazy one is switched to eager loading) and stays a
  pulsing gray box until its load or error event, then fades in. Video (2026-11-09): WebKit does
  not reliably apply a filter on a playing `<video>` itself, so hide media's tracker marks each
  video's frame (`data-snorkle-video-box`: the nearest same-size ancestor in the document, else one
  at most twice its area that contains it, else the video; open shadow roots one level deep, as
  dark mode's media box) and the gray box goes on that frame, revealed while pointed at; media inside
  it get no filter of their own. Inverted dark pages keep using dark mode's media box.
- **Shared content controllers** (fixed 2026-11-09): a popup's web view (a `target=_blank` link,
  `window.open`) is built from WebKit's configuration and so shares its opener's
  `WKUserContentController`; one install of the view-mode scripts serves all of them. The host
  installs a tab's own scripts (modes disabled for it) only when every tab on that controller has
  the same modes, else the app-wide ones (`scripts_on`); before, the tab installed last won in hash
  order, so a page's next load (a site refreshing itself) could lose hidden media. Per-tab modes
  still reach each loaded page through its `apply_js`; a tab whose modes differ from a sibling's
  gets the app-wide modes on its next load.
- **Split view:** the core keeps `splits: Vec<SplitView>`, pairs of tabs of the same profile; a
  tab is in at most one pair. The pair holding the active tab is on screen. Visibility effects
  carry a `Pane` (`Full`, `Left`/`Right` for a pair side by side, `Top`/`Bottom` for one in rows;
  `SplitView::orientation`), and the host places each container in its part, with an 8 pt gap.
  The chrome draws a divider in the gap (no page covers it, so it needs no cut-out); dragging it
  sets the pair's ratio (`SplitView::ratio`, first pane 20–80%, `Browser::set_split_ratio`),
  which the core sends to the host (`Effect::SetSplitRatio`) before placing a pair's panes. ⌘S (side by side) and ⌘⇧S (top and bottom; ⌘D / ⌘⇧D from 2026-11-03 to 2026-11-04) open an empty tab of the active tab's
  profile as the second pane (right or below) and focus its address bar
  (`Browser::split_new`); in a pair they only change its layout, never adding a third pane. The panes on screen are protected from discard; pairs not on screen can
  be discarded and are restored together when chosen. Choosing a tab outside any pair shows it
  alone and keeps the pairs. Closing a tab, pairing one of its tabs elsewhere, or unsplitting
  ends a pair. The sidebar shows each pair as one row with a half per tab; dragging a half out
  of the row unsplits it. Pairs are not saved in the session. A tap in a pane focuses it.

## 5f. URL bar

The address bar is a pill floating over the bottom of the page (pages run the full height of
the window): back, forward and reload on its left (the sidebar's toggle is a tab on the sidebar's own
edge); the address (about 400 pt wide; full
width in narrow windows), with the share button inside its right end (hidden while editing);
then a ⋯ button that opens a popup with the page actions (ad blocking,
bookmark, site scripts, page zoom), the per-tab headers-view button and the chat button. Toasts and banners
appear above the pill. With a pointer device the pill and the sidebar sit at 50% opacity until
hovered (the sidebar fades in and out over 0.3 s, the pill over 0.15 s). Since 2026-11-03 the
sidebar's toggle tab shows only in flyout mode; an attached sidebar is shown and hidden with ⌘\.
Two exceptions keep the tabs reachable without a keyboard: narrow windows (the tab opens the
overlaid sidebar) and touch-only devices with an attached sidebar hidden. The host still reports
the pointer near the left edge (`edge_pointer`); the chrome no longer uses it.

Page views are drawn above the chrome's web view, so the chrome cannot simply draw over a page.
Instead it reports the rectangles of the pill, the open actions popup and any toast or banner as
holes (`set_chrome_layout`, window coordinates with a corner radius). The host masks every
content container (and the sheet backdrop) with those holes cut out (`CAShapeLayer`, even-odd)
and the containers report points inside a hole as outside themselves, so touches and pointer
hover there reach the chrome. The strip around the pill is transparent and takes no input: the
page shows and is used there. While the console is open the page ends above it as before. The
pill centres over the page, between the sidebar and any docked panel. ⌘-click on a link opens it
in a new tab after the current one without switching to it (§4). Back in that tab, while it has
no back history of its own, loads the page the link was on (`Tab::back_to`, memory only, used
once; 2026-11-09): WebKit cannot give a new web view another view's history, so this is a load,
after which Back returns to the link's page and Forward is empty.

The same holes carry the other chrome that floats over the page: the find bar (⌘F), attached to
the top of the pill (its hole ends where the pill's begins and has only its top corners rounded,
since overlapping holes are merged into their bounding rectangle); the confirmation ⌘W shows on a
bookmark's tab; and, with the sidebar closed, a tab on the left edge and the sidebar pop-over it
opens (§5b). Find runs a constant script in the page's client world (`find::find_js`) that finds
the matches (case-insensitive, within single text nodes), highlights them with the CSS Custom
Highlight API (every match at 50%, the current one at 100%), scrolls to the current one and
returns its position, "3/12" (C18). Nothing is selected, so the selection menu stays away.
↩ / ⇧↩ in the bar and ⌘G / ⌘⇧G anywhere step through the matches, wrapping (⌘G opens the bar
if it is closed).

Keyboard shortcuts (`snorkle_content::SHORTCUTS`) are key commands vended by the app delegate
(`keyCommands`, added to tao's class at runtime), the last responder in every chain, so they
work whatever has focus, including nothing (sidebar hidden, no page). They are not menu-bar
items any more (as menu items they stopped working, apparently from the menu system's
validation and key-equivalent conflicts, e.g. ⌘I with Format → Italic); the menu bar only loses
the system's Close and Find. Recording a binding is native too: while recording, the app delegate
offers every accepted ⌘ combination and Escape and reports the one pressed. Shortcuts: ⌘T new tab, ⌘⇧T reopen the last
closed tab, ⌘W close tab (a
pin's tab never; a bookmark's tab after a confirmation, removing the bookmark; if the tab has left the
bookmark's page, the confirmation offers Return to Original URL first), ⌘F find, ⌘G /
⌘⇧G next / previous match, ⌘\ sidebar, ⌘⇧\ right panels, ⌘S / ⌘⇧S split side by side / top
and bottom, ⌘D bookmark (on a bookmark's tab: un-bookmark; ⌥⌘D from 2026-11-03 to 2026-11-04,
when ⌘D split the view), ⌘I images,
⌘1–5 sidebar items, ⌘⇧] / ⌘⇧[ next / previous tab (2026-11-08; Chrome's and Safari's keys:
the sidebar's order, wrapping; pins and bookmarks count only where they have a tab, so cycling never
loads one that was not open). The sidebar has two modes (Settings → General, or ⌘-click its toggle; its toggle tab reaches 3 pt
into the sidebar, over the resize edge's line, and in attached mode with the sidebar hidden and a
pointer device it shows only while the pointer is within 100 pt of it: a hover recognizer on the
window's root view reports the pointer near the left edge, since pages are native views the chrome
cannot see the pointer over):
**attached**, a full-height column beside the page that the toggle shows and hides, and
**flyout**, shown as soon as the pointer reaches the toggle and hidden 150 ms after it leaves
the sidebar. ⌘\ (and clicking the toggle) shows or hides an attached sidebar, and
turns a flyout, hidden or showing, into an attached, shown sidebar. The toggle tab is a child of
the sidebar on its right edge, so it always moves with it (a line in attached mode, an arrow in
flyout mode). All state changes go through `setSidebarState`. The chrome always reports two
sidebar holes, the panel (off-screen while hidden; left of the page while attached) and the tab,
so the set of holes keeps its shape and the host animates the mask path with an explicit
`CABasicAnimation` (0.25 s ease-in-out, back-dated by the time since the chrome started its CSS
slide, `set_chrome_layout` `animateSince`, so both move together), together with the page
frames (`UIView.animateWithDuration`). Settings → Keyboard rebinds them: the chrome keeps the user's bindings and
sends them at launch (`set_shortcuts`, validated, clashes refused). The
chrome's own keydown handler uses the same bindings when it has focus; a shortcut arriving twice
within 60 ms is run once. An input the chrome opens from a shortcut (palette, find, chat) first
asks the host to make the chrome's web view first responder (`focus_chrome`).

**Reopening closed tabs** (⌘⇧T, 2026-11-03): the core remembers the committed page of every closed
non-private tab except popup sheets (`Browser::closed`, at most 50, in memory only, most recent
last). ⌘⇧T takes the most recent one whose page is still in history (pages removed from history are
skipped and forgotten) and opens it in a new tab through the navigation policy
(`reopen_closed_tab`). Back/forward history of the closed tab is not restored.

**History** (2026-11-03): the panel lists visits by day and hour, with a search box. Pointing at a
visit replaces its time with an × that removes it; Select turns rows into checkboxes (Shift-click
selects a range) and Remove deletes the selection (`remove_history`, by URL and visit time). The
text of the 100 most recently visited pages is saved (`storage` `page-text/`, `SavedPageText`): 1.5 s
after a non-private page finishes loading, the host runs the constant `ai::PAGE_TEXT_JS` (C17) and
the title and visible text (`innerText`, ≤ 48,000 characters, cleaned) are written if the tab still
shows that page. The text is kept only while the page is in history: removing its last visit or
clearing history deletes it. Rows with saved text show its size beside the address; clicking one
opens a dialog with the text (paragraphs, `textContent` only, search matches marked) and Open
Page; other rows open the page as before. Search (`search_history`) matches titles, addresses and
the saved text (with a snippet). Images are not saved. Settings → Cache does not delete saved text
(it belongs to history).

**Tab and bookmark favicons** (2026-11-04): each tab and bookmark row shows its site's icon, fetched and stored the same way
as pins' (F1: a sandboxed, script-free web view loads `https://<host>/favicon.ico`; the app keeps
only a PNG it redrew itself, `icons-v3/`). The chrome asks `tab_icons` when its tabs' addresses
change; Rust maps each tab to its host from the committed URL and returns stored icons the chrome
lacks, and queues a fetch (once per host per session) only for non-private tabs, so a private
visit is never fetched or written to disk. An `icons` event follows each stored icon. Bundled
icons (Google sites, YouTube) and letter tiles are used as for pins. The chrome is greyscale
through one filter on its root; with a pointer it is lifted (0.3 s) while the sidebar is pointed at,
has focus inside or a drag runs, the same states that keep its contents from fading, so favicons
and emoji show in colour then.

**Swipe between pages** (Settings → General, default on; remembered by the chrome and sent at
launch, `set_swipe_navigation`): WebKit's back/forward swipe on every content view. **Pull to refresh**
(Settings → General, iPadOS, default on; `set_pull_to_refresh`) attaches or removes each page's
`UIRefreshControl`.

**Scroll probe** (debug builds, iPadOS, 2026-11-04): an extra target on each page's scroll pan
recognizer notes every pan in the console when it ends: trackpad/mouse (no touches) or touch, the
gesture's translation, how far the page's scroll view moved, and the modifier keys UIKit reports.
Added to find why trackpad scrolls arrive sideways or go Back (OPEN_QUESTIONS 109).

**Shortcut routes** (2026-11-04): every shortcut the chrome runs is logged in the console (Browser
entries) with its route, `native` (the key command, as the `shortcut` event) or `chrome` (the
chrome's own keydown), and repeats of one press are logged as such. ⌘T while the palette is
already open keeps what was typed.

**Attached URL bar** (⋯ → Attach URL Bar, 2026-11-04; remembered in the chrome's
`localStorage`): the pill becomes a full-width bar along the bottom of the page area (between the
sidebar and any docked right panel). It is no longer a hole: the chrome reports its height as the
content area's `bottom` inset, so pages end above it and text fields at the bottom of pages (chat
boxes) are not covered. Toasts, the find bar, the ⋯ popup and the ⌘W confirmation still float above
it as holes. With the console open, the console sits on the bar and the page ends above both.

**Blocked** (Settings → Blocked, 2026-11-04; `snorkle_core::blocklist`, `blocked.json`): regular
expressions (Rust `regex`: linear time, no backreferences) matched case-insensitively against
`host[:port]/path`, without scheme, user info, query or fragment. A main-frame navigation to a
match is cancelled in the core before it reaches WebKit, from every source (`navigate_trusted`,
`decide_navigation`: typed, links, redirects, script, back/forward, popups, background tabs,
links from other apps); frames inside pages are not affected. The tab shows a `UserBlocked` notice
with the user's message in its middle. Pages that reach a blocked address without a navigation
(a single-page app's `pushState`, a commit that skipped the policy, the list changing while the
page is open) are hidden behind the notice, and the core never shows a view whose committed page
matches (`showable`); Back then really goes back in the page instead of dismissing the notice.
Adding a pattern is immediate (also ⋯ → Block This Site, after a confirmation, which adds
`(^|\.)<registrable domain>(:|/)`). Changing or removing one needs the passage
(`blocklist::UNBLOCK_PASSAGE`) typed out in full each time: the chrome shows it greyed out, each
letter turns solid as it is typed, only single-character insertions at the end and deletions of the
last character are accepted (no paste, drop, dictation or predictive text), and the Edit/Remove
URL button is enabled only when the text matches. The core refuses the change unless the passage
it receives is exact. This is a commitment device, not a security boundary (the file is in the
app's container). The message is free to change.

Bookmarking a tab (⌘D, the ⋯ menu, or dragging it into the bookmarks) moves it into the
bookmark: the tab becomes that bookmark's tab (`Browser::bind_launcher`) instead of a second tab
appearing when the bookmark is opened. Un-bookmarking makes it an ordinary tab again.

**Page scale** (⋯ menu: −, the level, +; the level resets to 100%) is per tab. The core keeps it
on the tab (`Tab::zoom`, one of `tabs::ZOOM_LEVELS`, 50–150%), saves it with the session
(`SessionTab::zoom`, snapped to a level when read back; private tabs are never saved) and emits
`Effect::SetZoom`, both when it changes and right after a web view is created for the tab
(restore, discard and reload). The host lays the web view out at its pane's size divided by the
scale and draws it with a matching scale transform (`place_view`), so at 80% the page gets a
viewport 1.25 times wider (more CSS pixels, as on a larger screen) and everything is drawn
smaller; sites that serve an enlarged layout to a narrow iPad window get their wider layout. (Until
2026-11-03 this was `WKWebView.pageZoom`, which scaled text and boxes inside the same viewport.)

**Pins and bookmarks:** opening one returns to its tab as it was left (since 2026-11-03; before,
it always loaded the item's page again). Right-click or ⌘-click on one opens its menu: Copy URL (the page its tab shows, or the saved
address if it has no tab), Edit… and Return to Original URL (its tab loads the saved address);
tabs get the same menu on right-click without Return to Original URL. **Edit** (2026-11-09) is one
dialog for tabs, pins and bookmarks: title, URL and an emoji shown instead of the favicon (typed, or
chosen from a grid of common emoji). For a pin or bookmark the URL is read like the address bar
(addresses only; place is kept, its tab follows via `Browser::rekey_launcher`, its offline copy is
dropped when the URL changes) and the emoji is stored in `pin-emoji.json` for pins and bookmarks
alike (`storage::clean_emoji`; kept when a pin moves into the bookmarks, dropped with the item). For
a tab the title is its own name (`Tab::name`, shown instead of the page's title; empty follows the
page), the emoji `Tab::emoji` (both saved with the session, `Browser::set_tab_label`), and a changed
URL loads in the tab like the address bar. Bookmark rows are one line, like tabs. **Pins:** up to 12
(three rows of four); their menu adds Return to Bookmark (moves it to the top of the bookmarks). **Appearance**
(Settings → General: System, Light, Dark) sets the chrome's colours (`data-theme`) and the
window's `overrideUserInterfaceStyle`, so system UI and content views without dark pages mode
follow it.

**Offline copies:** when a pinned or bookmarked page finishes loading, the host saves it as a web
archive (`createWebArchiveData`), at most every 10 minutes per page, under the pin's or
bookmark's URL (also when the site redirected: the tab remembers which item it was opened from,
`Tab::landing`) or under the page's own URL if that is pinned or bookmarked. Copies live in
`offline/` (≤ 25 MB each, ≤ 400 MB in all, oldest deleted first; dropped when the item is
removed; Settings → Cache deletes them). When that URL later fails with a network error, the core
accepts the copy once (`Browser::use_offline_copy`) and the host loads it with
`loadData:MIMEType:…baseURL:` and JavaScript off for that navigation; the address bar shows an
"Offline copy" pill, the copy is not written to history, and the next load replaces it.

**Links sent to Snorkle** (2026-11-09): the app declares its own URL schemes besides http/https
(`Info.plist`, `Info.ios.plist`; `input::APP_SCHEMES`): `snorkle-https://…` / `snorkle-http://…`
(the scheme swapped, as Edge's `microsoft-edge-https://`), `snorkle://open-url?url=<encoded>` (as
Firefox's) and `snorkle://host/path` (https). They arrive like links from other apps
(`RunEvent::Opened`, `open_links`); `input::from_app_link` turns them into the page they stand for
(http/https, a host, no credentials, ≤ 8 KB) or refuses them, and `open_from_other_app` applies the
navigation policy. They are in `policy::INTERNAL_SCHEMES`, so pages inside Snorkle cannot use them.

**Links to apps** (2026-11-09, iPadOS): WebKit hands a cross-site main-frame navigation to an
installed app that claims the address (universal links) when the navigation delegate answers plain
`Allow`, which is why Google Docs and Sheets jumped to their apps. By default every allowed
navigation is now answered with WebKit's `_WKNavigationActionPolicyAllowWithoutTryingAppLink`
(`Allow + 2`, an undocumented value of the public enum, not a private selector; Chrome for iOS uses
it), so pages stay in the browser. iOS offers no way to ask whether an app claims an address without
opening it, so the choice is offered the other way round: ⋯ → Open in App (`open_in_app`) asks UIKit
to open the tab's committed URL (from the core; http/https only) with
`UIApplicationOpenURLOptionUniversalLinksOnly`, which opens nothing if no app claims it (a toast
says so). Settings → General → Links to apps → Automatic restores WebKit's behaviour
(`set_open_app_links`, remembered by the chrome and sent at launch). Links with an app's own scheme
(`googledocs:`) still ask first ("Open App" / "Stay in Browser"), and are blocked without a tap.

**Image menu:** right-click, ⌘-click (not on a linked image) or a 0.5 s press on an image in a
page offers Copy Image, Share…, Download… and Copy Address (C21). Copy, Share and Download fetch
the image natively (ephemeral session, no cookies, `image/*` only, ≤ 25 MB): Copy puts it on the
pasteboard, Share opens the share sheet with it as a file, Download opens the Files export
picker. iOS's own long-press callout is turned off for images.

The copy button (beside it, 2026-11-09) puts the page's committed URL on the clipboard. The share
button (inside the address field, at its right end) opens the system share sheet for the
page's committed URL, as a popover from the button (so Instapaper and other share extensions
appear). Selecting text on a page shows a small menu above it, cut out of the page: Google This
(a new tab), Ask AI (opens the chat with the quote as a draft) and Define (the system
dictionary). A watcher in the page's client world reports where the selection is (C19); the text
is read when the menu appears (C20).

## 5h. Downloads (2026-11-03)

A main-frame response WebKit cannot show, or one the server marks `Content-Disposition:
attachment`, and a click on a link with a `download` attribute become a `WKDownload` when
`ContentEvents::may_download` allows it (§3). From a frame inside the page, WebKit's decision is
held and the chrome asks "… wants to download a file" with Allow Download / Block above the URL
bar (one prompt at a time; no answer within 60 s blocks it). All downloaded files together may
take at most 500 MB: past it new downloads are refused and a running one is stopped. The
`downloads/` folder is excluded from device backups. The download
delegate (`snorkle-content/src/webkit/downloads.rs`) asks the app where to write: the app picks
`downloads/<id>/<name>` in the profile folder (`storage::Store::new_download`; the suggested name is
reduced to one cleaned path component, `clean_download_name`). Redirects are followed only to URLs
the policy accepts; authentication gets the system's default handling. Progress is read twice a
second (`NSProgress`) and sent to the chrome (`downloads` event); the menu button shows a pulsing
dot and the menu item a count while downloads run, and a toast says when one starts and ends.

Menu → Downloads lists them, newest first: progress while running (Cancel), and for finished files
Save to Files… (iPadOS, the Files export picker) or Save… (macOS, a save panel; the copy keeps the
quarantine mark), Share… and × (deletes the file). Clear deletes every finished or failed one.
Finished downloads of non-private tabs are recorded in `downloads.json` and kept until deleted;
failed and cancelled ones keep a record without a file. Private-tab downloads are listed for the
session only and deleted (cancelled if running) when the private session ends. Folders without a
record (interrupted downloads, earlier private ones) are deleted when the app starts. A
`target=_blank` link that turns out to be a download leaves its new tab empty.

## 5e. Docked panels

Native page views sit above the chrome's webview, so the chrome cannot draw over a page except
in holes (§5f). Panels that should leave the page visible dock beside it instead: the chrome
reports `right` and `bottom` insets and the host shrinks the content area. Site scripts dock on
the right, full height; otherwise the headers outline and the page chat do, the outline on top
and the chat below when both are open (in narrow windows site scripts and chat overlay and hide
the page). The right panels' width is dragged at their left edge (260–640 pt, at most 60% of the
area, remembered); ⌘⇧\ hides them without turning them off and shows them again. The console
docks at the bottom. A ring around the `{ }` row (and the ⋯ button while its popup is closed) marks
pages an enabled site script applies to. When the content area grows (a panel closes), the
host checks after 0.35 s whether WebKit left the page drawn narrower than the view (a zoom scale
kept from the narrower layout) and zooms it to fill the width.

## 5g. Page chat

The chat button beside the headers button opens a panel docked right of the page: a model menu
(the models of every connected provider, as listed by Settings → AI → Test; it sets the "Ask
Selection" use), the conversation,
and an estimate of what it has cost so far (provider-reported tokens × published prices,
`ai::estimate_cost_micros`; "≥" when a model has no known price). Each message is sent with the
whole conversation (at most 40 messages, each ≤ 8,000 chars). At that moment the host runs a
constant script in the page's client world that returns the title, visible text (`innerText`,
≤ 48,000 chars) and selection (C17); Rust adds the tab's committed URL from the core and sends it
all as data in the system prompt (`ai::chat_request`). Conversations are kept in the chrome's
memory only, one per tab, and restart when the tab's page changes. Usage is recorded like other
AI requests (Settings → AI → Usage).

The panel shows no explanatory text: a "+" beside its title starts a new chat, the model is a
link at the bottom (a menu of every connected provider's models; it sets the Ask Selection use)
beside the running cost, and Archive lists earlier chats. Chats begun in non-private tabs are
stored natively (`storage::ChatRecord`, one file per chat under `chats/`, at most 60, least
recently used dropped): the messages, and the context the first message used (the page's URL,
title, visible text and the selection at that moment). Continuing an archived chat, from any tab,
sends that stored context instead of reading the current page; nothing typed in a private tab is
stored. Chats can be deleted from the archive.

**Define** (2026-11-08; inline since 2026-11-09): the start page's Define row asks `Define “word”.`;
Rust sends `ai::define_request` (a dictionary system prompt and the word, validated again by
`ai::clean_word`: letters, with apostrophes or hyphens inside, at most 64) and reads no page. The
first request also asks the model to answer a likely misspelling with only `SPELLING: <word>`
(`ai::parse_spelling`); that answer is returned as a suggestion and nothing is archived, and the card
above the start page's input asks "Did you mean …?" with a button to define either spelling, sent
again with `exact` (no spelling check). The definition is shown in that card, not in the chat panel,
and becomes the tab's chat: "Ask a follow-up" opens the panel to continue it. It is archived like other chats
(`ChatRecord::word`, with no URL or page text; the archive shows it as "Definition"), except from
private tabs. The usage log names it "definition", without the word.

## 5d. In-app console

The Menu → Console panel is docked under the content area (the chrome reports a `bottom`
inset, so pages stay visible above it). It shows, in memory only:

- **Browser** entries: every navigation policy decision, every effect (views created, shown,
  hidden, loads, popups), and the chrome's own toasts and script errors. These are what
  `tauri dev` printed to the terminal, now visible on the device.
- **Page** entries: `console.debug/log/info/warn/error`, uncaught errors and unhandled promise
  rejections from every frame, attributed to the frame's origin. A constant hook
  (`snorkle_core::console::PAGE_HOOK_JS`) is installed at document start in the page world while
  recording is on, and posts to the `snorkleConsole` message handler (channel C13).

Recording is on by default in debug builds and off in release builds, and resets every
launch. Turning it on applies to pages from their next load. Filters: all, pages, browser,
errors/warnings, this tab only. Copy puts the visible entries on the clipboard.

## 5b. Chrome layout

Tab rows are one line, the page's title (2026-11-09). A row's buttons (close; Save for Later on
tabs) and the × of bookmarks and domain headings appear only while the row is pointed at (on
touch-only devices: on the active tab or bookmark, and always on domain headings).
**Save for Later** (2026-11-09; `snorkle_core::later`, `later/`): a tab's tray button snapshots what
its view shows (`takeSnapshotWithConfiguration`, visible bounds, 320 pt wide, JPEG 0.6, drawn by the
app from its own web view) in light mode, with images and in full colour: 1.5 s before the snapshot
the view modes are taken off the loaded page only (`viewmodes::plain_js`) and its view drawn light
(`force_plain`), so the page briefly shows that way before the tab closes (`saved::save`; put back if
saving fails). The tab's mode settings and the scripts installed for later loads are not touched. It saves the page as an **archive** that works offline (below), stores the
committed page, title and thumbnail (≤ 200 pages, ≤ 400 KB each, JPEG magic checked), and closes
the tab; private tabs and empty tabs have no such button. The chrome shows how far it is in a toast
with a percentage and a bar (`later_progress` events; also while adding pages). A tab without a live view is saved without
an archive, and one WebKit cannot draw without a thumbnail (the chrome shows the site's letter).
While any are saved, the same icon sits beside the menu and swaps the sidebar's contents for the
saved pages (thumbnail, title underneath, the number of pages when more than one). A click on the
thumbnail opens the saved copy in a new tab (it stays in the list; without an archive it opens the
live page). On hover: Recover (the live page in a new tab), "+" (add pages), Bookmark (added at the
end of the bookmarks) and Delete; Recover, Bookmark and Delete remove the page from the list.

*Archives* (the method of laffan/thedailyprophet, without its element removal, as WebKit web
archives; `snorkle_core::webarchive`, `src-tauri/src/saved.rs`): the page's HTML is kept as the
server sends it and every resource the page loaded under its original URL. Capture reads, from the
app's own web view: WebKit's archive of the page (`createWebArchiveData`, parsed in Rust: the
resources WebKit holds, cross-origin ones included), then a constant script run with
`callAsyncJavaScript` in the page's client world (C22, `later::CAPTURE_START_JS`) that fetches the
HTML again with the page's session (`cache: force-cache`) and lists what the page loaded (resource
timeline, element sources, `srcset`, stylesheets and their `url()`s, inline styles); the resources
WebKit did not have are fetched in the client world (same-origin with cookies, others without),
eight at a time by index, and handed over as base64 in pieces of about 8 MB. Everything is checked
in Rust (http(s) URLs without fragments or credentials, MIME types and charsets as plain tokens,
15 MB per resource, 80 MB and 1,500 resources per saved page, 90 s per capture, 1 GB for all
archives with the oldest dropped first) and written under app-chosen names in `later/<id>/`. If the
HTML cannot be fetched again, WebKit's copy of the live document is kept instead and shown with
JavaScript off. Reading builds one web archive per page (binary property list, main resource and
subresources) and loads it with `loadData:MIMEType:@"application/x-webarchive"` under the page's
own URL with scripts on; WebKit answers the page's requests (scripts, styles, fonts, images,
`fetch`, XHR) from the archive (`DocumentLoader::scheduleArchiveLoad`), so no URL is rewritten and
no URL scheme handler exists. A saved copy has its own tab (`Tab::saved`, `Browser::show_saved_copy`), never an
ordinary one: it is not listed with the tabs (its card in Save for Later is shown as selected, and
opening the card again returns to it), not saved in the session or reopened by ⌘⇧T, not written to
history, and closed when its item leaves the list; its address bar shows a "Saved copy" pill. A
link to another page of the archive is cancelled and that page loaded from the archive
(`Effect::OpenSavedPage`); Reload reloads from the archive; any other link or form, and an address
typed in its bar, opens in a new ordinary tab through the navigation policy (as a ⌘-click would);
script navigations and redirects out of the archive go nowhere. "+" opens the saved copy with scripts off and
a bar above the pill: each link tapped on the page (same host, at most 30 pages) is chosen or
unchosen in the core instead of navigating; Add loads each chosen page live in that tab, captures
it the same way into the same archive (resources shared), and shows the saved copy again.
Tabs and bookmarks are listed in a resizable sidebar along the left edge (200–420 pt, at most half the window). Dragging a tab into the bookmarks bar bookmarks it there, bookmarks reorder by dragging, dragging one tab onto the middle of another pairs them in split view, onto the top or bottom 30% of a row (or below the last) moves it there (2026-11-09, `Browser::move_tab`; the session keeps the order), and dragging a tab out of its pair unsplits it. A tab's close button appears on hover (on the active tab on touch-only devices); so does a pin's unpin button. Each pin and bookmark has its own tab (`Tab::launcher`, saved in the session): opening it returns to that tab, which is not listed with the other tabs, and the pin or bookmark is shown as selected; removing the pin or bookmark lists the tab again. New Tab (⌘T, or the menu; since 2026-11-08) is a real tab: an empty tab shows the start page on the chrome's surface (no web view exists until it navigates), with the new-tab input a little above the middle of its pane (its middle at 40% of the pane's height; in split view, the focused half) and Arc-style rows under it: typing fuzzy-matches open tabs, pins, bookmarks and then history (the latest 500 visits, one row per page, loaded when the start page opens), and the first rows go to, search for, or, for a single word, define what was typed. Go to, Search and history rows load in that tab; choosing another tab, pin or bookmark switches to it and closes the empty tab (unless it is half of a split), as Chrome's omnibox does. Escape clears the input. **Define** (single words only, `ai::clean_word`) shows the chat's model's dictionary-style definition in a card above the input (2026-11-09; before, it opened the chat panel), after asking which word was meant if the model thinks it is misspelt (§5g). **Each launch** restores the saved tabs unloaded and greets the user with a new start-page tab after the saved active tab (`Browser::restore_session_with_start_tab`); returning from the background is not a launch and adds nothing, and a cold launch from a link in another app shows that link instead. Empty tabs are never saved, so launches do not pile them up. (Until 2026-11-08 New Tab opened a palette over the hidden content area and no tab existed until a row was chosen.) The bottom bar holds the menu (New Tab, New Private Tab, History, Downloads, Console, Settings), colour, and show images and video. The chrome itself is black and white (grey palette, plus `filter: grayscale(1)` on its root for favicons and emoji). Settings deletes cache, cookies, site data, or logins (cookies and site data) from every profile's store. In windows at least 700 pt wide the sidebar is
docked: the chrome reports `left = sidebar width` and the native content container starts to its
right. In narrower windows it slides over the content area, and the content view is hidden
while it is open (`overlay`), so a page can never draw over the sidebar or the toolbar. Both
insets are clamped in Rust to at most half the window. Both sidebars' resize edges are a 3 pt line
inside the panel that lights up on hover. With the sidebar closed, a small tab stays on the left
edge; pointing at it (or tapping it, without a pointer) shows the sidebar as a pop-over inset
20 pt from the top and bottom, rounded on the right, over the page (a hole); it goes away 0.28 s
after the pointer leaves it, or on a tap outside or Escape. In narrow windows the tab opens the
overlaid sidebar instead.

## 6. Credentials

The only credentials the app stores are the user's own AI API keys (Settings → AI): iOS Keychain
generic passwords, service `app.snorkle.ai-keys`, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`
(`snorkle-content/src/webkit/keychain.rs`). They are read only to send a request to the provider and
are never returned to the chrome. Site-script requests carry the site's domain, its site script
and the user's prompt; Detect Headers sends short snippets of headline-like text; page chat sends
the page's URL, title, visible text and selection with the conversation (§5g). The model's
CSS/JS lands in the editor and runs only if the user saves it; chat replies are only displayed. The app stores no passwords, tokens or cookies otherwise. Cookies live in WebKit's data stores.
Password AutoFill in WKWebView uses the system (iCloud Keychain / Passwords app). Any future
credential feature must use the iOS Keychain (`kSecClassGenericPassword` /
`kSecClassInternetPassword`, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`) and needs a
security review. `config_hardening.rs` and `storage.rs` contain no credential fields by design.

## 7. Default-browser entitlement (`com.apple.developer.web-browser`)

The design assumes it is granted. Apple's documentation lists what it enables: full-domain script
access, Service Workers in WKWebView, eligibility as default browser, and "Add to Home Screen"
via the share sheet.

| Without the entitlement | Effect on Snorkle |
|---|---|
| Not selectable as default browser | Links from other apps open in Safari. (With it, iOS sends them to Snorkle as `RunEvent::Opened`; each http/https URL without credentials opens in a new tab through the normal policy, `Browser::open_from_other_app`.) |
| No Service Workers in WKWebView (unless App-Bound Domains, which we do not use) | PWAs, offline sites and push-dependent sites degrade. A WebKit bug report says SharedWorker pages also fail to load without the entitlement since iOS 16.1 (unverified for 26/27). |
| "Full script access" | Not needed today: we inject no scripts and do not opt into App-Bound Domains. |
| Entitlement key present but not provisioned | The build fails to sign. Build with `--without-web-browser` (`scripts/ios-entitlements.sh` then leaves the key out of the generated project). |
| Entitlement granted but not in the generated project | The app signs and runs but iPadOS never lists it as a browser. Tauri merges `Info.ios.plist` on every build but never copies entitlements, and `tauri ios init` writes the file empty; `scripts/ios-entitlements.sh` (run by `npm run ios:build`) copies them, and the build script reports what the signed app carries. |

Apple's rules that the code already follows: http and https schemes declared (`Info.ios.plist`);
no `UIWebView`; a URL field on launch; none of the rejected `Info.plist` keys (Photo Library
read, Always location, HomeKit, Bluetooth-always, Health); `config_hardening.rs` checks this.

## 8. OS policy

`MINIMUM = 26.0` is both the deployment target (`bundle.iOS.minimumSystemVersion`) and a runtime
gate: below it, no web view is ever created and the chrome shows an update screen. Within 26.x
and 27.x, versions older than `LATEST_KNOWN_PATCH` get a persistent banner. Update
`crates/snorkle-core/src/os.rs` with every Apple security release.

## 9. Code that requires human review

Every file whose header says `SECURITY-REVIEW` needs review, and so does every change to
`capabilities/`, `tauri.conf.json`, `Info.ios.plist`, `ios/Entitlements.plist`, `build.rs` and
`command_list.rs`. See `docs/SECURITY_REVIEW.md`.
