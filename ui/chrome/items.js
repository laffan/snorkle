// Browser chrome (see core.js): Pins, bookmarks and the Edit dialog.
'use strict';

// ---- opening links from pins / bookmarks / shortcuts -----------------------

// Each pin and bookmark has its own tab, which is not listed with the
// other tabs: opening one returns to it (created on first use), and the
// pin or bookmark is shown as selected.
// Opens a pin or bookmark (its own tab, as left); `reset` goes back to
// its saved address ("Return to Original URL").
function openLink(url, reset = false) {
  const opened = invoke('open_launcher', { url, reset });
  if (narrow) setSidebar(false);
  return opened;
}

// The pin or bookmark whose tab is shown, if any.
function selectedLauncher() {
  const id = state && shownTab();
  const t = state && state.tabs.find((x) => x.id === id);
  return (t && t.launcher) || null;
}

function renderLauncherSelection() {
  const sel = selectedLauncher();
  document.querySelectorAll('.pin, .bm').forEach((n) => n.classList.toggle('active', n.dataset.url === sel));
}

// ---- pinned grid ----------------------------------------------------------

// Bundled icons (ui/icons, Simple Icons, CC0) for sites whose favicons
// the fetcher cannot get. First match wins: host, then optional path prefix.
const PREFAB_ICONS = [
  ['mail.google.com', '', 'gmail'],
  ['drive.google.com', '', 'googledrive'],
  ['docs.google.com', '/spreadsheets', 'googlesheets'],
  ['docs.google.com', '/presentation', 'googleslides'],
  ['docs.google.com', '/forms', 'googleforms'],
  ['docs.google.com', '', 'googledocs'],
  ['sheets.google.com', '', 'googlesheets'],
  ['slides.google.com', '', 'googleslides'],
  ['calendar.google.com', '', 'googlecalendar'],
  ['keep.google.com', '', 'googlekeep'],
  ['meet.google.com', '', 'googlemeet'],
  ['chat.google.com', '', 'googlechat'],
  ['maps.google.com', '', 'googlemaps'],
  ['www.google.com', '/maps', 'googlemaps'],
  ['photos.google.com', '', 'googlephotos'],
  ['www.youtube.com', '', 'youtube'],
  ['youtube.com', '', 'youtube'],
];

function prefabIcon(url) {
  let u;
  try { u = new URL(url); } catch (e) { return null; }
  const hit = PREFAB_ICONS.find(([host, path]) => u.hostname === host && u.pathname.startsWith(path));
  return hit ? `icons/${hit[2]}.svg` : null;
}

function loadPins() {
  return T.core.invoke('list_pins').then((items) => { pins = items; renderPins(); }).catch(() => {});
}

// Letter tile shown until (or instead of) a favicon.
function letterTile(display) {
  const name = (display && display.registrable) || '?';
  const t = el('span', 'letter', name.charAt(0).toUpperCase());
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)) % 360;
  t.style.background = `hsl(0, 0%, ${28 + (h % 5) * 6}%)`; // greys only
  return t;
}

