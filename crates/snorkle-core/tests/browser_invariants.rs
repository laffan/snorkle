//! Security/performance invariants of the `Browser` state machine:
//! address bar shows committed URLs only, HTTPS-first never downgrades
//! silently, private tabs never reach storage, and the memory budget holds.

use snorkle_core::browser::{
    AfterCreate, Browser, Effect, NavErrorKind, NavKind, Pane, PopupStyle, SplitOrientation, SplitView, Verdict,
};
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
    assert!(out.contains(&Effect::ShowView { tab: t, pane: Pane::Full }));
    let out = b.navigate_input(t, "javascript:x", 0).unwrap();
    assert!(out.contains(&Effect::HideView(t)));
    assert!(!b.view().content_visible);
    let out = b.go_back(t, 0);
    assert!(out.contains(&Effect::ShowView { tab: t, pane: Pane::Full }));
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
fn new_window_fallback_inherits_profile() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/");
    let out = b.on_new_window_request(p, &u("https://b.example/"), 0);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { profile: Profile::Private, .. })));
}

#[test]
fn view_exemptions_are_saved_with_the_session() {
    use snorkle_core::viewmodes::Kind;
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let out = b.set_view_exempt(t, &[Kind::HideMedia, Kind::Desaturate, Kind::HideMedia, Kind::Headers]);
    assert!(out.iter().any(|e| matches!(e, Effect::PersistSession(_))));
    let session = b.session();
    assert_eq!(session.tabs[0].exempt, vec![Kind::Desaturate, Kind::HideMedia], "sorted, unique, no headers");
    let mut b2 = browser(4);
    b2.restore_session(&session, 0);
    let ex = b2.view_exemptions();
    assert_eq!(ex.len(), 1);
    assert_eq!(ex[0].1, vec![Kind::Desaturate, Kind::HideMedia]);
    let (p, _) = b.new_tab(Profile::Private, 1);
    assert!(b.set_view_exempt(p, &[Kind::Dark]).is_empty(), "private tabs are never saved");
}

#[test]
fn launch_greets_with_a_new_tab_after_the_saved_active_one() {
    let mut b = browser(4);
    for (i, url) in ["https://a.example/", "https://b.example/", "https://c.example/"].iter().enumerate() {
        let (t, _) = b.new_tab(Profile::Default, i as u64);
        b.navigate_input(t, url, i as u64).unwrap();
        webkit_load(&mut b, t, url);
    }
    let first = b.view().tabs[0].id;
    b.activate(first, 5);
    let session = b.session();
    assert_eq!(session.active, Some(0));

    let mut b2 = browser(4);
    let (start, out) = b2.restore_session_with_start_tab(&session, 10);
    assert!(!out.iter().any(|e| matches!(e, Effect::CreateView { .. })), "no restored page is loaded");
    assert!(out.contains(&Effect::NotifyChrome));
    let view = b2.view();
    assert_eq!(view.active, Some(start));
    assert_eq!(view.tabs.len(), 4);
    assert_eq!(view.tabs[1].id, start, "placed after the saved active tab");
    assert!(view.tabs[1].url.is_none(), "an empty tab: the start page");
    // An empty tab is never saved, so the next launch does not pile them up.
    assert_eq!(b2.session().tabs.len(), 3);

    // Nothing saved: just the new tab.
    let mut b3 = browser(4);
    let (start, _) = b3.restore_session_with_start_tab(&Session::default(), 0);
    assert_eq!(b3.view().tabs.len(), 1);
    assert_eq!(b3.view().active, Some(start));
}

#[test]
fn page_zoom_is_per_tab_and_saved_with_the_session() {
    use snorkle_core::tabs::ZoomStep;
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let (other, _) = b.new_tab(Profile::Default, 1);
    b.navigate_input(other, "https://b.example/", 1).unwrap();
    webkit_load(&mut b, other, "https://b.example/");

    let out = b.zoom(t, ZoomStep::In);
    assert!(out.contains(&Effect::SetZoom { tab: t, percent: 110 }));
    assert!(out.iter().any(|e| matches!(e, Effect::PersistSession(_))));
    b.zoom(t, ZoomStep::In);
    let view = b.view();
    assert_eq!(view.tabs.iter().find(|v| v.id == t).unwrap().zoom, 125);
    assert_eq!(view.tabs.iter().find(|v| v.id == other).unwrap().zoom, 100, "other tabs keep theirs");

    let session = b.session();
    assert_eq!(session.tabs[0].zoom, Some(125));
    assert_eq!(session.tabs[1].zoom, None, "100 % is not written");

    // Restored, the tab's new web view gets the zoom right after creation.
    let mut b2 = browser(4);
    let out = b2.restore_session(&Session { active: Some(0), ..session.clone() }, 0);
    let create = out.iter().position(|e| matches!(e, Effect::CreateView { .. })).expect("active tab is created");
    assert!(matches!(out[create + 1], Effect::SetZoom { percent: 125, .. }));
    let mut b3 = browser(4);
    let out = b3.restore_session(&Session { active: Some(1), ..session }, 0);
    assert!(!out.iter().any(|e| matches!(e, Effect::SetZoom { .. })), "nothing to set at 100 %");

    assert!(b.zoom(t, ZoomStep::Reset).contains(&Effect::SetZoom { tab: t, percent: 100 }));
    assert!(b.zoom(t, ZoomStep::Reset).is_empty(), "no change, no effects");

    let (p, _) = b.new_tab(Profile::Private, 2);
    b.navigate_input(p, "https://c.example/", 2).unwrap();
    webkit_load(&mut b, p, "https://c.example/");
    let out = b.zoom(p, ZoomStep::Out);
    assert!(out.contains(&Effect::SetZoom { tab: p, percent: 90 }));
    assert!(!out.iter().any(|e| matches!(e, Effect::PersistSession(_))), "private tabs are never saved");
}

