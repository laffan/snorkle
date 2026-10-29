//! User scripts/styles and black-and-white mode: stored settings → the
//! injected sources every content view gets. Generation and validation live
//! in `snorkle_core::userscripts`; injection in `snorkle_content`.

use std::sync::Arc;

use snorkle_core::userscripts;

use crate::state::{with_host, Shared};

/// Rebuilds the injected scripts for every content view from storage.
/// `run_now` is constant code run once in every loaded page (the
/// black-and-white toggle), so the change is visible without reloading.
pub fn apply(shared: &Arc<Shared>, run_now: Option<String>) {
    let Some(store) = shared.store.clone() else { return };
    let injections = userscripts::injections(&store.user_scripts(), store.settings().grayscale);
    snorkle_content::dispatch_main(move || with_host(|h| h.set_injections(&injections, run_now.as_deref())));
}
