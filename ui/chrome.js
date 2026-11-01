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
  let paletteOpen = false;
  let consoleOpen = false;
  let panel = null; // null | 'history'
  let modalOpen = false;
  let scriptsOpen = false;
  let menuOpen = false;
  let bookmarks = [];
  let pins = [];
  const MAX_PINS = 4;
  // View modes (session-only, kept in Rust): app-wide on/off, and tabs
  // exempted from a mode ("Disable for this tab").
  let viewModes = { modes: { desaturate: true, hide_media: true, headers: false }, exempt: {} };
  let headingsByTab = new Map();
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
    renderViewMode();
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

  // Sites whose tab group is folded (session only).
  const collapsedSites = new Set();

  function siteOf(t) {
    return t.url ? t.url.registrable : '';
  }

  // Tabs that can join a same-site group: listed, and not in a split pair.
  function groupable(t) {
    return t.sheet_over === null && t.launcher === null && !pairOf(t.id);
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
        // Same-site tabs (not paired, not bound to a pin or bookmark) are
        // collected under the first one.
        const site = siteOf(t);
        const members = site ? state.tabs.filter((o) => groupable(o) && o.private === t.private && siteOf(o) === site) : [t];
        if (members.length < 2) {
          list.appendChild(tabNode(t, null));
          placed.add(t.id);
          continue;
        }
        const key = (t.private ? 'p:' : 'n:') + site;
        if (members.some((m) => m.id === shownTab())) collapsedSites.delete(key);
        const collapsed = collapsedSites.has(key);
        const box = el('div', 'site-group' + (collapsed ? ' collapsed' : ''));
        box.setAttribute('role', 'group');
        box.setAttribute('aria-label', `${members.length} tabs on ${site}`);
        members.forEach((m, i) => {
          const node = tabNode(m, null);
          if (i === 0) {
            const toggle = el('button', 'site-toggle', String(members.length));
            toggle.setAttribute('aria-expanded', String(!collapsed));
            toggle.setAttribute('aria-label', `${collapsed ? 'Show' : 'Hide'} ${members.length - 1} more ${site} tabs`);
            toggle.addEventListener('click', (ev) => {
              ev.stopPropagation();
              if (collapsedSites.has(key)) collapsedSites.delete(key);
              else collapsedSites.add(key);
              tabsKey = '';
              renderTabs();
            });
            node.insertBefore(toggle, node.firstChild);
          } else {
            node.classList.add('child');
          }
          box.appendChild(node);
          placed.add(m.id);
        });
        list.appendChild(box);
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
    renderScriptsMark(tab);
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

  function dragMoveTo(x, y) {
    if (pendingDrag && Math.hypot(x - pendingDrag.x, y - pendingDrag.y) > MOVE_TOLERANCE_PX) {
      cancelPending(); // the user is scrolling
    }
    if (!dragState) return;
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

  function openMenu() { menuOpen = true; $('menu-pop').hidden = false; document.body.classList.add('menu-open'); }
  function closeMenu() { menuOpen = false; $('menu-pop').hidden = true; syncMenuOpen(); }
  // Keeps the chrome fully opaque while any of its menus is open.
  function syncMenuOpen() {
    const open = menuOpen || !$('vm-pop').hidden || !!document.querySelector('.drawer-open');
    document.body.classList.toggle('menu-open', open);
  }

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

  function setModal(open, section) {
    closeMenu();
    modalOpen = open;
    $('modal').hidden = !open;
    if (open) showSettings(section || 'general');
    reportLayout();
  }

  const SETTINGS_TITLES = { general: 'General', ai: 'AI', cache: 'Cache' };
  function showSettings(section) {
    document.querySelectorAll('.settings-nav button').forEach((b) => b.setAttribute('aria-current', b.dataset.section === section ? 'page' : 'false'));
    document.querySelectorAll('.settings-body [data-panel]').forEach((p) => { p.hidden = p.dataset.panel !== section; });
    $('settings-heading').textContent = SETTINGS_TITLES[section];
    if (section === 'ai') loadAi();
  }
  document.querySelectorAll('.settings-nav button').forEach((b) => b.addEventListener('click', () => showSettings(b.dataset.section)));

  // ---- AI connections -----------------------------------------------------
  //
  // Keys go to the Keychain through Rust and never come back; the chrome
  // only learns whether one is stored. The chosen model per provider and the
  // provider used for site scripts are UI preferences (localStorage).

  const AI_DEFAULT_MODEL = { anthropic: 'claude-opus-5-5', openai: '' };
  const AI_LABEL = { anthropic: 'Anthropic', openai: 'OpenAI' };
  let aiConfigured = { anthropic: false, openai: false };

  function aiModels(p) {
    try { return JSON.parse(pref('aiModels.' + p) || '[]'); } catch (e) { return []; }
  }
  function aiModel(p) { return pref('aiModel.' + p) || AI_DEFAULT_MODEL[p]; }
  function aiProvider() {
    const chosen = pref('aiProvider');
    if (chosen && aiConfigured[chosen]) return chosen;
    return ['anthropic', 'openai'].find((p) => aiConfigured[p]) || null;
  }

  function loadAi() {
    return T.core.invoke('ai_status').then((st) => { aiConfigured = st.configured; renderAi(); }).catch(() => {});
  }

  function renderAi() {
    document.querySelectorAll('.ai-provider').forEach((box) => {
      const p = box.dataset.provider;
      const on = !!aiConfigured[p];
      box.querySelector('.ai-state').textContent = on ? 'Key saved' : 'Not connected';
      box.querySelector('.ai-key').placeholder = on ? 'Replace the saved key' : `${AI_LABEL[p]} API key`;
      box.querySelector('.ai-test').disabled = !on;
      box.querySelector('.ai-remove').disabled = !on;
      const sel = box.querySelector('.ai-model');
      const models = aiModels(p);
      const current = aiModel(p);
      sel.replaceChildren();
      if (!models.length) {
        const o = el('option', null, current ? current : 'Test the key to list models');
        o.value = current || '';
        sel.appendChild(o);
      }
      for (const m of models) {
        const o = el('option', null, m.name === m.id ? m.id : `${m.name} (${m.id})`);
        o.value = m.id;
        sel.appendChild(o);
      }
      sel.value = current;
      sel.disabled = !on;
    });
    const use = $('ai-use');
    use.replaceChildren();
    const ps = ['anthropic', 'openai'].filter((p) => aiConfigured[p]);
    if (!ps.length) use.appendChild(el('option', null, 'Connect a provider first'));
    for (const p of ps) { const o = el('option', null, AI_LABEL[p]); o.value = p; use.appendChild(o); }
    use.disabled = !ps.length;
    if (ps.length) use.value = aiProvider();
    $('se-ai').hidden = !aiProvider();
  }

  document.querySelectorAll('.ai-provider').forEach((box) => {
    const p = box.dataset.provider;
    const msg = (t) => { box.querySelector('.ai-msg').textContent = t; };
    box.querySelector('.ai-save').addEventListener('click', () => {
      const input = box.querySelector('.ai-key');
      T.core.invoke('ai_set_key', { provider: p, key: input.value })
        .then((st) => { input.value = ''; aiConfigured = st.configured; renderAi(); msg('Saved. Testing…'); return test(); })
        .catch((e) => msg(String(e)));
    });
    const test = () => {
      msg('Testing…');
      return T.core.invoke('ai_test', { provider: p })
        .then((models) => {
          setPref('aiModels.' + p, JSON.stringify(models));
          if (models.length && !models.some((m) => m.id === aiModel(p))) setPref('aiModel.' + p, models[0].id);
          renderAi();
          msg(`Connected. ${models.length} model${models.length === 1 ? '' : 's'} available.`);
        })
        .catch((e) => msg(String(e)));
    };
    box.querySelector('.ai-test').addEventListener('click', test);
    box.querySelector('.ai-remove').addEventListener('click', () => {
      T.core.invoke('ai_delete_key', { provider: p })
        .then((st) => { aiConfigured = st.configured; renderAi(); msg('Key removed from this device.'); })
        .catch((e) => msg(String(e)));
    });
    box.querySelector('.ai-model').addEventListener('change', (ev) => setPref('aiModel.' + p, ev.target.value));
  });
  $('ai-use').addEventListener('change', (ev) => { setPref('aiProvider', ev.target.value); renderAi(); });

  // Site script helper: drafts CSS/JS into the editor for the user to review.
  $('se-ai-go').addEventListener('click', () => {
    const p = aiProvider();
    const out = $('se-ai-msg');
    if (!p) { out.textContent = 'Connect an AI provider in Settings → AI.'; return; }
    const model = aiModel(p);
    if (!model) { out.textContent = `Choose an ${AI_LABEL[p]} model in Settings → AI.`; return; }
    const btn = $('se-ai-go');
    btn.disabled = true;
    out.textContent = `Asking ${AI_LABEL[p]}…`;
    T.core.invoke('ai_write_script', {
      provider: p, model, domain: $('se-domain').value, css: $('se-css').value, js: $('se-js').value, prompt: $('se-ai-prompt').value,
    })
      .then((d) => {
        $('se-css').value = d.css;
        $('se-js').value = d.js;
        out.textContent = (d.note ? d.note + ' ' : '') + 'Review the code, then Save.';
      })
      .catch((e) => { out.textContent = String(e); })
      .finally(() => { btn.disabled = false; });
  });

  // ---- site scripts -------------------------------------------------------

  // Ring around the { } button when an enabled script applies to the page
  // (same rule as the injected guard: the domain or one of its subdomains).
  function renderScriptsMark(tab) {
    let host = '';
    try { host = tab && tab.url ? new URL(tab.url.full).hostname.toLowerCase() : ''; } catch (e) { host = ''; }
    const has = !!host && scripts.some((s) => s.enabled && (host === s.domain || host.endsWith('.' + s.domain)));
    const b = $('scripts-btn');
    b.classList.toggle('has-script', has);
    b.title = has ? 'Site scripts (active on this site)' : 'Site scripts';
    // While the drawer is closed, the ring shows on its toggle instead.
    $('actions-toggle').classList.toggle('has-script', has && $('actions-items').hidden);
  }

  function loadScripts() {
    return T.core.invoke('list_user_scripts').then((items) => { scripts = items; renderScripts(); }).catch((e) => toast(String(e)));
  }

  function renderScripts() {
    renderScriptsMark(activeTab());
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
    $('se-ai-prompt').value = '';
    $('se-ai-msg').textContent = '';
    $('se-ai').hidden = !aiProvider();
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
    $('scripts-pop').classList.toggle('dock', open && !narrow);
    renderOutline();
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
    // Pages start below the status bar; the URL strip runs along the bottom.
    const top = Math.ceil($('safe-top').getBoundingClientRect().height);
    const strip = Math.ceil(window.innerHeight - $('chrome').getBoundingClientRect().top);
    // Native page views are drawn above the chrome, so nothing here can
    // float over a page: panels either dock beside it (shrinking the
    // content area) or set `overlay`, which hides the page.
    const overlay = !!panel || modalOpen || paletteOpen || (scriptsOpen && narrow) || (narrow && sidebarOpen);
    // A docked right panel (site scripts) takes the right of the content area.
    const dock = rightPanel();
    const right = dock ? Math.min(420, Math.floor((window.innerWidth - left) * 0.45)) : 0;
    if (dock) {
      dock.style.top = top + 'px';
      dock.style.bottom = strip + 'px';
      dock.style.width = right + 'px';
    }
    // The docked console sits above the URL strip.
    const con = $('console');
    con.style.left = left + 'px';
    con.style.right = right + 'px';
    con.style.bottom = strip + 'px';
    const bottom = strip + (consoleOpen ? Math.ceil(con.getBoundingClientRect().height) : 0);
    // The chrome surface (start page, notices) covers the focused pane: in
    // split view that is one half of the content area (matching the native
    // pane frames: 2 pt divider, halves floored).
    let sLeft = left;
    let sRight = right;
    if (state && state.split && !overlay) {
      const w = window.innerWidth - left - right;
      const half = Math.floor((w - 2) / 2);
      if (state.active === state.split.left) sRight = right + w - half;
      else sLeft = left + w - half;
    }
    const s = $('surface');
    s.style.top = top + 'px';
    s.style.left = sLeft + 'px';
    s.style.right = sRight + 'px';
    s.style.bottom = bottom + 'px';
    const key = [top, left, right, bottom, overlay].join(':');
    if (key === lastLayout) return;
    lastLayout = key;
    invoke('set_chrome_layout', { top, left, right, bottom, overlay });
  }

  // The panel docked on the right of the content area, if any: site
  // scripts while open, else the headers outline.
  function rightPanel() {
    if (scriptsOpen && !narrow) return $('scripts-pop');
    if (outlineShown()) return $('outline');
    return null;
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

  // ---- view modes ---------------------------------------------------------
  //
  // Each button toggles its mode app-wide; a long press offers "Disable for
  // this tab". The colour and images buttons are "pressed" when the mode is
  // OFF (colour shown, media shown); the headers button when it is ON.

  const VIEW_BUTTONS = [
    { id: 'color', mode: 'desaturate', pressedWhenOff: true, name: 'Black and white' },
    { id: 'images', mode: 'hide_media', pressedWhenOff: true, name: 'Hide images and video' },
  ];

  // The headers view is per tab (its button is in the URL bar).
  function headersOn(tabId) {
    return tabId !== null && tabId !== undefined && (viewModes.headers_tabs || []).includes(tabId);
  }

  function exemptFor(tabId, mode) {
    return tabId !== null && tabId !== undefined && (viewModes.exempt[tabId] || []).includes(mode);
  }

  // Whether `mode` is in effect for a tab.
  function modeOn(mode, tabId) {
    return viewModes.modes[mode] && !exemptFor(tabId, mode);
  }

  function renderViewMode() {
    const tab = state ? state.active : null;
    for (const b of VIEW_BUTTONS) {
      const on = modeOn(b.mode, tab);
      const pressed = b.pressedWhenOff ? !on : on;
      const btn = $(b.id);
      const exempt = viewModes.modes[b.mode] && exemptFor(tab, b.mode);
      btn.classList.toggle('on', pressed && !exempt);
      btn.classList.toggle('exempt', exempt);
      btn.setAttribute('aria-pressed', String(pressed));
      btn.title = exempt ? `${b.name}: off for this tab (long-press to change)` : `${b.name} (long-press for this tab)`;
    }
    const h = $('headers');
    const on = headersOn(tab);
    h.classList.toggle('on', on);
    h.setAttribute('aria-pressed', String(on));
    renderOutline();
  }

  $('headers').addEventListener('click', withTab((t) =>
    applyViewModes(T.core.invoke('set_tab_headers', { tab: t.id, on: !headersOn(t.id) }))));

  // Page actions drawer: opens inside the pill, beside the address.
  function setDrawer(open) {
    $('actions').classList.toggle('drawer-open', open);
    $('actions-items').hidden = !open;
    $('actions-toggle').setAttribute('aria-expanded', String(open));
    syncMenuOpen();
    renderScriptsMark(activeTab());
  }
  $('actions-toggle').addEventListener('click', () => setDrawer($('actions-items').hidden));

  function applyViewModes(p) {
    p.then((m) => { viewModes = m; renderViewMode(); reportLayout(); }).catch((e) => toast(String(e)));
  }

  function toggleMode(mode) {
    applyViewModes(T.core.invoke('set_view_mode', { mode, on: !viewModes.modes[mode] }));
  }

  // Long press (or right click) on a view-mode button.
  let vmPress = null;
  let vmTarget = null;
  function openVmPop(b) {
    const tab = state && state.active;
    if (tab === null || tab === undefined) return;
    vmTarget = b;
    const exempt = exemptFor(tab, b.mode);
    const appOn = viewModes.modes[b.mode];
    $('vm-title').textContent = `${b.name}: ${appOn ? 'on' : 'off'} for all tabs`;
    const item = $('vm-tab');
    item.disabled = !appOn && !exempt;
    item.textContent = exempt ? 'Use the app setting for this tab' : appOn ? 'Disable for this tab' : 'Turn it on to set it per tab';
    closeMenu();
    $('vm-pop').hidden = false;
    syncMenuOpen();
  }
  function closeVmPop() { $('vm-pop').hidden = true; vmTarget = null; syncMenuOpen(); }

  for (const b of VIEW_BUTTONS) {
    const btn = $(b.id);
    btn.addEventListener('pointerdown', (ev) => {
      if (ev.button > 0) return;
      clearTimeout(vmPress && vmPress.timer);
      vmPress = { fired: false, timer: setTimeout(() => { vmPress.fired = true; openVmPop(b); }, 500) };
    });
    const cancel = () => { if (vmPress) clearTimeout(vmPress.timer); };
    btn.addEventListener('pointerup', cancel);
    btn.addEventListener('pointerleave', cancel);
    btn.addEventListener('pointercancel', cancel);
    btn.addEventListener('contextmenu', (ev) => { ev.preventDefault(); cancel(); if (vmPress) vmPress.fired = true; openVmPop(b); });
    btn.addEventListener('click', () => {
      if (vmPress && vmPress.fired) { vmPress = null; return; } // the long press already acted
      vmPress = null;
      toggleMode(b.mode);
    });
  }
  $('vm-tab').addEventListener('click', () => {
    const b = vmTarget;
    const tab = state && state.active;
    closeVmPop();
    if (!b || tab === null) return;
    applyViewModes(T.core.invoke('set_tab_view_exempt', { tab, mode: b.mode, exempt: !exemptFor(tab, b.mode) }));
  });

  // ---- headers view (outline) ---------------------------------------------

  function outlineShown() {
    return !!state && !narrow && !scriptsOpen && headersOn(state.active);
  }

  function renderOutline() {
    const show = outlineShown();
    $('outline').hidden = !show;
    $('outline').classList.toggle('dock', show);
    if (!show) return;
    const tab = state.active;
    if (!headingsByTab.has(tab)) {
      headingsByTab.set(tab, []);
      T.core.invoke('get_headings', { tab }).then((h) => { headingsByTab.set(tab, h); renderOutline(); }).catch(() => {});
    }
    const items = headingsByTab.get(tab) || [];
    const list = $('outline-list');
    list.replaceChildren();
    if (!items.length) {
      list.appendChild(el('li', 'muted', 'No headings on this page yet.'));
      return;
    }
    const min = Math.min(...items.map((h) => h.level));
    for (const h of items) {
      const li = el('li');
      const b = el('button', 'l' + (h.level - min + 1), h.text);
      b.style.paddingLeft = 8 + (h.level - min) * 14 + 'px';
      b.addEventListener('click', () => invoke('scroll_to_heading', { tab, index: h.index }));
      li.appendChild(b);
      list.appendChild(li);
    }
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

  // ---- command palette (new tab) -------------------------------------------
  //
  // Arc-style: ⌘T or New Tab opens a palette instead of a blank tab. Typing
  // fuzzy-matches open tabs, pins and bookmarks first; the last row searches
  // or goes to what was typed in a new tab. Nothing is created until a row
  // is chosen.

  let paletteRows = [];
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

  function paletteCandidates() {
    const out = [];
    if (state) {
      for (const t of state.tabs) {
        if (t.sheet_over !== null || t.launcher !== null) continue;
        out.push({ kind: t.private ? 'Private' : 'Tab', title: t.title || (t.url ? t.url.registrable : 'New Tab'), host: t.url ? t.url.registrable : '', run: () => invoke('activate_tab', { tab: t.id }) });
      }
    }
    for (const p of pins) out.push({ kind: 'Pinned', title: p.title || p.display.registrable, host: p.display.registrable, run: () => openLink(p.url) });
    for (const b of bookmarks) out.push({ kind: 'Bookmark', title: b.title || b.display.registrable, host: b.display.registrable, run: () => openLink(b.url) });
    return out;
  }

  function renderPalette() {
    const q = $('palette-input').value.trim();
    const ql = q.toLowerCase();
    let rows;
    if (!ql) {
      rows = paletteCandidates().slice(0, 8).map((c) => ({ ...c, hits: [] }));
    } else {
      rows = paletteCandidates()
        .map((c) => {
          const m = fuzzy(ql, c.title);
          const mh = fuzzy(ql, c.host);
          const best = m && (!mh || m.score >= mh.score) ? m : mh;
          return best && { ...c, score: best.score, hits: best === m ? best.hits : [] };
        })
        .filter(Boolean)
        .sort((a, b) => b.score - a.score)
        .slice(0, 8);
      rows.push({ kind: 'Go', title: q, host: '', hits: [], go: true, run: () => openTyped(q) });
    }
    paletteRows = rows;
    paletteSel = Math.min(paletteSel, Math.max(0, rows.length - 1));
    const list = $('palette-list');
    list.replaceChildren();
    rows.forEach((r, i) => {
      const li = el('li');
      li.setAttribute('role', 'option');
      li.setAttribute('aria-selected', String(i === paletteSel));
      li.appendChild(el('span', 'kind', r.go ? (looksLikeAddress(q) ? 'Go to' : 'Search') : r.kind));
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
    if (!rows.length) list.appendChild(el('li', 'muted', 'No open tabs or bookmarks yet. Type an address or a search.'));
  }

  function looksLikeAddress(q) {
    return /^[a-z][a-z0-9+.-]*:/i.test(q) || (/^[^\s]+\.[^\s]{2,}/.test(q) && !/\s/.test(q));
  }

  function openTyped(q) {
    T.core.invoke('new_tab', { private: false })
      .then((id) => invoke('navigate', { tab: id, input: q }))
      .catch((e) => toast(String(e)));
  }

  function choosePalette(i) {
    const r = paletteRows[i];
    closePalette();
    if (r) r.run();
  }

  function openPalette() {
    closeMenu();
    if (narrow) setSidebar(false);
    paletteOpen = true;
    paletteSel = 0;
    $('palette').hidden = false;
    const input = $('palette-input');
    input.value = '';
    renderPalette();
    input.focus();
    reportLayout();
  }

  function closePalette() {
    paletteOpen = false;
    $('palette').hidden = true;
    $('palette-input').blur();
    reportLayout();
  }

  $('palette-input').addEventListener('input', () => { paletteSel = 0; renderPalette(); });
  $('palette-input').addEventListener('keydown', (ev) => {
    if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
      ev.preventDefault();
      const n = paletteRows.length;
      if (n) paletteSel = (paletteSel + (ev.key === 'ArrowDown' ? 1 : n - 1)) % n;
      renderPalette();
    } else if (ev.key === 'Enter') {
      ev.preventDefault();
      choosePalette(paletteSel);
    } else if (ev.key === 'Escape') {
      ev.preventDefault();
      closePalette();
    }
  });
  $('palette').addEventListener('click', (ev) => { if (ev.target === $('palette')) closePalette(); });

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
      openPalette();
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
  $('menu-new-tab').addEventListener('click', () => { closeMenu(); openPalette(); });
  $('menu-new-private').addEventListener('click', () => {
    closeMenu();
    if (narrow) setSidebar(false);
    invoke('new_tab', { private: true }).then(beginEdit);
  });

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
    if (!$('vm-pop').hidden && !$('vm-pop').contains(ev.target)) closeVmPop();
    if (!$('actions-items').hidden && !$('actions').contains(ev.target)) setDrawer(false);
    if (narrow && sidebarOpen && !$('sidebar').contains(ev.target) && ev.target !== $('sidebar-toggle')) {
      setSidebar(false);
    }
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape' || editing) return;
    if (paletteOpen) closePalette();
    else if (menuOpen) closeMenu();
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
  loadScripts();
  loadAi();
  T.core.invoke('get_view_mode').then((m) => { viewModes = m; renderViewMode(); reportLayout(); }).catch(() => {});
  T.event.listen('headings', (e) => {
    headingsByTab.set(e.payload.tab, e.payload.headings);
    if (state && e.payload.tab === state.active) renderOutline();
  });
})();
