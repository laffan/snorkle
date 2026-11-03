//! SECURITY-REVIEW: the image menu (owner request 2026-11-03): right-click,
//! ⌘-click or long-press on an image in a page offers Copy Image, Share
//! Image, Download Image and Copy Image Address.
//!
//! A constant watcher ([`watch_js`]) runs in WebKit's client world, main
//! frame, and posts `{"img": url, "r": [x, y, w, h], "vw": w}` to the
//! selection handler (channel C21, sharing C19's client-world handler, so
//! pages cannot call it). It also makes iOS's own long-press callout skip
//! images (`-webkit-touch-callout: none` on `img`), so only this menu shows.
//!
//! Security assumptions:
//! - The report is page-influenced (the page decides what is under the
//!   pointer and its `src`): [`parse_report`] accepts bounded JSON only,
//!   an http(s) URL without credentials, at most [`MAX_URL_CHARS`], and
//!   finite coordinates. The chrome shows the menu at the reported place
//!   (inside the page's pane) and never displays the URL as text.
//! - The image is fetched only when the user picks Copy, Share or
//!   Download: an ephemeral session, no cookies, image types only, at most
//!   [`MAX_IMAGE_BYTES`] (see `snorkle-content` `ios/http.rs`).

use url::Url;

use crate::selection::{SelectionRect, HANDLER_NAME};

/// Longest image URL accepted.
pub const MAX_URL_CHARS: usize = 4096;
/// Largest report accepted, in bytes.
pub const MAX_REPORT: usize = 5000;
/// Largest image fetched for Copy, Share or Download.
pub const MAX_IMAGE_BYTES: usize = 25 * 1024 * 1024;

/// Watches for an image menu request: `contextmenu` (right-click or a
/// trackpad's secondary click), ⌘-click on an image that is not inside a
/// link (⌘-click on a link opens it in a background tab), and a 0.5 s
/// one-finger press that does not move. The image is the `<img>` under the
/// point, also beneath overlays (`elementsFromPoint`); its `currentSrc`
/// must be http(s). Constant code, main frame, document end.
pub fn watch_js() -> String {
    format!(
        r#"(function(){{var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HANDLER_NAME};
if(!h||window.__snorkleImg)return;window.__snorkleImg=1;
var st=document.createElement('style');st.textContent='img{{-webkit-touch-callout:none!important}}';(document.head||document.documentElement).appendChild(st);
function at(x,y,t){{var e=t&&t.closest?t.closest('img'):null;if(!e&&document.elementsFromPoint){{var l=document.elementsFromPoint(x,y);for(var i=0;i<l.length&&i<12;i++){{if(l[i].tagName==='IMG'){{e=l[i];break;}}}}}}
if(!e)return null;var u=e.currentSrc||e.src||'';return /^https?:/i.test(u)?[e,u]:null;}}
var shown=false;function hide(){{if(shown){{shown=false;h.postMessage('{{"img":null}}');}}}}
document.addEventListener('pointerdown',hide,{{passive:true,capture:true}});window.addEventListener('scroll',hide,{{passive:true,capture:true}});
function post(x,y,u){{shown=true;var v=window.visualViewport,ox=v?v.offsetLeft:0,oy=v?v.offsetTop:0;
h.postMessage(JSON.stringify({{img:u.slice(0,4096),r:[Math.round(x-ox),Math.round(y-oy),1,1],vw:Math.round(v?v.width:innerWidth)}}));}}
document.addEventListener('contextmenu',function(e){{var f=at(e.clientX,e.clientY,e.target);if(!f)return;e.preventDefault();post(e.clientX,e.clientY,f[1]);}},true);
document.addEventListener('click',function(e){{if(!e.metaKey)return;var f=at(e.clientX,e.clientY,e.target);if(!f||f[0].closest('a[href]'))return;e.preventDefault();e.stopPropagation();post(e.clientX,e.clientY,f[1]);}},true);
var p=null,fired=false;
document.addEventListener('touchstart',function(e){{fired=false;if(e.touches.length!==1){{p=null;return;}}var t=e.touches[0],f=at(t.clientX,t.clientY,e.target);if(!f){{p=null;return;}}
var x=t.clientX,y=t.clientY;p={{x:x,y:y,timer:setTimeout(function(){{p=null;fired=true;post(x,y,f[1]);}},500)}};}},{{passive:true,capture:true}});
function stop(){{if(p){{clearTimeout(p.timer);p=null;}}}}
document.addEventListener('touchmove',function(e){{if(!p)return;var t=e.touches[0];if(!t||Math.abs(t.clientX-p.x)>10||Math.abs(t.clientY-p.y)>10)stop();}},{{passive:true,capture:true}});
document.addEventListener('touchend',function(e){{stop();if(fired){{fired=false;if(e.cancelable)e.preventDefault();}}}},{{capture:true}});
document.addEventListener('touchcancel',stop,{{passive:true,capture:true}});}})();"#
    )
}

