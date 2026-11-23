// Browser chrome (see core.js): Wiring: buttons, events from Rust, and what runs at launch.
'use strict';

// ---- wiring -----------------------------------------------------------
// The Notebook (ui/notebook/), in its panel; open as it was left.
notebook = window.SnorkleNotebook.create($('notebook'), {
  invoke: (cmd, args) => T.core.invoke(cmd, args),
  toast,
  place: () => {
    const t = activeTab();
    return t && t.url && !t.private ? { url: t.url.full, title: t.title || '' } : null;
  },
  goTo: openTarget,
  openLink: openTypedLink,
  setClipMode,
  dragLink,
  drop: dropOnNotebook,
  close: () => setNotebook(false),
});

// Nothing dropped on the chrome navigates it (Tauri's own file-drop
// handler is off, so the notebook can take drops): a drop no part of
// the chrome takes is refused, except into text fields.
const takesText = (t) => t && t.closest && t.closest('input, textarea, [contenteditable="true"]');
document.addEventListener('dragover', (e) => {
  if (e.defaultPrevented || takesText(e.target)) return;
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'none';
});
document.addEventListener('drop', (e) => {
  if (e.defaultPrevented || takesText(e.target)) return;
  e.preventDefault();
});
notebookOpen = pref('notebookOpen') === '1';
applyNotebook();


$('back').addEventListener('click', withTab((t) => invoke('go_back', { tab: t.id })));
$('forward').addEventListener('click', withTab((t) => invoke('go_forward', { tab: t.id })));
$('reload').addEventListener('click', withTab((t) => invoke('reload', { tab: t.id })));
$('menu-new-tab').addEventListener('click', () => openNewTab(false));
$('menu-new-private').addEventListener('click', () => openNewTab(true));

$('unsplit').addEventListener('click', withTab((t) => invoke('unsplit', { tab: t.id })));
$('sheet-close').addEventListener('click', () => { if (state && state.sheet !== null) invoke('close_tab', { tab: state.sheet }); });
$('menu').addEventListener('click', () => (menuOpen ? closeMenu() : openMenu()));
$('menu-history').addEventListener('click', () => setHistory(true));
$('menu-downloads').addEventListener('click', openDownloads);
$('menu-settings').addEventListener('click', () => setModal(true));
$('menu-console').addEventListener('click', () => setConsole(!consoleOpen));
$('console-close').addEventListener('click', () => setConsole(false));
$('console-capture').addEventListener('click', () => setCapture(!consoleCapture));
$('console-clear').addEventListener('click', () => {
  consoleEntries = [];
  invoke('clear_console');
  renderConsole();
});
$('console-copy').addEventListener('click', copyConsole);
$('console-tab-only').addEventListener('change', renderConsole);
document.querySelectorAll('#console .seg button').forEach((b) => b.addEventListener('click', () => { consoleFilter = b.dataset.filter; renderConsole(); }));
$('modal-close').addEventListener('click', () => setModal(false));
$('modal').addEventListener('click', (ev) => { if (ev.target === $('modal')) setModal(false); });
// Deleting website data takes two taps.
const DATA_DONE = { cache: 'Cache deleted.', cookies: 'Cookies deleted.', site_data: 'Site data deleted.', logins: 'Signed out of all sites.' };
document.querySelectorAll('.clear-data').forEach((b) => b.addEventListener('click', () => {
  if (!b.classList.contains('armed')) {
    document.querySelectorAll('.clear-data.armed').forEach((x) => { x.classList.remove('armed'); x.textContent = 'Delete'; });
    b.classList.add('armed');
    b.textContent = 'Tap again';
    setTimeout(() => { b.classList.remove('armed'); b.textContent = 'Delete'; }, 4000);
    return;
  }
  b.classList.remove('armed');
  b.textContent = 'Delete';
  invoke('clear_website_data', { kind: b.dataset.kind }).then(() => toast(DATA_DONE[b.dataset.kind]));
}));
$('scripts-btn').addEventListener('click', () => { setActions(false); setScripts(!scriptsOpen); });
$('scripts-close').addEventListener('click', () => setScripts(false));
$('scripts-new').addEventListener('click', () => {
  const existing = scripts.find((s) => s.domain === currentSite());
  openEditor(existing || null);
});
$('se-save').addEventListener('click', () =>
  saveScript({ domain: $('se-domain').value, css: $('se-css').value, js: $('se-js').value, enabled: $('se-enabled').checked }));
