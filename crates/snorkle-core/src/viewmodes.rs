//! SECURITY-REVIEW: view modes, the browser's own ways of drawing pages:
//! desaturate (black and white), hide media (images and video) and headers
//! (an outline of the page's headings). Each is set app-wide and a tab can
//! be exempted from it ("Disable for this tab").
//!
//! This module generates the constant sources the content host injects
//! (via `ios/injection.rs`) and parses what the headers extractor reports.
//!
//! Security assumptions / design:
//! - All sources are constant code (no input from pages or the user except
//!   a heading index, an integer), run in WebKit's isolated client world,
//!   main frame only.
//! - Headers (channel C14): the extractor runs in the client world and
//!   posts the page's headings to a message handler registered in that
//!   world only, so pages cannot call it; they can only influence which
//!   headings exist. The text is page-controlled: [`parse_headings`]
//!   accepts a bounded JSON list, checks every field and sanitises the
//!   text, and the chrome renders it with `textContent`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::text::sanitize_page_text;
use crate::userscripts::{json_str, InjectAt, Injection};

/// The view modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Desaturate,
    HideMedia,
    Headers,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Desaturate, Kind::HideMedia, Kind::Headers];
}

/// Which view modes are on (app-wide, or effective for one tab).
/// Session-only: every launch starts with desaturate and hide media on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewModes {
    pub desaturate: bool,
    pub hide_media: bool,
    pub headers: bool,
}

impl Default for ViewModes {
    fn default() -> Self {
        ViewModes { desaturate: true, hide_media: true, headers: false }
    }
}

impl ViewModes {
    pub fn get(&self, k: Kind) -> bool {
        match k {
            Kind::Desaturate => self.desaturate,
            Kind::HideMedia => self.hide_media,
            Kind::Headers => self.headers,
        }
    }

    pub fn set(&mut self, k: Kind, on: bool) {
        match k {
            Kind::Desaturate => self.desaturate = on,
            Kind::HideMedia => self.hide_media = on,
            Kind::Headers => self.headers = on,
        }
    }

    /// The modes in effect for a tab exempted from `exempt`.
    pub fn for_tab(mut self, exempt: &BTreeSet<Kind>) -> ViewModes {
        for k in exempt {
            self.set(*k, false);
        }
        self
    }
}

/// Fixed style for desaturate. `filter` on the root element does not create
/// a containing block for fixed-position content.
pub const GRAYSCALE_CSS: &str = "html{filter:grayscale(1)!important}";
/// Fixed style for hide media: image and video elements, and embedded
/// players of the common video sites, are removed from layout, and CSS
/// background images are dropped (WebKit then does not fetch them; `<img>`
/// sources are still fetched). Inline SVG icons stay visible.
pub const NO_MEDIA_CSS: &str = concat!(
    "img,picture,input[type=image],svg image,object[type^=\"image/\"],embed[type^=\"image/\"],",
    "video,object[type^=\"video/\"],embed[type^=\"video/\"],",
    "iframe[src*=\"youtube.com/embed\"],iframe[src*=\"youtube-nocookie.com/embed\"],",
    "iframe[src*=\"player.vimeo.com\"],iframe[src*=\"dailymotion.com/embed\"],",
    "iframe[src*=\"player.twitch.tv\"],iframe[src*=\"tiktok.com/embed\"]",
    "{display:none!important}*,*::before,*::after{background-image:none!important}"
);

/// Name of the headings message handler (client world only).
pub const HEADINGS_HANDLER_NAME: &str = "snorkleHeadings";
/// Headings reported per page, and characters per heading.
pub const MAX_HEADINGS: usize = 300;
pub const MAX_HEADING_CHARS: usize = 200;
/// Largest report accepted from the extractor, in bytes.
pub const MAX_HEADINGS_REPORT: usize = 96 * 1024;

const HEADING_SELECTOR: &str = "h1,h2,h3,h4,h5,h6";

/// Starts the headings extractor (idempotent). It reports `[[level, text,
/// index], …]` as a JSON string on load and whenever the DOM changes
/// (debounced), only when the list changed. `index` is the heading's
/// position among all h1–h6 elements, used to scroll back to it.
fn headings_start_js() -> String {
    format!(
        r#"(function(){{
var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HEADINGS_HANDLER_NAME};
if(!h||window.__snorkleHeadings)return;
var last='',timer=0;
function collect(){{timer=0;var out=[],els=document.querySelectorAll({sel});
for(var i=0;i<els.length&&out.length<{MAX_HEADINGS};i++){{var e=els[i];if(!e.getClientRects().length)continue;
var t=(e.innerText||e.textContent||'').replace(/\s+/g,' ').trim().slice(0,{MAX_HEADING_CHARS});if(!t)continue;
out.push([+e.tagName.charAt(1),t,i]);}}
var s=JSON.stringify(out);if(s!==last){{last=s;h.postMessage(s);}}}}
function schedule(){{if(!timer)timer=setTimeout(collect,400);}}
var obs=new MutationObserver(schedule);
obs.observe(document.documentElement,{{childList:true,subtree:true,characterData:true}});
window.__snorkleHeadings={{obs:obs,cancel:function(){{clearTimeout(timer);}}}};
collect();
}})();"#,
        sel = json_str(HEADING_SELECTOR)
    )
}

const HEADINGS_STOP_JS: &str =
    "(function(){var s=window.__snorkleHeadings;if(!s)return;s.obs.disconnect();s.cancel();delete window.__snorkleHeadings;})();";

