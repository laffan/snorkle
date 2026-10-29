//! Security suite: address-bar spoofing.
//!
//! Each case states what the address bar must show for a URL an attacker
//! could construct. The registrable domain is what the user must be able to
//! rely on, so most assertions are about `registrable` and `punycode_forced`.

use snorkle_core::url_display::{display, DisplayUrl, Transport};
use snorkle_core::Url;

fn d(s: &str) -> DisplayUrl {
    display(&Url::parse(s).unwrap_or_else(|e| panic!("{s}: {e}")))
}

// ---- homographs ----------------------------------------------------------

#[test]
fn cyrillic_apple_is_punycode() {
    // аррӏе.com, entirely Cyrillic (classic 2017 PoC)
    let x = d("https://xn--80ak6aa92e.com/");
    assert!(x.punycode_forced);
    assert_eq!(x.registrable, "xn--80ak6aa92e.com");
}

#[test]
fn unicode_input_is_normalised_and_punycoded() {
    // Typed/linked in Unicode rather than punycode: same result.
    let x = d("https://\u{0430}\u{0440}\u{0440}\u{04CF}\u{0435}.com/");
    assert!(x.punycode_forced);
    assert!(x.registrable.starts_with("xn--"));
}

#[test]
fn mixed_script_label_is_punycode() {
    // "pаypal.com" with Cyrillic а
    let x = d("https://p\u{0430}ypal.com/");
    assert!(x.punycode_forced);
    assert!(x.registrable.is_ascii());
}

#[test]
fn greek_omicron_is_punycode() {
    let x = d("https://g\u{03BF}\u{03BF}gle.com/");
    assert!(x.punycode_forced);
}

#[test]
fn lookalike_in_subdomain_forces_whole_host_punycode() {
    let x = d("https://\u{0430}ccounts.example.com/");
    assert!(x.punycode_forced);
    assert!(x.subdomain.is_ascii() && x.registrable.is_ascii());
}

#[test]
fn legitimate_idn_shown_in_unicode() {
    for (url, reg) in [
        ("https://xn--mnchen-3ya.de/", "münchen.de"),
        ("https://www.xn--r8jz45g.jp/", "例え.jp"),
        ("https://xn--e1afmkfd.xn--p1ai/", "пример.рф"),
        ("https://xn--bcher-kva.example/", "bücher.example"),
    ] {
        let x = d(url);
        assert!(!x.punycode_forced, "{url}");
        assert_eq!(x.registrable, reg, "{url}");
    }
}

#[test]
fn cyrillic_lookalike_allowed_under_cyrillic_tld_only() {
    let ru = d("https://xn--80ak6aa92e.ru/");
    assert!(!ru.punycode_forced);
    let com = d("https://xn--80ak6aa92e.com/");
    assert!(com.punycode_forced);
}

#[test]
fn fullwidth_and_ideographic_dots_are_normalised() {
    // U+FF0E fullwidth full stop and U+3002 ideographic full stop are
    // label separators per UTS #46, so these are the real example.com.
    assert_eq!(d("https://example\u{FF0E}com/").registrable, "example.com");
    assert_eq!(d("https://example\u{3002}com/").registrable, "example.com");
}

#[test]
fn fake_slash_characters_never_shown() {
    // "google.com∕login.evil.example" with U+2215 DIVISION SLASH inside a label.
    if let Ok(u) = Url::parse("https://google.com\u{2215}login.evil.example/") {
        let x = display(&u);
        assert!(!x.full.contains('\u{2215}'));
        assert!(!x.registrable.contains("google.com"));
    }
}

#[test]
fn zero_width_chars_never_shown() {
    // UTS #46 maps U+200B away; whatever the parser does, the display
    // must not contain it.
    if let Ok(u) = Url::parse("https://exa\u{200B}mple.com/") {
        assert!(!display(&u).full.contains('\u{200B}'));
    }
}

// ---- structure-based spoofs ----------------------------------------------

#[test]
fn userinfo_is_never_displayed() {
    let x = d("https://www.paypal.com:secure@evil.example/login");
    assert!(x.had_userinfo);
    assert_eq!(x.registrable, "evil.example");
    assert!(!x.full.contains("paypal"));
    assert!(!x.subdomain.contains("paypal"));
}

#[test]
fn long_deceptive_subdomain_keeps_real_registrable_domain() {
    let x = d("https://www.bankofamerica.com.secure-login.verify.evil.co.uk/");
    assert_eq!(x.registrable, "evil.co.uk");
    assert_eq!(x.subdomain, "www.bankofamerica.com.secure-login.verify");
}

#[test]
fn private_suffixes_split_tenants() {
    assert_eq!(d("https://attacker.github.io/").registrable, "attacker.github.io");
    assert_eq!(d("https://x.attacker.blogspot.com/").registrable, "attacker.blogspot.com");
}

#[test]
fn bare_public_suffix_and_localhost() {
    assert_eq!(d("https://co.uk/").registrable, "co.uk");
    assert_eq!(d("http://localhost:8080/").registrable, "localhost");
    assert_eq!(d("http://localhost:8080/").port, Some(8080));
}

#[test]
fn degenerate_hosts_never_show_an_empty_identity() {
    // Found by fuzzing (fuzz/fuzz_targets/url_display.rs).
    let x = d("http://./.mazob");
    assert_eq!(x.registrable, ".");
    let y = d("https://../");
    assert!(!y.registrable.is_empty());
}

#[test]
fn ip_addresses() {
    let v4 = d("http://192.168.0.1/");
    assert_eq!(v4.registrable, "192.168.0.1");
    assert_eq!(v4.transport, Transport::Http);
    // Non-canonical IPv4 forms are canonicalised by the URL parser.
    assert_eq!(d("http://0xC0.0xA8.0.1/").registrable, "192.168.0.1");
    assert_eq!(d("http://3232235521/").registrable, "192.168.0.1");
    assert_eq!(d("https://[::1]/").registrable, "[::1]");
}

#[test]
fn case_and_trailing_dot() {
    let x = d("https://WWW.EXAMPLE.COM./");
    assert_eq!(x.registrable, "example.com");
    assert!(x.full.starts_with("https://www.example.com./"));
}

#[test]
fn percent_encoded_host_is_decoded_before_display() {
    assert_eq!(d("https://ex%61mple.com/").registrable, "example.com");
}

#[test]
fn path_and_query_never_in_collapsed_view() {
    let x = d("https://evil.example/https://www.bank.com/login?u=https://bank.com");
    assert_eq!(x.registrable, "evil.example");
    assert_eq!(x.subdomain, "");
    // `full` is only shown while editing.
    assert!(x.full.starts_with("https://evil.example/"));
}

#[test]
fn bidi_in_path_is_percent_encoded() {
    let x = d("https://evil.example/\u{202E}moc.knab");
    assert!(!x.full.contains('\u{202E}'));
}

#[test]
fn blob_shows_creator_origin() {
    let x = d("blob:https://evil.example/550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(x.registrable, "evil.example");
    assert!(x.full.starts_with("blob:https://evil.example/"));
}

#[test]
fn data_url_shows_only_scheme() {
    let x = d("data:text/html,<h1>https://www.bank.com</h1>");
    assert_eq!(x.registrable, "data:");
    assert!(!x.full.contains("bank"));
}

#[test]
fn about_blank() {
    let x = d("about:blank");
    assert_eq!(x.transport, Transport::Local);
    assert_eq!(x.registrable, "about:blank");
}
