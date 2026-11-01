//! SECURITY-REVIEW: the text-selection menu (owner request 2026-11-01):
//! "Google This", "Ask AI" and "Define" above the text the user selected.
//!
//! A constant watcher runs in WebKit's isolated client world (main frame)
//! and posts where the selection is to a handler registered in that world
//! only (channel C19), so page scripts cannot call it. It reports geometry
//! only; the selected text is read (C20) when the user picks an action.
//!
//! Security assumptions:
//! - Reports are page-influenced (the page decides what is selectable and
//!   where it is): [`parse_report`] accepts only `null` or four bounded
//!   finite numbers and a positive viewport width.
//! - The text read back is cleaned and capped ([`clean_text`]) and only
//!   used as a search query, a chat draft or a dictionary term.

use serde_json::Value;

use crate::ai::clean_multiline;

/// Name of the selection message handler (client world only).
pub const HANDLER_NAME: &str = "snorkleSelection";
/// Largest report accepted, in bytes.
pub const MAX_REPORT: usize = 256;
/// Characters of selected text used.
pub const MAX_TEXT_CHARS: usize = 2000;

/// Where the selection is, in CSS pixels relative to the visual viewport,
/// and that viewport's width in CSS pixels (the chrome scales by the
/// pane's width in points over `vw`).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct SelectionRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub vw: f64,
}

/// Watches the selection: on change (debounced) posts `{"r":[x,y,w,h],
/// "vw":w}` or `null`; on scroll posts `null` at once and the new place
/// when scrolling stops. Constant code, main frame, document end.
pub fn watch_js() -> String {
    format!(
        r#"(function(){{var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HANDLER_NAME};
if(!h||window.__snorkleSel)return;window.__snorkleSel=1;var t=0,last='';
function send(){{t=0;var s=window.getSelection(),out=null;
if(s&&s.rangeCount&&!s.isCollapsed&&String(s).trim()){{var r=s.getRangeAt(0).getBoundingClientRect(),v=window.visualViewport;
if(r.width||r.height){{var ox=v?v.offsetLeft:0,oy=v?v.offsetTop:0;
out={{r:[Math.round(r.left-ox),Math.round(r.top-oy),Math.round(r.width),Math.round(r.height)],vw:Math.round(v?v.width:innerWidth)}};}}}}
var j=JSON.stringify(out);if(j!==last){{last=j;h.postMessage(j);}}}}
function q(){{clearTimeout(t);t=setTimeout(send,250);}}
document.addEventListener('selectionchange',q);
window.addEventListener('scroll',function(){{if(last&&last!=='null'){{last='null';h.postMessage('null');}}q();}},{{passive:true,capture:true}});
window.addEventListener('resize',q);}})();"#
    )
}

/// Reads the selected text (main frame). Constant code; the result is a
/// string, at most [`MAX_TEXT_CHARS`] characters.
pub const TEXT_JS: &str =
    "(function(){var s=window.getSelection?String(window.getSelection()):'';return s.slice(0,2000);})()";

/// Parses a watcher report: `Some(None)` when nothing is selected,
/// `Some(Some(rect))` for a selection, `None` for anything malformed.
pub fn parse_report(body: &str) -> Option<Option<SelectionRect>> {
    if body.len() > MAX_REPORT {
        return None;
    }
    let v: Value = serde_json::from_str(body).ok()?;
    if v.is_null() {
        return Some(None);
    }
    let r = v.get("r")?.as_array()?;
    if r.len() != 4 {
        return None;
    }
    let n: Vec<f64> = r.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>()?;
    let vw = v.get("vw")?.as_f64()?;
    let bounded = |x: f64| x.is_finite() && x.abs() <= 100_000.0;
    if !n.iter().all(|x| bounded(*x)) || n[2] < 0.0 || n[3] < 0.0 || !bounded(vw) || vw < 1.0 {
        return None;
    }
    Some(Some(SelectionRect { x: n[0], y: n[1], w: n[2], h: n[3], vw }))
}

/// The selected text as read back: cleaned, capped; `None` if empty.
pub fn clean_text(raw: &str) -> Option<String> {
    let t = clean_multiline(raw, MAX_TEXT_CHARS);
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_strict() {
        assert_eq!(parse_report("null"), Some(None));
        assert_eq!(
            parse_report(r#"{"r":[10,20,300,18],"vw":1024}"#),
            Some(Some(SelectionRect { x: 10.0, y: 20.0, w: 300.0, h: 18.0, vw: 1024.0 }))
        );
        for bad in [
            r#"{"r":[1,2,3],"vw":9}"#,
            r#"{"r":[1,2,-3,4],"vw":9}"#,
            r#"{"r":[1,2,3,4],"vw":0}"#,
            r#"{"r":[1e9,2,3,4],"vw":9}"#,
            r#"{"r":["1",2,3,4],"vw":9}"#,
            r#"[1,2]"#,
            "nope",
        ] {
            assert!(parse_report(bad).is_none(), "{bad}");
        }
        assert!(parse_report(&format!("{{\"r\":[1,2,3,4],\"vw\":9,\"x\":\"{}\"}}", "a".repeat(400))).is_none());
        assert_eq!(clean_text("  hello \u{202e}\n\n\nworld ").as_deref(), Some("hello\n\nworld"));
        assert!(clean_text(" \n ").is_none());
        assert!(watch_js().contains(&format!("messageHandlers.{HANDLER_NAME}")));
    }
}
