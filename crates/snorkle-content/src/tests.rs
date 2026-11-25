use super::*;

#[test]
fn clamp() {
    assert_eq!(clamp_inset(f64::NAN, 1000.0), 0.0);
    assert_eq!(clamp_inset(f64::INFINITY, 1000.0), 0.0);
    assert_eq!(clamp_inset(-5.0, 1000.0), 0.0);
    assert_eq!(clamp_inset(5000.0, 1000.0), 500.0);
    assert_eq!(clamp_inset(64.0, 1000.0), 64.0);
}

#[test]
fn user_agent_matches_safari_desktop_format() {
    let ua = safari_desktop_user_agent(snorkle_core::os::OsVersion::new(26, 6, 1));
    assert_eq!(
        ua,
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6 Safari/605.1.15"
    );
}

#[test]
fn holes_are_validated_and_merged() {
    let h = |x, y, w, h, r| Hole { x, y, w, h, r, corners: ALL_CORNERS };
    let out = sanitize_holes(
        &[h(10.0, 900.0, 400.0, 46.0, 23.0), h(f64::NAN, 0.0, 1.0, 1.0, 0.0), h(5.0, 5.0, 0.0, 10.0, 0.0)],
        1000.0,
        1000.0,
    );
    assert_eq!(out, vec![h(10.0, 900.0, 400.0, 46.0, 23.0)]);
    let off = sanitize_holes(&[h(-300.0, 20.0, 264.0, 900.0, 99.0)], 1000.0, 1000.0);
    assert_eq!(off, vec![h(-300.0, 20.0, 264.0, 900.0, 99.0)], "may wait off-screen (radius ≤ half the side)");
    assert!(sanitize_holes(&[h(-5000.0, 0.0, 10.0, 10.0, 0.0)], 1000.0, 1000.0).is_empty(), "out of reach");
    let merged = sanitize_holes(&[h(0.0, 0.0, 10.0, 10.0, 4.0), h(5.0, 5.0, 10.0, 10.0, 2.0)], 100.0, 100.0);
    assert_eq!(merged, vec![h(0.0, 0.0, 15.0, 15.0, 2.0)]);
    let many: Vec<Hole> = (0..20).map(|i| h(i as f64 * 20.0, 0.0, 10.0, 10.0, 0.0)).collect();
    assert_eq!(sanitize_holes(&many, 1000.0, 1000.0).len(), MAX_HOLES);
    assert!(h(0.0, 0.0, 10.0, 10.0, 0.0).contains(5.0, 5.0) && !h(0.0, 0.0, 10.0, 10.0, 0.0).contains(10.0, 5.0));
    // Touching holes stay separate (the find bar sits on the pill).
    let touching = [h(0.0, 20.0, 10.0, 10.0, 2.0), Hole { corners: 3, ..h(2.0, 14.0, 6.0, 6.0, 2.0) }];
    let out = sanitize_holes(&touching, 100.0, 100.0);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].corners, 3, "top corners only");
    assert_eq!(
        sanitize_holes(&[Hole { corners: 0, ..h(0.0, 0.0, 9.0, 9.0, 1.0) }], 99.0, 99.0)[0].corners,
        ALL_CORNERS
    );
}

#[test]
fn pages_stay_in_the_browser_by_default() {
    // Settings → General → Links to apps: off until the chrome says so.
    assert!(!open_app_links());
    set_open_app_links(true);
    assert!(open_app_links());
    set_open_app_links(false);
}

