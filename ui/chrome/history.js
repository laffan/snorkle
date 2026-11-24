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
// many; ⌘- or ⇧-click starts or adjusts a selection too. While searching,
// Select Results chooses every result shown (2026-11-24); what is chosen
// can be deleted, restored as tabs, or sent as tabs to a research session.
// Days fold: today is open, every other day starts folded (2026-11-24).
// Pages whose text was saved (the 100 most recent) show its size; the
// search box also searches it. Clearing history keeps the saved pages.
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
// Folded days (owner request 2026-11-24): today open, the others folded,
// until a heading is clicked (remembered until the app quits). While
// searching every day starts open, and folding lasts for that search.
const dayFolds = new Map();
let searchFolds = new Map();
let foldsQuery = '';
const dayKey = (ms) => { const d = new Date(ms || 0); return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime(); };
function dayFolded(key) {
  const q = historyQuery.trim().toLowerCase();
  if (q !== foldsQuery) { foldsQuery = q; searchFolds = new Map(); }
  const folds = q ? searchFolds : dayFolds;
  if (folds.has(key)) return folds.get(key);
  return q ? false : key !== dayKey(Date.now());
}
function toggleDay(key) {
  const folds = historyQuery.trim() ? searchFolds : dayFolds;
  folds.set(key, !dayFolded(key));
  fillHistory($('history-list'), historyItems, historyQuery.trim().toLowerCase());
  renderHistoryHead();
}
// Rows shown (not in a folded day): the ones a range or Select Results takes.
const visitShown = (i) => !!historyItems[i] && !dayFolded(dayKey(historyItems[i].visited_ms));
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
  $('hv-clear').hidden = historySelecting;
  $('hv-title').textContent = historySelecting && historySel.size ? `${historySel.size} selected` : 'History';
  // Under the search box: searching, the results' number and Select
  // Results; selecting, what to do with the selection.
  const searching = !!historyQuery.trim() && historyItems.length > 0;
  $('hv-bar').hidden = !historySelecting && !searching;
  $('hv-count').textContent = historySelecting ? '' : `${historyItems.length} ${historyItems.length === 1 ? 'result' : 'results'}`;
  $('hv-select-results').hidden = historySelecting || !searching;
  for (const id of ['hv-remove', 'hv-restore', 'hv-send']) {
    $(id).hidden = !historySelecting;
    $(id).disabled = historySel.size === 0;
  }
  if (!historySelecting) closeHvSend();
}
function syncSelectedRows() {
  document.querySelectorAll('#history-list li.visit').forEach((li) => {
    const on = historySel.has(li.dataset.key);
    li.classList.toggle('selected', on);
    li.setAttribute('aria-selected', String(on));
  });
}
function setSelecting(on) {
  historySelecting = on;
  historySel.clear();
  historyAnchor = -1;
  syncSelectedRows();
  renderHistoryHead();
}
const selectedVisits = () => historyItems.filter((it) => historySel.has(visitKey(it)));
// The pages chosen, each once, newest visit first.
const selectedPages = () => [...new Set(selectedVisits().map((it) => it.url))];

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
    for (let k = a; k <= b; k++) if (visitShown(k)) historySel.add(visitKey(historyItems[k]));
  } else if (historySel.has(visitKey(it))) {
    historySel.delete(visitKey(it));
  } else {
    historySel.add(visitKey(it));
  }
  historyAnchor = i;
  syncSelectedRows();
  renderHistoryHead();
}

