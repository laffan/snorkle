// Browser chrome (see core.js): Save for Later (saved pages and capture mode); the start page.
'use strict';

// ---- Save for Later -------------------------------------------------------
//
// A tab's tray button archives its page (Rust, from the app's own web
// view), stores its address and title, and closes it; a visit's tray
// button in History does the same for a page loaded in a hidden tab.
// Saved pages are listed at the top of the History sidebar (no pictures,
// owner request 2026-11-23), each with Recover (open in a new tab), "+"
// (add pages), Bookmark and Delete on hover.

// The SVG namespace, from an icon already in the page (no URL literal
// in this file: the remote-code tripwire bans them).
const SVG_NS = $('menu').querySelector('svg').namespaceURI;
// Paths (24 × 24, stroked): a tray with an arrow into it / out of it,
// and a bookmark ribbon.
const TRAY_IN = 'M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4M12 3v11M8 10l4 4 4-4';
const TRAY_OUT = 'M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4M12 14V3M8 7l4-4 4 4';
// A speech bubble above the tray: Save for Later on an AI conversation,
// which captures the thread as the reader scrolls (2026-11-16).
const TRAY_CHAT = 'M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4M9 3h6a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2h-2l-1 2-1-2H9a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z';
// The AI conversation sites Save for Later captures (as Rust's
// `conversation::site_of`, which decides; this only picks the icon).
function conversationSite(t) {
  let host = '';
  try { host = new URL(t.url.full).hostname.toLowerCase(); } catch (e) { return false; }
  return /(^|\.)(chatgpt\.com|chat\.openai\.com|claude\.ai)$/.test(host) || host === 'gemini.google.com';
}
// A tray with a tick: a visit whose page is saved for later.
const TRAY_SAVED = 'M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4M8.5 8.5l2.5 2.5 4.5-5';
const RIBBON = 'M7 3h10a1 1 0 0 1 1 1v17l-6-4-6 4V4a1 1 0 0 1 1-1z';
const PLUS = 'M12 5v14M5 12h14';
function svgIcon(d) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '2');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  const path = document.createElementNS(SVG_NS, 'path');
  path.setAttribute('d', d);
  svg.appendChild(path);
  return svg;
}

let laterItems = [];

function loadLater() {
  return T.core.invoke('list_later').then((items) => { laterItems = items || []; renderLater(); }).catch(() => {});
}

// Saving reads the page into an archive first (Rust: `saved::capture`),
// which can take a few seconds on a large page; the tab closes after.
const savingTabs = new Set();
// An AI conversation (chatgpt.com, claude.ai, gemini.google.com) is
// saved through a capture mode (2026-11-16): the sites load and keep only
// part of a long thread, so the reader scrolls through it (up for earlier
// messages) while each message is captured, and then saves. Other pages
// are saved at once.
let capture = null; // { tab, count, timer }
function saveForLater(tab) {
  if (savingTabs.has(tab) || (capture && capture.tab === tab)) return;
  T.core.invoke('begin_conversation_capture', { tab })
    .then((n) => { if (typeof n === 'number') startCapture(tab, n); else saveNow(tab, false); })
    .catch(() => saveNow(tab, false));
}
function saveNow(tab, captured) {
  if (savingTabs.has(tab)) return;
  savingTabs.add(tab);
  progressToast('Saving for later…', 0);
  T.core.invoke('save_for_later', { tab, captured })
    .then(() => { toast('Saved for later.'); return loadLater(); })
    .catch((e) => toast(String(e)))
    .finally(() => savingTabs.delete(tab));
}
function startCapture(tab, count) {
  if (capture) endCapture(true);
  if (state && state.active !== tab) invoke('activate_tab', { tab });
  capture = { tab, count, timer: setInterval(pollCapture, 700) };
  renderCapture();
}
function renderCapture() {
  const bar = $('capture-bar');
  const shown = !!capture && !!state && (state.active === capture.tab || shownPanes().some((p) => p.tab === capture.tab));
  if (capture) {
    // With nothing found (the site's markup may have changed), Save
    // keeps the page as it is shown, as for any other page.
    const n = capture.count;
    $('capture-text').textContent = n
      ? `Scroll through the conversation to capture it (up for earlier messages). ${n} message${n === 1 ? '' : 's'} captured.`
      : 'No messages found yet. Scroll through the conversation to capture it, or Save to keep the page as shown.';
    $('capture-save').textContent = 'Save';
  }
  if (bar.hidden === !shown) return;
  bar.hidden = !shown;
  reportLayout();
}
function pollCapture() {
  const c = capture;
  if (!c) return;
  if (state && !state.tabs.some((t) => t.id === c.tab)) { endCapture(false); return; }
  T.core.invoke('conversation_capture_count', { tab: c.tab }).then((n) => {
    if (capture !== c) return;
    if (typeof n !== 'number') { endCapture(false); toast('The page changed, so the capture stopped.'); return; }
    c.count = n;
    renderCapture();
  }).catch(() => {});
}
function endCapture(cancelInPage) {
  const c = capture;
  if (!c) return;
  capture = null;
  clearInterval(c.timer);
  renderCapture();
  if (cancelInPage) T.core.invoke('cancel_conversation_capture', { tab: c.tab }).catch(() => {});
}
$('capture-save').addEventListener('click', () => {
  const c = capture;
  if (!c) return;
  endCapture(false);
  saveNow(c.tab, true);
});
$('capture-cancel').addEventListener('click', () => endCapture(true));

