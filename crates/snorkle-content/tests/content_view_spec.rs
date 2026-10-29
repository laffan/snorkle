//! Security suite: the configuration every content WKWebView is built from.
// The spec is a const, so these assertions are constant by design: they
// fail the build's test run when someone edits CONTENT_VIEW_SPEC.
#![allow(clippy::assertions_on_constants)]

use snorkle_content::CONTENT_VIEW_SPEC as S;

#[test]
fn no_page_to_native_channels() {
    assert!(S.user_scripts.is_empty(), "user scripts inject code into every page");
    assert!(S.script_message_handlers.is_empty(), "window.webkit.messageHandlers is a page-to-native channel");
    assert!(S.url_scheme_handlers.is_empty(), "custom schemes let pages fetch from native code");
}

#[test]
fn anti_spoofing_and_popup_settings() {
    assert!(!S.javascript_can_open_windows_automatically);
    assert!(!S.element_fullscreen, "element fullscreen lets a page hide the address bar");
    assert!(!S.allows_link_preview);
    assert!(S.upgrade_known_hosts_to_https);
    assert!(!S.limits_navigations_to_app_bound_domains);
    assert!(S.fraudulent_website_warning);
    assert!(S.safari_desktop_user_agent);
}

#[test]
#[cfg(not(debug_assertions))]
fn not_inspectable_in_release() {
    assert!(!S.inspectable);
}
