//! One writer preserves save ordering; acknowledgments identify the saved revision.
use crate::{storage::Library, vault::Store};
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

    fn open(directory: &std::path::Path) -> (Store, Library) {
        Store::open(directory.join("notes"), directory.join("settings.json")).unwrap()
    }

    /// The only Markdown file in the notes folder, and its text.
    fn only_note(directory: &std::path::Path) -> (std::path::PathBuf, String) {
        let mut files: Vec<_> = std::fs::read_dir(directory.join("notes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let path = files.remove(0);
        let text = std::fs::read_to_string(&path).unwrap();
        (path, text)
    }

    #[test]
    fn flush_saves_the_latest_snapshot_after_queued_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::new(store);
        let id = library.active_id.clone();
        library.set_document(&id, Document::from_markdown("Title\nFirst 中文"));
        persistence.save(1, library.clone()).unwrap();
        library.set_document(&id, Document::from_markdown("Title\nSecond 👩🏽‍💻"));
        persistence.save(2, library.clone()).unwrap();
        library.set_document(&id, Document::from_markdown("Title\nFinal é"));
        persistence.flush(library.clone()).unwrap();
        assert!(
            only_note(directory.path())
                .1
                .ends_with("---\nTitle\nFinal é\n")
        );
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
    fn a_note_changed_by_another_program_is_not_overwritten_but_others_are_saved() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::new(store);
        let id = library.active_id.clone();
        library.set_document(&id, Document::from_markdown("Shared"));
        persistence.save(10, library.clone()).unwrap();
        persistence.flush(library.clone()).unwrap();
        let (path, _) = only_note(directory.path());
        std::fs::write(&path, b"external content").unwrap();
        library.set_document(&id, Document::from_markdown("Shared, edited here"));
        library.new_note(Document::from_markdown("Keep this local work"));
        persistence.save(11, library.clone()).unwrap();
        assert!(persistence.flush(library).is_err());
        let acknowledgments = persistence.poll();
        assert_eq!(acknowledgments.len(), 2);
        assert!(acknowledgments[0].result.is_ok());
        assert_eq!(acknowledgments[1].revision, 11);
        assert!(acknowledgments[1].result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external content");
        let saved = std::fs::read_dir(directory.path().join("notes"))
            .unwrap()
            .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
            .any(|text| text.ends_with("Keep this local work\n"));
        assert!(saved);
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
        let (store, mut local) = open(directory.path());
        let persistence = Persistence::new(store);
        let id = local.active_id.clone();
        local.set_document(&id, Document::from_markdown("Original"));
        persistence.flush(local.clone()).unwrap();
        let (path, text) = only_note(directory.path());
        std::fs::write(&path, text.replace("Original", "External text")).unwrap();
        local.set_document(
            &id,
            Document::from_markdown("Discard this after confirmation"),
        );
        persistence.save(1, local).unwrap();
        let mut reloaded = persistence.reload().unwrap();
        assert_eq!(
            reloaded.note(&id).unwrap().document.plain_text(),
            "External text"
        );
        let acknowledgments = persistence.poll();
        assert_eq!(acknowledgments.len(), 1);
        assert_eq!(acknowledgments[0].revision, 1);
        assert!(acknowledgments[0].result.is_err());
        reloaded.set_document(&id, Document::from_markdown("External text, continued"));
        persistence.save(2, reloaded.clone()).unwrap();
        persistence.flush(reloaded).unwrap();
        assert!(persistence.poll()[0].result.is_ok());
        assert!(
            only_note(directory.path())
                .1
                .ends_with("External text, continued\n")
        );
    }
}