#[test]
fn shortcuts_are_unique_and_rebindable() {
    let ids: std::collections::BTreeSet<_> = SHORTCUTS.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), SHORTCUTS.len(), "ids (menu property lists) are unique: UIKit throws otherwise");
    let defaults = effective_bindings(&[]).expect("defaults do not collide");
    assert_eq!(defaults.len(), SHORTCUTS.len());
    assert_eq!(
        shortcut_index("bookmark").map(|i| &defaults[i]),
        Some(&KeyBinding { key: "d".into(), mods: 0 })
    );
    assert_eq!(
        shortcut_index("split_columns").map(|i| &defaults[i]),
        Some(&KeyBinding { key: "s".into(), mods: 0 })
    );
    assert_eq!(
        shortcut_index("split_rows").map(|i| &defaults[i]),
        Some(&KeyBinding { key: "s".into(), mods: MOD_SHIFT })
    );
    // Chrome's and Safari's tab switching: ⌘⇧] / ⌘⇧[ (the unshifted key).
    assert_eq!(
        shortcut_index("next_tab").map(|i| &defaults[i]),
        Some(&KeyBinding { key: "]".into(), mods: MOD_SHIFT })
    );
    assert_eq!(
        shortcut_index("prev_tab").map(|i| &defaults[i]),
        Some(&KeyBinding { key: "[".into(), mods: MOD_SHIFT })
    );
    assert_eq!(Shortcut::NextTab.name(), "next_tab");
    assert_eq!(Shortcut::PreviousTab.name(), "prev_tab");
    let b = effective_bindings(&[("new_tab".into(), "Y".into(), MOD_SHIFT)]).unwrap();
    assert_eq!(b[0], KeyBinding { key: "y".into(), mods: MOD_SHIFT });
    // ⌘⇧N is New Private Tab's since 2026-11-24.
    assert!(effective_bindings(&[("new_tab".into(), "n".into(), MOD_SHIFT)]).unwrap_err().contains("New Private Tab"));
    assert!(effective_bindings(&[("new_tab".into(), "w".into(), 0)]).unwrap_err().contains("Close Tab"));
    let ignored = effective_bindings(&[
        ("nope".into(), "x".into(), 0),
        ("find".into(), "F1".into(), 0),
        ("find".into(), "g".into(), 64),
    ]);
    assert_eq!(ignored.unwrap(), defaults, "unknown ids and invalid keys are ignored");
    assert!(validate_binding("\\", MOD_SHIFT).is_some() && validate_binding(" ", 0).is_none());
}

#[test]
fn passkey_permission_from_the_system_enum() {
    assert_eq!(PasskeyPermission::from_raw(0), PasskeyPermission::Authorized);
    assert_eq!(PasskeyPermission::from_raw(1), PasskeyPermission::Denied);
    assert_eq!(PasskeyPermission::from_raw(2), PasskeyPermission::NotDetermined);
    assert_eq!(PasskeyPermission::from_raw(-1), PasskeyPermission::Unavailable);
    assert_eq!(PasskeyPermission::from_raw(3).name(), "unavailable");
    assert_eq!(passkeys::entitled(), None, "no entitlement can be read off macOS");
}

#[test]
fn night_vision_history_and_quit_shortcuts() {
    // Owner request 2026-11-23: ⌘⇧R night vision, ⌘⇧H history, ⌘Q quit.
    let defaults = effective_bindings(&[]).unwrap();
    for (id, key, mods, name) in [
        ("night_vision", "r", MOD_SHIFT, "night_vision"),
        ("history", "h", MOD_SHIFT, "history"),
        ("new_private_tab", "n", MOD_SHIFT, "new_private_tab"),
        ("notebook_page", "d", MOD_SHIFT, "notebook_page"),
        // ⌘⇧\ shows or hides the notebook (2026-11-25).
        ("toggle_notebook", "\\", MOD_SHIFT, "toggle_notebook"),
        ("quit", "q", 0, "quit"),
    ] {
        let i = shortcut_index(id).unwrap();
        assert_eq!(defaults[i], KeyBinding { key: key.into(), mods }, "{id}");
        assert_eq!(SHORTCUTS[i].action.name(), name);
    }
    // ⌘⇧R is not Reload (⌘R).
    assert_ne!(defaults[shortcut_index("reload").unwrap()], defaults[shortcut_index("night_vision").unwrap()]);
    // Quit is macOS only: elsewhere ⌘Q is free for another shortcut.
    assert!(available_on(Shortcut::Quit, true) && !available_on(Shortcut::Quit, false));
    assert!(available_on(Shortcut::NewTab, false));
    let to_q = [("new_tab".to_string(), "q".to_string(), 0)];
    assert!(bindings_for(&to_q, |d| available_on(d.action, true)).unwrap_err().contains("Quit Snorkle"));
    assert!(bindings_for(&to_q, |d| available_on(d.action, false)).is_ok());
}

