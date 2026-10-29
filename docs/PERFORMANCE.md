# Performance: budgets, test plan, results

**Status: nothing has been measured on hardware yet.** Everything below is a target or a
procedure. Fill in the results tables from real devices; do not quote the targets as measurements.

## Budgets (targets)

| Item | Target | Where it is set |
|---|---|---|
| Live content web views | by physical RAM: <4 GB 2 · <6 GB 3 · <8 GB 4 · <16 GB 6 · ≥16 GB 8 | `tabs::MemoryBudget::for_physical_memory` |
| Background live-view TTL | 10 min, then discard | `MemoryBudget::DEFAULT_TTL_MS` |
| On memory warning | keep only the active view | `Browser::on_memory_warning` |
| WebContent crash | auto-reload ≤ 2 per 60 s, then notice | `browser.rs` `MAX_CRASH_RELOADS` |
| `decidePolicyForNavigationAction` cost | pure function, no I/O; target < 50 µs on device | `policy.rs` |
| Chrome state pushes | coalesced; progress in 5 % steps | `Browser::on_progress` |
| Disk writes | off main thread; session writes coalesced (400 ms) | `src-tauri/src/state.rs` |
| Restore | only the active tab gets a web view | `Browser::restore_session` |

## Test plan (real hardware)

Devices: one ProMotion iPad Pro (M4 or later), one non-ProMotion iPad with ≤ 4 GB RAM (e.g. iPad
9th/10th gen or A16). Use iPadOS 27.x and the newest 26.x. Release build, attached to Instruments.

1. **Max live tabs.** Open 30 tabs of heavy pages (news sites, maps, a video site, a large web app).
   Record: app memory (Instruments, Allocations + VM Tracker), WebContent process count and memory
   (Activity Monitor template), memory warnings received, WebContent terminations, and whether
   the app itself is ever jetsam-killed. Repeat with the budget tier ±1 to find where terminations
   start. Pick the final per-tier number from this.
2. **Discard/restore fidelity.** For discarded tabs, check back/forward history, scroll position
   and form state after activation (`interactionState`).
3. **Background TTL.** Leave 6 tabs and confirm discards at about 10 minutes; check that media
   tabs are affected (known gap, OPEN_QUESTIONS #26).
4. **Cold start to first paint** of a restored session with 50 tabs: target < 1 s to interactive
   chrome, and only one web view created.
5. **Navigation-policy overhead.** Signpost (`os_signpost`) around `decide_navigation`. Report
   p50/p99 on a redirect-heavy site.

## Frame rate on ProMotion (requestAnimationFrame in WKWebView)

Unverified: whether rAF in a third-party WKWebView runs at 120 Hz on ProMotion. Safari has
historically limited rAF to 60 Hz unless a feature flag was changed, and the behaviour for apps
using WKWebView is not documented. Procedure: serve `tests/device/raf-probe.html`, open it in
Snorkle and in Safari on the same device (Low Power Mode off, "Limit Frame Rate" off in
Accessibility → Motion), run it 3 times each, and paste the JSON output below. Also record a
Core Animation FPS trace in Instruments while scrolling a long page.

| Device | iPadOS | Browser | idle fps (mean / p95 ms) | loaded fps | Notes |
|---|---|---|---|---|---|
| (not measured) | | Snorkle | | | |
| (not measured) | | Safari | | | |

## Results: memory

| Device | RAM | iPadOS | Tier | Tabs open | Live views | Mem warnings | WC terminations | App killed? |
|---|---|---|---|---|---|---|---|---|
| (not measured) | | | | | | | | |
