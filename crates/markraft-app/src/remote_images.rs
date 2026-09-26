//! Fetching the remote images a note shows.
//!
//! The editor asks for each remote source once while it stays in the note and never
//! waits for the answer; this is what answers, on a background thread. A picture that
//! was fetched is kept on disk, so reopening a note, or opening it offline, shows it
//! without asking the server again until the copy is a week old.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use markraft_gpui::RemoteImageFetcher;

/// The most a picture may take: what the editor decodes from a local file too.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How long a slow server is waited for before the image counts as unavailable.
const TIMEOUT: Duration = Duration::from_secs(20);
/// How old a kept copy may be before the server is asked again.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The one fetcher every note editor shares, keeping its copies in
/// [`cache_directory`]. Sharing it shares its connections too.
pub fn shared() -> RemoteImageFetcher {
    static SHARED: std::sync::LazyLock<RemoteImageFetcher> =
        std::sync::LazyLock::new(|| fetcher(cache_directory()));
    SHARED.clone()
}

/// A fetcher keeping what it fetches under `cache`, or keeping nothing without one.
pub fn fetcher(cache: Option<PathBuf>) -> RemoteImageFetcher {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    Arc::new(move |url: &str| {
        let kept = cache.as_deref().map(|cache| cache.join(file_name(url)));
        if let Some(bytes) = kept.as_deref().and_then(fresh) {
            return Ok(bytes);
        }
        match download(&agent, url) {
            Ok(bytes) => {
                if let Some(path) = &kept {
                    // A copy that cannot be written only costs the next fetch.
                    let _ = keep(path, &bytes);
                }
                Ok(bytes)
            }
            // Offline, an old copy is better than none.
            Err(error) => kept.as_deref().and_then(read).ok_or(error),
        }
    })
}

/// Where remote images are kept: the user's cache folder, which the system may
/// empty, rather than beside the notes.
pub fn cache_directory() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Caches/Markraft/Remote Images"))
}

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    agent
        .get(url)
        .call()
        .map_err(|error| error.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|error| error.to_string())
}

/// A kept copy young enough to show without asking the server.
fn fresh(path: &Path) -> Option<Vec<u8>> {
    let age = fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()?;
    (age < FRESH_FOR).then(|| read(path)).flatten()
}

fn read(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok().filter(|bytes| !bytes.is_empty())
}

/// Write the copy beside its final name and move it there, so a reader never
/// finds half a picture.
fn keep(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.persist(path).map_err(|error| error.error)?;
    // A write may keep the old timestamp on some file systems; the age is the fetch's.
    let _ = fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    Ok(())
}

/// The name a URL's copy is kept under: a stable hash of the whole URL, since a
/// URL holds characters no file name may and two may differ only in their query.
fn file_name(url: &str) -> String {
    // FNV-1a: the standard library's hasher is not promised to stay the same
    // between releases, and a changed name would only orphan the old copies.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_kept_under_a_stable_name_of_its_own() {
        assert_eq!(
            file_name("https://a.example/x.png"),
            file_name("https://a.example/x.png")
        );
        assert_ne!(
            file_name("https://a.example/x.png?1"),
            file_name("https://a.example/x.png?2")
        );
        assert_eq!(file_name("").len(), 16);
    }

    #[test]
    fn a_kept_copy_is_shown_without_asking_again_and_after_a_failed_fetch() {
        let cache = tempfile::tempdir().expect("a temporary folder");
        let url = "http://127.0.0.1:9/never.png";
        keep(&cache.path().join(file_name(url)), b"picture").expect("written");
        let fetch = fetcher(Some(cache.path().to_owned()));
        assert_eq!(fetch(url).as_deref(), Ok(&b"picture"[..]));
        // Nothing listens on the discard port, so a fetch has to fail.
        assert!(fetcher(None)(url).is_err());
    }
}
