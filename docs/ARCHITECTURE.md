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
| Memory warning, did-enter-background | libdispatch's memory-pressure source (warning or critical; 2026-11-23, `macos/pressure.rs`); on resigning active the core ticks and the session is written; on quit (`RunEvent::Exit`) the session is written synchronously (`Shared::flush_blocking`) |
| OS version gate (`os::check`) | Safari's version (`/Applications/Safari.app`), since WebKit updates with Safari on older macOS too (`os::check_macos_safari`, minimum 26.0, newest known 27.2); it is also the version in the user agent |
| `overrideUserInterfaceStyle` | `NSAppearance` (window; content views in dark pages mode) |
| Keychain, `WhenUnlockedThisDeviceOnly` | The data-protection keychain with the same class when the build is signed with an application identifier; otherwise the login keychain (`webkit/keychain.rs`) |

**Window.** No title bar (`titleBarStyle: Overlay`, hidden title): pages run the full height. The
close, minimise and zoom buttons (`trafficLightPosition` 16, 20) are hidden except while the pointer
is in the window's top-left 96 × 44 pt (`keys::BUTTON_CORNER`, 2026-11-23; the mouse monitor that
reports the edge pointer shows and hides them), where they sit over the top of the sidebar. Until
2026-11-23 they sat in a 40 pt strip at the top of the docked sidebar. A 12 pt strip there
(`#titlebar`, invisible) is a `data-tauri-drag-region`: it drags the window and a double-click
zooms it, through Tauri's own drag script and two window permissions granted to the chrome
(`core:window:allow-start-dragging`, `core:window:allow-internal-toggle-maximize`). With the sidebar
hidden there is no drag area; ⌘\ shows it. ⌘Q quits (a shortcut taken by the key monitor before a
page could swallow it, then `terminate:` as the menu's Quit; macOS only). The chrome detects macOS
by `navigator.maxTouchPoints == 0` with a Macintosh user agent (an iPad reports touch points, also
in desktop mode). On iPadOS the status bar is hidden (2026-11-23; tao's root view controller,
`setPrefersStatusBarHidden:`), so the sidebar and pages reach the top of the window. iPadOS 26
drags (and un-fullscreens) a window by its top ~30 pt, so since 2026-11-24 the notebook's quote button
does not sit there on iPadOS (the sidebar's rows started 30 pt down too, until the owner asked for
no gap above the pins, 2026-11-24: the first row is now as far from the top as from the left, 10 pt,
on every platform, the macOS title-bar strip lying over the top 8 pt), and the right panels reach the top but
the notebook's bar (when it is the top one, `.dock-first`) keeps its buttons below the strip, which
is filled with the notebook's own background (`IPAD_TOP_STRIP`, `html.ipados`; until the fourth
round the panels themselves started 30 pt down, leaving a dark band). Pages still reach the top. A
drag of the notebook's quote button begun there went to the system, which shrank the window out of
full screen (owner report 2026-11-24).

**Data at rest.** macOS has no `NSFileProtectionComplete`; the profile and website data are in the
user's Library (`~/Library/Application Support/<identifier>` and WebKit's stores), protected by
the user account and FileVault. The app is not sandboxed; WebKit sandboxes its own content
processes as for Safari.

## 2. Trust boundaries

