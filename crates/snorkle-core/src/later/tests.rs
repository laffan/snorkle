use super::*;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("snorkle-later-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

#[test]
fn saves_lists_and_takes_pages() {
    let dir = tmpdir("basic");
    let s = LaterStore::open(&dir).unwrap();
    let a = s.add("https://a.example/x", "A\u{202e}", Some(JPEG), None, 1).unwrap().unwrap();
    let b = s.add("https://b.example/", "B", Some(b"<svg/>"), None, 2).unwrap().unwrap();
    assert!(s.add("javascript:alert(1)", "x", None, None, 3).unwrap().is_none(), "web pages only");
    assert!(a.thumb && !b.thumb, "only a JPEG is kept as a thumbnail");
    assert_eq!(a.title, "A", "titles are cleaned");
    assert_eq!(s.list().iter().map(|i| i.id.clone()).collect::<Vec<_>>(), vec![b.id.clone(), a.id.clone()]);
    assert_eq!(s.thumb(&a.id).as_deref(), Some(JPEG));
    assert_eq!(s.thumb_url(&a.id).as_deref(), Some("data:image/jpeg;base64,/9j/4AECAw=="));
    assert!(s.thumb("../index").is_none());
    drop(s);
    let s = LaterStore::open(&dir).unwrap();
    assert_eq!(s.list().len(), 2, "kept across launches");
    assert_eq!(s.take(&a.id).unwrap().map(|i| i.url), Some("https://a.example/x".to_string()));
    assert!(!dir.join(format!("{}.jpg", a.id)).exists(), "thumbnail deleted with it");
    assert!(s.take(&a.id).unwrap().is_none());
    fs::write(dir.join("stray.jpg"), JPEG).unwrap();
    drop(s);
    let _ = LaterStore::open(&dir).unwrap();
    assert!(!dir.join("stray.jpg").exists(), "files without an entry are removed");
}

#[test]
fn keeps_at_most_max_later() {
    let dir = tmpdir("cap");
    let s = LaterStore::open(&dir).unwrap();
    let first = s.add("https://first.example/", "", Some(JPEG), None, 0).unwrap().unwrap();
    for i in 1..=MAX_LATER as u64 {
        s.add("https://e.example/", "", None, None, i).unwrap();
    }
    assert_eq!(s.list().len(), MAX_LATER);
    assert!(s.list().iter().all(|i| i.id != first.id), "oldest dropped");
    assert!(!dir.join(format!("{}.jpg", first.id)).exists());
    assert!(!valid_thumb(&vec![0xFF; MAX_THUMB_BYTES + 1]));
}

fn capture(url: &str, html: &str, resources: Vec<Resource>) -> PageCapture {
    PageCapture { url: url.into(), title: "T".into(), html: html.as_bytes().to_vec(), scripts: true, resources }
}

fn res(url: &str, mime: &str, data: &[u8]) -> Resource {
    Resource { url: url.into(), mime: mime.into(), encoding: None, data: data.to_vec() }
}

#[test]
fn archives_pages_and_resources() {
    let dir = tmpdir("archive");
    let s = LaterStore::open(&dir).unwrap();
    let page = "https://a.example/story#top";
    let c = capture(
        page,
        "<p>story</p>",
        vec![
            res("https://a.example/app.js", "text/javascript; charset=UTF-8", b"run()"),
            res("https://cdn.example/x.png#frag", "image/png", &[1, 2, 3]),
            res("https://a.example/app.js", "text/javascript", b"duplicate"),
            res("javascript:alert(1)", "text/html", b"x"),
            res("https://a.example/empty", "text/plain", b""),
            res("https://user:pw@a.example/secret", "text/plain", b"x"),
            res("https://a.example/evil", "text/html\r\nX: y", b"x"),
        ],
    );
    let item = s.add("https://a.example/story", "Story", None, Some(c), 5).unwrap().unwrap();
    assert_eq!(item.pages.len(), 1);
    assert_eq!(item.pages[0].url, "https://a.example/story", "pages are kept without fragments");
    let open = s.archive(&item.id, None).unwrap();
    assert!(open.scripts && open.saved_ms == 5 && open.page.as_str() == "https://a.example/story");
    let main = crate::webarchive::read_main(&open.data).unwrap();
    assert_eq!(main.data, b"<p>story</p>");
    let subs = crate::webarchive::read_subresources(&open.data).unwrap();
    let mut got: Vec<(String, String, Vec<u8>)> = subs.into_iter().map(|r| (r.url, r.mime, r.data)).collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("https://a.example/app.js".into(), "text/javascript".into(), b"run()".to_vec()),
            ("https://a.example/evil".into(), "application/octet-stream".into(), b"x".to_vec()),
            ("https://cdn.example/x.png".into(), "image/png".into(), vec![1, 2, 3]),
        ],
        "first copy kept; bad URLs, empty data dropped; odd MIME types neutralised"
    );

    // Adding a page shares the resources; a page is opened by URL.
    let n = s
        .add_pages(&item.id, vec![capture("https://a.example/part-2", "<p>two</p>", vec![res("https://a.example/two.css", "text/css", b"p{}")])])
        .unwrap();
    assert_eq!(n, 2);
    let two = s.archive(&item.id, Some("https://a.example/part-2#x")).unwrap();
    assert_eq!(crate::webarchive::read_main(&two.data).unwrap().data, b"<p>two</p>");
    assert_eq!(two.pages.len(), 2);
    assert_eq!(crate::webarchive::read_subresources(&two.data).unwrap().len(), 4);

    // Kept across launches; removed with the item.
    drop(s);
    let s = LaterStore::open(&dir).unwrap();
    assert_eq!(s.item(&item.id).unwrap().pages.len(), 2);
    assert!(dir.join(&item.id).is_dir());
    fs::create_dir_all(dir.join("l1-1")).unwrap();
    drop(s);
    let s = LaterStore::open(&dir).unwrap();
    assert!(!dir.join("l1-1").exists(), "archives without an item are removed");
    s.take(&item.id).unwrap();
    assert!(!dir.join(&item.id).exists());
    assert!(s.archive(&item.id, None).is_none());
    assert!(s.archive("../x", None).is_none());
}

