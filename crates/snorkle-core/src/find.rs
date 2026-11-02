//! SECURITY-REVIEW: find in page (⌘F, owner request 2026-11-01; matches
//! highlighted instead of selected, 2026-11-02).
//!
//! A constant-shaped script in the client world finds the query's matches
//! in the page's text, highlights them with the CSS Custom Highlight API
//! (every match at 50 %, the current one at 100 %; the page's DOM and its
//! selection are left alone, so the selection menu does not appear),
//! scrolls the current one into view and returns its position, "3/12",
//! for the find bar's label. WebKit's native `findString:` is no longer
//! used: it selected each match.
//!
//! Security assumptions:
//! - The query comes from the user (trusted chrome) and is embedded only as
//!   an escaped JSON string literal; it runs in the client world.
//! - The position is read back from the page (channel C18):
//!   page-influenced, so only `digits/digits` with both numbers up to
//!   [`MAX_COUNT`] and the index not above the total is accepted.

use serde::{Deserialize, Serialize};

use crate::text::sanitize_page_text;
use crate::userscripts::json_str;

/// Longest query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 200;
/// Matches found at most (the label then reads "1000+").
pub const MAX_COUNT: u32 = 1000;

/// The query as it will be searched for: control and bidi characters
/// removed, whitespace collapsed, bounded. `None` if nothing is left.
pub fn validate_query(q: &str) -> Option<String> {
    let q = sanitize_page_text(q, MAX_QUERY_CHARS).trim_end_matches('…').to_string();
    (!q.is_empty()).then_some(q)
}

/// What the find bar asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindStep {
    /// A new query: find every match, current = the first one at or below
    /// the top of the view.
    Start,
    /// Next match (⌘G, ↩), wrapping.
    Next,
    /// Previous match (⌘⇧G, ⇧↩), wrapping.
    Prev,
}

/// Where the current match is: 1-based `index` of `total` (0 of 0 when
/// there is none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FindPosition {
    pub index: u32,
    pub total: u32,
}

/// Highlight styles (client world adds them as a `<style>`). Yellow-orange
/// so it stays visible in black and white (a light gray) and dark mode.
const HIGHLIGHT_CSS: &str = "::highlight(snorkle-find){background-color:rgba(255,176,0,.5)}\
::highlight(snorkle-find-current){background-color:rgb(255,176,0)}";

