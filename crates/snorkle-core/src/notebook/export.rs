//! A stored notebook outside the chrome (2026-11-24): exported as one
//! self-contained HTML page (Export Notebook in a research session's menu),
//! and a page added to it without it being open (a tab dropped on a
//! research session's row).
//!
//! Security: both work on a notebook already checked by [`super::clean`]
//! (ids, finite numbers, `#rrggbb` colours, http(s) URLs without
//! credentials, page text cleaned and capped, images only as
//! `data:image/{jpeg,png};base64`). The export escapes every string it
//! puts in the page, links only to those http(s) URLs, and carries a
//! content security policy that allows no scripts and no loads but its own
//! `data:` images, so the page shows the notebook and nothing runs when it
//! is opened.

use serde_json::{Map, Value};

use super::{clean, web_url, MAX_TITLE_CHARS};
use crate::text::sanitize_page_text;

/// Escapes text for HTML content and attribute values.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn f(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0)
}

fn s<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// A stroke's points as an SVG path, as the notebook draws it (quadratic
/// curves through the midpoints), moved by (`dx`, `dy`).
fn path_d(pts: &[f64], dx: f64, dy: f64) -> String {
    let p = |i: usize| format!("{:.1} {:.1}", pts[i] + dx, pts[i + 1] + dy);
    if pts.len() < 4 {
        return format!("M{}l0.01 0", p(0));
    }
    let mut d = format!("M{}", p(0));
    let mut i = 2;
    while i + 3 < pts.len() {
        let (mx, my) = ((pts[i] + pts[i + 2]) / 2.0 + dx, (pts[i + 1] + pts[i + 3]) / 2.0 + dy);
        d.push_str(&format!("Q{} {mx:.1} {my:.1}", p(i)));
        i += 2;
    }
    d.push_str(&format!("L{}", p(pts.len() - 2)));
    d
}

/// The site of an address, without `www.`.
fn site(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
        .unwrap_or_default()
}

/// A link's source line: its page's title (or site), linked.
fn source_link(t: &Map<String, Value>) -> String {
    let Some(url) = s(t, "url").and_then(web_url) else { return String::new() };
    let label = s(t, "title").map(str::to_string).unwrap_or_else(|| site(&url));
    format!("<a href=\"{}\">{}</a>", esc(&url), esc(&label))
}

/// A clip's links (2026-11-25), under its picture: each its words (or its
/// address), linked, and its site.
fn clip_links(t: &Map<String, Value>) -> String {
    let mut out = String::new();
    for k in t.get("links").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object) {
        let Some(url) = s(k, "url").and_then(web_url) else { continue };
        let words = s(k, "title").map(str::to_string).unwrap_or_else(|| url.clone());
        out.push_str(&format!(
            "<li><a href=\"{}\">{}</a> <span>{}</span></li>",
            esc(&url),
            esc(&words),
            esc(&site(&url))
        ));
    }
    if out.is_empty() {
        out
    } else {
        format!("<ul class=\"links\">{out}</ul>")
    }
}

