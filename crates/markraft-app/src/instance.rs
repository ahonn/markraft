//! Typed, bounded launch requests shared by CLI, Finder, and the running app.
use crate::fs::StoreError;
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

// Stay below macOS's default Unix datagram size. Reject oversized requests
// before delivery rather than truncating a path or opening only part of a batch.
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
            return Err(format!("An open request needs 1–{MAX_PATHS} absolute file paths.").into());
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, StoreError> {
        self.validate()?;
        let message = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if message.len() > MAX_REQUEST_BYTES {
            return Err(
                "Too many file paths for one launch. Open a smaller group of files.".into(),
            );
        }
        Ok(message)
    }

    fn decode(message: &[u8]) -> Result<Self, StoreError> {
        if message.len() > MAX_REQUEST_BYTES {
            return Err("Launch request is too large.".into());
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
        self.0
            .try_send(request)
            .map_err(|error| StoreError::from(format!("Could not queue the open request: {error}")))
    }

    pub fn open_urls(&self, urls: Vec<String>) -> Result<(), StoreError> {
        let paths = urls
            .into_iter()
            .map(|value| {
                url::Url::parse(&value)
                    .map_err(|error| format!("Invalid file URL: {error}"))?
                    .to_file_path()
                    .map_err(|_| "Only local file URLs can be opened.".to_owned())
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
    socket: UnixDatagram,
    _directory: tempfile::TempDir,
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
                    // macOS Unix sockets allow short paths only. TMPDIR and the notes
                    // directory can both exceed that limit, so use a private /tmp directory.
                    let directory = tempfile::Builder::new()
                        .prefix("markraft-")
                        .tempdir_in("/tmp")
                        .map_err(|error| relaunch_failure(&error))?;
                    let path = directory.path().join("show.sock");
                    let socket =
                        UnixDatagram::bind(&path).map_err(|error| relaunch_failure(&error))?;
                    socket
                        .set_nonblocking(true)
                        .map_err(|error| relaunch_failure(&error))?;
                    // Publish only after the socket is listening, while retaining the lock
                    // for this instance's lifetime. A simultaneous launcher waits below.
                    lock.set_len(0)
                        .map_err(|error| crate::fs::describe(file, &error))?;
                    lock.write_all(path.as_os_str().as_encoded_bytes())
                        .and_then(|_| lock.sync_all())
                        .map_err(|error| crate::fs::describe(file, &error))?;
                    let (sender, pending) = mpsc::sync_channel(MAX_PENDING);
                    let sender = RequestSender(sender);
                    sender.send(request)?;
                    return Ok(Launch::Primary(Self {
                        socket,
                        _directory: directory,
                        _lock: lock,
                        pending,
                        sender,
                    }));
                }
                Err(TryLockError::WouldBlock) => {
                    if let Ok(address) = std::fs::read_to_string(&lock_path)
                        && !address.is_empty()
                    {
                        let sender =
                            UnixDatagram::unbound().map_err(|error| relaunch_failure(&error))?;
                        sender
                            .set_write_timeout(Some(Duration::from_millis(100)))
                            .map_err(|error| relaunch_failure(&error))?;
                        // A datagram queues one complete request; no accept/read race or
                        // partial message can discard a show request on the UI thread.
                        if sender.send_to(&message, &address).is_ok() {
                            return Ok(Launch::Forwarded);
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(StoreError::Locked(
                            "Markraft is already running, but it did not answer. \
                             Wait a moment and open it again, or quit it from the \
                             menu bar first."
                                .into(),
                        ));
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
        let mut message = [0; MAX_REQUEST_BYTES + 1];
        for _ in 0..MAX_PENDING / 2 {
            if let Ok(request) = self.pending.try_recv() {
                requests.push(request);
            }
            match self.socket.recv(&mut message) {
                Ok(length) => match Request::decode(&message[..length]) {
                    Ok(request) => requests.push(request),
                    Err(error) => log::warn!("ignored invalid launch request: {error}"),
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    log::warn!("could not receive launch request: {error}");
                    break;
                }
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
    "Markraft could not set up the link that a second launch uses to reopen its window. \
     Quit Markraft and open it again."
        .into()
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
                .ok_or("Markraft needs a settings file to work with, not a folder.")?,
        ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

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
        assert_eq!(instance.requests(), vec![first, second]);
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
        let socket = UnixDatagram::unbound().unwrap();
        socket
            .send_to(b"not json", instance._directory.path().join("show.sock"))
            .unwrap();
        socket
            .send_to(
                &Request::Show.encode().unwrap(),
                instance._directory.path().join("show.sock"),
            )
            .unwrap();
        assert_eq!(instance.requests(), vec![Request::Show]);
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
    fn concurrent_launches_elect_one_primary_without_removing_its_socket() {
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