function laterAction(cmd, id, done) {
  T.core.invoke(cmd, { id }).then(() => { if (done) done(); return loadLater(); }).catch((e) => toast(String(e)));
}

// The saved pages, at the top of the History sidebar: one row each (the
// site's icon, the title, the site and how many pages), filtered by the
// history search. A click opens the saved copy (offline; it stays in the
// list), or the live page when there is none.
function renderLater() {
  const q = historyQuery.trim().toLowerCase();
  const items = q ? laterItems.filter((it) => (it.title + ' ' + it.url).toLowerCase().includes(q)) : laterItems;
  $('later-section').hidden = !items.length;
  const list = $('later-list');
  list.replaceChildren();
  for (const it of items) {
    const card = el('div', 'later-card');
    card.dataset.item = it.id;
    card.setAttribute('role', 'listitem');
    const open = el('button', 'open');
    open.setAttribute('aria-label', `Open ${it.title || it.host}`);
    open.title = it.pages ? 'Open the saved copy' : 'Open the page';
    open.appendChild(rowIcon(it.url, { registrable: it.host }, null));
    const text = el('span', 'text');
    const t = el('span', 't');
    appendMarked(t, it.title || it.host, q);
    text.appendChild(t);
    text.appendChild(el('span', 'u', it.pages > 1 ? `${it.host} · ${it.pages} pages` : it.host));
    open.appendChild(text);
    open.addEventListener('click', () => {
      if (it.pages) invoke('open_later', { id: it.id }).then(() => { if (narrow) setSidebar(false); });
      else laterAction('restore_later', it.id, () => { if (narrow) setSidebar(false); });
    });
    card.appendChild(open);
    const acts = el('div', 'later-actions');
    const btn = (d, label, run) => {
      const b = el('button');
      if (d) b.appendChild(svgIcon(d)); else b.textContent = '×';
      b.setAttribute('aria-label', label);
      b.title = label;
      b.addEventListener('click', (ev) => { ev.stopPropagation(); run(); });
      acts.appendChild(b);
    };
    btn(TRAY_OUT, 'Recover (open the live page)', () => laterAction('restore_later', it.id, () => { if (narrow) setSidebar(false); }));
    // "+": add pages to the saved copy by tapping links in it.
    if (it.pages) btn(PLUS, 'Add pages', () => invoke('add_later_pages', { id: it.id }).then(() => { if (narrow) setSidebar(false); }));
    btn(RIBBON, 'Bookmark', () => laterAction('bookmark_later', it.id, () => { loadBookmarks(); toast('Bookmarked.'); }));
    // Deleting drops the saved copy for good: asked first, beside the row.
    btn(null, 'Delete', () => {
      const name = it.title || it.host || 'this page';
      askConfirm(`Delete “${name}” from Saved for Later? Its saved copy is removed.`, 'Delete', null,
        () => laterAction('delete_later', it.id), null,
        { anchor: () => document.querySelector(`.later-card[data-item="${CSS.escape(it.id)}"]`) });
    });
    card.appendChild(acts);
    list.appendChild(card);
  }
  renderLaterSelection();
  // Visits of saved pages are marked in the list below.
  if (historyOpen) fillHistory($('history-list'), historyItems, q);
}