/// Finds, highlights and steps through the matches of `query`
/// (case-insensitive; any run of whitespace in the page matches a space;
/// within one text node, so a match split across elements is not found;
/// text in scripts, styles and hidden elements is skipped). Matches are
/// kept between steps and found again if the page replaced their text.
/// Returns "index/total".
pub fn find_js(query: &str, step: FindStep) -> String {
    let dir = match step {
        FindStep::Start => 0,
        FindStep::Next => 1,
        FindStep::Prev => -1,
    };
    format!(
        r#"(function(){{
var q={q},dir={dir},st=window.__snorkleFind;
function ensureStyle(){{var s=document.getElementById('__snorkle_find');if(!s){{s=document.createElement('style');s.id='__snorkle_find';s.textContent={css};}}
var r=document.head||document.documentElement;if(r&&!s.isConnected)r.appendChild(s);}}
function collect(){{var out=[],b=document.body;if(!b)return out;
var re=new RegExp(q.replace(/[.*+?^${{}}()|[\]\\]/g,'\\$&').replace(/ /g,'\\s+'),'gi');
var w=document.createTreeWalker(b,NodeFilter.SHOW_TEXT,{{acceptNode:function(n){{var p=n.parentElement;if(!p)return 2;
return /^(SCRIPT|STYLE|NOSCRIPT|TEXTAREA|TEMPLATE)$/.test(p.tagName)?2:1;}}}});
var n,m;while((n=w.nextNode())&&out.length<{MAX_COUNT}){{re.lastIndex=0;
while((m=re.exec(n.nodeValue))&&out.length<{MAX_COUNT}){{var r=document.createRange();r.setStart(n,m.index);r.setEnd(n,m.index+m[0].length);
if(r.getClientRects().length)out.push(r);}}}}return out;}}
function first(rs){{for(var k=0;k<rs.length;k++){{if(rs[k].getBoundingClientRect().bottom>=0)return k;}}return rs.length?0:-1;}}
var stale=!st||st.q!==q||st.r.some(function(r){{return !r.startContainer.isConnected;}});
if(dir===0||stale){{st={{q:q,r:collect(),cur:-1}};window.__snorkleFind=st;st.cur=first(st.r);}}
else if(st.r.length){{st.cur=(st.cur+dir+st.r.length)%st.r.length;}}
if(typeof CSS!=='undefined'&&CSS.highlights&&typeof Highlight!=='undefined'){{ensureStyle();
var all=new Highlight(),cur=new Highlight();cur.priority=1;
st.r.forEach(function(r,k){{(k===st.cur?cur:all).add(r);}});
CSS.highlights.set('snorkle-find',all);CSS.highlights.set('snorkle-find-current',cur);}}
var c=st.r[st.cur];if(c){{var rc=c.getBoundingClientRect(),h=window.innerHeight;
if(rc.top<80||rc.bottom>h-100){{var e=c.startContainer.parentElement;if(e)e.scrollIntoView({{block:'center'}});
rc=c.getBoundingClientRect();if(rc.top<80||rc.bottom>h-100)window.scrollBy(0,rc.top-h/2);}}}}
return (st.cur+1)+'/'+st.r.length;
}})()"#,
        q = json_str(query),
        css = json_str(HIGHLIGHT_CSS),
    )
}

/// Removes the highlights and the find state (closing the bar).
pub const CLEAR_JS: &str = "(function(){if(typeof CSS!=='undefined'&&CSS.highlights){CSS.highlights.delete('snorkle-find');\
CSS.highlights.delete('snorkle-find-current');}var s=document.getElementById('__snorkle_find');if(s)s.remove();\
delete window.__snorkleFind;})();";

/// Parses what [`find_js`] returned: `index/total`, digits only, both at
/// most [`MAX_COUNT`], index ≤ total, index 0 only with total 0.
pub fn parse_position(s: &str) -> Option<FindPosition> {
    let (i, t) = s.split_once('/')?;
    let num = |x: &str| {
        (!x.is_empty() && x.len() <= 4 && x.bytes().all(|b| b.is_ascii_digit()))
            .then(|| x.parse::<u32>().ok())
            .flatten()
            .filter(|n| *n <= MAX_COUNT)
    };
    let (index, total) = (num(i)?, num(t)?);
    (index <= total && (index > 0) == (total > 0)).then_some(FindPosition { index, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_and_positions() {
        assert_eq!(validate_query("  hello   world \u{202e} ").as_deref(), Some("hello world"));
        assert!(validate_query(" \n\t ").is_none());
        assert!(validate_query(&"a".repeat(500)).unwrap().chars().count() <= MAX_QUERY_CHARS);
        let js = find_js("a\"</script>'b", FindStep::Start);
        assert!(!js.contains("</script>") && js.contains("u003c"), "embedded as an escaped JSON literal");
        assert!(js.contains("dir=0") && find_js("x", FindStep::Prev).contains("dir=-1"));
        assert!(!js.contains("getSelection") && !js.contains("findString"), "highlights, never selects");
        assert!(js.contains("CSS.highlights.set('snorkle-find',all)"));
        assert_eq!(parse_position("3/12"), Some(FindPosition { index: 3, total: 12 }));
        assert_eq!(parse_position("0/0"), Some(FindPosition { index: 0, total: 0 }));
        for bad in ["", "3", "13/12", "0/4", "1/0", "-1/2", "1/1001", "1 /2", "1/2/3", "a/b", "99999/99999"] {
            assert!(parse_position(bad).is_none(), "{bad}");
        }
    }
}
