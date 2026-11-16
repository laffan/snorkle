//! SECURITY-REVIEW: text in the Notebook's clips (owner request 2026-11-16),
//! so search finds words shown in a picture: Apple's Vision text
//! recognition (`VNRecognizeTextRequest`), on the device, over a JPEG the
//! app drew itself from its own web view (never page image bytes). No
//! network. The text is only stored with the clip (not shown) and cleaned
//! and capped by the caller.
//!
//! Vision has no binding in the dependency graph, so its classes are found
//! by name and called through `msg_send!`; an Objective-C exception is
//! caught and treated as "no text". Selectors as documented in
//! `Vision/VNRequest.h`, `VNRecognizeTextRequest.h`,
//! `VNRequestHandler.h` and `VNObservation.h` (iOS 13, macOS 10.15).

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString};

#[link(name = "Vision", kind = "framework")]
extern "C" {}

/// `VNRequestTextRecognitionLevelAccurate`.
const LEVEL_ACCURATE: isize = 0;
/// Largest picture read, in bytes.
const MAX_JPEG: usize = 4 * 1024 * 1024;

/// The lines of text Vision reads in `jpeg`, top to bottom, at most
/// `max_chars`; `None` when there are none or recognition is unavailable.
/// Synchronous (Vision's `performRequests:error:`): call it off the main
/// thread.
pub fn recognize_text(jpeg: &[u8], max_chars: usize) -> Option<String> {
    if jpeg.is_empty() || jpeg.len() > MAX_JPEG {
        return None;
    }
    let request_class = AnyClass::get(c"VNRecognizeTextRequest")?;
    let handler_class = AnyClass::get(c"VNImageRequestHandler")?;
    let lines = objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
        let request: Retained<AnyObject> = msg_send![request_class, new];
        let _: () = msg_send![&*request, setRecognitionLevel: LEVEL_ACCURATE];
        let _: () = msg_send![&*request, setUsesLanguageCorrection: Bool::YES];
        let data = NSData::with_bytes(jpeg);
        let options = NSDictionary::<NSString, AnyObject>::new();
        let handler: Option<Retained<AnyObject>> =
            msg_send![msg_send![handler_class, alloc], initWithData: &*data, options: &*options];
        let handler = handler?;
        let requests = NSArray::from_slice(&[&*request]);
        let mut error: *mut NSError = std::ptr::null_mut();
        let ok: Bool = msg_send![&*handler, performRequests: &*requests, error: &mut error];
        if !ok.as_bool() {
            return None;
        }
        let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let mut lines = Vec::new();
        for observation in results?.iter() {
            let candidates: Option<Retained<NSArray<AnyObject>>> = msg_send![&*observation, topCandidates: 1usize];
            let Some(best) = candidates.and_then(|c| c.firstObject()) else { continue };
            let text: Option<Retained<NSString>> = msg_send![&*best, string];
            if let Some(t) = text {
                lines.push(t.to_string());
            }
        }
        Some(lines)
    }))
    .ok()
    .flatten()?;
    let text: String = lines.join("\n").chars().take(max_chars).collect();
    (!text.trim().is_empty()).then_some(text)
}
