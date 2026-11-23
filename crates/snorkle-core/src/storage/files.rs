//! Favicons and data URLs; reading the stores' folders (offline copies, page
//! text, downloads, chats) back at startup and search snippets.

use super::*;

/// Host whose `/favicon.ico` may be fetched for a pinned URL: https only,
/// validated to `[a-z0-9.-]` (safe to embed in HTML and file names).
pub fn favicon_host(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    if u.scheme() != "https" {
        return None;
    }
    match u.host()? {
        url::Host::Domain(d) => crate::blocking::normalise_site(d),
        _ => None,
    }
}

/// A small, plain PNG: signature, then an IHDR of at most 512×512 pixels,
/// 8 bits per channel, RGB or RGBA, not interlaced (what the favicon
/// fetcher produces). Anything else is refused rather than handed to the
/// chrome.
pub(super) fn is_png(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_ICON_BYTES || bytes.len() < 33 || !bytes.starts_with(PNG_SIGNATURE) {
        return false;
    }
    let ihdr = &bytes[8..33];
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (len, kind, w, h) = (be32(&ihdr[0..4]), &ihdr[4..8], be32(&ihdr[8..12]), be32(&ihdr[12..16]));
    let (depth, color, interlace) = (ihdr[16], ihdr[17], ihdr[20]);
    len == 13 && kind == b"IHDR" && (1..=512).contains(&w) && (1..=512).contains(&h) && depth == 8
        && matches!(color, 2 | 6) && interlace == 0
}

/// Describes a stored icon for the console ("128×128 RGBA, 5321 bytes").
pub fn png_summary(bytes: &[u8]) -> String {
    if bytes.len() < 33 || !bytes.starts_with(PNG_SIGNATURE) {
        return format!("not a PNG, {} bytes", bytes.len());
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    format!(
        "{}×{}, depth {}, colour type {}, interlace {}, {} bytes",
        be32(&bytes[16..20]),
        be32(&bytes[20..24]),
        bytes[24],
        bytes[25],
        bytes[28],
        bytes.len()
    )
}

/// `data:image/png;base64,…` for a validated PNG, for the chrome UI.
pub fn png_data_url(png: &[u8]) -> Option<String> {
    is_png(png).then(|| data_url("image/png", png))
}

/// `data:<mime>;base64,…` (the caller validated the bytes).
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(13 + mime.len() + bytes.len().div_ceil(3) * 4);
    out.push_str("data:");
    out.push_str(mime);
    out.push_str(";base64,");
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Reads the offline index, keeping entries with a valid file name, an
/// existing file and a URL that `keep` accepts (still pinned or
/// bookmarked); deletes every other file in the folder.
pub(super) fn load_offline(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<OfflineEntry> {
    let mut index: Vec<OfflineEntry> = load_or_reset(&dir.join(OFFLINE_INDEX));
    index.retain(|e| valid_offline_file(&e.file) && keep(&e.url) && dir.join(&e.file).is_file());
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name != OFFLINE_INDEX && !index.iter().any(|e| e.file == name) {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&index) {
        if dir.is_dir() {
            let _ = atomic_write(&dir.join(OFFLINE_INDEX), &bytes);
        }
    }
    index
}

/// Reads the page-text index, keeping entries with a valid file name, an
/// existing file and a URL still in history (`keep`), newest first and at
/// most [`MAX_PAGE_TEXTS`]; deletes every other file in the folder.
pub(super) fn load_page_texts(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PageTextEntry> {
    let mut index: Vec<PageTextEntry> = load_or_reset(&dir.join(OFFLINE_INDEX));
    index.retain(|e| {
        valid_page_text_file(&e.file)
            && storable_url(&e.url).as_deref() == Some(e.url.as_str())
            && keep(&e.url)
            && dir.join(&e.file).is_file()
    });
    index.sort_by_key(|e| std::cmp::Reverse(e.saved_ms));
    index.truncate(MAX_PAGE_TEXTS);
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name != OFFLINE_INDEX && !index.iter().any(|e| e.file == name) {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&index) {
        if dir.is_dir() {
            let _ = atomic_write(&dir.join(OFFLINE_INDEX), &bytes);
        }
    }
    index
}

/// Reads the download records, keeping those with a clean name, unique id
/// and an existing file; deletes every folder in `downloads/` without one.
pub(super) fn load_downloads(dir: &Path) -> Vec<DownloadRecord> {
    let root = dir.join(DOWNLOADS_DIR);
    let mut records: Vec<DownloadRecord> = load_or_reset(&dir.join(DOWNLOADS_FILE));
    let mut seen = BTreeSet::new();
    records.retain(|r| {
        clean_download_name(&r.name) == r.name
            && seen.insert(r.id)
            && (r.outcome != DownloadOutcome::Done || root.join(r.id.to_string()).join(&r.name).is_file())
    });
    records.sort_by_key(|r| std::cmp::Reverse(r.finished_ms));
    records.truncate(MAX_DOWNLOAD_RECORDS);
    if let Ok(entries) = fs::read_dir(&root) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let kept = records.iter().any(|r| r.id.to_string() == name && r.outcome == DownloadOutcome::Done);
            if !kept {
                let _ = fs::remove_dir_all(f.path());
            }
        }
    }
    records
}

/// Reads one saved page text, re-cleaned (a tampered file cannot carry
/// markup-relevant control characters or more than the cap).
pub(super) fn read_page_text(path: &Path) -> Option<SavedPageText> {
    if fs::metadata(path).ok()?.len() > MAX_PAGE_TEXT_FILE_BYTES {
        return None;
    }
    let mut p: SavedPageText = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    p.url = storable_url(&p.url)?;
    p.title = clean_title(&p.title);
    p.text = crate::ai::clean_multiline(&p.text, MAX_PAGE_TEXT_STORED_CHARS);
    Some(p)
}

/// About 160 characters of `text` around the first case-insensitive match
/// of `q` (already lowercase), on one line, with "…" where cut.
pub(super) fn snippet(text: &str, q: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let qc: Vec<char> = q.chars().collect();
    let at = (0..=lower.len().checked_sub(qc.len())?).find(|&i| lower[i..i + qc.len()] == qc[..])?;
    let start = at.saturating_sub(60);
    let end = (at + qc.len() + 100).min(chars.len());
    let body: String = chars[start..end].iter().map(|c| if c.is_whitespace() { ' ' } else { *c }).collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(body.trim());
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

pub(crate) fn load_or_reset<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(meta) = fs::metadata(path) else { return T::default() };
    let parsed = if meta.len() > MAX_FILE_BYTES {
        None
    } else {
        fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok())
    };
    match parsed {
        Some(v) => v,
        None => {
            let _ = fs::rename(path, path.with_extension("corrupt"));
            T::default()
        }
    }
}

/// Loads every valid chat file (others are deleted), newest first, capped.
pub(super) fn load_chats(dir: &Path) -> Vec<ChatRecord> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut chats: Vec<ChatRecord> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let ok = path.extension().is_some_and(|x| x == "json")
            && e.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
        let chat = ok
            .then(|| fs::read(&path).ok())
            .flatten()
            .and_then(|b| serde_json::from_slice::<ChatRecord>(&b).ok())
            .filter(|c| path.file_stem().and_then(|s| s.to_str()) == Some(c.id.as_str()))
            .and_then(validate_chat);
        match chat {
            Some(c) => chats.push(c),
            None => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    chats.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
    let keep = MAX_CHATS.min(chats.len());
    for c in chats.drain(keep..) {
        let _ = fs::remove_file(dir.join(format!("{}.json", c.id)));
    }
    chats
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}
