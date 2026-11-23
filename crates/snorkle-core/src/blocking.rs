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
pub const CONVERTER_VERSION: u32 = 3;

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
    // SAME list, so every chunk carries the exceptions at its end, followed
    // by the user's per-site allow rule.
    //
    // Element hiding (css-display-none) gets lists of its own (2026-11-23).
    // The converter ends its rules with one exception for every first-party
    // document (`ignore_previous_fp_documents`, so a site is never blocked
    // as a page), meant to follow the network rules only. WebKit takes a
    // page's hiding rules from its document load, so that exception, after
    // the hiding rules in the same list, cancelled every one of them on
    // every page: no element was ever hidden. Hiding lists carry every other
    // exception (`$document`, `$elemhide`, `$generichide` and the per-site
    // allow rule still turn hiding off where they should).
    let fp_documents =
        serde_json::to_value(adblock::content_blocking::ignore_previous_fp_documents()).map_err(|_| CompileError::Converter)?;
    let (mut exceptions, rest): (Vec<CbRule>, Vec<CbRule>) =
        rules.into_iter().partition(|r| matches!(r.action.typ, CbType::IgnorePreviousRules));
    let (hiding, blocking): (Vec<CbRule>, Vec<CbRule>) =
        rest.into_iter().partition(|r| matches!(r.action.typ, CbType::CssDisplayNone));
    let mut network_tail: Vec<serde_json::Value> = Vec::with_capacity(exceptions.len() + 1);
    for r in exceptions.drain(..) {
        network_tail.push(serde_json::to_value(r).map_err(|_| CompileError::Converter)?);
    }
    let exception_count = network_tail.len();
    if let Some(rule) = allow_sites_rule(allowed_sites) {
        network_tail.push(rule);
    }
    let hiding_tail: Vec<serde_json::Value> = network_tail.iter().filter(|r| **r != fp_documents).cloned().collect();
    if network_tail.len() >= MAX_RULES_PER_LIST {
        return Err(CompileError::TooManyExceptions(network_tail.len()));
    }
    let mut out = Vec::new();
    for (rules, tail) in [(&blocking, &network_tail), (&hiding, &hiding_tail)] {
        let per_chunk = MAX_RULES_PER_LIST - tail.len();
        for chunk in rules.chunks(per_chunk) {
            let mut list: Vec<serde_json::Value> = Vec::with_capacity(chunk.len() + tail.len());
            for r in chunk {
                let mut v = serde_json::to_value(r).map_err(|_| CompileError::Converter)?;
                top_frame_only_if_generic(&mut v);
                list.push(v);
            }
            list.extend(tail.iter().cloned());
            out.push(serde_json::Value::Array(list).to_string());
        }
    }
    if out.len() > MAX_LISTS {
        return Err(CompileError::TooManyRules(blocking.len() + hiding.len()));
    }
    Ok(Compiled { lists: out, rules: blocking.len() + hiding.len(), exceptions: exception_count, filters_used: used.len() })
}

/// Generic hiding (a selector for every site) applies to the page itself,
/// not to the frames in it (2026-11-23). WebKit adds the generic selectors
/// (13,636 from EasyList) to the style of every document they apply to;
/// measured in WebKitGTK (same WebCore code), a page with 40 frames took
/// 1,110 ms to load with them in every frame and 290 ms with them in the
/// page only (233 ms without lists), while the page itself hid the same.
/// Ads in frames are mostly stopped by the network rules. Hiding meant for
/// particular sites (`if-domain`, `if-top-url`) is left as it is.
fn top_frame_only_if_generic(rule: &mut serde_json::Value) {
    if rule["action"]["type"] != "css-display-none" {
        return;
    }
    let Some(trigger) = rule["trigger"].as_object_mut() else { return };
    let generic = trigger.get("url-filter").and_then(|f| f.as_str()) == Some(".*")
        && !trigger.contains_key("if-domain")
        && !trigger.contains_key("if-top-url");
    if generic {
        trigger.insert("load-context".into(), json!(["top-frame"]));
    }
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

    fn types(rules: &[serde_json::Value]) -> Vec<&str> {
        rules.iter().map(|r| r["action"]["type"].as_str().unwrap()).collect()
    }

    #[test]
    fn converts_network_cosmetic_and_exception_rules() {
        let c = compile(&lists(), &BTreeSet::new()).unwrap();
        assert_eq!(c.lists.len(), 2, "network rules, then element hiding");
        let (network, hiding) = (parsed(&c, 0), parsed(&c, 1));
        assert!(types(&network).contains(&"block") && !types(&network).contains(&"css-display-none"));
        assert!(types(&hiding).contains(&"css-display-none") && !types(&hiding).contains(&"block"));
        for rules in [&network, &hiding] {
            assert_eq!(*types(rules).last().unwrap(), "ignore-previous-rules", "exceptions must come last");
        }
        // Scriptlets are never emitted (they need script injection). `:has()`
        // is native CSS that WebKit supports, so it may be kept as a
        // css-display-none selector.
        assert!(!c.lists[0].contains("nobab"));
        assert!(!c.lists[0].contains("+js"));
    }

    #[test]
    fn hiding_is_not_cancelled_by_the_first_party_document_exception() {
        // WebKit takes a page's hiding rules from its document load; the
        // converter's exception for first-party documents (meant for the
        // network rules) cancelled them all when it followed them.
        let fp = serde_json::to_value(adblock::content_blocking::ignore_previous_fp_documents()).unwrap();
        let c = compile(&lists(), &BTreeSet::new()).unwrap();
        assert!(parsed(&c, 0).contains(&fp), "pages are still never blocked as documents");
        assert!(!parsed(&c, 1).contains(&fp), "the hiding list keeps its rules");
        // The other exceptions still apply to hiding (e.g. $document).
        let text = format!("{SAMPLE}@@||trusted.example^$document\n");
        let c = compile(&[FilterList { name: "doc", text: &text }], &BTreeSet::new()).unwrap();
        assert!(c.lists[1].contains("trusted"), "a $document exception turns hiding off too");
    }

    #[test]
    fn generic_hiding_applies_to_the_page_not_its_frames() {
        let text = format!("{SAMPLE}##.sponsor-box\n");
        let c = compile(&[FilterList { name: "generic", text: &text }], &BTreeSet::new()).unwrap();
        let hiding = parsed(&c, 1);
        let generic = hiding.iter().find(|r| r["action"]["selector"] == ".sponsor-box").unwrap();
        assert_eq!(generic["trigger"]["load-context"], json!(["top-frame"]));
        let site = hiding.iter().find(|r| r["action"]["selector"] == ".ad-slot").unwrap();
        assert!(site["trigger"]["if-domain"].is_array() && site["trigger"].get("load-context").is_none());
        assert!(parsed(&c, 0).iter().all(|r| r["trigger"].get("load-context").is_none()), "blocking is unchanged");
    }

    #[test]
    fn allowlisted_sites_add_a_final_exception() {
        let mut allow = BTreeSet::new();
        allow.insert("news.example".to_string());
        allow.insert("bad\"]}regex(".to_string()); // rejected by normalise_site
        let c = compile(&lists(), &allow).unwrap();
        for i in 0..c.lists.len() {
            let rules = parsed(&c, i);
            let last = rules.last().unwrap();
            assert_eq!(last["action"]["type"], "ignore-previous-rules");
            assert_eq!(last["trigger"]["if-top-url"], json!(["^[^:]+://+([^:/]+\\.)?news\\.example[:/]"]));
            assert!(!c.lists[i].contains("regex("));
        }
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
