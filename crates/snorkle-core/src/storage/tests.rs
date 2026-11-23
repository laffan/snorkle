use super::*;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("snorkle-store-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

#[test]
fn notebook_is_checked_both_ways() {
    let dir = tmpdir("notebook");
    let s = Store::open(&dir).unwrap();
    assert!(s.notebook().is_none(), "none yet");
    assert!(!s.set_notebook("[1,2]").unwrap(), "not a notebook");
    let pad = r#"{"strokes":[],"notes":[],"links":[{"id":"l-1","x":1,"y":2,"text":"hi","target":{"kind":"page","url":"javascript:x"}}],"pan":{"x":0,"y":0}}"#;
    assert!(s.set_notebook(pad).unwrap());
    let back = s.notebook().unwrap();
    assert!(back.contains("\"text\":\"hi\"") && !back.contains("javascript"));
    // A file changed on disk is checked again as it is read.
    fs::write(dir.join(NOTEBOOK_FILE), br#"{"links":[{"id":"x","x":1,"y":1,"image":"data:text/html;base64,AA","target":{"kind":"clip"}}]}"#).unwrap();
    assert!(!s.notebook().unwrap().contains("text/html"));
    fs::write(dir.join(NOTEBOOK_FILE), b"garbage").unwrap();
    assert!(s.notebook().is_none());
}

#[test]
fn chats_roundtrip_validate_and_cap() {
    use crate::ai::ChatRole;
    let dir = tmpdir("chats");
    let s = Store::open(&dir).unwrap();
    let chat = |id: &str, t: u64| ChatRecord {
        id: id.into(),
        created_ms: t,
        updated_ms: t,
        url: "https://a.example/x".into(),
        title: "A\u{202e}".into(),
        page_text: "Body".into(),
        selection: String::new(),
        messages: vec![
            ChatEntry { role: ChatRole::User, text: "Hi".into() },
            ChatEntry { role: ChatRole::Assistant, text: "Hello".into() },
        ],
        provider: Provider::Anthropic,
        model: "claude-opus-5-5".into(),
        input: 10,
        output: 2,
        cost_micros: 80,
        unpriced: false,
        word: String::new(),
    };
    assert!(s.save_chat(chat("c1", 1)).unwrap());
    // A Define chat has no page; its word is checked instead.
    let define = ChatRecord { url: String::new(), page_text: "x".into(), word: "ennui".into(), ..chat("c2", 2) };
    assert!(s.save_chat(define).unwrap());
    assert!(s.chat("c2").is_some_and(|c| c.word == "ennui" && c.page_text.is_empty()));
    assert!(!s.save_chat(ChatRecord { word: "two words".into(), ..chat("c4", 4) }).unwrap());
    s.delete_chat("c2").unwrap();
    assert!(!s.save_chat(chat("../evil", 2)).unwrap(), "ids are file names: validated");
    assert!(!s.save_chat(ChatRecord { url: "javascript:x".into(), ..chat("c3", 3) }).unwrap());
    drop(s);
    let s = Store::open(&dir).unwrap();
    let got = s.chat("c1").unwrap();
    assert_eq!(got.title, "A");
    assert_eq!(got.messages.len(), 2);
    for i in 0..(MAX_CHATS as u64 + 5) {
        s.save_chat(chat(&format!("c{}", 100 + i), 10 + i)).unwrap();
    }
    assert_eq!(s.chats().len(), MAX_CHATS);
    assert!(s.chat("c1").is_none(), "least recently used dropped");
    s.delete_chat("c110").unwrap();
    assert!(s.chat("c110").is_none());
    fs::write(dir.join(CHATS_DIR).join("c9.json"), b"{not json").unwrap();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.chats().len(), MAX_CHATS - 1);
    assert!(!dir.join(CHATS_DIR).join("c9.json").exists(), "corrupt chat files are removed");
}

#[test]
fn download_names_are_safe() {
    assert_eq!(clean_download_name("report.pdf"), "report.pdf");
    assert_eq!(clean_download_name("../../etc/passwd"), "passwd");
    assert_eq!(clean_download_name("C:\\Users\\x\\evil.exe"), "evil.exe");
    assert_eq!(clean_download_name(".hidden"), "hidden");
    assert_eq!(clean_download_name("..."), "download");
    assert_eq!(clean_download_name(""), "download");
    assert_eq!(clean_download_name("a\u{202E}gpj.exe"), "agpj.exe", "bidi override removed");
    assert_eq!(clean_download_name("x:y?.txt"), "x-y-.txt");
    let long = format!("{}.tar.gz", "n".repeat(300));
    let c = clean_download_name(&long);
    assert_eq!(c.chars().count(), MAX_DOWNLOAD_NAME_CHARS);
    assert!(c.ends_with(".gz"));
}

#[test]
fn downloads_are_recorded_and_swept() {
    let dir = tmpdir("downloads");
    let _ = fs::remove_dir_all(&dir);
    let s = Store::open(&dir).unwrap();
    let (a, name, path) = s.new_download("../a.pdf").unwrap();
    assert_eq!(name, "a.pdf");
    assert!(path.starts_with(dir.join(DOWNLOADS_DIR)));
    fs::write(&path, b"pdf").unwrap();
    let (b, _, bpath) = s.new_download("partial.zip").unwrap();
    assert!(b > a);
    fs::write(&bpath, b"half").unwrap();
    let rec = DownloadRecord {
        id: a,
        name: name.clone(),
        source: "https://example.com/a.pdf".into(),
        mime: "application/pdf".into(),
        bytes: 3,
        outcome: DownloadOutcome::Done,
        error: String::new(),
        started_ms: 1,
        finished_ms: 2,
    };
    s.record_download(rec.clone()).unwrap();
    assert_eq!(s.download_path(a, &name), Some(path.clone()));
    assert_eq!(s.download_path(a, "../x"), None);
    drop(s);
    // Reopened: the recorded file stays; the unrecorded (interrupted) one goes.
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.downloads(), vec![rec]);
    assert!(path.is_file() && !bpath.exists());
    let (c, _, _) = s.new_download("next").unwrap();
    assert!(c > a, "recorded ids are not reused (an unrecorded one may be: its folder is gone)");
    s.delete_download(a).unwrap();
    assert!(s.downloads().is_empty() && !path.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn history_removal_and_page_text_cache() {
    let dir = tmpdir("pagetext");
    let _ = fs::remove_dir_all(&dir);
    let s = Store::open(&dir).unwrap();
    s.add_history("https://a.example/", "A", 1).unwrap();
    s.add_history("https://b.example/", "B", 2).unwrap();
    s.add_history("https://a.example/", "A", 3).unwrap();
    assert!(!s.save_page_text("https://nothere.example/", "X", "text", 4).unwrap(), "only pages in history");
    assert!(s.save_page_text("https://a.example/", "A", "Hello\n\nNeedle in a haystack", 4).unwrap());
    assert!(s.save_page_text("https://b.example/", "B", "Other text", 5).unwrap());
    assert_eq!(s.page_text("https://a.example/").unwrap().text, "Hello\n\nNeedle in a haystack");
    let hits = s.search_page_texts("NEEDLE", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://a.example/");
    assert!(hits[0].snippet.contains("Needle"));
    assert!(s.page_text_sizes()["https://a.example/"] > 0);
    // One of a's two visits goes: its text stays; the other goes too: gone.
    assert_eq!(s.remove_history(&[("https://a.example/".into(), 1)]).unwrap(), 1);
    assert!(s.page_text("https://a.example/").is_some());
    assert_eq!(s.remove_history(&[("https://a.example/".into(), 3)]).unwrap(), 1);
    assert!(s.page_text("https://a.example/").is_none());
    assert!(!s.in_history("https://a.example/") && s.in_history("https://b.example/"));
    // Survives a reopen; clearing history clears the texts.
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert!(s.page_text("https://b.example/").is_some());
    assert!(s.page_text("https://a.example/").is_none());
    s.clear_history().unwrap();
    assert!(s.page_text("https://b.example/").is_none() && s.page_text_sizes().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn page_text_cache_keeps_the_newest_hundred() {
    let dir = tmpdir("pagetext-cap");
    let _ = fs::remove_dir_all(&dir);
    let s = Store::open(&dir).unwrap();
    for i in 0..(MAX_PAGE_TEXTS as u64 + 5) {
        let url = format!("https://p{i}.example/");
        s.add_history(&url, "t", i).unwrap();
        assert!(s.save_page_text(&url, "t", "words", i).unwrap());
    }
    assert_eq!(s.page_text_sizes().len(), MAX_PAGE_TEXTS);
    assert!(s.page_text("https://p0.example/").is_none());
    assert!(s.page_text(&format!("https://p{}.example/", MAX_PAGE_TEXTS + 4)).is_some());
    let files = fs::read_dir(dir.join(PAGE_TEXT_DIR)).unwrap().count();
    assert_eq!(files, MAX_PAGE_TEXTS + 1, "evicted files deleted (plus the index)");
    assert_eq!(snippet("abc", "abcd"), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn history_roundtrip_and_filtering() {
    let dir = tmpdir("hist");
    let s = Store::open(&dir).unwrap();
    s.add_history("https://a.example/", "A\u{202E}", 1).unwrap();
    s.add_history("javascript:alert(1)", "x", 2).unwrap();
    s.add_history("data:text/html,x", "x", 3).unwrap();
    s.add_history("https://a.example/", "A2", 4).unwrap();
    let h = s.history(10);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].title, "A2");
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.history(10).len(), 1);
}

#[test]
fn tampered_file_is_sanitised() {
    let dir = tmpdir("tamper");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(BOOKMARKS_FILE),
        r#"[{"url":"javascript:alert(1)","title":"x","created_ms":1},{"url":"https://ok.example","title":"ok","created_ms":1}]"#,
    )
    .unwrap();
    let s = Store::open(&dir).unwrap();
    let b = s.bookmarks();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].url, "https://ok.example/");
}

#[test]
fn corrupt_file_is_moved_aside() {
    let dir = tmpdir("corrupt");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(HISTORY_FILE), b"{not json").unwrap();
    let s = Store::open(&dir).unwrap();
    assert!(s.history(10).is_empty());
    assert!(dir.join("history.corrupt").exists());
}

