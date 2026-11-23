// The Notebook (see notebook/base.js): the lasso; each note's page above it.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { ICONS, SVG_NS, el, inPolygon, onPress, pathD, siteOf, strokeBox } = NB;

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

    // Items selected by other means (a paste): framed as a lasso's are.
    function selectItems(s, l) {
      clear();
      strokes = s;
      links = l;
      if (!strokes.length && !links.length) return;
      showFrame();
      window.addEventListener('keydown', onKey);
    }

    return {
      begin, extend, finish, drag, drop, clear, prune, remove, selectItems,
      selected: () => ({ strokes, links }),
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

  Object.assign(NB, { FRAME_PAD, createLasso, createNoteLabels });
})();
