//! User scripts/styles and view modes: stored scripts, the session's
//! app-wide view modes and per-tab exemptions, and the sites' detected
//! heading rules → what every content view gets injected. Generation and
//! validation live in `snorkle_core::userscripts` / `snorkle_core::viewmodes`;
//! injection in `snorkle_content`.

use std::sync::Arc;

use snorkle_content::{Injections, ViewInjections};
use snorkle_core::userscripts;
use snorkle_core::viewmodes::{self, HeadingRules, ViewModes};

use crate::state::{with_host, Shared};

fn view(modes: ViewModes, rules: &HeadingRules) -> ViewInjections {
    ViewInjections {
        sources: viewmodes::injections(modes, rules),
        apply_js: viewmodes::apply_js(modes, rules),
        headings: modes.headers,
        dark: modes.dark,
    }
}

/// Rebuilds the injected scripts for every content view. With `run_now`,
/// loaded pages are also brought in line with their view modes at once.
pub fn apply(shared: &Arc<Shared>, run_now: bool) {
    let scripts = shared.store.as_ref().map(|s| s.user_scripts()).unwrap_or_default();
    let rules = shared.store.as_ref().map(|s| s.heading_rules()).unwrap_or_default();
    let modes = shared.view_modes();
    let live: Vec<_> = shared.browser().view().tabs.iter().map(|t| t.id).collect();
    shared.exempt().retain(|t, set| live.contains(t) && !set.is_empty());
    shared.headers_tabs().retain(|t| live.contains(t));
    // Tabs whose modes differ from the app-wide ones get their own scripts.
    let tabs = live
        .iter()
        .map(|t| (*t, shared.modes_for(*t)))
        .filter(|(_, m)| *m != modes)
        .map(|(t, m)| (t, view(m, &rules)))
        .collect();
    let injections = Injections { user: userscripts::injections(&scripts), view: view(modes, &rules), tabs };
    snorkle_content::dispatch_main(move || with_host(|h| h.set_injections(injections, run_now)));
}
