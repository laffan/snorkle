# Snorkle: architecture and threat model

Status: initial shell, 2026-10-29. Targets iPadOS 27 (current) and 26 (prior).
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
| `crates/snorkle-content` | Creates, lays out and destroys the per-tab `WKWebView`s; maps profiles to `WKWebsiteDataStore`s; implements `WKNavigationDelegate`/`WKUIDelegate`/KVO and forwards WebKit metadata to the core; installs the compiled content rule list. iOS only (a stub elsewhere). | Trusted code that handles untrusted data. |
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

## 2. Trust boundaries

| # | Boundary | Direction | Mechanism | Enforced by |
|---|---|---|---|---|
| B1 | Web content → app process | page → native | **None by design.** Content `WKWebView`s get no `WKScriptMessageHandler` or `WKURLSchemeHandler`, and are not Tauri webviews, so no Tauri IPC script, invoke key or `ipc:` scheme exists in them. The only data that crosses is WebKit-generated metadata in delegate callbacks (URL, frame, navigation type, NSError, title via KVO). The only injected code is the user's own scripts and the view-mode styles (black and white, hidden images), in the isolated client world with no way to send messages back (§5c). Exception, only while console recording is on: a console hook in the page world posts page console output to one message handler (§5d, SECURITY_REVIEW C13). Exception, only when the user sends a page-chat message: the result of one constant script (title, visible text, selection) is read back (§5g, C17). | `CONTENT_VIEW_SPEC` (compile-time assert in `ios/mod.rs`), `tests/content_view_spec.rs`, `src-tauri/tests/channel_tripwire.rs`, on-device `tests/device/ipc-probe.html` |
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
| Drive-by downloads | A1 | Not supported: download navigations and non-renderable responses are cancelled. | – |
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
  (`snorkle-content/src/ios/rules.rs`).
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

- **User scripts and styles** (`snorkle_core::userscripts`, `snorkle-content/src/ios/injection.rs`)
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
  **Dark** (off by default; moon button) makes content web views prefer a dark appearance
  (`overrideUserInterfaceStyle`), so sites with their own dark theme use it; a constant detector
  then checks the page's background (root, body, else the canvas) and, if it is light, marks the
  root so a constant style inverts the page with hues kept (`invert(1) hue-rotate(180deg)`).
  Until the detector has run, the page is dimmed to 20% brightness so white pages do not flash
  while loading. Images, video and embeds are never shown inverted and are dimmed to 60% until
  pointed at (with hide media on, its gray boxes stay, and what it reveals is shown un-inverted). With black and white also on, the root filter includes `grayscale`.
  Pages are black and white with media hidden by default: images, video, canvas, embedded
  objects and video-player iframes keep their boxes but are drawn as gray rectangles; pointing
  at one fades it in (0.3 s) in black and white. Media under an overlay (video players, images
  behind links) never get `:hover`, so a pointer tracker marks whatever media are under the
  pointer (`elementsFromPoint`, plus video boxes by geometry). CSS background images are
  dropped, and videos are paused when they start unless pointed at. The hide-media style runs in every frame and re-attaches itself if
  a page removes it. The palette button shows colour and the images button shows images and
  video. Both are session-only (`Shared::view_modes`, not persisted), so every launch starts
  that way. Hidden `<img>` and `<video>` sources still download (hover needs them); CSS
  background images do not.
- **Split view:** the core keeps `splits: Vec<SplitView>`, pairs of tabs of the same profile; a
  tab is in at most one pair. The pair holding the active tab is on screen. Visibility effects
  carry a `Pane` (`Full`, `Left` or `Right`), and the host places each container in its half,
  with a 2 pt divider. The panes on screen are protected from discard; pairs not on screen can
  be discarded and are restored together when chosen. Choosing a tab outside any pair shows it
  alone and keeps the pairs. Closing a tab, pairing one of its tabs elsewhere, or unsplitting
  ends a pair. The sidebar shows each pair as one row with a half per tab; dragging a half out
  of the row unsplits it. Pairs are not saved in the session. A tap in a pane focuses it.

## 5f. URL bar

The address bar is a pill floating over the bottom of the page (pages run the full height of
the window): share, back, forward and reload on its left (the sidebar's toggle is a tab on the sidebar's own
edge); the address (about 400 pt wide; full
width in narrow windows); then a ⋯ button that opens a popup with the page actions (ad blocking,
bookmark, site scripts), the per-tab headers-view button and the chat button. Toasts and banners
appear above the pill. With a pointer device the pill and the sidebar sit at 50% opacity until
hovered.

