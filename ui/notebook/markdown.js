// The Notebook (see notebook/base.js): Markdown in typed notes.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { ICONS, el } = NB;

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

  Object.assign(NB, { INLINE, inline, parseLine, toggleTask, QUOTE_EM, DEPTH_EM, MARKER_EM, BOX_EM, BOX_GAP_EM, lineEl, renderText });
})();
