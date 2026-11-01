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

use std::collections::{BTreeMap, BTreeSet};

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
/// Elements hide media turns into gray boxes: images, video, canvas
/// (animations, WebGL), embedded objects and the players of common video
/// sites. Inline SVG icons stay as they are.
const MEDIA_SELECTOR: &str = concat!(
    "img,input[type=image],svg image,video,canvas,object,embed,",
    "iframe[src*=\"youtube.com/embed\"],iframe[src*=\"youtube-nocookie.com/embed\"],",
    "iframe[src*=\"player.vimeo.com\"],iframe[src*=\"dailymotion.com/embed\"],",
    "iframe[src*=\"player.twitch.tv\"],iframe[src*=\"tiktok.com/embed\"],",
    "iframe[src*=\"/embed/\"],iframe[src*=\"player\"],iframe[src*=\"video\"]"
);
/// Fixed style for hide media: each media element keeps its box but is
/// drawn as a solid gray rectangle (a gray background, then a filter that
/// flattens every pixel to the same gray); pointing at one fades it in, in
/// black and white. The hidden and shown filters use the same functions in
/// the same order, so the change is interpolated (contrast 0 → 1 brings the
/// picture out of the flat gray). Media under an overlay (video players,
/// images behind transparent links) never get `:hover`, so the pointer
/// tracker in [`style_js`] also marks the media under the pointer with
/// [`REVEAL_ATTR`]. CSS background images are dropped (WebKit then does not
/// fetch them); `<img>` sources are still fetched so pointing can show them.
pub const NO_MEDIA_CSS: &str = concat!(
    "img,input[type=image],svg image,video,canvas,object,embed,",
    "iframe[src*=\"youtube.com/embed\"],iframe[src*=\"youtube-nocookie.com/embed\"],",
    "iframe[src*=\"player.vimeo.com\"],iframe[src*=\"dailymotion.com/embed\"],",
    "iframe[src*=\"player.twitch.tv\"],iframe[src*=\"tiktok.com/embed\"],",
    "iframe[src*=\"/embed/\"],iframe[src*=\"player\"],iframe[src*=\"video\"]",
    "{background-color:#c8c8c8!important;filter:grayscale(1) contrast(0) brightness(1.57)!important;",
    "transition:filter .3s ease-out,background-color .3s ease-out!important}",
    "img:hover,input[type=image]:hover,svg image:hover,video:hover,canvas:hover,object:hover,embed:hover,iframe:hover,",
    ":root [data-snorkle-reveal][data-snorkle-reveal]",
    "{background-color:transparent!important;filter:grayscale(1) contrast(1) brightness(1)!important}",
    "*,*::before,*::after{background-image:none!important}"
);
/// Attribute the pointer tracker puts on the media under the pointer.
pub const REVEAL_ATTR: &str = "data-snorkle-reveal";

/// Name of the headings message handler (client world only).
pub const HEADINGS_HANDLER_NAME: &str = "snorkleHeadings";
/// Headings reported per page, and characters per heading.
pub const MAX_HEADINGS: usize = 300;
pub const MAX_HEADING_CHARS: usize = 200;
/// Largest report accepted from the extractor, in bytes.
pub const MAX_HEADINGS_REPORT: usize = 96 * 1024;

const HEADING_SELECTOR: &str = "h1,h2,h3,h4,h5,h6";

/// A site-specific heading rule (from "Detect Headers"): elements matching
/// `selector` are headings of `level`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadingRule {
    pub selector: String,
    pub level: u8,
}

/// Heading rules per site (registrable domain; also applies to subdomains).
pub type HeadingRules = BTreeMap<String, Vec<HeadingRule>>;

pub const MAX_RULES_PER_SITE: usize = 12;
pub const MAX_SELECTOR_CHARS: usize = 200;

/// Accepts a CSS selector list for `querySelectorAll` only: bounded, one
/// line, and none of the characters that could end a rule or start markup
/// (`{ } ; < \ @` and quotes are refused; attribute values must be
/// unquoted; `>` is the child combinator). An invalid selector only makes the extractor fall back.
pub fn validate_rule(selector: &str, level: u8) -> Option<HeadingRule> {
    let sel = selector.trim();
    let ok_chars =
        sel.chars().all(|c| c.is_ascii_graphic() || c == ' ') && !sel.chars().any(|c| "{};<\\@\"'`".contains(c));
    (ok_chars && !sel.is_empty() && sel.len() <= MAX_SELECTOR_CHARS && (1..=6).contains(&level))
        .then(|| HeadingRule { selector: sel.to_string(), level })
}

