//! Security/performance invariants of the `Browser` state machine:
//! address bar shows committed URLs only, HTTPS-first never downgrades
//! silently, private tabs never reach storage, and the memory budget holds.

use snorkle_core::browser::{AfterCreate, Browser, Effect, NavErrorKind, NavKind, Verdict};
use snorkle_core::os::{OsSupport, OsVersion};
use snorkle_core::policy::Frame;
use snorkle_core::storage::{Session, SessionTab};
use snorkle_core::tabs::{MemoryBudget, Notice, Profile, TabId};
use snorkle_core::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn browser(max_live: usize) -> Browser {
    Browser::new(MemoryBudget { max_live, background_ttl_ms: 600_000 }, OsSupport::Supported)
}

/// Simulates WebKit loading `url` successfully in `tab`.
fn webkit_load(b: &mut Browser, tab: TabId, url: &str) -> Vec<Effect> {
    let url = u(url);
    let (v, mut out) = b.decide_navigation(tab, &url, Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow, "{url}");
    out.extend(b.on_provisional_start(tab));
    out.extend(b.on_commit(tab, url));
    out.extend(b.on_finish(tab));
    out
}

fn displayed(b: &Browser, tab: TabId) -> Option<String> {
    b.view().tabs.into_iter().find(|t| t.id == tab).and_then(|t| t.url).map(|d| d.full)
}

fn notice(b: &Browser, tab: TabId) -> Option<Notice> {
    b.view().tabs.into_iter().find(|t| t.id == tab).and_then(|t| t.notice)
}

// ---- address bar -----------------------------------------------------------

#[test]
fn address_bar_ignores_provisional_and_cross_origin_url_changes() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://good.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://good.example/");
    assert_eq!(displayed(&b, t).unwrap(), "https://good.example/");

    // Same-origin pushState: accepted.
    b.on_url_changed(t, u("https://good.example/inbox"));
    assert_eq!(displayed(&b, t).unwrap(), "https://good.example/inbox");

    // A KVO URL change to another origin outside a commit: ignored.
    b.on_url_changed(t, u("https://evil.example/"));
    assert_eq!(displayed(&b, t).unwrap(), "https://good.example/inbox");

    // A provisional navigation to evil: not shown until it commits.
    let evil = u("https://evil.example/");
    let (v, _) = b.decide_navigation(t, &evil, Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Allow);
    b.on_provisional_start(t);
    b.on_url_changed(t, evil.clone());
    assert_eq!(displayed(&b, t).unwrap(), "https://good.example/inbox");

    // If it fails, the bar still shows the page that is actually rendered.
    b.on_fail(t, true, NavErrorKind::Network, Some(evil.clone()), "offline");
    assert_eq!(displayed(&b, t).unwrap(), "https://good.example/inbox");

    // If it commits, the bar updates.
    b.on_commit(t, evil);
    assert_eq!(displayed(&b, t).unwrap(), "https://evil.example/");
}

// ---- HTTPS-first -----------------------------------------------------------

#[test]
fn typed_http_is_upgraded() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    let out = b.navigate_input(t, "http://site.example/a", 0).unwrap();
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { then: AfterCreate::Load(url), .. } if url.as_str() == "https://site.example/a")));
    assert!(!out.iter().any(|e| matches!(e, Effect::Load { url, .. } | Effect::CreateView { then: AfterCreate::Load(url), .. } if url.scheme() == "http")));
}

#[test]
fn https_failure_shows_interstitial_never_downgrades() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "http://legacy.example/", 0).unwrap();
    let https = u("https://legacy.example/");
    let (v, _) = b.decide_navigation(t, &https, Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow);
    b.on_provisional_start(t);
    let out = b.on_fail(t, true, NavErrorKind::Network, Some(https), "could not connect");
    assert!(!out.iter().any(|e| matches!(e, Effect::Load { .. })), "no automatic downgrade");
    assert!(matches!(notice(&b, t), Some(Notice::HttpsUnavailable { .. })));

    // Only an explicit user decision loads http, and it is remembered for the
    // host in this profile only.
    let out = b.proceed_http(t, 0);
    assert!(out.iter().any(|e| matches!(e, Effect::Load { url, .. } if url.as_str() == "http://legacy.example/")));
    let http = u("http://legacy.example/");
    let (v, _) = b.decide_navigation(t, &http, Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow);

    let (p, _) = b.new_tab(Profile::Private, 0);
    let out = b.navigate_input(p, "http://legacy.example/", 0).unwrap();
    assert!(out
        .iter()
        .any(|e| matches!(e, Effect::CreateView { then: AfterCreate::Load(url), .. } if url.scheme() == "https")));
}

