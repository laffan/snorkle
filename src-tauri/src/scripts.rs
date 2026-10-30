//! User scripts/styles and view modes: stored scripts, the session's
//! app-wide view modes and per-tab exemptions → what every content view
//! gets injected. Generation and validation live in
//! `snorkle_core::userscripts` / `snorkle_core::viewmodes`; injection in
//! `snorkle_content`.

use std::sync::Arc;

use snorkle_content::{Injections, ViewInjections};
use snorkle_core::userscripts;
use snorkle_core::viewmodes::{self, ViewModes};

use crate::state::{with_host, Shared};

fn view(modes: ViewModes) -> ViewInjections {
    ViewInjections {
        sources: viewmodes::injections(modes),
        apply_js: viewmodes::apply_js(modes),
        headings: modes.headers,
    }
}

/// Rebuilds the injected scripts for every content view. With `run_now`,
/// loaded pages are also brought in line with their view modes at once.
pub fn apply(shared: &Arc<Shared>, run_now: bool) {
    let scripts = shared.store.as_ref().map(|s| s.user_scripts()).unwrap_or_default();
    let modes = shared.view_modes();
    let tabs = {
        let live: Vec<_> = shared.browser().view().tabs.iter().map(|t| t.id).collect();
        let mut exempt = shared.exempt();
        exempt.retain(|t, set| live.contains(t) && !set.is_empty());
        exempt.iter().map(|(t, set)| (*t, view(modes.for_tab(set)))).collect()
    };
    let injections = Injections { user: userscripts::injections(&scripts), view: view(modes), tabs };
    snorkle_content::dispatch_main(move || with_host(|h| h.set_injections(injections, run_now)));
}
