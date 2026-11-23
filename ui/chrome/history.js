// Browser chrome (see core.js): History in the sidebar, with the pages saved for later.
'use strict';

// ---- history ------------------------------------------------------------
//
// A sidebar view (⌘⇧H, Menu → History; owner request 2026-11-23; before,
// a panel over the page): the pages saved for later first, then visits
// by day. A visit can be removed with the × that replaces its time on
// hover, or saved for later with the tray beside it (the page is loaded
// in a hidden tab and archived there, `save_page_for_later`); Select
// turns rows into checkboxes (shift-click selects a range) for removing
// many. Pages whose text was saved (the 100 most recent) show its size,
// and clicking one shows that text; the search box also searches it.
// Clearing history keeps the saved pages.
let historyOpen = false;
// The sidebar was hidden when History opened: closing History hides it.
let historyShowedSidebar = false;
let historyItems = [];
let historyQuery = '';
let historySelecting = false;
const historySel = new Set();
let historyAnchor = -1;
let historySearchTimer = 0;
// Pages being saved from History (their tray shows it).
const savingUrls = new Set();
const visitKey = (it) => it.url + '\u0000' + it.visited_ms;
const pageOf = (url) => String(url || '').split('#')[0];

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

// Appends `text` to `parent`, with case-insensitive matches of `q` in
// <mark>s (built with textContent only).
function appendMarked(parent, text, q) {
  if (!q) { parent.appendChild(document.createTextNode(text)); return; }
  const lower = text.toLowerCase();
  let at = 0;
  for (;;) {
    const i = lower.indexOf(q, at);
    if (i < 0) break;
    if (i > at) parent.appendChild(document.createTextNode(text.slice(at, i)));
    parent.appendChild(el('mark', null, text.slice(i, i + q.length)));
    at = i + q.length;
  }
  if (at < text.length) parent.appendChild(document.createTextNode(text.slice(at)));
}

// Shows or hides History in the sidebar (showing the sidebar if needed).
// `focus`: the search field takes the keyboard (a shortcut or the menu).
function setHistory(open, focus = open) {
  if (open === historyOpen) {
    if (open && !sidebarShown) setSidebar(true);
    if (open && focus) focusInput($('history-search'));
    return;
  }
  historyOpen = open;
  closeMenu();
  historySelecting = false;
  historySel.clear();
  historyAnchor = -1;
  clearArmed = false;
  $('hv-clear').textContent = 'Clear';
  document.body.classList.toggle('history-open', open);
  $('history-view').hidden = !open;
  if (open) {
    historyShowedSidebar = !sidebarShown;
    if (!sidebarShown) setSidebar(true);
    loadHistory();
    loadLater();
    if (focus) focusInput($('history-search'));
  } else {
    closePageCopy();
    $('history-search').blur();
    if (historyShowedSidebar && sidebarShown && !narrow) setSidebar(false);
    historyShowedSidebar = false;
  }
  renderHistoryHead();
}

function loadHistory() {
  const q = historyQuery.trim().toLowerCase();
  const req = q ? T.core.invoke('search_history', { query: q }) : T.core.invoke('list_history', { limit: 500 });
  return req.then((items) => {
    if (q !== historyQuery.trim().toLowerCase()) return; // a newer search is on its way
    historyItems = items;
    const keys = new Set(items.map(visitKey));
    for (const k of [...historySel]) if (!keys.has(k)) historySel.delete(k);
    historyAnchor = -1;
    renderLater(); // the saved pages above, then the visits (fillHistory)
    renderHistoryHead();
  }).catch((e) => toast(String(e)));
}

function renderHistoryHead() {
  $('history-view').classList.toggle('selecting', historySelecting);
  $('hv-select').textContent = historySelecting ? 'Cancel' : 'Select';
  $('hv-remove').hidden = !historySelecting;
  $('hv-remove').disabled = historySel.size === 0;
  $('hv-remove').textContent = historySel.size ? `Remove ${historySel.size}` : 'Remove';
  $('hv-clear').hidden = historySelecting;
  $('hv-title').textContent = historySelecting && historySel.size ? `${historySel.size} selected` : 'History';
}