#[test]
fn certificate_error_on_explicit_https_is_not_bypassable() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://bad-cert.example/", 0).unwrap();
    let url = u("https://bad-cert.example/");
    b.decide_navigation(t, &url, Frame::Main, NavKind::Other, 0);
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Certificate, Some(url), "certificate invalid");
    match notice(&b, t) {
        Some(Notice::LoadError { certificate: true, .. }) => {}
        other => panic!("{other:?}"),
    }
    // proceed_http is a no-op for this notice.
    assert!(b.proceed_http(t, 0).is_empty());
}

#[test]
fn redirect_loop_back_to_http_shows_interstitial() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "http://loop.example/", 0).unwrap();
    let https = u("https://loop.example/");
    let http = u("http://loop.example/");
    assert_eq!(b.decide_navigation(t, &https, Frame::Main, NavKind::Other, 0).0, Verdict::Allow);
    // The server keeps redirecting https -> http.
    let mut loads = 0;
    for _ in 0..10 {
        let (v, out) = b.decide_navigation(t, &http, Frame::Main, NavKind::Other, 0);
        assert_eq!(v, Verdict::Cancel);
        if out.iter().any(|e| matches!(e, Effect::Load { .. })) {
            loads += 1;
            b.decide_navigation(t, &https, Frame::Main, NavKind::Other, 0);
        }
        if notice(&b, t).is_some() {
            break;
        }
    }
    assert!(loads < 3);
    assert!(matches!(notice(&b, t), Some(Notice::HttpsUnavailable { .. })));
}

#[test]
fn page_initiated_http_link_is_upgraded() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let (v, out) = b.decide_navigation(t, &u("http://b.example/x"), Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Cancel);
    assert!(out.iter().any(|e| matches!(e, Effect::Load { url, .. } if url.as_str() == "https://b.example/x")));
}

// ---- dangerous navigations -------------------------------------------------

#[test]
fn typed_javascript_and_data_never_reach_webkit() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    for input in [
        "javascript:alert(document.cookie)",
        "data:text/html,<script>x</script>",
        "file:///etc/hosts",
        "tauri://localhost/index.html",
        "ipc://localhost/navigate",
    ] {
        let out = b.navigate_input(t, input, 0).unwrap();
        assert!(
            !out.iter().any(|e| matches!(e, Effect::Load { .. } | Effect::CreateView { .. })),
            "{input} must not load: {out:?}"
        );
        assert!(matches!(notice(&b, t), Some(Notice::Blocked { .. })), "{input}");
    }
}

#[test]
fn external_app_requires_confirmation() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let (v, out) = b.decide_navigation(t, &u("tel:+15555550100"), Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Cancel);
    assert!(!out.iter().any(|e| matches!(e, Effect::OpenExternal(_))));
    assert!(matches!(notice(&b, t), Some(Notice::ExternalApp { .. })));
    let out = b.confirm_external(t);
    assert!(out.iter().any(|e| matches!(e, Effect::OpenExternal(_))));
    // Script-initiated (no gesture) external navigation: blocked outright.
    let (v, _) = b.decide_navigation(t, &u("tel:+15555550100"), Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Cancel);
    assert!(matches!(notice(&b, t), Some(Notice::Blocked { .. })));
}

#[test]
fn notices_hide_content_view() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    let out = webkit_load(&mut b, t, "https://a.example/");
    assert!(out.contains(&Effect::ShowView(t)));
    let out = b.navigate_input(t, "javascript:x", 0).unwrap();
    assert!(out.contains(&Effect::HideView(t)));
    assert!(!b.view().content_visible);
    let out = b.go_back(t);
    assert!(out.contains(&Effect::ShowView(t)));
}

// ---- private mode ----------------------------------------------------------

#[test]
fn private_tabs_never_reach_storage() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    let mut all = b.navigate_input(p, "https://secret.example/", 0).unwrap();
    all.extend(webkit_load(&mut b, p, "https://secret.example/"));
    all.extend(b.on_title(p, "Secret"));
    all.extend(b.on_url_changed(p, u("https://secret.example/2")));
    for e in &all {
        match e {
            Effect::RecordHistory { .. } => panic!("private history recorded"),
            Effect::PersistSession(s) => assert!(s.tabs.iter().all(|t| !t.url.contains("secret"))),
            _ => {}
        }
    }
    assert!(b.session().tabs.iter().all(|t| !t.url.contains("secret")));
    let closed = b.close(p, 1);
    assert!(closed.contains(&Effect::ReleasePrivateData));
    for e in &closed {
        if let Effect::PersistSession(s) = e {
            assert!(s.tabs.iter().all(|t| !t.url.contains("secret")));
        }
    }
}