/// Starts the headings extractor (idempotent). It reports `[[level, text,
/// index], …]` as a JSON string on load and whenever the DOM changes
/// (debounced), only when the list changed, and `{"c": index}` for the
/// heading the reader has scrolled to. `index` is the heading's position
/// among the elements matching the page's heading selector (h1–h6, or the
/// site's detected rules), used to scroll back to it.
fn headings_start_js(rules: &HeadingRules) -> String {
    let rules_json = json_str(&serde_json::to_string(rules).unwrap_or_else(|_| "{}".into()));
    format!(
        r#"(function(){{
var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HEADINGS_HANDLER_NAME};
if(!h||window.__snorkleHeadings)return;
var all=JSON.parse({rules_json}),host=location.hostname.toLowerCase(),rules=null;
for(var d in all){{if(host===d||host.endsWith('.'+d)){{rules=all[d];break;}}}}
var sel={default_sel};
if(rules&&rules.length){{try{{var u=rules.map(function(r){{return r.selector;}}).join(',');document.querySelector(u);sel=u;}}catch(e){{rules=null;}}}}else{{rules=null;}}
function level(e){{if(!rules)return +e.tagName.charAt(1)||1;for(var i=0;i<rules.length;i++){{try{{if(e.matches(rules[i].selector))return rules[i].level;}}catch(x){{}}}}return 1;}}
var last='',timer=0,list=[],cur=-1,stimer=0;
function collect(){{timer=0;var out=[],els=document.querySelectorAll(sel);list=[];
for(var i=0;i<els.length&&out.length<{MAX_HEADINGS};i++){{var e=els[i];if(!e.getClientRects().length)continue;
var t=(e.innerText||e.textContent||'').replace(/\s+/g,' ').trim().slice(0,{MAX_HEADING_CHARS});if(!t)continue;
out.push([level(e),t,i]);list.push([e,i]);}}
var s=JSON.stringify(out);if(s!==last){{last=s;h.postMessage(s);cur=-1;}}position();}}
function position(){{stimer=0;var c=-1;for(var k=0;k<list.length;k++){{if(list[k][0].getBoundingClientRect().top<=96)c=list[k][1];else break;}}
if(c<0&&list.length)c=list[0][1];if(c!==cur){{cur=c;h.postMessage(JSON.stringify({{c:c}}));}}}}
function schedule(){{if(!timer)timer=setTimeout(collect,400);}}
function onScroll(){{if(!stimer)stimer=setTimeout(position,150);}}
var obs=new MutationObserver(schedule);
obs.observe(document.documentElement,{{childList:true,subtree:true,characterData:true}});
window.addEventListener('scroll',onScroll,{{passive:true,capture:true}});
window.__snorkleHeadings={{obs:obs,sel:sel,cancel:function(){{clearTimeout(timer);clearTimeout(stimer);window.removeEventListener('scroll',onScroll,true);}}}};
collect();
}})();"#,
        default_sel = json_str(HEADING_SELECTOR)
    )
}

const HEADINGS_STOP_JS: &str =
    "(function(){var s=window.__snorkleHeadings;if(!s)return;s.obs.disconnect();s.cancel();delete window.__snorkleHeadings;})();";

/// Candidates sent to "Detect Headers".
pub const MAX_SAMPLE: usize = 200;
/// Largest sample accepted from a page, in bytes.
pub const MAX_SAMPLE_REPORT: usize = 96 * 1024;

