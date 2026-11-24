// Browser chrome (see core.js): The sidebar: modes, resizing, the flyout, the menu and Settings.
'use strict';

// ---- sidebar ----------------------------------------------------------

// Wide windows: the sidebar is docked and content starts to its right.
// Narrow windows (Split View / Slide Over): it slides over the content
// area, which is hidden while it is open (overlay), and closes after a
// tab is chosen.
const NARROW_PX = 700;
const MIN_SIDEBAR = 200;
const MAX_SIDEBAR = 420;
let narrow = window.innerWidth < NARROW_PX;
// The sidebar has two modes (remembered): `attached`, a full-height
// column beside the page that the toggle shows and hides, and `flyout`,
// hidden until the pointer rests on the toggle tab and gone again when it
// leaves. In narrow windows it slides over the page (mode ignored).
let sidebarMode = 'attached';
let sidebarShown = true;


// Appearance (Settings → General): system, light or dark. The chrome's
// colours follow `data-theme`; the host sets the window's interface
// style, so system UI (share sheet, keyboard) matches. Pages keep their
// own appearance (light, or dark in dark pages mode; 2026-11-15).
function applyAppearance(mode) {
  if (!['system', 'light', 'dark'].includes(mode)) mode = 'system';
  if (mode === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = mode;
  $('appearance').value = mode;
  T.core.invoke('set_appearance', { mode }).catch(() => {});
  // Favicons too dark for a dark chrome show their light copy.
  if (typeof iconsThemeChanged === 'function') iconsThemeChanged();
}
applyAppearance(pref('appearance') || 'system');
// Settings → General → Swipe between pages (WebKit's back/forward swipe
// on pages; remembered here, sent at launch).
function applySwipeNav(on) {
  document.querySelectorAll('[data-swipe-nav]').forEach((b) => b.setAttribute('aria-pressed', String((b.dataset.swipeNav === 'on') === on)));
  T.core.invoke('set_swipe_navigation', { on }).catch(() => {});
}
applySwipeNav(pref('swipeNav') !== '0');
document.querySelectorAll('[data-swipe-nav]').forEach((b) => b.addEventListener('click', () => {
  const on = b.dataset.swipeNav === 'on';
  setPref('swipeNav', on ? '1' : '0');
  applySwipeNav(on);
}));

// Settings → General → Pull to refresh (iPadOS only; remembered here,
// sent at launch).
function applyPullRefresh(on) {
  document.querySelectorAll('[data-pull-refresh]').forEach((b) => b.setAttribute('aria-pressed', String((b.dataset.pullRefresh === 'on') === on)));
  T.core.invoke('set_pull_to_refresh', { on }).catch(() => {});
}
$('pull-refresh-option').hidden = macOS;
applyPullRefresh(pref('pullRefresh') !== '0');
document.querySelectorAll('[data-pull-refresh]').forEach((b) => b.addEventListener('click', () => {
  const on = b.dataset.pullRefresh === 'on';
  setPref('pullRefresh', on ? '1' : '0');
  applyPullRefresh(on);
}));

// Settings → General → Links to apps (remembered here, sent at launch):
// off (default), WebKit never hands a page to an installed app by itself
// (Google Docs and Sheets jumped to their apps); ⋯ → Open in App does it
// on request (iPadOS).
function applyAppLinks(on) {
  document.querySelectorAll('[data-app-links]').forEach((b) => b.setAttribute('aria-pressed', String((b.dataset.appLinks === 'on') === on)));
  T.core.invoke('set_open_app_links', { on }).catch(() => {});
}
applyAppLinks(pref('appLinks') === '1');
document.querySelectorAll('[data-app-links]').forEach((b) => b.addEventListener('click', () => {
  const on = b.dataset.appLinks === 'on';
  setPref('appLinks', on ? '1' : '0');
  applyAppLinks(on);
}));
// AppKit has no "universal links only" open: iPadOS only.
$('open-in-app').hidden = macOS;
$('app-links-option').hidden = macOS;
$('open-in-app').addEventListener('click', withTab((t) => {
  setActions(false);
  if (!t.url) { toast('Open a page first.'); return; }
  T.core.invoke('open_in_app', { tab: t.id })
    .then((opened) => { if (!opened) toast(`No installed app opens ${t.url.registrable}.`); })
    .catch((e) => toast(String(e)));
}));

$('appearance').addEventListener('change', () => {
  setPref('appearance', $('appearance').value);
  applyAppearance($('appearance').value);
});

// Settings → General → Appearance → Night vision (2026-11-17): the
// browser and its pages in shades of red only, for the dark. Pages get
// it from Rust (a launch mode, `viewmodes::NIGHT_CSS`, stored, so the
// first page of a launch already has it); the chrome draws its own two
// layers (chrome.css). The choice is mirrored here so the chrome is red
// from its first frame, before Rust has answered `get_view_mode`. Menu →
// Turn On/Off Night Vision (2026-11-24; it was an eye at the sidebar's
// foot) and ⌘⇧R turn the same setting on and off.
function applyNightVisionChrome(on) {
  const have = $('night-vision-grey');
  if (on && !have) {
    for (const id of ['night-vision-grey', 'night-vision-red']) {
      const d = document.createElement('div');
      d.id = id;
      d.className = 'night-vision-layer';
      d.setAttribute('aria-hidden', 'true');
      document.body.appendChild(d);
    }
  } else if (!on && have) {
    have.remove();
    $('night-vision-red')?.remove();
  }
  $('night-vision').checked = on;
  $('menu-night').firstChild.textContent = on ? 'Turn Off Night Vision ' : 'Turn On Night Vision ';
  setPref('nightVision', on ? '1' : '0');
}
applyNightVisionChrome(pref('nightVision') === '1');
function setNightVision(on) {
  applyNightVisionChrome(on);
  applyViewModes(T.core.invoke('set_launch_mode', { mode: 'night', on }));
}
$('night-vision').addEventListener('change', () => setNightVision($('night-vision').checked));
$('menu-night').addEventListener('click', () => { closeMenu(); setNightVision(!$('night-vision').checked); });

function loadSidebarPrefs() {
  sidebarMode = pref('sidebarMode') === 'flyout' ? 'flyout' : 'attached';
  sidebarShown = !narrow && sidebarMode === 'attached' && pref('sidebarOpen') !== '0';
}
loadSidebarPrefs();
barAttached = false;
document.body.classList.toggle('bar-attached', barAttached);

// The page starts to the sidebar's right (attached and shown, wide window).
function sidebarDocked() {
  return !narrow && sidebarMode === 'attached' && sidebarShown;
}
function flyoutMode() {
  return !narrow && sidebarMode === 'flyout';
}

function clampWidth(w) {
  return Math.round(Math.max(MIN_SIDEBAR, Math.min(MAX_SIDEBAR, w, window.innerWidth * 0.5)));
}

function setSidebarWidth(w) {
  document.documentElement.style.setProperty('--sidebar-w', clampWidth(w) + 'px');
}

// The one place the sidebar's state changes. Everything else (⌘\, the
// toggle, hover, Settings, narrow windows) calls this.
function setSidebarState(mode, shown) {
  clearTimeout(flyTimer);
  clearTimeout(hoverTimer);
  const changed = mode !== sidebarMode || shown !== sidebarShown;
  sidebarMode = mode;
  sidebarShown = shown;
  if (!narrow) {
    setPref('sidebarMode', mode);
    if (mode === 'attached') setPref('sidebarOpen', shown ? '1' : '0');
  }
  if (!shown) closeMenu();
  renderSidebarMode();
  if (changed) applySidebar(true);
  // A confirmation beside a sidebar row follows the sidebar (or goes
  // back above the pill), once it has finished sliding.
  if (changed && confirmAction && confirmAction.anchor) {
    placeConfirm();
    setTimeout(() => { if (confirmAction && confirmAction.anchor) { placeConfirm(); reportLayout(); } }, 270);
  }
}

// Show or hide, keeping the mode.
function setSidebar(open) {
  setSidebarState(sidebarMode, open);
}

// ⌘\ / clicking the toggle: attached → show or hide; flyout (hidden
// or showing) → attached and shown.
function toggleSidebar() {
  if (narrow || sidebarMode === 'attached') setSidebarState(sidebarMode, !sidebarShown);
  else setSidebarState('attached', true);
}

// `animate`: the page and its cut-outs slide with the sidebar (the host
// animates them over the same 0.25 s as the CSS).
let slideStartedAt = 0;
// The edge tab shows only in flyout mode (2026-11-03). Exceptions, so
// the tabs can always be reached: narrow windows (it opens the overlaid
// sidebar) and, on touch-only devices, an attached sidebar that is
// hidden (no keyboard for the sidebar shortcut, no pointer for a flyout).
const hoverDevice = () => matchMedia('(any-hover: hover)').matches;
function edgeAway() {
  return !narrow && !flyoutMode() && (sidebarShown || hoverDevice());
}

function applySidebar(animate) {
  const b = document.body.classList;
  b.toggle('narrow', narrow);
  b.toggle('sb-flyout', flyoutMode());
  b.toggle('sb-hidden', !sidebarShown);
  b.toggle('sb-docked', sidebarDocked());
  b.toggle('edge-away', edgeAway());
  if (animate) slideStartedAt = Date.now();
  $('sidebar').setAttribute('aria-hidden', String(!sidebarShown));
  const tab = $('edge-tab');
  tab.setAttribute('aria-expanded', String(sidebarShown));
  tab.setAttribute('aria-label', flyoutMode() ? 'Tabs (flyout; click to attach)' : sidebarShown ? 'Hide tabs' : 'Show tabs');
  reportLayout({ animate: !!animate });
}

function renderSidebarMode() {
  document.querySelectorAll('[data-sidebar-mode]').forEach((b) =>
    b.setAttribute('aria-pressed', String(b.dataset.sidebarMode === sidebarMode)));
}
document.querySelectorAll('[data-sidebar-mode]').forEach((b) => b.addEventListener('click', () => {
  const mode = b.dataset.sidebarMode;
  setSidebarState(mode, mode === 'attached');
}));

// Resizing: the content view follows the edge, one layout report per
// animation frame.
let resize = null;
let layoutQueued = false;
function queueLayout() {
  if (layoutQueued) return;
  layoutQueued = true;
  requestAnimationFrame(() => { layoutQueued = false; reportLayout(); });
}
// Split panes: the first pane gets `ratio` (permille) of the space less
// the gap, floored; the second the rest (as the host's `pane_frame`).
const SPLIT_GAP = 8;
function splitSizes(total, ratio) {
  const first = Math.max(0, Math.floor((total - SPLIT_GAP) * (ratio || 500) / 1000));
  return [first, Math.max(0, total - SPLIT_GAP - first)];
}
// Dragging the divider: the panes follow, one update per frame.
let splitDrag = null;
$('split-resize').addEventListener('pointerdown', (ev) => {
  if (!state || !state.split) return;
  ev.preventDefault();
  $('split-resize').setPointerCapture(ev.pointerId);
  const rows = state.split.orientation === 'rows';
  splitDrag = { rows, ratio: state.split.ratio, queued: false };
  document.body.classList.add('resizing-split');
  document.body.classList.toggle('split-rows', rows);
});
$('split-resize').addEventListener('pointermove', (ev) => {
  if (!splitDrag || !state || !state.split) return;
  const b = contentBox;
  const total = splitDrag.rows ? window.innerHeight - b.top - b.bottom : window.innerWidth - b.left - b.right;
  const at = splitDrag.rows ? ev.clientY - b.top : ev.clientX - b.left;
  const ratio = Math.round(Math.min(800, Math.max(200, ((at - SPLIT_GAP / 2) / Math.max(1, total - SPLIT_GAP)) * 1000)));
  if (ratio === splitDrag.ratio) return;
  splitDrag.ratio = ratio;
  state.split.ratio = ratio; // the chrome's own surface and handle follow at once
  queueLayout();
  if (splitDrag.queued) return;
  splitDrag.queued = true;
  requestAnimationFrame(() => {
    if (!splitDrag) return;
    splitDrag.queued = false;
    invoke('set_split_ratio', { permille: splitDrag.ratio });
  });
});
const endSplitDrag = () => {
  if (!splitDrag) return;
  invoke('set_split_ratio', { permille: splitDrag.ratio });
  splitDrag = null;
  document.body.classList.remove('resizing-split', 'split-rows');
};
$('split-resize').addEventListener('pointerup', endSplitDrag);
$('split-resize').addEventListener('pointercancel', endSplitDrag);

$('sidebar-resize').addEventListener('pointerdown', (ev) => {
  ev.preventDefault();
  if (ev.metaKey) { switchSidebarMode(); return; }
  const handle = $('sidebar-resize');
  handle.setPointerCapture(ev.pointerId);
  const w = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w')) || 264;
  resize = { x: ev.clientX, w };
  document.body.classList.add('resizing');
});
$('sidebar-resize').addEventListener('pointermove', (ev) => {
  if (!resize) return;
  setSidebarWidth(resize.w + ev.clientX - resize.x);
  queueLayout();
});
const endResize = () => {
  if (!resize) return;
  resize = null;
  document.body.classList.remove('resizing');
  setPref('sidebarWidth', getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w').trim());
  reportLayout();
};
$('sidebar-resize').addEventListener('pointerup', endResize);
$('sidebar-resize').addEventListener('pointercancel', endResize);

// The right panel's width (summary, chat, notebook, site scripts), dragged at its
// left edge and remembered.
const MIN_RIGHT = 260;
const MAX_RIGHT = 640;
let rightWidth = parseFloat(pref('rightWidth') || '') || 380;
function clampRight(w, left) {
  return Math.round(Math.max(MIN_RIGHT, Math.min(MAX_RIGHT, w, (window.innerWidth - left) * 0.6)));
}
let rightDrag = null;
$('right-resize').addEventListener('pointerdown', (ev) => {
  ev.preventDefault();
  $('right-resize').setPointerCapture(ev.pointerId);
  rightDrag = { x: ev.clientX, w: rightWidth };
  document.body.classList.add('resizing-right');
});
$('right-resize').addEventListener('pointermove', (ev) => {
  if (!rightDrag) return;
  rightWidth = Math.max(MIN_RIGHT, Math.min(MAX_RIGHT, rightDrag.w + rightDrag.x - ev.clientX));
  queueLayout();
});
const endRightResize = () => {
  if (!rightDrag) return;
  rightDrag = null;
  document.body.classList.remove('resizing-right');
  setPref('rightWidth', String(Math.round(rightWidth)));
  reportLayout();
};
$('right-resize').addEventListener('pointerup', endRightResize);
$('right-resize').addEventListener('pointercancel', endRightResize);

// ---- sidebar pop-over (sidebar closed) -------------------------------------
//
// A tab stays on the left edge. Pointing at it (or tapping it, without a
// pointer) shows the sidebar as a pop-over over the page, inset 20 pt top
// and bottom; it goes away when the pointer leaves it. In narrow windows
// the tab opens the usual overlaid sidebar instead.
// ---- toggle tab and flyout ----------------------------------------------
//
// The tab is part of the sidebar (it moves with it). In flyout mode,
// pointing at it shows the sidebar at once; leaving the sidebar (tab
// included) hides it again. Clicking the tab follows ⌘\. Without a
// pointer, a first tap shows the flyout and a second attaches it.
// ⌘-click switches mode.
let flyTimer = 0;
let hoverTimer = 0;
const LEAVE_MS = 150;
let tabPointer = 'mouse';
function sidebarBusy() {
  return menuOpen || !$('vm-pop').hidden || !!dragState || !!resize || $('confirm-pop').classList.contains('anchored');
}
function hideFlyoutSoon() {
  clearTimeout(flyTimer);
  flyTimer = setTimeout(() => {
    if (flyoutMode() && sidebarShown && !sidebarBusy()) setSidebarState('flyout', false);
  }, LEAVE_MS);
}
function switchSidebarMode() {
  if (narrow) return;
  if (sidebarMode === 'attached') setSidebarState('flyout', false);
  else setSidebarState('attached', true);
}
const tabEl = $('edge-tab');
tabEl.addEventListener('pointerenter', (ev) => {
  clearTimeout(flyTimer);
  if (ev.pointerType === 'mouse' && flyoutMode() && !sidebarShown) setSidebarState('flyout', true);
});
tabEl.addEventListener('pointerdown', (ev) => { tabPointer = ev.pointerType || 'mouse'; });
tabEl.addEventListener('click', (ev) => {
  ev.stopPropagation();
  if (ev.metaKey) { switchSidebarMode(); return; }
  // No hover without a pointer: the first tap shows the flyout.
  if (flyoutMode() && !sidebarShown && tabPointer !== 'mouse') { setSidebarState('flyout', true); return; }
  toggleSidebar();
});
$('sidebar').addEventListener('pointerenter', () => clearTimeout(flyTimer));
$('sidebar').addEventListener('pointerleave', (ev) => {
  if (ev.pointerType === 'mouse' && flyoutMode() && sidebarShown) hideFlyoutSoon();
});
// The pointer may leave the chrome's web view straight onto the page.
document.addEventListener('mouseout', (ev) => { if (!ev.relatedTarget && flyoutMode() && sidebarShown) hideFlyoutSoon(); });

// ---- the sidebar in use (pointer over it) ----------------------------------
//
// With a pointer the sidebar's contents recede unless it is in use
// (base.css, `body.sb-live`). That used to be `#sidebar:hover`, which is
// WebKit's idea of where the pointer last was in the chrome; it could stay
// on the sidebar after the pointer went to a page, and the sidebar stayed
// opaque and in colour wherever the pointer was (owner report 2026-11-24).
// Now: the chrome's own pointer events set it (over the sidebar or not);
// leaving the sidebar, the chrome's web view or the app clears it; and so
// does the host whenever the pointer is not over the chrome (over a page,
// or out of the window: `chrome_pointer`, seen natively on every move).
let sbLive = false;
function setSbLive(on) {
  if (on === sbLive) return;
  sbLive = on;
  document.body.classList.toggle('sb-live', on);
}
const sbPointerSeen = (ev) => {
  if (ev.pointerType === 'touch') return;
  setSbLive($('sidebar').contains(ev.target));
};
document.addEventListener('pointermove', sbPointerSeen, { passive: true, capture: true });
document.addEventListener('pointerover', sbPointerSeen, { passive: true, capture: true });
$('sidebar').addEventListener('pointerleave', () => setSbLive(false));
document.addEventListener('mouseout', (ev) => { if (!ev.relatedTarget) setSbLive(false); });
document.addEventListener('visibilitychange', () => { if (document.visibilityState !== 'visible') setSbLive(false); });
// Once per launch, the Console says whether WebKit's own hover state was
// the stale one (to confirm the cause on a device).
let sbStaleNoted = false;
T.event.listen('chrome_pointer', (e) => {
  if (e.payload !== false) return;
  if (!sbStaleNoted && $('sidebar').matches(':hover')) {
    sbStaleNoted = true;
    chromeLog('info', 'sidebar: WebKit still had the pointer over the sidebar after it left the chrome (faded anyway)');
  }
  setSbLive(false);
});

// ---- menu, settings, history ----------------------------------------------

function openMenu() { menuOpen = true; $('menu-pop').hidden = false; document.body.classList.add('menu-open'); }
function closeMenu() { menuOpen = false; $('menu-pop').hidden = true; syncMenuOpen(); }
// Keeps the chrome fully opaque while any of its menus is open.
function syncMenuOpen() {
  const open = menuOpen || !$('vm-pop').hidden || !$('pin-pop').hidden || !$('actions-pop').hidden || findOpen || !$('confirm-pop').hidden;
  document.body.classList.toggle('menu-open', open);
}

function setModal(open, section) {
  closeMenu();
  modalOpen = open;
  $('modal').hidden = !open;
  if (open) showSettings(section || 'general');
  reportLayout();
}

const SETTINGS_TITLES = { general: 'General', ai: 'AI', keys: 'Keyboard', cache: 'Cache', blocked: 'Blocked' };
// Settings → General → Passkeys (2026-11-23). WebKit signs pages in
// with passkeys by itself, but only in a build signed with Apple's
// entitlement for browsers (macOS: `npm run macos:passkeys`; iPadOS: the
// default-browser entitlement), and only with the person's permission,
// which the system asks for once.
function renderPasskeys() {
  const text = $('passkeys-state');
  const allow = $('passkeys-allow');
  allow.hidden = true;
  T.core.invoke('passkey_status').then((st) => {
    if (st.entitled === false) {
      text.textContent = 'Not available in this build: sites can use passkeys and security keys only in a build signed with Apple’s passkey entitlement for browsers. Use the site’s other sign-in method meanwhile.';
      return;
    }
    const p = st.permission;
    if (p === 'authorized') text.textContent = 'On: sites can ask to sign you in with your passkeys and security keys.';
    else if (p === 'denied') text.textContent = macOS
      ? 'Off for Snorkle. It can be turned on again in System Settings → Privacy & Security.'
      : 'Off for Snorkle. It can be turned on again in the Settings app.';
    else if (p === 'not_determined') {
      text.textContent = 'Snorkle has not asked yet whether it may use your passkeys.';
      allow.hidden = false;
    } else text.textContent = 'Not available on this system.';
  }).catch(() => { text.textContent = ''; });
}
$('passkeys-allow').addEventListener('click', () => {
  T.core.invoke('request_passkeys').then(renderPasskeys).catch((e) => { toast(String(e)); renderPasskeys(); });
});

function showSettings(section) {
  document.querySelectorAll('.settings-nav button').forEach((b) => b.setAttribute('aria-current', b.dataset.section === section ? 'page' : 'false'));
  document.querySelectorAll('.settings-body [data-panel]').forEach((p) => { p.hidden = p.dataset.panel !== section; });
  $('settings-heading').textContent = SETTINGS_TITLES[section];
  if (section === 'general') renderPasskeys();
  if (section === 'ai') { loadAi(); loadUsage(); }
  if (section === 'keys') renderKeys();
  if (section === 'blocked') loadBlocked();
}
document.querySelectorAll('.settings-nav button').forEach((b) => b.addEventListener('click', () => showSettings(b.dataset.section)));
