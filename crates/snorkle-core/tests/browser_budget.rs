//! `Browser` state machine: history recording, the live-view budget,
//! discards and restores, lazy sessions, ad-block state and split views.

mod common;
use common::*;

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
fn closing_the_focused_half_shows_the_other_half() {
    // ⌘S puts the new half right after the active tab; ⌘W on it must show
    // the tab it was split from, not the tab after it in the list.
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 1);
    b.activate(a, 2);
    let (y, _) = b.split_new(SplitOrientation::Columns, 3);
    let y = y.unwrap();
    assert_eq!(b.view().tabs.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, y, d]);
    assert_eq!(b.active(), Some(y));
    let out = b.close(y, 4);
    assert_eq!(b.active(), Some(a), "the remaining half is shown");
    assert!(b.view().split.is_none());
    assert!(out.contains(&Effect::ShowView { tab: a, pane: Pane::Full }));
    assert!(!out.iter().any(|e| matches!(e, Effect::ShowView { tab, .. } if *tab == d)));

    // The same for a pair made by dragging, closing either half.
    let (c, _) = open_and_load(&mut b, "https://c.example/", 5);
    let (e, _) = open_and_load(&mut b, "https://e.example/", 6);
    b.split(c, a, 7); // a | c, c focused; e follows c in the list
    b.close(c, 8);
    assert_eq!(b.active(), Some(a));
    b.split(e, a, 9);
    b.activate(a, 10);
    b.close(a, 11);
    assert_eq!(b.active(), Some(e));
    assert!(b.view().splits.is_empty());
}

#[test]
fn closing_a_half_split_from_a_bookmark_shows_the_bookmark_not_the_next_one() {
    // Reported 2026-11-19: ⌘W on the half split from a bookmark's tab went
    // to the next bookmark's tab (the next tab in the list).
    let mut b = browser(8);
    let mut marks = Vec::new();
    for url in ["https://b1.example/", "https://b2.example/", "https://b3.example/"] {
        let (id, _) = b.open_launcher(url, u(url), false, 0);
        webkit_load(&mut b, id, url);
        marks.push(id);
    }
    b.open_launcher("https://b2.example/", u("https://b2.example/"), false, 1);
    let (y, _) = b.split_new(SplitOrientation::Columns, 2);
    let y = y.unwrap();
    b.navigate_input(y, "https://other.example/", 3).unwrap();
    webkit_load(&mut b, y, "https://other.example/");
    // The new half sits between b2's tab and b3's.
    let order: Vec<TabId> = b.view().tabs.iter().map(|t| t.id).collect();
    assert_eq!(order, vec![marks[0], marks[1], y, marks[2]]);
    let out = b.close(y, 4);
    assert_eq!(b.active(), Some(marks[1]));
    assert!(out.contains(&Effect::ShowView { tab: marks[1], pane: Pane::Full }));
    assert!(!out.iter().any(|e| matches!(e, Effect::ShowView { tab, .. } if *tab == marks[2])));
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

#[test]
fn on_screen_is_the_active_tab_or_its_split_partner() {
    let mut b = browser(4);
    let (a, _) = b.new_tab(Profile::Default, 0);
    let (c, _) = b.new_tab(Profile::Default, 1);
    assert!(b.is_on_screen(c) && !b.is_on_screen(a));
    b.split(a, c, 2);
    assert!(b.is_on_screen(a) && b.is_on_screen(c), "both halves of the pair on screen");
}
