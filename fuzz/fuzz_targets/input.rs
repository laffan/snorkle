//! Fuzz: address-bar input interpretation.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::input::{interpret, Interpretation, DEFAULT_SEARCH};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    match interpret(s, &DEFAULT_SEARCH) {
        Ok(Interpretation::Search(u)) => {
            // The query can never escape into the URL's authority or path.
            assert_eq!(u.host_str(), Some("duckduckgo.com"));
            assert_eq!(u.path(), "/");
            assert_eq!(u.query_pairs().count(), 1, "{u}");
        }
        Ok(Interpretation::Url(u)) => {
            assert_eq!(u.scheme(), u.scheme().to_ascii_lowercase());
            assert!(!u.as_str().chars().any(|c| c.is_control()));
        }
        Err(_) => {}
    }
});
