//! SECURITY-REVIEW: hand-written binding for UIKit's `WKWebView`.
//!
//! `objc2-web-kit` 0.3.2 generates `WKWebView` from the macOS SDK only (an
//! `NSView` subclass), so on iOS we declare the class ourselves, as wry does.
//! Only the selectors the browser uses are declared. Each must match the
//! iOS SDK header exactly; a wrong signature is undefined behaviour, and
//! this file cannot be compiled against the real SDK on CI (Linux). Verify
//! against `WKWebView.h` from the iOS 26 SDK when changing anything here.

#![allow(non_snake_case)]

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{extern_class, extern_methods, MainThreadOnly};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSObjectProtocol, NSString, NSURLRequest, NSURL};
use objc2_ui_kit::{UIResponder, UIScrollView, UIView};
use objc2_web_kit::{WKNavigation, WKNavigationDelegate, WKUIDelegate, WKWebViewConfiguration};

extern_class!(
    /// `@interface WKWebView : UIView` (iOS 8+).
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct WKWebView;
);

unsafe impl NSObjectProtocol for WKWebView {}

impl WKWebView {
    extern_methods!(
        #[unsafe(method(initWithFrame:configuration:))]
        #[unsafe(method_family = init)]
        pub unsafe fn initWithFrame_configuration(
            this: Allocated<Self>,
            frame: CGRect,
            configuration: &WKWebViewConfiguration,
        ) -> Retained<Self>;

        #[unsafe(method(configuration))]
        #[unsafe(method_family = none)]
        pub unsafe fn configuration(&self) -> Retained<WKWebViewConfiguration>;

        #[unsafe(method(setNavigationDelegate:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setNavigationDelegate(&self, delegate: Option<&ProtocolObject<dyn WKNavigationDelegate>>);

        #[unsafe(method(setUIDelegate:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setUIDelegate(&self, delegate: Option<&ProtocolObject<dyn WKUIDelegate>>);

        #[unsafe(method(loadRequest:))]
        #[unsafe(method_family = none)]
        pub unsafe fn loadRequest(&self, request: &NSURLRequest) -> Option<Retained<WKNavigation>>;

        #[unsafe(method(goBack))]
        #[unsafe(method_family = none)]
        pub unsafe fn goBack(&self) -> Option<Retained<WKNavigation>>;

        #[unsafe(method(goForward))]
        #[unsafe(method_family = none)]
        pub unsafe fn goForward(&self) -> Option<Retained<WKNavigation>>;

        #[unsafe(method(reload))]
        #[unsafe(method_family = none)]
        pub unsafe fn reload(&self) -> Option<Retained<WKNavigation>>;

        #[unsafe(method(stopLoading))]
        #[unsafe(method_family = none)]
        pub unsafe fn stopLoading(&self);

        #[unsafe(method(URL))]
        #[unsafe(method_family = none)]
        pub unsafe fn URL(&self) -> Option<Retained<NSURL>>;

        #[unsafe(method(title))]
        #[unsafe(method_family = none)]
        pub unsafe fn title(&self) -> Option<Retained<NSString>>;

        #[unsafe(method(estimatedProgress))]
        #[unsafe(method_family = none)]
        pub unsafe fn estimatedProgress(&self) -> f64;

        #[unsafe(method(hasOnlySecureContent))]
        #[unsafe(method_family = none)]
        pub unsafe fn hasOnlySecureContent(&self) -> bool;

        #[unsafe(method(canGoBack))]
        #[unsafe(method_family = none)]
        pub unsafe fn canGoBack(&self) -> bool;

        #[unsafe(method(canGoForward))]
        #[unsafe(method_family = none)]
        pub unsafe fn canGoForward(&self) -> bool;

        /// iOS 15+. Opaque, in-memory only.
        #[unsafe(method(interactionState))]
        #[unsafe(method_family = none)]
        pub unsafe fn interactionState(&self) -> Option<Retained<AnyObject>>;

        /// iOS 15+.
        #[unsafe(method(setInteractionState:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setInteractionState(&self, state: Option<&AnyObject>);

        /// iOS 16.4+.
        #[unsafe(method(setInspectable:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setInspectable(&self, inspectable: bool);

        #[unsafe(method(setAllowsBackForwardNavigationGestures:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setAllowsBackForwardNavigationGestures(&self, allows: bool);

        #[unsafe(method(setAllowsLinkPreview:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setAllowsLinkPreview(&self, allows: bool);

        /// iOS 9+. Replaces the whole User-Agent header and `navigator.userAgent`.
        #[unsafe(method(setCustomUserAgent:))]
        #[unsafe(method_family = none)]
        pub unsafe fn setCustomUserAgent(&self, user_agent: Option<&NSString>);

        #[unsafe(method(scrollView))]
        #[unsafe(method_family = none)]
        pub unsafe fn scrollView(&self) -> Retained<UIScrollView>;
    );
}
