//! SECURITY-REVIEW: the in-app console. Owner request 2026-10-30 (see
//! docs/SECURITY_REVIEW.md, channel C13).
//!
//! Holds, in memory only, what the browser logs (navigation decisions,
//! effects, notices) and, while capture is on, what pages write to their
//! JavaScript console and their uncaught errors.
//!
//! Security assumptions / design:
//! - Page messages are page-controlled text. They are only stored and shown
//!   (the chrome renders them with `textContent`); nothing acts on them.
//!   Each line is sanitised (`sanitize_page_text`: bidi, control and
//!   invisible characters removed), messages are capped in size and lines,
//!   and each tab may send at most [`PAGE_BURST`] messages per
//!   [`PAGE_WINDOW_MS`]; the rest are counted and dropped.
//! - Attribution (tab, frame origin) comes from WebKit, not from the page.
//! - Entries about private tabs are marked and dropped when the private
//!   session ends, like the rest of its in-memory state.
//! - Nothing here is persisted.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

use crate::tabs::TabId;
use crate::text::sanitize_page_text;

/// Entries kept; older ones are dropped first.
pub const MAX_ENTRIES: usize = 1000;
/// Characters kept per message.
pub const MAX_MESSAGE_CHARS: usize = 4000;
/// Lines kept per message (stack traces).
pub const MAX_MESSAGE_LINES: usize = 40;
/// Page messages accepted per tab per window.
pub const PAGE_BURST: u32 = 100;
pub const PAGE_WINDOW_MS: u64 = 10_000;

/// Name of the `window.webkit.messageHandlers` entry, page world only.
pub const PAGE_HANDLER_NAME: &str = "snorkleConsole";

/// Installed at document start in the page world of every frame while
/// capture is on. Forwards `console.*` calls and uncaught errors as one
/// string: a level letter (d, l, i, w, e) followed by the text. Constant
/// code; it takes no input from the app.
pub const PAGE_HOOK_JS: &str = r#"(function(){
var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.snorkleConsole;
if(!h||window.__snorkleConsole)return;
Object.defineProperty(window,'__snorkleConsole',{value:true});
var post=h.postMessage.bind(h);
function fmt(a){try{if(a instanceof Error)return a.name+': '+a.message+(a.stack?'\n'+a.stack:'');if(typeof a==='string')return a;var j=JSON.stringify(a);return j===undefined?String(a):j;}catch(e){try{return String(a);}catch(e2){return '[unprintable]';}}}
function send(l,args){try{post(l+Array.prototype.map.call(args,fmt).join(' ').slice(0,4000));}catch(e){}}
['debug','log','info','warn','error'].forEach(function(n){var o=console[n];console[n]=function(){send(n==='debug'?'d':n.charAt(0),arguments);return o.apply(console,arguments);};});
window.addEventListener('error',function(e){send('e',['Uncaught '+(e.error?fmt(e.error):e.message)+(e.filename?' ('+e.filename+':'+e.lineno+')':'')]);});
window.addEventListener('unhandledrejection',function(e){send('e',['Unhandled rejection: '+fmt(e.reason)]);});
})();"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The browser itself.
    App,
    /// A page's JavaScript console.
    Page,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub seq: u64,
    /// Milliseconds since app start.
    pub time_ms: u64,
    pub origin: Origin,
    pub level: Level,
    pub tab: Option<TabId>,
    /// Host of the frame that logged (page entries), from WebKit.
    pub host: String,
    pub main_frame: bool,
    pub text: String,
    #[serde(skip)]
    pub private: bool,
}

#[derive(Default)]
pub struct ConsoleLog {
    entries: VecDeque<Entry>,
    next_seq: u64,
    /// Per tab: window start, messages accepted, messages dropped.
    rate: HashMap<TabId, (u64, u32, u32)>,
}

impl ConsoleLog {
    pub fn app(&mut self, now: u64, level: Level, tab: Option<TabId>, private: bool, text: &str) -> &Entry {
        self.push(Entry {
            seq: 0,
            time_ms: now,
            origin: Origin::App,
            level,
            tab,
            host: String::new(),
            main_frame: true,
            text: clean(text),
            private,
        })
    }

