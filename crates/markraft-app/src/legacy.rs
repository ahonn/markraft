//! The single-file note library earlier versions wrote.
//!
//! That file holds a flat document — one block per line, each a kind, a nesting
//! depth and a list of marked spans — which no longer has a type of its own now
//! that notes are a tree. The shape is mirrored here, read once on the first
//! launch after the upgrade, and converted into the Markdown the vault stores.
//! Nothing writes this format.

use crate::doc;
use crate::storage::{Library, Note, Preferences};
use markraft_commonmark::schema as md;
use markraft_core::{Attrs, Mark, MarkSet, Node};
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyMarks {
    bold: bool,
    italic: bool,
    code: bool,
    strikethrough: bool,
    underline: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacySpan {
    text: String,
    #[serde(default)]
    marks: LegacyMarks,
    #[serde(default)]
    link: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
enum LegacyKind {
    #[default]
    Paragraph,
    Heading(u8),
    Bullet,
    Ordered,
    Task {
        checked: bool,
    },
    Quote,
    Code {
        language: String,
    },
    Divider,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyBlock {
    kind: LegacyKind,
    depth: u8,
    spans: Vec<LegacySpan>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyDocument {
    blocks: Vec<LegacyBlock>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyNote {
    id: String,
    #[serde(default)]
    document: LegacyDocument,
    created_at: u64,
    updated_at: u64,
    #[serde(default)]
    deleted_at: Option<u64>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    front_matter: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyLibrary {
    version: u32,
    active_id: String,
    notes: Vec<LegacyNote>,
    #[serde(default)]
    preferences: Preferences,
}

/// Read one exported document written by earlier versions.
pub fn read_document(text: &str) -> Result<Node, String> {
    let legacy: LegacyDocument = serde_json::from_str(text).map_err(|error| {
        eprintln!("Markraft: a note could not be read from JSON: {error}");
        "This file does not hold a note Markraft can read. \
         Import the Markdown file instead."
            .to_owned()
    })?;
    Ok(document(&legacy))
}

/// Read the single-file library written by earlier versions.
pub fn read_library(path: &Path) -> Result<Library, String> {
    let bytes = std::fs::read(path).map_err(|error| crate::vault::describe(path, &error))?;
    let legacy: LegacyLibrary = serde_json::from_slice(&bytes).map_err(|error| {
        eprintln!(
            "Markraft: {} is not a readable library: {error}",
            path.display()
        );
        format!(
            "“{}” does not hold notes Markraft can read.",
            crate::vault::file_label(path)
        )
    })?;
    let library = Library {
        version: legacy.version,
        active_id: legacy.active_id,
        notes: legacy.notes.into_iter().map(note).collect(),
        preferences: legacy.preferences,
    };
    library.validate()?;
    Ok(library)
}

fn note(legacy: LegacyNote) -> Note {
    Note {
        id: legacy.id,
        document: document(&legacy.document),
        created_at: legacy.created_at,
        updated_at: legacy.updated_at,
        deleted_at: legacy.deleted_at,
        pinned: legacy.pinned,
        front_matter: legacy.front_matter,
        lossy: false,
    }
}

/// The flat document as Markdown, which the current codec then reads back as a
/// tree. Going through the format rather than building the tree directly is what
/// keeps the nesting, the list kinds and the code fences the responsibility of
/// one piece of code.
fn document(legacy: &LegacyDocument) -> Node {
    let mut source = String::new();
    let mut open_code: Option<&str> = None;
    for block in &legacy.blocks {
        let indent = "  ".repeat(usize::from(block.depth));
        if let LegacyKind::Code { language } = &block.kind {
            if open_code != Some(language.as_str()) {
                if open_code.is_some() {
                    source.push_str("```\n");
                }
                source.push_str(&format!("```{language}\n"));
                open_code = Some(language);
            }
            source.push_str(&plain(block));
            source.push('\n');
            continue;
        }
        if open_code.take().is_some() {
            source.push_str("```\n");
        }
        let prefix = match &block.kind {
            LegacyKind::Paragraph => String::new(),
            LegacyKind::Heading(level) => {
                format!("{} ", "#".repeat(usize::from(*level).clamp(1, 6)))
            }
            LegacyKind::Bullet => "- ".into(),
            LegacyKind::Ordered => "1. ".into(),
            LegacyKind::Task { checked } => {
                if *checked {
                    "- [x] ".into()
                } else {
                    "- [ ] ".into()
                }
            }
            LegacyKind::Quote => "> ".into(),
            LegacyKind::Code { .. } | LegacyKind::Divider => String::new(),
        };
        if matches!(block.kind, LegacyKind::Divider) {
            source.push_str(&format!("{indent}---\n\n"));
            continue;
        }
        source.push_str(&format!("{indent}{prefix}{}\n", inline(block)));
        // A paragraph, a heading or a quote at top level needs a blank line after it
        // so the next block is not folded into it; list items must not have one, or
        // the list turns loose.
        if !matches!(
            block.kind,
            LegacyKind::Bullet | LegacyKind::Ordered | LegacyKind::Task { .. }
        ) {
            source.push('\n');
        }
    }
    if open_code.is_some() {
        source.push_str("```\n");
    }
    doc::from_markdown(source.trim_end_matches('\n'))
}

fn plain(block: &LegacyBlock) -> String {
    block
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>()
}

/// The block's spans as Markdown inline text, built through the schema so the
/// serialiser writes the delimiters and the escaping.
fn inline(block: &LegacyBlock) -> String {
    let schema = doc::schema();
    let mut nodes = Vec::new();
    for span in &block.spans {
        if span.text.is_empty() {
            continue;
        }
        let mut marks = Vec::new();
        let mut add = |name: &str, on: bool| {
            if on && let Some(ty) = schema.mark_id(name) {
                marks.push(Mark::new(ty));
            }
        };
        add(md::STRONG, span.marks.bold);
        add(md::EM, span.marks.italic);
        add(md::CODE, span.marks.code);
        add(md::STRIKETHROUGH, span.marks.strikethrough);
        add(md::UNDERLINE, span.marks.underline);
        if let (Some(href), Some(ty)) = (span.link.as_ref(), schema.mark_id(md::LINK)) {
            marks.push(Mark::with_attrs(
                ty,
                Attrs::from_pairs([("href", href.as_str())]),
            ));
        }
        nodes.push(schema.text_marked(&span.text, MarkSet::from_marks(schema, marks)));
    }
    let paragraph = match schema.node(md::PARAGRAPH, nodes) {
        Ok(paragraph) => paragraph,
        Err(_) => return plain(block),
    };
    let slice = markraft_core::Slice::new(markraft_core::Fragment::from_node(paragraph), 1, 1);
    markraft_commonmark::to_markdown_fragment(schema, &slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON an earlier version wrote, written out by hand rather than by a
    /// type that no longer exists.
    fn legacy_json(blocks: &str) -> String {
        format!(
            r#"{{"version":2,"active_id":"a","notes":[{{"id":"a","document":{{"blocks":{blocks}}},"created_at":1,"updated_at":2}}],"preferences":{{"hotkey":"Alt+M"}}}}"#
        )
    }

    fn imported(blocks: &str) -> String {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), legacy_json(blocks)).unwrap();
        let library = read_library(file.path()).expect("a legacy library");
        assert_eq!(library.preferences.hotkey, "Alt+M");
        doc::to_markdown(&library.notes[0].document)
    }

    #[test]
    fn a_flat_document_becomes_the_markdown_it_stood_for() {
        let blocks = r#"[
            {"kind":{"Heading":1},"spans":[{"text":"Title","marks":{"bold":true}}]},
            {"kind":"Paragraph","spans":[{"text":"a "},{"text":"link","link":"https://x.example"}]},
            {"kind":"Bullet","spans":[{"text":"one"}]},
            {"kind":"Bullet","depth":1,"spans":[{"text":"nested"}]},
            {"kind":{"Task":{"checked":true}},"spans":[{"text":"done"}]},
            {"kind":"Quote","spans":[{"text":"quoted"}]},
            {"kind":"Divider","spans":[]},
            {"kind":{"Code":{"language":"rust"}},"spans":[{"text":"let a = 1;"}]},
            {"kind":{"Code":{"language":"rust"}},"spans":[{"text":"let b = 2;"}]}
        ]"#;
        assert_eq!(
            imported(blocks),
            "# **Title**\n\na [link](https://x.example)\n\n\
             - one\n  - nested\n- [x] done\n\n\
             > quoted\n\n---\n\n```rust\nlet a = 1;\nlet b = 2;\n```"
        );
    }

    #[test]
    fn an_empty_library_still_validates() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), legacy_json("[]")).unwrap();
        let library = read_library(file.path()).expect("a legacy library");
        assert_eq!(doc::plain_text(&library.notes[0].document), "");
    }

    #[test]
    fn a_damaged_file_is_an_error_rather_than_a_panic() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"{ not json").unwrap();
        assert!(read_library(file.path()).is_err());
    }
}
