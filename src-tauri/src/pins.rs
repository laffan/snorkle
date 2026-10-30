//! Pinned grid: favicons are fetched once per host by the content host's
//! sandboxed fetcher (snorkle-content `ios/favicon.rs`), stored as validated
//! PNGs, and handed to the chrome as `data:` URLs.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use snorkle_core::storage::favicon_host;

use crate::state::{with_host, ChromeEvent, Shared};

/// Hosts already requested this session (successful or not), so a site
/// without a favicon is not refetched on every change.
fn requested() -> &'static Mutex<HashSet<String>> {
    static R: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Fetches favicons for pins that have none yet.
pub fn refresh_icons(shared: &Arc<Shared>) {
    let Some(store) = shared.store.clone() else { return };
    for pin in store.pins() {
        let Some(host) = favicon_host(&pin.url) else { continue };
        if store.icon(&host).is_some() || !requested().lock().unwrap_or_else(|e| e.into_inner()).insert(host.clone()) {
            continue;
        }
        let s = shared.clone();
        let st = store.clone();
        let h = host.clone();
        let done: snorkle_content::FaviconDone = Box::new(move |png| {
            if let Some(png) = png {
                if st.save_icon(&h, &png).unwrap_or(false) {
                    (s.emit)(ChromeEvent::PinsChanged);
                }
            }
        });
        snorkle_content::dispatch_main(move || with_host(|host_view| host_view.fetch_favicon(host, done)));
    }
}
