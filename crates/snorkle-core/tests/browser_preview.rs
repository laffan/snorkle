//! `Browser` state machine (2026-11-24): the History preview, a page shown
//! inside the chrome's History dialog.

mod common;
use common::*;

fn shows(out: &[Effect], tab: TabId, pane: Pane) -> bool {
    out.iter().any(|e| matches!(e, Effect::ShowView { tab: t, pane: p } if *t == tab && *p == pane))
}

#[test]
fn a_preview_is_shown_alone_and_never_listed_saved_or_recorded() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    // The History dialog is open: the chrome's overlay hides the page.
    b.set_chrome_overlay(true);
    let (p, out) = b.open_preview(u("https://p.example/read"), 1);
    let p = p.unwrap();
    assert!(!shows(&out, p, Pane::Preview), "nothing to show before it commits");
    let out = webkit_load(&mut b, p, "https://p.example/read");
    assert!(shows(&out, p, Pane::Preview), "shown in the dialog once it has a page");
    assert!(!out.iter().any(|e| matches!(e, Effect::RecordHistory { .. })), "a preview is not a visit");
    assert_eq!(b.active(), Some(a), "the active tab stays");
    let v = b.view();
    assert_eq!(v.preview, Some(p));
    assert!(!v.tabs.iter().any(|t| t.id == p), "not listed");
    assert!(!b.session().tabs.iter().any(|t| t.url.contains("p.example")), "not saved");
    // Closed: the overlay still hides the page, then the dialog goes.
    let out = b.close_preview();
    assert!(out.iter().any(|e| matches!(e, Effect::DestroyView { tab, .. } if *tab == p)));
    assert!(b.tab(p).is_none());
    assert_eq!(b.view().preview, None);
    let out = b.set_chrome_overlay(false);
    assert!(shows(&out, a, Pane::Full), "the page comes back");
    assert_eq!(b.pop_closed(), None, "nothing for ⌘⇧T to reopen");
}

#[test]
fn one_preview_at_a_time_and_choosing_a_tab_closes_it() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (c, _) = open_and_load(&mut b, "https://c.example/", 1);
    let (p1, _) = b.open_preview(u("https://p.example/1"), 2);
    let (p2, out) = b.open_preview(u("https://p.example/2"), 3);
    let (p1, p2) = (p1.unwrap(), p2.unwrap());
    assert!(b.tab(p1).is_none(), "the first preview made way");
    assert!(out.iter().any(|e| matches!(e, Effect::DestroyView { tab, .. } if *tab == p1)), "its view went with it");
    webkit_load(&mut b, p2, "https://p.example/2");
    // The preview is never made active.
    b.activate(p2, 4);
    assert_eq!(b.active(), Some(c));
    assert!(b.tab(p2).is_some());
    // Choosing another tab closes it.
    b.activate(a, 5);
    assert!(b.tab(p2).is_none());
    assert_eq!(b.view().preview, None);
}

#[test]
fn closing_the_active_tab_never_lands_on_the_preview() {
    let mut b = browser(6);
    let (a, _) = open_and_load(&mut b, "https://a.example/", 0);
    let (p, _) = b.open_preview(u("https://p.example/"), 1);
    let p = p.unwrap();
    webkit_load(&mut b, p, "https://p.example/");
    b.close(a, 2);
    let active = b.active().expect("a tab is active");
    assert_ne!(active, p);
    assert!(b.tab(p).is_none(), "the new active tab closed the preview");
}

#[test]
fn a_preview_takes_only_web_pages() {
    let mut b = browser(6);
    open_and_load(&mut b, "https://a.example/", 0);
    let (p, _) = b.open_preview(u("file:///etc/hosts"), 1);
    assert_eq!(p, None);
    assert_eq!(b.view().preview, None);
}