function renderPins() {
  const grid = $('pins');
  grid.replaceChildren();
  // Empty: nothing (no explainer text, 2026-11-23); it opens up as a
  // drop target while a row is dragged (CSS).
  if (!pins.length) return;
  pins.forEach((p, index) => {
    const node = el('div', 'pin');
    node.dataset.index = String(index);
    node.dataset.url = p.url;
    node.title = p.title || p.display.registrable;
    const open = el('button', 'open');
    open.setAttribute('aria-label', `Open ${p.title || p.display.registrable}`);
    const prefab = prefabIcon(p.url);
    if (p.emoji) {
      open.appendChild(el('span', 'emoji', p.emoji));
    } else if (prefab) {
      const img = el('img', 'prefab');
      img.alt = '';
      img.src = prefab;
      open.appendChild(img);
    } else if (p.icon && p.icon.startsWith('data:image/png;base64,')) {
      const img = el('img');
      img.alt = '';
      // Too dark for a dark chrome: its light copy (2026-11-24).
      const icon = chromeDark() && p.icon_dark && p.icon_dark.startsWith('data:image/png;base64,') ? p.icon_dark : p.icon;
      // Never show a broken image: fall back to the letter tile.
      img.addEventListener('error', () => {
        chromeLog('warn', `favicon for ${p.display.registrable} could not be displayed (${p.icon.length} chars)`);
        img.replaceWith(letterTile(p.display));
      });
      img.src = icon;
      open.appendChild(img);
    } else {
      open.appendChild(letterTile(p.display));
    }
    open.addEventListener('click', (ev) => {
      if (suppressClick) return;
      if (ev.metaKey) { openPinPop(p, node); return; } // ⌘-click: the pin menu
      openLink(p.url);
    });
    node.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      cancelPending();
      openPinPop(p, node);
      fillItemMore('pin', [p.url]);
      placePinPop(node);
    });
    const rm = el('button', 'unpin', '\u00D7');
    rm.setAttribute('aria-label', 'Unpin');
    // Asked first (owner request 2026-11-24), beside the pin; its tab stays
    // open as an ordinary tab.
    rm.addEventListener('click', (ev) => {
      ev.stopPropagation();
      const name = p.title || p.display.registrable;
      const anchor = () => [...document.querySelectorAll('#pins .pin')].find((n) => n.dataset.url === p.url);
      askConfirm(`Unpin “${name}”?`, 'Unpin', null, () => invoke('unpin', { url: p.url, bookmarkIndex: null }).then(loadPins), null, { anchor });
    });
    node.append(open, rm);
    node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'pin', url: p.url, index }));
    grid.appendChild(node);
  });
  if (state) renderLauncherSelection();
}

// Item menu (right-click or ⌘-click on a pin or bookmark, right-click on
// a tab): Copy URL and Edit… for all three; Return to Original URL for
// pins and bookmarks; Return to Bookmark for pins. Placed under the item,
// inside the sidebar.
let pinMenu = null; // the pin, bookmark or tab, plus { kind: 'pin' | 'bookmark' | 'tab' }
function openPinPop(p, node, kind = 'pin') {
  closeMenu();
  pinMenu = { ...p, kind };
  const pop = $('pin-pop');
  const side = $('sidebar').getBoundingClientRect();
  pop.style.top = Math.round(node.getBoundingClientRect().bottom - side.top + 6) + 'px';
  for (const e of pop.querySelectorAll('.pin-only')) e.hidden = kind !== 'pin';
  for (const e of pop.querySelectorAll('.item-only')) e.hidden = kind === 'tab';
  $('item-copy').hidden = kind === 'tab' && !p.url;
  $('item-edit').hidden = false;
  $('item-more').replaceChildren();
  delete pop.dataset.space;
  pop.hidden = false;
  syncMenuOpen();
}
function closePinPop() {
  if ($('pin-pop').hidden) return;
  $('pin-pop').hidden = true;
  pinMenu = null;
  syncMenuOpen();
}
// Copy URL: the page the tab shows; for a pin or bookmark, the page its
// tab shows now, or its saved address if it has no tab yet.
function itemPageUrl(p) {
  if (p.kind === 'tab') return p.url;
  const t = state && state.tabs.find((x) => x.launcher === p.url && x.sheet_over === null);
  return (t && t.url && t.url.full) || p.url;
}
$('item-copy').addEventListener('click', () => {
  const p = pinMenu;
  closePinPop();
  if (!p || !p.url) return;
  navigator.clipboard.writeText(itemPageUrl(p)).then(() => toast('Address copied.'), () => toast('Could not copy (clipboard unavailable).'));
});
// Back to the address the item was saved with, in its own tab.
$('item-original').addEventListener('click', () => {
  const p = pinMenu;
  closePinPop();
  if (p) openLink(p.url, true);
});
$('item-edit').addEventListener('click', () => {
  const p = pinMenu;
  closePinPop();
  if (p) openEdit(p);
});
$('pin-to-bookmark').addEventListener('click', () => {
  const p = pinMenu;
  closePinPop();
  if (p) invoke('unpin', { url: p.url, bookmarkIndex: 0 }).then(() => Promise.all([loadPins(), loadBookmarks()]));
});

