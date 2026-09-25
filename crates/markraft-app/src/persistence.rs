//! One worker owns the notes folder: it preserves save ordering, and it watches the
//! folder so that changes made by other programs reach the application.
use crate::fs::StoreError;
use crate::{
    storage::{Library, Note, Notices, Preferences},
    vault::{External, Store},
};
use futures_channel::{
    mpsc::{UnboundedReceiver, unbounded},
    oneshot,
};
use notify::Watcher;
#[cfg(test)]
use std::sync::mpsc::RecvTimeoutError;
use std::{future::Future, pin::Pin};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

/// Said when outside changes will not be noticed as they happen; the details
/// go to the log.
const WATCH_FAILED: &str = "Couldn't watch for outside changes.";
/// Said when a look for outside changes failed; the details go to the log.
const CHECK_FAILED: &str = "Couldn't check for outside changes.";

#[cfg(test)]
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// A completed snapshot attempt. Recovery can succeed without a Markdown file, so
/// the result and file metadata travel together.
#[derive(Debug)]
pub struct Saved {
    pub revision: u64,
    pub changes: Vec<(String, u64)>,
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
    /// the application calls [`Persistence::acknowledge_changes`].
    External(Vec<External>),
}
/// A response can be awaited on the UI executor without blocking it. Requests are
/// enqueued before this future is returned, preserving queue barriers.
pub type Pending<T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'static>>;
enum Reply<T> {
    #[cfg(test)]
    Blocking(Sender<T>),
    Async(oneshot::Sender<T>),
}
impl<T> Reply<T> {
    fn send(self, value: T) -> Result<(), T> {
        match self {
            #[cfg(test)]
            Self::Blocking(sender) => sender.send(value).map_err(|error| error.0),
            Self::Async(sender) => sender.send(value),
        }
    }
}
enum Request {
    Shutdown,
    Save(u64, Library, Preferences, Instant),
    Recover(Note, Reply<Result<(), StoreError>>),
    Markdown(Note, Reply<Result<String, StoreError>>),
    OpenFile(std::path::PathBuf, Reply<Result<Note, StoreError>>),
    Rename(
        String,
        String,
        Reply<Result<std::path::PathBuf, StoreError>>,
    ),
    Flush(u64, Library, Preferences, Reply<Saved>, Instant),
    Reload(Reply<Result<Library, StoreError>>),
    Refresh,
    RefreshPaths(Vec<std::path::PathBuf>),
    AcknowledgeChanges(Vec<External>),
    #[cfg(test)]
    Acknowledge(Vec<String>),
}
pub struct Persistence {
    requests: Sender<Request>,
    events: Receiver<Event>,
    wake: Option<UnboundedReceiver<()>>,
    /// Shared with the store the worker owns, so what it notices on its own
    /// thread still reaches the interface.
    notices: Notices,
    watches: Sender<Option<std::path::PathBuf>>,
    sources: Arc<crate::vault::Sources>,
    house: markraft_commonmark::HouseStyleHandle,
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
        let (pulse, wake) = unbounded();
        let notices = store.notices();
        let sources = store.source_cache();
        let house = store.house();
        let (watches, watch_requests) = mpsc::channel::<Option<std::path::PathBuf>>();
        if watching {
            let directory = store.directory().to_owned();
            let extra = store.extra_watch_directories();
            let requests = requests.clone();
            let notices = notices.clone();
            let pulse = pulse.clone();
            // FSEvents registration can wait on a system service. It must never
            // delay opening the editor or processing saves on the store worker.
            std::thread::spawn(move || {
                let mut watcher = watch(&directory, &extra, notices.clone(), requests);
                let _ = pulse.unbounded_send(());
                let mut watched: std::collections::HashSet<_> = extra.into_iter().collect();
                for path in watch_requests {
                    let Some(path) = path else {
                        break;
                    };
                    if path.starts_with(&directory) {
                        continue;
                    }
                    let Some(parent) = path.parent() else {
                        continue;
                    };
                    if watched.contains(parent) {
                        continue;
                    }
                    if let Some(watcher) = watcher.as_mut() {
                        match watcher.watch(parent, notify::RecursiveMode::NonRecursive) {
                            Ok(()) => {
                                watched.insert(parent.to_owned());
                            }
                            Err(error) => {
                                log::warn!("{} could not be watched: {error}", parent.display());
                                notices.raise(WATCH_FAILED.to_owned());
                            }
                        }
                    }
                }
            });
        }
        let watch_saved = watches.clone();
        std::thread::spawn(move || {
            let mut deferred = None;
            while let Some(request) = next_request(&incoming, &mut deferred) {
                match request {
                    Request::Shutdown => break,
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
                    Request::Save(revision, library, preferences, queued_at) => {
                        log::debug!(
                            "save_queue revision={revision} wait_us={}",
                            queued_at.elapsed().as_micros()
                        );
                        let saved = save_snapshot(&mut store, revision, &library, &preferences);
                        for (_, path) in &saved.paths {
                            let _ = watch_saved.send(Some(path.clone()));
                        }
                        let _ = outgoing.send(Event::Saved(saved));
                    }
                    Request::Flush(revision, library, preferences, response, queued_at) => {
                        log::debug!(
                            "flush_queue revision={revision} wait_us={}",
                            queued_at.elapsed().as_micros()
                        );
                        let saved = save_snapshot(&mut store, revision, &library, &preferences);
                        for (_, path) in &saved.paths {
                            let _ = watch_saved.send(Some(path.clone()));
                        }
                        let _ = response.send(saved);
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
                    Request::AcknowledgeChanges(changes) => store.acknowledge_changes(&changes),
                    #[cfg(test)]
                    Request::Acknowledge(ids) => store.acknowledge(&ids),
                }
                let _ = pulse.unbounded_send(());
            }
        });
        Self {
            requests,
            events,
            wake: Some(wake),
            notices,
            watches,
            sources,
            house,
        }
    }
    pub fn take_wake(&mut self) -> Option<UnboundedReceiver<()>> {
        self.wake.take()
    }
    pub fn refresh(&self) {
        let _ = self.requests.send(Request::Refresh);
    }
    #[cfg(test)]
    pub fn new_unwatched(mut store: Store, house: markraft_commonmark::HouseStyleHandle) -> Self {
        store.set_house(house);
        Self::start(store, false)
    }
    fn watch_file(&self, path: &std::path::Path) {
        let _ = self.watches.send(Some(path.to_owned()));
    }
    fn request_async<T: Send + 'static>(
        &self,
        request: impl FnOnce(Reply<T>) -> Request,
    ) -> Pending<T> {
        let (sender, receiver) = oneshot::channel();
        let sent = self
            .requests
            .send(request(Reply::Async(sender)))
            .map_err(|_| stopped());
        Box::pin(async move {
            sent?;
            receiver.await.map_err(|_| stopped())
        })
    }
    pub fn open_file_async(&self, path: std::path::PathBuf) -> Pending<Note> {
        self.watch_file(&path);
        let response = self.request_async(|reply| Request::OpenFile(path, reply));
        Box::pin(async move { response.await? })
    }
    pub fn rename_async(&self, id: String, name: String) -> Pending<std::path::PathBuf> {
        let response = self.request_async(|reply| Request::Rename(id, name, reply));
        Box::pin(async move { response.await? })
    }
    pub fn recover_async(&self, note: Note) -> Pending<()> {
        let response = self.request_async(|reply| Request::Recover(note, reply));
        Box::pin(async move { response.await? })
    }
    pub fn markdown_async(&self, note: Note) -> Pending<String> {
        let response = self.request_async(|reply| Request::Markdown(note, reply));
        Box::pin(async move { response.await? })
    }
    pub fn reload_async(&self) -> Pending<Library> {
        let response = self.request_async(Request::Reload);
        Box::pin(async move { response.await? })
    }
    pub fn flush_async(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
    ) -> Pending<Saved> {
        self.request_async(|reply| {
            Request::Flush(revision, library, preferences, reply, Instant::now())
        })
    }
    /// Local baseline rendering; no worker round-trip or cache lock is held while
    /// the source track renders the document.
    pub fn markdown(&self, note: Note) -> Result<String, StoreError> {
        match self.sources.source(&note)? {
            Some(track) => track
                .save(crate::doc::schema(), &note.document)
                .map_err(|error| error.to_string().into()),
            None => Ok(format!(
                "{}\n",
                crate::doc::to_markdown_in(&note.document, &self.house)
            )),
        }
    }
    pub fn source(
        &self,
        note: Note,
    ) -> Result<Option<Arc<markraft_commonmark::SourceTrack>>, StoreError> {
        self.sources.source(&note)
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
            .send(Request::Save(
                revision,
                library,
                preferences,
                Instant::now(),
            ))
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
    pub fn is_current_external(&self, change: &External) -> bool {
        self.sources.is_current_external(change)
    }
    pub fn acknowledge_changes(&self, changes: Vec<External>) {
        let _ = self.requests.send(Request::AcknowledgeChanges(changes));
    }
    #[cfg(test)]
    pub fn acknowledge(&self, ids: Vec<String>) {
        let _ = self.requests.send(Request::Acknowledge(ids));
    }
    /// Reload after all earlier save requests finish. The caller must confirm discarding
    /// local changes and invalidate their revision acknowledgments before adopting the result.
    #[cfg(test)]
    pub fn reload(&self) -> Result<Library, StoreError> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Reload(Reply::Blocking(response)))
            .map_err(|_| {
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
    #[cfg(test)]
    pub fn flush(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
    ) -> Result<Saved, StoreError> {
        self.flush_with_timeout(revision, library, preferences, REPLY_TIMEOUT)
    }
    #[cfg(test)]
    fn flush_with_timeout(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
        timeout: Duration,
    ) -> Result<Saved, StoreError> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Flush(
                revision,
                library,
                preferences,
                Reply::Blocking(response),
                Instant::now(),
            ))
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

impl Drop for Persistence {
    fn drop(&mut self) {
        // The watcher and store both own senders, so channel disconnection alone
        // cannot terminate them. Explicit shutdown also releases the folder lock
        // if FSEvents is still waiting for its system service.
        let _ = self.watches.send(None);
        let _ = self.requests.send(Request::Shutdown);
    }
}

/// Coalesce only adjacent autosave snapshots. Any other command is a barrier:
/// in particular refresh/acknowledgement and explicit flush retain their order.
fn next_request(incoming: &Receiver<Request>, deferred: &mut Option<Request>) -> Option<Request> {
    let mut request = deferred.take().or_else(|| incoming.recv().ok())?;
    if matches!(request, Request::Save(..)) {
        while let Ok(next) = incoming.try_recv() {
            match next {
                Request::Save(..) => request = next,
                barrier => {
                    *deferred = Some(barrier);
                    break;
                }
            }
        }
    }
    Some(request)
}

/// Capture metadata after every attempt, including errors after some notes were written.
fn save_snapshot(
    store: &mut Store,
    revision: u64,
    library: &Library,
    preferences: &Preferences,
) -> Saved {
    let started = std::time::Instant::now();
    let result = store.save(library, preferences);
    log::debug!(
        "save revision={revision} dirty={} notes={} elapsed_ms={} success={}",
        library.changes.len(),
        library.notes.len(),
        started.elapsed().as_millis(),
        result.is_ok()
    );
    let changes = if result.is_ok() {
        library
            .changes
            .iter()
            .map(|(id, generation)| (id.clone(), *generation))
            .collect()
    } else {
        Vec::new()
    };
    Saved {
        revision,
        changes,
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

#[cfg(test)]
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
fn watch(
    directory: &std::path::Path,
    extra: &[std::path::PathBuf],
    notices: Notices,
    requests: Sender<Request>,
) -> Option<notify::RecommendedWatcher> {
    let queued = Arc::new(AtomicBool::new(false));
    let paths = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let full_scan = Arc::new(AtomicBool::new(false));
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
        watcher.watch(directory, notify::RecursiveMode::Recursive)?;
        for parent in extra {
            watcher.watch(parent, notify::RecursiveMode::NonRecursive)?;
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
    fn adjacent_autosaves_coalesce_but_refresh_and_flush_are_barriers() {
        let (sender, receiver) = mpsc::channel();
        let snapshot = |revision| {
            Request::Save(
                revision,
                Library::default(),
                Preferences::default(),
                Instant::now(),
            )
        };
        sender.send(snapshot(1)).unwrap();
        sender.send(snapshot(2)).unwrap();
        sender.send(Request::Refresh).unwrap();
        sender.send(snapshot(3)).unwrap();
        let (reply, _) = mpsc::channel();
        sender
            .send(Request::Flush(
                4,
                Library::default(),
                Preferences::default(),
                Reply::Blocking(reply),
                Instant::now(),
            ))
            .unwrap();
        sender.send(snapshot(5)).unwrap();
        drop(sender);
        let mut deferred = None;
        assert!(matches!(
            next_request(&receiver, &mut deferred),
            Some(Request::Save(2, ..))
        ));
        assert!(matches!(
            next_request(&receiver, &mut deferred),
            Some(Request::Refresh)
        ));
        assert!(matches!(
            next_request(&receiver, &mut deferred),
            Some(Request::Save(3, ..))
        ));
        assert!(matches!(
            next_request(&receiver, &mut deferred),
            Some(Request::Flush(4, ..))
        ));
        assert!(matches!(
            next_request(&receiver, &mut deferred),
            Some(Request::Save(5, ..))
        ));
        assert!(next_request(&receiver, &mut deferred).is_none());
    }

    #[test]
    fn async_flush_is_enqueued_before_its_future_is_polled() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(store, false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Before barrier"));
        let unpolled = persistence.flush_async(1, library.clone(), Preferences::default());
        let reloaded = persistence.reload().unwrap();
        assert_eq!(
            doc::plain_text(&reloaded.note(&id).unwrap().document),
            "Before barrier"
        );
        drop(unpolled);
    }

    #[test]
    #[ignore = "requires a responsive native FSEvents service"]
    fn native_watcher_reports_a_file_added_by_another_program() {
        let directory = tempfile::tempdir().unwrap();
        let (store, _) = open(directory.path());
        let persistence = Persistence::new(store, Default::default());
        // Registration is asynchronous; write repeatedly while waiting so the
        // test also works when startup finishes after the first write.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            std::fs::write(directory.path().join("notes/watched.md"), "Watched").unwrap();
            std::thread::sleep(Duration::from_millis(200));
            if persistence.poll().iter().any(|event| matches!(event, Event::External(changes) if changes.iter().any(|change| matches!(change, External::Updated { note, .. } if doc::plain_text(&note.document) == "Watched")))) {
                return;
            }
        }
        panic!("native watcher did not deliver the external addition");
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
        let revisions = acknowledgments
            .iter()
            .map(|saved| saved.revision)
            .collect::<Vec<_>>();
        assert!(revisions == [1, 2] || revisions == [2], "{revisions:?}");
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
            wake: None,
            notices: Notices::default(),
            watches: mpsc::channel().0,
            sources: Arc::default(),
            house: Default::default(),
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
            wake: None,
            notices: Notices::default(),
            watches: mpsc::channel().0,
            sources: Arc::default(),
            house: Default::default(),
        };
        let error = persistence
            .flush_with_timeout(42, library, Preferences::default(), Duration::ZERO)
            .unwrap_err();
        assert!(error.to_string().contains("may still complete"));
        let Request::Flush(revision, library, _, response, _) = incoming.try_recv().unwrap() else {
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
        let persistence = Persistence::start(store, false);
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
        persistence.refresh();

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
        assert!(directory.path().join("notes/dropped.md").exists());
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
