// The Notebook (see notebook/base.js): ink: every stroke on one canvas.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { bounds, el } = NB;

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

  Object.assign(NB, { MAX_DPR, shapes, outline, shapeOf, strokeBox, strokeLine, createPadInk });
})();
