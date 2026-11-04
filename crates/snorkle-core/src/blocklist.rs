//! SECURITY-REVIEW: the user's block list (Settings → Blocked, 2026-11-04).
//!
//! Patterns are regular expressions (Rust `regex` syntax: linear-time, no
//! backreferences or look-around, so a pattern cannot hang the main
//! thread) matched case-insensitively, anywhere, against a web address
//! without its scheme, user info, query or fragment: `host[:port]/path`,
//! e.g. `www.reddit.com/r/rust/`. `reddit\.com` blocks every Reddit page;
//! `(^|\.)x\.com/` blocks x.com and its subdomains but not netflix.com;
//! `youtube\.com/shorts` blocks only Shorts.
//!
//! A main-frame navigation to a matching `http`/`https` address never
//! reaches WebKit (`Browser::navigate_trusted`, `decide_navigation`); a
//! page that reaches one anyway (a same-document URL change in a
//! single-page app, a list edited while the page is open) is hidden behind
//! the chrome's notice with the user's message. Frames inside pages are not
//! affected.
//!
//! Adding a pattern is free. Changing or removing one requires the passage
//! below, typed out in full: the chrome asks for it letter by letter (no
//! pasting) and sends it with the change, and the change is refused here
//! unless it matches exactly. This is a commitment device, not a security
//! boundary: the list is a file in the app's own container.

use regex::{RegexBuilder, RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::text::sanitize_page_text;

/// What must be typed to change or remove a blocked address.
pub const UNBLOCK_PASSAGE: &str = "The only way to get rid of a temptation is to yield to it. Resist it, and your soul grows sick with longing for the things it has forbidden to itself, with desire for what its monstrous laws have made monstrous and unlawful.";

pub const MAX_PATTERNS: usize = 500;
pub const MAX_PATTERN_LEN: usize = 300;
pub const MAX_MESSAGE_CHARS: usize = 500;
pub const DEFAULT_MESSAGE: &str = "You blocked this site.";
/// Compiled size limits: one pattern, and the whole list.
const PATTERN_SIZE_LIMIT: usize = 256 * 1024;
const SET_SIZE_LIMIT: usize = 32 * 1024 * 1024;

/// What is stored (`blocked.json`) and shown in Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSettings {
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Shown in the middle of a blocked tab. Empty: [`DEFAULT_MESSAGE`].
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    Empty,
    TooLong,
    TooMany,
    Duplicate,
    Invalid(String),
    NotFound,
    /// The passage was not typed exactly.
    Passage,
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockError::Empty => write!(f, "Enter an address pattern."),
            BlockError::TooLong => write!(f, "That pattern is too long (at most {MAX_PATTERN_LEN} characters)."),
            BlockError::TooMany => write!(f, "The list is full ({MAX_PATTERNS} patterns)."),
            BlockError::Duplicate => write!(f, "That pattern is already in the list."),
            BlockError::Invalid(e) => write!(f, "Not a valid pattern: {e}"),
            BlockError::NotFound => write!(f, "That pattern is no longer in the list."),
            BlockError::Passage => write!(f, "The passage was not typed exactly."),
        }
    }
}

/// The block list with its patterns compiled.
#[derive(Clone, Debug, Default)]
pub struct BlockList {
    settings: BlockSettings,
    set: Option<RegexSet>,
}

/// A pattern as stored: trimmed, and a leading `http://` or `https://`
/// dropped (addresses are matched without their scheme, so a pasted
/// address still works). Checked to compile on its own.
pub fn clean_pattern(p: &str) -> Result<String, BlockError> {
    let mut t = p.trim();
    for scheme in ["https://", "http://"] {
        if t.len() >= scheme.len() && t[..scheme.len()].eq_ignore_ascii_case(scheme) {
            t = &t[scheme.len()..];
            break;
        }
    }
    if t.is_empty() {
        return Err(BlockError::Empty);
    }
    if t.chars().count() > MAX_PATTERN_LEN {
        return Err(BlockError::TooLong);
    }
    if t.chars().any(char::is_control) {
        return Err(BlockError::Invalid("control characters are not allowed".into()));
    }
    RegexBuilder::new(t)
        .case_insensitive(true)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| BlockError::Invalid(first_line(&e.to_string())))?;
    Ok(t.to_string())
}

fn first_line(s: &str) -> String {
    // regex errors are multi-line drawings of the pattern; the last
    // line names the problem.
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches("error: ").to_string()
}

pub fn clean_message(m: &str) -> String {
    sanitize_page_text(m, MAX_MESSAGE_CHARS)
}

/// The text patterns are matched against: `host[:port]/path` for http(s)
/// addresses, `None` for anything else.
pub fn subject(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.');
    let mut s = String::with_capacity(host.len() + url.path().len() + 6);
    s.push_str(host);
    if let Some(port) = url.port() {
        s.push(':');
        s.push_str(&port.to_string());
    }
    s.push_str(url.path());
    Some(s)
}

