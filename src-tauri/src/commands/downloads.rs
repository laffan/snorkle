//! Downloads.

use super::*;

// ---- downloads ----------------------------------------------------------------

#[tauri::command]
pub fn list_downloads(s: S<'_>) -> Vec<crate::state::DownloadItem> {
    s.downloads().clone()
}

#[tauri::command]
pub fn cancel_download(s: S<'_>, id: u64) {
    if s.downloads().iter().any(|d| d.id == id && d.state == "active") {
        s.cancelled_downloads.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
        snorkle_content::dispatch_main(move || with_host(|h| h.cancel_download(id)));
    }
}

/// Removes a download from the list and deletes its file (a running one
/// is cancelled first).
#[tauri::command]
pub fn delete_download(s: S<'_>, id: u64) {
    cancel_download(s.clone(), id);
    s.downloads().retain(|d| d.id != id);
    if let Some(store) = &s.store {
        let _ = store.delete_download(id);
    }
    s.emit_downloads();
}

/// Removes every download that is not running, with its file.
#[tauri::command]
pub fn clear_downloads(s: S<'_>) {
    let gone: Vec<u64> = {
        let mut d = s.downloads();
        let gone = d.iter().filter(|x| x.state != "active").map(|x| x.id).collect();
        d.retain(|x| x.state == "active");
        gone
    };
    if let Some(store) = &s.store {
        for id in gone {
            let _ = store.delete_download(id);
        }
    }
    s.emit_downloads();
}

/// The staged file of a finished download.
pub(super) fn download_file(s: &S<'_>, id: u64) -> Result<std::path::PathBuf, String> {
    let name = s.downloads().iter().find(|d| d.id == id && d.state == "done").map(|d| d.name.clone());
    let name = name.ok_or("This download is not finished.")?;
    let store = s.store.as_ref().ok_or("Storage unavailable")?;
    let path = store.download_path(id, &name).ok_or("Invalid file name.")?;
    if !path.is_file() {
        return Err("The file is gone.".into());
    }
    Ok(path)
}

/// Runs `f` with the content host on the main thread and waits for its
/// result.
pub(super) async fn on_host(f: impl FnOnce(&mut snorkle_content::ContentHost) -> Result<(), String> + Send + 'static) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let mut result = Err("Not available.".to_string());
        with_host(|h| result = f(h));
        let _ = tx.send(result);
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(5)))
        .await
        .map_err(|_| "Not available.".to_string())?
        .unwrap_or(Ok(()))
}

/// Saves a copy of a download outside the app: the Files export picker
/// (iPadOS) or a save panel (macOS).
#[tauri::command]
pub async fn export_download(s: S<'_>, id: u64) -> Result<(), String> {
    let path = download_file(&s, id)?;
    on_host(move |h| h.export_file(&path)).await
}

/// The share sheet for a download, from the button at (x, y, w, h).
#[tauri::command]
pub async fn share_download(s: S<'_>, id: u64, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let path = download_file(&s, id)?;
    on_host(move |host| host.share_file(&path, (x, y, w, h))).await
}

/// The user's answer to a download from a frame inside a page
/// (`download_prompt` event). Unknown or expired requests are ignored.
#[tauri::command]
pub fn answer_download_prompt(ask: u64, allow: bool) {
    snorkle_content::dispatch_main(move || with_host(|h| h.answer_frame_download(ask, allow)));
}
