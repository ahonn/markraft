//! Local image resources belong to an editor's document directory, not the process.
//!
//! A remote image — an `http:`, `https:` or protocol-relative source — is fetched only
//! when the host has handed the view a [`RemoteImageFetcher`](crate::RemoteImageFetcher).
//! Layout never waits for one: the first [`Images::load`] of a source answers
//! [`ImageError::Loading`] and queues it, the view fetches the queue in the background,
//! and the result is kept here until the source leaves the document.
use crate::animation::{AnimatedFormat, Picture};
use gpui::{Image, ImageFormat, SvgRenderer};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use markraft_media::{ImageType, LocateError, MAX_IMAGE_BYTES, Root, is_fetchable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageError {
    Remote,
    /// A remote image that is still being fetched.
    Loading,
    /// A remote image the fetcher could not deliver.
    RemoteFailed,
    InvalidPath,
    Missing,
    TooLarge,
    Unsupported,
    Unreadable,
    UnsupportedRoot,
}

impl ImageError {
    pub(crate) fn message(self) -> crate::EditorMessage {
        use crate::EditorMessage::*;
        match self {
            Self::Remote => ImageRemote,
            Self::Loading => ImageLoading,
            Self::RemoteFailed => ImageRemoteFailed,
            Self::InvalidPath => ImageInvalidPath,
            Self::Missing => ImageMissing,
            Self::TooLarge => ImageTooLarge,
            Self::Unsupported => ImageUnsupported,
            Self::Unreadable => ImageUnreadable,
            Self::UnsupportedRoot => ImageUnsupportedRoot,
        }
    }
}

impl From<LocateError> for ImageError {
    fn from(error: LocateError) -> Self {
        match error {
            LocateError::Remote => ImageError::Remote,
            LocateError::InvalidPath => ImageError::InvalidPath,
            LocateError::UnsupportedRoot => ImageError::UnsupportedRoot,
        }
    }
}

pub(crate) type ImageResult = Result<Arc<Picture>, ImageError>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = path.metadata().ok()?;
    metadata.is_file().then(|| Stamp {
        bytes: metadata.len(),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
    })
}

struct CachedImage {
    stamp: Option<Stamp>,
    result: ImageResult,
}

/// Where a remote source stands. A fetched one is never refetched while the source
/// stays in the document; it is retried once it has left and come back.
enum RemoteImage {
    Loading,
    Loaded(ImageResult),
}

#[derive(Default)]
pub(crate) struct Images {
    base: Option<PathBuf>,
    root: Option<PathBuf>,
    root_invalid: bool,
    cache: RefCell<HashMap<PathBuf, CachedImage>>,
    checked_at: Option<Instant>,
    /// Whether remote sources are fetched at all. Off, every one reads as
    /// [`ImageError::Remote`].
    remote_enabled: bool,
    remote: RefCell<HashMap<String, RemoteImage>>,
    /// Remote sources layout asked for that no fetch has been started for yet.
    requested: RefCell<Vec<String>>,
}

