//! SECURITY-REVIEW: installs content-blocking rule lists into every content
//! web view.
//!
//! Lists are stored in WebKit's `WKContentRuleListStore` under identifiers
//! `snorkle.cb.<key>.<i>.of.<n>`, where `<key>` is the content hash from
//! `snorkle_core::blocking::cache_key`. On install:
//! 1. if the store already has a complete set for the key, the compiled
//!    lists are looked up (no conversion, no compile);
//! 2. otherwise the Rust conversion runs on a background thread and WebKit
//!    compiles the JSON (WebKit does this off the main thread);
//! 3. lists for any other key are removed from the store.
//!
//! An install for a newer key supersedes an older one; late completions of
//! the older install are ignored.
//!
//! Security assumptions: rule lists only block/hide/upgrade; they cannot
//! run code. All WebKit completion handlers here are invoked on the main
//! thread (checked at runtime; see docs/OPEN_QUESTIONS.md).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKUserContentController};
use snorkle_core::tabs::TabId;

use crate::{MakeRuleLists as MakeLists, RulesDone as Done};

const PREFIX: &str = "snorkle.cb.";

#[derive(Default)]
struct State {
    key: Option<String>,
    active: Vec<Retained<WKContentRuleList>>,
    controllers: HashMap<TabId, Retained<WKUserContentController>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn list_id(key: &str, i: usize, n: usize) -> String {
    format!("{PREFIX}{key}.{i}.of.{n}")
}

fn is_current(key: &str) -> bool {
    STATE.with(|s| s.borrow().key.as_deref() == Some(key))
}

/// Attaches the active lists to a new web view's controller.
pub fn register(tab: TabId, controller: Retained<WKUserContentController>) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for l in &s.active {
            unsafe { controller.addContentRuleList(l) };
        }
        s.controllers.insert(tab, controller);
    });
}

pub fn unregister(tab: TabId) {
    STATE.with(|s| {
        s.borrow_mut().controllers.remove(&tab);
    });
}

fn activate(key: &str, lists: Vec<Retained<WKContentRuleList>>) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.key.as_deref() != Some(key) {
            return;
        }
        for c in s.controllers.values() {
            unsafe {
                c.removeAllContentRuleLists();
                for l in &lists {
                    c.addContentRuleList(l);
                }
            }
        }
        s.active = lists;
    });
}

pub fn install(mtm: MainThreadMarker, key: String, make: MakeLists, done: Done) {
    STATE.with(|s| s.borrow_mut().key = Some(key.clone()));
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
        done(Err("content rule list store unavailable".into()));
        return;
    };
    let pending = Cell::new(Some((make, done)));
    let store2 = store.clone();
    let block = RcBlock::new(move |ids: *mut NSArray<NSString>| {
        let Some((make, done)) = pending.take() else { return };
        let ids: Vec<String> =
            unsafe { ids.as_ref() }.map(|a| a.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        remove_stale(&store2, &ids, &key);
        match cached_count(&ids, &key) {
            Some(n) => look_up_all(&store2, key.clone(), n, make, done),
            None => build(key.clone(), make, done),
        }
    });
    unsafe { store.getAvailableContentRuleListIdentifiers(Some(&block)) };
}

/// `n` if the store holds `0..n` lists for this key.
fn cached_count(ids: &[String], key: &str) -> Option<usize> {
    let own = format!("{PREFIX}{key}.");
    let n = ids.iter().filter_map(|id| id.strip_prefix(&own)?.split(".of.").nth(1)?.parse::<usize>().ok()).max()?;
    (n > 0 && (0..n).all(|i| ids.contains(&list_id(key, i, n)))).then_some(n)
}

fn remove_stale(store: &WKContentRuleListStore, ids: &[String], key: &str) {
    let own = format!("{PREFIX}{key}.");
    for id in ids.iter().filter(|id| id.starts_with(PREFIX) && !id.starts_with(&own)) {
        let noop = RcBlock::new(|_err: *mut NSError| {});
        unsafe {
            store.removeContentRuleListForIdentifier_completionHandler(Some(&NSString::from_str(id)), Some(&noop))
        };
    }
}

