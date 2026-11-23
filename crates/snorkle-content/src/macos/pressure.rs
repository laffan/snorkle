//! Memory pressure on macOS (2026-11-23). iPadOS tells apps through
//! `UIApplicationDidReceiveMemoryWarningNotification`; AppKit has no such
//! notification, so until now a Mac never discarded pages for memory, only
//! by count and age (OPEN_QUESTIONS 84). This listens to libdispatch's
//! memory-pressure source (the system's own signal, as Safari's) and passes
//! a warning or a critical level on as a memory warning: the core then
//! keeps only the pages on screen live. With that in place the Mac can keep
//! many more pages live and for longer (`MemoryBudget::for_mac`).
//!
//! libdispatch is part of libSystem; its C functions are declared here (no
//! binding crate in the graph). The handler only reports that pressure
//! happened: it carries nothing from any page.

use std::ffi::c_void;
use std::rc::Rc;

use block2::{Block, RcBlock};

use crate::ContentEvents;

/// `DISPATCH_MEMORYPRESSURE_WARN` and `DISPATCH_MEMORYPRESSURE_CRITICAL`.
const WARN: usize = 0x02;
const CRITICAL: usize = 0x04;

extern "C" {
    /// `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` is `&_dispatch_source_type_memorypressure`.
    static _dispatch_source_type_memorypressure: c_void;
    /// `dispatch_get_main_queue()` is `&_dispatch_main_q`.
    static _dispatch_main_q: c_void;
    fn dispatch_source_create(kind: *const c_void, handle: usize, mask: usize, queue: *const c_void) -> *mut c_void;
    fn dispatch_source_set_event_handler(source: *mut c_void, handler: &Block<dyn Fn()>);
    fn dispatch_source_get_data(source: *mut c_void) -> usize;
    fn dispatch_source_cancel(source: *mut c_void);
    fn dispatch_resume(object: *mut c_void);
    fn dispatch_release(object: *mut c_void);
}

/// The memory-pressure source; cancelled and released when dropped.
pub struct Pressure(*mut c_void);

impl Drop for Pressure {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                dispatch_source_cancel(self.0);
                dispatch_release(self.0);
            }
        }
    }
}

/// Starts listening; the handler runs on the main queue.
pub fn watch(events: Rc<dyn ContentEvents>) -> Pressure {
    unsafe {
        let kind = std::ptr::addr_of!(_dispatch_source_type_memorypressure);
        let main = std::ptr::addr_of!(_dispatch_main_q);
        let source = dispatch_source_create(kind, 0, WARN | CRITICAL, main);
        if source.is_null() {
            events.note("memory pressure: no dispatch source; pages are discarded by count and age only");
            return Pressure(source);
        }
        let src = source as usize;
        let handler = RcBlock::new(move || {
            let level = dispatch_source_get_data(src as *mut c_void);
            if level & (WARN | CRITICAL) != 0 {
                events.note(if level & CRITICAL != 0 { "memory pressure: critical" } else { "memory pressure: warning" });
                events.memory_warning();
            }
        });
        // libdispatch copies the block.
        dispatch_source_set_event_handler(source, &handler);
        dispatch_resume(source);
        Pressure(source)
    }
}