// ---- Edit (owner request 2026-11-09) -----------------------------------
//
// One dialog for tabs, pins and bookmarks: title, URL and an emoji shown
// instead of the favicon. Tabs: the title is the tab's own name (empty
// follows the page's title), a changed URL loads in the tab like the
// address bar, and name and emoji are kept with the tab (Rust:
// `set_tab_label`). Pins and bookmarks: `edit_item_url` (title and URL,
// validated in Rust) and `set_pin_emoji`. The emoji is typed (emoji
// keyboard or the system picker) or picked from the grid; the grid does
// not depend on the system picker, which did not fill the field on the
// iPad when the field sat in the sidebar's menu.
const EMOJI = ['⭐', '📌', '🔖', '📚', '📰', '📝', '📅', '📧', '💬', '🎵', '🎬', '📷', '🛒', '💰', '🏦', '📈',
  '🧭', '🗺️', '✈️', '🏠', '🍳', '☕', '🎮', '⚽', '🧪', '💡', '🔧', '💻', '🤖', '🔒', '❤️', '🔥',
  '✅', '🚀', '🌱', '🐱', '🐶', '🎓', '🏥', '⚖️'];
let editItem = null; // { kind, url, title, emoji, id, name }

function openEdit(p) {
  closeMenu();
  let item;
  if (p.kind === 'tab') {
    const t = state && state.tabs.find((x) => x.id === p.id);
    if (!t) return;
    item = { kind: 'tab', id: t.id, url: t.url ? t.url.full : '', title: t.name || '', placeholder: t.title || (t.url ? t.url.registrable : 'New Tab'), emoji: t.emoji || '' };
  } else {
    item = { kind: p.kind, url: p.url, title: p.title || '', placeholder: p.display ? p.display.registrable : '', emoji: p.emoji || '' };
  }
  editItem = item;
  $('edit-title').textContent = item.kind === 'tab' ? 'Edit Tab' : item.kind === 'pin' ? 'Edit Pin' : 'Edit Bookmark';
  $('edit-name').value = item.title;
  $('edit-name').placeholder = item.placeholder;
  $('edit-url').value = item.url;
  $('edit-url').placeholder = item.kind === 'tab' ? 'Search or enter address' : 'Web address';
  $('edit-emoji').value = item.emoji;
  $('edit-msg').textContent = '';
  renderEmojiGrid();
  $('edit-modal').hidden = false;
  document.body.classList.add('edit-open');
  reportLayout();
  focusInput($('edit-name'), true);
}

function closeEdit() {
  if ($('edit-modal').hidden) return;
  $('edit-modal').hidden = true;
  document.body.classList.remove('edit-open');
  for (const id of ['edit-name', 'edit-url', 'edit-emoji']) $(id).blur();
  editItem = null;
  reportLayout();
}

function renderEmojiGrid() {
  const grid = $('emoji-grid');
  const current = $('edit-emoji').value.trim();
  if (!grid.childElementCount) {
    for (const e of EMOJI) {
      const b = el('button', null, e);
      b.type = 'button';
      b.dataset.emoji = e;
      b.setAttribute('aria-label', e);
      // Keep the focus (and any open keyboard) where it is.
      b.addEventListener('pointerdown', (ev) => ev.preventDefault());
      b.addEventListener('click', () => {
        $('edit-emoji').value = $('edit-emoji').value.trim() === e ? '' : e;
        renderEmojiGrid();
      });
      grid.appendChild(b);
    }
  }
  for (const b of grid.children) b.setAttribute('aria-pressed', String(b.dataset.emoji === current));
}
$('edit-emoji').addEventListener('input', renderEmojiGrid);
$('edit-emoji-clear').addEventListener('click', () => { $('edit-emoji').value = ''; renderEmojiGrid(); });
$('edit-cancel').addEventListener('click', closeEdit);
$('edit-modal').addEventListener('pointerdown', (ev) => { if (ev.target === $('edit-modal')) closeEdit(); });

