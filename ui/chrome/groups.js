// Browser chrome (see core.js): choosing several tabs or bookmarks (⇧- and
// ⌘-click), tab groups and bookmark groups in the sidebar, and the row
// menu's group actions, Remove and Send to (owner request 2026-11-24).
'use strict';

// ---- choosing rows -----------------------------------------------------------
//
// Tabs (by id) or bookmarks (by URL), one kind at a time. ⌘-click adds or
// removes a row (the tab shown is chosen with the first); ⇧-click chooses
// every row from the last one clicked; a plain click opens the row and
// ends the choice, as does Escape. The row menu then acts on all of them.

let chosen = { kind: null, keys: [] };
let chosenAnchor = null;

function rowKind(node) { return node.classList.contains('tab') ? 'tab' : 'bookmark'; }
function rowKey(node) { return node.dataset.tab !== undefined ? Number(node.dataset.tab) : node.dataset.url; }
function rowsOf(kind) {
  const sel = kind === 'tab' ? '#tabs .tab[data-tab], #private-tabs .tab[data-tab]' : '#bookmarks .bm[data-url]';
  return [...document.querySelectorAll(sel)];
}
function isChosen(kind, key) { return chosen.kind === kind && chosen.keys.includes(key); }

// A row's click with ⌘ or ⇧ chooses rows instead of opening one: true
// when it did. Pins are never chosen.
function chooseClick(ev, node) {
  if (!(ev.metaKey || ev.ctrlKey || ev.shiftKey)) {
    clearChosen();
    return false;
  }
  const kind = rowKind(node);
  const key = rowKey(node);
  if (chosen.kind !== kind) chosen = { kind, keys: [] };
  const rows = rowsOf(kind).filter((n) => !n.closest('.collapsed'));
  if (ev.shiftKey) {
    const from = chosenAnchor && chosenAnchor.kind === kind ? chosenAnchor.key : (kind === 'tab' ? shownTab() : null);
    const a = rows.findIndex((n) => rowKey(n) === from);
    const b = rows.indexOf(node);
    if (a >= 0 && b >= 0) {
      chosen.keys = rows.slice(Math.min(a, b), Math.max(a, b) + 1).map(rowKey);
    } else {
      chosen.keys = [key];
    }
  } else {
    const shown = kind === 'tab' ? shownTab() : selectedLauncher();
    if (!chosen.keys.length && shown !== null && shown !== key && rows.some((n) => rowKey(n) === shown)) chosen.keys.push(shown);
    chosen.keys = chosen.keys.includes(key) ? chosen.keys.filter((k) => k !== key) : chosen.keys.concat([key]);
    chosenAnchor = { kind, key };
  }
  if (!chosen.keys.length) chosen.kind = null;
  renderChosen();
  return true;
}

function clearChosen() {
  if (!chosen.keys.length && !chosenAnchor) return;
  chosen = { kind: null, keys: [] };
  chosenAnchor = null;
  renderChosen();
}

// Marks the chosen rows (again, after the list was drawn); forgets rows
// that are gone.
function renderChosen() {
  if (chosen.kind) {
    const there = new Set(rowsOf(chosen.kind).map(rowKey));
    chosen.keys = chosen.keys.filter((k) => there.has(k));
    if (!chosen.keys.length) chosen.kind = null;
  }
  for (const kind of ['tab', 'bookmark']) {
    for (const n of rowsOf(kind)) n.classList.toggle('chosen', isChosen(kind, rowKey(n)));
  }
}
document.addEventListener('keydown', (ev) => { if (ev.key === 'Escape') clearChosen(); });

// The rows a menu opened on `key` acts on: every chosen row when it is one
// of them, else that row alone (the choice ends).
function menuTargets(kind, key) {
  if (isChosen(kind, key) && chosen.keys.length > 1) return chosen.keys.slice();
  clearChosen();
  return [key];
}

// ---- tab groups and bookmark groups -------------------------------------------

