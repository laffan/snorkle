//! Fuzz: arbitrary sequences of chrome actions and WebKit events against
//! the Browser state machine. Invariants checked after every step:
//! - live web views never exceed the memory budget;
//! - nothing from a private tab reaches history or the saved session;
//! - the address bar only ever shows a URL that was committed in that tab.
#![no_main]
use std::collections::{HashMap, HashSet};

use libfuzzer_sys::fuzz_target;
use snorkle_core::browser::{Browser, Effect, NavErrorKind, NavKind};
use snorkle_core::os::OsSupport;
use snorkle_core::policy::Frame;
use snorkle_core::tabs::{MemoryBudget, Profile, TabId};
use snorkle_core::Url;

const PUBLIC: &[&str] = &[
    "https://a.example/", "https://a.example/x", "http://b.example/", "https://b.example/",
    "https://c.example/#f", "javascript:void(0)", "data:text/html,x", "mailto:a@b.example",
    "tauri://localhost/", "about:blank", "blob:https://a.example/1",
];
const SECRET: &[&str] = &["https://secret.example/", "https://secret.example/2", "http://secret.example/3"];

fuzz_target!(|data: &[u8]| {
    let max_live = 1 + (data.first().copied().unwrap_or(0) % 4) as usize;
    let mut b = Browser::new(MemoryBudget { max_live, background_ttl_ms: 5_000 }, OsSupport::Supported);
    let mut tabs: Vec<(TabId, Profile)> = Vec::new();
    let mut committed: HashMap<TabId, HashSet<String>> = HashMap::new();
    let mut now = 0u64;

    for chunk in data.chunks(3).skip(1) {
        let (op, a, c) = (chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0));
        now += (c as u64) * 100;
        let pick = |tabs: &Vec<(TabId, Profile)>| tabs.get(a as usize % tabs.len().max(1)).copied();
        let url_for = |p: Profile, i: u8| {
            let set = if p == Profile::Private { SECRET } else { PUBLIC };
            Url::parse(set[i as usize % set.len()]).unwrap()
        };
        let effects: Vec<Effect> = match op % 14 {
            0 | 1 => {
                let p = if op % 2 == 0 { Profile::Default } else { Profile::Private };
                let (id, e) = b.new_tab(p, now);
                tabs.push((id, p));
                e
            }
            2 => match pick(&tabs) {
                Some((id, _)) => {
                    tabs.retain(|t| t.0 != id);
                    let e = b.close(id, now);
                    // close() may open a replacement default tab
                    for t in b.view().tabs {
                        if !tabs.iter().any(|x| x.0 == t.id) {
                            tabs.push((t.id, if t.private { Profile::Private } else { Profile::Default }));
                        }
                    }
                    e
                }
                None => vec![],
            },
            3 => pick(&tabs).map(|(id, _)| b.activate(id, now)).unwrap_or_default(),
            4 => pick(&tabs).map(|(id, p)| b.navigate_trusted(id, url_for(p, c), now)).unwrap_or_default(),
            5 => pick(&tabs)
                .map(|(id, p)| {
                    let kind = [NavKind::LinkActivated, NavKind::Other, NavKind::BackForward, NavKind::FormSubmitted][c as usize % 4];
                    let frame = if c & 0x10 == 0 { Frame::Main } else { Frame::Sub };
                    b.decide_navigation(id, &url_for(p, c), frame, kind, now).1
                })
                .unwrap_or_default(),
            6 => pick(&tabs).map(|(id, _)| b.on_provisional_start(id)).unwrap_or_default(),
            7 => pick(&tabs)
                .map(|(id, p)| {
                    let u = url_for(p, c);
                    committed.entry(id).or_default().insert(u.to_string());
                    b.on_commit(id, u)
                })
                .unwrap_or_default(),
            8 => pick(&tabs).map(|(id, p)| b.on_url_changed(id, url_for(p, c))).unwrap_or_default(),
            9 => pick(&tabs).map(|(id, _)| b.on_finish(id)).unwrap_or_default(),
            10 => pick(&tabs)
                .map(|(id, _)| {
                    let kind = [NavErrorKind::Cancelled, NavErrorKind::Certificate, NavErrorKind::Network, NavErrorKind::Other][c as usize % 4];
                    b.on_fail(id, c & 1 == 0, kind, None, "err")
                })
                .unwrap_or_default(),
            11 => pick(&tabs).map(|(id, _)| b.on_process_terminated(id, now)).unwrap_or_default(),
            12 => pick(&tabs)
                .map(|(id, p)| {
                    let e = b.on_new_window_request(id, &url_for(p, c), now);
                    for t in b.view().tabs {
                        if !tabs.iter().any(|x| x.0 == t.id) {
                            tabs.push((t.id, if t.private { Profile::Private } else { Profile::Default }));
                        }
                    }
                    e
                })
                .unwrap_or_default(),
            13 if c & 4 == 0 => pick(&tabs)
                .map(|(id, p)| {
                    let (popup, e) = b.open_popup(id, Some(&url_for(p, c)), now);
                    if let Some(pid) = popup {
                        tabs.push((pid, p));
                    }
                    e
                })
                .unwrap_or_default(),
            13 if c & 8 == 0 => pick(&tabs)
                .map(|(id, _)| {
                    let e = b.on_close_requested(id, now);
                    tabs.retain(|t| b.tab(t.0).is_some());
                    for t in b.view().tabs {
                        if !tabs.iter().any(|x| x.0 == t.id) {
                            tabs.push((t.id, if t.private { Profile::Private } else { Profile::Default }));
                        }
                    }
                    e
                })
                .unwrap_or_default(),
            13 if c & 2 == 0 => match (pick(&tabs), tabs.get(c as usize % tabs.len().max(1)).copied()) {
                (Some((x, _)), Some((y, _))) => b.split(x, y, now),
                _ => vec![],
            },
            _ => {
                if c & 1 == 0 { b.on_memory_warning() } else { b.tick(now) }
            }
        };

        assert!(b.live_count() <= max_live, "live {} > budget {}", b.live_count(), max_live);
        for e in &effects {
            match e {
                Effect::RecordHistory { url, .. } => assert!(!url.as_str().contains("secret"), "private history leaked"),
                Effect::PersistSession(s) => assert!(s.tabs.iter().all(|t| !t.url.contains("secret")), "private session leaked"),
                _ => {}
            }
        }
        for t in b.view().tabs {
            if let Some(d) = t.url {
                // Same origin as something committed in this tab (pushState
                // can change the path), or identical display for opaque URLs.
                let shown = Url::parse(&d.full).ok();
                let ok = committed.get(&t.id).map(|set| {
                    set.iter().any(|c| {
                        let cu = Url::parse(c).unwrap();
                        match &shown {
                            Some(su) if cu.origin().is_tuple() => su.origin() == cu.origin(),
                            _ => snorkle_core::url_display::display(&cu).full == d.full,
                        }
                    })
                });
                assert!(ok.unwrap_or(false), "tab {:?} shows {} which was never committed there", t.id, d.full);
            }
        }
    }
});
