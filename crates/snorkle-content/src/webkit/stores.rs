//! SECURITY-REVIEW: the configuration every content `WKWebViewConfiguration`
//! is built with (from `CONTENT_VIEW_SPEC` only), and deleting website data
//! from the profiles' data stores. Shared by the iPadOS and macOS hosts.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_foundation::{NSDate, NSObjectProtocol, NSSet, NSString};
use objc2_web_kit::{
    WKContentMode, WKInactiveSchedulingPolicy, WKWebViewConfiguration, WKWebsiteDataStore, WKWebsiteDataTypeCookies,
    WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeFetchCache, WKWebsiteDataTypeFileSystem,
    WKWebsiteDataTypeIndexedDBDatabases, WKWebsiteDataTypeLocalStorage, WKWebsiteDataTypeMediaKeys,
    WKWebsiteDataTypeMemoryCache, WKWebsiteDataTypeOfflineWebApplicationCache,
    WKWebsiteDataTypeServiceWorkerRegistrations, WKWebsiteDataTypeSessionStorage, WKWebsiteDataTypeWebSQLDatabases,
};

use crate::{DataKind, CONTENT_VIEW_SPEC};

thread_local! {
    /// The process pool all page views share (main thread only).
    #[allow(deprecated)]
    static SHARED_POOL: std::cell::OnceCell<Retained<objc2_web_kit::WKProcessPool>> = const { std::cell::OnceCell::new() };
}

/// A new configuration for a content web view in `store`, set up from
/// `CONTENT_VIEW_SPEC`. No user scripts, message handlers or URL scheme
/// handlers: injection (`injection.rs`) and rule lists (`rules.rs`) are
/// added to its controller when the view is mounted.
///
/// # Safety
/// Main thread; WebKit setters.
pub unsafe fn content_configuration(
    mtm: MainThreadMarker,
    store: &WKWebsiteDataStore,
) -> Retained<WKWebViewConfiguration> {
    let spec = &CONTENT_VIEW_SPEC;
    let config = WKWebViewConfiguration::new(mtm);
    config.setWebsiteDataStore(store);
    // One process pool for every page view (2026-11-23). Left to itself,
    // each configuration gets a pool of its own, and each new tab's web
    // process started cold before its page could begin; a shared pool lets
    // WebKit have the next process ready. Search (a WebKit browser for the
    // Mac) measured Return or a bookmark to load start at 41-59 ms without
    // and 9-10 ms with it on macOS 26. Apple's headers call the class
    // deprecated ("no longer has any effect"); setting it costs nothing if
    // so. To be measured (docs/PERFORMANCE.md). Data stores stay separate:
    // a pool is not a profile.
    #[allow(deprecated)]
    SHARED_POOL.with(|pool| {
        let pool = pool.get_or_init(|| objc2_web_kit::WKProcessPool::new(mtm));
        config.setProcessPool(pool);
    });

    let prefs = config.preferences();
    prefs.setJavaScriptCanOpenWindowsAutomatically(spec.javascript_can_open_windows_automatically);
    prefs.setFraudulentWebsiteWarningEnabled(spec.fraudulent_website_warning);
    if prefs.respondsToSelector(sel!(setElementFullscreenEnabled:)) {
        prefs.setElementFullscreenEnabled(spec.element_fullscreen);
    }
    if prefs.respondsToSelector(sel!(setInactiveSchedulingPolicy:)) {
        prefs.setInactiveSchedulingPolicy(WKInactiveSchedulingPolicy::Suspend);
    }
    if config.respondsToSelector(sel!(setUpgradeKnownHostsToHTTPS:)) {
        config.setUpgradeKnownHostsToHTTPS(spec.upgrade_known_hosts_to_https);
    }
    if config.respondsToSelector(sel!(setLimitsNavigationsToAppBoundDomains:)) {
        config.setLimitsNavigationsToAppBoundDomains(spec.limits_navigations_to_app_bound_domains);
    }
    // iOS-only property, absent from the macOS-generated bindings.
    if config.respondsToSelector(sel!(setAllowsInlineMediaPlayback:)) {
        let _: () = msg_send![&*config, setAllowsInlineMediaPlayback: true];
    }

    if spec.safari_desktop_user_agent {
        // Desktop-class content to match the desktop UA set on the view
        // (Safari's default on iPad, except in narrow multitasking; always
        // the case on macOS).
        config.defaultWebpagePreferences().setPreferredContentMode(WKContentMode::Desktop);
    }
    config
}

/// Deletes one kind of website data from each of `stores`.
///
/// # Safety
/// Main thread.
pub unsafe fn clear_data<'a>(stores: impl IntoIterator<Item = &'a WKWebsiteDataStore>, kind: DataKind) {
    let cache = [
        WKWebsiteDataTypeDiskCache,
        WKWebsiteDataTypeMemoryCache,
        WKWebsiteDataTypeFetchCache,
        WKWebsiteDataTypeOfflineWebApplicationCache,
    ];
    let site = [
        WKWebsiteDataTypeLocalStorage,
        WKWebsiteDataTypeSessionStorage,
        WKWebsiteDataTypeIndexedDBDatabases,
        WKWebsiteDataTypeWebSQLDatabases,
        WKWebsiteDataTypeServiceWorkerRegistrations,
        WKWebsiteDataTypeFileSystem,
        WKWebsiteDataTypeMediaKeys,
    ];
    let list: Vec<&NSString> = match kind {
        DataKind::Cache => cache.to_vec(),
        DataKind::Cookies => vec![WKWebsiteDataTypeCookies],
        DataKind::SiteData => site.to_vec(),
        DataKind::Logins => std::iter::once(WKWebsiteDataTypeCookies).chain(site).collect(),
    };
    let types = NSSet::from_slice(&list);
    for store in stores {
        let done = RcBlock::new(|| {});
        store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done);
    }
}

/// Removes everything from a (private) store before it is released, in
/// case WebKit keeps it alive internally.
///
/// # Safety
/// Main thread.
pub unsafe fn wipe(mtm: MainThreadMarker, store: &WKWebsiteDataStore) {
    let types = WKWebsiteDataStore::allWebsiteDataTypes(mtm);
    let done = RcBlock::new(|| {});
    store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done);
}