// Restore and Send to (owner request 2026-11-24): the chosen pages as tabs
// (not loaded until chosen) of the session shown, of another, or of a new
// one named after the search (`history_to_tabs`, at most 100).
async function visitsToTabs(to) {
  const urls = selectedPages();
  if (!urls.length) return;
  if (urls.length > 100) toast('Only the first 100 pages are opened.');
  try {
    let target = to;
    const fresh = to === 'new';
    if (fresh) {
      const q = historyQuery.trim();
      target = await T.core.invoke('new_research', { name: q || 'New Research' });
      if (!q) researchRenamePending = target;
    }
    const ids = await T.core.invoke('history_to_tabs', { urls: urls.slice(0, 100), to: target });
    setSelecting(false);
    if (fresh) {
      // The new session shows its first page; its empty start tab goes.
      if (ids.length) {
        await T.core.invoke('activate_tab', { tab: ids[0] });
        const s = await T.core.invoke('get_state');
        const start = s.tabs.find((t) => t.space === target && !t.url && !ids.includes(t.id));
        if (start) await T.core.invoke('close_tab', { tab: start.id });
      }
      setHistory(false);
      return;
    }
    const here = state ? state.space || null : null;
    const name = target === here ? null : target ? ((state.research || []).find((r) => r.id === target) || {}).name || 'the research session' : 'Everyday Browsing';
    const n = ids.length === 1 ? '1 page' : `${ids.length} pages`;
    toast(name ? `${n} sent to ${name}.` : `${n} restored as tabs.`);
  } catch (e) {
    toast(String(e));
  }
}
function closeHvSend() { $('hv-send-pop').hidden = true; }
function openHvSend() {
  const pop = $('hv-send-pop');
  pop.replaceChildren(el('div', 'menu-label', 'Send to'));
  for (const t of sendTargets()) {
    const b = el('button', null, t.label);
    b.setAttribute('role', 'menuitem');
    b.addEventListener('click', () => { closeHvSend(); visitsToTabs(t.to); });
    pop.appendChild(b);
  }
  const btn = $('hv-send');
  pop.style.top = Math.round(btn.offsetTop + btn.offsetHeight + 4) + 'px';
  pop.hidden = false;
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
  // Visits per day, for the headings.
  const perDay = new Map();
  for (const it of items) { const k = dayKey(it.visited_ms); perDay.set(k, (perDay.get(k) || 0) + 1); }
  let lastDay = null;
  items.forEach((it, i) => {
    const when = new Date(it.visited_ms || 0);
    const key = dayKey(it.visited_ms);
    const folded = dayFolded(key);
    if (key !== lastDay) {
      lastDay = key;
      const head = el('li', 'day hv-section');
      const b = el('button', 'day-toggle' + (folded ? ' folded' : ''));
      b.setAttribute('aria-expanded', String(!folded));
      b.append(el('span', 'chev', '▾'), el('span', 'day-name', dayName(when)), el('span', 'day-count', String(perDay.get(key))));
      b.addEventListener('click', () => toggleDay(key));
      head.appendChild(b);
      list.appendChild(head);
    }
    if (folded) return;
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
      // ⌘- or ⇧-click chooses rows (and starts choosing); in Select
      // mode a plain click does too.
      if (ev.metaKey || ev.ctrlKey || ev.shiftKey) {
        if (!historySelecting) setSelecting(true);
        selectVisit(i, ev.shiftKey);
        return;
      }
      if (historySelecting) { selectVisit(i, ev.shiftKey); return; }
      if (/^https?:/i.test(it.url)) { showPageCopy(it); return; }
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

// A visit's page in a dialog (owner request 2026-11-24: the page itself,
// no longer only its saved text). The host shows it in the preview tab
// (`open_preview`: never listed, saved or written to history) over the
// dialog's body, whose rectangle `reportLayout` reports; the saved text, if
// any, is under it until the page shows (or if it cannot). Open Page loads
// it in the current tab; Save for Later saves the preview's page.
let pageCopyUrl = null;
let previewTab = null;
let previewSeen = false;
let previewFound = false;
function showPageCopy(it) {
  const url = it.url;
  pageCopyUrl = url;
  previewTab = null;
  previewSeen = false;
  previewFound = false;
  $('copy-title').textContent = it.title || (it.display && it.display.registrable) || url;
  $('copy-meta').textContent = it.display ? it.display.full : url;
  const body = $('copy-body');
  body.replaceChildren(el('p', 'muted', 'Loading…'));
  body.scrollTop = 0;
  renderPreviewLater();
  $('copy-modal').hidden = false;
  reportLayout();
  $('copy-close').focus();
  T.core.invoke('open_preview', { url }).then((id) => {
    if (pageCopyUrl !== url) { T.core.invoke('close_preview').catch(() => {}); return; }
    previewTab = id;
    // The state that shows it may have come first (syncPreview).
    if (state && state.preview === id) previewSeen = true;
  }).catch((e) => { closePageCopy(); toast(String(e)); });
  // The saved text, if any, while the page loads (or if it cannot show).
  if (!it.cache_bytes) return;
  T.core.invoke('get_page_text', { url }).then((p) => {
    if (!p || pageCopyUrl !== url) return;
    body.replaceChildren();
    const q = historyQuery.trim().toLowerCase();
    for (const para of p.text.split(/\n{2,}/)) {
      const el_ = el('p');
      appendMarked(el_, para, q);
      body.appendChild(el_);
    }
    const first = body.querySelector('mark');
    if (first) first.scrollIntoView({ block: 'center' });
  }).catch(() => {});
}
function closePageCopy() {
  if ($('copy-modal').hidden) return;
  pageCopyUrl = null;
  if (previewTab !== null) T.core.invoke('close_preview').catch(() => {});
  previewTab = null;
  $('copy-modal').hidden = true;
  reportLayout();
}
// The preview's page finished loading: the History search's words are
// found in it, as ⌘F does.
function previewLoaded(p) {
  if (p.tab !== previewTab || p.loading || previewFound) return;
  previewFound = true;
  const q = historyQuery.trim();
  if (q) T.core.invoke('find_in_page', { tab: previewTab, query: q, step: 'start' }).catch(() => {});
}
// The host closed the preview (its page opened a tab, which was chosen):
// the dialog goes with it.
function syncPreview() {
  if (previewTab === null || !state) return;
  if (state.preview === previewTab) previewSeen = true;
  else if (previewSeen) {
    previewTab = null;
    closePageCopy();
  }
}
// Saving closes the preview's tab: the dialog closes without asking the
// host to close it again.
function previewSaved(url) {
  if (pageCopyUrl !== url) return;
  previewTab = null;
  closePageCopy();
}
// Its rectangle, for the host: the dialog's body, while the dialog is open.
function previewRect() {
  if ($('copy-modal').hidden) return null;
  const r = $('copy-body').getBoundingClientRect();
  return r.width > 0 && r.height > 0 ? { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) } : null;
}
function renderPreviewLater() {
  const b = $('copy-later');
  const saved = !!pageCopyUrl && laterItems.some((x) => pageOf(x.url) === pageOf(pageCopyUrl));
  b.disabled = saved || savingUrls.has(pageCopyUrl);
  b.textContent = saved ? 'Saved' : savingUrls.has(pageCopyUrl) ? 'Saving…' : 'Save for Later';
}
$('copy-close').addEventListener('click', closePageCopy);
$('copy-open').addEventListener('click', () => { if (pageCopyUrl) openHistoryItem(pageCopyUrl); });
$('copy-later').addEventListener('click', () => {
  const url = pageCopyUrl;
  if (!url || savingUrls.has(url)) return;
  savingUrls.add(url);
  renderPreviewLater();
  progressToast('Saving for later…', 0);
  // The page the dialog shows is saved as it is (the preview tab then
  // closes, and the dialog with it); before it has a tab, from history.
  const save = previewTab !== null
    ? T.core.invoke('save_for_later', { tab: previewTab })
    : T.core.invoke('save_page_for_later', { url });
  save.then(() => { toast('Saved for later.'); previewSaved(url); })
    .catch((e) => toast(String(e)))
    .finally(() => {
      savingUrls.delete(url);
      renderPreviewLater();
      loadLater().then(() => { renderPreviewLater(); fillHistory($('history-list'), historyItems, historyQuery.trim().toLowerCase()); });
    });
});
$('copy-modal').addEventListener('click', (ev) => { if (ev.target === $('copy-modal')) closePageCopy(); });
$('hv-select').addEventListener('click', () => setSelecting(!historySelecting));
$('hv-select-results').addEventListener('click', () => {
  setSelecting(true);
  historyItems.forEach((it, i) => { if (visitShown(i)) historySel.add(visitKey(it)); });
  syncSelectedRows();
  renderHistoryHead();
});
$('hv-remove').addEventListener('click', () => { const v = selectedVisits(); setSelecting(false); removeVisits(v); });
$('hv-restore').addEventListener('click', () => visitsToTabs(state ? state.space || null : null));
$('hv-send').addEventListener('click', () => ($('hv-send-pop').hidden ? openHvSend() : closeHvSend()));
document.addEventListener('pointerdown', (ev) => { if (!$('hv-send-pop').hidden && !ev.target.closest('#hv-send-pop, #hv-send')) closeHvSend(); });
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
