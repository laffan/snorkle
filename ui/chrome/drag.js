// Browser chrome (see core.js): Dragging tabs, pins and bookmarks in the sidebar.
'use strict';

// ---- drag and drop ------------------------------------------------------
//
// Touch: long-press, then move; a move before the long-press fires is a
// scroll. Mouse and trackpad: press and move (no delay, as on a desktop).
// Tab → bookmarks bar: bookmark it at that position. Tab → the middle of
// another tab: split view; → the top or bottom edge of one (or below the
// last): moved there. A tab of a split pair → anywhere off its pair:
// unsplit.
// Bookmark → bookmarks bar: reorder.
//
// Mouse drags used to start on a 120 ms timer even without movement, and
// every ended drag swallowed the click. A click held a little longer, or
// whose pointerup reached the chrome late because the app's main thread
// was busy with a heavy page, then did nothing. Now a drag that never
// moved lets its click through.

const DRAG_DELAY_MS = 280;
const MOVE_TOLERANCE_PX = 8;
let pendingDrag = null;
let dragState = null;

function armDrag(ev, node, item) {
  if (ev.button > 0 || ev.target.closest('.close, .unpin, .row-actions, input')) return;
  // ⌘- and ⇧-clicks choose rows (groups.js); they never start a drag.
  if (ev.metaKey || ev.ctrlKey || ev.shiftKey) return;
  cancelPending();
  const mouse = ev.pointerType === 'mouse';
  pendingDrag = { node, item, mouse, x: ev.clientX, y: ev.clientY, timer: mouse ? null : setTimeout(startDrag, DRAG_DELAY_MS) };
}

function cancelPending() {
  if (pendingDrag) clearTimeout(pendingDrag.timer);
  pendingDrag = null;
}

function startDrag() {
  const p = pendingDrag;
  pendingDrag = null;
  if (!p) return;
  const rect = p.node.getBoundingClientRect();
  const ghost = p.node.cloneNode(true);
  ghost.classList.add('ghost');
  ghost.style.width = rect.width + 'px';
  ghost.style.left = rect.left + 'px';
  ghost.style.top = rect.top + 'px';
  document.body.appendChild(ghost);
  p.node.classList.add('drag-src');
  document.body.classList.add('dragging');
  dragState = { item: p.item, node: p.node, ghost, x: p.x, y: p.y, dx: p.x - rect.left, dy: p.y - rect.top, target: null, moved: false };
  showDropZone(p.item);
}

// While dragging something that can be bookmarked, the bookmarks list
// grows a visible target at its end: an empty dashed box (no text,
// 2026-11-23), which is all an empty list shows.
function showDropZone(item) {
  const list = $('bookmarks');
  if (item.kind === 'tab' && item.private) return; // never persist private tabs
  if (item.kind === 'bgroup' || (item.kind === 'tgroup' && privateGroup(item.group))) return;
  const zone = el('div', 'drop-zone');
  zone.setAttribute('aria-label', item.kind === 'bookmark' ? 'Move to the end' : 'Drop here to bookmark');
  list.appendChild(zone);
}

// A tab group of private tabs (never bookmarked).
function privateGroup(id) {
  return !!state && state.tabs.some((t) => t.group === id && t.private);
}

// The page a dragged row stands for (a tab's, a pin's or a bookmark's),
// as a notebook link's target; null for private tabs and empty ones.
function draggedPage(item) {
  if (item.kind === 'tab') {
    const t = state && state.tabs.find((x) => x.id === item.id);
    return t && t.url && !t.private ? { kind: 'page', url: t.url.full, title: t.title || t.url.registrable } : null;
  }
  const b = item.kind === 'pin' ? pins.find((p) => p.url === item.url) : item.kind === 'bookmark' ? bookmarks.find((x) => x.url === item.url) : null;
  return b ? { kind: 'page', url: b.url, title: b.title || b.display.registrable } : null;
}

