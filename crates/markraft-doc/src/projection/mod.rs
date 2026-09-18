//! A flat, read-only view of a document for renderers and platform text APIs.
//!
//! Document positions are token offsets into a tree. Renderers and the text
//! APIs of every platform think in *lines* of text and, on Apple and Windows,
//! in UTF-16 code units. A [`Projection`] is the bridge: it flattens a document
//! into a `Vec<Line>` once and answers both kinds of question.
//!
//! # The data model
//!
//! * A [`Line`] is one textblock, or one block-level leaf such as a horizontal
//!   rule. It carries its token range, the ancestor chain down to and including
//!   its own block node — enough to draw list markers, quote bars and nesting —
//!   and the [`Run`]s its inline content breaks into.
//! * A [`Run`] is a stretch of text with one mark set, or one inline atom.
//! * A [`Row`] is the part of a line between two hard breaks. A node type
//!   counts as a hard break when it belongs to the [`LINE_BREAK_GROUP`] group.
//!   A group is used rather than a new schema flag so that P0 stays untouched
//!   and the convention remains pure schema data.
//!
//! # Offsets
//!
//! Inside a textblock every token is one `char` of the line's text: a text
//! leaf contributes one token and one `char` per Unicode scalar value, and an
//! inline atom contributes [`OBJECT_REPLACEMENT`] once per token it occupies.
//! A line break contributes `'\n'`. So within a line,
//! `char offset == pos - line.from` exactly, which is what makes
//! [`Projection::pos_to_line_offset`] and
//! [`Projection::line_offset_to_pos`] inverse.
//!
//! Across lines that no longer holds — the tokens that close one block and open
//! the next carry no text — so document-wide conversions go through the line
//! index rather than through arithmetic.
//!
//! # Caching
//!
//! [`projection`] is an extension holding a state field with an
//! `Arc<Projection>`. The field returns the previous value unchanged whenever a
//! transaction leaves the document alone, so a state that only moved the cursor
//! shares its predecessor's projection.

mod text;

pub use text::slice_to_plain_text;

use std::sync::{Arc, LazyLock};

use crate::attr::Attrs;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::schema::{NodeTypeId, Schema};
use crate::state::{EditorState, Extension, StateField, StateFieldConfig, Transaction};

/// The character that stands in for one token of an inline atom.
pub const OBJECT_REPLACEMENT: char = '\u{fffc}';

/// Node types in this group are treated as hard line breaks.
pub const LINE_BREAK_GROUP: &str = "line_break";

/// Whether `ty` is a hard line break, per [`LINE_BREAK_GROUP`].
pub fn is_line_break(schema: &Schema, ty: NodeTypeId) -> bool {
    schema.node_type(ty).in_group(LINE_BREAK_GROUP)
}

/// What kind of block a [`Line`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A block with inline content.
    Textblock,
    /// A block-level leaf, such as a horizontal rule.
    LeafBlock,
}

/// One node on the path from the document to a line's own block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestor {
    /// The node's type.
    pub node_type: NodeTypeId,
    /// The node's attributes.
    pub attrs: Attrs,
    /// The node's index in its parent.
    pub index: usize,
    /// The position directly before the node.
    pub before: usize,
}

/// What a [`Run`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunContent {
    /// A stretch of text.
    Text(String),
    /// One inline atom.
    Atom(Node),
}

/// A stretch of a line with one mark set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The run's content.
    pub content: RunContent,
    /// The marks every part of the run carries.
    pub marks: MarkSet,
    /// Start of the run, as a document position.
    pub from: usize,
    /// End of the run, as a document position.
    pub to: usize,
    /// Start of the run within the line's text, in `char`s.
    pub char_from: usize,
    /// End of the run within the line's text, in `char`s.
    pub char_to: usize,
}

/// The part of a line between two hard breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Start of the row, as a document position.
    pub from: usize,
    /// End of the row, as a document position.
    pub to: usize,
    /// Start of the row within the line's text, in `char`s.
    pub char_from: usize,
    /// End of the row within the line's text, in `char`s.
    pub char_to: usize,
}

