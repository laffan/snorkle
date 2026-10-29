//! SECURITY-REVIEW: content blocking (ad and tracker blocking).
//!
//! WKWebView offers no request interception for http(s) subresources, so
//! enforcement inside pages is done by WebKit's compiled content rule lists
//! (`WKContentRuleListStore`), which run in WebKit's own processes without
//! a round trip to the app. Rust owns everything before that:
//! - parsing ABP/uBlock-syntax filter lists and converting them to WebKit
//!   rules (Brave's `adblock` crate, `content-blocking` feature);
//! - splitting the result into lists under WebKit's per-list rule limit;
//! - the per-site "allow ads on this site" exceptions;
//! - a cache key so WebKit's compiled lists are reused across launches.
//!
//! Scriptlet / procedural cosmetic filters are NOT supported: they require
//! injecting scripts into pages, which `CONTENT_VIEW_SPEC` forbids. The
//! converter skips them.
//!
//! Security assumptions:
//! - Filter lists are bundled at build time (`src-tauri/resources/filters`)
//!   and trusted as data. A malicious list could block or hide content but
//!   cannot run code: WebKit rules only block loads, strip cookies, upgrade
//!   to https, or hide elements by CSS selector.
//! - Allowlisted sites come from the user via trusted chrome and are
//!   validated to plain ASCII domains before being placed in a regex.

use std::collections::BTreeSet;

use adblock::content_blocking::{CbRule, CbType};
use adblock::lists::{FilterSet, ParseOptions};
use serde_json::json;

/// WebKit's documented per-list limit is 150,000 rules. Lists are split
/// well below that so each chunk compiles quickly and leaves room for the
/// exception rules every chunk must repeat.
pub const MAX_RULES_PER_LIST: usize = 50_000;
/// Upper bound on generated lists, to bound compile time and memory.
pub const MAX_LISTS: usize = 8;
/// Bump when the conversion changes, to invalidate WebKit's compiled cache.
pub const CONVERTER_VERSION: u32 = 1;

pub struct FilterList<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

#[derive(Debug)]
pub struct Compiled {
    /// WebKit rule-list JSON documents, in install order.
    pub lists: Vec<String>,
    /// Rules across all lists, excluding repeated exceptions.
    pub rules: usize,
    /// Exception rules repeated in every list.
    pub exceptions: usize,
    /// Filter-list lines that produced at least one rule.
    pub filters_used: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompileError {
    Converter,
    TooManyExceptions(usize),
    TooManyRules(usize),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Converter => write!(f, "filter conversion failed"),
            CompileError::TooManyExceptions(n) => write!(f, "{n} exception rules exceed the per-list limit"),
            CompileError::TooManyRules(n) => write!(f, "{n} rules exceed {MAX_LISTS} lists"),
        }
    }
}

