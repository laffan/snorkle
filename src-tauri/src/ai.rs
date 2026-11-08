//! SECURITY-REVIEW: AI provider commands. Keys go from the chrome straight
//! into the Keychain and are read back only to send a request to the
//! provider's fixed API host (`snorkle_core::ai`); they are never returned
//! to the chrome. Requests and their outcome (never the key) are logged in
//! the in-app console.

use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use snorkle_core::ai::{self, ChatMessage, Model, Provider, ScriptDraft};
use snorkle_core::console::Level;
use snorkle_core::storage::{AiUsageEvent, ChatEntry, ChatRecord};

use crate::state::Shared;

type S<'a> = tauri::State<'a, std::sync::Arc<Shared>>;

/// Sends `req` and waits (off the async runtime's worker) for the reply.
/// Sends `req` and waits (off the async runtime's worker) for the reply. A
/// successful reply's token use is recorded against `model` (Settings →
/// AI → Usage).
async fn send(
    s: &S<'_>,
    p: Provider,
    model: Option<&str>,
    what: &str,
    req: ai::HttpRequest,
) -> Result<(u16, Vec<u8>), String> {
    let (tx, rx) = mpsc::channel();
    snorkle_content::http::send(req, Box::new(move |r| drop(tx.send(r))));
    let result = tauri::async_runtime::spawn_blocking(move || rx.recv().unwrap_or(Err("No reply.".into())))
        .await
        .map_err(|_| "Request failed.".to_string())?;
    let outcome = match &result {
        Ok((status, body)) => format!("{status}, {} bytes", body.len()),
        Err(e) => e.clone(),
    };
    s.log(Level::Info, None, &format!("AI {} {what}: {outcome}", p.label()));
    if let (Ok((200..=299, body)), Some(model), Some(store)) = (&result, model, s.store.as_ref()) {
        if let Some(usage) = ai::parse_usage(p, body) {
            let at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            let cost_micros = ai::estimate_cost_micros(p, model, usage);
            let event = AiUsageEvent {
                at_ms,
                provider: p,
                model: model.to_string(),
                input: usage.input,
                output: usage.output,
                cost_micros,
            };
            let _ = store.record_ai_usage(event);
        }
    }
    result
}

fn key(p: Provider) -> Result<String, String> {
    snorkle_content::keychain::get(p.key_name())
        .ok_or_else(|| format!("Add an {} API key in Settings → AI.", p.label()))
}

#[derive(Serialize)]
pub struct AiStatus {
    /// Provider → whether a key is stored.
    configured: BTreeMap<&'static str, bool>,
}

fn status() -> AiStatus {
    AiStatus {
        configured: Provider::ALL
            .iter()
            .map(|p| (p.key_name(), snorkle_content::keychain::get(p.key_name()).is_some()))
            .collect(),
    }
}

#[tauri::command]
pub fn ai_status() -> AiStatus {
    status()
}

/// Stores a key (validated) in the Keychain.
#[tauri::command]
pub fn ai_set_key(provider: Provider, key: String) -> Result<AiStatus, String> {
    let key = ai::validate_key(&key)?;
    snorkle_content::keychain::set(provider.key_name(), &key)?;
    Ok(status())
}

#[tauri::command]
pub fn ai_delete_key(provider: Provider) -> AiStatus {
    snorkle_content::keychain::delete(provider.key_name());
    status()
}

/// Every recorded AI request (no content: time, provider, model, tokens,
/// estimated cost). The chrome groups them by local date.
#[tauri::command]
pub fn ai_usage(s: S<'_>) -> Vec<AiUsageEvent> {
    s.store.as_ref().map(|st| st.ai_usage()).unwrap_or_default()
}

