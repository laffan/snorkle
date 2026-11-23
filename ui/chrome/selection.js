// Browser chrome (see core.js): The selection and image menus over pages.
'use strict';

// ---- selected text on a page -----------------------------------------------
//
// A watcher in the page reports where the selection is (C19); a small
// menu floats above it (cut out of the page, like the pill). Its text is
// read when the menu appears (C20) and used only by the chosen action.

let selectionGeneration = 0;
let selection = null; // { tab, text }

function paneBox(tab) {
  const b = contentBox;
  const w = window.innerWidth - b.left - b.right;
  const s = state && state.split;
  if (s && (s.left === tab || s.right === tab)) {
    if (s.orientation === 'rows') {
      const h = window.innerHeight - b.top - b.bottom;
      const [, second] = splitSizes(h, s.ratio);
      return { x: b.left, y: s.left === tab ? b.top : b.top + h - second, w };
    }
    const [first, second] = splitSizes(w, s.ratio);
    return s.left === tab ? { x: b.left, y: b.top, w: first } : { x: b.left + w - second, y: b.top, w: second };
  }
  return { x: b.left, y: b.top, w };
}

function hideSelection() {
  ++selectionGeneration;
  if (!selection && $('sel-pop').hidden) return;
  selection = null;
  $('sel-pop').hidden = true;
  reportLayout();
}

function showSelection(tab, r) { placeOverPage($('sel-pop'), tab, r); }

// Shows a menu above (or below) a rectangle reported by a page, in the
// page's visual-viewport coordinates (`vw` wide), converted to window
// points with the pane's scale.
function placeOverPage(pop, tab, r) {
  const pane = paneBox(tab);
  const k = pane.w / r.vw;
  const sx = pane.x + r.x * k;
  const sy = pane.y + r.y * k;
  const sw = r.w * k;
  const sh = r.h * k;
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
  const generation = ++selectionGeneration;
  const { tab, rect } = e.payload;
  const shown = state && (tab === state.active || (state.split && (tab === state.split.left || tab === state.split.right)));
  if (!rect || !shown || modalOpen || panel) {
    hideSelection();
    if (!rect && nbPick && nbPick.tab === tab && nbPick.kind === 'text') hideNbPop();
    return;
  }
  if (nbPick && nbPick.tab === tab) return; // "Send to Notebook" is up for it
  T.core.invoke('read_selection', { tab }).then((text) => {
    if (generation !== selectionGeneration) return;
    if (!text || !text.trim()) { hideSelection(); return; }
    selection = { tab, text };
    const ask = useChoice('ask');
    $('sel-ask').textContent = ask ? `Ask ${ask.name}` : 'Ask AI';
    showSelection(tab, rect);
  }).catch(() => { if (generation === selectionGeneration) hideSelection(); });
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

// ---- image menu (C21) ---------------------------------------------------
//
// The page's image watcher reports an image's address and where it was
// asked for. Copy, Share and Download have the host fetch the image (no
// cookies); Copy Address copies the URL.
let imageMenu = null; // { tab, url }
function hideImageMenu() {
  if (!imageMenu && $('img-pop').hidden) return;
  imageMenu = null;
  $('img-pop').hidden = true;
  reportLayout();
}
T.event.listen('image_menu', (e) => {
  const { tab, url, rect } = e.payload;
  if (!url || !rect) { if (imageMenu && imageMenu.tab === tab) hideImageMenu(); return; }
  const shown = state && (tab === state.active || (state.split && (tab === state.split.left || tab === state.split.right)));
  if (!shown || modalOpen || panel) return;
  hideSelection();
  imageMenu = { tab, url };
  placeOverPage($('img-pop'), tab, rect);
});
for (const [id, action] of [['img-copy', 'copy'], ['img-share', 'share'], ['img-download', 'download']]) {
  $(id).addEventListener('click', () => {
    if (!imageMenu) return;
    const r = $('img-pop').getBoundingClientRect();
    const url = imageMenu.url;
    hideImageMenu();
    if (action === 'copy') toast('Copying image…');
    T.core.invoke('image_action', { url, action, x: r.left, y: r.top, w: r.width, h: r.height })
      .then(() => { if (action === 'copy') toast('Image copied.'); })
      .catch((err) => toast(String(err)));
  });
}
$('img-copy-url').addEventListener('click', () => {
  if (!imageMenu) return;
  const url = imageMenu.url;
  hideImageMenu();
  navigator.clipboard.writeText(url).then(() => toast('Image address copied.'), () => toast('Could not copy (clipboard unavailable).'));
});

// Copy: the page's committed address (from Rust's DisplayUrl). With ⌥
// (2026-11-16): the page goes to the notebook as a link instead, and
// while ⌥ is held the button shows an arrow (⌥ as macOS reports it
// whatever has focus, `option_key`, or as the chrome's own events see it).
let optionHeld = false;
function setOptionHeld(on) {
  if (optionHeld === on) return;
  optionHeld = on;
  const b = $('copy-url');
  b.classList.toggle('to-notebook', on);
  b.setAttribute('aria-label', on ? 'Send this page to the notebook' : 'Copy this page’s address');
  b.title = on ? 'Send this page to the notebook' : 'Copy address (⌥: send to the notebook)';
}
T.event.listen('option_key', (e) => setOptionHeld(!!e.payload));
document.addEventListener('keydown', (ev) => { if (ev.key === 'Alt') setOptionHeld(true); }, true);
document.addEventListener('keyup', (ev) => { if (ev.key === 'Alt') setOptionHeld(false); }, true);
for (const type of ['pointerenter', 'pointermove']) {
  $('copy-url').addEventListener(type, (ev) => setOptionHeld(ev.altKey && !ev.metaKey && !ev.ctrlKey));
}
window.addEventListener('blur', () => setOptionHeld(false));
$('copy-url').addEventListener('click', (ev) => {
  const t = activeTab();
  if (!t) return;
  if (!t.url) { toast('Open a page to copy its address.'); return; }
  if (ev.altKey || optionHeld) {
    if (t.private) { toast('Private tabs don’t send anything to the notebook.'); return; }
    if (!notebook) return;
    notebook.addQuote({ kind: 'page', url: t.url.full, title: t.title || t.url.registrable });
    toast(notebookShown() ? 'Page added to the notebook.' : 'Page sent to the notebook.');
    return;
  }
  navigator.clipboard.writeText(t.url.full).then(() => toast('Address copied.'), () => toast('Could not copy (clipboard unavailable).'));
});

// Share: the system share sheet for this page, pointing at the button.
$('share').addEventListener('click', withTab((t) => {
  if (!t.url) { toast('Open a page to share it.'); return; }
  const r = $('share').getBoundingClientRect();
  invoke('share_page', { tab: t.id, x: r.left, y: r.top, w: r.width, h: r.height });
}));