function groupNode(kind, g, count, active) {
  const box = el('div', (kind === 'tab' ? 'tgroup' : 'bgroup') + (g.collapsed ? ' collapsed' : '') + (active ? ' active' : ''));
  box.dataset.group = String(g.id);
  box.setAttribute('role', 'group');
  box.setAttribute('aria-label', g.name);
  const head = el('div', 'group-head');
  const toggle = el('button', 'group-toggle');
  toggle.setAttribute('aria-expanded', String(!g.collapsed));
  toggle.title = g.collapsed ? 'Show the group' : 'Fold the group (double-click to rename)';
  toggle.append(el('span', 'group-chev', g.collapsed ? '▸' : '▾'), el('span', 'group-name', g.name), el('span', 'group-count', String(count)));
  toggle.addEventListener('click', () => {
    if (suppressClick) return;
    if (kind === 'tab') invoke('edit_tab_group', { group: g.id, collapsed: !g.collapsed });
    else invoke('edit_bookmark_group', { id: g.id, collapsed: !g.collapsed }).then(loadBookmarks);
  });
  toggle.addEventListener('dblclick', (ev) => { ev.preventDefault(); renameGroup(kind, g.id); });
  const close = el('button', 'close', '×');
  close.setAttribute('aria-label', kind === 'tab' ? `Close the ${count} tabs of ${g.name}` : `Remove the ${count} bookmarks of ${g.name}`);
  close.addEventListener('click', (ev) => { ev.stopPropagation(); removeGroup(kind, g.id); });
  const acts = el('span', 'row-actions');
  acts.appendChild(close);
  head.append(toggle, acts);
  head.addEventListener('contextmenu', (ev) => {
    ev.preventDefault();
    cancelPending();
    openGroupMenu(kind, g, head);
  });
  head.addEventListener('pointerdown', (ev) => armDrag(ev, head, { kind: kind === 'tab' ? 'tgroup' : 'bgroup', group: g.id }));
  box.appendChild(head);
  return box;
}

// A tab group's rows: a pair of split tabs as one row if both are in it.
function tabGroupNode(g, members, byId, placed) {
  const box = groupNode('tab', g, members.length, members.some((m) => m.id === shownTab()));
  for (const m of members) {
    if (placed.has(m.id)) continue;
    const pair = pairOf(m.id);
    const other = pair && byId.get(pair.left === m.id ? pair.right : pair.left);
    if (pair && other && other.group === g.id) {
      const row = el('div', 'tab-group child' + (shownTab() === pair.left || shownTab() === pair.right ? ' active' : ''));
      row.dataset.pair = String(pair.left);
      for (const id of [pair.left, pair.right]) { row.appendChild(tabNode(byId.get(id), pair)); placed.add(id); }
      box.appendChild(row);
    } else {
      const node = tabNode(m, pair);
      node.classList.add('child');
      box.appendChild(node);
      placed.add(m.id);
    }
  }
  return box;
}

// Renames a group in place: its name becomes a field (↩ keeps, Escape
// drops the change).
function renameGroup(kind, id) {
  const box = [...document.querySelectorAll(kind === 'tab' ? '.tgroup' : '.bgroup')].find((b) => b.dataset.group === String(id));
  const toggle = box && box.querySelector('.group-head > .group-toggle');
  if (!toggle || box.querySelector('.group-rename')) return;
  const label = toggle.querySelector('.group-name');
  // Beside the heading's button, not in it (a space there would fold).
  const field = el('input', 'group-rename');
  field.value = label.textContent;
  field.maxLength = 60;
  field.setAttribute('aria-label', 'Group name');
  let done = false;
  const finish = (keep) => {
    if (done) return;
    done = true;
    const name = field.value.trim();
    field.remove();
    toggle.hidden = false;
    if (!keep || !name || name === label.textContent) return;
    label.textContent = name;
    if (kind === 'tab') invoke('edit_tab_group', { group: id, name });
    else invoke('edit_bookmark_group', { id, name }).then(loadBookmarks);
  };
  field.addEventListener('keydown', (ev) => {
    ev.stopPropagation();
    if (ev.key === 'Enter') finish(true);
    else if (ev.key === 'Escape') finish(false);
  });
  field.addEventListener('blur', () => finish(true));
  field.addEventListener('pointerdown', (ev) => ev.stopPropagation());
  toggle.hidden = true;
  toggle.before(field);
  focusInput(field, true);
}

function tabGroupMembers(id) {
  return state ? state.tabs.filter((t) => t.group === id).map((t) => t.id) : [];
}
function bookmarkGroupMembers(id) {
  return bookmarks.filter((b) => b.group === id).map((b) => b.url);
}

