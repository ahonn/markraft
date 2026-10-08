//! One worker owns the notes folder: it preserves save ordering, and it watches the
//! folder so that changes made by other programs reach the application.
use crate::engine::WorkerBackend;
use crate::fs::StoreError;
use crate::locale::Message;
use crate::{Asset, AssetId, BackendCapabilities, NewRecord, NoteSaveOutcome, StorageRevision};
use crate::{
    storage::{Library, Note, Notices, Preferences},
    vault::{External, Store},
};
use futures_channel::{
    mpsc::{UnboundedReceiver, unbounded},
    oneshot,
};
use futures_util::FutureExt;
use notify::Watcher;
use std::collections::HashMap;
#[cfg(any(test, feature = "test-support"))]
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
const WATCH_FAILED: &str = "error.watch-failed";
/// Said when a look for outside changes failed; the details go to the log.
const CHECK_FAILED: &str = "error.check-failed";

#[cfg(any(test, feature = "test-support"))]
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
    /// Notes this save was asked to delete and left in place; see [`Store::kept`].
    pub kept: Vec<String>,
    pub outcomes: Vec<NoteSaveOutcome>,
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
    #[cfg(any(test, feature = "test-support"))]
    Blocking(Sender<T>),
    Async(oneshot::Sender<T>),
}
impl<T> Reply<T> {
    fn send(self, value: T) -> Result<(), T> {
        match self {
            #[cfg(any(test, feature = "test-support"))]
            Self::Blocking(sender) => sender.send(value).map_err(|error| error.0),
            Self::Async(sender) => sender.send(value),
        }
    }
}
enum Request {
    CreateRecord(NewRecord, Reply<Result<Note, StoreError>>),
    RenameRecord(String, String, Reply<Result<Note, StoreError>>),
    ReadAsset(AssetId, Reply<Result<Asset, StoreError>>),
    WriteAsset(Asset, Reply<Result<(), StoreError>>),
    Shutdown,
    Save(u64, Library, Preferences, Instant),
    Recover(Note, Reply<Result<(), StoreError>>),
    Snapshot(
        crate::doc::PendingSnapshot,
        Reply<Result<crate::doc::DocumentSnapshot, StoreError>>,
    ),
    OpenFile(std::path::PathBuf, Reply<Result<Note, StoreError>>),
    CreateNote(NewNote, Reply<Result<Created, StoreError>>),
    Rename(
        String,
        String,
        Reply<Result<std::path::PathBuf, StoreError>>,
    ),
    Flush(u64, Library, Preferences, Reply<Saved>, Instant),
    Reload(Reply<Result<Library, StoreError>>),
    Refresh,
    RefreshPaths(Vec<std::path::PathBuf>),
    RefreshNotes(Vec<crate::NoteId>),
    AcknowledgeChanges(Vec<External>),
    #[cfg(any(test, feature = "test-support"))]
    Acknowledge(Vec<String>),
}
/// A note to make at a path in the notes folder, starting from a template when
/// there is one: `fill` turns the template's text — `None` when there is none, or it
/// could not be read — into the new note's.
pub struct NewNote {
    pub relative: std::path::PathBuf,
    pub template: Option<std::path::PathBuf>,
    pub fill: FillTemplate,
}

/// Turns a template's text, or `None`, into a new note's.
pub type FillTemplate = Box<dyn FnOnce(Option<&str>) -> String + Send>;

/// The note [`NewNote`] asked for, and whether its template was missing.
pub struct Created {
    pub note: Note,
    pub template_missing: bool,
}

impl NewNote {
    fn create(self, store: &mut dyn WorkerBackend) -> Result<Created, StoreError> {
        let template = self
            .template
            .as_deref()
            .map(|template| store.read_text(template));
        let template_missing = matches!(template, Some(None));
        let text = (self.fill)(template.flatten().as_deref());
        let note = store.create_note(&self.relative, &text)?;
        Ok(Created {
            note,
            template_missing,
        })
    }
}

