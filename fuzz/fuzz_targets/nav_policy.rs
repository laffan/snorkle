//! Fuzz: navigation policy invariants.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::policy::{decide, Decision, Frame, HttpExceptions, Request, Source, INTERNAL_HOSTS, INTERNAL_SCHEMES};
use snorkle_core::Url;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let Ok(s) = std::str::from_utf8(rest) else { return };
    let Ok(url) = Url::parse(s) else { return };
    let frame = if sel & 1 == 0 { Frame::Main } else { Frame::Sub };
    let source = match (sel >> 1) % 5 {
        0 => Source::Typed,
        1 => Source::Page { user_gesture: true },
        2 => Source::Page { user_gesture: false },
        3 => Source::Redirect,
        _ => Source::History,
    };
    let initiator = Url::parse("https://initiator.example/").unwrap();
    let req = Request { url: &url, frame, source, initiator: Some(&initiator) };
    let d = decide(&req, &HttpExceptions::default());
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("").trim_end_matches('.');

    if INTERNAL_SCHEMES.contains(&scheme) || INTERNAL_HOSTS.contains(&host) || scheme == "file" {
        assert!(matches!(d, Decision::Block(_)), "{url} -> {d:?}");
    }
    if scheme == "data" && frame == Frame::Main {
        assert!(matches!(d, Decision::Block(_)));
    }
    if scheme == "javascript" && !matches!(source, Source::Page { .. }) {
        assert!(matches!(d, Decision::Block(_)));
    }
    if scheme == "http" && frame == Frame::Main {
        // No silent HTTP in the main frame without an exception.
        assert!(!matches!(d, Decision::Allow), "{url} allowed over http");
    }
    match &d {
        Decision::UpgradeToHttps(u) => {
            assert_eq!(u.scheme(), "https");
            assert_eq!(u.host_str(), url.host_str());
            assert_eq!(u.path(), url.path());
            assert_eq!(u.query(), url.query());
        }
        Decision::External(u) => {
            assert_eq!(frame, Frame::Main);
            assert!(matches!(source, Source::Typed | Source::Page { user_gesture: true }));
            assert!(!matches!(u.scheme(), "http" | "https" | "about" | "data" | "blob" | "javascript" | "file"));
        }
        _ => {}
    }
});