#[test]
fn offline_copy_replaces_a_network_error_once() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let page = u("https://news.example/");
    // Not after a different kind of failure, or for another URL.
    assert!(!b.use_offline_copy(t, &page, 5).0, "no error shown");
    b.navigate_trusted(t, page.clone(), 1);
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Network, Some(page.clone()), "offline");
    assert!(!b.use_offline_copy(t, &u("https://other.example/"), 5).0);
    let (ok, _) = b.use_offline_copy(t, &page, 5);
    assert!(ok);
    assert!(b.view().tabs[0].notice.is_none(), "the error gives way to the copy");
    // The copy's load (base URL = the page) is the browser's own.
    let (v, _) = b.decide_navigation(t, &page, Frame::Main, NavKind::Other, 2);
    assert_eq!(v, Verdict::Allow);
    b.on_provisional_start(t);
    b.on_commit(t, page.clone());
    let out = b.on_finish(t);
    assert_eq!(b.view().tabs[0].offline_saved_ms, Some(5), "the address bar says it is a copy");
    assert!(!out.iter().any(|e| matches!(e, Effect::RecordHistory { .. })), "not a visit");
    assert!(b.shows_offline_copy(t));
    // The next load replaces it.
    b.on_provisional_start(t);
    assert_eq!(b.view().tabs[0].offline_saved_ms, None);
    // A copy whose own load fails is not tried again.
    b.on_fail(t, true, NavErrorKind::Network, Some(page.clone()), "offline");
    assert!(b.use_offline_copy(t, &page, 5).0);
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Other, Some(page.clone()), "bad archive");
    assert!(!b.shows_offline_copy(t));
    b.on_fail(t, true, NavErrorKind::Network, Some(page.clone()), "offline");
    assert!(!b.use_offline_copy(t, &page, 5).0, "tried already since the last commit");
    // Never in private tabs.
    let (p, _) = b.new_tab(Profile::Private, 3);
    b.navigate_trusted(p, page.clone(), 3);
    b.on_provisional_start(p);
    b.on_fail(p, true, NavErrorKind::Network, Some(page.clone()), "offline");
    assert!(!b.use_offline_copy(p, &page, 5).0);
}

#[test]
fn opening_a_launcher_marks_its_landing_load() {
    let mut b = browser(4);
    let (t, _) = b.open_launcher("https://nytimes.com/", u("https://nytimes.com/"), false, 0);
    assert_eq!(b.landing(t).as_deref(), Some("https://nytimes.com/"));
    webkit_load(&mut b, t, "https://www.nytimes.com/");
    assert_eq!(b.landing(t), None, "cleared when that load finishes");
}

#[test]
fn split_shortcuts_add_an_empty_tab_or_change_the_layout() {
    let mut b = browser(4);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (n, out) = b.split_new(SplitOrientation::Columns, 1);
    let n = n.expect("a new tab");
    assert_eq!(b.active(), Some(n), "the new tab is focused");
    assert_eq!(
        b.view().split,
        Some(SplitView { left: a, right: n, orientation: SplitOrientation::Columns, ratio: 500 })
    );
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Left }), "the page keeps the left half");
    // Same shortcut again: nothing.
    let (again, out) = b.split_new(SplitOrientation::Columns, 2);
    assert!(again.is_none() && out.is_empty());
    // The other one: the same pair, one above the other; no third pane.
    let (again, out) = b.split_new(SplitOrientation::Rows, 3);
    assert!(again.is_none());
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Top }));
    assert_eq!(b.view().split.map(|s| s.orientation), Some(SplitOrientation::Rows));
    assert_eq!(b.view().tabs.len(), 2);
    // From the other half too.
    b.activate(a, 4);
    let (_, out) = b.split_new(SplitOrientation::Columns, 5);
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Left }));
    // A private tab's new pane is private.
    let mut b = browser(4);
    let (pv, _) = b.new_tab(Profile::Private, 0);
    let (n, _) = b.split_new(SplitOrientation::Rows, 1);
    assert!(b.view().tabs.iter().any(|t| Some(t.id) == n && t.private));
    assert_eq!(b.view().split.map(|s| (s.left, s.orientation)), Some((pv, SplitOrientation::Rows)));
    // Not with a budget of one live view.
    let mut b = browser(1);
    open_and_load(&mut b, "https://a.example/", 0);
    assert!(b.split_new(SplitOrientation::Columns, 1).0.is_none());
}

#[test]
fn split_divider_sets_the_ratio() {
    let mut b = browser(4);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    b.split(c, a, 2);
    assert_eq!(b.set_split_ratio(650), vec![Effect::SetSplitRatio(650), Effect::NotifyChrome]);
    assert_eq!(b.view().split.map(|s| s.ratio), Some(650));
    assert!(b.set_split_ratio(650).is_empty(), "no change");
    assert_eq!(b.set_split_ratio(990), vec![Effect::SetSplitRatio(800), Effect::NotifyChrome], "clamped");
    assert_eq!(b.set_split_ratio(0).first(), Some(&Effect::SetSplitRatio(200)));
    // Another pair comes on screen with its own ratio, sent before its panes.
    let (d, _) = open_and_load(&mut b, "https://d.example/", 3);
    let (e, _) = open_and_load(&mut b, "https://e.example/", 4);
    b.split(e, d, 5);
    b.activate(a, 6);
    let out = b.activate(d, 7);
    let ratio = out.iter().position(|x| *x == Effect::SetSplitRatio(500)).expect("the new pair's ratio");
    let shown = out.iter().position(|x| matches!(x, Effect::ShowView { .. })).expect("its panes");
    assert!(ratio < shown);
    // Not without a pair on screen.
    b.unsplit(d);
    assert!(b.set_split_ratio(300).is_empty());
}