function removeGroup(kind, id) {
  const g = kind === 'tab' ? (state.groups || []).find((x) => x.id === id) : bookmarkGroups.find((x) => x.id === id);
  if (!g) return;
  const anchor = () => [...document.querySelectorAll(kind === 'tab' ? '.tgroup' : '.bgroup')].find((b) => b.dataset.group === String(id));
  if (kind === 'tab') {
    const tabs = tabGroupMembers(id);
    askConfirm(`Close the ${tabs.length} tabs of “${g.name}”?`, 'Close tabs', null, () => invoke('close_tabs', { tabs }), null, { anchor });
  } else {
    const urls = bookmarkGroupMembers(id);
    askConfirm(`Remove the ${urls.length} bookmarks of “${g.name}”?`, 'Remove', null, () => invoke('remove_bookmarks', { urls }).then(loadBookmarks), null, { anchor });
  }
}

// After a group is made from the menu, its name is ready to be typed
// (once it is drawn: the state may come before or after the answer).
let renameNewGroup = null;
function afterRender() {
  renderChosen();
  if (renameNewGroup) {
    const { kind, id } = renameNewGroup;
    if ([...document.querySelectorAll(kind === 'tab' ? '.tgroup' : '.bgroup')].some((b) => b.dataset.group === String(id))) {
      renameNewGroup = null;
      setTimeout(() => renameGroup(kind, id), 0);
    }
  }
}
function renameWhenDrawn(kind, id) {
  renameNewGroup = { kind, id };
  afterRender();
}

// ---- the row menu: groups, Remove, Send to ---------------------------------------

// Where tabs can be sent: everyday browsing (when not in it), the other
// research sessions, and a new one.
function sendTargets() {
  const here = state ? state.space || null : null;
  const out = [];
  if (here) out.push({ to: null, label: 'Everyday Browsing' });
  for (const r of (state && state.research) || []) if (r.id !== here) out.push({ to: r.id, label: r.name });
  out.push({ to: 'new', label: 'New Research Session' });
  return out;
}

function menuButton(label, run) {
  const b = el('button', null, label);
  b.setAttribute('role', 'menuitem');
  b.addEventListener('click', () => { closePinPop(); run(); });
  return b;
}

// Sends tabs (`tabs`, moved) or pins' and bookmarks' pages (`urls`,
// copied) to a session; `to: 'new'` makes one first (entered, with the
// tabs; its empty start tab goes).
async function sendTo(to, { tabs = [], urls = [] }) {
  try {
    let target = to;
    let fresh = null;
    if (to === 'new') {
      target = await T.core.invoke('new_research', { name: 'New Research' });
      fresh = state && state.active;
      researchRenamePending = target;
    }
    if (tabs.length) await T.core.invoke('send_tabs', { tabs, to: target });
    for (const url of urls) await T.core.invoke('send_page', { url, to: target });
    if (to === 'new') {
      const [first] = tabs;
      if (first !== undefined) {
        await T.core.invoke('activate_tab', { tab: first });
        // Its start tab was the one shown when the session was made.
        const start = await T.core.invoke('get_state').then((s) => s.tabs.find((t) => t.space === target && !t.url && t.id !== first));
        if (start) await T.core.invoke('close_tab', { tab: start.id });
      }
      return;
    }
    const name = target ? ((state.research || []).find((r) => r.id === target) || {}).name : 'Everyday Browsing';
    const n = tabs.length + urls.length;
    toast(`${n === 1 ? 'Sent' : `${n} sent`} to ${name || 'the research session'}.`);
  } catch (e) {
    toast(String(e));
  }
}

function sendSection(more, what) {
  more.appendChild(el('div', 'menu-sep'));
  more.appendChild(el('div', 'menu-label', 'Send to'));
  for (const t of sendTargets()) more.appendChild(menuButton(t.label, () => sendTo(t.to, what)));
}