// The card whose saved copy is the shown tab is shown as selected.
function renderLaterSelection() {
  const id = state && shownTab();
  const t = state && state.tabs.find((x) => x.id === id);
  const item = t && t.saved ? t.saved.item : null;
  document.querySelectorAll('.later-card').forEach((c) => c.classList.toggle('active', c.dataset.item === item));
}

// Adding pages ("+" on a saved page): its saved copy shows with scripts
// off and each link tapped on the page is chosen (again: unchosen); Rust
// keeps the choice (`Browser::saved_copy_navigation`). Add loads each
// chosen page and saves it into the same copy.
let pickKey = '';
function renderPickBar(tab) {
  const picking = tab && tab.saved && tab.saved.picking;
  const bar = $('pick-bar');
  const key = picking ? JSON.stringify(picking) : '';
  if (!picking) { if (!bar.hidden) { bar.hidden = true; pickKey = ''; reportLayout(); } return; }
  if (key === pickKey && !bar.hidden) return;
  pickKey = key;
  const n = picking.length;
  $('pick-text').textContent = n ? `${n} page${n === 1 ? '' : 's'} chosen: ${picking.slice(0, 3).join(', ')}${n > 3 ? '…' : ''}` : 'Tap links on this page to add their pages.';
  $('pick-text').title = picking.join('\n');
  $('pick-add').textContent = n ? `Add ${n} Page${n === 1 ? '' : 's'}` : 'Done';
  if (bar.hidden) { bar.hidden = false; reportLayout(); }
}
$('pick-cancel').addEventListener('click', withTab((t) => invoke('cancel_adding_pages', { tab: t.id })));
$('pick-add').addEventListener('click', withTab((t) => {
  progressToast('Adding pages…', 0);
  T.core.invoke('finish_adding_pages', { tab: t.id })
    .then((n) => { toast(n ? `Added ${n} page${n === 1 ? '' : 's'}.` : 'No pages added.'); return loadLater(); })
    .catch((e) => toast(String(e)));
}));

// ---- start page (a new tab) ----------------------------------------------
//
// An empty tab shows the start page on the chrome's surface (no content
// view exists until it navigates): the new-tab input, a little above the
// middle of its pane, with the palette's rows under it. Each launch opens
// one (Rust, `restore_session_with_start_tab`); ⌘T and New Tab create one.

let startTab = null; // the tab whose start page is shown
let startTabsKey = '';
let startShown = false;
function renderStart(tab) {
  const input = $('palette-input');
  const fresh = startTab !== tab.id;
  if (fresh) {
    startTab = tab.id;
    input.value = '';
    paletteSel = 0;
    loadPaletteHistory();
  }
  input.placeholder = tab.private ? 'Search or enter an address (private)' : 'Search tabs and bookmarks, or enter an address';
  // Other tabs' titles and addresses are rows: re-render when they change
  // (not on every progress update, so a click is not lost to a rebuild).
  const key = JSON.stringify(state.tabs.map((t) => [t.id, t.title, t.url && t.url.full]));
  if (fresh || key !== startTabsKey) { startTabsKey = key; renderPalette(); }
  if (fresh || !startShown) focusInput(input);
}

// History, newest first, under a heading per day and per hour.
