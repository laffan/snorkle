//! Declared icon URLs must remain HTTPS, credential-free, bounded and inert in HTML.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::{favicon, Url};
fuzz_target!(|data: &[u8]| {
    let Ok(html) = std::str::from_utf8(data) else { return };
    if let Some(icon) = favicon::discover("https://example.org/path/", html) {
        let url = Url::parse(&icon).unwrap();
        assert_eq!(url.scheme(), "https");
        assert!(url.username().is_empty() && url.password().is_none());
        assert!(icon.len() <= 4096);
        let doc = favicon::document(&icon, 64);
        assert_eq!(doc.matches("<img ").count(), 1);
        assert!(!doc.contains("<script"));
    }
});
