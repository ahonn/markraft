//! A copy of a note in an Obsidian vault, written as a file.
//!
//! Obsidian's own URL scheme carries a note's text in the URL, which long notes
//! outgrow; writing the file needs neither Obsidian running nor the clipboard.
//! The vault's settings are read, never written: new notes and attachments go
//! where the vault says they go.

use crate::fs::StoreError;
use markraft_commonmark::SourceSnapshot;
use markraft_commonmark::schema as md;
use markraft_core::Fragment;
use markraft_core::Node;
use markraft_media::{ImageLocation, ImageType};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
#[cfg(not(feature = "sandbox"))]
use std::sync::Mutex;
#[cfg(not(feature = "sandbox"))]
use std::time::SystemTime;

/// A vault Obsidian knows about.
#[cfg(not(feature = "sandbox"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vault {
    /// The vault's folder name, which is how Obsidian names it.
    pub name: String,
    pub path: PathBuf,
}

#[cfg(not(feature = "sandbox"))]
#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    vaults: HashMap<String, RegisteredVault>,
}

#[cfg(not(feature = "sandbox"))]
#[derive(Deserialize)]
struct RegisteredVault {
    path: PathBuf,
    #[serde(default)]
    ts: u64,
}

/// The vaults Obsidian has registered on this Mac, most recently used first,
/// leaving out any whose folder is gone.
///
/// The ⌘K panel asks on every draw, so the registry is read again only when it
/// changes.
#[cfg(not(feature = "sandbox"))]
pub fn vaults() -> Vec<Vault> {
    static CACHE: Mutex<Option<(SystemTime, Vec<Vault>)>> = Mutex::new(None);
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let registry = Path::new(&home).join("Library/Application Support/obsidian/obsidian.json");
    let Ok(modified) = std::fs::metadata(&registry).and_then(|meta| meta.modified()) else {
        return Vec::new();
    };
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    let registered = match cache.as_ref() {
        Some((read_at, vaults)) if *read_at == modified => vaults.clone(),
        _ => {
            let vaults = std::fs::read_to_string(&registry)
                .map(|json| parse_registry(&json))
                .unwrap_or_default();
            *cache = Some((modified, vaults.clone()));
            vaults
        }
    };
    // A vault's folder can go without the registry changing.
    registered
        .into_iter()
        .filter(|vault| vault.path.is_dir())
        .collect()
}

#[cfg(not(feature = "sandbox"))]
fn parse_registry(json: &str) -> Vec<Vault> {
    let Ok(registry) = serde_json::from_str::<Registry>(json) else {
        return Vec::new();
    };
    let mut vaults: Vec<_> = registry.vaults.into_values().collect();
    vaults.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| a.path.cmp(&b.path)));
    vaults
        .into_iter()
        .map(|vault| Vault {
            name: vault
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: vault.path,
        })
        .collect()
}

/// A selected vault must be its root, so its settings and attachment locations
/// are interpreted relative to the folder the user granted access to.
pub fn validate_selected_vault(path: &Path) -> Result<(), crate::locale::Message> {
    if path.join(".obsidian").is_dir() {
        Ok(())
    } else {
        Err(crate::locale::Message::new("error.obsidian-vault-folder"))
    }
}

/// The settings of a vault that decide where a new note and its attachments go.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultSettings {
    #[serde(default)]
    new_file_location: String,
    #[serde(default)]
    new_file_folder_path: String,
    #[serde(default)]
    attachment_folder_path: Option<String>,
}

