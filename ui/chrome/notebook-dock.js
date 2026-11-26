// Browser chrome (see core.js): The Notebook docked with the right panels, fed from pages.
'use strict';

// ---- Notebook (owner request 2026-11-15) ------------------------------------
//
// ui/notebook/ draws it (zotero-helper's notepad); here it is docked with
// the right panels and fed from pages: ⌥-click on text or an image shows
// "Send to Notebook" above it, ⌥-drag clips a part of the page (C23), the
// scissors turn clip mode on in the page, and the quote button dragged
// onto a page links the sentence there. Nothing comes from private tabs.

function applyNotebook() {
  const shown = notebookShown();
  const nb = $('notebook');
  const was = !nb.hidden;
  nb.hidden = !shown;
  nb.classList.toggle('dock', shown && !narrow);
  if (!shown) nb.classList.remove('split-lower');
  $('notebook-btn').setAttribute('aria-pressed', String(notebookOpen));
  $('notebook-btn').classList.toggle('on', notebookOpen);
  if (notebook && shown && !was) requestAnimationFrame(() => notebook.shown());
  if (notebook && !shown && was) notebook.hidden();
}

// The notebook, its code loaded and created on first use (2026-11-23).
// `notebookEpoch` counts switches between sessions' notebooks
// (research.js): a load begun for another session's makes nothing.
let notebookLoading = null;
let notebookEpoch = 0;
function ensureNotebook() {
  if (notebook) return Promise.resolve(notebook);
  if (!notebookLoading) {
    const epoch = notebookEpoch;
    notebookLoading = window.snorkleLoadNotebook().then(() => {
      if (epoch !== notebookEpoch) return notebook;
      createNotebook();
      notebookPage = '';
      notebookTrack();
      if (notebookShown()) requestAnimationFrame(() => notebook.shown());
      return notebook;
    });
    notebookLoading.catch((e) => { notebookLoading = null; toast(String(e)); });
  }
  return notebookLoading;
}

function setNotebook(open) {
  notebookOpen = open;
  // Remembered for the session shown (research.js): a research session's
  // notebook is open until closed there.
  rememberNotebookOpen(open);
  if (open) ensureNotebook();
  if (open) {
    if (scriptsOpen) setScripts(false);
  } else if (notebook && notebook.clipping()) {
    endClipMode();
  }
  applyNotebook();
  applyChat();
  renderSummary();
  reportLayout();
}

// The tab a capture is from (the focused pane), or null with a toast.
function captureTab(tab) {
  const t = state && state.tabs.find((x) => x.id === tab);
  if (!t || !t.url) return null;
  if (t.private) { toast('Private tabs don’t send anything to the notebook.'); return null; }
  return t;
}

// A link to the sentence `text` on page `url` (a text fragment), so
// going there scrolls to it and marks it.
function textFragmentUrl(url, text) {
  const base = url.split('#')[0];
  const words = text.replace(/\s+/g, ' ').trim().split(' ').filter(Boolean);
  const enc = (w) => encodeURIComponent(w).replace(/-/g, '%2D').replace(/,/g, '%2C');
  const frag = words.length <= 10 ? enc(words.join(' ')) : `${enc(words.slice(0, 5).join(' '))},${enc(words.slice(-5).join(' '))}`;
  return frag ? `${base}#:~:text=${frag}` : base;
}

// Opens a notebook link's page: the tab already showing it (the one shown,
// if it does; moved to the quote, if there is one), or a new tab. Through
// the navigation policy. A clip goes back to where it was taken in its
// page (2026-11-25; see clipBackStep).
function openTarget(target) {
  const url = target && target.url;
  if (!url || !/^https?:/i.test(url)) { toast('This note has no page to go to.'); return; }
  const page = url.split('#')[0];
  const at = target.kind === 'clip' && Array.isArray(target.at) && target.at.length === 4 ? target.at : null;
  const shows = (t) => !!t && !t.private && t.sheet_over === null && !!t.url && t.url.full.split('#')[0] === page;
  const active = activeTab();
  const open = shows(active) ? active : state && state.tabs.find(shows);
  const back = (tab, fresh) => ({ tab, page, at, fresh, loaded: false, busy: false, until: performance.now() + 30000 });
  if (open) {
    invoke('activate_tab', { tab: open.id });
    if (url !== open.url.full && url.includes('#')) invoke('navigate', { tab: open.id, input: url });
    if (at) { clipBack = back(open.id, !open.live); clipBackStep(); }
    return;
  }
  T.core.invoke('new_tab', { private: false })
    .then((id) => {
      if (at) clipBack = back(id, true);
      return invoke('navigate', { tab: id, input: url });
    })
    .catch((e) => toast(String(e)));
}