impl Images {
    /// A cache resolving relative sources against `base`. The view builds its
    /// own through [`Shaping`](crate::shaping::Shaping); the tests build one
    /// directly.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn new(base: Option<PathBuf>) -> Self {
        Self {
            base: base.and_then(|directory| std::path::absolute(directory).ok()),
            ..Self::default()
        }
    }

    pub(crate) fn set_root(&mut self, root: Result<Option<PathBuf>, String>) {
        self.root_invalid = root.is_err();
        self.root = root
            .ok()
            .flatten()
            .and_then(|root| match std::path::absolute(root) {
                Ok(root) => Some(root),
                Err(_) => {
                    self.root_invalid = true;
                    None
                }
            });
        self.cache.get_mut().clear();
        self.checked_at = None;
    }

    pub(crate) fn set_base(&mut self, base: Option<PathBuf>) {
        self.base = base.and_then(|directory| std::path::absolute(directory).ok());
        self.cache.get_mut().clear();
        self.checked_at = None;
    }

    fn resolve(&self, source: &str) -> Result<PathBuf, ImageError> {
        let root = match (self.root_invalid, self.root.as_deref()) {
            (true, _) => Root::Unusable,
            (false, Some(root)) => Root::At(root),
            (false, None) => Root::None,
        };
        Ok(markraft_media::resolve(source, self.base.as_deref(), root)?)
    }

    pub(crate) fn load(&self, source: &str) -> ImageResult {
        if is_fetchable(source) {
            return self.load_remote(source);
        }
        let path = self.resolve(source)?;
        let current = stamp(&path);
        let mut cache = self.cache.borrow_mut();
        if let Some(cached) = cache.get(&path)
            && cached.stamp == current
        {
            return cached.result.clone();
        }
        let result = decode(&path);
        cache.insert(
            path,
            CachedImage {
                stamp: current,
                result: result.clone(),
            },
        );
        result
    }

    fn load_remote(&self, source: &str) -> ImageResult {
        if !self.remote_enabled {
            return Err(ImageError::Remote);
        }
        let mut remote = self.remote.borrow_mut();
        match remote.get(source) {
            Some(RemoteImage::Loaded(result)) => result.clone(),
            Some(RemoteImage::Loading) => Err(ImageError::Loading),
            None => {
                remote.insert(source.to_owned(), RemoteImage::Loading);
                self.requested.borrow_mut().push(source.to_owned());
                Err(ImageError::Loading)
            }
        }
    }

    /// Fetch remote sources from now on, or stop: either way what was fetched is
    /// dropped, so a source reads as the switch now says.
    pub(crate) fn set_remote_enabled(&mut self, enabled: bool) {
        if self.remote_enabled == enabled {
            return;
        }
        self.remote_enabled = enabled;
        self.remote.get_mut().clear();
        self.requested.get_mut().clear();
    }

    /// Whether layout has asked for a remote source no fetch has started for.
    pub(crate) fn has_requests(&self) -> bool {
        !self.requested.borrow().is_empty()
    }

    /// The remote sources to fetch, each once.
    pub(crate) fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.requested.borrow_mut())
    }

    /// Keep what fetching `source` gave, and say whether layout has to be redone.
    /// A source that left the document meanwhile, or a switch turned off, keeps
    /// nothing.
    pub(crate) fn finish_remote(&mut self, source: &str, result: ImageResult) -> bool {
        match self.remote.get_mut().get_mut(source) {
            Some(entry @ RemoteImage::Loading) => {
                *entry = RemoteImage::Loaded(result);
                true
            }
            _ => false,
        }
    }

    /// Keep resources for the current document only. Layout holds the same image
    /// Arcs, so evicting a fixed number here would save no memory while repeatedly
    /// decoding long image-heavy notes and losing failed-file change tracking.
    pub(crate) fn retain_sources<'a>(&self, sources: impl Iterator<Item = &'a str>) {
        let mut remote = HashSet::new();
        let mut paths = HashSet::new();
        for source in sources {
            if is_fetchable(source) {
                remote.insert(source);
            } else if let Ok(path) = self.resolve(source) {
                paths.insert(path);
            }
        }
        self.cache
            .borrow_mut()
            .retain(|path, _| paths.contains(path));
        self.remote
            .borrow_mut()
            .retain(|source, _| remote.contains(source.as_str()));
        self.requested
            .borrow_mut()
            .retain(|source| remote.contains(source.as_str()));
    }

    /// The host can poll its visible editor. Metadata is checked at most once a
    /// second; only a changed file requests a repaint and another decode.
    pub(crate) fn refresh(&mut self) -> bool {
        if self
            .checked_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return false;
        }
        self.checked_at = Some(Instant::now());
        self.invalidate_changed()
    }

    fn invalidate_changed(&mut self) -> bool {
        let cache = self.cache.get_mut();
        let before = cache.len();
        cache.retain(|path, cached| stamp(path) == cached.stamp);
        before != cache.len()
    }
}

fn decode(path: &Path) -> ImageResult {
    let metadata = path.metadata().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ImageError::Missing,
        _ => ImageError::Unreadable,
    })?;
    if !metadata.is_file() {
        return Err(ImageError::Missing);
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let format = ImageType::of_path(path)
        .map(gpui_format)
        .ok_or(ImageError::Unsupported)?;
    let bytes = std::fs::read(path).map_err(|_| ImageError::Unreadable)?;
    decode_bytes(format, bytes)
}

