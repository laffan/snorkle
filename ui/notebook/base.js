// The Notebook (owner request 2026-11-15): the notepad of laffan/zotero-helper
// (src/notepad/, NotepadBody.tsx), ported to plain DOM for the chrome. One
// notebook, not bound to a page or a tab: strokes, notes typed in Markdown,
// quotes and clips from any page, kept natively (`notebook.json`, checked
// in Rust: snorkle_core::notebook::clean). It lives in the right panel; the
// chrome (ui/chrome/notebook-dock.js) hosts it and brings things in from pages.
//
// What is the same as in zotero-helper: one continuous surface — a slim bar
// of tools over a canvas with a faint dot grid that pans. The pen (a Pencil
// only, since 2026-11-16) writes in the theme's ink or in red; a finger, a
// mouse or trackpad drag, or a wheel pans; held still for half a second any
// press draws a lasso, which selects strokes and links (framed, with Delete; dragged to
// move; ⌫ deletes, Escape lets go). The eraser takes out what it passes
// over. Two taps (or a double-click) on the bare canvas type a note in
// Markdown. Links are chips: dragged, tapped to go to their place, held (or
// right-clicked) for a menu with Delete (and Edit for a typed note); quotes
// and clips are sized by a dotted grip and fixed by a pin (then drawn under
// the ink, to be written over). Undo takes back each change; the trash can
// clears the pad once confirmed. The map shows the whole pad small.
//
// What differs, for web pages instead of a PDF: a note remembers the page
// (address and title) it was written beside, shown small above it (the
// site) instead of "p. 12"; a tap goes back to that page. The quote button,
// dragged onto a page, quotes the sentence there (a link with a text
// fragment); the scissors turn on clip mode in the page (a drag there clips
// it). The scroll link follows the page shown: the pad glides to what was
// written about it. The pad's pictures are JPEGs the app drew itself from
// its own web view (never a page's image bytes); the dark copy of each is
// made here, as zotero-helper's `darkPagesCopy`.
//
// Security (docs/SECURITY_REVIEW.md): page-derived strings (titles, quotes,
// addresses) are set with textContent only; links open through the
// navigation policy (the chrome's `openTarget`); no markup is parsed.
//
// Split into ordered files (ui/notebook/*.js, listed in index.html) for the
// 700-line rule. Each is its own closure; what one defines for the next is
// put on `window.SnorkleNb` and taken from there.
(() => {
  'use strict';
  const NB = (window.SnorkleNb = {});

  const SVG_NS = document.getElementById('menu').querySelector('svg').namespaceURI;

  // ---- small helpers -------------------------------------------------------

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // An icon: `paths` are [tag, attributes] pairs drawn in a 16 × 16 box.
  function icon(paths, size = 14, attrs = {}) {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 16 16');
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
    svg.setAttribute('aria-hidden', 'true');
    const base = { fill: 'none', stroke: 'currentColor', 'stroke-width': '1.5', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
    for (const [k, v] of Object.entries({ ...base, ...attrs })) svg.setAttribute(k, v);
    for (const [tag, a] of paths) {
      const p = document.createElementNS(SVG_NS, tag);
      for (const [k, v] of Object.entries(a)) p.setAttribute(k, v);
      svg.appendChild(p);
    }
    return svg;
  }

  const ICONS = {
    link: () => icon([['path', { d: 'M6.8 9.2a2.6 2.6 0 0 0 3.7 0l2.2-2.2a2.6 2.6 0 0 0-3.7-3.7l-1 1' }], ['path', { d: 'M9.2 6.8a2.6 2.6 0 0 0-3.7 0L3.3 9a2.6 2.6 0 0 0 3.7 3.7l1-1' }]], 12, { 'stroke-width': '1.6' }),
    pin: () => icon([['path', { d: 'M6 1.5h4l-.5 4 2 2.5H4.5l2-2.5z' }], ['path', { d: 'M8 8v6' }]], 11),
    trash: () => icon([['path', { d: 'M2.5 4.5h11' }], ['path', { d: 'M6 4.5V3a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1.5' }], ['path', { d: 'M4 4.5l.7 8.6a1 1 0 0 0 1 .9h4.6a1 1 0 0 0 1-.9l.7-8.6' }]], 14),
    trashBig: () => icon([['path', { d: 'M2.5 4.5h11' }], ['path', { d: 'M6 4.5V3a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v1.5' }], ['path', { d: 'M4 4.5l.7 8.6a1 1 0 0 0 1 .9h4.6a1 1 0 0 0 1-.9l.7-8.6' }], ['path', { d: 'M6.7 7v4.5M9.3 7v4.5' }]], 15),
    edit: () => icon([['path', { d: 'M10.8 2.7l2.5 2.5L6 12.5l-3.2.7.7-3.2z' }], ['path', { d: 'M9.3 4.2l2.5 2.5' }]], 14),
    quote: () => icon([
      ['path', { d: 'M2.5 9.6c0-3 1.4-5.2 4-6.3l.6 1.1C5.6 5.1 4.8 6.2 4.7 7.4h.7c1 0 1.8.8 1.8 1.9v1.5c0 1-.8 1.9-1.8 1.9H4.3c-1 0-1.8-.8-1.8-1.9z' }],
      ['path', { d: 'M8.8 9.6c0-3 1.4-5.2 4-6.3l.6 1.1c-1.5.7-2.3 1.8-2.4 3h.7c1 0 1.8.8 1.8 1.9v1.5c0 1-.8 1.9-1.8 1.9h-1.1c-1 0-1.8-.8-1.8-1.9z' }],
    ], 13, { fill: 'currentColor', stroke: 'none' }),
    scissors: () => icon([['circle', { cx: '4', cy: '4.2', r: '2.1' }], ['circle', { cx: '4', cy: '11.8', r: '2.1' }], ['path', { d: 'M5.8 5.3L14 12.5M5.8 10.7L14 3.5' }]], 13),
    eraser: () => icon([['path', { d: 'M6.2 13.5L2.4 9.7a1 1 0 0 1 0-1.4l5.9-5.9a1 1 0 0 1 1.4 0l3.8 3.8a1 1 0 0 1 0 1.4l-6 5.9z' }], ['path', { d: 'M5 7.1l4.9 4.9' }], ['path', { d: 'M6.2 13.5h7.3' }]], 14, { 'stroke-width': '1.6' }),
    lasso: () => icon([['ellipse', { cx: '8.5', cy: '6.3', rx: '5.5', ry: '3.8', 'stroke-dasharray': '2.2 1.8' }], ['path', { d: 'M5.2 9.4c-1 .9-1 2.2 0 2.8.9.6.9 1.6.1 2.3' }]], 14),
    search: () => icon([['circle', { cx: '7', cy: '7', r: '4.6' }], ['path', { d: 'M10.4 10.4L14 14' }]], 16, { 'stroke-width': '1.7' }),
    map: () => icon([['path', { d: 'M1.8 3.8l4-1.6 4.4 1.6 4-1.6v10l-4 1.6-4.4-1.6-4 1.6z' }], ['path', { d: 'M5.8 2.2v10M10.2 3.8v10' }]], 17),
    follow: () => icon([['path', { d: 'M8 1.8v12.4' }], ['path', { d: 'M5.6 4.2L8 1.8l2.4 2.4' }], ['path', { d: 'M5.6 11.8L8 14.2l2.4-2.4' }], ['path', { d: 'M5.2 7h5.6M5.2 9h5.6' }]], 17),
    undo: () => icon([['path', { d: 'M5.5 3.5L2.5 6.5l3 3' }], ['path', { d: 'M2.5 6.5h7a3.5 3.5 0 0 1 0 7H7' }]], 16, { 'stroke-width': '2' }),
    tick: () => {
      const s = icon([['path', { d: 'M22 55L42 78L82 25', 'vector-effect': 'non-scaling-stroke' }]], 10);
      s.setAttribute('viewBox', '0 0 100 100');
      return s;
    },
  };

  /** Keep a pointer's events coming to `e`, if it is still there. */
  function capture(e, id) {
    try { e.setPointerCapture(id); } catch (err) { /* the pointer is already gone */ }
  }

  /** A button that acts on the release of a press on it (or a click from
   *  the keyboard) — never twice for one press. */
  function onPress(btn, act) {
    let pressed = false;
    btn.addEventListener('pointerdown', (e) => { e.stopPropagation(); pressed = true; });
    btn.addEventListener('pointerup', (e) => {
      if (!pressed) return;
      pressed = false;
      e.preventDefault();
      act();
    });
    btn.addEventListener('pointercancel', () => { pressed = false; });
    btn.addEventListener('click', (e) => { if (e.detail === 0) act(); });
  }

  function pref(key, fallback) {
    try { const v = localStorage.getItem(key); return v === null ? fallback : v; } catch (e) { return fallback; }
  }
  function setPref(key, value) {
    try { localStorage.setItem(key, value); } catch (e) { /* storage unavailable */ }
  }

  // ---- the pen's activity (zotero-helper's lib/penActivity.ts) ---------------
  //
  // While the pen writes, what can wait (the save, the minimap's redraw)
  // waits for it to pause.
  const WRITING_PAUSE_MS = 1800;
  const pens = new Set();
  let penHeardAt = 0;
  function penDown(who) { pens.add(who); penHeardAt = performance.now(); }
  function penUp(who) { pens.delete(who); penHeardAt = performance.now(); }
  function penHeard(t) { penHeardAt = t; }
  function penRestedMs() { return pens.size ? 0 : performance.now() - penHeardAt; }
  function penRested(ms) { return penRestedMs() >= ms; }

  // ---- the model (model.ts) ----------------------------------------------------

  const emptyPad = () => ({ strokes: [], notes: [], links: [], pan: { x: 0, y: 0 } });
  const newId = (prefix) => `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;

  /** A page's site, for the small labels: its host without `www.`. */
  function siteOf(url) {
    if (!url) return '';
    try { return new URL(url).hostname.replace(/^www\./, ''); } catch (e) { return ''; }
  }
  /** An address without its fragment (to compare pages). */
  function pageOf(url) {
    if (!url) return '';
    const i = url.indexOf('#');
    return i < 0 ? url : url.slice(0, i);
  }
  /** What a link says: a quote's words, else the page's title or site. */
  function linkLabel(t) {
    if (t.kind === 'quote' && t.text) return t.text.replace(/\s+/g, ' ').trim();
    return t.title || siteOf(t.url) || 'Page';
  }

  // ---- geometry (geometry.ts) ----------------------------------------------------

  function pathD(pts) {
    if (pts.length < 2) return '';
    if (pts.length < 4) return `M${pts[0]} ${pts[1]}l0.01 0`;
    let d = `M${pts[0]} ${pts[1]}`;
    for (let i = 2; i + 3 < pts.length; i += 2) {
      const mx = (pts[i] + pts[i + 2]) / 2;
      const my = (pts[i + 1] + pts[i + 3]) / 2;
      d += `Q${pts[i]} ${pts[i + 1]} ${mx} ${my}`;
    }
    return `${d}L${pts[pts.length - 2]} ${pts[pts.length - 1]}`;
  }

  function bounds(pts) {
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (let i = 0; i + 1 < pts.length; i += 2) {
      x0 = Math.min(x0, pts[i]); y0 = Math.min(y0, pts[i + 1]);
      x1 = Math.max(x1, pts[i]); y1 = Math.max(y1, pts[i + 1]);
    }
    return [x0, y0, x1, y1];
  }

  function distanceTo(pts, x, y) {
    let best = Infinity;
    for (let i = 0; i + 1 < pts.length; i += 2) {
      const ax = pts[i], ay = pts[i + 1];
      const bx = pts[i + 2] ?? ax, by = pts[i + 3] ?? ay;
      const dx = bx - ax, dy = by - ay;
      const t = dx || dy ? Math.max(0, Math.min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy))) : 0;
      best = Math.min(best, Math.hypot(x - (ax + t * dx), y - (ay + t * dy)));
    }
    return best;
  }

  function touches(s, x, y, reach) {
    const [x0, y0, x1, y1] = strokeBox(s);
    if (x < x0 - reach || x > x1 + reach || y < y0 - reach || y > y1 + reach) return false;
    return distanceTo(s.points, x, y) <= reach;
  }

  function inPolygon(x, y, poly) {
    let inside = false;
    const n = poly.length;
    for (let i = 0, j = n - 2; i + 1 < n; j = i, i += 2) {
      const xi = poly[i], yi = poly[i + 1];
      const xj = poly[j], yj = poly[j + 1];
      if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
    }
    return inside;
  }

  // ---- undo (history.ts) ---------------------------------------------------------

  function shiftItems(pad, strokeIds, linkIds, dx, dy) {
    for (const s of pad.strokes) {
      if (!strokeIds.includes(s.id)) continue;
      s.points = s.points.map((v, i) => Math.round((v + (i % 2 ? dy : dx)) * 10) / 10);
    }
    for (const l of pad.links) {
      if (!linkIds.includes(l.id)) continue;
      l.x = Math.round(l.x + dx);
      l.y = Math.round(l.y + dy);
    }
  }

  function dropEmpty(pad) {
    const gone = pad.notes.filter((n) => !pad.strokes.some((s) => s.note === n.id));
    pad.notes = pad.notes.filter((n) => !gone.includes(n));
    return gone;
  }

  function takeBack(pad, step) {
    if (step.kind === 'link') pad.links = pad.links.filter((l) => l.id !== step.id);
    else if (step.kind === 'move') shiftItems(pad, step.strokes, step.links, -step.dx, -step.dy);
    else if (step.kind === 'fix') step.link.fixed = !step.link.fixed || undefined;
    else if (step.kind === 'recolor') for (const [st, c] of step.was) st.color = c;
    else if (step.kind === 'text') step.link.text = step.was;
    else if (step.kind === 'size') { step.link.w = step.was.w; step.link.h = step.was.h; }
    else if (step.kind === 'paste') {
      pad.strokes = pad.strokes.filter((s) => !step.strokes.includes(s.id));
      pad.links = pad.links.filter((l) => !step.links.includes(l.id));
      pad.notes = pad.notes.filter((n) => !step.notes.includes(n.id));
    }
    else if (step.kind === 'stroke') {
      pad.strokes = pad.strokes.filter((s) => s.id !== step.id);
      dropEmpty(pad);
    } else {
      pad.strokes.push(...step.strokes);
      pad.notes.push(...step.notes);
      if (step.kind !== 'erase') pad.links.push(...step.links);
    }
  }

  Object.assign(NB, { SVG_NS, el, icon, ICONS, capture, onPress, pref, setPref, WRITING_PAUSE_MS, pens, penHeardAt, penDown, penUp, penHeard, penRestedMs, penRested, emptyPad, newId, siteOf, pageOf, linkLabel, pathD, bounds, distanceTo, touches, inPolygon, shiftItems, dropEmpty, takeBack });
})();