/// One textblock or block-level leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Start of the line's content, as a document position. For a leaf block
    /// this is the position directly before the node.
    pub from: usize,
    /// End of the line's content. Equal to [`Line::from`] for a leaf block.
    pub to: usize,
    /// Whether the line stands for a textblock or a block-level leaf.
    pub kind: LineKind,
    /// The chain from the document's first level down to the line's own block,
    /// which is the last entry.
    pub ancestors: Vec<Ancestor>,
    /// The line's inline content.
    pub runs: Vec<Run>,
    /// The stretches between hard breaks. Always at least one entry.
    pub rows: Vec<Row>,
    /// Byte range of the line inside [`Projection::plain_text`].
    pub byte_start: usize,
    /// End of [`Line::byte_start`].
    pub byte_end: usize,
    /// `char` offset of the line inside [`Projection::plain_text`].
    pub char_start: usize,
    /// UTF-16 offset of the line inside [`Projection::plain_text`].
    pub utf16_start: usize,
}

impl Line {
    /// The line's own block node type.
    pub fn node_type(&self) -> Option<NodeTypeId> {
        self.ancestors.last().map(|a| a.node_type)
    }

    /// How many `char`s — equivalently, how many token positions — the line
    /// holds.
    pub fn len(&self) -> usize {
        self.to - self.from
    }

    /// Whether the line holds no content.
    pub fn is_empty(&self) -> bool {
        self.to == self.from
    }

    /// The nesting depth of the line's block, counting from the document.
    pub fn depth(&self) -> usize {
        self.ancestors.len()
    }
}

/// A flattened document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    lines: Vec<Line>,
    text: String,
    utf16_len: usize,
    doc_size: usize,
}

impl Projection {
    /// Flatten `doc`.
    pub fn of(doc: &Node, schema: &Schema) -> Projection {
        let mut builder = Builder {
            schema,
            lines: Vec::new(),
            text: String::new(),
            utf16_len: 0,
            char_len: 0,
        };
        builder.walk(doc, 0, &mut Vec::new());
        Projection {
            lines: builder.lines,
            text: builder.text,
            utf16_len: builder.utf16_len,
            doc_size: doc.content_size(),
        }
    }

    /// The lines, in document order.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// The line at `index`.
    pub fn line(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    /// The number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The whole document as text, lines joined by `'\n'`.
    pub fn plain_text(&self) -> &str {
        &self.text
    }

    /// The length of [`Projection::plain_text`] in UTF-16 code units.
    pub fn utf16_len(&self) -> usize {
        self.utf16_len
    }

    /// The size of the document this was built from.
    pub fn doc_size(&self) -> usize {
        self.doc_size
    }

    /// The text of one line.
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let line = self.lines.get(line)?;
        Some(&self.text[line.byte_start..line.byte_end])
    }
}

struct Builder<'a> {
    schema: &'a Schema,
    lines: Vec<Line>,
    text: String,
    utf16_len: usize,
    char_len: usize,
}

