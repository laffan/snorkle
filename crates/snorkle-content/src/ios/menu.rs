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
    UIApplication, UICommand, UIKeyCommand, UIKeyModifierFlags, UIMenu, UIMenuBuilder, UIMenuClose, UIMenuElement,
    UIMenuFile, UIMenuFind, UIMenuIdentifier, UIMenuOptions, UIMenuSystem, UIMenuView,
};

use crate::{
    shortcut_index, ContentEvents, KeyBinding, Shortcut, ShortcutMenu, MOD_ALT, MOD_CTRL, MOD_SHIFT, SHORTCUTS,
};

thread_local! {
    static EVENTS: RefCell<Option<Rc<dyn ContentEvents>>> = const { RefCell::new(None) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    /// Effective binding of each `SHORTCUTS` entry (the user's or default).
    static BINDINGS: RefCell<Vec<KeyBinding>> = RefCell::new(
        SHORTCUTS.iter().map(|s| KeyBinding { key: s.key.to_string(), mods: s.mods }).collect()
    );
    /// While the shortcut editor records keys, no shortcut is bound.
    static SUSPENDED: Cell<bool> = const { Cell::new(false) };
}

/// Whether the main menu carries the shortcuts (then the content
/// containers offer none, so no key is bound twice). Set when a menu build
/// succeeds; cleared if one throws.
pub fn installed() -> bool {
    INSTALLED.with(Cell::get)
}

/// Whether shortcuts are off (the editor is recording a key).
pub fn suspended() -> bool {
    SUSPENDED.with(Cell::get)
}

/// New bindings (already validated, one per `SHORTCUTS` entry) and the
/// suspended state; the menu is rebuilt.
pub fn set_bindings(mtm: MainThreadMarker, bindings: Vec<KeyBinding>, suspend: bool) {
    if bindings.len() == SHORTCUTS.len() {
        BINDINGS.with(|b| *b.borrow_mut() = bindings);
    }
    SUSPENDED.with(|s| s.set(suspend));
    UIMenuSystem::mainSystem(mtm).setNeedsRebuild();
}

/// The key command for `SHORTCUTS[index]` with its current binding. Its
/// property list is the entry's id, unique as UIKit requires.
pub fn key_command(mtm: MainThreadMarker, index: usize) -> Retained<UIKeyCommand> {
    let def = SHORTCUTS[index];
    let b = BINDINGS.with(|b| b.borrow()[index].clone());
    let mut flags = UIKeyModifierFlags::Command;
    if b.mods & MOD_SHIFT != 0 {
        flags |= UIKeyModifierFlags::Shift;
    }
    if b.mods & MOD_ALT != 0 {
        flags |= UIKeyModifierFlags::Alternate;
    }
    if b.mods & MOD_CTRL != 0 {
        flags |= UIKeyModifierFlags::Control;
    }
    unsafe {
        let plist = NSString::from_str(def.id);
        let c = UIKeyCommand::commandWithTitle_image_action_input_modifierFlags_propertyList(
            &NSString::from_str(def.title),
            None,
            sel!(snorkleShortcut:),
            &NSString::from_str(&b.key),
            flags,
            Some(&plist),
            mtm,
        );
        if c.respondsToSelector(sel!(setWantsPriorityOverSystemBehavior:)) {
            c.setWantsPriorityOverSystemBehavior(true);
        }
        c
    }
}

/// The shortcut a command stands for (its property list is the id).
pub fn shortcut_of(command: &UICommand) -> Option<Shortcut> {
    let plist = command.propertyList()?;
    let id = plist.downcast_ref::<NSString>()?.to_string();
    shortcut_index(&id).map(|i| SHORTCUTS[i].action)
}

fn inline_menu(mtm: MainThreadMarker, id: &str, title: &str, which: ShortcutMenu) -> Retained<UIMenu> {
    let items: Vec<Retained<UIMenuElement>> = if suspended() {
        Vec::new()
    } else {
        (0..SHORTCUTS.len())
            .filter(|i| SHORTCUTS[*i].menu == which)
            .map(|i| Retained::into_super(Retained::into_super(key_command(mtm, i))))
            .collect()
    };
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
    // An Objective-C exception escaping here (UIKit validates the menu as
    // it is edited) would terminate the app at launch. Catch it, keep what
    // UIKit built, and fall back to the content containers' key commands.
    let built = objc2::exception::catch(std::panic::AssertUnwindSafe(|| build(mtm, builder)));
    INSTALLED.with(|i| i.set(built.is_ok()));
    let note = match built {
        Ok(menus) => format!(
            "menu: shortcuts in the menu bar ({menus}){}",
            if suspended() { ", suspended while recording a key" } else { "" }
        ),
        Err(e) => {
            let reason = e.map(|e| format!("{e:?}")).unwrap_or_default();
            format!("menu: shortcuts NOT in the menu bar (page key commands instead): {reason}")
        }
    };
    eprintln!("[snorkle] {note}");
    EVENTS.with(|e| {
        if let Some(events) = e.borrow().as_ref() {
            events.note(&note);
        }
    });
}

/// Edits the menu. Each target menu is looked up first: menus differ
/// between iPadOS versions, and a missing one is skipped.
/// Returns which menus were changed.
fn build(mtm: MainThreadMarker, builder: &ProtocolObject<dyn UIMenuBuilder>) -> String {
    let has = |id: &UIMenuIdentifier| builder.menuForIdentifier(id).is_some();
    let mut done: Vec<&str> = Vec::new();
    unsafe {
        if has(UIMenuFile) {
            let file = inline_menu(mtm, "app.snorkle.file", "", ShortcutMenu::File);
            builder.insertChildMenu_atStartOfMenuForIdentifier(&file, UIMenuFile);
            done.push("File");
        }
        if has(UIMenuClose) {
            let close = inline_menu(mtm, "app.snorkle.close-tab", "", ShortcutMenu::Close);
            builder.replaceMenuForIdentifier_withMenu(UIMenuClose, &close);
            done.push("Close");
        }
        if has(UIMenuFind) {
            let find = inline_menu(mtm, "app.snorkle.find", "", ShortcutMenu::Find);
            builder.replaceMenuForIdentifier_withMenu(UIMenuFind, &find);
            done.push("Find");
        }
        if has(UIMenuView) {
            let view = inline_menu(mtm, "app.snorkle.view", "", ShortcutMenu::View);
            builder.insertChildMenu_atStartOfMenuForIdentifier(&view, UIMenuView);
            let tabs = inline_menu(mtm, "app.snorkle.tabs", "Tabs", ShortcutMenu::Tabs);
            builder.insertSiblingMenu_afterMenuForIdentifier(&tabs, UIMenuView);
            done.push("View, Tabs");
        }
    }
    if done.is_empty() { "no target menus found".into() } else { done.join(", ") }
}

unsafe extern "C-unwind" fn perform(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let Some(sender) = (unsafe { sender.as_ref() }) else { return };
    let Some(command) = sender.downcast_ref::<UICommand>() else { return };
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
            UIMenuSystem::mainSystem(mtm).setNeedsRebuild();
        }
    }
}
