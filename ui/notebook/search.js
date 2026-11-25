// The Notebook (see notebook/base.js): search in the pad.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { el, strokeBox } = NB;

  // ---- search in the pad (owner request 2026-11-16; zotero-helper has none) ----
  //
  // Split out of the canvas (notebook/surface.js) for the 700-line rule:
  // `ctx` carries what it reads of the canvas's closure.
  function createPadSearch(ctx) {
    const { env, chips, layer, labels, ink, host, glideTo } = ctx;
    //
    // What can be found: a typed note's words, a quote's words, and for
    // every item (handwritten notes included) the title and address of the
    // page it came from, and a clip's links (2026-11-25). Handwriting itself
    // is not read. Every word of the
    // query must appear (case and accents ignored). Matches are outlined,
    // everything else faded; stepping goes through them top to bottom and
    // glides the pad to each.
    let search = null; // { terms, hits, at, frames }
    const fold = (t) => String(t || '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
    const where = (url) => String(url || '').replace(/^[a-z]+:\/\/(www\.)?/i, '');
    const clipLinks = (l) => (Array.isArray(l.target.links) ? l.target.links : []).flatMap((k) => [k && k.title, k && where(k.url)]);
    const linkWords = (l) => fold([l.text, l.target.text, l.target.title, where(l.target.url), ...clipLinks(l)].filter(Boolean).join(' '));
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
      const inView = x0 + ctx.pan().x >= 0 && x1 + ctx.pan().x <= w && y0 + ctx.pan().y >= 0 && y1 + ctx.pan().y <= ht;
      if (inView) return;
      const cx = (x0 + x1) / 2;
      const top = y1 - y0 > ht * 0.8 ? y0 - 20 : (y0 + y1) / 2 - ht / 2;
      const fitsX = x0 + ctx.pan().x >= 0 && x1 + ctx.pan().x <= w;
      const x = fitsX ? ctx.pan().x : x1 - x0 > w * 0.9 ? Math.round(20 - x0) : Math.round(w / 2 - cx);
      glideTo({ x, y: Math.round(-top) });
    }

    function setSearch(query) {
      const terms = fold(query).split(/\s+/).filter(Boolean);
      if (!search) search = { terms: [], hits: [], at: -1, frames: [] };
      search.terms = terms;
      search.hits = terms.length ? findHits() : [];
      // The first match from the top of the view down, else the first.
      const top = -ctx.pan().y;
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

    return { setSearch, stepSearch, endSearch, refreshSearch, paintSearch, active: () => !!search };
  }

  Object.assign(NB, { createPadSearch });
})();
