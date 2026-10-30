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
| B1 | Web content → app process | page → native | **None by design.** Content `WKWebView`s get no `WKScriptMessageHandler` or `WKURLSchemeHandler`, and are not Tauri webviews, so no Tauri IPC script, invoke key or `ipc:` scheme exists in them. The only data that crosses is WebKit-generated metadata in delegate callbacks (URL, frame, navigation type, NSError, title via KVO). The only injected code is the user's own scripts and the view-mode styles (black and white, hidden images), in the isolated client world with no way to send messages back (§5c). Exception, only while console recording is on: a console hook in the page world posts page console output to one message handler (§5d, SECURITY_REVIEW C13). | `CONTENT_VIEW_SPEC` (compile-time assert in `ios/mod.rs`), `tests/content_view_spec.rs`, `src-tauri/tests/channel_tripwire.rs`, on-device `tests/device/ipc-probe.html` |
| B2 | Chrome UI → Rust commands | chrome → native | Tauri IPC with app ACL manifest (`build.rs`): every app command and core plugin command is checked against `capabilities/chrome.json`, which grants every app command in `command_list.rs` and `event:listen/unlisten` to webview label `main`, local origin only. No `remote` URLs, no window-label grants, `dynamic-acl` feature compiled out. | Tauri runtime authority; `src-tauri/tests/ipc_isolation.rs`, `config_hardening.rs` |
| B3 | Rust → chrome | native → chrome | `emit_to("main", "state" \| "toast")`. Payload is core state. Page-derived strings (titles, hosts) are sanitised in Rust (`text::sanitize_page_text`: bidi and control characters removed, length capped) and rendered with `textContent` only. | `text.rs` tests, `channel_tripwire.rs` (no `innerHTML` and similar) |
| B4 | Content pixels ↔ chrome pixels | visual | Content containers sit in their own UIViews below the toolbar, `clipsToBounds`. Element fullscreen is disabled. Interstitials, prompts and errors are drawn by the chrome while the content view is hidden, never inside page content. Chrome overlays hide content (`set_chrome_layout(overlay)`). | `ios/mod.rs`, `Browser::sync_visibility`, `browser_invariants.rs` |
| B5 | App ↔ disk | native → storage | JSON files in the app container, written atomically off the main thread. Only http(s) URLs are stored, and only from non-private tabs. Data protection `NSFileProtectionComplete` (entitlement). | `storage.rs` tests, `browser_invariants.rs::private_tabs_never_reach_storage`, fuzz `browser_events` |
| B6 | App ↔ network | content → internet | WebKit only. The app itself makes no network requests (no telemetry, no update checks). ATS stays on for the app; `NSAllowsArbitraryLoadsInWebContent` is set only so HTTP can load after explicit user consent. TLS: system validation, `PerformDefaultHandling` for every auth challenge, deprecated TLS refused. | `delegates.rs`, `config_hardening.rs` |
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
- **View modes** (`snorkle_core::viewmodes`): desaturate, hide media and headers. Each is on or
  off app-wide; a long press on its button offers "Disable for this tab", which exempts that
  tab (popups inherit their opener's exemptions, since they share its content controller). The
  host compiles the injected scripts once for the app-wide modes and once per exempted tab. The
  **headers** mode runs a constant extractor in the client world that reports the page's h1–h6
  to a client-world message handler (C14); the chrome shows them, nested by level, in a panel
  docked right of the page, and a click scrolls to the heading. The style modes use the same
  mechanism with constant styles, plus one `evaluateJavaScript` to update already-loaded pages.
  Pages are black and white with images and video hidden by default (`img`, `picture`, SVG `image`, `video`, embedded players of the common video sites,
  CSS background images); the palette button shows colour and the images button shows images
  and video. Both are session-only (`Shared::view_mode`, not persisted), so every launch starts
  that way. Hidden `<img>` and `<video>` elements may still download; CSS background images do
  not. Players inside other sites' iframes are hidden only for the listed video sites.
- **Split view:** the core keeps `splits: Vec<SplitView>`, pairs of tabs of the same profile; a
  tab is in at most one pair. The pair holding the active tab is on screen. Visibility effects
  carry a `Pane` (`Full`, `Left` or `Right`), and the host places each container in its half,
  with a 2 pt divider. The panes on screen are protected from discard; pairs not on screen can
  be discarded and are restored together when chosen. Choosing a tab outside any pair shows it
  alone and keeps the pairs. Closing a tab, pairing one of its tabs elsewhere, or unsplitting
  ends a pair. The sidebar shows each pair as one row with a half per tab; dragging a half out
  of the row unsplits it. Pairs are not saved in the session. A tap in a pane focuses it.

## 5e. Docked panels

Native page views sit above the chrome's webview, so the chrome can never draw over a page.
Panels that should leave the page visible dock beside it instead: the chrome reports `right`
and `bottom` insets and the host shrinks the content area. The site-scripts panel docks on the
right (in narrow windows it overlays and hides the page); the console docks at the bottom. A
ring around the `{ }` button marks pages an enabled site script applies to.

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
insets are clamped in Rust to at most half the window.

## 6. Credentials

The app stores no passwords, tokens or cookies itself. Cookies live in WebKit's data stores.
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
| Not selectable as default browser | Links from other apps open in Safari. |
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
