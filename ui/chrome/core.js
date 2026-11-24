// Browser chrome. Trusted, bundled, local. Renders state pushed from Rust
// and sends user intents back as Tauri commands.
//
// Security rules for this file (see docs/ARCHITECTURE.md):
// - Never use innerHTML/insertAdjacentHTML/document.write; build DOM with
//   createElement + textContent only. Page-controlled strings (titles,
//   hosts) must never be interpreted as markup.
// - The address bar shows only what Rust's DisplayUrl provides.
// - Never load remote resources (CSP also forbids it).
// - Anything drawn over the content area must set `overlay` (see
//   reportLayout) so native page views are hidden while it is open.
//
// The chrome is split into ordered classic scripts (ui/chrome/*.js, listed
// in index.html) sharing one global scope: names declared at the top of one
// are visible to the others. Loaded in order with `defer`; a file may call
// another's functions at load time only if that file comes earlier.
'use strict';

const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args).catch((e) => toast(String(e)));
const $ = (id) => document.getElementById(id);
// macOS (no touch points; an iPad, also in desktop mode, has them): no
// title bar, so the sidebar keeps a strip for the window buttons, and the
// web engine's version is Safari's.
const macOS = navigator.maxTouchPoints === 0 && /Macintosh/.test(navigator.userAgent);
if (macOS) document.documentElement.classList.add('macos');
// iPadOS (touch points, a Mac or iPad user agent). Since the status bar is
// hidden (2026-11-23) the window's top ~30 pt is where iPadOS 26 drags,
// shrinks or un-fullscreens the window: a drag begun there (the notebook's
// quote button, a pin) went to the system. Nothing the chrome drags sits
// there (`IPAD_TOP_STRIP`, html.ipados; owner report 2026-11-24); pages
// still reach the top.
const iPadOS = navigator.maxTouchPoints > 0 && /Macintosh|iPad/.test(navigator.userAgent);
const IPAD_TOP_STRIP = 30;
if (iPadOS) document.documentElement.classList.add('ipados');

let state = null;
// Define cards on start pages, by tab (see defineWord).
const defines = new Map();
let editing = false;
let consoleOpen = false;
// ⋯ → Attach URL Bar (remembered): the address bar is a full-width bar
// along the bottom of the page, which ends above it, instead of a pill
// floating over the page.
let barAttached = false;
let panel = null; // null | 'downloads'
let modalOpen = false;
let scriptsOpen = false;
let menuOpen = false;
let chatOpen = false;
// The Notebook (ui/notebook/; remembered): docked with the right panels.
let notebookOpen = false;
let notebook = null;
// ⌘⇧\ hides the right panels without closing them (the summary view
// and the chat stay on, and come back with the next ⌘⇧\).
let rightHidden = false;
// The sidebar shown as a pop-over from the left-edge tab (sidebar closed).
let findOpen = false; // ⌘F bar
let confirmAction = null; // { tab, run } while a confirmation is shown
let bookmarks = [];
let pins = [];
const MAX_PINS = 12; // three rows of four (storage::MAX_PINS)
// View modes (session-only, kept in Rust): app-wide on/off, and tabs
// exempted from a mode ("Disable for this tab"). Night vision starts
// from the chrome's own copy of the setting until Rust answers, so a
// render before then doesn't take it off (applyNightVisionChrome).
const nightAtLaunch = pref('nightVision') === '1';
let viewModes = {
  modes: { desaturate: true, hide_media: true, dark: false, night: nightAtLaunch },
  launch: { desaturate: true, hide_media: true, dark: false, night: nightAtLaunch },
  exempt: {},
};
let scripts = [];
let editingDomain = null; // domain of the script open in the editor, or '' for new
let tabsKey = '';
let toastTimer = 0;
// What the toast shows: '' (hidden), 'plain' (a message), 'later' (Save
// for Later's progress) or 'download' (downloads' progress).
let toastKind = '';
let clearArmed = false;
let suppressClick = false;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// Remembered settings of the chrome itself (localStorage; may be
// unavailable).
function pref(key) {
  try { return localStorage.getItem(key); } catch (e) { return null; }
}
function setPref(key, value) {
  try { localStorage.setItem(key, value); } catch (e) { /* storage unavailable */ }
}

// A handler that runs `f` with the active tab, if there is one.
function withTab(f) {
  return () => { const t = activeTab(); if (t) f(t); };
}

function activeTab() {
  if (!state || state.active === null) return null;
  return state.tabs.find((t) => t.id === state.active) || null;
}

// Renders a host inside one LTR isolate, with each label in its own
// isolate, so RTL labels cannot visually reorder the other labels (and
// the container's rtl direction, used only for left-side elision, cannot
// move the dots).
function fillLabels(node, host, trailingDot) {
  node.replaceChildren();
  if (!host) return;
  const wrap = el('bdi');
  wrap.dir = 'ltr';
  const labels = host.split('.');
  labels.forEach((label, i) => {
    wrap.appendChild(el('bdi', null, label));
    if (i < labels.length - 1 || trailingDot) wrap.appendChild(document.createTextNode('.'));
  });
  node.appendChild(wrap);
}

function renderUrlInto(sub, reg, port, d) {
  fillLabels(sub, d ? d.subdomain : '', !!(d && d.subdomain));
  fillLabels(reg, d ? d.registrable : '', false);
  if (port) port.textContent = d && d.port ? ':' + d.port : '';
}
