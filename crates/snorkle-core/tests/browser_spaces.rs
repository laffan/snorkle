//! `Browser` state machine (2026-11-24): pins keep their page, research
//! sessions, tab groups.

mod common;
use common::*;

/// A pin at `key` whose tab has loaded it.
fn pinned(b: &mut Browser, key: &str, now: u64) -> TabId {
    b.set_pins([key.to_string()]);
    let (p, _) = b.open_launcher(key, u(key), false, now);
    webkit_load(b, p, key);
    p
}

fn link(b: &mut Browser, tab: TabId, url: &str, now: u64) -> (Verdict, Vec<Effect>) {
    b.decide_navigation(tab, &u(url), Frame::Main, NavKind::LinkActivated, now)
}

// ---- pins keep their page ---------------------------------------------------

#[test]
fn a_link_followed_in_a_pin_opens_a_new_tab() {
    let mut b = browser(6);
    let (t, _) = open_and_load(&mut b, "https://a.example/", 0);
    let key = "https://news.example/";
    let p = pinned(&mut b, key, 1);
    let out = b.on_commit(p, u(key));
    assert!(out.contains(&Effect::PinGuard { tab: p, on: true }), "the guard is turned on after a commit");
    let (v, out) = link(&mut b, p, "https://news.example/item?id=1", 2);
    assert_eq!(v, Verdict::Cancel, "the pin stays on its page");
    let fresh = b.active().unwrap();
    assert_ne!(fresh, p);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, then: AfterCreate::Load(url), .. } if *tab == fresh && url.as_str() == "https://news.example/item?id=1")));
    assert_eq!(*b.tab_ids().last().unwrap(), fresh, "at the end of the list");
    assert_eq!(b.view().tabs.iter().find(|x| x.id == fresh).unwrap().launcher, None, "an ordinary tab");
    assert_eq!(displayed(&b, p).as_deref(), Some(key));
    let _ = t;
    // Back in the new tab returns to the pin's page.
    assert!(b.view().tabs.iter().find(|x| x.id == fresh).unwrap().can_go_back);
}

#[test]
fn what_stays_in_the_pin() {
    let mut b = browser(6);
    let key = "https://mail.example/inbox";
    let p = pinned(&mut b, key, 1);
    // Fragment only (Gmail's views), redirects and script loads, forms,
    // back/forward and reload stay in the pin's tab.
    assert_eq!(link(&mut b, p, "https://mail.example/inbox#msg-1", 2).0, Verdict::Allow);
    for kind in [NavKind::Other, NavKind::FormSubmitted, NavKind::BackForward, NavKind::Reload] {
        assert_eq!(
            b.decide_navigation(p, &u("https://mail.example/elsewhere"), Frame::Main, kind, 2).0,
            Verdict::Allow,
            "{kind:?}"
        );
    }
    // Frames inside the page are the page's.
    assert_eq!(
        b.decide_navigation(p, &u("https://ads.example/"), Frame::Sub, NavKind::LinkActivated, 2).0,
        Verdict::Allow
    );
    assert_eq!(b.active(), Some(p));
    // A bookmark's tab is not a pin's.
    let bm = "https://docs.example/";
    let (d, _) = b.open_launcher(bm, u(bm), false, 3);
    webkit_load(&mut b, d, bm);
    assert_eq!(link(&mut b, d, "https://docs.example/other", 4).0, Verdict::Allow);
    // Unpinned (released), the tab is ordinary again and the guard goes off.
    let out = b.release_launcher(key);
    assert!(out.contains(&Effect::PinGuard { tab: p, on: false }));
    assert_eq!(link(&mut b, p, "https://mail.example/other", 5).0, Verdict::Allow);
}

#[test]
fn an_address_typed_in_a_pin_opens_a_new_tab() {
    let mut b = browser(6);
    let key = "https://pin.example/";
    let p = pinned(&mut b, key, 1);
    b.navigate_input(p, "other.example", 2).unwrap();
    let fresh = b.active().unwrap();
    assert_ne!(fresh, p);
    assert_eq!(displayed(&b, p).as_deref(), Some(key));
    // Its own address (Return to Original URL's load) stays.
    b.activate(p, 3);
    b.navigate_input(p, key, 3).unwrap();
    assert_eq!(b.active(), Some(p));
}