/// Collects a compact sample of the page's heading-like elements for
/// "Detect Headers" and posts `{"sample": [...]}`: for each candidate (in
/// document order) its tag, id, first classes, font size and weight,
/// whether it is uppercase (section labels often are), the parent's tag and
/// classes, its enclosing section element (`section`, `article`, `aside`,
/// … with classes and `aria-label`), and the start of its text. Constant code; run
/// only when the user presses the button.
pub fn headings_sample_js() -> String {
    format!(
        r#"(function(){{
var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HEADINGS_HANDLER_NAME};if(!h)return;
function cls(e){{return (e.className&&e.className.baseVal===undefined?String(e.className):'').split(/\s+/).filter(Boolean).slice(0,4).map(function(c){{return c.slice(0,40);}});}}
function sec(e){{var a=e.parentElement;while(a&&a!==document.body){{var t=a.tagName.toLowerCase();
if(/^(section|article|aside|nav|main|header|footer)$/.test(t)||a.getAttribute('role')==='region'){{
var l=(a.getAttribute('aria-label')||'').replace(/\s+/g,' ').trim().slice(0,30);return t+(cls(a).length?'.'+cls(a).join('.'):'')+(l?' ['+l+']':'');}}
a=a.parentElement;}}return '';}}
var ps=[].slice.call(document.querySelectorAll('p')).slice(0,40).map(function(p){{return parseFloat(getComputedStyle(p).fontSize)||16;}}).sort(function(a,b){{return a-b;}});
var base=ps.length?ps[ps.length>>1]:16,out=[],els=document.body?document.body.getElementsByTagName('*'):[];
for(var i=0;i<els.length&&i<8000&&out.length<{MAX_SAMPLE};i++){{var e=els[i],tag=e.tagName.toLowerCase();
if(/^(script|style|noscript|svg|path|img|video|canvas|iframe|button|input|select|textarea|option)$/.test(tag))continue;
var own='';for(var n=e.firstChild;n;n=n.nextSibling)if(n.nodeType===3)own+=n.nodeValue;own=own.replace(/\s+/g,' ').trim();
var text=own.length>=3?own:'';if(!text&&e.children.length===1&&!e.children[0].children.length)text=(e.textContent||'').replace(/\s+/g,' ').trim();
if(text.length<3||text.length>160)continue;if(!e.getClientRects().length)continue;
var cs=getComputedStyle(e),fs=parseFloat(cs.fontSize)||0,fw=parseInt(cs.fontWeight,10)||400;
var isH=/^h[1-6]$/.test(tag)||e.getAttribute('role')==='heading';
var label=cs.textTransform==='uppercase'&&fw>=500&&text.length<=40;
if(!isH&&!label&&fs<base*1.15&&!(fw>=600&&fs>=base*1.05))continue;
var p=e.parentElement;
out.push({{tag:tag,id:(e.id||'').slice(0,40),cls:cls(e),fs:Math.round(fs),fw:fw,up:cs.textTransform==='uppercase',parent:p?p.tagName.toLowerCase():'',pcls:p?cls(p):[],sec:sec(e),text:text.slice(0,80)}});}}
h.postMessage(JSON.stringify({{sample:out,base:Math.round(base)}}));
}})();"#
    )
}

/// What the headings extractor reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadingsReport {
    Outline(Vec<Heading>),
    /// Index of the heading the reader has scrolled to.
    Current(u32),
    /// A "Detect Headers" sample (validated JSON, re-serialised).
    Sample(String),
}

