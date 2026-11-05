//! SECURITY-REVIEW: bounded discovery of icon links from untrusted HTML.
//! Only credential-free HTTPS image URLs leave this parser. They are escaped
//! into a fixed, script-free image document and decoded by sandboxed WebKit.
use regex::Regex;
use url::Url;

pub fn discover(base: &str, html: &str) -> Option<String> {
    if html.len() > 1_048_576 {
        return None;
    }
    let base = Url::parse(base).ok()?;
    if base.scheme() != "https" {
        return None;
    }
    let inert = Regex::new(r"(?is)<!--.*?-->|<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>").unwrap();
    let html = inert.replace_all(html, "");
    let tags = Regex::new(r#"(?is)<link\b(?:[^'"<>]|"[^"]*"|'[^']*')*>"#).unwrap();
    let attrs = Regex::new(r#"(?is)([a-z][a-z0-9_-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap();
    let mut touch = None;
    for tag in tags.find_iter(&html).take(128) {
        let mut rel = String::new();
        let mut href = None;
        for attr in attrs.captures_iter(tag.as_str()) {
            let value = attr.get(2).or_else(|| attr.get(3)).or_else(|| attr.get(4))?.as_str();
            match attr[1].to_ascii_lowercase().as_str() {
                "rel" => rel = value.to_ascii_lowercase(),
                "href" => href = Some(value.replace("&amp;", "&")),
                _ => {}
            }
        }
        let icon = rel.split_ascii_whitespace().any(|r| r == "icon");
        let apple = rel.split_ascii_whitespace().any(|r| r == "apple-touch-icon");
        if !icon && !apple {
            continue;
        }
        let Some(href) = href else { continue };
        let Ok(mut url) = base.join(&href) else { continue };
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || crate::storage::favicon_host(url.as_str()).is_none()
        {
            continue;
        }
        url.set_fragment(None);
        if url.as_str().len() > 4096 {
            continue;
        }
        if icon {
            return Some(url.into());
        }
        touch = Some(url.into());
    }
    touch
}

pub fn document(url: &str, size: u32) -> String {
    let escaped = url.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<!doctype html><meta name=viewport content=\"width={size},initial-scale=1,user-scalable=no\"><style>html,body{{margin:0;padding:0;background:transparent;overflow:hidden}}img{{position:fixed;left:0;top:0;width:{size}px;height:{size}px;object-fit:contain}}</style><img alt=\"\" src=\"{escaped}\">")
}

/// Inspect an app-rendered 32-bit bitmap, never encoded/untrusted image bytes.
/// Blank snapshots can exceed a PNG byte-size heuristic; ignore row padding.
pub fn has_detail(pixels: &[u8], width: usize, height: usize, stride: usize) -> bool {
    if width == 0
        || height == 0
        || width > 512
        || height > 512
        || stride < width * 4
        || stride.checked_mul(height).is_none_or(|n| n > pixels.len())
    {
        return false;
    }
    let first = &pixels[..4];
    (0..height).any(|y| pixels[y * stride..y * stride + width * 4].chunks_exact(4).any(|p| p != first))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_bitmaps_are_rejected_independent_of_png_size() {
        let mut pixels = vec![255; 128 * 128 * 4];
        assert!(!has_detail(&pixels, 128, 128, 512));
        pixels[512] = 0;
        assert!(has_detail(&pixels, 128, 128, 512));
        assert!(!has_detail(&pixels[..10], 128, 128, 512));
        assert!(!has_detail(&[], 0, 128, 0));
    }
    #[test]
    fn discovers_declared_icons_and_resolves_relative_urls() {
        assert_eq!(discover("https://example.org/path/", "<link rel='apple-touch-icon' href='/apple.png'><LINK sizes='32x32' rel='shortcut icon' href='../brand.svg?v=2&amp;x=1'>").as_deref(), Some("https://example.org/brand.svg?v=2&x=1"));
        assert_eq!(
            discover("https://example.org/", "<link rel=icon href=https://cdn.example.org/icon.png>").as_deref(),
            Some("https://cdn.example.org/icon.png")
        );
    }
    #[test]
    fn rejects_active_urls_and_ignores_inert_markup() {
        for href in [
            "javascript:alert(1)",
            "data:image/svg+xml,bad",
            "http://example.org/a",
            "https://user:pass@example.org/a",
            "https://127.0.0.1/icon",
        ] {
            assert!(discover("https://example.org/", &format!("<link rel=icon href='{href}'>")).is_none());
        }
        assert!(discover(
            "https://example.org/",
            "<!-- <link rel=icon href='/bad'> --><script>let x=\"<link rel=icon href='/bad'>\";</script>"
        )
        .is_none());
        assert!(discover("https://example.org/", &"x".repeat(1_048_577)).is_none());
        assert!(!document("https://example.org/\"><script>x</script>", 64).contains("<script>"));
    }
}
