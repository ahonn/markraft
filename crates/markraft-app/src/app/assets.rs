//! Image insertion policy. Existing images are never moved or garbage-collected.
use crate::storage::{AttachmentPolicy, ImageNaming};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone)]
pub(super) enum Asset {
    File(PathBuf),
    Copy(PathBuf),
    Image(gpui::Image),
}

pub(super) fn from_clipboard(item: gpui::ClipboardItem) -> Vec<Asset> {
    item.into_entries()
        .flat_map(|entry| match entry {
            gpui::ClipboardEntry::Image(image) => vec![Asset::Image(image)],
            gpui::ClipboardEntry::ExternalPaths(paths) => {
                paths.0.into_iter().map(Asset::Copy).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

pub(super) fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tiff" | "tif" | "ico"
            )
        })
}

fn subdirectory(base: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("Choose an image folder inside the current folder.".into());
    }
    let destination = base.join(relative);
    let mut ancestor = destination.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or("The image folder is unavailable.")?;
    }
    if !ancestor
        .canonicalize()
        .map_err(|error| error.to_string())?
        .starts_with(base.canonicalize().map_err(|error| error.to_string())?)
    {
        return Err("The image folder links outside the selected folder.".into());
    }
    Ok(destination)
}

fn destination(document: &Path, root: &Path, policy: &AttachmentPolicy) -> Result<PathBuf, String> {
    let parent = document
        .parent()
        .ok_or("Save the document before inserting images.")?;
    match policy {
        AttachmentPolicy::Default => subdirectory(parent, Path::new("assets")),
        AttachmentPolicy::WorkspaceFolder(relative) => subdirectory(root, relative),
    }
}

/// Produce a URL path, escaping spaces, delimiters and non-ASCII bytes once.
fn relative_url(parent: &Path, path: &Path) -> Result<String, String> {
    let from: Vec<_> = parent.components().collect();
    let to: Vec<_> = path.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in shared..from.len() {
        relative.push("..");
    }
    for component in &to[shared..] {
        relative.push(component.as_os_str());
    }
    let text = relative
        .to_str()
        .ok_or("The image filename is not valid Unicode.")?;
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut encoded, "%{byte:02X}").unwrap();
        }
    }
    Ok(encoded)
}

/// The Markdown to insert, and where each image ended up relative to the note. A
/// caller that can no longer insert the Markdown still has to be able to say what
/// is now on disk.
pub(super) struct Inserted {
    pub markdown: String,
    pub urls: Vec<String>,
    /// Where each image is on disk, in the order of `urls`.
    pub paths: Vec<PathBuf>,
}

/// Where a copied image goes in `folder`, under a name no file there has yet.
fn image_path(folder: &Path, document: &Path, naming: ImageNaming, extension: &str) -> PathBuf {
    let stem = match naming {
        ImageNaming::RandomId => {
            return folder.join(format!("image-{}.{extension}", uuid::Uuid::new_v4()));
        }
        ImageNaming::NoteAndDate => {
            let note = document
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_owned());
            let local = crate::storage::timestamp()
                .saturating_add_signed(crate::platform::local_utc_offset() * 1000);
            let stamp = time_stamp(local);
            // A note named for the day it was made already says the date.
            let (date, time) = stamp.split_once(' ').unwrap_or((&stamp, ""));
            if note.contains(date) {
                format!("{note} {time}")
            } else {
                format!("{note} {stamp}")
            }
        }
    };
    // Two images pasted within a second take the same stamp; the later one is numbered.
    (1..)
        .map(|n| {
            folder.join(if n == 1 {
                format!("{stem}.{extension}")
            } else {
                format!("{stem} {n}.{extension}")
            })
        })
        .find(|path| !path.exists())
        .expect("some number is free")
}

/// A local time as a file name can hold it, `2026-09-24 10.21.05`.
fn time_stamp(local_milliseconds: u64) -> String {
    let (year, month, day, hour, minute, second, _) = crate::vault::civil(local_milliseconds);
    format!("{year:04}-{month:02}-{day:02} {hour:02}.{minute:02}.{second:02}")
}

