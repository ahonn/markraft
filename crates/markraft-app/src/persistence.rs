//! One worker owns the notes folder: it preserves save ordering, and it watches the
//! folder so that changes made by other programs reach the application.
use crate::{
    storage::Library,
    vault::{External, Store},
};
use notify::Watcher;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    time::Duration,
};

pub struct Saved {
    pub revision: u64,
    pub result: Result<(), String>,
}
pub enum Event {
    Saved(Saved),
    /// Other programs changed these notes. Snapshots are not written for them until
    /// the application calls [`Persistence::acknowledge`].
    External(Vec<External>),
}
enum Request {
    Save(u64, Library),
    Flush(Library, Sender<Result<(), String>>),
    Reload(Sender<Result<Library, String>>),
    Refresh,
    Acknowledge(Vec<String>),
}
pub struct Persistence {
    requests: Sender<Request>,
    events: Receiver<Event>,
    // Dropping the watcher stops it; the worker ends when `requests` is dropped.
    _watcher: Option<notify::RecommendedWatcher>,
}
impl Persistence {
    pub fn new(store: Store) -> Self {
        Self::start(store, true)
    }
    fn start(mut store: Store, watching: bool) -> Self {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        let watcher = watching.then(|| watch(&store, requests.clone())).flatten();
        std::thread::spawn(move || {
            for request in incoming {
                match request {
                    Request::Save(revision, library) => {
                        let result = store.save(&library);
                        let _ = outgoing.send(Event::Saved(Saved { revision, result }));
                    }
                    Request::Flush(library, response) => {
                        let _ = response.send(store.save(&library));
                    }
                    Request::Reload(response) => {
                        let _ = response.send(store.reload());
                    }
                    Request::Refresh => {
                        if let Ok(changes) = store.refresh()
                            && !changes.is_empty()
                        {
                            let _ = outgoing.send(Event::External(changes));
                        }
                    }
                    Request::Acknowledge(ids) => store.acknowledge(&ids),
                }
            }
        });
        Self {
            requests,
            events,
            _watcher: watcher,
        }
    }
    pub fn save(&self, revision: u64, library: Library) -> Result<(), String> {
        self.requests
            .send(Request::Save(revision, library))
            .map_err(|_| "The save worker stopped. Copy your note before quitting.".into())
    }
    /// The caller compares save revisions with its current document revision. Old
    /// successful acknowledgments must not clear a newer pending change or its error.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
    pub fn acknowledge(&self, ids: Vec<String>) {
        let _ = self.requests.send(Request::Acknowledge(ids));
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
            .recv_timeout(Duration::from_secs(10))
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

/// One refresh request per burst of file events. A program saving a file produces
/// several, and Markraft's own writes produce them too; the store tells those apart.
fn watch(store: &Store, requests: Sender<Request>) -> Option<notify::RecommendedWatcher> {
    let queued = Arc::new(AtomicBool::new(false));
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() && !queued.swap(true, Ordering::SeqCst) {
            let queued = queued.clone();
            let requests = requests.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                queued.store(false, Ordering::SeqCst);
                let _ = requests.send(Request::Refresh);
            });
        }
    })
    .ok()?;
    watcher
        .watch(store.directory(), notify::RecursiveMode::Recursive)
        .ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc;

    fn saves(persistence: &Persistence) -> Vec<Saved> {
        persistence
            .poll()
            .into_iter()
            .filter_map(|event| match event {
                Event::Saved(saved) => Some(saved),
                Event::External(_) => None,
            })
            .collect()
    }

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
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Title\n\nFirst 中文"));
        persistence.save(1, library.clone()).unwrap();
        library.set_document(&id, doc::from_markdown("Title\n\nSecond 👩🏽‍💻"));
        persistence.save(2, library.clone()).unwrap();
        library.set_document(&id, doc::from_markdown("Title\n\nFinal é"));
        persistence.flush(library.clone()).unwrap();
        assert!(
            only_note(directory.path())
                .1
                .ends_with("---\nTitle\n\nFinal é\n")
        );
        let acknowledgments = saves(&persistence);
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
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Shared"));
        persistence.save(10, library.clone()).unwrap();
        persistence.flush(library.clone()).unwrap();
        let (path, _) = only_note(directory.path());
        std::fs::write(&path, b"external content").unwrap();
        library.set_document(&id, doc::from_markdown("Shared, edited here"));
        library.new_note(doc::from_markdown("Keep this local work"));
        persistence.save(11, library.clone()).unwrap();
        assert!(persistence.flush(library).is_err());
        let acknowledgments = saves(&persistence);
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
        let persistence = Persistence {
            requests,
            events: results,
            _watcher: None,
        };
        assert!(persistence.save(1, Library::default()).is_err());
        assert!(persistence.flush(Library::default()).is_err());
        assert!(persistence.reload().is_err());
    }

    #[test]
    fn reload_is_a_barrier_after_conflicts_and_new_edits_can_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut local) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = local.active_id.clone();
        local.set_document(&id, doc::from_markdown("Original"));
        persistence.flush(local.clone()).unwrap();
        let (path, text) = only_note(directory.path());
        std::fs::write(&path, text.replace("Original", "External text")).unwrap();
        local.set_document(&id, doc::from_markdown("Discard this after confirmation"));
        persistence.save(1, local).unwrap();
        let mut reloaded = persistence.reload().unwrap();
        assert_eq!(
            doc::plain_text(&reloaded.note(&id).unwrap().document),
            "External text"
        );
        let acknowledgments = saves(&persistence);
        assert_eq!(acknowledgments.len(), 1);
        assert_eq!(acknowledgments[0].revision, 1);
        assert!(acknowledgments[0].result.is_err());
        reloaded.set_document(&id, doc::from_markdown("External text, continued"));
        persistence.save(2, reloaded.clone()).unwrap();
        persistence.flush(reloaded).unwrap();
        assert!(saves(&persistence)[0].result.is_ok());
        assert!(
            only_note(directory.path())
                .1
                .ends_with("External text, continued\n")
        );
    }

    #[test]
    fn changes_by_other_programs_are_reported_and_held_back_until_acknowledged() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::new(store);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Original"));
        persistence.flush(library.clone()).unwrap();
        let (path, text) = only_note(directory.path());
        std::fs::write(&path, text.replace("Original", "From another editor")).unwrap();
        std::fs::write(directory.path().join("notes/dropped.md"), "Dropped in").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while seen.len() < 2 && std::time::Instant::now() < deadline {
            for event in persistence.poll() {
                if let Event::External(changes) = event {
                    seen.extend(changes);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let texts: Vec<_> = seen
            .iter()
            .map(|change| match change {
                External::Updated { note, .. } => doc::plain_text(&note.document),
                External::Removed(_) => panic!("nothing was removed"),
            })
            .collect();
        assert!(
            texts.contains(&"From another editor".to_owned()),
            "{texts:?}"
        );
        assert!(texts.contains(&"Dropped in".to_owned()), "{texts:?}");

        // A snapshot taken before the change must not undo it.
        library.set_document(&id, doc::from_markdown("Stale local edit"));
        persistence.flush(library.clone()).unwrap();
        assert!(folder_text(directory.path()).contains("From another editor"));
        assert!(!folder_text(directory.path()).contains("Stale local edit"));
        persistence.acknowledge(vec![id]);
        persistence.flush(library).unwrap();
        assert!(folder_text(directory.path()).contains("Stale local edit"));
        assert!(!folder_text(directory.path()).contains("From another editor"));
    }

    /// Every note in the folder, concatenated; saving may rename a note's file.
    fn folder_text(directory: &std::path::Path) -> String {
        std::fs::read_dir(directory.join("notes"))
            .unwrap()
            .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
            .collect()
    }
}
