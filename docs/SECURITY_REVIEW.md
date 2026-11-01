# Security review rules

## Areas that always need a human reviewer

Code that touches the page-to-native boundary, URL handling, navigation policy or storage is
kept minimal, carries a `SECURITY-REVIEW` header stating its assumptions, and needs human
review on every change.

| Path | Why |
|---|---|
| `crates/snorkle-core/src/policy.rs` | Navigation policy |
| `crates/snorkle-core/src/browser.rs` | Address-bar URL, HTTPS-first, private-mode persistence rules |
| `crates/snorkle-core/src/url_display.rs`, `idn.rs` | Anti-spoofing display |
| `crates/snorkle-core/src/input.rs` | Address-bar input → URL |
| `crates/snorkle-core/src/storage.rs` | Persistence |
| `crates/snorkle-core/src/console.rs` | In-app console: page console messages (C13) |
| `crates/snorkle-core/src/ai.rs`, `src-tauri/src/ai.rs`, `crates/snorkle-content/src/ios/http.rs`, `ios/keychain.rs` | AI provider requests, API keys in the Keychain |
| `crates/snorkle-core/src/viewmodes.rs` | View modes: constant injected sources, headings parser (C14) |
| `crates/snorkle-core/src/userscripts.rs`, `crates/snorkle-content/src/ios/injection.rs` | Script injection into content (user scripts, view modes: black and white, hidden images) |
| `crates/snorkle-core/src/blocking.rs`, `crates/snorkle-content/src/ios/rules.rs`, `src-tauri/src/adblock.rs` | Content blocking, allowlist regex generation, WebKit rule store |
| `crates/snorkle-content/src/lib.rs` (`CONTENT_VIEW_SPEC`) | Content web view configuration |
| `crates/snorkle-content/src/ios/*.rs` | Native boundary, unsafe FFI, hand-written bindings |
| `src-tauri/src/commands.rs`, `command_list.rs`, `build.rs` | Command surface and ACL manifest |
| `src-tauri/capabilities/*`, `tauri.conf.json` | ACL and CSP |
| `src-tauri/Info.ios.plist`, `src-tauri/ios/Entitlements.plist` | ATS, entitlements |
| `ui/chrome.js` | Renders page-controlled strings |

## Page-to-native channel register

**Adding any channel through which web content can send data to native code requires the
owner's written sign-off before merge.** Channels include a `WKScriptMessageHandler`, a
`WKUserScript`, a URL scheme handler, `evaluateJavaScript` results, a returned popup web view,
downloads, or a new Tauri webview/command reachable by content. The tripwire tests
(`src-tauri/tests/channel_tripwire.rs`, `crates/snorkle-content/tests/content_view_spec.rs`, and
the compile-time assert in `crates/snorkle-content/src/ios/mod.rs`) fail when such code appears.
The fix that lands with the sign-off updates them.

Current channels (all carry WebKit-generated data, none carry page-chosen messages):

