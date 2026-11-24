// Browser chrome (see core.js): The start page's input and its rows.
'use strict';

// ---- command palette (the start page's input) ------------------------------
//
// Arc-style rows under the start page's input: typing fuzzy-matches open
// tabs, pins and bookmarks first, then history; the first rows go to,
// search for, or (a single word) define what was typed. Going, searching
// and history load in this tab; choosing another tab, pin or bookmark
// switches to it and closes this empty one, as Chrome's omnibox does.

let paletteRows = [];
// Recent history for the palette (newest first, one entry per URL),
// loaded when a start page opens.
let paletteHistory = [];
let paletteSel = 0;

// Subsequence match of `q` in `text`, scored: consecutive runs, word
// starts and an early first match score higher. Returns null if `q` is
// not a subsequence.
function fuzzy(q, text) {
  const t = text.toLowerCase();
  let score = 0;
  let run = 0;
  let ti = 0;
  const hits = [];
  for (const ch of q) {
    const at = t.indexOf(ch, ti);
    if (at < 0) return null;
    run = at === ti && hits.length ? run + 1 : 0;
    const wordStart = at === 0 || /[\s./\-_:]/.test(t[at - 1]);
    score += 1 + run * 3 + (wordStart ? 4 : 0);
    hits.push(at);
    ti = at + 1;
  }
  score -= hits[0] * 0.2 + (t.length - q.length) * 0.02;
  return { score, hits };
}

// Each row's `run` gets the start page's tab.
function paletteCandidates() {
  const out = [];
  if (state) {
    for (const t of state.tabs) {
      // Empty tabs (this one included) are not worth switching to; a
      // research session's start page offers its own tabs only.
      if (!listedHere(t) || !t.url) continue;
      out.push({ kind: t.private ? 'Private' : 'Tab', title: t.name || t.title || t.url.registrable, host: t.url.registrable, run: (here) => leaveStart(here, invoke('activate_tab', { tab: t.id })) });
    }
  }
  // Pins and bookmarks are everyday browsing's (2026-11-24).
  const research = !!(state && state.space);
  const items = research ? [] : [...pins.map((p) => ['Pinned', p]), ...bookmarks.map((b) => ['Bookmark', b])];
  for (const [kind, p] of items) out.push({ kind, title: p.title || p.display.registrable, host: p.display.registrable, run: (here) => leaveStart(here, openLink(p.url)) });
  // History, after the rest; pages already pinned or bookmarked once.
  const seen = new Set(items.map(([, p]) => p.url));
  for (const h of paletteHistory) {
    if (seen.has(h.url)) continue;
    seen.add(h.url);
    out.push({ kind: 'History', history: true, title: h.title || h.display.registrable, host: h.display.registrable, run: (here) => goHere(here, h.url) });
  }
  return out;
}

