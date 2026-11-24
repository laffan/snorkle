//! Bookmark groups, research sessions' notebooks, dark icons and the
//! session's lists (2026-11-24).

use super::*;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("snorkle-store-g-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn urls(s: &Store) -> Vec<(String, Option<String>)> {
    s.bookmarks()
        .into_iter()
        .map(|b| (b.url.trim_start_matches("https://").trim_end_matches(".example/").to_string(), b.group))
        .collect()
}

#[test]
fn bookmark_groups_keep_their_bookmarks_together() {
    let dir = tmpdir("bm");
    let s = Store::open(&dir).unwrap();
    for (i, n) in ["a", "b", "c", "d"].iter().enumerate() {
        assert!(s.add_bookmark(&format!("https://{n}.example/"), n, i as u64).unwrap());
    }
    let g =
        s.group_bookmarks(&["https://d.example/".into(), "https://b.example/".into()], " Work ", 7).unwrap().unwrap();
    assert!(valid_bookmark_group_id(&g));
    let names: Vec<String> = urls(&s).into_iter().map(|(u, _)| u).collect();
    assert_eq!(names, ["a", "b", "d", "c"], "gathered where the first is");
    assert_eq!(s.bookmark_groups()[0].name, "Work");
    // Dropped into the group, out of it, and a new bookmark into it.
    assert!(s.place_bookmark("https://c.example/", 1, Some(&g)).unwrap());
    assert_eq!(s.bookmark_group_members(&g).len(), 3);
    assert!(s.place_bookmark("https://c.example/", 0, None).unwrap());
    assert!(!s.place_bookmark("https://c.example/", 0, Some("g1")).unwrap(), "no such group");
    assert!(s.insert_bookmark_in("https://e.example/", "e", 9, Some(&g), 8).unwrap());
    assert_eq!(s.bookmark_group_members(&g).len(), 3);
    // Moving a grouped bookmark away keeps it with its group.
    s.move_bookmark("https://b.example/", 0).unwrap();
    let order: Vec<(String, Option<String>)> = urls(&s);
    let first_grouped = order.iter().position(|(_, g)| g.is_some()).unwrap();
    assert!(order[first_grouped..first_grouped + 3].iter().all(|(_, x)| x.as_deref() == Some(g.as_str())));
    // Renamed, folded; kept across a reopen.
    assert!(s.rename_bookmark_group(&g, "\u{202E}Later").unwrap());
    assert!(s.set_bookmark_group_collapsed(&g, true).unwrap());
    let s2 = Store::open(&dir).unwrap();
    assert_eq!(s2.bookmark_groups(), vec![BookmarkGroup { id: g.clone(), name: "Later".into(), collapsed: true }]);
    // Ungrouped, removed; a group left empty goes.
    let g2 = s2.group_bookmarks(&["https://a.example/".into()], "One", 9).unwrap().unwrap();
    assert_eq!(s2.remove_bookmarks(&["https://a.example/".into()]).unwrap(), vec!["https://a.example/".to_string()]);
    assert!(!s2.bookmark_groups().iter().any(|x| x.id == g2));
    assert!(s2.ungroup_bookmarks(&g).unwrap());
    assert!(s2.bookmark_groups().is_empty() && s2.bookmarks().iter().all(|b| b.group.is_none()));
    assert!(s2.group_bookmarks(&["https://nope.example/".into()], "x", 10).unwrap().is_none());
    // Taken into the pins, a bookmark leaves its group.
    let g3 = s2.group_bookmarks(&["https://c.example/".into()], "C", 11).unwrap().unwrap();
    let taken = s2.take_bookmark("https://c.example/").unwrap().unwrap();
    assert_eq!(taken.group, None);
    assert!(!s2.bookmark_groups().iter().any(|x| x.id == g3));
}

