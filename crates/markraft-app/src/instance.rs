//! Typed, bounded launch requests shared by CLI, Finder, and the running app.
use crate::fs::StoreError;
use crate::locale::Message;
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

use notify::Watcher;
use serde::{Deserialize, Serialize};

// Bound both disk and memory use. Reject oversized requests before delivery
// rather than truncating a path or opening only part of a batch.
const MAX_REQUEST_BYTES: usize = 2048;
const MAX_PATHS: usize = 32;
const MAX_PENDING: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "paths", deny_unknown_fields)]
pub enum Request {
    Show,
    OpenPaths(Vec<PathBuf>),
}

impl Request {
    fn validate(&self) -> Result<(), StoreError> {
        if let Self::OpenPaths(paths) = self
            && (paths.is_empty()
                || paths.len() > MAX_PATHS
                || paths.iter().any(|p| !p.is_absolute()))
        {
            return Err(Message::new("error.launch-path-count")
                .arg("max", MAX_PATHS.to_string())
                .into());
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, StoreError> {
        self.validate()?;
        let message = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if message.len() > MAX_REQUEST_BYTES {
            return Err(Message::new("error.launch-too-many").into());
        }
        Ok(message)
    }

    fn decode(message: &[u8]) -> Result<Self, StoreError> {
        if message.len() > MAX_REQUEST_BYTES {
            return Err(Message::new("error.launch-large").into());
        }
        let request: Self = serde_json::from_slice(message).map_err(|error| error.to_string())?;
        request.validate()?;
        Ok(request)
    }
}

/// Native URL events can arrive before the main window exists. This bounded
/// queue keeps them until the app polls, including Finder cold launches.
#[derive(Clone)]
pub struct RequestSender(SyncSender<Request>);

impl RequestSender {
    pub fn send(&self, request: Request) -> Result<(), StoreError> {
        request.encode()?;
        self.0.try_send(request).map_err(|error| {
            StoreError::from(Message::new("error.launch-queue").arg("detail", error.to_string()))
        })
    }

    pub fn open_urls(&self, urls: Vec<String>) -> Result<(), StoreError> {
        let paths = urls
            .into_iter()
            .map(|value| {
                url::Url::parse(&value)
                    .map_err(|error| {
                        Message::new("error.invalid-file-url").arg("detail", error.to_string())
                    })?
                    .to_file_path()
                    .map_err(|_| Message::new("error.local-files-only"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.send(Request::OpenPaths(paths))
    }
}

pub enum Launch {
    Primary(Instance),
    Forwarded,
}

pub struct Instance {
    channel: LaunchChannel,
    _lock: File,
    pending: Receiver<Request>,
    sender: RequestSender,
}

impl Instance {
    pub fn acquire(file: &Path, request: Request) -> Result<Launch, StoreError> {
        let message = request.encode()?;
        let canonical = canonical_target(file)?;
        let mut lock_name = canonical.into_os_string();
        lock_name.push(".instance-lock");
        let lock_path = PathBuf::from(lock_name);
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| crate::fs::describe(file, &error))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    let channel = LaunchChannel::create(&std::env::temp_dir())
                        .map_err(|error| relaunch_failure(&error))?;
                    // Publish only after the channel is ready, while retaining the lock
                    // for this instance's lifetime. A simultaneous launcher waits below.
                    lock.set_len(0)
                        .map_err(|error| crate::fs::describe(file, &error))?;
                    lock.write_all(channel.directory.path().as_os_str().as_encoded_bytes())
                        .and_then(|_| lock.sync_all())
                        .map_err(|error| crate::fs::describe(file, &error))?;
                    let (sender, pending) = mpsc::sync_channel(MAX_PENDING);
                    let sender = RequestSender(sender);
                    sender.send(request)?;
                    return Ok(Launch::Primary(Self {
                        channel,
                        _lock: lock,
                        pending,
                        sender,
                    }));
                }
                Err(TryLockError::WouldBlock) => {
                    if let Ok(address) = std::fs::read(&lock_path)
                        && !address.is_empty()
                    {
                        let address = PathBuf::from(std::ffi::OsString::from_vec(address));
                        if LaunchChannel::send(&address, &message).is_ok() {
                            return Ok(Launch::Forwarded);
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(StoreError::Locked(Message::new(
                            "error.instance-unresponsive",
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::Error(error)) => {
                    return Err(crate::fs::describe(file, &error));
                }
            }
        }
    }

    pub fn sender(&self) -> RequestSender {
        self.sender.clone()
    }

    pub fn requests(&self) -> Vec<Request> {
        let mut requests = Vec::new();
        // Alternate native/initial requests and IPC so neither source can starve
        // the other; the fixed limit also bounds work on the UI thread.
        let mut forwarded = self.channel.receive().into_iter();
        for _ in 0..MAX_PENDING / 2 {
            if let Ok(request) = self.pending.try_recv() {
                requests.push(request);
            }
            if let Some(request) = forwarded.next() {
                requests.push(request);
            }
        }
        requests
    }
}

/// A bounded spool in the app's temporary directory works inside App Sandbox
/// without network permissions. Unlike a Unix socket address, this directory can
/// exceed macOS's 104-byte socket pathname limit. Each operation opens its own
/// lock handle, so concurrent callers never share ownership of the queue lock.
struct LaunchChannel {
    _watcher: notify::RecommendedWatcher,
    dirty: Arc<AtomicBool>,
    directory: tempfile::TempDir,
}

struct QueueLock(File);

impl Drop for QueueLock {
    fn drop(&mut self) {
        // A concurrent process launch can inherit the file handle until exec.
        // Closing our handle alone would leave its shared lock alive meanwhile.
        if let Err(error) = self.0.unlock() {
            log::warn!("could not unlock launch channel: {error}");
        }
    }
}

impl LaunchChannel {
    fn create(root: &Path) -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("markraft-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path().join("queue.lock"))?;
        let dirty = Arc::new(AtomicBool::new(true));
        let changed = dirty.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                // Polling the UI should only inspect memory while the queue is idle.
                // Ignore our own removals; create/modify also cover a crashed writer.
                if event.is_err()
                    || event.is_ok_and(|event| {
                        event.need_rescan()
                            || !matches!(event.kind, notify::EventKind::Remove(_))
                                && event.paths.iter().any(|path| {
                                    path.extension()
                                        .is_some_and(|extension| extension == "request")
                                })
                    })
                {
                    changed.store(true, Ordering::Release);
                }
            })
            .map_err(io::Error::other)?;
        watcher
            .watch(directory.path(), notify::RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        // Establish the watcher before publishing the address. Starting dirty
        // additionally covers changes that arrive before its first callback.
        Ok(Self {
            _watcher: watcher,
            dirty,
            directory,
        })
    }

    fn lock(directory: &Path) -> io::Result<QueueLock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("queue.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(QueueLock(file)),
            Err(TryLockError::WouldBlock) => Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    fn entries(directory: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(sequence) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".request"))
                .and_then(|name| name.parse::<u64>().ok())
            {
                entries.push((sequence, entry.path()));
            }
        }
        entries.sort_unstable_by_key(|(sequence, _)| *sequence);
        Ok(entries)
    }

    fn send(directory: &Path, message: &[u8]) -> io::Result<()> {
        if message.len() > MAX_REQUEST_BYTES {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let _lock = Self::lock(directory)?;
        let entries = Self::entries(directory)?;
        if entries.len() >= MAX_PENDING {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let sequence = match entries.last() {
            Some((sequence, _)) => sequence
                .checked_add(1)
                .ok_or_else(|| io::Error::other("launch sequence exhausted"))?,
            None => 0,
        };
        let path = directory.join(format!("{sequence}.request"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        // Readers hold the same lock, so they never observe an in-progress write.
        // A writer that crashes can leave an incomplete file; decoding rejects it.
        if let Err(error) = file.write_all(message) {
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
        Ok(())
    }

    fn receive(&self) -> Vec<Request> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }
        let directory = self.directory.path();
        let _lock = match Self::lock(directory) {
            Ok(lock) => lock,
            Err(error) => {
                if error.kind() == io::ErrorKind::WouldBlock {
                    // Retry next poll if the notification preceded write completion.
                    self.dirty.store(true, Ordering::Release);
                } else {
                    log::warn!("could not lock launch channel: {error}");
                }
                return Vec::new();
            }
        };
        let entries = match Self::entries(directory) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("could not read launch channel: {error}");
                return Vec::new();
            }
        };
        if entries.len() > MAX_PENDING / 2 {
            self.dirty.store(true, Ordering::Release);
        }
        let mut requests = Vec::new();
        for (_, path) in entries.into_iter().take(MAX_PENDING / 2) {
            let mut message = Vec::new();
            let result = File::open(&path).and_then(|file| {
                file.take((MAX_REQUEST_BYTES + 1) as u64)
                    .read_to_end(&mut message)
            });
            if let Err(error) = std::fs::remove_file(&path) {
                // Do not deliver a request we could not remove: a later poll
                // would otherwise execute it again.
                log::warn!("could not remove launch request: {error}");
                continue;
            }
            match result {
                Ok(_) => match Request::decode(&message) {
                    Ok(request) => requests.push(request),
                    Err(error) => log::warn!("ignored invalid launch request: {error}"),
                },
                Err(error) => log::warn!("could not read launch request: {error}"),
            }
        }
        requests
    }
}

/// The private channel a second launch uses to hand its request to this one.
/// Its failures are about this machine rather than about the notes, so they
/// share one sentence and leave the detail in the log.
fn relaunch_failure(error: &io::Error) -> StoreError {
    log::warn!("the launch channel failed: {error}");
    Message::new("error.relaunch-link").into()
}

fn canonical_target(file: &Path) -> Result<PathBuf, StoreError> {
    if file.exists() {
        return file
            .canonicalize()
            .map_err(|error| crate::fs::describe(file, &error));
    }
    let parent = file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| crate::fs::describe(parent, &error))?;
    Ok(parent
        .canonicalize()
        .map_err(|error| crate::fs::describe(parent, &error))?
        .join(
            file.file_name()
                .ok_or(Message::new("error.settings-is-folder"))?,
        ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn wait_for_requests(instance: &Instance, count: usize) -> Vec<Request> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        while requests.len() < count {
            requests.extend(instance.requests());
            assert!(Instant::now() < deadline, "launch requests did not arrive");
            std::thread::sleep(Duration::from_millis(10));
        }
        requests
    }

    #[test]
    fn cold_and_warm_launches_preserve_open_paths_without_creating_documents() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let first = Request::OpenPaths(vec![dir.path().join("中文 note #1.md")]);
        let second = Request::OpenPaths(vec![
            dir.path().join("other.md"),
            dir.path().join("same name.md"),
        ]);
        let Launch::Primary(instance) = Instance::acquire(&settings, first.clone()).unwrap() else {
            panic!("primary");
        };
        assert!(matches!(
            Instance::acquire(&settings, second.clone()).unwrap(),
            Launch::Forwarded
        ));
        assert_eq!(wait_for_requests(&instance, 2), vec![first, second]);
        assert!(instance.requests().is_empty());
        assert!(!dir.path().join("中文 note #1.md").exists());
        assert!(!dir.path().join("other.md").exists());
    }

    #[test]
    fn native_urls_queue_before_a_window_exists_and_decode_escaped_paths() {
        let dir = tempfile::tempdir().unwrap();
        let Launch::Primary(instance) =
            Instance::acquire(&dir.path().join("settings.json"), Request::Show).unwrap()
        else {
            panic!("primary");
        };
        instance.requests();
        let paths = vec![
            dir.path().join("中文 # 100%.md"),
            dir.path().join("another.md"),
        ];
        instance
            .sender()
            .open_urls(
                paths
                    .iter()
                    .map(|p| url::Url::from_file_path(p).unwrap().into())
                    .collect(),
            )
            .unwrap();
        assert_eq!(instance.requests(), vec![Request::OpenPaths(paths)]);
        assert!(
            instance
                .sender()
                .open_urls(vec!["https://example.com/note.md".into()])
                .is_err()
        );
        assert!(
            instance
                .sender()
                .open_urls(vec!["file://remote-host/note.md".into()])
                .is_err()
        );
        assert!(instance.requests().is_empty());
    }

    #[test]
    fn malformed_and_oversized_messages_are_rejected_without_partial_delivery() {
        for message in [
            br#"{"type":"Unknown"}"#.as_slice(),
            br#"{"type":"OpenPaths","paths":[]}"#,
            br#"{"type":"OpenPaths","paths":["relative.md"]}"#,
            br#"{"type":"Show","extra":true}"#,
        ] {
            assert!(Request::decode(message).is_err());
        }
        assert!(Request::decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
        assert!(
            Request::OpenPaths(vec![PathBuf::from("/note.md"); MAX_PATHS + 1])
                .encode()
                .is_err()
        );
        assert!(
            Request::OpenPaths(vec![PathBuf::from(format!(
                "/{}.md",
                "n".repeat(MAX_REQUEST_BYTES)
            ))])
            .encode()
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let Launch::Primary(instance) =
            Instance::acquire(&dir.path().join("settings.json"), Request::Show).unwrap()
        else {
            panic!("primary");
        };
        instance.requests();
        LaunchChannel::send(instance.channel.directory.path(), b"not json").unwrap();
        LaunchChannel::send(
            instance.channel.directory.path(),
            &Request::Show.encode().unwrap(),
        )
        .unwrap();
        assert_eq!(wait_for_requests(&instance, 1), vec![Request::Show]);
    }

    #[test]
    fn native_queue_is_bounded_and_remains_responsive_after_draining() {
        let dir = tempfile::tempdir().unwrap();
        let Launch::Primary(instance) =
            Instance::acquire(&dir.path().join("settings.json"), Request::Show).unwrap()
        else {
            panic!("primary");
        };
        let sender = instance.sender();
        for _ in 1..MAX_PENDING {
            sender.send(Request::Show).unwrap();
        }
        assert!(sender.send(Request::Show).is_err());
        assert_eq!(instance.requests().len(), MAX_PENDING / 2);
        sender.send(Request::Show).unwrap();
    }

    #[test]
    fn second_launch_forwards_and_a_dropped_instance_can_be_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let Launch::Primary(instance) = Instance::acquire(&path, Request::Show).unwrap() else {
            panic!("primary");
        };
        assert!(matches!(
            Instance::acquire(&path, Request::Show).unwrap(),
            Launch::Forwarded
        ));
        assert!(matches!(
            Instance::acquire(&path, Request::Show).unwrap(),
            Launch::Forwarded
        ));
        assert!(instance.requests().contains(&Request::Show));
        assert!(instance.requests().is_empty());
        drop(instance);
        assert!(matches!(
            Instance::acquire(&path, Request::Show).unwrap(),
            Launch::Primary(_)
        ));
    }

    #[test]
    fn concurrent_launches_elect_one_primary_without_removing_its_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let path = path.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Instance::acquire(&path, Request::Show).unwrap()
                })
            })
            .collect();
        // Keep the winner alive in its JoinHandle/result until every launcher has completed.
        let launches: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let primaries: Vec<_> = launches
            .iter()
            .filter_map(|launch| match launch {
                Launch::Primary(instance) => Some(instance),
                Launch::Forwarded => None,
            })
            .collect();
        assert_eq!(primaries.len(), 1);
        assert_eq!(primaries[0].requests(), vec![Request::Show; 8]);
    }

