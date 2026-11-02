//! Keyboard shortcuts (`crate::SHORTCUTS`, with the user's bindings).
//!
//! The shortcuts are key commands vended by the app delegate (`keyCommands`),
//! the last responder in every responder chain, so UIKit finds them
//! whatever has focus: a page, the chrome, or nothing. They are not menu-bar
//! commands: building them into the main menu tied them to the menu
//! system's validation and to conflicts with the system's own key
//! equivalents (e.g. Format → Italic, ⌘I), and they stopped working. The
//! menu bar is changed only to remove the system's Close (⌘W closes the
//! window) and Find (⌘F), whose keys the browser uses.
//!
//! Recording a new binding (Settings → Keyboard) is done here too: while
//! recording, the app delegate offers a key command for every ⌘ combination
//! the editor accepts (and Escape), so the pressed keys arrive natively
//! rather than depending on WebKit passing ⌘ keys to the chrome's page.
//!
//! The app delegate is Tauri's (tao); the methods are added to its class at
//! runtime. If the class already has any of them, nothing is replaced and
//! the content containers' key commands (`keys.rs`) are the fallback, while
//! a page has focus.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::rc::Rc;

use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, ProtocolObject, Sel};
use objc2::{sel, MainThreadMarker};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};
use objc2_ui_kit::{
    UIApplication, UICommand, UIKeyCommand, UIKeyInputEscape, UIKeyModifierFlags, UIMenuBuilder, UIMenuClose,
    UIMenuFind, UIMenuSystem,
};

use crate::{shortcut_index, ContentEvents, KeyBinding, Shortcut, MOD_ALT, MOD_CTRL, MOD_SHIFT, SHORTCUTS};