function dropTargetAt(x, y) {
  const hit = document.elementFromPoint(x, y);
  if (!hit) return null;
  const kind = dragState.item.kind;
  // Into a notebook (2026-11-24): on the one shown, a link where it is let
  // go; on a research session's row, a link in that session's notebook.
  const page = (hit.closest('#notebook') || hit.closest('.rs-row')) && draggedPage(dragState.item);
  if (page && hit.closest('#notebook')) return { kind: 'notebook', page, x, y };
  if (page && hit.closest('.rs-row')) return { kind: 'rs-notebook', page, row: hit.closest('.rs-row'), space: hit.closest('.rs-row').dataset.space };
  // A group's heading goes only into the bookmarks bar (a tab group
  // becomes a bookmark group, 2026-11-24).
  if (kind === 'bgroup') return null;
  if (kind === 'tgroup') {
    const list = hit.closest('#bookmarks');
    return list && !privateGroup(dragState.item.group) ? { kind: 'bookmarks', index: bookmarks.length, list, items: [], atEnd: true } : null;
  }
  const grid = hit.closest('#pins');
  if (grid) {
    if (dragState.item.kind === 'tab' && dragState.item.private) return null; // never persist private tabs
    const tiles = [...grid.querySelectorAll('.pin')];
    const index = tiles.filter((t) => { const r = t.getBoundingClientRect(); return r.left + r.width / 2 < x; }).length;
    return { kind: 'pins', index, grid };
  }
  const list = hit.closest('#bookmarks');
  if (list) {
    if (dragState.item.kind === 'tab' && dragState.item.private) return null; // never persist private tabs
    // Rows of folded groups are not on screen; `index` is the place in
    // the whole list (each row's data-index). Dropped inside a group's
    // box, the bookmark is in that group.
    const items = [...list.querySelectorAll('.bm')].filter((n) => n.getBoundingClientRect().height > 0);
    let at = items.length;
    for (let i = 0; i < items.length; i++) {
      const r = items[i].getBoundingClientRect();
      if (y < r.top + r.height / 2) { at = i; break; }
    }
    const index = at < items.length ? Number(items[at].dataset.index) : bookmarks.length;
    const box = hit.closest('.bgroup');
    const group = box && !box.classList.contains('collapsed') ? box.dataset.group : null;
    return { kind: 'bookmarks', index, list, items, at, atEnd: at === items.length, group };
  }
  if (dragState.item.kind !== 'tab') return null;
  const item = dragState.item;
  const group = item.paired ? dragState.node.closest('.tab-group') : null;
  if (group && group.contains(hit)) return null; // still inside its own pair
  // A tab group's heading: its top half puts the tab before the group,
  // its bottom half first in it.
  const head = hit.closest('.tgroup > .group-head');
  if (head) {
    const box = head.parentElement;
    const first = box.querySelector('.tab[data-tab]');
    const r = head.getBoundingClientRect();
    const inside = y > r.top + r.height / 2;
    if (first && Number(first.dataset.tab) !== item.id) {
      return { kind: 'reorder', before: Number(first.dataset.tab), node: inside ? first : head, edge: 'top', group: inside ? Number(box.dataset.group) : null };
    }
  }
  const node = hit.closest('.tab');
  if (node) {
    const id = Number(node.dataset.tab);
    const other = state.tabs.find((x) => x.id === id);
    // Tabs of different profiles are never paired or mixed.
    if (id !== item.id && other && other.private === item.private) {
      // The top and bottom edges of a row reorder (2026-11-09); its
      // middle pairs the two tabs in split view, as before.
      const r = node.getBoundingClientRect();
      const at = (y - r.top) / Math.max(1, r.height);
      // Next to a grouped tab, the dropped tab joins its group.
      const box = node.closest('.tgroup');
      const group = box ? Number(box.dataset.group) : null;
      if (at < REORDER_EDGE) return { kind: 'reorder', before: id, node, edge: 'top', group };
      if (at > 1 - REORDER_EDGE) return { kind: 'reorder', before: tabAfter(id), node, edge: 'bottom', group };
      return { kind: 'tab', id, node };
    }
  }
  // Below the last row of its own list: to the end.
  const tabList = hit.closest('#tabs, #private-tabs');
  if (tabList && (tabList.id === 'private-tabs') === !!item.private) {
    const rows = [...tabList.querySelectorAll('.tab')].filter((n) => n !== dragState.node && n.getBoundingClientRect().height > 0);
    const last = rows[rows.length - 1];
    if (last && y > last.getBoundingClientRect().bottom) return { kind: 'reorder', before: null, node: last, edge: 'bottom', group: null };
  }
  return group ? { kind: 'unsplit', group } : null;
}