struct Acc {
    lists: Vec<Option<Retained<WKContentRuleList>>>,
    remaining: usize,
    finished: bool,
    fallback: Option<(Option<MakeLists>, Done)>,
}

fn look_up_all(store: &WKContentRuleListStore, key: String, n: usize, make: MakeLists, done: Done) {
    let acc = Rc::new(RefCell::new(Acc { lists: vec![None; n], remaining: n, finished: false, fallback: Some((Some(make), done)) }));
    for i in 0..n {
        let acc = acc.clone();
        let k = key.clone();
        let block = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
            assert!(MainThreadMarker::new().is_some(), "WebKit completion off main thread");
            let mut a = acc.borrow_mut();
            if a.finished {
                return;
            }
            match unsafe { Retained::retain(list) }.filter(|_| err.is_null()) {
                Some(l) => {
                    a.lists[i] = Some(l);
                    a.remaining -= 1;
                    if a.remaining == 0 {
                        a.finished = true;
                        let lists = a.lists.drain(..).flatten().collect();
                        let (_, done) = a.fallback.take().expect("fallback present");
                        drop(a);
                        activate(&k, lists);
                        done(Ok(n));
                    }
                }
                None => {
                    // Cache entry unusable: convert and compile from scratch.
                    a.finished = true;
                    let (make, done) = a.fallback.take().expect("fallback present");
                    drop(a);
                    if let Some(make) = make {
                        build(k.clone(), make, done);
                    }
                }
            }
        });
        unsafe {
            store.lookUpContentRuleListForIdentifier_completionHandler(
                Some(&NSString::from_str(&list_id(&key, i, n))),
                Some(&block),
            )
        };
    }
}

fn build(key: String, make: MakeLists, done: Done) {
    std::thread::Builder::new()
        .name("snorkle-adblock".into())
        .spawn(move || {
            let lists = make();
            super::dispatch_main(move || compile_all(key, lists, done));
        })
        .expect("spawn adblock thread");
}

fn compile_all(key: String, lists: Result<Vec<String>, String>, done: Done) {
    if !is_current(&key) {
        return; // superseded by a newer install
    }
    let lists = match lists {
        Ok(l) => l,
        Err(e) => return done(Err(e)),
    };
    let n = lists.len();
    if n == 0 {
        activate(&key, Vec::new());
        return done(Ok(0));
    }
    let mtm = MainThreadMarker::new().expect("compile_all runs on the main thread");
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
        return done(Err("content rule list store unavailable".into()));
    };
    let acc = Rc::new(RefCell::new(Acc { lists: vec![None; n], remaining: n, finished: false, fallback: Some((None, done)) }));
    for (i, json) in lists.into_iter().enumerate() {
        let acc = acc.clone();
        let k = key.clone();
        let block = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
            assert!(MainThreadMarker::new().is_some(), "WebKit completion off main thread");
            let mut a = acc.borrow_mut();
            if a.finished {
                return;
            }
            match unsafe { Retained::retain(list) }.filter(|_| err.is_null()) {
                Some(l) => {
                    a.lists[i] = Some(l);
                    a.remaining -= 1;
                    if a.remaining == 0 {
                        a.finished = true;
                        let lists = a.lists.drain(..).flatten().collect();
                        let (_, done) = a.fallback.take().expect("done present");
                        drop(a);
                        activate(&k, lists);
                        done(Ok(n));
                    }
                }
                None => {
                    a.finished = true;
                    let (_, done) = a.fallback.take().expect("done present");
                    drop(a);
                    let msg = unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "rule list compile failed".into());
                    done(Err(msg));
                }
            }
        });
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&NSString::from_str(&list_id(&key, i, n))),
                Some(&NSString::from_str(&json)),
                Some(&block),
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_detection() {
        let ids: Vec<String> = ["snorkle.cb.k.0.of.2", "snorkle.cb.k.1.of.2", "snorkle.cb.old.0.of.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(cached_count(&ids, "k"), Some(2));
        assert_eq!(cached_count(&ids[..1], "k"), None);
        assert_eq!(cached_count(&ids, "other"), None);
    }
}
