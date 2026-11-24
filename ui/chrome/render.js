// Browser chrome (see core.js): Rendering state from Rust: tabs, the toolbar, the surface, notices.
'use strict';

// ---- rendering -------------------------------------------------------

function render() {
  if (!state) return;
  const started = performance.now();
  const tab = activeTab();
  barAttached = !!(tab && tab.url_bar_attached);
  document.body.classList.toggle('bar-attached', barAttached);
  renderAttachBar();
  const wasDark = chromeDark();
  document.body.classList.toggle('private', !!(tab && tab.private));
  // Private chrome is always dark: icons switch to their dark copies.
  if (chromeDark() !== wasDark) iconsThemeChanged();
  renderOs();
  renderResearch();
  renderTabs();
  renderLauncherSelection();
  renderLaterSelection();
  renderViewMode();
  renderToolbar(tab);
  renderSurface(tab);
  renderChat();
  // Find and confirmations belong to the tab they were opened on.
  if (findOpen && find.tab !== null && find.tab !== state.active) closeFind();
  if (selection && selection.tab !== state.active && !(state.split && (selection.tab === state.split.left || selection.tab === state.split.right))) hideSelection();
  if (confirmAction && confirmAction.tab !== null && confirmAction.tab !== state.active && confirmAction.tab !== state.sheet) closeConfirm();
  if (confirmAction && confirmAction.anchor) placeConfirm(); // its row may have been drawn again
  notebookTrack();
  syncPreview();
  if (capture) renderCapture();
  reportLayout();
  // Timing (Menu → Console, 2026-11-23): slow renders only.
  const ms = performance.now() - started;
  if (ms >= 4) chromeLog('debug', `timing: chrome render took ${ms.toFixed(1)} ms (${state.tabs.length} tabs)`);
}