/// Write assets before returning their references. A partial failure leaves files
/// intact: another application may already have discovered or referenced them.
pub(super) fn insert(
    assets: Vec<Asset>,
    document: &Path,
    root: &Path,
    policy: &AttachmentPolicy,
    naming: ImageNaming,
    journal: &Path,
) -> Result<Inserted, String> {
    let parent = document
        .parent()
        .ok_or("Save the document before inserting images.")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut references = Vec::new();
    let mut urls = Vec::new();
    let mut paths = Vec::new();
    for asset in assets {
        let copy = matches!(asset, Asset::Copy(_));
        let (existing, bytes, extension) = match asset {
            Asset::File(path) | Asset::Copy(path) => {
                if !is_image(&path) {
                    return Err("Only image files can be inserted here.".into());
                }
                // The name handed over says what the image is: a symbolic
                // link's target may have no extension at all.
                let extension = path
                    .extension()
                    .map(|extension| extension.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let path = path.canonicalize().map_err(|error| error.to_string())?;
                if !copy && path.starts_with(&root) {
                    (Some(path), Vec::new(), String::new())
                } else {
                    let metadata = path.metadata().map_err(|error| error.to_string())?;
                    if metadata.len() > MAX_BYTES {
                        return Err("The image exceeds 16 MB.".into());
                    }
                    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
                    (None, bytes, extension)
                }
            }
            Asset::Image(image) => (
                None,
                image.bytes().to_vec(),
                image.format().extension().to_owned(),
            ),
        };
        let path = if let Some(path) = existing {
            path
        } else {
            if bytes.len() as u64 > MAX_BYTES {
                return Err("The image exceeds 16 MB.".into());
            }
            let folder = destination(document, &root, policy)?;
            fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
            let path = image_path(&folder, document, naming, &extension);
            // Record the destination before writing. If copying, insertion or
            // document saving is interrupted, the retained image stays traceable
            // without assuming it is safe to delete a shared attachment.
            let record = journal.join(format!("{}.json", uuid::Uuid::new_v4()));
            let entry = serde_json::json!({
                "version": 1,
                "document": document,
                "image": path,
                "bytes": bytes.len(),
                "markdown": format!("![image]({})", relative_url(&parent, &path)?),
            });
            crate::fs::atomic_write(
                &record,
                &serde_json::to_vec(&entry).map_err(|e| e.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| error.to_string())?;
            path.canonicalize().map_err(|error| error.to_string())?
        };
        let url = relative_url(&parent, &path)?;
        references.push(format!("![image]({url})"));
        urls.push(url);
        paths.push(path);
    }
    Ok(Inserted {
        markdown: references.join("\n\n"),
        urls,
        paths,
    })
}

/// Read the supported scalar form of the `typora-root-url` image-preview root
/// from actual front matter. Never use body text or nested metadata as
/// application settings.
/// This intentionally is not a general YAML parser: unsupported values are
/// explicit diagnostics, so the caller can disable ambiguous image previews.
pub(super) fn image_root(source: &str, document: &Path) -> Result<Option<PathBuf>, String> {
    let normalized = source
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines: Vec<_> = normalized.lines().collect();
    if lines.first() != Some(&"---") {
        return Ok(None);
    }
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| matches!(*line, "---" | "...").then_some(index))
    else {
        return Ok(None);
    };
    let mut value = None;
    let mut merged = false;
    for (index, line) in lines[1..end].iter().enumerate() {
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with('{') && line.contains("typora-root-url") {
            return Err("Unsupported typora-root-url YAML mapping; use a top-level scalar.".into());
        }
        let Some((key, scalar)) = line.split_once(':') else {
            continue;
        };
        merged |= key.trim() == "<<";
        if !matches!(
            key.trim(),
            "typora-root-url" | "'typora-root-url'" | "\"typora-root-url\""
        ) {
            continue;
        }
        if value.is_some() {
            return Err(
                "Duplicate typora-root-url values; image root preview is unavailable.".into(),
            );
        }
        // A plain YAML scalar may continue on an indented line. Using only
        // its first line would silently select the wrong image directory.
        for next in &lines[index + 2..end] {
            if next.trim().is_empty() || next.trim_start().starts_with('#') {
                continue;
            }
            if next.starts_with(char::is_whitespace) {
                return Err(
                    "Multiline typora-root-url is unsupported; use a single-line path.".into(),
                );
            }
            break;
        }
        value = Some(root_scalar(scalar.trim())?);
    }
    let Some(value) = value else {
        return if merged {
            Err("Image roots inherited through YAML merges are unsupported.".into())
        } else {
            Ok(None)
        };
    };
    if value.is_empty() || value.chars().any(char::is_control) || value.starts_with('~') {
        return Err("Unsupported typora-root-url path; use a local absolute or document-relative directory.".into());
    }
    if let Ok(url) = url::Url::parse(&value) {
        if url.scheme() != "file" {
            return Err(
                "Remote typora-root-url preview is unavailable; use a local directory.".into(),
            );
        }
        return url
            .to_file_path()
            .map(Some)
            .map_err(|_| "Invalid local typora-root-url.".into());
    }
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        document
            .parent()
            .ok_or("Save the document before using a relative typora-root-url.")?
            .join(path)
    };
    std::path::absolute(path)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn root_scalar(value: &str) -> Result<String, String> {
    const UNSUPPORTED: &str =
        "Unsupported typora-root-url YAML value; use a single-line plain or quoted path.";
    let tail_is_comment = |tail: &str| tail.trim().is_empty() || tail.trim_start().starts_with('#');
    if value.starts_with('\"') {
        let mut values = serde_json::Deserializer::from_str(value).into_iter::<String>();
        let decoded = values.next().ok_or(UNSUPPORTED)?.map_err(|_| UNSUPPORTED)?;
        if !tail_is_comment(&value[values.byte_offset()..]) {
            return Err(UNSUPPORTED.into());
        }
        return Ok(decoded);
    }
    if let Some(value) = value.strip_prefix('\'') {
        let mut decoded = String::new();
        let mut chars = value.char_indices().peekable();
        while let Some((offset, ch)) = chars.next() {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    decoded.push('\'');
                } else {
                    if !tail_is_comment(&value[offset + ch.len_utf8()..]) {
                        return Err(UNSUPPORTED.into());
                    }
                    return Ok(decoded);
                }
            } else {
                decoded.push(ch);
            }
        }
        return Err(UNSUPPORTED.into());
    }
    let value = value
        .char_indices()
        .find_map(|(offset, ch)| {
            (ch == '#' && (offset == 0 || value[..offset].ends_with(char::is_whitespace)))
                .then_some(&value[..offset])
        })
        .unwrap_or(value)
        .trim();
    if value.is_empty()
        || value.starts_with(['[', '{', '&', '*', '!', '|', '>', '@', '`', ']', '}', ','])
        || value.contains(": ")
        || matches!(
            value,
            "~" | "null" | "Null" | "NULL" | "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
        )
        || value.parse::<f64>().is_ok()
    {
        return Err(UNSUPPORTED.into());
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typora_root_reads_only_top_level_frontmatter_scalars() {
        let document = Path::new("/notes/posts/note.md");
        for (value, expected) in [
            ("/website", PathBuf::from("/website")),
            (
                "../images # comment",
                PathBuf::from("/notes/posts/../images"),
            ),
            ("'../my images'", PathBuf::from("/notes/posts/../my images")),
            ("\"/my images\" # comment", PathBuf::from("/my images")),
            (
                "'/my ''quoted'' images'",
                PathBuf::from("/my 'quoted' images"),
            ),
            ("file:///my%20images", PathBuf::from("/my images")),
        ] {
            let source = format!("\u{feff}---\r\ntypora-root-url: {value}\r\n---\r\nbody");
            assert_eq!(image_root(&source, document).unwrap(), Some(expected));
        }
        for source in [
            "typora-root-url: /body",
            "---\nother: value\n---\ntypora-root-url: /body",
            "---\nother:\n  typora-root-url: /nested\n---\nbody",
            "---\ndescription: |\n  typora-root-url: /body\n---\nbody",
            "---\ntypora-root-url: /not-closed",
        ] {
            assert_eq!(image_root(source, document).unwrap(), None);
        }
    }

    #[test]
    fn unsupported_typora_root_values_are_diagnosed_without_guessing() {
        for value in [
            "",
            "[]",
            "{}",
            "*alias",
            "&anchor /path",
            "!tag /path",
            "|",
            ">",
            "null",
            "true",
            "42",
            "\"unterminated",
            "'unterminated",
            "https://example.com",
            "~/images",
        ] {
            let source = format!("---\ntypora-root-url: {value}\n---\nbody");
            assert!(
                image_root(&source, Path::new("/notes/note.md")).is_err(),
                "{value}"
            );
        }
        assert!(
            image_root(
                "---\ntypora-root-url: /one\ntypora-root-url: /two\n---",
                Path::new("/note.md")
            )
            .is_err()
        );
    }

    #[test]
    fn multiline_and_merged_roots_never_select_a_guessed_directory() {
        for source in [
            "---\ntypora-root-url: /first\n  second\n---",
            "---\ndefaults: &defaults\n  typora-root-url: /nested\n<<: *defaults\n---",
            "---\n{\"typora-root-url\": \"/nested\"}\n---",
        ] {
            assert!(image_root(source, Path::new("/note.md")).is_err());
        }
    }

    #[test]
    fn existing_images_are_referenced_without_copying() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        fs::create_dir(root.join("notes")).unwrap();
        fs::write(root.join("a 中文.png"), b"image").unwrap();
        let markdown = insert(
            vec![Asset::File(root.join("a 中文.png"))],
            &root.join("notes/note.md"),
            &root,
            &AttachmentPolicy::Default,
            ImageNaming::RandomId,
            &root.join("unused-journal"),
        )
        .unwrap();
        assert_eq!(markdown.markdown, "![image](../a%20%E4%B8%AD%E6%96%87.png)");
        assert_eq!(markdown.urls, ["../a%20%E4%B8%AD%E6%96%87.png"]);
        assert_eq!(fs::read_dir(root.join("notes")).unwrap().count(), 0);
    }

    #[test]
    fn pasted_files_always_copy_even_when_already_inside_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("notes");
        let journal = directory.path().join("private/image-imports");
        fs::create_dir(&root).unwrap();
        for source in [
            root.join("inside.png"),
            directory.path().join("outside.png"),
        ] {
            fs::write(&source, b"original image").unwrap();
            let clipboard = gpui::ClipboardItem {
                entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                    vec![source.clone()].into(),
                ))],
            };
            let mut previous = None;
            for policy in [
                AttachmentPolicy::Default,
                AttachmentPolicy::WorkspaceFolder("assets".into()),
            ] {
                let assets = from_clipboard(clipboard.clone());
                let inserted = insert(
                    assets,
                    &root.join("note.md"),
                    &root,
                    &policy,
                    ImageNaming::RandomId,
                    &journal,
                )
                .unwrap();
                let markdown = inserted.markdown;
                assert!(markdown.starts_with("![image](assets/image-"));
                assert_ne!(previous.as_ref(), Some(&markdown));
                let [relative] = inserted.urls.as_slice() else {
                    panic!("one image, one url");
                };
                assert!(markdown.contains(relative));
                assert_eq!(fs::read(root.join(relative)).unwrap(), b"original image");
                previous = Some(markdown);
            }
            assert_eq!(fs::read(source).unwrap(), b"original image");
        }
        assert_eq!(fs::read_dir(root.join("assets")).unwrap().count(), 4);
    }

    #[test]
    fn a_linked_image_takes_the_link_s_extension_not_its_target_s() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("notes");
        fs::create_dir(&root).unwrap();
        let target = directory.path().join("blob");
        fs::write(&target, b"linked image").unwrap();
        let link = directory.path().join("linked.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let inserted = insert(
            vec![Asset::File(link)],
            &root.join("note.md"),
            &root,
            &AttachmentPolicy::Default,
            ImageNaming::RandomId,
            &directory.path().join("journal"),
        )
        .unwrap();
        let [relative] = inserted.urls.as_slice() else {
            panic!("one image, one url");
        };
        assert!(relative.ends_with(".png"), "{relative}");
        assert_eq!(fs::read(root.join(relative)).unwrap(), b"linked image");
    }

    #[test]
    fn copied_images_use_selected_folder_and_never_replace_existing_images() {
        let root = tempfile::tempdir().unwrap();
        let journal = tempfile::tempdir().unwrap();
        let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, b"test image".to_vec());
        let policy = AttachmentPolicy::WorkspaceFolder("assets".into());
        let first = insert(
            vec![Asset::Image(image.clone())],
            &root.path().join("note.md"),
            root.path(),
            &policy,
            ImageNaming::RandomId,
            journal.path(),
        )
        .unwrap();
        let second = insert(
            vec![Asset::Image(image)],
            &root.path().join("note.md"),
            root.path(),
            &policy,
            ImageNaming::RandomId,
            journal.path(),
        )
        .unwrap();
        assert_ne!(first.markdown, second.markdown);
        assert_eq!(fs::read_dir(root.path().join("assets")).unwrap().count(), 2);
        let records: Vec<_> = fs::read_dir(journal.path()).unwrap().collect();
        assert_eq!(records.len(), 2);
        for record in records {
            let entry: serde_json::Value =
                serde_json::from_slice(&fs::read(record.unwrap().path()).unwrap()).unwrap();
            assert_eq!(
                entry["document"],
                root.path().join("note.md").to_str().unwrap()
            );
            assert!(Path::new(entry["image"].as_str().unwrap()).is_file());
            assert_eq!(entry["bytes"], 10);
        }
        assert!(
            destination(
                &root.path().join("note.md"),
                root.path(),
                &AttachmentPolicy::WorkspaceFolder("../outside".into())
            )
            .is_err()
        );
    }

    #[test]
    fn an_image_can_be_named_for_its_note_and_when_it_came() {
        assert_eq!(time_stamp(1_790_165_105_000), "2026-09-23 12.05.05");
        let folder = tempfile::tempdir().unwrap();
        let note = Path::new("/notes/Meeting notes.md");
        let first = image_path(folder.path(), note, ImageNaming::NoteAndDate, "png");
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("Meeting notes 20") && name.ends_with(".png"),
            "{name}"
        );
        // The same second again: the second copy is numbered, not written over.
        fs::write(&first, b"one").unwrap();
        let second = image_path(folder.path(), note, ImageNaming::NoteAndDate, "png");
        assert_ne!(first, second);
        assert!(
            second.to_string_lossy().ends_with(" 2.png") || second.file_name() != first.file_name()
        );
        // A note named for today keeps the date once: the image adds only the time.
        let local = crate::storage::timestamp()
            .saturating_add_signed(crate::platform::local_utc_offset() * 1000);
        let today = time_stamp(local)[..10].to_owned();
        let dated = folder.path().join(format!("{today} 09.53.md"));
        let image = image_path(folder.path(), &dated, ImageNaming::NoteAndDate, "png");
        let image = image.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(image.matches(&today).count(), 1, "{image}");
        let random = image_path(folder.path(), note, ImageNaming::RandomId, "png");
        assert!(
            random
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("image-")
        );
    }
}
