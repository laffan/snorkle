// Browser chrome (see core.js): Keyboard shortcuts, confirmations, bookmarks, right panels.
'use strict';

// ---- keyboard shortcuts -------------------------------------------------
//
// ⌘T new tab, ⌘\ toggle sidebar, ⌘1–⌘5 the first five sidebar items
// (pins, then bookmarks, then tabs). Handled here when the chrome has
// keyboard focus; when a page has focus, native UIKeyCommands forward the
// same names via the `shortcut` event.

function sidebarItems() {
  const items = [];
  // A research session's sidebar has its tabs only (2026-11-24).
  if (state && !state.space) {
    for (const p of pins) items.push({ link: p });
    for (const b of bookmarks) items.push({ link: b });
  }
  if (state) {
    for (const t of state.tabs) if (!t.private && listedHere(t)) items.push({ tab: t });
    for (const t of state.tabs) if (t.private && listedHere(t)) items.push({ tab: t });
  }
  return items;
}

// A shortcut can arrive twice for one key press (the native menu command
// and this page's own keydown, depending on what has focus).
let lastShortcut = { name: '', at: 0 };
// `via`: 'native' (a key command while a page or nothing had focus, sent
// as the `shortcut` event) or 'chrome' (this page's keydown). Both are
// logged (Menu → Console, Browser), so a press that does nothing shows
// which route it took, or that it never arrived.
function runShortcut(name, via = 'native') {
  // The shortcut editor's recording, done natively (record:<mods>:<key>).
  if (name.startsWith('record:')) {
    if (!recordingKey) return;
    if (name === 'record:cancel') { stopRecording(''); return; }
    const rest = name.slice(7);
    const i = rest.indexOf(':');
    finishRecording(rest.slice(i + 1), Number(rest.slice(0, i)) || 0);
    return;
  }
  const now = performance.now();
  if (name === lastShortcut.name && now - lastShortcut.at < 60) {
    chromeLog('debug', `shortcut ${name} via ${via}: same press, already handled`);
    return;
  }
  lastShortcut = { name, at: now };
  chromeLog('debug', `shortcut ${name} via ${via}`);
  if (name === 'new_tab') {
    openNewTab(false);
  } else if (name === 'new_private_tab') {
    // ⌘⇧N (2026-11-24), as Menu → New Private Tab.
    openNewTab(true);
  } else if (name === 'next_tab' || name === 'prev_tab') {
    cycleTab(name === 'next_tab' ? 1 : -1);
  } else if (name === 'reload') {
    const tab = activeTab();
    if (tab) invoke('reload', { tab: tab.id });
  } else if (name === 'close_tab') {
    closeTabShortcut();
  } else if (name === 'reopen_tab') {
    // ⌘⇧T: the most recently closed tab whose page is still in history.
    T.core.invoke('reopen_closed_tab')
      .then((ok) => { if (!ok) toast('No closed tabs to reopen.'); })
      .catch((e) => toast(String(e)));
  } else if (name === 'toggle_sidebar') {
    toggleSidebar();
  } else if (name === 'toggle_right') {
    toggleRight();
  } else if (name === 'bookmark') {
    bookmarkShortcut();
  } else if ((name === 'find' || name === 'find_next' || name === 'find_prev') && notebook && notebookShown() && notebook.hasFocus()) {
    // In the notebook, ⌘F / ⌘G search the notebook instead of the page.
    notebook.findKey(name);
  } else if (name === 'find') {
    openFind();
  } else if (name === 'split_columns' || name === 'split_rows') {
    // ⌘S / ⌘⇧S: a new empty tab beside (or below) this one, ready for an
    // address; in a split, its layout changes instead.
    const orientation = name === 'split_columns' ? 'columns' : 'rows';
    // The new tab's start page (in its pane) takes the focus.
    T.core.invoke('split_new', { orientation }).catch((e) => toast(String(e)));
  } else if (name === 'find_next' || name === 'find_prev') {
    // ⌘G / ⌘⇧G: step through the matches; open the bar if it is closed.
    if (findOpen && find.query) findStep(name === 'find_prev');
    else openFind();
  } else if (name === 'toggle_images') {
    toggleMode('hide_media');
  } else if (name === 'night_vision') {
    // ⌘⇧R (2026-11-23): as the eye at the sidebar's foot.
    setNightVision(!viewModes.modes.night);
  } else if (name === 'notebook_page') {
    // ⌘⇧D (2026-11-24): the page shown, to the notebook as a link.
    sendPageToNotebook(activeTab());
  } else if (name === 'history') {
    // ⌘⇧H (2026-11-23): the history sidebar, shown or hidden.
    setHistory(!historyOpen);
  } else if (name === 'go_back' || name === 'go_forward') {
    // ⌘← / ⌘→ (2026-11-15): as the pill's buttons.
    const btn = $(name === 'go_back' ? 'back' : 'forward');
    if (!btn.disabled) btn.click();
  } else if (name === 'detach_bar' || name === 'attach_bar') {
    // ⌘↑ floats the URL bar as the pill, ⌘↓ attaches it along the
    // bottom of the page (this tab, as ⋯ → Attach URL Bar).
    const tab = activeTab();
    const attached = name === 'attach_bar';
    if (tab && attached !== barAttached) invoke('set_url_bar_attached', { tab: tab.id, attached });
  } else if (name.startsWith('item:')) {
    const item = sidebarItems()[Number(name.slice(5)) - 1];
    if (!item) return;
    if (item.tab) invoke('activate_tab', { tab: item.tab.id });
    else openLink(item.link.url);
  }
}

