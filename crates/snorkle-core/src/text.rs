//! Sanitising of page-controlled strings (titles, dialog text) before they
//! reach browser chrome.
//!
//! Security assumptions: the chrome UI always inserts these strings with
//! `textContent` (never as HTML). This module additionally removes bidi
//! overrides/isolates, control and invisible characters so a page cannot
//! reorder or hide text shown next to trusted UI, and caps the length.

/// Maximum number of characters of page-provided text shown in chrome.
pub const MAX_PAGE_TEXT_CHARS: usize = 300;

/// Returns `input` with control, bidi-control and invisible characters
/// removed, runs of whitespace collapsed to a single space, and at most
/// `max_chars` characters (an ellipsis is appended when truncated).
pub fn sanitize_page_text(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars * 4));
    let mut count = 0usize;
    let mut pending_space = false;
    for c in input.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if is_stripped(c) {
            continue;
        }
        if pending_space {
            if count + 1 >= max_chars {
                out.push('…');
                return out;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= max_chars {
            out.push('…');
            return out;
        }
        out.push(c);
        count += 1;
    }
    out
}

fn is_stripped(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bidi_and_controls() {
        let s = sanitize_page_text("Pay\u{202E}lapyap\u{202C}\n\tnow\u{0000}", 100);
        assert_eq!(s, "Paylapyap now");
    }

    #[test]
    fn truncates() {
        let s = sanitize_page_text(&"a".repeat(1000), 10);
        assert_eq!(s.chars().count(), 11);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(sanitize_page_text("  a   b  ", 100), "a b");
    }
}
