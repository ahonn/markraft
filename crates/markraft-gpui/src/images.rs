//! Local image resources belong to an editor's document directory, not the process.
use gpui::{Image, ImageFormat, RenderImage, SvgRenderer};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageError {
    Remote,
    InvalidPath,
    Missing,
    TooLarge,
    Unsupported,
    Unreadable,
    UnsupportedRoot,
}

impl ImageError {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Remote => "Remote preview unavailable",
            Self::InvalidPath => "Invalid image path",
            Self::Missing => "Image file not found",
            Self::TooLarge => "Image exceeds 16 MB",
            Self::Unsupported => "Unsupported image format",
            Self::Unreadable => "Cannot read image",
            Self::UnsupportedRoot => "Unsupported typora-root-url",
        }
    }
}

type ImageResult = Result<Arc<RenderImage>, ImageError>;

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

#[derive(Default)]
pub(crate) struct Images {
    base: Option<PathBuf>,
    root: Option<PathBuf>,
    root_invalid: bool,
    cache: RefCell<HashMap<PathBuf, CachedImage>>,
    checked_at: Option<Instant>,
}

impl Images {
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
        if source.starts_with("//") {
            return Err(ImageError::Remote);
        }
        if let Ok(url) = url::Url::parse(source) {
            return if url.scheme() == "file" {
                url.to_file_path().map_err(|_| ImageError::InvalidPath)
            } else {
                Err(ImageError::Remote)
            };
        }
        let path = Path::new(source);
        let (base, source) = if path.is_absolute() {
            if self.root_invalid {
                return Err(ImageError::UnsupportedRoot);
            }
            match self.root.as_deref() {
                Some(root) => (root, source.trim_start_matches('/')),
                None => (Path::new("/"), source),
            }
        } else {
            (self.base.as_deref().ok_or(ImageError::InvalidPath)?, source)
        };
        let base = url::Url::from_directory_path(base).map_err(|_| ImageError::InvalidPath)?;
        base.join(source)
            .map_err(|_| ImageError::InvalidPath)?
            .to_file_path()
            .map_err(|_| ImageError::InvalidPath)
    }

    pub(crate) fn load(&self, source: &str) -> ImageResult {
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

    /// Keep resources for the current document only. Layout holds the same image
    /// Arcs, so evicting a fixed number here would save no memory while repeatedly
    /// decoding long image-heavy notes and losing failed-file change tracking.
    pub(crate) fn retain_sources<'a>(&self, sources: impl Iterator<Item = &'a str>) {
        let paths: HashSet<_> = sources
            .filter_map(|source| self.resolve(source).ok())
            .collect();
        self.cache
            .borrow_mut()
            .retain(|path, _| paths.contains(path));
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
    if metadata.len() > MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    let format = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => ImageFormat::Png,
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        Some("webp") => ImageFormat::Webp,
        Some("gif") => ImageFormat::Gif,
        Some("svg") => ImageFormat::Svg,
        Some("bmp") => ImageFormat::Bmp,
        Some("tif" | "tiff") => ImageFormat::Tiff,
        Some("ico") => ImageFormat::Ico,
        _ => return Err(ImageError::Unsupported),
    };
    let bytes = std::fs::read(path).map_err(|_| ImageError::Unreadable)?;
    Image::from_bytes(format, bytes)
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .map_err(|_| ImageError::Unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn typora_root_only_changes_slash_prefixed_urls() {
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
        let original_width = images.load("late.svg").unwrap().size(0).width.0;
        assert!(original_width > 0);
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="10"></svg>"#,
        )
        .unwrap();
        assert!(images.invalidate_changed());
        assert_eq!(
            images.load("late.svg").unwrap().size(0).width.0,
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
