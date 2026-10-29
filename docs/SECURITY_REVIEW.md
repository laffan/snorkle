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
| C1 | `decidePolicyForNavigationAction` | URL, target frame main/sub, navigation type | Page chooses the URL | WHATWG-parsed; unparseable → cancel; `policy::decide` |
| C2 | `decidePolicyForNavigationResponse` | `canShowMIMEType` | Server chooses the MIME type | Non-renderable → cancel |
| C3 | provisional start / commit / finish / fail | URL at commit, NSError domain/code/description | Server influences the error text | Text sanitised and length-capped; certificate errors classified by code |
| C4 | KVO `URL`, `title`, `estimatedProgress`, `canGoBack/Forward`, `hasOnlySecureContent` | Those values | Page controls title and same-document URL | Title sanitised; URL accepted only if same-origin and not provisional |
| C5 | `createWebViewWithConfiguration` | Popup URL | Page chooses it (requires a user gesture) | Policy-checked new tab; returns nil (no opener) |
| C6 | JS alert/confirm/prompt | Frame's security-origin host | Page triggers it | Suppressed; host shown in a toast, at most 20 per view |
| C7 | Media capture / device-motion permission | Request type | Page triggers it | WebKit system prompt (`.prompt`) |
| C8 | `webViewWebContentProcessDidTerminate` | Tab id | Page can cause a crash | Bounded reload, then a notice |
| C9 | Auth challenge / deprecated TLS | Challenge | Server | Always default handling / refuse |

Sign-off log:

| Date | Change | Approved by |
|---|---|---|
| 2026-10-29 | Content web views built with objc2-web-kit instead of wry (Tauri/wry cannot host child webviews on iOS) | Owner (in session) |

## Checklist for AI-assisted changes in review-required areas

- State the security assumption in the file header or next to the code.
- Keep it minimal: no speculative options, no new dependencies without `docs/DEPENDENCIES.md`.
- Add or extend a test in `tests/` and, for parsers or policies, a fuzz target invariant.
- Fail closed on parse errors and unexpected states.
- Run `cargo test --workspace`, `scripts/check-ios.sh` and `scripts/check-deps.sh`, and fuzz the
  touched target for at least 5 minutes.
- List anything about Apple or Tauri behaviour that you assumed but could not verify in
  `docs/OPEN_QUESTIONS.md`.