/// Injected sources for the modes in `modes` (constant code).
pub fn injections(modes: ViewModes) -> Vec<Injection> {
    let mut out = Vec::new();
    if modes.desaturate {
        out.push(Injection {
            source: style_js("__snorkle_grayscale", GRAYSCALE_CSS, true),
            at: InjectAt::DocumentStart,
        });
    }
    if modes.hide_media {
        out.push(Injection {
            source: style_js("__snorkle_no_images", NO_MEDIA_CSS, true),
            at: InjectAt::DocumentStart,
        });
    }
    if modes.headers {
        out.push(Injection { source: headings_start_js(), at: InjectAt::DocumentEnd });
    }
    out
}

/// Brings an already-loaded page in line with `modes` (adds or removes the
/// styles, starts or stops the headings extractor). Constant code.
pub fn apply_js(modes: ViewModes) -> String {
    let headers = if modes.headers { headings_start_js() } else { HEADINGS_STOP_JS.to_string() };
    style_js("__snorkle_grayscale", GRAYSCALE_CSS, modes.desaturate)
        + &style_js("__snorkle_no_images", NO_MEDIA_CSS, modes.hide_media)
        + &headers
}

/// Scrolls the page to the heading at `index` among its h1–h6 elements.
/// The only input is an integer.
pub fn scroll_to_heading_js(index: u32) -> String {
    format!(
        "(function(){{var e=document.querySelectorAll({})[{index}];if(e)e.scrollIntoView({{block:'start',behavior:'smooth'}});}})();",
        json_str(HEADING_SELECTOR)
    )
}

fn style_js(id: &str, css: &str, on: bool) -> String {
    let id = json_str(id);
    if on {
        format!(
            "(function(){{if(document.getElementById({id}))return;var st=document.createElement('style');st.id={id};st.textContent={};(document.head||document.documentElement).appendChild(st);}})();",
            json_str(css)
        )
    } else {
        format!("(function(){{var st=document.getElementById({id});if(st)st.remove();}})();")
    }
}

/// One heading of the page outline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Heading {
    /// 1–6.
    pub level: u8,
    pub text: String,
    /// Position among the page's h1–h6 elements (for scrolling).
    pub index: u32,
}

/// Parses an extractor report. Fails closed: anything but a bounded list of
/// `[level 1–6, non-empty string, index]` entries is rejected.
pub fn parse_headings(body: &str) -> Option<Vec<Heading>> {
    if body.len() > MAX_HEADINGS_REPORT {
        return None;
    }
    let raw: Vec<(u8, String, u32)> = serde_json::from_str(body).ok()?;
    if raw.len() > MAX_HEADINGS {
        return None;
    }
    raw.into_iter()
        .map(|(level, text, index)| {
            let text = sanitize_page_text(&text, MAX_HEADING_CHARS);
            ((1..=6).contains(&level) && !text.is_empty()).then_some(Heading { level, text, index })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_exemptions() {
        let m = ViewModes::default();
        assert!(m.desaturate && m.hide_media && !m.headers);
        let exempt: BTreeSet<Kind> = [Kind::Desaturate].into();
        let t = m.for_tab(&exempt);
        assert!(!t.desaturate && t.hide_media);
        assert_eq!(injections(t).len(), 1);
    }

    #[test]
    fn injections_per_mode() {
        let all = ViewModes { desaturate: true, hide_media: true, headers: true };
        let inj = injections(all);
        assert_eq!(inj.len(), 3);
        assert!(inj[0].source.contains("grayscale(1)"));
        assert!(inj[1].source.contains("video,"));
        assert!(inj[2].source.contains(&format!("messageHandlers.{HEADINGS_HANDLER_NAME}")));
        assert_eq!(inj[2].at, InjectAt::DocumentEnd);
        assert!(injections(ViewModes { desaturate: false, hide_media: false, headers: false }).is_empty());
        let off = apply_js(ViewModes { desaturate: false, hide_media: false, headers: false });
        assert!(off.contains("__snorkle_grayscale\");if(st)st.remove()"));
        assert!(off.contains("obs.disconnect()"));
    }

    #[test]
    fn headings_are_validated() {
        let h = parse_headings("[[1,\"Intro \\u202e evil\",0],[3,\"Deep\",4]]").unwrap();
        assert_eq!(
            h,
            vec![
                Heading { level: 1, text: "Intro evil".into(), index: 0 },
                Heading { level: 3, text: "Deep".into(), index: 4 },
            ]
        );
        assert!(parse_headings(r#"[[7,"x",0]]"#).is_none(), "level out of range");
        assert!(parse_headings(r#"[[1,"",0]]"#).is_none(), "empty text");
        assert!(parse_headings(r#"[[1,"x",-1]]"#).is_none());
        assert!(parse_headings(r#"{"a":1}"#).is_none());
        assert!(parse_headings(&"x".repeat(MAX_HEADINGS_REPORT + 1)).is_none());
        let many = format!("[{}]", vec![r#"[2,"x",1]"#; MAX_HEADINGS + 1].join(","));
        assert!(parse_headings(&many).is_none());
        assert_eq!(parse_headings("[]").unwrap(), vec![]);
    }

    #[test]
    fn scroll_takes_only_an_index() {
        let js = scroll_to_heading_js(12);
        assert!(js.contains("[12]"));
        assert!(js.contains("scrollIntoView"));
    }
}
