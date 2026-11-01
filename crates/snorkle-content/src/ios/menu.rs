//! The app's main menu (iPadOS menu bar and ⌘-shortcuts): the system's
//! "Close Window" (⌘W) is replaced with "Close Tab" (⌘W), so ⌘W closes the
//! current tab instead of the app's only window, whatever has focus.
//!
//! The app delegate is Tauri's (tao), so the two methods UIKit needs are
//! added to its class at runtime: `buildMenuWithBuilder:` (edits the menu)
//! and `snorkleCloseTab:` (the command's action, which reaches the app
//! delegate through the responder chain). If the class already has either
//! method, nothing is replaced and ⌘W still closes tabs while a page has
//! focus (content containers' key commands, `keys.rs`).

use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::Rc;

use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, ProtocolObject, Sel};
use objc2::{sel, MainThreadMarker};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use objc2_ui_kit::{
    UIApplication, UIKeyCommand, UIKeyModifierFlags, UIMenu, UIMenuBuilder, UIMenuClose, UIMenuElement, UIMenuOptions,
    UIMenuSystem,
};

use crate::{ContentEvents, Shortcut};

thread_local! {
    static EVENTS: RefCell<Option<Rc<dyn ContentEvents>>> = const { RefCell::new(None) };
}

unsafe extern "C-unwind" fn build_menu(_this: *mut AnyObject, _cmd: Sel, builder: *mut AnyObject) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(builder) = (unsafe { (builder as *const ProtocolObject<dyn UIMenuBuilder>).as_ref() }) else { return };
    unsafe {
        let close = UIKeyCommand::commandWithTitle_image_action_input_modifierFlags_propertyList(
            &NSString::from_str("Close Tab"),
            None,
            sel!(snorkleCloseTab:),
            &NSString::from_str("w"),
            UIKeyModifierFlags::Command,
            None,
            mtm,
        );
        if close.respondsToSelector(sel!(setWantsPriorityOverSystemBehavior:)) {
            close.setWantsPriorityOverSystemBehavior(true);
        }
        let children: Retained<NSArray<UIMenuElement>> =
            NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(close))]);
        let menu = UIMenu::menuWithTitle_image_identifier_options_children(
            &NSString::from_str(""),
            None,
            Some(&NSString::from_str("app.snorkle.close-tab")),
            UIMenuOptions::DisplayInline,
            &children,
            mtm,
        );
        builder.replaceMenuForIdentifier_withMenu(UIMenuClose, &menu);
    }
}

unsafe extern "C-unwind" fn close_tab(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    EVENTS.with(|e| {
        if let Some(events) = e.borrow().as_ref() {
            events.shortcut(Shortcut::CloseTab);
        }
    });
}

/// Installs the menu changes on the app delegate's class. Main thread.
pub fn install(mtm: MainThreadMarker, events: Rc<dyn ContentEvents>) {
    EVENTS.with(|e| *e.borrow_mut() = Some(events));
    unsafe {
        let app = UIApplication::sharedApplication(mtm);
        let Some(delegate) = app.delegate() else { return };
        let cls = object_getClass(Retained::as_ptr(&delegate).cast()) as *mut AnyClass;
        if cls.is_null() {
            return;
        }
        const V_AT_AT: &CStr = c"v@:@";
        // SAFETY: both functions match the `v@:@` signature (self, _cmd, one
        // object argument, no return value).
        let build: Imp =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(build_menu);
        let close: Imp =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(close_tab);
        let added_build = class_addMethod(cls, sel!(buildMenuWithBuilder:), build, V_AT_AT.as_ptr()).as_bool();
        let added_close = class_addMethod(cls, sel!(snorkleCloseTab:), close, V_AT_AT.as_ptr()).as_bool();
        if added_build && added_close {
            UIMenuSystem::mainSystem(mtm).setNeedsRebuild();
        }
    }
}
