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
function turns() {
  const out = [];
  const add = (role, el, key) => { if (el) out.push({ role, el, key: key || '' }); };
  if (site === 'chatgpt') {
    const list = document.querySelectorAll('[data-message-author-role]');
    if (list.length) {
      for (const el of list) {
        const r = el.getAttribute('data-message-author-role');
        if (r === 'user' || r === 'assistant') add(r, el, el.getAttribute('data-message-id'));
      }
    } else {
      for (const h of document.querySelectorAll('h4')) {
        const t = h.textContent.trim();
        const r = /^You said:?$/.test(t) ? 'user' : /^ChatGPT said:?$/.test(t) ? 'assistant' : '';
        if (!r) continue;
        let w = h.parentElement;
        for (let i = 0; i < 12 && w && !(w.classList && w.classList.contains('group')); i++) w = w.parentElement;
        add(r, w || h.parentElement);
      }
    }
  } else if (site === 'claude') {
    const rows = document.querySelectorAll('[data-testid="transcript-row"]');
    if (rows.length) {
      for (const row of rows) {
        add('user', row.querySelector('[data-testid="user-message"]'));
        add('assistant', row.querySelector('[data-testid="assistant-message"]'));
      }
    } else {
      const seen = [];
      for (const el of document.querySelectorAll('[data-testid="user-message"], [data-testid="assistant-message"], .font-claude-response, .font-claude-message')) {
        if (seen.some((s) => s.contains(el))) continue;
        seen.push(el);
        add(el.getAttribute('data-testid') === 'user-message' ? 'user' : 'assistant', el);
      }
    }
  } else if (site === 'gemini') {
    for (const el of document.querySelectorAll('user-query, model-response')) {
      const u = el.tagName === 'USER-QUERY';
      add(u ? 'user' : 'assistant', u ? (el.querySelector('.query-text') || el.querySelector('user-query-content') || el)
        : (el.querySelector('message-content') || el.querySelector('.response-content') || el.querySelector('.markdown') || el), el.id);
    }
  }
  return out;
}
// The turns in the document now, in reading order: by where they are on
// screen (a list that keeps only some turns may add them to the document
// in any order), the document's order breaking ties.
function snap() {
  budget = 0;
  return turns().map((t, i) => {
    const text = (t.el.textContent || '').replace(/\s+/g, ' ').trim();
    return { role: t.role, key: t.role + ':' + (t.key || text.slice(0, 200)), text, tree: tree(t.el, 0), top: t.el.getBoundingClientRect().top, i };
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

struct Builder {
    nodes: usize,
    /// Text or images written in the current turn.
    content: usize,
    data_bytes: usize,
    images: Vec<Url>,
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // Page text in a page of its own (not beside the browser's
            // own UI): only control characters other than line breaks
            // and tabs are left out.
            c if c.is_control() && !matches!(c, '\n' | '\t' | '\r') => {}
            c => out.push(c),
        }
    }
}

/// An http(s) URL without credentials, at most 4,096 characters.
fn web_url(raw: &str) -> Option<Url> {
    if raw.len() > 4096 {
        return None;
    }
    let u = Url::parse(raw).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none() && u.host().is_some())
        .then_some(u)
}

