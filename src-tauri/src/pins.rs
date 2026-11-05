//! Favicons for the pinned grid and (since 2026-11-04) the tab list: fetched
//! per host by the content host's sandboxed fetcher (snorkle-content
//! `ios/favicon.rs`), stored as validated PNGs, and handed to the chrome as
//! `data:` URLs. Failed requests may retry after five minutes.
//! Hosts of private tabs are never fetched or stored.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use snorkle_core::console::Level;
use snorkle_core::storage::{favicon_host, png_summary};

use crate::state::{with_host, ChromeEvent, Shared};

/// Throttle failed requests for five minutes; transient failures can recover.
fn requested() -> &'static Mutex<HashMap<String, Instant>> {
    static R: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fetches favicons for pins that have none yet.
pub fn refresh_icons(shared: &Arc<Shared>) {
    for pin in shared.store.as_ref().map(|s| s.pins()).unwrap_or_default() {
        if let Some(host) = favicon_host(&pin.url) {
            fetch_icon(shared, host);
        }
    }
}

/// Queues a fetch of `host`'s favicon (a validated `favicon_host`) unless
/// it is stored or was recently requested. When one is saved,
/// the chrome reloads pins and tab icons.
pub fn fetch_icon(shared: &Arc<Shared>, host: String) {
    let Some(store) = shared.store.clone() else { return };
    if store.icon(&host).is_some() {
        return;
    }
    {
        let mut requests = requested().lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if requests.get(&host).is_some_and(|last| now.duration_since(*last) < Duration::from_secs(300)) {
            return;
        }
        requests.insert(host.clone(), now);
    }
    let s = shared.clone();
    let h = host.clone();
    let done: snorkle_content::FaviconDone = Box::new(move |png| match png {
        Some(png) if store.save_icon(&h, &png).unwrap_or(false) => {
            s.log(Level::Info, None, &format!("favicon {h}: {}", png_summary(&png)));
            (s.emit)(ChromeEvent::PinsChanged);
            (s.emit)(ChromeEvent::IconsChanged);
        }
        Some(png) => s.log(Level::Warn, None, &format!("favicon {h} refused: {}", png_summary(&png))),
        None => s.log(Level::Info, None, &format!("favicon {h}: none (blank or timed out)")),
    });
    snorkle_content::dispatch_main(move || with_host(|host_view| host_view.fetch_favicon(host, done)));
}