// ⌘W: the pop-up sheet in front, else the active tab. A pin's or a
// bookmark's tab is that item, so it stays: ⌘W opens a new tab instead
// (owner request 2026-11-23; before, a pin's tab showed a message and a
// bookmark's tab asked whether to remove the bookmark).
function closeTabShortcut() {
  const id = state && (state.sheet !== null ? state.sheet : state.active);
  if (id === null || id === undefined) return;
  const t = state.tabs.find((x) => x.id === id);
  const key = t && t.sheet_over === null ? t.launcher : null;
  if (key && (pins.some((p) => p.url === key) || bookmarks.some((b) => b.url === key))) {
    openNewTab(false);
    return;
  }
  invoke('close_tab', { tab: id });
}

// A small confirmation floating above the pill (a hole, like the pill).
// The primary button confirms, Escape cancels. `alt` adds a second
// action ({ label, run }), which then has the focus (Return). Options:
// `cancel` (run when dismissed: Cancel, Escape, a click elsewhere),
// `cancelLabel`, and `anchor` (a function returning the sidebar row the
// confirmation is about: a site's tabs, a saved page). With an anchor in
// a sidebar that is showing, it sits just right of the sidebar beside
// that row (2026-11-15) instead of above the pill.
function askConfirm(text, ok, tab, run, alt = null, opts = {}) {
  if (confirmAction) closeConfirm();
  confirmAction = {
    tab,
    run: () => { closeConfirm(false); run(); },
    alt: alt && (() => { closeConfirm(false); alt.run(); }),
    cancel: opts.cancel || null,
    anchor: opts.anchor || null,
  };
  $('confirm-text').textContent = text;
  $('confirm-ok').textContent = ok;
  $('confirm-cancel').textContent = opts.cancelLabel || 'Cancel';
  $('confirm-alt').hidden = !alt;
  $('confirm-alt').textContent = alt ? alt.label : '';
  $('confirm-pop').hidden = false;
  placeConfirm();
  $(alt ? 'confirm-alt' : 'confirm-ok').focus();
  syncMenuOpen();
  reportLayout();
}
// Beside its sidebar row (see askConfirm), or above the pill when the
// row is not on screen (sidebar hidden, row scrolled away).
function placeConfirm() {
  const pop = $('confirm-pop');
  const row = confirmAction && confirmAction.anchor ? confirmAction.anchor() : null;
  const side = $('sidebar').getBoundingClientRect();
  const r = row && row.isConnected ? row.getBoundingClientRect() : null;
  const visible = r && sidebarShown && r.height > 0 && side.right > 40 && r.bottom > side.top && r.top < side.bottom;
  pop.classList.toggle('anchored', !!visible);
  if (!visible) {
    pop.style.left = pop.style.top = pop.style.maxWidth = '';
    return;
  }
  const left = Math.round(side.right) + 10;
  pop.style.left = left + 'px';
  pop.style.maxWidth = Math.max(220, Math.min(420, window.innerWidth - left - 12)) + 'px';
  const h = pop.offsetHeight;
  const mid = r.top + r.height / 2;
  const top = Math.max(8, Math.min(mid - h / 2, window.innerHeight - h - 8));
  pop.style.top = Math.round(top) + 'px';
  pop.style.setProperty('--notch', Math.round(Math.max(12, Math.min(h - 12, mid - top))) + 'px');
}
// `dismissed`: closed without choosing an action (runs `cancel`).
function closeConfirm(dismissed = true) {
  const was = confirmAction;
  confirmAction = null;
  $('confirm-pop').hidden = true;
  $('confirm-pop').classList.remove('anchored');
  $('confirm-alt').hidden = true;
  syncMenuOpen();
  reportLayout();
  if (dismissed && was && was.cancel) was.cancel();
  else setTimeout(nextDownloadAsk, 0); // a waiting download prompt comes next
}

