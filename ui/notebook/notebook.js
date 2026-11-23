// The Notebook (see notebook/base.js): the notebook: its bar over the canvas.
(() => {
  'use strict';
  const NB = window.SnorkleNb;
  const { ICONS, RED_INK, SVG_NS, WRITING_PAUSE_MS, capture, createMinimap, createSurface, darkCopy, el, emptyPad, icon, pageOf, penRestedMs, pref, setPref } = NB;

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

    // ⌘C / ⌘X / ⌘V in the notebook (2026-11-16; not while typing a note):
    // the lasso's selection is copied as items (kept here, to paste back
    // in) and as text (the system clipboard, for other apps). A paste is
    // caught in a hidden field, so the system's own paste brings its text
    // without a permission prompt; if it is the text copied here, the
    // items are pasted, else the text, as a typed note or a link.
    let copied = null; // { items, text, n }
    let catcher = null;
    let catchTimer = 0;
    const pasted = (text) => {
      clearTimeout(catchTimer);
      if (catcher) catcher.remove();
      catcher = null;
      root.focus({ preventScroll: true });
      if (!surface) return;
      const t = String(text || '').replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '').trim().slice(0, 4000);
      if (copied && (!t || t === copied.text.trim())) surface.pasteItems(copied.items, ++copied.n);
      else if (t) surface.pasteText(t);
    };
    root.addEventListener('keydown', (e) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || !surface) return;
      if (e.target.closest && e.target.closest('textarea, input')) return;
      const k = e.key.toLowerCase();
      if (k === 'c' || k === 'x') {
        const c = k === 'x' ? surface.cutItems() : surface.copyItems();
        if (!c) return;
        e.preventDefault();
        copied = { ...c, n: 0 };
        if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(c.text).catch(() => {});
      } else if (k === 'v') {
        catcher = el('textarea', 'notepad-paste-catcher');
        catcher.setAttribute('aria-hidden', 'true');
        catcher.tabIndex = -1;
        catcher.addEventListener('paste', (pe) => {
          pe.preventDefault();
          pasted(pe.clipboardData ? pe.clipboardData.getData('text/plain') : '');
        });
        root.appendChild(catcher);
        catcher.focus({ preventScroll: true });
        // No paste event (nothing on the clipboard the system lets us
        // have): what was copied here, if anything.
        catchTimer = setTimeout(() => { if (catcher) pasted(''); }, 400);
      }
    });

    // Typing over the notebook (owner request 2026-11-16): keyboard focus
    // follows a mouse or trackpad pointer here, and back to what had it
    // (a page) when the pointer leaves (natively: `hover_focus`, macOS),
    // unless a text field elsewhere in the chrome is being typed in. A
    // printable key then begins a typed note where the pointer is; keys
    // with ⌘ or ⌃ stay shortcuts.
    let hovering = false;
    let tookFocus = false;
    let hoverAt = null;
    const typingElsewhere = () => {
      const a = document.activeElement;
      return !!a && !root.contains(a) && (a.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName));
    };
    root.addEventListener('pointerenter', (e) => {
      if (e.pointerType !== 'mouse') return;
      hovering = true;
      if (typingElsewhere()) return;
      tookFocus = true;
      if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
      env.invoke('hover_focus', { on: true }).then(() => {
        if (hovering && !typingElsewhere() && !root.contains(document.activeElement)) root.focus({ preventScroll: true });
      }).catch(() => {});
    });
    root.addEventListener('pointerleave', (e) => {
      if (e.pointerType !== 'mouse') return;
      hovering = false;
      hoverAt = null;
      if (!tookFocus) return;
      tookFocus = false;
      // A note being typed keeps the focus (and the notebook) until done.
      if (document.activeElement && document.activeElement.closest && document.activeElement.closest('textarea, input') && root.contains(document.activeElement)) return;
      if (root.contains(document.activeElement)) root.blur();
      env.invoke('hover_focus', { on: false }).catch(() => {});
    });
    canvas.addEventListener('pointermove', (e) => { if (e.pointerType === 'mouse') hoverAt = [e.clientX, e.clientY]; });
    root.addEventListener('keydown', (e) => {
      if (e.metaKey || e.ctrlKey || e.isComposing || !surface || !hovering || !hoverAt) return;
      if (e.target.closest && e.target.closest('textarea, input')) return;
      if (e.key.length !== 1 || !e.key.trim()) return;
      e.preventDefault();
      surface.typeAt(hoverAt[0], hoverAt[1], e.key);
    });

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
      // A clip's text (for search; not shown), when the words read in its
      // picture come. Set on the clip's target itself: the clip keeps that
      // object, and may still be on its way into the pad.
      setClipText(target, text) {
        if (!target || typeof text !== 'string' || !text.trim()) return;
        target.text = text.slice(0, 4000);
        whenReady(() => {
          if (!pad.links.some((l) => l.target === target)) return;
          saveSoon();
          if (!searchRow.hidden && searchInput.value.trim()) showCount(surface.search(searchInput.value));
        });
      },
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
