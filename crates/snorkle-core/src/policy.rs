//! SECURITY-REVIEW: navigation policy. Every navigation in a content web view
//! (typed, link, redirect, iframe, popup) is decided here, synchronously,
//! from `WKNavigationDelegate.decidePolicyForNavigationAction`.
//!
//! Security assumptions:
//! - The caller reports `frame` and `source` truthfully from WebKit's
//!   `WKNavigationAction` (`targetFrame.isMainFrame`, `navigationType`); the
//!   iOS glue does not let page script influence these values.
//! - WebKit enforces the same-origin policy, mixed-content blocking and
//!   certificate validation; this policy is an additional layer and never
//!   relaxes WebKit's defaults.
//! - The function is pure and cheap (no I/O, no allocation on the common
//!   path beyond the upgraded URL) because it runs on the main thread for
//!   every navigation.

use std::collections::HashSet;

use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    Main,
    Sub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Initiated by the user through trusted chrome: address bar, bookmark,
    /// history entry, "proceed over HTTP" button.
    Typed,
    /// Initiated by page content (link click, form, script). `user_gesture`
    /// is WebKit's report of whether a user activation was involved, if known.
    Page { user_gesture: bool },
    /// Server-side redirect of an earlier navigation.
    Redirect,
    /// Back/forward list traversal or reload of an existing entry.
    History,
}

#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub url: &'a Url,
    pub frame: Frame,
    pub source: Source,
    /// URL of the document that initiated the navigation, when known.
    pub initiator: Option<&'a Url>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Cancel this navigation and load the given `https:` URL instead.
    UpgradeToHttps(Url),
    /// Hand to the chrome, which asks the user before opening another app.
    External(Url),
    Block(BlockReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    /// Schemes/hosts used by the app itself (`tauri:`, `ipc:` …).
    InternalScheme,
    File,
    /// Top-level `data:` documents (phishing vector; blocked by Chrome and
    /// Firefox as well).
    TopLevelData,
    /// `javascript:` typed or pasted into the address bar (self-XSS).
    TypedJavascript,
    /// `blob:` top-level navigation not created by the initiating origin.
    ForeignBlob,
    /// Page-initiated navigation to a URL containing credentials.
    Credentials,
    /// External app scheme without a user gesture (or from a subframe).
    ExternalWithoutGesture,
    UnsupportedAbout,
}

/// Hosts the user explicitly allowed over plain HTTP after the HTTPS-first
/// interstitial. In memory only, per profile, never persisted.
#[derive(Clone, Debug, Default)]
pub struct HttpExceptions {
    hosts: HashSet<String>,
}

impl HttpExceptions {
    pub fn allow(&mut self, host: &str) {
        self.hosts.insert(host.to_ascii_lowercase());
    }
    pub fn contains(&self, host: &str) -> bool {
        self.hosts.contains(host)
    }
    pub fn clear(&mut self) {
        self.hosts.clear();
    }
}

/// Schemes registered by Tauri/the app for the trusted chrome. Content web
/// views never register handlers for these, but navigation to them is
/// rejected explicitly as defence in depth.
pub const INTERNAL_SCHEMES: &[&str] = &["tauri", "ipc", "asset", "snorkle", "x-snorkle"];
/// Hostnames Tauri uses for local content/IPC on some platforms.
pub const INTERNAL_HOSTS: &[&str] = &["tauri.localhost", "ipc.localhost", "asset.localhost"];

pub fn decide(req: &Request<'_>, http_exceptions: &HttpExceptions) -> Decision {
    let url = req.url;
    let scheme = url.scheme();

    if INTERNAL_SCHEMES.contains(&scheme) {
        return Decision::Block(BlockReason::InternalScheme);
    }
    if let Some(host) = url.host_str() {
        if INTERNAL_HOSTS.contains(&host.trim_end_matches('.')) {
            return Decision::Block(BlockReason::InternalScheme);
        }
    }

    match scheme {
        "https" | "http" => {
            let has_credentials = !url.username().is_empty() || url.password().is_some();
            if has_credentials && !matches!(req.source, Source::Typed) {
                return Decision::Block(BlockReason::Credentials);
            }
            if scheme == "http" && req.frame == Frame::Main {
                let host = url.host_str().unwrap_or("");
                if !http_exceptions.contains(host) {
                    return match https_version(url) {
                        Some(u) => Decision::UpgradeToHttps(u),
                        // Unreachable for valid http URLs; fail closed.
                        None => Decision::Block(BlockReason::InternalScheme),
                    };
                }
            }
            Decision::Allow
        }
        "about" => {
            if matches!(url.path(), "blank" | "srcdoc") {
                Decision::Allow
            } else {
                Decision::Block(BlockReason::UnsupportedAbout)
            }
        }
        "file" => Decision::Block(BlockReason::File),
        "data" => match req.frame {
            Frame::Main => Decision::Block(BlockReason::TopLevelData),
            Frame::Sub => Decision::Allow,
        },
        "javascript" => match req.source {
            // Executes in the page's own context: no privilege change.
            Source::Page { .. } => Decision::Allow,
            _ => Decision::Block(BlockReason::TypedJavascript),
        },
        "blob" => match req.frame {
            Frame::Sub => Decision::Allow,
            Frame::Main => {
                let blob_origin = Url::parse(url.path()).ok().map(|u| u.origin());
                let initiator_origin = req.initiator.map(|u| u.origin());
                match (blob_origin, initiator_origin, req.source) {
                    (Some(b), Some(i), Source::Page { .. }) if b.is_tuple() && b == i => Decision::Allow,
                    _ => Decision::Block(BlockReason::ForeignBlob),
                }
            }
        },
        // ws/wss are not navigations; anything else is an external app scheme.
        _ => match (req.frame, req.source) {
            (Frame::Main, Source::Typed) | (Frame::Main, Source::Page { user_gesture: true }) => {
                Decision::External(url.clone())
            }
            _ => Decision::Block(BlockReason::ExternalWithoutGesture),
        },
    }
}

