//! Timings in Menu → Console (2026-11-23), to compare Snorkle with other
//! browsers on the same Mac (docs/PERFORMANCE.md): how long after launch the
//! setup finished, the chrome asked for its first state and the first page
//! committed; for a page the user asked for (an address, a link, a
//! dropped tab coming back), how long until its load started, committed and
//! finished; how long a tab switch took in the app. Logged like the other
//! browser entries (while recording; always on stderr in debug builds).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use snorkle_core::console::Level;
use snorkle_core::tabs::TabId;

use super::Shared;

static LAUNCH: OnceLock<Instant> = OnceLock::new();
static PENDING: Mutex<Option<HashMap<TabId, (Instant, &'static str)>>> = Mutex::new(None);
static SEEN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Marks the launch (first thing in `run`).
pub fn launched() {
    LAUNCH.get_or_init(Instant::now);
}

fn since_launch_ms() -> u128 {
    LAUNCH.get().map_or(0, |t| t.elapsed().as_millis())
}

/// The first time `what` happens after launch, with the time since launch.
pub fn first(shared: &Shared, what: &'static str) {
    {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if seen.contains(&what) {
            return;
        }
        seen.push(what);
    }
    shared.log(Level::Info, None, &format!("timing: {what} {} ms after launch", since_launch_ms()));
}

/// A page the user is waiting for in `tab` (`what`: "load", "restore").
pub fn request(tab: TabId, what: &'static str) {
    let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    p.get_or_insert_with(HashMap::new).insert(tab, (Instant::now(), what));
}

/// A stage of that page's load; `last` ends the measurement.
pub fn stage(shared: &Shared, tab: TabId, stage: &'static str, last: bool) {
    let started = {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(map) = p.as_mut() else { return };
        if last { map.remove(&tab) } else { map.get(&tab).copied() }
    };
    if let Some((t, what)) = started {
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        shared.log(Level::Info, Some(tab), &format!("timing: {what} in tab {}: {stage} after {ms:.0} ms", tab.0));
    }
}

/// How long a step in the app took (`since`: when it began).
pub fn took(shared: &Shared, tab: TabId, what: &str, since: Instant) {
    let ms = since.elapsed().as_secs_f64() * 1000.0;
    shared.log(Level::Info, Some(tab), &format!("timing: {what} tab {} took {ms:.1} ms in the app", tab.0));
}
