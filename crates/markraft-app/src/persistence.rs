//! One worker owns the notes folder: it preserves save ordering, and it watches the
//! folder so that changes made by other programs reach the application.
use crate::{
    storage::{Library, Note, Notices},
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
    pub paths: Vec<(String, std::path::PathBuf)>,
    pub conflicts: Vec<String>,
}
pub enum Event {
    Saved(Saved),
    /// Other programs changed these notes. Snapshots are not written for them until
    /// the application calls [`Persistence::acknowledge`].
    External(Vec<External>),
}
type Purged = (Vec<String>, Result<(), String>);
enum Request {
    Save(u64, Library),
    Recover(Note, Sender<Result<(), String>>),
    Markdown(Note, Sender<Result<String, String>>),
    Review(Note, Sender<Result<Option<String>, String>>),
    Resolve(Note, Sender<Result<Option<Note>, String>>),
    OpenFile(std::path::PathBuf, Sender<Result<Note, String>>),
    Paths(Sender<Vec<(String, std::path::PathBuf)>>),
    Conflicts(Sender<Vec<String>>),
    Flush(Library, Sender<Result<(), String>>),
    Reload(Sender<Result<Library, String>>),
    Refresh,
    RefreshPaths(Vec<std::path::PathBuf>),
    Acknowledge(Vec<String>),
    Purge(Vec<String>, Sender<Purged>),
}
pub struct Persistence {
    requests: Sender<Request>,
    events: Receiver<Event>,
    /// Shared with the store the worker owns, so what it notices on its own
    /// thread still reaches the interface.
    notices: Notices,
    // Dropping the watcher stops it; the worker ends when `requests` is dropped.
    _watcher: std::sync::Mutex<Option<notify::RecommendedWatcher>>,
    watch_root: std::path::PathBuf,
    extra_watches: std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
}
impl Persistence {
    pub fn new(store: Store) -> Self {
        Self::start(store, true)
    }
    fn start(mut store: Store, watching: bool) -> Self {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        let notices = store.notices();
        let watch_root = store.directory().to_owned();
        let extra_watches =
            std::sync::Mutex::new(store.extra_watch_directories().into_iter().collect());
        let watcher = watching.then(|| watch(&store, requests.clone())).flatten();
        std::thread::spawn(move || {
            for request in incoming {
                match request {
                    Request::Conflicts(response) => {
                        let _ = response.send(store.conflicts());
                    }
                    Request::Review(note, response) => {
                        let _ = response.send(store.review_conflict(&note));
                    }
                    Request::Markdown(note, response) => {
                        let _ = response.send(store.markdown(&note));
                    }
                    Request::Recover(note, response) => {
                        let _ = response.send(store.recover(&note));
                    }
                    Request::Resolve(note, response) => {
                        let _ = response.send(store.resolve_conflict(&note));
                    }
                    Request::OpenFile(path, response) => {
                        let _ = response.send(store.add_file(path));
                    }
                    Request::Paths(response) => {
                        let _ = response.send(store.paths());
                    }
                    Request::Save(revision, library) => {
                        let result = store.save(&library);
                        let _ = outgoing.send(Event::Saved(Saved {
                            revision,
                            result,
                            paths: store.paths(),
                            conflicts: store.conflicts(),
                        }));
                    }
                    Request::Flush(library, response) => {
                        let _ = response.send(store.save(&library));
                    }
                    Request::Reload(response) => {
                        let _ = response.send(store.reload());
                    }
                    Request::RefreshPaths(paths) => match store.refresh_paths(&paths) {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => store.notices().raise(error),
                        _ => {}
                    },
                    Request::Refresh => match store.refresh() {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => store.notices().raise(error),
                        _ => {}
                    },
                    Request::Acknowledge(ids) => store.acknowledge(&ids),
                    Request::Purge(ids, response) => {
                        let _ = response.send(store.purge(&ids));
                    }
                }
            }
        });
        Self {
            requests,
            events,
            notices,
            _watcher: std::sync::Mutex::new(watcher),
            watch_root,
            extra_watches,
        }
    }
    pub fn refresh(&self) {
        let _ = self.requests.send(Request::Refresh);
    }
    pub fn conflicts(&self) -> Result<Vec<String>, String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Conflicts(tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped".into())
    }
    pub fn paths(&self) -> Result<Vec<(String, std::path::PathBuf)>, String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Paths(tx))
            .map_err(|_| "The save worker stopped")?;
        let paths = rx
            .recv()
            .map_err(|_| "The save worker stopped".to_owned())?;
        for (_, path) in &paths {
            self.watch_file(path);
        }
        Ok(paths)
    }
    fn watch_file(&self, path: &std::path::Path) {
        if path.starts_with(&self.watch_root) {
            return;
        }
        let Some(parent) = path.parent() else { return };
        if let (Ok(mut watched), Ok(mut watcher)) =
            (self.extra_watches.lock(), self._watcher.lock())
            && !watched.contains(parent)
            && let Some(watcher) = watcher.as_mut()
        {
            match watcher.watch(parent, notify::RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(parent.to_owned());
                }
                Err(error) => self.notices.raise(format!(
                    "This file could not be watched: {error}. Refresh to check external changes."
                )),
            }
        }
    }
    pub fn open_file(&self, path: std::path::PathBuf) -> Result<Note, String> {
        self.watch_file(&path);
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::OpenFile(path, tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped")?
    }
    pub fn markdown(&self, note: Note) -> Result<String, String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Markdown(note, tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped")?
    }
    pub fn recover(&self, note: Note) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Recover(note, tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped")?
    }
    pub fn review_conflict(&self, note: Note) -> Result<Option<String>, String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Review(note, tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped")?
    }
    pub fn resolve_conflict(&self, note: Note) -> Result<Option<Note>, String> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Resolve(note, tx))
            .map_err(|_| "The save worker stopped")?;
        rx.recv().map_err(|_| "The save worker stopped")?
    }
    pub fn save(&self, revision: u64, library: Library) -> Result<(), String> {
        self.requests
            .send(Request::Save(revision, library))
            .map_err(|_| {
                "Saving stopped working. Copy your note (⇧⌘C), then quit and reopen Markraft."
                    .into()
            })
    }
    /// Everything the notes folder gave the user to read, once there is somewhere
    /// to show it. Empty after it has been taken.
    pub fn notices(&self) -> Vec<String> {
        self.notices.take()
    }
    /// Delete these notes' files for good, after all earlier requests have run. Returns
    /// the ids that are gone, which the caller drops from its library, and the reason a
    /// purge stopped short. A timeout leaves the request queued and reports it, so the
    /// notes stay in the trash rather than disappearing from a folder that still has
    /// them; the next refresh reconciles whatever the worker did get to.
    pub fn purge(&self, ids: Vec<String>) -> Purged {
        let (response, result) = mpsc::channel();
        let disconnected = || {
            "Markraft can no longer reach your notes folder, so nothing was deleted. \
             Quit and reopen Markraft, then try again."
                .to_string()
        };
        if self.requests.send(Request::Purge(ids, response)).is_err() {
            return (Vec::new(), Err(disconnected()));
        }
        result
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| {
                (
                    Vec::new(),
                    Err(match error {
                        RecvTimeoutError::Timeout => {
                            "Deleting is taking too long. The notes are still in \
                             Recently Deleted; try again."
                                .to_string()
                        }
                        RecvTimeoutError::Disconnected => disconnected(),
                    }),
                )
            })
    }
    /// The caller compares save revisions with its current document revision. Old
    /// successful acknowledgments must not clear a newer pending change or its error.
    pub fn poll(&self) -> Vec<Event> {
        let events: Vec<_> = self.events.try_iter().collect();
        for event in &events {
            if let Event::Saved(saved) = event {
                for (_, path) in &saved.paths {
                    self.watch_file(path);
                }
            }
        }
        events
    }
    pub fn acknowledge(&self, ids: Vec<String>) {
        let _ = self.requests.send(Request::Acknowledge(ids));
    }
    /// Reload after all earlier save requests finish. The caller must confirm discarding
    /// local changes and invalidate their revision acknowledgments before adopting the result.
    pub fn reload(&self) -> Result<Library, String> {
        let (response, result) = mpsc::channel();
        self.requests.send(Request::Reload(response)).map_err(|_| {
            "Markraft can no longer reach your notes folder. Copy your note (⇧⌘C), \
             then quit and reopen Markraft."
                .to_string()
        })?;
        // Do not time out and leave an invisible baseline change queued: the UI must
        // receive the adopted library before any later local snapshot can be saved.
        result.recv().map_err(|_| {
            "Markraft stopped reading your notes folder before it had finished. \
             Copy your note (⇧⌘C), then quit and reopen Markraft."
                .to_string()
        })?
    }
    /// A queue barrier: all earlier requests finish before this latest snapshot is saved.
    /// A timeout leaves the request queued; callers must retain the note and show the error.
    pub fn flush(&self, library: Library) -> Result<(), String> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Flush(library, response))
            .map_err(|_| {
                "Saving stopped working. Copy your note (⇧⌘C), then quit and reopen Markraft."
                    .to_string()
            })?;
        result
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| {
                match error {
                    RecvTimeoutError::Timeout => {
                        "Saving is taking too long. The note remains open; try again."
                    }
                    RecvTimeoutError::Disconnected => {
                        "Saving stopped before your note was written. \
                         Copy your note (⇧⌘C), then quit and reopen Markraft."
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
    let paths = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let full_scan = Arc::new(AtomicBool::new(false));
    let notices = store.notices();
    let callback_notices = notices.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match event {
            Ok(event) => {
                let mut relevant = false;
                for path in event.paths {
                    let markdown = path.extension().is_some_and(|e| {
                        e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown")
                    });
                    if path.is_dir()
                        || (!markdown
                            && matches!(
                                event.kind,
                                notify::EventKind::Remove(notify::event::RemoveKind::Folder)
                                    | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                            ))
                    {
                        full_scan.store(true, Ordering::SeqCst);
                        relevant = true;
                    } else if markdown {
                        if let Ok(mut paths) = paths.lock() {
                            paths.insert(path);
                        }
                        relevant = true;
                    }
                }
                relevant
            }
            Err(error) => {
                callback_notices.raise(format!(
                    "File watching failed: {error}. Refresh the folder to check external edits."
                ));
                full_scan.store(true, Ordering::SeqCst);
                true
            }
        };
        if relevant && !queued.swap(true, Ordering::SeqCst) {
            let queued = queued.clone();
            let paths = paths.clone();
            let full_scan = full_scan.clone();
            let requests = requests.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                // Release the scheduling flag before draining so an event arriving
                // during this drain always schedules another pass.
                queued.store(false, Ordering::SeqCst);
                let paths = paths
                    .lock()
                    .map(|mut paths| paths.drain().collect())
                    .unwrap_or_default();
                let full = full_scan.swap(false, Ordering::SeqCst);
                let _ = requests.send(if full {
                    Request::Refresh
                } else {
                    Request::RefreshPaths(paths)
                });
            });
        }
    });
    let result = watcher.and_then(|mut watcher| {
        watcher.watch(store.directory(), notify::RecursiveMode::Recursive)?;
        for parent in store.extra_watch_directories() {
            watcher.watch(&parent, notify::RecursiveMode::NonRecursive)?;
        }
        Ok(watcher)
    });
    match result {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            notices.raise(format!("File watching could not start: {error}. Refresh the folder to check external edits."));
            None
        }
    }
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
    fn composition_candidates_never_enter_saved_snapshots() {
        use markraft_core::{
            EditorState, EditorStateConfig, Selection, committed_document, composition,
            finish_composition, update_composition,
        };

        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        let document = doc::from_markdown("hello");
        let mut state = EditorState::create(
            EditorStateConfig::new(doc::schema().clone())
                .doc(document)
                .selection(Selection::text(1, 6))
                .extensions(composition()),
        )
        .unwrap();
        for candidate in ["n", "ni", "你"] {
            let spec = update_composition(&state, candidate, candidate.chars().count()).unwrap();
            state = state.update([spec]).unwrap().state().clone();
            library.set_document(&id, committed_document(&state).clone());
            persistence.flush(library.clone()).unwrap();
            assert!(only_note(directory.path()).1.ends_with("hello\n"));
        }

        state = state
            .update([finish_composition()])
            .unwrap()
            .state()
            .clone();
        library.set_document(&id, committed_document(&state).clone());
        persistence.flush(library).unwrap();
        assert!(only_note(directory.path()).1.ends_with("你\n"));
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
                .ends_with("Title\n\nFinal é\n")
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
            notices: Notices::default(),
            _watcher: std::sync::Mutex::new(None),
            watch_root: Default::default(),
            extra_watches: Default::default(),
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

        // Paths are refreshed as the watcher names them, so one write may be
        // reported more than once; wait for both files rather than for two events.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut texts = Vec::new();
        let wanted = ["From another editor", "Dropped in"];
        while !wanted
            .iter()
            .all(|text| texts.contains(&(*text).to_owned()))
            && std::time::Instant::now() < deadline
        {
            for event in persistence.poll() {
                if let Event::External(changes) = event {
                    texts.extend(changes.iter().map(|change| match change {
                        External::Updated { note, .. } => doc::plain_text(&note.document),
                        External::Removed(_) => panic!("nothing was removed"),
                    }));
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            texts.contains(&"From another editor".to_owned()),
            "{texts:?}"
        );
        assert!(texts.contains(&"Dropped in".to_owned()), "{texts:?}");

        // A snapshot taken before the change must neither undo it nor claim a successful save.
        library.set_document(&id, doc::from_markdown("Stale local edit"));
        assert!(persistence.flush(library.clone()).is_err());
        assert!(folder_text(directory.path()).contains("From another editor"));
        assert!(!folder_text(directory.path()).contains("Stale local edit"));
        assert_eq!(persistence.conflicts().unwrap(), vec![id.clone()]);
        library
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .unwrap()
            .conflicted = true;
        persistence.flush(library.clone()).unwrap();
        let local = library.note(&id).unwrap().clone();
        persistence.review_conflict(local.clone()).unwrap();
        let disk = persistence.resolve_conflict(local).unwrap().unwrap();
        library.adopt(disk);
        library.set_document(&id, doc::from_markdown("From another editor, continued"));
        persistence.flush(library).unwrap();
        assert!(folder_text(directory.path()).contains("From another editor, continued"));
        assert!(!folder_text(directory.path()).contains("Stale local edit"));
    }

    /// Every note in the folder, concatenated.
    fn folder_text(directory: &std::path::Path) -> String {
        std::fs::read_dir(directory.join("notes"))
            .unwrap()
            .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
            .collect()
    }
}
