// Browser chrome. Trusted, bundled, local. Renders state pushed from Rust
// and sends user intents back as Tauri commands.
//
// Security rules for this file (see docs/ARCHITECTURE.md):
// - Never use innerHTML/insertAdjacentHTML/document.write; build DOM with
//   createElement + textContent only. Page-controlled strings (titles,
//   hosts) must never be interpreted as markup.
// - The address bar shows only what Rust's DisplayUrl provides.
// - Never load remote resources (CSP also forbids it).
// - Anything drawn over the content area must set `overlay` (see
//   reportLayout) so native page views are hidden while it is open.
'use strict';

(() => {
  const T = window.__TAURI__;
  const invoke = (cmd, args) => T.core.invoke(cmd, args).catch((e) => toast(String(e)));
  const $ = (id) => document.getElementById(id);

  let state = null;
  let editing = false;
  let consoleOpen = false;
  let panel = null; // null | 'history'
  let modalOpen = false;
  let scriptsOpen = false;
  let menuOpen = false;
  let bookmarks = [];
  let pins = [];
  const MAX_PINS = 4;
  let viewMode = { color: false, hide_images: true }; // session-only, set in Rust
  let scripts = [];
  let editingDomain = null; // domain of the script open in the editor, or '' for new
  let tabsKey = '';
  let toastTimer = 0;
  let clearArmed = false;
  let suppressClick = false;

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function activeTab() {
    if (!state || state.active === null) return null;
    return state.tabs.find((t) => t.id === state.active) || null;
  }

  // Renders a host inside one LTR isolate, with each label in its own
  // isolate, so RTL labels cannot visually reorder the other labels (and
  // the container's rtl direction, used only for left-side elision, cannot
  // move the dots).
  function fillLabels(node, host, trailingDot) {
    node.replaceChildren();
    if (!host) return;
    const wrap = el('bdi');
    wrap.dir = 'ltr';
    const labels = host.split('.');
    labels.forEach((label, i) => {
      wrap.appendChild(el('bdi', null, label));
      if (i < labels.length - 1 || trailingDot) wrap.appendChild(document.createTextNode('.'));
    });
    node.appendChild(wrap);
  }

  function renderUrlInto(sub, reg, port, d) {
    fillLabels(sub, d ? d.subdomain : '', !!(d && d.subdomain));
    fillLabels(reg, d ? d.registrable : '', false);
    if (port) port.textContent = d && d.port ? ':' + d.port : '';
  }

  // ---- rendering -------------------------------------------------------

  function render() {
    if (!state) return;
    const tab = activeTab();
    document.body.classList.toggle('private', !!(tab && tab.private));
    renderOs();
    renderTabs();
    renderLauncherSelection();
    renderToolbar(tab);
    renderSurface(tab);
    reportLayout();
  }

  function renderOs() {
    const os = state.os;
    const banner = $('os-banner');
    const block = $('unsupported');
    banner.hidden = os.status !== 'update_recommended' && os.status !== 'preview_only';
    if (os.status === 'preview_only') {
      banner.textContent = 'Desktop preview: only the browser chrome runs here. Web pages render on iPadOS only (device or Simulator: npm run tauri ios dev).';
    }
    block.hidden = os.status !== 'unsupported';
    const v = (x) => `${x.major}.${x.minor}.${x.patch}`;
    if (os.status === 'update_recommended') {
      banner.textContent = `iPadOS ${v(os.current)} is missing security fixes for the web engine. Update to ${v(os.recommended)} or later.`;
    } else if (os.status === 'unsupported') {
      $('unsupported-body').textContent = `This version of iPadOS (${v(os.current)}) no longer receives the web-engine security fixes Snorkle depends on. iPadOS ${v(os.minimum)} or later is required.`;
    }
  }

  // The split pair holding a tab, if any.
  function pairOf(id) {
    return (state.splits || []).find((p) => p.left === id || p.right === id) || null;
  }

  function renderTabs() {
    const key = JSON.stringify([
      state.tabs.map((t) => [t.id, t.title, t.private, t.live, t.url && t.url.registrable, t.sheet_over]),
      state.active,
      state.splits,
    ]);
    if (key === tabsKey) return;
    tabsKey = key;
    const normal = $('tabs');
    const priv = $('private-tabs');
    normal.replaceChildren();
    priv.replaceChildren();
    const byId = new Map(state.tabs.map((t) => [t.id, t]));
    const placed = new Set();
    for (const t of state.tabs) {
      // Pop-up sheets are not tabs (their opener is shown as selected), and
      // tabs that belong to a pin or bookmark are shown as that item.
      if (placed.has(t.id) || t.sheet_over !== null || t.launcher !== null) continue;
      const pair = pairOf(t.id);
      const list = t.private ? priv : normal;
      if (pair && byId.has(pair.left) && byId.has(pair.right)) {
        // A split pair is one row, at the position of whichever tab comes
        // first, left pane on the left.
        const group = el('div', 'tab-group' + (shownTab() === pair.left || shownTab() === pair.right ? ' active' : ''));
        group.dataset.pair = String(pair.left);
        group.setAttribute('role', 'group');
        group.setAttribute('aria-label', 'Split view');
        for (const id of [pair.left, pair.right]) {
          group.appendChild(tabNode(byId.get(id), pair));
          placed.add(id);
        }
        list.appendChild(group);
      } else {
        list.appendChild(tabNode(t, null));
        placed.add(t.id);
      }
    }
    $('private-head').hidden = !priv.childElementCount;
    const act = $('sidebar').querySelector('.tab.active');
    if (act && !dragState) act.scrollIntoView({ block: 'nearest' });
  }

  // The tab shown as selected in the sidebar: the active tab, or the tab
  // under the active pop-up sheet.
  function shownTab() {
    const t = activeTab();
    return t && t.sheet_over !== null ? t.sheet_over : state.active;
  }

  function tabNode(t, pair) {
    const selected = t.id === shownTab();
    const node = el('div', 'tab' + (selected ? ' active' : '') + (t.private ? ' private' : '') + (t.live ? '' : ' discarded'));
    node.dataset.tab = String(t.id);
    node.setAttribute('role', 'tab');
    node.setAttribute('aria-selected', String(selected));
    const open = el('button', 'open');
    open.appendChild(el('span', 't', t.title || (t.url ? t.url.registrable : t.private ? 'Private Tab' : 'New Tab')));
    if (t.url) {
      const host = el('span', 'h');
      fillLabels(host, t.url.registrable, false);
      open.appendChild(host);
    }
    open.addEventListener('click', () => {
      if (suppressClick) return;
      invoke('activate_tab', { tab: t.id });
      if (narrow) setSidebar(false);
    });
    node.appendChild(open);
    const close = el('button', 'close', '×');
    close.setAttribute('aria-label', 'Close tab');
    close.addEventListener('click', (ev) => { ev.stopPropagation(); invoke('close_tab', { tab: t.id }); });
    node.appendChild(close);
    node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'tab', id: t.id, private: t.private, paired: !!pair }));
    return node;
  }

  function renderToolbar(tab) {
    $('back').disabled = !(tab && tab.can_go_back);
    $('forward').disabled = !(tab && tab.can_go_forward);
    const reload = $('reload');
    reload.textContent = tab && tab.loading ? '×' : '↻';
    reload.setAttribute('aria-label', tab && tab.loading ? 'Stop' : 'Reload');
    $('bookmark').disabled = !(tab && tab.url && !tab.private);
    $('unsplit').hidden = !state.split;
    $('sheet-close').hidden = !state.sheet;
    $('sheet-pill').hidden = !state.sheet;
    renderShield(tab);
    $('private-pill').hidden = !(tab && tab.private);

    const d = tab && tab.url;
    const sec = $('security');
    sec.className = 'security';
    const security = tab ? tab.security : 'none';
    if (security === 'not_secure') { sec.textContent = 'Not Secure'; sec.classList.add('not-secure'); }
    else if (security === 'mixed') { sec.textContent = 'Not Fully Secure'; sec.classList.add('mixed'); }
    else { sec.textContent = ''; }
    $('lock').toggleAttribute('hidden', security !== 'secure'); // SVG: no .hidden property
    renderUrlInto($('url-sub'), $('url-reg'), $('url-port'), d);
    $('url-warn').hidden = !(d && d.punycode_forced);
    if (!d) $('url-reg').textContent = 'Search or enter address';

    const bar = $('progress-bar');
    const p = tab && tab.loading ? Math.max(5, tab.progress) / 100 : 0;
    bar.style.transform = `scaleX(${p})`;
  }

  function renderSurface(tab) {
    const notice = tab && tab.notice;
    const showPanel = !!panel;
    $('panel').hidden = !showPanel;
    $('notice').hidden = showPanel || !notice;
    $('start').hidden = showPanel || !!notice || !tab || !!tab.url;
    if (notice && !showPanel) renderNotice(tab, notice);
    if (!$('start').hidden) renderStart(tab);
  }

  function renderNotice(tab, n) {
    const title = $('notice-title');
    const body = $('notice-body');
    const detail = $('notice-detail');
    const primary = $('notice-primary');
    const secondary = $('notice-secondary');
    primary.className = 'button primary';
    primary.hidden = false;
    secondary.hidden = false;
    secondary.textContent = 'Go Back';
    secondary.onclick = () => invoke('go_back', { tab: tab.id });
    detail.textContent = '';
    switch (n.kind) {
      case 'https_unavailable':
        title.textContent = 'This site does not offer a secure connection';
        body.textContent = `Snorkle tried to connect to ${n.host} using HTTPS and could not. If you continue, the page loads over plain HTTP: anyone on your network can read and change what you see and send.`;
        detail.textContent = n.http_url;
        primary.textContent = 'Continue to HTTP (Not Secure)';
        primary.className = 'button danger';
        primary.onclick = () => invoke('proceed_http', { tab: tab.id });
        break;
      case 'blocked':
        title.textContent = 'Blocked';
        body.textContent = n.reason;
        detail.textContent = n.url;
        primary.textContent = 'OK';
        primary.onclick = () => invoke('dismiss_notice', { tab: tab.id });
        break;
      case 'load_error':
        title.textContent = n.certificate ? 'This connection is not private' : 'Can’t open this page';
        body.textContent = n.certificate
          ? 'The site’s certificate could not be verified. Snorkle does not allow bypassing certificate errors.'
          : n.message;
        detail.textContent = n.url;
        primary.textContent = 'Try Again';
        primary.onclick = () => invoke('reload', { tab: tab.id });
        break;
      case 'external_app':
        title.textContent = 'Open another app?';
        body.textContent = `This page wants to open a “${n.scheme}:” link in another app.`;
        detail.textContent = n.url.slice(0, 200);
        primary.textContent = 'Open';
        primary.onclick = () => invoke('confirm_external', { tab: tab.id });
        secondary.textContent = 'Cancel';
        secondary.onclick = () => invoke('dismiss_notice', { tab: tab.id });
        break;
      case 'crashed':
        title.textContent = 'This page keeps crashing';
        body.textContent = 'The page was reloaded several times after running out of memory or crashing.';
        primary.textContent = 'Reload';
        primary.onclick = () => { invoke('dismiss_notice', { tab: tab.id }); invoke('reload', { tab: tab.id }); };
        break;
      default:
        title.textContent = 'Something went wrong';
        body.textContent = '';
        primary.hidden = true;
    }
  }

  function renderStart(tab) {
    $('start-title').textContent = tab && tab.private ? 'New Private Tab' : 'New Tab';
    $('start-private').hidden = !(tab && tab.private);
  }

  // History, newest first, under a heading per day and per hour.
  function fillHistory(list, items) {
    list.replaceChildren();
    if (!items.length) {
      list.appendChild(el('li', 'muted', 'Nothing here yet.'));
      return;
    }
    const dayName = (d) => {
      const today = new Date();
      const start = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
      const diff = Math.round((start(today) - start(d)) / 86400000);
      if (diff === 0) return 'Today';
      if (diff === 1) return 'Yesterday';
      return d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long', year: diff > 300 ? 'numeric' : undefined });
    };
    const hourName = (d) => d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }).replace(/:\d\d/, ':00');
    let lastDay = '';
    let lastHour = '';
    for (const it of items) {
      const when = new Date(it.visited_ms || 0);
      const day = dayName(when);
      const hour = day + hourName(when);
      if (day !== lastDay) { list.appendChild(el('li', 'day', day)); lastDay = day; lastHour = ''; }
      if (hour !== lastHour) { list.appendChild(el('li', 'hour', hourName(when))); lastHour = hour; }
      const li = el('li');
      const open = el('button', 'open');
      open.appendChild(el('span', 't', it.title || it.display.registrable));
      const u = el('span', 'u');
      const sub = el('span', 'sub');
      const reg = el('span', 'reg');
      renderUrlInto(sub, reg, null, it.display);
      u.append(sub, reg);
      open.appendChild(u);
      open.addEventListener('click', () => {
        const tab = activeTab();
        closePanel();
        if (tab) invoke('navigate', { tab: tab.id, input: it.url });
      });
      li.appendChild(open);
      li.appendChild(el('span', 'when', when.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })));
      list.appendChild(li);
    }
  }

  // ---- opening links from pins / bookmarks / shortcuts -----------------------

  // Each pin and bookmark has its own tab, which is not listed with the
  // other tabs: opening one returns to it (created on first use), and the
  // pin or bookmark is shown as selected.
  function openLink(url) {
    invoke('open_launcher', { url });
    if (narrow) setSidebar(false);
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
    if (!pins.length) {
      grid.appendChild(el('div', 'hint', 'Drag up to 4 tabs or bookmarks here.'));
      return;
    }
    pins.forEach((p, index) => {
      const node = el('div', 'pin');
      node.dataset.index = String(index);
      node.dataset.url = p.url;
      node.title = p.title || p.display.registrable;
      const open = el('button', 'open');
      open.setAttribute('aria-label', `Open ${p.title || p.display.registrable}`);
      const prefab = prefabIcon(p.url);
      if (prefab) {
        const img = el('img', 'prefab');
        img.alt = '';
        img.src = prefab;
        open.appendChild(img);
      } else if (p.icon && p.icon.startsWith('data:image/png;base64,')) {
        const img = el('img');
        img.alt = '';
        // Never show a broken image: fall back to the letter tile.
        img.addEventListener('error', () => {
          chromeLog('warn', `favicon for ${p.display.registrable} could not be displayed (${p.icon.length} chars)`);
          img.replaceWith(letterTile(p.display));
        });
        img.src = p.icon;
        open.appendChild(img);
      } else {
        open.appendChild(letterTile(p.display));
      }
      open.addEventListener('click', () => { if (!suppressClick) openLink(p.url); });
      const rm = el('button', 'unpin', '\u00D7');
      rm.setAttribute('aria-label', 'Unpin');
      rm.addEventListener('click', (ev) => {
        ev.stopPropagation();
        invoke('unpin', { url: p.url, bookmarkIndex: null }).then(loadPins);
      });
      node.append(open, rm);
      node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'pin', url: p.url, index }));
      grid.appendChild(node);
    });
    if (state) renderLauncherSelection();
  }

  // ---- bookmarks bar ------------------------------------------------------

  function loadBookmarks() {
    return T.core.invoke('list_bookmarks').then((items) => { bookmarks = items; renderBookmarks(); }).catch(() => {});
  }

  function renderBookmarks() {
    const list = $('bookmarks');
    list.replaceChildren();
    if (!bookmarks.length) {
      list.appendChild(el('div', 'hint', 'Drag a tab here to bookmark it.'));
      return;
    }
    bookmarks.forEach((b, index) => {
      const node = el('div', 'bm');
      node.dataset.index = String(index);
      node.dataset.url = b.url;
      const open = el('button', 'open');
      open.appendChild(el('span', 't', b.title || b.display.registrable));
      const host = el('span', 'h');
      fillLabels(host, b.display.registrable, false);
      open.appendChild(host);
      open.addEventListener('click', () => { if (!suppressClick) openLink(b.url); });
      const rm = el('button', 'close', '×');
      rm.setAttribute('aria-label', 'Remove bookmark');
      rm.addEventListener('click', (ev) => {
        ev.stopPropagation();
        invoke('remove_bookmark', { url: b.url }).then(loadBookmarks);
      });
      node.append(open, rm);
      node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'bookmark', url: b.url, index }));
      list.appendChild(node);
    });
    if (state) renderLauncherSelection();
  }

  // ---- drag and drop ------------------------------------------------------
  //
  // Long-press (or mouse press) then move. Tab → bookmarks bar: bookmark it
  // at that position. Tab → another tab: split view. A tab of a split pair
  // → anywhere off its pair: unsplit. Bookmark → bookmarks bar: reorder. A
  // move before the long-press fires is a scroll.

  const DRAG_DELAY_MS = 280;
  const MOVE_TOLERANCE_PX = 8;
  let pendingDrag = null;
  let dragState = null;

  function armDrag(ev, node, item) {
    if (ev.button > 0 || ev.target.closest('.close, .unpin')) return;
    cancelPending();
    const delay = ev.pointerType === 'mouse' ? 120 : DRAG_DELAY_MS;
    pendingDrag = { node, item, x: ev.clientX, y: ev.clientY, timer: setTimeout(startDrag, delay) };
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
    dragState = { item: p.item, node: p.node, ghost, dx: p.x - rect.left, dy: p.y - rect.top, target: null };
    showDropZone(p.item);
  }

  // While dragging something that can be bookmarked, the bookmarks list
  // grows a visible target at its end (once it has items, the empty-list
  // hint is gone and there is otherwise little to aim at).
  function showDropZone(item) {
    const list = $('bookmarks');
    if (item.kind === 'tab' && item.private) return; // never persist private tabs
    if (!list.querySelector('.bm')) return; // the empty-list hint is the target
    list.appendChild(el('div', 'drop-zone', item.kind === 'bookmark' ? 'Move to the end' : 'Drop here to bookmark'));
  }

  function dropTargetAt(x, y) {
    const hit = document.elementFromPoint(x, y);
    if (!hit) return null;
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
      const items = [...list.querySelectorAll('.bm')];
      let index = items.length;
      for (let i = 0; i < items.length; i++) {
        const r = items[i].getBoundingClientRect();
        if (y < r.top + r.height / 2) { index = i; break; }
      }
      return { kind: 'bookmarks', index, list, items };
    }
    if (dragState.item.kind !== 'tab') return null;
    const item = dragState.item;
    const group = item.paired ? dragState.node.closest('.tab-group') : null;
    if (group && group.contains(hit)) return null; // still inside its own pair
    const node = hit.closest('.tab');
    if (node) {
      const id = Number(node.dataset.tab);
      const other = state.tabs.find((x) => x.id === id);
      // Tabs of different profiles are never paired.
      if (id !== item.id && other && other.private === item.private) return { kind: 'tab', id, node };
    }
    return group ? { kind: 'unsplit', group } : null;
  }

  function showDropFeedback(target) {
    document.querySelectorAll('.drop-target').forEach((n) => n.classList.remove('drop-target'));
    document.querySelectorAll('.tab-group.leaving').forEach((n) => n.classList.remove('leaving'));
    const line = $('drop-line');
    line.hidden = true;
    if (!target) return;
    if (target.kind === 'tab') {
      target.node.classList.add('drop-target');
    } else if (target.kind === 'unsplit') {
      target.group.classList.add('leaving');
    } else if (target.kind === 'pins') {
      target.grid.classList.add('drop-target');
    } else {
      target.list.classList.add('drop-target');
      const zone = target.list.querySelector('.drop-zone');
      if (zone) zone.classList.toggle('hot', target.index === target.items.length);
      const lr = target.list.getBoundingClientRect();
      let y;
      if (!target.items.length) y = lr.top + lr.height / 2;
      else if (target.index < target.items.length) y = target.items[target.index].getBoundingClientRect().top - 1;
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
    suppressClick = true;
    setTimeout(() => { suppressClick = false; }, 0);
    const t = d.target;
    if (!commit || !t) return;
    if (d.item.kind === 'tab' && t.kind === 'tab') {
      invoke('split_tabs', { tab: d.item.id, with: t.id });
    } else if (t.kind === 'unsplit') {
      invoke('unsplit', { tab: d.item.id });
    } else if (d.item.kind === 'tab' && t.kind === 'bookmarks') {
      T.core.invoke('bookmark_tab', { tab: d.item.id, index: t.index })
        .then((added) => { if (!added) toast('Already bookmarked, or nothing to bookmark yet.'); return loadBookmarks(); })
        .catch((e) => toast(String(e)));
    } else if (d.item.kind === 'bookmark' && t.kind === 'bookmarks') {
      const to = t.index > d.item.index ? t.index - 1 : t.index;
      if (to !== d.item.index) invoke('move_bookmark', { url: d.item.url, index: to }).then(loadBookmarks);
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

  document.addEventListener('pointermove', (ev) => {
    if (pendingDrag && Math.hypot(ev.clientX - pendingDrag.x, ev.clientY - pendingDrag.y) > MOVE_TOLERANCE_PX) {
      cancelPending(); // the user is scrolling
    }
    if (!dragState) return;
    dragState.ghost.style.left = ev.clientX - dragState.dx + 'px';
    dragState.ghost.style.top = ev.clientY - dragState.dy + 'px';
    dragState.target = dropTargetAt(ev.clientX, ev.clientY);
    showDropFeedback(dragState.target);
  });
  document.addEventListener('pointerup', () => { cancelPending(); if (dragState) endDrag(true); });
  document.addEventListener('pointercancel', () => { cancelPending(); if (dragState) endDrag(false); });
  // Once a drag has started, stop the sidebar from scrolling under it.
  document.addEventListener('touchmove', (ev) => { if (dragState) ev.preventDefault(); }, { passive: false });

  // ---- sidebar ----------------------------------------------------------

  // Wide windows: the sidebar is docked and content starts to its right.
  // Narrow windows (Split View / Slide Over): it slides over the content
  // area, which is hidden while it is open (overlay), and closes after a
  // tab is chosen.
  const NARROW_PX = 700;
  const MIN_SIDEBAR = 200;
  const MAX_SIDEBAR = 420;
  let narrow = window.innerWidth < NARROW_PX;
  let sidebarOpen = loadSidebarPref();

  function pref(key) {
    try { return localStorage.getItem(key); } catch (e) { return null; }
  }
  function setPref(key, value) {
    try { localStorage.setItem(key, value); } catch (e) { /* storage unavailable */ }
  }

  function loadSidebarPref() {
    const v = pref('sidebarOpen');
    if (v !== null) return v === '1' && !narrow;
    return !narrow;
  }

  function clampWidth(w) {
    return Math.round(Math.max(MIN_SIDEBAR, Math.min(MAX_SIDEBAR, w, window.innerWidth * 0.5)));
  }

  function setSidebarWidth(w) {
    document.documentElement.style.setProperty('--sidebar-w', clampWidth(w) + 'px');
  }

  function setSidebar(open) {
    sidebarOpen = open;
    if (!narrow) setPref('sidebarOpen', open ? '1' : '0');
    closeMenu();
    applySidebar();
  }

  function applySidebar() {
    document.body.classList.toggle('narrow', narrow);
    document.body.classList.toggle('sidebar-closed', !sidebarOpen);
    $('sidebar').setAttribute('aria-hidden', String(!sidebarOpen));
    $('sidebar-toggle').setAttribute('aria-expanded', String(sidebarOpen));
    reportLayout();
  }

  // Resizing: the content view follows the edge, one layout report per
  // animation frame.
  let resize = null;
  let layoutQueued = false;
  function queueLayout() {
    if (layoutQueued) return;
    layoutQueued = true;
    requestAnimationFrame(() => { layoutQueued = false; reportLayout(); });
  }
  $('sidebar-resize').addEventListener('pointerdown', (ev) => {
    ev.preventDefault();
    const handle = $('sidebar-resize');
    handle.setPointerCapture(ev.pointerId);
    const w = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w')) || 264;
    resize = { x: ev.clientX, w };
    document.body.classList.add('resizing');
  });
  $('sidebar-resize').addEventListener('pointermove', (ev) => {
    if (!resize) return;
    setSidebarWidth(resize.w + ev.clientX - resize.x);
    queueLayout();
  });
  const endResize = () => {
    if (!resize) return;
    resize = null;
    document.body.classList.remove('resizing');
    setPref('sidebarWidth', getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w').trim());
    reportLayout();
  };
  $('sidebar-resize').addEventListener('pointerup', endResize);
  $('sidebar-resize').addEventListener('pointercancel', endResize);

  // ---- menu, settings, history ----------------------------------------------

  function openMenu() { menuOpen = true; $('menu-pop').hidden = false; }
  function closeMenu() { menuOpen = false; $('menu-pop').hidden = true; }

  function openPanel() {
    closeMenu();
    panel = 'history';
    clearArmed = false;
    $('panel-clear').textContent = 'Clear history';
    T.core.invoke('list_history', { limit: 500 }).then((items) => fillHistory($('panel-list'), items));
    if (narrow) setSidebar(false);
    render();
  }

  function closePanel() {
    panel = null;
    render();
  }

  function setModal(open) {
    closeMenu();
    modalOpen = open;
    $('modal').hidden = !open;
    reportLayout();
  }

  // ---- site scripts -------------------------------------------------------

  function loadScripts() {
    return T.core.invoke('list_user_scripts').then((items) => { scripts = items; renderScripts(); }).catch((e) => toast(String(e)));
  }

  function renderScripts() {
    const list = $('scripts-list');
    list.replaceChildren();
    if (!scripts.length) {
      list.appendChild(el('li', 'muted', 'No site scripts yet.'));
    }
    for (const s of scripts) {
      const li = el('li');
      const enabled = el('input');
      enabled.type = 'checkbox';
      enabled.checked = s.enabled;
      enabled.setAttribute('aria-label', `Enable scripts for ${s.domain}`);
      enabled.addEventListener('change', () => saveScript({ ...s, enabled: enabled.checked }));
      const open = el('button', 'open');
      open.appendChild(el('span', 't', s.domain));
      const parts = [s.css.trim() ? 'CSS' : '', s.js.trim() ? 'JS' : ''].filter(Boolean).join(' + ') || 'empty';
      open.appendChild(el('span', 'u', parts + (s.enabled ? '' : ' (off)')));
      open.addEventListener('click', () => openEditor(s));
      const rm = el('button', 'remove', '×');
      rm.setAttribute('aria-label', `Delete scripts for ${s.domain}`);
      rm.addEventListener('click', () => deleteScript(s.domain));
      li.append(enabled, open, rm);
      list.appendChild(li);
    }
  }

  function openEditor(s) {
    editingDomain = s ? s.domain : '';
    $('script-editor').hidden = false;
    $('se-domain').value = s ? s.domain : currentSite();
    $('se-enabled').checked = s ? s.enabled : true;
    $('se-css').value = s ? s.css : '';
    $('se-js').value = s ? s.js : '';
    $('se-delete').hidden = !s;
  }

  function currentSite() {
    const t = activeTab();
    return (t && t.adblock && t.adblock.site) || '';
  }

  function saveScript(s) {
    return T.core.invoke('save_user_script', s)
      .then((saved) => {
        // Renaming the domain in the editor replaces the old entry.
        const renamed = editingDomain && editingDomain !== saved.domain ? T.core.invoke('delete_user_script', { domain: editingDomain }) : null;
        editingDomain = saved.domain;
        $('se-domain').value = saved.domain;
        $('se-delete').hidden = false;
        return Promise.resolve(renamed).then(loadScripts).then(() => reloadIfMatching(saved.domain));
      })
      .catch((e) => toast(String(e)));
  }

  function deleteScript(domain) {
    invoke('delete_user_script', { domain }).then(() => {
      if (editingDomain === domain) $('script-editor').hidden = true;
      return loadScripts();
    }).then(() => reloadIfMatching(domain));
  }

  function reloadIfMatching(domain) {
    const t = activeTab();
    const site = t && t.adblock && t.adblock.site;
    if (site && (site === domain || site.endsWith('.' + domain) || domain.endsWith('.' + site))) {
      invoke('reload', { tab: t.id });
      toast(`Saved. Reloading ${site}…`);
    } else {
      toast('Saved. Applies the next time a matching page loads.');
    }
  }

  function setScripts(open) {
    scriptsOpen = open;
    $('scripts-pop').hidden = !open;
    if (open) {
      $('script-editor').hidden = true;
      loadScripts();
    }
    reportLayout();
  }

  // ---- address bar ------------------------------------------------------

  function beginEdit() {
    const tab = activeTab();
    editing = true;
    const input = $('url-input');
    $('url-display').hidden = true;
    input.hidden = false;
    input.value = tab && tab.url ? tab.url.full : '';
    input.focus();
    input.select();
  }

  function endEdit() {
    editing = false;
    $('url-input').hidden = true;
    $('url-display').hidden = false;
  }

  // ---- layout -----------------------------------------------------------

  // Tells Rust where the content area starts and whether chrome UI is
  // covering it (content views are hidden while `overlay` is true).
  let lastLayout = '';
  function reportLayout() {
    // Use the sidebar's layout width (unaffected by the slide transform) so
    // the content area moves once, to its final position.
    const left = sidebarOpen && !narrow ? Math.ceil($('sidebar').offsetWidth) : 0;
    $('chrome').style.left = left + 'px';
    const top = Math.ceil($('chrome').getBoundingClientRect().bottom);
    const overlay = !!panel || modalOpen || scriptsOpen || (narrow && sidebarOpen);
    // The docked console takes the bottom of the content area.
    const con = $('console');
    con.style.left = left + 'px';
    const bottom = consoleOpen ? Math.ceil(con.getBoundingClientRect().height) : 0;
    // The chrome surface (start page, notices) covers the focused pane: in
    // split view that is one half of the content area (matching the native
    // pane frames: 2 pt divider, halves floored).
    let sLeft = left;
    let sRight = 0;
    if (state && state.split && !overlay) {
      const w = window.innerWidth - left;
      const half = Math.floor((w - 2) / 2);
      if (state.active === state.split.left) sRight = w - half;
      else sLeft = left + w - half;
    }
    const s = $('surface');
    s.style.top = top + 'px';
    s.style.left = sLeft + 'px';
    s.style.right = sRight + 'px';
    s.style.bottom = bottom + 'px';
    const key = top + ':' + left + ':' + bottom + ':' + overlay;
    if (key === lastLayout) return;
    lastLayout = key;
    invoke('set_chrome_layout', { top, left, bottom, overlay });
  }

  // ---- ad blocking ------------------------------------------------------

  let lastAdblockFailure = '';
  function renderShield(tab) {
    const b = $('shield');
    const a = tab && tab.adblock;
    const status = state.adblock;
    b.className = 'pillbtn';
    if (status.state === 'failed') {
      b.textContent = 'Blocker failed';
      b.classList.add('off');
      b.disabled = true;
      if (status.message !== lastAdblockFailure) {
        lastAdblockFailure = status.message;
        toast('Ad blocker could not start: ' + status.message);
      }
      return;
    }
    if (status.state === 'compiling') {
      b.textContent = 'Blocker…';
      b.disabled = true;
      return;
    }
    b.hidden = !a;
    if (!a) return;
    b.textContent = a.blocking ? 'Ads: Blocked' : 'Ads: Allowed';
    b.classList.add(a.blocking ? 'on' : 'off');
    b.disabled = !a.can_toggle;
    b.title = a.can_toggle
      ? (a.blocking ? `Allow ads on ${a.site}` : `Block ads on ${a.site} again`)
      : 'Per-site settings can’t be changed in a private tab';
  }

  function renderViewMode() {
    const c = $('color');
    c.classList.toggle('on', viewMode.color);
    c.setAttribute('aria-pressed', String(viewMode.color));
    // Images and video are hidden by default; the button shows them.
    const i = $('images');
    i.classList.toggle('on', !viewMode.hide_images);
    i.setAttribute('aria-pressed', String(!viewMode.hide_images));
  }

  function setViewMode(change) {
    const next = { ...viewMode, ...change };
    T.core.invoke('set_view_mode', { color: next.color, hideImages: next.hide_images })
      .then((m) => { viewMode = m; renderViewMode(); })
      .catch((e) => toast(String(e)));
  }

  // ---- console -----------------------------------------------------------
  //
  // Entries come from Rust (browser events, and page console output while
  // recording) plus the chrome's own errors and notes. All text, page
  // messages included, is set with textContent only.

  const MAX_CONSOLE = 1000;
  let consoleEntries = [];
  let consoleCapture = false;
  let consoleFilter = 'all';
  let chromeSeq = 0;
  let consoleQueued = false;

  function addConsole(entry) {
    consoleEntries.push(entry);
    if (consoleEntries.length > MAX_CONSOLE) consoleEntries.splice(0, consoleEntries.length - MAX_CONSOLE);
    if (consoleOpen && !consoleQueued) {
      consoleQueued = true;
      requestAnimationFrame(() => { consoleQueued = false; renderConsole(); });
    }
  }

  // The chrome's own messages (not from Rust or pages).
  function chromeLog(level, text) {
    addConsole({ seq: 'c' + chromeSeq++, time_ms: Math.round(performance.now()), origin: 'chrome', level, tab: null, host: '', main_frame: true, text: String(text) });
  }

  function loadConsole() {
    return T.core.invoke('get_console').then((c) => {
      consoleCapture = c.capture;
      const mine = consoleEntries.filter((e) => e.origin === 'chrome');
      consoleEntries = c.entries.concat(mine).sort((a, b) => a.time_ms - b.time_ms);
      renderConsole();
    }).catch(() => {});
  }

  function visibleEntries() {
    const tabOnly = $('console-tab-only').checked;
    const t = activeTab();
    return consoleEntries.filter((e) => {
      if (consoleFilter === 'page' && e.origin !== 'page') return false;
      if (consoleFilter === 'app' && e.origin === 'page') return false;
      if (consoleFilter === 'errors' && e.level !== 'error' && e.level !== 'warn') return false;
      if (tabOnly && t && e.tab !== t.id) return false;
      return true;
    });
  }

  function sourceLabel(e) {
    if (e.origin === 'page') return (e.host || '(no origin)') + (e.main_frame ? '' : ' (frame)');
    return e.origin === 'chrome' ? 'chrome' : 'browser';
  }

  function stamp(ms) {
    const s = ms / 1000;
    return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, '0')}`;
  }

  function renderConsole() {
    const list = $('console-list');
    const atEnd = list.scrollTop + list.clientHeight >= list.scrollHeight - 24;
    list.replaceChildren();
    for (const e of visibleEntries()) {
      const li = el('li', e.level);
      li.append(el('span', 'when', stamp(e.time_ms)), el('span', 'src', sourceLabel(e)), el('span', 'msg', e.text));
      list.appendChild(li);
    }
    if (!list.childElementCount) list.appendChild(el('li', 'debug', consoleCapture ? 'Nothing logged yet.' : 'Recording is off. Tap Record to log browser events and page console output.'));
    if (atEnd) list.scrollTop = list.scrollHeight;
    const rec = $('console-capture');
    rec.setAttribute('aria-pressed', String(consoleCapture));
    rec.textContent = consoleCapture ? '● Recording' : 'Record';
    document.querySelectorAll('#console .seg button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.filter === consoleFilter)));
  }

  function setConsole(open) {
    closeMenu();
    consoleOpen = open;
    $('console').hidden = !open;
    if (open) loadConsole();
    reportLayout();
  }

  function setCapture(on) {
    T.core.invoke('set_console_capture', { on }).then(() => {
      consoleCapture = on;
      const note = $('console-note');
      note.hidden = !on;
      note.textContent = 'Page console output is recorded from the next page load. Reload a page to capture it from the start.';
      renderConsole();
    }).catch((e) => toast(String(e)));
  }

  function copyConsole() {
    const text = visibleEntries().map((e) => `${stamp(e.time_ms)} [${e.level}] ${sourceLabel(e)}: ${e.text}`).join('\n');
    navigator.clipboard.writeText(text).then(() => toast('Console copied.'), () => toast('Could not copy (clipboard unavailable).'));
  }

  window.addEventListener('error', (e) => chromeLog('error', `Uncaught ${e.message} (${e.filename}:${e.lineno})`));
  window.addEventListener('unhandledrejection', (e) => chromeLog('error', 'Unhandled rejection: ' + String(e.reason)));
  document.addEventListener('securitypolicyviolation', (e) =>
    chromeLog('error', `CSP blocked ${e.blockedURI.slice(0, 40)} (${e.violatedDirective})`));

  function toast(msg) {
    chromeLog('info', 'toast: ' + msg);
    const t = $('toast');
    t.textContent = msg;
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.hidden = true; reportLayout(); }, 4000);
    reportLayout();
  }

  // ---- keyboard shortcuts -------------------------------------------------
  //
  // ⌘T new tab, ⌘S toggle sidebar, ⌘1–⌘5 the first five sidebar items
  // (pins, then bookmarks, then tabs). Handled here when the chrome has
  // keyboard focus; when a page has focus, native UIKeyCommands forward the
  // same names via the `shortcut` event.

  function sidebarItems() {
    const items = [];
    for (const p of pins) items.push({ link: p });
    for (const b of bookmarks) items.push({ link: b });
    if (state) {
      const listed = (t) => t.sheet_over === null && t.launcher === null;
      for (const t of state.tabs) if (!t.private && listed(t)) items.push({ tab: t });
      for (const t of state.tabs) if (t.private && listed(t)) items.push({ tab: t });
    }
    return items;
  }

  function runShortcut(name) {
    if (name === 'new_tab') {
      if (narrow) setSidebar(false);
      invoke('new_tab', { private: false }).then(beginEdit);
    } else if (name === 'toggle_sidebar') {
      setSidebar(!sidebarOpen);
    } else if (name.startsWith('item:')) {
      const item = sidebarItems()[Number(name.slice(5)) - 1];
      if (!item) return;
      if (item.tab) invoke('activate_tab', { tab: item.tab.id });
      else openLink(item.link.url);
    }
  }

  document.addEventListener('keydown', (ev) => {
    if (!ev.metaKey || ev.altKey || ev.ctrlKey) return;
    const k = ev.key.toLowerCase();
    let name = null;
    if (k === 't') name = 'new_tab';
    else if (k === 's') name = 'toggle_sidebar';
    else if (k >= '1' && k <= '5' && k.length === 1) name = 'item:' + k;
    if (!name) return;
    ev.preventDefault();
    runShortcut(name);
  });

  // ---- wiring -----------------------------------------------------------

  function withTab(f) {
    return () => { const t = activeTab(); if (t) f(t); };
  }

  $('back').addEventListener('click', withTab((t) => invoke('go_back', { tab: t.id })));
  $('forward').addEventListener('click', withTab((t) => invoke('go_forward', { tab: t.id })));
  $('reload').addEventListener('click', withTab((t) => invoke(t.loading ? 'stop' : 'reload', { tab: t.id })));
  $('menu-new-tab').addEventListener('click', () => {
    closeMenu();
    if (narrow) setSidebar(false);
    invoke('new_tab', { private: false }).then(beginEdit);
  });
  $('menu-new-private').addEventListener('click', () => {
    closeMenu();
    if (narrow) setSidebar(false);
    invoke('new_tab', { private: true }).then(beginEdit);
  });
  $('color').addEventListener('click', () => setViewMode({ color: !viewMode.color }));
  $('images').addEventListener('click', () => setViewMode({ hide_images: !viewMode.hide_images }));
  $('sidebar-toggle').addEventListener('click', () => setSidebar(!sidebarOpen));
  $('unsplit').addEventListener('click', withTab((t) => invoke('unsplit', { tab: t.id })));
  $('sheet-close').addEventListener('click', () => { if (state && state.sheet !== null) invoke('close_tab', { tab: state.sheet }); });
  $('menu').addEventListener('click', () => (menuOpen ? closeMenu() : openMenu()));
  $('menu-history').addEventListener('click', openPanel);
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
  $('scripts-btn').addEventListener('click', () => setScripts(!scriptsOpen));
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
    if (narrow && sidebarOpen && !$('sidebar').contains(ev.target) && ev.target !== $('sidebar-toggle')) {
      setSidebar(false);
    }
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape' || editing) return;
    if (menuOpen) closeMenu();
    else if (modalOpen) setModal(false);
    else if (scriptsOpen) setScripts(false);
    else if (panel) closePanel();
  });
  $('shield').addEventListener('click', withTab((t) => {
    const a = t.adblock;
    if (!a || !a.can_toggle) return;
    T.core.invoke('toggle_site_adblock', { tab: t.id })
      .then((blocking) => toast(blocking ? `Blocking ads on ${a.site} again. Reloading…` : `Ads allowed on ${a.site}. Reloading…`))
      .catch((e) => toast(String(e)));
  }));
  $('bookmark').addEventListener('click', withTab((t) =>
    T.core.invoke('toggle_bookmark', { tab: t.id })
      .then((on) => { toast(on ? 'Bookmarked' : 'Bookmark removed'); return loadBookmarks(); })
      .catch((e) => toast(String(e)))));
  $('panel-close').addEventListener('click', closePanel);
  $('panel-clear').addEventListener('click', () => {
    if (!clearArmed) { clearArmed = true; $('panel-clear').textContent = 'Tap again to clear'; return; }
    invoke('clear_history').then(openPanel);
  });

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
      sidebarOpen = nowNarrow ? false : loadSidebarPref();
    }
    setSidebarWidth(parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w')) || 264);
    applySidebar();
  });
  const savedWidth = parseFloat(pref('sidebarWidth') || '');
  if (savedWidth) setSidebarWidth(savedWidth);
  applySidebar();
  renderBookmarks();
  renderPins();

  T.event.listen('state', (e) => { state = e.payload; render(); });
  T.event.listen('toast', (e) => toast(String(e.payload)));
  T.event.listen('shortcut', (e) => runShortcut(String(e.payload)));
  T.event.listen('pins', () => loadPins());
  T.event.listen('console', (e) => addConsole(e.payload));
  T.event.listen('console_reset', () => loadConsole());
  T.core.invoke('get_state').then((s) => { state = s; render(); });
  loadBookmarks();
  loadPins();
  T.core.invoke('get_view_mode').then((m) => { viewMode = m; renderViewMode(); }).catch(() => {});
})();
