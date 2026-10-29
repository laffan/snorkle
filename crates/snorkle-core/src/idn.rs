//! SECURITY-REVIEW: IDN display policy (homograph / lookalike defence).
//!
//! Decides whether a host may be *shown* in Unicode or must be shown as
//! punycode (`xn--…`). It never changes which host is loaded, only how the
//! host is presented to the user.
//!
//! Security assumptions:
//! - The input host has already been through WHATWG URL host parsing (the
//!   `url` crate: UTS #46 mapping, NFC, IDNA validity and bidi checks). This
//!   module adds display-only restrictions on top.
//! - When in doubt we fall back to punycode. A false positive is ugly but not
//!   dangerous; a false negative is a spoofing bug.
//! - The policy is modelled on UTS #39 "Highly Restrictive" plus a
//!   whole-script-confusable check, similar in spirit to Chromium's IDN
//!   display policy, but it is NOT a port of it and has not been checked
//!   against Chromium's test corpus (see docs/OPEN_QUESTIONS.md).

use unicode_script::{Script, UnicodeScript};

/// Returns true if every label of `unicode_host` may be displayed in Unicode.
///
/// `unicode_host` is the host after `idna::domain_to_unicode`; `ascii_tld` is
/// the last label of the ASCII (punycode) form, used for the
/// whole-script-confusable exemption (e.g. Cyrillic is expected under `.ru`).
pub fn host_is_safe_for_unicode(unicode_host: &str, ascii_tld: &str) -> bool {
    let host = unicode_host.strip_suffix('.').unwrap_or(unicode_host);
    if host.is_empty() {
        return false;
    }
    host.split('.').all(|label| label_is_safe(label, ascii_tld))
}

fn label_is_safe(label: &str, ascii_tld: &str) -> bool {
    if label.is_ascii() {
        // Plain ASCII labels are displayed as-is ([a-z0-9-] after URL parsing).
        return true;
    }
    let mut scripts: Vec<Script> = Vec::with_capacity(2);
    for c in label.chars() {
        if is_forbidden_char(c) {
            return false;
        }
        match c.script() {
            // Common (digits, hyphen) carries no script information.
            Script::Common => {}
            // After NFC, leftover generic combining marks are a classic
            // spoofing tool (e.g. "i" + U+0307). Reject outright.
            Script::Inherited | Script::Unknown => return false,
            s => {
                if !scripts.contains(&s) {
                    scripts.push(s);
                }
            }
        }
    }
    if !scripts_allowed_together(&scripts) {
        return false;
    }
    !is_whole_script_latin_confusable(label, &scripts, ascii_tld)
}

/// UTS #39 "Highly Restrictive": a single script, or Latin combined with one
/// of the CJK combinations that are used together in practice.
fn scripts_allowed_together(scripts: &[Script]) -> bool {
    if scripts.len() <= 1 {
        return true;
    }
    const JAPANESE: &[Script] = &[Script::Latin, Script::Han, Script::Hiragana, Script::Katakana];
    const CHINESE: &[Script] = &[Script::Latin, Script::Han, Script::Bopomofo];
    const KOREAN: &[Script] = &[Script::Latin, Script::Han, Script::Hangul];
    [JAPANESE, CHINESE, KOREAN].iter().any(|set| scripts.iter().all(|s| set.contains(s)))
}

/// A label written entirely in Cyrillic/Greek/Armenian letters that look like
/// Latin letters (e.g. "аррӏе" in Cyrillic) is shown as punycode unless the
/// TLD is one where that script is expected.
fn is_whole_script_latin_confusable(label: &str, scripts: &[Script], ascii_tld: &str) -> bool {
    let (lookalikes, native_tlds): (&str, &[&str]) = match scripts {
        [Script::Cyrillic] => (CYRILLIC_LATIN_LOOKALIKES, CYRILLIC_TLDS),
        [Script::Greek] => (GREEK_LATIN_LOOKALIKES, GREEK_TLDS),
        [Script::Armenian] => (ARMENIAN_LATIN_LOOKALIKES, ARMENIAN_TLDS),
        _ => return false,
    };
    if native_tlds.contains(&ascii_tld) {
        return false;
    }
    label.chars().all(|c| c.is_ascii_digit() || c == '-' || lookalikes.contains(c))
}

// Lowercase letters that render (near-)identically to Latin letters in common
// system fonts. Deliberately generous: false positives only cost punycode.
const CYRILLIC_LATIN_LOOKALIKES: &str = "асԁеԍһіјӏорԛѕԝхуүъьҽпгѵѡкмнтвз";
const GREEK_LATIN_LOOKALIKES: &str = "αικνορτυχηω";
const ARMENIAN_LATIN_LOOKALIKES: &str = "ահոսօցզ";

