// Browser chrome (see core.js): View modes and the page actions popup.
'use strict';

// ---- view modes ---------------------------------------------------------
//
// Each button toggles its mode app-wide; a long press offers "Disable for
// this tab". Each shows what the pages look like: the palette and the
// picture are struck through while their mode is in effect (no colour,
// no images); dark shows a moon when on, a sun when off.

const VIEW_BUTTONS = [
  { id: 'color', mode: 'desaturate', name: 'Black and white pages' },
  { id: 'images', mode: 'hide_media', name: 'Hide images and video' },
  { id: 'dark', mode: 'dark', name: 'Dark pages' },
];

// The summary is shown with the chat, in the AI sidebar (one button in
// the URL bar, owner request 2026-11-23; before, a summary button per
// tab and a chat button). `chatOpen` is the AI sidebar being open.
function summaryOn() {
  return chatOpen;
}

function exemptFor(tabId, mode) {
  return tabId !== null && tabId !== undefined && (viewModes.exempt[tabId] || []).includes(mode);
}

// Whether `mode` is in effect for a tab.
function modeOn(mode, tabId) {
  return viewModes.modes[mode] && !exemptFor(tabId, mode);
}

function renderViewMode() {
  const tab = state ? state.active : null;
  for (const b of VIEW_BUTTONS) {
    const on = modeOn(b.mode, tab);
    const btn = $(b.id);
    const exempt = viewModes.modes[b.mode] && exemptFor(tab, b.mode);
    btn.classList.toggle('on', on);
    btn.classList.toggle('struck', on && b.mode !== 'dark');
    btn.classList.toggle('exempt', exempt);
    btn.setAttribute('aria-pressed', String(on));
    const status = exempt ? 'off for this tab' : on ? 'on' : 'off';
    btn.title = `${b.name}: ${status} (long-press for this tab)`;
  }
  // Settings → General: what each mode starts as at launch.
  const launch = viewModes.launch || {};
  document.querySelectorAll('[data-launch-mode]').forEach((b) =>
    b.setAttribute('aria-pressed', String(!!launch[b.dataset.launchMode] === (b.dataset.on === 'true'))));
  applyNightVisionChrome(!!viewModes.modes.night);
  $('ai-btn').classList.toggle('on', chatOpen);
  $('ai-btn').setAttribute('aria-pressed', String(chatOpen));
  renderSummary();
}

$('ai-btn').addEventListener('click', () => setAi(!chatOpen, true));
$('summary-close').addEventListener('click', () => setAi(false));

// Page actions: a popup above the ⋯ button, floating over the page (a
// hole, like the pill).
function setActions(open) {
  $('actions').classList.toggle('open', open);
  $('actions-pop').hidden = !open;
  $('actions-toggle').setAttribute('aria-expanded', String(open));
  syncMenuOpen();
  renderScriptsMark(activeTab());
  reportLayout();
}
$('actions-toggle').addEventListener('click', () => setActions($('actions-pop').hidden));

function renderAttachBar() {
  const b = $('attach-bar');
  b.setAttribute('aria-checked', String(barAttached));
  b.querySelector('.state').textContent = barAttached ? 'On' : 'Off';
}
renderAttachBar();
$('attach-bar').addEventListener('click', () => {
  const tab = activeTab();
  if (!tab) return;
  invoke('set_url_bar_attached', { tab: tab.id, attached: !barAttached });
  setActions(false);
});

// ⋯ → Block This Site: adds the site (and its subdomains) to Settings →
// Blocked. Asked first, since only the passage undoes it.
$('block-site').addEventListener('click', withTab((t) => {
  setActions(false);
  const site = t.adblock && t.adblock.site;
  if (!site) return;
  askConfirm(`Block ${site}? Unblocking it later means typing out a passage in Settings → Blocked.`, 'Block', null, () => {
    T.core.invoke('block_site', { tab: t.id })
      .then(() => toast(`Blocked ${site}.`))
      .catch((e) => toast(String(e)));
  });
}));

// Page scale (⋯ menu): per tab, decided and saved by the core; the
// content host lays the page out for a larger (or smaller) window and
// draws it scaled. The menu stays open so the buttons can be pressed
// repeatedly.
const ZOOM_MIN = 50, ZOOM_MAX = 150;
function renderZoom(tab) {
  const z = tab ? tab.zoom : 100;
  const page = !!(tab && tab.url);
  const level = $('zoom-level');
  level.textContent = `${z}%`;
  level.classList.toggle('changed', z !== 100);
  level.disabled = !page || z === 100;
  $('zoom-out').disabled = !page || z <= ZOOM_MIN;
  $('zoom-in').disabled = !page || z >= ZOOM_MAX;
}
for (const [id, step] of [['zoom-out', 'out'], ['zoom-in', 'in'], ['zoom-level', 'reset']]) {
  $(id).addEventListener('click', withTab((t) => invoke('zoom_page', { tab: t.id, step })));
}

function applyViewModes(p) {
  p.then((m) => { viewModes = m; renderViewMode(); reportLayout(); }).catch((e) => toast(String(e)));
}

function toggleMode(mode) {
  applyViewModes(T.core.invoke('set_view_mode', { mode, on: !viewModes.modes[mode] }));
}

document.querySelectorAll('[data-launch-mode]').forEach((b) => b.addEventListener('click', () => {
  applyViewModes(T.core.invoke('set_launch_mode', { mode: b.dataset.launchMode, on: b.dataset.on === 'true' }));
}));

// Long press (or right click) on a view-mode button.
let vmPress = null;
let vmTarget = null;
function openVmPop(b) {
  const tab = state && state.active;
  if (tab === null || tab === undefined) return;
  vmTarget = b;
  const exempt = exemptFor(tab, b.mode);
  const appOn = viewModes.modes[b.mode];
  $('vm-title').textContent = `${b.name}: ${appOn ? 'on' : 'off'} for all tabs`;
  const item = $('vm-tab');
  item.disabled = !appOn && !exempt;
  item.textContent = exempt ? 'Use the app setting for this tab' : appOn ? 'Disable for this tab' : 'Turn it on to set it per tab';
  closeMenu();
  $('vm-pop').hidden = false;
  syncMenuOpen();
}
function closeVmPop() { $('vm-pop').hidden = true; vmTarget = null; syncMenuOpen(); }

for (const b of VIEW_BUTTONS) {
  const btn = $(b.id);
  btn.addEventListener('pointerdown', (ev) => {
    if (ev.button > 0) return;
    clearTimeout(vmPress && vmPress.timer);
    vmPress = { fired: false, timer: setTimeout(() => { vmPress.fired = true; openVmPop(b); }, 500) };
  });
  const cancel = () => { if (vmPress) clearTimeout(vmPress.timer); };
  btn.addEventListener('pointerup', cancel);
  btn.addEventListener('pointerleave', cancel);
  btn.addEventListener('pointercancel', cancel);
  btn.addEventListener('contextmenu', (ev) => { ev.preventDefault(); cancel(); if (vmPress) vmPress.fired = true; openVmPop(b); });
  btn.addEventListener('click', () => {
    if (vmPress && vmPress.fired) { vmPress = null; return; } // the long press already acted
    vmPress = null;
    toggleMode(b.mode);
  });
}
$('vm-tab').addEventListener('click', () => {
  const b = vmTarget;
  const tab = state && state.active;
  closeVmPop();
  if (!b || tab === null) return;
  applyViewModes(T.core.invoke('set_tab_view_exempt', { tab, mode: b.mode, exempt: !exemptFor(tab, b.mode) }));
});
