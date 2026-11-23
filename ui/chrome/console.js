// Browser chrome (see core.js): The in-app console; toasts.
'use strict';

// ---- console -----------------------------------------------------------
//
// Entries come from Rust (browser events, and page console output while
// recording) plus the chrome's own errors and notes. All text, page
// messages included, is set with textContent only.

const MAX_CONSOLE = 1000;
let consoleEntries = [];
let consoleCapture = false;
let consoleFilter = 'all';
let chromeSeq = 0;
let consoleQueued = false;

function addConsole(entry) {
  consoleEntries.push(entry);
  if (consoleEntries.length > MAX_CONSOLE) consoleEntries.splice(0, consoleEntries.length - MAX_CONSOLE);
  if (consoleOpen && !consoleQueued) {
    consoleQueued = true;
    requestAnimationFrame(() => { consoleQueued = false; renderConsole(); });
  }
}

// The chrome's own messages (not from Rust or pages).
function chromeLog(level, text) {
  addConsole({ seq: 'c' + chromeSeq++, time_ms: Math.round(performance.now()), origin: 'chrome', level, tab: null, host: '', main_frame: true, text: String(text) });
}

function loadConsole() {
  return T.core.invoke('get_console').then((c) => {
    consoleCapture = c.capture;
    const mine = consoleEntries.filter((e) => e.origin === 'chrome');
    consoleEntries = c.entries.concat(mine).sort((a, b) => a.time_ms - b.time_ms);
    renderConsole();
  }).catch(() => {});
}

function visibleEntries() {
  const tabOnly = $('console-tab-only').checked;
  const t = activeTab();
  return consoleEntries.filter((e) => {
    if (consoleFilter === 'page' && e.origin !== 'page') return false;
    if (consoleFilter === 'app' && e.origin === 'page') return false;
    if (consoleFilter === 'errors' && e.level !== 'error' && e.level !== 'warn') return false;
    if (tabOnly && t && e.tab !== t.id) return false;
    return true;
  });
}

function sourceLabel(e) {
  if (e.origin === 'page') return (e.host || '(no origin)') + (e.main_frame ? '' : ' (frame)');
  return e.origin === 'chrome' ? 'chrome' : 'browser';
}

function stamp(ms) {
  const s = ms / 1000;
  return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, '0')}`;
}

function renderConsole() {
  const list = $('console-list');
  const atEnd = list.scrollTop + list.clientHeight >= list.scrollHeight - 24;
  list.replaceChildren();
  for (const e of visibleEntries()) {
    const li = el('li', e.level);
    li.append(el('span', 'when', stamp(e.time_ms)), el('span', 'src', sourceLabel(e)), el('span', 'msg', e.text));
    list.appendChild(li);
  }
  if (!list.childElementCount) list.appendChild(el('li', 'debug', consoleCapture ? 'Nothing logged yet.' : 'Recording is off. Tap Record to log browser events and page console output.'));
  if (atEnd) list.scrollTop = list.scrollHeight;
  const rec = $('console-capture');
  rec.setAttribute('aria-pressed', String(consoleCapture));
  rec.textContent = consoleCapture ? '● Recording' : 'Record';
  document.querySelectorAll('#console .seg button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.filter === consoleFilter)));
}

function setConsole(open) {
  closeMenu();
  consoleOpen = open;
  $('console').hidden = !open;
  if (open) loadConsole();
  reportLayout();
}

function setCapture(on) {
  T.core.invoke('set_console_capture', { on }).then(() => {
    consoleCapture = on;
    const note = $('console-note');
    note.hidden = !on;
    note.textContent = 'Page console output is recorded from the next page load. Reload a page to capture it from the start.';
    renderConsole();
  }).catch((e) => toast(String(e)));
}

function copyConsole() {
  const text = visibleEntries().map((e) => `${stamp(e.time_ms)} [${e.level}] ${sourceLabel(e)}: ${e.text}`).join('\n');
  navigator.clipboard.writeText(text).then(() => toast('Console copied.'), () => toast('Could not copy (clipboard unavailable).'));
}

window.addEventListener('error', (e) => chromeLog('error', `Uncaught ${e.message} (${e.filename}:${e.lineno})`));
window.addEventListener('unhandledrejection', (e) => chromeLog('error', 'Unhandled rejection: ' + String(e.reason)));
document.addEventListener('securitypolicyviolation', (e) =>
  chromeLog('error', `CSP blocked ${e.blockedURI.slice(0, 40)} (${e.violatedDirective})`));

// Download progress never takes the toast from another message
// (`toastKind`); it comes back when that message hides.
function clearToast() {
  clearTimeout(toastTimer);
  const t = $('toast');
  t.hidden = true;
  t.className = 'toast';
  t.replaceChildren();
  toastKind = '';
  reportLayout();
}
function hideToast() {
  clearToast();
  downloadProgressToast();
}

// A toast that stays while something runs, with how far it is (Save for
// Later: Rust's `later_progress` events). A plain toast replaces it.
function progressToast(label, percent) {
  const p = Math.max(0, Math.min(100, Math.round(percent)));
  const t = $('toast');
  t.className = 'toast progress';
  t.textContent = `${label} ${p}%`;
  const bar = el('span', 'toast-bar');
  bar.style.width = p + '%';
  t.appendChild(bar);
  t.hidden = false;
  toastKind = 'later';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(hideToast, 120000);
  reportLayout();
}
T.event.listen('later_progress', (e) => {
  const { kind, percent } = e.payload || {};
  if (percent >= 100) return; // the command's own toast follows
  progressToast(kind === 'add' ? 'Adding pages…' : 'Saving for later…', percent);
});

// A message for 4 s. With `action` ({ label, run }) it ends in a link
// button and stays 8 s.
function toast(msg, action) {
  chromeLog('info', 'toast: ' + msg);
  const t = $('toast');
  t.className = 'toast';
  t.textContent = msg;
  if (action) {
    const b = el('button', 'toast-link', action.label);
    b.addEventListener('click', () => { action.run(); hideToast(); });
    t.append(' ', b);
  }
  t.hidden = false;
  toastKind = 'plain';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(hideToast, action ? 8000 : 4000);
  reportLayout();
}
