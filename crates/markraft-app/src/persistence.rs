//! One writer preserves save ordering; acknowledgments identify the saved revision.
use crate::storage::{Library, Store};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

pub struct Saved {
    pub revision: u64,
    pub result: Result<(), String>,
}
enum Request {
    Save(u64, Library),
    Flush(Library, Sender<Result<(), String>>),
    Reload(Sender<Result<Library, String>>),
}
pub struct Persistence {
    requests: Sender<Request>,
    results: Receiver<Saved>,
}
impl Persistence {
    pub fn new(mut store: Store) -> Self {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, results) = mpsc::channel();
        std::thread::spawn(move || {
            for request in incoming {
                match request {
                    Request::Save(revision, library) => {
                        // Store checks external modifications even for an unchanged snapshot.
                        let result = store.save(&library);
                        let _ = outgoing.send(Saved { revision, result });
                    }
                    Request::Flush(library, response) => {
                        let _ = response.send(store.save(&library));
                    }
                    Request::Reload(response) => {
                        let _ = response.send(store.reload());
                    }
                }
            }
        });
        Self { requests, results }
    }
    pub fn save(&self, revision: u64, library: Library) -> Result<(), String> {
        self.requests
            .send(Request::Save(revision, library))
            .map_err(|_| "The save worker stopped. Copy your note before quitting.".into())
    }
    /// The caller compares revisions with its current document revision. Old successful
    /// acknowledgments must not clear a newer pending change or its save error.
    pub fn poll(&self) -> Vec<Saved> {
        self.results.try_iter().collect()
    }
    /// Reload after all earlier save requests finish. The caller must confirm discarding
    /// local changes and invalidate their revision acknowledgments before adopting the result.
    pub fn reload(&self) -> Result<Library, String> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Reload(response))
            .map_err(|_| "The save worker stopped.".to_string())?;
        // Do not time out and leave an invisible baseline change queued: the UI must
        // receive the adopted library before any later local snapshot can be saved.
        result
            .recv()
            .map_err(|_| "The save worker stopped before reloading finished.".to_string())?
    }
    /// A queue barrier: all earlier requests finish before this latest snapshot is saved.
    /// A timeout leaves the request queued; callers must retain the note and show the error.
    pub fn flush(&self, library: Library) -> Result<(), String> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Flush(library, response))
            .map_err(|_| "The save worker stopped.".to_string())?;
        result
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|error| {
                match error {
                    RecvTimeoutError::Timeout => {
                        "Saving is taking too long. The note remains open; try again."
                    }
                    RecvTimeoutError::Disconnected => {
                        "The save worker stopped before saving finished. \
                         Copy your note before quitting."
                    }
                }
                .to_string()
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_core::Document;

    #[test]
    fn flush_saves_the_latest_snapshot_after_queued_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.json");
        let (store, mut library) = Store::open(path.clone()).unwrap();
        let persistence = Persistence::new(store);
        let id = library.active_id.clone();
        library.set_document(&id, Document::from_markdown("First 中文"));
        persistence.save(1, library.clone()).unwrap();
        library.set_document(&id, Document::from_markdown("Second 👩🏽‍💻"));
        persistence.save(2, library.clone()).unwrap();
        library.set_document(&id, Document::from_markdown("Final é"));
        persistence.flush(library.clone()).unwrap();
        let saved: Library = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved, library);
        let acknowledgments = persistence.poll();
        assert_eq!(
            acknowledgments
                .iter()
                .map(|saved| saved.revision)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(acknowledgments.iter().all(|saved| saved.result.is_ok()));
        assert!(persistence.poll().is_empty());
    }

    #[test]
    fn external_conflicts_propagate_for_saves_and_unchanged_flushes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.json");
        let (store, mut library) = Store::open(path.clone()).unwrap();
        let persistence = Persistence::new(store);
        persistence.save(10, library.clone()).unwrap();
        persistence.flush(library.clone()).unwrap();
        std::fs::write(&path, b"external content").unwrap();
        // Even when our local snapshot did not change, a flush must not report that
        // this snapshot is safely persisted if the file now contains somebody else's data.
        assert!(persistence.flush(library.clone()).is_err());
        library.new_note(Document::from_markdown("Keep this local work"));
        persistence.save(11, library.clone()).unwrap();
        assert!(persistence.flush(library).is_err());
        let acknowledgments = persistence.poll();
        assert_eq!(acknowledgments.len(), 2);
        assert_eq!(acknowledgments[0].revision, 10);
        assert!(acknowledgments[0].result.is_ok());
        assert_eq!(acknowledgments[1].revision, 11);
        assert!(acknowledgments[1].result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"external content");
    }

    #[test]
    fn disconnected_worker_reports_failure_instead_of_a_successful_flush() {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, results) = mpsc::channel();
        drop(incoming);
        drop(outgoing);
        let persistence = Persistence { requests, results };
        assert!(persistence.save(1, Library::default()).is_err());
        assert!(persistence.flush(Library::default()).is_err());
        assert!(persistence.reload().is_err());
    }

    #[test]
    fn reload_is_a_barrier_after_conflicts_and_new_edits_can_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.json");
        let (store, mut local) = Store::open(path.clone()).unwrap();
        let persistence = Persistence::new(store);
        persistence.flush(local.clone()).unwrap();
        let mut external = local.clone();
        external.new_note(Document::from_markdown("External text"));
        std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
        local.new_note(Document::from_markdown("Discard this after confirmation"));
        persistence.save(1, local).unwrap();
        let mut reloaded = persistence.reload().unwrap();
        assert_eq!(reloaded, external);
        let acknowledgments = persistence.poll();
        assert_eq!(acknowledgments.len(), 1);
        assert_eq!(acknowledgments[0].revision, 1);
        assert!(acknowledgments[0].result.is_err());
        reloaded.new_note(Document::from_markdown("New local note"));
        persistence.save(2, reloaded.clone()).unwrap();
        persistence.flush(reloaded.clone()).unwrap();
        let saved: Library = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved, reloaded);
        assert!(persistence.poll()[0].result.is_ok());
    }
}
