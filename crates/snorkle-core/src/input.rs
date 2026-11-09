//! SECURITY-REVIEW: interpretation of text typed or pasted into the address
//! bar.
//!
//! This module only turns text into a candidate URL (or a search URL). It
//! does NOT decide whether loading it is allowed; every result still goes
//! through [`crate::policy::decide`] with `Source::Typed`, which is where
//! `javascript:`, `data:`, `file:` and internal schemes are rejected.
//!
//! Security assumptions:
//! - Pasted text can contain newlines/control characters (clipboard attacks);
//!   they are removed before interpretation, as other browsers do.
//! - A bare host defaults to `https://` (HTTPS-first).

use url::Url;

/// Upper bound on accepted input; longer input is rejected rather than
/// truncated so that a truncated URL is never loaded silently.
pub const MAX_INPUT_LEN: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Interpretation {
    /// Load this URL (subject to navigation policy).
    Url(Url),
    /// Run a search; the URL is the search engine's result page.
    Search(Url),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Empty,
    TooLong,
}

/// A search provider. The template must contain exactly one `{query}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEngine {
    pub name: &'static str,
    pub template: &'static str,
}

/// Default search engine. Choice of provider is an open product question
/// (docs/OPEN_QUESTIONS.md); no request is sent unless the user searches.
pub const DEFAULT_SEARCH: SearchEngine =
    SearchEngine { name: "DuckDuckGo", template: "https://duckduckgo.com/?q={query}" };

/// Schemes recognised as "the user typed a URL with a scheme". Anything else
/// followed by a colon (e.g. `foo:bar`) is treated as a search.
const KNOWN_SCHEMES: &[&str] = &[
    "http",
    "https",
    "about",
    "javascript",
    "data",
    "file",
    "blob",
    "mailto",
    "tel",
    "sms",
    "facetime",
    "facetime-audio",
    "maps",
    "itms-apps",
    "ftp",
    "ws",
    "wss",
    "tauri",
    "ipc",
    "asset",
];

pub fn interpret(raw: &str, search: &SearchEngine) -> Result<Interpretation, InputError> {
    if raw.len() > MAX_INPUT_LEN {
        return Err(InputError::TooLong);
    }
    // Drop control characters (incl. CR/LF/TAB from pastes) entirely.
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let text = cleaned.trim();
    if text.is_empty() {
        return Err(InputError::Empty);
    }

    if let Some(url) = parse_with_scheme(text) {
        return Ok(Interpretation::Url(url));
    }
    if !text.contains(char::is_whitespace) {
        if let Some(url) = parse_as_host(text) {
            return Ok(Interpretation::Url(url));
        }
    }
    Ok(Interpretation::Search(search_url(text, search)))
}

fn parse_with_scheme(text: &str) -> Option<Url> {
    let colon = text.find(':')?;
    let scheme = &text[..colon];
    if scheme.is_empty()
        || !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let lower = scheme.to_ascii_lowercase();
    let rest = &text[colon + 1..];
    // "localhost:8080" / "example.com:443/x" are host:port, not schemes.
    if !KNOWN_SCHEMES.contains(&lower.as_str()) {
        let port_like = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !port_like.is_empty() && port_like.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if !rest.starts_with("//") {
            return None;
        }
    }
    Url::parse(text).ok()
}

fn parse_as_host(text: &str) -> Option<Url> {
    let url = Url::parse(&format!("https://{text}")).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        // "user@host" without a scheme is far more likely an email address or
        // a spoofing attempt than an intended navigation.
        return None;
    }
    match url.host()? {
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => Some(url),
        url::Host::Domain(d) => {
            let d = d.strip_suffix('.').unwrap_or(d);
            if d == "localhost" {
                return Some(url);
            }
            if !d.contains('.') {
                return None;
            }
            let known = psl::suffix(d.as_bytes()).map(|s| s.is_known()).unwrap_or(false);
            known.then_some(url)
        }
    }
}

fn search_url(query: &str, engine: &SearchEngine) -> Url {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let s = engine.template.replacen("{query}", &encoded, 1);
    // The template is a compile-time constant; failure is a programming error.
    Url::parse(&s).expect("search template must be a valid URL")
}

/// The app's own URL schemes (declared in `Info.plist` and
/// `Info.ios.plist`), so other apps, Shortcuts and web pages in Safari can
/// send a link to Snorkle specifically (owner request 2026-11-09). The same
/// forms other browsers use:
/// - `snorkle-https://example.com/a` and `snorkle-http://…`: the scheme
///   swapped (as `microsoft-edge-https://`, Chrome's `googlechromes://`);
/// - `snorkle://open-url?url=<percent-encoded http(s) URL>` (as Firefox's
///   `firefox://open-url?url=`);
/// - `snorkle://example.com/a`: shorthand for `https://example.com/a`.
pub const APP_SCHEMES: &[&str] = &["snorkle", "snorkle-http", "snorkle-https"];