// Fills the item menu's lower part for a pin, bookmark(s) or tab(s), and
// hides the one-item rows when several are chosen.
function fillItemMore(kind, keys) {
  const more = $('item-more');
  more.replaceChildren();
  const several = keys.length > 1;
  if (several) {
    for (const id of ['item-copy', 'item-edit', 'item-original', 'pin-to-bookmark']) $(id).hidden = true;
    for (const e of $('pin-pop').querySelectorAll('.pin-only')) e.hidden = true;
    more.appendChild(el('div', 'menu-label', `${keys.length} ${kind === 'tab' ? 'tabs' : 'bookmarks'}`));
  } else {
    more.appendChild(el('div', 'menu-sep'));
  }
  if (kind === 'pin') {
    more.replaceChildren();
    sendSection(more, { urls: keys });
    return;
  }
  if (kind === 'tab') {
    const tabs = keys.map((id) => state.tabs.find((t) => t.id === id)).filter(Boolean);
    const groupable = tabs.filter((t) => t.launcher === null && !t.saved);
    if (groupable.length) {
      more.appendChild(menuButton(several ? 'Group These Tabs' : 'Add to New Group', () => {
        T.core.invoke('group_tabs', { tabs: groupable.map((t) => t.id), name: '' })
          .then((id) => { clearChosen(); if (id !== null && id !== undefined) renameWhenDrawn('tab', id); })
          .catch((e) => toast(String(e)));
      }));
    }
    if (tabs.some((t) => t.group !== null)) {
      more.appendChild(menuButton('Remove from Group', () => invoke('ungroup_tabs', { tabs: tabs.map((t) => t.id) })));
    }
    more.appendChild(menuButton('Remove', () => { clearChosen(); invoke('close_tabs', { tabs: tabs.map((t) => t.id) }); }));
    const movable = groupable.filter((t) => t.sheet_over === null);
    if (movable.length) sendSection(more, { tabs: movable.map((t) => t.id) });
    return;
  }
  // Bookmarks.
  const items = keys.map((url) => bookmarks.find((b) => b.url === url)).filter(Boolean);
  more.appendChild(menuButton(several ? 'Group These Bookmarks' : 'Add to New Group', () => {
    T.core.invoke('group_bookmarks', { urls: items.map((b) => b.url), name: '' })
      .then((id) => { clearChosen(); if (id) renameNewGroup = { kind: 'bookmark', id }; return loadBookmarks(); })
      .catch((e) => toast(String(e)));
  }));
  if (items.some((b) => b.group)) {
    more.appendChild(menuButton('Remove from Group', async () => {
      for (const b of items.filter((x) => x.group)) {
        await T.core.invoke('move_bookmark', { url: b.url, index: bookmarks.indexOf(b), group: null }).catch(() => {});
      }
      loadBookmarks();
    }));
  }
  more.appendChild(menuButton('Remove', () => {
    clearChosen();
    invoke('remove_bookmarks', { urls: items.map((b) => b.url) }).then(loadBookmarks);
  }));
  sendSection(more, { urls: items.map((b) => b.url) });
}

// A group heading's menu (right-click): Rename, Ungroup, Move to
// Bookmarks (tab groups), Remove and Send to.
function openGroupMenu(kind, g, head) {
  openPinPop({ url: '', id: null }, head, 'group');
  for (const id of ['item-copy', 'item-edit', 'item-original', 'pin-to-bookmark']) $(id).hidden = true;
  for (const e of $('pin-pop').querySelectorAll('.pin-only, .item-only')) e.hidden = true;
  const more = $('item-more');
  more.replaceChildren(el('div', 'menu-label', g.name));
  more.appendChild(menuButton('Rename', () => renameGroup(kind, g.id)));
  if (kind === 'tab') {
    const tabs = tabGroupMembers(g.id);
    more.appendChild(menuButton('Ungroup', () => invoke('edit_tab_group', { group: g.id, ungroup: true })));
    const priv = tabs.every((id) => (state.tabs.find((t) => t.id === id) || {}).private);
    if (!priv) {
      more.appendChild(menuButton('Move to Bookmarks', () => {
        T.core.invoke('bookmark_tab_group', { group: g.id })
          .then((id) => { if (!id) toast('Nothing to bookmark yet.'); return loadBookmarks(); })
          .catch((e) => toast(String(e)));
      }));
    }
    more.appendChild(menuButton('Remove', () => removeGroup('tab', g.id)));
    sendSection(more, { tabs });
  } else {
    more.appendChild(menuButton('Ungroup', () => invoke('edit_bookmark_group', { id: g.id, ungroup: true }).then(loadBookmarks)));
    more.appendChild(menuButton('Remove', () => removeGroup('bookmark', g.id)));
    sendSection(more, { urls: bookmarkGroupMembers(g.id) });
  }
  placePinPop(head);
}

// Keeps the item menu inside the sidebar (it grew with the Send section).
function placePinPop(node) {
  const pop = $('pin-pop');
  const side = $('sidebar').getBoundingClientRect();
  const r = node.getBoundingClientRect();
  let top = r.bottom - side.top + 6;
  const h = pop.offsetHeight;
  if (top + h > side.height - 8) top = Math.max(8, r.top - side.top - h - 6);
  pop.style.top = Math.round(top) + 'px';
}
