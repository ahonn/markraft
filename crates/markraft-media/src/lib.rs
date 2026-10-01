//! Where a note's pictures are and what kind they are, independent of any
//! renderer.
//!
//! The editor view draws pictures, exports embed them, and pasting files decides
//! whether they are pictures. All three read a source and a file the same way
//! through this crate, so a picture the view shows is one an export carries.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::path::{Path, PathBuf};

/// Pictures larger than this are neither drawn nor carried into an export.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// A picture format the editor draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageType {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF, animated or not.
    Gif,
    /// WebP, animated or not.
    Webp,
    /// SVG.
    Svg,
    /// BMP.
    Bmp,
    /// TIFF.
    Tiff,
    /// ICO.
    Ico,
}

impl ImageType {
    /// The format a file's extension names, ignoring case.
    pub fn from_extension(extension: &str) -> Option<ImageType> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "png" => ImageType::Png,
            "jpg" | "jpeg" => ImageType::Jpeg,
            "gif" => ImageType::Gif,
            "webp" => ImageType::Webp,
            "svg" => ImageType::Svg,
            "bmp" => ImageType::Bmp,
            "tif" | "tiff" => ImageType::Tiff,
            "ico" => ImageType::Ico,
            _ => return None,
        })
    }

    /// The format `path`'s extension names.
    pub fn of_path(path: &Path) -> Option<ImageType> {
        path.extension()
            .and_then(|extension| extension.to_str())
            .and_then(ImageType::from_extension)
    }

    /// The format `bytes` start like. A URL often names no extension, and a
    /// server's content type is not always right, so the bytes decide.
    pub fn sniff(bytes: &[u8]) -> Option<ImageType> {
        let starts = |prefix: &[u8]| bytes.starts_with(prefix);
        Some(if starts(b"\x89PNG\r\n\x1a\n") {
            ImageType::Png
        } else if starts(b"\xff\xd8\xff") {
            ImageType::Jpeg
        } else if starts(b"GIF87a") || starts(b"GIF89a") {
            ImageType::Gif
        } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            ImageType::Webp
        } else if starts(b"BM") {
            ImageType::Bmp
        } else if starts(b"II*\0") || starts(b"MM\0*") {
            ImageType::Tiff
        } else if starts(b"\0\0\x01\0") {
            ImageType::Ico
        } else {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
            let head = head.trim_start_matches('\u{feff}').trim_start();
            if head.starts_with('<') && head.contains("<svg") {
                ImageType::Svg
            } else {
                return None;
            }
        })
    }

    /// The media type, as a `data:` URI or an HTTP header names it.
    pub fn mime(self) -> &'static str {
        match self {
            ImageType::Png => "image/png",
            ImageType::Jpeg => "image/jpeg",
            ImageType::Gif => "image/gif",
            ImageType::Webp => "image/webp",
            ImageType::Svg => "image/svg+xml",
            ImageType::Bmp => "image/bmp",
            ImageType::Tiff => "image/tiff",
            ImageType::Ico => "image/x-icon",
        }
    }
}

/// Where an image's source points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageLocation {
    /// A file on this machine.
    File(PathBuf),
    /// A URL fetched over the network, protocol-relative ones as HTTPS.
    Remote(String),
}

/// Why a source names no file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocateError {
    /// It is a URL on the network, or another scheme than `file:`.
    Remote,
    /// It does not read as a path: a relative one with no note directory, or
    /// one that does not join.
    InvalidPath,
    /// It is absolute and the note names a root that cannot be used.
    UnsupportedRoot,
}

/// The root absolute picture paths start from: the note's `typora-root-url`.
#[derive(Clone, Copy, Debug)]
pub enum Root<'a> {
    /// The note names none, so an absolute path is read as it stands.
    None,
    /// The note names this folder.
    At(&'a Path),
    /// The note names one that cannot be used, which makes every absolute path
    /// unusable rather than silently rooted at `/`.
    Unusable,
}

/// [`Root`] owned, for holding beside a note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ImageRoot {
    /// The note names none.
    #[default]
    None,
    /// The note names this folder.
    At(PathBuf),
    /// The note names one that cannot be used.
    Unusable,
}

impl ImageRoot {
    /// The root, borrowed for [`resolve`] and [`locate`].
    pub fn as_root(&self) -> Root<'_> {
        match self {
            ImageRoot::None => Root::None,
            ImageRoot::At(path) => Root::At(path),
            ImageRoot::Unusable => Root::Unusable,
        }
    }
}