#[tauri::command]
pub fn ai_reset_usage(s: S<'_>) -> Result<(), String> {
    match s.store.as_ref() {
        Some(st) => st.clear_ai_usage().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Checks the stored key by listing the account's models.
#[tauri::command]
pub async fn ai_test(s: S<'_>, provider: Provider) -> Result<Vec<Model>, String> {
    let req = ai::list_models_request(provider, &key(provider)?);
    let (status, body) = send(&s, provider, None, "list models", req).await?;
    ai::parse_models(provider, status, &body)
}

#[derive(Serialize)]
pub struct DetectedHeadings {
    rules: usize,
    url: String,
    summaries: Vec<ai::HeadingSummary>,
}

/// "Detect Headers": samples the tab's heading-like elements (headers view
/// on), asks the model which are headings on this site, stores the rules
/// for the site and restarts the outline with them. Returns the rule count
/// and page-specific summaries. Sends the sample and bounded page text to
/// the provider only when the user presses the button.
#[tauri::command]
pub async fn detect_headings(s: S<'_>, tab: u64, provider: Provider, model: String) -> Result<DetectedHeadings, String> {
    let tab = snorkle_core::tabs::TabId(tab);
    let site = s
        .browser()
        .view()
        .tabs
        .into_iter()
        .find(|t| t.id == tab)
        .and_then(|t| t.adblock)
        .map(|a| a.site)
        .ok_or("Open a web page first.")?;
    let url = s.browser().tab(tab).and_then(|t| t.committed_url.clone()).ok_or("Open a web page first.")?;
    let key = key(provider)?;
    let (tx, rx) = mpsc::channel();
    s.pending_samples.lock().unwrap_or_else(|e| e.into_inner()).insert(tab, tx);
    snorkle_content::dispatch_main(move || crate::state::with_host(|h| h.sample_headings(tab)));
    let sample = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(8)))
        .await
        .map_err(|_| "Request failed.".to_string())?;
    s.pending_samples.lock().unwrap_or_else(|e| e.into_inner()).remove(&tab);
    let sample = sample.map_err(|_| "The page didn't answer. Reload it with the headers view on and try again.")?;
    let (tx, rx) = mpsc::channel();
    snorkle_content::dispatch_main(move || {
        crate::state::with_host(|h| h.read_page_text(tab, Box::new(move |t| drop(tx.send(t)))))
    });
    let text = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(8)))
        .await.map_err(|_| "Request failed.")?;
    let page = text.ok().flatten().and_then(|t| ai::parse_page_text(&t, url.as_str()))
        .ok_or("The page didn't answer. Wait for it to load and try again.")?;
    let mut sample: serde_json::Value = serde_json::from_str(&sample).map_err(|_| "Invalid heading sample")?;
    sample["page_text"] = serde_json::Value::String(page.text);
    let req = ai::headings_request(provider, &key, &model, &site, &sample.to_string())?;
    let (status, body) =
        send(&s, provider, Some(&model), &format!("detect headings for {site} ({model})"), req).await?;
    let rules = ai::parse_headings_reply(provider, status, &body)?;
    let summaries = ai::parse_heading_summaries(provider, status, &body)?;
    if s.browser().tab(tab).and_then(|t| t.committed_url.as_ref()) != Some(&url) {
        return Err("The page changed. Detect headers again on the new page.".into());
    }
    if let Some(store) = s.store.clone() {
        store.set_heading_rules(&site, &rules).map_err(|e| e.to_string())?;
    }
    crate::scripts::apply(&s, true);
    Ok(DetectedHeadings { rules: rules.len(), url: url.to_string(), summaries })
}

/// A page-chat answer, with what the request used.
#[derive(Serialize)]
pub struct ChatReply {
    /// The archived chat this turn belongs to (`None` in private tabs,
    /// whose chats are never stored).
    chat_id: Option<String>,
    text: String,
    input: u64,
    output: u64,
    /// Estimated cost in millionths of a dollar; `None` for a model without
    /// a known price.
    cost_micros: Option<u64>,
}

fn wall_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Only provider-supplied, validated capabilities are cached; the chrome never supplies them.
async fn effort_support(s: &S<'_>, provider: Provider, model: &str) -> ai::EffortSupport {
    let fallback = ai::fallback_effort(provider, model);
    if provider != Provider::Anthropic {
        return fallback;
    }
    static CACHE: OnceLock<Mutex<BTreeMap<String, ai::EffortSupport>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let cached = cache.lock().unwrap_or_else(|e| e.into_inner()).get(model).cloned();
    if let Some(cached) = cached {
        return cached;
    }
    let Ok(key) = key(provider) else { return fallback };
    let Ok(req) = ai::model_effort_request(&key, model) else { return fallback };
    let Ok((status, body)) = send(s, provider, None, "model effort capabilities", req).await else { return fallback };
    let Ok(Some(support)) = ai::parse_effort_support(status, &body) else { return fallback };
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(model.to_string(), support.clone());
    support
}