#[test]
fn adblock_allowlist_roundtrip_and_validation() {
    let dir = tmpdir("adblock");
    let s = Store::open(&dir).unwrap();
    s.set_adblock_allowed("News.Example", true).unwrap();
    s.set_adblock_allowed("bad\"]}(", true).unwrap();
    assert_eq!(s.adblock_allowed().into_iter().collect::<Vec<_>>(), vec!["news.example".to_string()]);
    drop(s);
    fs::write(dir.join(ADBLOCK_ALLOW_FILE), r#"["ok.example","x(y"]"#).unwrap();
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.adblock_allowed().into_iter().collect::<Vec<_>>(), vec!["ok.example".to_string()]);
    s.set_adblock_allowed("ok.example", false).unwrap();
    assert!(s.adblock_allowed().is_empty());
}

#[test]
fn bookmark_insert_and_move() {
    let dir = tmpdir("bm-order");
    let s = Store::open(&dir).unwrap();
    for (i, u) in ["https://a.example/", "https://b.example/", "https://c.example/"].iter().enumerate() {
        assert!(s.insert_bookmark(u, "t", 99, i as u64).unwrap());
    }
    assert!(s.insert_bookmark("https://z.example/", "t", 0, 9).unwrap());
    assert!(!s.insert_bookmark("https://a.example/", "dup", 0, 9).unwrap());
    s.move_bookmark("https://z.example/", 99).unwrap();
    s.move_bookmark("https://c.example/", 0).unwrap();
    let order: Vec<String> = s.bookmarks().into_iter().map(|b| b.url).collect();
    assert_eq!(order, ["https://c.example/", "https://a.example/", "https://b.example/", "https://z.example/"]);
    drop(s);
    assert_eq!(Store::open(&dir).unwrap().bookmarks().len(), 4);
}