// Downloads from a frame inside a page wait for the user (2026-11-04):
// one prompt at a time; unanswered requests are blocked natively after a
// minute, and the prompt goes with them.
const downloadAsks = [];
function answerDownload(a, allow) {
  const i = downloadAsks.indexOf(a);
  if (i < 0) return;
  downloadAsks.splice(i, 1);
  clearTimeout(a.timer);
  invoke('answer_download_prompt', { ask: a.ask, allow });
  setTimeout(nextDownloadAsk, 0);
}
function nextDownloadAsk() {
  if (confirmAction || !downloadAsks.length) return;
  const a = downloadAsks[0];
  const who = a.from ? `${a.from}, from a frame inside this page,` : 'A frame inside this page';
  askConfirm(`${who} wants to download a file.`, 'Allow Download', null, () => answerDownload(a, true), null,
    { cancel: () => answerDownload(a, false), cancelLabel: 'Block' });
}
T.event.listen('download_prompt', (e) => {
  const p = e.payload || {};
  const a = { ask: p.ask, from: String(p.from || '') };
  a.timer = setTimeout(() => {
    const i = downloadAsks.indexOf(a);
    if (i < 0) return;
    downloadAsks.splice(i, 1);
    if (i === 0 && confirmAction && confirmAction.cancel) closeConfirm(false);
    nextDownloadAsk();
  }, 60000);
  downloadAsks.push(a);
  nextDownloadAsk();
});
// The row moves with the sidebar's scrolling and the window's size.
$('side-scroll').addEventListener('scroll', () => { if (confirmAction && confirmAction.anchor) { placeConfirm(); reportLayout(); } }, { passive: true });
window.addEventListener('resize', () => { if (confirmAction && confirmAction.anchor) placeConfirm(); });
$('confirm-ok').addEventListener('click', () => { if (confirmAction) confirmAction.run(); });
$('confirm-alt').addEventListener('click', () => { if (confirmAction && confirmAction.alt) confirmAction.alt(); });
$('confirm-cancel').addEventListener('click', closeConfirm);

// ⌘D (and the ⋯ menu's Bookmark row): bookmarks the page, moving this
// tab into the bookmark; on a bookmark's own tab, removes the bookmark
// and the tab becomes an ordinary tab again; on a pin's tab (2026-11-24),
// bookmarks the pin (its address and title; the pin stays).
const BOOKMARK_RESULT = {
  added: 'Bookmarked',
  removed: 'Bookmark removed',
  already: 'Already bookmarked',
  from_pin: 'Bookmarked the pin',
  none: 'Nothing to bookmark yet.',
};
function toggleBookmark(t) {
  if (!t || !t.url) { toast('Open a page to bookmark it.'); return; }
  if (t.private) { toast('Private tabs can’t be bookmarked.'); return; }
  T.core.invoke('toggle_bookmark', { tab: t.id })
    .then((r) => { toast(BOOKMARK_RESULT[r] || String(r)); return loadBookmarks(); })
    .then(render)
    .catch((e) => toast(String(e)));
}
function bookmarkShortcut() { toggleBookmark(activeTab()); }

// ⌘⇧\: hide the right panels (keeping them on), or show them again; with
// none on, open the chat.
function toggleRight() {
  const anyOn = scriptsOpen || chatOpen || notebookOpen;
  if (!anyOn) { setChat(true); return; }
  if (!rightHidden && scriptsOpen) { setScripts(false); if (!chatOpen && !notebookOpen) return; }
  rightHidden = !rightHidden;
  applyChat();
  applyNotebook();
  renderSummary();
  renderViewMode();
  reportLayout();
}