/// Stable cache key (FNV-1a 64) over the converter version, list contents
/// and allowlist. Same inputs → same key → WebKit's compiled lists are
/// reused instead of recompiled.
pub fn cache_key(lists: &[FilterList<'_>], allowed_sites: &BTreeSet<String>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    feed(&CONVERTER_VERSION.to_le_bytes());
    for l in lists {
        feed(l.name.as_bytes());
        feed(l.text.as_bytes());
    }
    for s in allowed_sites {
        feed(s.as_bytes());
    }
    format!("{h:016x}")
}

/// Converts filter lists to WebKit content rule lists. CPU-heavy (about a
/// second for EasyList + EasyPrivacy on a laptop): call off the main thread.
pub fn compile(lists: &[FilterList<'_>], allowed_sites: &BTreeSet<String>) -> Result<Compiled, CompileError> {
    let mut set = FilterSet::new(true); // debug mode is required by the converter
    for l in lists {
        set.add_filter_list(l.text.to_string(), ParseOptions::default());
    }
    let (rules, used) = set.into_content_blocking().map_err(|_| CompileError::Converter)?;

    // WebKit applies `ignore-previous-rules` only to earlier rules in the
    // SAME list, so every chunk carries all exceptions at its end, followed
    // by the user's per-site allow rule.
    let (mut exceptions, blocking): (Vec<CbRule>, Vec<CbRule>) =
        rules.into_iter().partition(|r| matches!(r.action.typ, CbType::IgnorePreviousRules));
    let mut tail: Vec<serde_json::Value> = Vec::with_capacity(exceptions.len() + 1);
    for r in exceptions.drain(..) {
        tail.push(serde_json::to_value(r).map_err(|_| CompileError::Converter)?);
    }
    let exception_count = tail.len();
    if let Some(rule) = allow_sites_rule(allowed_sites) {
        tail.push(rule);
    }
    if tail.len() >= MAX_RULES_PER_LIST {
        return Err(CompileError::TooManyExceptions(tail.len()));
    }
    let per_chunk = MAX_RULES_PER_LIST - tail.len();
    let chunk_count = blocking.len().div_ceil(per_chunk);
    if chunk_count > MAX_LISTS {
        return Err(CompileError::TooManyRules(blocking.len()));
    }

    let mut out = Vec::with_capacity(chunk_count);
    for chunk in blocking.chunks(per_chunk) {
        let mut list: Vec<serde_json::Value> = Vec::with_capacity(chunk.len() + tail.len());
        for r in chunk {
            list.push(serde_json::to_value(r).map_err(|_| CompileError::Converter)?);
        }
        list.extend(tail.iter().cloned());
        out.push(serde_json::Value::Array(list).to_string());
    }
    Ok(Compiled { lists: out, rules: blocking.len(), exceptions: exception_count, filters_used: used.len() })
}

/// One `ignore-previous-rules` rule that disables blocking on pages whose
/// top-level document is on an allowlisted site (and its subdomains).
fn allow_sites_rule(sites: &BTreeSet<String>) -> Option<serde_json::Value> {
    let patterns: Vec<String> = sites
        .iter()
        .filter_map(|s| normalise_site(s))
        .map(|d| format!("^[^:]+://+([^:/]+\\.)?{}[:/]", d.replace('.', "\\.")))
        .collect();
    if patterns.is_empty() {
        return None;
    }
    Some(json!({
        "trigger": { "url-filter": ".*", "if-top-url": patterns },
        "action": { "type": "ignore-previous-rules" },
    }))
}

/// The site key used for the per-site toggle: the registrable domain
/// (eTLD+1) of an http(s) URL, or the host when there is none (IP,
/// localhost). ASCII/punycode only.
pub fn site_key(url: &url::Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match url.host()? {
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        // IPv6 literals cannot be expressed in the allowlist regex.
        url::Host::Ipv6(_) => None,
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.');
            normalise_site(psl::domain_str(d).unwrap_or(d))
        }
    }
}

/// Lowercase ASCII domain made of `[a-z0-9-]` labels (or an IPv4 address),
/// else None. Guarantees the value is safe to embed in a WebKit regex.
pub fn normalise_site(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('.').to_ascii_lowercase();
    if s.is_empty() || s.len() > 253 {
        return None;
    }
    let ok = s.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    ok.then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
! Title: test
||ads.example^
||tracker.example^$third-party
/banner/*$image
example.com##.ad-slot
@@||ads.example/allowed.js
##+js(nobab)
example.com#?#div:has(> .sponsored)
";

    fn lists() -> Vec<FilterList<'static>> {
        vec![FilterList { name: "sample", text: SAMPLE }]
    }

    fn parsed(c: &Compiled, i: usize) -> Vec<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(&c.lists[i]).unwrap().as_array().unwrap().clone()
    }

    #[test]
    fn converts_network_cosmetic_and_exception_rules() {
        let c = compile(&lists(), &BTreeSet::new()).unwrap();
        assert_eq!(c.lists.len(), 1);
        let rules = parsed(&c, 0);
        let types: Vec<&str> = rules.iter().map(|r| r["action"]["type"].as_str().unwrap()).collect();
        assert!(types.contains(&"block"));
        assert!(types.contains(&"css-display-none"));
        assert_eq!(*types.last().unwrap(), "ignore-previous-rules", "exceptions must come last");
        // Scriptlets are never emitted (they need script injection). `:has()`
        // is native CSS that WebKit supports, so it may be kept as a
        // css-display-none selector.
        assert!(!c.lists[0].contains("nobab"));
        assert!(!c.lists[0].contains("+js"));
    }

    #[test]
    fn allowlisted_sites_add_a_final_exception() {
        let mut allow = BTreeSet::new();
        allow.insert("news.example".to_string());
        allow.insert("bad\"]}regex(".to_string()); // rejected by normalise_site
        let c = compile(&lists(), &allow).unwrap();
        let rules = parsed(&c, 0);
        let last = rules.last().unwrap();
        assert_eq!(last["action"]["type"], "ignore-previous-rules");
        assert_eq!(last["trigger"]["if-top-url"], json!(["^[^:]+://+([^:/]+\\.)?news\\.example[:/]"]));
        assert!(!c.lists[0].contains("regex("));
    }

    #[test]
    fn chunks_each_repeat_all_exceptions() {
        let mut text = String::new();
        for i in 0..(MAX_RULES_PER_LIST + 10) {
            text.push_str(&format!("||ads{i}.example^\n"));
        }
        text.push_str("@@||ads1.example/ok.js\n");
        let c = compile(&[FilterList { name: "big", text: &text }], &BTreeSet::new()).unwrap();
        assert_eq!(c.lists.len(), 2);
        for i in 0..2 {
            let rules = parsed(&c, i);
            assert!(rules.len() <= MAX_RULES_PER_LIST);
            assert!(rules.iter().any(|r| r["action"]["type"] == "ignore-previous-rules"));
        }
    }

    #[test]
    fn cache_key_depends_on_inputs() {
        let a = cache_key(&lists(), &BTreeSet::new());
        assert_eq!(a, cache_key(&lists(), &BTreeSet::new()));
        let mut allow = BTreeSet::new();
        allow.insert("x.example".to_string());
        assert_ne!(a, cache_key(&lists(), &allow));
        assert_ne!(a, cache_key(&[FilterList { name: "sample", text: "||other.example^" }], &BTreeSet::new()));
    }

    #[test]
    fn site_keys() {
        let k = |s: &str| site_key(&url::Url::parse(s).unwrap());
        assert_eq!(k("https://www.news.co.uk/a"), Some("news.co.uk".into()));
        assert_eq!(k("https://user.github.io/"), Some("user.github.io".into()));
        assert_eq!(k("http://192.168.1.1/"), Some("192.168.1.1".into()));
        assert_eq!(k("https://xn--mnchen-3ya.de/"), Some("xn--mnchen-3ya.de".into()));
        assert_eq!(k("about:blank"), None);
        assert_eq!(k("https://[::1]/"), None);
    }
}
