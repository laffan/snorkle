//! SECURITY-REVIEW: the Notebook (owner request 2026-11-15): one notebook,
//! not tied to a page or a tab, in the right panel — the notepad of
//! laffan/zotero-helper (ink, typed notes, quotes and clips), with
//! captures from any page.
//!
//! From the page (channel C23, posting to C19's client-world handler, so
//! page scripts cannot call it): a constant watcher ([`watch_js`]) in the
//! client world, main frame, every page, reports
//! - ⌥-click on a selection, or on text (it selects that sentence):
//!   `{"nb":"text","r":[x,y,w,h],"vw":w}`; the text is then read as the
//!   selection menu reads it (C20);
//! - ⌥-click on an image, video or canvas: `{"nb":"img", ...}` with its
//!   box (cut to what is in view);
//! - ⌥-drag, or a drag while the notebook's scissors are on (clip mode):
//!   `{"nb":"clip", ...}` with the box drawn;
//! - Escape in clip mode: `{"nb":"end"}`; a press or a scroll after a
//!   pick: `{"nb":"hide"}` (its "Send to Notebook" goes);
//! - a drag beginning in the page (2026-11-16): `{"nb":"dtext", ...}` with
//!   where the selection is, or `{"nb":"dimg", ...}` with the image's box;
//!   and `{"nb":"dend"}` when it ends. Dropped on the notebook, the drag
//!   becomes a quote (the selection read with C20) or a clip of that box:
//!   the drag's own data (the page's HTML, image bytes) is never used.
//!
//! Reports carry geometry only, parsed strictly ([`parse_report`]). An
//! image or a region becomes a picture the app draws itself from its own
//! web view (`takeSnapshotWithConfiguration`, as Save for Later's
//! thumbnails), never the page's image bytes, so the chrome decodes only
//! app-made JPEGs. Dragging the notebook's quote button onto a page shows
//! where it would land (constant code taking two numbers, [`hover_js`])
//! and reads the sentence there when let go ([`drop_js`], read back like
//! C20: [`parse_drop`]).
//!
//! Storage (`notebook.json`): what the chrome sends is checked and
//! rebuilt field by field ([`clean`]): ids, numbers, colours, http(s)
//! URLs, page text cleaned and capped, images only as
//! `data:image/{jpeg,png};base64` within bounds. Captures from private
//! tabs are refused (natively and in the chrome), and notes written while
//! a private tab is shown keep no address.

use serde_json::{Map, Value};
use url::Url;

use crate::selection::{SelectionRect, HANDLER_NAME};
use crate::text::sanitize_page_text;

/// Largest report accepted, in bytes.
pub const MAX_REPORT: usize = 256;
/// Largest notebook accepted (it holds its clips' images).
pub const MAX_NOTEBOOK_BYTES: usize = 64 * 1024 * 1024;
/// Largest clip image (a data URL), and the most kept.
pub const MAX_IMAGE_CHARS: usize = 4 * 1024 * 1024;
pub const MAX_STROKES: usize = 200_000;
pub const MAX_POINTS: usize = 40_000;
pub const MAX_NOTES: usize = 50_000;
pub const MAX_LINKS: usize = 20_000;
/// A typed note's words; a quote's text; a title.
pub const MAX_TEXT_CHARS: usize = 20_000;
pub const MAX_QUOTE_CHARS: usize = 4_000;
pub const MAX_TITLE_CHARS: usize = 300;
/// Largest sentence read back by [`drop_js`].
pub const MAX_DROP_BYTES: usize = 16 * 1024;

