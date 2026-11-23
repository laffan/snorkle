// Browser chrome (see core.js): Find on page (⌘F).
'use strict';

// ---- find on page (⌘F) ------------------------------------------------------
//
// A script in the page highlights every match (50 %) and the current one
// (100 %) without selecting anything, scrolls to the current one and
// returns "3/12" (digits only). ⌘G / ⌘⇧G and ↩ / ⇧↩ step through them.
// The bar sits on top of the pill.
let find = { tab: null, query: '', total: null, index: 0, timer: 0, seq: 0 };

function openFind() {
  const t = activeTab();
  findOpen = true;
  find.tab = t ? t.id : null;
  $('findbar').hidden = false;
  focusInput($('find-input'), true);
  renderFind();
  syncMenuOpen();
  reportLayout();
}

function closeFind() {
  if (!findOpen) return;
  findOpen = false;
  clearTimeout(find.timer);
  if (find.tab !== null && find.query) invoke('end_find', { tab: find.tab });
  find = { tab: null, query: '', total: null, index: 0, timer: 0, seq: find.seq + 1 };
  $('findbar').hidden = true;
  $('find-input').value = '';
  syncMenuOpen();
  reportLayout();
}

function renderFind() {
  const c = $('find-count');
  const q = find.query;
  $('findbar').classList.toggle('none', !!q && find.total === 0);
  if (!q) c.textContent = '';
  else if (find.total === null) c.textContent = '…';
  else if (find.total === 0) c.textContent = 'No matches';
  else c.textContent = `${Math.max(1, find.index)} of ${find.total >= 1000 ? '1000+' : find.total}`;
  $('find-prev').disabled = $('find-next').disabled = !q || find.total === 0;
}

// Applies a position from the page ({index, total}, or null when the
// page did not answer).
function findResult(seq, pos) {
  if (seq !== find.seq) return;
  if (pos) { find.index = pos.index; find.total = pos.total; }
  else if (find.total === null) find.total = 0;
  renderFind();
}

// A new query: highlight its matches, the first one in view current.
function findQuery(q) {
  const t = activeTab();
  if (find.tab !== null && find.query && !q) invoke('end_find', { tab: find.tab });
  find.query = q;
  find.total = null;
  find.index = 0;
  const seq = ++find.seq;
  renderFind();
  if (!q || !t || !t.url) { if (q) { find.total = 0; renderFind(); } return; }
  find.tab = t.id;
  T.core.invoke('find_in_page', { tab: t.id, query: q, step: 'start' })
    .then((pos) => findResult(seq, pos))
    .catch((e) => toast(String(e)));
}

function findStep(backwards) {
  const t = activeTab();
  if (!t || !find.query || !find.total) return;
  const seq = find.seq;
  T.core.invoke('find_in_page', { tab: t.id, query: find.query, step: backwards ? 'prev' : 'next' })
    .then((pos) => findResult(seq, pos))
    .catch((e) => toast(String(e)));
}

$('find-input').addEventListener('input', () => {
  clearTimeout(find.timer);
  const q = $('find-input').value.trim();
  find.timer = setTimeout(() => findQuery(q), 180);
});
$('find-input').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') { ev.preventDefault(); findStep(ev.shiftKey); }
  else if (ev.key === 'Escape') { ev.preventDefault(); closeFind(); }
});
$('find-prev').addEventListener('click', () => findStep(true));
$('find-next').addEventListener('click', () => findStep(false));
$('find-close').addEventListener('click', closeFind);

document.addEventListener('keydown', (ev) => {
  if (!ev.metaKey || recordingKey) return;
  const pressed = keyOf(ev);
  // ⌘← and the other arrows move the caret in a text field.
  if (pressed && ARROW_LABELS[pressed.key] && editableTarget(ev.target)) return;
  const hit = pressed && shortcutList.find((x) => x.key === pressed.key && x.mods === pressed.mods);
  const name = hit ? hit.action : null;
  if (!name) return;
  ev.preventDefault();
  runShortcut(name, 'chrome');
});

// Text being edited with keyboard focus, here or in a page (C19
// reports, by tab): arrow-key shortcuts are left to the text meanwhile
// (natively too: `set_text_editing`).
function editableTarget(e) {
  if (!e || !e.tagName) return false;
  if (e.isContentEditable || e.tagName === 'TEXTAREA' || e.tagName === 'SELECT') return true;
  return e.tagName === 'INPUT' && !/^(button|submit|reset|checkbox|radio|range|color|file|image|hidden)$/i.test(e.type || 'text');
}
const pageEditing = new Map(); // tab id → true while its page edits text
let textEditingSent = false;
function syncTextEditing() {
  if (state) for (const id of [...pageEditing.keys()]) if (!state.tabs.some((t) => t.id === id)) pageEditing.delete(id);
  const on = (document.hasFocus() && editableTarget(document.activeElement)) || [...pageEditing.values()].some(Boolean);
  if (on === textEditingSent) return;
  textEditingSent = on;
  T.core.invoke('set_text_editing', { on }).catch(() => {});
}
document.addEventListener('focusin', syncTextEditing);
document.addEventListener('focusout', () => setTimeout(syncTextEditing, 0));
window.addEventListener('focus', syncTextEditing);
window.addEventListener('blur', () => setTimeout(syncTextEditing, 0));
T.event.listen('page_editing', (e) => {
  const { tab, on } = e.payload || {};
  if (typeof tab !== 'number') return;
  if (on) pageEditing.set(tab, true);
  else pageEditing.delete(tab);
  syncTextEditing();
});