function renderOs() {
  const os = state.os;
  const banner = $('os-banner');
  const block = $('unsupported');
  banner.hidden = os.status !== 'update_recommended' && os.status !== 'preview_only';
  if (os.status === 'preview_only') {
    banner.textContent = 'Desktop preview: only the browser chrome runs here. Web pages render on iPadOS and macOS only (npm run tauri dev on a Mac; npm run tauri ios dev for the iPad).';
  }
  block.hidden = os.status !== 'unsupported';
  const v = (x) => `${x.major}.${x.minor}.${x.patch}`;
  if (macOS) {
    // macOS: WebKit is updated with Safari (Software Update), on older
    // macOS versions too.
    if (os.status === 'update_recommended') {
      banner.textContent = `Safari ${v(os.current)} is missing security fixes for the web engine. Update Safari to ${v(os.recommended)} or later in System Settings → General → Software Update.`;
    } else if (os.status === 'unsupported') {
      $('unsupported-title').textContent = 'Update Safari to browse';
      $('unsupported-body').textContent = `This version of Safari's web engine (${v(os.current)}) no longer receives the security fixes Snorkle depends on. Safari ${v(os.minimum)} or later is required.`;
    }
    return;
  }
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

// Tabs listed in the sidebar now: not popup sheets, pins' or bookmarks'
// tabs or saved copies, and of the session shown (everyday or research).
function listedHere(t) {
  return t.sheet_over === null && t.launcher === null && !t.saved && (t.space || null) === (state.space || null);
}

// Tabs that can join a same-site group: listed, not in a split pair or
// in a tab group of the user's.
function groupable(t) {
  return listedHere(t) && !pairOf(t.id) && (t.group === null || t.group === undefined);
}

// Whether the chrome is dark (Settings → Appearance, the system's, or a
// private tab's chrome): favicons too dark to see there show their light
// copy (2026-11-24, made when the icon was fetched).
function chromeDark() {
  if (document.body && document.body.classList.contains('private')) return true;
  const theme = document.documentElement.dataset.theme;
  if (theme) return theme === 'dark';
  return matchMedia('(prefers-color-scheme: dark)').matches;
}
function iconsThemeChanged() {
  tabsKey = '';
  if (state) renderTabs();
  renderBookmarks();
  renderPins();
}
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', iconsThemeChanged);

// The split pair holding a tab, if any.
function pairOf(id) {
  return (state.splits || []).find((p) => p.left === id || p.right === id) || null;
}

// Tab favicons (2026-11-04): Rust says which favicon host each tab is
// on and sends icons this chrome has not seen yet (`tab_icons`), fetching
// missing ones for non-private tabs; `icons` events say one arrived.
// Cached here by host for the session.
const tabIcons = { hosts: new Map(), bookmarks: new Map(), icons: new Map(), dark: new Map(), version: 0 };
let tabIconsKey = '';
let tabIconsTimer = 0;
function refreshTabIcons(force) {
  const key = (state ? state.tabs.map((t) => `${t.id}=${t.url ? t.url.full : ''}`).join('|') : '') + '#' + bookmarks.map((b) => b.url).join('|');
  if (!force && key === tabIconsKey) return;
  tabIconsKey = key;
  clearTimeout(tabIconsTimer);
  tabIconsTimer = setTimeout(() => {
    T.core.invoke('tab_icons', { have: [...tabIcons.icons.keys()] }).then((r) => {
      tabIcons.hosts = new Map(Object.entries(r.tabs).map(([id, h]) => [Number(id), h]));
      tabIcons.bookmarks = new Map(Object.entries(r.bookmarks || {}));
      const bmBefore = bookmarkIconsKey();
      for (const [h, url] of Object.entries(r.icons)) {
        if (typeof url === 'string' && url.startsWith('data:image/png;base64,')) tabIcons.icons.set(h, url);
      }
      for (const [h, url] of Object.entries(r.dark || {})) {
        if (typeof url === 'string' && url.startsWith('data:image/png;base64,')) tabIcons.dark.set(h, url);
      }
      tabIcons.version++;
      if (state) renderTabs();
      if (bookmarkIconsKey() !== bmBefore || Object.keys(r.icons).length) renderBookmarks();
    }).catch(() => {});
  }, 250);
}

function bookmarkIconsKey() {
  return bookmarks.map((b) => (tabIcons.icons.has(tabIcons.bookmarks.get(b.url)) ? 1 : 0)).join('');
}

// A row's icon (tabs and bookmarks): a bundled one for some Google sites
// and YouTube, the site's favicon, else its letter (an empty square for
// a new tab). `display` is the row's DisplayUrl, `url` its address.
function rowIcon(url, display, host) {
  const prefab = url && prefabIcon(url);
  const fav = host && ((chromeDark() && tabIcons.dark.get(host)) || tabIcons.icons.get(host));
  const fallback = () => {
    if (!display) return el('span', 'tab-icon');
    const l = letterTile(display);
    l.classList.add('tab-icon');
    return l;
  };
  if (prefab || fav) {
    const img = el('img', 'tab-icon' + (prefab ? ' prefab' : ''));
    img.alt = '';
    img.addEventListener('error', () => img.replaceWith(fallback()));
    img.src = prefab || fav;
    return img;
  }
  return fallback();
}
function tabIcon(t) {
  if (t.emoji) return emojiIcon(t.emoji);
  return rowIcon(t.url && t.url.full, t.url, tabIcons.hosts.get(t.id));
}
// The user's emoji (the Edit dialog) in place of a row's favicon.
function emojiIcon(e) {
  return el('span', 'tab-icon emoji-icon', e);
}

// The selected row alone changed (a tab switch, 2026-11-23): move the
// highlight instead of building the list again.
let tabsSelected = null;
function renderTabSelection() {
  const sel = shownTab();
  if (sel === tabsSelected) return;
  tabsSelected = sel;
  document.querySelectorAll('#tabs .tab, #private-tabs .tab').forEach((node) => {
    const on = Number(node.dataset.tab) === sel;
    node.classList.toggle('active', on);
    node.setAttribute('aria-selected', String(on));
  });
  document.querySelectorAll('#tabs .tab-group, #private-tabs .tab-group').forEach((g) => g.classList.toggle('active', !!g.querySelector('.tab.active')));
  const act = $('sidebar').querySelector('.tab.active');
  if (act && !dragState) act.scrollIntoView({ block: 'nearest' });
}

function renderTabs() {
  refreshTabIcons(false);
  const key = JSON.stringify([
    state.tabs.map((t) => [t.id, !!t.saved, t.title, t.name, t.emoji, t.private, t.live, t.url && t.url.registrable, !!t.url && conversationSite(t), t.sheet_over, t.launcher, tabIcons.hosts.get(t.id), t.space, t.group]),
    state.splits,
    state.groups,
    state.space,
    tabIcons.version,
    chromeDark(),
  ]);
  if (key === tabsKey) { renderTabSelection(); return; }
  tabsKey = key;
  tabsSelected = shownTab();
  const normal = $('tabs');
  const priv = $('private-tabs');
  normal.replaceChildren();
  priv.replaceChildren();
  const byId = new Map(state.tabs.map((t) => [t.id, t]));
  const placed = new Set();
  for (const t of state.tabs) {
    // Pop-up sheets are not tabs (their opener is shown as selected),
    // tabs that belong to a pin or bookmark are shown as that item, and a
    // saved copy's tab only as its card in Save for Later.
    if (placed.has(t.id) || !listedHere(t)) continue;
    const pair = pairOf(t.id);
    const list = t.private ? priv : normal;
    // The user's tab groups (2026-11-24): a heading and their tabs.
    const g = t.group !== null && t.group !== undefined && (state.groups || []).find((x) => x.id === t.group);
    if (g) {
      const members = state.tabs.filter((o) => o.group === g.id && listedHere(o));
      list.appendChild(tabGroupNode(g, members, byId, placed));
      continue;
    }
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
      // collected under a separate domain heading.
      const site = siteOf(t);
      const members = site ? state.tabs.filter((o) => groupable(o) && o.private === t.private && siteOf(o) === site) : [t];
      if (members.length < 2) {
        list.appendChild(tabNode(t, null));
        placed.add(t.id);
        continue;
      }
      const key = (t.private ? 'p:' : 'n:') + site;
      const collapsed = collapsedSites.has(key);
      const box = el('div', 'site-group' + (collapsed ? ' collapsed' : ''));
      box.setAttribute('role', 'group');
      box.setAttribute('aria-label', `${members.length} tabs on ${site}`);
      const head = el('div', 'site-heading');
      const toggle = el('button', 'site-toggle', site);
      toggle.setAttribute('aria-expanded', String(!collapsed));
      toggle.setAttribute('aria-label', `${collapsed ? 'Show' : 'Hide'} ${members.length} tabs on ${site}`);
      toggle.addEventListener('click', () => {
        if (collapsedSites.has(key)) collapsedSites.delete(key);
        else collapsedSites.add(key);
        tabsKey = '';
        renderTabs();
      });
      head.dataset.site = key;
      const close = el('button', 'close', '×');
      close.setAttribute('aria-label', `Close all ${members.length} tabs on ${site}`);
      close.addEventListener('click', () => {
        const ids = members.map((m) => m.id);
        askConfirm(`Close ${ids.length} tabs on ${site}?`, 'Close tabs', null, async () => {
          for (const tab of ids) await invoke('close_tab', { tab });
        }, null, { anchor: () => document.querySelector(`.site-heading[data-site="${CSS.escape(key)}"]`) });
      });
      const acts = el('span', 'row-actions');
      acts.appendChild(close);
      head.append(toggle, acts);
      box.appendChild(head);
      members.forEach((m) => {
        const node = tabNode(m, null);
        node.classList.add('child');
        box.appendChild(node);
        placed.add(m.id);
      });
      list.appendChild(box);
    }
  }
  $('private-head').hidden = !priv.childElementCount;
  const act = $('sidebar').querySelector('.tab.active');
  if (act && !dragState) act.scrollIntoView({ block: 'nearest' });
  afterRender();
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
  node.appendChild(tabIcon(t));
  // One line: the page's title (its site until it has one).
  const open = el('button', 'open');
  open.appendChild(el('span', 't', t.name || t.title || (t.url ? t.url.registrable : t.private ? 'Private Tab' : 'New Tab')));
  if (t.url) open.title = t.url.registrable;
  open.addEventListener('click', (ev) => {
    if (suppressClick) return;
    // ⌘- and ⇧-click choose tabs (2026-11-24), for the menu's group,
    // Remove and Send to.
    if (chooseClick(ev, node)) return;
    invoke('activate_tab', { tab: t.id });
    if (narrow) setSidebar(false);
  });
  node.appendChild(open);
  const acts = el('span', 'row-actions');
  // Save for Later: web pages of ordinary tabs (private pages are never
  // written).
  if (t.url && !t.private) {
    const later = el('button', 'later');
    const chat = conversationSite(t);
    later.appendChild(svgIcon(chat ? TRAY_CHAT : TRAY_IN));
    later.setAttribute('aria-label', chat ? 'Save this conversation for later' : 'Save for later');
    later.title = chat ? 'Save this conversation for later (scroll through it to capture it)' : 'Save for later';
    later.addEventListener('click', (ev) => { ev.stopPropagation(); saveForLater(t.id); });
    acts.appendChild(later);
  }
  const close = el('button', 'close', '×');
  close.setAttribute('aria-label', 'Close tab');
  close.addEventListener('click', (ev) => { ev.stopPropagation(); invoke('close_tab', { tab: t.id }); });
  acts.appendChild(close);
  node.appendChild(acts);
  node.addEventListener('pointerdown', (ev) => armDrag(ev, node, { kind: 'tab', id: t.id, private: t.private, paired: !!pair }));
  // Right-click: the item menu (Copy URL, Edit…; groups, Remove, Send to),
  // for every chosen tab when this is one of them.
  node.addEventListener('contextmenu', (ev) => {
    ev.preventDefault();
    cancelPending();
    const keys = menuTargets('tab', t.id);
    openPinPop({ url: t.url ? t.url.full : '', id: t.id }, node, 'tab');
    fillItemMore('tab', keys);
    placePinPop(node);
  });
  return node;
}