$('edit-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const item = editItem;
  if (!item) return;
  const name = $('edit-name').value.trim();
  const url = $('edit-url').value.trim();
  const emoji = $('edit-emoji').value.trim();
  const fail = (e) => { $('edit-msg').textContent = String(e); };
  try {
    if (item.kind === 'tab') {
      await T.core.invoke('set_tab_label', { tab: item.id, name, emoji: emoji || null });
      if (url && url !== item.url) await T.core.invoke('navigate', { tab: item.id, input: url });
    } else {
      if (!url) { fail('Enter a web address.'); return; }
      let key = item.url;
      if (url !== item.url || name !== item.title) {
        key = await T.core.invoke('edit_item_url', { url: item.url, newUrl: url, title: name });
      }
      if (emoji !== item.emoji) {
        const ok = await T.core.invoke('set_pin_emoji', { url: key, emoji: emoji || null });
        if (!ok) { fail('Use one emoji (no spaces).'); await Promise.all([loadPins(), loadBookmarks()]); return; }
      }
      await Promise.all([loadPins(), loadBookmarks()]);
    }
    closeEdit();
  } catch (e) {
    fail(e);
  }
});

// ---- bookmarks bar ------------------------------------------------------

// Groups of bookmarks (2026-11-24): their bookmarks name them (`group`)
// and are next to each other in the list.
let bookmarkGroups = [];
function loadBookmarks() {
  return Promise.all([T.core.invoke('list_bookmarks'), T.core.invoke('list_bookmark_groups')])
    .then(([items, groups]) => { bookmarks = items; bookmarkGroups = groups; renderBookmarks(); })
    .catch(() => {});
}

function renderBookmarks() {
  const list = $('bookmarks');
  list.replaceChildren();
  if (!bookmarks.length) return; // see renderPins
  let box = null;
  bookmarks.forEach((b, index) => {
    const g = b.group && bookmarkGroups.find((x) => x.id === b.group);
    if (!g) box = null;
    else if (!box || box.dataset.group !== g.id) {
      const members = bookmarks.filter((x) => x.group === g.id);
      box = groupNode('bookmark', g, members.length, members.some((x) => x.url === selectedLauncher()));
      list.appendChild(box);
    }
    const node = el('div', 'bm' + (g ? ' child' : ''));
    if (g) node.dataset.bgroup = g.id;
    node.dataset.index = String(index);
    node.dataset.url = b.url;
    node.appendChild(b.emoji ? emojiIcon(b.emoji) : rowIcon(b.url, b.display, tabIcons.bookmarks.get(b.url)));
    // One line, as tabs: the title (its site until it has one).
    const open = el('button', 'open');
    open.appendChild(el('span', 't', b.title || b.display.registrable));
    open.title = b.display.registrable;
    open.addEventListener('click', (ev) => {
      if (suppressClick) return;
      // ⌘- and ⇧-click choose bookmarks (2026-11-24; until then ⌘-click
      // opened the item menu, which right-click still does).
      if (chooseClick(ev, node)) return;
      openLink(b.url);
    });
    node.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      cancelPending();
      const keys = menuTargets('bookmark', b.url);
      openPinPop(b, node, 'bookmark');
      fillItemMore('bookmark', keys);
      placePinPop(node);
    });
    const rm = el('button', 'close', '×');
    rm.setAttribute('aria-label', 'Remove bookmark');
    rm.addEventListener('click', (ev) => {
      ev.stopPropagation();
      invoke('remove_bookmark', { url: b.url }).then(loadBookmarks);
    });
    const acts = el('span', 'row-actions');
    acts.appendChild(rm);
    node.append(open, acts);
    node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'bookmark', url: b.url, index, group: b.group || null }));
    (box || list).appendChild(node);
  });
  if (state) { renderLauncherSelection(); renderToolbar(activeTab()); }
  refreshTabIcons(false);
  afterRender();
}