#[test]
fn archive_limits_hold() {
    let dir = tmpdir("archive-cap");
    let s = LaterStore::open(&dir).unwrap();
    let big = vec![0u8; MAX_RESOURCE_BYTES];
    let resources: Vec<Resource> = (0..8).map(|i| res(&format!("https://a.example/{i}.bin"), "application/octet-stream", &big)).collect();
    let too_big = res("https://a.example/huge", "a/b", &vec![0u8; MAX_RESOURCE_BYTES + 1]);
    let mut all = resources;
    all.push(too_big);
    let item = s.add("https://a.example/", "", None, Some(capture("https://a.example/", "<p>", all)), 1).unwrap().unwrap();
    assert!(item.archive_bytes <= MAX_ARCHIVE_BYTES);
    let open = s.archive(&item.id, None).unwrap();
    let subs = crate::webarchive::read_subresources(&open.data).unwrap();
    assert_eq!(subs.len() as u64, MAX_ARCHIVE_BYTES / MAX_RESOURCE_BYTES as u64, "what does not fit is left out");
    let pages: Vec<PageCapture> = (0..MAX_PAGES + 5).map(|i| capture(&format!("https://a.example/p{i}"), "<p>", vec![])).collect();
    assert_eq!(s.add_pages(&item.id, pages).unwrap(), MAX_PAGES);
}

#[test]
fn capture_reports_are_checked() {
    let start = r#"{"html":"<p>hi</p>","title":"T\u202e","urls":["https://a.example/x.js","javascript:1","https://a.example/y.css#z"]}"#;
    let r = parse_start(start).unwrap();
    assert_eq!(r.html.as_deref(), Some("<p>hi</p>"));
    assert_eq!(r.title, "T");
    assert_eq!(r.urls.len(), 3, "indices stay aligned");
    assert!(r.urls[1].is_none());
    assert_eq!(r.urls[2].as_ref().unwrap().as_str(), "https://a.example/y.css");
    assert!(parse_start("nope").is_none());
    let drain = r#"{"items":[{"i":0,"t":"text/javascript; charset=utf-8","b":"cnVuKCk="},{"i":1,"t":"x/y","b":"AA=="},{"i":9,"t":"","b":"AA=="},{"i":2,"t":"text/css","b":"!!"}],"more":true}"#;
    let (items, more) = parse_drain(drain, &r.urls).unwrap();
    assert!(more);
    assert_eq!(items.len(), 1, "unknown or invalid indices and bad base64 are dropped");
    assert_eq!((items[0].url.as_str(), items[0].mime.as_str(), items[0].encoding.as_deref()), ("https://a.example/x.js", "text/javascript", Some("utf-8")));
    assert_eq!(items[0].data, b"run()");
    assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
    assert_eq!(base64_decode("aGVsbG8gd29ybGQ="), Some(b"hello world".to_vec()));
    assert!(fetch_js(&[1, 2, 3]).contains("[1,2,3].map"));
    assert_eq!(clean_mime("TEXT/HTML; charset=x"), "text/html");
    assert_eq!(clean_mime("text/html\"><script>"), "application/octet-stream");
    assert_eq!(charset_of("text/html; charset=\"ISO-8859-1\""), Some("ISO-8859-1".into()));
    assert_eq!(charset_of("text/html; charset=a b"), None);
}