#[test]
fn tampered_bookmark_groups_are_cleaned() {
    let dir = tmpdir("bm-tamper");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(BOOKMARKS_FILE),
        br#"[{"url":"https://a.example/","title":"a","created_ms":1,"group":"g1"},{"url":"https://b.example/","title":"b","created_ms":2,"group":"g../x"},{"url":"https://c.example/","title":"c","created_ms":3,"group":"g1"}]"#,
    )
    .unwrap();
    fs::write(dir.join(groups::BOOKMARK_GROUPS_FILE), r#"[{"id":"g1","name":"\u202eA"},{"id":"g1","name":"dup"},{"id":"../x","name":"bad"},{"id":"g2","name":"empty"}]"#.as_bytes()).unwrap();
    fs::write(dir.join(PINS_FILE), br#"[{"url":"https://p.example/","title":"p","created_ms":1,"group":"g1"}]"#)
        .unwrap();
    let s = Store::open(&dir).unwrap();
    assert_eq!(s.bookmark_groups(), vec![BookmarkGroup { id: "g1".into(), name: "A".into(), collapsed: false }]);
    let order: Vec<(String, Option<String>)> = urls(&s);
    assert_eq!(order, vec![("a".into(), Some("g1".into())), ("c".into(), Some("g1".into())), ("b".into(), None)]);
    assert!(s.pins().iter().all(|p| p.group.is_none()), "pins have no groups");
}

#[test]
fn research_sessions_have_their_own_notebooks() {
    let dir = tmpdir("nb");
    let s = Store::open(&dir).unwrap();
    let pad = r#"{"strokes":[],"notes":[],"links":[],"pan":{"x":3,"y":0}}"#;
    assert!(s.set_notebook_in(Some("r17"), pad).unwrap());
    assert!(s.notebook_in(None).is_none(), "everyday browsing's notebook is separate");
    assert!(s.notebook_in(Some("r17")).unwrap().contains("\"x\":3"));
    assert!(s.notebook_in(Some("r18")).is_none());
    for bad in ["../notebook", "r1/../../x", "x17", ""] {
        assert!(!s.set_notebook_in(Some(bad), pad).unwrap(), "{bad}");
        assert!(s.notebook_in(Some(bad)).is_none());
    }
    s.delete_notebook_in("r17").unwrap();
    assert!(s.notebook_in(Some("r17")).is_none());
    s.delete_notebook_in("r17").unwrap();
    s.delete_notebook_in("../history").unwrap();
    assert!(dir.join(HISTORY_FILE).exists() || !dir.join("../history").exists());
}

#[test]
fn exports_keep_one_cleaned_file() {
    let dir = tmpdir("exports");
    let s = Store::open(&dir).unwrap();
    let a = s.export_file("../../Tide: pools.html", b"one").unwrap();
    assert_eq!(a, dir.join("exports").join("Tide- pools.html"));
    let b = s.export_file("Other.html", b"two").unwrap();
    assert!(!a.exists() && fs::read(&b).unwrap() == b"two");
    assert_eq!(fs::read_dir(dir.join("exports")).unwrap().count(), 1);
}

#[test]
fn icons_keep_a_dark_copy() {
    let dir = tmpdir("icons-dark");
    let s = Store::open(&dir).unwrap();
    let png = super::tests::test_png(16, 8, 6);
    let dark = super::tests::test_png(16, 8, 2);
    assert!(s.save_icons("x.example", &png, Some(&dark)).unwrap());
    assert_eq!(s.icon("x.example").unwrap(), png);
    assert_eq!(s.icon_dark("x.example").unwrap(), dark);
    // Fetched again without one: the old copy goes.
    assert!(s.save_icon("x.example", &png).unwrap());
    assert!(s.icon_dark("x.example").is_none());
    assert!(!s.save_icons("x.example", &png, Some(b"<svg>")).unwrap(), "only PNGs");
    assert!(!s.save_icons("../x", &png, Some(&dark)).unwrap());
    // An older cache folder is deleted when the store opens.
    fs::create_dir_all(dir.join("icons-v4")).unwrap();
    Store::open(&dir).unwrap();
    assert!(!dir.join("icons-v4").exists());
}

#[test]
fn session_lists_are_cleaned() {
    let dir = tmpdir("session-lists");
    let s = Store::open(&dir).unwrap();
    let sess = Session {
        tabs: vec![
            SessionTab {
                url: "https://a.example/".into(),
                space: Some("r1".into()),
                group: Some(4),
                ..Default::default()
            },
            SessionTab {
                url: "https://b.example/".into(),
                space: Some("r9".into()),
                group: Some(5),
                ..Default::default()
            },
        ],
        active: Some(0),
        groups: vec![
            SessionGroup { id: 4, name: "\u{202E}G".into(), collapsed: true },
            SessionGroup { id: 4, name: "dup".into(), collapsed: false },
            SessionGroup { id: 6, name: "no tabs".into(), collapsed: false },
        ],
        research: vec![
            SessionResearch { id: "r1".into(), name: " ".into(), created_ms: 1 },
            SessionResearch { id: "../x".into(), name: "bad".into(), created_ms: 2 },
            SessionResearch { id: "r1".into(), name: "dup".into(), created_ms: 3 },
        ],
    };
    s.save_session(&sess).unwrap();
    let back = s.load_session();
    assert_eq!(back.research, vec![SessionResearch { id: "r1".into(), name: "Research".into(), created_ms: 1 }]);
    assert_eq!(back.groups, vec![SessionGroup { id: 4, name: "G".into(), collapsed: true }]);
    assert_eq!((back.tabs[0].space.as_deref(), back.tabs[0].group), (Some("r1"), Some(4)));
    assert_eq!((back.tabs[1].space.as_deref(), back.tabs[1].group), (None, None));
}
