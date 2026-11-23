// Browser chrome (see core.js): Settings → Keyboard: recording bindings.
'use strict';

// ---- keyboard shortcuts (Settings → Keyboard) ------------------------------
//
// The bindings live natively (the menu bar's key commands); the chrome
// keeps the user's changes (localStorage) and sends them at launch. The
// keydown handler above uses the same list for when the chrome has focus.

const MOD_SHIFT = 1;
const MOD_ALT = 2;
const MOD_CTRL = 4;
const CODE_KEYS = {
  BracketLeft: '[', BracketRight: ']', Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.',
  Slash: '/', Minus: '-', Equal: '=', Backquote: '`',
};
// When `code` is missing: the unshifted key of a shifted character (US).
const SHIFTED = {
  '|': '\\', '{': '[', '}': ']', ':': ';', '"': "'", '<': ',', '>': '.', '?': '/', '_': '-', '+': '=', '~': '`',
  '!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
};
let shortcutList = [];
let recordingKey = null; // id of the shortcut being rebound

// The unshifted key of a ⌘ key press (by physical key, so ⇧ does not turn
// \ into |), and its other modifiers.
const ARROWS = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down' };
const ARROW_LABELS = { left: '←', right: '→', up: '↑', down: '↓' };
function keyOf(ev) {
  let key = null;
  if (ARROWS[ev.code] || ARROWS[ev.key]) key = ARROWS[ev.code] || ARROWS[ev.key];
  else if (/^Key[A-Z]$/.test(ev.code)) key = ev.code.slice(3).toLowerCase();
  else if (/^Digit[0-9]$/.test(ev.code)) key = ev.code.slice(5);
  else if (CODE_KEYS[ev.code]) key = CODE_KEYS[ev.code];
  else if (ev.key && ev.key.length === 1) key = SHIFTED[ev.key] || ev.key.toLowerCase();
  if (!key) return null;
  const mods = (ev.shiftKey ? MOD_SHIFT : 0) | (ev.altKey ? MOD_ALT : 0) | (ev.ctrlKey ? MOD_CTRL : 0);
  return { key, mods };
}

function comboLabel(key, mods) {
  return (mods & MOD_CTRL ? '⌃' : '') + (mods & MOD_ALT ? '⌥' : '') + (mods & MOD_SHIFT ? '⇧' : '') + '⌘' + (key === ' ' ? 'Space' : ARROW_LABELS[key] || key.toUpperCase());
}

function savedBindings() {
  try { return JSON.parse(pref('shortcuts') || '[]'); } catch (e) { return []; }
}

// Sends bindings to the host; on success they are saved.
function applyBindings(bindings, suspend) {
  return T.core.invoke('set_shortcuts', { bindings, suspend }).then((list) => {
    shortcutList = list;
    if (!suspend) setPref('shortcuts', JSON.stringify(bindings));
    return list;
  });
}

function renderKeys() {
  const list = $('keys-list');
  list.replaceChildren();
  for (const sc of shortcutList) {
    const li = el('li');
    li.appendChild(el('span', 'keys-title', sc.title));
    const changed = sc.key !== sc.default_key || sc.mods !== sc.default_mods;
    if (changed) li.appendChild(el('span', 'keys-default muted small', 'default ' + comboLabel(sc.default_key, sc.default_mods)));
    const b = el('button', 'keys-combo' + (recordingKey === sc.id ? ' recording' : ''), recordingKey === sc.id ? 'Press keys…' : comboLabel(sc.key, sc.mods));
    b.setAttribute('aria-label', `${sc.title}: ${comboLabel(sc.key, sc.mods)}. Change`);
    b.addEventListener('click', () => startRecording(sc.id));
    li.appendChild(b);
    list.appendChild(li);
  }
}

function keysMsg(t) { $('keys-msg').textContent = t || ''; }

// While recording, the menu bar's shortcuts are off, so the keys reach
// the chrome instead of running a command.
function startRecording(id) {
  recordingKey = id;
  keysMsg('');
  renderKeys();
  applyBindings(savedBindings(), true).catch(() => {});
}
function stopRecording(msg) {
  recordingKey = null;
  applyBindings(savedBindings(), false).catch(() => {}).finally(renderKeys);
  keysMsg(msg);
}

document.addEventListener('keydown', (ev) => {
  if (!recordingKey) return;
  ev.preventDefault();
  ev.stopImmediatePropagation();
  if (ev.key === 'Escape') { stopRecording(''); return; }
  if (['Meta', 'Shift', 'Alt', 'Control', 'CapsLock'].includes(ev.key)) return;
  if (!ev.metaKey) { keysMsg('Hold ⌘ with the key.'); return; }
  const pressed = keyOf(ev);
  if (!pressed) { keysMsg('That key can’t be used.'); return; }
  finishRecording(pressed.key, pressed.mods);
}, true);

// Saves the recorded combination for the shortcut being changed (from the
// native recording, or this page's keydown where ⌘ keys reach it).
function finishRecording(key, mods) {
  const id = recordingKey;
  if (!id) return;
  const next = savedBindings().filter((b) => b.id !== id).concat([{ id, key, mods }]);
  T.core.invoke('set_shortcuts', { bindings: next, suspend: false })
    .then((list) => {
      shortcutList = list;
      setPref('shortcuts', JSON.stringify(next));
      recordingKey = null;
      renderKeys();
      keysMsg(`Saved: ${comboLabel(key, mods)}.`);
    })
    .catch((e) => {
      keysMsg(String(e));
      // Still recording: let the host offer the combinations again.
      applyBindings(savedBindings(), true).catch(() => {});
    });
}

$('keys-reset').addEventListener('click', () => {
  recordingKey = null;
  setPref('shortcuts', '[]');
  applyBindings([], false).then(() => { renderKeys(); keysMsg('Defaults restored.'); }).catch((e) => keysMsg(String(e)));
});

// At launch: the saved bindings, or the defaults if they no longer apply.
applyBindings(savedBindings(), false)
  .catch(() => applyBindings([], false))
  .catch(() => T.core.invoke('get_shortcuts').then((l) => { shortcutList = l; }))
  .catch(() => {});