#[test]
fn single_page_app_links_and_routes() {
    let mut b = browser(6);
    let key = "https://app.example/home";
    let p = pinned(&mut b, key, 1);
    // A link the watcher stopped (C25).
    let out = b.on_pin_link(p, &u("https://app.example/status/42"), 10);
    let fresh = b.active().unwrap();
    assert_ne!(fresh, p);
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, .. } if *tab == fresh)));
    // One to the page itself (another fragment) is ignored.
    b.activate(p, 11);
    assert!(b.on_pin_link(p, &u("https://app.example/home#top"), 12).is_empty());
    assert!(b.on_pin_link(fresh, &u("https://app.example/x"), 12).is_empty(), "not a pin's tab");
    // A route change with no click before it is the page's own.
    b.on_url_changed(p, u("https://app.example/home?tab=1"), 20);
    assert_eq!(b.active(), Some(p));
    assert_eq!(displayed(&b, p).as_deref(), Some("https://app.example/home?tab=1"));
    // Right after a real click it is the user's: a new tab, the pin back.
    b.on_history_state(p, true, false);
    b.on_pin_gesture(p, 30);
    let out = b.on_url_changed(p, u("https://app.example/status/7"), 31);
    let routed = b.active().unwrap();
    assert_ne!(routed, p);
    assert!(out.contains(&Effect::GoBack(p)), "the pin goes back: {out:?}");
    assert!(out.iter().any(|e| matches!(e, Effect::CreateView { tab, then: AfterCreate::Load(url), .. } if *tab == routed && url.as_str() == "https://app.example/status/7")));
    // Too late after the click: the page's own.
    b.activate(p, 40);
    b.on_pin_gesture(p, 50);
    b.on_url_changed(p, u("https://app.example/home"), 50 + GESTURE_WINDOW_MS + 1);
    assert_eq!(b.active(), Some(p));
}

#[test]
fn a_pin_sends_a_bounded_number_of_pages() {
    let mut b = browser(40);
    let key = "https://pin.example/";
    let p = pinned(&mut b, key, 1);
    for i in 0..MAX_DIVERTS {
        b.activate(p, 2);
        assert_eq!(link(&mut b, p, &format!("https://pin.example/{i}"), 2).0, Verdict::Cancel);
    }
    b.activate(p, 3);
    assert_eq!(link(&mut b, p, "https://pin.example/more", 3).0, Verdict::Allow, "over the limit: the pin follows it");
    assert!(b.on_pin_link(p, &u("https://pin.example/again"), 4).is_empty());
    // A minute later it works again.
    assert!(!b.on_pin_link(p, &u("https://pin.example/later"), 70_000).is_empty());
}

#[test]
fn private_tabs_and_unknown_pins_are_never_guarded() {
    let mut b = browser(6);
    let (t, _) = b.new_tab(Profile::Private, 0);
    webkit_load(&mut b, t, "https://p.example/");
    b.on_pin_gesture(t, 1);
    assert!(b.on_pin_link(t, &u("https://p.example/x"), 1).is_empty());
    assert!(!b.is_pin_tab(t));
    let out = b.set_pins(["https://p.example/".to_string()]);
    assert!(out.is_empty(), "no tab became a pin's");
}

// ---- research sessions ------------------------------------------------------