#[test]
fn arrow_shortcuts_yield_to_text_editing() {
    let defaults = effective_bindings(&[]).unwrap();
    for (id, key, name) in [("back", "left", "go_back"), ("forward", "right", "go_forward"), ("detach_bar", "up", "detach_bar"), ("attach_bar", "down", "attach_bar")] {
        let i = shortcut_index(id).unwrap();
        assert_eq!(defaults[i], KeyBinding { key: key.into(), mods: 0 }, "{id}");
        assert_eq!(SHORTCUTS[i].action.name(), name);
    }
    assert!(validate_binding("Left", MOD_SHIFT).is_some());
    assert!(validate_binding("leftward", 0).is_none() && validate_binding("home", 0).is_none());
    let left = KeyBinding { key: "left".into(), mods: 0 };
    let t = KeyBinding { key: "t".into(), mods: 0 };
    set_text_editing(true);
    assert!(yields_to_editing(&left) && !yields_to_editing(&t));
    set_text_editing(false);
    assert!(!yields_to_editing(&left));
}

#[test]
fn only_holes_over_a_view_mask_it() {
    let hole = |x: f64, y: f64, w: f64, h: f64| Hole { x, y, w, h, r: 0.0, corners: 0 };
    let pill = hole(400.0, 700.0, 500.0, 50.0);
    let sidebar = hole(-1.0, -1.0, 265.0, 802.0);
    let page = (264.0, 0.0, 1016.0, 800.0);
    // The attached sidebar's hole lies beside the page (touching it at most).
    assert_eq!(holes_over(&[pill, sidebar], page), vec![pill]);
    assert!(holes_over(&[sidebar], page).is_empty(), "no mask at all");
    // A flyout over the page does cut into it.
    let fly = hole(-1.0, 19.0, 300.0, 762.0);
    assert_eq!(holes_over(&[fly], page), vec![fly]);
    // A split pane only gets the holes over its own half.
    let left = (264.0, 0.0, 504.0, 800.0);
    let right = (776.0, 0.0, 504.0, 800.0);
    let toast = hole(300.0, 600.0, 200.0, 30.0);
    assert_eq!(holes_over(&[toast], left), vec![toast]);
    assert!(holes_over(&[toast], right).is_empty());
}

#[test]
fn clip_rects_are_cut_to_the_view() {
    // A page 1,000 CSS px wide in a view 500 pt wide: half the size.
    assert_eq!(clip_rect((100.0, 50.0, 200.0, 100.0), 1000.0, (500.0, 400.0)), Some((50.0, 25.0, 100.0, 50.0)));
    assert_eq!(clip_rect((-100.0, 700.0, 300.0, 400.0), 1000.0, (500.0, 400.0)), Some((0.0, 350.0, 100.0, 50.0)));
    assert_eq!(clip_rect((2000.0, 0.0, 100.0, 100.0), 1000.0, (500.0, 400.0)), None, "outside the view");
    assert_eq!(clip_rect((0.0, 0.0, 4.0, 100.0), 1000.0, (500.0, 400.0)), None, "too narrow");
    assert_eq!(clip_rect((0.0, 0.0, f64::NAN, 100.0), 1000.0, (500.0, 400.0)), None);
    assert_eq!(clip_rect((0.0, 0.0, 100.0, 100.0), 0.0, (500.0, 400.0)), None);
}

#[test]
fn store_id_nonzero() {
    assert!(DEFAULT_PROFILE_STORE_ID.iter().any(|b| *b != 0));
}

#[test]
fn preview_rect_stays_in_the_window() {
    assert_eq!(preview_rect((100.0, 80.0, 600.0, 500.0), 1200.0, 800.0), Some((100.0, 80.0, 600.0, 500.0)));
    // Cut to the window, rounded.
    assert_eq!(preview_rect((-20.4, 700.0, 300.0, 400.0), 1200.0, 800.0), Some((0.0, 700.0, 280.0, 100.0)));
    // Not finite, or too small to show a page.
    assert_eq!(preview_rect((f64::NAN, 0.0, 300.0, 300.0), 1200.0, 800.0), None);
    assert_eq!(preview_rect((10.0, 10.0, 30.0, 300.0), 1200.0, 800.0), None);
    assert_eq!(preview_rect((1190.0, 10.0, 300.0, 300.0), 1200.0, 800.0), None);
}