thread_local! {
    static EVENTS: RefCell<Option<Rc<dyn ContentEvents>>> = const { RefCell::new(None) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    /// Effective binding of each `SHORTCUTS` entry (the user's or default).
    static BINDINGS: RefCell<Vec<KeyBinding>> = RefCell::new(
        SHORTCUTS.iter().map(|s| KeyBinding { key: s.key.to_string(), mods: s.mods }).collect()
    );
    /// The editor is recording a key: no shortcut runs; every candidate
    /// combination is reported instead.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    /// The key commands currently offered (rebuilt when bindings change).
    static COMMANDS: RefCell<Option<Retained<NSArray<UIKeyCommand>>>> = const { RefCell::new(None) };
}

/// Keys the editor accepts (as in `crate::validate_binding`).
const RECORD_KEYS: &str = "abcdefghijklmnopqrstuvwxyz0123456789[]\\;',./-=`";
const RECORD_CANCEL: &str = "record-cancel";

/// Whether the app delegate offers the shortcuts (then the content
/// containers offer none).
pub fn installed() -> bool {
    INSTALLED.with(Cell::get)
}

/// Whether a key is being recorded.
pub fn suspended() -> bool {
    RECORDING.with(Cell::get)
}

/// New bindings (validated, one per `SHORTCUTS` entry) and whether the
/// editor is recording a key.
pub fn set_bindings(_mtm: MainThreadMarker, bindings: Vec<KeyBinding>, record: bool) {
    if bindings.len() == SHORTCUTS.len() {
        BINDINGS.with(|b| *b.borrow_mut() = bindings);
    }
    RECORDING.with(|r| r.set(record));
    COMMANDS.with(|c| *c.borrow_mut() = None);
}

fn flags(mods: u8) -> UIKeyModifierFlags {
    let mut f = UIKeyModifierFlags::Command;
    if mods & MOD_SHIFT != 0 {
        f |= UIKeyModifierFlags::Shift;
    }
    if mods & MOD_ALT != 0 {
        f |= UIKeyModifierFlags::Alternate;
    }
    if mods & MOD_CTRL != 0 {
        f |= UIKeyModifierFlags::Control;
    }
    f
}

fn command(mtm: MainThreadMarker, title: Option<&str>, input: &NSString, f: UIKeyModifierFlags, action: Sel, plist: &str) -> Retained<UIKeyCommand> {
    unsafe {
        let c = UIKeyCommand::commandWithTitle_image_action_input_modifierFlags_propertyList(
            &NSString::from_str(title.unwrap_or("")),
            None,
            action,
            input,
            f,
            Some(&NSString::from_str(plist)),
            mtm,
        );
        if let Some(t) = title {
            c.setDiscoverabilityTitle(Some(&NSString::from_str(t)));
        }
        if c.respondsToSelector(sel!(setWantsPriorityOverSystemBehavior:)) {
            c.setWantsPriorityOverSystemBehavior(true);
        }
        c
    }
}

/// The key command for `SHORTCUTS[index]` with its current binding. Its
/// property list is the entry's id.
pub fn key_command(mtm: MainThreadMarker, index: usize) -> Retained<UIKeyCommand> {
    let def = SHORTCUTS[index];
    let b = BINDINGS.with(|b| b.borrow()[index].clone());
    command(mtm, Some(def.title), &NSString::from_str(&b.key), flags(b.mods), sel!(snorkleShortcut:), def.id)
}

/// What the app delegate offers: the shortcuts, or while recording, every
/// candidate combination and Escape.
fn commands(mtm: MainThreadMarker) -> Retained<NSArray<UIKeyCommand>> {
    COMMANDS.with(|cache| {
        if let Some(c) = cache.borrow().as_ref() {
            return c.clone();
        }
        let list: Vec<Retained<UIKeyCommand>> = if suspended() {
            let mut v = Vec::new();
            for key in RECORD_KEYS.chars() {
                let input = NSString::from_str(&key.to_string());
                for mods in 0..8u8 {
                    v.push(command(mtm, None, &input, flags(mods), sel!(snorkleRecord:), &format!("record\t{key}\t{mods}")));
                }
            }
            v.push(command(mtm, None, unsafe { UIKeyInputEscape }, UIKeyModifierFlags::empty(), sel!(snorkleRecord:), RECORD_CANCEL));
            v
        } else {
            (0..SHORTCUTS.len()).map(|i| key_command(mtm, i)).collect()
        };
        let arr = NSArray::from_retained_slice(&list);
        *cache.borrow_mut() = Some(arr.clone());
        arr
    })
}

/// The shortcut a command stands for (its property list is the id).
pub fn shortcut_of(command: &UICommand) -> Option<Shortcut> {
    let plist = command.propertyList()?;
    let id = plist.downcast_ref::<NSString>()?.to_string();
    shortcut_index(&id).map(|i| SHORTCUTS[i].action)
}

fn with_events(f: impl FnOnce(&Rc<dyn ContentEvents>)) {
    EVENTS.with(|e| {
        if let Some(events) = e.borrow().as_ref() {
            f(events);
        }
    });
}

unsafe extern "C-unwind" fn key_commands(_this: *mut AnyObject, _cmd: Sel) -> *mut AnyObject {
    let Some(mtm) = MainThreadMarker::new() else { return std::ptr::null_mut() };
    // +0: the cache keeps the array alive.
    Retained::as_ptr(&commands(mtm)) as *mut AnyObject
}

unsafe extern "C-unwind" fn perform(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let Some(sender) = (unsafe { sender.as_ref() }) else { return };
    let Some(command) = sender.downcast_ref::<UICommand>() else { return };
    if suspended() {
        return;
    }
    let Some(s) = shortcut_of(command) else { return };
    with_events(|e| e.shortcut(s));
}

unsafe extern "C-unwind" fn record(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let Some(sender) = (unsafe { sender.as_ref() }) else { return };
    let Some(command) = sender.downcast_ref::<UICommand>() else { return };
    let Some(plist) = command.propertyList() else { return };
    let Some(text) = plist.downcast_ref::<NSString>().map(|s| s.to_string()) else { return };
    if !suspended() {
        return;
    }
    let parts: Vec<&str> = text.split('\t').collect();
    let recorded = match parts.as_slice() {
        ["record", key, mods] => mods.parse::<u8>().ok().map(|m| (key.to_string(), m)),
        _ => None,
    };
    with_events(|e| e.key_recorded(recorded));
}

unsafe extern "C-unwind" fn build_menu(_this: *mut AnyObject, _cmd: Sel, builder: *mut AnyObject) {
    let Some(builder) = (unsafe { (builder as *const ProtocolObject<dyn UIMenuBuilder>).as_ref() }) else { return };
    // An Objective-C exception escaping here would terminate the app.
    let done = objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
        if builder.menuForIdentifier(UIMenuClose).is_some() {
            builder.removeMenuForIdentifier(UIMenuClose);
        }
        if builder.menuForIdentifier(UIMenuFind).is_some() {
            builder.removeMenuForIdentifier(UIMenuFind);
        }
    }));
    let note = match done {
        Ok(()) => "menu: system Close and Find removed (⌘W, ⌘F are browser shortcuts)".to_string(),
        Err(e) => format!("menu: could not edit the menu bar: {}", e.map(|e| format!("{e:?}")).unwrap_or_default()),
    };
    with_events(|e| e.note(&note));
}

/// Installs the shortcuts on the app delegate's class. Main thread.
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
        const AT_AT: &CStr = c"@@:";
        // SAFETY: each function matches the type encoding it is added with.
        let build: Imp =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(build_menu);
        let action: Imp =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(perform);
        let rec: Imp = std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(record);
        let keys: Imp = std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject, Imp>(key_commands);
        let added_action = class_addMethod(cls, sel!(snorkleShortcut:), action, V_AT_AT.as_ptr()).as_bool();
        let added_rec = class_addMethod(cls, sel!(snorkleRecord:), rec, V_AT_AT.as_ptr()).as_bool();
        let added_keys = class_addMethod(cls, sel!(keyCommands), keys, AT_AT.as_ptr()).as_bool();
        INSTALLED.with(|i| i.set(added_action && added_rec && added_keys));
        let note = if installed() {
            "keys: shortcuts offered by the app delegate (work whatever has focus)"
        } else {
            "keys: app delegate already has key commands; shortcuts only while a page has focus"
        };
        with_events(|e| e.note(note));
        if class_addMethod(cls, sel!(buildMenuWithBuilder:), build, V_AT_AT.as_ptr()).as_bool() {
            UIMenuSystem::mainSystem(mtm).setNeedsRebuild();
        }
    }
}