#[test]
fn user_scripts_roundtrip() {
    let dir = tmpdir("userscripts");
    let s = Store::open(&dir).unwrap();
    let us = |d: &str, js: &str| UserScript { domain: d.into(), css: String::new(), js: js.into(), enabled: true };
    assert_eq!(s.save_user_script(us("Example.com", "a()")).unwrap().domain, "example.com");
    s.save_user_script(us("example.com", "b()")).unwrap();
    assert!(s.save_user_script(us("not a domain", "c()")).is_err());
    assert_eq!(s.user_scripts().len(), 1);
    assert_eq!(s.user_scripts()[0].js, "b()");
    drop(s);
    fs::write(
        dir.join(USER_SCRIPTS_FILE),
        r#"[{"domain":"ok.example","css":"","js":"x()","enabled":true},{"domain":"bad\"]","css":"","js":"","enabled":true}]"#,
    )
    .unwrap();
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.user_scripts().len(), 1, "tampered entries are dropped on load");
    s.delete_user_script("ok.example").unwrap();
    assert!(s.user_scripts().is_empty());
}

#[test]
fn block_list_is_stored_and_cleaned() {
    use crate::blocklist::{BlockList, BlockSettings};
    let dir = tmpdir("blocked");
    {
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.blocked(), BlockSettings::default());
        let mut l = BlockList::from_settings(s.blocked());
        l.add("reddit\\.com").unwrap();
        l.set_message("Back to work.");
        s.set_blocked(l.settings()).unwrap();
    }
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.blocked().patterns, ["reddit\\.com"]);
    assert_eq!(s.blocked().message, "Back to work.");
    // A hand-edited file: invalid patterns are dropped on load.
    fs::write(dir.join(BLOCKED_FILE), br#"{"patterns":["(","a\\.com"],"message":""}"#).unwrap();
    assert_eq!(Store::open(&dir).unwrap().blocked().patterns, ["a\\.com"]);
}