/// The notebook (`json`, as stored) as one HTML page titled `title`, with
/// `date` under the title. Its canvas is drawn as it is in the app: the
/// ink as an SVG over the chips (quotes, clips, typed notes and links)
/// where they were placed; pinned chips under the ink. `None` when `json`
/// is not a notebook.
pub fn export_html(json: &str, title: &str, date: &str) -> Option<String> {
    let v: Value = serde_json::from_str(&clean(json)?).ok()?;
    let strokes = v.get("strokes").and_then(Value::as_array).cloned().unwrap_or_default();
    let links = v.get("links").and_then(Value::as_array).cloned().unwrap_or_default();
    // The canvas's extent: every stroke's points, every chip's corner
    // (chips are about 240 wide and 60 high unless sized).
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |x: f64, y: f64| {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    };
    for st in &strokes {
        for p in st.get("points").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]).chunks_exact(2) {
            grow(f(Some(&p[0])), f(Some(&p[1])));
        }
    }
    for l in &links {
        let (x, y) = (f(l.get("x")), f(l.get("y")));
        let w = l.get("w").and_then(Value::as_f64).unwrap_or(240.0);
        let h = l.get("h").and_then(Value::as_f64).unwrap_or(if l.get("image").is_some() { w * 0.75 } else { 60.0 });
        grow(x, y);
        grow(x + w, y + h);
    }
    let empty = !x0.is_finite();
    let (dx, dy) = if empty { (0.0, 0.0) } else { (24.0 - x0, 24.0 - y0) };
    let (width, height) = if empty { (0.0, 0.0) } else { (x1 - x0 + 48.0, y1 - y0 + 48.0) };

    let mut ink = String::new();
    for st in &strokes {
        let pts: Vec<f64> = st
            .get("points")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|p| f(Some(p))).collect())
            .unwrap_or_default();
        if pts.len() < 2 {
            continue;
        }
        let color = st.get("color").and_then(Value::as_str).filter(|c| !c.is_empty()).unwrap_or("var(--ink)");
        let width = f(st.get("width")).max(0.5);
        ink.push_str(&format!(
            "<path d=\"{}\" stroke=\"{}\" stroke-width=\"{width:.1}\"/>",
            path_d(&pts, dx, dy),
            esc(color)
        ));
    }

    let mut chips = String::new();
    for l in &links {
        let Some(o) = l.as_object() else { continue };
        let Some(t) = o.get("target").and_then(Value::as_object) else { continue };
        let (x, y) = (f(o.get("x")) + dx, f(o.get("y")) + dy);
        let width = o.get("w").and_then(Value::as_f64).map(|w| format!("width:{w:.0}px;")).unwrap_or_default();
        let fixed = if o.get("fixed").and_then(Value::as_bool) == Some(true) { " fixed" } else { "" };
        let source = source_link(t);
        let inner = if let Some(img) = s(o, "image") {
            let cap = if source.is_empty() { String::new() } else { format!("<figcaption>{source}</figcaption>") };
            format!("<figure><img alt=\"\" src=\"{}\">{}{cap}</figure>", esc(img), clip_links(t))
        } else if let Some(text) = s(o, "text") {
            let from = if source.is_empty() { String::new() } else { format!("<div class=\"from\">{source}</div>") };
            format!("<div class=\"note\">{}</div>{from}", esc(text))
        } else if s(t, "kind") == Some("quote") {
            let q = s(t, "text").unwrap_or("");
            format!("<blockquote>{}</blockquote><div class=\"from\">{source}</div>", esc(q))
        } else {
            match s(t, "url").and_then(web_url) {
                Some(url) => {
                    let label = s(t, "title").map(str::to_string).unwrap_or_else(|| url.clone());
                    format!(
                        "<a class=\"page\" href=\"{}\">{}</a><div class=\"from\">{}</div>",
                        esc(&url),
                        esc(&label),
                        esc(&site(&url))
                    )
                }
                None => continue,
            }
        };
        chips.push_str(&format!(
            "<div class=\"chip{fixed}\" style=\"left:{x:.0}px;top:{y:.0}px;{width}\">{inner}</div>"
        ));
    }

    let title = sanitize_page_text(title, MAX_TITLE_CHARS);
    let date = sanitize_page_text(date, 64);
    let body = if empty {
        "<p class=\"empty\">This notebook is empty.</p>".to_string()
    } else {
        format!(
            "<div class=\"pad\" style=\"width:{width:.0}px;height:{height:.0}px\">{chips}<svg width=\"{width:.0}\" height=\"{height:.0}\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{ink}</svg></div>"
        )
    };
    Some(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{t}</title><style>{STYLE}</style></head>\
<body><header><h1>{t}</h1><p>{d} · Snorkle notebook</p></header>{body}</body></html>",
        t = esc(&title),
        d = esc(&date),
    ))
}