/// Resolve an image `source` to a path: relative to `base`, the note's
/// directory; absolute under `root`; and a `file:` URL as its path, with
/// percent-escapes decoded.
pub fn resolve(source: &str, base: Option<&Path>, root: Root<'_>) -> Result<PathBuf, LocateError> {
    if source.starts_with("//") {
        return Err(LocateError::Remote);
    }
    if let Ok(url) = url::Url::parse(source) {
        return if url.scheme() == "file" {
            url.to_file_path().map_err(|_| LocateError::InvalidPath)
        } else {
            Err(LocateError::Remote)
        };
    }
    let path = Path::new(source);
    let (base, source) = if path.is_absolute() {
        match root {
            Root::None => (Path::new("/"), source),
            Root::At(root) => (root, source.trim_start_matches('/')),
            Root::Unusable => return Err(LocateError::UnsupportedRoot),
        }
    } else {
        (base.ok_or(LocateError::InvalidPath)?, source)
    };
    let base = url::Url::from_directory_path(base).map_err(|_| LocateError::InvalidPath)?;
    base.join(source)
        .map_err(|_| LocateError::InvalidPath)?
        .to_file_path()
        .map_err(|_| LocateError::InvalidPath)
}

/// Where an image `source` points, read as [`resolve`] reads it, with sources
/// on the network told apart. `None` for a source that names neither.
pub fn locate(source: &str, base: Option<&Path>, root: Root<'_>) -> Option<ImageLocation> {
    if is_fetchable(source) {
        return Some(ImageLocation::Remote(fetch_url(source)));
    }
    resolve(source, base, root).ok().map(ImageLocation::File)
}

/// Whether `source` is on the network: an `http:` or `https:` URL, or a
/// protocol-relative `//host/…`.
pub fn is_fetchable(source: &str) -> bool {
    source.starts_with("//")
        || url::Url::parse(source).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// The URL a fetchable source is fetched from: a protocol-relative one over HTTPS.
pub fn fetch_url(source: &str) -> String {
    match source.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => source.to_owned(),
    }
}

/// `path` as a relative URL from the directory `from`: `/`-separated, with the
/// every byte but unreserved ASCII percent-encoded,
/// so [`resolve`] reads it back to `path`.
pub fn relative_url(from: &Path, path: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = path.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> =
        std::iter::repeat_n("..".to_owned(), from.len() - shared).collect();
    parts.extend(
        to[shared..]
            .iter()
            .map(|part| escape_url_part(&part.as_os_str().to_string_lossy())),
    );
    parts.join("/")
}

/// One path segment as a URL holds it: every byte but unreserved ASCII is
/// percent-encoded, multi-byte characters included, so the URL reads the same
/// in any tool.
fn escape_url_part(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_type_comes_from_its_extension_or_its_bytes() {
        assert_eq!(
            ImageType::of_path(Path::new("a/B.JPEG")),
            Some(ImageType::Jpeg)
        );
        assert_eq!(ImageType::of_path(Path::new("a.heic")), None);
        assert_eq!(
            ImageType::sniff(b"\x89PNG\r\n\x1a\nrest"),
            Some(ImageType::Png)
        );
        assert_eq!(
            ImageType::sniff(b"\xef\xbb\xbf <svg xmlns=''/>"),
            Some(ImageType::Svg)
        );
        assert_eq!(ImageType::sniff(b"plain text"), None);
        assert_eq!(ImageType::Svg.mime(), "image/svg+xml");
    }

    #[test]
    fn a_source_is_located_as_the_view_reads_it() {
        let base = Path::new("/notes/trip");
        let file = |path: &str| Some(ImageLocation::File(PathBuf::from(path)));
        assert_eq!(
            locate("assets/a%20b.png", Some(base), Root::None),
            file("/notes/trip/assets/a b.png")
        );
        assert_eq!(
            locate("../up.png", Some(base), Root::None),
            file("/notes/up.png")
        );
        assert_eq!(
            locate("/img/x.png", Some(base), Root::At(Path::new("/site"))),
            file("/site/img/x.png")
        );
        assert_eq!(locate("/img/x.png", None, Root::None), file("/img/x.png"));
        assert_eq!(
            locate("file:///tmp/y.png", None, Root::None),
            file("/tmp/y.png")
        );
        assert_eq!(
            locate("//cdn.example/z.png", Some(base), Root::None),
            Some(ImageLocation::Remote("https://cdn.example/z.png".into()))
        );
        assert_eq!(locate("relative.png", None, Root::None), None);
        assert_eq!(locate("ftp://example/z.png", Some(base), Root::None), None);
        assert_eq!(
            resolve("/img/x.png", Some(base), Root::Unusable),
            Err(LocateError::UnsupportedRoot)
        );
    }

    #[test]
    fn a_relative_url_reads_back_to_its_file() {
        let from = Path::new("/v/Inbox");
        for target in [
            "/v/Inbox/assets/a b.png",
            "/v/x (1).png",
            "/v/Inbox/100%.png",
            "/v/Inbox/a#1?.png",
            "/v/Inbox/图片 一.png",
        ] {
            let url = relative_url(from, Path::new(target));
            assert_eq!(
                resolve(&url, Some(from), Root::None).unwrap(),
                PathBuf::from(target),
                "{url}"
            );
        }
        assert_eq!(
            relative_url(from, Path::new("/v/Inbox/a b.png")),
            "a%20b.png"
        );
        assert_eq!(relative_url(from, Path::new("/v/x.png")), "../x.png");
    }
}