/// What the watcher reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Report {
    /// ⌥-click on text: the selection (made or found) is here.
    Text(SelectionRect),
    /// ⌥-click on an image: its box in view.
    Image(SelectionRect),
    /// ⌥-drag, or a drag in clip mode: the box drawn.
    Clip(SelectionRect),
    /// Clip mode was left from the page (Escape).
    End,
    /// The page was pressed or scrolled after a pick: "Send to Notebook"
    /// goes.
    Hide,
    /// A drag began in the page: selected text (where the selection is),
    /// or an image (its box in view). What is dropped in the notebook is
    /// taken from the page by these, not from the drag's data.
    DragText(SelectionRect),
    DragImage(SelectionRect),
    /// A drag that began in the page ended (dropped anywhere, or not).
    DragEnd,
}

/// Watches for ⌥-click, ⌥-drag and clip mode (see the module docs).
/// Constant code, main frame, document end. `window.__snorkleNb` (client
/// world only) also holds the functions [`clip_mode_js`], [`hover_js`],
/// [`drop_js`] and [`LEAVE_JS`] call.
pub fn watch_js() -> String {
    format!(
        r#"(function(){{var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.{HANDLER_NAME};
if(!h||window.__snorkleNb)return;var nb=window.__snorkleNb={{clip:false}};
var st=document.createElement('style');st.textContent="html[data-snorkle-nb-clip],html[data-snorkle-nb-clip] *{{cursor:crosshair!important;-webkit-user-select:none!important;user-select:none!important;touch-action:none!important}}::highlight(snorkle-nb){{background-color:rgba(255,95,87,.3)}}";
(document.head||document.documentElement).appendChild(st);
function vp(){{var v=window.visualViewport;return [v?v.offsetLeft:0,v?v.offsetTop:0,Math.round(v?v.width:innerWidth),Math.round(v?v.height:innerHeight)];}}
var picked=0;function unpick(){{if(picked){{picked=0;h.postMessage('{{"nb":"hide"}}');}}}}
function post(kind,r){{if(kind==='text'||kind==='img')picked=1;var o=vp(),x0=Math.max(0,r.left-o[0]),y0=Math.max(0,r.top-o[1]),x1=Math.min(o[2],r.right-o[0]),y1=Math.min(o[3],r.bottom-o[1]);
if(x1-x0<1||y1-y0<1)return;h.postMessage(JSON.stringify({{nb:kind,r:[Math.round(x0),Math.round(y0),Math.round(x1-x0),Math.round(y1-y0)],vw:o[2]}}));}}
function el(css){{var e=document.createElement('div');for(var k in css)e.style.setProperty(k,css[k],'important');document.documentElement.appendChild(e);return e;}}
var MEDIA=/^(IMG|VIDEO|CANVAS|PICTURE)$/;
function mediaAt(x,y){{var l=document.elementsFromPoint?document.elementsFromPoint(x,y):[];for(var i=0;i<l.length&&i<12;i++){{var e=l[i];if(MEDIA.test(e.tagName)||e.hasAttribute('data-snorkle-video-box'))return e;if(e.tagName==='svg'&&e.getBoundingClientRect().width>40)return e;}}return null;}}
function blockOf(n){{var e=n.nodeType===1?n:n.parentElement;while(e&&e!==document.body&&e!==document.documentElement){{var d=getComputedStyle(e).display;if(d!=='inline'&&d!=='contents'&&d.indexOf('inline-')!==0)return e;e=e.parentElement;}}return document.body||document.documentElement;}}
function sentenceAt(x,y){{var c=document.caretRangeFromPoint?document.caretRangeFromPoint(x,y):null;if(!c||!c.startContainer||c.startContainer.nodeType!==3)return null;
var node=c.startContainer,off=c.startOffset,hit=document.elementFromPoint(x,y);if(!hit||!hit.contains(node))return null;
var b=blockOf(node),tw=document.createTreeWalker(b,NodeFilter.SHOW_TEXT),nodes=[],pos=0,at=-1,t;
while((t=tw.nextNode())&&nodes.length<5000&&pos<40000){{if(t===node)at=pos+off;nodes.push([t,pos]);pos+=t.nodeValue.length;}}
if(at<0)return null;var text=nodes.map(function(n){{return n[0].nodeValue;}}).join(''),s=0,e=text.length;
if(window.Intl&&Intl.Segmenter){{var it=new Intl.Segmenter(undefined,{{granularity:'sentence'}}).segment(text);var g=it.containing(Math.min(at,text.length-1));if(g){{s=g.index;e=g.index+g.segment.length;}}}}
else{{var re=/[.!?。！？]+["'”’)\]]*\s+/g,m;while((m=re.exec(text))){{if(m.index+m[0].length<=at)s=m.index+m[0].length;else{{e=m.index+m[0].length;break;}}}}}}
while(e>s&&/\s/.test(text[e-1]))e--;while(s<e&&/\s/.test(text[s]))s++;if(e<=s)return null;
function loc(p,end){{for(var i=0;i<nodes.length;i++){{var n=nodes[i],len=n[0].nodeValue.length;if(p<n[1]+len||(end&&p===n[1]+len))return [n[0],p-n[1]];}}var z=nodes[nodes.length-1];return [z[0],z[0].nodeValue.length];}}
var r=document.createRange(),a=loc(s,false),z=loc(e,true);r.setStart(a[0],a[1]);r.setEnd(z[0],z[1]);return r;}}
function inSelection(x,y){{var s=window.getSelection();if(!s||!s.rangeCount||s.isCollapsed||!String(s).trim())return null;var r=s.getRangeAt(0),l=r.getClientRects();
for(var i=0;i<l.length;i++){{var q=l[i];if(x>=q.left-4&&x<=q.right+4&&y>=q.top-4&&y<=q.bottom+4)return r;}}return null;}}
function optionClick(x,y){{var r=inSelection(x,y);if(r){{post('text',r.getBoundingClientRect());return true;}}
var m=mediaAt(x,y);if(m){{post('img',m.getBoundingClientRect());return true;}}
r=sentenceAt(x,y);if(!r)return false;var s=window.getSelection();s.removeAllRanges();s.addRange(r);post('text',r.getBoundingClientRect());return true;}}
var box=null,drag=null,swallow=0;
function band(x0,y0,x1,y1){{if(!box)box=el({{position:'fixed','z-index':'2147483647','pointer-events':'none',border:'2px dashed #ff3b30','box-shadow':'0 0 0 100vmax rgba(0,0,0,.14)',margin:'0',padding:'0',background:'transparent','box-sizing':'border-box'}});
box.style.setProperty('left',Math.min(x0,x1)+'px','important');box.style.setProperty('top',Math.min(y0,y1)+'px','important');
box.style.setProperty('width',Math.abs(x1-x0)+'px','important');box.style.setProperty('height',Math.abs(y1-y0)+'px','important');}}
function unband(){{if(box){{box.remove();box=null;}}}}
nb.setClip=function(on){{nb.clip=!!on;if(on)document.documentElement.setAttribute('data-snorkle-nb-clip','');else document.documentElement.removeAttribute('data-snorkle-nb-clip');if(!on){{unband();drag=null;}}}};
window.addEventListener('scroll',unpick,{{passive:true,capture:true}});
document.addEventListener('pointerdown',function(e){{if(!(e.altKey&&!e.metaKey&&!e.ctrlKey)&&!nb.clip){{unpick();return;}}if(e.pointerType==='mouse'&&e.button!==0)return;
e.preventDefault();e.stopPropagation();drag={{id:e.pointerId,x:e.clientX,y:e.clientY,moved:false,alt:e.altKey&&!nb.clip}};}},true);
document.addEventListener('pointermove',function(e){{if(!drag||e.pointerId!==drag.id)return;e.preventDefault();
if(!drag.moved&&Math.abs(e.clientX-drag.x)+Math.abs(e.clientY-drag.y)<6)return;drag.moved=true;band(drag.x,drag.y,e.clientX,e.clientY);}},true);
function up(e){{if(!drag||e.pointerId!==drag.id)return;var d=drag;drag=null;e.preventDefault();e.stopPropagation();swallow=Date.now();
if(e.type!=='pointerup'){{unband();return;}}
if(d.moved){{var x0=Math.min(d.x,e.clientX),y0=Math.min(d.y,e.clientY),wd=Math.abs(e.clientX-d.x),ht=Math.abs(e.clientY-d.y);unband();
if(wd>=12&&ht>=12){{if(nb.clip)nb.setClip(false);post('clip',{{left:x0,top:y0,right:x0+wd,bottom:y0+ht}});}}return;}}
unband();if(d.alt)optionClick(e.clientX,e.clientY);}}
document.addEventListener('pointerup',up,true);document.addEventListener('pointercancel',up,true);
['click','mousedown','mouseup','dblclick','auxclick'].forEach(function(t){{document.addEventListener(t,function(e){{var own=swallow&&Date.now()-swallow<400;
if((e.altKey&&!e.metaKey&&!e.ctrlKey)||nb.clip||own){{e.preventDefault();e.stopPropagation();if(t==='click')swallow=0;}}}},true);}});
document.addEventListener('selectstart',function(e){{if(drag||nb.clip)e.preventDefault();}},true);
document.addEventListener('keydown',function(e){{if(nb.clip&&e.key==='Escape'){{e.preventDefault();e.stopPropagation();nb.setClip(false);h.postMessage('{{"nb":"end"}}');}}}},true);
var dot=null,line=null,hl=null;
function clearHover(){{if(dot){{dot.remove();dot=null;}}if(line){{line.remove();line=null;}}if(window.CSS&&CSS.highlights)CSS.highlights.delete('snorkle-nb');hl=null;}}
function point(fx,fy){{var o=vp();return [o[0]+fx*o[2],o[1]+fy*o[3]];}}
nb.hover=function(fx,fy,fy0){{var p=point(fx,fy),o=vp();if(!dot)dot=el({{position:'fixed','z-index':'2147483647','pointer-events':'none',width:'10px',height:'10px','border-radius':'5px',background:'#ff5f57',margin:'0',padding:'0'}});
dot.style.setProperty('left',(p[0]-o[0]-5)+'px','important');dot.style.setProperty('top',(p[1]-o[1]-5)+'px','important');
if(!line)line=el({{position:'fixed','z-index':'2147483646','pointer-events':'none',height:'0',margin:'0',padding:'0','border-top':'2px dashed #ff5f57','transform-origin':'0 0'}});
var x0=o[2],y0=fy0*o[3],dx=p[0]-o[0]-x0,dy=p[1]-o[1]-y0;line.style.setProperty('left',x0+'px','important');line.style.setProperty('top',y0+'px','important');
line.style.setProperty('width',Math.hypot(dx,dy)+'px','important');line.style.setProperty('transform','rotate('+Math.atan2(dy,dx)+'rad)','important');
var r=sentenceAt(p[0]-o[0],p[1]-o[1]);if(window.CSS&&CSS.highlights&&window.Highlight){{if(r)CSS.highlights.set('snorkle-nb',new Highlight(r));else CSS.highlights.delete('snorkle-nb');}}hl=r;}};
nb.drop=function(fx,fy){{var p=point(fx,fy),o=vp(),r=sentenceAt(p[0]-o[0],p[1]-o[1]);clearHover();return JSON.stringify({{t:r?String(r).slice(0,2000):''}});}};
nb.leave=clearHover;
document.addEventListener('dragstart',function(e){{var t=e.target&&e.target.nodeType===1?e.target:(e.target&&e.target.parentElement),s=window.getSelection(),
txt=s&&s.rangeCount&&!s.isCollapsed&&String(s).trim(),img=t&&t.closest?t.closest('img,picture,video,canvas'):null;
if(img&&!(txt&&s.containsNode(img,true)))post('dimg',img.getBoundingClientRect());else if(txt)post('dtext',s.getRangeAt(0).getBoundingClientRect());}},true);
document.addEventListener('dragend',function(){{h.postMessage('{{"nb":"dend"}}');}},true);
window.addEventListener('pagehide',function(){{nb.setClip(false);clearHover();}});}})();"#
    )
}