// One word of letters (apostrophes and hyphens inside it), as Rust's
// `ai::clean_word` accepts.
function singleWord(q) {
  return [...q].length <= 64 && /^\p{Alphabetic}(?:[\p{Alphabetic}'’-]*\p{Alphabetic})?$/u.test(q);
}

function renderPalette() {
  const q = $('palette-input').value.trim();
  const ql = q.toLowerCase();
  let rows;
  if (!ql) {
    // Open tabs, pins and bookmarks first; recent history fills the rest.
    rows = paletteCandidates().slice(0, 8).map((c) => ({ ...c, hits: [] }));
  } else {
    rows = paletteCandidates()
      .map((c) => {
        const m = fuzzy(ql, c.title);
        const mh = fuzzy(ql, c.host);
        const best = m && (!mh || m.score >= mh.score) ? m : mh;
        // History ranks just below an equally good tab or bookmark.
        return best && { ...c, score: best.score - (c.history ? 0.5 : 0), hits: best === m ? best.hits : [] };
      })
      .filter(Boolean)
      .sort((a, b) => ({Tab: 0, Private: 0, Pinned: 1, Bookmark: 1, History: 2}[a.kind] - {Tab: 0, Private: 0, Pinned: 1, Bookmark: 1, History: 2}[b.kind]) || b.score - a.score)
      .slice(0, 8);
    const head = [];
    if (looksLikeAddress(q)) head.push({ kind: 'Go to', title: q, host: '', hits: [], run: (here) => goHere(here, q) });
    head.push({ kind: 'Search', title: q, host: '', hits: [], run: (here) => invoke('search_google', { query: q, private: !!(here && here.private), tab: here ? here.id : null }) });
    // Define (owner request 2026-11-08): a single word only; the AI's
    // definition appears above the input (2026-11-09).
    if (singleWord(q)) head.push({ kind: 'Define', title: q, host: '', hits: [], run: (here) => defineWord(here, q) });
    rows = head.concat(rows);
  }
  paletteRows = rows;
  paletteSel = Math.min(paletteSel, Math.max(0, rows.length - 1));
  const list = $('palette-list');
  list.replaceChildren();
  rows.forEach((r, i) => {
    const li = el('li');
    li.setAttribute('role', 'option');
    li.setAttribute('aria-selected', String(i === paletteSel));
    li.appendChild(el('span', 'kind', r.kind));
    const t = el('span', 't');
    // Highlight matched characters (built from text nodes, never markup).
    const hits = new Set(r.hits);
    [...r.title].forEach((ch, k) => t.appendChild(hits.has(k) ? el('mark', null, ch) : document.createTextNode(ch)));
    li.appendChild(t);
    if (r.host) li.appendChild(el('span', 'h', r.host));
    li.addEventListener('pointerdown', (ev) => ev.preventDefault()); // keep the input focused
    li.addEventListener('click', () => choosePalette(i));
    list.appendChild(li);
  });
  if (!rows.length) list.appendChild(el('li', 'muted', 'No open tabs, bookmarks or history yet. Type an address or a search.'));
}

function looksLikeAddress(q) {
  if (/\s/.test(q)) return false;
  try {
    const u = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(q) ? q : 'https://' + q);
    return ['http:', 'https:'].includes(u.protocol) && (u.hostname.includes('.') || u.hostname === 'localhost');
  } catch (_) { return false; }
}

// Loads `q` (typed, or a history address) in the start page's tab.
function goHere(here, q) {
  if (here) invoke('navigate', { tab: here.id, input: q });
}

// After switching to another tab, pin or bookmark (`switched`, a
// promise): closes the start page's empty tab, unless it is half of a
// split view.
function leaveStart(here, switched) {
  if (!here || pairOf(here.id)) return;
  Promise.resolve(switched).then(() => {
    const t = state && state.tabs.find((x) => x.id === here.id);
    if (t && !t.url && state.active !== here.id) invoke('close_tab', { tab: here.id });
  });
}

function choosePalette(i) {
  const r = paletteRows[i];
  const here = activeTab();
  if (!r || !here || here.url) return;
  $('palette-input').blur();
  r.run(here);
}

function loadPaletteHistory() {
  T.core.invoke('list_history', { limit: 500 }).then((items) => {
    paletteHistory = items;
    if (startShown) renderPalette();
  }).catch(() => {});
}

// ⌘T and New Tab: a new empty tab, which shows the start page (focused).
// There is only ever one start page (2026-11-24; Rust leaves a blank tab
// shown as it is, and closes one that is left): on it, ⌘T only gives its
// field the focus.
function openNewTab(priv) {
  closeMenu();
  if (narrow) setSidebar(false);
  const t = activeTab();
  if (t && !t.url && !t.loading && !t.notice && !t.saved && t.launcher === null && !!t.private === !!priv) {
    focusInput($('palette-input'), true);
    return;
  }
  invoke('new_tab', { private: !!priv });
}

// Focuses an input the chrome opened from a shortcut. After a shortcut a
// page (or nothing) has keyboard focus, and focusing inside the chrome's
// web view does nothing until that web view is first responder: ask the
// host for it, then focus again.
// The input asked for last: an earlier request's retries never take the
// focus from it (2026-11-24: the start page of a new research session
// took it back from the topic being named).
let focusWanted = null;
function focusInput(input, select) {
  focusWanted = input;
  const go = () => {
    if (focusWanted !== input) return;
    input.focus({ preventScroll: true });
    if (select) input.select();
  };
  go();
  const ask = () => T.core.invoke('focus_chrome').catch(() => {}).finally(() => { go(); requestAnimationFrame(go); });
  // Right after the app returns from another app, the first request can
  // come too early for UIKit: ask once more if focus did not land.
  ask().then(() => setTimeout(() => { if (focusWanted === input && !input.hidden && document.activeElement !== input) ask(); }, 150));
}

// Back from another app: a shown start page or open find bar gets focus
// again (no new tab: only a launch opens one).
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible') return;
  if (findOpen) focusInput($('find-input'));
  else if (startShown) focusInput($('palette-input'));
});

$('palette-input').addEventListener('input', () => { paletteSel = 0; renderPalette(); });
$('palette-input').addEventListener('keydown', (ev) => {
  if (ev.isComposing) return;
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
    ev.preventDefault();
    const n = paletteRows.length;
    if (n) paletteSel = (paletteSel + (ev.key === 'ArrowDown' ? 1 : n - 1)) % n;
    renderPalette();
  } else if (ev.key === 'Enter') {
    ev.preventDefault();
    choosePalette(paletteSel);
  } else if (ev.key === 'Escape') {
    // A tab, not a dialog: Escape clears what was typed.
    ev.preventDefault();
    ev.stopPropagation();
    if ($('palette-input').value) { $('palette-input').value = ''; paletteSel = 0; renderPalette(); }
    else $('palette-input').blur();
  }
});

// ⌘⇧] / ⌘⇧[: the next or previous tab in the sidebar's order, wrapping,
// as in Chrome. Pins and bookmarks count where they have a tab (one not
// yet opened is skipped rather than loaded); a split pair's halves are
// stepped through in turn.
function cycleTab(step) {
  if (!state) return;
  const launcherTabs = new Map();
  for (const t of state.tabs) if (t.launcher !== null && t.sheet_over === null && !t.private) launcherTabs.set(t.launcher, t.id);
  const ids = [];
  for (const n of $('sidebar').querySelectorAll('.pin[data-url], .bm[data-url], .tab[data-tab]')) {
    // Not the pins and bookmarks a research session hides, nor the tabs
    // of a folded group (2026-11-24).
    if ((state.space && !n.classList.contains('tab')) || n.closest('.collapsed')) continue;
    const id = n.dataset.tab !== undefined ? Number(n.dataset.tab) : launcherTabs.get(n.dataset.url);
    if (id !== undefined && !ids.includes(id)) ids.push(id);
  }
  if (!ids.length) return;
  const at = ids.indexOf(shownTab());
  const next = at < 0 ? ids[step > 0 ? 0 : ids.length - 1] : ids[(at + step + ids.length) % ids.length];
  if (next !== shownTab()) invoke('activate_tab', { tab: next });
}