impl BlockList {
    /// Builds the list from stored settings, dropping patterns that are
    /// invalid or duplicated (a file edited by hand, a newer version).
    pub fn from_settings(s: BlockSettings) -> Self {
        let mut patterns: Vec<String> = Vec::new();
        for p in s.patterns {
            if let Ok(p) = clean_pattern(&p) {
                if !patterns.contains(&p) && patterns.len() < MAX_PATTERNS {
                    patterns.push(p);
                }
            }
        }
        let settings = BlockSettings { patterns, message: clean_message(&s.message) };
        let mut list = BlockList { settings, set: None };
        if list.compile().is_err() {
            // Too big together: keep what fits, in order.
            let all = std::mem::take(&mut list.settings.patterns);
            for p in all {
                list.settings.patterns.push(p);
                if list.compile().is_err() {
                    list.settings.patterns.pop();
                }
            }
            let _ = list.compile();
        }
        list
    }

    fn compile(&mut self) -> Result<(), BlockError> {
        if self.settings.patterns.is_empty() {
            self.set = None;
            return Ok(());
        }
        let set = RegexSetBuilder::new(&self.settings.patterns)
            .case_insensitive(true)
            .size_limit(SET_SIZE_LIMIT)
            .build()
            .map_err(|e| BlockError::Invalid(first_line(&e.to_string())))?;
        self.set = Some(set);
        Ok(())
    }

    pub fn settings(&self) -> &BlockSettings {
        &self.settings
    }

    pub fn patterns(&self) -> &[String] {
        &self.settings.patterns
    }

    /// The message for a blocked tab.
    pub fn message(&self) -> &str {
        if self.settings.message.is_empty() {
            DEFAULT_MESSAGE
        } else {
            &self.settings.message
        }
    }

    /// Whether a main-frame visit to `url` is blocked.
    pub fn matches(&self, url: &Url) -> bool {
        match (&self.set, subject(url)) {
            (Some(set), Some(s)) => set.is_match(&s),
            _ => false,
        }
    }

    /// The first pattern that blocks `url`, if any.
    pub fn matching_pattern(&self, url: &Url) -> Option<&str> {
        let set = self.set.as_ref()?;
        let s = subject(url)?;
        set.matches(&s).iter().next().map(|i| self.settings.patterns[i].as_str())
    }

    /// Appends a pattern. Returns the pattern as stored.
    pub fn add(&mut self, pattern: &str) -> Result<String, BlockError> {
        let p = clean_pattern(pattern)?;
        if self.settings.patterns.contains(&p) {
            return Err(BlockError::Duplicate);
        }
        if self.settings.patterns.len() >= MAX_PATTERNS {
            return Err(BlockError::TooMany);
        }
        self.settings.patterns.push(p.clone());
        if let Err(e) = self.compile() {
            self.settings.patterns.pop();
            let _ = self.compile();
            return Err(e);
        }
        Ok(p)
    }

    /// Replaces `old` with `new`, or removes it when `new` is `None`.
    /// Refused unless `passage` is exactly [`UNBLOCK_PASSAGE`].
    pub fn edit(&mut self, old: &str, new: Option<&str>, passage: &str) -> Result<(), BlockError> {
        if passage != UNBLOCK_PASSAGE {
            return Err(BlockError::Passage);
        }
        let i = self.settings.patterns.iter().position(|p| p == old).ok_or(BlockError::NotFound)?;
        let before = self.settings.patterns.clone();
        match new {
            None => {
                self.settings.patterns.remove(i);
            }
            Some(n) => {
                let n = clean_pattern(n)?;
                if n != old && self.settings.patterns.contains(&n) {
                    return Err(BlockError::Duplicate);
                }
                self.settings.patterns[i] = n;
            }
        }
        if let Err(e) = self.compile() {
            self.settings.patterns = before;
            let _ = self.compile();
            return Err(e);
        }
        Ok(())
    }

    pub fn set_message(&mut self, message: &str) {
        self.settings.message = clean_message(message);
    }
}