/// Turns clip mode on or off in a page (the notebook's scissors).
pub fn clip_mode_js(on: bool) -> String {
    format!("window.__snorkleNb&&window.__snorkleNb.setClip({on});")
}

/// A fraction of the page's view, finite and within 0–1.
fn frac(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The quote button dragged over a page: where it would land (fractions
/// of the view) and the height it left the page's right edge at.
pub fn hover_js(fx: f64, fy: f64, from_y: f64) -> String {
    format!("window.__snorkleNb&&window.__snorkleNb.hover({:.5},{:.5},{:.5});", frac(fx), frac(fy), frac(from_y))
}

/// The quote button let go over a page: the sentence there, as
/// `{"t": text}` (read back, [`parse_drop`]).
pub fn drop_js(fx: f64, fy: f64) -> String {
    format!("(function(){{var n=window.__snorkleNb;return n?n.drop({:.5},{:.5}):'';}})()", frac(fx), frac(fy))
}

/// The quote button left the page (or the drag ended elsewhere).
pub const LEAVE_JS: &str = "window.__snorkleNb&&window.__snorkleNb.leave();";

/// The sentence [`drop_js`] read: cleaned and capped; `None` if there is
/// none or the answer is malformed.
pub fn parse_drop(text: &str) -> Option<String> {
    if text.len() > MAX_DROP_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let t = crate::selection::clean_text(v.get("t")?.as_str()?)?;
    Some(t)
}

/// Whether a body posted to the selection handler is a notebook report.
pub fn is_report(body: &str) -> bool {
    body.starts_with("{\"nb\":")
}

/// Parses a [`watch_js`] report. Fails closed.
pub fn parse_report(body: &str) -> Option<Report> {
    if body.len() > MAX_REPORT || !is_report(body) {
        return None;
    }
    if body == r#"{"nb":"end"}"# {
        return Some(Report::End);
    }
    if body == r#"{"nb":"hide"}"# {
        return Some(Report::Hide);
    }
    if body == r#"{"nb":"dend"}"# {
        return Some(Report::DragEnd);
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let kind = v.get("nb")?.as_str()?;
    let r = v.get("r")?.as_array()?;
    if r.len() != 4 {
        return None;
    }
    let n: Vec<f64> = r.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>()?;
    let vw = v.get("vw")?.as_f64()?;
    let bounded = |x: f64| x.is_finite() && x.abs() <= 100_000.0;
    if !n.iter().all(|x| bounded(*x)) || n[2] < 1.0 || n[3] < 1.0 || !bounded(vw) || vw < 1.0 {
        return None;
    }
    let rect = SelectionRect { x: n[0], y: n[1], w: n[2], h: n[3], vw };
    match kind {
        "text" => Some(Report::Text(rect)),
        "img" => Some(Report::Image(rect)),
        "dtext" => Some(Report::DragText(rect)),
        "dimg" => Some(Report::DragImage(rect)),
        "clip" => Some(Report::Clip(rect)),
        _ => None,
    }
}