const STYLE: &str = ":root{--ink:#161616;--bg:#fff;--fg:#161616;--muted:#6e6e6e;--chip:#f4f2ee;--line:#d9d9d9;color-scheme:light dark}\
@media (prefers-color-scheme:dark){:root{--ink:#efefef;--bg:#151515;--fg:#efefef;--muted:#a0a0a0;--chip:#262626;--line:#363636}}\
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 -apple-system,system-ui,sans-serif}\
header{padding:24px 24px 8px}h1{margin:0;font-size:22px}header p{margin:4px 0 0;color:var(--muted)}\
.pad{position:relative;margin:0 0 40px}.pad svg{position:absolute;left:0;top:0;z-index:1;pointer-events:none}\
.chip{position:absolute;z-index:2;max-width:420px;min-width:120px;box-sizing:border-box;padding:8px 10px;border-radius:10px;background:var(--chip);border:1px solid var(--line)}\
.chip.fixed{z-index:0}.note{white-space:pre-wrap}blockquote{margin:0;padding-left:10px;border-left:3px solid #ffd400}\
figure{margin:0}img{display:block;max-width:100%;border-radius:6px}figcaption,.from{margin-top:4px;font-size:12px;color:var(--muted)}\
a{color:inherit}.page{font-weight:600}.empty{padding:0 24px;color:var(--muted)}\
.links{margin:6px 0 0;padding:0;list-style:none;font-size:13px}.links li{margin:2px 0}.links span{color:var(--muted);font-size:12px}";

