//! A clip's region of its page (C23 read-back; see the module docs): the
//! text shown there, the links laid out there and its box in the page's
//! document (2026-11-25), checked; and the constant code that goes back to
//! that box.

use serde_json::Value;

use super::{web_url, MAX_CLIP_LINKS, MAX_LINK_TITLE_CHARS, MAX_REGION_BYTES, MAX_REGION_CHARS};
use crate::text::sanitize_page_text;

/// What a region of the page holds (visual-viewport CSS pixels, as the
/// clip's report gave it), read back as `{"t": text, "l": links, "at":
/// box}` ([`parse_region`]): the text shown there, for the notebook's
/// search (text nodes laid out in the region and images' `alt`); the
/// http(s) links laid out there (2026-11-25: at most 20, in page order,
/// each `[address, its text]`, links within the page itself left out),
/// shown under the clip; and the region's box in the page's document
/// (CSS pixels from its top left), so going back to the clip scrolls there
/// ([`back_js`]).
pub fn region_text_js(x: f64, y: f64, w: f64, h: f64) -> String {
    let n = |v: f64| if v.is_finite() { v.clamp(-100_000.0, 100_000.0) } else { 0.0 };
    format!(
        "(function(){{var n=window.__snorkleNb;return n&&n.regionText?n.regionText({:.1},{:.1},{:.1},{:.1}):'';}})()",
        n(x),
        n(y),
        n(w).max(0.0),
        n(h).max(0.0)
    )
}

/// A link laid out in a clip's region (2026-11-25): its address (http(s),
/// [`web_url`]) and its text (cleaned; may be empty).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ClipLink {
    pub url: String,
    pub title: String,
}

/// What [`region_text_js`] read in a clip's region, checked: its text
/// (cleaned and capped; may be empty), its links (at most
/// [`MAX_CLIP_LINKS`], no address twice) and its box in the page's
/// document ([`clip_box`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    pub text: String,
    pub links: Vec<ClipLink>,
    pub at: Option<[f64; 4]>,
}

/// [`region_text_js`]'s answer, checked ([`Region`]); `None` if it is
/// malformed or too large.
pub fn parse_region(text: &str) -> Option<Region> {
    if text.len() > MAX_REGION_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let o = v.as_object()?;
    let t = crate::ai::clean_multiline(o.get("t")?.as_str()?, MAX_REGION_CHARS);
    Some(Region { text: t, links: clip_links(o.get("l")), at: clip_box(o.get("at")) })
}

/// A clip's links, as the page reports them (`[[address, text], ...]`) or
/// as the notebook stores them (`[{"url", "title"}, ...]`): valid http(s)
/// addresses only, each once, at most [`MAX_CLIP_LINKS`]; texts cleaned.
pub(super) fn clip_links(v: Option<&Value>) -> Vec<ClipLink> {
    let mut out: Vec<ClipLink> = Vec::new();
    for item in v.and_then(Value::as_array).into_iter().flatten().take(4 * MAX_CLIP_LINKS) {
        let (url, title) = match item {
            Value::Array(a) => (a.first().and_then(Value::as_str), a.get(1).and_then(Value::as_str)),
            Value::Object(m) => (m.get("url").and_then(Value::as_str), m.get("title").and_then(Value::as_str)),
            _ => continue,
        };
        let Some(url) = url.and_then(web_url) else { continue };
        if out.iter().any(|l| l.url == url) {
            continue;
        }
        out.push(ClipLink { url, title: sanitize_page_text(title.unwrap_or(""), MAX_LINK_TITLE_CHARS) });
        if out.len() == MAX_CLIP_LINKS {
            break;
        }
    }
    out
}

/// A clip's box in its page's document (`[x, y, width, height]`, CSS
/// pixels): four finite numbers, the corner at most 10,000,000 from the
/// top left (and not before it), the size at most 100,000.
pub(super) fn clip_box(v: Option<&Value>) -> Option<[f64; 4]> {
    let a = v?.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let n = |i: usize, max: f64| a[i].as_f64().filter(|n| n.is_finite()).map(|n| n.clamp(0.0, max).round());
    Some([n(0, 1e7)?, n(1, 1e7)?, n(2, 1e5)?, n(3, 1e5)?])
}

/// Going back to a clip in its page (2026-11-25): scrolls the page to the
/// clip's box ([`Region::at`]) and outlines it there for a moment, with
/// the clip stage's dashed border. Constant code taking four numbers.
pub fn back_js(x: f64, y: f64, w: f64, h: f64) -> String {
    let n = |v: f64, max: f64| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
    format!(
        "window.__snorkleNb&&window.__snorkleNb.back({:.0},{:.0},{:.0},{:.0});",
        n(x, 1e7),
        n(y, 1e7),
        n(w, 1e5),
        n(h, 1e5)
    )
}