/// The web page a link in one of [`APP_SCHEMES`] stands for: an http(s)
/// URL with a host and no credentials, or `None`. The result still goes
/// through the navigation policy like any link from another app.
pub fn from_app_link(link: &Url) -> Option<Url> {
    if link.as_str().len() > MAX_INPUT_LEN {
        return None;
    }
    // What follows the scheme's colon, e.g. "//example.com/a?b#c".
    let rest = &link.as_str()[link.scheme().len() + 1..];
    if !rest.starts_with("//") {
        return None;
    }
    let target = match link.scheme() {
        "snorkle-https" => Url::parse(&format!("https:{rest}")).ok()?,
        "snorkle-http" => Url::parse(&format!("http:{rest}")).ok()?,
        "snorkle" if link.host_str() == Some("open-url") => {
            let (_, url) = link.query_pairs().find(|(k, _)| k == "url")?;
            Url::parse(url.trim()).ok()?
        }
        "snorkle" => Url::parse(&format!("https:{rest}")).ok()?,
        _ => return None,
    };
    let plain = matches!(target.scheme(), "http" | "https")
        && target.host_str().is_some_and(|h| !h.is_empty())
        && target.username().is_empty()
        && target.password().is_none()
        && target.as_str().len() <= MAX_INPUT_LEN;
    plain.then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(s: &str) -> Interpretation {
        interpret(s, &DEFAULT_SEARCH).unwrap()
    }
    fn url(s: &str) -> String {
        match i(s) {
            Interpretation::Url(u) => u.to_string(),
            Interpretation::Search(u) => panic!("expected url for {s}, got search {u}"),
        }
    }
    fn is_search(s: &str) -> bool {
        matches!(i(s), Interpretation::Search(_))
    }

    #[test]
    fn bare_host_defaults_to_https() {
        assert_eq!(url("example.com"), "https://example.com/");
        assert_eq!(url("example.com/a?b"), "https://example.com/a?b");
        assert_eq!(url("localhost:8080"), "https://localhost:8080/");
        assert_eq!(url("192.168.1.1"), "https://192.168.1.1/");
    }

    #[test]
    fn explicit_scheme_kept() {
        assert_eq!(url("http://example.com"), "http://example.com/");
        assert_eq!(url("HTTPS://Example.COM"), "https://example.com/");
        // Rejected later by policy, but interpreted as URLs, not searches.
        assert!(url("javascript:alert(1)").starts_with("javascript:"));
        assert!(url("data:text/html,hi").starts_with("data:"));
    }

    #[test]
    fn searches() {
        assert!(is_search("how to bake bread"));
        assert!(is_search("rust"));
        assert!(is_search("foo:bar"));
        assert!(is_search("not.a.realtld"));
        assert!(is_search("user@example.com"));
    }

    #[test]
    fn search_is_encoded() {
        match i("a&b=c #d") {
            Interpretation::Search(u) => assert_eq!(u.as_str(), "https://duckduckgo.com/?q=a%26b%3Dc+%23d"),
            _ => panic!(),
        }
    }

    #[test]
    fn control_chars_removed() {
        assert_eq!(url("exa\nmple.com\r\n"), "https://example.com/");
    }

    #[test]
    fn errors() {
        assert_eq!(interpret("   ", &DEFAULT_SEARCH), Err(InputError::Empty));
        assert_eq!(interpret(&"a".repeat(MAX_INPUT_LEN + 1), &DEFAULT_SEARCH), Err(InputError::TooLong));
    }

    #[test]
    fn app_links_stand_for_web_pages() {
        let go = |s: &str| from_app_link(&Url::parse(s).unwrap()).map(|u| u.to_string());
        assert_eq!(go("snorkle-https://Example.com/a?b=1#c").as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(go("snorkle-http://example.com/").as_deref(), Some("http://example.com/"));
        assert_eq!(
            go("snorkle://open-url?url=https%3A%2F%2Fexample.com%2Fx%3Fq%3D1%26r%3D2").as_deref(),
            Some("https://example.com/x?q=1&r=2")
        );
        assert_eq!(go("snorkle://example.com/path?x").as_deref(), Some("https://example.com/path?x"));
        for bad in [
            "snorkle://open-url?url=javascript%3Aalert(1)",
            "snorkle://open-url?url=file%3A%2F%2F%2Fetc%2Fpasswd",
            "snorkle://open-url",
            "snorkle-https://user:pw@example.com/",
            "snorkle-https:example",
            "snorkle:example.com",
            "mailto:a@example.com",
            "https://example.com/",
        ] {
            assert_eq!(go(bad), None, "{bad}");
        }
        for s in APP_SCHEMES {
            assert!(crate::policy::INTERNAL_SCHEMES.contains(s), "pages cannot navigate to {s}");
        }
    }
}
