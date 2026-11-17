//! SECURITY-REVIEW: the summary view (owner request 2026-11-16; replaces
//! the headers view and its channel C14). A per-tab panel docked right of
//! the page shows an AI summary of the page as bullet points; clicking a
//! point scrolls the page to the text it was drawn from.
//!
//! Channel C24: when the user asks for a summary, the host runs the
//! constant [`blocks_js`] in the page's client world (main frame) and
//! reads back its string result (as the page chat reads the page, C17):
//! the page's title and its visible text as numbered blocks (paragraphs,
//! list items, headings, cells), in document order. The page keeps the
//! blocks' elements in the client world (`window.__snorkleSum`), where
//! page scripts cannot reach them. [`parse_blocks`] checks the result
//! strictly; the blocks go to the user's AI provider with the tab's
//! committed URL (from the core), and the reply is parsed by
//! `ai::parse_summary` (bounded text, block numbers checked against the
//! blocks sent). Clicking a point runs [`scroll_js`]: constant code whose
//! only input is up to [`MAX_POINT_BLOCKS`] integers.
//!
//! Nothing is installed in pages and nothing posts back to the app: the
//! page is read only when the user asks, and summaries live in the
//! chrome's memory (one per tab, dropped when the tab's page changes).

use serde::Serialize;
use serde_json::Value;

use crate::text::sanitize_page_text;

/// Blocks read from a page, characters per block, and characters in all.
pub const MAX_BLOCKS: usize = 800;
pub const MAX_BLOCK_CHARS: usize = 1200;
pub const MAX_TEXT_CHARS: usize = 40_000;
/// Largest result accepted from [`blocks_js`], in bytes (UTF-8 and JSON
/// escapes of the text above, plus the array).
pub const MAX_BLOCKS_REPORT: usize = 4 * MAX_TEXT_CHARS + 16 * MAX_BLOCKS + 4096;
/// Points in a summary, characters per point, blocks a point refers to.
pub const MAX_POINTS: usize = 12;
pub const MAX_POINT_CHARS: usize = 400;
pub const MAX_POINT_BLOCKS: usize = 4;

/// Collects the page's visible text as blocks: each text node goes to its
/// nearest ancestor that is not laid out inline (a paragraph, list item,
/// heading, cell…), in document order. Navigation, controls, scripts,
/// SVG and embedded media are skipped, as is text in hidden elements.
/// Bounded: [`MAX_BLOCKS`] blocks, [`MAX_BLOCK_CHARS`] each,
/// [`MAX_TEXT_CHARS`] in all, at most 200,000 nodes looked at.
const COLLECT_JS: &str = r#"function collect(){var MAXB=__MAXB__,MAXC=__MAXC__,MAXT=__MAXT__;
var SKIP=/^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|SVG|MATH|NAV|BUTTON|SELECT|OPTION|TEXTAREA|INPUT|IFRAME|OBJECT|EMBED|CANVAS|VIDEO|AUDIO|DIALOG)$/i;
var root=document.body||document.documentElement,ds=new Map(),vs=new Map();
function dsp(e){var d=ds.get(e);if(d===undefined){d=getComputedStyle(e).display;ds.set(e,d);}return d;}
function shown(e){var v=vs.get(e);if(v===undefined){v=e.checkVisibility?e.checkVisibility({visibilityProperty:true}):e.getClientRects().length>0;vs.set(e,v);}return v;}
function blockOf(n){var e=n.parentElement;while(e&&e!==root){var d=dsp(e);if(d!=='inline'&&d!=='contents'&&d.indexOf('inline')!==0)return e;e=e.parentElement;}return root;}
var tw=document.createTreeWalker(root,NodeFilter.SHOW_ELEMENT|NodeFilter.SHOW_TEXT,{acceptNode:function(n){
if(n.nodeType===1)return SKIP.test(n.tagName)||n.getAttribute('role')==='navigation'?NodeFilter.FILTER_REJECT:NodeFilter.FILTER_SKIP;
return /\S/.test(n.nodeValue)?NodeFilter.FILTER_ACCEPT:NodeFilter.FILTER_SKIP;}});
var els=[],tx=[],idx=new Map(),total=0,seen=0,t;
while((t=tw.nextNode())&&seen<200000&&total<MAXT){seen++;var p=t.parentElement;if(!p)continue;var b=blockOf(t);
if(!shown(b)||(p!==b&&dsp(p)!=='contents'&&!shown(p)))continue;var i=idx.get(b);
if(i===undefined){if(els.length>=MAXB)continue;i=els.length;idx.set(b,i);els.push(b);tx.push('');}
var room=Math.min(MAXC-tx[i].length,MAXT-total);if(room<=0)continue;var v=t.nodeValue.replace(/\s+/g,' ').slice(0,room);tx[i]+=v;total+=v.length;}
return {els:els,tx:tx.map(function(s){return s.replace(/\s+/g,' ').trim();})};}"#;