impl VaultSettings {
    fn read(vault: &Path) -> VaultSettings {
        std::fs::read_to_string(vault.join(".obsidian/app.json"))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Where a new note goes. "Same folder as the current file" has no current
    /// file here, so it means the vault's root, as it does in a fresh vault.
    fn note_folder(&self, vault: &Path) -> PathBuf {
        match self.new_file_location.as_str() {
            "folder" => inside(vault, &self.new_file_folder_path),
            _ => vault.to_owned(),
        }
    }

    /// Where attachments go: the vault's root by default, a folder beside the
    /// note for a path starting `./`, and otherwise that folder of the vault.
    fn attachment_folder(&self, vault: &Path, note_folder: &Path) -> PathBuf {
        match self.attachment_folder_path.as_deref().map(str::trim) {
            None | Some("") | Some("/") => vault.to_owned(),
            Some(beside) if beside == "." || beside.starts_with("./") => {
                inside(note_folder, beside.trim_start_matches('.'))
            }
            Some(folder) => inside(vault, folder),
        }
    }
}

/// `relative` under `base`, ignoring any part that would climb out of it.
fn inside(base: &Path, relative: &str) -> PathBuf {
    let mut path = base.to_owned();
    for component in Path::new(relative.trim_matches('/')).components() {
        if let Component::Normal(part) = component {
            path.push(part);
        }
    }
    path
}

/// The note being sent: what it says, and how to write a changed copy of it.
pub struct Note<'a> {
    /// The name the copy is written under: the note's own file name, without
    /// its extension.
    pub name: &'a str,
    pub document: &'a Node,
    /// The note's Markdown as it would be saved now.
    pub markdown: &'a str,
    /// The file's bytes as read, to write a changed copy back with everything
    /// it did not change as it was.
    pub source: Option<&'a SourceSnapshot>,
    /// How to spell the note when it has no source to write a copy against.
    pub house: &'a markraft_commonmark::HouseStyleHandle,
    /// Where its pictures are read from, as the editor reads them.
    pub base: Option<&'a Path>,
    pub root: markraft_media::Root<'a>,
}

/// Write `note` as a new note in `vault`, with the local pictures it shows
/// copied in beside it and its links to them pointing at the copies. Returns
/// the new note's path.
pub fn send(vault: &Path, note: &Note<'_>) -> Result<PathBuf, StoreError> {
    let settings = VaultSettings::read(vault);
    let note_folder = settings.note_folder(vault);
    let attachments = settings.attachment_folder(vault, &note_folder);
    let mut copies: HashMap<String, PathBuf> = HashMap::new();
    for reference in references(note.document) {
        if copies.contains_key(&reference) {
            continue;
        }
        let Some(ImageLocation::File(file)) =
            markraft_media::locate(&reference, note.base, note.root)
        else {
            continue;
        };
        let Some(name) = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if !file.is_file() || ImageType::of_path(&file).is_none() {
            continue;
        }
        create_dir(&attachments)?;
        let target = unused(&attachments, &name, None);
        let copied = std::fs::read(&file)
            .map_err(|error| crate::fs::describe(&file, &error))
            .and_then(|bytes| write_new(&target, &bytes));
        if let Err(error) = copied {
            discard(copies.values());
            return Err(error);
        }
        copies.insert(reference, target);
    }
    let markdown = if copies.is_empty() {
        note.markdown.to_owned()
    } else {
        let relinked = relink(note.document, &|reference| {
            copies.get(reference).map(|target| Link {
                url: markraft_media::relative_url(&note_folder, target),
                name: target
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        });
        // The source writes back only what changed; a document it cannot map is
        // written whole rather than with stale links.
        note.source
            .and_then(|source| source.render(crate::doc::schema(), &relinked).ok())
            .unwrap_or_else(|| format!("{}\n", crate::doc::to_markdown_in(&relinked, note.house)))
    };
    let written = create_dir(&note_folder).and_then(|()| {
        let path = unused(&note_folder, &file_stem(note.name), Some("md"));
        write_new(&path, markdown.as_bytes()).map(|()| path)
    });
    // Pictures copied for a note that was never written would be strays in
    // someone's vault.
    if written.is_err() {
        discard(copies.values());
    }
    written
}

/// Remove files this send copied in, best effort: the error being reported is
/// the one that matters.
fn discard<'a>(files: impl IntoIterator<Item = &'a PathBuf>) {
    for file in files {
        let _ = std::fs::remove_file(file);
    }
}

fn create_dir(path: &Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(path).map_err(|error| crate::fs::describe(path, &error))
}

/// A new file: never one that is already there, even one that appeared since
/// its name was chosen.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| crate::fs::describe(path, &error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| crate::fs::describe(path, &error))
}

