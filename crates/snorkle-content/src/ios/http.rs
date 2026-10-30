//! SECURITY-REVIEW: the app's own HTTPS requests to AI providers (owner
//! request 2026-10-30). The only other app-initiated requests are favicon
//! fetches (`favicon.rs`).
//!
//! Security assumptions:
//! - Only URLs accepted by `snorkle_core::ai::is_allowed_url` (the two API
//!   hosts, https) are sent; anything else fails without a request.
//! - An ephemeral `NSURLSession`: no cookies, cache or credentials are kept
//!   or shared with web content; system TLS validation applies.
//! - The reply is handed back as bytes, capped in size; parsing happens in
//!   `snorkle_core::ai`.

use std::sync::Mutex;

use block2::RcBlock;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURLResponse, NSURLSession,
    NSURLSessionConfiguration, NSURL,
};
use snorkle_core::ai::{is_allowed_url, HttpRequest, MAX_REPLY_BYTES};

use crate::HttpDone;

pub fn send(req: HttpRequest, done: HttpDone) {
    if !is_allowed_url(&req.url) {
        done(Err("Blocked: not an AI provider address.".into()));
        return;
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(&req.url)) else {
        done(Err("Invalid address.".into()));
        return;
    };
    let request = NSMutableURLRequest::requestWithURL(&url);
    request.setHTTPMethod(&NSString::from_str(req.method));
    for (name, value) in &req.headers {
        request.setValue_forHTTPHeaderField(Some(&NSString::from_str(value)), &NSString::from_str(name));
    }
    if let Some(body) = &req.body {
        request.setHTTPBody(Some(&NSData::with_bytes(body)));
    }
    // Model replies can take minutes.
    request.setTimeoutInterval(300.0);

    let session = NSURLSession::sessionWithConfiguration(&NSURLSessionConfiguration::ephemeralSessionConfiguration());
    let done = Mutex::new(Some(done));
    let block = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
        let Some(done) = done.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        // SAFETY: WebKit/Foundation pass valid (or null) object pointers
        // that live for the duration of the callback.
        let (data, response, error) = unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };
        if let Some(err) = error {
            done(Err(snorkle_core::text::sanitize_page_text(&err.localizedDescription().to_string(), 200)));
            return;
        }
        let status = response
            .and_then(|r| r.downcast_ref::<NSHTTPURLResponse>())
            .map(|r| u16::try_from(r.statusCode()).unwrap_or(0))
            .unwrap_or(0);
        let bytes = data.map(|d| d.to_vec()).unwrap_or_default();
        if bytes.len() > MAX_REPLY_BYTES {
            done(Err("The reply was too large.".into()));
        } else {
            done(Ok((status, bytes)));
        }
    });
    // SAFETY: the block only touches a Mutex and a Send callback.
    let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &block) };
    task.resume();
    session.finishTasksAndInvalidate();
}