/// A pattern for "Block This Site": the site's registrable domain and its
/// subdomains, e.g. `(^|\.)reddit\.com(:|/)` (not `notreddit.com`).
pub fn site_pattern(url: &Url) -> Option<String> {
    let site = crate::blocking::site_key(url)?;
    Some(format!(r"(^|\.){}(:|/)", regex::escape(&site)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn list(patterns: &[&str]) -> BlockList {
        BlockList::from_settings(BlockSettings {
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            message: String::new(),
        })
    }

    #[test]
    fn matches_host_and_path_without_scheme() {
        let l = list(&["reddit\\.com", "youtube\\.com/shorts", "(^|\\.)x\\.com/"]);
        assert!(l.matches(&u("https://www.reddit.com/r/rust")));
        assert!(l.matches(&u("http://REDDIT.com/")));
        assert!(l.matches(&u("https://www.youtube.com/shorts/abc")));
        assert!(!l.matches(&u("https://www.youtube.com/watch?v=1")));
        assert!(l.matches(&u("https://x.com/home")));
        assert!(l.matches(&u("https://mobile.x.com/")));
        assert!(!l.matches(&u("https://netflix.com/")));
        // The query is not matched: a search for the site is not blocked.
        assert!(!l.matches(&u("https://www.google.com/search?q=reddit.com")));
        // Only web addresses.
        assert!(!l.matches(&u("about:blank")));
        assert_eq!(l.matching_pattern(&u("https://old.reddit.com/")), Some("reddit\\.com"));
    }

    #[test]
    fn empty_list_blocks_nothing() {
        let l = BlockList::default();
        assert!(!l.matches(&u("https://example.com/")));
        assert_eq!(l.message(), DEFAULT_MESSAGE);
    }

    #[test]
    fn subject_shape() {
        assert_eq!(subject(&u("https://user:pw@Example.com.:8443/a/b?c#d")).as_deref(), Some("example.com:8443/a/b"));
        assert_eq!(subject(&u("https://example.com")).as_deref(), Some("example.com/"));
        assert_eq!(subject(&u("data:text/html,x")), None);
    }

    #[test]
    fn patterns_are_cleaned_and_validated() {
        assert_eq!(clean_pattern("  https://reddit.com/ ").unwrap(), "reddit.com/");
        assert_eq!(clean_pattern("HTTP://a.b").unwrap(), "a.b");
        assert_eq!(clean_pattern("  "), Err(BlockError::Empty));
        assert!(matches!(clean_pattern("(unclosed"), Err(BlockError::Invalid(_))));
        assert_eq!(clean_pattern(&"a".repeat(MAX_PATTERN_LEN + 1)), Err(BlockError::TooLong));
        assert!(matches!(clean_pattern("a\nb"), Err(BlockError::Invalid(_))));
        // Huge compiled programs are refused rather than built.
        assert!(matches!(clean_pattern("\\w{1000}{1000}"), Err(BlockError::Invalid(_))));
    }

    #[test]
    fn adding_is_free_but_duplicates_and_bad_patterns_are_refused() {
        let mut l = BlockList::default();
        assert_eq!(l.add("reddit\\.com").unwrap(), "reddit\\.com");
        assert_eq!(l.add("reddit\\.com"), Err(BlockError::Duplicate));
        assert!(matches!(l.add("[z-a]"), Err(BlockError::Invalid(_))));
        assert_eq!(l.patterns(), ["reddit\\.com"]);
        assert!(l.matches(&u("https://reddit.com/")));
    }

    #[test]
    fn editing_and_removing_need_the_exact_passage() {
        let mut l = list(&["reddit\\.com", "news\\.ycombinator\\.com"]);
        let almost = UNBLOCK_PASSAGE.trim_end_matches('.');
        assert_eq!(l.edit("reddit\\.com", None, almost), Err(BlockError::Passage));
        assert_eq!(l.edit("reddit\\.com", None, ""), Err(BlockError::Passage));
        assert_eq!(l.edit("reddit\\.com", None, &UNBLOCK_PASSAGE.to_lowercase()), Err(BlockError::Passage));
        assert!(l.matches(&u("https://reddit.com/")));

        assert_eq!(l.edit("reddit\\.com", Some("old\\.reddit\\.com"), UNBLOCK_PASSAGE), Ok(()));
        assert!(!l.matches(&u("https://www.reddit.com/")));
        assert!(l.matches(&u("https://old.reddit.com/")));
        assert_eq!(
            l.edit("old\\.reddit\\.com", Some("news\\.ycombinator\\.com"), UNBLOCK_PASSAGE),
            Err(BlockError::Duplicate)
        );
        assert!(matches!(l.edit("old\\.reddit\\.com", Some("("), UNBLOCK_PASSAGE), Err(BlockError::Invalid(_))));
        assert_eq!(l.edit("missing", None, UNBLOCK_PASSAGE), Err(BlockError::NotFound));

        assert_eq!(l.edit("old\\.reddit\\.com", None, UNBLOCK_PASSAGE), Ok(()));
        assert_eq!(l.patterns(), ["news\\.ycombinator\\.com"]);
        assert!(!l.matches(&u("https://old.reddit.com/")));
    }

    #[test]
    fn stored_lists_are_cleaned_on_load() {
        let l = BlockList::from_settings(BlockSettings {
            patterns: vec!["a\\.com".into(), "(".into(), "a\\.com".into(), " https://b.com ".into()],
            message: "Not now.\u{202E}\n".into(),
        });
        assert_eq!(l.patterns(), ["a\\.com", "b.com"]);
        assert_eq!(l.message(), "Not now.");
    }

    #[test]
    fn site_pattern_blocks_the_site_and_its_subdomains_only() {
        let p = site_pattern(&u("https://www.reddit.com/r/x")).unwrap();
        assert_eq!(p, "(^|\\.)reddit\\.com(:|/)");
        let l = list(&[&p]);
        assert!(l.matches(&u("https://reddit.com/")));
        assert!(l.matches(&u("https://old.reddit.com/r/rust")));
        assert!(!l.matches(&u("https://notreddit.com/")));
        assert!(!l.matches(&u("https://reddit.com.evil.example/")));
    }
}
