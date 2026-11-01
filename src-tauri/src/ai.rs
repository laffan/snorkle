//! SECURITY-REVIEW: AI provider commands. Keys go from the chrome straight
//! into the Keychain and are read back only to send a request to the
//! provider's fixed API host (`snorkle_core::ai`); they are never returned
//! to the chrome. Requests and their outcome (never the key) are logged in
//! the in-app console.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use snorkle_core::ai::{self, ChatMessage, Model, Provider, ScriptDraft};
use snorkle_core::console::Level;
use snorkle_core::storage::AiUsageEvent;

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

/// "Detect Headers": samples the tab's heading-like elements (headers view
/// on), asks the model which are headings on this site, stores the rules
/// for the site and restarts the outline with them. Returns the number of
/// rules. Sends the sample (tags, classes, styles, short texts) to the
/// provider; runs only when the user presses the button.
#[tauri::command]
pub async fn detect_headings(s: S<'_>, tab: u64, provider: Provider, model: String) -> Result<usize, String> {
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
    let key = key(provider)?;
    let (tx, rx) = mpsc::channel();
    s.pending_samples.lock().unwrap_or_else(|e| e.into_inner()).insert(tab, tx);
    snorkle_content::dispatch_main(move || crate::state::with_host(|h| h.sample_headings(tab)));
    let sample = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(8)))
        .await
        .map_err(|_| "Request failed.".to_string())?;
    s.pending_samples.lock().unwrap_or_else(|e| e.into_inner()).remove(&tab);
    let sample = sample.map_err(|_| "The page didn't answer. Reload it with the headers view on and try again.")?;
    let req = ai::headings_request(provider, &key, &model, &site, &sample)?;
    let (status, body) =
        send(&s, provider, Some(&model), &format!("detect headings for {site} ({model})"), req).await?;
    let rules = ai::parse_headings_reply(provider, status, &body)?;
    if let Some(store) = s.store.clone() {
        store.set_heading_rules(&site, &rules).map_err(|e| e.to_string())?;
    }
    crate::scripts::apply(&s, true);
    Ok(rules.len())
}

/// A page-chat answer, with what the request used.
#[derive(Serialize)]
pub struct ChatReply {
    text: String,
    input: u64,
    output: u64,
    /// Estimated cost in millionths of a dollar; `None` for a model without
    /// a known price.
    cost_micros: Option<u64>,
}

/// AI chat about the tab's page: reads the page's title, visible text and
/// selection (channel C17) when the user sends a message, and sends them
/// with the conversation to the provider. The URL comes from the core.
#[tauri::command]
pub async fn ai_chat(
    s: S<'_>,
    tab: u64,
    provider: Provider,
    model: String,
    messages: Vec<ChatMessage>,
) -> Result<ChatReply, String> {
    let tab = snorkle_core::tabs::TabId(tab);
    let url = s
        .browser()
        .tab(tab)
        .and_then(|t| t.committed_url.clone())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or("Open a web page first.")?;
    let key = key(provider)?;
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
    let host = url.host_str().unwrap_or("").to_string();
    let req = ai::chat_request(provider, &key, &model, &page, &messages)?;
    let (status, body) = send(&s, provider, Some(&model), &format!("chat about {host} ({model})"), req).await?;
    let text = ai::parse_chat_reply(provider, status, &body)?;
    let usage = ai::parse_usage(provider, &body).unwrap_or(ai::Usage { input: 0, output: 0 });
    Ok(ChatReply {
        text,
        input: usage.input,
        output: usage.output,
        cost_micros: ai::estimate_cost_micros(provider, &model, usage),
    })
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