function removeVisits(items) {
  if (!items.length) return;
  T.core.invoke('remove_history', { visits: items.map((it) => ({ url: it.url, visited_ms: it.visited_ms })) })
    .then(() => { for (const it of items) historySel.delete(visitKey(it)); return loadHistory(); })
    .catch((e) => toast(String(e)));
}

// A click on row `i` in Select mode: toggles it, or with Shift selects
// every row from the last one clicked.
function selectVisit(i, shift) {
  const it = historyItems[i];
  if (!it) return;
  if (shift && historyAnchor >= 0) {
    const [a, b] = historyAnchor < i ? [historyAnchor, i] : [i, historyAnchor];
    for (let k = a; k <= b; k++) historySel.add(visitKey(historyItems[k]));
  } else if (historySel.has(visitKey(it))) {
    historySel.delete(visitKey(it));
  } else {
    historySel.add(visitKey(it));
  }
  historyAnchor = i;
  document.querySelectorAll('#history-list li.visit').forEach((li) => {
    const on = historySel.has(li.dataset.key);
    li.classList.toggle('selected', on);
    li.setAttribute('aria-selected', String(on));
  });
  renderHistoryHead();
}

// Save for Later on a visit: the page is loaded in a hidden tab, archived
// and listed with the saved pages above.
function saveVisit(it) {
  if (savingUrls.has(it.url)) return;
  savingUrls.add(it.url);
  fillHistory($('history-list'), historyItems, historyQuery.trim().toLowerCase());
  progressToast('Saving for later…', 0);
  T.core.invoke('save_page_for_later', { url: it.url })
    .then(() => { toast('Saved for later.'); })
    .catch((e) => toast(String(e)))
    .finally(() => { savingUrls.delete(it.url); loadLater(); });
}

function fillHistory(list, items, q = '') {
  list.replaceChildren();
  if (!items.length) {
    list.appendChild(el('li', 'empty muted', q ? 'No matches.' : 'Nothing here yet.'));
    return;
  }
  const saved = new Set(laterItems.map((it) => pageOf(it.url)));
  const dayName = (d) => {
    const today = new Date();
    const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((start(today) - start(d)) / 86400000);
    if (diff === 0) return 'Today';
    if (diff === 1) return 'Yesterday';
    return d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long', year: diff > 300 ? 'numeric' : undefined });
  };
  let lastDay = '';
  items.forEach((it, i) => {
    const when = new Date(it.visited_ms || 0);
    const day = dayName(when);
    if (day !== lastDay) { list.appendChild(el('li', 'day hv-section', day)); lastDay = day; }
    const li = el('li', 'visit');
    li.dataset.key = visitKey(it);
    const selected = historySel.has(li.dataset.key);
    li.classList.toggle('selected', selected);
    li.setAttribute('aria-selected', String(selected));
    li.appendChild(el('span', 'check'));
    const open = el('button', 'open');
    const t = el('span', 't');
    appendMarked(t, it.title || it.display.registrable, q);
    open.appendChild(t);
    const u = el('span', 'u');
    const sub = el('span', 'sub');
    const reg = el('span', 'reg');
    renderUrlInto(sub, reg, null, it.display);
    u.append(sub, reg);
    if (it.cache_bytes) {
      const size = el('span', 'size', fmtBytes(it.cache_bytes));
      size.title = 'Saved page text';
      u.appendChild(size);
    }
    open.appendChild(u);
    if (it.snippet) {
      const snip = el('span', 'snip');
      appendMarked(snip, it.snippet, q);
      open.appendChild(snip);
    }
    open.addEventListener('click', (ev) => {
      if (historySelecting) { selectVisit(i, ev.shiftKey); return; }
      if (it.cache_bytes) { showPageCopy(it.url); return; }
      openHistoryItem(it.url);
    });
    li.appendChild(open);
    const end = el('span', 'end');
    const isSaved = saved.has(pageOf(it.url));
    end.appendChild(el('span', 'when', when.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })));
    const acts = el('span', 'acts');
    if (isSaved || savingUrls.has(it.url)) {
      // Saved (or being saved): a mark in place of the button.
      const mark = el('span', 'saved-mark' + (isSaved ? '' : ' busy'));
      mark.appendChild(svgIcon(isSaved ? TRAY_SAVED : TRAY_IN));
      mark.title = isSaved ? 'Saved for later' : 'Saving for later…';
      end.appendChild(mark);
    } else {
      const later = el('button', 'later');
      later.appendChild(svgIcon(TRAY_IN));
      later.setAttribute('aria-label', 'Save for later');
      later.title = 'Save for later';
      later.addEventListener('click', (ev) => { ev.stopPropagation(); saveVisit(it); });
      acts.appendChild(later);
    }
    const x = el('button', 'x', '×');
    x.setAttribute('aria-label', 'Remove from history');
    x.title = 'Remove from history';
    x.addEventListener('click', (ev) => { ev.stopPropagation(); removeVisits([it]); });
    acts.appendChild(x);
    end.appendChild(acts);
    li.appendChild(end);
    list.appendChild(li);
  });
}

