//! One worker owns the notes folder: it preserves save ordering, and it watches the
//! folder so that changes made by other programs reach the application.
use crate::fs::StoreError;
use crate::{
    storage::{Library, Note, Notices, Preferences},
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

/// Said when outside changes will not be noticed as they happen; the details
/// go to the log.
const WATCH_FAILED: &str = "Couldn't watch for outside changes.";
/// Said when a look for outside changes failed; the details go to the log.
const CHECK_FAILED: &str = "Couldn't check for outside changes.";

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// A completed snapshot attempt. Recovery can succeed without a Markdown file, so
/// the result and file metadata travel together.
#[derive(Debug)]
pub struct Saved {
    pub revision: u64,
    pub result: Result<(), StoreError>,
    pub paths: Vec<(String, std::path::PathBuf)>,
    /// Notes whose local edits were kept as a conflicted copy because disk won.
    pub conflicts: Vec<String>,
    /// Where this save's deletions landed in the Trash, when the platform said.
    pub trashed: Vec<std::path::PathBuf>,
}
pub enum Event {
    Saved(Saved),
    /// Other programs changed these notes. Snapshots are not written for them until
    /// the application calls [`Persistence::acknowledge`].
    External(Vec<External>),
}
enum Request {
    Save(u64, Library, Preferences),
    Recover(Note, Sender<Result<(), StoreError>>),
    Markdown(Note, Sender<Result<String, StoreError>>),
    OpenFile(std::path::PathBuf, Sender<Result<Note, StoreError>>),
    Rename(
        String,
        String,
        Sender<Result<std::path::PathBuf, StoreError>>,
    ),
    Flush(u64, Library, Preferences, Sender<Saved>),
    Reload(Sender<Result<Library, StoreError>>),
    Refresh,
    RefreshPaths(Vec<std::path::PathBuf>),
    Acknowledge(Vec<String>),
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
    /// Start saving `store`'s notes on a thread of their own, spelling new
    /// Markdown in `house`'s style.
    pub fn new(mut store: Store, house: markraft_commonmark::HouseStyleHandle) -> Self {
        store.set_house(house);
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
                    Request::Markdown(note, response) => {
                        let _ = response.send(store.markdown(&note));
                    }
                    Request::Recover(note, response) => {
                        let _ = response.send(store.recover(&note));
                    }
                    Request::OpenFile(path, response) => {
                        let _ = response.send(store.add_file(path));
                    }
                    Request::Rename(id, name, response) => {
                        let _ = response.send(store.rename(&id, &name));
                    }
                    Request::Save(revision, library, preferences) => {
                        let saved = save_snapshot(&mut store, revision, &library, &preferences);
                        let _ = outgoing.send(Event::Saved(saved));
                    }
                    Request::Flush(revision, library, preferences, response) => {
                        let _ = response.send(save_snapshot(
                            &mut store,
                            revision,
                            &library,
                            &preferences,
                        ));
                    }
                    Request::Reload(response) => {
                        let _ = response.send(store.reload());
                    }
                    Request::RefreshPaths(paths) => match store.refresh_paths(&paths) {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => {
                            log::warn!("refreshing changed paths failed: {error}");
                            store.notices().raise(CHECK_FAILED.to_owned());
                        }
                        _ => {}
                    },
                    Request::Refresh => match store.refresh() {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => {
                            log::warn!("refreshing the folder failed: {error}");
                            store.notices().raise(CHECK_FAILED.to_owned());
                        }
                        _ => {}
                    },
                    Request::Acknowledge(ids) => store.acknowledge(&ids),
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
                Err(error) => {
                    log::warn!("{} could not be watched: {error}", parent.display());
                    self.notices.raise(WATCH_FAILED.to_owned());
                }
            }
        }
    }
    pub fn open_file(&self, path: std::path::PathBuf) -> Result<Note, StoreError> {
        self.watch_file(&path);
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::OpenFile(path, tx))
            .map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
    }
    /// Rename a note's file where it is, answering with the path it has now.
    pub fn rename(&self, id: String, name: String) -> Result<std::path::PathBuf, StoreError> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Rename(id, name, tx))
            .map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
    }
    pub fn markdown(&self, note: Note) -> Result<String, StoreError> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Markdown(note, tx))
            .map_err(|_| stopped())?;
        receive(rx, REPLY_TIMEOUT)?
    }
    pub fn recover(&self, note: Note) -> Result<(), StoreError> {
        let (tx, rx) = mpsc::channel();
        self.requests
            .send(Request::Recover(note, tx))
            .map_err(|_| stopped())?;
        rx.recv().map_err(|_| stopped())?
    }
    /// Hand a snapshot to the worker. The write's outcome arrives later through
    /// [`Self::poll`], tagged with `revision`.
    pub fn save(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
    ) -> Result<(), StoreError> {
        self.requests
            .send(Request::Save(revision, library, preferences))
            .map_err(|_| {
                StoreError::Worker(
                    "Saving stopped working. Copy your note (⇧⌘C), then quit and reopen Markraft."
                        .into(),
                )
            })
    }
    /// Everything the notes folder gave the user to read, once there is somewhere
    /// to show it. Empty after it has been taken.
    pub fn notices(&self) -> Vec<String> {
        self.notices.take()
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
    pub fn reload(&self) -> Result<Library, StoreError> {
        let (response, result) = mpsc::channel();
        self.requests.send(Request::Reload(response)).map_err(|_| {
            StoreError::Worker(
                "Markraft can no longer reach your notes folder. Copy your note (⇧⌘C), \
                 then quit and reopen Markraft."
                    .into(),
            )
        })?;
        // Do not time out and leave an invisible baseline change queued: the UI must
        // receive the adopted library before any later local snapshot can be saved.
        result.recv().map_err(|_| {
            StoreError::Worker(
                "Markraft stopped reading your notes folder before it had finished. \
                 Copy your note (⇧⌘C), then quit and reopen Markraft."
                    .into(),
            )
        })?
    }
    /// A queue barrier: all earlier requests finish before this latest snapshot is saved.
    /// The outer result confirms receipt, not whether writing succeeded; callers must
    /// apply paths even when `Saved::result` reports a partial failure.
    /// A timeout leaves the request queued; callers retain unsaved state until a later
    /// snapshot confirms it. No result queries are needed after this call.
    pub fn flush(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
    ) -> Result<Saved, StoreError> {
        self.flush_with_timeout(revision, library, preferences, REPLY_TIMEOUT)
    }
    fn flush_with_timeout(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
        timeout: Duration,
    ) -> Result<Saved, StoreError> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Flush(revision, library, preferences, response))
            .map_err(|_| {
                StoreError::Worker(
                    "Saving stopped working. Copy your note (⇧⌘C), then quit and reopen Markraft."
                        .into(),
                )
            })?;
        let saved = receive(result, timeout)?;
        for (_, path) in &saved.paths {
            self.watch_file(path);
        }
        Ok(saved)
    }
}