/// An animated file keeps its first frame decoded and its bytes to play from; gpui's
/// own decoding would hold every frame.
fn decode_bytes(format: ImageFormat, bytes: Vec<u8>) -> ImageResult {
    if let Some(animated) = AnimatedFormat::of(format, &bytes) {
        return Picture::animated(animated, bytes)
            .map(Arc::new)
            .ok_or(ImageError::Unreadable);
    }
    Image::from_bytes(format, bytes)
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .map(|image| Arc::new(Picture::still(image)))
        .map_err(|_| ImageError::Unreadable)
}

/// The format gpui decodes a picture of `kind` as.
fn gpui_format(kind: ImageType) -> ImageFormat {
    match kind {
        ImageType::Png => ImageFormat::Png,
        ImageType::Jpeg => ImageFormat::Jpeg,
        ImageType::Gif => ImageFormat::Gif,
        ImageType::Webp => ImageFormat::Webp,
        ImageType::Svg => ImageFormat::Svg,
        ImageType::Bmp => ImageFormat::Bmp,
        ImageType::Tiff => ImageFormat::Tiff,
        ImageType::Ico => ImageFormat::Ico,
    }
}

/// Fetch `source` with `fetcher` and decode it. Runs on a background thread.
pub(crate) fn fetch(fetcher: &crate::RemoteImageFetcher, source: &str) -> ImageResult {
    let bytes =
        fetcher(&markraft_media::fetch_url(source)).map_err(|_| ImageError::RemoteFailed)?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let format = ImageType::sniff(&bytes)
        .map(gpui_format)
        .ok_or(ImageError::Unsupported)?;
    decode_bytes(format, bytes)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"></svg>"#;

    #[test]
    fn a_remote_image_is_queued_once_and_shown_once_fetched() {
        let mut images = Images::default();
        let source = "https://example.com/a.png";
        assert_eq!(
            images.load(source),
            Err(ImageError::Remote),
            "off by default"
        );
        assert!(!images.has_requests());

        images.set_remote_enabled(true);
        assert_eq!(images.load(source), Err(ImageError::Loading));
        assert_eq!(images.load(source), Err(ImageError::Loading));
        assert_eq!(images.take_requests(), [source], "asked for once");
        assert!(!images.has_requests());

        let fetcher: crate::RemoteImageFetcher = Arc::new(|_| Ok(SVG.to_vec()));
        let fetched = fetch(&fetcher, source);
        assert!(images.finish_remote(source, fetched));
        assert!(images.load(source).is_ok());
        // Once delivered, a second delivery changes nothing.
        assert!(!images.finish_remote(source, Err(ImageError::RemoteFailed)));
        assert!(images.load(source).is_ok());
    }

    #[test]
    fn a_remote_image_that_left_the_document_keeps_nothing() {
        let mut images = Images::default();
        images.set_remote_enabled(true);
        let source = "//example.com/a.png";
        assert_eq!(images.load(source), Err(ImageError::Loading));
        images.retain_sources(std::iter::empty());
        assert!(images.take_requests().is_empty());
        assert!(!images.finish_remote(source, Err(ImageError::RemoteFailed)));
        // Turning fetching off forgets what was fetched.
        assert_eq!(images.load(source), Err(ImageError::Loading));
        images.set_remote_enabled(false);
        assert_eq!(images.load(source), Err(ImageError::Remote));
    }

    #[test]
    fn a_fetched_image_is_decoded_by_what_its_bytes_say() {
        let seen = std::sync::Mutex::new(Vec::new());
        let bytes = SVG.to_vec();
        let fetcher: crate::RemoteImageFetcher = Arc::new(move |url| {
            seen.lock().unwrap().push(url.to_owned());
            Ok(bytes.clone())
        });
        assert!(fetch(&fetcher, "//example.com/no-extension").is_ok());
        let failing: crate::RemoteImageFetcher = Arc::new(|_| Err("offline".into()));
        assert_eq!(
            fetch(&failing, "https://a.example/x.png"),
            Err(ImageError::RemoteFailed)
        );
        let text: crate::RemoteImageFetcher = Arc::new(|_| Ok(b"<html>no</html>".to_vec()));
        assert_eq!(
            fetch(&text, "https://a.example/x.png"),
            Err(ImageError::Unsupported)
        );
    }

    #[test]
    fn paths_are_relative_to_the_document_and_decode_url_escapes() {
        let images = Images::new(Some(PathBuf::from("/notes/project")));
        assert_eq!(
            images.resolve("../photo%20one.png").unwrap(),
            PathBuf::from("/notes/photo one.png")
        );
        assert_eq!(
            images.resolve("file:///notes/photo%20one.png").unwrap(),
            PathBuf::from("/notes/photo one.png")
        );
        assert_eq!(
            images.resolve("/notes/photo%20one.png").unwrap(),
            PathBuf::from("/notes/photo one.png")
        );
        assert_eq!(
            images.resolve("https://example.com/a.png"),
            Err(ImageError::Remote)
        );
        assert_eq!(
            Images::default().resolve("a.png"),
            Err(ImageError::InvalidPath)
        );
        let relative = Images::new(Some(PathBuf::from("notes")));
        assert_eq!(
            relative.resolve("a.png").unwrap(),
            std::env::current_dir().unwrap().join("notes/a.png")
        );
    }

    #[test]
    fn an_image_root_only_changes_slash_prefixed_urls() {
        let mut images = Images::new(Some(PathBuf::from("/notes/posts")));
        images.set_root(Ok(Some(PathBuf::from("/website"))));
        assert_eq!(
            images.resolve("/blog/img/test%20one.png").unwrap(),
            PathBuf::from("/website/blog/img/test one.png")
        );
        assert_eq!(
            images.resolve("img/test.png").unwrap(),
            PathBuf::from("/notes/posts/img/test.png")
        );
        assert_eq!(
            images.resolve("../test.png").unwrap(),
            PathBuf::from("/notes/test.png")
        );
        assert_eq!(
            images.resolve("file:///actual/test.png").unwrap(),
            PathBuf::from("/actual/test.png")
        );
        images.set_base(Some(PathBuf::from("/moved/posts")));
        assert_eq!(
            images.resolve("/test.png").unwrap(),
            PathBuf::from("/website/test.png")
        );
        assert_eq!(
            images.resolve("test.png").unwrap(),
            PathBuf::from("/moved/posts/test.png")
        );
    }

    #[test]
    fn unsupported_roots_do_not_fall_back_to_a_wrong_absolute_image() {
        let mut images = Images::new(Some(PathBuf::from("/notes")));
        images.set_root(Err("Unsupported YAML value".into()));
        assert_eq!(
            images.resolve("/test.png"),
            Err(ImageError::UnsupportedRoot)
        );
        assert_eq!(
            images.resolve("test.png").unwrap(),
            PathBuf::from("/notes/test.png")
        );
        assert_eq!(
            images.resolve("file:///actual/test.png").unwrap(),
            PathBuf::from("/actual/test.png")
        );
        images.set_root(Ok(None));
        assert_eq!(
            images.resolve("/test.png").unwrap(),
            PathBuf::from("/test.png")
        );
    }

    #[test]
    fn missing_replaced_and_deleted_images_are_invalidated() {
        let directory =
            std::env::temp_dir().join(format!("markraft-images-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("late.svg");
        let mut images = Images::new(Some(directory.clone()));
        assert!(matches!(images.load("late.svg"), Err(ImageError::Missing)));
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"></svg>"#,
        )
        .unwrap();
        assert!(images.invalidate_changed());
        let original_width = images.load("late.svg").unwrap().size().width.0;
        assert!(original_width > 0);
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="10"></svg>"#,
        )
        .unwrap();
        assert!(images.invalidate_changed());
        assert_eq!(
            images.load("late.svg").unwrap().size().width.0,
            original_width * 20
        );
        std::fs::remove_file(&path).unwrap();
        assert!(images.invalidate_changed());
        assert!(matches!(images.load("late.svg"), Err(ImageError::Missing)));
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn all_document_images_remain_tracked_until_removed() {
        let images = Images::new(Some(std::env::temp_dir()));
        let sources: Vec<_> = (0..30)
            .map(|n| format!("missing-markraft-{}-{n}.png", std::process::id()))
            .collect();
        for source in &sources {
            assert!(images.load(source).is_err());
        }
        assert_eq!(images.cache.borrow().len(), 30);
        images.retain_sources(sources.iter().take(2).map(String::as_str));
        assert_eq!(images.cache.borrow().len(), 2);
    }
}