// ---- storage -------------------------------------------------------------

/// An id the chrome made: letters, digits and `-`, at most 64.
fn id(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| s.to_string())
}

fn num(v: Option<&Value>, max: f64) -> Option<f64> {
    v?.as_f64().filter(|n| n.is_finite() && n.abs() <= max)
}

/// The theme's ink (""), or `#rrggbb`.
fn color(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let ok = s.is_empty() || (s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit()));
    ok.then(|| s.to_ascii_lowercase())
}

/// An http(s) address without credentials, at most 4,096 characters.
pub fn web_url(raw: &str) -> Option<String> {
    if raw.len() > 4096 {
        return None;
    }
    let u = Url::parse(raw).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none() && u.host().is_some())
        .then(|| u.to_string())
}

/// A clip's picture: `data:image/jpeg` or `data:image/png`, base64.
fn image(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let payload = s.strip_prefix("data:image/jpeg;base64,").or_else(|| s.strip_prefix("data:image/png;base64,"))?;
    (s.len() <= MAX_IMAGE_CHARS
        && !payload.is_empty()
        && payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')))
    .then(|| s.to_string())
}

/// Typed text: control characters other than line breaks and tabs go.
fn typed(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).take(max).collect()
}

/// Where something came from: an address and a title, either may be
/// missing (a private tab's, or the start page).
fn source(o: &Map<String, Value>, out: &mut Map<String, Value>) {
    if let Some(u) = o.get("url").and_then(Value::as_str).and_then(web_url) {
        out.insert("url".into(), Value::String(u));
    }
    if let Some(t) = o.get("title").and_then(Value::as_str) {
        let t = sanitize_page_text(t, MAX_TITLE_CHARS);
        if !t.is_empty() {
            out.insert("title".into(), Value::String(t));
        }
    }
}

