//! SECURITY-REVIEW: Save for Later for AI conversations (owner request
//! 2026-11-15): on chatgpt.com, claude.ai and gemini.google.com the saved
//! copy is the conversation itself, as a plain, self-contained page that
//! works offline, instead of the site's own page (a single-page app whose
//! HTML is an empty shell, and whose long threads keep only the turns in
//! view in the document).
//!
//! Capture (part of channel C22): constant code run with
//! `callAsyncJavaScript` in the page's client world, main frame, only when
//! the user saves the page. [`start_js`] finds the turns and the element
//! that scrolls them and scrolls to the top until no earlier turns load;
//! [`STEP_JS`] scrolls down through the thread a little at a time, noting
//! each turn as the site puts it in the document; [`END_JS`] scrolls back
//! to where the reader was and hands over every turn as a tree of nodes
//! (`[tag, {attrs}, ...children]` or a text string), never as markup.
//! Images the page holds as `blob:` URLs are read and handed over as
//! `data:` URLs (bounded).
//!
//! Everything is rebuilt here ([`parse`], [`to_html`]): JSON only; a fixed
//! set of tags (text formatting, lists, tables, code, MathML) and of
//! attributes per tag, each value checked (links and image sources
//! http(s) without credentials, or `data:image/…;base64` of a few image
//! types); text and attribute values escaped; sizes, depth and counts
//! bounded. No script, style, event handler, form, frame or `style`
//! attribute can come out. The page is stored as the archive's main
//! resource and shown with JavaScript off ([`crate::later::LaterPage::scripts`]
//! false), with the page's own images from WebKit's archive.
//!
//! Choosing to ask the site for a share link instead was considered and not
//! done: it publishes the conversation on the provider's servers (a public
//! URL), needs the site's own buttons pressed, and the shared page is the
//! same kind of single-page app.

use serde_json::Value;
use url::Url;

use crate::text::sanitize_page_text;

/// The sites whose conversations are saved this way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    ChatGpt,
    Claude,
    Gemini,
}

impl Site {
    /// The assistant's name, as the saved page labels its turns.
    pub fn assistant(self) -> &'static str {
        match self {
            Site::ChatGpt => "ChatGPT",
            Site::Claude => "Claude",
            Site::Gemini => "Gemini",
        }
    }
}

/// The site of a page's (committed, http(s)) URL, if it is one of them.
pub fn site_of(url: &Url) -> Option<Site> {
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let on = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if on("chatgpt.com") || on("chat.openai.com") {
        Some(Site::ChatGpt)
    } else if on("claude.ai") {
        Some(Site::Claude)
    } else if host == "gemini.google.com" {
        Some(Site::Gemini)
    } else {
        None
    }
}

/// Largest answer of [`END_JS`] read.
pub const MAX_END_BYTES: usize = 16 * 1024 * 1024;
/// Largest answer of [`start_js`] / [`STEP_JS`] read.
pub const MAX_STEP_BYTES: usize = 256;
/// Most turns kept.
pub const MAX_TURNS: usize = 3000;
/// Most nodes kept in all, and the deepest nesting followed.
pub const MAX_NODES: usize = 500_000;
pub const MAX_DEPTH: usize = 48;
/// Largest `data:` image kept, and all of them together.
pub const MAX_DATA_IMAGE: usize = 1_500_000;
pub const MAX_DATA_IMAGES: usize = 5 * 1024 * 1024;
/// Largest page written (the archive's main resource).
pub const MAX_HTML: usize = crate::later::MAX_PAGE_BYTES;

/// Shared code of the three calls: the site's turns in the document now,
/// each as `{role, key, text, tree}`, and merging what is seen into the
/// thread so far. Kept on `window.__snorkleConv` (client world) between
/// the calls.
const COMMON_JS: &str = r#"
const host = location.hostname;
const site = /(^|\.)(chatgpt\.com|chat\.openai\.com)$/.test(host) ? 'chatgpt'
  : /(^|\.)claude\.ai$/.test(host) ? 'claude' : host === 'gemini.google.com' ? 'gemini' : '';
