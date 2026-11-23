//! The storage thread: history, page text and the session written off the
//! main thread, session writes coalesced.

use super::*;

/// How long after a page finished loading its text is read for history
/// (pages often fill in after the load event).
pub(super) const PAGE_TEXT_DELAY: Duration = Duration::from_millis(1500);

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
