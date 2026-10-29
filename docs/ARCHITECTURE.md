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
│              No IPC bridge, no user scripts, no message handlers, no schemes.   │
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
| B1 | Web content → app process | page → native | **None by design.** Content `WKWebView`s get no `WKScriptMessageHandler`, `WKUserScript` or `WKURLSchemeHandler`, and are not Tauri webviews, so no Tauri IPC script, invoke key or `ipc:` scheme exists in them. The only data that crosses is WebKit-generated metadata in delegate callbacks (URL, frame, navigation type, NSError, title via KVO). | `CONTENT_VIEW_SPEC` (compile-time assert in `ios/mod.rs`), `tests/content_view_spec.rs`, `src-tauri/tests/channel_tripwire.rs`, on-device `tests/device/ipc-probe.html` |
| B2 | Chrome UI → Rust commands | chrome → native | Tauri IPC with app ACL manifest (`build.rs`): every app command and core plugin command is checked against `capabilities/chrome.json`, which grants the 18 app commands and `event:listen/unlisten` to webview label `main`, local origin only. No `remote` URLs, no window-label grants, `dynamic-acl` feature compiled out. | Tauri runtime authority; `src-tauri/tests/ipc_isolation.rs`, `config_hardening.rs` |
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
| Popups / tabnabbing | A1 | `javaScriptCanOpenWindowsAutomatically = false`. `createWebView` always returns nil and opens a policy-checked tab, so there is never a `window.opener`. | Breaks OAuth popups relying on `opener.postMessage` (OPEN_QUESTIONS #14). |
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
5. **Popups:** `createWebView` → `on_new_window_request` → policy → new tab in the same profile.

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
  inside its own processes, with no per-request work in the app. No scripts are injected, so
  scriptlet filters are unsupported.
- **Per-site toggle:** stored by registrable domain (`adblock-allow.json`). It becomes one
  trailing `ignore-previous-rules` rule with `if-top-url`, then the lists are rebuilt and the tab
  reloaded. Disabled in private tabs, because the setting is persisted.
- **Security:** a rule list can only block, hide or upgrade content, never run code. Allowlist
  entries are validated to `[a-z0-9-.]` domains before they go into a regex. Rule lists are not a
  page-to-native channel.

## 5b. Chrome layout

Tabs are listed in a sidebar along the left edge. In windows at least 700 pt wide the sidebar is
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
