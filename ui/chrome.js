// Browser chrome. Trusted, bundled, local. Renders state pushed from Rust
// and sends user intents back as Tauri commands.
//
// Security rules for this file (see docs/ARCHITECTURE.md):
// - Never use innerHTML/insertAdjacentHTML/document.write; build DOM with
//   createElement + textContent only. Page-controlled strings (titles,
//   hosts) must never be interpreted as markup.
// - The address bar shows only what Rust's DisplayUrl provides.
// - Never load remote resources (CSP also forbids it).
'use strict';

(() => {
  const T = window.__TAURI__;
  const invoke = (cmd, args) => T.core.invoke(cmd, args).catch((e) => toast(String(e)));
  const $ = (id) => document.getElementById(id);

  let state = null;
  let editing = false;
  let panel = null; // null | 'bookmarks' | 'history'
  let tabsKey = '';
  let toastTimer = 0;
  let clearArmed = false;

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
    renderToolbar(tab);
    renderSurface(tab);
    reportLayout();
  }

  function renderOs() {
    const os = state.os;
    const banner = $('os-banner');
    const block = $('unsupported');
    banner.hidden = os.status !== 'update_recommended';
    block.hidden = os.status !== 'unsupported';
    const v = (x) => `${x.major}.${x.minor}.${x.patch}`;
    if (os.status === 'update_recommended') {
      banner.textContent = `iPadOS ${v(os.current)} is missing security fixes for the web engine. Update to ${v(os.recommended)} or later.`;
    } else if (os.status === 'unsupported') {
      $('unsupported-body').textContent = `This version of iPadOS (${v(os.current)}) no longer receives the web-engine security fixes Snorkle depends on. iPadOS ${v(os.minimum)} or later is required.`;
    }
  }

  function renderTabs() {
    const key = JSON.stringify(state.tabs.map((t) => [t.id, t.title, t.private, t.live, t.url && t.url.registrable]).concat([state.active]));
    if (key === tabsKey) return;
    tabsKey = key;
    const strip = $('tabs');
    strip.replaceChildren();
    for (const t of state.tabs) {
      const node = el('div', 'tab' + (t.id === state.active ? ' active' : '') + (t.private ? ' private' : '') + (t.live ? '' : ' discarded'));
      node.setAttribute('role', 'tab');
      node.setAttribute('aria-selected', String(t.id === state.active));
      const label = t.title || (t.url ? t.url.registrable : (t.private ? 'Private Tab' : 'New Tab'));
      const title = el('button', 'title', label);
      title.addEventListener('click', () => invoke('activate_tab', { tab: t.id }));
      const close = el('button', 'close', '×');
      close.setAttribute('aria-label', 'Close tab');
      close.addEventListener('click', (ev) => { ev.stopPropagation(); invoke('close_tab', { tab: t.id }); });
      node.append(title, close);
      strip.appendChild(node);
    }
    const act = strip.querySelector('.tab.active');
    if (act) act.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  function renderToolbar(tab) {
    $('back').disabled = !(tab && tab.can_go_back);
    $('forward').disabled = !(tab && tab.can_go_forward);
    const reload = $('reload');
    reload.textContent = tab && tab.loading ? '×' : '↻';
    reload.setAttribute('aria-label', tab && tab.loading ? 'Stop' : 'Reload');
    $('bookmark').disabled = !(tab && tab.url && !tab.private);
    $('private-pill').hidden = !(tab && tab.private);

    const d = tab && tab.url;
    const sec = $('security');
    sec.className = 'security';
    const security = tab ? tab.security : 'none';
    if (security === 'not_secure') { sec.textContent = 'Not Secure'; sec.classList.add('not-secure'); }
    else if (security === 'mixed') { sec.textContent = 'Not Fully Secure'; sec.classList.add('mixed'); }
    else if (security === 'secure') { sec.textContent = '\u{1F512}'; }
    else { sec.textContent = ''; }
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

  let startKey = '';
  function renderStart(tab) {
    $('start-title').textContent = tab && tab.private ? 'New Private Tab' : 'New Tab';
    $('start-private').hidden = !(tab && tab.private);
    const key = String(tab && tab.id);
    if (key === startKey) return;
    startKey = key;
    T.core.invoke('list_bookmarks').then((items) => fillList($('start-bookmarks'), items, null));
  }

  function fillList(list, items, onRemove) {
    list.replaceChildren();
    if (!items.length) {
      list.appendChild(el('li', 'muted', 'Nothing here yet.'));
      return;
    }
    for (const it of items) {
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
      if (onRemove) {
        const rm = el('button', 'remove', '×');
        rm.setAttribute('aria-label', 'Remove');
        rm.addEventListener('click', () => onRemove(it));
        li.appendChild(rm);
      }
      list.appendChild(li);
    }
  }

  // ---- panels (bookmarks / history) ----------------------------------

  function openPanel(which) {
    panel = which;
    clearArmed = false;
    $('panel-bookmarks').classList.toggle('on', which === 'bookmarks');
    $('panel-history').classList.toggle('on', which === 'history');
    $('panel-clear').hidden = which !== 'history';
    $('panel-clear').textContent = 'Clear history';
    const list = $('panel-list');
    if (which === 'bookmarks') {
      T.core.invoke('list_bookmarks').then((items) =>
        fillList(list, items, (it) => invoke('remove_bookmark', { url: it.url }).then(() => openPanel('bookmarks'))));
    } else {
      T.core.invoke('list_history', { limit: 300 }).then((items) => fillList(list, items, null));
    }
    render();
  }

  function closePanel() {
    panel = null;
    startKey = '';
    render();
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
    const top = Math.ceil($('chrome').getBoundingClientRect().bottom);
    $('surface').style.top = top + 'px';
    const overlay = !!panel;
    const key = top + ':' + overlay;
    if (key === lastLayout) return;
    lastLayout = key;
    invoke('set_chrome_layout', { top, overlay });
  }

  function toast(msg) {
    const t = $('toast');
    t.textContent = msg;
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.hidden = true; reportLayout(); }, 4000);
    reportLayout();
  }

  // ---- wiring -----------------------------------------------------------

  function withTab(f) {
    return () => { const t = activeTab(); if (t) f(t); };
  }

  $('back').addEventListener('click', withTab((t) => invoke('go_back', { tab: t.id })));
  $('forward').addEventListener('click', withTab((t) => invoke('go_forward', { tab: t.id })));
  $('reload').addEventListener('click', withTab((t) => invoke(t.loading ? 'stop' : 'reload', { tab: t.id })));
  $('new-tab').addEventListener('click', () => invoke('new_tab', { private: false }).then(beginEdit));
  $('new-private').addEventListener('click', () => invoke('new_tab', { private: true }).then(beginEdit));
  $('bookmark').addEventListener('click', withTab((t) =>
    T.core.invoke('toggle_bookmark', { tab: t.id }).then((on) => toast(on ? 'Bookmarked' : 'Bookmark removed'))));
  $('menu').addEventListener('click', () => (panel ? closePanel() : openPanel('bookmarks')));
  $('panel-bookmarks').addEventListener('click', () => openPanel('bookmarks'));
  $('panel-history').addEventListener('click', () => openPanel('history'));
  $('panel-close').addEventListener('click', closePanel);
  $('panel-clear').addEventListener('click', () => {
    if (!clearArmed) { clearArmed = true; $('panel-clear').textContent = 'Tap again to clear'; return; }
    invoke('clear_history').then(() => openPanel('history'));
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
  window.addEventListener('resize', reportLayout);

  T.event.listen('state', (e) => { state = e.payload; render(); });
  T.event.listen('toast', (e) => toast(String(e.payload)));
  T.core.invoke('get_state').then((s) => { state = s; render(); });
})();
