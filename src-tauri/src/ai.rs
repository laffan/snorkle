//! SECURITY-REVIEW: AI provider commands. Keys go from the chrome straight
//! into the Keychain and are read back only to send a request to the
//! provider's fixed API host (`snorkle_core::ai`); they are never returned
//! to the chrome. Requests and their outcome (never the key) are logged in
//! the in-app console.

use std::collections::BTreeMap;
use std::sync::mpsc;

use serde::Serialize;
use snorkle_core::ai::{self, Model, Provider, ScriptDraft};
use snorkle_core::console::Level;

use crate::state::Shared;

type S<'a> = tauri::State<'a, std::sync::Arc<Shared>>;

/// Sends `req` and waits (off the async runtime's worker) for the reply.
async fn send(s: &S<'_>, p: Provider, what: &str, req: ai::HttpRequest) -> Result<(u16, Vec<u8>), String> {
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

/// Checks the stored key by listing the account's models.
#[tauri::command]
pub async fn ai_test(s: S<'_>, provider: Provider) -> Result<Vec<Model>, String> {
    let req = ai::list_models_request(provider, &key(provider)?);
    let (status, body) = send(&s, provider, "list models", req).await?;
    ai::parse_models(provider, status, &body)
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
    let (status, body) = send(&s, provider, &format!("write script for {domain} ({model})"), req).await?;
    ai::parse_script_reply(provider, status, &body)
}