// ASCII forms of TLDs where the script is native.
const CYRILLIC_TLDS: &[&str] = &[
    "ru",
    "su",
    "by",
    "bg",
    "kz",
    "kg",
    "mk",
    "mn",
    "rs",
    "tj",
    "ua",
    "uz",
    "xn--p1ai",
    "xn--90ais",
    "xn--90ae",
    "xn--80ao21a",
    "xn--j1amh",
    "xn--d1alf",
    "xn--90a3ac",
    "xn--l1acc",
    "xn--p1acf",
];
const GREEK_TLDS: &[&str] = &["gr", "cy", "xn--qxam"];
const ARMENIAN_TLDS: &[&str] = &["am", "xn--y9a3aq"];

/// Characters that can imitate URL syntax (slashes, dots, colons), are
/// invisible, or are otherwise never legitimate in a displayed hostname.
fn is_forbidden_char(c: char) -> bool {
    matches!(c,
        // Invisible / format / special
        '\u{00AD}' | '\u{034F}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFD}'
        // Slash, dot, colon and fraction lookalikes
        | '\u{00B7}' | '\u{01C3}' | '\u{02D0}' | '\u{0337}' | '\u{0338}' | '\u{0589}'
        | '\u{05C3}' | '\u{05F4}' | '\u{0609}' | '\u{060A}' | '\u{066A}' | '\u{06D4}'
        | '\u{0701}'..='\u{0704}' | '\u{2024}' | '\u{2027}' | '\u{2036}'..='\u{2038}'
        | '\u{2041}' | '\u{2044}' | '\u{2052}' | '\u{2215}' | '\u{2236}' | '\u{23AE}'
        | '\u{29F6}' | '\u{29F8}' | '\u{2AFB}' | '\u{2AFD}' | '\u{2FF0}'..='\u{2FFB}'
        | '\u{3014}' | '\u{3015}' | '\u{3033}' | '\u{321D}' | '\u{321E}' | '\u{33AE}'
        | '\u{33AF}' | '\u{33C6}' | '\u{33DF}' | '\u{A789}' | '\u{FE14}' | '\u{FE15}'
        | '\u{FE3F}' | '\u{FE5D}' | '\u{FE5E}' | '\u{FF0E}' | '\u{FF0F}' | '\u{FF61}'
        // Box-drawing / block elements that can draw fake UI
        | '\u{2500}'..='\u{259F}'
    ) || c.is_control()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_safe() {
        assert!(host_is_safe_for_unicode("www.example.com", "com"));
    }

    #[test]
    fn single_script_non_latin_is_safe() {
        assert!(host_is_safe_for_unicode("münchen.de", "de"));
        assert!(host_is_safe_for_unicode("例え.jp", "jp"));
        assert!(host_is_safe_for_unicode("ελληνικά.gr", "gr"));
        assert!(host_is_safe_for_unicode("пример.рф", "xn--p1ai"));
        assert!(host_is_safe_for_unicode("日本語とカタカナ.jp", "jp"));
        assert!(host_is_safe_for_unicode("한국어.kr", "kr"));
    }

    #[test]
    fn mixed_scripts_are_unsafe() {
        // Latin "pple" + Cyrillic "а"
        assert!(!host_is_safe_for_unicode("\u{0430}pple.com", "com"));
        // Latin + Greek omicron
        assert!(!host_is_safe_for_unicode("g\u{03BF}ogle.com", "com"));
        // Cyrillic + Greek
        assert!(!host_is_safe_for_unicode("\u{0430}\u{03B1}.com", "com"));
    }

    #[test]
    fn whole_script_confusables_are_unsafe_outside_native_tld() {
        // "аррӏе" all Cyrillic
        let apple = "\u{0430}\u{0440}\u{0440}\u{04CF}\u{0435}.com";
        assert!(!host_is_safe_for_unicode(apple, "com"));
        // Same label under .ru is expected Cyrillic
        assert!(host_is_safe_for_unicode("\u{0430}\u{0440}\u{0440}\u{04CF}\u{0435}.ru", "ru"));
        // Greek "οκ" lookalike
        assert!(!host_is_safe_for_unicode("\u{03BF}\u{03BA}.com", "com"));
    }

    #[test]
    fn forbidden_characters_are_unsafe() {
        assert!(!host_is_safe_for_unicode("a\u{2215}b.com", "com")); // division slash
        assert!(!host_is_safe_for_unicode("a\u{200B}b.com", "com")); // zero width space
        assert!(!host_is_safe_for_unicode("a\u{0338}.com", "com")); // combining long solidus
        assert!(!host_is_safe_for_unicode("ex\u{0307}ample.com", "com")); // combining dot
    }

    #[test]
    fn empty_is_unsafe() {
        assert!(!host_is_safe_for_unicode("", "com"));
    }
}
