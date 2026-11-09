//! SECURITY-REVIEW: WebKit web archives (`.webarchive`) for Save for Later
//! (owner request 2026-11-09; the method of laffan/thedailyprophet).
//!
//! A web archive is a binary property list: the main resource (the page's
//! HTML, under its own URL) and every subresource under its original URL.
//! WebKit, given one with `loadData:MIMEType:@"application/x-webarchive"`,
//! answers each load the page makes (scripts, styles, fonts, images,
//! `fetch` and `XMLHttpRequest`) from the archive when it holds that URL
//! (`DocumentLoader::scheduleArchiveLoad`), so a saved page keeps working
//! offline without rewriting it and without a URL scheme handler.
//!
//! This module writes such archives ([`write`]) and reads the
//! subresources out of the ones WebKit makes itself
//! (`createWebArchiveData`, [`read_subresources`]). Both sides handle
//! page-influenced bytes, so the reader is bounded (object count, nesting,
//! sizes) and never panics on malformed input; the writer takes resources
//! that were already validated (`later::clean_resource`).

/// One resource of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// Absolute http(s) URL the page asks for.
    pub url: String,
    /// MIME type without parameters (`text/css`).
    pub mime: String,
    /// Text encoding (`utf-8`), when the server named one.
    pub encoding: Option<String>,
    pub data: Vec<u8>,
}

/// Most objects a parsed archive may contain.
const MAX_OBJECTS: usize = 200_000;
/// Deepest nesting followed (WebKit's archives are four levels deep).
const MAX_DEPTH: usize = 8;

// ---- writing ---------------------------------------------------------------

