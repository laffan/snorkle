//! SECURITY-REVIEW: find in page (⌘F, owner request 2026-11-01).
//!
//! Finding itself uses WebKit's native `findString:withConfiguration:`
//! (selects and scrolls to the next match; only a "found" flag comes back).
//! This module validates the query the user typed and generates the
//! constant-shaped script that counts matches for the "3 of 12" label.
//!
//! Security assumptions:
//! - The query comes from the user (trusted chrome) and is embedded only as
//!   an escaped JSON string literal; it runs in the client world.
//! - The count is read back from the page (channel C18): page-influenced,
//!   so only a decimal integer up to [`MAX_COUNT`] is accepted.

use crate::text::sanitize_page_text;
use crate::userscripts::json_str;

/// Longest query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 200;
/// Matches counted at most (the label then reads "1000+").
pub const MAX_COUNT: u32 = 1000;

/// The query as it will be searched for: control and bidi characters
/// removed, whitespace collapsed, bounded. `None` if nothing is left.
pub fn validate_query(q: &str) -> Option<String> {
    let q = sanitize_page_text(q, MAX_QUERY_CHARS).trim_end_matches('…').to_string();
    (!q.is_empty()).then_some(q)
}

/// Counts case-insensitive occurrences of `query` in the page's visible
/// text, up to [`MAX_COUNT`], and returns the number as a string.
pub fn count_js(query: &str) -> String {
    format!(
        "(function(){{var q={q}.toLowerCase(),b=document.body;if(!q||!b)return '0';\
         var t=(b.innerText||'').replace(/\\s+/g,' ').toLowerCase(),n=0,i=0;\
         while(n<{MAX_COUNT}&&(i=t.indexOf(q,i))>=0){{n++;i+=q.length;}}return String(n);}})()",
        q = json_str(query)
    )
}

/// Clears the selection the native find left on the page (closing the bar).
pub const CLEAR_JS: &str =
    "(function(){var s=window.getSelection&&window.getSelection();if(s)s.removeAllRanges();})();";

/// Parses what [`count_js`] returned: digits only, at most [`MAX_COUNT`].
pub fn parse_count(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().map(|n| n.min(MAX_COUNT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_and_counts() {
        assert_eq!(validate_query("  hello   world \u{202e} ").as_deref(), Some("hello world"));
        assert!(validate_query(" \n\t ").is_none());
        assert!(validate_query(&"a".repeat(500)).unwrap().chars().count() <= MAX_QUERY_CHARS);
        let js = count_js("a\"</script>'b");
        assert!(!js.contains("</script>") && js.contains("u003c"), "embedded as an escaped JSON literal");
        assert_eq!(parse_count("12"), Some(12));
        assert_eq!(parse_count("99999"), Some(MAX_COUNT));
        for bad in ["", "-1", "1.5", "12a", "123456", " 3"] {
            assert!(parse_count(bad).is_none(), "{bad}");
        }
    }
}
