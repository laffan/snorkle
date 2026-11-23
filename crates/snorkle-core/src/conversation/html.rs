//! The page rebuilt from the captured turns: a fixed set of tags and checked
//! attributes (no scripts, styles, frames or handlers can come out).

use super::*;

pub(super) struct Builder {
    nodes: usize,
    /// Text or images written in the current turn.
    content: usize,
    data_bytes: usize,
    images: Vec<Url>,
}

pub(super) fn escape_into(out: &mut String, text: &str) {
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
pub(super) fn web_url(raw: &str) -> Option<Url> {
    if raw.len() > 4096 {
        return None;
    }
    let u = Url::parse(raw).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none() && u.host().is_some())
        .then_some(u)
}

/// A `data:image/{png,jpeg,gif,webp};base64,` URL whose payload is base64.
pub(super) fn data_image(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("data:image/") else { return false };
    let Some((kind, payload)) = rest.split_once(";base64,") else { return false };
    matches!(kind, "png" | "jpeg" | "jpg" | "gif" | "webp")
        && !payload.is_empty()
        && payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

pub(super) fn small_int(v: &str, max: u32) -> Option<String> {
    v.parse::<u32>().ok().filter(|n| (1..=max).contains(n)).map(|n| n.to_string())
}

/// A MathML length: digits with an optional sign, point and unit.
pub(super) fn length(v: &str) -> bool {
    let v = v.trim_start_matches(['+', '-']);
    let num = v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
    let unit = &v[num.len()..];
    !num.is_empty()
        && num.len() <= 12
        && num.chars().all(|c| c.is_ascii_digit() || c == '.')
        && matches!(unit, "" | "em" | "ex" | "px" | "pt" | "%" | "mu")
}

/// An attribute of `tag`, checked; the value to write, or `None`.
pub(super) fn attr(b: &mut Builder, tag: &str, name: &str, value: &str) -> Option<String> {
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
pub(super) fn node(b: &mut Builder, v: &Value, depth: usize, out: &mut String) -> bool {
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

pub(super) fn close(out: &mut String, tag: &str) {
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
pub(super) fn day(ms: u64) -> String {
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

pub(super) const STYLE: &str = concat!(
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
