//! A second launch asks the existing process for this library to show its window.
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    os::unix::{fs::OpenOptionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub enum Launch {
    Primary(Instance),
    Forwarded,
}

pub struct Instance {
    socket: UnixDatagram,
    _directory: tempfile::TempDir,
    _lock: File,
}

impl Instance {
    pub fn acquire(file: &Path) -> Result<Launch, String> {
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
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    // macOS Unix sockets allow short paths only. TMPDIR and the notes
                    // directory can both exceed that limit, so use a private /tmp directory.
                    let directory = tempfile::Builder::new()
                        .prefix("markraft-")
                        .tempdir_in("/tmp")
                        .map_err(|error| error.to_string())?;
                    let path = directory.path().join("show.sock");
                    let socket = UnixDatagram::bind(&path).map_err(|error| error.to_string())?;
                    socket
                        .set_nonblocking(true)
                        .map_err(|error| error.to_string())?;
                    // Publish only after the socket is listening, while retaining the lock
                    // for this instance's lifetime. A simultaneous launcher waits below.
                    lock.set_len(0).map_err(|error| error.to_string())?;
                    lock.write_all(path.as_os_str().as_encoded_bytes())
                        .and_then(|_| lock.sync_all())
                        .map_err(|error| error.to_string())?;
                    return Ok(Launch::Primary(Self {
                        socket,
                        _directory: directory,
                        _lock: lock,
                    }));
                }
                Err(TryLockError::WouldBlock) => {
                    if let Ok(address) = std::fs::read_to_string(&lock_path)
                        && !address.is_empty()
                    {
                        let sender = UnixDatagram::unbound().map_err(|error| error.to_string())?;
                        sender
                            .set_write_timeout(Some(Duration::from_millis(100)))
                            .map_err(|error| error.to_string())?;
                        // A datagram queues one complete request; no accept/read race or
                        // partial message can discard a show request on the UI thread.
                        if sender.send_to(b"show", &address).is_ok() {
                            return Ok(Launch::Forwarded);
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err("Another Markraft instance is starting or not responding. \
                                    Try opening it again."
                            .into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::Error(error)) => return Err(error.to_string()),
            }
        }
    }

    pub fn requested_show(&self) -> bool {
        let mut requested = false;
        let mut message = [0; 16];
        // Coalesce repeated launches, with a bound so a busy socket cannot monopolize UI work.
        for _ in 0..64 {
            match self.socket.recv(&mut message) {
                Ok(length) => requested |= &message[..length] == b"show",
                Err(_) => break,
            }
        }
        requested
    }
}

fn canonical_target(file: &Path) -> Result<PathBuf, String> {
    if file.exists() {
        return file.canonicalize().map_err(|error| error.to_string());
    }
    let parent = file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    Ok(parent
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(file.file_name().ok_or("The notes path must name a file.")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn second_launch_forwards_and_a_dropped_instance_can_be_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let Launch::Primary(instance) = Instance::acquire(&path).unwrap() else {
            panic!("primary");
        };
        assert!(matches!(
            Instance::acquire(&path).unwrap(),
            Launch::Forwarded
        ));
        assert!(matches!(
            Instance::acquire(&path).unwrap(),
            Launch::Forwarded
        ));
        assert!(instance.requested_show());
        assert!(!instance.requested_show());
        drop(instance);
        assert!(matches!(
            Instance::acquire(&path).unwrap(),
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
                    Instance::acquire(&path).unwrap()
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
        assert!(primaries[0].requested_show());
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
        let Launch::Primary(instance) = Instance::acquire(&path).unwrap() else {
            panic!("primary");
        };
        assert!(matches!(
            Instance::acquire(&alias).unwrap(),
            Launch::Forwarded
        ));
        assert!(instance.requested_show());
    }

    #[test]
    fn stale_published_address_is_replaced_after_the_owner_exits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".instance-lock");
        std::fs::write(PathBuf::from(lock_path), b"/tmp/no-such-markraft.sock").unwrap();
        assert!(matches!(
            Instance::acquire(&path).unwrap(),
            Launch::Primary(_)
        ));
    }
}
