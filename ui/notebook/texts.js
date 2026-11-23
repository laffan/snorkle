// The Notebook (see notebook/base.js): typed notes.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { el, newId, toggleTask } = NB;

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

    function create(cx, cy, text = '') {
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
      }, text);
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
      // A key typed over the canvas: a note where the pointer is, begun
      // with it (or the note being typed, if one is open).
      typeAt(cx, cy, text) {
        if (!open) { create(cx, cy, text); return; }
        focus();
        ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, 'end');
        fit();
      },
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

  // Copied links as plain text, top to bottom: a note's words, a quote
  // with its page, a link or a clip as its page.
  function itemsText(links) {
    return [...links].sort((a, b) => a.y - b.y || a.x - b.x).map((l) => {
      const t = l.target || {};
      const where = [t.title, t.url].filter(Boolean).join(' — ');
      if (l.text !== undefined) return l.text;
      if (t.kind === 'quote') return `“${t.text || ''}”${where ? `\n${where}` : ''}`;
      if (l.image) return `[Clip]${where ? ` ${where}` : ''}`;
      return where;
    }).filter(Boolean).join('\n\n');
  }

  Object.assign(NB, { DOUBLE_MS, DOUBLE_PX, TEXT_MAX_W, TEXT_MIN_W, EDGE_PX, tidy, wrap, linkify, createTextNotes, itemsText });
})();