/// Every way the note can name a picture, as written: an image's source, a
/// wiki embed's target, and in HTML blocks and tags an `<img>`'s `src` and a
/// link definition's destination. Which of them are local pictures is for
/// [`markraft_media::locate`] to say.
fn references(document: &Node) -> Vec<String> {
    let schema = crate::doc::schema();
    let kind = |name| schema.node_id(name);
    let (image, wiki, raw_block, raw_inline) = (
        kind(md::IMAGE),
        kind(md::WIKI_LINK),
        kind(md::RAW_BLOCK),
        kind(md::RAW_INLINE),
    );
    let mut found = Vec::new();
    let mut stack = vec![document];
    while let Some(node) = stack.pop() {
        let ty = Some(node.type_id());
        let text = |name| {
            node.attrs()
                .get(name)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        if ty == image {
            found.extend(text("src"));
        } else if ty == wiki && flag(node, "embed") {
            found.extend(text("target"));
        } else if ty == raw_block {
            let source: String = node.children().filter_map(Node::text).collect();
            found.extend(html_sources(&source).into_iter().map(|(_, value)| value));
            found.extend(definitions(&source).into_iter().map(|(_, value)| value));
        } else if ty == raw_inline {
            let source = text("source").unwrap_or_default();
            found.extend(html_sources(&source).into_iter().map(|(_, value)| value));
        }
        stack.extend(node.children().rev());
    }
    found
}

fn flag(node: &Node, name: &str) -> bool {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Where a copied picture is, as the note links to it.
struct Link {
    /// A URL relative to the note.
    url: String,
    /// The file's name, which a wiki embed finds it by.
    name: String,
}

/// `document` with every reference `copied` knows pointing at the copy.
fn relink(document: &Node, copied: &dyn Fn(&str) -> Option<Link>) -> Node {
    let schema = crate::doc::schema();
    let kind = |name| schema.node_id(name);
    let ty = Some(document.type_id());
    let attr = |name| {
        document
            .attrs()
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
    };
    if ty == kind(md::IMAGE) {
        return match copied(attr("src")) {
            Some(link) => document.with_attrs(document.attrs().with("src", link.url)),
            None => document.clone(),
        };
    }
    if ty == kind(md::WIKI_LINK) {
        return match flag(document, "embed")
            .then(|| copied(attr("target")))
            .flatten()
        {
            Some(link) => document.with_attrs(document.attrs().with("target", link.name)),
            None => document.clone(),
        };
    }
    if ty == kind(md::RAW_INLINE) {
        let source = rewrite_html(attr("source"), copied);
        return document.with_attrs(document.attrs().with("source", source));
    }
    if ty == kind(md::RAW_BLOCK) {
        let children = document.children().map(|child| match child.text() {
            Some(text) => {
                child.with_text(&rewrite_definitions(&rewrite_html(text, copied), copied))
            }
            None => child.clone(),
        });
        return document.copy(Fragment::from_nodes(children));
    }
    if document.child_count() == 0 {
        return document.clone();
    }
    document.copy(Fragment::from_nodes(
        document.children().map(|child| relink(child, copied)),
    ))
}

/// The value of every `src` attribute in `html`, with the range it occupies:
/// quoted or bare, with or without spaces around `=`, in any case, and only
/// the attribute itself (`data-src` is another attribute).
fn html_sources(html: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let bytes = html.as_bytes();
    let mut found = Vec::new();
    for tag in tags(bytes) {
        attribute_values(html, tag, &mut found);
    }
    found
}

/// The byte ranges of `html`'s tags, `<` to `>`, with quoted values skipped
/// so a `>` inside one does not end its tag.
fn tags(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut tags = Vec::new();
    let mut at = 0;
    while let Some(open) = bytes[at..].iter().position(|&byte| byte == b'<') {
        let start = at + open;
        let mut cursor = start + 1;
        let mut quote = None;
        while let Some(&byte) = bytes.get(cursor) {
            match (quote, byte) {
                (None, b'"' | b'\'') => quote = Some(byte),
                (Some(open), byte) if byte == open => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            cursor += 1;
        }
        tags.push(start..cursor.min(bytes.len()));
        at = cursor.min(bytes.len());
        if at >= bytes.len() {
            break;
        }
    }
    tags
}

/// The `src` values inside one tag of `html`.
fn attribute_values(
    html: &str,
    tag: std::ops::Range<usize>,
    found: &mut Vec<(std::ops::Range<usize>, String)>,
) {
    let bytes = &html.as_bytes()[..tag.end];
    let mut at = tag.start;
    while at + 3 <= bytes.len() {
        let name_ends = at + 3;
        let is_src = bytes[at..name_ends].eq_ignore_ascii_case(b"src")
            && at > 0
            && bytes[at - 1].is_ascii_whitespace();
        if !is_src {
            at += 1;
            continue;
        }
        let mut cursor = name_ends;
        let skip_spaces = |mut cursor: usize| {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            cursor
        };
        cursor = skip_spaces(cursor);
        if bytes.get(cursor) != Some(&b'=') {
            at = name_ends;
            continue;
        }
        cursor = skip_spaces(cursor + 1);
        let (start, end) = match bytes.get(cursor) {
            Some(&quote @ (b'"' | b'\'')) => {
                let start = cursor + 1;
                let Some(length) = bytes[start..].iter().position(|&byte| byte == quote) else {
                    break;
                };
                (start, start + length)
            }
            Some(_) => {
                let length = bytes[cursor..]
                    .iter()
                    .position(|&byte| byte.is_ascii_whitespace() || byte == b'>')
                    .unwrap_or(bytes.len() - cursor);
                (cursor, cursor + length)
            }
            None => return,
        };
        if end > start {
            found.push((start..end, decode_entities(&html[start..end])));
        }
        at = end.max(name_ends);
    }
}

/// An attribute value as a browser reads it: the entities a path can hold
/// decoded, so `a&amp;b.png` names the file `a&b.png`.
fn decode_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn rewrite_html(html: &str, copied: &dyn Fn(&str) -> Option<Link>) -> String {
    let mut out = html.to_owned();
    for (range, value) in html_sources(html).into_iter().rev() {
        if let Some(link) = copied(&value) {
            out.replace_range(range, &link.url);
        }
    }
    out
}

/// Each link definition's destination in `text` (`[label]: destination`,
/// bare or in angle brackets), with the range it occupies.
fn definitions(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent..];
        if rest.starts_with('[')
            && let Some(close) = rest.find("]:")
        {
            let after = indent + close + 2;
            let spaces = line[after..].len() - line[after..].trim_start().len();
            let start = after + spaces;
            let tail = &line[start..];
            let (start, length) = match tail.strip_prefix('<') {
                Some(inner) => (start + 1, inner.find('>').unwrap_or(0)),
                None => (start, tail.find(char::is_whitespace).unwrap_or(tail.len())),
            };
            if length > 0 {
                found.push((
                    offset + start..offset + start + length,
                    line[start..start + length].to_owned(),
                ));
            }
        }
        offset += line.len();
    }
    found
}

fn rewrite_definitions(text: &str, copied: &dyn Fn(&str) -> Option<Link>) -> String {
    let mut out = text.to_owned();
    for (range, value) in definitions(text).into_iter().rev() {
        if let Some(link) = copied(&value) {
            out.replace_range(range, &link.url);
        }
    }
    out
}

/// A name as a file name: the characters Obsidian refuses in a note name
/// become `-`, and an empty name is "Untitled".
fn file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let stem = stem.trim_start_matches('.').trim();
    if stem.is_empty() {
        "Untitled".to_owned()
    } else {
        stem.to_owned()
    }
}

