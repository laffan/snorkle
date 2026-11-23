//! `Browser` state machine: links from other apps, launchers, popups and
//! sheets, reopening closed tabs, the block list, per-tab bar and labels,
//! saved copies.

mod common;
use common::*;

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

    // Any other link opens in a new ordinary tab; the saved copy's tab
    // stays as it is. Scripts and redirects go nowhere.
    let (v, out) = b.decide_navigation(t, &u("https://elsewhere.example/"), Frame::Main, NavKind::LinkActivated, 0);
    assert_eq!(v, Verdict::Cancel);
    let fresh = b.active().unwrap();
    assert_ne!(fresh, t);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, then: AfterCreate::Load(u), .. } if *tab == fresh && u.as_str() == "https://elsewhere.example/")));
    assert!(b.saved_copy(fresh).is_none() && b.saved_copy(t).is_some());
    assert_eq!(b.decide_navigation(t, &u("https://elsewhere.example/"), Frame::Main, NavKind::Other, 0).0, Verdict::Cancel);
    // So does an address typed in it.
    b.activate(t, 0);
    b.navigate_input(t, "https://typed.example/", 0).unwrap();
    let typed = b.active().unwrap();
    assert!(typed != t && typed != fresh && b.saved_copy(t).is_some());

    // Never in the session or ⌘⇧T; reopening the item finds its tab; the
    // item's removal closes it.
    webkit_load(&mut b, fresh, "https://elsewhere.example/");
    assert_eq!(b.session().tabs.iter().map(|x| x.url.as_str()).collect::<Vec<_>>(), vec!["https://elsewhere.example/"]);
    assert_eq!(b.saved_tab("l1-0"), Some(t));
    b.close_saved("l1-0", 0);
    assert!(b.tab(t).is_none() && b.saved_tab("l1-0").is_none());
    assert!(b.pop_closed().is_none(), "a saved copy's tab is not reopened");

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