#[test]
fn saved_zoom_is_snapped_to_a_level() {
    let session = Session {
        tabs: vec![SessionTab {
            url: "https://a.example/".into(),
            title: String::new(),
            launcher: None,
            exempt: vec![],
            zoom: Some(4000),
            url_bar_attached: false,
            name: None,
            emoji: None,
        }],
        active: Some(0),
    };
    let mut b = browser(4);
    b.restore_session(&session, 0);
    assert_eq!(b.view().tabs[0].zoom, 150);
}

#[test]
fn bookmarking_moves_the_tab() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://a.example/");
    let (ok, out) = b.bind_launcher(t, "https://a.example/");
    assert!(ok && out.iter().any(|e| matches!(e, Effect::PersistSession(_))));
    assert_eq!(b.tab(t).unwrap().launcher.as_deref(), Some("https://a.example/"));
    assert_eq!(b.session().tabs[0].launcher.as_deref(), Some("https://a.example/"));
    // Opening the bookmark returns to this tab instead of making another.
    let (id, _) = b.open_launcher("https://a.example/", u("https://a.example/"), false, 1);
    assert_eq!(id, t);
    let (t2, _) = b.new_tab(Profile::Default, 2);
    assert!(!b.bind_launcher(t2, "https://a.example/").0, "one tab per item");
    let (p, _) = b.new_tab(Profile::Private, 3);
    assert!(!b.bind_launcher(p, "https://p.example/").0, "private tabs are never bound");
    b.release_launcher("https://a.example/");
    assert!(b.tab(t).unwrap().launcher.is_none(), "un-bookmarking makes it a normal tab");
}

#[test]
fn command_click_opens_a_background_tab() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/");
    let (t, out) = b.on_background_tab_request(p, &u("http://b.example/x"), 5);
    let t = t.expect("a tab is opened");
    assert_eq!(b.active(), Some(p), "the current tab stays in front");
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, profile: Profile::Private, .. } if *tab == t)));
    assert!(!out.iter().any(|e| matches!(e, Effect::ShowView { tab, .. } if *tab == t)), "loaded hidden");
    assert!(
        out.iter()
            .any(|e| matches!(e, Effect::CreateView { then: AfterCreate::Load(url), .. } if url.scheme() == "https")),
        "http upgraded"
    );
    // Blocked and other-app links open nothing.
    assert!(b.on_background_tab_request(p, &u("javascript:alert(1)"), 6).0.is_none());
    assert!(b.on_background_tab_request(p, &u("tel:123"), 6).0.is_none());
    assert!(b.on_background_tab_request(TabId(999), &u("https://c.example/"), 6).0.is_none());
}

#[test]
fn back_from_a_command_clicked_tab_returns_to_the_page_it_came_from() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(p, "https://a.example/list", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/list");
    let (t, _) = b.on_background_tab_request(p, &u("https://b.example/story"), 5);
    let t = t.unwrap();
    webkit_load(&mut b, t, "https://b.example/story");
    let view = |b: &Browser| b.view().tabs.into_iter().find(|v| v.id == t).unwrap();
    assert!(view(&b).can_go_back, "Back is offered at once");
    b.activate(t, 6);
    let out = b.go_back(t, 7);
    assert!(
        out.iter().any(|e| matches!(e, Effect::Load { url, .. } if url.as_str() == "https://a.example/list")),
        "loads the opener's page: {out:?}"
    );
    webkit_load(&mut b, t, "https://a.example/list");
    b.on_history_state(t, true, false);
    let out = b.go_back(t, 8);
    assert!(out.contains(&Effect::GoBack(t)), "then WebKit's own history");
    b.on_history_state(t, false, true);
    assert!(!view(&b).can_go_back, "the opener is offered once");
    // Tabs opened otherwise have no such Back.
    let (n, _) = b.new_tab(Profile::Default, 9);
    assert!(!b.view().tabs.into_iter().find(|v| v.id == n).unwrap().can_go_back);
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
        tabs: (0..10)
            .map(|i| SessionTab {
                url: format!("https://s{i}.example/"),
                title: String::new(),
                launcher: None,
                exempt: vec![],
                zoom: None,
                url_bar_attached: false,
                name: None,
                emoji: None,
            })
            .collect(),
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
    assert!(!b.view().tabs[0].loading, "no view can load, so the tab must not show progress");
}

#[test]
fn preview_only_platform_never_shows_loading() {
    let mut b = Browser::new(MemoryBudget::for_physical_memory(8 << 30), OsSupport::PreviewOnly);
    let (t, _) = b.new_tab(Profile::Default, 0);
    let out = b.navigate_input(t, "example.com", 0).unwrap();
    assert!(!out.iter().any(|e| matches!(e, Effect::CreateView { .. } | Effect::Load { .. })));
    assert!(!b.view().tabs[0].loading);
}

// ---- ad blocking -------------------------------------------------------------

#[test]
fn adblock_state_per_site_and_private_tabs_cannot_toggle() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://www.news.example/", 0).unwrap();
    webkit_load(&mut b, t, "https://www.news.example/");
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://www.news.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://www.news.example/");

    let mut allow = std::collections::BTreeSet::new();
    allow.insert("news.example".to_string());
    b.set_adblock_allowlist(allow);
    let view = b.view();
    let a = view.tabs.iter().find(|x| x.id == t).unwrap().adblock.clone().unwrap();
    assert_eq!(a.site, "news.example");
    assert!(!a.blocking);
    assert!(a.can_toggle);
    let pa = view.tabs.iter().find(|x| x.id == p).unwrap().adblock.clone().unwrap();
    assert!(!pa.can_toggle, "private tabs must not persist per-site settings");
}

