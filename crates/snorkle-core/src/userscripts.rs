//! SECURITY-REVIEW: user scripts and styles ("Greasemonkey-like"), plus the
//! browser's own view modes (black and white, hidden images). Approved by
//! the owner 2026-10-29 as the first script injection into content (see
//! docs/SECURITY_REVIEW.md).
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
}

/// How pages are drawn. Session-only (not persisted): every launch starts
/// in black and white with images and video hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewMode {
    /// Show pages in colour (off: black and white).
    pub color: bool,
    /// Hide images and video before pages are drawn.
    pub hide_images: bool,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode { color: false, hide_images: true }
    }
}

/// A constant style the browser itself injects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixedStyle {
    Grayscale,
    NoImages,
}

/// Fixed style for black-and-white mode. `filter` on the root element does
/// not create a containing block for fixed-position content.
pub const GRAYSCALE_CSS: &str = "html{filter:grayscale(1)!important}";
/// Fixed style for hidden images and video: image and video elements, and
/// embedded players of the common video sites, are removed from layout,
/// and CSS background images are dropped (WebKit then does not fetch them;
/// `<img>` sources are still fetched). Inline SVG icons stay visible.
pub const NO_IMAGES_CSS: &str = concat!(
    "img,picture,input[type=image],svg image,object[type^=\"image/\"],embed[type^=\"image/\"],",
    "video,object[type^=\"video/\"],embed[type^=\"video/\"],",
    "iframe[src*=\"youtube.com/embed\"],iframe[src*=\"youtube-nocookie.com/embed\"],",
    "iframe[src*=\"player.vimeo.com\"],iframe[src*=\"dailymotion.com/embed\"],",
    "iframe[src*=\"player.twitch.tv\"],iframe[src*=\"tiktok.com/embed\"]",
    "{display:none!important}*,*::before,*::after{background-image:none!important}"
);

impl FixedStyle {
    fn id(self) -> &'static str {
        match self {
            FixedStyle::Grayscale => "__snorkle_grayscale",
            FixedStyle::NoImages => "__snorkle_no_images",
        }
    }
    fn css(self) -> &'static str {
        match self {
            FixedStyle::Grayscale => GRAYSCALE_CSS,
            FixedStyle::NoImages => NO_IMAGES_CSS,
        }
    }
}

/// Builds every injected source: the view-mode styles that are on and each
/// enabled user script.
pub fn injections(scripts: &[UserScript], view: ViewMode) -> Vec<Injection> {
    let mut out = Vec::new();
    if !view.color {
        out.push(Injection { source: fixed_style_js(FixedStyle::Grayscale, true), at: InjectAt::DocumentStart });
    }
    if view.hide_images {
        out.push(Injection { source: fixed_style_js(FixedStyle::NoImages, true), at: InjectAt::DocumentStart });
    }
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
            });
        }
        if !s.js.trim().is_empty() && s.js.len() <= MAX_JS_BYTES {
            out.push(Injection {
                source: format!("(function(){{{guard}\n{}\n}})();", s.js),
                at: InjectAt::DocumentEnd,
            });
        }
    }
    out
}

/// Brings an already-loaded page in line with `view` (adds or removes the
/// view-mode styles). Constant code; takes no page input and returns nothing.
pub fn view_mode_js(view: ViewMode) -> String {
    fixed_style_js(FixedStyle::Grayscale, !view.color) + &fixed_style_js(FixedStyle::NoImages, view.hide_images)
}

fn fixed_style_js(style: FixedStyle, on: bool) -> String {
    let id = json_str(style.id());
    if on {
        format!(
            "(function(){{if(document.getElementById({id}))return;var st=document.createElement('style');st.id={id};st.textContent={};(document.head||document.documentElement).appendChild(st);}})();",
            json_str(style.css())
        )
    } else {
        format!("(function(){{var st=document.getElementById({id});if(st)st.remove();}})();")
    }
}

fn json_str(s: &str) -> String {
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
        let inj = injections(
            &[s("example.com", "</style><script>alert(1)</script>\"});alert(2);//", "")],
            ViewMode { color: true, hide_images: false },
        );
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
            injections(&[s("example.com", "a{}", "console.log(1)")], ViewMode { color: true, hide_images: false });
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
        assert!(injections(&[off, tampered], ViewMode { color: true, hide_images: false }).is_empty());
    }

    #[test]
    fn view_modes() {
        // Default: black and white, images and video hidden.
        let inj = injections(&[], ViewMode::default());
        assert_eq!(inj.len(), 2);
        assert!(inj[0].source.contains("grayscale(1)"));
        assert!(inj[1].source.contains("video,"));
        assert_eq!(inj[0].at, InjectAt::DocumentStart);
        let inj = injections(&[], ViewMode { color: true, hide_images: true });
        assert_eq!(inj.len(), 1);
        assert!(inj[0].source.contains("background-image:none"));
        assert!(injections(&[], ViewMode { color: true, hide_images: false }).is_empty());

        let js = view_mode_js(ViewMode { color: true, hide_images: true });
        assert!(js.contains("getElementById(\"__snorkle_grayscale\");if(st)st.remove()"));
        assert!(js.contains("display:none"));
        let js = view_mode_js(ViewMode { color: false, hide_images: false });
        assert!(js.contains("grayscale(1)"));
        assert!(js.contains("getElementById(\"__snorkle_no_images\");if(st)st.remove()"));
    }

    #[test]
    fn size_limits() {
        assert_eq!(validate(s("example.com", &"a".repeat(MAX_CSS_BYTES + 1), "")), Err(UserScriptError::TooLarge));
        assert!(validate(s("example.com", "a{}", "b()")).is_ok());
    }
}