// Going back to a clip (2026-11-25): once its page is shown, loaded, in
// the tab, the page scrolls to the clip's box and outlines it for a
// moment (`notebook_clip_back`). A tab that had to load the page (a new
// one, or one that was asleep) waits for that load; it is given up after
// 30 s, or when the tab is on another page. Run on every render and load
// progress.
let clipBack = null; // { tab, page, at, fresh, loaded, busy, until }
function clipBackStep() {
  const b = clipBack;
  if (!b || b.busy) return;
  const t = state && state.tabs.find((x) => x.id === b.tab);
  if (!t || performance.now() > b.until) { clipBack = null; return; }
  if (t.loading) { b.loaded = true; return; }
  if (!t.url || !t.live || (b.fresh && !b.loaded)) return;
  if (t.url.full.split('#')[0] !== b.page) { clipBack = null; return; }
  b.busy = true;
  T.core.invoke('notebook_clip_back', { tab: b.tab, x: b.at[0], y: b.at[1], w: b.at[2], h: b.at[3] })
    .then(() => { if (clipBack === b) clipBack = null; })
    .catch(() => { b.busy = false; });
}

// A link typed in a note: a web address (or a bare host) opens in a new
// tab; anything else is not opened.
function openTypedLink(href) {
  const h = String(href || '').trim();
  if (/^https?:/i.test(h)) openTarget({ url: h });
  else if (/^[a-z0-9-]+(\.[a-z0-9-]+)+(\/\S*)?$/i.test(h)) {
    T.core.invoke('new_tab', { private: false }).then((id) => invoke('navigate', { tab: id, input: h })).catch((e) => toast(String(e)));
  } else toast('Only web addresses open from the notebook.');
}

// The pages on screen (both halves of a split) and where each is.
function shownPanes() {
  if (!state || state.active === null || state.sheet !== null) return [];
  const b = contentBox;
  const w = window.innerWidth - b.left - b.right;
  const h = window.innerHeight - b.top - b.bottom;
  const s = state.split;
  if (!s) return [{ tab: state.active, x: b.left, y: b.top, w, h }];
  if (s.orientation === 'rows') {
    const [first, second] = splitSizes(h, s.ratio);
    return [{ tab: s.left, x: b.left, y: b.top, w, h: first }, { tab: s.right, x: b.left, y: b.top + h - second, w, h: second }];
  }
  const [first, second] = splitSizes(w, s.ratio);
  return [{ tab: s.left, x: b.left, y: b.top, w: first, h }, { tab: s.right, x: b.left + w - second, y: b.top, w: second, h }];
}
function paneAt(x, y) {
  return shownPanes().find((p) => x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h) || null;
}

// The quote button dragged out of the notebook: the page under the
// pointer marks the sentence it would link (and draws the line on from
// its right edge); let go, that sentence is quoted.
// One hover call at a time; a call still under way when the drag ends
// is followed by one that takes its mark away (2026-11-16: a late
// hover could leave a sentence marked after the drop).
const linkDrag = { tab: null, next: null, busy: false, ended: false };
function sendLinkHover() {
  if (linkDrag.busy || !linkDrag.next) return;
  const n = linkDrag.next;
  linkDrag.next = null;
  linkDrag.busy = true;
  T.core.invoke('notebook_link_hover', n).catch(() => {}).finally(() => {
    linkDrag.busy = false;
    if (linkDrag.ended && n.fx >= 0) T.core.invoke('notebook_link_hover', { tab: n.tab, fx: -1, fy: 0, fromY: 0 }).catch(() => {});
    requestAnimationFrame(sendLinkHover);
  });
}
function dragLink(phase, x, y, fromX, fromY) {
  linkDrag.ended = phase !== 'move';
  const pane = phase === 'cancel' ? null : paneAt(x, y);
  const tab = pane && state.tabs.find((t) => t.id === pane.tab);
  const usable = !!tab && !tab.private && !!tab.url && !tab.loading;
  if (linkDrag.tab !== null && (!usable || linkDrag.tab !== pane.tab)) {
    linkDrag.next = { tab: linkDrag.tab, fx: -1, fy: 0, fromY: 0 };
    sendLinkHover();
    linkDrag.tab = null;
  }
  if (phase === 'cancel') return Promise.resolve(null);
  if (!usable) {
    if (phase === 'end') toast(!pane ? 'Let go over a page to quote it.' : tab && tab.loading ? 'Wait for the page to finish loading.' : 'Private tabs don’t send anything to the notebook.');
    return Promise.resolve(null);
  }
  const fx = (x - pane.x) / pane.w;
  const fy = (y - pane.y) / pane.h;
  if (phase === 'move') {
    linkDrag.tab = pane.tab;
    // The chrome's line is hidden under the page: the page draws it on
    // from where it crosses the page's right edge, so the two meet.
    const edge = pane.x + pane.w;
    const cross = fromX > edge && x < edge ? fromY + (y - fromY) * (fromX - edge) / (fromX - x) : fromY;
    linkDrag.next = { tab: pane.tab, fx, fy, fromY: (cross - pane.y) / pane.h };
    sendLinkHover();
    return Promise.resolve(null);
  }
  linkDrag.tab = null;
  linkDrag.next = null;
  const t = captureTab(pane.tab);
  if (!t) return Promise.resolve(null);
  return T.core.invoke('notebook_link_drop', { tab: t.id, fx, fy }).then((text) => {
    if (text) return { kind: 'quote', url: textFragmentUrl(t.url.full, text), title: t.title || '', text };
    return { kind: 'page', url: t.url.full, title: t.title || '' };
  }).catch((e) => { toast(String(e)); return null; });
}