// ---- split view ----------------------------------------------------------------

#[test]
fn split_shows_both_tabs_side_by_side() {
    let mut b = browser(4);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let out = b.split(c, a, 2);
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Left }));
    assert!(out.contains(&Effect::ShowView { tab: c, pane: Pane::Right }));
    assert_eq!(
        b.view().split,
        Some(SplitView { left: a, right: c, orientation: SplitOrientation::Columns, ratio: 500 })
    );
    assert_eq!(b.active(), Some(c));
    // Focusing the other pane keeps the split.
    b.activate(a, 3);
    assert!(b.view().split.is_some());
}

#[test]
fn split_pairs_survive_choosing_another_tab_and_end_when_a_pane_closes() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 2);
    b.split(c, a, 3);
    let out = b.activate(d, 4);
    assert!(b.view().split.is_none(), "a tab outside the pair is shown alone");
    assert_eq!(
        b.view().splits,
        vec![SplitView { left: a, right: c, orientation: SplitOrientation::Columns, ratio: 500 }],
        "the pair is kept"
    );
    assert!(out.contains(&Effect::HideView(a)) && out.contains(&Effect::HideView(c)));
    assert!(out.contains(&Effect::ShowView { tab: d, pane: Pane::Full }));
    // Choosing either tab of the pair shows the pair again.
    let out = b.activate(a, 5);
    assert_eq!(
        b.view().split,
        Some(SplitView { left: a, right: c, orientation: SplitOrientation::Columns, ratio: 500 })
    );
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Left }));
    assert!(out.contains(&Effect::ShowView { tab: c, pane: Pane::Right }));
    b.close(a, 6);
    assert!(b.view().splits.is_empty());
    assert!(b.view().split.is_none());
}

#[test]
fn several_pairs_and_repairing() {
    let mut b = browser(8);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 2);
    let (e, _) = open_and_load(&mut b, "https://e.example/", 3);
    b.split(c, a, 4);
    b.split(e, d, 5);
    assert_eq!(
        b.view().splits,
        vec![
            SplitView { left: a, right: c, orientation: SplitOrientation::Columns, ratio: 500 },
            SplitView { left: d, right: e, orientation: SplitOrientation::Columns, ratio: 500 }
        ]
    );
    assert_eq!(
        b.view().split,
        Some(SplitView { left: d, right: e, orientation: SplitOrientation::Columns, ratio: 500 })
    );
    // Pairing tabs from two pairs ends both and makes one new pair.
    b.split(d, a, 6);
    assert_eq!(
        b.view().splits,
        vec![SplitView { left: a, right: d, orientation: SplitOrientation::Columns, ratio: 500 }]
    );
}

#[test]
fn dragging_a_tab_out_of_its_pair_unsplits_it() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 2);
    b.split(c, a, 3);
    b.activate(d, 4);
    // Unsplitting a pair that is not on screen changes nothing visible.
    assert!(!b.unsplit(a).iter().any(|e| matches!(e, Effect::ShowView { .. } | Effect::HideView(_))));
    assert!(b.view().splits.is_empty());
    assert!(b.unsplit(a).is_empty(), "no pair left");
    // Unsplitting the pair on screen leaves the active tab full width.
    b.split(c, a, 5);
    let out = b.unsplit(a);
    assert_eq!(b.active(), Some(c));
    assert!(out.contains(&Effect::HideView(a)));
    assert!(out.contains(&Effect::ShowView { tab: c, pane: Pane::Full }));
}

#[test]
fn private_and_default_tabs_are_never_paired() {
    let mut b = browser(4);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (p, _) = b.new_tab(Profile::Private, 1);
    assert!(b.split(p, a, 2).is_empty());
    assert!(b.view().splits.is_empty());
}

#[test]
fn hidden_pair_can_be_discarded_and_comes_back_whole() {
    let mut b = browser(2);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    b.split(c, a, 2);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 3);
    assert!(b.live_count() <= 2);
    let out = b.activate(a, 4);
    assert_eq!(
        b.view().split,
        Some(SplitView { left: a, right: c, orientation: SplitOrientation::Columns, ratio: 500 })
    );
    assert_eq!(b.live_count(), 2);
    let created: Vec<_> = out
        .iter()
        .filter_map(|e| match e {
            Effect::CreateView { tab, .. } => Some(*tab),
            _ => None,
        })
        .collect();
    assert!(!created.is_empty(), "discarded panes are restored");
    assert!(b.tab(d).is_some());
}

#[test]
fn split_partner_is_never_discarded_and_restores_when_unloaded() {
    let mut b = browser(2);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    open_and_load(&mut b, "https://b.example/", 1);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 2);
    // With a budget of 2, `a` was discarded; splitting brings it back.
    let out = b.split(c, a, 3);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, .. } if *tab == a)));
    b.on_memory_warning();
    assert_eq!(b.live_count(), 2, "both panes stay live under memory pressure");
    assert!(b.live_count() <= 2);
    for _ in 0..5 {
        b.tick(10_000_000);
    }
    assert_eq!(b.live_count(), 2);
}

#[test]
fn split_refused_when_budget_cannot_hold_two_views() {
    // Found by fuzzing (fuzz/fuzz_targets/browser_events.rs).
    let mut b = browser(1);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    assert!(b.split(c, a, 2).is_empty());
    assert!(b.live_count() <= 1);
}

#[test]
fn split_with_self_is_ignored() {
    let mut b = browser(4);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    assert!(b.split(a, a, 1).is_empty());
    assert!(b.view().split.is_none());
}

// ---- links from other apps (default browser) -----------------------------------