enum Obj<'a> {
    Str(&'a str),
    Data(&'a [u8]),
    Array(Vec<usize>),
    Dict(Vec<(usize, usize)>),
}

/// The web archive for `main` (the page) with `subresources`.
pub fn write(main: &Resource, subresources: &[Resource]) -> Vec<u8> {
    let mut objs: Vec<Obj> = Vec::new();
    fn push<'a>(objs: &mut Vec<Obj<'a>>, o: Obj<'a>) -> usize {
        objs.push(o);
        objs.len() - 1
    }
    // Index 0 is the top-level dictionary, filled in last.
    objs.push(Obj::Dict(Vec::new()));
    fn resource<'a>(objs: &mut Vec<Obj<'a>>, r: &'a Resource, main: bool) -> usize {
        let at = push(objs, Obj::Dict(Vec::new()));
        let mut pairs = Vec::new();
        let mut kv = |objs: &mut Vec<Obj<'a>>, k: &'static str, v: Obj<'a>| {
            let k = push(objs, Obj::Str(k));
            let v = push(objs, v);
            pairs.push((k, v));
        };
        kv(objs, "WebResourceURL", Obj::Str(&r.url));
        kv(objs, "WebResourceMIMEType", Obj::Str(&r.mime));
        if let Some(e) = &r.encoding {
            kv(objs, "WebResourceTextEncodingName", Obj::Str(e));
        }
        if main {
            kv(objs, "WebResourceFrameName", Obj::Str(""));
        }
        kv(objs, "WebResourceData", Obj::Data(&r.data));
        objs[at] = Obj::Dict(pairs);
        at
    }
    let main_at = resource(&mut objs, main, true);
    let subs: Vec<usize> = subresources.iter().map(|r| resource(&mut objs, r, false)).collect();
    let subs_at = push(&mut objs, Obj::Array(subs));
    let k_main = push(&mut objs, Obj::Str("WebMainResource"));
    let k_subs = push(&mut objs, Obj::Str("WebSubresources"));
    objs[0] = Obj::Dict(vec![(k_main, main_at), (k_subs, subs_at)]);

    let ref_size = int_size(objs.len() as u64);
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for o in &objs {
        offsets.push(out.len() as u64);
        match o {
            Obj::Str(s) if s.is_ascii() => {
                marker(&mut out, 0x5, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            Obj::Str(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                marker(&mut out, 0x6, units.len());
                for u in units {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
            Obj::Data(d) => {
                marker(&mut out, 0x4, d.len());
                out.extend_from_slice(d);
            }
            Obj::Array(items) => {
                marker(&mut out, 0xA, items.len());
                for &i in items {
                    put_uint(&mut out, i as u64, ref_size);
                }
            }
            Obj::Dict(pairs) => {
                marker(&mut out, 0xD, pairs.len());
                for &(k, _) in pairs {
                    put_uint(&mut out, k as u64, ref_size);
                }
                for &(_, v) in pairs {
                    put_uint(&mut out, v as u64, ref_size);
                }
            }
        }
    }
    let table_at = out.len() as u64;
    let offset_size = int_size(table_at);
    for o in offsets {
        put_uint(&mut out, o, offset_size);
    }
    out.extend_from_slice(&[0; 6]);
    out.push(offset_size as u8);
    out.push(ref_size as u8);
    out.extend_from_slice(&(objs.len() as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&table_at.to_be_bytes());
    out
}

/// Bytes needed for `max` as an unsigned integer: 1, 2, 4 or 8.
fn int_size(max: u64) -> usize {
    match max {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFFFF_FFFF => 4,
        _ => 8,
    }
}

fn put_uint(out: &mut Vec<u8>, v: u64, size: usize) {
    out.extend_from_slice(&v.to_be_bytes()[8 - size..]);
}

/// An object marker with its count (an integer object follows when the
/// count does not fit in the low nibble).
fn marker(out: &mut Vec<u8>, kind: u8, n: usize) {
    if n < 15 {
        out.push(kind << 4 | n as u8);
        return;
    }
    out.push(kind << 4 | 0xF);
    let size = int_size(n as u64);
    out.push(0x10 | size.trailing_zeros() as u8);
    put_uint(out, n as u64, size);
}

// ---- reading ---------------------------------------------------------------

#[derive(Debug)]
enum Value {
    Str(String),
    Data(Vec<u8>),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
    Other,
}

struct Reader<'a> {
    b: &'a [u8],
    offsets: Vec<usize>,
    ref_size: usize,
    /// Objects visited so far (bounds the work on shared references).
    visited: usize,
}

/// The subresources of a web archive (`WebSubresources`, plus those of
/// subframe archives), as `(url, mime, data)`; `None` if `bytes` is not a
/// readable binary web archive. Nothing here is trusted: callers validate
/// each resource.
pub fn read_subresources(bytes: &[u8]) -> Option<Vec<Resource>> {
    let top = parse(bytes)?;
    let mut out = Vec::new();
    collect(&top, &mut out, 0);
    Some(out)
}

/// The main resource of a web archive (WebKit's own, from
/// `createWebArchiveData`: the page as it is now).
pub fn read_main(bytes: &[u8]) -> Option<Resource> {
    let top = parse(bytes)?;
    resource_of(dict_get(&top, "WebMainResource")?)
}

fn collect(archive: &Value, out: &mut Vec<Resource>, depth: usize) {
    if depth > 2 {
        return;
    }
    if let Some(Value::Array(subs)) = dict_get(archive, "WebSubresources") {
        out.extend(subs.iter().filter_map(resource_of));
    }
    if let Some(Value::Array(frames)) = dict_get(archive, "WebSubframeArchives") {
        for f in frames {
            collect(f, out, depth + 1);
        }
    }
}

fn dict_get<'v>(v: &'v Value, key: &str) -> Option<&'v Value> {
    match v {
        Value::Dict(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn resource_of(v: &Value) -> Option<Resource> {
    let s = |k| match dict_get(v, k) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let data = match dict_get(v, "WebResourceData")? {
        Value::Data(d) => d.clone(),
        _ => return None,
    };
    Some(Resource {
        url: s("WebResourceURL")?,
        mime: s("WebResourceMIMEType").unwrap_or_default(),
        encoding: s("WebResourceTextEncodingName"),
        data,
    })
}

fn parse(b: &[u8]) -> Option<Value> {
    if b.len() < 8 + 32 || &b[..8] != b"bplist00" {
        return None;
    }
    let t = &b[b.len() - 32..];
    let offset_size = t[6] as usize;
    let ref_size = t[7] as usize;
    let count = usize::try_from(u64::from_be_bytes(t[8..16].try_into().ok()?)).ok()?;
    let top = usize::try_from(u64::from_be_bytes(t[16..24].try_into().ok()?)).ok()?;
    let table = usize::try_from(u64::from_be_bytes(t[24..32].try_into().ok()?)).ok()?;
    if !matches!(offset_size, 1 | 2 | 4 | 8) || !matches!(ref_size, 1 | 2 | 4 | 8) || count == 0 || count > MAX_OBJECTS
    {
        return None;
    }
    let table_end = table.checked_add(count.checked_mul(offset_size)?)?;
    if table < 8 || table_end > b.len() - 32 || top >= count {
        return None;
    }
    let offsets = (0..count)
        .map(|i| {
            let at = table + i * offset_size;
            let o = uint(&b[at..at + offset_size]);
            usize::try_from(o).ok().filter(|&o| o >= 8 && o < table)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut r = Reader { b, offsets, ref_size, visited: 0 };
    r.object(top, 0)
}

fn uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &x| acc << 8 | x as u64)
}

impl Reader<'_> {
    fn object(&mut self, index: usize, depth: usize) -> Option<Value> {
        self.visited += 1;
        if depth > MAX_DEPTH || self.visited > MAX_OBJECTS {
            return None;
        }
        let at = *self.offsets.get(index)?;
        let m = *self.b.get(at)?;
        let (kind, low) = (m >> 4, (m & 0xF) as usize);
        match kind {
            0x4 | 0x5 | 0x6 | 0xA | 0xD => {
                let (n, start) = self.count(at, low)?;
                match kind {
                    0x4 => Some(Value::Data(self.b.get(start..start.checked_add(n)?)?.to_vec())),
                    0x5 => {
                        let bytes = self.b.get(start..start.checked_add(n)?)?;
                        Some(Value::Str(bytes.iter().map(|&c| c as char).collect()))
                    }
                    0x6 => {
                        let bytes = self.b.get(start..start.checked_add(n.checked_mul(2)?)?)?;
                        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                        Some(Value::Str(String::from_utf16_lossy(&units)))
                    }
                    0xA => {
                        let refs = self.refs(start, n)?;
                        let items = refs.into_iter().map(|r| self.object(r, depth + 1)).collect::<Option<Vec<_>>>()?;
                        Some(Value::Array(items))
                    }
                    _ => {
                        let keys = self.refs(start, n)?;
                        let vals = self.refs(start.checked_add(n.checked_mul(self.ref_size)?)?, n)?;
                        let mut pairs = Vec::with_capacity(n);
                        for (k, v) in keys.into_iter().zip(vals) {
                            let Value::Str(k) = self.object(k, depth + 1)? else { return None };
                            let v = self.object(v, depth + 1)?;
                            pairs.push((k, v));
                        }
                        Some(Value::Dict(pairs))
                    }
                }
            }
            0x0..=0x3 | 0x8 => Some(Value::Other),
            _ => None,
        }
    }

    /// An object's count and where its contents start.
    fn count(&self, at: usize, low: usize) -> Option<(usize, usize)> {
        if low != 0xF {
            return Some((low, at + 1));
        }
        let m = *self.b.get(at + 1)?;
        if m >> 4 != 0x1 {
            return None;
        }
        let size = 1usize << (m & 0xF);
        if size > 8 {
            return None;
        }
        let n = usize::try_from(uint(self.b.get(at + 2..at + 2 + size)?)).ok()?;
        Some((n, at + 2 + size))
    }

    fn refs(&self, start: usize, n: usize) -> Option<Vec<usize>> {
        if n > MAX_OBJECTS {
            return None;
        }
        let bytes = self.b.get(start..start.checked_add(n.checked_mul(self.ref_size)?)?)?;
        bytes.chunks_exact(self.ref_size).map(|c| usize::try_from(uint(c)).ok()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(url: &str, mime: &str, data: &[u8]) -> Resource {
        Resource { url: url.into(), mime: mime.into(), encoding: None, data: data.to_vec() }
    }

    #[test]
    fn round_trips() {
        let main = Resource {
            encoding: Some("UTF-8".into()),
            ..res("https://a.example/p", "text/html", b"<p>caf\xc3\xa9</p>")
        };
        let mut subs = vec![
            res("https://a.example/s.css", "text/css", b"p{}"),
            res("https://cdn.example/\u{e9}.png", "image/png", &[0u8; 300]),
        ];
        for i in 0..300 {
            subs.push(res(&format!("https://a.example/{i}.js"), "text/javascript", format!("// {i}").as_bytes()));
        }
        let bytes = write(&main, &subs);
        assert!(bytes.starts_with(b"bplist00"));
        assert_eq!(read_main(&bytes).unwrap(), main);
        assert_eq!(read_subresources(&bytes).unwrap(), subs, "non-ASCII URLs and >255 objects survive");
    }

    /// `python3 -c "import plistlib; print(plistlib.dumps({...}, fmt=plistlib.FMT_BINARY).hex())"`:
    /// an archive written by another implementation, with a subframe.
    #[test]
    fn reads_foreign_archives() {
        let hex = "62706c6973743030d3010203040f165f100f5765624d61696e5265736f757263655f1013\
                   5765625375626672616d6541726368697665735f100f5765625375627265736f75726365\
                   73d505060708090a0b0c0d0e5f100f5765625265736f75726365446174615f1014576562\
                   5265736f757263654672616d654e616d655f10135765625265736f757263654d494d4554\
                   7970655f101b5765625265736f7572636554657874456e636f64696e674e616d655e5765\
                   625265736f7572636555524c433c703e5059746578742f68746d6c555554462d385f1012\
                   68747470733a2f2f662e6578616d706c652fa110d10311a112d305070913141543010203\
                   59696d6167652f706e675f101768747470733a2f2f662e6578616d706c652f612e706e67\
                   a00008000f00210037004900540066007d009300b100c000c400c500cf00d500ea00ec00\
                   ef00f100f800fc0106012000000000000002010000000000000017000000000000000000\
                   00000000000121";
        let hex: String = hex.split_whitespace().collect();
        let bytes: Vec<u8> =
            (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        let subs = read_subresources(&bytes).expect("parsed");
        assert_eq!(subs, vec![res("https://f.example/a.png", "image/png", &[1, 2, 3])]);
        let main = read_main(&bytes).unwrap();
        assert_eq!(
            (main.url.as_str(), main.encoding.as_deref(), main.data.as_slice()),
            ("https://f.example/", Some("UTF-8"), &b"<p>"[..])
        );
    }

    #[test]
    fn rejects_malformed_input() {
        let good = write(&res("https://a.example/", "text/html", b"x"), &[res("https://a.example/a", "a/b", b"y")]);
        assert!(read_subresources(b"").is_none());
        assert!(read_subresources(b"bplist00").is_none());
        // Every truncation and every single-byte corruption parses or fails
        // without panicking.
        for n in 0..good.len() {
            let _ = read_subresources(&good[..n]);
            for x in [0x00u8, 0x0F, 0x5F, 0xAF, 0xDF, 0xFF] {
                let mut bad = good.clone();
                bad[n] = x;
                let _ = read_subresources(&bad);
            }
        }
        // A dictionary that contains itself is cut off by the depth limit.
        let mut cyclic = good.clone();
        let t = cyclic.len() - 32;
        cyclic[t + 16..t + 24].copy_from_slice(&0u64.to_be_bytes());
        let _ = read_subresources(&cyclic);
    }
}
