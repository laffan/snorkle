//! Ad and tracker blocking: bundled filter lists → rule lists in every
//! content web view. Conversion lives in `snorkle_core::blocking`; WebKit
//! caching and installation in `snorkle_content`. This module only wires
//! them together and reports status to the chrome.

use std::sync::Arc;

use snorkle_core::blocking::{cache_key, compile, FilterList};
use snorkle_core::browser::AdblockStatus;
use snorkle_core::tabs::TabId;

use crate::state::{with_host, Shared};

/// Bundled at build time (see src-tauri/resources/filters/SOURCES.md).
/// EasyList and EasyPrivacy: © The EasyList authors (https://easylist.to/),
/// dual-licensed GPLv3+ / CC BY-SA 3.0+; used here under CC BY-SA 3.0.
const EASYLIST: &str = include_str!("../resources/filters/easylist.txt");
const EASYPRIVACY: &str = include_str!("../resources/filters/easyprivacy.txt");

fn lists() -> [FilterList<'static>; 2] {
    [FilterList { name: "easylist", text: EASYLIST }, FilterList { name: "easyprivacy", text: EASYPRIVACY }]
}

/// (Re)installs the rule lists for the current per-site allowlist. Reuses
/// WebKit's compiled lists when nothing changed; otherwise converts on a
/// background thread. Reloads `reload` once the new lists are active, so a
/// per-site toggle takes effect on the page the user is looking at.
pub fn rebuild(shared: &Arc<Shared>, reload: Option<TabId>) {
    let allow = shared.browser().adblock_allowlist().clone();
    let key = cache_key(&lists(), &allow);
    let effects = shared.browser().set_adblock_status(AdblockStatus::Compiling);
    shared.run(effects);

    let make: snorkle_content::MakeRuleLists = Box::new(move || {
        let started = std::time::Instant::now();
        let compiled = compile(&lists(), &allow).map_err(|e| e.to_string())?;
        if cfg!(debug_assertions) {
            eprintln!(
                "[snorkle] adblock: {} rules + {} exceptions in {} lists, converted in {:?}",
                compiled.rules,
                compiled.exceptions,
                compiled.lists.len(),
                started.elapsed()
            );
        }
        Ok(compiled.lists)
    });
    let s = shared.clone();
    let done: snorkle_content::RulesDone = Box::new(move |result| {
        let status = match result {
            Ok(lists) => AdblockStatus::Active { lists },
            Err(message) => AdblockStatus::Failed { message },
        };
        if cfg!(debug_assertions) {
            eprintln!("[snorkle] adblock: {status:?}");
        }
        let ok = matches!(status, AdblockStatus::Active { .. });
        let mut effects = s.browser().set_adblock_status(status);
        if let (true, Some(tab)) = (ok, reload) {
            effects.extend(s.browser().reload(tab));
        }
        s.run(effects);
    });
    snorkle_content::dispatch_main(move || with_host(|h| h.install_rules(key, make, done)));
}