#[test]
fn links_from_other_apps_open_in_a_new_tab() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (id, out) = b.open_from_other_app(u("https://news.example/story"), 1);
    let id = id.expect("opened");
    assert_ne!(id, t);
    assert_eq!(b.active(), Some(id));
    assert!(out.iter().any(|e| matches!(e, Effect::Load { tab, .. } | Effect::CreateView { tab, .. } if *tab == id)));
    for bad in
        ["javascript:alert(1)", "file:///etc/hosts", "https://user:pw@bank.example/", "data:text/html,x", "tel:123"]
    {
        assert!(b.open_from_other_app(u(bad), 2).0.is_none(), "{bad}");
    }
    // http is still upgraded by the normal policy.
    let (h, out) = b.open_from_other_app(u("http://plain.example/"), 3);
    assert!(h.is_some());
    assert!(out.iter().any(|e| matches!(e, Effect::Load { url, .. } | Effect::CreateView { then: AfterCreate::Load(url), .. } if url.scheme() == "https")));
}

// ---- pins and bookmarks own a tab -----------------------------------------

#[test]
fn launcher_reuses_its_tab_and_survives_restart() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);
    let key = "https://pin.example/";
    let (p, out) = b.open_launcher(key, u(key), false, 1);
    assert_ne!(p, t);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { .. } | Effect::Load { .. })));
    webkit_load(&mut b, p, key);
    // Navigating inside the pin's tab keeps it bound.
    webkit_load(&mut b, p, "https://pin.example/page");
    b.activate(t, 2);
    let (again, out) = b.open_launcher(key, u(key), false, 3);
    assert_eq!(again, p, "no new tab");
    assert!(!out.iter().any(|e| matches!(e, Effect::Load { .. })), "back to the tab as it was left");
    assert_eq!(b.active(), Some(p));
    b.activate(t, 3);
    let (again, out) = b.open_launcher(key, u(key), true, 3);
    assert_eq!(again, p);
    assert!(
        out.iter().any(|e| matches!(e, Effect::Load { tab, url } if *tab == p && url.as_str() == key)),
        "Return to Original URL: back to the pinned page"
    );
    webkit_load(&mut b, p, key);
    let (_, out) = b.open_launcher(key, u(key), true, 4);
    assert!(!out.iter().any(|e| matches!(e, Effect::Load { .. })), "already there: no reload");
    assert_eq!(b.active(), Some(p));
    assert_eq!(b.view().tabs.len(), 2);
    assert_eq!(b.view().tabs.iter().find(|x| x.id == p).unwrap().launcher.as_deref(), Some(key));
    // Saved and restored with its binding.
    let session = b.session();
    assert!(session.tabs.iter().any(|s| s.launcher.as_deref() == Some(key)));
    let mut b2 = browser(4);
    b2.restore_session(&session, 0);
    assert!(b2.view().tabs.iter().any(|x| x.launcher.as_deref() == Some(key)));
    // Removing the pin turns its tab into an ordinary tab.
    b.release_launcher(key);
    assert!(b.view().tabs.iter().all(|x| x.launcher.is_none()));
    assert_eq!(b.view().tabs.len(), 2);
}

// ---- popups (window.open with opener) -------------------------------------

#[test]
fn popup_keeps_opener_alive_and_returns_to_it_on_close() {
    let mut b = browser(2);
    let (op, _) = open_and_load(&mut b, "https://www.pinterest.com/", 0);
    let (popup, out) = b.open_popup(op, Some(&u("https://accounts.google.com/o/oauth2/auth")), PopupStyle::Tab, 1);
    let popup = popup.expect("popup allowed");
    assert!(out.contains(&Effect::AdoptPopup { tab: popup }));
    assert!(!out.iter().any(|e| matches!(e, Effect::CreateView { .. })), "WebKit supplies the popup view");
    assert_eq!(b.active(), Some(popup));
    assert_eq!(b.tab(popup).unwrap().profile, Profile::Default);
    // Memory pressure must not kill the opener while its popup is in front.
    b.on_memory_warning();
    assert_eq!(b.live_count(), 2);
    // window.close() from the popup returns to the opener.
    let out = b.on_close_requested(popup, 2);
    assert!(out.contains(&Effect::DestroyView { tab: popup, keep_state: false }));
    assert_eq!(b.active(), Some(op));
}

