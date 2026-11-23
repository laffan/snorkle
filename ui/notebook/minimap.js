// The Notebook (see notebook/base.js): the minimap; dark copies of clips.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { SVG_NS, WRITING_PAUSE_MS, capture, el, penRested, strokeLine } = NB;

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

  Object.assign(NB, { MAP_MARGIN, createMinimap, DARK_FILTER, HUE_180, darkCopy });
})();
