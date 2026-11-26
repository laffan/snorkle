//! Keyboard shortcuts and pointer events on macOS.
//!
//! Shortcuts (`crate::SHORTCUTS`, with the user's bindings) are matched in
//! a local key-down monitor on the application, which sees every key event
//! of the app before any view or the menu bar does, so they work whatever
//! has focus: a page, the chrome, or nothing. A matched event is consumed;
//! anything else (⌘C, ⌘V, typing) goes on as usual. Quit (⌘Q, a shortcut
//! since 2026-11-23) is the one done here rather than by the chrome. This is the macOS
//! counterpart of the app delegate's key commands on iPadOS (`ios/menu.rs`).
//! Recording a new binding (Settings → Keyboard) is done here too: while
//! recording, the next accepted ⌘ combination, or Escape, is reported and
//! consumed.
//!
//! A local mouse monitor gives the two pointer signals the iPad gets from
//! gesture recognizers: a click on a page focuses its pane (split view),
//! and the pointer's position near the window's left edge is reported for
//! the sidebar's edge tab (pages are native views, so the chrome does not
//! see the pointer over them).
//!
//! The same mouse monitor shows the window's close, minimise and zoom
//! buttons only while the pointer is in the window's top-left corner
//! (owner request 2026-11-23; before, they sat in a 40 pt strip at the
//! top of the docked sidebar): [`BUTTON_CORNER`].
//!
//! A local flags-changed monitor reports ⌥ being pressed or let go
//! (2026-11-16), for the chrome's copy button, which sends the page to the
//! notebook with ⌥.
//!
//! None can be triggered by a page: all come from the hardware via AppKit
//! and carry only which shortcut was pressed, a tab id, a pointer position,
//! or whether ⌥ is down.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSView, NSWindow, NSWindowButton,
};

use super::views::Container;
use crate::{
    is_arrow, yields_to_editing, ContentEvents, KeyBinding, Shortcut, MOD_ALT, MOD_CTRL, MOD_SHIFT, SHORTCUTS,
};

thread_local! {
    /// Effective binding of each `SHORTCUTS` entry (the user's or default).
    static BINDINGS: RefCell<Vec<KeyBinding>> = RefCell::new(
        SHORTCUTS.iter().map(|s| KeyBinding { key: s.key.to_string(), mods: s.mods }).collect()
    );
    /// The editor is recording a key: no shortcut runs.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Keys the editor accepts (as in `crate::validate_binding`).
const RECORD_KEYS: &str = "abcdefghijklmnopqrstuvwxyz0123456789[]\\;',./-=`";
/// `kVK_Escape`.
const ESCAPE: u16 = 53;

/// The arrow keys' names (`crate::ARROW_KEYS`) by virtual key code
/// (`kVK_LeftArrow` 123, `kVK_RightArrow` 124, `kVK_DownArrow` 125,
/// `kVK_UpArrow` 126), which do not depend on the keyboard layout.
fn arrow_name(code: u16) -> Option<&'static str> {
    match code {
        123 => Some("left"),
        124 => Some("right"),
        125 => Some("down"),
        126 => Some("up"),
        _ => None,
    }
}

/// New bindings (validated, one per `SHORTCUTS` entry) and whether the
/// editor is recording a key.
pub fn set_bindings(bindings: Vec<KeyBinding>, record: bool) {
    if bindings.len() == SHORTCUTS.len() {
        BINDINGS.with(|b| *b.borrow_mut() = bindings);
    }
    RECORDING.with(|r| r.set(record));
}

/// The event monitors; removed when dropped.
pub struct Monitors(Vec<Retained<AnyObject>>);

impl Drop for Monitors {
    fn drop(&mut self) {
        for m in &self.0 {
            unsafe { NSEvent::removeMonitor(m) };
        }
    }
}

/// The key and `MOD_*` bits of a key-down with ⌘ held, or `None`. The key
/// is what the key produces without modifiers on the current layout
/// (`charactersByApplyingModifiers:`), so ⌘⇧\ reads as `\` with Shift, as
/// UIKit reports it on the iPad.
fn command_key(event: &NSEvent) -> Option<(String, u8)> {
    let flags = event.modifierFlags();
    if !flags.contains(NSEventModifierFlags::Command) {
        return None;
    }
    let mut mods = 0;
    if flags.contains(NSEventModifierFlags::Shift) {
        mods |= MOD_SHIFT;
    }
    if flags.contains(NSEventModifierFlags::Option) {
        mods |= MOD_ALT;
    }
    if flags.contains(NSEventModifierFlags::Control) {
        mods |= MOD_CTRL;
    }
    if let Some(arrow) = arrow_name(event.keyCode()) {
        return Some((arrow.to_string(), mods));
    }
    let key = event.charactersByApplyingModifiers(NSEventModifierFlags::empty())?.to_string().to_ascii_lowercase();
    (key.chars().count() == 1).then_some((key, mods))
}

