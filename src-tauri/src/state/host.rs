//! The content host on the main thread (`with_host`) and the events it sends
//! back (`Events`).

use super::*;

thread_local! {
    /// The content host lives on the main thread only (UIKit/AppKit objects).
    pub static HOST: RefCell<Option<ContentHost>> = const { RefCell::new(None) };
}

/// Runs `f` with the content host if it exists on this thread.
pub fn with_host(f: impl FnOnce(&mut ContentHost)) {
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            f(host)
        }
    })
}

/// WebKit → core bridge. Called on the main thread from delegate
/// callbacks; locks the core briefly and routes the resulting effects.
pub struct Events {
    pub shared: Arc<Shared>,
}

impl Events {
    pub(super) fn apply(&self, f: impl FnOnce(&mut Browser) -> Vec<Effect>) {
        let effects = f(&mut self.shared.browser());
        self.shared.run(effects);
    }
}

impl Events {
    /// After a page of a pin or bookmark finished loading: saves its
    /// offline copy (under the pin's or bookmark's URL when the tab was
    /// opened from it, else under the page's URL if that is pinned or
    /// bookmarked), at most every `storage::OFFLINE_REFRESH_MS`.
    pub(super) fn save_offline_copy(&self, tab: TabId, landing: Option<String>, page: Option<String>) {
        let Some(store) = self.shared.store.clone() else { return };
        let now = wall_ms();
        let Some(key) = landing.into_iter().chain(page).find(|k| store.offline_copy_wanted(k, now)) else { return };
        let shared = self.shared.clone();
        snorkle_content::dispatch_main(move || {
            with_host(|h| {
                h.save_offline(
                    tab,
                    Box::new(move |data| match data {
                        Some(data) => {
                            shared.log(
                                console::Level::Info,
                                Some(tab),
                                &format!("offline copy saved: {key} ({} KB)", data.len() / 1024),
                            );
                            shared.storage(StorageJob::OfflineCopy { url: key, data });
                        }
                        None => shared.log(console::Level::Info, Some(tab), &format!("offline copy not saved: {key}")),
                    }),
                )
            })
        });
    }

    /// After a non-private page finished loading: reads its text (the
    /// constant `ai::PAGE_TEXT_JS`, channel C17) once it has had
    /// `PAGE_TEXT_DELAY` to fill in, and saves it for history if the tab
    /// still shows that page and is still not private.
    pub(super) fn save_page_text(&self, tab: TabId, page: String) {
        if !page.starts_with("http://") && !page.starts_with("https://") {
            return;
        }
        let shared = self.shared.clone();
        let _ = std::thread::Builder::new().name("snorkle-page-text".into()).spawn(move || {
            std::thread::sleep(PAGE_TEXT_DELAY);
            snorkle_content::dispatch_main(move || {
                with_host(|h| {
                    h.read_page_text(
                        tab,
                        Box::new(move |text| {
                            let still = {
                                let b = shared.browser();
                                b.tab(tab).is_some_and(|t| {
                                    !t.is_private() && t.committed_url.as_ref().map(|u| u.to_string()) == Some(page.clone())
                                }) && !b.shows_offline_copy(tab)
                            };
                            let parsed = text.and_then(|t| snorkle_core::ai::parse_page_text(&t, &page));
                            if let (true, Some(p)) = (still, parsed) {
                                shared.storage(StorageJob::PageText { url: page, title: p.title, text: p.text });
                            }
                        }),
                    )
                })
            });
        });
    }

    /// The network failed for `url`: if a pin's or bookmark's offline copy
    /// exists for it (or for the item the tab was opened from), shows the
    /// copy instead of the error.
    pub(super) fn show_offline_copy(&self, tab: TabId, url: Url, landing: Option<String>) {
        let Some(store) = self.shared.store.clone() else { return };
        let Some((data, saved_ms)) =
            std::iter::once(url.to_string()).chain(landing).find_map(|k| store.offline_copy(&k))
        else {
            return;
        };
        let (ok, effects) = self.shared.browser().use_offline_copy(tab, &url, saved_ms);
        self.shared.run(effects);
        if ok {
            self.shared.log(console::Level::Info, Some(tab), &format!("offline: showing the saved copy of {url}"));
            snorkle_content::dispatch_main(move || with_host(|h| h.load_offline(tab, &url, data)));
        }
    }
}
