# Performance: budgets, test plan, results

**Status: nothing has been measured on hardware yet.** Everything below is a target or a
procedure. Fill in the results tables from real devices; do not quote the targets as measurements.

## Budgets (targets)

| Item | Target | Where it is set |
|---|---|---|
| Live content web views | by physical RAM: <4 GB 2 · <6 GB 3 · <8 GB 4 · <16 GB 6 · ≥16 GB 8 | `tabs::MemoryBudget::for_physical_memory` |
| Background live-view TTL | 10 min, then discard | `MemoryBudget::DEFAULT_TTL_MS` |
| On memory warning | keep only the active view | `Browser::on_memory_warning` |
| WebContent crash | auto-reload ≤ 2 per 60 s, then notice | `browser/mod.rs` `MAX_CRASH_RELOADS` |
| `decidePolicyForNavigationAction` cost | pure function, no I/O; target < 50 µs on device | `policy.rs` |
| Chrome state pushes | coalesced; progress in 5 % steps | `Browser::on_progress` |
| Disk writes | off main thread; session writes coalesced (400 ms) | `src-tauri/src/state/` |
| Restore | only the active tab gets a web view | `Browser::restore_session` |
| Ad-block conversion | off main thread; skipped when WebKit already has the compiled lists | `src-tauri/src/adblock.rs` |

Measured off-device (not iPad): converting EasyList + EasyPrivacy (2026-10-29 versions) takes
0.53 s in a release build on the Linux build machine, producing 127,351 rules in 3 lists (14 MB
of JSON). WebKit's compile time for those lists on an iPad has not been measured. Record it,
plus first-launch versus cached-launch time to the first page load, in the results below.

## Review against Search (2026-11-23)

The owner finds Search (searchbrowser.app, by Office Commun, MIT-licensed Swift) much quicker on the
Mac. It runs on the same engine, the macOS `WKWebView`, so pages run their scripts and render at
the same speed in both. Search's own measurements (its site, 23 Sep 2026, not independent) show
it level with Safari on rendering and JavaScript. The difference is in everything around WebKit.
What follows compares Search's source (`github.com/driceroland/search`, read at 1.0.19) with
Snorkle's. **Nothing here was measured on a Mac or an iPad.** The numbers marked *here* come from
the Linux build machine and headless Chromium and only show proportions.

Search's published figures (M3 Pro, macOS 26.6.2): 320 ms from a cold launch to the first window
(Safari 730, Chrome 526); a new tab's load starting about 10 ms after a click; five tabs in 11
processes; a sleeping tab waking in 0.2–0.25 s.

### Findings, in order of likely effect

