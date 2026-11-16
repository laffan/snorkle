// The Notebook (owner request 2026-11-15): the notepad of laffan/zotero-helper
// (src/notepad/, NotepadBody.tsx), ported to plain DOM for the chrome. One
// notebook, not bound to a page or a tab: strokes, notes typed in Markdown,
// quotes and clips from any page, kept natively (`notebook.json`, checked
// in Rust: snorkle_core::notebook::clean). It lives in the right panel; the
// chrome (chrome.js) hosts it and brings things in from pages.
//
// What is the same as in zotero-helper: one continuous surface — a slim bar
// of tools over a canvas with a faint dot grid that pans. The pen (a Pencil,
// or the mouse with a drag) writes in the theme's ink or in red; a finger,
// a wheel or a trackpad pans; held still for half a second any press draws
// a lasso, which selects strokes and links (framed, with Delete; dragged to
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
'use strict';

(() => {
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
    else if (step.kind === 'stroke') {
      pad.strokes = pad.strokes.filter((s) => s.id !== step.id);
      dropEmpty(pad);
    } else {
      pad.strokes.push(...step.strokes);
      pad.notes.push(...step.notes);
      if (step.kind !== 'erase') pad.links.push(...step.links);
    }
  }

  // ---- ink (ink.ts): every stroke on one canvas the size of the view -----------------

  const MAX_DPR = 2;
  const shapes = new WeakMap();

  function outline(pts) {
    const path = new Path2D();
    const n = pts.length;
    path.moveTo(pts[0], pts[1]);
    if (n < 4) path.lineTo(pts[0] + 0.01, pts[1]);
    else {
      for (let i = 2; i + 3 < n; i += 2) path.quadraticCurveTo(pts[i], pts[i + 1], (pts[i] + pts[i + 2]) / 2, (pts[i + 1] + pts[i + 3]) / 2);
      path.lineTo(pts[n - 2], pts[n - 1]);
    }
    return path;
  }

  function shapeOf(pts) {
    let s = shapes.get(pts);
    if (!s) shapes.set(pts, (s = { box: bounds(pts), path: outline(pts) }));
    return s;
  }
  const strokeBox = (s) => shapeOf(s.points).box;
  const strokeLine = (s) => shapeOf(s.points).path;

  function createPadInk(host, strokes) {
    const canvas = el('canvas', 'notepad-ink');
    host.prepend(canvas);
    const ctx = canvas.getContext('2d', { desynchronized: true });
    let dpr = 1;
    let w = 0;
    let h = 0;
    let pan = { x: 0, y: 0 };
    let lifted = null;
    let lift = { x: 0, y: 0 };
    // While a search shows matches: the notes drawn in full (others faint).
    let keep = null;
    let themeInk = '';
    let writing = false;
    let frame = 0;
    let live = null;

    const inkColor = (c) => c || (themeInk ||= getComputedStyle(host).getPropertyValue('--notepad-ink').trim() || '#000');

    function fit() {
      w = host.clientWidth;
      h = host.clientHeight;
      dpr = Math.min(MAX_DPR, window.devicePixelRatio || 1);
      const cw = Math.round(w * dpr);
      const ch = Math.round(h * dpr);
      if (canvas.width !== cw) canvas.width = cw;
      if (canvas.height !== ch) canvas.height = ch;
      return cw > 0 && ch > 0;
    }

    function style(color, width) {
      ctx.strokeStyle = ctx.fillStyle = inkColor(color);
      ctx.lineWidth = width;
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';
    }

    function draw() {
      frame = 0;
      if (!canvas.width || !canvas.height) return;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      const tx = Math.round(pan.x * dpr);
      const ty = Math.round(pan.y * dpr);
      const [vx0, vy0, vx1, vy1] = [-pan.x - 4, -pan.y - 4, w - pan.x + 4, h - pan.y + 4];
      let color = null;
      let width = -1;
      for (const s of strokes()) {
        if (s.points.length < 2) continue;
        const { box, path } = shapeOf(s.points);
        const up = lifted && lifted.has(s);
        const [ox, oy] = up ? [lift.x, lift.y] : [0, 0];
        if (box[2] + ox < vx0 || box[0] + ox > vx1 || box[3] + oy < vy0 || box[1] + oy > vy1) continue;
        if (s.color !== color || s.width !== width) style((color = s.color), (width = s.width));
        ctx.globalAlpha = keep && !keep.has(s.note) ? 0.2 : 1;
        ctx.setTransform(dpr, 0, 0, dpr, tx + ox * dpr, ty + oy * dpr);
        ctx.stroke(path);
      }
      ctx.globalAlpha = 1;
      if (live) drawLive();
    }

    function drawLive() {
      if (!live) return;
      penTransform();
      style(live.color, live.width);
      ctx.stroke(outline(live.pts));
    }

    const penTransform = () => ctx.setTransform(dpr, 0, 0, dpr, Math.round(pan.x * dpr), Math.round(pan.y * dpr));

    function redraw() {
      if (!frame) frame = requestAnimationFrame(draw);
    }

    const misfit = () => canvas.width !== Math.round(host.clientWidth * dpr) || canvas.height !== Math.round(host.clientHeight * dpr);
    const sized = new ResizeObserver(() => { if (!writing && fit()) draw(); });
    sized.observe(host);

    const themed = () => { themeInk = ''; redraw(); };
    const watchTheme = new MutationObserver(themed);
    watchTheme.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class'] });
    watchTheme.observe(document.body, { attributes: true, attributeFilter: ['class'] });
    const scheme = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
    if (scheme && scheme.addEventListener) scheme.addEventListener('change', themed);

    fit();

    return {
      setPan(x, y) {
        if (x === pan.x && y === pan.y) return;
        pan = { x, y };
        cancelAnimationFrame(frame);
        draw();
      },
      redraw,
      drawNow() {
        cancelAnimationFrame(frame);
        if (fit()) draw();
      },
      begin(x, y, color, width) {
        writing = true;
        live = { pts: [x, y], color, width };
        penTransform();
        style(color, width);
        ctx.beginPath();
        ctx.arc(x, y, width / 2, 0, Math.PI * 2);
        ctx.fill();
      },
      extend(pts) {
        if (!live) return;
        live.pts = pts;
        const m = pts.length;
        ctx.beginPath();
        if (m === 4) {
          ctx.moveTo(pts[0], pts[1]);
          ctx.lineTo((pts[0] + pts[2]) / 2, (pts[1] + pts[3]) / 2);
        } else {
          ctx.moveTo((pts[m - 6] + pts[m - 4]) / 2, (pts[m - 5] + pts[m - 3]) / 2);
          ctx.quadraticCurveTo(pts[m - 4], pts[m - 3], (pts[m - 4] + pts[m - 2]) / 2, (pts[m - 3] + pts[m - 1]) / 2);
        }
        ctx.stroke();
      },
      finish() {
        const pts = live && live.pts;
        live = null;
        writing = false;
        if (pts && pts.length >= 4) {
          const m = pts.length;
          ctx.beginPath();
          ctx.moveTo((pts[m - 4] + pts[m - 2]) / 2, (pts[m - 3] + pts[m - 1]) / 2);
          ctx.lineTo(pts[m - 2], pts[m - 1]);
          ctx.stroke();
        }
        if (misfit() && fit()) draw();
      },
      drop() {
        if (!live) return;
        live = null;
        writing = false;
        cancelAnimationFrame(frame);
        if (misfit()) fit();
        draw();
      },
      // A search's matching notes (ids) drawn in full and the rest faint;
      // null: all in full.
      dim(notes) {
        keep = notes;
        redraw();
      },
      lift(list, dx = 0, dy = 0) {
        lifted = list && list.length ? new Set(list) : null;
        lift = { x: dx, y: dy };
        redraw();
      },
      destroy() {
        cancelAnimationFrame(frame);
        sized.disconnect();
        watchTheme.disconnect();
        if (scheme && scheme.removeEventListener) scheme.removeEventListener('change', themed);
        canvas.width = canvas.height = 0;
        canvas.remove();
      },
    };
  }

  // ---- Markdown in typed notes (markdown.ts, as Hush reads a text shape) ---------------

  const INLINE = /\*\*(.+?)\*\*|\*(.+?)\*|_(.+?)_|\[([^\]]+)\]\(([^)]+)\)|==(.+?)==/g;

  function inline(text) {
    const runs = [];
    let last = 0;
    for (const m of text.matchAll(INLINE)) {
      const at = m.index || 0;
      if (at > last) runs.push({ text: text.slice(last, at) });
      if (m[1] !== undefined) runs.push({ text: m[1], bold: true });
      else if (m[2] !== undefined || m[3] !== undefined) runs.push({ text: m[2] ?? m[3], italic: true });
      else if (m[4] !== undefined) runs.push({ text: m[4], link: m[5] });
      else if (m[6] !== undefined) for (const r of inline(m[6])) runs.push({ ...r, highlight: true });
      last = at + m[0].length;
    }
    if (last < text.length) runs.push({ text: text.slice(last) });
    return runs;
  }

  function parseLine(raw) {
    let s = raw;
    const quote = /^>+\s?(.*)$/.exec(s);
    if (quote) s = quote[1];
    let task;
    const t = /^\s*[-*+]\s+\[([ xX])\]\s+(.*)$/.exec(s);
    if (t) { task = t[1] !== ' '; s = t[2]; }
    let list;
    if (!t) {
      const bullet = /^(\s*)[-*+]\s+(.*)$/.exec(s);
      const numbered = bullet ? null : /^(\s*)(\d+)[.)]\s+(.*)$/.exec(s);
      if (bullet) { list = { marker: '•', depth: Math.floor(bullet[1].length / 2) }; s = bullet[2]; }
      else if (numbered) { list = { marker: `${numbered[2]}.`, depth: Math.floor(numbered[1].length / 2) }; s = numbered[3]; }
    }
    let heading = 0;
    const hd = /^(#{1,3})\s+(.*)$/.exec(s);
    if (hd) { heading = hd[1].length; s = hd[2]; }
    const runs = inline(s);
    if (quote) for (const r of runs) r.italic = true;
    return { runs, heading, quote: Boolean(quote), task, list };
  }

  function toggleTask(text, index) {
    const lines = text.split('\n');
    const m = /^((?:>+\s?)?\s*[-*+]\s+)\[([ xX])\](\s+.*)$/.exec(lines[index] || '');
    if (!m) return null;
    lines[index] = `${m[1]}[${m[2] === ' ' ? 'x' : ' '}]${m[3]}`;
    return lines.join('\n');
  }

  const QUOTE_EM = 0.9;
  const DEPTH_EM = 1.2;
  const MARKER_EM = 1.5;
  const BOX_EM = 0.85;
  const BOX_GAP_EM = 0.4;

  function lineEl(line, index) {
    const e = el('div', 'notepad-text-line');
    if (line.heading) e.classList.add(`h${line.heading}`);
    if (line.quote) e.classList.add('quote');
    let left = line.quote ? QUOTE_EM : 0;
    if (line.list) {
      const marker = el('span', 'notepad-text-marker', line.list.marker);
      left += DEPTH_EM * line.list.depth;
      marker.style.left = `${left}em`;
      e.appendChild(marker);
      left += MARKER_EM;
    } else if (line.task !== undefined) {
      const box = el('span', line.task ? 'notepad-text-box done' : 'notepad-text-box');
      box.dataset.task = String(index);
      box.setAttribute('role', 'checkbox');
      box.setAttribute('aria-checked', String(line.task));
      box.style.left = `${left}em`;
      if (line.task) box.appendChild(ICONS.tick());
      e.appendChild(box);
      left += BOX_EM + BOX_GAP_EM;
    }
    if (left) e.style.paddingLeft = `${left}em`;
    const words = el('span', 'notepad-text-words');
    for (const r of line.runs) {
      if (!r.text) continue;
      const cls = [r.bold && 'b', r.italic && 'i', r.highlight && 'hl', r.link && 'ln'].filter(Boolean).join(' ');
      const run = el('span', cls, r.text);
      if (r.link) run.dataset.href = r.link;
      words.appendChild(run);
    }
    if (!words.firstChild) words.appendChild(document.createElement('br'));
    e.appendChild(words);
    return e;
  }

  function renderText(text) {
    const body = el('div', 'notepad-text-body');
    text.split('\n').forEach((raw, i) => body.appendChild(lineEl(parseLine(raw), i)));
    return body;
  }

  // ---- chips: links, quotes, clips, typed notes (links.ts) ---------------------------------

  const HOLD_MS = 550;
  const CHIP_TAP_PX = 6;
  const MIN_W = 90;
  const MAX_W = 640;
  const MIN_H = 24;

  function createLinkChips(host, env) {
    let menu = null;

    function closeMenu() {
      if (!menu) return;
      menu.off();
      menu.el.remove();
      menu = null;
    }

    function linkMenu(l, chip) {
      closeMenu();
      const m = el('div', 'notepad-link-menu');
      const item = (svg, label, act) => {
        const b = el('button');
        b.type = 'button';
        b.append(svg, el('span', '', label));
        onPress(b, () => { closeMenu(); act(); });
        m.appendChild(b);
        return b;
      };
      if (l.text !== undefined) item(ICONS.edit(), 'Edit', () => env.edit(l)).className = 'notepad-menu-edit';
      item(ICONS.trash(), 'Delete', () => env.remove(l));
      m.addEventListener('pointerdown', (e) => e.stopPropagation());
      host.appendChild(m);
      const hr = host.getBoundingClientRect();
      const cr = chip.getBoundingClientRect();
      const below = cr.bottom - hr.top + 6;
      m.style.left = `${Math.max(6, Math.min(cr.left - hr.left, host.clientWidth - m.offsetWidth - 6))}px`;
      m.style.top = `${below + m.offsetHeight > host.clientHeight ? cr.top - hr.top - m.offsetHeight - 6 : below}px`;
      const away = (e) => { if (!m.contains(e.target)) closeMenu(); };
      setTimeout(() => window.addEventListener('pointerdown', away, true));
      menu = { el: m, off: () => window.removeEventListener('pointerdown', away, true) };
    }

    function applySize(l, e) {
      if (l.text !== undefined) {
        e.style.maxWidth = l.w ? `${l.w}px` : '';
        return;
      }
      e.style.width = l.w ? `${l.w}px` : '';
      e.style.height = l.h && !l.image ? `${l.h}px` : '';
      e.classList.toggle('sized', Boolean(l.w || l.h));
    }

    function pin(l) {
      const b = el('span', 'notepad-link-pin');
      b.setAttribute('role', 'button');
      b.setAttribute('aria-pressed', String(Boolean(l.fixed)));
      b.title = l.fixed ? 'Fixed — write over it; press to let it move again' : 'Fix it to the pad, to write over it';
      b.appendChild(ICONS.pin());
      onPress(b, () => env.toggleFixed(l));
      return b;
    }

    function sizeGrip(l, e) {
      const text = l.text !== undefined;
      const grip = el('span', 'notepad-link-size');
      grip.title = text ? 'Drag to set the width the note wraps at' : 'Drag to size it';
      grip.addEventListener('pointerdown', (ev) => {
        ev.stopPropagation();
        ev.preventDefault();
        if (ev.pointerType === 'mouse' && ev.button !== 0) return;
        capture(grip, ev.pointerId);
        const from = { x: ev.clientX, y: ev.clientY, w: e.offsetWidth, h: e.offsetHeight };
        const was = { w: l.w, h: l.h };
        const move = (mv) => {
          l.w = Math.round(Math.max(MIN_W, Math.min(MAX_W, from.w + mv.clientX - from.x)));
          if (!l.image && !text) l.h = Math.round(Math.max(MIN_H, from.h + mv.clientY - from.y));
          applySize(l, e);
          if (text) e.style.width = `${l.w}px`;
        };
        const up = () => {
          grip.removeEventListener('pointermove', move);
          grip.removeEventListener('pointerup', up);
          grip.removeEventListener('pointercancel', up);
          if (text) e.style.width = '';
          if (l.w !== was.w || l.h !== was.h) env.sized(l, was);
        };
        grip.addEventListener('pointermove', move);
        grip.addEventListener('pointerup', up);
        grip.addEventListener('pointercancel', up);
      });
      return grip;
    }

    // A clip: both its pictures — as the page showed it, and white on
    // black — and the theme and the pin decide which shows (notebook.css).
    function clipBody(l, e) {
      const image = (src, cls) => {
        const img = el('img', `notepad-clip-img ${cls}`);
        img.src = src;
        img.alt = `Clip from ${siteOf(l.target.url) || 'a page'}`;
        img.draggable = false;
        e.appendChild(img);
      };
      image(l.image || '', 'light');
      if (l.imageDark) image(l.imageDark, 'dark');
      e.classList.toggle('has-dark', Boolean(l.imageDark));
      const foot = el('span', 'notepad-clip-foot');
      foot.append(pin(l), el('span', 'notepad-link-page', siteOf(l.target.url) || l.target.title || ''));
      e.appendChild(foot);
    }

    function textChip(l) {
      const e = el('div', 'notepad-text');
      e.dataset.link = l.id;
      e.style.left = `${l.x}px`;
      e.style.top = `${l.y}px`;
      const site = siteOf(l.target.url);
      if (site) e.appendChild(el('span', 'notepad-note-label', site));
      e.append(renderText(l.text || ''), sizeGrip(l, e));
      applySize(l, e);
      e.title = site ? `Go to ${l.target.title || site} (double-click to edit)` : 'Double-click to edit';
      handlePresses(l, e);
      return e;
    }

    function chip(l) {
      if (l.text !== undefined) return textChip(l);
      const quote = l.target.kind === 'quote';
      const e = el('button', `notepad-link${quote ? ' notepad-link-quote' : ''}${l.image ? ' notepad-clip' : ''}${l.fixed ? ' fixed' : ''}`);
      e.type = 'button';
      e.dataset.link = l.id;
      e.style.left = `${l.x}px`;
      e.style.top = `${l.y}px`;
      if (l.image) {
        clipBody(l, e);
        e.appendChild(sizeGrip(l, e));
        applySize(l, e);
      } else if (quote) {
        const icons = el('span', 'notepad-link-icons');
        icons.append(ICONS.link(), pin(l));
        e.appendChild(icons);
      } else e.appendChild(ICONS.link());
      if (!l.image) chipText(l, e, quote);
      const where = l.target.title || siteOf(l.target.url) || 'the page';
      e.title = l.image ? `Go to the clip's page: ${where}` : quote ? `Go to the quote on ${where}` : `Go to ${where}`;
      handlePresses(l, e);
      return e;
    }

    function chipText(l, e, quote) {
      const text = el('span', 'notepad-link-text', linkLabel(l.target));
      if (quote) {
        const site = siteOf(l.target.url);
        if (site) text.appendChild(el('span', 'notepad-link-page', ` ${site}`));
      }
      e.appendChild(text);
      if (quote) {
        e.appendChild(sizeGrip(l, e));
        applySize(l, e);
      }
    }

    function handlePresses(l, e) {
      e.addEventListener('contextmenu', (ev) => { ev.preventDefault(); linkMenu(l, e); });
      e.addEventListener('pointerdown', (ev) => {
        if (l.fixed) return;
        ev.stopPropagation();
        ev.preventDefault();
        if (ev.pointerType === 'mouse' && ev.button !== 0) return;
        const on = ev.target instanceof Element ? ev.target : null;
        const from = { x: ev.clientX, y: ev.clientY, lx: l.x, ly: l.y };
        let moved = false;
        let held = false;
        const hold = setTimeout(() => { held = true; linkMenu(l, e); }, HOLD_MS);
        capture(e, ev.pointerId);
        const move = (mv) => {
          if (held) return;
          moved ||= Math.hypot(mv.clientX - from.x, mv.clientY - from.y) > CHIP_TAP_PX;
          if (moved) clearTimeout(hold);
          if (!moved) return;
          l.x = from.lx + mv.clientX - from.x;
          l.y = from.ly + mv.clientY - from.y;
          e.style.left = `${l.x}px`;
          e.style.top = `${l.y}px`;
        };
        const up = (u) => {
          clearTimeout(hold);
          e.removeEventListener('pointermove', move);
          e.removeEventListener('pointerup', up);
          e.removeEventListener('pointercancel', up);
          if (held) return;
          if (moved) env.moved(l, l.x - from.lx, l.y - from.ly);
          else if (u.type !== 'pointerup') return;
          else if (l.text !== undefined) env.tapText(l, on, u.clientX, u.clientY);
          else env.goTo(l.target);
        };
        e.addEventListener('pointermove', move);
        e.addEventListener('pointerup', up);
        e.addEventListener('pointercancel', up);
      });
    }

    return { chip, closeMenu };
  }

  // ---- the lasso (lasso.ts) -------------------------------------------------------------

  const FRAME_PAD = 8;

  function createLasso(layer, svg, env) {
    const loop = document.createElementNS(SVG_NS, 'path');
    loop.setAttribute('class', 'notepad-lasso');
    const frame = el('div', 'notepad-selection');
    frame.style.display = 'none';
    const bar = el('div', 'notepad-selection-menu');
    const del = el('button');
    del.type = 'button';
    del.append(ICONS.trash(), el('span', '', 'Delete'));
    onPress(del, () => remove());
    bar.appendChild(del);
    bar.addEventListener('pointerdown', (e) => e.stopPropagation());
    frame.appendChild(bar);
    layer.appendChild(frame);

    let pts = null;
    let strokes = [];
    let links = [];
    let box = null;

    function begin(x, y) {
      clear();
      pts = [x, y];
      loop.setAttribute('d', pathD(pts));
      svg.appendChild(loop);
    }

    function extend(x, y) {
      if (!pts) return;
      const n = pts.length;
      if (Math.hypot(x - pts[n - 2], y - pts[n - 1]) < 1) return;
      pts.push(x, y);
      loop.setAttribute('d', `${pathD(pts)}Z`);
    }

    function finish() {
      const poly = pts;
      pts = null;
      loop.remove();
      if (!poly || poly.length < 6) return false;
      const pad = env.pad();
      strokes = pad.strokes.filter((s) => {
        for (let i = 0; i + 1 < s.points.length; i += 2) if (inPolygon(s.points[i], s.points[i + 1], poly)) return true;
        return false;
      });
      links = pad.links.filter((l) => {
        if (l.fixed) return false;
        const c = env.chipOf(l.id);
        return inPolygon(l.x + (c ? c.offsetWidth : 0) / 2, l.y + (c ? c.offsetHeight : 0) / 2, poly);
      });
      if (!strokes.length && !links.length) return false;
      showFrame();
      window.addEventListener('keydown', onKey);
      return true;
    }

    function showFrame() {
      let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
      for (const s of strokes) {
        const [a, b, c, d] = strokeBox(s);
        x0 = Math.min(x0, a); y0 = Math.min(y0, b); x1 = Math.max(x1, c); y1 = Math.max(y1, d);
      }
      for (const l of links) {
        const c = env.chipOf(l.id);
        x0 = Math.min(x0, l.x); y0 = Math.min(y0, l.y);
        x1 = Math.max(x1, l.x + (c ? c.offsetWidth : 0)); y1 = Math.max(y1, l.y + (c ? c.offsetHeight : 0));
      }
      box = [x0 - FRAME_PAD, y0 - FRAME_PAD, x1 + FRAME_PAD, y1 + FRAME_PAD];
      Object.assign(frame.style, {
        left: `${box[0]}px`, top: `${box[1]}px`, width: `${box[2] - box[0]}px`, height: `${box[3] - box[1]}px`, transform: '', display: '',
      });
    }

    function clear() {
      pts = null;
      loop.remove();
      strokes = [];
      links = [];
      box = null;
      frame.style.display = 'none';
      window.removeEventListener('keydown', onKey);
    }

    function prune() {
      if (!box) return;
      const pad = env.pad();
      strokes = strokes.filter((s) => pad.strokes.includes(s));
      links = links.filter((l) => pad.links.includes(l));
      if (strokes.length || links.length) showFrame();
      else clear();
    }

    function remove() {
      if (!strokes.length && !links.length) return;
      const gone = { strokes, links };
      clear();
      env.removed(gone.strokes, gone.links);
    }

    function onKey(e) {
      if (e.defaultPrevented || (e.target && e.target.closest && e.target.closest('input, textarea, [contenteditable]'))) return;
      if (e.key === 'Backspace' || e.key === 'Delete') {
        e.preventDefault();
        remove();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        clear();
      }
    }

    function drag(dx, dy) {
      const t = `translate(${dx}px, ${dy}px)`;
      env.lift(strokes, dx, dy);
      for (const l of links) { const c = env.chipOf(l.id); if (c) c.style.transform = t; }
      frame.style.transform = t;
    }

    function drop(dx, dy) {
      env.lift(null, 0, 0);
      for (const l of links) { const c = env.chipOf(l.id); if (c) c.style.transform = ''; }
      if (Math.abs(dx) >= 0.5 || Math.abs(dy) >= 0.5) env.moved(strokes, links, dx, dy);
      showFrame();
    }

    return {
      begin, extend, finish, drag, drop, clear, prune, remove,
      drawing: () => pts !== null,
      strokes: () => strokes,
      has: () => box !== null,
      contains: (x, y) => Boolean(box && x >= box[0] && x <= box[2] && y >= box[1] && y <= box[3]),
      destroy() { clear(); frame.remove(); },
    };
  }

  // ---- each note's page, small, above it (noteLabels.ts) -----------------------------------

  function createNoteLabels(pad) {
    const els = new Map();
    const corners = new Map();

    function cornerOf(id) {
      let c = corners.get(id);
      if (c) return c;
      let [x0, y0] = [Infinity, Infinity];
      for (const s of pad().strokes) {
        if (s.note !== id) continue;
        const b = strokeBox(s);
        x0 = Math.min(x0, b[0]);
        y0 = Math.min(y0, b[1]);
      }
      if (x0 === Infinity) return null;
      corners.set(id, (c = [x0, y0]));
      return c;
    }

    function label(n) {
      const site = siteOf(n.url);
      if (!site) return null;
      const corner = cornerOf(n.id);
      if (!corner) return null;
      let e = els.get(n.id);
      if (!e) {
        e = el('span', 'notepad-note-label', site);
        e.title = n.title || site;
        els.set(n.id, e);
      }
      e.style.left = `${corner[0]}px`;
      e.style.top = `${corner[1] - 16}px`;
      return e;
    }

    return {
      all() {
        els.clear();
        corners.clear();
        return pad().notes.flatMap((n) => label(n) || []);
      },
      grew(s) {
        const box = strokeBox(s);
        const c = corners.get(s.note);
        if (c) corners.set(s.note, [Math.min(c[0], box[0]), Math.min(c[1], box[1])]);
        const n = pad().notes.find((x) => x.id === s.note);
        return n ? label(n) : null;
      },
    };
  }

  // ---- typed notes (texts.ts) ------------------------------------------------------------

  const DOUBLE_MS = 400;
  const DOUBLE_PX = 24;
  const TEXT_MAX_W = 350;
  const TEXT_MIN_W = 120;
  const EDGE_PX = 12;

  const tidy = (text) => text.replace(/^(?:[ \t]*\n)+/, '').trimEnd();

  function wrap(ta, marker) {
    const v = ta.value;
    const start = ta.selectionStart;
    const end = ta.selectionEnd;
    const n = marker.length;
    const sel = v.slice(start, end);
    let next;
    let [a, b] = [start + n, end + n];
    if (start === end) next = v.slice(0, start) + marker + marker + v.slice(end);
    else if (sel.length >= 2 * n && sel.startsWith(marker) && sel.endsWith(marker)) {
      next = v.slice(0, start) + sel.slice(n, -n) + v.slice(end);
      [a, b] = [start, end - 2 * n];
    } else if (v.slice(Math.max(0, start - n), start) === marker && v.slice(end, end + n) === marker) {
      next = v.slice(0, start - n) + sel + v.slice(end + n);
      [a, b] = [start - n, end - n];
    } else next = v.slice(0, start) + marker + sel + marker + v.slice(end);
    ta.value = next;
    ta.setSelectionRange(a, b);
  }

  function linkify(ta) {
    const v = ta.value;
    const start = ta.selectionStart;
    const end = ta.selectionEnd;
    const sel = v.slice(start, end);
    ta.value = `${v.slice(0, start)}[${sel}](url)${v.slice(end)}`;
    if (sel) ta.setSelectionRange(start + sel.length + 3, start + sel.length + 6);
    else ta.setSelectionRange(start + 1, start + 1);
  }

  function createTextNotes(env) {
    const ta = el('textarea', 'notepad-text-editor');
    ta.setAttribute('aria-label', 'Text note, in Markdown');
    ta.setAttribute('autocapitalize', 'sentences');
    let open = null;
    let session = 0;

    function fit() {
      ta.classList.toggle('empty', !ta.value);
      ta.style.height = '0px';
      ta.style.height = `${ta.scrollHeight}px`;
    }

    function focus() {
      try {
        ta.focus({ preventScroll: true });
        ta.setSelectionRange(ta.value.length, ta.value.length);
      } catch (e) { /* gone meanwhile */ }
    }

    function begin(o, text) {
      commit();
      open = o;
      session++;
      ta.value = text;
      Object.assign(ta.style, { left: `${o.x}px`, top: `${o.y}px`, width: `${o.w || TEXT_MAX_W}px` });
      env.layer.appendChild(ta);
      if (o.link) { const c = env.chipOf(o.link.id); if (c) c.classList.add('editing'); }
      fit();
      focus();
      setTimeout(() => { if (open === o && document.activeElement !== ta) focus(); }, 20);
    }

    function create(cx, cy) {
      const r = env.host.getBoundingClientRect();
      const pan = env.pan();
      const p = env.place();
      const room = r.right - cx - EDGE_PX;
      begin({
        link: null,
        x: Math.round(cx - r.left - pan.x),
        y: Math.round(cy - r.top - pan.y),
        target: { kind: 'page', url: p && p.url, title: p && p.title },
        w: room < TEXT_MAX_W ? Math.round(Math.max(TEXT_MIN_W, room)) : undefined,
      }, '');
    }

    function edit(l) {
      if (!open || open.link !== l) begin({ link: l, x: l.x, y: l.y, target: l.target, w: l.w }, l.text || '');
    }

    function commit() {
      const o = open;
      if (!o) return;
      open = null;
      session++;
      const text = tidy(ta.value);
      ta.remove();
      if (!o.link) {
        if (text) {
          const target = { kind: 'page' };
          if (o.target.url) target.url = o.target.url;
          if (o.target.title) target.title = o.target.title;
          env.add({ id: newId('text'), x: o.x, y: o.y, target, text, ...(o.w ? { w: o.w } : {}) });
        }
        return;
      }
      const c = env.chipOf(o.link.id);
      if (c) c.classList.remove('editing');
      if (!env.pad().links.includes(o.link)) return;
      if (!text) env.remove(o.link);
      else if (text !== o.link.text) env.retext(o.link, text);
    }

    ta.addEventListener('input', fit);
    ta.addEventListener('keydown', (e) => {
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === 'Escape' || (mod && e.key === 'Enter')) {
        e.preventDefault();
        e.stopPropagation();
        commit();
        return;
      }
      if (!mod || e.altKey || e.shiftKey || e.isComposing) return;
      const key = e.key.toLowerCase();
      const marker = key === 'b' ? '**' : key === 'i' ? '*' : key === '=' || key === '+' ? '==' : '';
      if (marker) wrap(ta, marker);
      else if (key === 'k') linkify(ta);
      else return;
      e.preventDefault();
      e.stopPropagation();
      fit();
    });
    ta.addEventListener('blur', () => {
      const s = session;
      setTimeout(() => { if (open && session === s && document.activeElement !== ta) commit(); }, 150);
    });
    ta.addEventListener('pointerdown', (e) => e.stopPropagation());

    const pressed = (e) => { if (open && !ta.contains(e.target)) commit(); };
    env.host.addEventListener('pointerdown', pressed, true);
    const hidden = () => {
      if (document.visibilityState !== 'hidden' || !open) return;
      commit();
      env.flush();
    };
    document.addEventListener('visibilitychange', hidden);
    const padEl = env.host.closest('.notepad') || env.host;
    const unscroll = (e) => {
      const t = e.target;
      if (!(t instanceof Element) || !t.contains(env.host) || !padEl.contains(t)) return;
      t.scrollTop = 0;
      t.scrollLeft = 0;
    };
    padEl.addEventListener('scroll', unscroll, true);

    let last = null;

    function second(on, cx, cy, then) {
      const was = last;
      const now = performance.now();
      last = null;
      if (was) clearTimeout(was.timer);
      if (was && was.on === on && now - was.t < DOUBLE_MS && Math.hypot(cx - was.x, cy - was.y) < DOUBLE_PX) return true;
      if (was && was.then) was.then();
      const kept = { on, x: cx, y: cy, t: now, timer: 0, then };
      if (then) {
        kept.timer = setTimeout(() => {
          if (last === kept) last = null;
          then();
        }, DOUBLE_MS);
      }
      last = kept;
      return false;
    }

    return {
      edit,
      commit,
      editing: () => (open && open.link ? open.link.id : null),
      bareTap(cx, cy) { if (second('', cx, cy)) create(cx, cy); },
      tap(l, on, cx, cy) {
        const box = on && on.closest('[data-task]');
        const ticked = box ? toggleTask(l.text || '', Number(box.dataset.task)) : null;
        if (ticked !== null) return env.retext(l, ticked);
        const ref = on && on.closest('[data-href]');
        if (ref) return env.openLink(ref.dataset.href);
        if (second(l.id, cx, cy, () => env.goTo(l.target))) edit(l);
        return undefined;
      },
      destroy() {
        commit();
        if (last) clearTimeout(last.timer);
        env.host.removeEventListener('pointerdown', pressed, true);
        padEl.removeEventListener('scroll', unscroll, true);
        document.removeEventListener('visibilitychange', hidden);
      },
    };
  }

  // ---- the canvas (surface.ts) ------------------------------------------------------------

  const PEN_WIDTH = 2;
  const RED_INK = '#ff5f57';
  const HIT_PX = 12;
  const TAP_PX = 6;
  const STILL_PX = 4;
  const LASSO_HOLD_MS = 500;
  const STALE_MS = 1500;
  const ERASE_PX = 10;
  const ERASER_BUTTON = 32;
  const GLIDE_MS = 350;

  function createSurface(host, env) {
    const layer = el('div', 'notepad-layer');
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'notepad-strokes');
    const labels = el('div', 'notepad-labels');
    layer.append(svg, labels);
    host.appendChild(layer);

    const ink = createPadInk(host, () => env.pad().strokes);
    const under = el('div', 'notepad-layer notepad-under');
    host.prepend(under);
    const chips = new Map();
    const noteLabels = createNoteLabels(env.pad);
    const history = [];
    let current = null;
    let tool = 'pen';
    let color = '';
    let pan = { ...env.pad().pan };
    // The view's last known size (the panel may be hidden when something
    // arrives from a page).
    let seen = { w: host.clientWidth || 320, h: host.clientHeight || 480 };
    const watchers = new Set();
    const tell = (what) => watchers.forEach((fn) => fn(what));

    let panFrame = 0;
    const showPan = () => {
      panFrame = 0;
      ink.setPan(pan.x, pan.y);
      layer.style.transform = under.style.transform = `translate(${pan.x}px, ${pan.y}px)`;
      host.style.backgroundPosition = `${pan.x}px ${pan.y}px`;
      tell('view');
    };
    const applyPan = () => { if (!panFrame) panFrame = requestAnimationFrame(showPan); };

    // Following the page shown (the scroll link): a glide to what was
    // written about it (follow.ts, for addresses instead of PDF pages).
    let glide = 0;
    function stopGlide() { cancelAnimationFrame(glide); glide = 0; }
    function follow(url) {
      const page = pageOf(url);
      if (!page || g) return;
      const pad = env.pad();
      let top = null;
      const take = (x, y) => { if (top === null || y < top.y) top = { x, y }; };
      for (const n of pad.notes) {
        if (pageOf(n.url) !== page) continue;
        for (const s of pad.strokes) if (s.note === n.id) { const b = strokeBox(s); take(b[0], b[1]); }
      }
      for (const l of pad.links) if (pageOf(l.target.url) === page) take(l.x, l.y);
      if (!top) return;
      const w = host.clientWidth || seen.w;
      const to = { x: pan.x, y: Math.round(40 - top.y) };
      if (top.x + pan.x < 0 || top.x + pan.x > w - 40) to.x = Math.round(20 - top.x);
      glideTo(to);
    }

    // The pan eased to `to` (350 ms; a press stops it), then kept.
    function glideTo(to) {
      const from = { ...pan };
      const t0 = performance.now();
      stopGlide();
      const stepGlide = (t) => {
        const k = Math.min(1, (t - t0) / GLIDE_MS);
        const e = 1 - Math.pow(1 - k, 3);
        pan = { x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e };
        showPan();
        if (k < 1) glide = requestAnimationFrame(stepGlide);
        else { glide = 0; env.pad().pan = { ...pan }; env.changed(); }
      };
      glide = requestAnimationFrame(stepGlide);
    }

    let origin = null;
    const toCanvas = (cx, cy) => {
      const r = origin || host.getBoundingClientRect();
      return [cx - r.left - pan.x, cy - r.top - pan.y];
    };

    const dropEmptyNotes = () => dropEmpty(env.pad());

    const linkChips = createLinkChips(host, {
      moved: (l, dx, dy) => {
        history.push({ kind: 'move', strokes: [], links: [l.id], dx, dy });
        env.changed();
      },
      sized: (l, was) => {
        history.push({ kind: 'size', link: l, was });
        env.changed();
      },
      goTo: (t) => env.goTo(t),
      remove: (l) => removeItems([], [l]),
      toggleFixed: (l) => {
        l.fixed = !l.fixed || undefined;
        history.push({ kind: 'fix', link: l });
        drawLabels();
        env.changed();
      },
      tapText: (l, on, cx, cy) => texts.tap(l, on, cx, cy),
      edit: (l) => texts.edit(l),
    });

    const texts = createTextNotes({
      host,
      layer,
      pad: env.pad,
      pan: () => pan,
      place: env.place,
      chipOf: (id) => chips.get(id),
      add: (l) => {
        env.pad().links.push(l);
        history.push({ kind: 'link', id: l.id });
        redrawChip(l);
        env.changed();
      },
      retext: (l, text) => {
        history.push({ kind: 'text', link: l, was: l.text || '' });
        l.text = text;
        redrawChip(l);
        env.changed();
      },
      remove: (l) => removeItems([], [l]),
      goTo: (t) => env.goTo(t),
      openLink: env.openLink,
      flush: env.flush,
    });

    // ---- search (owner request 2026-11-16; zotero-helper has none) ------------
    //
    // What can be found: a typed note's words, a quote's words, and for
    // every item (handwritten notes included) the title and address of the
    // page it came from. Handwriting itself is not read. Every word of the
    // query must appear (case and accents ignored). Matches are outlined,
    // everything else faded; stepping goes through them top to bottom and
    // glides the pad to each.
    let search = null; // { terms, hits, at, frames }
    const fold = (t) => String(t || '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
    const where = (url) => String(url || '').replace(/^[a-z]+:\/\/(www\.)?/i, '');
    const linkWords = (l) => fold([l.text, l.target.text, l.target.title, where(l.target.url)].filter(Boolean).join(' '));
    const noteWords = (n) => fold([n.title, where(n.url)].filter(Boolean).join(' '));

    function noteBox(id) {
      let b = null;
      for (const st of env.pad().strokes) {
        if (st.note !== id) continue;
        const k = strokeBox(st);
        b = b ? [Math.min(b[0], k[0]), Math.min(b[1], k[1]), Math.max(b[2], k[2]), Math.max(b[3], k[3])] : [...k];
      }
      return b;
    }

    function findHits() {
      const pad = env.pad();
      const hits = [];
      const all = (words) => search.terms.every((t) => words.includes(t));
      for (const l of pad.links) {
        if (!all(linkWords(l))) continue;
        const c = chips.get(l.id);
        hits.push({ link: l.id, box: [l.x, l.y, l.x + (c ? c.offsetWidth : 120), l.y + (c ? c.offsetHeight : 30)] });
      }
      for (const n of pad.notes) {
        if (!all(noteWords(n))) continue;
        const box = noteBox(n.id);
        if (box) hits.push({ note: n.id, box });
      }
      hits.sort((a, b) => a.box[1] - b.box[1] || a.box[0] - b.box[0]);
      return hits;
    }

    // Marks the matches (and fades the rest) as they are drawn now.
    function paintSearch() {
      if (!search) return;
      const on = search.terms.length > 0;
      const linkHits = new Set(search.hits.filter((h) => h.link).map((h) => h.link));
      const cur = search.hits[search.at] || null;
      for (const [id, c] of chips) {
        c.classList.toggle('nb-dim', on && !linkHits.has(id));
        c.classList.toggle('nb-hit', linkHits.has(id));
        c.classList.toggle('nb-current', !!cur && cur.link === id);
      }
      for (const f of search.frames) f.remove();
      search.frames = search.hits.filter((h) => h.note).map((h) => {
        const f = el('div', 'nb-note-hit' + (cur === h ? ' nb-current' : ''));
        Object.assign(f.style, { left: `${h.box[0] - 6}px`, top: `${h.box[1] - 6}px`, width: `${h.box[2] - h.box[0] + 12}px`, height: `${h.box[3] - h.box[1] + 12}px` });
        layer.insertBefore(f, labels);
        return f;
      });
      ink.dim(on ? new Set(search.hits.filter((h) => h.note).map((h) => h.note)) : null);
    }

    // Brings a match into view (only if it is not already).
    function showHit(h) {
      const w = host.clientWidth;
      const ht = host.clientHeight;
      if (!h || !w || !ht) return;
      const [x0, y0, x1, y1] = h.box;
      const inView = x0 + pan.x >= 0 && x1 + pan.x <= w && y0 + pan.y >= 0 && y1 + pan.y <= ht;
      if (inView) return;
      const cx = (x0 + x1) / 2;
      const top = y1 - y0 > ht * 0.8 ? y0 - 20 : (y0 + y1) / 2 - ht / 2;
      const fitsX = x0 + pan.x >= 0 && x1 + pan.x <= w;
      const x = fitsX ? pan.x : x1 - x0 > w * 0.9 ? Math.round(20 - x0) : Math.round(w / 2 - cx);
      glideTo({ x, y: Math.round(-top) });
    }

    function setSearch(query) {
      const terms = fold(query).split(/\s+/).filter(Boolean);
      if (!search) search = { terms: [], hits: [], at: -1, frames: [] };
      search.terms = terms;
      search.hits = terms.length ? findHits() : [];
      // The first match from the top of the view down, else the first.
      const top = -pan.y;
      const next = search.hits.findIndex((h) => h.box[3] >= top);
      search.at = search.hits.length ? (next >= 0 ? next : 0) : -1;
      paintSearch();
      showHit(search.hits[search.at]);
      return { at: search.at, count: search.hits.length };
    }

    function stepSearch(dir) {
      if (!search || !search.hits.length) return { at: -1, count: 0 };
      search.at = (search.at + dir + search.hits.length) % search.hits.length;
      paintSearch();
      showHit(search.hits[search.at]);
      return { at: search.at, count: search.hits.length };
    }

    function endSearch() {
      if (!search) return;
      for (const f of search.frames) f.remove();
      search = null;
      for (const c of chips.values()) c.classList.remove('nb-dim', 'nb-hit', 'nb-current');
      ink.dim(null);
    }

    // The pad changed while searching: the matches again, where they are
    // now (the view stays where it is).
    function refreshSearch() {
      if (!search || !search.terms.length) { paintSearch(); return; }
      const was = search.hits[search.at];
      search.hits = findHits();
      const same = was ? search.hits.findIndex((h) => (h.link && h.link === was.link) || (h.note && h.note === was.note)) : -1;
      search.at = search.hits.length ? Math.max(0, same) : -1;
      paintSearch();
      if (env.searchChanged) env.searchChanged({ at: search.at, count: search.hits.length });
    }

    function drawLabels() {
      const pad = env.pad();
      labels.replaceChildren();
      under.replaceChildren();
      chips.clear();
      labels.append(...noteLabels.all());
      for (const l of pad.links) {
        const c = linkChips.chip(l);
        chips.set(l.id, c);
        (l.fixed ? under : labels).appendChild(c);
      }
      const editing = chips.get(texts.editing() || '');
      if (editing) editing.classList.add('editing');
      lasso.prune();
      refreshSearch();
      tell('content');
    }

    function redrawChip(l) {
      const old = chips.get(l.id);
      const c = linkChips.chip(l);
      chips.set(l.id, c);
      if (old && old.isConnected) old.replaceWith(c);
      else (l.fixed ? under : labels).appendChild(c);
      lasso.prune();
      refreshSearch();
      tell('content');
    }

    const lasso = createLasso(layer, svg, {
      pad: env.pad,
      lift: (strokes, dx, dy) => ink.lift(strokes, dx, dy),
      chipOf: (id) => chips.get(id),
      moved: (strokes, links, dx, dy) => {
        shift(strokes.map((s) => s.id), links.map((l) => l.id), dx, dy);
        history.push({ kind: 'move', strokes: strokes.map((s) => s.id), links: links.map((l) => l.id), dx, dy });
        env.changed();
      },
      removed: (strokes, links) => removeItems(strokes, links),
    });

    function shift(strokeIds, linkIds, dx, dy) {
      shiftItems(env.pad(), strokeIds, linkIds, dx, dy);
      ink.redraw();
      drawLabels();
    }

    function removeItems(strokes, links) {
      const pad = env.pad();
      pad.strokes = pad.strokes.filter((s) => !strokes.includes(s));
      pad.links = pad.links.filter((l) => !links.includes(l));
      history.push({ kind: 'remove', strokes, notes: dropEmptyNotes(), links });
      ink.redraw();
      drawLabels();
      env.changed();
    }

    // A new link, once drawn, is kept inside the view's width and moved
    // down past anything it would cover (zotero-helper stacked them at the
    // middle, 14 px apart, where a clip could hide a quote). One dropped
    // where the user let it go (`avoid` false) stays there.
    function settle(link, vw, avoid = true) {
      const chip = chips.get(link.id);
      if (!chip) return;
      const w = chip.offsetWidth || 120;
      const h = chip.offsetHeight || 30;
      const right = vw - pan.x - 8;
      if (link.x + w > right) link.x = Math.round(Math.max(-pan.x + 8, right - w));
      if (link.x < -pan.x + 8) link.x = Math.round(-pan.x + 8);
      const boxes = env.pad().links.filter((l) => l !== link).map((l) => {
        const c = chips.get(l.id);
        return [l.x, l.y, l.x + (c ? c.offsetWidth : 120), l.y + (c ? c.offsetHeight : 30)];
      });
      for (let i = 0; avoid && i < 40; i++) {
        const hit = boxes.find((b) => link.x < b[2] + 6 && link.x + w > b[0] - 6 && link.y < b[3] + 6 && link.y + h > b[1] - 6);
        if (!hit) break;
        link.y = Math.round(hit[3] + 12);
      }
      chip.style.left = `${link.x}px`;
      chip.style.top = `${link.y}px`;
      lasso.prune();
      tell('content');
    }

    function render() {
      lasso.clear();
      pan = { ...env.pad().pan };
      drawLabels();
      cancelAnimationFrame(panFrame);
      showPan();
      ink.drawNow();
    }

    function setTool(t) {
      tool = t;
      host.classList.toggle('erasing', t === 'eraser');
      host.classList.toggle('lassoing', t === 'lasso');
      if (env.toolChanged) env.toolChanged(t);
    }

    function eraseAt(x, y, step) {
      const pad = env.pad();
      const hit = pad.strokes.filter((s) => touches(s, x, y, ERASE_PX));
      if (!hit.length) return;
      step.strokes.push(...hit);
      pad.strokes = pad.strokes.filter((s) => !hit.includes(s));
      step.notes.push(...dropEmptyNotes());
      ink.redraw();
      drawLabels();
    }

    let g = null;
    let heardAt = 0;
    const me = {};

    function armHold(gs) {
      gs.hold = setTimeout(() => {
        if (g !== gs || (gs.kind !== 'write' && gs.kind !== 'pan')) return;
        if (gs.kind === 'write') ink.drop();
        g = { ...gs, kind: 'lasso', hold: 0, borrowed: true };
        lasso.begin(...toCanvas(gs.from[0], gs.from[1]));
        if (env.lassoBorrowed) env.lassoBorrowed(true);
      }, LASSO_HOLD_MS);
    }

    function onDown(e) {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      const pen = e.pointerType === 'pen';
      if (g) {
        if (pen || performance.now() - heardAt > STALE_MS) end(g, 'pointercancel', g.last[0], g.last[1]);
        else return;
      }
      if (!pen && !e.isPrimary) return;
      e.preventDefault();
      capture(host, e.pointerId);
      stopGlide();
      origin = host.getBoundingClientRect();
      heardAt = performance.now();
      if (e.pointerType !== 'touch') penDown(me);
      const press = { id: e.pointerId, from: [e.clientX, e.clientY], last: [e.clientX, e.clientY], t0: heardAt, hold: 0 };
      const [x, y] = toCanvas(e.clientX, e.clientY);
      let cleared = false;
      if (lasso.has()) {
        if (lasso.contains(x, y)) { g = { ...press, kind: 'move' }; return; }
        lasso.clear();
        cleared = true;
      }
      if (e.pointerType === 'touch') {
        g = { ...press, kind: 'pan', panning: false, cleared };
        armHold(g);
        return;
      }
      if (tool === 'eraser' || e.buttons & ERASER_BUTTON) {
        g = { ...press, kind: 'erase', step: { strokes: [], notes: [] } };
        eraseAt(x, y, g.step);
        return;
      }
      if (tool === 'lasso') {
        g = { ...press, kind: 'lasso' };
        lasso.begin(x, y);
        return;
      }
      g = { ...press, kind: 'write', pts: [x, y], mouse: e.pointerType === 'mouse' };
      ink.begin(x, y, color, PEN_WIDTH);
      armHold(g);
    }

    function onMove(e) {
      if (!g || e.pointerId !== g.id) return;
      heardAt = performance.now();
      penHeard(heardAt);
      const [dx, dy] = [e.clientX - g.last[0], e.clientY - g.last[1]];
      g.last = [e.clientX, e.clientY];
      const away = Math.hypot(e.clientX - g.from[0], e.clientY - g.from[1]);
      if (g.hold && away > STILL_PX) { clearTimeout(g.hold); g.hold = 0; }
      const [x, y] = toCanvas(e.clientX, e.clientY);
      if (g.kind === 'pan') {
        if (!g.panning && away <= TAP_PX) return;
        g.panning = true;
        pan = { x: pan.x + dx, y: pan.y + dy };
        applyPan();
      } else if (g.kind === 'lasso') lasso.extend(x, y);
      else if (g.kind === 'move') lasso.drag(e.clientX - g.from[0], e.clientY - g.from[1]);
      else if (g.kind === 'erase') eraseAt(x, y, g.step);
      else {
        const n = g.pts.length;
        if (Math.hypot(x - g.pts[n - 2], y - g.pts[n - 1]) < 0.8) return;
        g.pts.push(Math.round(x * 10) / 10, Math.round(y * 10) / 10);
        ink.extend(g.pts);
      }
    }

    function noteAt(cx, cy) {
      const [x, y] = toCanvas(cx, cy);
      const pad = env.pad();
      for (let i = pad.strokes.length - 1; i >= 0; i--) if (touches(pad.strokes[i], x, y, HIT_PX)) return pad.strokes[i].note;
      return null;
    }

    function tapOn(cx, cy) {
      const id = noteAt(cx, cy);
      const note = id ? env.pad().notes.find((n) => n.id === id) : null;
      if (note && note.url) return env.goTo({ kind: 'page', url: note.url, title: note.title });
      const fixed = env.pad().links.find((l) => {
        const c = l.fixed && chips.get(l.id);
        const r = c && c.getBoundingClientRect();
        return r && cx >= r.left && cx <= r.right && cy >= r.top && cy <= r.bottom;
      });
      if (fixed) env.goTo(fixed.target);
      else texts.bareTap(cx, cy);
      return undefined;
    }

    function onUp(e) {
      if (g && e.pointerId === g.id) end(g, e.type, e.clientX, e.clientY);
    }

    function end(done, type, cx, cy) {
      if (g !== done) return;
      g = null;
      origin = null;
      clearTimeout(done.hold);
      penUp(me);
      const tap = type === 'pointerup' && Math.hypot(cx - done.from[0], cy - done.from[1]) < TAP_PX && performance.now() - done.t0 < LASSO_HOLD_MS;
      if (done.kind === 'lasso') {
        lasso.finish();
        if (done.borrowed && env.lassoBorrowed) env.lassoBorrowed(false);
        return;
      }
      if (done.kind === 'move') {
        if (tap) lasso.clear();
        else lasso.drop(cx - done.from[0], cy - done.from[1]);
        return;
      }
      if (done.kind === 'erase') {
        if (done.step.strokes.length) {
          history.push({ kind: 'erase', ...done.step });
          env.changed();
        }
        return;
      }
      if (done.kind === 'pan') {
        if (tap && !done.cleared) tapOn(cx, cy);
        else if (done.panning) { env.pad().pan = { ...pan }; env.changed(); }
        return;
      }
      if (done.mouse && tap) {
        ink.drop();
        tapOn(cx, cy);
        return;
      }
      if (type === 'pointercancel' && done.pts.length < 4) { ink.drop(); return; }
      ink.finish();
      finishStroke(done.pts);
    }

    function finishStroke(pts) {
      const pad = env.pad();
      const place = env.place();
      const visit = place ? place.visit : -1;
      let note = current && current.visit === visit && pad.notes.some((n) => n.id === current.note) ? current.note : '';
      if (!note) {
        note = newId('note');
        const n = { id: note, createdMs: Date.now() };
        if (place && place.url) { n.url = place.url; if (place.title) n.title = place.title; }
        pad.notes.push(n);
        current = { note, visit };
      }
      const stroke = { id: newId('s'), points: pts, width: PEN_WIDTH, color, note };
      pad.strokes.push(stroke);
      history.push({ kind: 'stroke', id: stroke.id });
      const lab = noteLabels.grew(stroke);
      if (lab && !lab.isConnected) labels.prepend(lab);
      if (search) refreshSearch();
      tell('content');
      env.changed();
    }

    function onWheel(e) {
      e.preventDefault();
      stopGlide();
      pan = { x: pan.x - e.deltaX, y: pan.y - e.deltaY };
      applyPan();
      env.pad().pan = { ...pan };
      env.changed();
    }

    const stopTouch = (e) => {
      if (!(e.target.closest && e.target.closest('.notepad-link-menu, .notepad-selection-menu, .notepad-text-editor'))) e.preventDefault();
    };
    host.addEventListener('pointerdown', onDown);
    window.addEventListener('pointermove', onMove, true);
    window.addEventListener('pointerup', onUp, true);
    window.addEventListener('pointercancel', onUp, true);
    host.addEventListener('wheel', onWheel, { passive: false });
    host.addEventListener('touchstart', stopTouch, { passive: false });
    host.addEventListener('touchmove', stopTouch, { passive: false });
    const sizeWatch = new ResizeObserver(() => {
      if (host.clientWidth && host.clientHeight) seen = { w: host.clientWidth, h: host.clientHeight };
    });
    sizeWatch.observe(host);

    render();

    return {
      render,
      follow,
      tool: () => tool,
      setTool,
      setColor(c) {
        color = c;
        const picked = lasso.strokes().filter((s) => s.color !== c);
        if (!picked.length) return lasso.strokes().length > 0;
        history.push({ kind: 'recolor', was: picked.map((s) => [s, s.color]) });
        for (const s of picked) s.color = c;
        ink.redraw();
        env.changed();
        return true;
      },
      // A link (or, with `clip`, a clip `w` wide) from a page: where it was
      // dropped (`at`, a client point), else at the middle of the view
      // while the pad is shown; while it is not, under everything on the
      // pad, and the pad panned to it for next time.
      addLink(target, clip, at) {
        const pad = env.pad();
        const shown = host.clientWidth > 0 && host.clientHeight > 0;
        const vw = shown ? host.clientWidth : seen.w;
        const vh = shown ? host.clientHeight : seen.h;
        const half = clip ? Math.max(80, Math.min(clip.w, vw * 0.7)) / 2 : 90;
        const link = { id: newId('link'), x: 0, y: 0, target };
        if (clip) { link.image = clip.image; if (clip.imageDark) link.imageDark = clip.imageDark; link.w = Math.round(half * 2); }
        if (shown && at) {
          const [x, y] = toCanvas(at.x, at.y);
          link.x = Math.round(x - (clip ? half : 12));
          link.y = Math.round(y - 12);
        } else if (shown) {
          const nudge = (pad.links.length % 5) * 14;
          link.x = Math.round(vw / 2 - pan.x - half + nudge);
          link.y = Math.round(vh / 2 - pan.y - (clip ? 60 : 12) + nudge);
        } else {
          let bottom = -Infinity;
          for (const s of pad.strokes) bottom = Math.max(bottom, strokeBox(s)[3]);
          for (const l of pad.links) { const c = chips.get(l.id); bottom = Math.max(bottom, l.y + (c && c.offsetHeight ? c.offsetHeight : l.h || (l.image ? 200 : 40))); }
          link.x = Math.round(Math.max(20, 24 - pan.x));
          link.y = Math.round(bottom === -Infinity ? 40 - pan.y : bottom + 28);
          pan = { x: pan.x, y: Math.round(Math.min(pan.y, vh * 0.35 - link.y)) };
          pad.pan = { ...pan };
          showPan();
        }
        pad.links.push(link);
        history.push({ kind: 'link', id: link.id });
        drawLabels();
        settle(link, vw, !at);
        env.changed();
        return link;
      },
      // A typed note with `text` (dropped text from outside a page), its
      // top left where it was let go.
      addText(text, target, at) {
        const [x, y] = toCanvas(at.x, at.y);
        const link = { id: newId('text'), x: Math.round(x), y: Math.round(y), target, text };
        env.pad().links.push(link);
        history.push({ kind: 'link', id: link.id });
        drawLabels();
        settle(link, host.clientWidth || seen.w, false);
        env.changed();
        return link;
      },
      // A picture made later (its dark copy) is given to its link.
      setDark(link, dark) {
        if (!env.pad().links.includes(link) || !dark) return;
        link.imageDark = dark;
        redrawChip(link);
        env.changed();
      },
      undo() {
        texts.commit();
        const step = history.pop();
        if (!step) return;
        lasso.clear();
        takeBack(env.pad(), step);
        ink.redraw();
        drawLabels();
        env.changed();
      },
      canUndo: () => history.length > 0,
      clear() {
        texts.commit();
        const pad = env.pad();
        if (!pad.strokes.length && !pad.links.length) return;
        history.push({ kind: 'clear', strokes: pad.strokes, notes: pad.notes, links: pad.links });
        pad.strokes = [];
        pad.notes = [];
        pad.links = [];
        current = null;
        render();
        env.changed();
      },
      commitText: () => texts.commit(),
      view: () => ({ x: -pan.x, y: -pan.y, w: host.clientWidth, h: host.clientHeight }),
      linkBoxes: () => env.pad().links.map((l) => {
        const c = chips.get(l.id);
        const col = l.text !== undefined ? '#8a857d' : l.image ? '#9a958d' : l.target.kind === 'quote' ? '#ffd400' : RED_INK;
        return { x: l.x, y: l.y, w: c ? c.offsetWidth : 120, h: c ? c.offsetHeight : 24, color: col };
      }),
      panTo(x, y, done) {
        stopGlide();
        pan = { x: Math.round(host.clientWidth / 2 - x), y: Math.round(host.clientHeight / 2 - y) };
        applyPan();
        if (!done) return;
        env.pad().pan = { ...pan };
        env.changed();
      },
      watch(fn) { watchers.add(fn); return () => watchers.delete(fn); },
      closeMenus() { linkChips.closeMenu(); lasso.clear(); },
      search: setSearch,
      stepSearch,
      endSearch,
    };
  }

  // ---- the minimap (minimap.ts) ------------------------------------------------------------

  const MAP_MARGIN = 5;

  function createMinimap(box, surface, pad) {
    const canvas = el('canvas', 'notepad-minimap-ink');
    const ctx = canvas.getContext('2d');
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'notepad-minimap-svg');
    const g = document.createElementNS(SVG_NS, 'g');
    const links = document.createElementNS(SVG_NS, 'g');
    const frame = document.createElementNS(SVG_NS, 'rect');
    frame.setAttribute('class', 'notepad-minimap-frame');
    frame.setAttribute('vector-effect', 'non-scaling-stroke');
    g.append(links, frame);
    svg.appendChild(g);
    box.append(canvas, svg);
    let drawnFit = '';
    let content = null;
    let fit = { s: 1, tx: 0, ty: 0 };
    let world = [0, 0, 0, 0];
    let dragging = false;

    function drawStrokes() {
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      const w = Math.round(box.clientWidth * dpr);
      const h = Math.round(box.clientHeight * dpr);
      if (canvas.width !== w) canvas.width = w;
      if (canvas.height !== h) canvas.height = h;
      drawnFit = `${fit.s} ${fit.tx} ${fit.ty} ${w} ${h}`;
      if (!w || !h) return;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, w, h);
      ctx.setTransform(fit.s * dpr, 0, 0, fit.s * dpr, fit.tx * dpr, fit.ty * dpr);
      ctx.lineWidth = 1 / fit.s;
      ctx.lineCap = ctx.lineJoin = 'round';
      const inkCol = getComputedStyle(box).getPropertyValue('--notepad-ink').trim() || '#000';
      let col = null;
      for (const s of pad().strokes) {
        if (s.points.length < 2) continue;
        if (s.color !== col) ctx.strokeStyle = (col = s.color) || inkCol;
        ctx.stroke(strokeLine(s));
      }
    }

    function drawContent() {
      drawnFit = '';
      links.replaceChildren();
      let b = [Infinity, Infinity, -Infinity, -Infinity];
      const grow = (x0, y0, x1, y1) => { b = [Math.min(b[0], x0), Math.min(b[1], y0), Math.max(b[2], x1), Math.max(b[3], y1)]; };
      for (const s of pad().strokes) if (s.points.length >= 2) grow(...strokeBox(s));
      for (const lb of surface.linkBoxes()) {
        const r = document.createElementNS(SVG_NS, 'rect');
        r.setAttribute('x', String(lb.x));
        r.setAttribute('y', String(lb.y));
        r.setAttribute('width', String(lb.w));
        r.setAttribute('height', String(lb.h));
        r.setAttribute('rx', '4');
        r.style.fill = lb.color;
        links.appendChild(r);
        grow(lb.x, lb.y, lb.x + lb.w, lb.y + lb.h);
      }
      content = Number.isFinite(b[0]) ? b : null;
    }

    function place() {
      const v = surface.view();
      frame.setAttribute('x', String(v.x));
      frame.setAttribute('y', String(v.y));
      frame.setAttribute('width', String(v.w));
      frame.setAttribute('height', String(v.h));
      if (dragging) return;
      let [x0, y0, x1, y1] = [v.x, v.y, v.x + v.w, v.y + v.h];
      if (content) [x0, y0, x1, y1] = [Math.min(x0, content[0]), Math.min(y0, content[1]), Math.max(x1, content[2]), Math.max(y1, content[3])];
      const w = box.clientWidth;
      const h = box.clientHeight;
      if (!w || !h || x1 <= x0 || y1 <= y0) return;
      world = [x0, y0, x1, y1];
      const s = Math.min((w - 2 * MAP_MARGIN) / (x1 - x0), (h - 2 * MAP_MARGIN) / (y1 - y0));
      fit = { s, tx: (w - (x1 - x0) * s) / 2 - x0 * s, ty: MAP_MARGIN - y0 * s };
      g.setAttribute('transform', `translate(${fit.tx} ${fit.ty}) scale(${s})`);
      if (drawnFit !== `${fit.s} ${fit.tx} ${fit.ty} ${canvas.width} ${canvas.height}`) drawStrokes();
    }

    let queued = false;
    let contentStale = false;
    let waitTimer = 0;
    const schedule = () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        if (contentStale && penRested(WRITING_PAUSE_MS)) { contentStale = false; drawContent(); }
        place();
      });
    };
    const waitForPause = () => {
      clearTimeout(waitTimer);
      if (penRested(WRITING_PAUSE_MS)) schedule();
      else waitTimer = setTimeout(waitForPause, 250);
    };
    const onChange = (what) => {
      if (what === 'content') { contentStale = true; waitForPause(); } else schedule();
    };
    const unwatch = surface.watch(onChange);
    const resize = new ResizeObserver(() => place());
    resize.observe(box);

    const toCanvas = (e) => {
      const r = box.getBoundingClientRect();
      const v = surface.view();
      const x = (e.clientX - r.left - fit.tx) / fit.s;
      const y = (e.clientY - r.top - fit.ty) / fit.s;
      return [
        Math.max(world[0] + v.w / 2, Math.min(x, world[2] - v.w / 2)),
        Math.max(world[1] + v.h / 2, Math.min(y, world[3] + v.h / 2)),
      ];
    };
    const onDown = (e) => {
      if (!e.isPrimary || (e.pointerType === 'mouse' && e.button !== 0)) return;
      e.preventDefault();
      e.stopPropagation();
      capture(box, e.pointerId);
      dragging = true;
      surface.panTo(...toCanvas(e), false);
      const move = (ev) => surface.panTo(...toCanvas(ev), false);
      const up = (ev) => {
        box.removeEventListener('pointermove', move);
        box.removeEventListener('pointerup', up);
        box.removeEventListener('pointercancel', up);
        surface.panTo(...toCanvas(ev), true);
        dragging = false;
        place();
      };
      box.addEventListener('pointermove', move);
      box.addEventListener('pointerup', up);
      box.addEventListener('pointercancel', up);
    };
    box.addEventListener('pointerdown', onDown);

    drawContent();
    place();

    return {
      destroy() {
        clearTimeout(waitTimer);
        unwatch();
        resize.disconnect();
        box.removeEventListener('pointerdown', onDown);
        canvas.width = canvas.height = 0;
        canvas.remove();
        svg.remove();
      },
    };
  }

  // ---- a clip as dark pages draw it (zotero-helper's darkPagesCopy) ---------------------------

  const DARK_FILTER = 'invert(1) hue-rotate(180deg)';
  const HUE_180 = [-0.574, 1.43, 0.144, 0.426, 0.43, 0.144, 0.426, 1.43, -0.856];

  // `src`: a data URL the app made (a JPEG of its own web view). Resolves
  // to the white-on-black copy, or '' if it cannot be drawn.
  function darkCopy(src) {
    return new Promise((resolve) => {
      const img = new Image();
      img.onload = () => {
        try {
          const out = document.createElement('canvas');
          out.width = img.naturalWidth;
          out.height = img.naturalHeight;
          const ctx = out.getContext('2d');
          if (!ctx || !out.width || !out.height) { resolve(''); return; }
          if ('filter' in CanvasRenderingContext2D.prototype) {
            ctx.filter = DARK_FILTER;
            ctx.drawImage(img, 0, 0);
          } else {
            ctx.drawImage(img, 0, 0);
            const data = ctx.getImageData(0, 0, out.width, out.height);
            const d = data.data;
            const m = HUE_180;
            for (let i = 0; i < d.length; i += 4) {
              const r = 255 - d[i], gr = 255 - d[i + 1], b = 255 - d[i + 2];
              d[i] = m[0] * r + m[1] * gr + m[2] * b;
              d[i + 1] = m[3] * r + m[4] * gr + m[5] * b;
              d[i + 2] = m[6] * r + m[7] * gr + m[8] * b;
            }
            ctx.putImageData(data, 0, 0);
          }
          resolve(out.toDataURL('image/jpeg', 0.82));
        } catch (e) { resolve(''); }
      };
      img.onerror = () => resolve('');
      img.src = src;
    });
  }

  // ---- the notebook: its bar over the canvas (NotepadBody.tsx) ----------------------------------

  const COLOR_KEY = 'nbPenColor';
  const MAP_KEY = 'nbMap';
  const FOLLOW_KEY = 'nbFollow';
  const SAVE_MS = 600;

  // `root`: the panel to fill. `env` (from chrome.js):
  //   invoke(cmd, args) → Promise; toast(text);
  //   place() → { url, title } of the page shown (null: none, or private);
  //   goTo(target) — open a link's page; openLink(href) — a typed link;
  //   setClipMode(on) → Promise<boolean>; dragLink(phase, x, y, fromY)
  //   → Promise<target|null> on 'end'; close().
  function create(root, env) {
    let pad = emptyPad();
    let loaded = false;
    let surface = null;
    let minimap = null;
    let visit = 0;
    let lastPage = '';

    // --- saving (model.ts): the whole pad, a moment after each change,
    // once the pen has paused; at once when the app is put away.
    let saveTimer = 0;
    let pending = false;
    function flush() {
      clearTimeout(saveTimer);
      if (!pending || !loaded) return;
      pending = false;
      env.invoke('save_notebook', { notebook: JSON.stringify(pad) }).catch((e) => env.toast(String(e)));
    }
    function saveSoon() {
      pending = true;
      clearTimeout(saveTimer);
      const wait = () => {
        const rest = penRestedMs();
        if (rest >= WRITING_PAUSE_MS) flush();
        else saveTimer = setTimeout(wait, WRITING_PAUSE_MS - rest);
      };
      saveTimer = setTimeout(wait, SAVE_MS);
    }
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') { if (surface) surface.commitText(); flush(); } });
    window.addEventListener('pagehide', flush);

    // --- the bar
    root.classList.add('notepad');
    const bar = el('div', 'notepad-bar');
    const fromPages = el('div', 'notepad-from-pages');
    fromPages.setAttribute('role', 'group');
    fromPages.setAttribute('aria-label', 'Bring in from pages');
    const quoteBtn = el('button', 'notepad-quote-btn');
    quoteBtn.type = 'button';
    quoteBtn.title = 'Quote — drag onto a page to link to the sentence there';
    quoteBtn.setAttribute('aria-label', 'Link to a sentence on a page');
    quoteBtn.appendChild(ICONS.quote());
    const clipBtn = el('button', 'notepad-clip-btn');
    clipBtn.type = 'button';
    clipBtn.title = 'Clip — drag over the page to put that part of it in the notebook (⌥-drag works any time)';
    clipBtn.setAttribute('aria-label', 'Clip part of the page');
    clipBtn.setAttribute('aria-pressed', 'false');
    clipBtn.appendChild(ICONS.scissors());
    fromPages.append(quoteBtn, clipBtn);

    const toolBtn = (svg, title, label) => {
      const b = el('button', 'notepad-tool');
      b.type = 'button';
      b.title = title;
      b.setAttribute('aria-label', label);
      b.appendChild(svg);
      return b;
    };
    const mapBtn = toolBtn(ICONS.map(), 'Show a minimap of the whole notebook', 'Minimap');
    const followBtn = toolBtn(ICONS.follow(), 'Follow the page shown: the notebook shows what was written about it', 'Follow the page');
    const searchBtn = toolBtn(ICONS.search(), 'Search the notebook (⌘F here)', 'Search the notebook');
    const grab = el('div', 'notepad-bar-grab');
    const modes = el('div', 'notepad-modes');
    modes.setAttribute('role', 'radiogroup');
    modes.setAttribute('aria-label', 'What the pen does');
    const modeBtn = (child, title, label) => {
      const b = el('button', 'notepad-mode');
      b.type = 'button';
      b.setAttribute('role', 'radio');
      b.title = title;
      b.setAttribute('aria-label', label);
      b.appendChild(child);
      modes.appendChild(b);
      return b;
    };
    const dot = (c) => { const d = el('span', 'notepad-mode-dot'); if (c) d.style.setProperty('--swatch', c); return d; };
    const inkBtn = modeBtn(dot(''), 'Write in ink (black, white in the dark theme)', 'Ink');
    const redBtn = modeBtn(dot(RED_INK), 'Write in red', 'Red');
    const eraserBtn = modeBtn(ICONS.eraser(), 'Eraser — takes out what it passes over', 'Eraser');
    const lassoBtn = modeBtn(ICONS.lasso(), 'Lasso — draw a loop round what to select (or hold still)', 'Lasso');
    const undoBtn = toolBtn(ICONS.undo(), 'Undo (⌘Z)', 'Undo');
    undoBtn.classList.add('notepad-undo');
    undoBtn.disabled = true;
    const closeBtn = el('button', 'icon notepad-close', '×');
    closeBtn.type = 'button';
    closeBtn.title = 'Close the notebook';
    closeBtn.setAttribute('aria-label', 'Close the notebook');
    bar.append(fromPages, mapBtn, followBtn, searchBtn, grab, modes, undoBtn, closeBtn);

    // Search (2026-11-16): a field under the bar; ↩ / ⇧↩ (or the arrows)
    // step through the matches, Escape closes it.
    const searchRow = el('div', 'notepad-search');
    searchRow.hidden = true;
    const searchInput = el('input');
    searchInput.type = 'text';
    searchInput.setAttribute('enterkeyhint', 'search');
    searchInput.placeholder = 'Search notes, quotes and pages';
    searchInput.setAttribute('aria-label', 'Search the notebook');
    searchInput.setAttribute('autocapitalize', 'off');
    searchInput.setAttribute('autocorrect', 'off');
    searchInput.spellcheck = false;
    searchInput.maxLength = 200;
    const searchCount = el('span', 'notepad-search-count');
    searchCount.setAttribute('aria-live', 'polite');
    const stepBtn = (d, label, title) => {
      const b = el('button', 'notepad-tool', d < 0 ? '‹' : '›');
      b.type = 'button';
      b.title = title;
      b.setAttribute('aria-label', label);
      b.addEventListener('click', () => stepSearch(d));
      return b;
    };
    const searchPrev = stepBtn(-1, 'Previous match', 'Previous (⇧↩)');
    const searchNext = stepBtn(1, 'Next match', 'Next (↩)');
    const searchClose = el('button', 'notepad-tool', '×');
    searchClose.type = 'button';
    searchClose.title = 'Close search (Esc)';
    searchClose.setAttribute('aria-label', 'Close search');
    searchRow.append(searchInput, searchCount, searchPrev, searchNext, searchClose);

    const main = el('div', 'notepad-main');
    const mapBox = el('div', 'notepad-minimap');
    const canvas = el('div', 'notepad-canvas');
    const trash = el('button', 'notepad-trash');
    trash.type = 'button';
    trash.title = 'Clear the notebook';
    trash.setAttribute('aria-label', 'Clear the notebook');
    trash.appendChild(ICONS.trashBig());
    main.append(canvas, trash);
    root.append(bar, searchRow, main);

    // --- state of the bar
    let tool = 'pen';
    let borrowed = false;
    let color = pref(COLOR_KEY, '') === RED_INK ? RED_INK : '';
    let map = pref(MAP_KEY, '0') === '1';
    let followOn = pref(FOLLOW_KEY, '0') === '1';
    let clipping = false;
    let confirming = null;

    function renderBar() {
      const shown = borrowed ? 'lasso' : tool;
      const on = (b, v) => { b.classList.toggle('on', v); b.setAttribute('aria-checked', String(v)); };
      on(inkBtn, shown === 'pen' && color === '');
      on(redBtn, shown === 'pen' && color === RED_INK);
      on(eraserBtn, shown === 'eraser');
      on(lassoBtn, shown === 'lasso');
      mapBtn.classList.toggle('active', map);
      mapBtn.setAttribute('aria-pressed', String(map));
      followBtn.classList.toggle('active', followOn);
      followBtn.setAttribute('aria-pressed', String(followOn));
      clipBtn.classList.toggle('on', clipping);
      clipBtn.setAttribute('aria-pressed', String(clipping));
      undoBtn.disabled = !(surface && surface.canUndo());
    }

    function setMap(onOff) {
      map = onOff;
      setPref(MAP_KEY, onOff ? '1' : '0');
      if (minimap) { minimap.destroy(); minimap = null; }
      main.classList.toggle('with-map', onOff);
      if (onOff) {
        main.prepend(mapBox);
        if (surface) minimap = createMinimap(mapBox, surface, () => pad);
      } else mapBox.remove();
      renderBar();
    }

    function pickColor(c) {
      color = c;
      setPref(COLOR_KEY, c);
      if (!surface) return;
      const recoloured = surface.setColor(c);
      if (!recoloured && surface.tool() !== 'pen') surface.setTool('pen');
      renderBar();
    }

    inkBtn.addEventListener('click', () => pickColor(''));
    redBtn.addEventListener('click', () => pickColor(RED_INK));
    eraserBtn.addEventListener('click', () => surface && surface.setTool('eraser'));
    lassoBtn.addEventListener('click', () => surface && surface.setTool('lasso'));
    undoBtn.addEventListener('click', () => surface && surface.undo());
    mapBtn.addEventListener('click', () => setMap(!map));
    followBtn.addEventListener('click', () => {
      followOn = !followOn;
      setPref(FOLLOW_KEY, followOn ? '1' : '0');
      renderBar();
      const p = env.place();
      if (followOn && surface && p) surface.follow(p.url);
    });
    closeBtn.addEventListener('click', () => env.close());

    function showCount(r) {
      const q = searchInput.value.trim();
      searchCount.textContent = !q ? '' : r.count ? `${r.at + 1}/${r.count}` : 'None';
      searchCount.classList.toggle('none', !!q && !r.count);
      searchPrev.disabled = searchNext.disabled = r.count < 2;
    }
    function openSearch() {
      searchRow.hidden = false;
      searchBtn.classList.add('active');
      searchBtn.setAttribute('aria-pressed', 'true');
      searchInput.focus();
      searchInput.select();
      if (surface && searchInput.value.trim()) showCount(surface.search(searchInput.value));
    }
    function closeSearch() {
      searchRow.hidden = true;
      searchBtn.classList.remove('active');
      searchBtn.setAttribute('aria-pressed', 'false');
      if (surface) surface.endSearch();
      searchCount.textContent = '';
    }
    function stepSearch(d) {
      if (!surface) return;
      showCount(surface.stepSearch(d));
    }
    let searchTimer = 0;
    searchInput.addEventListener('input', () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(() => { if (surface) showCount(surface.search(searchInput.value)); }, 120);
    });
    searchInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        clearTimeout(searchTimer);
        if (surface && e.key === 'Enter' && !searchCount.textContent && searchInput.value.trim()) { showCount(surface.search(searchInput.value)); return; }
        stepSearch(e.shiftKey || e.key === 'ArrowUp' ? -1 : 1);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        closeSearch();
      }
    });
    searchBtn.addEventListener('click', () => (searchRow.hidden ? openSearch() : closeSearch()));
    searchClose.addEventListener('click', closeSearch);

    // The scissors: clip mode in the page shown, on or off.
    clipBtn.addEventListener('click', () => {
      const on = !clipping;
      env.setClipMode(on).then((ok) => {
        clipping = on && ok !== false;
        renderBar();
      });
    });

    // The quote button: dragged out onto a page, a dashed line from it to
    // the pointer (the page draws the rest over itself); let go there,
    // the sentence there is linked.
    const lineSvg = document.createElementNS(SVG_NS, 'svg');
    lineSvg.setAttribute('class', 'notepad-link-line');
    lineSvg.setAttribute('aria-hidden', 'true');
    const line = document.createElementNS(SVG_NS, 'line');
    const lineEnd = document.createElementNS(SVG_NS, 'circle');
    lineEnd.setAttribute('r', '5');
    lineSvg.append(line, lineEnd);
    quoteBtn.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      const r = quoteBtn.getBoundingClientRect();
      const x1 = r.left + r.width / 2;
      const y1 = r.top + r.height / 2;
      const x0 = e.clientX;
      const y0 = e.clientY;
      let moved = false;
      capture(quoteBtn, e.pointerId);
      quoteBtn.classList.add('on');
      const move = (ev) => {
        moved ||= Math.hypot(ev.clientX - x0, ev.clientY - y0) > 4;
        if (!moved) return;
        line.setAttribute('x1', String(x1)); line.setAttribute('y1', String(y1));
        line.setAttribute('x2', String(ev.clientX)); line.setAttribute('y2', String(ev.clientY));
        lineEnd.setAttribute('cx', String(ev.clientX)); lineEnd.setAttribute('cy', String(ev.clientY));
        if (!lineSvg.isConnected) document.body.appendChild(lineSvg);
        env.dragLink('move', ev.clientX, ev.clientY, y1);
      };
      const up = (ev) => {
        quoteBtn.removeEventListener('pointermove', move);
        quoteBtn.removeEventListener('pointerup', up);
        quoteBtn.removeEventListener('pointercancel', up);
        quoteBtn.classList.remove('on');
        lineSvg.remove();
        if (!moved || ev.type === 'pointercancel') { env.dragLink('cancel', 0, 0, 0); return; }
        env.dragLink('end', ev.clientX, ev.clientY, y1).then((target) => {
          if (target && surface) surface.addLink(target);
        });
      };
      quoteBtn.addEventListener('pointermove', move);
      quoteBtn.addEventListener('pointerup', up);
      quoteBtn.addEventListener('pointercancel', up);
    });

    // Clearing, asked first: any press elsewhere drops the question.
    function closeConfirm() {
      if (!confirming) return;
      confirming.remove();
      confirming = null;
      window.removeEventListener('pointerdown', awayConfirm, true);
    }
    function awayConfirm(e) {
      if (!(e.target.closest && e.target.closest('.notepad-confirm, .notepad-trash'))) closeConfirm();
    }
    trash.addEventListener('click', () => {
      if (confirming) { closeConfirm(); return; }
      confirming = el('div', 'notepad-confirm');
      confirming.setAttribute('role', 'dialog');
      confirming.setAttribute('aria-label', 'Clear the notebook');
      const yes = el('button', 'notepad-confirm-clear', 'Clear');
      yes.type = 'button';
      const no = el('button', 'notepad-confirm-cancel', 'Cancel');
      no.type = 'button';
      yes.addEventListener('click', () => { closeConfirm(); if (surface) surface.clear(); });
      no.addEventListener('click', closeConfirm);
      confirming.append(el('span', '', 'Clear the whole notebook?'), yes, no);
      main.appendChild(confirming);
      window.addEventListener('pointerdown', awayConfirm, true);
    });

    // Dragged in (2026-11-16): selected text or an image from a page, let
    // go on the canvas, lands there (the chrome takes what it is from the
    // page itself: `env.drop`); text from elsewhere becomes a typed note.
    // A note being typed takes a drop into its words as usual.
    let dropDepth = 0;
    const dropShown = (on) => main.classList.toggle('drop-target', on);
    const intoText = (e) => e.target && e.target.closest && e.target.closest('textarea');
    canvas.addEventListener('dragenter', (e) => {
      if (intoText(e)) return;
      e.preventDefault();
      dropDepth++;
      dropShown(true);
    });
    canvas.addEventListener('dragover', (e) => {
      if (intoText(e)) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
      dropShown(true);
    });
    canvas.addEventListener('dragleave', () => {
      dropDepth = Math.max(0, dropDepth - 1);
      if (!dropDepth) dropShown(false);
    });
    canvas.addEventListener('drop', (e) => {
      if (intoText(e)) return;
      e.preventDefault();
      e.stopPropagation();
      dropDepth = 0;
      dropShown(false);
      const dt = e.dataTransfer;
      const text = dt ? dt.getData('text/plain') : '';
      const files = !!(dt && dt.files && dt.files.length);
      const at = { x: e.clientX, y: e.clientY };
      Promise.resolve(env.drop({ text, files })).then((got) => {
        if (!got || !surface) return;
        if (got.kind === 'clip') placeClip(got.image, got.target, at);
        else if (got.kind === 'text') surface.addText(got.text, got.target, at);
        else surface.addLink(got.target, null, at);
      });
    });

    // A press in the notebook gives it the keyboard (the canvas's own
    // presses take no focus), so ⌘Z and ⌘F act on it.
    root.tabIndex = -1;
    root.addEventListener('pointerdown', (e) => {
      if (e.target.closest && e.target.closest('input, textarea, button')) return;
      if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
    }, true);

    // ⌘Z in the notebook (not while typing a note).
    root.addEventListener('keydown', (e) => {
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== 'z' || e.shiftKey) return;
      if (e.target.closest && e.target.closest('textarea, input')) return;
      e.preventDefault();
      if (surface) surface.undo();
    });

    // --- the surface, once the notebook has loaded
    env.invoke('load_notebook').then((json) => {
      if (json) {
        try {
          const p = JSON.parse(json);
          if (p && Array.isArray(p.strokes)) pad = { ...emptyPad(), ...p };
        } catch (e) { /* kept as empty */ }
      }
    }).catch((e) => env.toast(`Couldn't read the notebook: ${e}`)).finally(() => {
      loaded = true;
      surface = createSurface(canvas, {
        pad: () => pad,
        changed: () => { saveSoon(); renderBar(); },
        place: () => {
          const p = env.place();
          return p ? { url: p.url, title: p.title, visit } : { visit };
        },
        goTo: (t) => env.goTo(t),
        openLink: (href) => env.openLink(href),
        toolChanged: (t) => { tool = t; renderBar(); },
        lassoBorrowed: (b) => { borrowed = b; renderBar(); },
        searchChanged: (r) => { if (!searchRow.hidden) showCount(r); },
        flush,
      });
      surface.setColor(color);
      setMap(map);
      renderBar();
      for (const f of waiting.splice(0)) f();
    });
    const waiting = [];
    const whenReady = (f) => { if (surface) f(); else waiting.push(f); };

    // A clip (an app-made JPEG), as wide as it was on screen (at most
    // 600 px), at `at` or where links from pages go; then its dark copy.
    function placeClip(image, target, at) {
      const probe = new Image();
      probe.onload = () => {
        const w = Math.min(probe.naturalWidth / Math.min(2, window.devicePixelRatio || 1), 600);
        const link = surface.addLink(target, { image, w: Math.max(80, Math.round(w)) }, at);
        darkCopy(image).then((dark) => surface.setDark(link, dark));
      };
      probe.onerror = () => env.toast('That clip could not be shown.');
      probe.src = image;
    }

    return {
      // The page shown changed (another page, or another tab): a stroke
      // after it starts a new note; the pad follows it, if asked to.
      pageChanged(url) {
        const page = pageOf(url || '');
        if (page === lastPage) return;
        lastPage = page;
        visit++;
        if (followOn && page) whenReady(() => surface.follow(page));
      },
      // Clip mode ended in the page (a clip made there, Escape), or the
      // page changed under it.
      clipEnded() { clipping = false; renderBar(); },
      clipping: () => clipping,
      // Something from a page: a quote (target only), or a clip.
      addQuote(target) { whenReady(() => surface.addLink(target)); },
      addClip(image, target) { whenReady(() => placeClip(image, target, null)); },
      shown() {
        whenReady(() => { surface.render(); renderBar(); });
      },
      // ⌘F / ⌘G / ⌘⇧G while the notebook has the focus: its own search.
      hasFocus: () => root.contains(document.activeElement),
      findKey(name) {
        if (name === 'find' || searchRow.hidden) openSearch();
        else stepSearch(name === 'find_prev' ? -1 : 1);
      },
      hidden() {
        if (surface) { surface.commitText(); surface.closeMenus(); }
        closeConfirm();
        flush();
      },
      flush,
    };
  }

  window.SnorkleNotebook = { create };
})();
