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

/// Checks the size of an app-drawn bitmap (as [`has_detail`]).
fn bitmap_ok(pixels: &[u8], width: usize, height: usize, stride: usize) -> bool {
    width > 0
        && height > 0
        && width <= 512
        && height <= 512
        && stride >= width * 4
        && stride.checked_mul(height).is_some_and(|n| n <= pixels.len())
}

/// A pixel's colour channels without the alpha premultiplied in, 0–1, and
/// its alpha. The platforms draw premultiplied 8-bit pixels with alpha in
/// the fourth byte (RGBA on macOS, BGRA on iPadOS); everything here treats
/// the first and third channels alike, so either order works.
fn unpremultiply(p: &[u8]) -> ([f64; 3], f64) {
    let a = f64::from(p[3]) / 255.0;
    if a <= 0.0 {
        return ([0.0; 3], 0.0);
    }
    let c = |v: u8| (f64::from(v) / 255.0 / a).min(1.0);
    ([c(p[0]), c(p[1]), c(p[2])], a)
}

/// Brightness of a colour, the first and third channels weighted alike
/// (red and blue are close in luminance, and the order is not known).
fn brightness(c: [f64; 3]) -> f64 {
    0.1424 * c[0] + 0.7152 * c[1] + 0.1424 * c[2]
}

/// Whether an icon the app drew (2026-11-24) is too dark to see on the
/// dark sidebar: what is visible of it is dark on average (brightness
/// below 0.3) and almost none of it is light (under 10% above 0.55). A
/// dark tile with a light mark on it is visible as it is; a black glyph
/// on transparency is not.
pub fn needs_dark_variant(pixels: &[u8], width: usize, height: usize, stride: usize) -> bool {
    if !bitmap_ok(pixels, width, height, stride) {
        return false;
    }
    let (mut weight, mut sum, mut light) = (0.0, 0.0, 0.0);
    for y in 0..height {
        for p in pixels[y * stride..y * stride + width * 4].chunks_exact(4) {
            let (c, a) = unpremultiply(p);
            if a < 0.1 {
                continue;
            }
            let b = brightness(c);
            weight += a;
            sum += a * b;
            if b > 0.55 {
                light += a;
            }
        }
    }
    weight > 0.0 && sum / weight < 0.3 && light / weight < 0.1
}

/// The icon's copy for the dark appearance, drawn in place: each pixel's
/// lightness turned around (HSL: `l` becomes `1 − l`) with its hue,
/// saturation and alpha kept, so black becomes white, dark blue light
/// blue, and mid tones (a pure red) stay as they are.
pub fn make_dark_variant(pixels: &mut [u8], width: usize, height: usize, stride: usize) {
    if !bitmap_ok(pixels, width, height, stride) {
        return;
    }
    for y in 0..height {
        for p in pixels[y * stride..y * stride + width * 4].chunks_exact_mut(4) {
            let (c, a) = unpremultiply(p);
            if a <= 0.0 {
                continue;
            }
            let out = invert_lightness(c);
            for i in 0..3 {
                p[i] = (out[i] * a * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn invert_lightness(c: [f64; 3]) -> [f64; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-9 {
        return [1.0 - l; 3];
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    } / 6.0;
    let l = 1.0 - l;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4×4 bitmap of one premultiplied colour (alpha last), stride 16.
    fn bitmap(rgba: [u8; 4]) -> Vec<u8> {
        let a = f64::from(rgba[3]) / 255.0;
        let pre = |v: u8| (f64::from(v) * a).round() as u8;
        [pre(rgba[0]), pre(rgba[1]), pre(rgba[2]), rgba[3]].repeat(16)
    }

    #[test]
    fn dark_icons_get_a_light_copy() {
        // A black glyph on transparency: inverted to white, alpha kept.
        let mut px = bitmap([0, 0, 0, 255]);
        px[60..64].copy_from_slice(&[0, 0, 0, 0]);
        assert!(needs_dark_variant(&px, 4, 4, 16));
        make_dark_variant(&mut px, 4, 4, 16);
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        assert_eq!(&px[60..64], &[0, 0, 0, 0], "transparent pixels stay transparent");
        // Half-transparent dark grey: still premultiplied after.
        let mut px = bitmap([40, 40, 40, 128]);
        assert!(needs_dark_variant(&px, 4, 4, 16));
        make_dark_variant(&mut px, 4, 4, 16);
        assert_eq!(px[3], 128);
        assert!(px[0] > 100 && px[0] <= 128, "light, premultiplied: {}", px[0]);
        // Dark blue keeps its hue: blue stays the strongest channel.
        let mut px = bitmap([10, 20, 90, 255]);
        assert!(needs_dark_variant(&px, 4, 4, 16));
        make_dark_variant(&mut px, 4, 4, 16);
        assert!(px[2] > px[0] && px[2] > px[1] && px[1] > 150, "{:?}", &px[0..4]);
    }

    #[test]
    fn visible_icons_are_left_alone() {
        assert!(!needs_dark_variant(&bitmap([255, 255, 255, 255]), 4, 4, 16), "white");
        assert!(!needs_dark_variant(&bitmap([60, 200, 90, 255]), 4, 4, 16), "a bright green");
        // A dark tile with a light mark on a quarter of it.
        let mut px = bitmap([0, 0, 0, 255]);
        for i in 0..4 {
            px[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        assert!(!needs_dark_variant(&px, 4, 4, 16));
        assert!(!needs_dark_variant(&bitmap([0, 0, 0, 0]), 4, 4, 16), "nothing visible");
        assert!(!needs_dark_variant(&[0; 8], 4, 4, 16), "a bitmap too small for its size");
        // A pure red is a mid tone: unchanged by the copy.
        let mut px = bitmap([255, 0, 0, 255]);
        make_dark_variant(&mut px, 4, 4, 16);
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
    }

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