/// Parses any report from the extractor (see [`parse_headings`]).
pub fn parse_report(body: &str) -> Option<HeadingsReport> {
    if body.starts_with('[') {
        return parse_headings(body).map(HeadingsReport::Outline);
    }
    if body.len() > MAX_SAMPLE_REPORT {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(c) = v.get("c") {
        return u32::try_from(c.as_u64()?).ok().map(HeadingsReport::Current);
    }
    let sample = v.get("sample")?.as_array()?;
    if sample.len() > MAX_SAMPLE {
        return None;
    }
    // Keep only the expected fields, sanitising every string.
    let s = |x: &serde_json::Value, max: usize| sanitize_page_text(x.as_str().unwrap_or(""), max);
    let list = |x: &serde_json::Value| -> Vec<String> {
        x.as_array().map(|a| a.iter().take(4).map(|c| s(c, 40)).collect()).unwrap_or_default()
    };
    let clean: Vec<serde_json::Value> = sample
        .iter()
        .map(|e| {
            serde_json::json!({
                "tag": s(&e["tag"], 16), "id": s(&e["id"], 40), "cls": list(&e["cls"]),
                "fs": e["fs"].as_u64().unwrap_or(0).min(400), "fw": e["fw"].as_u64().unwrap_or(400).min(1000),
                "up": e["up"].as_bool().unwrap_or(false),
                "parent": s(&e["parent"], 16), "pcls": list(&e["pcls"]), "sec": s(&e["sec"], 80),
                "text": s(&e["text"], 80),
            })
        })
        .collect();
    let base = v.get("base").and_then(|b| b.as_u64()).unwrap_or(16).min(200);
    Some(HeadingsReport::Sample(serde_json::json!({ "base_font_px": base, "candidates": clean }).to_string()))
}

/// Injected sources for the modes in `modes` (constant code).
pub fn injections(modes: ViewModes, rules: &HeadingRules) -> Vec<Injection> {
    let mut out = Vec::new();
    if modes.desaturate {
        out.push(Injection {
            source: style_js("__snorkle_grayscale", GRAYSCALE_CSS, true, false),
            at: InjectAt::DocumentStart,
            all_frames: false,
        });
    }
    if modes.hide_media {
        // In every frame: embedded players and ads live in iframes.
        out.push(Injection {
            source: style_js("__snorkle_no_images", NO_MEDIA_CSS, true, true),
            at: InjectAt::DocumentStart,
            all_frames: true,
        });
    }
    if modes.headers {
        out.push(Injection { source: headings_start_js(rules), at: InjectAt::DocumentEnd, all_frames: false });
    }
    out
}

/// Brings an already-loaded page in line with `modes` (adds or removes the
/// styles, starts or stops the headings extractor). Constant code.
/// The headings extractor restarts, so new heading rules take effect.
pub fn apply_js(modes: ViewModes, rules: &HeadingRules) -> String {
    let headers = if modes.headers {
        HEADINGS_STOP_JS.to_string() + &headings_start_js(rules)
    } else {
        HEADINGS_STOP_JS.to_string()
    };
    style_js("__snorkle_grayscale", GRAYSCALE_CSS, modes.desaturate, false)
        + &style_js("__snorkle_no_images", NO_MEDIA_CSS, modes.hide_media, true)
        + &headers
}

/// Scrolls the page to the heading at `index` among the elements matching
/// the extractor's heading selector, then highlights it briefly (an
/// outline and a gray wash that fade out; Web Animations, so the page's own
/// styles are untouched). The only input is an integer.
pub fn scroll_to_heading_js(index: u32) -> String {
    format!(
        "(function(){{var s=window.__snorkleHeadings,sel=s?s.sel:{};var e=document.querySelectorAll(sel)[{index}];\
         if(!e)return;e.scrollIntoView({{block:'start',behavior:'smooth'}});if(!e.animate)return;\
         var k=function(a,o){{return {{outlineStyle:'solid',outlineWidth:'3px',outlineOffset:'4px',\
         outlineColor:'rgba(110,110,110,'+a+')',backgroundColor:'rgba(128,128,128,'+(a*.22)+')',offset:o}};}};\
         e.animate([k(0,0),k(1,.15),k(1,.65),k(0,1)],{{duration:2200,delay:300,easing:'ease-in-out'}});}})();",
        json_str(HEADING_SELECTOR)
    )
}

/// Adds (or removes) a fixed style. While on, the style re-attaches itself
/// if the page removes it (single-page apps that rebuild `<head>`). With
/// `pause_media`, videos that start playing are paused unless the pointer is
/// over them (hidden media should not play), and `MEDIA_SELECTOR` is used
/// for nothing else.
fn style_js(id: &str, css: &str, on: bool, pause_media: bool) -> String {
    let key = json_str(&format!("{id}_state"));
    let id = json_str(id);
    let attr = json_str(REVEAL_ATTR);
    if !on {
        return format!(
            "(function(){{var s=window[{key}];if(!s)return;s.obs.disconnect();s.st.remove();\
             if(s.onPlay)document.removeEventListener('play',s.onPlay,true);\
             if(s.onMove){{document.removeEventListener('pointermove',s.onMove,true);\
             document.removeEventListener('scroll',s.onScroll,true);document.removeEventListener('mouseout',s.onOut,true);\
             if(s.raf)cancelAnimationFrame(s.raf);s.rev.forEach(function(e){{e.removeAttribute({attr});}});}}\
             delete window[{key}];}})();"
        );
    }
    // Pointer tracker: marks every media element under the pointer (also
    // under overlays, which `:hover` misses), at most once per frame. Video
    // boxes are also checked by geometry, since players often put a layer
    // with `pointer-events:none` or their controls over the video.
    let pause = if pause_media {
        format!(
            "var sel={sel};s.rev=[];s.raf=0;s.x=null;\
             s.mark=function(){{s.raf=0;var on=[];if(s.x!==null){{\
             var els=document.elementsFromPoint?document.elementsFromPoint(s.x,s.y):[];\
             for(var i=0;i<els.length;i++){{var e=els[i];if(e.matches&&e.matches(sel))on.push(e);}}\
             document.querySelectorAll('video').forEach(function(v){{if(on.indexOf(v)>=0)return;var r=v.getBoundingClientRect();\
             if(s.x>=r.left&&s.x<=r.right&&s.y>=r.top&&s.y<=r.bottom)on.push(v);}});}}\
             s.rev.forEach(function(e){{if(on.indexOf(e)<0)e.removeAttribute({attr});}});\
             on.forEach(function(e){{if(!e.hasAttribute({attr}))e.setAttribute({attr},'');}});s.rev=on;}};\
             s.queue=function(){{if(!s.raf)s.raf=requestAnimationFrame(s.mark);}};\
             s.onMove=function(e){{if(e.pointerType==='touch')return;s.x=e.clientX;s.y=e.clientY;s.queue();}};\
             s.onScroll=function(){{if(s.x!==null)s.queue();}};\
             s.onOut=function(e){{if(!e.relatedTarget){{s.x=null;s.queue();}}}};\
             document.addEventListener('pointermove',s.onMove,{{passive:true,capture:true}});\
             document.addEventListener('scroll',s.onScroll,{{passive:true,capture:true}});\
             document.addEventListener('mouseout',s.onOut,true);\
             s.shown=function(t){{return t.matches(':hover')||t.hasAttribute({attr});}};\
             s.onPlay=function(e){{var t=e.target;if(t&&t.matches&&t.matches(sel)&&!s.shown(t)&&t.pause)t.pause();}};\
             document.addEventListener('play',s.onPlay,true);\
             document.querySelectorAll('video').forEach(function(v){{if(!v.paused&&!s.shown(v))v.pause();}});",
            sel = json_str(MEDIA_SELECTOR)
        )
    } else {
        String::new()
    };
    // The root element may not exist yet at document start; the observer
    // watches the document, <html> and <head> (each once it exists) and
    // re-attaches the style whenever it is missing.
    format!(
        "(function(){{if(window[{key}])return;var s={{}};window[{key}]=s;\
         var st=document.createElement('style');st.id={id};st.textContent={css};s.st=st;\
         var seen=[];var watch=function(n){{if(n&&seen.indexOf(n)<0){{seen.push(n);s.obs.observe(n,{{childList:true}});}}}};\
         var put=function(){{watch(document.documentElement);watch(document.head);\
         var r=document.head||document.documentElement;if(r&&!st.isConnected)r.appendChild(st);}};\
         s.obs=new MutationObserver(put);watch(document);put();{pause}}})();",
        css = json_str(css)
    )
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
        assert_eq!(injections(t, &HeadingRules::new()).len(), 1);
    }

    #[test]
    fn injections_per_mode() {
        let all = ViewModes { desaturate: true, hide_media: true, headers: true };
        let inj = injections(all, &HeadingRules::new());
        assert_eq!(inj.len(), 3);
        assert!(inj[0].source.contains("grayscale(1)"));
        assert!(inj[1].source.contains("video,"));
        assert!(inj[2].source.contains(&format!("messageHandlers.{HEADINGS_HANDLER_NAME}")));
        assert_eq!(inj[2].at, InjectAt::DocumentEnd);
        assert!(injections(ViewModes { desaturate: false, hide_media: false, headers: false }, &HeadingRules::new())
            .is_empty());
        let off = apply_js(ViewModes { desaturate: false, hide_media: false, headers: false }, &HeadingRules::new());
        assert!(off.contains("window[\"__snorkle_grayscale_state\"];if(!s)return;s.obs.disconnect();s.st.remove()"));
        assert!(inj[1].all_frames, "hide media also covers iframes");
        assert!(!inj[0].all_frames && !inj[2].all_frames);
        assert!(inj[1].source.contains("addEventListener('play'"));
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
    fn heading_rules_are_validated_and_embedded_safely() {
        assert!(validate_rule("h2.story-heading", 2).is_some());
        assert!(validate_rule("div[data-testid=headline] > span", 1).is_some());
        for bad in ["h2{color:red}", "h2;x", "</script>", "h2[a='x']", "@import", "a\\62", "", "h2\nh3"] {
            assert!(validate_rule(bad, 1).is_none(), "{bad}");
        }
        assert!(validate_rule("h2", 0).is_none() && validate_rule("h2", 7).is_none());
        assert!(validate_rule(&"a".repeat(MAX_SELECTOR_CHARS + 1), 1).is_none());
        let mut rules = HeadingRules::new();
        rules.insert("nytimes.com".into(), vec![validate_rule("h2.story-heading", 1).unwrap()]);
        let js = &injections(ViewModes { desaturate: false, hide_media: false, headers: true }, &rules)[0].source;
        assert!(js.contains("story-heading"));
        assert!(!js.contains("</"), "rules are embedded as an escaped JSON literal");
        let apply = apply_js(ViewModes { desaturate: false, hide_media: false, headers: true }, &rules);
        let stop = apply.find(HEADINGS_STOP_JS).expect("stops the running extractor");
        assert!(apply[stop..].contains("story-heading"), "then restarts it with the new rules");
    }

    #[test]
    fn reports() {
        assert_eq!(parse_report("{\"c\":4}"), Some(HeadingsReport::Current(4)));
        assert!(matches!(parse_report("[[2,\"A\",0]]"), Some(HeadingsReport::Outline(v)) if v.len() == 1));
        let sample = r#"{"sample":[{"tag":"div","id":"x","cls":["headline","a","b","c","d"],"fs":28,"fw":700,"parent":"article","pcls":[],"text":"Big \u202e news","extra":"dropped"}],"base":17}"#;
        let Some(HeadingsReport::Sample(clean)) = parse_report(sample) else { panic!("sample") };
        let v: serde_json::Value = serde_json::from_str(&clean).unwrap();
        assert_eq!(v["base_font_px"], 17);
        assert_eq!(v["candidates"][0]["cls"].as_array().unwrap().len(), 4);
        assert_eq!(v["candidates"][0]["text"], "Big news");
        assert!(v["candidates"][0].get("extra").is_none());
        assert!(parse_report("{\"c\":-1}").is_none());
        assert!(parse_report("{\"nope\":1}").is_none());
    }

    #[test]
    fn scroll_takes_only_an_index() {
        let js = scroll_to_heading_js(12);
        assert!(js.contains("[12]"));
        assert!(js.contains("scrollIntoView"));
        assert!(js.contains("e.animate("), "the heading is highlighted after scrolling");
    }

    #[test]
    fn hidden_media_fades_in_and_is_revealed_under_overlays() {
        // Same filter functions in the same order, so the change animates.
        let hidden = "filter:grayscale(1) contrast(0) brightness(1.57)";
        let shown = "filter:grayscale(1) contrast(1) brightness(1)";
        assert!(NO_MEDIA_CSS.contains(hidden) && NO_MEDIA_CSS.contains(shown));
        assert!(NO_MEDIA_CSS.contains("transition:filter"));
        assert!(NO_MEDIA_CSS.contains(&format!("[{REVEAL_ATTR}]")));
        let on = style_js("__snorkle_no_images", NO_MEDIA_CSS, true, true);
        assert!(on.contains("elementsFromPoint") && on.contains("addEventListener('pointermove'"));
        assert!(on.contains("hasAttribute(\"data-snorkle-reveal\")"), "revealed video may play");
        let off = style_js("__snorkle_no_images", NO_MEDIA_CSS, false, true);
        assert!(off.contains("removeEventListener('pointermove'") && off.contains("removeAttribute("));
        assert!(!style_js("__snorkle_grayscale", GRAYSCALE_CSS, true, false).contains("pointermove"));
    }

    #[test]
    fn sample_reports_sections() {
        let sample = r#"{"sample":[{"tag":"span","cls":[],"fs":13,"fw":700,"up":true,"parent":"div","pcls":[],"sec":"section.opinion [Opinion]","text":"Opinion"}],"base":16}"#;
        let Some(HeadingsReport::Sample(clean)) = parse_report(sample) else { panic!("sample") };
        let v: serde_json::Value = serde_json::from_str(&clean).unwrap();
        assert_eq!(v["candidates"][0]["sec"], "section.opinion [Opinion]");
        assert_eq!(v["candidates"][0]["up"], true);
        assert!(headings_sample_js().contains("function sec(e)"));
    }
}