| # | Boundary | Direction | Mechanism | Enforced by |
|---|---|---|---|---|
| B1 | Web content → app process | page → native | **None by design.** Content `WKWebView`s get no `WKScriptMessageHandler` or `WKURLSchemeHandler`, and are not Tauri webviews, so no Tauri IPC script, invoke key or `ipc:` scheme exists in them. The only data that crosses is WebKit-generated metadata in delegate callbacks (URL, frame, navigation type, NSError, title via KVO). The only injected code is the user's own scripts and the view-mode styles (black and white, hidden images), in the isolated client world with no way to send messages back (§5c). Exception, only while console recording is on: a console hook in the page world posts page console output to one message handler (§5d, SECURITY_REVIEW C13). Exception, only when the user sends a page-chat message: the result of one constant script (title, visible text, selection) is read back (§5g, C17). Exception, only when the user saves a page for later or adds pages to a saved one: constant capture code reads back the page's HTML, the URLs it loaded and those resources (§5b, C22; on AI chat sites the conversation's turns as node trees, C22b). Exception, the Notebook (§5i, C23): a client-world watcher reports ⌥-click and ⌥-drag geometry, and the sentence under the quote button is read back when it is let go. Exception, pins keep their page (§5k, C25): in a pin's tab only, a client-world watcher stops real clicks on links to other pages and reports their address (or that a real click happened). | `CONTENT_VIEW_SPEC` (compile-time assert in `ios/mod.rs`), `tests/content_view_spec.rs`, `src-tauri/tests/channel_tripwire.rs`, on-device `tests/device/ipc-probe.html` |
| B2 | Chrome UI → Rust commands | chrome → native | Tauri IPC with app ACL manifest (`build.rs`): every app command and core plugin command is checked against `capabilities/chrome.json`, which grants every app command in `command_list.rs` and `event:listen/unlisten` to webview label `main`, local origin only. No `remote` URLs, no window-label grants, `dynamic-acl` feature compiled out. | Tauri runtime authority; `src-tauri/tests/ipc_isolation.rs`, `config_hardening.rs` |
| B3 | Rust → chrome | native → chrome | `emit_to("main", "state" \| "toast" \| "progress" …)` (`progress`: a tab's load progress alone, 2026-11-23). Payload is core state. Page-derived strings (titles, hosts) are sanitised in Rust (`text::sanitize_page_text`: bidi and control characters removed, length capped) and rendered with `textContent` only. | `text.rs` tests, `channel_tripwire.rs` (no `innerHTML` and similar) |
| B4 | Content pixels ↔ chrome pixels | visual | Content containers sit in their own UIViews inside the content area (between the status bar, the sidebar and any docked panel; down to the bottom of the window), `clipsToBounds`. The URL pill, its popup and toasts float over the content area as **holes**: every container is masked there and does not take touches there, so the chrome below shows through and a page can never draw over or under them (§5f). Element fullscreen is disabled. Interstitials, prompts and errors are drawn by the chrome while the content view is hidden, never inside page content. Other chrome overlays hide content (`set_chrome_layout(overlay)`). | `ios/mod.rs` (`apply_mask`), `ios/keys.rs` (`pointInside`), `snorkle_content::sanitize_holes`, `Browser::sync_visibility`, `browser_invariants.rs` |
| B5 | App ↔ disk | native → storage | JSON files in the app container, written atomically off the main thread. Only http(s) URLs are stored, and only from non-private tabs. Data protection `NSFileProtectionComplete` (entitlement). | `storage/tests.rs`, `browser_invariants.rs::private_tabs_never_reach_storage`, fuzz `browser_events` |
| B6 | App ↔ network | content → internet | WebKit for pages. The app itself makes no telemetry or update requests; its own requests are favicon fetches for pins (sandboxed web view) and, only when the user has connected an AI account, HTTPS calls to `api.openai.com` / `api.anthropic.com` (`snorkle_core::ai::is_allowed_url`, ephemeral `NSURLSession`): site-script drafts, page summaries and page chat (which send the page's text, §5g, §5j). ATS stays on for the app; `NSAllowsArbitraryLoadsInWebContent` is set only so HTTP can load after explicit user consent. TLS: system validation, `PerformDefaultHandling` for every auth challenge, deprecated TLS refused. | `delegates.rs`, `config_hardening.rs` |
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
  Background views idle for 10 min are discarded. On a Mac (2026-11-23, `MemoryBudget::for_mac`):
  <8 GB: 6, <16 GB: 12, <32 GB: 20, otherwise 30, and 30 min, with discards on memory pressure. A memory warning leaves only the active view.
  These are **targets, not measured values** (see `docs/PERFORMANCE.md`).
- Heavy work stays off the main thread: blocklist parsing and rule JSON on a background thread;
  WebKit compiles the rule list in its own process; disk writes on a storage thread with
  session-write coalescing; chrome state updates coalesce progress to 5% steps.

## 5a. Content blocking

- **Lists:** EasyList and EasyPrivacy, bundled (`src-tauri/resources/filters`). They are
  converted by Brave's `adblock` crate (`snorkle_core::blocking`) into WebKit content-rule-list
  JSON: about 127k rules, split into 3 lists of network rules (at most 50k each) and 1 list of
  element hiding (2026-11-23). Every list repeats the 1.4k exception rules, because WebKit applies
  `ignore-previous-rules` only within its own list, except that the hiding list leaves out the
  converter's exception for every first-party document (`ignore_previous_fp_documents`, meant to
  follow the network rules): WebKit takes a page's hiding rules from its document load, so with
  that exception after them no element was ever hidden (until 2026-11-23). Generic hiding (the
  13,636 selectors for every site) applies to the page, not to frames in it (`load-context:
  top-frame`): measured in WebKitGTK, a page with 40 frames loaded in 1,110 ms with them in every
  frame and in 239 ms this way (233 ms without lists).
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
- **View modes** (`snorkle_core::viewmodes`): desaturate, hide media and dark. Each is on or
  off app-wide; a long press on its button offers "Disable for this tab", which exempts that
  tab (popups inherit their opener's exemptions, since they share its content controller). The
  host compiles the injected scripts once for the app-wide modes and once per exempted tab. (The
  headers view, a per-tab outline mode with Detect Headers, was replaced on 2026-11-16 by the
  summary view, §5j, which installs nothing in pages.) The modes are constant styles installed as
  user scripts, plus one `evaluateJavaScript` to update already-loaded pages.
  **Dark** (off by default; moon button; remembered across launches) makes content web views prefer a dark appearance
  (`overrideUserInterfaceStyle`), so sites with their own dark theme use it (without the mode a content view is
  always light over white, whatever the app's appearance; 2026-11-15); a constant detector
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
  while loading. Sites get their own dark theme first (2026-11-15): when the mode is turned on for
  a loaded page, the first check waits (≤ 0.6 s) for `prefers-color-scheme: dark` to reach the
  page, and the page is checked again when the preference changes, when the root's or body's
  `class`, `style` or theme attributes change (a site applying its theme from a script) and when
  their colour transitions end; `<meta name="color-scheme">` counts as opting in. Images, video and embeds are never shown inverted and are dimmed to 60% until
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
  at most twice its area that contains it, else the video) and the gray box goes on that frame,
  revealed while pointed at; media inside it get no filter of their own. Web-component players
  (`<nyt-betamax>`, Mux, Media Chrome, Vidstack, lite-youtube/vimeo) are media themselves. Open
  shadow roots up to four deep (where the page's style cannot reach; NYT's video feed is drawn
  entirely in them) get the same rules as an adopted constructed stylesheet
  (`viewmodes::shadow_no_media_css`, removed when the mode is turned off), video frames are marked in
  whichever tree they are (skipping `display: contents` wrappers), frames and images there are
  revealed by pointer position (the tracker cannot see into shadow trees), and videos there are
  paused when they start unless pointed at (their `play` events do not leave the shadow tree, so
  each root is listened to).
  Inverted dark pages keep using dark mode's media box, which also covers the players.
- **Night vision** (2026-11-17; Settings → General → Appearance, a checkbox, Menu → Turn On/Off
  Night Vision since 2026-11-24 (before, an eye button at the sidebar's foot), and ⌘⇧R since
  2026-11-23): the browser and every
  page drawn in shades of red only, no blue or green light, for keeping dark-adapted eyes adapted.
  It is `viewmodes::Kind::Night`, stored with the launch modes and set through `set_launch_mode`,
  so a launch's first page already has it; the chrome offers no per-tab exemption. Pages get one
  more constant style (`NIGHT_CSS`, main frame, document start): the root's `::before` is a
  full-viewport `backdrop-filter: grayscale(1)` and its `::after` pure red with
  `mix-blend-mode: multiply`, both at the top z-index, so each pixel's luminance lands in the red
  channel and green and blue are zero (whatever escapes the grey layer is still multiplied). Only
  filter functions and a blend mode: SVG `url()` filters leave WebKit's accelerated path and
  `backdrop-filter` refuses them. A root `filter` would apply over the root's pseudo-elements, so
  with night vision the root's filter is cleared and dark pages' inversion moves into the grey
  layer (`invert(1) hue-rotate(180deg) grayscale(1)`), which draws the same picture; a dark page
  with a transparent root is given a black one (the red layer would otherwise paint the
  transparent canvas); top-layer elements (`:modal`, `:popover-open`, `:fullscreen`) get a red
  multiply of their own. The content views' native canvas is pure red (light) or black (dark)
  while it is on (`set_light` / `set_dark`). The chrome draws the same two layers over its own
  document (`ui/style/`, `applyNightVisionChrome`), keeping a copy of the setting so it is red
  from its first frame. Save for Later's plain preview leaves it on. Native UI (menus, sheets, the
  keyboard, selection handles, full-screen video) is not covered. OPEN_QUESTIONS: Night vision.
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
  ends a pair; closing the focused half (⌘W) shows the other half full width (2026-11-19;
  before, the tab after it in the list). The sidebar shows each pair as one row with a half per tab; dragging a half out
  of the row unsplits it. Pairs are not saved in the session. A tap in a pane focuses it.
  On iPadOS a tap or click in a page also makes its web view first responder if it is not
  (2026-11-10, `gestures::TapFocus`): WebKit does this itself for a touch tap but not for a
  trackpad or mouse click, so after the chrome had keyboard focus (start page, find, chat) a page
  that focuses its editor from its own click handler (Google Docs' hidden text target) got no
  input session and typing went nowhere until the page was touched. (macOS: AppKit makes the
  clicked web view first responder.)

## 5f. URL bar

The address bar is a pill floating over the bottom of the page (pages run the full height of
the window): back, forward and reload on its left (the sidebar's toggle is a tab on the sidebar's own
edge); the address (about 400 pt wide; full
width in narrow windows), with the share button inside its left end and the copy button inside
its right end (both hidden while editing, and when the field is too narrow for them beside the
address);
then the AI button (a sparkle: the summary and the chat, §5j, §5g), the Notebook's (§5i) and,
last, a ⋯ button that opens a popup with the page actions (ad blocking, bookmark, site scripts,
page zoom); in that order since 2026-11-23. The share and copy buttons show only while the pill is
pointed at (always on touch-only devices), and an empty address reads "search / url". Nothing in
the chrome casts a shadow (2026-11-23); floating parts keep a 1 pt border.
The confirmation for closing a site's group of tabs (its ×) or deleting a saved page sits beside
that row, just right of the sidebar, pointing at it, when the sidebar shows (2026-11-15; a flyout
stays open meanwhile); otherwise above the pill. It is a hole either way. Toasts and banners
appear above the pill. With a pointer device the pill and the sidebar sit at 50% opacity until
hovered (the sidebar fades in and out over 0.3 s, the pill over 0.15 s). Since 2026-11-03 the
sidebar's toggle tab shows only in flyout mode; an attached sidebar is shown and hidden with ⌘\.
Two exceptions keep the tabs reachable without a keyboard: narrow windows (the tab opens the
overlaid sidebar) and touch-only devices with an attached sidebar hidden. The host still reports
the pointer near the left edge (`edge_pointer`); the chrome no longer uses it.

"Hovered", for the sidebar, is `body.sb-live` (2026-11-24), not CSS `:hover`: the chrome's own
pointer events set it (pointer over the sidebar or not), and it is cleared when the pointer leaves
the sidebar, the chrome's web view or the app, and whenever the host reports that the pointer is
not over the chrome (`chrome_pointer`, a boolean sent when it changes: on macOS from the window's
mouse monitor, a hit test of the content root on each move, `false` when the pointer leaves the
window; on iPadOS from the root view's hover recognizer, a hit test for a page container, `false`
when hovering ends). Before, the sidebar could stay opaque, its icons in colour, wherever the
pointer was (owner report 2026-11-24): `:hover` is WebKit's idea of where the pointer last was in
the chrome, and nothing guaranteed it was told when the pointer went to a page. Keyboard focus keeps
the sidebar opaque only as `:focus-visible` (a field being typed in, or keyboard navigation), and
an open sidebar menu, a drag of a row or a resize of the sidebar keeps it opaque too (the drag,
resize and menu rules had lost to the fading rule on specificity until then).

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

Keyboard shortcuts (`snorkle_content::SHORTCUTS`; since 2026-11-24 also ⌘⇧N New Private Tab, ⌘⇧D Send Page to Notebook (§5l), and ⌘D on a pin's tab bookmarks the pin, §5k) are key commands vended by the app delegate
(`keyCommands`, added to tao's class at runtime), the last responder in every chain, so they
work whatever has focus, including nothing (sidebar hidden, no page). They are not menu-bar
items any more (as menu items they stopped working, apparently from the menu system's
validation and key-equivalent conflicts, e.g. ⌘I with Format → Italic); the menu bar only loses
the system's Close and Find. Recording a binding is native too: while recording, the app delegate
offers every accepted ⌘ combination and Escape and reports the one pressed. Shortcuts: ⌘T new tab, ⌘⇧T reopen the last
closed tab, ⌘W close tab (a pin's or bookmark's tab is never closed: ⌘W opens a new tab instead,
2026-11-23; before, a pin's tab showed a message and a bookmark's asked whether to remove the
bookmark; with History's page dialog open, ⌘W closes the dialog, 2026-11-24. On iPadOS, closing or
hiding the page that had keyboard focus left nothing first responder, and key commands were then
not delivered until a tap, so the next ⌘W did nothing; since 2026-11-24 the host gives focus to
the page in front, or the chrome, once the effects are applied: `ios/effects.rs` `refocus_soon`), ⌘⇧R night vision and ⌘⇧H History (2026-11-23), ⌘Q quit (macOS only, 2026-11-23:
neither offered nor reserved on iPadOS, `ShortcutDef::available`), ⌘F find, ⌘G /
⌘⇧G next / previous match, ⌘\ sidebar, ⌘⇧\ notebook (2026-11-25; until then it hid and showed all
the right panels), ⌘S / ⌘⇧S split side by side / top
and bottom, ⌘D bookmark (on a bookmark's tab: un-bookmark; ⌥⌘D from 2026-11-03 to 2026-11-04,
when ⌘D split the view), ⌘I images,
⌘1–5 sidebar items, ⌘← / ⌘→ back / forward and ⌘↑ / ⌘↓ detach / attach the URL bar (2026-11-15;
the arrow keys are named `left`…`down` in bindings; while a text field or editable region has keyboard
focus, in a page (reported by the selection watcher, C19) or in the chrome, arrow bindings are left to
the text: withdrawn from the key commands on iPadOS, passed on by the key monitor on macOS),
⌘⇧] / ⌘⇧[ next / previous tab (2026-11-08; Chrome's and Safari's keys:
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

**History** (2026-11-03; in the sidebar since 2026-11-23): Menu → History or ⌘⇧H replaces the
sidebar's contents (pins, bookmarks, tabs) with History, as Save for Later's list did before (× or
Escape returns; a hidden sidebar is shown and hidden again with it). Its heading row (Select, Clear,
×) and search box stay at the top; then the pages saved for later (§5b), then the visits by day,
each with its time. Clicking a visit opens it in a dialog (see the preview below); Open Page there
loads it in the current tab, and History stays open beside the page.
Pointing at a visit replaces its time with a tray (Save for Later, below) and an × that removes it;
a visit of a page already saved shows a tray with a tick instead. Select turns rows into
checkboxes (a click or ⌘-click toggles one, Shift-click selects a range); ⌘- or Shift-click on a
visit also starts selecting (2026-11-24). While searching, a bar under the search box gives the
number of results and Select Results, which selects every result shown. Selected visits can be
deleted (Delete, `remove_history`, by URL and visit time), restored as tabs of the session shown
(Restore), or sent as tabs to another session or a new one named after the search (Send to;
owner request 2026-11-24): `history_to_tabs` opens each page once, only addresses in history,
at most 100, as tabs at the end of the session's list that load when chosen
(`Browser::send_page`, as restored tabs). Days fold (2026-11-24): each day's heading shows its
number of visits and folds or unfolds it; today is open and every other day starts folded
(remembered until the app quits); while searching every day starts open. A range or Select
Results takes only the visits shown. Clear (pressed twice) clears history; pages saved for later stay. The text of
the 100 most recently visited pages is saved (`storage` `page-text/`, `SavedPageText`): 8 s
(1.5 s until 2026-11-23) after a non-private page finishes loading, if it is still on screen, done
loading and on the same address, the host runs the constant `ai::PAGE_TEXT_JS` (C17) and
the title and visible text (`innerText`, ≤ 48,000 characters, cleaned) are written if the tab still
shows that page. The text is kept only while the page is in history: removing its last visit or
clearing history deletes it. Rows with saved text show its size beside the address. Search (`search_history`) matches titles, addresses and
the saved text (with a snippet), and filters the saved pages above by title and address. Images
are not saved. Settings → Cache does not delete saved text (it belongs to history).

**The History preview** (owner request 2026-11-24; before, the dialog showed only the saved text,
and visits without it opened at once): clicking a visit opens a dialog with its title, address and
Open Page, Save for Later and Close, and the page itself loads in it. The page is a tab of its own
(`Browser::open_preview`, `open_preview`): default profile, loaded through the navigation policy as
a typed address, at most one, never listed with the tabs (`ChromeState::preview` names it), never
saved in the session, reopened by ⌘⇧T, written to history (opening a visit does not move it to the
top) or made active. Once it has committed a page it is the only view on screen
(`Pane::Preview`): the chrome hides the rest with its dialog's overlay and reports the dialog
body's rectangle with its layout (`set_chrome_layout` `preview`; the host keeps it inside the
window, `snorkle_content::preview_rect`, and rounds its corners to match). Until then, or if the
page cannot show (a notice), the body shows the saved text (search matches marked) or "Loading…".
When the page has loaded, History's search words are found in it as ⌘F does. Save for Later saves
the page as shown (`save_for_later` on the preview tab, whose tab then closes, and the dialog with
it; before the tab exists, `save_page_for_later`); it reads Saved for a page already saved. Close,
a click beside the dialog, or Escape while the chrome has the keyboard (not after a click into the
page, which then has it) close the preview (`close_preview`); choosing another tab
closes it too, and so does a page it opens in a new tab (which stays, as an ordinary tab).

**Save for Later from History** (2026-11-23, `save_page_for_later`): the visit's address (http(s),
as history stores it) is loaded in a new tab of the default profile that is never shown
(`Browser::open_in_background`: the navigation policy, Settings → Blocked and HTTPS-first as for a
typed address; within the live-view budget, as a ⌘-clicked link's tab), and once it has finished
loading (≤ 40 s, then 1.5 s for late content) it is saved as a tab's own button saves it
(`saved::save_url` → `saved::save`), which closes it. A page that does not load is not saved and its
tab is closed. Refused for a page already saved.

**Tab and bookmark favicons** (2026-11-04): each tab and bookmark row shows its site's icon, fetched and stored the same way
as pins' (F1: a sandboxed, script-free web view loads `https://<host>/favicon.ico`; the app keeps
only a PNG it redrew itself, `icons-v5/`). **Dark copies** (2026-11-24): an icon too dark to see on
the dark chrome (what is visible of it has a mean brightness under 0.3 and under 10% of it is
light; `favicon::needs_dark_variant`, on the app's own 128 × 128 bitmap) is drawn a second time with
each pixel's lightness turned around and its hue, saturation and alpha kept
(`favicon::make_dark_variant`; black becomes white, mid tones such as a pure red stay), and stored
beside it (`icons-v5/dark/`). `list_pins` and `tab_icons` send both; the chrome shows the copy while
it is dark (Settings → Appearance, the system's, or a private tab's always-dark chrome) and
switches when that changes. The cache moved from `icons-v4` to `icons-v5`, so icons are fetched
once more. The chrome asks `tab_icons` when its tabs' addresses
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
it always loaded the item's page again). Right-click on one (⌘-click on a pin) opens its menu: Copy URL (the page its tab shows, or the saved
address if it has no tab), Edit… and Return to Original URL (its tab loads the saved address);
tabs get the same menu on right-click without Return to Original URL. Since 2026-11-24 the menu also
has the group actions, Remove and Send to (§5l, §5m), and ⌘-click on a bookmark or tab chooses it
instead (§5m). **Edit** (2026-11-09) is one
dialog for tabs, pins and bookmarks: title, URL and an emoji shown instead of the favicon (typed, or
chosen from a grid of common emoji). For a pin or bookmark the URL is read like the address bar
(addresses only; place is kept, its tab follows via `Browser::rekey_launcher`, its offline copy is
dropped when the URL changes) and the emoji is stored in `pin-emoji.json` for pins and bookmarks
alike (`storage::clean_emoji`; kept when a pin moves into the bookmarks, dropped with the item). For
a tab the title is its own name (`Tab::name`, shown instead of the page's title; empty follows the
page), the emoji `Tab::emoji` (both saved with the session, `Browser::set_tab_label`), and a changed
URL loads in the tab like the address bar. Bookmark rows are one line, like tabs. **Pins:** up to 12
(three rows of four); their menu adds Return to Bookmark (moves it to the top of the bookmarks). A
pin's × asks first (2026-11-24: "Unpin …?", beside the pin; its tab stays open as an ordinary tab). **Appearance**
(Settings → General: System, Light, Dark) sets the chrome's colours (`data-theme`) and the
window's `overrideUserInterfaceStyle`, so system UI follows it. Pages do not (2026-11-15): a
content view is light over a white canvas unless dark pages mode is on for it (until then a view
without the mode followed the window, and pages without a background of their own were drawn on
the system's black).

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
dot and the menu item a count while downloads run. While Downloads is closed the toast shows the
running downloads' progress (name or count, percentage or size, a bar), and when one finishes or
fails it says so with a **Show in Downloads** link (2026-11-19; before, Rust sent plain toasts
when one started and ended).

Menu → Downloads lists them, newest first: progress while running (Cancel), and for finished files
Save to Files… (iPadOS, the Files export picker) or Save… (macOS, a save panel; the copy keeps the
quarantine mark), Share… and × (deletes the file). Clear deletes every finished or failed one.
Done (solid, high-contrast; also on History) or Escape returns to the page; the heading row with
Done stays in view while the list scrolls (2026-11-19).
Finished downloads of non-private tabs are recorded in `downloads.json` and kept until deleted;
failed and cancelled ones keep a record without a file. Private-tab downloads are listed for the
session only and deleted (cancelled if running) when the private session ends. Folders without a
record (interrupted downloads, earlier private ones) are deleted when the app starts. A
`target=_blank` link that turns out to be a download leaves its new tab empty.

## 5e. Docked panels

Native page views sit above the chrome's webview, so the chrome cannot draw over a page except
in holes (§5f). Panels that should leave the page visible dock beside it instead: the chrome
reports `right` and `bottom` insets and the host shrinks the content area. Site scripts dock on
the right, full height; otherwise the AI panel (the summary with the page chat in it, §5j, §5g)
and the Notebook (§5i) do, in that order from the top, sharing the height when both are open (in
narrow windows site scripts and the AI panel overlay and hide the page). Each session (everyday
browsing and each research session, §5l) has its own: whether the AI panel and the Notebook are open
(remembered across launches). The right panels' width is dragged at their left edge (260–640 pt, at most 60% of the
area, remembered). ⌘⇧\ shows or hides the Notebook, as its button does (2026-11-25; until then it hid
all the right panels without turning them off, and showed them again). The console
docks at the bottom. A ring around the `{ }` row in the ⋯ popup marks pages an enabled site script
applies to (until 2026-11-24 also a ring on the ⋯ button, which read as a stray white circle in the
dark appearance). When the content area grows (a panel closes), the
host checks after 0.35 s whether WebKit left the page drawn narrower than the view (a zoom scale
kept from the narrower layout) and zooms it to fill the width.

## 5g. Page chat

The AI button in the URL bar (2026-11-23; before, a chat button beside the summary button) opens
the AI panel (§5j); since 2026-11-24 the chat is part of it: its conversation follows the summary in
the same scrolling column, and its box is at the bottom of the panel, over a line with a model menu
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
**Save for Later** (2026-11-09; `snorkle_core::later`, `later/`): a tab's tray button (and a
visit's in History, which loads the page in a hidden tab first, §5f) saves its page. 1.5 s before
the capture the view modes are taken off the loaded page only (`viewmodes::plain_js`) and its view
drawn light (`force_plain`), so hidden images and CSS backgrounds load and the archive holds them;
the page briefly shows that way before the tab closes (`saved::save`; put back if saving fails). The
tab's mode settings and the scripts installed for later loads are not touched. It saves the page as
an **archive** that works offline (below), stores the committed page and title (≤ 200 pages), and
closes the tab; private tabs and empty tabs have no such button. No thumbnail is taken
(2026-11-23; until then a 320 pt JPEG snapshot, `takeSnapshotWithConfiguration`, was shown as a
card; stored ones stay on disk with their items). The chrome shows how far it is in a toast with a
percentage and a bar (`later_progress` events; also while adding pages). A tab without a live view
is saved without an archive. The saved pages are listed at the top of the History sidebar (§5f;
until 2026-11-23 an icon beside the menu swapped the sidebar's contents for them): one row each
with the site's letter (or bundled icon), the title, the site and the number of pages when more
than one. A click opens the saved copy in a new tab (it stays in the list; without an archive it
opens the live page). On hover: Recover (the live page in a new tab), "+" (add pages), Bookmark
(added at the end of the bookmarks) and Delete; Recover, Bookmark and Delete remove the page from
the list. Only hover or keyboard focus shows them, not a click's focus; on touch-only devices, the
row whose saved copy is open (2026-11-19).

*AI conversations* (2026-11-15; `snorkle_core::conversation`, C22b): on chatgpt.com (and
chat.openai.com), claude.ai and gemini.google.com the archive's page is the conversation itself,
built by the app, instead of the site's app shell. Since 2026-11-16 Save for Later there starts a
**capture mode** (the automatic sweep did not load whole threads on the real sites): a bar above
the pill (a hole) says how many messages are captured; the reader scrolls through the thread
(up for earlier messages, which the sites load as they are reached) while constant code in the
client world notes every turn the site puts in the document (on DOM changes and scrolling, at most
every 0.3 s), each snapshot sorted by position on screen and merged into the thread at the
longest run of turns they share, so either scrolling direction works; Save saves exactly what was
captured, Cancel (or Escape) drops it, and the bar hides while another tab is shown. Saving
without capture mode still sweeps automatically (constant code finds the turns, scrolls the thread
to the top until no earlier turns load, ≤ 20 s, then down a screen at a time, then back to where
the reader was). Either way the turns are handed over as node trees (text, or tag, attributes and
children; `blob:` images as `data:` URLs). Rust rebuilds a page from them with a fixed set of tags
and checked attributes (no scripts, styles, frames or handlers can come out), labels each turn
(You / ChatGPT / Claude / Gemini) and adds the title, the date and the address; it is shown with
JavaScript off, with the images WebKit's archive holds. With no turns found, WebKit's copy of the
live document is kept instead, also with JavaScript off. Asking the site for a share link was not
done: it would publish the conversation.

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
ordinary one: it is not listed with the tabs (its row in the History sidebar's saved pages is shown as selected, and
opening the card again returns to it), not saved in the session or reopened by ⌘⇧T, not written to
history, and closed when its item leaves the list; its address bar shows a "Saved copy" pill. A
link to another page of the archive is cancelled and that page loaded from the archive
(`Effect::OpenSavedPage`); Reload reloads from the archive; any other link or form, and an address
typed in its bar, opens in a new ordinary tab through the navigation policy (as a ⌘-click would);
script navigations and redirects out of the archive go nowhere. "+" opens the saved copy with scripts off and
a bar above the pill: each link tapped on the page (same host, at most 30 pages) is chosen or
unchosen in the core instead of navigating; Add loads each chosen page live in that tab, captures
it the same way into the same archive (resources shared), and shows the saved copy again.
Tabs and bookmarks are listed in a resizable sidebar along the left edge (200–420 pt, at most half the window). Dragging a tab into the bookmarks bar bookmarks it there, bookmarks reorder by dragging, dragging one tab onto the middle of another pairs them in split view, onto the top or bottom 30% of a row (or below the last) moves it there (2026-11-09, `Browser::move_tab`; the session keeps the order), and dragging a tab out of its pair unsplits it. A tab's close button appears on hover (on the active tab on touch-only devices); so does a pin's unpin button. Each pin and bookmark has its own tab (`Tab::launcher`, saved in the session): opening it returns to that tab, which is not listed with the other tabs, and the pin or bookmark is shown as selected; removing the pin or bookmark lists the tab again. New Tab (⌘T, or the menu; since 2026-11-08) is a real tab: an empty tab shows the start page on the chrome's surface (no web view exists until it navigates), with the new-tab input a little above the middle of its pane (its middle at 40% of the pane's height; in split view, the focused half) and Arc-style rows under it: typing fuzzy-matches open tabs, pins, bookmarks and then history (the latest 500 visits, one row per page, loaded when the start page opens), and the first rows go to, search for, or, for a single word, define what was typed. Go to, Search and history rows load in that tab; choosing another tab, pin or bookmark switches to it and closes the empty tab (unless it is half of a split), as Chrome's omnibox does. Escape clears the input; its placeholder is "search / url" (2026-11-24). Since 2026-11-24 the start page has no URL bar, its input (52 pt tall, 600 pt wide at most) sits at the middle of the pane (the pill and its AI and Notebook buttons are hidden while an empty tab is shown; ⌘⇧\ still shows or hides the Notebook), lists nothing until something is typed (before, open tabs, pins, bookmarks and recent history were listed at once), is drawn in the sidebar's own background, and its input is a field rounded alike at all four corners, with the rows in a separate card under it. There is at most one empty New Tab (2026-11-24): ⌘T or the menu in an empty tab of the same profile only focuses its input, and an empty tab left for another (never loaded, not half of a split, no pin's or bookmark's) closes (`Browser::new_tab`, `abandoned_blank`). **Define** (single words only, `ai::clean_word`) shows the chat's model's dictionary-style definition in a card above the input (2026-11-09; before, it opened the chat panel), after asking which word was meant if the model thinks it is misspelt (§5g). **Each launch** restores the saved tabs unloaded and greets the user with a new start-page tab after the saved active tab (`Browser::restore_session_with_start_tab`); returning from the background is not a launch and adds nothing, and a cold launch from a link in another app shows that link instead. Empty tabs are never saved, so launches do not pile them up. (Until 2026-11-08 New Tab opened a palette over the hidden content area and no tab existed until a row was chosen.) The bottom bar holds the menu (New Tab, New Private Tab, Turn On/Off Night Vision, History, Downloads, Console, Settings), New Research Session (a book), dark pages, colour, and show images and video; with a pointer its buttons show only while the sidebar is pointed at or has focus (2026-11-23). Pins are bare icons (a dot under the one whose tab is shown); empty pins and bookmarks take no room and show no explainer text, and open up as dashed drop targets while a row is dragged (2026-11-23). The chrome itself is black and white (grey palette, plus `filter: grayscale(1)` on its root for favicons and emoji). Settings deletes cache, cookies, site data, or logins (cookies and site data) from every profile's store. In windows at least 700 pt wide the sidebar is
docked: the chrome reports `left = sidebar width` and the native content container starts to its
right. In narrower windows it slides over the content area, and the content view is hidden
while it is open (`overlay`), so a page can never draw over the sidebar or the toolbar. Both
insets are clamped in Rust to at most half the window. Both sidebars' resize edges are a 3 pt line
inside the panel that lights up on hover. With the sidebar closed, a small tab stays on the left
edge; pointing at it (or tapping it, without a pointer) shows the sidebar as a pop-over inset
20 pt from the top and bottom, rounded on the right, over the page (a hole); it goes away 0.28 s
after the pointer leaves it, or on a tap outside or Escape. In narrow windows the tab opens the
overlaid sidebar instead.

## 5i. Notebook (2026-11-15)

One notebook for every page, not bound to a tab: the notepad of laffan/zotero-helper
(`src/notepad/`, `NotepadBody.tsx`) ported to the chrome as plain DOM (`ui/notebook/`,
`ui/notebook.css`). The toggle sits between the menu and Save for Later at the sidebar's foot; the
notebook docks right of the page with the other right panels (remembered open or closed; ⌘⇧\
shows or hides it, 2026-11-25). Since 2026-11-25 it is drawn in the sidebar's colour, and its bar is
one capsule drawn as the URL bar is (the bar's colour, a hairline border, round ends) with no line
under it: the quote button and the scissors (plain tools, no longer an orange pill), the minimap,
the scroll link, search, a switch of ink / red / eraser / lasso, and undo; there is no close button
(the Notebook button and ⌘⇧\ close it). Under it, a canvas with a faint dot grid. The pen or the mouse writes (one canvas the size of the view; a
stroke costs the same on a full pad), a finger or a wheel pans, a press held still draws a lasso
(framed selection, Delete, drag to move, ⌫, Escape), two taps type a note in Markdown (headings,
bold, italic, links, highlights, quotes, tasks, lists), links are chips (dragged, tapped to go,
held for Delete / Edit, sized by a grip, fixed by a pin under the ink), undo takes back each
change, and the trash can clears it after asking. A note remembers the page it was written beside
(its site shows small above it; a tap goes there); the scroll link makes the pad glide to what was
written about the page shown. Not ported: the floating window, auto-shift, Pencil double-tap.
**Search** (2026-11-16, not in zotero-helper): the magnifier in the bar, or ⌘F while the notebook
has the focus (⌘G / ⌘⇧G step; elsewhere they still find in the page), opens a field under the bar.
It finds typed notes' and quotes' words and, for every item including handwritten notes, the title
and address of the page it came from; every word must appear, case and accents ignored.
Handwriting itself is not read. Matches are outlined (handwritten notes framed), everything else
faded; ↩ / ⇧↩ go through them top to bottom and glide the pad to each. Chrome only: no command,
nothing stored.

Since 2026-11-16 the notebook's button sits in the URL bar (since 2026-11-23 between the AI
button and ⋯), and the URL bar's share button is its last button, right of the address field.
**Copy and paste**: ⌘C / ⌘X copy or cut the lasso's selection (strokes with their notes, and
links), kept in the chrome to paste back in and put on the system clipboard as plain text; ⌘V
pastes them (moved 24 pt each time, selected), or text from another app as a typed note (a web
address as a link). The paste is caught in a hidden field so the system's own paste supplies the
text; no clipboard permission is asked.

Bringing things in (C23): a constant watcher in every page's client world reports
- ⌥ held (2026-11-24): the element under the pointer is outlined with the clip stage's dashed
  border: an image, video or canvas, else a link, else the innermost element laid out as a block (at
  least 16 × 12 pt, no larger than the view; nothing is outlined over a larger one). ⌥-click clips
  it at once (the clip's report, as a drawn stage's), or sends the link (below);
- ⌥-click on a selection: "Send to Notebook" appears above it (a hole); pressed, the text is read
  as the selection menu reads it (C20) and becomes a quote linking back with a text fragment
  (`#:~:text=`). Over text with no outlined element, ⌥-click selects the sentence the same way
  (`Intl.Segmenter`); until 2026-11-24 it always did, and an image got "Send to Notebook" too;
- ⌥-drag, or a drag while the scissors are on (clip mode, which then ends): while ⌥ is held over
  a page (key events, or the modifier state of pointer moves), and in clip mode, a cover lies over
  the page: a crosshair, and every press goes to it, so no text is selected and nothing is
  followed (2026-11-16). Holding ⌥ alone does not dim the page; it dims once a drag begins (the
  stage's shade around its box), and in clip mode the cover also fades it slightly (2026-11-16,
  replacing a fade whenever ⌥ was held). ⌥ with ⇧ (or ⌘, ⌃) is not the notebook's: no cover,
  no ⌥-click, no clip, so ⌥⇧ arrows select word by word as usual. A drag draws a dashed box that stays as a stage, as
  zotero-helper's clip mode: its corners size it, a press inside moves it, a press outside draws
  another; Clip (or ↩) makes the clip, × (or Escape) drops it. Every handler takes only real input
  (`isTrusted`). A clip carries the text it shows, for search, not shown: the page's text laid out
  in the region (and images' `alt`), read before the picture, and the lines Apple's on-device text
  recognition (Vision, `webkit/ocr.rs`) reads in the picture that the page's text lacks;
- ⌥-click on a link (2026-11-16): "Send to Notebook" adds a link titled with the linked page's own
  `<title>` (or `og:title`), read from the start of that page fetched without cookies (≤ 512 KB of
  an HTML reply, 10 s; `http::fetch_page_head`, `notebook::page_title`); its host if it has none;

- dragging selected text or an image out of a page (2026-11-16): the watcher reports where the
  drag began (the selection, or the image's box) and when it ends; let go on the notebook's
  canvas, it lands where it was dropped as a quote (the selection read with C20) or a clip of that
  box. The drag's own data (the page's HTML, image files) is not used. Text dragged from elsewhere
  becomes a typed note (a bare web address, a link); images from elsewhere are refused. A drag
  resting on the notebook's button opens it. Tauri's macOS file-drop handler is off for the
  chrome's window (`dragDropEnabled: false`), since it swallows every drag into the chrome; the
  chrome cancels any drop it does not take (outside text fields), so nothing dropped navigates it.

A clip keeps the links laid out in its region (2026-11-25): the region read-back that gives its
text also gives the page's http(s) links whose boxes meet the region (at most 20, in page order,
links within the page itself left out), each with its words (or an image's `alt`, or its
`aria-label`). They are listed under the clip's picture and above its page line, one a line as link
chips show them (the icon, the words, the site); a press on one opens that page (a pinned clip: a
tap on the row). The read-back also gives the region's box in the page's document; a tap on the
clip opens its page as before (the tab showing it, or a new one) and, once that page is loaded
there, scrolls it to the box and outlines it for 2 s with the clip stage's dashed border
(`notebook_clip_back`, constant code with four numbers; given up after 30 s or if the tab goes to
another page). Before, a tap on a clip whose page was the one shown did nothing visible. Search
finds a clip by its links' words and sites; the export lists them under its picture.

Where things from pages land (2026-11-25): where they were dropped on the canvas; otherwise
(⌥-click, a clip drawn on the page, the quote button, Send Page to Notebook) under everything on
the pad, 24 pt below the lowest item and in line with the lowest chip, so clips made one after
another form one column. The pad then pans to the new item: it glides there while the notebook is
shown (unless the item is in view already), and is left there for next time while it is hidden.
Chips are measured even while the notebook is hidden (laid out apart, off screen), and a clip's
picture has its proportions (read from its JPEG or PNG header) before it is decoded, so a chip's
height is known when the next item is placed. Until then, items arriving while the notebook was
shown went to the middle of the view, and clips made while it was hidden were counted 200 pt tall
(hidden chips measured nothing) and overlapped.

A clip in the notebook has two copy buttons at the end of its foot (2026-11-24; shown while it is
pointed at, always on touch-only devices): Copy Text puts the text kept with it for search (the
page's text in its region and what text recognition read in its picture) on the clipboard, or says
there is none; Copy Picture puts its picture there as a PNG (redrawn from the clip's JPEG in the
chrome, through the Clipboard API with the press's activation). Neither moves nor follows the clip.
Copy Text pressed and dragged (more than 6 px) carries a quote of that text instead: let go on the
notebook, it is a quote chip there (the clip's page, the text as its words, up to 4,000 characters);
elsewhere, nothing.

A clip is a JPEG the app draws from its own web view (`takeSnapshotWithConfiguration` with the
region; the page is shown without its view modes for 0.32 s first, so black and white, hidden media
and the dark inversion are not in it); the chrome makes its white-on-black copy (zotero-helper's
`darkPagesCopy`). The quote button dragged onto a page: the page marks the sentence under the
pointer and draws the line on from where the chrome's line crosses its right edge, so the two read
as one (constant code with three numbers; the line and its dot are manual popovers in the page's
top layer, so the view modes' root filters leave them red; 2026-11-24); let go, that sentence
is read back and quoted (or the page linked when there is none). While the page shown is loading the
quote button is dimmed and does nothing (a load that starts mid-drag ends the drag), and a page still
loading under the pointer is not marked (2026-11-24: sentences still move while a page loads). Only the sentence under the
pointer is marked (one highlight, cleared on drop and when the pointer leaves the page; a mark
call still under way when the drag ends is followed by one that clears it). Links open the page shown in an
open tab or in a new one, through the navigation policy. Nothing comes from private tabs (every
command refuses them) and notes written while one is shown keep no address. The notebook is one
file, `notebook.json` (≤ 64 MB), written a moment after each change once the pen pauses, and
rebuilt field by field in Rust on every write and read (`snorkle_core::notebook::clean`).

## 5j. Summary view (2026-11-16)

Replaces the headers view (its outline, Detect Headers and the per-site heading rules; channel
C14 is retired and `heading-rules.json` is deleted when the store opens). The AI button
in the URL bar (2026-11-23; before, a summary button per tab and a separate chat button) opens the
AI panel docked right of the page (the Notebook below it when open): since 2026-11-24 one panel, the
summary followed by the chat about the page in one scrolling column, the chat's box at the bottom,
under it the model, effort and cost line with New Chat (+) and Earlier Chats (an icon; the archive
takes the panel's place while open), and one close button (until then the chat was a second
panel under the summary's). The summary is no longer per tab: it follows the tab shown.
Pressing the AI button on a page that has no summary yet asks for one at once (opened another way,
Ask AI on a selection or a definition's follow-up, or on another tab, it asks "Summarize
“title”?" first); the panel then shows the summary as bullet points, the model and an estimate of
the cost, and a circular-arrow button to summarize again. When the tab moves to another page (its address without the fragment changes),
the summary is dropped and the panel asks "Summarize “title”?" with a Summarize button; a reply
still on its way for the old page is ignored. Summaries are kept in the chrome's memory only, one
per tab, and nothing is stored, in private tabs or otherwise. The service and model are Settings →
AI → Uses → Summary View (it starts from the old Header View Parsing choice).

Reading the page (C24): only when a summary is asked for, the host runs constant code in the
page's client world, main frame (`summary::blocks_js`) and reads back its string result, as the
page chat does (C17). It walks the visible text in document order and gives each text node to its
nearest ancestor that is not laid out inline (paragraph, list item, heading, cell); navigation,
controls, scripts, SVG, media and hidden elements are skipped; at most 800 blocks, 1,200
characters each, 40,000 in all. The blocks' elements stay in the client world
(`window.__snorkleSum`). `summary::parse_blocks` checks and sanitises the result; Rust sends the
tab's committed URL (from the core), the title and the non-empty blocks as `[n] text` lines to
the provider (`ai::summary_request`), with a system prompt that says the page is data. The reply
(`ai::parse_summary`) keeps at most 12 points of at most 400 characters, each with at most four
block numbers below the number of blocks read; the chrome shows them with `textContent`. If the
tab's committed URL changed meanwhile, the reply is discarded.

Following a point (a click, or ↩ / space when focused; its words can still be selected and
copied): `scroll_to_summary` runs constant code whose only input is the block numbers
(`summary::scroll_js`). It scrolls the first block into view (centred, or its top when it is
taller than 60% of the view) and highlights each block briefly, as the headers view did. If any
of the elements is gone (the page was reloaded, or a script redrew it), the page is read into
blocks again the same way and those are used, so a reload of the same content still lands on
the same text. Points the model gave no valid block are shown without a link.

## 5k. Pins keep their page (2026-11-24)

A pin's tab stays on the page it shows. Going elsewhere from it opens the page in a new ordinary tab
(the pin's profile and session, at the end of the list, shown at once; Back there loads the pin's
page), so the pin stays as it was and the new tab can be bookmarked or closed. The core knows the
pins' URLs (`Browser::set_pins`, sent at launch and after every pin change) and acts only in a pin's
own tab (not private, not a sheet or saved copy):

- a link the user follows (`decide_navigation`, main frame, `LinkActivated`) whose address, fragment
  aside, differs from the page shown: cancelled, opened in the new tab through
  `on_background_tab_request` (the navigation policy as a link with a user gesture);
- an address typed in the bar there (`navigate_input`), unless it is the page shown;
- single-page apps (`history.pushState` from their own click handlers, no navigation): a constant
  watcher in every page's client world (`pinguard::watch_js`, channel C25), inert until the host
  turns the guard on for a pin's tab (`Effect::PinGuard`, after each commit and finished load; off
  when the tab stops being a pin's), stops a real plain click on a link to another page before the
  page's handlers see it and reports the address (`on_pin_link`); any other real click or Return /
  space is reported as a tap (`on_pin_gesture`), and a same-origin address change within 1.5 s of
  one (`on_url_changed`, `GESTURE_WINDOW_MS`) opens the new address in a new tab and sends the pin
  back (`GoBack`, or a load of its previous address when it has no history).

Left in the pin on purpose: redirects and address changes the page makes without a user action
(a site moving to its real address, or rewriting it after loading), form submissions (a sign-in form
posts there; its data cannot be sent again in another tab), back/forward and reload, fragment-only
changes (Gmail's views), and the browser's own loads (opening the pin, Return to Original URL). A pin
sends at most 10 pages a minute (`MAX_DIVERTS`); past that its links are followed in place. Bookmarks'
tabs are not guarded.

⌘D on a pin's tab (before: "This is a pinned site's tab.") bookmarks the pin's address and title; the
pin stays, and since the bookmark has the same address, opening it comes back to the pin's tab. While
the address is both, removing either leaves the tab bound to the other.

## 5l. Research sessions (2026-11-24)

A research session is a fresh sidebar (no pins, no bookmarks: only its own tabs) and its own
notebook. Every tab belongs to everyday browsing (`Tab::space` `None`) or to one session; the session
shown is the active tab's (`Browser::space`), so choosing a tab of another session switches to it.
The book button at the sidebar's foot makes one (2026-11-24; before, Menu → New Research
Session; `new_research`: an id `r` + the wall-clock time, a topic,
entered at once with an empty tab) and puts its topic in place to be typed. Its topic heads the tab
list beside a back button to everyday browsing (`enter_research(None)`: the tab used there last, a
pin's or bookmark's included); pointing at it shows a pencil, and the topic itself is a button that
renames in place. Every session is a row at the foot of the sidebar, above its buttons, with no line
above them, shown with the foot's buttons (while the sidebar is pointed at, has the focus, one of
its menus is open or a row is dragged; always on touch-only devices) (its name and tab count; a click enters it, or leaves it when it is shown). Each session
has a menu (≡: on its row while it is pointed at, always right of its topic inside the session, and
on right-click on its row): **Export Notebook** (below), **Send Tabs to Main** (every tab of the
session with a page goes to everyday browsing; from inside the session, everyday browsing is then
shown with the first of them) and **Delete** (closes its tabs and deletes its notebook, after a
confirmation).

**Export Notebook** (`export_notebook`): the session's notebook, saved first if it is the one shown,
becomes one self-contained HTML page (`notebook::export_html`), named after the topic and offered to
the Files export picker (iPadOS) or a save panel (macOS), as Downloads' Save… does; the file is
written to the app's `exports/` folder, which keeps only the latest. The page draws the canvas as in
the app (the ink as SVG, quotes with their source links, clips, links, typed notes as their Markdown
text; pinned chips under the ink), with the topic and the date above it. Every string is escaped,
links are the notebook's checked http(s) addresses, and a content security policy allows no scripts
and no loads but its own `data:` images.

**Tabs into notebooks**: a tab (or a pin or bookmark) dragged from the sidebar onto the notebook
shown becomes a link where it is let go; dropped on a research session's row, it becomes a link in
that session's notebook, below everything in it (`notebook_add_page`: the address from the core or
the store, never a private tab; `notebook::add_page` on the stored notebook, or the open notebook
itself when that session is shown). The tab stays where it is. ⌘⇧D sends the page shown to the
notebook as a link, as ⌥-click on the copy button does.

New tabs, popups and ⌘-clicked links open in their opener's session; closing a session's last tab
opens an empty one there (the session stays until deleted). Pins' and bookmarks' tabs are everyday
browsing's. The start page in a session offers its own tabs and history, not pins or bookmarks.
History and Downloads are shared.

**Send to** (every tab's, pin's and bookmark's menu; a group's too): everyday browsing, the other
sessions and New Research Session. Tabs are moved (`send_tabs`: they leave their pair and group and go
to the end of that session's list; a sheet goes with its opener; when the tab shown is sent away, the
session shown stays and shows its last-used tab or a new one). A pin's or bookmark's page is copied
(`send_page`: a new tab there, unloaded until chosen). Sent to a new session, the session is entered
with the first tab sent shown.

**Notebook**: one per session (`notebooks/<id>.json`; everyday browsing keeps `notebook.json`),
checked the same way (`notebook::clean`). Moving between sessions saves the shown notebook and loads
the other's; `load_notebook` / `save_notebook` name the session, and a session the core does not have
is refused (a deleted session's late save is dropped). A session's notebook is open by default (the
chrome remembers closing it there, `nbClosed:<id>`); everyday browsing's opens as last left.

Saved with the session (`Session::research`, `SessionTab::space`); a launch greets the user in the
session they were in. At most 50 sessions; topics are cleaned like titles (≤ 80 characters).

## 5m. Tab groups and bookmark groups (2026-11-24)

Rows are chosen with ⌘-click (adds or removes one; the tab shown is chosen with the first) and
⇧-click (every row from the last clicked, folded groups skipped), tabs or bookmarks, one kind at a
time; a plain click opens a row and ends the choice, as does Escape. Right-click on a chosen row acts
on all of them: Group These Tabs / Bookmarks (one row: Add to New Group), Remove from Group, Remove
(closes tabs, removes bookmarks) and Send to (§5l). A new group's name is ready to be typed.

A **tab group** (core: `Browser::groups`, `Tab::group`) is a named, foldable run of tabs of one
profile and one session; its tabs stay next to each other (`compact_groups`). Its heading shows the
name and the number of tabs; a click folds or unfolds it, a double-click renames it in place, × closes
its tabs (after a confirmation), and right-click offers Rename, Ungroup, Move to Bookmarks (each tab
with a page is bookmarked and becomes that bookmark's tab, in one bookmark group of the same name;
also by dragging the heading into the bookmarks bar), Remove and Send to. Tabs opened from a grouped
tab join its group; ⌘T opens after the group. Dragging a tab next to a group's tab puts it in that
group (the heading's top half: before the group; its bottom half: first in it); elsewhere, out of it.
A tab leaving the list (bookmarked, sent to another session) leaves its group; a group without tabs
goes. Pinned and bookmarked items' tabs, popups and saved copies are never in one. Saved with the
session (`Session::groups`, `SessionTab::group`), private tabs' groups never.

A **bookmark group** (store: `bookmark-groups.json`, `Bookmark::group`, ids `g` + digits) is the same
in the bookmarks bar: its bookmarks stay together (`tidy_bookmark_groups`, on every change and when
read), dragging a bookmark (or a tab) into its box puts it there, and its heading offers Rename,
Ungroup, Remove (its bookmarks) and Send to (copies). The pinned grid has no groups. Bookmarks go
back to being tabs by dragging them into the tab list (2026-11-24; `unbookmark_to_tabs`): one
bookmark, the chosen ones together, or a group's heading (the group becomes a tab group of the
same name); the list opens a dashed target at its end and a line shows where they go (before or
after a row, in that row's group). Each becomes its bookmark's tab when it has one (no longer a
bookmark's, so it is listed), else a new unloaded tab; the bookmarks are removed. Rows chosen
together are also dragged together the other way (tabs into the bookmarks bar, and tabs within the
list), with their number on the dragged row. Groups have no side line (2026-11-24). Same-site
headings (§5b) are unchanged and apply only to tabs outside a group.

## 6. Credentials

The only credentials the app stores are the user's own AI API keys (Settings → AI): iOS Keychain
generic passwords, service `app.snorkle.ai-keys`, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`
(`snorkle-content/src/webkit/keychain.rs`). They are read only to send a request to the provider and
are never returned to the chrome. Site-script requests carry the site's domain, its site script
and the user's prompt; the summary view sends the page's URL, title and visible text as numbered
blocks (§5j); page chat sends the page's URL, title, visible text and selection with the
conversation (§5g). The model's
CSS/JS lands in the editor and runs only if the user saves it; chat replies are only displayed. The app stores no passwords, tokens or cookies otherwise. Cookies live in WebKit's data stores.
Password AutoFill in WKWebView uses the system (iCloud Keychain / Passwords app). Any future
credential feature must use the iOS Keychain (`kSecClassGenericPassword` /
`kSecClassInternetPassword`, `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`) and needs a
security review. `config_hardening.rs` and `storage/` contain no credential fields by design.

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