#[test]
fn popup_sheet_shows_over_its_running_opener() {
    let mut b = browser(4);
    let (op, _) = open_and_load(&mut b, "https://www.pinterest.com/", 0);
    let auth = "https://accounts.google.com/o/oauth2/auth";
    let (sheet, out) = b.open_popup(op, Some(&u(auth)), PopupStyle::Sheet, 1);
    let sheet = sheet.expect("popup allowed");
    assert!(out.contains(&Effect::AdoptPopup { tab: sheet }));
    assert_eq!(b.active(), Some(sheet), "the address bar shows the sheet's URL");
    assert_eq!(b.view().sheet, Some(sheet));
    // Before the sheet commits, the opener stays on screen (and running).
    assert!(!out.contains(&Effect::HideView(op)));
    let out = webkit_load(&mut b, sheet, auth);
    assert!(out.contains(&Effect::ShowView { tab: sheet, pane: Pane::Sheet }));
    assert!(!out.contains(&Effect::HideView(op)));
    let v = b.view();
    assert_eq!(v.tabs.iter().find(|t| t.id == sheet).unwrap().sheet_over, Some(op));
    assert!(!b.session().tabs.iter().any(|t| t.url.starts_with(auth)), "sheets are never saved");
    // Choosing another tab hides both; choosing the opener brings the sheet back in front.
    let (other, _) = open_and_load(&mut b, "https://c.example/", 2);
    assert_eq!(b.view().sheet, None);
    let out = b.activate(op, 3);
    assert_eq!(b.active(), Some(sheet));
    let shown: Vec<_> = out
        .iter()
        .filter_map(|e| match e {
            Effect::ShowView { tab, pane } => Some((*tab, *pane)),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![(op, Pane::Full), (sheet, Pane::Sheet)], "the sheet is shown last, on top");
    assert!(out.contains(&Effect::HideView(other)));
    // window.close() returns to the opener, full width.
    let out = b.on_close_requested(sheet, 4);
    assert_eq!(b.active(), Some(op));
    assert!(out.contains(&Effect::DestroyView { tab: sheet, keep_state: false }));
    assert_eq!(b.view().sheet, None);
}

#[test]
fn sheet_stays_on_top_when_the_opener_is_shown_again() {
    let mut b = browser(4);
    let (op, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (sheet, _) = b.open_popup(op, Some(&u("https://login.example/")), PopupStyle::Sheet, 1);
    let sheet = sheet.unwrap();
    webkit_load(&mut b, sheet, "https://login.example/");
    // The opener shows a notice (hidden), then is shown again under the sheet.
    let (_, out) = b.decide_navigation(op, &u("tel:+15555550100"), Frame::Main, NavKind::LinkActivated, 2);
    assert!(out.contains(&Effect::HideView(op)));
    let out = b.dismiss_notice(op);
    let shown: Vec<_> = out
        .iter()
        .filter_map(|e| match e {
            Effect::ShowView { tab, pane } => Some((*tab, *pane)),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![(op, Pane::Full), (sheet, Pane::Sheet)]);
}

#[test]
fn notice_in_a_sheet_is_drawn_by_the_chrome() {
    let mut b = browser(4);
    let (op, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (sheet, _) = b.open_popup(op, Some(&u("https://login.example/")), PopupStyle::Sheet, 1);
    let sheet = sheet.unwrap();
    webkit_load(&mut b, sheet, "https://login.example/");
    let (_, out) = b.decide_navigation(sheet, &u("tel:+15555550100"), Frame::Main, NavKind::LinkActivated, 2);
    assert!(out.contains(&Effect::HideView(sheet)) && out.contains(&Effect::HideView(op)));
}

#[test]
fn closing_an_opener_closes_its_sheets() {
    let mut b = browser(4);
    let (op, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (sheet, _) = b.open_popup(op, Some(&u("https://login.example/")), PopupStyle::Sheet, 1);
    let sheet = sheet.unwrap();
    b.close(op, 2);
    assert!(b.tab(sheet).is_none());
    assert!(b.tab(op).is_none());
    assert!(b.active().is_some());
}

#[test]
fn popups_inherit_private_profile() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/");
    let (popup, _) = b.open_popup(p, Some(&u("https://b.example/")), PopupStyle::Tab, 1);
    assert_eq!(b.tab(popup.unwrap()).unwrap().profile, Profile::Private);
}

#[test]
fn window_close_is_ignored_for_normal_tabs() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);
    assert!(b.on_close_requested(t, 1).is_empty());
    assert!(b.tab(t).is_some());
}

#[test]
fn popup_policy_blocks_dangerous_urls() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);
    // (Page-initiated javascript: runs with the page's own privileges and is allowed by policy.)
    for bad in ["data:text/html,x", "file:///etc/hosts", "tauri://localhost/"] {
        let (popup, _) = b.open_popup(t, Some(&u(bad)), PopupStyle::Sheet, 1);
        assert!(popup.is_none(), "{bad}");
    }
    let (popup, _) = b.open_popup(t, Some(&u("tel:+15555550100")), PopupStyle::Sheet, 1);
    assert!(popup.is_none());
    assert!(matches!(notice(&b, t), Some(Notice::ExternalApp { .. })));
    // about:blank popups (filled by script) are allowed.
    assert!(b.open_popup(t, None, PopupStyle::Sheet, 2).0.is_some());
}

// ---- reopening closed tabs (⌘⇧T) ------------------------------------------

#[test]
fn closed_tabs_reopen_newest_first_and_never_private() {
    let mut b = browser(4);
    let (a, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(a, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, a, "https://a.example/");
    let (c, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(c, "https://c.example/", 0).unwrap();
    webkit_load(&mut b, c, "https://c.example/");
    let (p, _) = b.new_tab(Profile::Private, 0);
    b.navigate_input(p, "https://private.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://private.example/");
    let (empty, _) = b.new_tab(Profile::Default, 0);
    b.close(a, 1);
    b.close(c, 2);
    b.close(p, 3);
    b.close(empty, 4);
    assert_eq!(b.pop_closed(), Some(u("https://c.example/")));
    let (t, effects) = b.reopen_closed(u("https://a.example/"), 5);
    let t = t.unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::CreateView { tab, .. } if *tab == t)));
    assert_eq!(b.pop_closed(), Some(u("https://a.example/")));
    assert_eq!(b.pop_closed(), None, "private and empty tabs are not remembered");
}

// ---- the user's block list (Settings → Blocked) ----------------------------

fn block(b: &mut Browser, patterns: &[&str]) -> Vec<Effect> {
    use snorkle_core::blocklist::{BlockList, BlockSettings};
    b.set_blocklist(BlockList::from_settings(BlockSettings {
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        message: "Not now.".into(),
    }))
}

fn user_blocked(b: &Browser, tab: TabId) -> bool {
    matches!(notice(b, tab), Some(Notice::UserBlocked { ref message, .. }) if message == "Not now.")
}

