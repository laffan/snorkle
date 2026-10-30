//! User scripts/styles and view modes (black and white, hidden images):
//! stored scripts and the session's view mode → the injected sources every content view gets. Generation and validation live
//! in `snorkle_core::userscripts`; injection in `snorkle_content`.

use std::sync::Arc;

use snorkle_core::userscripts;

use crate::state::{with_host, Shared};

/// Rebuilds the injected scripts for every content view. `run_now` is
/// constant code run once in every loaded page (the view-mode toggle), so
/// the change is visible without reloading.
pub fn apply(shared: &Arc<Shared>, run_now: Option<String>) {
    let scripts = shared.store.as_ref().map(|s| s.user_scripts()).unwrap_or_default();
    let injections = userscripts::injections(&scripts, shared.view_mode());
    snorkle_content::dispatch_main(move || with_host(|h| h.set_injections(&injections, run_now.as_deref())));
}