#[test]
fn research_sessions_have_their_own_tabs() {
    let mut b = browser(6);
    let (main, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (r, _) = b.new_research("  Tide pools ", 10, 10).unwrap();
    assert_eq!(b.space(), Some(r.as_str()));
    let v = b.view();
    assert_eq!(v.space.as_deref(), Some(r.as_str()));
    assert_eq!(v.research[0].name, "Tide pools");
    let start = b.active().unwrap();
    assert_ne!(start, main);
    // ⌘T, popups and ⌘-clicked links stay in the session.
    webkit_load(&mut b, start, "https://r.example/");
    let (t2, _) = b.new_tab(Profile::Default, 11);
    assert_eq!(b.tab(t2).unwrap().space.as_deref(), Some(r.as_str()));
    let (bg, _) = b.on_background_tab_request(start, &u("https://r.example/2"), 12);
    assert_eq!(b.tab(bg.unwrap()).unwrap().space.as_deref(), Some(r.as_str()));
    // Back to everyday browsing: the tab used last there.
    b.enter_space(None, 13);
    assert_eq!(b.active(), Some(main));
    assert_eq!(b.space(), None);
    // And into the session again: its tab used last.
    b.enter_space(Some(&r), 14);
    assert_eq!(b.active(), Some(t2));
    // Closing a session's tabs never leaves it.
    for t in [t2, bg.unwrap(), start] {
        b.close(t, 15);
    }
    assert_eq!(b.space(), Some(r.as_str()), "a new tab in the session");
    assert_ne!(b.active(), Some(main));
    assert!(b.enter_space(Some("r999"), 16).is_empty(), "no such session");
}

#[test]
fn tabs_are_sent_between_sessions() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (r, _) = b.new_research("Topic", 2, 2).unwrap();
    b.enter_space(None, 3);
    b.activate(c, 4);
    // The tab shown is sent away: everyday browsing stays shown.
    b.send_tabs(&[c], Some(&r), 5);
    assert_eq!(b.tab(c).unwrap().space.as_deref(), Some(r.as_str()));
    assert_eq!(b.active(), Some(a));
    assert_eq!(b.space(), None);
    // From the session back to everyday browsing.
    b.enter_space(Some(&r), 6);
    b.send_tabs(&[c], None, 7);
    assert_eq!(b.tab(c).unwrap().space, None);
    assert_eq!(b.space(), Some(r.as_str()));
    // A pin's tab is not sent; a copy of its page is.
    let key = "https://pin.example/";
    b.enter_space(None, 8);
    let (p, _) = b.open_launcher(key, u(key), false, 8);
    b.send_tabs(&[p], Some(&r), 9);
    assert_eq!(b.tab(p).unwrap().space, None);
    let (copy, _) = b.send_page(u(key), "Pin", Some(&r), 10);
    let copy = copy.unwrap();
    assert_eq!(b.tab(copy).unwrap().space.as_deref(), Some(r.as_str()));
    assert_eq!(b.active(), Some(p), "sending a copy shows nothing");
    assert!(b.send_page(u("javascript:alert(1)"), "x", Some(&r), 11).0.is_none());
    assert!(b.send_tabs(&[a], Some("r1"), 12).is_empty(), "no such session");
}

#[test]
fn research_sessions_are_saved_and_deleted() {
    let mut b = browser(6);
    open_and_load(&mut b, "https://a.example/", 0);
    let (r, _) = b.new_research("Saved topic", 1, 1).unwrap();
    let t = b.active().unwrap();
    webkit_load(&mut b, t, "https://r.example/");
    let (empty, _) = b.new_research("Empty", 2, 2).unwrap();
    b.rename_research(&empty, "Renamed");
    let s = b.session();
    assert_eq!(s.research.len(), 2);
    assert_eq!(s.research[1].name, "Renamed");
    assert!(s.tabs.iter().any(|x| x.space.as_deref() == Some(r.as_str())));
    let mut b2 = browser(6);
    let (start, _) = b2.restore_session_with_start_tab(&s, 3);
    assert_eq!(b2.research().len(), 2, "an empty session is kept");
    assert_eq!(b2.tab(start).unwrap().space.as_deref(), Some(r.as_str()), "the launch greets in the session left");
    // Deleting closes its tabs and shows everyday browsing.
    b2.delete_research(&r, 4);
    assert_eq!(b2.research().len(), 1);
    assert_eq!(b2.space(), None);
    assert!(b2.view().tabs.iter().all(|x| x.space.as_deref() != Some(r.as_str())));
    // A session naming an unknown research session loads into everyday.
    let mut s = b.session();
    s.research.clear();
    let mut b3 = browser(6);
    b3.restore_session(&s, 5);
    assert!(b3.view().tabs.iter().all(|x| x.space.is_none()));
}

#[test]
fn research_names_and_ids_are_checked() {
    use snorkle_core::browser::{clean_research_name, valid_research_id};
    assert_eq!(clean_research_name("  \u{202E}Evil\n "), "Evil");
    assert_eq!(clean_research_name(""), "Research");
    assert_eq!(clean_research_name(&"x".repeat(200)).chars().count(), 81, "80 and an ellipsis");
    assert!(valid_research_id("r1764000000000"));
    for bad in ["r", "x1", "r1/../x", "r12a", &format!("r{}", "1".repeat(30))] {
        assert!(!valid_research_id(bad), "{bad}");
    }
    let mut b = browser(4);
    for i in 0..snorkle_core::browser::MAX_RESEARCH {
        b.new_research("t", i as u64, i as u64).unwrap();
    }
    assert!(b.new_research("one too many", 99, 99).is_err());
}