/// Adds a page (`url`, http(s)) titled `title` to a stored notebook
/// (`None`: a new one) as a link, at the left, below everything in it.
/// `seed` makes the link's id (the chrome's ids look like this one).
/// Returns the notebook to store, checked again; `None` when `url` is not
/// a web address or the result is not a notebook.
pub fn add_page(json: Option<&str>, url: &str, title: &str, seed: u64) -> Option<String> {
    let url = web_url(url)?;
    let mut v: Value = match json {
        Some(j) => serde_json::from_str(&clean(j)?).ok()?,
        None => serde_json::json!({ "strokes": [], "notes": [], "links": [], "pan": { "x": 0, "y": 0 } }),
    };
    let o = v.as_object_mut()?;
    let mut bottom = f64::NEG_INFINITY;
    let mut left = f64::INFINITY;
    for st in o.get("strokes").and_then(Value::as_array).into_iter().flatten() {
        for p in st.get("points").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]).chunks_exact(2) {
            left = left.min(f(Some(&p[0])));
            bottom = bottom.max(f(Some(&p[1])));
        }
    }
    for l in o.get("links").and_then(Value::as_array).into_iter().flatten() {
        let h = l.get("h").and_then(Value::as_f64).unwrap_or(if l.get("image").is_some() { 200.0 } else { 40.0 });
        left = left.min(f(l.get("x")));
        bottom = bottom.max(f(l.get("y")) + h);
    }
    let (x, y) = if bottom.is_finite() { (left.max(-1e6).round(), (bottom + 28.0).round()) } else { (40.0, 40.0) };
    let title = sanitize_page_text(title, MAX_TITLE_CHARS);
    let link = serde_json::json!({
        "id": format!("link-{seed:x}-tab"),
        "x": x,
        "y": y,
        "target": { "kind": "page", "url": url, "title": if title.is_empty() { url.clone() } else { title } },
    });
    o.entry("links").or_insert_with(|| Value::Array(Vec::new())).as_array_mut()?.push(link);
    clean(&serde_json::to_string(&v).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: &str = r##"{"strokes":[{"id":"s-1","points":[10,10,20,20,30,10],"width":2,"color":"","note":"n-1"},
        {"id":"s-2","points":[5,50,60,50],"width":3,"color":"#e5484d","note":"n-1"}],
      "notes":[{"id":"n-1","createdMs":1,"url":"https://a.example/x","title":"A"}],
      "links":[{"id":"l-1","x":100,"y":200,"target":{"kind":"quote","url":"https://a.example/x#:~:text=hi","title":"A <b>page</b>","text":"Said \"so\" & <i>more</i>"}},
        {"id":"l-2","x":40,"y":300,"text":"# My note\n<script>alert(1)</script>","target":{"kind":"page"}},
        {"id":"l-3","x":400,"y":20,"fixed":true,"w":160,"image":"data:image/jpeg;base64,AAAA","target":{"kind":"clip","url":"https://b.example/","title":"B","links":[{"url":"https://b.example/more","title":"Read <more>"},{"url":"https://z.example/q?a=1&b=2","title":""}]}},
        {"id":"l-4","x":300,"y":400,"target":{"kind":"page","url":"https://c.example/p","title":"C page"}}],
      "pan":{"x":0,"y":0}}"##;

    #[test]
    fn export_shows_everything_escaped() {
        let html = export_html(PAD, "Tide <pools>", "2026-11-24").unwrap();
        assert!(html.contains("<title>Tide &lt;pools&gt;</title>"));
        assert!(html.contains("default-src 'none'; img-src data:"), "no scripts or loads run");
        assert!(!html.contains("<script>") && !html.contains("<i>more") && !html.contains("<b>page"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "typed notes are text");
        assert!(html.contains("Said &quot;so&quot; &amp; &lt;i&gt;more&lt;/i&gt;"));
        assert!(html.contains("href=\"https://c.example/p\">C page</a>"));
        assert!(html.contains("src=\"data:image/jpeg;base64,AAAA\"") && html.contains("chip fixed"));
        // A clip's links, under its picture, escaped (2026-11-25).
        assert!(html.contains("<li><a href=\"https://b.example/more\">Read &lt;more&gt;</a> <span>b.example</span></li>"), "{html}");
        assert!(html.contains("<a href=\"https://z.example/q?a=1&amp;b=2\">https://z.example/q?a=1&amp;b=2</a>"));
        assert_eq!(html.matches("<path ").count(), 2);
        assert!(html.contains("stroke=\"#e5484d\"") && html.contains("stroke=\"var(--ink)\""));
        // Moved so the leftmost and topmost points are 24 px in (x 5, y 10):
        // the first stroke's (10, 10) is drawn at (29, 24).
        assert!(html.contains("d=\"M29.0 24.0Q"), "{html}");
        assert!(export_html("[1]", "x", "y").is_none());
        let empty = export_html(r#"{"strokes":[],"links":[]}"#, "", "").unwrap();
        assert!(empty.contains("This notebook is empty."));
    }

    #[test]
    fn export_drops_what_clean_drops() {
        let bad = r#"{"links":[{"id":"l-1","x":1,"y":1,"target":{"kind":"page","url":"javascript:alert(1)","title":"x"}},
            {"id":"l-2","x":1,"y":1,"image":"data:text/html;base64,AAAA","target":{"kind":"clip"}}]}"#;
        let html = export_html(bad, "t", "d").unwrap();
        assert!(!html.contains("javascript:") && !html.contains("text/html"));
    }

    #[test]
    fn pages_are_added_below_everything() {
        let out = add_page(Some(PAD), "https://d.example/", "D \u{202E}page", 7).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let links = v["links"].as_array().unwrap();
        let added = links.last().unwrap();
        assert_eq!(added["target"]["url"], "https://d.example/");
        assert_eq!(added["target"]["title"], "D page");
        assert_eq!(added["x"], 5.0, "at the left of what is there");
        assert_eq!(added["y"], 468.0, "below the lowest chip (400 + 40) and a gap");
        assert_eq!(links.len(), 5);
        let fresh = add_page(None, "https://e.example/", "", 8).unwrap();
        let v: Value = serde_json::from_str(&fresh).unwrap();
        assert_eq!(v["links"][0]["target"]["title"], "https://e.example/");
        assert!(add_page(None, "javascript:alert(1)", "x", 9).is_none());
        assert!(add_page(Some("garbage"), "https://e.example/", "x", 9).is_none());
    }
}