| # | Channel | Data reaching native | Page influence | Handling |
|---|---|---|---|---|
| C1 | `decidePolicyForNavigationAction` | URL, target frame main/sub, navigation type, modifier flags (iOS 18.4+) | Page chooses the URL | WHATWG-parsed; unparseable → cancel; `policy::decide`. Since 2026-11-01 a link activation with ⌘ held (WebKit's `modifierFlags`; pages cannot set them) is cancelled and opened in a background tab (`Browser::on_background_tab_request`: http/https only, `policy::decide` with HTTPS-first, opener's profile) |
| C2 | `decidePolicyForNavigationResponse` | `canShowMIMEType` | Server chooses the MIME type | Non-renderable → cancel |
| C3 | provisional start / commit / finish / fail | URL at commit, NSError domain/code/description | Server influences the error text | Text sanitised and length-capped; certificate errors classified by code |
| C4 | KVO `URL`, `title`, `estimatedProgress`, `canGoBack/Forward`, `hasOnlySecureContent` | Those values | Page controls title and same-document URL | Title sanitised; URL accepted only if same-origin and not provisional |
| C5 | `createWebViewWithConfiguration` | Popup URL | Page chooses it (requires a user gesture) | Policy-checked new tab. Since 2026-10-30 (to fix OAuth logins) returns a real web view built from WebKit's configuration, so `window.opener` works where WebKit allows it (WebKit applies `noopener` to `target=_blank` links and pages that ask for it). The opener can be navigated by its popup (tabnabbing), but the address bar always shows each tab's committed URL. Since 2026-10-30 a popup that asks for a window size is shown as a sheet over its opener (`Pane::Sheet`): the toolbar shows the sheet's committed URL and a "Pop-up" marker, and a native backdrop blocks touches to the opener. |
| C11 | `webViewDidClose` | Tab id | Page calls `window.close()` | Honoured only for popups; focus returns to the opener |
| C6 | JS alert/confirm/prompt | Frame's security-origin host | Page triggers it | Suppressed; host shown in a toast, at most 20 per view |
| C7 | Media capture / device-motion permission | Request type | Page triggers it | WebKit system prompt (`.prompt`) |
| C8 | `webViewWebContentProcessDidTerminate` | Tab id | Page can cause a crash | Bounded reload, then a notice |
| C9 | Auth challenge / deprecated TLS | Challenge | Server | Always default handling / refuse |
| C10 | Tap on a content pane (`UITapGestureRecognizer`) | Tab id only | None (native UI event, not page-generated) | Focuses that tab in split view |
| C13 | Page console capture: `WKScriptMessageHandler` `snorkleConsole`, page world, all frames, **only while the user has Record on** in the console (default on in debug builds only, off in release; session-only) | A string: level letter + text | Full: the hook replaces `console.*` in the page's own world, and any page or frame can post anything to the handler | String bodies only. Attributed by WebKit (`frameInfo` origin, sending web view → tab). Sanitised per line (bidi/control/invisible removed), ≤ 4,000 chars and 40 lines, ≤ 100 messages per tab per 10 s, ≤ 1,000 entries kept, in memory only, private-tab entries dropped with the private session. Only stored and rendered with `textContent`; nothing acts on it. The handler's presence is visible to pages while recording (fingerprintable). Installed and removed in `ios/injection.rs` only. |
| C14 | Headers view: `WKScriptMessageHandler` `snorkleHeadings` in the **client world** (not reachable from page scripts), main frame, only while headers mode is on for the tab | A JSON string `[[level, text, index], …]` from the constant extractor | Indirect: the page decides which headings exist and their text | String bodies only; strict parse (≤ 96 KB, ≤ 300 entries, level 1–6, non-empty text sanitised to ≤ 200 chars, integer index). Rendered with `textContent`. Clicking scrolls via constant code taking only the integer index. | Since 2026-11-01 it also reports `{"c": index}` (the heading scrolled to; integer only) and, when the user presses "Detect Headers", `{"sample": [...]}`: candidate elements' tag, id, up to 4 classes, font size/weight, whether uppercase, parent tag/classes, enclosing section element (tag, classes, `aria-label`; ≤ 80 chars, since 2026-11-01) and up to 80 chars of text, at most 200, re-built field by field and sanitised in `viewmodes::parse_report`. The sample goes to the user's AI provider (N1); the returned selectors are validated (`viewmodes::validate_rule`), stored per site (`heading-rules.json`) and only passed to `querySelectorAll` / `matches` in the client world.
| C15 | Pull to refresh (`UIRefreshControl` on each content scroll view) | Tab id only | None (native UI gesture) | Asks the core to reload the tab; the spinner stops on finish/fail or after 15 s |
| C16 | Links from other apps (default browser; tao `scene:openURLContexts:` → `RunEvent::Opened`) | A URL | Another app chooses it | http/https only, no userinfo, ≤ 8 KB; opened in a new default-profile tab through `policy::decide` (HTTPS-first applies). Logged in the console. |
| C17 | AI chat page text: `evaluateJavaScript` **with a completion handler** (`injection::read_now`), client world, main frame, only when the user sends a chat message | The string result of the constant `ai::PAGE_TEXT_JS`: `{"t": title, "x": innerText, "s": selection}` | Full: the page controls its text (prompt injection aimed at the model is possible) | Non-strings ignored; truncated to `MAX_PAGE_TEXT_BYTES` (~212 KB) natively, then `ai::parse_page_text`: JSON with three strings, each cleaned (bidi/control/invisible removed) and capped (48,000 / 300 / 4,000 chars). The URL comes from the core. Sent to the user's AI provider (N1) inside the system prompt, labelled as data with an instruction to ignore instructions in it. The reply is cleaned, capped and shown with `textContent`; nothing acts on it. |
| C18 | Find in page (⌘F): WebKit's `findString:withConfiguration:` (a "found" flag), and a match count read back from `evaluateJavaScript` with a completion handler (`injection::read_now`), client world, main frame, only while the user types in the find bar | A boolean; a string from the constant `find::count_js` with the user's query embedded as an escaped JSON literal | Indirect: the page's text decides the count | Non-strings ignored; `find::parse_count` accepts 1–5 ASCII digits only, capped at 1,000. Shown as a number. The query is the user's (sanitised, ≤ 200 chars). |
| C19 | Selection menu: `WKScriptMessageHandler` `snorkleSelection` in the **client world**, main frame, every page; constant watcher `selection::watch_js` (2026-11-01) | A JSON string: `null` or the selection's rectangle and the visual viewport's width, CSS px | Indirect: the page decides what is selectable and where | String bodies only; `selection::parse_report`: ≤ 256 bytes, exactly 4 finite numbers within ±100,000 (width and height ≥ 0) and a width ≥ 1, or `null`. Used only to place the chrome's menu (a hole). |
| C20 | Selected text: `evaluateJavaScript` with a completion handler (`injection::read_now`, constant `selection::TEXT_JS`), when the selection menu appears | A string, ≤ 2,000 chars | Full (page text) | Truncated natively, cleaned and capped (`selection::clean_text`). Used only as a Google query in a new tab, a chat draft the user edits and sends, or a dictionary term (system `UIReferenceLibraryViewController`, ≤ 100 chars). |
| C12 | Keyboard shortcuts (`crate::SHORTCUTS`: ⌘T, ⌘W, ⌘F, ⌘S, ⌘\, ⌘⇧\, ⌘⇧D, ⌘1–5). Since 2026-11-01 commands of the app's main menu (`ios/menu.rs`, methods added to tao's app-delegate class), with `UIKeyCommand`s on content containers only as a fallback | Which shortcut (the command's property list) | None (hardware keyboard via UIKit; pages cannot synthesise it) | Forwarded to the chrome as a shortcut name |

Native → page code paths (not channels back to native, but reviewed the same way):

| # | Path | What runs | Constraints |
|---|---|---|---|
| I1 | User scripts and styles (`WKUserScript`) | User-written CSS/JS for domains they chose | Isolated default client world; main frame only; guarded by the document's real `location.hostname`; CSS and domain embedded as escaped JSON literals; no message handler or `GM_*` API. Every saved script's source is present (guarded) in every page's client world. |
| F1 | Pinned-grid favicons (`ios/favicon.rs`) | Fixed HTML `<img src="https://<validated host>/favicon.ico">` in a hidden web view: JS off, non-persistent store, only `about:blank` navigation allowed | WebKit decodes the image in its sandbox; the app receives snapshot pixels and re-encodes a PNG. Storage accepts only PNGs ≤ 64 KB, and the chrome receives them as `data:` URLs (`img-src 'self' data:`, the only `data:` in its CSP). App-initiated request: one per pinned host. |
| N1 | AI requests (only with a user-supplied key) | HTTPS to `api.openai.com` / `api.anthropic.com` only; ephemeral session | Key from the Keychain, never sent to the chrome. Body: site domain, current site script, user prompt; for Detect Headers the C14 sample; for page chat (2026-11-01) the conversation plus the page's URL, title, visible text and selection (C17), read only when the user sends a message. Reply parsed strictly (`snorkle_core::ai`, ≤ 2 MB); generated code is shown for review, never run automatically. Each successful request's token counts and estimated cost (time, provider, model; no content) are kept in `ai-usage.json` (≤ 20,000 entries) for Settings → AI → Usage, until reset. |
| I2 | View modes (`viewmodes.rs`): desaturate (default on), hide media (default on, all frames since 2026-11-01), headers (per tab); app-wide with per-tab exemptions | Constant styles (`GRAYSCALE_CSS`, `NO_MEDIA_CSS`), via user script and one `evaluateJavaScript` (no completion handler). Since 2026-11-01 hide media also runs a pointer tracker that sets `data-snorkle-reveal` on the media under the pointer (so media under overlays fade in), and scrolling to a heading highlights it with a Web Animation | Same world and frame constraints; takes no input except a heading index. Session-only state |
| L1 | Floating chrome (2026-11-01): the chrome reports the rectangles of the URL pill, its popup, the find bar, the ⌘W confirmation, toasts, and, with the sidebar closed, the left-edge tab or the sidebar pop-over (`set_chrome_layout` `holes`, each with a corner radius and which corners are rounded) | Content containers and the sheet backdrop get a layer mask with those holes and refuse touches inside them (`pointInside:withEvent:`), so the chrome's web view underneath shows through | Holes are validated (`sanitize_holes`: finite, inside the window, ≤ 8, overlaps merged). A page is never drawn in a hole, so it cannot draw over or under the address bar; it can still draw anything around it, as before. |

Sign-off log:

| Date | Change | Approved by |
|---|---|---|
| 2026-10-29 | Content web views built with objc2-web-kit instead of wry (Tauri/wry cannot host child webviews on iOS) | Owner (in session) |
| 2026-10-30 | Popups keep `window.opener` (C5) to fix OAuth logins, owner request; favicons via sandboxed snapshot (F1), owner choice; native keyboard shortcuts (C12) | Owner (in session) |
| 2026-10-29 | Script injection into content: user scripts/styles per domain and black-and-white mode (I1, I2), confined to `ios/injection.rs` (tripwire allowlist) | Owner (feature request in session) |
| 2026-10-30 | Black and white on by default, colour and hidden images as session-only toggles (I2 extended with a second constant style), owner request | **Pending owner sign-off** |
| 2026-10-30 | AI connections (OpenAI, Anthropic): app-initiated requests (N1), API keys in the Keychain, AI-drafted site scripts | Owner (feature request in session); **written sign-off pending** |
| 2026-10-30 | View modes with per-tab exemptions; headers view (C14, client-world handler) | Owner (feature request in session); **written sign-off pending** |
| 2026-11-01 | Page chat (C17: page text read back from `evaluateJavaScript` and sent to the AI provider), ⌘-click background tabs (C1 modifier flags), floating URL pill over the page (L1), Detect Headers sections (C14 sample adds `up` and `sec`) | Owner (feature request in session); **written sign-off pending** |
| 2026-11-01 | Chat archive: page text, URL, selection and messages of chats from non-private tabs stored in `chats/` (`storage::ChatRecord`, validated on write and load, ids are file names so restricted to `c[0-9-]`); dark view mode (constant style and detector, I2; `overrideUserInterfaceStyle` on content views); animated content frames | Owner (feature request in session); **written sign-off pending** |
| 2026-11-01 | Selection menu (C19 watcher + handler on every page, C20 selected text), system share sheet and dictionary (native `UIActivityViewController` / `UIReferenceLibraryViewController` presented by the host; the share URL comes from the core), user-editable shortcut bindings (validated, `effective_bindings`), chrome focus (`becomeFirstResponder` on the chrome's web view) | Owner (feature request in session); **written sign-off pending** |
| 2026-11-01 | Find in page (C18: match count read back, digits only); shortcuts moved to the main menu (C12); more floating chrome as holes (L1) | Owner (feature request in session); **written sign-off pending** |
| 2026-10-30 | In-app console with page console capture (C13): a page-world script and message handler, on demand, to debug sign-in popups without a Mac. Tripwire allowlist extended for `ios/injection.rs` | Owner (feature request in session); **written sign-off pending** |

## Checklist for AI-assisted changes in review-required areas

- State the security assumption in the file header or next to the code.
- Keep it minimal: no speculative options, no new dependencies without `docs/DEPENDENCIES.md`.
- Add or extend a test in `tests/` and, for parsers or policies, a fuzz target invariant.
- Fail closed on parse errors and unexpected states.
- Run `cargo test --workspace`, `scripts/check-ios.sh` and `scripts/check-deps.sh`, and fuzz the
  touched target for at least 5 minutes.
- List anything about Apple or Tauri behaviour that you assumed but could not verify in
  `docs/OPEN_QUESTIONS.md`.