/// The finished page code: [`COLLECT_JS`] with its limits, then the read.
fn with_collect(body: &str) -> String {
    let collect = COLLECT_JS
        .replace("__MAXB__", &MAX_BLOCKS.to_string())
        .replace("__MAXC__", &MAX_BLOCK_CHARS.to_string())
        .replace("__MAXT__", &MAX_TEXT_CHARS.to_string());
    format!("(function(){{{collect}\n{body}}})()")
}

/// Reads the page for a summary (C24): `{"t": title, "b": [block, …]}` as
/// a JSON string (the script's result). The blocks' elements stay in the
/// client world for [`scroll_js`]. Constant code, run in the client world
/// of the main frame only when the user asks for a summary.
pub fn blocks_js() -> String {
    with_collect(
        "var r=collect();window.__snorkleSum={els:r.els};\
         return JSON.stringify({t:String(document.title||'').slice(0,300),b:r.tx});",
    )
}

/// The page read for a summary: its title and blocks (indices are the
/// page's; a block may be empty after cleaning, and keeps its place).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageBlocks {
    pub title: String,
    pub blocks: Vec<String>,
}

impl PageBlocks {
    /// Whether any block has text.
    pub fn has_text(&self) -> bool {
        self.blocks.iter().any(|b| !b.is_empty())
    }
}

/// Parses what [`blocks_js`] returned (page-controlled). Fails closed:
/// anything but a bounded object with a title string and a list of at
/// most [`MAX_BLOCKS`] strings is rejected. Every string is sanitised
/// (control, bidi and invisible characters removed, one line, capped).
pub fn parse_blocks(body: &str) -> Option<PageBlocks> {
    if body.len() > MAX_BLOCKS_REPORT {
        return None;
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let title = sanitize_page_text(v.get("t").and_then(Value::as_str).unwrap_or(""), 300);
    let raw = v.get("b")?.as_array()?;
    if raw.len() > MAX_BLOCKS {
        return None;
    }
    let mut total = 0usize;
    let blocks = raw
        .iter()
        .map(|b| {
            let room = MAX_TEXT_CHARS.saturating_sub(total).min(MAX_BLOCK_CHARS);
            let t = if room == 0 { String::new() } else { sanitize_page_text(b.as_str()?, room) };
            total += t.chars().count();
            Some(t)
        })
        .collect::<Option<Vec<String>>>()?;
    Some(PageBlocks { title, blocks })
}

/// One bullet of a summary and the blocks (page indices) it was drawn
/// from, first where the point is made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SummaryPoint {
    pub text: String,
    pub blocks: Vec<u32>,
}