#[test]
fn blocked_addresses_never_reach_webkit_from_any_source() {
    let mut b = browser(4);
    block(&mut b, &["reddit\\.com"]);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);

    // Typed (also over http, which would otherwise be upgraded).
    for typed in ["https://www.reddit.com/r/rust", "http://reddit.com/", "reddit.com"] {
        let out = b.navigate_input(t, typed, 1).unwrap();
        assert!(!out.iter().any(|e| matches!(e, Effect::Load { .. } | Effect::CreateView { .. })), "{typed}");
        assert!(user_blocked(&b, t), "{typed}");
        assert!(out.contains(&Effect::HideView(t)) || !b.view().content_visible);
        b.go_back(t, 0);
    }
    // Back returns to the page underneath.
    assert_eq!(notice(&b, t), None);
    assert!(b.view().content_visible);

    // Links, redirects and script navigations in the main frame.
    for kind in [NavKind::LinkActivated, NavKind::Other, NavKind::FormSubmitted, NavKind::BackForward] {
        let (v, _) = b.decide_navigation(t, &u("https://old.reddit.com/"), Frame::Main, kind, 2);
        assert_eq!(v, Verdict::Cancel, "{kind:?}");
        assert!(user_blocked(&b, t));
        b.dismiss_notice(t);
    }
    // Frames inside a page are not affected.
    let (v, _) = b.decide_navigation(t, &u("https://www.reddit.com/embed"), Frame::Sub, NavKind::Other, 3);
    assert_eq!(v, Verdict::Allow);

    // New tabs: from other apps, ⌘-click, reopened tabs.
    let (o, _) = b.open_from_other_app(u("https://reddit.com/"), 4);
    let o = o.unwrap();
    assert!(user_blocked(&b, o));
    let (bg, _) = b.on_background_tab_request(t, &u("https://reddit.com/x"), 5);
    let bg = bg.unwrap();
    let (v, _) = b.decide_navigation(bg, &u("https://reddit.com/x"), Frame::Main, NavKind::Other, 6);
    assert_eq!(v, Verdict::Cancel);
    assert!(user_blocked(&b, bg));
}

#[test]
fn single_page_app_moving_to_a_blocked_address_is_hidden() {
    let mut b = browser(4);
    block(&mut b, &["youtube\\.com/shorts"]);
    let (t, _) = open_and_load(&mut b, "https://www.youtube.com/", 0);
    assert!(b.view().content_visible);

    // pushState to a blocked address: the page is hidden behind the notice.
    let out = b.on_url_changed(t, u("https://www.youtube.com/shorts/abc"));
    assert!(out.contains(&Effect::HideView(t)));
    assert!(user_blocked(&b, t));

    // Back really goes back in the page (the blocked page is underneath),
    // and dismissing cannot reveal it.
    let out = b.go_back(t, 0);
    assert!(out.contains(&Effect::GoBack(t)));
    assert!(user_blocked(&b, t));
    let out = b.dismiss_notice(t);
    assert!(!out.iter().any(|e| matches!(e, Effect::ShowView { .. })));
    assert!(!b.view().content_visible);

    // The page leaves the blocked address: shown again.
    block(&mut b, &["youtube\\.com/shorts"]);
    b.on_url_changed(t, u("https://www.youtube.com/shorts/abc"));
    let out = b.on_url_changed(t, u("https://www.youtube.com/"));
    assert!(out.contains(&Effect::ShowView { tab: t, pane: Pane::Full }));
    assert_eq!(notice(&b, t), None);
}

#[test]
fn adding_a_pattern_hides_open_pages_at_once() {
    let mut b = browser(4);
    let (t, _) = open_and_load(&mut b, "https://news.example/", 0);
    let (other, _) = open_and_load(&mut b, "https://fine.example/", 1);
    b.activate(t, 2);
    let out = block(&mut b, &["news\\.example"]);
    assert!(out.contains(&Effect::HideView(t)));
    assert!(user_blocked(&b, t));
    assert_eq!(notice(&b, other), None);
    // A commit that skipped the policy (restored back/forward state) is
    // still hidden.
    let (p, _) = open_and_load(&mut b, "https://fine.example/b", 3);
    b.on_commit(p, u("https://news.example/again"));
    assert!(user_blocked(&b, p));
    assert!(!b.view().content_visible);
}

#[test]
fn url_bar_attachment_is_per_tab_and_restored() {
    let mut b = browser(4);
    let (first, _) = b.new_tab(Profile::Default, 0);
    let (second, _) = b.new_tab(Profile::Default, 0);
    webkit_load(&mut b, first, "https://a.example/");
    webkit_load(&mut b, second, "https://b.example/");
    b.set_url_bar_attached(first, true);
    assert!(b.tab(first).unwrap().url_bar_attached);
    assert!(!b.tab(second).unwrap().url_bar_attached);
    let session = b.session();
    let mut restored = browser(4);
    restored.restore_session(&session, 0);
    let tabs = restored.view().tabs;
    assert!(tabs[0].url_bar_attached);
    assert!(!tabs[1].url_bar_attached);
}

// ---- tab order and labels (2026-11-09) -------------------------------------