// Share of a tab row's height, at its top and at its bottom, where a
// dropped tab is moved instead of paired.
const REORDER_EDGE = 0.3;
// The tab after `id` in the browser's order (null at the end).
function tabAfter(id) {
  const i = state.tabs.findIndex((t) => t.id === id);
  const next = i >= 0 ? state.tabs[i + 1] : null;
  return next ? next.id : null;
}

function showDropFeedback(target) {
  document.querySelectorAll('.drop-target').forEach((n) => n.classList.remove('drop-target'));
  $('notebook').classList.toggle('nb-drop', !!target && target.kind === 'notebook');
  if (target && target.kind === 'rs-notebook') target.row.classList.add('drop-target');
  document.querySelectorAll('.tab-group.leaving').forEach((n) => n.classList.remove('leaving'));
  const line = $('drop-line');
  line.hidden = true;
  if (!target) return;
  if (target.kind === 'notebook' || target.kind === 'rs-notebook') {
    return;
  } else if (target.kind === 'tab') {
    target.node.classList.add('drop-target');
  } else if (target.kind === 'reorder') {
    const r = target.node.getBoundingClientRect();
    const lr = (target.node.closest('.tgroup') || target.node.closest('.tablist')).getBoundingClientRect();
    line.style.left = lr.left + 6 + 'px';
    line.style.width = lr.width - 12 + 'px';
    line.style.top = (target.edge === 'top' ? r.top - 1 : r.bottom + 1) + 'px';
    line.hidden = false;
  } else if (target.kind === 'unsplit') {
    target.group.classList.add('leaving');
  } else if (target.kind === 'pins') {
    target.grid.classList.add('drop-target');
  } else {
    target.list.classList.add('drop-target');
    const zone = target.list.querySelector('.drop-zone');
    if (zone) zone.classList.toggle('hot', target.atEnd);
    const lr = target.list.getBoundingClientRect();
    let y;
    if (!target.items.length) y = lr.top + lr.height / 2;
    else if (!target.atEnd) y = target.items[target.at].getBoundingClientRect().top - 1;
    else y = target.items[target.items.length - 1].getBoundingClientRect().bottom + 1;
    line.style.left = lr.left + 6 + 'px';
    line.style.width = lr.width - 12 + 'px';
    line.style.top = y + 'px';
    line.hidden = !!(zone && zone.classList.contains('hot'));
  }
}

