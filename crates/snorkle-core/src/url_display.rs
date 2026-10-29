//! SECURITY-REVIEW: how a URL is presented in the address bar.
//!
//! The address bar is the user's primary security indicator, so this module
//! decides exactly what is shown:
//! - the registrable domain (eTLD+1, Public Suffix List incl. private
//!   suffixes such as `github.io`) is returned separately so the chrome can
//!   emphasise it and elide *subdomains* first, never the registrable part;
//! - IDN hosts are shown in Unicode only when [`crate::idn`] says so,
//!   otherwise as punycode;
//! - userinfo (`https://bank.com@evil.example`) is never displayed;
//! - path/query are never shown in the collapsed (non-editing) view.
//!
//! Security assumptions:
//! - `Url` values come from the `url` crate (WHATWG parsing), so hosts are
//!   already lowercase, IDNA-processed ASCII.
//! - The Public Suffix List compiled into `psl` may be stale; an unknown
//!   suffix falls back to "last label is the suffix" (psl's `*` rule).
//! - This module only describes; the chrome UI must render the parts with
//!   `textContent` and with each label in its own bidi isolate.

use serde::Serialize;
use url::{Host, Url};

/// Security state of the connection, as far as the URL alone tells us. The
/// final indicator also needs WebKit's `hasOnlySecureContent` (see
/// `tabs::Tab::secure_content`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Https,
    Http,
    /// about:blank and similar local documents.
    Local,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DisplayUrl {
    pub transport: Transport,
    /// Subdomain labels before the registrable domain, display form, without
    /// the trailing dot. Empty when there is none.
    pub subdomain: String,
    /// Registrable domain (eTLD+1) in display form, or the whole host when
    /// there is no registrable domain (IP address, `localhost`, bare suffix).
    pub registrable: String,
    /// Non-default port, if any.
    pub port: Option<u16>,
    /// True when the host had IDN labels that the policy refused to show in
    /// Unicode, so the chrome shows punycode and may add a warning.
    pub punycode_forced: bool,
    /// True when the URL carried a username/password (never displayed).
    pub had_userinfo: bool,
    /// Full URL for the editing view: userinfo removed, host in display form,
    /// path/query/fragment as the (percent-encoded) serialisation.
    pub full: String,
}

/// Builds the display description of `url`.
pub fn display(url: &Url) -> DisplayUrl {
    // blob: URLs are shown as their origin (the creating page), which is what
    // determines who controls the content.
    if url.scheme() == "blob" {
        if let Ok(inner) = Url::parse(url.path()) {
            if matches!(inner.scheme(), "http" | "https") {
                let mut d = display(&inner);
                d.full = format!("blob:{}", d.full);
                return d;
            }
        }
        return opaque(url, Transport::Other);
    }

    let transport = match url.scheme() {
        "https" => Transport::Https,
        "http" => Transport::Http,
        "about" => return opaque(url, Transport::Local),
        _ => Transport::Other,
    };
    let had_userinfo = !url.username().is_empty() || url.password().is_some();

    let (subdomain, registrable, punycode_forced, display_host) = match url.host() {
        Some(Host::Domain(ascii)) => split_domain(ascii),
        Some(Host::Ipv4(ip)) => (String::new(), ip.to_string(), false, ip.to_string()),
        Some(Host::Ipv6(ip)) => {
            let s = format!("[{ip}]");
            (String::new(), s.clone(), false, s)
        }
        None => return opaque(url, transport),
    };

    let port = url.port();
    let mut full = String::with_capacity(url.as_str().len());
    full.push_str(url.scheme());
    full.push_str("://");
    full.push_str(&display_host);
    if let Some(p) = port {
        full.push(':');
        full.push_str(&p.to_string());
    }
    full.push_str(url.path());
    if let Some(q) = url.query() {
        full.push('?');
        full.push_str(q);
    }
    if let Some(f) = url.fragment() {
        full.push('#');
        full.push_str(f);
    }

    DisplayUrl { transport, subdomain, registrable, port, punycode_forced, had_userinfo, full }
}

