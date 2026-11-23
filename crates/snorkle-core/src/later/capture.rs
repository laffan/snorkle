//! Capture (constant scripts, client world): the page's HTML and the
//! resources it loaded, read back in pieces and checked.

use super::*;

// ---- capture (constant scripts, client world) -------------------------------

/// Starts a capture (channel C22): run with `callAsyncJavaScript` in the
/// page's client world, main frame, only when the user saves the page (or
/// adds pages to a saved one). Collects the URLs the page loaded (the
/// resource timeline, element sources, stylesheets and the `url()`s inside
/// readable ones), fetches the page's HTML again with its session
/// (`cache: force-cache`, so usually from the HTTP cache), keeps the URL
/// list in the client world for [`fetch_js`], and returns
/// `{"html": string|null, "title": string, "urls": [string]}`.
pub const CAPTURE_START_JS: &str = r#"
pub(super) const MAX_URLS = 1500;
pub(super) const here = (() => { const u = new URL(location.href); u.hash = ''; return u.href; })();
pub(super) const seen = new Set();
pub(super) const urls = [];
pub(super) const add = (raw, base) => {
  if (!raw || urls.length >= MAX_URLS) return;
  try {
    const u = new URL(raw, base || document.baseURI);
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return;
    u.hash = '';
    const s = u.href;
    if (s === here || seen.has(s)) return;
    seen.add(s);
    urls.push(s);
  } catch (e) {}
};
pub(super) const cssUrls = (text, base) => {
  const re = /url\(\s*(['"]?)([^'")]+)\1\s*\)/g;
  let m;
  while ((m = re.exec(text)) && urls.length < MAX_URLS) if (!/^(data|blob|#)/i.test(m[2])) add(m[2], base);
};
try { for (const e of performance.getEntriesByType('resource')) add(e.name); } catch (e) {}
for (const el of document.querySelectorAll('script[src], link[href], img, source, video, audio, track, input[type=image], [style*="url("]')) {
  add(el.getAttribute('src'));
  if (el.tagName === 'LINK') add(el.getAttribute('href'));
  if (el.currentSrc) add(el.currentSrc);
  if (el.getAttribute('poster')) add(el.getAttribute('poster'));
  const set = el.getAttribute('srcset');
  if (set) for (const part of set.split(',')) add(part.trim().split(/\s+/)[0]);
  const style = el.getAttribute('style');
  if (style) cssUrls(style);
}
for (const sheet of document.styleSheets) {
  if (sheet.href) add(sheet.href);
  try { for (const rule of sheet.cssRules) cssUrls(rule.cssText, sheet.href || document.baseURI); } catch (e) {}
}
let html = null;
try {
  const r = await fetch(here, { credentials: 'include', cache: 'force-cache' });
  const type = r.headers.get('content-type') || 'text/html';
  if (r.ok && /html/i.test(type)) html = await r.text();
} catch (e) {}
window.__snorkleArchive = { urls, cache: new Map() };
return JSON.stringify({ html, title: document.title || '', urls });
"#;

/// Bytes a [`DRAIN_JS`] answer may carry (base64 included).
pub const MAX_DRAIN_BYTES: usize = 24 * 1024 * 1024;
/// Bytes a [`CAPTURE_START_JS`] answer may carry.
pub const MAX_START_BYTES: usize = 12 * 1024 * 1024;

/// Fetches the URLs at `indices` of the list [`CAPTURE_START_JS`] kept
/// (same-origin ones with the page's cookies, others without; at most
/// [`MAX_RESOURCE_BYTES`] each, 20 s each) into the client world's cache.
/// Only integers are put into the source. Returns the number fetched.
pub fn fetch_js(indices: &[usize]) -> String {
    let list = indices.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    format!(
        r#"
pub(super) const S = window.__snorkleArchive;
if (!S) return 0;
pub(super) const MAX = {max};
pub(super) const b64 = (buf) => {{
  const u = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode.apply(null, u.subarray(i, i + 0x8000));
  return btoa(s);
}};
let n = 0;
await Promise.all([{list}].map(async (i) => {{
  const u = S.urls[i];
  if (!u) return;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 20000);
  try {{
    const same = new URL(u).origin === location.origin;
    const r = await fetch(u, {{ credentials: same ? 'include' : 'omit', cache: 'force-cache', signal: ctl.signal }});
    if (!r.ok || Number(r.headers.get('content-length') || 0) > MAX) return;
    const buf = await r.arrayBuffer();
    if (!buf.byteLength || buf.byteLength > MAX) return;
    S.cache.set(i, {{ i, t: r.headers.get('content-type') || '', b: b64(buf) }});
    n++;
  }} catch (e) {{}} finally {{ clearTimeout(timer); }}
}}));
return n;
"#,
        max = MAX_RESOURCE_BYTES,
    )
}

/// Hands over fetched resources, at most about 8 MB of base64 per call
/// (always at least one), and forgets them: `{"items": [{"i": index, "t":
/// content type, "b": base64}], "more": bool}`.
pub const DRAIN_JS: &str = r#"
pub(super) const S = window.__snorkleArchive;
if (!S) return JSON.stringify({ items: [], more: false });
pub(super) const items = [];
let size = 0;
for (const [k, v] of S.cache) {
  if (items.length && size + v.b.length > 8 * 1024 * 1024) break;
  items.push(v);
  size += v.b.length;
  S.cache.delete(k);
}
return JSON.stringify({ items, more: S.cache.size > 0 });
"#;

/// Ends a capture: drops the client world's list and cache.
pub const END_JS: &str = "delete window.__snorkleArchive; return 0;";

/// What [`CAPTURE_START_JS`] reported, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartReport {
    /// The page's HTML as the server sends it, if it could be fetched.
    pub html: Option<String>,
    pub title: String,
    /// The URLs the page loaded, by the index the script keeps (entries
    /// that are not acceptable URLs are `None`, so indices stay aligned).
    pub urls: Vec<Option<Url>>,
}

/// Parses a [`CAPTURE_START_JS`] answer.
pub fn parse_start(text: &str) -> Option<StartReport> {
    #[derive(Deserialize)]
    struct Raw {
        html: Option<String>,
        #[serde(default)]
        title: String,
        #[serde(default)]
        urls: Vec<String>,
    }
    if text.len() > MAX_START_BYTES {
        return None;
    }
    let raw: Raw = serde_json::from_str(text).ok()?;
    Some(StartReport {
        html: raw.html.filter(|h| !h.is_empty() && h.len() <= MAX_PAGE_BYTES),
        title: sanitize_page_text(&raw.title, TITLE_CHARS),
        urls: raw.urls.iter().take(MAX_CAPTURE_URLS).map(|u| clean_url(u)).collect(),
    })
}

/// Parses a [`DRAIN_JS`] answer into resources (URLs from `urls`, the
/// checked list, never from the answer) and whether more are waiting.
pub fn parse_drain(text: &str, urls: &[Option<Url>]) -> Option<(Vec<Resource>, bool)> {
    #[derive(Deserialize)]
    struct Item {
        i: usize,
        #[serde(default)]
        t: String,
        b: String,
    }
    #[derive(Deserialize)]
    struct Raw {
        items: Vec<Item>,
        #[serde(default)]
        more: bool,
    }
    if text.len() > MAX_DRAIN_BYTES {
        return None;
    }
    let raw: Raw = serde_json::from_str(text).ok()?;
    let out = raw
        .items
        .into_iter()
        .filter_map(|it| {
            let url = urls.get(it.i)?.as_ref()?;
            let data = base64_decode(&it.b).filter(|d| d.len() <= MAX_RESOURCE_BYTES)?;
            clean_resource(Resource { url: url.to_string(), mime: clean_mime(&it.t), encoding: charset_of(&it.t), data })
        })
        .collect();
    Some((out, raw.more))
}

/// Standard base64 (padding optional); `None` on anything else.
pub(super) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = s.trim_end_matches('=').as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}