// Clip mode (the notebook's scissors) is in one page: the focused one.
let clipTab = null;
function setClipMode(on) {
  if (!on) { endClipMode(); return Promise.resolve(false); }
  const t = activeTab();
  if (!t || !t.url) { toast('Open a page to clip it.'); return Promise.resolve(false); }
  if (t.private) { toast('Private tabs don’t send anything to the notebook.'); return Promise.resolve(false); }
  return T.core.invoke('notebook_clip_mode', { tab: t.id, on: true }).then(() => {
    clipTab = t.id;
    toast('Drag over the page, adjust the box, then Clip (Esc stops).');
    return true;
  }).catch((e) => { toast(String(e)); return false; });
}
function endClipMode() {
  if (clipTab !== null) T.core.invoke('notebook_clip_mode', { tab: clipTab, on: false }).catch(() => {});
  clipTab = null;
  if (notebook) notebook.clipEnded();
}

// What a clip carries besides its picture and text (2026-11-25): the links
// laid out in it (shown under it) and its box in its page (to go back to).
function clipExtras(target, clip) {
  if (Array.isArray(clip.links) && clip.links.length) target.links = clip.links;
  if (Array.isArray(clip.at) && clip.at.length === 4) target.at = clip.at;
  return target;
}

// Draws a clip of a page's region (the app's own picture of its view)
// and puts it in the notebook.
function clipToNotebook(t, r) {
  // Drawing it (and reading its words) can take a moment: say so.
  const slow = setTimeout(() => toast('Clipping…'), 600);
  T.core.invoke('notebook_clip', { tab: t.id, x: r.x, y: r.y, w: r.w, h: r.h, vw: r.vw })
    .finally(() => clearTimeout(slow))
    .then((clip) => {
      // The clip's text (the page's, then the words recognition reads
      // in the picture) goes with it for search; it is not shown.
      const target = clipExtras({ kind: 'clip', url: t.url.full, title: t.title || '' }, clip);
      if (clip.text) target.text = clip.text;
      ensureNotebook().then((nb) => {
        nb.addClip(clip.image, target);
        if (!notebookShown()) toast('Clipped to the notebook.');
        clipWords(clip, target);
      });
    })
    .catch((e) => toast(String(e)));
}
// The words on-device recognition reads in a clip's picture, added to
// its text when they come (the clip is in the notebook already).
function clipWords(clip, target) {
  if (!clip || !clip.words) return;
  T.core.invoke('notebook_clip_words', { words: clip.words })
    .then((text) => { if (text) notebook.setClipText(target, text); })
    .catch(() => {});
}