/// What a key-down in the browser window does. `true`: consumed.
fn key_down(event: &NSEvent, events: &Rc<dyn ContentEvents>) -> bool {
    if RECORDING.with(Cell::get) {
        if event.keyCode() == ESCAPE && !event.modifierFlags().contains(NSEventModifierFlags::Command) {
            events.key_recorded(None);
            return true;
        }
        return match command_key(event) {
            Some((key, mods)) if is_arrow(&key) || (key.chars().count() == 1 && RECORD_KEYS.contains(key.as_str())) => {
                events.key_recorded(Some((key, mods)));
                true
            }
            _ => false,
        };
    }
    let Some((key, mods)) = command_key(event) else { return false };
    // An arrow binding while text is being edited is left to the text
    // (⌘← to the line's start, and so on): not consumed.
    let hit = BINDINGS.with(|b| b.borrow().iter().position(|k| k.key == key && k.mods == mods && !yields_to_editing(k)));
    let Some(i) = hit else { return false };
    // A held key repeats; a shortcut runs once per press.
    if !event.isARepeat() {
        match SHORTCUTS[i].action {
            // Quit (⌘Q by default): taken here, before a page could swallow
            // the key, and done as the menu's Quit does it (`terminate:`,
            // so the session is written on the way out, `RunEvent::Exit`).
            // After this handler returns, not from inside it.
            Shortcut::Quit => crate::dispatch_main(|| {
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).terminate(None);
                }
            }),
            action => events.shortcut(action),
        }
    }
    true
}

/// The window's top-left corner (width, height in points from the top
/// left) in which its close, minimise and zoom buttons show; elsewhere they
/// are hidden. Covers the buttons themselves (`trafficLightPosition` 16, 20
/// in tauri.conf.json) with room around them.
pub const BUTTON_CORNER: (f64, f64) = (96.0, 44.0);

fn in_button_corner(x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite() && (0.0..=BUTTON_CORNER.0).contains(&x) && (0.0..=BUTTON_CORNER.1).contains(&y)
}

/// Shows or hides the window's close, minimise and zoom buttons.
pub fn set_window_buttons(window: &NSWindow, shown: bool) {
    for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = window.standardWindowButton(b) {
            button.setHidden(!shown);
        }
    }
}

pub struct Edge {
    near: Cell<bool>,
    last: Cell<Option<std::time::Instant>>,
}

/// Reports whether the pointer is over the chrome (see
/// `ContentEvents::chrome_pointer`), only when that changes.
fn chrome_pointer(state: &Cell<Option<bool>>, events: &Rc<dyn ContentEvents>, over: bool) {
    if state.replace(Some(over)) != Some(over) {
        events.chrome_pointer(over);
    }
}

/// Reports the pointer near the window's left edge, at most every 33 ms,
/// and once when it leaves the strip (`None`).
fn pointer_at(edge: &Edge, events: &Rc<dyn ContentEvents>, at: Option<(f64, f64)>) {
    match at {
        Some((x, y)) if x.is_finite() && y.is_finite() && (0.0..=crate::EDGE_ZONE).contains(&x) => {
            let now = std::time::Instant::now();
            if edge.near.get() && edge.last.get().is_some_and(|t| now.duration_since(t).as_millis() < 33) {
                return;
            }
            edge.near.set(true);
            edge.last.set(Some(now));
            events.edge_pointer(Some((x, y)));
        }
        _ => {
            if edge.near.replace(false) {
                events.edge_pointer(None);
            }
        }
    }
}