fn stroke(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let pts = o.get("points")?.as_array()?;
    if pts.len() < 2 || pts.len() > MAX_POINTS || pts.len() % 2 != 0 {
        return None;
    }
    let points: Vec<Value> = pts.iter().map(|p| num(Some(p), 1e7).map(Value::from)).collect::<Option<Vec<Value>>>()?;
    let width = num(o.get("width"), 50.0).filter(|w| *w > 0.0)?;
    Some(serde_json::json!({
        "id": id(o.get("id"))?,
        "points": points,
        "width": width,
        "color": color(o.get("color"))?,
        "note": id(o.get("note"))?,
    }))
}

fn note(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id(o.get("id"))?));
    out.insert("createdMs".into(), Value::from(o.get("createdMs").and_then(Value::as_u64).unwrap_or(0)));
    source(o, &mut out);
    Some(Value::Object(out))
}

fn link(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id(o.get("id"))?));
    out.insert("x".into(), Value::from(num(o.get("x"), 1e7)?));
    out.insert("y".into(), Value::from(num(o.get("y"), 1e7)?));
    for k in ["w", "h"] {
        if let Some(n) = num(o.get(k), 10_000.0).filter(|n| *n > 0.0) {
            out.insert(k.into(), Value::from(n));
        }
    }
    if o.get("fixed").and_then(Value::as_bool) == Some(true) {
        out.insert("fixed".into(), Value::Bool(true));
    }
    if let Some(t) = o.get("text").and_then(Value::as_str) {
        out.insert("text".into(), Value::String(typed(t, MAX_TEXT_CHARS)));
    }
    if let Some(img) = image(o.get("image")) {
        out.insert("image".into(), Value::String(img));
        if let Some(dark) = image(o.get("imageDark")) {
            out.insert("imageDark".into(), Value::String(dark));
        }
    }
    let t = o.get("target")?.as_object()?;
    let kind = match t.get("kind")?.as_str()? {
        k @ ("page" | "quote" | "clip") => k,
        _ => return None,
    };
    let mut target = Map::new();
    target.insert("kind".into(), Value::String(kind.into()));
    source(t, &mut target);
    if let Some(q) = t.get("text").and_then(Value::as_str) {
        let q = crate::ai::clean_multiline(q, MAX_QUOTE_CHARS);
        if !q.is_empty() {
            target.insert("text".into(), Value::String(q));
        }
    }
    // A clip has its picture; a quote its words.
    if (kind == "clip" && !out.contains_key("image"))
        || (kind == "quote" && !target.contains_key("text") && !out.contains_key("text"))
    {
        return None;
    }
    out.insert("target".into(), Value::Object(target));
    Some(Value::Object(out))
}