/// A `data:image/{png,jpeg,gif,webp};base64,` URL whose payload is base64.
fn data_image(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("data:image/") else { return false };
    let Some((kind, payload)) = rest.split_once(";base64,") else { return false };
    matches!(kind, "png" | "jpeg" | "jpg" | "gif" | "webp")
        && !payload.is_empty()
        && payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

fn small_int(v: &str, max: u32) -> Option<String> {
    v.parse::<u32>().ok().filter(|n| (1..=max).contains(n)).map(|n| n.to_string())
}

/// A MathML length: digits with an optional sign, point and unit.
fn length(v: &str) -> bool {
    let v = v.trim_start_matches(['+', '-']);
    let num = v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
    let unit = &v[num.len()..];
    !num.is_empty()
        && num.len() <= 12
        && num.chars().all(|c| c.is_ascii_digit() || c == '.')
        && matches!(unit, "" | "em" | "ex" | "px" | "pt" | "%" | "mu")
}

/// An attribute of `tag`, checked; the value to write, or `None`.
fn attr(b: &mut Builder, tag: &str, name: &str, value: &str) -> Option<String> {
    let math = tag.starts_with('m') && tag != "mark";
    match (tag, name) {
        ("a", "href") => web_url(value).map(|u| u.to_string()),
        ("img", "src") => {
            if let Some(u) = web_url(value) {
                if b.images.len() < 2000 && !b.images.contains(&u) {
                    b.images.push(u.clone());
                }
                return Some(u.to_string());
            }
            (data_image(value) && value.len() <= MAX_DATA_IMAGE && b.data_bytes + value.len() <= MAX_DATA_IMAGES).then(
                || {
                    b.data_bytes += value.len();
                    value.to_string()
                },
            )
        }
        ("img", "alt") => Some(sanitize_page_text(value, 300)),
        ("td" | "th", "colspan" | "rowspan") => small_int(value, 100),
        ("ol", "start") => small_int(value, 100_000),
        ("pre", "lang") => (value.len() <= 30
            && value.chars().all(|c| c.is_ascii_alphanumeric() || "+#._-".contains(c)))
        .then(|| value.to_string()),
        (_, "display") if math => matches!(value, "block" | "inline").then(|| value.to_string()),
        (_, "mathvariant") if math => {
            (value.len() <= 24 && value.chars().all(|c| c.is_ascii_lowercase() || c == '-')).then(|| value.to_string())
        }
        (_, "stretchy" | "fence" | "separator" | "accent" | "accentunder" | "displaystyle") if math => {
            matches!(value, "true" | "false").then(|| value.to_string())
        }
        (_, "form") if math => matches!(value, "prefix" | "infix" | "postfix").then(|| value.to_string()),
        (_, "scriptlevel") if math => (value.len() <= 3
            && value.trim_start_matches(['+', '-']).chars().all(|c| c.is_ascii_digit()))
        .then(|| value.to_string()),
        (_, "lspace" | "rspace" | "linethickness" | "width" | "height" | "depth" | "voffset") if math => {
            length(value).then(|| value.to_string())
        }
        (_, "notation") if math => {
            (value.len() <= 60 && value.chars().all(|c| c.is_ascii_lowercase() || c == ' ')).then(|| value.to_string())
        }
        _ => None,
    }
}

/// Writes one node (a text string, or `[tag, {attrs}, ...children]`) as
/// HTML; `false` once a bound is passed (the rest is left out).
fn node(b: &mut Builder, v: &Value, depth: usize, out: &mut String) -> bool {
    b.nodes += 1;
    if b.nodes > MAX_NODES || depth > MAX_DEPTH || out.len() > MAX_HTML {
        return false;
    }
    match v {
        Value::String(t) => {
            if !t.trim().is_empty() {
                b.content += 1;
            }
            escape_into(out, t);
            true
        }
        Value::Array(items) => {
            let Some(raw_tag) = items.first().and_then(Value::as_str) else { return true };
            let tag = TAGS.iter().find(|t| **t == raw_tag).copied();
            if let Some(tag) = tag {
                // `pre`: one string child, its language as a label.
                out.push('<');
                out.push_str(tag);
                if let Some(Value::Object(attrs)) = items.get(1) {
                    for (k, val) in attrs.iter().take(24) {
                        let Some(val) = val.as_str() else { continue };
                        let Some(clean) = attr(b, tag, k, val) else { continue };
                        if tag == "img" && k == "src" {
                            b.content += 1;
                        }
                        let name = if tag == "pre" && k == "lang" { "data-lang" } else { k.as_str() };
                        out.push(' ');
                        out.push_str(name);
                        out.push_str("=\"");
                        escape_into(out, &clean);
                        out.push('"');
                    }
                }
                if tag == "a" {
                    out.push_str(" rel=\"noreferrer\"");
                }
                out.push('>');
                if VOID.contains(&tag) {
                    return true;
                }
                if tag == "pre" {
                    out.push_str("<code>");
                }
            }
            for child in items.iter().skip(2) {
                if !node(b, child, depth + 1, out) {
                    if let Some(tag) = tag {
                        close(out, tag);
                    }
                    return false;
                }
            }
            if let Some(tag) = tag {
                close(out, tag);
            }
            true
        }
        _ => true,
    }
}

fn close(out: &mut String, tag: &str) {
    if VOID.contains(&tag) {
        return;
    }
    if tag == "pre" {
        out.push_str("</code>");
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

/// Parses and rebuilds [`END_JS`]'s answer. `None` when it is malformed or
/// holds no turn.
pub fn parse(text: &str) -> Option<Conversation> {
    if text.len() > MAX_END_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let turns = v.get("turns")?.as_array()?;
    let mut b = Builder { nodes: 0, content: 0, data_bytes: 0, images: Vec::new() };
    let mut out = Vec::new();
    let mut total = 0usize;
    for t in turns.iter().take(MAX_TURNS) {
        let user = match t.get("r").and_then(Value::as_str) {
            Some("user") => true,
            Some("assistant") => false,
            _ => continue,
        };
        let Some(c) = t.get("c") else { continue };
        let mut html = String::new();
        b.content = 0;
        let complete = node(&mut b, c, 0, &mut html);
        total += html.len();
        if b.content > 0 && complete && total <= MAX_HTML {
            out.push(Turn { user, html });
        }
        if !complete || total > MAX_HTML {
            break;
        }
    }
    (!out.is_empty()).then_some(Conversation { turns: out, images: b.images })
}

/// The day of a time, `YYYY-MM-DD` (UTC; civil-from-days).
fn day(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The saved page: the conversation's title, where and when it was saved,
/// and its turns (consecutive turns of one side together), in a fixed
/// style that follows light and dark.
pub fn to_html(conv: &Conversation, site: Site, url: &Url, title: &str, saved_ms: u64) -> Vec<u8> {
    let mut out = String::with_capacity(conv.turns.iter().map(|t| t.html.len() + 120).sum::<usize>() + 4096);
    let title = sanitize_page_text(title, 300);
    out.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\"><title>");
    escape_into(&mut out, &title);
    out.push_str("</title><style>");
    out.push_str(STYLE);
    out.push_str("</style></head><body><header class=\"head\"><h1>");
    escape_into(&mut out, if title.is_empty() { site.assistant() } else { &title });
    out.push_str("</h1><p class=\"meta\">");
    escape_into(&mut out, &format!("{} conversation · saved {} · ", site.assistant(), day(saved_ms)));
    out.push_str("<a href=\"");
    escape_into(&mut out, url.as_str());
    out.push_str("\" rel=\"noreferrer\">");
    escape_into(&mut out, url.as_str());
    out.push_str("</a></p></header><main>");
    let mut open: Option<bool> = None;
    for t in &conv.turns {
        if open != Some(t.user) {
            if open.is_some() {
                out.push_str("</section>");
            }
            out.push_str(if t.user {
                "<section class=\"turn user\"><div class=\"who\">You</div>"
            } else {
                "<section class=\"turn assistant\"><div class=\"who\">"
            });
            if !t.user {
                out.push_str(site.assistant());
                out.push_str("</div>");
            }
            open = Some(t.user);
        }
        out.push_str("<div class=\"msg\">");
        out.push_str(&t.html);
        out.push_str("</div>");
    }
    if open.is_some() {
        out.push_str("</section>");
    }
    out.push_str("</main></body></html>");
    out.into_bytes()
}

const STYLE: &str = concat!(
    ":root{color-scheme:light dark;--fg:#1d1d1f;--muted:#6e6e73;--bg:#fff;--user:#f2f2f4;--line:#d9d9de;--code:#f5f5f7}",
    "@media (prefers-color-scheme:dark){:root{--fg:#ececf0;--muted:#a1a1a8;--bg:#161618;--user:#26262a;--line:#38383d;--code:#202024}}",
    "html{background:var(--bg);color:var(--fg)}",
    "body{margin:0 auto;max-width:780px;padding:28px 20px 80px;font:16px/1.55 -apple-system,BlinkMacSystemFont,\"Helvetica Neue\",sans-serif}",
    ".head h1{font-size:24px;line-height:1.25;margin:0 0 6px}.meta{color:var(--muted);font-size:13px;margin:0 0 28px;overflow-wrap:anywhere}",
    ".meta a{color:inherit}.turn{margin:0 0 26px}.who{font-size:12px;font-weight:600;letter-spacing:.04em;text-transform:uppercase;color:var(--muted);margin:0 0 6px}",
    ".user .msg{background:var(--user);border-radius:14px;padding:10px 14px;white-space:pre-wrap;margin:0 0 8px}",
    ".msg+.msg{margin-top:10px}.msg>:first-child{margin-top:0}.msg>:last-child{margin-bottom:0}",
    "pre{background:var(--code);border:1px solid var(--line);border-radius:10px;padding:12px 14px;overflow:auto;font:13px/1.45 ui-monospace,Menlo,monospace;white-space:pre}",
    "pre[data-lang]::before{content:attr(data-lang);display:block;font-size:11px;color:var(--muted);margin:0 0 6px;font-family:-apple-system,sans-serif}",
    "code{font-family:ui-monospace,Menlo,monospace;font-size:.92em}:not(pre)>code{background:var(--code);border-radius:5px;padding:.1em .3em}",
    "table{border-collapse:collapse;display:block;overflow:auto;margin:12px 0}th,td{border:1px solid var(--line);padding:6px 10px;text-align:left;vertical-align:top}",
    "blockquote{margin:12px 0;padding:0 0 0 14px;border-left:3px solid var(--line);color:var(--muted)}",
    "img{max-width:100%;height:auto;border-radius:8px}a{color:#0a66d8}@media (prefers-color-scheme:dark){a{color:#6aa7ff}}",
    "math[display=block]{margin:10px 0;overflow:auto}hr{border:0;border-top:1px solid var(--line)}"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn sites() {
        assert_eq!(site_of(&u("https://chatgpt.com/c/123")), Some(Site::ChatGpt));
        assert_eq!(site_of(&u("https://chat.openai.com/c/1")), Some(Site::ChatGpt));
        assert_eq!(site_of(&u("https://claude.ai/chat/abc")), Some(Site::Claude));
        assert_eq!(site_of(&u("https://gemini.google.com/app/x")), Some(Site::Gemini));
        for no in
            ["https://notchatgpt.com/", "https://claude.ai.example.com/", "https://google.com/", "http://claude.ai/"]
        {
            assert_eq!(site_of(&u(no)), None, "{no}");
        }
    }

    #[test]
    fn rebuilds_only_safe_markup() {
        let answer = serde_json::json!({ "turns": [
            { "r": "user", "c": ["div", {}, "Hi <script>alert(1)</script> & \u{202e}", ["img", { "src": "data:image/png;base64,iVBORw0K", "alt": "a\"b" }]] },
            { "r": "assistant", "c": ["", {}, ["p", { "style": "x", "onclick": "y" }, "Answer"],
                ["script", {}, "alert(1)"],
                ["a", { "href": "javascript:alert(1)" }, "bad"],
                ["a", { "href": "https://example.com/a?b=1&c=\"" }, "good"],
                ["img", { "src": "https://cdn.example.com/i.png" }],
                ["img", { "src": "data:text/html;base64,PHNjcmlwdD4=" }],
                ["pre", { "lang": "rust\"><x" }, "fn main() {}"],
                ["pre", { "lang": "python" }, "print('<b>')"],
                ["td", { "colspan": "999" }, "x"],
                ["math", { "display": "block", "onload": "x" }, ["mi", { "mathvariant": "bold\"" }, "x"]],
                ["iframe", { "src": "https://evil.example" }, "frame text"],
            ]},
            { "r": "system", "c": ["p", {}, "ignored"] },
        ]});
        let c = parse(&answer.to_string()).unwrap();
        assert_eq!(c.turns.len(), 2);
        assert!(c.turns[0].user && !c.turns[1].user);
        let a = &c.turns[0].html;
        assert_eq!(
            a,
            "<div>Hi &lt;script&gt;alert(1)&lt;/script&gt; &amp; \u{202e}<img alt=\"a&quot;b\" src=\"data:image/png;base64,iVBORw0K\"></div>"
        );
        let b = &c.turns[1].html;
        for bad in
            ["<script", "onclick", "style=", "javascript:", "text/html", "onload", "<iframe", "bold&quot;", "colspan"]
        {
            assert!(!b.contains(bad), "{bad} in {b}");
        }
        assert!(b.contains("<p>Answer</p>") && b.contains("alert(1)"), "unknown tags keep their text: {b}");
        assert!(b.contains("<a href=\"https://example.com/a?b=1&amp;c=%22\" rel=\"noreferrer\">good</a>"), "{b}");
        assert!(b.contains("<a rel=\"noreferrer\">bad</a>"));
        assert!(b.contains("<pre><code>fn main() {}</code></pre>"), "bad language dropped: {b}");
        assert!(b.contains("<pre data-lang=\"python\"><code>print(&#39;&lt;b&gt;&#39;)</code></pre>"));
        assert!(b.contains("<math display=\"block\"><mi>x</mi></math>"));
        assert!(b.contains("frame text"));
        assert_eq!(c.images, vec![u("https://cdn.example.com/i.png")]);
    }

    #[test]
    fn bounded_and_fails_closed() {
        assert!(parse("nope").is_none());
        assert!(parse(r#"{"turns":[]}"#).is_none());
        assert!(parse(r#"{"turns":[{"r":"user","c":["p",{}]}]}"#).is_none(), "empty turns dropped");
        // Deep nesting stops at the bound without failing the whole thread
        // (and serde_json refuses anything past 128 levels).
        let mut deep = serde_json::json!("deep");
        for _ in 0..60 {
            deep = serde_json::json!(["div", {}, deep]);
        }
        let t = serde_json::json!({ "turns": [ { "r": "user", "c": ["p", {}, "first"] }, { "r": "assistant", "c": deep } ] });
        let c = parse(&t.to_string()).unwrap();
        assert_eq!(c.turns.len(), 1, "the turn that passed the bound is left out, and what follows it");
        // Data images: bounded each and in all.
        let big = format!("data:image/png;base64,{}", "A".repeat(MAX_DATA_IMAGE));
        let t = serde_json::json!({ "turns": [ { "r": "user", "c": ["p", {}, "x", ["img", { "src": big }]] } ] });
        let c = parse(&t.to_string()).unwrap();
        assert!(!c.turns[0].html.contains("data:"));
        assert!(parse(&"x".repeat(MAX_END_BYTES + 1)).is_none());
        assert_eq!(parse_steps(r#"{"steps":12}"#), Some(12));
        assert_eq!(parse_steps(r#"{"steps":-1}"#), None);
        assert_eq!(parse_step(r#"{"done":false,"at":50,"of":200}"#), Some((false, 25)));
        assert_eq!(parse_step(r#"{"done":true,"at":500,"of":200}"#), Some((true, 100)));
        assert_eq!(parse_step(r#"{"done":true,"at":1,"of":0}"#), None);
    }

    #[test]
    fn page() {
        let c = Conversation {
            turns: vec![
                Turn { user: true, html: "<p>Q</p>".into() },
                Turn { user: false, html: "<p>A1</p>".into() },
                Turn { user: false, html: "<p>A2</p>".into() },
            ],
            images: vec![],
        };
        let html =
            String::from_utf8(to_html(&c, Site::Claude, &u("https://claude.ai/chat/1"), "My <chat>", 0)).unwrap();
        assert!(html.contains("<title>My &lt;chat&gt;</title>"));
        assert!(html.contains("Claude conversation · saved 1970-01-01"));
        assert_eq!(html.matches("<section").count(), 2, "consecutive turns of one side share a section");
        assert!(!html.contains("<script"));
        assert_eq!(day(1_763_164_800_000), "2025-11-15");
    }

    #[test]
    fn capture_mode() {
        let begin = capture_begin_js();
        assert!(begin.contains("st.live") || begin.contains("live: true"));
        assert!(begin.contains("new MutationObserver(soon)") && begin.contains("setTimeout(take, 300)"));
        assert!(CAPTURE_CANCEL_JS.contains("st.stop()") && END_JS.contains("if (st.stop) st.stop();"));
        assert_eq!(parse_count(r#"{"count":12}"#), Some(12));
        assert_eq!(parse_count(r#"{"count":-1}"#), None);
        assert_eq!(parse_count("nope"), None);
    }

    #[test]
    fn scripts_are_constant_and_bounded() {
        let start = start_js();
        assert!(start.contains("window.__snorkleConv = st") && start.contains("Date.now() + 20000"));
        assert!(STEP_JS.contains("Date.now() - t0 < 6000") && STEP_JS.contains("75000"));
        assert!(END_JS.contains("delete window.__snorkleConv") && END_JS.contains("st.back()"));
        for js in [start.as_str(), STEP_JS, END_JS] {
            assert!(!js.contains("messageHandlers"), "answers go back only as the call's result");
        }
    }
}