/// Installs the key and mouse monitors for `window`, whose content view
/// holds `root` (the content root view). Main thread.
pub fn install(
    mtm: MainThreadMarker,
    window: &NSWindow,
    root: &super::views::ContentRoot,
    events: Rc<dyn ContentEvents>,
) -> Monitors {
    let _ = mtm;
    let mut monitors = Vec::new();
    let win = window as *const NSWindow as usize;
    let in_window = move |e: &NSEvent| {
        let w: *const AnyObject = unsafe { objc2::msg_send![e, window] };
        w as usize == win
    };

    let ev = events.clone();
    let keys = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { event.as_ref() };
        if in_window(e) && key_down(e, &ev) {
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    if let Some(m) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &keys) } {
        monitors.push(m);
    }

    // ⌥ alone pressed or let go (only its changes are reported; the
    // hardware's modifier state, nothing a page can set). ⌥ with ⇧, ⌘ or ⌃
    // is not the notebook's (⌥⇧ arrows select word by word), so it counts
    // as let go (2026-11-25; before, ⌥ with anything counted).
    let ev = events.clone();
    let option = Cell::new(false);
    let flags = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { event.as_ref() };
        if in_window(e) {
            let f = e.modifierFlags();
            let others = NSEventModifierFlags::Shift | NSEventModifierFlags::Command | NSEventModifierFlags::Control;
            let down = f.contains(NSEventModifierFlags::Option) && !f.intersects(others);
            if option.replace(down) != down {
                ev.option_key(down);
            }
        }
        event.as_ptr()
    });
    if let Some(m) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &flags) }
    {
        monitors.push(m);
    }

    let edge = Rc::new(Edge { near: Cell::new(false), last: Cell::new(None) });
    let root_ptr: Retained<NSView> = objc2::Message::retain(&**root);
    let (ev, edge2) = (events.clone(), edge.clone());
    // The window buttons: hidden until the pointer is in the corner.
    set_window_buttons(window, false);
    let buttons: Retained<NSWindow> = objc2::Message::retain(window);
    let corner = Rc::new(Cell::new(false));
    let (buttons2, corner2) = (buttons.clone(), corner.clone());
    // Whether the pointer was over the chrome when last reported.
    let on_chrome: Rc<Cell<Option<bool>>> = Rc::default();
    let on_chrome2 = on_chrome.clone();
    let mouse = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { event.as_ref() };
        if !in_window(e) {
            return event.as_ptr();
        }
        let p = e.locationInWindow();
        let height = root_ptr.bounds().size.height;
        match e.r#type() {
            NSEventType::LeftMouseDown | NSEventType::RightMouseDown => {
                // The view the click lands on (holes and the chrome give
                // no container): focus that pane.
                if let Some(sup) = unsafe { root_ptr.superview() } {
                    let local = sup.convertPoint_fromView(p, None);
                    if let Some(tab) = root_ptr.hitTest(local).as_deref().and_then(Container::tab_of) {
                        ev.focus_requested(tab);
                    }
                }
            }
            _ => {
                let inside = in_button_corner(p.x, height - p.y);
                if corner2.replace(inside) != inside {
                    set_window_buttons(&buttons2, inside);
                }
                pointer_at(&edge2, &ev, Some((p.x, height - p.y)));
                // Over the chrome: in the window, and no page view (the
                // root's subviews, holes left out) under the pointer.
                if let Some(sup) = unsafe { root_ptr.superview() } {
                    let local = sup.convertPoint_fromView(p, None);
                    let f = root_ptr.frame();
                    let in_window = local.x >= f.origin.x
                        && local.y >= f.origin.y
                        && local.x < f.origin.x + f.size.width
                        && local.y < f.origin.y + f.size.height;
                    chrome_pointer(&on_chrome2, &ev, in_window && root_ptr.hitTest(local).is_none());
                }
            }
        }
        event.as_ptr()
    });
    let mask = NSEventMask::LeftMouseDown
        | NSEventMask::RightMouseDown
        | NSEventMask::MouseMoved
        | NSEventMask::LeftMouseDragged;
    if let Some(m) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &mouse) } {
        monitors.push(m);
    }
    // Leaving the window (the root view's tracking area).
    let ev = events.clone();
    *root.ivars().on_exit.borrow_mut() = Some(Box::new(move || {
        if corner.replace(false) {
            set_window_buttons(&buttons, false);
        }
        pointer_at(&edge, &ev, None);
        chrome_pointer(&on_chrome, &ev, false);
    }));
    window.setAcceptsMouseMovedEvents(true);

    events.note("keys: shortcuts handled by a key-event monitor (work whatever has focus)");
    Monitors(monitors)
}