    #[test]
    fn long_note_paths_and_symlink_aliases_share_the_same_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("long-library-directory-".repeat(8))
            .join("notes.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}").unwrap();
        let alias = dir.path().join("alias.json");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let Launch::Primary(instance) = Instance::acquire(&path, Request::Show).unwrap() else {
            panic!("primary");
        };
        assert!(matches!(
            Instance::acquire(&alias, Request::Show).unwrap(),
            Launch::Forwarded
        ));
        assert!(instance.requests().contains(&Request::Show));
    }

    #[test]
    fn forwarded_launch_from_another_process() {
        const SETTINGS_ENV: &str = "MARKRAFT_TEST_INSTANCE_SETTINGS";
        if let Some(settings) = std::env::var_os(SETTINGS_ENV) {
            assert!(matches!(
                Instance::acquire(Path::new(&settings), Request::Show).unwrap(),
                Launch::Forwarded,
            ));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join("settings.json");
        let Launch::Primary(instance) = Instance::acquire(&settings, Request::Show).unwrap() else {
            panic!("primary");
        };
        assert_eq!(instance.requests(), vec![Request::Show]);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::forwarded_launch_from_another_process",
            ])
            .env(SETTINGS_ENV, &settings)
            .output()
            .unwrap();
        assert!(output.status.success(), "child failed: {output:?}");
        assert_eq!(wait_for_requests(&instance, 1), vec![Request::Show]);
    }

    #[test]
    fn forwarded_requests_wake_an_idle_channel() {
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join("settings.json");
        let Launch::Primary(instance) = Instance::acquire(&settings, Request::Show).unwrap() else {
            panic!("primary");
        };
        assert_eq!(instance.requests(), vec![Request::Show]);
        assert!(!instance.channel.dirty.load(Ordering::Acquire));
        assert!(matches!(
            Instance::acquire(&settings, Request::Show).unwrap(),
            Launch::Forwarded,
        ));
        assert_eq!(wait_for_requests(&instance, 1), vec![Request::Show]);
    }

    #[test]
    fn long_temporary_paths_forward_requests_without_changing_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let long_root = root
            .path()
            .join("long-sandbox-temporary-directory-".repeat(6));
        std::fs::create_dir(&long_root).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let channel = LaunchChannel::create(&long_root).unwrap();
        assert!(channel.directory.path().as_os_str().len() > 104);
        let request = Request::OpenPaths(vec![long_root.join("中文 note.md")]);
        LaunchChannel::send(channel.directory.path(), &request.encode().unwrap()).unwrap();
        assert_eq!(channel.receive(), vec![request]);
        assert!(channel.receive().is_empty());
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }

    #[test]
    fn forwarded_queue_is_bounded_and_resumes_after_draining() {
        let root = tempfile::tempdir().unwrap();
        let channel = LaunchChannel::create(root.path()).unwrap();
        let message = Request::Show.encode().unwrap();
        for _ in 0..MAX_PENDING {
            LaunchChannel::send(channel.directory.path(), &message).unwrap();
        }
        assert_eq!(
            LaunchChannel::send(channel.directory.path(), &message)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
        );
        assert_eq!(channel.receive(), vec![Request::Show; MAX_PENDING / 2]);
        LaunchChannel::send(channel.directory.path(), &message).unwrap();
        assert_eq!(channel.receive(), vec![Request::Show; MAX_PENDING / 2]);
        assert_eq!(channel.receive(), vec![Request::Show]);
    }

    #[test]
    fn channel_files_are_private_and_removed_when_the_instance_exits() {
        let root = tempfile::tempdir().unwrap();
        let channel = LaunchChannel::create(root.path()).unwrap();
        let directory = channel.directory.path().to_path_buf();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o077,
            0,
        );
        LaunchChannel::send(&directory, &Request::Show.encode().unwrap()).unwrap();
        for name in ["queue.lock", "0.request"] {
            assert_eq!(
                std::fs::metadata(directory.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0,
            );
        }
        drop(channel);
        assert!(!directory.exists());
    }

    #[test]
    fn forwarded_requests_remain_ordered_and_readers_skip_busy_writers() {
        let root = tempfile::tempdir().unwrap();
        let channel = LaunchChannel::create(root.path()).unwrap();
        let requests: Vec<_> = (0..12)
            .map(|i| Request::OpenPaths(vec![root.path().join(format!("{i}.md"))]))
            .collect();
        for request in &requests {
            LaunchChannel::send(channel.directory.path(), &request.encode().unwrap()).unwrap();
        }
        let lock = LaunchChannel::lock(channel.directory.path()).unwrap();
        assert!(channel.receive().is_empty());
        drop(lock);
        assert_eq!(channel.receive(), requests);
    }

    #[test]
    fn queue_lock_is_released_even_while_an_inherited_handle_remains_open() {
        let root = tempfile::tempdir().unwrap();
        let channel = LaunchChannel::create(root.path()).unwrap();
        let lock = LaunchChannel::lock(channel.directory.path()).unwrap();
        let inherited = lock.0.try_clone().unwrap();
        assert!(matches!(
            LaunchChannel::lock(channel.directory.path()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock,
        ));
        drop(lock);
        let next = LaunchChannel::lock(channel.directory.path()).unwrap();
        drop(next);
        drop(inherited);
    }

    #[test]
    fn stale_published_address_is_replaced_after_the_owner_exits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".instance-lock");
        std::fs::write(PathBuf::from(lock_path), b"/tmp/no-such-markraft.sock").unwrap();
        assert!(matches!(
            Instance::acquire(&path, Request::Show).unwrap(),
            Launch::Primary(_)
        ));
    }
}
