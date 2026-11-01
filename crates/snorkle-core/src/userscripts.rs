//! SECURITY-REVIEW: user scripts and styles ("Greasemonkey-like"). Approved
//! by the owner 2026-10-29 as the first script injection into content (see
//! docs/SECURITY_REVIEW.md). The browser's own view modes are in
//! `crate::viewmodes`.
//!
//! This module validates what the user saved and generates the exact
//! JavaScript sources the content host installs as `WKUserScript`s.
//!
//! Security assumptions / design:
//! - Scripts run in WebKit's isolated client world (`defaultClientWorld`),
//!   never the page world: the page cannot read, call or tamper with them,
//!   and they get DOM access only. There is no `GM_*` API and no message
//!   handler: nothing flows from pages back to native code.
//! - WKUserScript has no URL matching, so every script is installed in
//!   every content view, wrapped in a guard that checks the document's real
//!   `location.hostname` (main frame only). A page cannot spoof its own
//!   hostname. Consequence: the (guarded) source of every saved script is
//!   present in every page's client world; a compromised WebContent process
//!   could read it. Users should not put secrets in scripts.
//! - CSS and the domain are embedded as JSON string literals, so they cannot
//!   break out of the generated code. The user's JS is embedded verbatim (it
//!   is code the user wrote to run); deliberately closing the wrapper would
//!   only defeat the user's own domain guard.

use serde::{Deserialize, Serialize};

pub const MAX_SCRIPTS: usize = 200;
pub const MAX_CSS_BYTES: usize = 100 * 1024;
pub const MAX_JS_BYTES: usize = 200 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserScript {
    /// ASCII registrable domain or host; also matches its subdomains.
    pub domain: String,
    pub css: String,
    pub js: String,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserScriptError {
    InvalidDomain,
    TooLarge,
    TooMany,
}

impl std::fmt::Display for UserScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            UserScriptError::InvalidDomain => "Enter a domain such as example.com",
            UserScriptError::TooLarge => "Script or style is too large",
            UserScriptError::TooMany => "Too many saved sites",
        })
    }
}

/// Normalises user input ("Example.com", "https://www.example.com/x",
/// "bücher.de") to an ASCII domain, or fails.
pub fn normalise_domain(input: &str) -> Result<String, UserScriptError> {
    let t = input.trim();
    let host = match url::Url::parse(t) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u.host_str().unwrap_or("").to_string(),
        _ => t.split(['/', '?', '#']).next().unwrap_or("").to_string(),
    };
    let ascii = idna::domain_to_ascii(&host).map_err(|_| UserScriptError::InvalidDomain)?;
    crate::blocking::normalise_site(&ascii).ok_or(UserScriptError::InvalidDomain)
}

/// Validates and normalises a script before it is stored.
pub fn validate(mut s: UserScript) -> Result<UserScript, UserScriptError> {
    s.domain = normalise_domain(&s.domain)?;
    if s.css.len() > MAX_CSS_BYTES || s.js.len() > MAX_JS_BYTES {
        return Err(UserScriptError::TooLarge);
    }
    Ok(s)
}

/// When an injected source runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectAt {
    DocumentStart,
    DocumentEnd,
}

/// A source to install as a main-frame-only `WKUserScript` in the default
/// client world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Injection {
    pub source: String,
    pub at: InjectAt,
    /// Also run in subframes (default: main frame only).
    pub all_frames: bool,
}

/// Builds the injected source of each enabled user script. (View modes are
/// in `crate::viewmodes`.)
pub fn injections(scripts: &[UserScript]) -> Vec<Injection> {
    let mut out = Vec::new();
    for s in scripts.iter().filter(|s| s.enabled) {
        // Re-validate: stored data is not trusted blindly.
        let Ok(domain) = normalise_domain(&s.domain) else { continue };
        let domain_lit = json_str(&domain);
        let guard = format!(
            "var h=location.hostname.toLowerCase();if(h!=={domain_lit}&&!h.endsWith('.'+{domain_lit}))return;"
        );
        if !s.css.trim().is_empty() && s.css.len() <= MAX_CSS_BYTES {
            out.push(Injection {
                source: format!(
                    "(function(){{{guard}var st=document.createElement('style');st.textContent={};(document.head||document.documentElement).appendChild(st);}})();",
                    json_str(&s.css)
                ),
                at: InjectAt::DocumentStart,
                all_frames: false,
            });
        }
        if !s.js.trim().is_empty() && s.js.len() <= MAX_JS_BYTES {
            out.push(Injection {
                source: format!("(function(){{{guard}\n{}\n}})();", s.js),
                at: InjectAt::DocumentEnd,
                all_frames: false,
            });
        }
    }
    out
}

pub(crate) fn json_str(s: &str) -> String {
    // serde_json escapes quotes, backslashes and control characters; also
    // escape '<' and U+2028/2029 so the literal is inert in any context.
    serde_json::to_string(s)
        .expect("string serialisation cannot fail")
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(domain: &str, css: &str, js: &str) -> UserScript {
        UserScript { domain: domain.into(), css: css.into(), js: js.into(), enabled: true }
    }

    #[test]
    fn domains_are_normalised() {
        assert_eq!(normalise_domain("Example.COM").unwrap(), "example.com");
        assert_eq!(normalise_domain("https://www.example.com/path").unwrap(), "www.example.com");
        assert_eq!(normalise_domain("bücher.de").unwrap(), "xn--bcher-kva.de");
        for bad in ["", "exa mple.com", "javascript:alert(1)", "a'b.com", "x\");alert(1);(\"", ".."] {
            assert!(normalise_domain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn css_cannot_break_out() {
        let inj = injections(&[s("example.com", "</style><script>alert(1)</script>\"});alert(2);//", "")]);
        assert_eq!(inj.len(), 1);
        let src = &inj[0].source;
        assert!(!src.contains("</style>"));
        // The quote is escaped inside the JSON literal, so it cannot close it.
        assert!(src.contains("\\\"});alert(2)"));
        assert_eq!(src.matches("st.textContent=\"").count(), 1);
        assert!(src.contains("location.hostname"));
    }

    #[test]
    fn guard_and_timing() {
        let inj =
            injections(&[s("example.com", "a{}", "console.log(1)")]);
        assert_eq!(inj.len(), 2);
        assert_eq!(inj[0].at, InjectAt::DocumentStart);
        assert_eq!(inj[1].at, InjectAt::DocumentEnd);
        assert!(inj[1].source.contains("h!==\"example.com\"&&!h.endsWith('.'+\"example.com\")"));
        assert!(inj[1].source.contains("console.log(1)"));
    }

    #[test]
    fn disabled_and_invalid_scripts_are_skipped() {
        let mut off = s("example.com", "a{}", "x()");
        off.enabled = false;
        let tampered = s("evil\"]);alert(1);//", "a{}", "x()");
        assert!(injections(&[off, tampered]).is_empty());
    }

    #[test]
    fn size_limits() {
        assert_eq!(validate(s("example.com", &"a".repeat(MAX_CSS_BYTES + 1), "")), Err(UserScriptError::TooLarge));
        assert!(validate(s("example.com", "a{}", "b()")).is_ok());
    }
}
