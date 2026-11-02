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
  let chatOpen = false;
  // ⌘⇧\ hides the right panels without closing them (headers mode and
  // the chat stay on, and come back with the next ⌘⇧\).
  let rightHidden = false;
  // The sidebar shown as a pop-over from the left-edge tab (sidebar closed).
  let findOpen = false; // ⌘F bar
  let confirmAction = null; // { tab, run } while a confirmation is shown
  let bookmarks = [];
  let pins = [];
  const MAX_PINS = 4;
  // View modes (session-only, kept in Rust): app-wide on/off, and tabs
  // exempted from a mode ("Disable for this tab").
  let viewModes = { modes: { desaturate: true, hide_media: true, headers: false, dark: false }, exempt: {} };
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
    renderChat();
    // Find and confirmations belong to the tab they were opened on.
    if (findOpen && find.tab !== null && find.tab !== state.active) closeFind();
    if (selection && selection.tab !== state.active && !(state.split && (selection.tab === state.split.left || selection.tab === state.split.right))) hideSelection();
    if (confirmAction && confirmAction.tab !== state.active && confirmAction.tab !== state.sheet) closeConfirm();
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
    const marked = !!(tab && ((tab.launcher && bookmarks.some((b) => b.url === tab.launcher)) || (tab.url && bookmarks.some((b) => b.url === tab.url.full))));
    $('bookmark').querySelector('.label').textContent = marked ? 'Remove Bookmark' : 'Bookmark';
    $('bookmark').querySelector('.state').textContent = marked ? '\u2605' : '\u2606';
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
    if (state) { renderLauncherSelection(); renderToolbar(activeTab()); }
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
  // The sidebar has two modes (remembered): `attached`, a full-height
  // column beside the page that the toggle shows and hides, and `flyout`,
  // hidden until the pointer rests on the toggle tab and gone again when it
  // leaves. In narrow windows it slides over the page (mode ignored).
  let sidebarMode = 'attached';
  let sidebarShown = true;

  function pref(key) {
    try { return localStorage.getItem(key); } catch (e) { return null; }
  }
  function setPref(key, value) {
    try { localStorage.setItem(key, value); } catch (e) { /* storage unavailable */ }
  }

  function loadSidebarPrefs() {
    sidebarMode = pref('sidebarMode') === 'flyout' ? 'flyout' : 'attached';
    sidebarShown = !narrow && sidebarMode === 'attached' && pref('sidebarOpen') !== '0';
  }
  loadSidebarPrefs();

  // The page starts to the sidebar's right (attached and shown, wide window).
  function sidebarDocked() {
    return !narrow && sidebarMode === 'attached' && sidebarShown;
  }
  function flyoutMode() {
    return !narrow && sidebarMode === 'flyout';
  }

  function clampWidth(w) {
    return Math.round(Math.max(MIN_SIDEBAR, Math.min(MAX_SIDEBAR, w, window.innerWidth * 0.5)));
  }

  function setSidebarWidth(w) {
    document.documentElement.style.setProperty('--sidebar-w', clampWidth(w) + 'px');
  }

  // The one place the sidebar's state changes. Everything else (⌘\, the
  // toggle, hover, Settings, narrow windows) calls this.
  function setSidebarState(mode, shown) {
    clearTimeout(flyTimer);
    clearTimeout(hoverTimer);
    const changed = mode !== sidebarMode || shown !== sidebarShown;
    sidebarMode = mode;
    sidebarShown = shown;
    if (!narrow) {
      setPref('sidebarMode', mode);
      if (mode === 'attached') setPref('sidebarOpen', shown ? '1' : '0');
    }
    if (!shown) closeMenu();
    renderSidebarMode();
    if (changed) applySidebar(true);
  }

  // Show or hide, keeping the mode.
  function setSidebar(open) {
    setSidebarState(sidebarMode, open);
  }

  // ⌘\ / ⌘S / clicking the toggle: attached → show or hide; flyout (hidden
  // or showing) → attached and shown.
  function toggleSidebar() {
    if (narrow || sidebarMode === 'attached') setSidebarState(sidebarMode, !sidebarShown);
    else setSidebarState('attached', true);
  }

  // `animate`: the page and its cut-outs slide with the sidebar (the host
  // animates them over the same 0.25 s as the CSS).
  let slideStartedAt = 0;
  function applySidebar(animate) {
    const b = document.body.classList;
    b.toggle('narrow', narrow);
    b.toggle('sb-flyout', flyoutMode());
    b.toggle('sb-hidden', !sidebarShown);
    b.toggle('sb-docked', sidebarDocked());
    if (animate) slideStartedAt = Date.now();
    $('sidebar').setAttribute('aria-hidden', String(!sidebarShown));
    const tab = $('edge-tab');
    tab.setAttribute('aria-expanded', String(sidebarShown));
    tab.setAttribute('aria-label', flyoutMode() ? 'Tabs (flyout; click to attach)' : sidebarShown ? 'Hide tabs' : 'Show tabs');
    reportLayout({ animate: !!animate });
  }

  function renderSidebarMode() {
    document.querySelectorAll('[data-sidebar-mode]').forEach((b) =>
      b.setAttribute('aria-pressed', String(b.dataset.sidebarMode === sidebarMode)));
  }
  document.querySelectorAll('[data-sidebar-mode]').forEach((b) => b.addEventListener('click', () => {
    const mode = b.dataset.sidebarMode;
    setSidebarState(mode, mode === 'attached');
  }));

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

  // The right panel's width (headers, chat, site scripts), dragged at its
  // left edge and remembered.
  const MIN_RIGHT = 260;
  const MAX_RIGHT = 640;
  let rightWidth = parseFloat(pref('rightWidth') || '') || 380;
  function clampRight(w, left) {
    return Math.round(Math.max(MIN_RIGHT, Math.min(MAX_RIGHT, w, (window.innerWidth - left) * 0.6)));
  }
  let rightDrag = null;
  $('right-resize').addEventListener('pointerdown', (ev) => {
    ev.preventDefault();
    $('right-resize').setPointerCapture(ev.pointerId);
    rightDrag = { x: ev.clientX, w: rightWidth };
    document.body.classList.add('resizing-right');
  });
  $('right-resize').addEventListener('pointermove', (ev) => {
    if (!rightDrag) return;
    rightWidth = Math.max(MIN_RIGHT, Math.min(MAX_RIGHT, rightDrag.w + rightDrag.x - ev.clientX));
    queueLayout();
  });
  const endRightResize = () => {
    if (!rightDrag) return;
    rightDrag = null;
    document.body.classList.remove('resizing-right');
    setPref('rightWidth', String(Math.round(rightWidth)));
    reportLayout();
  };
  $('right-resize').addEventListener('pointerup', endRightResize);
  $('right-resize').addEventListener('pointercancel', endRightResize);

  // ---- sidebar pop-over (sidebar closed) -------------------------------------
  //
  // A tab stays on the left edge. Pointing at it (or tapping it, without a
  // pointer) shows the sidebar as a pop-over over the page, inset 20 pt top
  // and bottom; it goes away when the pointer leaves it. In narrow windows
  // the tab opens the usual overlaid sidebar instead.
  // ---- toggle tab and flyout ----------------------------------------------
  //
  // The tab is part of the sidebar (it moves with it). In flyout mode,
  // pointing at it shows the sidebar at once; leaving the sidebar (tab
  // included) hides it again. Clicking the tab follows ⌘\. Without a
  // pointer, a first tap shows the flyout and a second attaches it.
  // ⌘-click switches mode.
  let flyTimer = 0;
  let hoverTimer = 0;
  const LEAVE_MS = 150;
  let tabPointer = 'mouse';
  function sidebarBusy() {
    return menuOpen || !$('vm-pop').hidden || !!dragState || !!resize;
  }
  function hideFlyoutSoon() {
    clearTimeout(flyTimer);
    flyTimer = setTimeout(() => {
      if (flyoutMode() && sidebarShown && !sidebarBusy()) setSidebarState('flyout', false);
    }, LEAVE_MS);
  }
  function switchSidebarMode() {
    if (narrow) return;
    if (sidebarMode === 'attached') setSidebarState('flyout', false);
    else setSidebarState('attached', true);
  }
  const tabEl = $('edge-tab');
  tabEl.addEventListener('pointerenter', (ev) => {
    clearTimeout(flyTimer);
    if (ev.pointerType === 'mouse' && flyoutMode() && !sidebarShown) setSidebarState('flyout', true);
  });
  tabEl.addEventListener('pointerdown', (ev) => { tabPointer = ev.pointerType || 'mouse'; });
  tabEl.addEventListener('click', (ev) => {
    ev.stopPropagation();
    if (ev.metaKey) { switchSidebarMode(); return; }
    // No hover without a pointer: the first tap shows the flyout.
    if (flyoutMode() && !sidebarShown && tabPointer !== 'mouse') { setSidebarState('flyout', true); return; }
    toggleSidebar();
  });
  $('sidebar').addEventListener('pointerenter', () => clearTimeout(flyTimer));
  $('sidebar').addEventListener('pointerleave', (ev) => {
    if (ev.pointerType === 'mouse' && flyoutMode() && sidebarShown) hideFlyoutSoon();
  });
  // The pointer may leave the chrome's web view straight onto the page.
  document.addEventListener('mouseout', (ev) => { if (!ev.relatedTarget && flyoutMode() && sidebarShown) hideFlyoutSoon(); });

  // ---- menu, settings, history ----------------------------------------------

  function openMenu() { menuOpen = true; $('menu-pop').hidden = false; document.body.classList.add('menu-open'); }
  function closeMenu() { menuOpen = false; $('menu-pop').hidden = true; syncMenuOpen(); }
  // Keeps the chrome fully opaque while any of its menus is open.
  function syncMenuOpen() {
    const open = menuOpen || !$('vm-pop').hidden || !$('actions-pop').hidden || findOpen || !$('confirm-pop').hidden;
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

  const SETTINGS_TITLES = { general: 'General', ai: 'AI', keys: 'Keyboard', cache: 'Cache' };
  function showSettings(section) {
    document.querySelectorAll('.settings-nav button').forEach((b) => b.setAttribute('aria-current', b.dataset.section === section ? 'page' : 'false'));
    document.querySelectorAll('.settings-body [data-panel]').forEach((p) => { p.hidden = p.dataset.panel !== section; });
    $('settings-heading').textContent = SETTINGS_TITLES[section];
    if (section === 'ai') { loadAi(); loadUsage(); }
    if (section === 'keys') renderKeys();
  }
  document.querySelectorAll('.settings-nav button').forEach((b) => b.addEventListener('click', () => showSettings(b.dataset.section)));

  // ---- AI connections -----------------------------------------------------
  //
  // Keys go to the Keychain through Rust and never come back; the chrome
  // only learns whether one is stored (Authentication). Each feature that
  // uses AI has its own service and model (Uses); both are UI preferences
  // (localStorage), as are the model lists Test fetched.

  const AI_DEFAULT_MODEL = { anthropic: 'claude-opus-5-5', openai: '' };
  const AI_LABEL = { anthropic: 'Anthropic', openai: 'OpenAI' };
  const AI_USES = [
    { id: 'scripts', label: 'Site script helper', note: 'Drafts CSS and JavaScript in Site Scripts.' },
    { id: 'ask', label: 'Ask Selection', note: 'The page chat and “Ask AI” on selected text.' },
    { id: 'headers', label: 'Header View Parsing', note: 'Detect Headers in the headers view.' },
  ];
  let aiConfigured = { anthropic: false, openai: false };

  function aiModels(p) {
    try { return JSON.parse(pref('aiModels.' + p) || '[]'); } catch (e) { return []; }
  }
  function connectedProviders() {
    return ['anthropic', 'openai'].filter((p) => aiConfigured[p]);
  }
  // The service and model a use is set to: its saved choice if that
  // service is still connected, else the first connected one with its
  // first listed model (or the legacy per-provider model).
  function useChoice(id) {
    const ps = connectedProviders();
    if (!ps.length) return null;
    const saved = pref(`use.${id}.provider`);
    const p = ps.includes(saved) ? saved : ps[0];
    const models = aiModels(p);
    const m = pref(`use.${id}.model`);
    const model = (p === saved && m && (!models.length || models.some((x) => x.id === m)) ? m : null)
      || pref('aiModel.' + p) || (models[0] && models[0].id) || AI_DEFAULT_MODEL[p];
    if (!model) return null;
    const hit = models.find((x) => x.id === model);
    return { p, id: model, name: hit ? hit.name : model };
  }
  function setUse(id, p, model) {
    setPref(`use.${id}.provider`, p);
    setPref(`use.${id}.model`, model);
  }

  function loadAi() {
    return T.core.invoke('ai_status').then((st) => { aiConfigured = st.configured; renderAi(); }).catch(() => {});
  }

  function renderAi() {
    document.querySelectorAll('.ai-provider').forEach((box) => {
      const p = box.dataset.provider;
      const on = !!aiConfigured[p];
      const n = aiModels(p).length;
      box.querySelector('.ai-state').textContent = on ? (n ? `Key saved · ${n} models` : 'Key saved') : 'Not connected';
      box.querySelector('.ai-key').placeholder = on ? 'Replace the saved key' : `${AI_LABEL[p]} API key`;
      box.querySelector('.ai-test').disabled = !on;
      box.querySelector('.ai-remove').disabled = !on;
    });
    renderUses();
    $('se-ai').hidden = !useChoice('scripts');
  }

  // Uses: one row per feature, with a service menu and a model menu.
  function renderUses() {
    const box = $('ai-uses');
    box.replaceChildren();
    const ps = connectedProviders();
    for (const u of AI_USES) {
      const row = el('div', 'ai-use');
      const label = el('div', 'ai-use-label');
      label.append(el('div', 'option-title', u.label), el('div', 'muted small', u.note));
      const svc = el('select');
      svc.setAttribute('aria-label', `${u.label}: service`);
      const model = el('select');
      model.setAttribute('aria-label', `${u.label}: model`);
      const choice = useChoice(u.id);
      if (!ps.length) {
        svc.appendChild(el('option', null, 'Connect a service above'));
        svc.disabled = model.disabled = true;
      } else {
        for (const p of ps) { const o = el('option', null, AI_LABEL[p]); o.value = p; svc.appendChild(o); }
        svc.value = choice ? choice.p : ps[0];
        const list = aiModels(svc.value);
        if (!list.length) {
          const o = el('option', null, choice ? choice.id : 'Test the key to list models');
          o.value = choice ? choice.id : '';
          model.appendChild(o);
        }
        for (const m of list) {
          const o = el('option', null, m.name === m.id ? m.id : `${m.name} (${m.id})`);
          o.value = m.id;
          model.appendChild(o);
        }
        if (choice) model.value = choice.id;
      }
      svc.addEventListener('change', () => {
        const list = aiModels(svc.value);
        setUse(u.id, svc.value, (list[0] && list[0].id) || AI_DEFAULT_MODEL[svc.value] || '');
        renderAi();
        renderChatModels();
      });
      model.addEventListener('change', () => { setUse(u.id, svc.value, model.value); renderChatModels(); });
      const pickers = el('div', 'ai-use-pickers');
      pickers.append(svc, model);
      row.append(label, pickers);
      box.appendChild(row);
    }
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
          renderAi();
          msg(`Connected. ${models.length} model${models.length === 1 ? '' : 's'} available; choose them under Uses.`);
        })
        .catch((e) => msg(String(e)));
    };
    box.querySelector('.ai-test').addEventListener('click', test);
    box.querySelector('.ai-remove').addEventListener('click', () => {
      T.core.invoke('ai_delete_key', { provider: p })
        .then((st) => { aiConfigured = st.configured; renderAi(); msg('Key removed from this device.'); })
        .catch((e) => msg(String(e)));
    });
  });

  // Usage: AI requests grouped by local date, with estimated cost.
  function money(micros) {
    const d = micros / 1e6;
    return '$' + (d > 0 && d < 0.01 ? d.toFixed(4) : d.toFixed(2));
  }
  function loadUsage() {
    T.core.invoke('ai_usage').then((events) => {
      const days = new Map();
      for (const e of events) {
        const dt = new Date(e.at_ms);
        const key = `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, '0')}-${String(dt.getDate()).padStart(2, '0')}`;
        const d = days.get(key) || { label: dt.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' }), n: 0, input: 0, output: 0, cost: 0, unpriced: false };
        d.n += 1; d.input += e.input; d.output += e.output;
        if (e.cost_micros === null || e.cost_micros === undefined) d.unpriced = true; else d.cost += e.cost_micros;
        days.set(key, d);
      }
      const body = $('ai-usage-table').tBodies[0];
      const foot = $('ai-usage-table').tFoot;
      body.replaceChildren();
      foot.replaceChildren();
      const fmt = (n) => n.toLocaleString();
      const row = (cells, cls) => { const tr = el('tr', cls); for (const c of cells) tr.appendChild(el('td', null, c)); return tr; };
      const keys = [...days.keys()].sort().reverse();
      if (!keys.length) { body.appendChild(row(['No AI requests yet.', '', '', '', ''])); return; }
      const total = { n: 0, input: 0, output: 0, cost: 0, unpriced: false };
      for (const k of keys) {
        const d = days.get(k);
        body.appendChild(row([d.label, fmt(d.n), fmt(d.input), fmt(d.output), (d.unpriced ? '≥ ' : '') + money(d.cost)]));
        total.n += d.n; total.input += d.input; total.output += d.output; total.cost += d.cost; total.unpriced ||= d.unpriced;
      }
      foot.appendChild(row(['Total', fmt(total.n), fmt(total.input), fmt(total.output), (total.unpriced ? '≥ ' : '') + money(total.cost)]));
    }).catch(() => {});
  }
  $('ai-usage-reset').addEventListener('click', () => {
    const b = $('ai-usage-reset');
    if (!b.classList.contains('armed')) {
      b.classList.add('armed'); b.textContent = 'Tap again to reset';
      setTimeout(() => { b.classList.remove('armed'); b.textContent = 'Reset'; }, 4000);
      return;
    }
    b.classList.remove('armed'); b.textContent = 'Reset';
    invoke('ai_reset_usage').then(loadUsage);
  });

  // Site script helper: drafts CSS/JS into the editor for the user to review.
  $('se-ai-go').addEventListener('click', () => {
    const use = useChoice('scripts');
    const out = $('se-ai-msg');
    if (!use) { out.textContent = 'Connect an AI service in Settings → AI.'; return; }
    const p = use.p;
    const model = use.id;
    const btn = $('se-ai-go');
    btn.disabled = true;
    out.textContent = `Asking ${AI_LABEL[p]}…`;
    T.core.invoke('ai_write_script', {
      provider: p, model, domain: $('se-domain').value, css: $('se-css').value, js: $('se-js').value, prompt: $('se-ai-prompt').value,
    })
      .then((d) => {
        loadUsage();
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
    b.querySelector('.state').textContent = has ? 'Active { }' : '{ }';
    b.title = has ? 'Site scripts (active on this site)' : 'Site scripts';
    // While the popup is closed, the ring shows on its toggle instead.
    $('actions-toggle').classList.toggle('has-script', has && $('actions-pop').hidden);
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
    $('se-ai').hidden = !useChoice('scripts');
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
  let contentBox = { left: 0, top: 0, right: 0, bottom: 0 };
  function reportLayout(opts) {
    // Use the sidebar's layout width (unaffected by the slide transform) so
    // the content area moves once, to its final position.
    const left = sidebarDocked() ? Math.ceil($('sidebar').offsetWidth) : 0;
    // Pages start below the status bar and run to the bottom of the window;
    // the URL pill floats over them.
    const top = Math.ceil($('safe-top').getBoundingClientRect().height);
    // Native page views are drawn above the chrome. Panels either dock
    // beside the page (shrinking the content area), float over it as a
    // hole (the pill, its popup, toasts: the page is cut out there), or set
    // `overlay`, which hides the page.
    const overlay = !!panel || modalOpen || paletteOpen || (scriptsOpen && narrow) || (chatShown() && narrow) || (narrow && sidebarShown);
    // Docked right panels (site scripts; or the headers outline and the
    // chat, one above the other when both are open) take the right of the
    // content area, full height, at the width the user dragged.
    const docks = rightPanels();
    const right = docks.length ? clampRight(rightWidth, left) : 0;
    $('chrome').style.left = left + 'px';
    $('chrome').style.right = right + 'px';
    const strip = Math.ceil(window.innerHeight - $('chrome').getBoundingClientRect().top);
    const mid = Math.round(top + (window.innerHeight - top) / 2);
    docks.forEach((d, i) => {
      const split = docks.length > 1;
      d.style.top = (split && i === 1 ? mid : top) + 'px';
      d.style.bottom = (split && i === 0 ? window.innerHeight - mid : 0) + 'px';
      d.style.width = right + 'px';
      d.classList.toggle('split-lower', split && i === 1);
    });
    const edge = $('right-resize');
    edge.hidden = !docks.length;
    edge.style.left = window.innerWidth - right + 'px';
    edge.style.top = top + 'px';
    // The docked console sits above the URL strip; while it is open the
    // page ends above it (the pill then floats over chrome, not the page).
    const con = $('console');
    con.style.left = left + 'px';
    con.style.right = right + 'px';
    con.style.bottom = strip + 'px';
    const bottom = consoleOpen ? strip + Math.ceil(con.getBoundingClientRect().height) : 0;
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
    contentBox = { left, top, right, bottom };
    const holes = overlay ? [] : floatingHoles();
    const key = JSON.stringify([top, left, right, bottom, overlay, holes]);
    if (key === lastLayout) return;
    lastLayout = key;
    // When the sidebar slides, the host animates the page and its cut-outs
    // to match, starting from when the CSS transition did.
    invoke('set_chrome_layout', { top, left, right, bottom, overlay, holes, animateSince: opts && opts.animate ? slideStartedAt : null });
  }

  // Chrome that floats over the page: the pill, its popup, toasts and
  // banners. Each is cut out of the page (window coordinates, rounded to
  // whole points, 1 pt larger than the element).
  // `c`: which corners are rounded (1 top left, 2 top right, 4 bottom left,
  // 8 bottom right). The find bar sits on the pill: its hole ends where the
  // pill's begins and only its top corners are rounded, so the two never
  // overlap (overlapping holes are merged into one rectangle).
  function floatingHoles() {
    const out = [];
    const hole = (e, c) => {
      const r = e.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) return null;
      const radius = parseFloat(getComputedStyle(e).borderTopRightRadius) || 0;
      const x = Math.floor(r.left) - 1;
      const y = Math.floor(r.top) - 1;
      return { x, y, w: Math.ceil(r.right) + 1 - x, h: Math.ceil(r.bottom) + 1 - y, r: radius + 1, c };
    };
    const pill = hole($('toolbar'), 15);
    if (pill) out.push(pill);
    if (findOpen) {
      const f = hole($('findbar'), 3);
      if (f && pill) f.h = Math.max(1, pill.y - f.y);
      if (f) out.push(f);
    }
    for (const id of ['actions-pop', 'confirm-pop', 'sel-pop', 'toast', 'os-banner']) {
      const e = $(id);
      if (e.hidden) continue;
      const h = hole(e, 15);
      if (h) out.push(h);
    }
    out.push(...sidebarHoles());
    return out;
  }

  // The sidebar's two cut-outs, always reported (so the host can slide
  // them): the panel, where the flyout is or will be (off-screen while
  // hidden; when attached it lies left of the page and cuts nothing), and
  // the toggle tab on its right edge. Computed from the target state, not
  // the on-screen boxes, which may still be sliding.
  const FLY_INSET = 20;
  const TAB_H = 76;
  function sidebarHoles() {
    if (narrow) {
      if (sidebarShown) return [];
      return [{ x: 0, y: Math.round((window.innerHeight - TAB_H) / 2) - 1, w: 15, h: TAB_H + 2, r: 11, c: 10 }];
    }
    const w = $('sidebar').offsetWidth;
    const fly = sidebarMode === 'flyout';
    const top = fly ? FLY_INSET + $('safe-top').offsetHeight : 0;
    const height = window.innerHeight - top - (fly ? FLY_INSET + $('safe-bottom').offsetHeight : 0);
    const panelX = sidebarShown ? -1 : -(w + 3);
    const tabX = sidebarShown ? w : 0;
    return [
      { x: panelX, y: top - 1, w: w + 1, h: height + 2, r: 15, c: 10 },
      { x: tabX, y: Math.round(top + (height - TAB_H) / 2) - 1, w: 15, h: TAB_H + 2, r: 11, c: 10 },
    ];
  }

  // The panels docked on the right of the content area: site scripts while
  // open; otherwise the headers outline and/or the chat (outline on top).
  function rightPanels() {
    if (narrow || rightHidden) return [];
    if (scriptsOpen) return [$('scripts-pop')];
    const out = [];
    if (outlineShown()) out.push($('outline'));
    if (chatShown()) out.push($('chat'));
    return out;
  }

  function chatShown() {
    return chatOpen && !rightHidden;
  }

  // ---- ad blocking ------------------------------------------------------

  let lastAdblockFailure = '';
  function renderShield(tab) {
    const b = $('shield');
    const a = tab && tab.adblock;
    const status = state.adblock;
    const label = b.querySelector('.label');
    const st = b.querySelector('.state');
    b.className = '';
    st.textContent = '';
    if (status.state === 'failed') {
      label.textContent = 'Ad blocker failed';
      b.classList.add('off');
      b.disabled = true;
      if (status.message !== lastAdblockFailure) {
        lastAdblockFailure = status.message;
        toast('Ad blocker could not start: ' + status.message);
      }
      return;
    }
    if (status.state === 'compiling') {
      label.textContent = 'Ad blocker starting…';
      b.disabled = true;
      return;
    }
    b.hidden = !a;
    if (!a) return;
    label.textContent = 'Ad Blocking';
    st.textContent = a.blocking ? 'On' : 'Off for this site';
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
    { id: 'dark', mode: 'dark', pressedWhenOff: false, name: 'Dark pages' },
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
    $('chat-btn').classList.toggle('on', chatOpen);
    $('chat-btn').setAttribute('aria-pressed', String(chatOpen));
    renderOutline();
  }

  $('headers').addEventListener('click', withTab((t) => {
    const on = !headersOn(t.id);
    if (on && rightHidden) { rightHidden = false; applyChat(); }
    applyViewModes(T.core.invoke('set_tab_headers', { tab: t.id, on }));
  }));

  // Page actions: a popup above the ⋯ button, floating over the page (a
  // hole, like the pill).
  function setActions(open) {
    $('actions').classList.toggle('open', open);
    $('actions-pop').hidden = !open;
    $('actions-toggle').setAttribute('aria-expanded', String(open));
    syncMenuOpen();
    renderScriptsMark(activeTab());
    reportLayout();
  }
  $('actions-toggle').addEventListener('click', () => setActions($('actions-pop').hidden));

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

  // Index of the heading each tab's reader has scrolled to.
  const currentHeading = new Map();

  function markCurrentHeading(tab) {
    const cur = currentHeading.get(tab);
    let shown = null;
    document.querySelectorAll('#outline-list li[data-index]').forEach((li) => {
      const on = Number(li.dataset.index) === cur;
      li.classList.toggle('current', on);
      if (on) shown = li;
    });
    document.querySelectorAll('#outline-list li.section').forEach((sec) =>
      sec.classList.toggle('has-current', !!shown && sec.contains(shown)));
    // Keep the current entry in view in the panel (its group's label when folded).
    if (shown) {
      const folded = shown.closest('li.section.collapsed');
      (folded ? folded.querySelector('.sec-row') : shown).scrollIntoView({ block: 'nearest' });
    }
  }

  function outlineMsg(text) {
    $('outline-msg').hidden = !text;
    $('outline-msg').textContent = text || '';
  }

  // "Detect Headers": the AI picks this site's heading elements.
  $('detect-headers').addEventListener('click', () => {
    const tab = state && state.active;
    const use = useChoice('headers');
    if (!use) { outlineMsg('Connect an AI service in Settings → AI to detect headers.'); return; }
    const p = use.p;
    const model = use.id;
    const btn = $('detect-headers');
    btn.disabled = true;
    outlineMsg(`Asking ${AI_LABEL[p]} which elements are headings…`);
    T.core.invoke('detect_headings', { tab, provider: p, model })
      .then((n) => outlineMsg(`Using ${n} rule${n === 1 ? '' : 's'} for this site: section labels group the headings under them.`))
      .catch((e) => outlineMsg(String(e)))
      .finally(() => { btn.disabled = false; });
  });

  function outlineShown() {
    return !!state && !narrow && !scriptsOpen && !rightHidden && headersOn(state.active);
  }

  // Folded sections, per tab and section index (session only).
  const foldedSections = new Set();

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
    const entry = (h, base) => {
      const li = el('li');
      li.dataset.index = String(h.index);
      if (currentHeading.get(tab) === h.index) li.classList.add('current');
      const b = el('button', 'l' + (h.level - min + 1), h.text);
      b.style.paddingLeft = 8 + Math.max(0, h.level - base) * 14 + 'px';
      b.addEventListener('click', () => invoke('scroll_to_heading', { tab, index: h.index }));
      li.appendChild(b);
      return li;
    };
    // Sections: several top-level entries with headings under them (what
    // Detect Headers produces for front pages). Otherwise a nested list.
    const tops = items.filter((h) => h.level === min).length;
    const sectioned = tops >= 2 && items.some((h) => h.level > min);
    if (!sectioned) {
      for (const h of items) list.appendChild(entry(h, min));
      markCurrentHeading(tab);
      return;
    }
    let group = null;
    for (const h of items) {
      if (h.level === min || !group) {
        const key = `${tab}:${h.index}`;
        const li = el('li', 'section' + (foldedSections.has(key) ? ' collapsed' : ''));
        li.dataset.index = String(h.index);
        const row = el('div', 'sec-row');
        const toggle = el('button', 'sec-toggle', '\u25BE');
        toggle.setAttribute('aria-label', `Fold or unfold ${h.text}`);
        toggle.setAttribute('aria-expanded', String(!foldedSections.has(key)));
        toggle.addEventListener('click', () => {
          if (foldedSections.has(key)) foldedSections.delete(key); else foldedSections.add(key);
          renderOutline();
        });
        const title = el('button', 'sec-title', h.text);
        title.addEventListener('click', () => invoke('scroll_to_heading', { tab, index: h.index }));
        const count = el('span', 'sec-count');
        row.append(toggle, title, count);
        const sub = el('ol');
        li.append(row, sub);
        list.appendChild(li);
        group = { sub, count, n: 0 };
        if (h.level === min) continue;
      }
      group.sub.appendChild(entry(h, min + 1));
      group.n += 1;
      group.count.textContent = String(group.n);
    }
    markCurrentHeading(tab);
  }

  // ---- AI chat about the page ---------------------------------------------
  //
  // One conversation per tab. A new one starts when the tab's page changes
  // (unless it was opened from the archive) or with "+". Each message is
  // sent with the whole conversation. The first reads the page; Rust keeps
  // the chat (with that page context) in the archive, except in private
  // tabs, and later turns reuse it. Replies are model output and are shown
  // with textContent only.

  // tab id -> { url, sticky, chatId, context, messages: [{role, text}],
  //             log, input, output, cost, unpriced, busy }
  const chats = new Map();
  let archiveOpen = false;

  function newChat(url) {
    return { url, sticky: false, chatId: null, context: '', messages: [], log: [], input: 0, output: 0, cost: 0, unpriced: false, busy: false };
  }

  function chatFor(t) {
    const url = t && t.url ? t.url.full : '';
    let c = chats.get(t.id);
    if (!c || (c.url !== url && !c.sticky && !c.busy)) {
      c = newChat(url);
      chats.set(t.id, c);
    }
    return c;
  }

  // Models from every connected provider (the lists Settings → AI → Test
  // fetched).
  function chatModels() {
    const out = [];
    for (const p of ['anthropic', 'openai']) {
      if (!aiConfigured[p]) continue;
      const models = aiModels(p);
      if (!models.length) {
        const c = useChoice('ask');
        const id = c && c.p === p ? c.id : AI_DEFAULT_MODEL[p];
        if (id) out.push({ p, id, name: id });
      }
      for (const m of models) out.push({ p, id: m.id, name: m.name || m.id });
    }
    return out;
  }

  // The chat's model is the "Ask Selection" use (Settings → AI → Uses);
  // the model link in the chat changes that use.
  function chatChoice() {
    const models = chatModels();
    const c = useChoice('ask');
    return (c && models.find((m) => m.p === c.p && m.id === c.id)) || (c && { p: c.p, id: c.id, name: c.name }) || models[0] || null;
  }

  function renderChatModels() {
    const c = chatChoice();
    const btn = $('chat-model-btn');
    btn.textContent = c ? `${c.name} ▾` : 'No model ▾';
    btn.disabled = !chatModels().length;
  }

  function setModelMenu(open) {
    const menu = $('chat-model-menu');
    menu.hidden = !open;
    $('chat-model-btn').setAttribute('aria-expanded', String(open));
    if (!open) return;
    menu.replaceChildren();
    const cur = chatChoice();
    let lastP = null;
    for (const m of chatModels()) {
      if (m.p !== lastP && connectedProviders().length > 1) { menu.appendChild(el('div', 'vm-title', AI_LABEL[m.p])); lastP = m.p; }
      const b = el('button', cur && cur.p === m.p && cur.id === m.id ? 'chosen' : '', m.name);
      b.setAttribute('role', 'menuitemradio');
      b.setAttribute('aria-checked', String(!!(cur && cur.p === m.p && cur.id === m.id)));
      b.addEventListener('click', () => {
        setUse('ask', m.p, m.id);
        setModelMenu(false);
        renderChatModels();
        chatKey = '';
        renderChat();
      });
      menu.appendChild(b);
    }
  }
  $('chat-model-btn').addEventListener('click', () => setModelMenu($('chat-model-menu').hidden));

  function dateLabel(ms) {
    return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
  }

  let chatKey = '';
  function renderChat() {
    if (!chatShown()) { chatKey = ''; return; }
    const t = activeTab();
    $('chat-archive').setAttribute('aria-pressed', String(archiveOpen));
    $('chat-log').hidden = archiveOpen;
    $('chat-form').hidden = archiveOpen;
    $('chat-archive-list').hidden = !archiveOpen;
    if (archiveOpen) { $('chat-context').hidden = true; return; }
    // State updates arrive often (load progress); rebuild only on change,
    // so a selection in the log survives.
    const cur = t ? chats.get(t.id) : null;
    const pick = chatChoice();
    const key = JSON.stringify([t && t.id, t && t.url && t.url.full, cur && cur.log.length, cur && cur.busy, cur && cur.chatId, pick && pick.p + pick.id]);
    if (key === chatKey) return;
    chatKey = key;
    renderChatModels();
    const log = $('chat-log');
    const atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 24;
    log.replaceChildren();
    const c = t && (t.url || (cur && cur.sticky)) ? chatFor(t) : null;
    $('chat-context').hidden = !(c && c.context);
    $('chat-context').textContent = c ? c.context : '';
    if (!c) {
      $('chat-cost').textContent = '';
      $('chat-send').disabled = true;
      return;
    }
    for (const m of c.log) {
      const li = el('li', m.role, m.text);
      if (m.meta) li.appendChild(el('span', 'meta', m.meta));
      log.appendChild(li);
    }
    if (c.busy) log.appendChild(el('li', 'assistant pending', '…'));
    const tokens = c.input + c.output;
    $('chat-cost').textContent = tokens ? `${c.unpriced ? '≥ ' : '≈ '}${money(c.cost)} · ${tokens.toLocaleString()} tokens` : '';
    $('chat-send').disabled = c.busy || !pick;
    if (atEnd || c.busy) log.scrollTop = log.scrollHeight;
  }

  // ---- archive: earlier chats, to read again, continue or delete ----------

  function setArchive(open) {
    archiveOpen = open;
    setModelMenu(false);
    if (open) loadArchive();
    chatKey = '';
    renderChat();
  }

  function loadArchive() {
    const list = $('chat-archive-list');
    T.core.invoke('list_chats').then((items) => {
      list.replaceChildren();
      for (const it of items) {
        const li = el('li');
        const open = el('button', 'open');
        open.appendChild(el('span', 't', it.first || it.title || it.host));
        const cost = it.cost_micros || it.unpriced ? ` · ${it.unpriced ? '≥ ' : ''}${money(it.cost_micros)}` : '';
        open.appendChild(el('span', 'u', `${it.host} · ${dateLabel(it.updated_ms)}${cost}`));
        open.addEventListener('click', () => openArchived(it.id));
        const rm = el('button', 'remove', '×');
        rm.setAttribute('aria-label', 'Delete this chat');
        rm.addEventListener('click', () => {
          for (const [tab, c] of chats) if (c.chatId === it.id) chats.delete(tab);
          invoke('delete_chat', { id: it.id }).then(loadArchive);
        });
        li.append(open, rm);
        list.appendChild(li);
      }
      if (!items.length) list.appendChild(el('li', 'muted empty', '—'));
    }).catch((e) => toast(String(e)));
  }

  // Continues an archived chat in this tab, with the context it began with.
  function openArchived(id) {
    const t = activeTab();
    if (!t) return;
    T.core.invoke('get_chat', { id }).then((v) => {
      if (!v) { loadArchive(); return; }
      const c = newChat(t.url ? t.url.full : '');
      c.sticky = true;
      c.chatId = v.id;
      c.context = `${v.host} · ${dateLabel(v.created_ms)}` + (v.selection ? ` · “${v.selection.length > 120 ? v.selection.slice(0, 120) + '…' : v.selection}”` : '');
      c.messages = v.messages.map((m) => ({ role: m.role, text: m.text }));
      c.log = c.messages.map((m) => ({ role: m.role, text: m.text }));
      c.input = v.input;
      c.output = v.output;
      c.cost = v.cost_micros;
      c.unpriced = v.unpriced;
      chats.set(t.id, c);
      setArchive(false);
      focusInput($('chat-input'));
    }).catch((e) => toast(String(e)));
  }

  // Shows or hides the chat panel (open, and the right panels not hidden).
  function applyChat() {
    const shown = chatShown();
    $('chat').hidden = !shown;
    $('chat').classList.toggle('dock', shown && !narrow);
    if (!shown) $('chat').classList.remove('split-lower');
  }

  function setChat(open) {
    chatOpen = open;
    if (open) rightHidden = false;
    if (!open) { archiveOpen = false; setModelMenu(false); }
    applyChat();
    if (open) {
      loadAi().then(() => { chatKey = ''; renderChat(); });
      focusInput($('chat-input'));
    }
    renderViewMode();
    reportLayout();
  }

  function sendChat() {
    const t = activeTab();
    const input = $('chat-input');
    const text = input.value.trim();
    const choice = chatChoice();
    if (!t || !text || !choice) return;
    const c = chatFor(t);
    if (c.busy || (!t.url && !c.chatId)) return;
    input.value = '';
    c.messages.push({ role: 'user', text });
    c.log.push({ role: 'user', text });
    c.busy = true;
    renderChat();
    T.core.invoke('ai_chat', { tab: t.id, provider: choice.p, model: choice.id, messages: c.messages, chatId: c.chatId })
      .then((r) => {
        c.messages.push({ role: 'assistant', text: r.text });
        if (r.chat_id) c.chatId = r.chat_id;
        c.input += r.input;
        c.output += r.output;
        if (r.cost_micros === null || r.cost_micros === undefined) c.unpriced = true; else c.cost += r.cost_micros;
        const cost = r.cost_micros === null || r.cost_micros === undefined ? 'price unknown' : '≈ ' + money(r.cost_micros);
        c.log.push({ role: 'assistant', text: r.text, meta: `${choice.name} · ${(r.input + r.output).toLocaleString()} tokens · ${cost}` });
        loadUsage();
      })
      .catch((e) => {
        // Not part of the conversation: the next message is sent without it.
        c.messages.pop();
        c.log.push({ role: 'error', text: String(e) });
      })
      .finally(() => { c.busy = false; renderChat(); });
  }

  $('chat-btn').addEventListener('click', () => setChat(!chatOpen));
  $('chat-close').addEventListener('click', () => setChat(false));
  $('chat-new').addEventListener('click', () => {
    const t = activeTab();
    if (t) chats.set(t.id, newChat(t.url ? t.url.full : ''));
    if (archiveOpen) setArchive(false);
    chatKey = '';
    renderChat();
    focusInput($('chat-input'));
  });
  $('chat-archive').addEventListener('click', () => setArchive(!archiveOpen));
  $('chat-form').addEventListener('submit', (ev) => { ev.preventDefault(); sendChat(); });
  $('chat-input').addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter' && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); sendChat(); }
    else if (ev.key === 'Escape') { ev.preventDefault(); setChat(false); }
  });
  document.addEventListener('pointerdown', (ev) => {
    if (!$('chat-model-menu').hidden && !$('chat-model-menu').contains(ev.target) && ev.target !== $('chat-model-btn')) setModelMenu(false);
  });

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
    focusInput(input);
    reportLayout();
  }

  // Focuses an input the chrome opened from a shortcut. After a shortcut a
  // page (or nothing) has keyboard focus, and focusing inside the chrome's
  // web view does nothing until that web view is first responder: ask the
  // host for it, then focus again.
  function focusInput(input, select) {
    const go = () => { input.focus({ preventScroll: true }); if (select) input.select(); };
    go();
    T.core.invoke('focus_chrome').catch(() => {}).finally(() => { go(); requestAnimationFrame(go); });
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

  // A shortcut can arrive twice for one key press (the native menu command
  // and this page's own keydown, depending on what has focus).
  let lastShortcut = { name: '', at: 0 };
  function runShortcut(name) {
    // The shortcut editor's recording, done natively (record:<mods>:<key>).
    if (name.startsWith('record:')) {
      if (!recordingKey) return;
      if (name === 'record:cancel') { stopRecording(''); return; }
      const rest = name.slice(7);
      const i = rest.indexOf(':');
      finishRecording(rest.slice(i + 1), Number(rest.slice(0, i)) || 0);
      return;
    }
    const now = performance.now();
    if (name === lastShortcut.name && now - lastShortcut.at < 60) return;
    lastShortcut = { name, at: now };
    if (name === 'new_tab') {
      openPalette();
    } else if (name === 'close_tab') {
      closeTabShortcut();
    } else if (name === 'toggle_sidebar') {
      toggleSidebar();
    } else if (name === 'toggle_right') {
      toggleRight();
    } else if (name === 'bookmark') {
      bookmarkShortcut();
    } else if (name === 'find') {
      openFind();
    } else if (name === 'toggle_images') {
      toggleMode('hide_media');
    } else if (name.startsWith('item:')) {
      const item = sidebarItems()[Number(name.slice(5)) - 1];
      if (!item) return;
      if (item.tab) invoke('activate_tab', { tab: item.tab.id });
      else openLink(item.link.url);
    }
  }

  // ⌘W: the pop-up sheet in front, else the active tab. A pin's tab is
  // never closed this way; a bookmark's tab is, after confirming, and the
  // bookmark is removed with it (that tab is the bookmark).
  function closeTabShortcut() {
    const id = state && (state.sheet !== null ? state.sheet : state.active);
    if (id === null || id === undefined) return;
    const t = state.tabs.find((x) => x.id === id);
    const key = t && t.sheet_over === null ? t.launcher : null;
    if (key && pins.some((p) => p.url === key)) {
      toast('Pinned tabs stay open. Unpin the site to close its tab.');
      return;
    }
    const bm = key ? bookmarks.find((b) => b.url === key) : null;
    if (!bm) { invoke('close_tab', { tab: id }); return; }
    if (confirmAction && confirmAction.tab === id) { confirmAction.run(); return; } // ⌘W again confirms
    askConfirm(`Close this tab and remove the bookmark “${bm.title || bm.display.registrable}”?`, 'Remove Bookmark', id, () =>
      T.core.invoke('remove_bookmark', { url: bm.url })
        .then(loadBookmarks)
        .then(() => invoke('close_tab', { tab: id }))
        .catch((e) => toast(String(e))));
  }

  // A small confirmation floating above the pill (a hole, like the pill).
  // ⌘W or Return confirms, Escape cancels.
  function askConfirm(text, ok, tab, run) {
    confirmAction = { tab, run: () => { closeConfirm(); run(); } };
    $('confirm-text').textContent = text;
    $('confirm-ok').textContent = ok + ' (⌘W)';
    $('confirm-pop').hidden = false;
    $('confirm-ok').focus();
    syncMenuOpen();
    reportLayout();
  }
  function closeConfirm() {
    confirmAction = null;
    $('confirm-pop').hidden = true;
    syncMenuOpen();
    reportLayout();
  }
  $('confirm-ok').addEventListener('click', () => { if (confirmAction) confirmAction.run(); });
  $('confirm-cancel').addEventListener('click', closeConfirm);

  // ⌘D (and the ⋯ menu's Bookmark row): bookmarks the page, moving this
  // tab into the bookmark; on a bookmark's own tab, removes the bookmark
  // and the tab becomes an ordinary tab again.
  const BOOKMARK_RESULT = {
    added: 'Bookmarked',
    removed: 'Bookmark removed',
    already: 'Already bookmarked',
    pinned: 'This is a pinned site’s tab.',
    none: 'Nothing to bookmark yet.',
  };
  function toggleBookmark(t) {
    if (!t || !t.url) { toast('Open a page to bookmark it.'); return; }
    if (t.private) { toast('Private tabs can’t be bookmarked.'); return; }
    T.core.invoke('toggle_bookmark', { tab: t.id })
      .then((r) => { toast(BOOKMARK_RESULT[r] || String(r)); return loadBookmarks(); })
      .then(render)
      .catch((e) => toast(String(e)));
  }
  function bookmarkShortcut() { toggleBookmark(activeTab()); }

  // ⌘⇧\: hide the right panels (keeping them on), or show them again; with
  // none on, open the chat.
  function toggleRight() {
    const anyOn = scriptsOpen || chatOpen || (state && headersOn(state.active));
    if (!anyOn) { setChat(true); return; }
    if (!rightHidden && scriptsOpen) { setScripts(false); if (!chatOpen && !(state && headersOn(state.active))) return; }
    rightHidden = !rightHidden;
    applyChat();
    renderOutline();
    renderViewMode();
    reportLayout();
  }

  // ---- find on page (⌘F) ------------------------------------------------------
  //
  // WebKit's own find selects and scrolls to each match; a separate count
  // (digits only) gives "3 of 12". The bar sits on top of the pill.
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

  // A new query: count, then select the first match.
  function findQuery(q) {
    const t = activeTab();
    find.query = q;
    find.total = null;
    find.index = 0;
    const seq = ++find.seq;
    renderFind();
    if (!q || !t || !t.url) { if (q) { find.total = 0; renderFind(); } return; }
    find.tab = t.id;
    T.core.invoke('count_in_page', { tab: t.id, query: q }).then((n) => {
      if (seq !== find.seq) return;
      find.total = n === null || n === undefined ? find.total : n;
      return T.core.invoke('find_in_page', { tab: t.id, query: q, backwards: false }).then((found) => {
        if (seq !== find.seq) return;
        if (!found) find.total = 0;
        else { find.index = 1; if (!find.total) find.total = 1; }
        renderFind();
        $('find-input').focus(); // WebKit may move focus to the page
      });
    }).catch((e) => toast(String(e)));
  }

  function findStep(backwards) {
    const t = activeTab();
    if (!t || !find.query || !find.total) return;
    const seq = find.seq;
    T.core.invoke('find_in_page', { tab: t.id, query: find.query, backwards }).then((found) => {
      if (seq !== find.seq || !found) return;
      const n = find.total;
      find.index = backwards ? (find.index <= 1 ? n : find.index - 1) : (find.index >= n ? 1 : find.index + 1);
      renderFind();
      $('find-input').focus();
    }).catch((e) => toast(String(e)));
  }

  $('find-input').addEventListener('input', () => {
    clearTimeout(find.timer);
    const q = $('find-input').value.trim();
    find.timer = setTimeout(() => findQuery(q), 180);
  });
  $('find-input').addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') { ev.preventDefault(); findStep(ev.shiftKey); }
    else if (ev.key === 'Escape') { ev.preventDefault(); closeFind(); }
    else if (ev.metaKey && ev.key.toLowerCase() === 'g') { ev.preventDefault(); findStep(ev.shiftKey); }
  });
  $('find-prev').addEventListener('click', () => findStep(true));
  $('find-next').addEventListener('click', () => findStep(false));
  $('find-close').addEventListener('click', closeFind);

  document.addEventListener('keydown', (ev) => {
    if (!ev.metaKey || recordingKey) return;
    const pressed = keyOf(ev);
    const hit = pressed && shortcutList.find((x) => x.key === pressed.key && x.mods === pressed.mods);
    const name = hit ? hit.action : null;
    if (!name) return;
    ev.preventDefault();
    runShortcut(name);
  });

  // ---- keyboard shortcuts (Settings → Keyboard) ------------------------------
  //
  // The bindings live natively (the menu bar's key commands); the chrome
  // keeps the user's changes (localStorage) and sends them at launch. The
  // keydown handler above uses the same list for when the chrome has focus.

  const MOD_SHIFT = 1;
  const MOD_ALT = 2;
  const MOD_CTRL = 4;
  const CODE_KEYS = {
    BracketLeft: '[', BracketRight: ']', Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.',
    Slash: '/', Minus: '-', Equal: '=', Backquote: '`',
  };
  // When `code` is missing: the unshifted key of a shifted character (US).
  const SHIFTED = {
    '|': '\\', '{': '[', '}': ']', ':': ';', '"': "'", '<': ',', '>': '.', '?': '/', '_': '-', '+': '=', '~': '`',
    '!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
  };
  let shortcutList = [];
  let recordingKey = null; // id of the shortcut being rebound

  // The unshifted key of a ⌘ key press (by physical key, so ⇧ does not turn
  // \ into |), and its other modifiers.
  function keyOf(ev) {
    let key = null;
    if (/^Key[A-Z]$/.test(ev.code)) key = ev.code.slice(3).toLowerCase();
    else if (/^Digit[0-9]$/.test(ev.code)) key = ev.code.slice(5);
    else if (CODE_KEYS[ev.code]) key = CODE_KEYS[ev.code];
    else if (ev.key && ev.key.length === 1) key = SHIFTED[ev.key] || ev.key.toLowerCase();
    if (!key) return null;
    const mods = (ev.shiftKey ? MOD_SHIFT : 0) | (ev.altKey ? MOD_ALT : 0) | (ev.ctrlKey ? MOD_CTRL : 0);
    return { key, mods };
  }

  function comboLabel(key, mods) {
    return (mods & MOD_CTRL ? '⌃' : '') + (mods & MOD_ALT ? '⌥' : '') + (mods & MOD_SHIFT ? '⇧' : '') + '⌘' + (key === ' ' ? 'Space' : key.toUpperCase());
  }

  function savedBindings() {
    try { return JSON.parse(pref('shortcuts') || '[]'); } catch (e) { return []; }
  }

  // Sends bindings to the host; on success they are saved.
  function applyBindings(bindings, suspend) {
    return T.core.invoke('set_shortcuts', { bindings, suspend }).then((list) => {
      shortcutList = list;
      if (!suspend) setPref('shortcuts', JSON.stringify(bindings));
      return list;
    });
  }

  function renderKeys() {
    const list = $('keys-list');
    list.replaceChildren();
    for (const sc of shortcutList) {
      const li = el('li');
      li.appendChild(el('span', 'keys-title', sc.title));
      const changed = sc.key !== sc.default_key || sc.mods !== sc.default_mods;
      if (changed) li.appendChild(el('span', 'keys-default muted small', 'default ' + comboLabel(sc.default_key, sc.default_mods)));
      const b = el('button', 'keys-combo' + (recordingKey === sc.id ? ' recording' : ''), recordingKey === sc.id ? 'Press keys…' : comboLabel(sc.key, sc.mods));
      b.setAttribute('aria-label', `${sc.title}: ${comboLabel(sc.key, sc.mods)}. Change`);
      b.addEventListener('click', () => startRecording(sc.id));
      li.appendChild(b);
      list.appendChild(li);
    }
  }

  function keysMsg(t) { $('keys-msg').textContent = t || ''; }

  // While recording, the menu bar's shortcuts are off, so the keys reach
  // the chrome instead of running a command.
  function startRecording(id) {
    recordingKey = id;
    keysMsg('');
    renderKeys();
    applyBindings(savedBindings(), true).catch(() => {});
  }
  function stopRecording(msg) {
    recordingKey = null;
    applyBindings(savedBindings(), false).catch(() => {}).finally(renderKeys);
    keysMsg(msg);
  }

  document.addEventListener('keydown', (ev) => {
    if (!recordingKey) return;
    ev.preventDefault();
    ev.stopImmediatePropagation();
    if (ev.key === 'Escape') { stopRecording(''); return; }
    if (['Meta', 'Shift', 'Alt', 'Control', 'CapsLock'].includes(ev.key)) return;
    if (!ev.metaKey) { keysMsg('Hold ⌘ with the key.'); return; }
    const pressed = keyOf(ev);
    if (!pressed) { keysMsg('That key can’t be used.'); return; }
    finishRecording(pressed.key, pressed.mods);
  }, true);

  // Saves the recorded combination for the shortcut being changed (from the
  // native recording, or this page's keydown where ⌘ keys reach it).
  function finishRecording(key, mods) {
    const id = recordingKey;
    if (!id) return;
    const next = savedBindings().filter((b) => b.id !== id).concat([{ id, key, mods }]);
    T.core.invoke('set_shortcuts', { bindings: next, suspend: false })
      .then((list) => {
        shortcutList = list;
        setPref('shortcuts', JSON.stringify(next));
        recordingKey = null;
        renderKeys();
        keysMsg(`Saved: ${comboLabel(key, mods)}.`);
      })
      .catch((e) => {
        keysMsg(String(e));
        // Still recording: let the host offer the combinations again.
        applyBindings(savedBindings(), true).catch(() => {});
      });
  }

  $('keys-reset').addEventListener('click', () => {
    recordingKey = null;
    setPref('shortcuts', '[]');
    applyBindings([], false).then(() => { renderKeys(); keysMsg('Defaults restored.'); }).catch((e) => keysMsg(String(e)));
  });

  // At launch: the saved bindings, or the defaults if they no longer apply.
  applyBindings(savedBindings(), false)
    .catch(() => applyBindings([], false))
    .catch(() => T.core.invoke('get_shortcuts').then((l) => { shortcutList = l; }))
    .catch(() => {});

  // ---- selected text on a page -----------------------------------------------
  //
  // A watcher in the page reports where the selection is (C19); a small
  // menu floats above it (cut out of the page, like the pill). Its text is
  // read when the menu appears (C20) and used only by the chosen action.

  let selection = null; // { tab, text }

  function paneBox(tab) {
    const b = contentBox;
    const w = window.innerWidth - b.left - b.right;
    if (state && state.split && (state.split.left === tab || state.split.right === tab)) {
      const half = Math.floor((w - 2) / 2);
      return { x: state.split.left === tab ? b.left : b.left + w - half, y: b.top, w: half };
    }
    return { x: b.left, y: b.top, w };
  }

  function hideSelection() {
    if (!selection && $('sel-pop').hidden) return;
    selection = null;
    $('sel-pop').hidden = true;
    reportLayout();
  }

  function showSelection(tab, r) {
    const pane = paneBox(tab);
    const k = pane.w / r.vw;
    const sx = pane.x + r.x * k;
    const sy = pane.y + r.y * k;
    const sw = r.w * k;
    const sh = r.h * k;
    const pop = $('sel-pop');
    pop.hidden = false;
    const pw = pop.offsetWidth;
    const ph = pop.offsetHeight;
    let top = sy - ph - 10;
    if (top < pane.y + 6) top = sy + sh + 10;
    const left = Math.max(pane.x + 6, Math.min(sx + sw / 2 - pw / 2, pane.x + pane.w - pw - 6));
    pop.style.left = Math.round(left) + 'px';
    pop.style.top = Math.round(Math.min(top, window.innerHeight - ph - 70)) + 'px';
    reportLayout();
  }

  T.event.listen('selection', (e) => {
    const { tab, rect } = e.payload;
    const shown = state && (tab === state.active || (state.split && (tab === state.split.left || tab === state.split.right)));
    if (!rect || !shown || paletteOpen || modalOpen || panel) { hideSelection(); return; }
    T.core.invoke('read_selection', { tab }).then((text) => {
      if (!text) { hideSelection(); return; }
      selection = { tab, text };
      const ask = useChoice('ask');
      $('sel-ask').textContent = ask ? `Ask ${ask.name}` : 'Ask AI';
      showSelection(tab, rect);
    }).catch(() => hideSelection());
  });

  $('sel-google').addEventListener('click', () => {
    if (!selection) return;
    const q = selection.text.replace(/\s+/g, ' ').trim().slice(0, 300);
    hideSelection();
    invoke('search_google', { query: q, private: !!(activeTab() && activeTab().private) });
  });
  $('sel-ask').addEventListener('click', () => {
    if (!selection) return;
    const quote = selection.text.length > 1500 ? selection.text.slice(0, 1500) + '…' : selection.text;
    hideSelection();
    setChat(true);
    const input = $('chat-input');
    input.value = `“${quote}”\n\n`;
    focusInput(input);
    requestAnimationFrame(() => { input.selectionStart = input.selectionEnd = input.value.length; });
  });
  $('sel-define').addEventListener('click', () => {
    if (!selection) return;
    const r = $('sel-pop').getBoundingClientRect();
    const term = selection.text.replace(/\s+/g, ' ').trim().slice(0, 100);
    hideSelection();
    invoke('define_term', { term, x: r.left, y: r.top, w: r.width, h: r.height });
  });

  // Share: the system share sheet for this page, pointing at the button.
  $('share').addEventListener('click', withTab((t) => {
    if (!t.url) { toast('Open a page to share it.'); return; }
    const r = $('share').getBoundingClientRect();
    invoke('share_page', { tab: t.id, x: r.left, y: r.top, w: r.width, h: r.height });
  }));

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
  $('scripts-btn').addEventListener('click', () => { setActions(false); setScripts(!scriptsOpen); });
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
    if (!$('actions-pop').hidden && !$('actions').contains(ev.target)) setActions(false);
    // Taps outside close the overlaid sidebar (narrow) and the flyout.
    if ((narrow || flyoutMode()) && sidebarShown && !$('sidebar').contains(ev.target)) setSidebar(false);
    if (confirmAction && !$('confirm-pop').contains(ev.target)) closeConfirm();
  });
  document.addEventListener('keydown', (ev) => {
    if (ev.key !== 'Escape' || editing) return;
    if (paletteOpen) closePalette();
    else if (confirmAction) closeConfirm();
    else if (findOpen) closeFind();
    else if (flyoutMode() && sidebarShown) setSidebar(false);
    else if (!$('actions-pop').hidden) setActions(false);
    else if (menuOpen) closeMenu();
    else if (modalOpen) setModal(false);
    else if (scriptsOpen) setScripts(false);
    else if (chatOpen) setChat(false);
    else if (panel) closePanel();
  });
  $('shield').addEventListener('click', withTab((t) => {
    setActions(false);
    const a = t.adblock;
    if (!a || !a.can_toggle) return;
    T.core.invoke('toggle_site_adblock', { tab: t.id })
      .then((blocking) => toast(blocking ? `Blocking ads on ${a.site} again. Reloading…` : `Ads allowed on ${a.site}. Reloading…`))
      .catch((e) => toast(String(e)));
  }));
  $('bookmark').addEventListener('click', withTab((t) => { setActions(false); toggleBookmark(t); }));
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
      loadSidebarPrefs();
    }
    setSidebarWidth(parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sidebar-w')) || 264);
    applyChat();
    $('scripts-pop').classList.toggle('dock', scriptsOpen && !narrow);
    applySidebar();
  });
  const savedWidth = parseFloat(pref('sidebarWidth') || '');
  if (savedWidth) setSidebarWidth(savedWidth);
  renderSidebarMode();
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
  T.event.listen('heading_current', (e) => {
    currentHeading.set(e.payload.tab, e.payload.index);
    if (state && e.payload.tab === state.active) markCurrentHeading(e.payload.tab);
  });
  T.event.listen('headings', (e) => {
    headingsByTab.set(e.payload.tab, e.payload.headings);
    if (state && e.payload.tab === state.active) renderOutline();
  });
})();