#[test]
fn tabs_reorder_and_keep_their_labels_across_launches() {
    let mut b = browser(4);
    let (a, _) = b.new_tab(Profile::Default, 0);
    webkit_load(&mut b, a, "https://a.example/");
    let (c, _) = b.new_tab(Profile::Default, 0);
    webkit_load(&mut b, c, "https://c.example/");
    let (d, _) = b.new_tab(Profile::Default, 0);
    webkit_load(&mut b, d, "https://d.example/");
    let order = |b: &Browser| b.view().tabs.iter().map(|t| t.id).collect::<Vec<_>>();
    let out = b.move_tab(d, Some(a));
    assert_eq!(order(&b), vec![d, a, c]);
    assert!(out.iter().any(|e| matches!(e, Effect::PersistSession(s) if s.tabs[0].url == "https://d.example/")));
    b.move_tab(d, None);
    assert_eq!(order(&b), vec![a, c, d], "None moves to the end");
    assert!(b.move_tab(a, Some(a)).is_empty() && b.move_tab(a, Some(TabId(999))).is_empty());
    assert_eq!(order(&b), vec![a, c, d]);

    b.set_tab_label(c, Some("  Reading \u{202e}list "), Some("📚")).unwrap();
    assert!(b.set_tab_label(c, None, Some("two emoji ok?")).is_err(), "not an emoji");
    let v = b.view().tabs.into_iter().find(|t| t.id == c).unwrap();
    assert_eq!((v.name.as_deref(), v.emoji.as_deref()), (Some("Reading list"), Some("📚")));
    let session = b.session();
    let mut b2 = browser(4);
    b2.restore_session(&session, 0);
    let v = b2.view().tabs.into_iter().find(|t| t.title.is_empty() && t.name.is_some()).expect("restored with its name");
    assert_eq!(v.emoji.as_deref(), Some("📚"));
    b.set_tab_label(c, Some("   "), None).unwrap();
    let v = b.view().tabs.into_iter().find(|t| t.id == c).unwrap();
    assert!(v.name.is_none() && v.emoji.is_none(), "cleared");
}

// ---- Save for Later archives (2026-11-09) ----------------------------------

fn saved_copy(page: &str, pages: &[&str]) -> snorkle_core::tabs::SavedCopy {
    snorkle_core::tabs::SavedCopy {
        item: "l1-0".into(),
        page: u(page),
        pages: pages.iter().map(|p| u(p)).collect(),
        saved_ms: 7,
        committed: false,
        picking: None,
    }
}

/// The host loading the archive: WebKit asks about its base URL, commits.
fn load_saved(b: &mut Browser, tab: TabId, page: &str) {
    let (v, _) = b.decide_navigation(tab, &u(page), Frame::Main, NavKind::Other, 0);
    assert_eq!(v, Verdict::Allow, "the archive's own load");
    b.on_provisional_start(tab);
    b.on_commit(tab, u(page));
    let out = b.on_finish(tab);
    assert!(!out.iter().any(|e| matches!(e, Effect::RecordHistory { .. })), "a saved copy is not a visit");
}

#[test]
fn saved_archives_open_their_own_pages_and_nothing_else() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    let out = b.show_saved_copy(t, saved_copy("https://a.example/one", &["https://a.example/one", "https://a.example/two"]));
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { then: AfterCreate::Restore { fallback: None }, .. })), "a view, nothing loaded");
    load_saved(&mut b, t, "https://a.example/one");
    assert!(b.shows_offline_copy(t));
    let view = b.view().tabs.into_iter().find(|x| x.id == t).unwrap();
    assert_eq!(view.saved.as_ref().map(|s| (s.pages, s.saved_ms)), Some((2, 7)));

    // A link to the archive's other page opens it from the archive.
    let (v, out) = b.decide_navigation(t, &u("https://a.example/two#part"), Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Cancel);
    assert!(out.iter().any(|e| matches!(e, Effect::OpenSavedPage { url, item, .. } if url.as_str() == "https://a.example/two" && item == "l1-0")));
    // Same page (an anchor) and back/forward are WebKit's.
    assert_eq!(b.decide_navigation(t, &u("https://a.example/one#x"), Frame::Main, NavKind::LinkActivated, 0).0, Verdict::Allow);
    assert_eq!(b.decide_navigation(t, &u("https://a.example/two"), Frame::Main, NavKind::BackForward, 0).0, Verdict::Allow);
    // Reload reloads from the archive.
    assert!(matches!(b.reload(t).as_slice(), [Effect::OpenSavedPage { .. }]));

    // Any other link leaves the archive (the live page, through the policy).
    let (v, _) = b.decide_navigation(t, &u("https://elsewhere.example/"), Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Allow);
    b.on_provisional_start(t);
    assert!(b.saved_copy(t).is_none(), "the next load replaces the saved copy");

    // Never in private tabs.
    let (p, _) = b.new_tab(Profile::Private, 0);
    assert!(b.show_saved_copy(p, saved_copy("https://a.example/one", &["https://a.example/one"])).is_empty());
    assert!(b.saved_copy(p).is_none());
}

#[test]
fn choosing_pages_to_add_navigates_nowhere() {
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    let mut copy = saved_copy("https://a.example/one", &["https://a.example/one"]);
    copy.picking = Some(Vec::new());
    b.show_saved_copy(t, copy);
    load_saved(&mut b, t, "https://a.example/one");
    let link = |b: &mut Browser, url: &str, kind| b.decide_navigation(t, &u(url), Frame::Main, kind, 0).0;
    assert_eq!(link(&mut b, "https://a.example/two#x", NavKind::LinkActivated), Verdict::Cancel);
    assert_eq!(link(&mut b, "https://a.example/three", NavKind::LinkActivated), Verdict::Cancel);
    assert_eq!(link(&mut b, "https://other.example/", NavKind::LinkActivated), Verdict::Cancel, "other sites are not added");
    assert_eq!(link(&mut b, "https://a.example/one", NavKind::LinkActivated), Verdict::Cancel, "already in the archive");
    assert_eq!(link(&mut b, "https://a.example/four", NavKind::Other), Verdict::Cancel, "only link taps choose");
    assert_eq!(link(&mut b, "https://a.example/three", NavKind::LinkActivated), Verdict::Cancel, "a second tap unchooses");
    let view = b.view().tabs.into_iter().find(|x| x.id == t).unwrap();
    assert_eq!(view.saved.unwrap().picking, Some(vec!["/two".to_string()]));
    let (item, picks) = b.take_picks(t).unwrap();
    assert_eq!((item.as_str(), picks), ("l1-0", vec![u("https://a.example/two")]));
    assert!(b.take_picks(t).is_none());
}