const KEEP = new Set('P BR H1 H2 H3 H4 H5 H6 UL OL LI BLOCKQUOTE TABLE THEAD TBODY TFOOT TR TH TD STRONG B EM I U S DEL INS A IMG HR SUP SUB SPAN DIV DETAILS SUMMARY MARK KBD FIGURE FIGCAPTION DL DT DD CODE SECTION ARTICLE HEADER FOOTER'.split(' '));
const MATH = new Set('math semantics mrow mi mo mn ms mtext mspace msup msub msubsup mfrac msqrt mroot mtable mtr mtd munder mover munderover mstyle mpadded mphantom menclose'.split(' '));
const DROP = new Set('BUTTON SVG SCRIPT STYLE NOSCRIPT TEMPLATE INPUT TEXTAREA SELECT OPTION IFRAME CANVAS VIDEO AUDIO OBJECT EMBED FORM LINK META MODEL-THOUGHTS'.split(' '));
const MATTR = ['display', 'mathvariant', 'stretchy', 'fence', 'separator', 'form', 'lspace', 'rspace', 'accent', 'accentunder', 'linethickness', 'displaystyle', 'scriptlevel', 'width', 'height', 'depth', 'voffset', 'notation'];
let budget = 0;
function tree(n, depth) {
  if (budget++ > 200000 || depth > 44) return null;
  if (n.nodeType === 3) return n.nodeValue;
  if (n.nodeType !== 1) return null;
  const tag = n.tagName, low = n.localName;
  if (DROP.has(tag)) return null;
  const cls = typeof n.className === 'string' ? n.className : '';
  if (/(^|\s)(sr-only|visually-hidden|cdk-visually-hidden|katex-html)(\s|$)/.test(cls)) return null;
  if (n.getAttribute('role') === 'button' || low === 'annotation' || low === 'annotation-xml') return null;
  if (n.getAttribute('aria-hidden') === 'true' && (n.textContent || '').trim().length < 50 && !n.querySelector('img')) return null;
  if (tag === 'PRE') {
    const c = n.querySelector('code') || n;
    const m = /language-([\w+#.-]{1,30})/.exec(typeof c.className === 'string' ? c.className : '');
    return ['pre', m ? { lang: m[1] } : {}, c.textContent || ''];
  }
  if (tag === 'IMG') {
    const s = n.currentSrc || n.src || '';
    return s ? ['img', { src: s, alt: (n.getAttribute('alt') || '').slice(0, 300) }] : null;
  }
  const name = KEEP.has(tag) ? low : MATH.has(low) ? low : '';
  const attrs = {};
  if (tag === 'A' && /^https?:/i.test(n.href || '')) attrs.href = n.href;
  if (tag === 'TD' || tag === 'TH') { if (n.colSpan > 1) attrs.colspan = String(n.colSpan); if (n.rowSpan > 1) attrs.rowspan = String(n.rowSpan); }
  if (tag === 'OL' && n.getAttribute('start')) attrs.start = n.getAttribute('start');
  if (MATH.has(low)) for (const a of MATTR) { const v = n.getAttribute(a); if (v) attrs[a] = v; }
  const out = [name, attrs];
  for (const c of n.childNodes) { const t = tree(c, depth + 1); if (t !== null && t !== '') out.push(t); }
  return out.length > 2 || name === 'br' || name === 'hr' ? out : null;
}
// The site's turns in the document, each `{role, el, key}`. The sites
// change their markup often; each lists the forms seen, newest first,
// and the first that finds anything is used. Hidden nodes (a site may
// keep an earlier thread's turns in the document, hidden) are skipped.
function turns() {
  const out = [];
  // Has a box, or contents with one (a wrapper may be `display: contents`).
  const shown = (el) => {
    if (!el.isConnected) return false;
    if (el.getClientRects().length) return true;
    const r = document.createRange();
    r.selectNodeContents(el);
    return r.getClientRects().length > 0;
  };
  const add = (role, el, key) => { if (el && shown(el)) out.push({ role, el, key: key || '' }); };
  // Elements matching `sel`, outermost only, in document order.
  const outer = (sel, root) => {
    const seen = [];
    for (const el of (root || document).querySelectorAll(sel)) if (!seen.some((s) => s.contains(el))) seen.push(el);
    return seen;
  };
  if (site === 'chatgpt') {
    // 2026-10 and later (logged in): a unit per message, keyed "…:user" / "…:assistant".
    for (const el of outer('[data-content-search-unit-key]')) {
      const k = el.getAttribute('data-content-search-unit-key') || '';
      const r = /:user$/.test(k) ? 'user' : /:assistant$/.test(k) ? 'assistant' : '';
      if (r) add(r, el, k);
    }
    // Earlier: the author's role on each message.
    if (!out.length) {
      for (const el of outer('[data-message-author-role]')) {
        const r = el.getAttribute('data-message-author-role');
        if (r === 'user' || r === 'assistant') add(r, el, el.getAttribute('data-message-id'));
      }
    }
    // A group per exchange: the user's bubble, then the answer.
    if (!out.length) {
      for (const g of outer('[data-turn-key]')) {
        const k = g.getAttribute('data-turn-key') || '';
        add('user', g.querySelector('[data-user-message-bubble="true"]'), k && k + ':u');
        outer('[data-markdown-text-style="assistant-message"], .markdown', g).forEach((a, i) => add('assistant', a, k && k + ':a' + i));
      }
    }
    // The headings read to screen readers.
    if (!out.length) {
      for (const h of document.querySelectorAll('h4, h5, h6')) {
        const t = h.textContent.trim();
        const r = /^You said:?$/.test(t) ? 'user' : /^ChatGPT said:?$/.test(t) ? 'assistant' : '';
        if (!r) continue;
        let w = h.closest('[data-turn-key], [data-testid^="conversation-turn"], article');
        if (!w) { w = h.parentElement; for (let i = 0; i < 12 && w && !(w.classList && w.classList.contains('group')); i++) w = w.parentElement; }
        add(r, w || h.parentElement);
      }
    }
  } else if (site === 'claude') {
    const user = '[data-testid="user-message"], .font-user-message';
    for (const el of outer(user + ', [data-testid="assistant-message"], .font-claude-response, .font-claude-message')) {
      add(el.matches(user) || el.querySelector(user) ? 'user' : 'assistant', el);
    }
  } else if (site === 'gemini') {
    for (const el of document.querySelectorAll('user-query, model-response')) {
      const u = el.tagName === 'USER-QUERY';
      add(u ? 'user' : 'assistant', u ? (el.querySelector('.query-text') || el.querySelector('user-query-content') || el)
        : (el.querySelector('message-content') || el.querySelector('.response-content') || el.querySelector('.markdown') || el), el.id);
    }
    if (!out.length) {
      for (const el of outer('.user-query-container, .response-container, .response-content')) {
        add(el.matches('.user-query-container') ? 'user' : 'assistant', el, el.id);
      }
    }
  }
  return out;
}
// The turns in the document now, in reading order: by where they are on
// screen (a list that keeps only some turns may add them to the document
// in any order), the document's order breaking ties.
// Where an element starts on screen; a `display: contents` wrapper has no
// box of its own, so its contents' box is used.
function topOf(el) {
  const r = el.getBoundingClientRect();
  if (r.width || r.height) return r.top;
  const g = document.createRange();
  g.selectNodeContents(el);
  return g.getBoundingClientRect().top;
}
function snap() {
  budget = 0;
  return turns().map((t, i) => {
    const text = (t.el.textContent || '').replace(/\s+/g, ' ').trim();
    return { role: t.role, key: t.role + ':' + (t.key || text.slice(0, 200)), text, tree: tree(t.el, 0), top: topOf(t.el), i };
  }).filter((t) => t.tree && (t.text || JSON.stringify(t.tree).indexOf('"img"') >= 0))
    .sort((a, b) => a.top - b.top || a.i - b.i);
}
// Merges what is in the document now (S, in reading order) into the
// thread so far: anchored where the longest run of S's turns matches the
// thread (so a message repeated word for word, "yes", does not mislead
// it), turns before the anchor go in before it and turns after it after
// it, so the reader may scroll either way. With no turn in common, S goes
// before the thread when the view moved up (`up`), else after it.
function merge(all, S, up) {
  if (!S.length) return;
  if (!all.length) { for (const t of S) all.push(t); return; }
  let k = -1, p = -1, best = 0;
  for (let i = 0; i < S.length; i++) for (let j = 0; j < all.length; j++) {
    if (all[j].key !== S[i].key || (i > 0 && j > 0 && all[j - 1].key === S[i - 1].key)) continue;
    let run = 1;
    while (i + run < S.length && j + run < all.length && all[j + run].key === S[i + run].key) run++;
    if (run > best) { best = run; k = i; p = j; }
  }
  if (k < 0) { if (up) all.unshift(...S); else for (const t of S) all.push(t); return; }
  let q = p - 1, m = k - 1;
  while (m >= 0 && q >= 0 && all[q].key === S[m].key) { if (S[m].text.length > all[q].text.length) all[q] = S[m]; q--; m--; }
  if (m >= 0) { all.splice(q + 1, 0, ...S.slice(0, m + 1)); p += m + 1; }
  for (let j = k; j < S.length; j++) {
    const i = p + (j - k);
    if (i >= all.length) { all.push(S[j]); continue; }
    if (all[i].key === S[j].key) { if (S[j].text.length > all[i].text.length) all[i] = S[j]; continue; }
    let f = -1;
    for (let q2 = i + 1; q2 < Math.min(all.length, i + 6); q2++) if (all[q2].key === S[j].key) { f = q2; break; }
    if (f >= 0) { p += f - i; j--; continue; }
    all.splice(i, 0, S[j]);
  }
}
function scrollerOf(el) {
  for (let e = el && el.parentElement; e && e !== document.documentElement; e = e.parentElement) {
    const s = getComputedStyle(e);
    if (/(auto|scroll|overlay)/.test(s.overflowY) && e.scrollHeight > e.clientHeight + 20) return e;
  }
  return document.scrollingElement || document.documentElement;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
"#;

/// Finds the thread and loads its earliest turns (scrolling to the top
/// until the thread stops growing, at most 20 s), then notes the turns
/// there. Answers `{"steps": n}` (0: no conversation found). The code
/// shared by the three calls ([`COMMON_JS`]) is kept on
/// `window.__snorkleConv` (client world) for the next calls.
pub fn start_js() -> String {
    format!(
        "{COMMON_JS}{}",
        r#"
if (window.__snorkleConv && window.__snorkleConv.back) { try { window.__snorkleConv.back(); } catch (e) {} }
const st = { all: [], box: null, was: 0, reverse: false, pos: 0, step: 400, started: Date.now(), done: false, snap, merge };
// Where the reader was, from the thread's end (earlier turns may load
// above it meanwhile); a reversed scroller counts from its end already.
st.back = () => { const b = st.box; if (b) b.scrollTop = st.reverse ? st.was : b.scrollHeight - st.was; };
window.__snorkleConv = st;
if (!site) return JSON.stringify({ steps: 0 });
const first = turns()[0];
if (!first) return JSON.stringify({ steps: 0 });
let box = null;
for (let e = first.el.parentElement; e && e !== document.documentElement; e = e.parentElement) {
  const s = getComputedStyle(e);
  if (/(auto|scroll|overlay)/.test(s.overflowY) && e.scrollHeight > e.clientHeight + 20) { box = e; break; }
}
if (!box) box = document.scrollingElement || document.documentElement;
st.box = box;
st.reverse = getComputedStyle(box).flexDirection.indexOf('reverse') >= 0;
st.was = st.reverse ? box.scrollTop : box.scrollHeight - box.scrollTop;
const span = () => Math.max(0, box.scrollHeight - box.clientHeight);
let h = -1, same = 0;
const until = Date.now() + 20000;
while (Date.now() < until && same < 2) {
  box.scrollTop = st.reverse ? -span() : 0;
  await sleep(800);
  if (box.scrollHeight === h) same++; else { same = 0; h = box.scrollHeight; }
}
st.step = Math.max(200, Math.floor(box.clientHeight * 0.8));
st.pos = 0;
merge(st.all, snap());
return JSON.stringify({ steps: Math.ceil(span() / st.step) + 1 });
"#
    )
}

/// Scrolls on through the thread (for at most 6 s per call, 75 s in all
/// from the start), noting the turns as they appear. Answers `{"done": bool, "at": n, "of": n}`
/// (scroll position and height, for progress).
pub const STEP_JS: &str = r#"
const st = window.__snorkleConv;
if (!st || !st.box) return JSON.stringify({ done: true, at: 0, of: 0 });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const box = st.box;
const t0 = Date.now();
while (!st.done && Date.now() - t0 < 6000) {
  const span = Math.max(0, box.scrollHeight - box.clientHeight);
  st.pos = Math.min(span, st.pos + st.step);
  box.scrollTop = st.reverse ? st.pos - span : st.pos;
  await sleep(250);
  st.merge(st.all, st.snap());
  if (st.pos >= span || Date.now() - st.started > 75000) st.done = true;
}
const span = Math.max(1, box.scrollHeight - box.clientHeight);
return JSON.stringify({ done: st.done, at: Math.round(st.pos), of: Math.round(span) });
"#;

/// Scrolls back to where the reader was and answers the thread:
/// `{"turns": [{"r": "user" | "assistant", "c": node}]}`. `blob:` images
/// become `data:` URLs (at most 1.5 MB each, 5 MB in all); other `data:`
/// images are kept within the same bounds.
pub const END_JS: &str = r#"
const st = window.__snorkleConv;
delete window.__snorkleConv;
if (!st) return JSON.stringify({ turns: [] });
if (st.stop) st.stop();
if (st.live || !st.all.length) st.merge(st.all, st.snap(), st.up);
st.back();
let left = 5 * 1024 * 1024;
const read = (blob) => new Promise((res) => { const f = new FileReader(); f.onload = () => res(String(f.result || '')); f.onerror = () => res(''); f.readAsDataURL(blob); });
async function inline(node) {
  if (!Array.isArray(node)) return;
  if (node[0] === 'img' && node[1] && typeof node[1].src === 'string') {
    const s = node[1].src;
    if (/^blob:/i.test(s)) {
      let d = '';
      try { const b = await (await fetch(s)).blob(); if (/^image\//.test(b.type) && b.size <= 1100000 && b.size * 1.4 < left) d = await read(b); } catch (e) {}
      if (d) { left -= d.length; node[1].src = d; } else delete node[1].src;
    } else if (/^data:/i.test(s)) {
      if (s.length <= 1500000 && s.length < left) left -= s.length; else delete node[1].src;
    }
    return;
  }
  for (let i = 2; i < node.length; i++) await inline(node[i]);
}
for (const t of st.all) await inline(t.tree);
return JSON.stringify({ turns: st.all.map((t) => ({ r: t.role, c: t.tree })) });
"#;

/// Capture mode (owner request 2026-11-16): the reader scrolls through
/// the thread while every turn the site puts in the document is noted (on
/// DOM changes and scrolling, at most every 0.3 s), in order, whichever way
/// the reader scrolls; nothing is scrolled for them. Answers
/// `{"count": n}` (−1: not a conversation). [`END_JS`] then hands over
/// what was captured; [`CAPTURE_CANCEL_JS`] stops it.
pub fn capture_begin_js() -> String {
    format!(
        "{COMMON_JS}{}",
        r#"
if (window.__snorkleConv && window.__snorkleConv.stop) { try { window.__snorkleConv.stop(); } catch (e) {} }
const st = { all: [], live: true, up: false, lastTop: null, box: null, timer: 0, snap, merge: (a, S, up) => merge(a, S, up), back: () => {} };
window.__snorkleConv = st;
if (!site) return JSON.stringify({ count: -1 });
const take = () => {
  st.timer = 0;
  const S = snap();
  if (!S.length) return;
  if (!st.box || !st.box.isConnected) { const first = turns()[0]; st.box = first ? scrollerOf(first.el) : null; }
  if (st.box) { const top = st.box.scrollTop; if (st.lastTop !== null && top !== st.lastTop) st.up = top < st.lastTop; st.lastTop = top; }
  merge(st.all, S, st.up);
};
const soon = () => { if (!st.timer) st.timer = setTimeout(take, 300); };
document.addEventListener('scroll', soon, { capture: true, passive: true });
const obs = new MutationObserver(soon);
obs.observe(document.body || document.documentElement, { childList: true, subtree: true });
st.stop = () => { clearTimeout(st.timer); st.timer = 0; document.removeEventListener('scroll', soon, { capture: true }); obs.disconnect(); };
take();
return JSON.stringify({ count: st.all.length });
"#
    )
}

/// How many turns capture mode holds: `{"count": n}` (−1: none running).
pub const CAPTURE_COUNT_JS: &str = r#"
const st = window.__snorkleConv;
return JSON.stringify({ count: st && st.live ? st.all.length : -1 });
"#;

/// Stops capture mode and forgets what it held.
pub const CAPTURE_CANCEL_JS: &str = r#"
const st = window.__snorkleConv;
if (st && st.stop) st.stop();
delete window.__snorkleConv;
return '{}';
"#;

/// A capture-mode count; `None` when none is running or malformed.
pub fn parse_count(text: &str) -> Option<u32> {
    if text.len() > MAX_STEP_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let n = v.get("count")?.as_i64()?;
    (0..=1_000_000).contains(&n).then_some(n as u32)
}

/// One turn of the thread, rebuilt.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub user: bool,
    /// The turn's content as safe HTML (built by [`parse`]).
    pub html: String,
}

/// The thread, rebuilt, and the http(s) image URLs it shows (to be served
/// from WebKit's archive).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
    pub images: Vec<Url>,
}

/// Parses [`start_js`]'s answer: the number of sweep steps.
pub fn parse_steps(text: &str) -> Option<u32> {
    if text.len() > MAX_STEP_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let n = v.get("steps")?.as_u64()?;
    Some(n.min(100_000) as u32)
}

/// Parses [`STEP_JS`]'s answer: whether the sweep is done, and how far it
/// is (0–100).
pub fn parse_step(text: &str) -> Option<(bool, u8)> {
    if text.len() > MAX_STEP_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let done = v.get("done")?.as_bool()?;
    let at = v.get("at")?.as_f64().filter(|n| n.is_finite())?.max(0.0);
    let of = v.get("of")?.as_f64().filter(|n| n.is_finite() && *n >= 1.0)?;
    Some((done, ((at / of).min(1.0) * 100.0) as u8))
}

/// Tags kept (HTML and MathML). Anything else is dropped and its children
/// kept in its place.
const TAGS: &[&str] = &[
    "p",
    "br",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "blockquote",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "strong",
    "b",
    "em",
    "i",
    "u",
    "s",
    "del",
    "ins",
    "a",
    "img",
    "hr",
    "sup",
    "sub",
    "span",
    "div",
    "details",
    "summary",
    "mark",
    "kbd",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "code",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "math",
    "semantics",
    "mrow",
    "mi",
    "mo",
    "mn",
    "ms",
    "mtext",
    "mspace",
    "msup",
    "msub",
    "msubsup",
    "mfrac",
    "msqrt",
    "mroot",
    "mtable",
    "mtr",
    "mtd",
    "munder",
    "mover",
    "munderover",
    "mstyle",
    "mpadded",
    "mphantom",
    "menclose",
];
const VOID: &[&str] = &["br", "hr", "img"];

#[cfg(test)]
mod tests;

mod html;
pub use html::*;
