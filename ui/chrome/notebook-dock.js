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
let notebookLoading = null;
function ensureNotebook() {
  if (notebook) return Promise.resolve(notebook);
  if (!notebookLoading) {
    notebookLoading = window.snorkleLoadNotebook().then(() => {
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
  setPref('notebookOpen', open ? '1' : '0');
  if (open) ensureNotebook();
  if (open) {
    rightHidden = false;
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

// Opens a notebook link's page: the tab already showing it (moved to the
// quote, if there is one), or a new tab. Through the navigation policy.
function openTarget(target) {
  const url = target && target.url;
  if (!url || !/^https?:/i.test(url)) { toast('This note has no page to go to.'); return; }
  const page = url.split('#')[0];
  const open = state && state.tabs.find((t) => !t.private && t.sheet_over === null && t.url && t.url.full.split('#')[0] === page);
  if (open) {
    invoke('activate_tab', { tab: open.id });
    if (url !== open.url.full && url.includes('#')) invoke('navigate', { tab: open.id, input: url });
    return;
  }
  T.core.invoke('new_tab', { private: false })
    .then((id) => invoke('navigate', { tab: id, input: url }))
    .catch((e) => toast(String(e)));
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
function dragLink(phase, x, y, fromY) {
  linkDrag.ended = phase !== 'move';
  const pane = phase === 'cancel' ? null : paneAt(x, y);
  const usable = pane && state.tabs.some((t) => t.id === pane.tab && !t.private && t.url);
  if (linkDrag.tab !== null && (!usable || linkDrag.tab !== pane.tab)) {
    linkDrag.next = { tab: linkDrag.tab, fx: -1, fy: 0, fromY: 0 };
    sendLinkHover();
    linkDrag.tab = null;
  }
  if (phase === 'cancel') return Promise.resolve(null);
  if (!usable) {
    if (phase === 'end') toast(pane ? 'Private tabs don’t send anything to the notebook.' : 'Let go over a page to quote it.');
    return Promise.resolve(null);
  }
  const fx = (x - pane.x) / pane.w;
  const fy = (y - pane.y) / pane.h;
  if (phase === 'move') {
    linkDrag.tab = pane.tab;
    linkDrag.next = { tab: pane.tab, fx, fy, fromY: (fromY - pane.y) / pane.h };
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
      const target = { kind: 'clip', url: t.url.full, title: t.title || '' };
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

// ⌥-click on text or an image: "Send to Notebook" above it.
let nbPick = null; // { tab, kind, rect }
function hideNbPop() {
  if (!nbPick && $('nb-pop').hidden) return;
  nbPick = null;
  $('nb-pop').hidden = true;
  reportLayout();
}
T.event.listen('notebook_pick', (e) => {
  const { tab, kind, rect } = e.payload || {};
  if (kind === 'end') { if (tab === clipTab) { clipTab = null; if (notebook) notebook.clipEnded(); } return; }
  // A drag began in a page (selected text, or an image), or ended.
  if (kind === 'dtext' || kind === 'dimg') { if (rect) pageDrag = { tab, kind, rect, at: performance.now(), ended: 0 }; return; }
  if (kind === 'dend') { if (pageDrag && pageDrag.tab === tab) pageDrag.ended = performance.now(); return; }
  if (kind === 'hide') { if (nbPick && nbPick.tab === tab) hideNbPop(); return; }
  const shown = shownPanes().some((p) => p.tab === tab);
  if (kind === 'clip') chromeLog('info', `notebook: clip from tab ${tab}${!rect || !shown || modalOpen || panel ? ' (ignored: page not shown, or a sheet open)' : ''}`);
  if (!rect || !shown || modalOpen || panel) return;
  const t = captureTab(tab);
  if (kind === 'clip') {
    if (tab === clipTab) { clipTab = null; if (notebook) notebook.clipEnded(); }
    if (t) clipToNotebook(t, rect);
    return;
  }
  if (!t) return;
  hideSelection();
  hideImageMenu();
  nbPick = { tab, kind, rect, url: kind === 'link' ? String(e.payload.url || '') : '' };
  placeOverPage($('nb-pop'), tab, rect);
});
$('nb-send').addEventListener('click', () => {
  const pick = nbPick;
  hideNbPop();
  const t = pick && captureTab(pick.tab);
  if (!t) return;
  if (pick.kind === 'img') { clipToNotebook(t, pick.rect); return; }
  if (pick.kind === 'link') {
    // The linked page's own title, read from the start of that page.
    const url = pick.url;
    if (!/^https?:/i.test(url)) return;
    toast('Adding the link…');
    T.core.invoke('notebook_link_title', { tab: t.id, url })
      .catch(() => null)
      .then((title) => {
        let host = '';
        try { host = new URL(url).hostname.replace(/^www\./, ''); } catch (err) { host = url; }
        ensureNotebook().then((nb) => {
          nb.addQuote({ kind: 'page', url, title: title || host });
          toast(notebookShown() ? 'Link added.' : 'Link sent to the notebook.');
        });
      });
    return;
  }
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
          const target = { kind: 'clip', ...source, ...(clip.text ? { text: clip.text } : {}) };
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
  const key = t ? `${t.id} ${t.url && !t.private ? t.url.full : ''}` : '';
  if (key === notebookPage) return;
  notebookPage = key;
  if (clipTab !== null && (!t || t.id !== clipTab)) endClipMode();
  if (nbPick && !shownPanes().some((p) => p.tab === nbPick.tab)) hideNbPop();
  notebook.pageChanged(t && t.url && !t.private ? t.url.full : '');
}

$('notebook-btn').addEventListener('click', () => setNotebook(!notebookOpen));
