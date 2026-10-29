//! Content blocking.
//!
//! WKWebView offers no request interception for http(s) subresources, so
//! enforcement inside pages is done by WebKit's compiled content rule lists
//! (`WKContentRuleListStore`), which run in WebKit's own processes without
//! a round trip to the app. Rust owns everything before that: parsing and
//! validating filter lists, de-duplication, and generating the rule-list
//! JSON (off the main thread). WebKit compiles the JSON asynchronously.
//!
//! Supported list syntax (deliberately small):
//! - `example.com` (one domain per line)
//! - hosts-file lines: `0.0.0.0 example.com`, `127.0.0.1 example.com`
//! - Adblock-style domain anchors: `||example.com^`
//! - comments starting with `#` or `!`
//!
//! Everything else is counted as skipped, never guessed at.

use std::collections::BTreeSet;

use serde_json::json;

/// WebKit's documented per-list limit is 150,000 rules; stay well below.
pub const MAX_RULES: usize = 100_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blocklist {
    domains: BTreeSet<String>,
    pub skipped_lines: usize,
}

impl Blocklist {
    pub fn parse(text: &str) -> Self {
        let mut list = Blocklist::default();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let candidate = if let Some(rest) = line.strip_prefix("||") {
                rest.strip_suffix('^').unwrap_or(rest)
            } else {
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (Some("0.0.0.0" | "127.0.0.1" | "::"), Some(host), None) => host,
                    (Some(host), None, None) => host,
                    _ => {
                        list.skipped_lines += 1;
                        continue;
                    }
                }
            };
            match normalise_domain(candidate) {
                Some(d) if list.domains.len() < MAX_RULES => {
                    list.domains.insert(d);
                }
                _ => list.skipped_lines += 1,
            }
        }
        list
    }

    pub fn len(&self) -> usize {
        self.domains.len()
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }

    /// True if `host` or any parent domain is listed.
    pub fn is_blocked(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        let mut rest = host;
        loop {
            if self.domains.contains(rest) {
                return true;
            }
            match rest.find('.') {
                Some(i) => rest = &rest[i + 1..],
                None => return false,
            }
        }
    }

    /// WebKit content-rule-list JSON blocking third-party loads from listed
    /// domains and their subdomains.
    pub fn to_webkit_rules_json(&self) -> String {
        let rules: Vec<_> = self
            .domains
            .iter()
            .map(|d| {
                json!({
                    "trigger": {
                        "url-filter": format!("^[^:]+://+([^:/]+\\.)?{}[:/]", regex_escape(d)),
                        "load-type": ["third-party"],
                    },
                    "action": { "type": "block" },
                })
            })
            .collect();
        serde_json::Value::Array(rules).to_string()
    }
}

/// Lowercase ASCII (punycode) domain with at least one dot, or None.
fn normalise_domain(s: &str) -> Option<String> {
    let s = s.trim_end_matches('.');
    if s.is_empty() || s.len() > 253 || !s.contains('.') {
        return None;
    }
    let ascii = idna::domain_to_ascii(s).ok()?;
    let valid = ascii.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    valid.then_some(ascii)
}

fn regex_escape(domain: &str) -> String {
    // Domains are validated to [a-z0-9-.], so only '.' needs escaping.
    domain.replace('.', "\\.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_formats() {
        let l = Blocklist::parse(
            "# comment\n! abp comment\nads.example\n0.0.0.0 tracker.example\n||cdn.ads.test^\n\nbad line here\n||x$third-party\nlocalhost\n",
        );
        assert_eq!(l.len(), 3);
        assert_eq!(l.skipped_lines, 3);
        assert!(l.is_blocked("ads.example"));
        assert!(l.is_blocked("a.b.tracker.example"));
        assert!(!l.is_blocked("example"));
        assert!(!l.is_blocked("notads.example"));
    }

    #[test]
    fn rules_json_is_valid_and_escaped() {
        let l = Blocklist::parse("ads.example\n");
        let v: serde_json::Value = serde_json::from_str(&l.to_webkit_rules_json()).unwrap();
        assert_eq!(v[0]["trigger"]["url-filter"], "^[^:]+://+([^:/]+\\.)?ads\\.example[:/]");
        assert_eq!(v[0]["action"]["type"], "block");
    }

    #[test]
    fn rejects_injection_attempts() {
        let l = Blocklist::parse("evil.example\"]}\n.*\n||(a|b).example^\n");
        assert!(l.is_empty());
    }
}