#[test]
fn private_and_default_use_distinct_profiles() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    let out = b.navigate_input(p, "https://a.example/", 0).unwrap();
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { profile: Profile::Private, .. })));
    let (d, _) = b.new_tab(Profile::Default, 0);
    let out = b.navigate_input(d, "https://a.example/", 0).unwrap();
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { profile: Profile::Default, .. })));
}

#[test]
fn popups_inherit_profile() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/");
    let out = b.on_new_window_request(p, &u("https://b.example/"), 0);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { profile: Profile::Private, .. })));
}

#[test]
fn normal_tabs_are_recorded() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    let out = webkit_load(&mut b, t, "https://a.example/");
    assert!(out.iter().any(|e| matches!(e, Effect::RecordHistory { .. })));
    assert_eq!(b.session().tabs.len(), 1);
}

// ---- memory budget -----------------------------------------------------------

fn open_and_load(b: &mut Browser, url: &str, now: u64) -> (TabId, Vec<Effect>) {
    let (t, mut out) = b.new_tab(Profile::Default, now);
    out.extend(b.navigate_input(t, url, now).unwrap());
    out.extend(webkit_load(b, t, url));
    (t, out)
}

#[test]
fn live_tab_count_never_exceeds_budget() {
    let mut b = browser(3);
    for i in 0..20u64 {
        open_and_load(&mut b, &format!("https://site{i}.example/"), i * 1000);
        assert!(b.live_count() <= 3, "live = {}", b.live_count());
    }
    assert_eq!(b.view().tabs.len(), 20);
}

#[test]
fn lru_tab_is_discarded_with_state_and_restored_on_activation() {
    let mut b = browser(2);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (_bt, _) = open_and_load(&mut b, "https://b.example/", 10);
    let (_c, out) = open_and_load(&mut b, "https://c.example/", 20);
    assert!(out.contains(&Effect::DestroyView { tab: a, keep_state: true }));
    let out = b.activate(a, 30);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, then: AfterCreate::Restore { fallback: Some(u) }, .. } if *tab == a && u.as_str() == "https://a.example/")));
    assert!(b.live_count() <= 2);
}

#[test]
fn background_ttl_discards() {
    let mut b = Browser::new(MemoryBudget { max_live: 6, background_ttl_ms: 1_000 }, OsSupport::Supported);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    open_and_load(&mut b, "https://b.example/", 0);
    assert!(b.tick(500).is_empty());
    let out = b.tick(5_000);
    assert!(out.contains(&Effect::DestroyView { tab: a, keep_state: true }));
}

#[test]
fn memory_warning_keeps_only_active() {
    let mut b = browser(6);
    for i in 0..5 {
        open_and_load(&mut b, &format!("https://s{i}.example/"), i);
    }
    assert_eq!(b.live_count(), 5);
    b.on_memory_warning();
    assert_eq!(b.live_count(), 1);
}

#[test]
fn crash_loop_is_bounded() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://heavy.example/", 0);
    assert!(b.on_process_terminated(t, 1).contains(&Effect::Reload(t)));
    assert!(b.on_process_terminated(t, 2).contains(&Effect::Reload(t)));
    let out = b.on_process_terminated(t, 3);
    assert!(!out.contains(&Effect::Reload(t)));
    assert_eq!(notice(&b, t), Some(Notice::Crashed));
}

#[test]
fn new_tab_creates_no_web_view_until_navigation() {
    let mut b = browser(4);
    let (_, out) = b.new_tab(Profile::Default, 0);
    assert!(!out.iter().any(|e| matches!(e, Effect::CreateView { .. })));
}

#[test]
fn session_restore_is_lazy() {
    let mut b = browser(4);
    let s = Session {
        tabs: (0..10).map(|i| SessionTab { url: format!("https://s{i}.example/"), title: String::new() }).collect(),
        active: Some(3),
    };
    let out = b.restore_session(&s, 0);
    let creates = out.iter().filter(|e| matches!(e, Effect::CreateView { .. })).count();
    assert_eq!(creates, 1);
    assert_eq!(b.view().tabs.len(), 10);
}

// ---- OS gate ---------------------------------------------------------------

#[test]
fn unsupported_os_creates_no_web_views() {
    let os = snorkle_core::os::check(OsVersion::new(18, 0, 0));
    let mut b = Browser::new(MemoryBudget::for_physical_memory(8 << 30), os);
    let (t, _) = b.new_tab(Profile::Default, 0);
    let out = b.navigate_input(t, "https://example.com/", 0).unwrap();
    assert!(!out.iter().any(|e| matches!(e, Effect::CreateView { .. } | Effect::Load { .. })));
}