#[test]
fn launch_modes_are_stored() {
    use crate::viewmodes::{Kind, LaunchModes};
    let dir = tmpdir("launch-modes");
    {
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.launch_modes(), LaunchModes::default());
        s.set_launch_mode(Kind::Dark, true).unwrap();
        s.set_launch_mode(Kind::HideMedia, false).unwrap();
        s.set_launch_mode(Kind::Night, true).unwrap();
    }
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.launch_modes(), LaunchModes { desaturate: true, hide_media: false, dark: true, night: true });
    // A partial file (one written before night vision, say) keeps the
    // defaults for what it lacks.
    fs::write(dir.join(LAUNCH_MODES_FILE), br#"{"dark":true}"#).unwrap();
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.launch_modes(), LaunchModes { desaturate: true, hide_media: true, dark: true, night: false });
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn offline_copies() {
    let dir = tmpdir("offline");
    let url = "https://news.example/";
    {
        let s = Store::open(&dir).unwrap();
        assert!(!s.offline_copy_wanted(url, 0), "not pinned or bookmarked");
        assert!(!s.save_offline_copy(url, b"archive", 0).unwrap());
        s.insert_bookmark(url, "News", 0, 0).unwrap();
        assert!(s.offline_copy_wanted(url, 0));
        assert!(!s.save_offline_copy(url, &[], 0).unwrap(), "empty");
        assert!(s.save_offline_copy(url, b"archive", 1_000).unwrap());
        assert!(!s.offline_copy_wanted(url, 1_000 + OFFLINE_REFRESH_MS - 1), "fresh");
        assert!(s.offline_copy_wanted(url, 1_000 + OFFLINE_REFRESH_MS));
        assert_eq!(s.offline_copy(url), Some((b"archive".to_vec(), 1_000)));
        fs::write(dir.join(OFFLINE_DIR).join("stray.txt"), b"x").unwrap();
    }
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.offline_copy(url).map(|c| c.1), Some(1_000), "kept across launches");
    assert!(!dir.join(OFFLINE_DIR).join("stray.txt").exists(), "unlisted files are deleted");
    s.take_bookmark(url).unwrap();
    assert!(s.offline_copy(url).is_none(), "only while bookmarked");
    let s = Store::open(&dir).unwrap();
    assert!(fs::read_dir(dir.join(OFFLINE_DIR)).unwrap().count() == 1, "only the index is left");
    s.insert_pin(url, "News", 0, 0).unwrap();
    s.save_offline_copy(url, b"a2", 5).unwrap();
    s.clear_offline_copies();
    assert!(s.offline_copy(url).is_none() && !dir.join(OFFLINE_DIR).exists());
    assert!(valid_offline_file(&offline_file(url)) && !valid_offline_file("../x.webarchive"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn item_urls_can_be_changed() {
    let dir = tmpdir("edit-url");
    let s = Store::open(&dir).unwrap();
    s.insert_pin("https://p.example/", "P", 0, 0).unwrap();
    s.set_pin_emoji("https://p.example/", Some("📌")).unwrap();
    s.insert_bookmark("https://b.example/", "B", 0, 0).unwrap();
    s.save_offline_copy("https://b.example/", b"x", 1).unwrap();
    assert_eq!(
        s.change_item_url("https://p.example/", "https://p.example/home").unwrap().as_deref(),
        Some("https://p.example/home")
    );
    assert_eq!(s.pins()[0].url, "https://p.example/home");
    assert_eq!(s.pins()[0].title, "P", "title kept");
    assert_eq!(s.pin_emoji("https://p.example/home").as_deref(), Some("📌"), "emoji follows");
    assert!(s.change_item_url("https://b.example/", "https://p.example/home").unwrap().is_none(), "already pinned");
    assert!(s.change_item_url("https://b.example/", "javascript:alert(1)").unwrap().is_none());
    assert!(s.change_item_url("https://none.example/", "https://x.example/").unwrap().is_none());
    assert!(s.change_item_url("https://b.example/", "https://b.example/new").unwrap().is_some());
    assert!(
        s.offline_copy("https://b.example/new").is_none() && s.offline_copy("https://b.example/").is_none(),
        "old copy dropped"
    );
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.bookmarks()[0].url, "https://b.example/new");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pin_emoji() {
    assert_eq!(clean_emoji(" 📰 ").as_deref(), Some("📰"));
    assert_eq!(
        clean_emoji("👨\u{200D}👩\u{200D}👧").as_deref(),
        Some("👨\u{200D}👩\u{200D}👧"),
        "joined sequences survive"
    );
    for bad in ["", "  ", "a b", "\u{202E}x", "x\u{7}y", &"😀".repeat(MAX_EMOJI_CHARS + 1)] {
        assert!(clean_emoji(bad).is_none(), "{bad:?}");
    }
    let dir = tmpdir("pin-emoji");
    {
        let s = Store::open(&dir).unwrap();
        s.insert_pin("https://n.example/", "n", 0, 0).unwrap();
        assert!(!s.set_pin_emoji("https://other.example/", Some("📰")).unwrap(), "only for pins and bookmarks");
        s.insert_bookmark("https://b.example/", "b", 0, 0).unwrap();
        assert!(s.set_pin_emoji("https://b.example/", Some("🔖")).unwrap(), "bookmarks too");
        assert!(!s.set_pin_emoji("https://n.example/", Some("not one")).unwrap());
        assert!(s.set_pin_emoji("https://n.example/", Some("📰")).unwrap());
    }
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.pin_emoji("https://n.example/").as_deref(), Some("📰"));
    assert_eq!(s.pin_emoji("https://b.example/").as_deref(), Some("🔖"));
    let p = s.remove_pin("https://n.example/").unwrap().unwrap();
    s.insert_bookmark(&p.url, &p.title, 0, 0).unwrap();
    s.prune_item_emoji().unwrap();
    assert_eq!(s.pin_emoji("https://n.example/").as_deref(), Some("📰"), "kept when moved to the bookmarks");
    s.remove_bookmark("https://n.example/").unwrap();
    assert!(s.pin_emoji("https://n.example/").is_none(), "removed with the item");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pins_are_capped_and_movable() {
    let dir = tmpdir("pins");
    let s = Store::open(&dir).unwrap();
    for i in 0..MAX_PINS + 2 {
        s.insert_pin(&format!("https://p{i}.example/"), "p", 99, 0).unwrap();
    }
    assert_eq!(s.pins().len(), MAX_PINS);
    assert!(!s.insert_pin("https://p0.example/", "dup", 0, 0).unwrap());
    s.move_pin("https://p3.example/", 0).unwrap();
    assert_eq!(s.pins()[0].url, "https://p3.example/");
    assert!(s.remove_pin("https://p3.example/").unwrap().is_some());
    assert_eq!(s.pins().len(), MAX_PINS - 1);
    assert!(s.insert_bookmark("https://b.example/", "b", 0, 0).unwrap());
    assert_eq!(s.take_bookmark("https://b.example/").unwrap().unwrap().title, "b");
    assert!(s.bookmarks().is_empty());
}

#[test]
fn icons_must_be_small_pngs() {
    let dir = tmpdir("icons");
    let s = Store::open(&dir).unwrap();
    let png = test_png(64, 8, 6);
    assert!(s.save_icon("www.example.com", &png).unwrap());
    // Unusual encodings are refused: 16-bit, palette, too large.
    assert!(!s.save_icon("www.example.com", &test_png(64, 16, 6)).unwrap());
    assert!(!s.save_icon("www.example.com", &test_png(64, 8, 3)).unwrap());
    assert!(!s.save_icon("www.example.com", &test_png(4096, 8, 6)).unwrap());
    assert!(!s.save_icon("www.example.com", &[PNG_SIGNATURE, b"rest"].concat()).unwrap());
    assert_eq!(s.icon("www.example.com").unwrap(), png);
    assert!(!s.save_icon("www.example.com", b"<svg onload=alert(1)>").unwrap());
    assert!(!s.save_icon("../../etc/passwd", &png).unwrap());
    assert!(!s.save_icon("x.example", &[PNG_SIGNATURE, &vec![0u8; MAX_ICON_BYTES]].concat()).unwrap());
    assert!(s.icon("nope.example").is_none());
}

/// Signature + IHDR (+ a stand-in for the rest of the file).
fn test_png(size: u32, depth: u8, color: u8) -> Vec<u8> {
    let mut v = PNG_SIGNATURE.to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(&[depth, color, 0, 0, 0]);
    v.extend_from_slice(&[0, 0, 0, 0]); // CRC (not checked)
    v
}

#[test]
fn ai_usage_roundtrip_and_reset() {
    let dir = tmpdir("aiusage");
    let s = Store::open(&dir).unwrap();
    let e = AiUsageEvent { at_ms: 1, provider: Provider::Anthropic, model: "claude-opus-5-5".into(), input: 10, output: 5, cost_micros: Some(140) };
    s.record_ai_usage(e.clone()).unwrap();
    s.record_ai_usage(AiUsageEvent { model: "bad model\"".into(), ..e.clone() }).unwrap();
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.ai_usage(), vec![e]);
    s.clear_ai_usage().unwrap();
    assert!(Store::open(&dir).unwrap().ai_usage().is_empty());
}

#[test]
fn old_icon_cache_is_removed() {
    let dir = tmpdir("oldicons");
    fs::create_dir_all(dir.join("icons")).unwrap();
    fs::write(dir.join("icons/www.example.com.png"), test_png(64, 8, 6)).unwrap();
    fs::write(dir.join(OLD_HEADING_RULES_FILE), br#"{"nytimes.com":[{"selector":"h2","level":2}]}"#).unwrap();
    let s = Store::open(&dir).unwrap();
    assert!(s.icon("www.example.com").is_none(), "refetched in the new format");
    assert!(!dir.join("icons").exists());
    assert!(!dir.join(OLD_HEADING_RULES_FILE).exists(), "the headers view's rules are gone");
}

#[test]
fn favicon_hosts() {
    assert_eq!(favicon_host("https://www.Pinterest.com/x").as_deref(), Some("www.pinterest.com"));
    assert_eq!(favicon_host("http://plain.example/"), None);
    assert_eq!(favicon_host("https://192.168.0.1/"), None);
    assert_eq!(favicon_host("javascript:alert(1)"), None);
}

#[test]
fn data_url_encoding() {
    let png = test_png(16, 8, 6);
    let url = png_data_url(&png).unwrap();
    assert!(url.starts_with("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAAAAAA"));
    assert!(png_data_url(b"GIF89a").is_none());
}

#[test]
fn session_roundtrip() {
    let dir = tmpdir("session");
    let s = Store::open(&dir).unwrap();
    let sess = Session {
        tabs: vec![
            SessionTab {
                url: "https://a.example/".into(),
                title: "a".into(),
                launcher: None,
                exempt: vec![],
                zoom: None,
                url_bar_attached: false,
                name: None,
                emoji: None,
            },
            SessionTab { url: "file:///x".into(), title: "f".into(), launcher: None, exempt: vec![], zoom: None, url_bar_attached: false, name: None, emoji: None },
        ],
        active: Some(1),
    };
    s.save_session(&sess).unwrap();
    let loaded = s.load_session();
    assert_eq!(loaded.tabs.len(), 1);
    assert_eq!(loaded.active, None);
}
