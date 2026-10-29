//! Fuzz: address-bar display of arbitrary URLs.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::url_display::display;
use snorkle_core::Url;

fn hidden_or_bidi(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{00AD}' | '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let Ok(url) = Url::parse(s) else { return };
    let d = display(&url);
    for part in [&d.subdomain, &d.registrable, &d.full] {
        assert!(!part.chars().any(hidden_or_bidi), "hidden/bidi char shown: {part:?} for {url}");
    }
    if matches!(url.scheme(), "http" | "https") {
        if let Some(host) = url.host_str() {
            assert!(!d.registrable.is_empty(), "empty registrable for {url}");
            // What we show must parse back to the same host: the user can
            // trust the displayed text to identify the origin.
            if let Ok(back) = Url::parse(&d.full) {
                assert_eq!(back.host_str(), Some(host), "display {:?} does not round-trip for {url}", d.full);
                // Userinfo is never shown.
                assert!(back.username().is_empty() && back.password().is_none(), "userinfo shown for {url}");
            } else {
                panic!("display {:?} does not parse for {url}", d.full);
            }
            // The non-punycode form must be accepted by the IDN policy.
            if !d.punycode_forced && !d.registrable.is_ascii() {
                let tld = host.trim_end_matches('.').rsplit('.').next().unwrap_or("");
                let shown = if d.subdomain.is_empty() { d.registrable.clone() } else { format!("{}.{}", d.subdomain, d.registrable) };
                assert!(snorkle_core::idn::host_is_safe_for_unicode(&shown, tld), "unsafe unicode shown {shown:?}");
            }
        }
    }
});
