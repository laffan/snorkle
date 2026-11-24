// The Notebook (see notebook/base.js): chips: links, quotes, clips.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { ICONS, capture, el, icon, linkLabel, onPress, renderText, siteOf } = NB;

  // A clip's copy buttons (2026-11-24): its text, and its picture.
  const COPY_TEXT_ICON = () => icon([['rect', { x: '5', y: '5', width: '8.5', height: '9', rx: '1.5' }], ['path', { d: 'M10.5 5V3a1 1 0 0 0-1-1H3.5a1 1 0 0 0-1 1v7a1 1 0 0 0 1 1H5' }], ['path', { d: 'M7 8h4.5M7 10.5h3' }]], 12);
  const COPY_IMAGE_ICON = () => icon([['rect', { x: '2', y: '3', width: '12', height: '10', rx: '1.5' }], ['circle', { cx: '5.7', cy: '6.4', r: '1.1' }], ['path', { d: 'M2.5 11.5l3.5-3 2.5 2 2-1.6 3 2.6' }]], 12);

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
      foot.append(
        pin(l),
        el('span', 'notepad-link-page', siteOf(l.target.url) || l.target.title || ''),
        copyButton(COPY_TEXT_ICON(), 'Copy the text in this clip', () => copyClipText(l)),
        copyButton(COPY_IMAGE_ICON(), 'Copy this clip as a picture', () => copyClipImage(l)),
      );
      e.appendChild(foot);
    }

    // Copy buttons on a clip (owner request 2026-11-24). A press on one is
    // its own: the clip is not moved or followed.
    function copyButton(svg, title, act) {
      const b = el('span', 'notepad-clip-copy');
      b.setAttribute('role', 'button');
      b.setAttribute('aria-label', title);
      b.title = title;
      b.appendChild(svg);
      onPress(b, act);
      return b;
    }

    // The clip's text: what the page had in its region and what text
    // recognition read in its picture (kept for search, `target.text`).
    function copyClipText(l) {
      const text = (l.target.text || '').trim();
      if (!text) { env.toast('No text was found in this clip.'); return; }
      navigator.clipboard.writeText(text).then(() => env.toast('Text copied.'), () => env.toast('Could not copy (clipboard unavailable).'));
    }

    // The clip's picture (as the page showed it), as a PNG: the clipboard
    // takes PNG pictures. Given to the clipboard as a promise, so the press
    // that asked still counts while the picture is converted.
    function copyClipImage(l) {
      if (!l.image || !navigator.clipboard || !window.ClipboardItem) { env.toast('Could not copy (clipboard unavailable).'); return; }
      const png = new Promise((resolve, reject) => {
        const img = new Image();
        img.onload = () => {
          const c = document.createElement('canvas');
          c.width = img.naturalWidth;
          c.height = img.naturalHeight;
          c.getContext('2d').drawImage(img, 0, 0);
          c.toBlob((b) => (b ? resolve(b) : reject(new Error('no picture'))), 'image/png');
        };
        img.onerror = () => reject(new Error('no picture'));
        img.src = l.image;
      });
      navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
        .then(() => env.toast('Picture copied.'), () => env.toast('Could not copy the picture.'));
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

  Object.assign(NB, { HOLD_MS, CHIP_TAP_PX, MIN_W, MAX_W, MIN_H, createLinkChips });
})();