// ---- tab groups ---------------------------------------------------------------

#[test]
fn groups_gather_their_tabs() {
    let mut b = browser(8);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (d, _) = open_and_load(&mut b, "https://d.example/", 2);
    let (e, _) = open_and_load(&mut b, "https://e.example/", 3);
    let order = b.tab_ids();
    assert_eq!(order, vec![a, c, d, e]);
    let (g, _) = b.group_tabs(&[e, c], "  Reading ").unwrap();
    assert_eq!(b.tab_ids(), vec![a, c, e, d], "gathered where the first is");
    assert_eq!(b.groups()[0].name, "Reading");
    assert_eq!(b.group_members(g), vec![c, e]);
    // A tab opened from a grouped tab joins it; ⌘T opens after the group.
    b.activate(c, 4);
    let (bg, _) = b.on_background_tab_request(c, &u("https://c.example/2"), 5);
    let bg = bg.unwrap();
    webkit_load(&mut b, bg, "https://c.example/2");
    assert_eq!(b.group_members(g), vec![c, bg, e]);
    let (fresh, _) = b.new_tab(Profile::Default, 6);
    assert_eq!(b.tab(fresh).unwrap().group, None);
    assert_eq!(b.tab_ids(), vec![a, c, bg, e, fresh, d]);
    // Dragged out and back in.
    b.move_tab(e, None, None);
    assert_eq!(b.group_members(g), vec![c, bg]);
    b.move_tab(e, Some(c), Some(g));
    assert_eq!(b.group_members(g), vec![e, c, bg]);
    // Leaving a group keeps it together.
    b.ungroup_tabs(&[c]);
    assert_eq!(b.group_members(g), vec![e, bg]);
    let ids = b.tab_ids();
    let at = |x| ids.iter().position(|t| *t == x).unwrap();
    assert_eq!(at(bg), at(e) + 1);
    // Renamed, folded, saved and restored.
    b.rename_group(g, "Later");
    b.set_group_collapsed(g, true);
    let s = b.session();
    assert_eq!(s.groups.len(), 1);
    let mut b2 = browser(8);
    b2.restore_session(&s, 7);
    let v = b2.view();
    assert_eq!(v.groups.len(), 1);
    assert!(v.groups[0].collapsed && v.groups[0].name == "Later");
    assert_eq!(v.tabs.iter().filter(|t| t.group == Some(v.groups[0].id)).count(), 2);
    // Ungrouped, and closing a group's last tab drops it.
    b.ungroup(g);
    assert!(b.groups().is_empty());
    let (g2, _) = b.group_tabs(&[d], "").unwrap();
    assert_eq!(b.groups()[0].name, "Group");
    b.close(d, 8);
    assert!(b.groups().iter().all(|x| x.id != g2));
}

#[test]
fn groups_hold_one_kind_of_tab() {
    let mut b = browser(8);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (p, _) = b.new_tab(Profile::Private, 1);
    let key = "https://pin.example/";
    let (pin, _) = b.open_launcher(key, u(key), false, 2);
    let (g, _) = b.group_tabs(&[a, p, pin], "Mixed").unwrap();
    assert_eq!(b.group_members(g), vec![a], "the first one's profile; no pin's tab");
    assert!(b.group_tabs(&[pin], "x").is_none());
    // Sending a tab to a research session takes it out of its group.
    let (r, _) = b.new_research("R", 3, 3).unwrap();
    b.send_tabs(&[a], Some(&r), 4);
    assert!(b.groups().is_empty());
    // Bookmarking a tab moves it out of the list and its group.
    let (x, _) = open_and_load(&mut b, "https://x.example/", 5);
    let (g, _) = b.group_tabs(&[x], "X").unwrap();
    b.bind_launcher(x, "https://x.example/");
    assert!(b.group_members(g).is_empty() && b.groups().is_empty());
}