/// The notebook as the chrome sends it, checked and rebuilt (see the
/// module docs): what is malformed is left out; `None` when the whole is
/// not a notebook or is too large.
pub fn clean(json: &str) -> Option<String> {
    if json.len() > MAX_NOTEBOOK_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(json).ok()?;
    let o = v.as_object()?;
    let list = |k: &str, max: usize, f: fn(&Value) -> Option<Value>| -> Vec<Value> {
        o.get(k).and_then(Value::as_array).map(|a| a.iter().take(max).filter_map(f).collect()).unwrap_or_default()
    };
    let strokes = list("strokes", MAX_STROKES, stroke);
    let notes = list("notes", MAX_NOTES, note);
    let links = list("links", MAX_LINKS, link);
    let pan = o.get("pan").and_then(Value::as_object);
    let pan = serde_json::json!({
        "x": pan.and_then(|p| num(p.get("x"), 1e7)).unwrap_or(0.0),
        "y": pan.and_then(|p| num(p.get("y"), 1e7)).unwrap_or(0.0),
    });
    let out = serde_json::json!({ "version": 1, "strokes": strokes, "notes": notes, "links": links, "pan": pan });
    let text = serde_json::to_string(&out).ok()?;
    (text.len() <= MAX_NOTEBOOK_BYTES).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_strict() {
        let r = SelectionRect { x: 10.0, y: 20.0, w: 300.0, h: 18.0, vw: 1024.0 };
        assert_eq!(parse_report(r#"{"nb":"text","r":[10,20,300,18],"vw":1024}"#), Some(Report::Text(r)));
        assert_eq!(parse_report(r#"{"nb":"img","r":[10,20,300,18],"vw":1024}"#), Some(Report::Image(r)));
        assert_eq!(parse_report(r#"{"nb":"clip","r":[10,20,300,18],"vw":1024}"#), Some(Report::Clip(r)));
        assert_eq!(parse_report(r#"{"nb":"end"}"#), Some(Report::End));
        assert_eq!(parse_report(r#"{"nb":"hide"}"#), Some(Report::Hide));
        assert_eq!(parse_report(r#"{"nb":"dend"}"#), Some(Report::DragEnd));
        assert_eq!(parse_report(r#"{"nb":"dtext","r":[10,20,300,18],"vw":1024}"#), Some(Report::DragText(r)));
        assert_eq!(parse_report(r#"{"nb":"dimg","r":[10,20,300,18],"vw":1024}"#), Some(Report::DragImage(r)));
        for bad in [
            r#"{"nb":"other","r":[1,2,3,4],"vw":9}"#,
            r#"{"nb":"clip","r":[1,2,0,4],"vw":9}"#,
            r#"{"nb":"clip","r":[1,2,3],"vw":9}"#,
            r#"{"nb":"clip","r":[1e9,2,3,4],"vw":9}"#,
            r#"{"nb":"clip","r":[1,2,3,4],"vw":0}"#,
            r#"{"nb":"clip","r":["1",2,3,4],"vw":9}"#,
            r#"{"img":"x"}"#,
        ] {
            assert!(parse_report(bad).is_none(), "{bad}");
        }
        assert!(parse_report(&format!(r#"{{"nb":"clip","r":[1,2,3,4],"vw":9,"x":"{}"}}"#, "a".repeat(300))).is_none());
        assert!(is_report(r#"{"nb":"end"}"#) && !is_report(r#"{"img":null}"#) && !is_report(r#"{"edit":1}"#));
    }

    #[test]
    fn page_code_is_constant() {
        let w = watch_js();
        assert!(w.contains(&format!("messageHandlers.{HANDLER_NAME}")) && w.contains("window.__snorkleNb"));
        assert_eq!(clip_mode_js(true), "window.__snorkleNb&&window.__snorkleNb.setClip(true);");
        assert_eq!(
            hover_js(0.5, f64::NAN, 2.0),
            "window.__snorkleNb&&window.__snorkleNb.hover(0.50000,0.00000,1.00000);"
        );
        assert!(drop_js(-3.0, 0.25).contains("n.drop(0.00000,0.25000)"));
        assert_eq!(parse_drop("{\"t\":\"  A sentence.\\u202e \"}").as_deref(), Some("A sentence."));
        assert_eq!(parse_drop(r#"{"t":""}"#), None);
        assert_eq!(parse_drop("[]"), None);
    }

    #[test]
    fn notebook_is_rebuilt() {
        let jpeg = "data:image/jpeg;base64,/9j/4AAQ";
        let input = serde_json::json!({
            "strokes": [
                { "id": "s-1", "points": [1, 2, 3.5, 4], "width": 2, "color": "", "note": "note-1" },
                { "id": "s-2", "points": [1, 2, 3], "width": 2, "color": "", "note": "note-1" },
                { "id": "s 3", "points": [1, 2], "width": 2, "color": "", "note": "note-1" },
                { "id": "s-4", "points": [1, 2], "width": 2, "color": "red;x", "note": "note-1" },
                { "id": "s-5", "points": [1, 2], "width": 2, "color": "#FF5F57", "note": "note-1", "evil": 1 },
            ],
            "notes": [
                { "id": "note-1", "createdMs": 5, "url": "javascript:alert(1)", "title": "T\u{202e}itle" },
                { "id": "note-2", "createdMs": 6, "url": "https://user:pw@example.com/", "title": "" },
                { "id": "note-3", "createdMs": 7, "url": "https://example.com/a#:~:text=x" },
            ],
            "links": [
                { "id": "l-1", "x": 1, "y": 2, "target": { "kind": "quote", "url": "https://e.com/", "text": "Quoted", "title": "E" } },
                { "id": "l-2", "x": 1, "y": 2, "image": jpeg, "imageDark": "data:image/svg+xml;base64,PHN2Zz4=", "target": { "kind": "clip", "url": "https://e.com/" } },
                { "id": "l-3", "x": 1, "y": 2, "image": "data:text/html;base64,PGI+", "target": { "kind": "clip" } },
                { "id": "l-4", "x": 1, "y": 2, "text": "# Note\u{0}\nline", "fixed": true, "target": { "kind": "page" } },
                { "id": "l-5", "x": 1, "y": 2, "target": { "kind": "script" } },
                { "id": "l-6", "x": f64::MAX, "y": 2, "target": { "kind": "page" } },
                { "id": "l-7", "x": 1, "y": 2, "target": { "kind": "quote", "text": "  " } },
            ],
            "pan": { "x": 10, "y": "no" },
            "extra": "dropped",
        });
        let out: Value = serde_json::from_str(&clean(&input.to_string()).unwrap()).unwrap();
        let strokes = out["strokes"].as_array().unwrap();
        assert_eq!(strokes.len(), 2, "odd points, bad ids and colours are left out");
        assert_eq!(strokes[1]["color"], "#ff5f57");
        assert!(strokes[1].get("evil").is_none());
        let notes = out["notes"].as_array().unwrap();
        assert_eq!(notes[0]["title"], "Title");
        assert!(notes[0].get("url").is_none() && notes[1].get("url").is_none());
        assert_eq!(notes[2]["url"], "https://example.com/a#:~:text=x");
        let links = out["links"].as_array().unwrap();
        let ids: Vec<&str> = links.iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["l-1", "l-2", "l-4"]);
        assert_eq!(links[1]["image"], jpeg);
        assert!(links[1].get("imageDark").is_none(), "only jpeg and png");
        assert_eq!(links[2]["text"], "# Note\nline");
        assert_eq!(links[2]["fixed"], true);
        assert_eq!(out["pan"], serde_json::json!({ "x": 10.0, "y": 0.0 }));
        assert!(out.get("extra").is_none());
        assert!(clean("[]").is_none() && clean("nope").is_none());
        assert_eq!(
            clean("{}").as_deref(),
            Some(r#"{"links":[],"notes":[],"pan":{"x":0.0,"y":0.0},"strokes":[],"version":1}"#)
        );
    }
}