/// `name` in `folder`, or `name 1`, `name 2` and on for the first that is free,
/// as Obsidian numbers its own copies.
fn unused(folder: &Path, name: &str, extension: Option<&str>) -> PathBuf {
    let (stem, extension) = match extension {
        Some(extension) => (name.to_owned(), Some(extension.to_owned())),
        None => {
            let path = Path::new(name);
            (
                path.file_stem()
                    .map_or(name.to_owned(), |stem| stem.to_string_lossy().into_owned()),
                path.extension()
                    .map(|ext| ext.to_string_lossy().into_owned()),
            )
        }
    };
    let file = |suffix: String| {
        let stem = format!("{stem}{suffix}");
        match &extension {
            Some(extension) => folder.join(format!("{stem}.{extension}")),
            None => folder.join(stem),
        }
    };
    let mut candidate = file(String::new());
    let mut number = 1;
    while candidate.exists() {
        candidate = file(format!(" {number}"));
        number += 1;
    }
    candidate
}

/// The URL that opens `note` in Obsidian, whose parameters are URI-encoded,
/// slashes included.
pub fn open_url(note: &Path) -> String {
    let mut out = String::from("obsidian://open?path=");
    for byte in note.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Whether `folder` is inside one of `vaults`, where a note already is in
/// Obsidian and sending it would only make a copy.
#[cfg(not(feature = "sandbox"))]
pub fn within(folder: &Path, vaults: &[Vault]) -> bool {
    // Through links and aliases such as /private/var for /var.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let folder = real(folder);
    vaults
        .iter()
        .any(|vault| folder.starts_with(real(&vault.path)))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "sandbox"))]
    fn the_registry_lists_vaults_most_recent_first() {
        let json = r#"{"vaults":{
            "a1":{"path":"/Users/me/Old","ts":100},
            "b2":{"path":"/Users/me/Work Notes","ts":300,"open":true},
            "c3":{"path":"/Users/me/Home","ts":200}}}"#;
        let names: Vec<_> = parse_registry(json).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["Work Notes", "Home", "Old"]);
        assert!(parse_registry("not json").is_empty());
        assert!(parse_registry("{}").is_empty());
    }

    #[test]
    fn a_selected_vault_requires_its_settings_directory() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_selected_vault(directory.path()).is_err());
        std::fs::write(directory.path().join(".obsidian"), "not a directory").unwrap();
        assert!(validate_selected_vault(directory.path()).is_err());
        std::fs::remove_file(directory.path().join(".obsidian")).unwrap();
        std::fs::create_dir(directory.path().join(".obsidian")).unwrap();
        assert!(validate_selected_vault(directory.path()).is_ok());
    }

    #[test]
    fn a_vault_decides_where_notes_and_attachments_go() {
        let vault = Path::new("/v");
        let settings = |json: &str| -> VaultSettings { serde_json::from_str(json).unwrap() };
        let fresh = settings("{}");
        assert_eq!(fresh.note_folder(vault), Path::new("/v"));
        assert_eq!(
            fresh.attachment_folder(vault, Path::new("/v")),
            Path::new("/v")
        );
        let filed = settings(
            r#"{"newFileLocation":"folder","newFileFolderPath":"Inbox/","attachmentFolderPath":"./assets"}"#,
        );
        let notes = filed.note_folder(vault);
        assert_eq!(notes, Path::new("/v/Inbox"));
        assert_eq!(
            filed.attachment_folder(vault, &notes),
            Path::new("/v/Inbox/assets")
        );
        let shared = settings(r#"{"attachmentFolderPath":"Files/../../escape"}"#);
        assert_eq!(
            shared.attachment_folder(vault, Path::new("/v")),
            Path::new("/v/Files/escape")
        );
    }

    #[test]
    fn a_name_becomes_a_free_file_name() {
        assert_eq!(file_stem("Plan: Q4 / draft?"), "Plan- Q4 - draft-");
        assert_eq!(file_stem("   "), "Untitled");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            unused(dir.path(), "Trip", Some("md")),
            dir.path().join("Trip.md")
        );
        std::fs::write(dir.path().join("Trip.md"), "").unwrap();
        std::fs::write(dir.path().join("Trip 1.md"), "").unwrap();
        assert_eq!(
            unused(dir.path(), "Trip", Some("md")),
            dir.path().join("Trip 2.md")
        );
        std::fs::write(dir.path().join("a b.png"), "").unwrap();
        assert_eq!(
            unused(dir.path(), "a b.png", None),
            dir.path().join("a b 1.png")
        );
    }

    /// A vault with an attachment folder, a notes folder holding `files`, and
    /// `markdown` sent from there as "Trip". Returns the vault and the new note.
    fn send_from_notes(
        home: &Path,
        markdown: &str,
        files: &[&str],
        with_source: bool,
    ) -> (PathBuf, PathBuf) {
        let vault = home.join("Vault");
        std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
        std::fs::write(
            vault.join(".obsidian/app.json"),
            r#"{"attachmentFolderPath":"attachments"}"#,
        )
        .unwrap();
        let notes = home.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        for file in files {
            std::fs::write(notes.join(file), b"png").unwrap();
        }
        let origin =
            markraft_commonmark::SourceDocument::parse(crate::doc::schema(), markdown).unwrap();
        let source = markraft_commonmark::SourceTrack::new(origin.clone()).snapshot();
        let note = Note {
            name: "Trip",
            document: origin.document(),
            markdown,
            source: with_source.then_some(&source),
            house: &markraft_commonmark::HouseStyleHandle::default(),
            base: Some(&notes),
            root: markraft_media::Root::None,
        };
        let path = send(&vault, &note).unwrap();
        (vault, path)
    }

    #[test]
    fn every_way_of_naming_a_picture_points_at_its_copy() {
        let markdown = "Intro *keep*  spacing.\n\n\
            ![a](pic.png \"t\") ![b](<a b.png>) ![[pic.png]]\n\n\
            ![r][ref]\n\n[ref]: 100%25.png\n\n\
            <img src=\"pic.png\" width=\"40\">\n\n\
            Not a picture: [doc](pic.png) and ![web](https://e.com/x.png).\n";
        let home = tempfile::tempdir().unwrap();
        let (vault, path) = send_from_notes(
            home.path(),
            markdown,
            &["pic.png", "a b.png", "100%.png"],
            true,
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Intro *keep*  spacing.\n\n\
             ![a](attachments/pic.png \"t\") ![b](attachments/a%20b.png) ![[pic.png]]\n\n\
             ![r][ref]\n\n[ref]: attachments/100%25.png\n\n\
             <img src=\"attachments/pic.png\" width=\"40\">\n\n\
             Not a picture: [doc](pic.png) and ![web](https://e.com/x.png).\n"
        );
        for file in ["pic.png", "a b.png", "100%.png"] {
            assert!(vault.join("attachments").join(file).is_file(), "{file}");
        }
    }

    #[test]
    fn a_second_send_never_replaces_a_file_and_follows_renamed_copies() {
        let markdown = "![[pic.png]] ![p](pic.png)\n";
        let home = tempfile::tempdir().unwrap();
        let (vault, first) = send_from_notes(home.path(), markdown, &["pic.png"], false);
        assert_eq!(first, vault.join("Trip.md"));
        let (_, second) = send_from_notes(home.path(), markdown, &["pic.png"], false);
        assert_eq!(second, vault.join("Trip 1.md"));
        assert!(
            std::fs::read_to_string(first)
                .unwrap()
                .contains("![[pic.png]]")
        );
        let written = std::fs::read_to_string(second).unwrap();
        assert!(written.contains("![[pic 1.png]]"), "{written}");
        assert!(written.contains("(attachments/pic%201.png)"), "{written}");
    }

    #[test]
    fn html_sources_are_read_as_a_browser_reads_them() {
        let values = |html: &str| -> Vec<String> {
            html_sources(html)
                .into_iter()
                .map(|(_, value)| value)
                .collect()
        };
        assert_eq!(
            values("<img SRC = \"a b.png\"> <img src='c.png'> <img src=d.png>"),
            ["a b.png", "c.png", "d.png"]
        );
        assert!(values("<img data-src=\"x.png\"> src=\"text\"").is_empty());
        assert_eq!(values("<img src=\"a&amp;b.png\">"), ["a&b.png"]);
        assert_eq!(decode_entities("&#x41;&#66;&bogus;&"), "AB&bogus;&");
        let html = "<p>图 <img alt=\"é\" src=\"p.png\"></p>";
        let (range, _) = html_sources(html).remove(0);
        assert_eq!(&html[range], "p.png");
    }

    #[test]
    fn a_note_that_cannot_be_written_leaves_no_copied_pictures() {
        let home = tempfile::tempdir().unwrap();
        let vault = home.path().join("Vault");
        std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
        // The new-note folder is a file, so the note cannot be written.
        std::fs::write(
            vault.join(".obsidian/app.json"),
            r#"{"newFileLocation":"folder","newFileFolderPath":"Inbox","attachmentFolderPath":"files"}"#,
        )
        .unwrap();
        std::fs::write(vault.join("Inbox"), b"not a folder").unwrap();
        let notes = home.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("pic.png"), b"png").unwrap();
        let markdown = "![p](pic.png)\n";
        let document = crate::doc::from_markdown(markdown);
        let note = Note {
            name: "Trip",
            document: &document,
            markdown,
            source: None,
            house: &markraft_commonmark::HouseStyleHandle::default(),
            base: Some(&notes),
            root: markraft_media::Root::None,
        };
        assert!(send(&vault, &note).is_err());
        assert!(!vault.join("files/pic.png").exists());
    }

    #[test]
    fn the_open_url_escapes_the_path() {
        assert_eq!(
            open_url(Path::new("/Users/me/Work Notes/Trip 1.md")),
            "obsidian://open?path=%2FUsers%2Fme%2FWork%20Notes%2FTrip%201.md"
        );
    }

    #[test]
    #[cfg(not(feature = "sandbox"))]
    fn a_folder_inside_a_vault_is_already_there() {
        let vaults = [Vault {
            name: "V".into(),
            path: PathBuf::from("/v"),
        }];
        assert!(within(Path::new("/v/sub"), &vaults));
        assert!(!within(Path::new("/w"), &vaults));
    }
}