/// Scrolls the page to the first of `blocks` and highlights each briefly
/// (an outline and a gray wash that fade out; Web Animations, so the
/// page's own styles are untouched). The only input is up to
/// [`MAX_POINT_BLOCKS`] integers. If the page no longer holds any of the
/// elements read for the summary (reloaded, or redrawn by a script), it
/// reads its blocks again the same way and uses those.
pub fn scroll_js(blocks: &[u32]) -> String {
    let ids: Vec<String> =
        blocks.iter().filter(|b| (**b as usize) < MAX_BLOCKS).take(MAX_POINT_BLOCKS).map(u32::to_string).collect();
    with_collect(&format!(
        "var ids=[{}],s=window.__snorkleSum;\
         function pick(){{return ids.map(function(i){{return s&&s.els[i];}}).filter(function(e){{return e&&e.isConnected;}});}}\
         var l=pick();if(l.length<ids.length){{s=window.__snorkleSum={{els:collect().els}};l=pick();}}if(!l.length)return;\
         var e=l[0],r=e.getBoundingClientRect();e.scrollIntoView({{block:r.height>innerHeight*.6?'start':'center',behavior:'smooth'}});\
         var k=function(a,o){{return {{outlineStyle:'solid',outlineWidth:'3px',outlineOffset:'4px',\
         outlineColor:'rgba(110,110,110,'+a+')',backgroundColor:'rgba(128,128,128,'+(a*.22)+')',offset:o}};}};\
         l.forEach(function(x){{if(x.animate&&x!==document.body&&x!==document.documentElement)\
         x.animate([k(0,0),k(1,.15),k(1,.65),k(0,1)],{{duration:2200,delay:300,easing:'ease-in-out'}});}});",
        ids.join(",")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_code_is_constant_and_bounded() {
        let js = blocks_js();
        assert!(js.starts_with("(function(){function collect(){var MAXB=800,MAXC=1200,MAXT=40000;"));
        assert!(js.ends_with("b:r.tx});})()"));
        assert!(js.contains("window.__snorkleSum={els:r.els}"));
        assert!(!js.contains("__MAX") && !js.contains("messageHandlers"), "limits filled in; posts nothing");
        assert!(js.contains("seen<200000&&total<MAXT") && js.contains("els.length>=MAXB"));
        assert!(js.contains("|NAV|BUTTON|") && js.contains("role')==='navigation'"));
        assert!(js.contains("checkVisibility({visibilityProperty:true})"));
    }

    #[test]
    fn scroll_takes_only_indices() {
        let js = scroll_js(&[7, 3, 900_000, 4, 5, 6]);
        assert!(js.contains("var ids=[7,3,4,5],"), "out of range dropped, at most four");
        assert!(js.contains("scrollIntoView") && js.contains("x.animate("));
        assert!(
            js.contains("if(l.length<ids.length){s=window.__snorkleSum={els:collect().els};l=pick();}"),
            "reads again after a reload or a redraw"
        );
        assert!(scroll_js(&[]).contains("var ids=[],"));
    }

    #[test]
    fn blocks_are_validated() {
        let b = parse_blocks("{\"t\":\"Title \u{202e} x\",\"b\":[\"One\\nline\",\"\",\"\u{200b}\",\"Two\"]}").unwrap();
        assert_eq!(b.title, "Title x");
        assert_eq!(b.blocks, vec!["One line", "", "", "Two"], "indices kept");
        assert!(b.has_text());
        assert!(!parse_blocks(r#"{"t":"x","b":[]}"#).unwrap().has_text());
        assert!(parse_blocks(r#"{"t":"x"}"#).is_none());
        assert!(parse_blocks(r#"{"t":"x","b":[1]}"#).is_none(), "strings only");
        assert!(parse_blocks("[]").is_none());
        assert!(parse_blocks(&"x".repeat(MAX_BLOCKS_REPORT + 1)).is_none());
        let many = format!(r#"{{"b":[{}]}}"#, vec!["\"a\""; MAX_BLOCKS + 1].join(","));
        assert!(parse_blocks(&many).is_none());
        let long = "w".repeat(MAX_BLOCK_CHARS + 10);
        let b = parse_blocks(&format!(r#"{{"b":["{long}"]}}"#)).unwrap();
        assert_eq!(b.blocks[0].chars().count(), MAX_BLOCK_CHARS + 1, "capped with an ellipsis");
        // In all, at most MAX_TEXT_CHARS (plus an ellipsis per cut block).
        let body = format!(r#"{{"b":[{}]}}"#, vec![format!("\"{}\"", "w".repeat(MAX_BLOCK_CHARS)); 40].join(","));
        let b = parse_blocks(&body).unwrap();
        assert!(b.blocks.iter().map(|s| s.chars().count()).sum::<usize>() <= MAX_TEXT_CHARS + 1);
        assert_eq!(b.blocks.len(), 40);
    }
}