Page views are drawn above the chrome's web view, so the chrome cannot simply draw over a page.
Instead it reports the rectangles of the pill, the open actions popup and any toast or banner as
holes (`set_chrome_layout`, window coordinates with a corner radius). The host masks every
content container (and the sheet backdrop) with those holes cut out (`CAShapeLayer`, even-odd)
and the containers report points inside a hole as outside themselves, so touches and pointer
hover there reach the chrome. The strip around the pill is transparent and takes no input: the
page shows and is used there. While the console is open the page ends above it as before. The
pill centres over the page, between the sidebar and any docked panel. ⌘-click on a link opens it
in a new tab after the current one without switching to it (§4).

The same holes carry the other chrome that floats over the page: the find bar (⌘F), attached to
the top of the pill (its hole ends where the pill's begins and has only its top corners rounded,
since overlapping holes are merged into their bounding rectangle); the confirmation ⌘W shows on a
bookmark's tab; and, with the sidebar closed, a tab on the left edge and the sidebar pop-over it
opens (§5b). Find uses WebKit's own `findString:withConfiguration:` (selects and scrolls to each
match, wrapping) plus a match count read back from a constant script (C18).

Keyboard shortcuts (`snorkle_content::SHORTCUTS`) are key commands vended by the app delegate
(`keyCommands`, added to tao's class at runtime), the last responder in every chain, so they
work whatever has focus, including nothing (sidebar hidden, no page). They are not menu-bar
items any more (as menu items they stopped working, apparently from the menu system's
validation and key-equivalent conflicts, e.g. ⌘I with Format → Italic); the menu bar only loses
the system's Close and Find. Recording a binding is native too: while recording, the app delegate
offers every accepted ⌘ combination and Escape and reports the one pressed. Shortcuts: ⌘T new tab, ⌘W close tab (a
pin's tab never; a bookmark's tab after a confirmation, removing the bookmark), ⌘F find, ⌘S or
⌘\ sidebar, ⌘⇧\ right panels, ⌘D bookmark (on a bookmark's tab: un-bookmark), ⌘I images,
⌘1–5 sidebar items. The sidebar has two modes (Settings → General, or ⌘-click its toggle):
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

Bookmarking a tab (⌘D, the ⋯ menu, or dragging it into the bookmarks) moves it into the
bookmark: the tab becomes that bookmark's tab (`Browser::bind_launcher`) instead of a second tab
appearing when the bookmark is opened. Un-bookmarking makes it an ordinary tab again.

The share button (between the sidebar toggle and Back) opens the system share sheet for the
page's committed URL, as a popover from the button (so Instapaper and other share extensions
appear). Selecting text on a page shows a small menu above it, cut out of the page: Google This
(a new tab), Ask AI (opens the chat with the quote as a draft) and Define (the system
dictionary). A watcher in the page's client world reports where the selection is (C19); the text
is read when the menu appears (C20).

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

Tabs and bookmarks are listed in a resizable sidebar along the left edge (200–420 pt, at most half the window). Dragging a tab into the bookmarks bar bookmarks it there, bookmarks reorder by dragging, dragging one tab onto another pairs them in split view, and dragging a tab out of its pair unsplits it. A tab's close button appears on hover (on the active tab on touch-only devices); so does a pin's unpin button. Each pin and bookmark has its own tab (`Tab::launcher`, saved in the session): opening it returns to that tab, which is not listed with the other tabs, and the pin or bookmark is shown as selected; removing the pin or bookmark lists the tab again. New Tab (⌘T) opens an Arc-style command palette centred over the content area (which is hidden while it is open): typing fuzzy-matches open tabs, pins and bookmarks, and the last row searches or goes to the text in a new tab; no tab exists until a row is chosen. The bottom bar holds the menu (New Tab, New Private Tab, History, Console, Settings), colour, and show images and video. The chrome itself is black and white (grey palette, plus `filter: grayscale(1)` on its root for favicons and emoji). Settings deletes cache, cookies, site data, or logins (cookies and site data) from every profile's store. In windows at least 700 pt wide the sidebar is
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
(`snorkle-content/src/ios/keychain.rs`). They are read only to send a request to the provider and
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
| Entitlement key present but not provisioned | The build fails to sign. Remove the key from `ios/Entitlements.plist` for such builds. |

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