| # | Snorkle today | Search | Likely effect | Change |
|---|---|---|---|---|
| 1 | At most 8 live pages (≥ 16 GB), and a background page is dropped after 10 min (`MemoryBudget`). Going back to it reloads from the network. macOS has no memory-pressure handling (OPEN_QUESTIONS 84). | No count limit. A tab sleeps after 30 min unseen (2 h or 10 min by setting, shorter under memory pressure) and keeps a picture of itself, shown while the page comes back. | Switching tabs: instant versus a full reload. Probably the largest difference in feel. | On macOS: listen for memory pressure (`DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`), then raise the limit and the wait to 30 min; show the tab's last picture while a dropped page reloads. |
| 2 | Every page view is masked (`CAShapeLayer`, even-odd) to cut out the pill, popups and the sidebar's holes. The holes are always reported, so the mask is always there. A layer mask makes Core Animation draw the page offscreen and composite it again, on every frame of a scroll. | The address pill is a native view above the web view. No masks. | Scrolling smoothness and GPU energy, most on large Retina and 120 Hz screens. | Now: build each mask only from the holes that cross that view; none (attached bar, sidebar only) means no mask. Later: draw the floating pill in its own small surface above the pages so no page needs a mask. |
| 3 | Black and white and hidden media are on by default: `html{filter:grayscale(1)}` on every page, a filter on every image, video, canvas and frame, and a pointer tracker that calls `elementsFromPoint` on pointer moves. | Pages are shown as they are. | Raster cost on image-heavy pages and while scrolling. Unmeasured in WebKit. | Measure first (below). If costly: apply grayscale once to the view instead of in the page, or keep these off for pages being scrolled. |
| 4 | Each content view's configuration gets its own default process pool. | One shared `WKProcessPool`, so WebKit has the next web process ready: Return or a bookmark to load start went from 41–59 ms to 9–10 ms (its note in `Tab.swift`). | New tab and link-in-new-tab latency. | One shared pool for all content views. Low risk (data stores stay separate). |
| 5 | EasyList + EasyPrivacy: 127,351 rules in 3 lists, 14 MB of JSON. 24,424 hiding rules, 13,636 of them on every page and frame (*here*). | About 40 rules: a list of ad and tracker domains, one hiding rule of about 20 selectors, a few per site. | Each page and frame gets the stylesheet built from 13,636 hiding selectors (style work, memory per frame). The first launch also compiles 14 MB of rules. The network rules make pages lighter, so keep those. | Drop the generic hiding rules and keep network blocking and per-site hiding, or move to a short curated list. Fewer ads blocked, especially empty ad boxes shown. |
| 6 | The chrome is a web page. Each state change sends the whole state (5 KB with 10 tabs, 20 KB with 40, 51 KB with 100) through the UI process's main thread, then `render()`: about 1 ms per load-progress step, 2 / 6 / 12 ms to rebuild the tab list for 10 / 40 / 100 tabs (*here*). About 25 pushes per page load. | Native AppKit chrome. | Main-thread work during every load and tab switch; small for few tabs, growing with many. | Send only what changed (the active tab's progress apart from the tab list); update changed rows instead of rebuilding the list; skip layout reads when nothing moved. |
| 7 | On every page: the selection, notebook and image-menu watchers (client world), and 1.5 s after each load the page's whole text read (`innerText`: a full layout, up to 48,000 characters back over IPC) and written to disk. | Reads nothing from a page unless asked. | Brief page jank 1.5 s after loads on large pages; IPC and disk per load. | Read page text when the page is idle, and only for pages kept open a while; start watchers on first use where the feature allows. |
| 8 | Launch: the chrome page loads about 8,000 lines of script (`chrome.js` 5,300, `notebook.js` 2,700) and makes about 20 calls before it is complete; content-blocking lookup and session restore run alongside. Unmeasured. | 320 ms to the first window; its changelog shows launch work moved out of the way (a bookmarks import had held the window 230 ms). | Time to the first usable window. | Load `notebook.js` when the notebook opens; defer downloads, site scripts, AI status and the notebook's data until after the first paint. Measure. |
| 9 | WebKit's default: page animations near 60 fps on 120 Hz screens. | The same by default; Settings offers 120 Hz always or on power (WebKit's `PreferPageRenderingUpdatesNear60FPSEnabled` flag, through private SPI). | Smoothness of page animations, at about 1.5× the energy on an animated page (Search's note). Not trackpad scrolling. | Optional setting, later. |
| 10 | Favicons of new sites are fetched with a hidden web view per site (F1). | Native fetch. | A web process per new site at launch. | Low priority; F1 was a security choice. |

### Measuring it (needed before and after each change)

Search ships a scriptable bench (`./bench`). Snorkle has only the console. The first step is
timings in Menu → Console (and `os_signpost` for Instruments) for:

- cold launch → first chrome frame → first content commit;
- click on a tab → its view shown (and → first paint, for a dropped page);
- new tab / bookmark / Return → load start → commit → finish;
- each chrome `render()` and each `set_chrome_layout`.

Then, on one Mac, against Search with the same pages: cold launch; new tab to load start; switching
to a tab left 15 minutes; scrolling a long image-heavy page (Activity Monitor's GPU and Energy
columns, or Instruments' Metal System Trace) with the floating pill and with the attached bar, and
with black and white and hidden media on and off. The last two need no code: ⋯ → Attach URL Bar,
and the palette and images buttons.

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