/// Splits an ASCII domain into (subdomain, registrable, punycode_forced,
/// full display host), choosing Unicode or punycode for the whole host.
fn split_domain(ascii: &str) -> (String, String, bool, String) {
    let trimmed = ascii.strip_suffix('.').unwrap_or(ascii);
    let tld = trimmed.rsplit('.').next().unwrap_or(trimmed);

    let has_idn = trimmed.split('.').any(|l| l.starts_with("xn--"));
    let (host_for_display, punycode_forced) = if has_idn {
        let (unicode, result) = idna::domain_to_unicode(trimmed);
        if result.is_ok() && crate::idn::host_is_safe_for_unicode(&unicode, tld) {
            (unicode, false)
        } else {
            (trimmed.to_string(), true)
        }
    } else {
        (trimmed.to_string(), false)
    };

    // Registrable domain is computed on the ASCII form; label counts are
    // identical in both forms, so we cut the display form at the same label.
    let registrable_labels =
        psl::domain(trimmed.as_bytes()).map(|d| d.as_bytes().split(|b| *b == b'.').count()).unwrap_or(0);

    let labels: Vec<&str> = host_for_display.split('.').collect();
    let (sub, mut reg) = if registrable_labels == 0 || registrable_labels > labels.len() {
        // No registrable domain (localhost, bare public suffix): the whole
        // host is the identity.
        (String::new(), host_for_display.clone())
    } else {
        let cut = labels.len() - registrable_labels;
        (labels[..cut].join("."), labels[cut..].join("."))
    };
    // The identity shown must never be empty (e.g. the URL parser accepts
    // `http://./`, whose host is just the root dot): fall back to the raw
    // ASCII host.
    if reg.is_empty() {
        reg = ascii.to_string();
    }
    // Keep a trailing dot visible in the full form: `example.com.` is a
    // distinct origin from `example.com`.
    let full_host = if ascii.ends_with('.') { format!("{host_for_display}.") } else { host_for_display };
    (sub, reg, punycode_forced, full_host)
}

fn opaque(url: &Url, transport: Transport) -> DisplayUrl {
    // Opaque URLs (about:, data:, mailto: …) have no host identity. Show the
    // scheme only plus a short, sanitised prefix of the rest.
    let shown: String =
        if url.scheme() == "about" { url.as_str().chars().take(64).collect() } else { format!("{}:", url.scheme()) };
    DisplayUrl {
        transport,
        subdomain: String::new(),
        registrable: shown.clone(),
        port: None,
        punycode_forced: false,
        had_userinfo: false,
        full: shown,
    }
}

/// Origin equality for URL-change acceptance (scheme, host, port).
pub fn same_origin(a: &Url, b: &Url) -> bool {
    let (oa, ob) = (a.origin(), b.origin());
    oa.is_tuple() && oa == ob
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> DisplayUrl {
        display(&Url::parse(s).unwrap())
    }

    #[test]
    fn simple_registrable() {
        let x = d("https://www.example.com/path?q=1");
        assert_eq!(x.subdomain, "www");
        assert_eq!(x.registrable, "example.com");
        assert_eq!(x.transport, Transport::Https);
        assert_eq!(x.full, "https://www.example.com/path?q=1");
    }

    #[test]
    fn multi_label_suffix() {
        let x = d("https://login.bank.co.uk/");
        assert_eq!(x.registrable, "bank.co.uk");
        assert_eq!(x.subdomain, "login");
    }

    #[test]
    fn private_suffix_is_respected() {
        let x = d("https://victim.github.io/");
        assert_eq!(x.registrable, "victim.github.io");
        assert_eq!(x.subdomain, "");
    }

    #[test]
    fn deceptive_subdomain() {
        let x = d("https://accounts.google.com.evil.example.co.uk/login");
        assert_eq!(x.registrable, "example.co.uk");
        assert_eq!(x.subdomain, "accounts.google.com.evil");
    }

    #[test]
    fn userinfo_hidden() {
        let x = d("https://www.bank.com:pw@evil.example/");
        assert!(x.had_userinfo);
        assert_eq!(x.registrable, "evil.example");
        assert!(!x.full.contains("bank.com"));
    }
}