function renderToolbar(tab) {
  $('back').disabled = !(tab && tab.can_go_back);
  $('forward').disabled = !(tab && tab.can_go_forward);
  const reload = $('reload');
  reload.textContent = '↻';
  reload.setAttribute('aria-label', 'Reload');
  $('bookmark').disabled = !(tab && tab.url && !tab.private);
  const marked = !!(tab && ((tab.launcher && bookmarks.some((b) => b.url === tab.launcher)) || (tab.url && bookmarks.some((b) => b.url === tab.url.full))));
  // A pin's tab: ⌘D bookmarks the pin; a bookmark made from it is not
  // removed from there.
  const pinTab = !!(tab && tab.launcher && pins.some((p) => p.url === tab.launcher));
  $('bookmark').querySelector('.label').textContent = pinTab ? (marked ? 'Pin Bookmarked' : 'Bookmark Pin') : marked ? 'Remove Bookmark' : 'Bookmark';
  $('bookmark').querySelector('.state').textContent = marked ? '\u2605' : '\u2606';
  $('block-site').disabled = !(tab && tab.url && tab.adblock);
  $('open-in-app').disabled = !(tab && tab.url);
  $('unsplit').hidden = !state.split;
  $('sheet-close').hidden = !state.sheet;
  $('sheet-pill').hidden = !state.sheet;
  renderShield(tab);
  renderScriptsMark(tab);
  renderZoom(tab);
  $('private-pill').hidden = !(tab && tab.private);
  // A pin's or bookmark's saved copy, shown because the network failed.
  const saved = $('saved-pill');
  saved.hidden = !(tab && tab.saved);
  if (!saved.hidden) saved.title = `Saved for later on ${new Date(tab.saved.saved_ms).toLocaleString()}; works offline. Links to pages it holds open from it.`;
  renderPickBar(tab);
  const offline = $('offline-pill');
  offline.hidden = !(tab && tab.offline_saved_ms);
  if (!offline.hidden) offline.title = `No connection: this is the copy saved ${new Date(tab.offline_saved_ms).toLocaleString()}. Scripts are off. Reload to try the live page.`;

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
  if (!d) $('url-reg').textContent = 'search / url';

  renderProgress(tab);
}

