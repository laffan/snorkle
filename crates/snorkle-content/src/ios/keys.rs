//! Hardware-keyboard shortcuts while a web page has focus.
//!
//! When a content `WKWebView` is first responder, key events never reach the
//! chrome's web view, so its JavaScript shortcuts would not fire. Each
//! content container is a `UIView` subclass exposing `UIKeyCommand`s; UIKit
//! finds them through the responder chain (page → web view → container).
//! Pages cannot trigger these: they come from the hardware keyboard via
//! UIKit, and only identify which shortcut was pressed.
//!
//! The container also implements the cut-outs for chrome floating over the
//! page (`crate::Hole`, drawn by `super::apply_mask`): points inside a hole
//! are not inside the container, so touches and pointer hover there reach
//! the chrome's web view underneath instead of the page.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use objc2_ui_kit::{UIKeyCommand, UIKeyModifierFlags, UIResponder, UIScrollView, UIView};

use crate::{ContentEvents, Hole, Shortcut};

/// The current holes, shared by the host and every container.
pub type Holes = Rc<RefCell<Vec<Hole>>>;

pub struct KeyIvars {
    events: Rc<dyn ContentEvents>,
    commands: OnceCell<Retained<NSArray<UIKeyCommand>>>,
    holes: Holes,
}

const KEYS: &[(&str, &str)] = &[
    ("t", "New Tab"),
    ("w", "Close Tab"),
    ("s", "Show or Hide Sidebar"),
    ("1", "Go to Item 1"),
    ("2", "Go to Item 2"),
    ("3", "Go to Item 3"),
    ("4", "Go to Item 4"),
    ("5", "Go to Item 5"),
];

define_class!(
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleContentContainer"]
    #[ivars = KeyIvars]
    pub struct KeyContainer;

    impl KeyContainer {
        #[unsafe(method_id(keyCommands))]
        fn key_commands(&self) -> Option<Retained<NSArray<UIKeyCommand>>> {
            let mtm = self.mtm();
            Some(
                self.ivars()
                    .commands
                    .get_or_init(|| {
                        let cmds: Vec<Retained<UIKeyCommand>> = KEYS
                            .iter()
                            .map(|(input, title)| unsafe {
                                let c = UIKeyCommand::keyCommandWithInput_modifierFlags_action(
                                    &NSString::from_str(input),
                                    UIKeyModifierFlags::Command,
                                    sel!(snorkleShortcut:),
                                    mtm,
                                );
                                c.setDiscoverabilityTitle(Some(&NSString::from_str(title)));
                                if c.respondsToSelector(sel!(setWantsPriorityOverSystemBehavior:)) {
                                    c.setWantsPriorityOverSystemBehavior(true);
                                }
                                c
                            })
                            .collect();
                        NSArray::from_retained_slice(&cmds)
                    })
                    .clone(),
            )
        }

        /// Outside the holes only (see the module docs). Containers are
        /// direct subviews of the root view, so window coordinates are the
        /// point plus the container's origin.
        #[unsafe(method(pointInside:withEvent:))]
        fn point_inside(&self, point: CGPoint, event: Option<&AnyObject>) -> bool {
            let o = self.frame().origin;
            let (x, y) = (point.x + o.x, point.y + o.y);
            let in_hole = self.ivars().holes.borrow().iter().any(|h| h.contains(x, y));
            !in_hole && unsafe { msg_send![super(self), pointInside: point, withEvent: event] }
        }

        /// After the content area grew: if the page is drawn narrower than
        /// the view (WebKit kept the smaller zoom scale it used while the
        /// area was narrow), zoom so the page fills the width again.
        #[unsafe(method(snorkleFitWidth))]
        fn fit_width(&self) {
            for sub in self.subviews().iter() {
                if !sub.respondsToSelector(sel!(scrollView)) {
                    continue;
                }
                let scroll: Retained<UIScrollView> = unsafe { msg_send![&*sub, scrollView] };
                let width = scroll.bounds().size.width;
                let content = scroll.contentSize().width;
                let zoom = scroll.zoomScale();
                if content > 1.0 && content + 1.0 < width {
                    let fit = (zoom * width / content).min(scroll.maximumZoomScale());
                    if fit > zoom {
                        scroll.setZoomScale_animated(fit, false);
                    }
                }
            }
        }

        #[unsafe(method(snorkleShortcut:))]
        fn shortcut(&self, command: &UIKeyCommand) {
            let input = command.input().map(|s| s.to_string()).unwrap_or_default();
            let shortcut = match input.as_str() {
                "t" => Some(Shortcut::NewTab),
                "w" => Some(Shortcut::CloseTab),
                "s" => Some(Shortcut::ToggleSidebar),
                n => n.parse::<u8>().ok().filter(|n| (1..=5).contains(n)).map(Shortcut::Item),
            };
            if let Some(s) = shortcut {
                self.ivars().events.shortcut(s);
            }
        }
    }
);

impl KeyContainer {
    /// A container view (as `UIView`) that offers the shortcuts and leaves
    /// out `holes`.
    pub fn new_view(
        mtm: MainThreadMarker,
        frame: CGRect,
        events: Rc<dyn ContentEvents>,
        holes: Holes,
    ) -> Retained<UIView> {
        let this = Self::alloc(mtm).set_ivars(KeyIvars { events, commands: OnceCell::new(), holes });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        Retained::into_super(this)
    }
}
