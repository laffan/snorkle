//! Helpers shared by the `Browser` state-machine tests (browser_*.rs).

#![allow(dead_code, unused_imports)]

pub use snorkle_core::browser::{
    AfterCreate, Browser, Effect, NavErrorKind, NavKind, Pane, PopupStyle, SplitOrientation, SplitView, Verdict,
    GESTURE_WINDOW_MS, MAX_DIVERTS,
};
pub use snorkle_core::os::{OsSupport, OsVersion};
pub use snorkle_core::policy::Frame;
pub use snorkle_core::storage::{Session, SessionTab};
pub use snorkle_core::tabs::{MemoryBudget, Notice, Profile, TabId};
pub use snorkle_core::Url;

pub fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

pub fn browser(max_live: usize) -> Browser {
    Browser::new(MemoryBudget { max_live, background_ttl_ms: 600_000 }, OsSupport::Supported)
}

/// Simulates WebKit loading `url` successfully in `tab`.
pub fn webkit_load(b: &mut Browser, tab: TabId, url: &str) -> Vec<Effect> {
    let url = u(url);
    let (v, mut out) = b.decide_navigation(tab, &url, Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow, "{url}");
    out.extend(b.on_provisional_start(tab));
    out.extend(b.on_commit(tab, url));
    out.extend(b.on_finish(tab));
    out
}

pub fn displayed(b: &Browser, tab: TabId) -> Option<String> {
    b.view().tabs.into_iter().find(|t| t.id == tab).and_then(|t| t.url).map(|d| d.full)
}

pub fn notice(b: &Browser, tab: TabId) -> Option<Notice> {
    b.view().tabs.into_iter().find(|t| t.id == tab).and_then(|t| t.notice)
}

pub fn open_and_load(b: &mut Browser, url: &str, now: u64) -> (TabId, Vec<Effect>) {
    let (t, mut out) = b.new_tab(Profile::Default, now);
    out.extend(b.navigate_input(t, url, now).unwrap());
    out.extend(webkit_load(b, t, url));
    (t, out)
}

pub fn block(b: &mut Browser, patterns: &[&str]) -> Vec<Effect> {
    use snorkle_core::blocklist::{BlockList, BlockSettings};
    b.set_blocklist(BlockList::from_settings(BlockSettings {
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        message: "Not now.".into(),
    }))
}

pub fn user_blocked(b: &Browser, tab: TabId) -> bool {
    matches!(notice(b, tab), Some(Notice::UserBlocked { ref message, .. }) if message == "Not now.")
}

pub fn saved_copy(page: &str, pages: &[&str]) -> snorkle_core::tabs::SavedCopy {
    snorkle_core::tabs::SavedCopy {
        item: "l1-0".into(),
        page: u(page),
        pages: pages.iter().map(|p| u(p)).collect(),
        saved_ms: 7,
        committed: false,
        picking: None,
        adding: false,
    }
}

/// The host loading the archive: WebKit asks about its base URL, commits.
pub fn load_saved(b: &mut Browser, tab: TabId, page: &str) {
    let (v, _) = b.decide_navigation(tab, &u(page), Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow, "the archive's own load");
    b.on_provisional_start(tab);
    b.on_commit(tab, u(page));
    let out = b.on_finish(tab);
    assert!(!out.iter().any(|e| matches!(e, Effect::RecordHistory { .. })), "a saved copy is not a visit");
}
