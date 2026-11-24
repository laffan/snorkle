//! SECURITY-REVIEW: the pin guard's page watcher (owner request
//! 2026-11-24, channel C25). A pin's tab keeps its page; going elsewhere
//! from it opens a new tab (`browser::pins`). Real link navigations are
//! caught in the navigation policy, but a single-page app changes its
//! address without one (`history.pushState` from its own click handler),
//! so a constant watcher in WebKit's isolated client world (main frame,
//! every page, with the selection watcher, posting to C19's handler)
//! stops such clicks while the guard is on for the tab.
//!
//! The guard is off in every page until the host turns it on for a pin's
//! tab ([`set_js`], constant code with one boolean, after each commit and
//! finished load). While on, the watcher:
//! - for a real (`isTrusted`) plain primary click on an `http(s)` link to
//!   another page (the address without its fragment differs), with no
//!   `download` attribute and no other window as its target: stops the
//!   click before the page's own handlers see it (capture phase on the
//!   window, `preventDefault` and `stopImmediatePropagation`) and posts
//!   `{"pin":"link","u":"<the link's address>"}`;
//! - for any other real click or Return / space press: posts
//!   `{"pin":"tap"}`, so an address change right after it is known to be
//!   the user's (`browser::GESTURE_WINDOW_MS`).
//!
//! Security assumptions:
//! - Reports are page-influenced: the page decides its links' addresses
//!   and can arrange what a real click lands on. [`parse_report`] accepts
//!   only the two shapes above, with an http(s) address without
//!   credentials of at most [`MAX_URL_CHARS`]; the core sends it through
//!   the navigation policy as a link with a user gesture (as a ⌘-clicked
//!   link), only for a pin's tab and only when it leaves the tab's page,
//!   at most `browser::MAX_DIVERTS` a minute.
//! - The page's scripts cannot see or call the watcher or its handler
//!   (client world), nor turn the guard on or off; they can only stop
//!   reports by stopping the click themselves earlier in the same phase.

use url::Url;

use crate::selection::HANDLER_NAME;

/// Largest report accepted, in bytes.
pub const MAX_REPORT: usize = 4_200;
/// Longest link address reported.
pub const MAX_URL_CHARS: usize = 4_096;

/// What the watcher reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinReport {
    /// A real click on a link to another page, stopped.
    Link(Url),
    /// Another real click or key press.
    Tap,
}

/// The watcher (constant code, client world, main frame, document end).
pub fn watch_js() -> String {
    format!(
        r#"(function(){{var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HANDLER_NAME};
if(!h||window.__snorklePinWatch)return;window.__snorklePinWatch=1;
function bare(u){{var x=new URL(u,location.href);x.hash='';return x.href;}}
function link(e){{var p=e.composedPath?e.composedPath():[];for(var i=0;i<p.length&&i<64;i++){{var n=p[i];
if(n===document||n===window)break;if(n&&n.nodeType===1&&n.localName==='a'&&n.hasAttribute('href'))return n;}}return null;}}
window.addEventListener('click',function(e){{if(!window.__snorklePinGuard||!e.isTrusted)return;
var a=(e.button===0&&!e.metaKey&&!e.ctrlKey&&!e.shiftKey&&!e.altKey)?link(e):null;
if(a){{var t=(a.getAttribute('target')||'').toLowerCase(),u=null;try{{u=new URL(a.getAttribute('href'),document.baseURI);}}catch(x){{}}
if(u&&/^https?:$/.test(u.protocol)&&!a.hasAttribute('download')&&(!t||t==='_self'||t==='_top'||t==='_parent')&&bare(u.href)!==bare(location.href)&&u.href.length<={MAX_URL_CHARS}){{
e.preventDefault();e.stopImmediatePropagation();h.postMessage(JSON.stringify({{pin:'link',u:u.href}}));return;}}}}
h.postMessage('{{"pin":"tap"}}');}},true);
window.addEventListener('keydown',function(e){{if(window.__snorklePinGuard&&e.isTrusted&&(e.key==='Enter'||e.key===' '))h.postMessage('{{"pin":"tap"}}');}},true);}})();"#
    )
}

/// Turns the guard on or off in the page shown (constant code, one
/// boolean; client world, so pages cannot read or set it).
pub fn set_js(on: bool) -> String {
    format!("window.__snorklePinGuard={};", if on { "true" } else { "false" })
}

/// Whether a report on C19's handler is the pin guard's.
pub fn is_report(body: &str) -> bool {
    body.starts_with("{\"pin\":")
}

/// Parses a report from [`watch_js`]. Fails closed.
pub fn parse_report(body: &str) -> Option<PinReport> {
    if body.len() > MAX_REPORT || !is_report(body) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let o = v.as_object()?;
    match o.get("pin")?.as_str()? {
        "tap" if o.len() == 1 => Some(PinReport::Tap),
        "link" if o.len() == 2 => {
            let raw = o.get("u")?.as_str()?;
            if raw.chars().count() > MAX_URL_CHARS {
                return None;
            }
            let url = Url::parse(raw).ok()?;
            let plain =
                matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none();
            plain.then_some(PinReport::Link(url))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_parsed_strictly() {
        assert_eq!(parse_report(r#"{"pin":"tap"}"#), Some(PinReport::Tap));
        assert_eq!(
            parse_report(r#"{"pin":"link","u":"https://x.example/a?b=1#c"}"#),
            Some(PinReport::Link(Url::parse("https://x.example/a?b=1#c").unwrap()))
        );
        for bad in [
            r#"{"pin":"link","u":"javascript:alert(1)"}"#,
            r#"{"pin":"link","u":"https://user:pw@x.example/"}"#,
            r#"{"pin":"link","u":"file:///etc/passwd"}"#,
            r#"{"pin":"link","u":"not a url"}"#,
            r#"{"pin":"link","u":"https://x.example/","x":1}"#,
            r#"{"pin":"tap","u":"https://x.example/"}"#,
            r#"{"pin":"other"}"#,
            r#"{"pin":1}"#,
            r#"{"img":null}"#,
            "null",
        ] {
            assert_eq!(parse_report(bad), None, "{bad}");
        }
        let long = format!(r#"{{"pin":"link","u":"https://x.example/{}"}}"#, "a".repeat(5000));
        assert_eq!(parse_report(&long), None);
        assert!(is_report(r#"{"pin":"tap"}"#) && !is_report(r#"{"nb":"end"}"#));
    }

    #[test]
    fn the_watcher_is_constant_and_off_until_told() {
        let js = watch_js();
        assert!(js.contains("if(!window.__snorklePinGuard||!e.isTrusted)return;"));
        assert!(js.contains(HANDLER_NAME) && js.contains("stopImmediatePropagation"));
        assert_eq!(set_js(true), "window.__snorklePinGuard=true;");
        assert_eq!(set_js(false), "window.__snorklePinGuard=false;");
    }
}