$('se-delete').addEventListener('click', () => { if (editingDomain) deleteScript(editingDomain); });

// Taps outside close the menu, and the overlaid sidebar in narrow windows.
document.addEventListener('pointerdown', (ev) => {
  if (menuOpen && !$('menu-pop').contains(ev.target) && ev.target !== $('menu')) closeMenu();
  if (!$('vm-pop').hidden && !$('vm-pop').contains(ev.target)) closeVmPop();
  if (!$('pin-pop').hidden && !$('pin-pop').contains(ev.target)) closePinPop();
  if (!$('img-pop').hidden && !$('img-pop').contains(ev.target)) hideImageMenu();
  if (!$('nb-pop').hidden && !$('nb-pop').contains(ev.target)) hideNbPop();
  if (!$('actions-pop').hidden && !$('actions').contains(ev.target)) setActions(false);
  // Taps outside close the overlaid sidebar (narrow) and the flyout.
  if ((narrow || flyoutMode()) && sidebarShown && !$('sidebar').contains(ev.target)) setSidebar(false);
  if (confirmAction && !$('confirm-pop').contains(ev.target)) closeConfirm();
});
document.addEventListener('keydown', (ev) => {
  if (ev.key !== 'Escape' || editing) return;
  if (!$('edit-modal').hidden) closeEdit();
  else if (historyOpen && $('sidebar').contains(document.activeElement)) setHistory(false);
  else if (!$('copy-modal').hidden) closePageCopy();
  else if (unblock) closeUnblock();
  else if (confirmAction) closeConfirm();
  else if (capture) endCapture(true);
  else if (findOpen) closeFind();
  else if (flyoutMode() && sidebarShown) setSidebar(false);
  else if (!$('actions-pop').hidden) setActions(false);
  else if (!$('pin-pop').hidden) closePinPop();
  else if (!$('img-pop').hidden) hideImageMenu();
  else if (!$('nb-pop').hidden) hideNbPop();
  else if (menuOpen) closeMenu();
  else if (modalOpen) setModal(false);
  else if (scriptsOpen) setScripts(false);
  else if (chatOpen) setChat(false);
  else if (panel) closePanel();
});
$('shield').addEventListener('click', withTab((t) => {
  setActions(false);
  const a = t.adblock;
  if (!a || !a.can_toggle) return;
  T.core.invoke('toggle_site_adblock', { tab: t.id })
    .then((blocking) => toast(blocking ? `Blocking ads on ${a.site} again. Reloading…` : `Ads allowed on ${a.site}. Reloading…`))
    .catch((e) => toast(String(e)));
}));
$('bookmark').addEventListener('click', withTab((t) => { setActions(false); toggleBookmark(t); }));

$('url-display').addEventListener('click', beginEdit);
const input = $('url-input');
input.addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') {
    const tab = activeTab();
    const value = input.value;
    endEdit();
    if (tab) invoke('navigate', { tab: tab.id, input: value });
  } else if (ev.key === 'Escape') {
    endEdit();
  }
});
input.addEventListener('blur', () => { if (editing) endEdit(); });

new ResizeObserver(reportLayout).observe($('chrome'));
window.addEventListener('resize', () => {
  const nowNarrow = window.innerWidth < NARROW_PX;
  if (nowNarrow !== narrow) {
    narrow = nowNarrow;
    loadSidebarPrefs();
  }
  setSidebarWidth(parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w')) || 264);
  applyChat();
  applyNotebook();
  $('scripts-pop').classList.toggle('dock', scriptsOpen && !narrow);
  applySidebar();
});
const savedWidth = parseFloat(pref('sidebarWidth') || '');
if (savedWidth) setSidebarWidth(savedWidth);
renderSidebarMode();
applySidebar();
renderBookmarks();
renderPins();

T.event.listen('state', (e) => { state = e.payload; render(); });
T.event.listen('toast', (e) => toast(String(e.payload)));
T.event.listen('shortcut', (e) => runShortcut(String(e.payload)));
T.event.listen('pins', () => loadPins());
T.event.listen('icons', () => refreshTabIcons(true));
T.event.listen('console', (e) => addConsole(e.payload));
T.event.listen('console_reset', () => loadConsole());
T.core.invoke('get_state').then((s) => { state = s; render(); });
loadBookmarks();
loadPins();
loadLater();
loadScripts();
loadAi();
T.core.invoke('get_view_mode').then((m) => { viewModes = m; renderViewMode(); reportLayout(); }).catch(() => {});
