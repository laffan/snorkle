// The Notebook (see notebook/base.js): the canvas.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { SVG_NS, bounds, capture, createLasso, createLinkChips, createNoteLabels, createPadInk, createPadSearch, createTextNotes, dropEmpty, el, itemsText, newId, pageOf, penDown, penHeard, penUp, shiftItems, strokeBox, takeBack, touches } = NB;

  // ---- the canvas (surface.ts) ------------------------------------------------------------

  const PEN_WIDTH = 2;
  const RED_INK = '#ff5f57'; // also notebook.css --nb-red (typed notes' headings)
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

    // Search in the pad: notebook/search.js (the closure's state it reads is
    // passed in; `pan` changes, so it is read through a function).
    const padSearch = createPadSearch({ env, chips, layer, labels, ink, host, pan: () => pan, glideTo });
    const { setSearch, stepSearch, endSearch, refreshSearch } = padSearch;

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
      // Only a pencil draws (owner request 2026-11-16): a finger, a mouse
      // or a trackpad pans, a tap is a tap, and holding still selects (the
      // lasso). The eraser and lasso tools, when chosen, work with a mouse.
      const mouseTool = e.pointerType === 'mouse' && (tool === 'eraser' || tool === 'lasso');
      if (e.pointerType === 'touch' || (e.pointerType !== 'pen' && !mouseTool)) {
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
      if (padSearch.active()) refreshSearch();
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
      addText(text, target, at, avoid = false) {
        const [x, y] = toCanvas(at.x, at.y);
        const link = { id: newId('text'), x: Math.round(x), y: Math.round(y), target, text };
        env.pad().links.push(link);
        history.push({ kind: 'link', id: link.id });
        drawLabels();
        settle(link, host.clientWidth || seen.w, avoid);
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
      // Copy (2026-11-16): the selected strokes (with their notes) and
      // links, as data to paste back in, and as plain text for other apps.
      copyItems() {
        const sel = lasso.selected();
        if (!sel.strokes.length && !sel.links.length) return null;
        const noteIds = new Set(sel.strokes.map((st) => st.note));
        const items = JSON.parse(JSON.stringify({
          strokes: sel.strokes,
          notes: env.pad().notes.filter((n) => noteIds.has(n.id)),
          links: sel.links,
        }));
        return { items, text: itemsText(sel.links) || 'Handwriting from the Snorkle notebook' };
      },
      cutItems() {
        const c = this.copyItems();
        if (c) lasso.remove();
        return c;
      },
      // Items copied before: placed `step` × 24 px down and right of where
      // they were while that is in view, else at the view's top left; new
      // ids; selected, to be moved at once. One undo takes them away.
      pasteItems(items, step) {
        const pad = env.pad();
        let box = [Infinity, Infinity, -Infinity, -Infinity];
        const grow = (b) => { box = [Math.min(box[0], b[0]), Math.min(box[1], b[1]), Math.max(box[2], b[2]), Math.max(box[3], b[3])]; };
        for (const st of items.strokes) grow(bounds(st.points));
        for (const l of items.links) grow([l.x, l.y, l.x + (l.w || 120), l.y + (l.h || 40)]);
        if (!Number.isFinite(box[0])) return;
        const v = { x: -pan.x, y: -pan.y, w: host.clientWidth || seen.w, h: host.clientHeight || seen.h };
        const inView = box[0] >= v.x && box[1] >= v.y && box[0] < v.x + v.w - 40 && box[1] < v.y + v.h - 40;
        const dx = inView ? 24 * step : Math.round(v.x + 30 - box[0]);
        const dy = inView ? 24 * step : Math.round(v.y + 40 - box[1]);
        const ids = new Map();
        const notes = items.notes.map((n) => { const id = newId('note'); ids.set(n.id, id); return { ...n, id, createdMs: Date.now() }; });
        const strokes = items.strokes.map((st) => {
          if (!ids.has(st.note)) { const id = newId('note'); ids.set(st.note, id); notes.push({ id, createdMs: Date.now() }); }
          return { ...st, id: newId('s'), note: ids.get(st.note), points: st.points.map((p, i) => Math.round((p + (i % 2 ? dy : dx)) * 10) / 10) };
        });
        const links = items.links.map((l) => ({ ...l, id: newId(l.text !== undefined ? 'text' : 'link'), x: Math.round(l.x + dx), y: Math.round(l.y + dy), fixed: undefined }));
        pad.notes.push(...notes);
        pad.strokes.push(...strokes);
        pad.links.push(...links);
        history.push({ kind: 'paste', strokes: strokes.map((x) => x.id), links: links.map((x) => x.id), notes: notes.map((x) => x.id) });
        ink.redraw();
        drawLabels();
        lasso.selectItems(strokes, links);
        env.changed();
      },
      typeAt: (cx, cy, text) => texts.typeAt(cx, cy, text),
      // Text from elsewhere: a web address becomes a link, other text a
      // typed note, near the view's top left.
      pasteText(text) {
        const r = host.getBoundingClientRect();
        const at = { x: r.left + 30, y: r.top + 40 };
        if (/^https?:\/\/\S+$/i.test(text)) this.addLink({ kind: 'page', url: text, title: text });
        else this.addText(text, { kind: 'page' }, at, true);
      },
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

  Object.assign(NB, { PEN_WIDTH, RED_INK, HIT_PX, TAP_PX, STILL_PX, LASSO_HOLD_MS, STALE_MS, ERASE_PX, ERASER_BUTTON, GLIDE_MS, createSurface });
})();