function endDrag(commit) {
  const d = dragState;
  dragState = null;
  document.body.classList.remove('dragging');
  document.querySelectorAll('.drop-zone').forEach((n) => n.remove());
  d.ghost.remove();
  d.node.classList.remove('drag-src');
  showDropFeedback(null);
  // Only a drag that moved replaces the click (see above).
  if (d.moved) {
    suppressClick = true;
    setTimeout(() => { suppressClick = false; }, 0);
  }
  const t = d.target;
  if (!commit || !t) return;
  if (t.kind === 'notebook') {
    ensureNotebook().then((nb) => { nb.addLinkAt(t.page, { x: t.x, y: t.y }); toast('Added to the notebook.'); });
  } else if (t.kind === 'rs-notebook') {
    addToSessionNotebook(t.space, d.item, t.page);
  } else if (d.item.kind === 'tab' && t.kind === 'tab') {
    invoke('split_tabs', { tab: d.item.id, with: t.id });
  } else if (d.item.kind === 'tab' && t.kind === 'reorder') {
    // A half of a split pair dragged elsewhere in the list leaves its pair.
    const unpair = d.item.paired ? invoke('unsplit', { tab: d.item.id }) : Promise.resolve();
    unpair.then(() => invoke('move_tab', { tab: d.item.id, before: t.before, group: t.group === undefined ? null : t.group }));
  } else if (t.kind === 'unsplit') {
    invoke('unsplit', { tab: d.item.id });
  } else if (d.item.kind === 'tgroup' && t.kind === 'bookmarks') {
    T.core.invoke('bookmark_tab_group', { group: d.item.group })
      .then((id) => { if (!id) toast('Nothing to bookmark yet.'); return loadBookmarks(); })
      .catch((e) => toast(String(e)));
  } else if (d.item.kind === 'tab' && t.kind === 'bookmarks') {
    T.core.invoke('bookmark_tab', { tab: d.item.id, index: t.index, group: t.group || null })
      .then((added) => { if (!added) toast('Already bookmarked, or nothing to bookmark yet.'); return loadBookmarks(); })
      .catch((e) => toast(String(e)));
  } else if (d.item.kind === 'bookmark' && t.kind === 'bookmarks') {
    const to = t.index > d.item.index ? t.index - 1 : t.index;
    const group = t.group || null;
    if (to !== d.item.index || group !== d.item.group) invoke('move_bookmark', { url: d.item.url, index: to, group }).then(loadBookmarks);
  } else if (t.kind === 'pins' && d.item.kind === 'pin') {
    const to = t.index > d.item.index ? t.index - 1 : t.index;
    if (to !== d.item.index) invoke('move_pin', { url: d.item.url, index: to }).then(loadPins);
  } else if (t.kind === 'pins') {
    if (pins.length >= MAX_PINS) { toast(`Pinned holds up to ${MAX_PINS} sites. Unpin one first.`); return; }
    const call = d.item.kind === 'tab'
      ? T.core.invoke('pin_tab', { tab: d.item.id, index: t.index })
      : T.core.invoke('pin_bookmark', { url: d.item.url, index: t.index });
    call.then((ok) => { if (!ok) toast('Already pinned, or nothing to pin yet.'); return Promise.all([loadPins(), loadBookmarks()]); })
      .catch((e) => toast(String(e)));
  } else if (d.item.kind === 'pin' && t.kind === 'bookmarks') {
    invoke('unpin', { url: d.item.url, bookmarkIndex: t.index }).then(() => Promise.all([loadPins(), loadBookmarks()]));
  }
}

function dragMoveTo(x, y) {
  if (pendingDrag && Math.hypot(x - pendingDrag.x, y - pendingDrag.y) > MOVE_TOLERANCE_PX) {
    if (pendingDrag.mouse) startDrag(); // pressed and moved: a drag
    else cancelPending(); // touch: the user is scrolling
  }
  if (!dragState) return;
  if (!dragState.moved && Math.hypot(x - dragState.x, y - dragState.y) > MOVE_TOLERANCE_PX) dragState.moved = true;
  dragState.ghost.style.left = x - dragState.dx + 'px';
  dragState.ghost.style.top = y - dragState.dy + 'px';
  dragState.target = dropTargetAt(x, y);
  showDropFeedback(dragState.target);
}

// WebKit on iPadOS may cancel the pointer stream during a long press
// (it hands the touch to its own scroll or drag recognisers), depending on
// what is under the finger. While a finger is still down, a cancelled
// pointer therefore does not end the drag: touch events carry it on.
let touchDown = false;
document.addEventListener('pointermove', (ev) => dragMoveTo(ev.clientX, ev.clientY));
document.addEventListener('pointerup', () => { cancelPending(); if (dragState) endDrag(true); });
document.addEventListener('pointercancel', () => {
  if (touchDown) return;
  cancelPending();
  if (dragState) endDrag(false);
});
document.addEventListener('touchstart', () => { touchDown = true; }, { passive: true, capture: true });
// Once a drag has started, stop the sidebar from scrolling under it.
document.addEventListener('touchmove', (ev) => {
  const t = ev.touches[0];
  if (t) dragMoveTo(t.clientX, t.clientY);
  if (dragState) ev.preventDefault();
}, { passive: false });
const touchEnded = (commit) => (ev) => {
  if (ev.touches.length) return;
  touchDown = false;
  cancelPending();
  if (dragState) endDrag(commit);
};
document.addEventListener('touchend', touchEnded(true));
document.addEventListener('touchcancel', touchEnded(false));
