// Browser chrome (see core.js): Site scripts, the address bar, layout and holes, ad blocking.
'use strict';

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
  renderSummary();
  applyNotebook();
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
  $('copy-url').hidden = true;
  $('share').hidden = true;
  input.hidden = false;
  input.value = tab && tab.url ? tab.url.full : '';
  input.focus();
  input.select();
}

function endEdit() {
  editing = false;
  $('url-input').hidden = true;
  $('url-display').hidden = false;
  $('copy-url').hidden = false;
  $('share').hidden = false;
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
  const overlay = !!panel || modalOpen || !$('edit-modal').hidden || !$('copy-modal').hidden || (scriptsOpen && narrow) || (chatShown() && narrow) || (notebookShown() && narrow) || (narrow && sidebarShown);
  // Docked right panels (site scripts; or the summary, the chat and the
  // notebook, one above the other when several are open) take the right of the
  // content area, full height, at the width the user dragged.
  const docks = rightPanels();
  const right = docks.length ? clampRight(rightWidth, left) : 0;
  $('chrome').style.left = left + 'px';
  $('chrome').style.right = right + 'px';
  const strip = Math.ceil(window.innerHeight - $('chrome').getBoundingClientRect().top);
  // Several right panels share the height, one under the other.
  const part = (window.innerHeight - top) / Math.max(1, docks.length);
  docks.forEach((d, i) => {
    d.style.top = Math.round(top + part * i) + 'px';
    d.style.bottom = (i === docks.length - 1 ? 0 : Math.round(window.innerHeight - top - part * (i + 1))) + 'px';
    d.style.width = right + 'px';
    d.classList.toggle('split-lower', i > 0);
  });
  const edge = $('right-resize');
  edge.hidden = !docks.length;
  edge.style.left = window.innerWidth - right + 'px';
  edge.style.top = top + 'px';
  // The docked console sits above the URL strip; while it is open the
  // page ends above it (the pill then floats over chrome, not the page).
  // An attached URL bar is not over the page: the page ends above it.
  const barBox = $('toolbar').getBoundingClientRect();
  const bar = barAttached && barBox.height > 0 ? Math.ceil(window.innerHeight - barBox.top) : 0;
  const con = $('console');
  con.style.left = left + 'px';
  con.style.right = right + 'px';
  con.style.bottom = (barAttached ? bar : strip) + 'px';
  const bottom = consoleOpen ? (barAttached ? bar : strip) + Math.ceil(con.getBoundingClientRect().height) : bar;
  // The chrome surface (start page, notices) covers the focused pane: in
  // split view that is one half of the content area, side by side or one
  // above the other (matching the native pane frames: 2 pt divider,
  // halves floored).
  let sLeft = left;
  let sRight = right;
  let sTop = top;
  let sBottom = bottom;
  const handle = $('split-resize');
  const split = state && state.split && state.sheet === null && !overlay ? state.split : null;
  handle.hidden = !split;
  if (split) {
    const first = state.active === split.left;
    const rows = split.orientation === 'rows';
    handle.classList.toggle('rows', rows);
    if (rows) {
      const h = window.innerHeight - top - bottom;
      const [a, b] = splitSizes(h, split.ratio);
      if (first) sBottom = bottom + h - a;
      else sTop = top + h - b;
      Object.assign(handle.style, { left: left + 'px', right: right + 'px', width: '', top: top + a + 'px', height: SPLIT_GAP + 'px', bottom: '' });
    } else {
      const w = window.innerWidth - left - right;
      const [a, b] = splitSizes(w, split.ratio);
      if (first) sRight = right + w - a;
      else sLeft = left + w - b;
      Object.assign(handle.style, { left: left + a + 'px', width: SPLIT_GAP + 'px', right: '', top: top + 'px', bottom: bottom + 'px', height: '' });
    }
  }
  const s = $('surface');
  s.style.top = sTop + 'px';
  s.style.left = sLeft + 'px';
  s.style.right = sRight + 'px';
  s.style.bottom = sBottom + 'px';
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
  // An attached bar lies below the page and needs no cut-out (the find
  // bar on top of it still uses its edge).
  const pill = hole($('toolbar'), 15);
  if (pill && !barAttached) out.push(pill);
  if (findOpen) {
    const f = hole($('findbar'), 3);
    if (f && pill) f.h = Math.max(1, pill.y - f.y);
    if (f) out.push(f);
  }
  for (const id of ['actions-pop', 'confirm-pop', 'pick-bar', 'capture-bar', 'sel-pop', 'img-pop', 'nb-pop', 'toast', 'os-banner']) {
    const e = $(id);
    if (e.hidden) continue;
    const h = hole(e, 15);
    if (h) out.push(h);
  }
  const side = sidebarHoles();
  // The flyout sidebar is drawn over the pill and the popups, so their
  // holes stop at its edge (2026-11-16). The host merges overlapping holes
  // into their bounding rectangle: with a right panel open the pill moves
  // left under the flyout, and the merged hole blanked most of the page.
  const fly = !narrow && flyoutMode() && sidebarShown ? side[0] : null;
  const kept = fly ? out.map((h) => {
    const edge = fly.x + fly.w + 1;
    const overlapsY = h.y < fly.y + fly.h && h.y + h.h > fly.y;
    if (!overlapsY || h.x >= edge) return h;
    if (h.x + h.w <= edge + 8) return null;
    return { ...h, w: h.x + h.w - edge, x: edge };
  }).filter(Boolean) : out;
  kept.push(...side);
  return kept;
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
  const panel = { x: panelX, y: top - 1, w: w + 1, h: height + 2, r: 15, c: 10 };
  // The hidden edge tab is not cut out (the page shows there).
  if (document.body.classList.contains('edge-away')) return [panel];
  return [panel, { x: tabX, y: Math.round(top + (height - TAB_H) / 2) - 1, w: 15, h: TAB_H + 2, r: 11, c: 10 }];
}

// The panels docked on the right of the content area: site scripts while
// open; otherwise the summary, the chat and the notebook (in that order
// from the top).
function rightPanels() {
  if (narrow || rightHidden) return [];
  if (scriptsOpen) return [$('scripts-pop')];
  const out = [];
  if (summaryShown()) out.push($('summary'));
  if (chatShown()) out.push($('chat'));
  if (notebookShown()) out.push($('notebook'));
  return out;
}

function chatShown() {
  return chatOpen && !rightHidden;
}

function notebookShown() {
  return notebookOpen && !rightHidden && !scriptsOpen;
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