/// `http://host[:80]/…` → `https://host/…`. Explicit non-default ports are
/// kept (the fallback interstitial handles hosts that do not speak TLS).
pub fn https_version(url: &Url) -> Option<Url> {
    let mut u = url.clone();
    let port = u.port();
    u.set_scheme("https").ok()?;
    if port == Some(80) || port.is_none() {
        u.set_port(None).ok()?;
    }
    Some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(url: &str, frame: Frame, source: Source) -> Decision {
        let u = Url::parse(url).unwrap();
        decide(&Request { url: &u, frame, source, initiator: None }, &HttpExceptions::default())
    }
    const LINK: Source = Source::Page { user_gesture: true };
    const SCRIPT: Source = Source::Page { user_gesture: false };

    #[test]
    fn https_allowed() {
        assert_eq!(dec("https://example.com/", Frame::Main, LINK), Decision::Allow);
    }

    #[test]
    fn http_main_frame_upgraded() {
        assert_eq!(
            dec("http://example.com:80/a?b", Frame::Main, Source::Typed),
            Decision::UpgradeToHttps(Url::parse("https://example.com/a?b").unwrap())
        );
        assert_eq!(
            dec("http://example.com:8080/", Frame::Main, Source::Redirect),
            Decision::UpgradeToHttps(Url::parse("https://example.com:8080/").unwrap())
        );
    }

    #[test]
    fn http_exception_respected() {
        let u = Url::parse("http://legacy.example/").unwrap();
        let mut ex = HttpExceptions::default();
        ex.allow("legacy.example");
        let r = Request { url: &u, frame: Frame::Main, source: LINK, initiator: None };
        assert_eq!(decide(&r, &ex), Decision::Allow);
    }

    #[test]
    fn internal_schemes_blocked_everywhere() {
        for url in [
            "tauri://localhost/",
            "ipc://localhost/x",
            "asset://localhost/a",
            "https://tauri.localhost/",
            "http://ipc.localhost/",
        ] {
            for frame in [Frame::Main, Frame::Sub] {
                for source in [Source::Typed, LINK, SCRIPT, Source::Redirect, Source::History] {
                    assert_eq!(dec(url, frame, source), Decision::Block(BlockReason::InternalScheme), "{url}");
                }
            }
        }
    }

    #[test]
    fn dangerous_typed_schemes_blocked() {
        assert_eq!(
            dec("javascript:alert(1)", Frame::Main, Source::Typed),
            Decision::Block(BlockReason::TypedJavascript)
        );
        assert_eq!(dec("data:text/html,x", Frame::Main, Source::Typed), Decision::Block(BlockReason::TopLevelData));
        assert_eq!(dec("data:text/html,x", Frame::Main, LINK), Decision::Block(BlockReason::TopLevelData));
        assert_eq!(dec("file:///etc/passwd", Frame::Main, Source::Typed), Decision::Block(BlockReason::File));
        assert_eq!(dec("file:///etc/passwd", Frame::Sub, LINK), Decision::Block(BlockReason::File));
    }

    #[test]
    fn subframe_data_allowed() {
        assert_eq!(dec("data:text/html,x", Frame::Sub, SCRIPT), Decision::Allow);
    }

    #[test]
    fn credentials_blocked_from_pages() {
        assert_eq!(dec("https://bank.com@evil.example/", Frame::Main, LINK), Decision::Block(BlockReason::Credentials));
        assert_eq!(dec("https://u:p@evil.example/", Frame::Sub, SCRIPT), Decision::Block(BlockReason::Credentials));
    }

    #[test]
    fn external_schemes_need_gesture() {
        assert!(matches!(dec("mailto:a@b.c", Frame::Main, LINK), Decision::External(_)));
        assert_eq!(dec("mailto:a@b.c", Frame::Main, SCRIPT), Decision::Block(BlockReason::ExternalWithoutGesture));
        assert_eq!(dec("tel:123", Frame::Sub, LINK), Decision::Block(BlockReason::ExternalWithoutGesture));
        assert_eq!(
            dec("itms-apps://x", Frame::Main, Source::Redirect),
            Decision::Block(BlockReason::ExternalWithoutGesture)
        );
    }

    #[test]
    fn blob_same_origin_only() {
        let blob = Url::parse("blob:https://a.example/123").unwrap();
        let same = Url::parse("https://a.example/page").unwrap();
        let other = Url::parse("https://b.example/page").unwrap();
        let ex = HttpExceptions::default();
        let r = |init| Request { url: &blob, frame: Frame::Main, source: LINK, initiator: Some(init) };
        assert_eq!(decide(&r(&same), &ex), Decision::Allow);
        assert_eq!(decide(&r(&other), &ex), Decision::Block(BlockReason::ForeignBlob));
        let typed = Request { url: &blob, frame: Frame::Main, source: Source::Typed, initiator: None };
        assert_eq!(decide(&typed, &ex), Decision::Block(BlockReason::ForeignBlob));
    }

    #[test]
    fn about() {
        assert_eq!(dec("about:blank", Frame::Main, Source::Typed), Decision::Allow);
        assert_eq!(dec("about:config", Frame::Main, Source::Typed), Decision::Block(BlockReason::UnsupportedAbout));
    }
}
