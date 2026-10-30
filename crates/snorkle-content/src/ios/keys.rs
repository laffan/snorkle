//! Hardware-keyboard shortcuts while a web page has focus.
//!
//! When a content `WKWebView` is first responder, key events never reach the
//! chrome's web view, so its JavaScript shortcuts would not fire. Each
//! content container is a `UIView` subclass exposing `UIKeyCommand`s; UIKit
//! finds them through the responder chain (page → web view → container).
//! Pages cannot trigger these: they come from the hardware keyboard via
//! UIKit, and only identify which shortcut was pressed.

use std::cell::OnceCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use objc2_ui_kit::{UIKeyCommand, UIKeyModifierFlags, UIResponder, UIView};

use crate::{ContentEvents, Shortcut};

pub struct KeyIvars {
    events: Rc<dyn ContentEvents>,
    commands: OnceCell<Retained<NSArray<UIKeyCommand>>>,
}

const KEYS: &[(&str, &str)] = &[
    ("t", "New Tab"),
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

        #[unsafe(method(snorkleShortcut:))]
        fn shortcut(&self, command: &UIKeyCommand) {
            let input = command.input().map(|s| s.to_string()).unwrap_or_default();
            let shortcut = match input.as_str() {
                "t" => Some(Shortcut::NewTab),
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
    /// A container view (as `UIView`) that offers the shortcuts.
    pub fn new_view(mtm: MainThreadMarker, frame: CGRect, events: Rc<dyn ContentEvents>) -> Retained<UIView> {
        let this = Self::alloc(mtm).set_ivars(KeyIvars { events, commands: OnceCell::new() });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        Retained::into_super(this)
    }
}