/// An image menu request: the image's URL and where it was asked for (a
/// 1×1 rectangle in the page's visual viewport, as for the selection).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageReport {
    pub url: Url,
    pub at: SelectionRect,
}

/// Whether a report body is an image report (else it is a selection one).
/// `{"img":null}` asks to close the menu (the page was touched or
/// scrolled).
pub fn is_image_report(body: &str) -> bool {
    body.starts_with("{\"img\":")
}

/// Whether an image report asks to close the menu.
pub fn is_hide_report(body: &str) -> bool {
    body == "{\"img\":null}"
}

/// Parses a report from [`watch_js`]. Fails closed.
pub fn parse_report(body: &str) -> Option<ImageReport> {
    if body.len() > MAX_REPORT || !is_image_report(body) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let raw = v.get("img")?.as_str()?;
    if raw.chars().count() > MAX_URL_CHARS {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let r = v.get("r")?.as_array()?;
    let num = |x: &serde_json::Value| x.as_f64().filter(|n| n.is_finite()).map(|n| n.clamp(-100_000.0, 100_000.0));
    if r.len() != 4 {
        return None;
    }
    let vw = v.get("vw").and_then(num).filter(|w| *w >= 1.0)?;
    Some(ImageReport {
        url,
        at: SelectionRect { x: num(&r[0])?, y: num(&r[1])?, w: num(&r[2])?.max(1.0), h: num(&r[3])?.max(1.0), vw },
    })
}

/// What to do with an image (the chrome's choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageAction {
    Copy,
    Share,
    Download,
}

/// A file name for a downloaded image: the URL's last path segment,
/// reduced to letters, digits, `-`, `_` and `.`, at most 80 characters,
/// with an extension from the MIME type when it has none.
pub fn download_name(url: &Url, mime: &str) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let mut name: String =
        last.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).take(80).collect();
    name = name.trim_start_matches('.').to_string();
    if name.is_empty() {
        name = "image".into();
    }
    if !name.contains('.') {
        let ext = match mime {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            "image/heic" => "heic",
            "image/avif" => "avif",
            _ => "jpg",
        };
        name = format!("{name}.{ext}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_validated() {
        let ok = parse_report(r#"{"img":"https://a.example/p.jpg","r":[10,20,1,1],"vw":800}"#).unwrap();
        assert_eq!(ok.url.as_str(), "https://a.example/p.jpg");
        assert_eq!((ok.at.x, ok.at.y, ok.at.vw), (10.0, 20.0, 800.0));
        for bad in [
            r#"{"img":"javascript:alert(1)","r":[1,1,1,1],"vw":800}"#,
            r#"{"img":"data:image/png;base64,AA","r":[1,1,1,1],"vw":800}"#,
            r#"{"img":"https://u:p@a.example/x","r":[1,1,1,1],"vw":800}"#,
            r#"{"img":"https://a.example/x","r":[1,1,1],"vw":800}"#,
            r#"{"img":"https://a.example/x","r":[1,1,1,1],"vw":0}"#,
            r#"{"r":[1,1,1,1],"vw":800}"#,
        ] {
            assert!(parse_report(bad).is_none(), "{bad}");
        }
        assert!(parse_report(&format!(
            r#"{{"img":"https://a.example/{}","r":[1,1,1,1],"vw":8}}"#,
            "a".repeat(MAX_REPORT)
        ))
        .is_none());
        assert!(is_image_report(r#"{"img":"x"}"#) && !is_image_report(r#"{"r":[1,2,3,4],"vw":1}"#));
        assert!(is_hide_report(r#"{"img":null}"#) && parse_report(r#"{"img":null}"#).is_none());
        let js = watch_js();
        assert!(js.contains(&format!("messageHandlers.{HANDLER_NAME}")) && js.contains("-webkit-touch-callout:none"));
    }

    #[test]
    fn download_names() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(download_name(&u("https://a.example/img/cat.png?x=1"), "image/png"), "cat.png");
        assert_eq!(download_name(&u("https://a.example/img/photo"), "image/webp"), "photo.webp");
        assert_eq!(download_name(&u("https://a.example/"), "image/jpeg"), "image.jpg");
        assert_eq!(download_name(&u("https://a.example/..%2F..%2Fetc"), "image/png"), "2F..2Fetc");
        assert!(!download_name(&u("https://a.example/a%2Fb.png"), "").contains('/'));
    }
}