pub struct Persistence {
    storage_worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    watcher_cancelled: Arc<AtomicBool>,
    shutdown: std::sync::Mutex<Option<futures_util::future::Shared<Pending<()>>>>,
    requests: Sender<Request>,
    events: Receiver<Event>,
    wake: Option<UnboundedReceiver<()>>,
    /// Shared with the store the worker owns, so what it notices on its own
    /// thread still reaches the interface.
    notices: Notices,
    watches: Sender<Option<std::path::PathBuf>>,
    sources: Arc<crate::vault::Sources>,
    house: markraft_commonmark::HouseStyleHandle,
    capabilities: BackendCapabilities,
    persisted: Arc<std::sync::Mutex<HashMap<String, StorageRevision>>>,
}
impl Persistence {
    /// Start saving `store`'s notes on a thread of their own, spelling new
    /// Markdown in `house`'s style.
    pub fn new(mut store: Store, house: markraft_commonmark::HouseStyleHandle) -> Self {
        store.set_house(house);
        Self::start(Box::new(store), true)
    }
    pub(crate) fn from_worker(store: Box<dyn WorkerBackend>) -> Self {
        Self::start(store, true)
    }
    fn start(mut store: Box<dyn WorkerBackend>, watching: bool) -> Self {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        let (pulse, wake) = unbounded();
        let notices = store.notices();
        let capabilities = store.capabilities();
        let persisted = Arc::new(std::sync::Mutex::new(
            store.persisted().into_iter().collect::<HashMap<_, _>>(),
        ));
        let persisted_worker = persisted.clone();
        let sources = store.sources();
        let notify_requests = requests.clone();
        store.set_change_notifier(crate::ChangeNotifier::new(move |ids| {
            let _ = notify_requests.send(match ids {
                Some(ids) => Request::RefreshNotes(ids),
                None => Request::Refresh,
            });
        }));
        let house = store.house();
        let (watches, watch_requests) = mpsc::channel::<Option<std::path::PathBuf>>();
        let watcher_cancelled = Arc::new(AtomicBool::new(false));
        if let Some(directory) = store.watch_root().filter(|_| watching) {
            let extra = store.extra_watch_directories();
            let requests = requests.clone();
            let notices = notices.clone();
            let pulse = pulse.clone();
            let cancelled = watcher_cancelled.clone();
            // FSEvents registration can wait on a system service. It must never
            // delay opening the editor or processing saves on the store worker.
            // Native registration and deregistration can both block in FSEvents
            // IPC. This thread never owns Store or its locks. Cancellation stops
            // callbacks immediately; native cleanup finishes when macOS answers.
            std::thread::spawn(move || {
                let mut watcher = watch(
                    &directory,
                    &extra,
                    notices.clone(),
                    requests,
                    cancelled.clone(),
                );
                let _ = pulse.unbounded_send(());
                let mut watched: std::collections::HashSet<_> = extra.into_iter().collect();
                for path in watch_requests {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
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
                                notices.raise(Message::new(WATCH_FAILED));
                            }
                        }
                    }
                }
            });
        }
        let watch_saved = watches.clone();
        let storage_worker = std::thread::spawn(move || {
            let mut deferred = None;
            while let Some(request) = next_request(&incoming, &mut deferred) {
                match request {
                    Request::Shutdown => break,
                    Request::CreateRecord(request, response) => {
                        let result = store.create_record(request);
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::RenameRecord(id, name, response) => {
                        let result = store.rename_record(&id, &name);
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::ReadAsset(id, response) => {
                        let _ = response.send(store.read_asset(&id));
                    }
                    Request::WriteAsset(asset, response) => {
                        let _ = response.send(store.write_asset(asset));
                    }
                    Request::Snapshot(snapshot, response) => {
                        let _ = response.send(snapshot.render());
                    }
                    Request::Recover(note, response) => {
                        let _ = response.send(store.recover(&note));
                    }
                    Request::OpenFile(path, response) => {
                        let result = store.add_file(path);
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::CreateNote(new, response) => {
                        let result = new.create(store.as_mut());
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::Rename(id, name, response) => {
                        let result = store.rename(&id, &name);
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::Save(revision, library, preferences, queued_at) => {
                        log::debug!(
                            "save_queue revision={revision} wait_us={}",
                            queued_at.elapsed().as_micros()
                        );
                        let saved = store.save(revision, &library, &preferences);
                        sync_versions(store.as_ref(), &persisted_worker);
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
                        let saved = store.save(revision, &library, &preferences);
                        sync_versions(store.as_ref(), &persisted_worker);
                        for (_, path) in &saved.paths {
                            let _ = watch_saved.send(Some(path.clone()));
                        }
                        let _ = response.send(saved);
                    }
                    Request::Reload(response) => {
                        let result = store.reload();
                        sync_versions(store.as_ref(), &persisted_worker);
                        let _ = response.send(result);
                    }
                    Request::RefreshPaths(paths) => match store.refresh_paths(&paths) {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => {
                            log::warn!("refreshing changed paths failed: {error}");
                            store.notices().raise(Message::new(CHECK_FAILED));
                        }
                        _ => {}
                    },
                    Request::RefreshNotes(ids) => match store.refresh_notes(&ids) {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => {
                            log::warn!("refreshing changed notes failed: {error}");
                            store.notices().raise(Message::new(CHECK_FAILED));
                        }
                        _ => {}
                    },
                    Request::Refresh => match store.refresh() {
                        Ok(changes) if !changes.is_empty() => {
                            let _ = outgoing.send(Event::External(changes));
                        }
                        Err(error) => {
                            log::warn!("refreshing the folder failed: {error}");
                            store.notices().raise(Message::new(CHECK_FAILED));
                        }
                        _ => {}
                    },
                    Request::AcknowledgeChanges(changes) => store.acknowledge_changes(&changes),
                    #[cfg(any(test, feature = "test-support"))]
                    Request::Acknowledge(ids) => store.acknowledge(&ids),
                }
                sync_versions(store.as_ref(), &persisted_worker);
                let _ = pulse.unbounded_send(());
            }
        });
        Self {
            storage_worker: std::sync::Mutex::new(Some(storage_worker)),
            watcher_cancelled,
            shutdown: std::sync::Mutex::new(None),
            requests,
            events,
            wake: Some(wake),
            notices,
            watches,
            sources,
            house,
            capabilities,
            persisted,
        }
    }
    /// Stop after queued work, then wait for the storage worker and its directory
    /// locks to be released. Watch callbacks are cancelled, but native watcher
    /// registration/cleanup may finish later if the OS service is unresponsive.
    /// Callers must flush first; shutdown does not invent a final document snapshot.
    pub fn shutdown_async(&self) -> Pending<()> {
        let mut completion = self.shutdown.lock().expect("shutdown completion");
        if let Some(pending) = completion.as_ref() {
            let pending = pending.clone();
            return Box::pin(pending);
        }
        self.watcher_cancelled.store(true, Ordering::Release);
        let _ = self.watches.send(None);
        let _ = self.requests.send(Request::Shutdown);
        let worker = self
            .storage_worker
            .lock()
            .expect("storage worker handle")
            .take();
        let (sender, receiver) = oneshot::channel();
        std::thread::spawn(move || {
            let mut result = Ok(());
            if let Some(worker) = worker
                && worker.join().is_err()
            {
                result = Err(stopped());
            }
            let _ = sender.send(result);
        });
        let pending: Pending<()> = Box::pin(async move { receiver.await.map_err(|_| stopped())? });
        let pending = pending.shared();
        *completion = Some(pending.clone());
        Box::pin(pending)
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        self.capabilities
    }
    pub fn house(&self) -> markraft_commonmark::HouseStyleHandle {
        self.house.clone()
    }
    pub fn storage_revision(&self, id: &str) -> Option<StorageRevision> {
        self.persisted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }
    pub fn is_persisted(&self, id: &str) -> bool {
        self.storage_revision(id).is_some()
    }
    /// Register source before accepting an edit; no storage write occurs here.
    pub fn register_source(&self, note: &Note, markdown: &str) -> Result<(), StoreError> {
        let source = markraft_commonmark::SourceDocument::parse(crate::doc::schema(), markdown)
            .map_err(|e| StoreError::Backend(crate::BackendError::Invalid(e.to_string())))?;
        self.sources.install(
            note,
            Arc::new(markraft_commonmark::SourceTrack::new(source)),
        );
        Ok(())
    }
    pub fn create_record_async(&self, request: NewRecord) -> Pending<Note> {
        let response = self.request_async(|reply| Request::CreateRecord(request, reply));
        Box::pin(async move { response.await? })
    }
    pub fn rename_note_async(&self, id: String, name: String) -> Pending<Note> {
        let response = self.request_async(|reply| Request::RenameRecord(id, name, reply));
        Box::pin(async move { response.await? })
    }
    /// A cloneable handle for background image fetchers. Calling it only queues
    /// work; awaiting the result never blocks the UI thread.
    pub fn asset_reader(&self) -> Arc<dyn Fn(AssetId) -> Pending<Asset> + Send + Sync> {
        let requests = self.requests.clone();
        Arc::new(move |id| {
            let (sender, receiver) = oneshot::channel();
            let sent = requests
                .send(Request::ReadAsset(id, Reply::Async(sender)))
                .map_err(|_| stopped());
            Box::pin(async move {
                sent?;
                receiver.await.map_err(|_| stopped())?
            })
        })
    }
    pub fn asset_writer(&self) -> Arc<dyn Fn(Asset) -> Pending<()> + Send + Sync> {
        let requests = self.requests.clone();
        Arc::new(move |asset| {
            let (sender, receiver) = oneshot::channel();
            let sent = requests
                .send(Request::WriteAsset(asset, Reply::Async(sender)))
                .map_err(|_| stopped());
            Box::pin(async move {
                sent?;
                receiver.await.map_err(|_| stopped())?
            })
        })
    }
    pub fn read_asset_async(&self, id: AssetId) -> Pending<Asset> {
        let response = self.request_async(|reply| Request::ReadAsset(id, reply));
        Box::pin(async move { response.await? })
    }
    pub fn write_asset_async(&self, asset: Asset) -> Pending<()> {
        let response = self.request_async(|reply| Request::WriteAsset(asset, reply));
        Box::pin(async move { response.await? })
    }

    pub fn take_wake(&mut self) -> Option<UnboundedReceiver<()>> {
        self.wake.take()
    }
    pub fn refresh(&self) {
        let _ = self.requests.send(Request::Refresh);
    }
    /// Read only these notes again after the host changed them in storage.
    pub fn refresh_notes(&self, ids: Vec<crate::NoteId>) {
        let _ = self.requests.send(Request::RefreshNotes(ids));
    }
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn new_unwatched(mut store: Store, house: markraft_commonmark::HouseStyleHandle) -> Self {
        store.set_house(house);
        Self::start(Box::new(store), false)
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
    /// Make the note `new` describes, or take the one already at its path.
    pub fn create_note_async(&self, new: NewNote) -> Pending<Created> {
        let response = self.request_async(|reply| Request::CreateNote(new, reply));
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
    /// Capture committed inputs before queueing; later edits cannot change the export.
    pub fn snapshot_async(
        &self,
        note: Note,
        library_generation: u64,
        auto_number_equations: bool,
    ) -> Pending<crate::doc::DocumentSnapshot> {
        let source = match self.sources.source(&note) {
            Ok(source) => source.map(|track| track.snapshot()),
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let snapshot = crate::doc::PendingSnapshot::new(
            note,
            library_generation,
            auto_number_equations,
            source,
            self.house.get(),
        );
        let response = self.request_async(|reply| Request::Snapshot(snapshot, reply));
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
                .map_err(|error| {
                    Message::new("error.markdown-preserve")
                        .arg("detail", error.to_string())
                        .into()
                }),
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
            .map_err(|_| StoreError::Worker(Message::new("error.saving-stopped")))
    }
    /// Everything the notes folder gave the user to read, once there is somewhere
    /// to show it. Empty after it has been taken.
    pub fn notices(&self) -> Vec<Message> {
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
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn acknowledge(&self, ids: Vec<String>) {
        let _ = self.requests.send(Request::Acknowledge(ids));
    }
    /// Reload after all earlier save requests finish. The caller must confirm discarding
    /// local changes and invalidate their revision acknowledgments before adopting the result.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn reload(&self) -> Result<Library, StoreError> {
        let (response, result) = mpsc::channel();
        self.requests
            .send(Request::Reload(Reply::Blocking(response)))
            .map_err(|_| StoreError::Worker(Message::new("error.worker-folder-unreachable")))?;
        // Do not time out and leave an invisible baseline change queued: the UI must
        // receive the adopted library before any later local snapshot can be saved.
        result
            .recv()
            .map_err(|_| StoreError::Worker(Message::new("error.worker-read-stopped")))?
    }
    /// A queue barrier: all earlier requests finish before this latest snapshot is saved.
    /// The outer result confirms receipt, not whether writing succeeded; callers must
    /// apply paths even when `Saved::result` reports a partial failure.
    /// A timeout leaves the request queued; callers retain unsaved state until a later
    /// snapshot confirms it. No result queries are needed after this call.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn flush(
        &self,
        revision: u64,
        library: Library,
        preferences: Preferences,
    ) -> Result<Saved, StoreError> {
        self.flush_with_timeout(revision, library, preferences, REPLY_TIMEOUT)
    }
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(coverage_nightly, coverage(off))]
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
            .map_err(|_| StoreError::Worker(Message::new("error.saving-stopped")))?;
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
        self.watcher_cancelled.store(true, Ordering::Release);
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
fn sync_versions(
    store: &dyn WorkerBackend,
    versions: &std::sync::Mutex<HashMap<String, StorageRevision>>,
) {
    *versions.lock().unwrap_or_else(|e| e.into_inner()) = store.persisted().into_iter().collect();
}

/// The worker's channel is closed: the thread is gone.
fn stopped() -> StoreError {
    StoreError::Worker(Message::new("error.worker-stopped"))
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(coverage_nightly, coverage(off))]
fn receive<T>(receiver: Receiver<T>, timeout: Duration) -> Result<T, StoreError> {
    receiver.recv_timeout(timeout).map_err(|error| match error {
        RecvTimeoutError::Timeout => StoreError::Worker(Message::new("error.worker-timeout")),
        RecvTimeoutError::Disconnected => {
            StoreError::Worker(Message::new("error.worker-no-response"))
        }
    })
}

/// One refresh request per burst of file events. A program saving a file produces
/// several, and Markraft's own writes produce them too; the store tells those apart.
fn watch(
    directory: &std::path::Path,
    extra: &[std::path::PathBuf],
    notices: Notices,
    requests: Sender<Request>,
    cancelled: Arc<AtomicBool>,
) -> Option<notify::RecommendedWatcher> {
    let queued = Arc::new(AtomicBool::new(false));
    let paths = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let full_scan = Arc::new(AtomicBool::new(false));
    let callback_notices = notices.clone();
    let callback_cancelled = cancelled.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if callback_cancelled.load(Ordering::Acquire) {
            return;
        }
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
                callback_notices.raise(Message::new(WATCH_FAILED));
                full_scan.store(true, Ordering::SeqCst);
                true
            }
        };
        if relevant && !queued.swap(true, Ordering::SeqCst) {
            let queued = queued.clone();
            let paths = paths.clone();
            let full_scan = full_scan.clone();
            let requests = requests.clone();
            let cancelled = callback_cancelled.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
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
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
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
            notices.raise(Message::new(WATCH_FAILED));
            None
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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
        let persistence = Persistence::start(Box::new(store), false);
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
    fn export_snapshot_is_frozen_before_later_edits_and_does_not_mark_dirty() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut library) = open(directory.path());
        let persistence = Persistence::start(Box::new(store), false);
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Captured"));
        let note = library.active_note().clone();
        let generation = library.generation;
        let changes = library.changes.clone();
        let pending = persistence.snapshot_async(note.clone(), generation, true);
        assert_eq!(library.changes, changes);
        library.set_document(&id, doc::from_markdown("Later"));
        // Flush is a queue barrier, so the snapshot response is now ready.
        persistence
            .flush(0, library.clone(), Preferences::default())
            .unwrap()
            .result
            .unwrap();
        let snapshot = futures_util::FutureExt::now_or_never(pending)
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.note_id, id);
        assert_eq!(snapshot.document, note.document);
        assert_eq!(snapshot.markdown, "Captured\n");
        assert_eq!(snapshot.library_generation, generation);
        assert_eq!(
            snapshot.base_path,
            note.path
                .and_then(|path| path.parent().map(ToOwned::to_owned))
        );
        assert!(snapshot.auto_number_equations);
        assert_eq!(doc::plain_text(&library.active_note().document), "Later");
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
        let persistence = Persistence::start(Box::new(store), false);
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
            let captured = persistence.snapshot_async(
                library.active_note().clone(),
                library.generation,
                false,
            );
            persistence
                .flush(0, library.clone(), Preferences::default())
                .unwrap()
                .result
                .unwrap();
            assert!(only_note(directory.path()).1.ends_with("hello\n"));
            let captured = futures_util::FutureExt::now_or_never(captured)
                .unwrap()
                .unwrap();
            assert_eq!(captured.document, *committed_document(&state));
            assert_eq!(captured.markdown, "hello\n");
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
        let persistence = Persistence::start(Box::new(store), false);
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
        let persistence = Persistence::start(Box::new(store), false);
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
        let persistence = Persistence::start(Box::new(store), false);
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
            storage_worker: Default::default(),
            watcher_cancelled: Default::default(),
            shutdown: Default::default(),
            requests,
            events: results,
            wake: None,
            notices: Notices::default(),
            watches: mpsc::channel().0,
            sources: Arc::default(),
            house: Default::default(),
            capabilities: Default::default(),
            persisted: Default::default(),
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
            storage_worker: Default::default(),
            watcher_cancelled: Default::default(),
            shutdown: Default::default(),
            requests,
            events,
            wake: None,
            notices: Notices::default(),
            watches: mpsc::channel().0,
            sources: Arc::default(),
            house: Default::default(),
            capabilities: Default::default(),
            persisted: Default::default(),
        };
        let error = persistence
            .flush_with_timeout(42, library, Preferences::default(), Duration::ZERO)
            .unwrap_err();
        assert!(error.to_string().contains("may still complete"));
        let Request::Flush(revision, library, _, response, _) = incoming.try_recv().unwrap() else {
            panic!("the timed-out flush must remain queued");
        };
        let saved = WorkerBackend::save(&mut store, revision, &library, &Preferences::default());
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
        let persistence = Persistence::start(Box::new(store), false);
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
        let persistence = Persistence::start(Box::new(store), false);
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

        // One refresh may report both files in a single event; wait for both files
        // rather than for two events.
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