impl Builder<'_> {
    /// Walk the children of `node`, whose content starts at `content_start`.
    fn walk(&mut self, node: &Node, content_start: usize, ancestors: &mut Vec<Ancestor>) {
        let mut pos = content_start;
        for (index, child) in node.children().enumerate() {
            let ty = self.schema.node_type(child.type_id());
            let ancestor = Ancestor {
                node_type: child.type_id(),
                attrs: child.attrs().clone(),
                index,
                before: pos,
            };
            if ty.is_textblock() {
                ancestors.push(ancestor);
                self.push_textblock(child, pos + 1, ancestors);
                ancestors.pop();
            } else if ty.is_block() && child.is_container() {
                ancestors.push(ancestor);
                self.walk(child, pos + 1, ancestors);
                ancestors.pop();
            } else if ty.is_block() {
                ancestors.push(ancestor);
                self.push_leaf_block(pos, ancestors);
                ancestors.pop();
            }
            pos += child.node_size();
        }
    }

    fn start_line(&mut self) -> (usize, usize, usize) {
        if !self.lines.is_empty() {
            self.text.push('\n');
            self.utf16_len += 1;
            self.char_len += 1;
        }
        (self.text.len(), self.char_len, self.utf16_len)
    }

    fn push_leaf_block(&mut self, pos: usize, ancestors: &[Ancestor]) {
        let (byte_start, char_start, utf16_start) = self.start_line();
        self.lines.push(Line {
            from: pos,
            to: pos,
            kind: LineKind::LeafBlock,
            ancestors: ancestors.to_vec(),
            runs: Vec::new(),
            rows: vec![Row {
                from: pos,
                to: pos,
                char_from: 0,
                char_to: 0,
            }],
            byte_start,
            byte_end: byte_start,
            char_start,
            utf16_start,
        });
    }

    fn push_textblock(&mut self, block: &Node, from: usize, ancestors: &[Ancestor]) {
        let (byte_start, char_start, utf16_start) = self.start_line();
        let mut runs: Vec<Run> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut row_start = (from, 0usize);
        let mut pos = from;
        let mut offset = 0usize;

        for child in block.children() {
            let size = child.node_size();
            if let Some(text) = child.text() {
                self.text.push_str(text);
                self.utf16_len += text.encode_utf16().count();
                self.char_len += size;
                runs.push(Run {
                    content: RunContent::Text(text.to_string()),
                    marks: child.marks().clone(),
                    from: pos,
                    to: pos + size,
                    char_from: offset,
                    char_to: offset + size,
                });
            } else {
                let is_break = is_line_break(self.schema, child.type_id());
                let filler = if is_break { '\n' } else { OBJECT_REPLACEMENT };
                for _ in 0..size {
                    self.text.push(filler);
                }
                self.utf16_len += size;
                self.char_len += size;
                runs.push(Run {
                    content: RunContent::Atom(child.clone()),
                    marks: child.marks().clone(),
                    from: pos,
                    to: pos + size,
                    char_from: offset,
                    char_to: offset + size,
                });
                if is_break {
                    rows.push(Row {
                        from: row_start.0,
                        to: pos,
                        char_from: row_start.1,
                        char_to: offset,
                    });
                    row_start = (pos + size, offset + size);
                }
            }
            pos += size;
            offset += size;
        }

        rows.push(Row {
            from: row_start.0,
            to: pos,
            char_from: row_start.1,
            char_to: offset,
        });
        self.lines.push(Line {
            from,
            to: pos,
            kind: LineKind::Textblock,
            ancestors: ancestors.to_vec(),
            runs,
            rows,
            byte_start,
            byte_end: self.text.len(),
            char_start,
            utf16_start,
        });
    }
}

static PROJECTION_FIELD: LazyLock<StateField<Arc<Projection>>> = LazyLock::new(|| {
    StateField::define(
        StateFieldConfig::new(
            |state: &EditorState| Arc::new(Projection::of(state.doc(), state.schema())),
            update_projection,
        )
        .compare(Arc::ptr_eq),
    )
});

fn update_projection(value: &Arc<Projection>, tr: &Transaction) -> Arc<Projection> {
    if !tr.doc_changed() {
        return value.clone();
    }
    Arc::new(Projection::of(tr.new_doc(), tr.start_state().schema()))
}

/// The field the cached projection lives in.
pub fn projection_field() -> &'static StateField<Arc<Projection>> {
    &PROJECTION_FIELD
}

/// The extension that caches a projection in the state.
pub fn projection() -> Extension {
    projection_field().extension()
}

/// The cached projection of `state`, when the extension is configured.
pub fn cached_projection(state: &EditorState) -> Option<&Arc<Projection>> {
    state.field(projection_field())
}

/// The projection of `state`, using the cached one when there is one and
/// building a fresh one otherwise.
pub fn projection_of(state: &EditorState) -> Arc<Projection> {
    match cached_projection(state) {
        Some(projection) => projection.clone(),
        None => Arc::new(Projection::of(state.doc(), state.schema())),
    }
}
