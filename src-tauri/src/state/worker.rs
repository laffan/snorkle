//! The storage thread: history, page text and the session written off the
//! main thread, session writes coalesced.

use super::*;

/// How long after a page finished loading its text is read for history
/// (pages often fill in after the load event). Eight seconds since
/// 2026-11-23 (1.5 s before): the read (`innerText`, a walk over the whole
/// page, then up to 48,000 characters over IPC and a disk write) then
/// happens after the page's own late work, and only for a page that is
/// still on screen, not loading, and showing the same address, so pages
/// only glanced at are not read at all.
pub(super) const PAGE_TEXT_DELAY: Duration = Duration::from_millis(8000);

/// Longest a session snapshot waits before it is written.
pub(super) const SESSION_DELAY: Duration = Duration::from_millis(400);

/// Storage worker: history appends are written as they come; session
/// snapshots are coalesced (only the newest within a short window is
/// written), since every commit produces one.
pub(super) fn spawn_storage_worker(store: Arc<Store>) -> Sender<StorageJob> {
    let (tx, rx) = mpsc::channel::<StorageJob>();
    std::thread::Builder::new()
        .name("snorkle-storage".into())
        .spawn(move || {
            // Session snapshots are coalesced: the latest one is written at
            // most 400 ms after the first one that arrived since the last
            // write. A fixed deadline, so a steady stream of jobs (a busy
            // page's navigations and history) cannot postpone it forever.
            let mut pending: Option<(Session, Instant)> = None;
            loop {
                let job = match &pending {
                    Some((_, due)) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(j) => Some(j),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(j) => Some(j),
                        Err(_) => break,
                    },
                };
                match job {
                    Some(StorageJob::History { url, title }) => {
                        let _ = store.add_history(&url, &title, wall_ms());
                    }
                    Some(StorageJob::Session(s)) => {
                        let due = pending.take().map(|(_, d)| d).unwrap_or_else(|| Instant::now() + SESSION_DELAY);
                        pending = Some((s, due));
                    }
                    Some(StorageJob::OfflineCopy { url, data }) => {
                        let _ = store.save_offline_copy(&url, &data, wall_ms());
                    }
                    Some(StorageJob::PageText { url, title, text }) => {
                        let _ = store.save_page_text(&url, &title, &text, wall_ms());
                    }
                    Some(StorageJob::Flush) | None => {
                        if let Some((s, _)) = pending.take() {
                            let _ = store.save_session(&s);
                        }
                    }
                    Some(StorageJob::FlushNow(done)) => {
                        if let Some((s, _)) = pending.take() {
                            let _ = store.save_session(&s);
                        }
                        let _ = done.send(());
                    }
                }
                if pending.as_ref().is_some_and(|(_, due)| *due <= Instant::now()) {
                    if let Some((s, _)) = pending.take() {
                        let _ = store.save_session(&s);
                    }
                }
            }
            if let Some((s, _)) = pending.take() {
                let _ = store.save_session(&s);
            }
        })
        .expect("spawn storage thread");
    tx
}
