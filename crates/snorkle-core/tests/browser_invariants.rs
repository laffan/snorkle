//! Security/performance invariants of the `Browser` state machine:
//! address bar shows committed URLs only, HTTPS-first never downgrades
//! silently, private tabs never reach storage, and the memory budget holds.


mod common;
use common::*;

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

#[test]
fn try_again_loads_the_page_that_failed() {
    // A new tab whose first page fails (offline): nothing was committed, so
    // the web view's reload would do nothing. Try Again loads the address.
    let mut b = browser(4);
    let (t, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(t, "https://news.example/story", 0).unwrap();
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Network, Some(u("https://news.example/story")), "offline");
    assert!(matches!(b.tab(t).unwrap().notice, Some(Notice::LoadError { .. })));
    let out = b.reload_by_user(t, 0);
    assert!(out
        .iter()
        .any(|e| matches!(e, Effect::Load { tab, url } if *tab == t && url.as_str() == "https://news.example/story")));
    assert!(b.tab(t).unwrap().notice.is_none());

    // After a page that loaded, a failed link: the failed page, not the one before.
    webkit_load(&mut b, t, "https://news.example/story");
    let next = u("https://news.example/next");
    b.decide_navigation(t, &next, Frame::Main, NavKind::LinkActivated, 0);
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Network, Some(next), "offline");
    let out = b.reload_by_user(t, 0);
    assert!(out.iter().any(|e| matches!(e, Effect::Load { url, .. } if url.as_str() == "https://news.example/next")));
    assert!(!out.iter().any(|e| matches!(e, Effect::Reload(_))));

    // A certificate failure is not retried from its address; no notice: a plain reload.
    b.on_provisional_start(t);
    b.on_fail(t, true, NavErrorKind::Certificate, Some(u("https://news.example/x")), "bad certificate");
    assert!(!b.reload_by_user(t, 0).iter().any(|e| matches!(e, Effect::Load { .. })));
    webkit_load(&mut b, t, "https://news.example/story");
    assert!(b.reload_by_user(t, 0).iter().any(|e| matches!(e, Effect::Reload(_))));
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
    let out = b.set_view_exempt(t, &[Kind::HideMedia, Kind::Desaturate, Kind::HideMedia]);
    assert!(out.iter().any(|e| matches!(e, Effect::PersistSession(_))));
    let session = b.session();
    assert_eq!(session.tabs[0].exempt, vec![Kind::Desaturate, Kind::HideMedia], "sorted, unique");
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
fn save_for_later_from_history_loads_in_a_hidden_tab() {
    let mut b = browser(4);
    let (p, _) = b.new_tab(Profile::Default, 0);
    b.navigate_input(p, "https://a.example/", 0).unwrap();
    webkit_load(&mut b, p, "https://a.example/");
    let (t, out) = b.open_in_background(u("http://b.example/story"), 5);
    let t = t.expect("a tab is opened");
    assert_eq!(b.active(), Some(p), "the current tab stays in front");
    assert_eq!(b.tab(t).unwrap().profile, Profile::Default);
    assert!(!out.iter().any(|e| matches!(e, Effect::ShowView { tab, .. } if *tab == t)), "loaded hidden");
    assert!(
        out.iter().any(|e| matches!(e, Effect::CreateView { tab, then: AfterCreate::Load(url), .. }
            if *tab == t && url.as_str() == "https://b.example/story")),
        "http upgraded, through the policy: {out:?}"
    );
    // Only web pages.
    assert!(b.open_in_background(u("javascript:alert(1)"), 6).0.is_none());
    assert!(b.open_in_background(u("file:///etc/passwd"), 6).0.is_none());
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
