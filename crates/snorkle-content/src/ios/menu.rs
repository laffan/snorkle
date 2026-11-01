//! The app's main menu (iPadOS menu bar and ⌘-shortcuts). Every browser
//! shortcut (`crate::SHORTCUTS`: ⌘T, ⌘W, ⌘F, ⌘S, ⌘\, ⌘⇧\, ⌘⇧D, ⌘1–5) is a
//! menu key command, so it works whatever has focus: a page, the chrome, or
//! nothing (with the sidebar hidden and no page, nothing is first
//! responder, and neither the chrome's JavaScript nor the content
//! containers' key commands would see the key). The system's Close
//! (⌘W, "Close Window") and Find menus are replaced.
//!
//! The app delegate is Tauri's (tao), so the methods UIKit needs are added
//! to its class at runtime: `buildMenuWithBuilder:` (edits the menu) and
//! `snorkleShortcut:` (every command's action; it reaches the app delegate
//! through the responder chain). The command's property list names the
//! shortcut. If the class already has either method, nothing is replaced
//! and the content containers' key commands (`keys.rs`) remain the only
//! shortcuts, while a page has focus.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::rc::Rc;

use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, ProtocolObject, Sel};
use objc2::{sel, MainThreadMarker};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use objc2_ui_kit::{
    UIApplication, UIKeyCommand, UIKeyModifierFlags, UIMenu, UIMenuBuilder, UIMenuClose, UIMenuElement, UIMenuFile,
    UIMenuFind, UIMenuOptions, UIMenuSystem, UIMenuView,
};

use crate::{ContentEvents, Shortcut, ShortcutMenu, SHORTCUTS};

thread_local! {
    static EVENTS: RefCell<Option<Rc<dyn ContentEvents>>> = const { RefCell::new(None) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Whether the main menu carries the shortcuts (then the content
/// containers offer none, so no key is bound twice).
pub fn installed() -> bool {
    INSTALLED.with(Cell::get)
}

/// A key command for one shortcut; its property list is the shortcut's name.
pub fn key_command(mtm: MainThreadMarker, title: &str, key: &str, shift: bool, s: Shortcut) -> Retained<UIKeyCommand> {
    let flags =
        if shift { UIKeyModifierFlags::Command | UIKeyModifierFlags::Shift } else { UIKeyModifierFlags::Command };
    unsafe {
        let name = NSString::from_str(&s.name());
        let c = UIKeyCommand::commandWithTitle_image_action_input_modifierFlags_propertyList(
            &NSString::from_str(title),
            None,
            sel!(snorkleShortcut:),
            &NSString::from_str(key),
            flags,
            Some(&name),
            mtm,
        );
        if c.respondsToSelector(sel!(setWantsPriorityOverSystemBehavior:)) {
            c.setWantsPriorityOverSystemBehavior(true);
        }
        c
    }
}

/// The shortcut a key command stands for (its property list).
pub fn shortcut_of(command: &UIKeyCommand) -> Option<Shortcut> {
    let plist = command.propertyList()?;
    let name = plist.downcast_ref::<NSString>()?.to_string();
    Shortcut::from_name(&name)
}

fn inline_menu(mtm: MainThreadMarker, id: &str, title: &str, which: ShortcutMenu) -> Retained<UIMenu> {
    let items: Vec<Retained<UIMenuElement>> = SHORTCUTS
        .iter()
        .filter(|s| s.0 == which)
        .map(|(_, title, key, shift, s)| {
            Retained::into_super(Retained::into_super(key_command(mtm, title, key, *shift, *s)))
        })
        .collect();
    let children: Retained<NSArray<UIMenuElement>> = NSArray::from_retained_slice(&items);
    let options = if title.is_empty() { UIMenuOptions::DisplayInline } else { UIMenuOptions::empty() };
    UIMenu::menuWithTitle_image_identifier_options_children(
        &NSString::from_str(title),
        None,
        Some(&NSString::from_str(id)),
        options,
        &children,
        mtm,
    )
}

unsafe extern "C-unwind" fn build_menu(_this: *mut AnyObject, _cmd: Sel, builder: *mut AnyObject) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(builder) = (unsafe { (builder as *const ProtocolObject<dyn UIMenuBuilder>).as_ref() }) else { return };
    unsafe {
        let file = inline_menu(mtm, "app.snorkle.file", "", ShortcutMenu::File);
        builder.insertChildMenu_atStartOfMenuForIdentifier(&file, UIMenuFile);
        let close = inline_menu(mtm, "app.snorkle.close-tab", "", ShortcutMenu::Close);
        builder.replaceMenuForIdentifier_withMenu(UIMenuClose, &close);
        let find = inline_menu(mtm, "app.snorkle.find", "", ShortcutMenu::Find);
        builder.replaceMenuForIdentifier_withMenu(UIMenuFind, &find);
        let view = inline_menu(mtm, "app.snorkle.view", "", ShortcutMenu::View);
        builder.insertChildMenu_atStartOfMenuForIdentifier(&view, UIMenuView);
        let tabs = inline_menu(mtm, "app.snorkle.tabs", "Tabs", ShortcutMenu::Tabs);
        builder.insertSiblingMenu_afterMenuForIdentifier(&tabs, UIMenuView);
    }
}

unsafe extern "C-unwind" fn perform(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let Some(sender) = (unsafe { sender.as_ref() }) else { return };
    let Some(command) = sender.downcast_ref::<UIKeyCommand>() else { return };
    let Some(s) = shortcut_of(command) else { return };
    EVENTS.with(|e| {
        if let Some(events) = e.borrow().as_ref() {
            events.shortcut(s);
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
        let action: Imp =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(perform);
        let added_build = class_addMethod(cls, sel!(buildMenuWithBuilder:), build, V_AT_AT.as_ptr()).as_bool();
        let added_action = class_addMethod(cls, sel!(snorkleShortcut:), action, V_AT_AT.as_ptr()).as_bool();
        if added_build && added_action {
            INSTALLED.with(|i| i.set(true));
            UIMenuSystem::mainSystem(mtm).setNeedsRebuild();
        }
    }
}