// A visit opens in the current tab; History stays open beside it.
function openHistoryItem(url) {
  const tab = activeTab();
  closePageCopy();
  if (narrow) setSidebar(false);
  if (tab) invoke('navigate', { tab: tab.id, input: url });
  else T.core.invoke('new_tab', { private: false }).then((id) => invoke('navigate', { tab: id, input: url })).catch((e) => toast(String(e)));
}

// The saved text of a page, as paragraphs (textContent only).
let pageCopyUrl = null;
function showPageCopy(url) {
  T.core.invoke('get_page_text', { url }).then((p) => {
    if (!p) { openHistoryItem(url); return; }
    pageCopyUrl = p.url;
    $('copy-title').textContent = p.title || p.display.registrable;
    $('copy-meta').textContent = `${p.display.full} · saved ${new Date(p.saved_ms).toLocaleString()} · text only`;
    const body = $('copy-body');
    body.replaceChildren();
    const q = historyQuery.trim().toLowerCase();
    for (const para of p.text.split(/\n{2,}/)) {
      const el_ = el('p');
      appendMarked(el_, para, q);
      body.appendChild(el_);
    }
    body.scrollTop = 0;
    $('copy-modal').hidden = false;
    reportLayout();
    const first = body.querySelector('mark');
    if (first) first.scrollIntoView({ block: 'center' });
    $('copy-close').focus();
  }).catch((e) => toast(String(e)));
}
function closePageCopy() {
  if ($('copy-modal').hidden) return;
  pageCopyUrl = null;
  $('copy-modal').hidden = true;
  reportLayout();
}
$('copy-close').addEventListener('click', closePageCopy);
$('copy-open').addEventListener('click', () => { if (pageCopyUrl) openHistoryItem(pageCopyUrl); });
$('copy-modal').addEventListener('click', (ev) => { if (ev.target === $('copy-modal')) closePageCopy(); });
$('hv-select').addEventListener('click', () => {
  historySelecting = !historySelecting;
  historySel.clear();
  historyAnchor = -1;
  document.querySelectorAll('#history-list li.visit').forEach((li) => { li.classList.remove('selected'); li.setAttribute('aria-selected', 'false'); });
  renderHistoryHead();
});
$('hv-remove').addEventListener('click', () => removeVisits(historyItems.filter((it) => historySel.has(visitKey(it)))));
$('hv-close').addEventListener('click', () => setHistory(false));
// Clear asks by changing its label first; saved pages are kept.
$('hv-clear').addEventListener('click', () => {
  if (!clearArmed) { clearArmed = true; $('hv-clear').textContent = 'Clear all?'; return; }
  clearArmed = false;
  $('hv-clear').textContent = 'Clear';
  T.core.invoke('clear_history').then(loadHistory).catch((e) => toast(String(e)));
});
$('hv-clear').addEventListener('blur', () => { clearArmed = false; $('hv-clear').textContent = 'Clear'; });
$('history-search').addEventListener('input', () => {
  historyQuery = $('history-search').value;
  clearTimeout(historySearchTimer);
  historySearchTimer = setTimeout(loadHistory, 180);
});
$('history-search').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') { ev.preventDefault(); ev.stopPropagation(); if ($('history-search').value) { $('history-search').value = ''; historyQuery = ''; loadHistory(); } else setHistory(false); }
});