// The load bar along the pill. Also on its own: Rust sends a tab's
// progress without the rest of the state (`progress` events, 2026-11-23).
function renderProgress(tab) {
  const p = tab && tab.loading ? Math.max(5, tab.progress) / 100 : 0;
  $('progress-bar').style.transform = `scaleX(${p})`;
}

function renderSurface(tab) {
  const notice = tab && tab.notice;
  const showPanel = !!panel;
  $('downloads').hidden = panel !== 'downloads';
  $('notice').hidden = showPanel || !notice;
  $('start').hidden = showPanel || !!notice || !tab || !!tab.url;
  if (notice && !showPanel) renderNotice(tab, notice);
  if (!$('start').hidden) renderStart(tab);
  else startTab = null;
  startShown = !$('start').hidden;
  renderDefine();
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
  $('notice').classList.toggle('user-blocked', n.kind === 'user_blocked');
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
      primary.textContent = 'Open App';
      primary.onclick = () => invoke('confirm_external', { tab: tab.id });
      secondary.textContent = 'Stay in Browser';
      secondary.onclick = () => invoke('dismiss_notice', { tab: tab.id });
      break;
    case 'user_blocked':
      // Settings → Blocked: the user's own message, in the middle of the tab.
      title.textContent = n.message;
      body.textContent = '';
      detail.textContent = n.url;
      primary.hidden = true;
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