    /// Records a message posted by the page hook. Returns the stored entry,
    /// or `None` if the message was malformed or rate-limited.
    pub fn page(
        &mut self,
        now: u64,
        tab: Option<TabId>,
        private: bool,
        host: &str,
        main_frame: bool,
        body: &str,
    ) -> Option<&Entry> {
        let mut chars = body.chars();
        let level = match chars.next()? {
            'd' => Level::Debug,
            'l' => Level::Log,
            'i' => Level::Info,
            'w' => Level::Warn,
            'e' => Level::Error,
            _ => return None,
        };
        let mut note = None;
        if let Some(t) = tab {
            let (start, accepted, dropped) = self.rate.entry(t).or_insert((now, 0, 0));
            if now.saturating_sub(*start) >= PAGE_WINDOW_MS {
                if *dropped > 0 {
                    note = Some(format!("{dropped} more messages from this tab were dropped"));
                }
                *start = now;
                *accepted = 0;
                *dropped = 0;
            }
            if *accepted >= PAGE_BURST {
                *dropped += 1;
                return None;
            }
            *accepted += 1;
        }
        if let Some(n) = note {
            self.app(now, Level::Warn, tab, private, &n);
        }
        Some(self.push(Entry {
            seq: 0,
            time_ms: now,
            origin: Origin::Page,
            level,
            tab,
            host: sanitize_page_text(host, 253),
            main_frame,
            text: clean(chars.as_str()),
            private,
        }))
    }

    fn push(&mut self, mut e: Entry) -> &Entry {
        e.seq = self.next_seq;
        self.next_seq += 1;
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
        self.entries.back().expect("just pushed")
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.entries.iter().cloned().collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.rate.clear();
    }

    /// The private session ended: forget everything about it.
    pub fn drop_private(&mut self) {
        self.entries.retain(|e| !e.private);
    }
}

/// Sanitises each line, keeping line breaks, within the size limits.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut budget = MAX_MESSAGE_CHARS;
    for (i, line) in text.lines().enumerate() {
        if i == MAX_MESSAGE_LINES || budget == 0 {
            out.push_str("\n…");
            break;
        }
        let l = sanitize_page_text(line, budget);
        budget = budget.saturating_sub(l.chars().count());
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_messages_are_parsed_and_sanitised() {
        let mut c = ConsoleLog::default();
        let e = c
            .page(0, Some(TabId(1)), false, "accounts.google.com", false, "wbad \u{202E}thing\nat f (x.js:1)")
            .unwrap();
        assert_eq!(e.level, Level::Warn);
        assert_eq!(e.text, "bad thing\nat f (x.js:1)");
        assert_eq!(e.origin, Origin::Page);
        assert!(!e.main_frame);
        assert!(c.page(0, Some(TabId(1)), false, "a", true, "xnope").is_none(), "unknown level");
        assert!(c.page(0, Some(TabId(1)), false, "a", true, "").is_none());
    }

    #[test]
    fn size_and_line_limits() {
        let mut c = ConsoleLog::default();
        let long = "l".to_string() + &"x".repeat(10_000);
        assert!(c.page(0, None, false, "a", true, &long).unwrap().text.chars().count() <= MAX_MESSAGE_CHARS + 1);
        let many = "e".to_string() + &"line\n".repeat(100);
        assert!(c.page(0, None, false, "a", true, &many).unwrap().text.lines().count() <= MAX_MESSAGE_LINES + 1);
        for i in 0..MAX_ENTRIES + 10 {
            c.app(i as u64, Level::Log, None, false, "x");
        }
        assert_eq!(c.entries().len(), MAX_ENTRIES);
    }

    #[test]
    fn pages_are_rate_limited_per_tab() {
        let mut c = ConsoleLog::default();
        let t = Some(TabId(7));
        let accepted = (0..PAGE_BURST + 50).filter(|_| c.page(0, t, false, "a", true, "lspam").is_some()).count();
        assert_eq!(accepted, PAGE_BURST as usize);
        assert!(c.page(0, Some(TabId(8)), false, "b", true, "lother tab").is_some());
        // The next window reports what was dropped.
        assert!(c.page(PAGE_WINDOW_MS, t, false, "a", true, "lagain").is_some());
        assert!(c.entries().iter().any(|e| e.text.contains("50 more messages")));
    }

    #[test]
    fn private_entries_are_dropped_with_the_private_session() {
        let mut c = ConsoleLog::default();
        c.app(0, Level::Log, Some(TabId(1)), true, "secret.example");
        c.page(0, Some(TabId(1)), true, "secret.example", true, "lhi");
        c.app(0, Level::Log, Some(TabId(2)), false, "public");
        c.drop_private();
        assert_eq!(c.entries().len(), 1);
        assert_eq!(c.entries()[0].text, "public");
    }

    #[test]
    fn hook_posts_to_the_named_handler_only() {
        assert!(PAGE_HOOK_JS.contains(&format!("messageHandlers.{PAGE_HANDLER_NAME}")));
        assert!(!PAGE_HOOK_JS.contains("eval("));
    }
}