/// Read-only capability metadata from the selected provider (no page data).
#[tauri::command]
pub async fn ai_efforts(s: S<'_>, provider: Provider, model: String) -> Result<Vec<String>, String> {
    Ok(effort_support(&s, provider, &model).await.levels)
}

/// AI chat about a page. A new chat reads the tab's page (title, visible
/// text, selection; channel C17) when the user sends the first message;
/// the URL comes from the core. In a non-private tab the chat is archived
/// with that context, and later turns (also when continued from the
/// archive, from any tab) reuse the stored context instead of reading the
/// page again. With `word` (Define on the new-tab page), a new chat is about
/// that single word instead: no page is read and the tab may be empty.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ai_chat(
    s: S<'_>,
    tab: u64,
    provider: Provider,
    model: String,
    messages: Vec<ChatMessage>,
    chat_id: Option<String>,
    effort: Option<String>,
    word: Option<String>,
) -> Result<ChatReply, String> {
    let key = key(provider)?;
    // Nothing typed in a private tab is stored, also when it continues an
    // archived chat.
    let tab_private = s.browser().tab(snorkle_core::tabs::TabId(tab)).is_some_and(|t| t.is_private());
    let stored = chat_id.as_deref().and_then(|id| s.store.as_ref()?.chat(id));
    let (context, private, record) = match stored {
        Some(rec) if !rec.word.is_empty() => (Context::Word(rec.word.clone()), tab_private, Some(rec)),
        Some(rec) => {
            let page = ai::PageText {
                title: rec.title.clone(),
                url: rec.url.clone(),
                text: rec.page_text.clone(),
                selection: rec.selection.clone(),
            };
            (Context::Page(page), tab_private, Some(rec))
        }
        None if word.is_some() => {
            let word = word.as_deref().and_then(ai::clean_word).ok_or("Define takes a single word.")?;
            (Context::Word(word), tab_private, None)
        }
        None => {
            let tab = snorkle_core::tabs::TabId(tab);
            let (url, private) = {
                let b = s.browser();
                let t = b.tab(tab).ok_or("Open a web page first.")?;
                let url = t
                    .committed_url
                    .clone()
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                    .ok_or("Open a web page first.")?;
                (url, t.is_private())
            };
            let (tx, rx) = mpsc::channel();
            snorkle_content::dispatch_main(move || {
                crate::state::with_host(|h| h.read_page_text(tab, Box::new(move |t| drop(tx.send(t)))))
            });
            let text = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(8)))
                .await
                .map_err(|_| "Request failed.".to_string())?;
            let page = text
                .ok()
                .flatten()
                .and_then(|t| ai::parse_page_text(&t, url.as_str()))
                .ok_or("The page didn't answer. Wait for it to load and try again.")?;
            (Context::Page(page), private, None)
        }
    };
    let (mut req, what) = match &context {
        Context::Page(page) => {
            let host = snorkle_core::Url::parse(&page.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            (ai::chat_request(provider, &key, &model, page, &messages)?, format!("chat about {host} ({model})"))
        }
        // The usage log names no word: it is what the user typed.
        Context::Word(word) => {
            (ai::define_request(provider, &key, &model, word, &messages)?, format!("definition ({model})"))
        }
    };
    if effort.as_deref().is_some_and(|e| !e.is_empty() && e != "default") {
        let support = effort_support(&s, provider, &model).await;
        ai::apply_supported_effort(&mut req, provider, effort.as_deref(), &support)?;
    }
    let (status, body) = send(&s, provider, Some(&model), &what, req).await?;
    let text = ai::parse_chat_reply(provider, status, &body)?;
    let usage = ai::parse_usage(provider, &body).unwrap_or(ai::Usage { input: 0, output: 0 });
    let cost_micros = ai::estimate_cost_micros(provider, &model, usage);
    // Archive (never for private tabs).
    let mut chat_id = None;
    if let (false, Some(store)) = (private, s.store.as_ref()) {
        let now = wall_ms();
        let mut rec = record.unwrap_or_else(|| {
            let (url, title, page_text, selection, word) = match &context {
                Context::Page(p) => {
                    (p.url.clone(), p.title.clone(), p.text.clone(), p.selection.clone(), String::new())
                }
                Context::Word(w) => (String::new(), w.clone(), String::new(), String::new(), w.clone()),
            };
            ChatRecord {
                id: format!("c{now}-{}", s.chat_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
                created_ms: now,
                updated_ms: now,
                url,
                title,
                page_text,
                selection,
                word,
                messages: Vec::new(),
                provider,
                model: model.clone(),
                input: 0,
                output: 0,
                cost_micros: 0,
                unpriced: false,
            }
        });
        rec.updated_ms = now;
        rec.provider = provider;
        rec.model = model.clone();
        rec.messages = messages
            .iter()
            .map(|m| ChatEntry { role: m.role, text: m.text.clone() })
            .chain([ChatEntry { role: ai::ChatRole::Assistant, text: text.clone() }])
            .collect();
        rec.input += usage.input;
        rec.output += usage.output;
        match cost_micros {
            Some(c) => rec.cost_micros += c,
            None => rec.unpriced = true,
        }
        let id = rec.id.clone();
        if store.save_chat(rec).unwrap_or(false) {
            chat_id = Some(id);
        }
    }
    Ok(ChatReply { chat_id, text, input: usage.input, output: usage.output, cost_micros })
}

/// What a chat is about: a page, or a word to define.
enum Context {
    Page(ai::PageText),
    Word(String),
}

/// One archived chat in the list.
#[derive(Serialize)]
pub struct ChatSummary {
    id: String,
    created_ms: u64,
    updated_ms: u64,
    host: String,
    title: String,
    /// The first question, shortened.
    first: String,
    /// A Define chat's word (empty for page chats).
    word: String,
    messages: usize,
    cost_micros: u64,
    unpriced: bool,
}

#[tauri::command]
pub fn list_chats(s: S<'_>) -> Vec<ChatSummary> {
    let Some(store) = s.store.as_ref() else { return Vec::new() };
    store
        .chats()
        .into_iter()
        .map(|c| ChatSummary {
            host: snorkle_core::Url::parse(&c.url)
                .ok()
                .map(|u| snorkle_core::url_display::display(&u).registrable)
                .unwrap_or_default(),
            first: c
                .messages
                .iter()
                .find(|m| m.role == ai::ChatRole::User)
                .map(|m| snorkle_core::text::sanitize_page_text(&m.text, 140))
                .unwrap_or_default(),
            messages: c.messages.len(),
            word: c.word,
            id: c.id,
            created_ms: c.created_ms,
            updated_ms: c.updated_ms,
            title: c.title,
            cost_micros: c.cost_micros,
            unpriced: c.unpriced,
        })
        .collect()
}

/// An archived chat to show and continue (without the stored page text).
#[derive(Serialize)]
pub struct ChatView {
    id: String,
    created_ms: u64,
    url: String,
    host: String,
    title: String,
    selection: String,
    word: String,
    messages: Vec<ChatEntry>,
    input: u64,
    output: u64,
    cost_micros: u64,
    unpriced: bool,
}

#[tauri::command]
pub fn get_chat(s: S<'_>, id: String) -> Option<ChatView> {
    let c = s.store.as_ref()?.chat(&id)?;
    let host = snorkle_core::Url::parse(&c.url)
        .ok()
        .map(|u| snorkle_core::url_display::display(&u).registrable)
        .unwrap_or_default();
    Some(ChatView {
        id: c.id,
        created_ms: c.created_ms,
        url: c.url,
        host,
        title: c.title,
        selection: c.selection,
        word: c.word,
        messages: c.messages,
        input: c.input,
        output: c.output,
        cost_micros: c.cost_micros,
        unpriced: c.unpriced,
    })
}

#[tauri::command]
pub fn delete_chat(s: S<'_>, id: String) -> Result<(), String> {
    match s.store.as_ref() {
        Some(st) => st.delete_chat(&id).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Drafts or revises a site script. The draft is shown in the editor; it
/// runs only if the user saves it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ai_write_script(
    s: S<'_>,
    provider: Provider,
    model: String,
    domain: String,
    css: String,
    js: String,
    prompt: String,
) -> Result<ScriptDraft, String> {
    let domain = snorkle_core::userscripts::normalise_domain(&domain).map_err(|e| e.to_string())?;
    let req = ai::script_request(provider, &key(provider)?, &model, &domain, &css, &js, &prompt)?;
    let (status, body) = send(&s, provider, Some(&model), &format!("write script for {domain} ({model})"), req).await?;
    ai::parse_script_reply(provider, status, &body)
}