// ⌥-click in a page goes straight to the notebook (owner request
// 2026-11-25; until then a link, an image or text got a "Send to Notebook"
// button first): the element clicked is clipped, with its text and links;
// on selected text, the selection is quoted.
T.event.listen('notebook_pick', (e) => {
  const { tab, kind, rect } = e.payload || {};
  if (kind === 'end') { if (tab === clipTab) { clipTab = null; if (notebook) notebook.clipEnded(); } return; }
  // A drag began in a page (selected text, or an image), or ended.
  if (kind === 'dtext' || kind === 'dimg') { if (rect) pageDrag = { tab, kind, rect, at: performance.now(), ended: 0 }; return; }
  if (kind === 'dend') { if (pageDrag && pageDrag.tab === tab) pageDrag.ended = performance.now(); return; }
  if (kind === 'hide' || kind === 'link') return;
  const shown = shownPanes().some((p) => p.tab === tab);
  chromeLog('info', `notebook: ${kind} from tab ${tab}${!rect || !shown || modalOpen || panel ? ' (ignored: page not shown, or a sheet open)' : ''}`);
  if (!rect || !shown || modalOpen || panel) return;
  if (kind === 'clip' && tab === clipTab) { clipTab = null; if (notebook) notebook.clipEnded(); }
  const t = captureTab(tab);
  if (!t) return;
  if (kind === 'clip' || kind === 'img') { clipToNotebook(t, rect); return; }
  if (kind !== 'text') return;
  T.core.invoke('read_selection', { tab: t.id }).then((text) => {
    if (!text) { toast('Nothing selected to send.'); return; }
    ensureNotebook().then((nb) => {
      nb.addQuote({ kind: 'quote', url: textFragmentUrl(t.url.full, text), title: t.title || '', text });
      if (!notebookShown()) toast('Sent to the notebook.');
    });
  }).catch((err) => toast(String(err)));
});

// Dragged onto the notebook (2026-11-16): what a drag from a page holds
// is taken from the page itself, as ⌥-click takes it (selected text read
// with C20, an image clipped from the app's own picture of the page);
// the drag's own data (HTML, image files) is never used. The page reports
// where a drag began; its report stays good until the drag ends (and a
// moment after, as the end may arrive before the drop) or for 2 minutes.
let pageDrag = null; // { tab, kind, rect, at, ended }
const cleanDropped = (t) => String(t || '').replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '').replace(/\n{3,}/g, '\n\n').trim().slice(0, 4000);
const sameText = (a, b) => a.replace(/\s+/g, ' ').trim().slice(0, 40) === b.replace(/\s+/g, ' ').trim().slice(0, 40);
function dropOnNotebook({ text, files }) {
  const now = performance.now();
  const d = pageDrag && now - pageDrag.at < 120000 && (!pageDrag.ended || now - pageDrag.ended < 3000) ? pageDrag : null;
  pageDrag = null;
  const dropped = cleanDropped(text);
  if (d) {
    const t = captureTab(d.tab);
    if (!t) return Promise.resolve(null);
    const source = { url: t.url.full, title: t.title || '' };
    if (d.kind === 'dimg') {
      return T.core.invoke('notebook_clip', { tab: t.id, x: d.rect.x, y: d.rect.y, w: d.rect.w, h: d.rect.h, vw: d.rect.vw })
        .then((clip) => {
          const target = clipExtras({ kind: 'clip', ...source, ...(clip.text ? { text: clip.text } : {}) }, clip);
          clipWords(clip, target);
          return { kind: 'clip', image: clip.image, target };
        })
        .catch((err) => { toast(String(err)); return null; });
    }
    return T.core.invoke('read_selection', { tab: t.id }).then((sel) => {
      // Text dropped from elsewhere meanwhile: not this page's selection.
      if (sel && (!dropped || sameText(sel, dropped))) {
        return { kind: 'quote', target: { kind: 'quote', url: textFragmentUrl(t.url.full, sel), title: source.title, text: sel } };
      }
      return dropped ? { kind: 'text', text: dropped, target: { kind: 'page' } } : null;
    }).catch(() => null);
  }
  // Not from a page: a link becomes a link, other text a typed note;
  // pictures from elsewhere are not taken (the chrome decodes no images
  // it did not make).
  if (/^https?:\/\/\S+$/i.test(dropped)) return Promise.resolve({ kind: 'page', target: { kind: 'page', url: dropped, title: dropped } });
  if (dropped) return Promise.resolve({ kind: 'text', text: dropped, target: { kind: 'page' } });
  if (files) toast('Images can be dragged in from pages.');
  return Promise.resolve(null);
}
// A drag resting on the notebook's button opens it, to drop into.
$('notebook-btn').addEventListener('dragenter', (e) => { e.preventDefault(); if (!notebookOpen) setNotebook(true); });

// The page shown changes: a stroke after it starts a new note, the
// notebook may follow it, and clip mode ends in the page left.
let notebookPage = '';
function notebookTrack() {
  if (!notebook || !state) return;
  const t = activeTab();
  notebook.setPageLoading(!!(t && t.loading));
  const key = t ? `${t.id} ${t.url && !t.private ? t.url.full : ''}` : '';
  if (key === notebookPage) return;
  notebookPage = key;
  if (clipTab !== null && (!t || t.id !== clipTab)) endClipMode();
  notebook.pageChanged(t && t.url && !t.private ? t.url.full : '');
}

$('notebook-btn').addEventListener('click', () => setNotebook(!notebookOpen));
