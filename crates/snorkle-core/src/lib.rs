//! snorkle-core: the platform-independent half of the browser.
//!
//! Everything in this crate is pure Rust with no platform or UI dependencies
//! so it can be unit-tested and fuzzed on any host. The iPadOS glue
//! (`snorkle-content`, `src-tauri`) feeds platform events in and executes the
//! [`browser::Effect`]s that come out.
//!
//! Modules marked `SECURITY-REVIEW` sit on a trust boundary (URL handling,
//! navigation policy, storage) and must be reviewed by a human on every
//! change. See `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

pub mod ai;
pub mod blocking;
pub mod browser;
pub mod console;
pub mod idn;
pub mod input;
pub mod os;
pub mod policy;
pub mod storage;
pub mod tabs;
pub mod text;
pub mod url_display;
pub mod userscripts;
pub mod viewmodes;

pub use url::Url;