/// Capture metadata after every attempt, including errors after some notes were written.
fn save_snapshot(
    store: &mut Store,
    revision: u64,
    library: &Library,
    preferences: &Preferences,
) -> Saved {
    let result = store.save(library, preferences);
    Saved {
        revision,
        result,
        paths: store.paths(),
        conflicts: store.conflicts(),
        trashed: store.trashed(),
    }
}

/// The worker's channel is closed: the thread is gone.
fn stopped() -> StoreError {
    StoreError::Worker("The save worker stopped".into())
}

fn receive<T>(receiver: Receiver<T>, timeout: Duration) -> Result<T, StoreError> {
    receiver.recv_timeout(timeout).map_err(|error| match error {
        RecvTimeoutError::Timeout => StoreError::Worker(
            "The notes folder is taking too long to respond. The request may still complete; \
             your note remains open. Try again."
                .into(),
        ),
        RecvTimeoutError::Disconnected => StoreError::Worker(
            "The save worker stopped before responding. Copy your note (⇧⌘C), \
             then quit and reopen Markraft."
                .into(),
        ),
    })
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
                log::warn!("file watching failed: {error}");
                callback_notices.raise(WATCH_FAILED.to_owned());
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
            log::warn!("file watching could not start: {error}");
            notices.raise(WATCH_FAILED.to_owned());
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
        let notes = directory.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        crate::vault::open_reading_settings(notes, directory.join("settings.json")).unwrap()
    }

    /// The only Markdown file in the notes folder, and its text.
    fn only_note(directory: &std::path::Path) -> (std::path::PathBuf, String) {
        let mut files: Vec<_> = std::fs::read_dir(directory.join("notes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let path = std::fs::canonicalize(files.remove(0)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        (path, text)
    }

    #[test]
    fn composition_candidates_never_enter_saved_snapshots() {
        use markraft_core::{
            EditorState, EditorStateConfig, Selection,
            composition::{
                committed_document, composition, finish_composition, update_composition,
            },
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
            persistence
                .flush(0, library.clone(), Preferences::default())
                .unwrap()
                .result
                .unwrap();
            assert!(only_note(directory.path()).1.ends_with("hello\n"));
        }

        state = state
            .update([finish_composition()])
            .unwrap()
            .state()
            .clone();
        library.set_document(&id, committed_document(&state).clone());
        persistence
            .flush(0, library, Preferences::default())
            .unwrap()
            .result
            .unwrap();
        assert!(only_note(directory.path()).1.ends_with("你\n"));
    }

    #[test]
    fn flush_saves_the_latest_snapshot_after_queued_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Title\n\nFirst 中文"));
        persistence
            .save(1, library.clone(), Preferences::default())
            .unwrap();
        library.set_document(&id, doc::from_markdown("Title\n\nSecond 👩🏽‍💻"));
        persistence
            .save(2, library.clone(), Preferences::default())
            .unwrap();
        library.set_document(&id, doc::from_markdown("Title\n\nFinal é"));
        let receipt = persistence
            .flush(3, library.clone(), Preferences::default())
            .unwrap();
        assert_eq!(receipt.revision, 3);
        receipt.result.unwrap();
        assert_eq!(receipt.paths, vec![(id, only_note(directory.path()).0)]);
        assert!(receipt.conflicts.is_empty());
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
    fn a_new_note_is_filed_under_its_title_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Meeting notes for Q3"));
        let filed = persistence
            .flush(1, library, Preferences::default())
            .unwrap();
        assert_eq!(filed.revision, 1);
        filed.result.unwrap();
        assert!(filed.conflicts.is_empty());
        let (path, text) = only_note(directory.path());
        assert_eq!(filed.paths, vec![(id, path.clone())]);
        assert!(path.ends_with("Meeting notes for Q3.md"), "{path:?}");
        assert_eq!(text, "Meeting notes for Q3\n");
    }

    #[test]
    fn a_note_changed_by_another_program_keeps_disk_and_a_conflicted_copy() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Shared"));
        persistence
            .save(10, library.clone(), Preferences::default())
            .unwrap();
        persistence
            .flush(0, library.clone(), Preferences::default())
            .unwrap()
            .result
            .unwrap();
        let (path, _) = only_note(directory.path());
        std::fs::write(&path, b"external content").unwrap();
        library.set_document(&id, doc::from_markdown("Shared, edited here"));
        library.new_note(doc::from_markdown("Keep this local work"));
        persistence
            .save(11, library.clone(), Preferences::default())
            .unwrap();
        let receipt = persistence
            .flush(12, library, Preferences::default())
            .unwrap();
        assert_eq!(receipt.revision, 12);
        assert!(receipt.result.is_err());
        assert!(receipt.conflicts.contains(&id));
        assert_eq!(std::fs::read(&path).unwrap(), b"external content");
        let saved = std::fs::read_dir(directory.path().join("notes"))
            .unwrap()
            .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
            .any(|text| text.ends_with("Keep this local work\n"));
        assert!(saved);
        // Local edits for the conflicted note landed beside the file, not on disk.
        // A queued save and the flush behind it both see this conflict, and it is
        // still one copy: the second would say exactly what the first does.
        let copies: Vec<_> = std::fs::read_dir(directory.path().join("notes"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("conflicted copy"))
            })
            .collect();
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert!(copies.iter().any(|copy| {
            std::fs::read_to_string(copy)
                .unwrap()
                .contains("Shared, edited here")
        }));
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
        assert!(
            persistence
                .save(1, Library::default(), Preferences::default())
                .is_err()
        );
        assert!(
            persistence
                .flush(0, Library::default(), Preferences::default())
                .is_err()
        );
        assert!(persistence.reload().is_err());
    }

    #[test]
    fn timed_out_flush_remains_queued_and_can_still_write() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, mut library) = open(directory.path());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Still queued"));
        let (requests, incoming) = mpsc::channel();
        let (_, events) = mpsc::channel();
        let persistence = Persistence {
            requests,
            events,
            notices: Notices::default(),
            _watcher: std::sync::Mutex::new(None),
            watch_root: Default::default(),
            extra_watches: Default::default(),
        };
        let error = persistence
            .flush_with_timeout(42, library, Preferences::default(), Duration::ZERO)
            .unwrap_err();
        assert!(error.to_string().contains("may still complete"));
        let Request::Flush(revision, library, _, response) = incoming.try_recv().unwrap() else {
            panic!("the timed-out flush must remain queued");
        };
        let saved = save_snapshot(&mut store, revision, &library, &Preferences::default());
        assert_eq!(saved.revision, 42);
        assert!(saved.result.is_ok());
        assert!(response.send(saved).is_err());
        assert_eq!(only_note(directory.path()).1, "Still queued\n");
    }

    #[test]
    fn disconnected_response_is_not_reported_as_a_timeout_or_success() {
        let (response, result) = mpsc::channel::<Saved>();
        drop(response);
        let error = receive(result, Duration::ZERO).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("stopped before responding"));
        assert!(!error.contains("may still complete"));
    }

    #[test]
    fn reload_is_a_barrier_after_disk_wins_and_new_edits_can_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut local) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = local.active_id.clone();
        local.set_document(&id, doc::from_markdown("Original"));
        persistence
            .flush(0, local.clone(), Preferences::default())
            .unwrap()
            .result
            .unwrap();
        let (path, text) = only_note(directory.path());
        std::fs::write(&path, text.replace("Original", "External text")).unwrap();
        local.set_document(&id, doc::from_markdown("Discard this after confirmation"));
        persistence.save(1, local, Preferences::default()).unwrap();
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
        persistence
            .save(2, reloaded.clone(), Preferences::default())
            .unwrap();
        persistence
            .flush(0, reloaded, Preferences::default())
            .unwrap()
            .result
            .unwrap();
        assert!(saves(&persistence)[0].result.is_ok());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("External text, continued\n")
        );
        assert!(
            std::fs::read_dir(directory.path().join("notes"))
                .unwrap()
                .filter_map(|entry| entry.ok())
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .contains("conflicted copy")
                })
        );
    }

    #[test]
    fn changes_by_other_programs_are_reported_and_held_back_until_acknowledged() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::new(store, Default::default());
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Original"));
        persistence
            .flush(0, library.clone(), Preferences::default())
            .unwrap()
            .result
            .unwrap();
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

        // A snapshot taken before acknowledging must neither undo the disk version
        // nor claim a successful save of the stale local edit — that edit is kept
        // as a conflicted copy beside the file.
        library.set_document(&id, doc::from_markdown("Stale local edit"));
        let failed = persistence
            .flush(2, library.clone(), Preferences::default())
            .unwrap();
        assert!(failed.result.is_err());
        assert!(failed.conflicts.contains(&id));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("From another editor")
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("Stale local edit")
        );
        assert!(folder_text(directory.path()).contains("Stale local edit"));
        persistence.acknowledge(vec![id.clone()]);
        // After acknowledging, adopt the disk note and continue editing.
        library
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .unwrap()
            .document = doc::from_markdown("From another editor");
        library.set_document(&id, doc::from_markdown("From another editor, continued"));
        persistence
            .flush(0, library, Preferences::default())
            .unwrap()
            .result
            .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("From another editor, continued")
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("Stale local edit")
        );
    }

    /// Every note in the folder, concatenated.
    fn folder_text(directory: &std::path::Path) -> String {
        std::fs::read_dir(directory.join("notes"))
            .unwrap()
            .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
            .collect()
    }
}
