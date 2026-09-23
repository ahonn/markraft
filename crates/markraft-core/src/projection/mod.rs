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
//!   and the [`Run`]s its inline content breaks into. A line is a
//!   [`LineStart`] plus a shared body measured from that start, so a line an
//!   edit only moved keeps its body.
//! * A [`Run`] is a stretch of text with one mark set, or one inline atom.
//! * A [`Row`] is the part of a line between two hard breaks: atoms whose type
//!   declares [`BreakKind::Hard`]. An atom of a [`BreakKind::Soft`] type stays
//!   within its row and reads as a space.
//!
//! # Offsets
//!
//! Text and atoms contribute visible characters. Non-atomic inline containers
//! contribute their content recursively, inheriting marks, while their opening
//! and closing tokens stay invisible. Position conversions use the projected
//! runs rather than `pos - line.from()`, and each visible boundary has one
//! canonical caret position. This prevents extra arrow-key stops at style edges.
//! Runs and rows record their bounds relative to the line's start;
//! [`Line::abs`] turns them into document positions.
//!
//! # Caching
//!
//! [`projection`] is an extension holding a state field with an
//! `Arc<Projection>`. The field returns the previous value unchanged whenever a
//! transaction leaves the document alone, so a state that only moved the cursor
//! shares its predecessor's projection. When the document did change, the
//! field derives the new projection with [`Projection::update`], which rebuilds
//! only the lines of the changed region.

mod text;

pub use text::slice_to_plain_text;

use std::ops::Range;
use std::sync::{Arc, LazyLock};

use crate::attr::Attrs;
use crate::mark::MarkSet;
use crate::node::{Node, diff_region};
use crate::schema::{BreakKind, NodeTypeId, Schema};
use crate::state::{EditorState, Extension, StateField, StateFieldConfig, Transaction};

/// The character that stands in for one token of an inline atom.
pub const OBJECT_REPLACEMENT: char = '\u{fffc}';

/// The character each token of a non-text inline leaf of type `ty` reads as:
/// `'\n'` for a hard break, a space for a soft one, and
/// [`OBJECT_REPLACEMENT`] for anything else.
pub(crate) fn atom_filler(schema: &Schema, ty: NodeTypeId) -> char {
    match schema.node_type(ty).break_kind() {
        Some(BreakKind::Hard) => '\n',
        Some(BreakKind::Soft) => ' ',
        None => OBJECT_REPLACEMENT,
    }
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
    /// How far the line's start ([`Line::from`]) lies past the position
    /// directly before the node. [`Line::ancestor_before`] turns it back into
    /// a document position.
    pub before_offset: usize,
}

/// What a [`Run`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunContent {
    /// A stretch of text.
    Text(Arc<str>),
    /// One inline atom.
    Atom(Node),
}

/// A stretch of a line with one mark set.
///
/// `start` and `end` are relative to the line's start; [`Line::abs`] turns
/// them into document positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The run's content.
    pub content: RunContent,
    /// The marks every part of the run carries.
    pub marks: MarkSet,
    /// Start of the run, in tokens from the line's start.
    pub start: usize,
    /// End of the run, in tokens from the line's start.
    pub end: usize,
    /// Start of the run within the line's text, in `char`s.
    pub char_from: usize,
    /// End of the run within the line's text, in `char`s.
    pub char_to: usize,
}

/// The part of a line between two hard breaks.
///
/// `start` and `end` are relative to the line's start; [`Line::abs`] turns
/// them into document positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Start of the row, in tokens from the line's start.
    pub start: usize,
    /// End of the row, in tokens from the line's start.
    pub end: usize,
    /// Start of the row within the line's text, in `char`s.
    pub char_from: usize,
    /// End of the row within the line's text, in `char`s.
    pub char_to: usize,
}

/// Where a line starts, in each of the coordinates a [`Projection`] answers
/// in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineStart {
    /// The document position of the line's content. For a leaf block this is
    /// the position directly before the node.
    pub pos: usize,
    /// Byte offset of the line inside [`Projection::plain_text`].
    pub byte: usize,
    /// `char` offset of the line inside [`Projection::plain_text`].
    pub char: usize,
    /// UTF-16 offset of the line inside [`Projection::plain_text`].
    pub utf16: usize,
}

/// One textblock or block-level leaf.
///
/// A line is a [`LineStart`] and a shared body that holds everything relative
/// to it, so moving a line — because an edit before it changed the document's
/// length — only moves its start. Cloning a line is cheap, and two lines whose
/// bodies are the same allocation ([`Line::same_body`]) hold the same content
/// laid out the same way.
#[derive(Debug, Clone)]
pub struct Line {
    start: LineStart,
    body: Arc<LineBody>,
}

/// Everything about a line that does not depend on where it starts.
#[derive(Debug, PartialEq, Eq)]
struct LineBody {
    /// Tokens from the line's start to its end: the textblock's content size,
    /// or zero for a leaf block.
    size: usize,
    kind: LineKind,
    ancestors: Vec<Ancestor>,
    runs: Vec<Run>,
    /// Always at least one entry.
    rows: Vec<Row>,
    /// Canonical editable position of each visible character boundary, in
    /// tokens from the line's start.
    positions: Vec<usize>,
    byte_len: usize,
    char_len: usize,
    utf16_len: usize,
}

impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && (self.same_body(other) || self.body == other.body)
    }
}

impl Eq for Line {}

/// A point in [`Projection::plain_text`], in every unit it is measured in.
#[derive(Debug, Clone, Copy, Default)]
struct TextPoint {
    byte: usize,
    char: usize,
    utf16: usize,
}

impl Line {
    /// Where the line starts.
    pub fn start(&self) -> LineStart {
        self.start
    }

    /// Start of the line's content, as a document position. For a leaf block
    /// this is the position directly before the node.
    pub fn from(&self) -> usize {
        self.start.pos
    }

    /// End of the line's content. Equal to [`Line::from`] for a leaf block.
    pub fn to(&self) -> usize {
        self.start.pos + self.body.size
    }

    /// Whether the line stands for a textblock or a block-level leaf.
    pub fn kind(&self) -> LineKind {
        self.body.kind
    }

    /// The chain from the document's first level down to the line's own block,
    /// which is the last entry.
    pub fn ancestors(&self) -> &[Ancestor] {
        &self.body.ancestors
    }

    /// The line's inline content.
    pub fn runs(&self) -> &[Run] {
        &self.body.runs
    }

    /// The stretches between hard breaks. Always at least one entry.
    pub fn rows(&self) -> &[Row] {
        &self.body.rows
    }

    /// The document position `rel` tokens past the line's start: where a
    /// [`Run`] or [`Row`] boundary lies in the document.
    pub fn abs(&self, rel: usize) -> usize {
        self.start.pos + rel
    }

    /// The position directly before the ancestor at `index` in
    /// [`Line::ancestors`].
    ///
    /// # Panics
    ///
    /// When `index` is out of range, as indexing the slice would.
    pub fn ancestor_before(&self, index: usize) -> usize {
        self.start.pos - self.body.ancestors[index].before_offset
    }

    /// The position directly before the line's own block node: the last of
    /// [`Line::ancestors`]. For a leaf block this is [`Line::from`]; for a
    /// textblock it is one token before it. `None` only for a line without
    /// ancestors, which a projection never builds.
    pub fn block_before(&self) -> Option<usize> {
        let own = self.depth().checked_sub(1)?;
        Some(self.ancestor_before(own))
    }

    /// Byte range of the line inside [`Projection::plain_text`].
    pub fn byte_range(&self) -> Range<usize> {
        self.start.byte..self.start.byte + self.body.byte_len
    }

    /// `char` offset of the line inside [`Projection::plain_text`].
    pub fn char_start(&self) -> usize {
        self.start.char
    }

    /// UTF-16 offset of the line inside [`Projection::plain_text`].
    pub fn utf16_start(&self) -> usize {
        self.start.utf16
    }

    /// Whether `self` and `other` share one body: the same content, laid out
    /// the same way, wherever each starts. A projection updated with
    /// [`Projection::update`] keeps the body of every line the edit did not
    /// reach, so a renderer can use this to keep what it built for a line.
    pub fn same_body(&self, other: &Line) -> bool {
        Arc::ptr_eq(&self.body, &other.body)
    }

    /// The line's own block node type.
    pub fn node_type(&self) -> Option<NodeTypeId> {
        self.body.ancestors.last().map(|a| a.node_type)
    }

    /// How many visible Unicode scalar values the line holds. Transparent
    /// inline-container boundaries contribute no characters.
    pub fn len(&self) -> usize {
        self.body.positions.len().saturating_sub(1)
    }

    /// Whether the line holds no content.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert a visible character offset to its canonical editable position.
    pub fn offset_to_pos(&self, offset: usize) -> Option<usize> {
        self.body.positions.get(offset).map(|rel| self.abs(*rel))
    }

    /// Convert a document position to the corresponding visible character
    /// offset. Structural boundaries share an offset with their adjacent text.
    pub fn pos_to_offset(&self, pos: usize) -> Option<usize> {
        if !(self.from()..=self.to()).contains(&pos) {
            return None;
        }
        let rel = pos - self.from();
        for run in &self.body.runs {
            if rel <= run.end {
                return Some(run.char_from + rel.saturating_sub(run.start));
            }
        }
        Some(self.len())
    }

    /// The nesting depth of the line's block, counting from the document.
    pub fn depth(&self) -> usize {
        self.body.ancestors.len()
    }

    /// Whether `pos` is one of the line's canonical editable positions.
    fn is_position(&self, pos: usize) -> bool {
        pos.checked_sub(self.from())
            .is_some_and(|rel| self.body.positions.binary_search(&rel).is_ok())
    }

    fn text_start(&self) -> TextPoint {
        TextPoint {
            byte: self.start.byte,
            char: self.start.char,
            utf16: self.start.utf16,
        }
    }

    fn text_end(&self) -> TextPoint {
        TextPoint {
            byte: self.start.byte + self.body.byte_len,
            char: self.start.char + self.body.char_len,
            utf16: self.start.utf16 + self.body.utf16_len,
        }
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
        let mut builder = Builder::new(schema);
        builder.walk(doc, 0, &mut Vec::new());
        Projection {
            lines: builder.lines,
            text: builder.text,
            utf16_len: builder.utf16_len,
            doc_size: doc.content_size(),
        }
    }

    /// The projection of `new_doc`, given that `self` is the projection of
    /// `old_doc` and `new_doc` was derived from it.
    ///
    /// The result equals [`Projection::of`] on `new_doc`. It is built by
    /// rebuilding only the lines of the smallest run of sibling blocks that
    /// holds every difference between the two documents, as far as they can
    /// be told apart by identity ([`Node::ptr_eq`]): the lines before that
    /// region are kept as they are, and the lines after it are moved. A moved
    /// line keeps its body unless one of its ancestors encloses the region,
    /// since that ancestor's distance to the line changed, or it lies under
    /// the region's parent and the region changed how many children the parent
    /// has, since its index there changed. When the documents share no child
    /// at the top, this is a full rebuild.
    pub fn update(&self, schema: &Schema, old_doc: &Node, new_doc: &Node) -> Projection {
        if old_doc.ptr_eq(new_doc) {
            return self.clone();
        }
        debug_assert_eq!(
            self.doc_size,
            old_doc.content_size(),
            "a projection updated from a document it was not built from"
        );

        // The region is a run of children of one parent, and everything above
        // it is shared markup, so the steps down to the parent are the
        // ancestors of each new line.
        let region = diff_region(old_doc, new_doc, schema);
        let mut frames = Vec::new();
        let (mut old_parent, mut new_parent) = (old_doc, new_doc);
        let mut content_start = 0;
        for step in &region.path {
            let before =
                content_start + children_size(&new_parent.content().as_slice()[..step.index]);
            frames.push(Frame {
                node_type: step.new.type_id(),
                attrs: step.new.attrs().clone(),
                index: step.index,
                before,
            });
            (old_parent, new_parent) = (&step.old, &step.new);
            content_start = before + 1;
        }
        let first = region.new.start;
        let old_region = &old_parent.content().as_slice()[region.old.clone()];
        let new_region = &new_parent.content().as_slice()[region.new.clone()];
        let lo = content_start + children_size(&new_parent.content().as_slice()[..first]);
        let old_hi = lo + children_size(old_region);
        let new_hi = lo + children_size(new_region);

        // Lines start strictly increasing, and every line of the region starts
        // in `lo..old_hi`: a leaf block at the position before it, a textblock
        // one token later, and nothing at the region's end, where either the
        // parent closes or the next sibling — perhaps a leaf block — begins.
        let run_start = self.lines.partition_point(|line| line.from() < lo);
        let run_end = self.lines.partition_point(|line| line.from() < old_hi);

        let mut builder = Builder::new(schema);
        let mut pos = lo;
        for (offset, child) in new_region.iter().enumerate() {
            builder.visit(first + offset, child, pos, &mut frames);
            pos += child.node_size();
        }

        // Splice the text, carrying the '\n' that joins the run to its
        // neighbours: the one before it when there is a line before it, else
        // the one after it when there is a line after it.
        let previous = run_start.checked_sub(1).map(|index| &self.lines[index]);
        let has_next = run_end < self.lines.len();
        let rebuilt = !builder.lines.is_empty();
        let joiner = usize::from(rebuilt && (previous.is_some() || has_next));
        let (span_start, span_end) = match previous {
            Some(previous) => {
                let end = match run_end > run_start {
                    true => self.lines[run_end - 1].text_end(),
                    false => previous.text_end(),
                };
                (previous.text_end(), end)
            }
            None if has_next => (TextPoint::default(), self.lines[run_end].text_start()),
            None => (
                TextPoint::default(),
                self.lines
                    .last()
                    .map_or_else(TextPoint::default, Line::text_end),
            ),
        };
        let mut replacement = String::with_capacity(builder.text.len() + joiner);
        match previous {
            Some(_) if rebuilt => {
                replacement.push('\n');
                replacement.push_str(&builder.text);
            }
            None if joiner == 1 => {
                replacement.push_str(&builder.text);
                replacement.push('\n');
            }
            _ => replacement.push_str(&builder.text),
        }
        let replaced_end = TextPoint {
            byte: span_start.byte + replacement.len(),
            char: span_start.char + builder.char_len + joiner,
            utf16: span_start.utf16 + builder.utf16_len + joiner,
        };
        let mut text = self.text.clone();
        text.replace_range(span_start.byte..span_end.byte, &replacement);

        let mut lines =
            Vec::with_capacity(run_start + builder.lines.len() + self.lines.len() - run_end);
        lines.extend_from_slice(&self.lines[..run_start]);
        let base = match previous {
            Some(_) => TextPoint {
                byte: span_start.byte + 1,
                char: span_start.char + 1,
                utf16: span_start.utf16 + 1,
            },
            None => TextPoint::default(),
        };
        lines.extend(builder.lines.into_iter().map(|mut line| {
            line.start.byte += base.byte;
            line.start.char += base.char;
            line.start.utf16 += base.utf16;
            line
        }));
        let splice = Splice {
            lo,
            old_hi,
            new_hi,
            old_text: span_end,
            new_text: replaced_end,
            level: frames.len(),
            old_count: old_region.len(),
            new_count: new_region.len(),
        };
        lines.extend(self.lines[run_end..].iter().map(|line| line.moved(&splice)));

        Projection {
            lines,
            text,
            utf16_len: self.utf16_len - (span_end.utf16 - span_start.utf16)
                + (replaced_end.utf16 - span_start.utf16),
            doc_size: new_doc.content_size(),
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
        Some(&self.text[line.byte_range()])
    }
}

/// How [`Projection::update`] replaced a region: the lines after it move by
/// what these describe.
struct Splice {
    /// Where the region starts, in both documents.
    lo: usize,
    /// Where the region ended in the old document.
    old_hi: usize,
    /// Where its replacement ends in the new document.
    new_hi: usize,
    /// Where the region's lines ended in the old text.
    old_text: TextPoint,
    /// Where the replacement's lines end in the new text.
    new_text: TextPoint,
    /// The depth of the region's children in a line's ancestors: the number
    /// of ancestors the region's parent and the nodes above it take up.
    level: usize,
    /// How many children the region held.
    old_count: usize,
    /// How many children its replacement holds.
    new_count: usize,
}

impl Line {
    /// This line, which lay after the region `splice` replaced, moved to
    /// follow the replacement.
    ///
    /// An ancestor that starts before the region's start encloses the region,
    /// so it stays where it was while the line moves; its offset changes. A
    /// line under the region's parent also sits in a later sibling of the
    /// region, whose index changes by as many children as the region gained.
    /// Those are the only parts of the body that change.
    fn moved(&self, splice: &Splice) -> Line {
        let pos = self.start.pos - splice.old_hi + splice.new_hi;
        let encloses = |ancestor: &Ancestor| self.start.pos - ancestor.before_offset < splice.lo;
        let ancestors = &self.body.ancestors;
        let under_parent = splice
            .level
            .checked_sub(1)
            .is_none_or(|parent| ancestors.get(parent).is_some_and(encloses));
        let reindex = under_parent && splice.old_count != splice.new_count;
        let body = if !reindex && (pos == self.start.pos || !ancestors.iter().any(encloses)) {
            self.body.clone()
        } else {
            let ancestors = ancestors
                .iter()
                .enumerate()
                .map(|(depth, ancestor)| {
                    let mut ancestor = ancestor.clone();
                    if encloses(&ancestor) {
                        ancestor.before_offset = ancestor.before_offset + pos - self.start.pos;
                    }
                    if reindex && depth == splice.level {
                        ancestor.index = ancestor.index + splice.new_count - splice.old_count;
                    }
                    ancestor
                })
                .collect();
            Arc::new(LineBody {
                size: self.body.size,
                kind: self.body.kind,
                ancestors,
                runs: self.body.runs.clone(),
                rows: self.body.rows.clone(),
                positions: self.body.positions.clone(),
                byte_len: self.body.byte_len,
                char_len: self.body.char_len,
                utf16_len: self.body.utf16_len,
            })
        };
        Line {
            start: LineStart {
                pos,
                byte: self.start.byte - splice.old_text.byte + splice.new_text.byte,
                char: self.start.char - splice.old_text.char + splice.new_text.char,
                utf16: self.start.utf16 - splice.old_text.utf16 + splice.new_text.utf16,
            },
            body,
        }
    }
}

fn children_size(children: &[Node]) -> usize {
    children.iter().map(Node::node_size).sum()
}

/// An ancestor while a walk is inside it, with its absolute position.
struct Frame {
    node_type: NodeTypeId,
    attrs: Attrs,
    index: usize,
    before: usize,
}

impl Frame {
    fn ancestor(&self, line_from: usize) -> Ancestor {
        Ancestor {
            node_type: self.node_type,
            attrs: self.attrs.clone(),
            index: self.index,
            before_offset: line_from - self.before,
        }
    }
}

/// Builds lines and their joined text. Line starts are measured from the start
/// of the builder's own text; positions are document positions.
struct Builder<'a> {
    schema: &'a Schema,
    lines: Vec<Line>,
    text: String,
    utf16_len: usize,
    char_len: usize,
}

impl<'a> Builder<'a> {
    fn new(schema: &'a Schema) -> Builder<'a> {
        Builder {
            schema,
            lines: Vec::new(),
            text: String::new(),
            utf16_len: 0,
            char_len: 0,
        }
    }

    /// Walk the children of `node`, whose content starts at `content_start`.
    fn walk(&mut self, node: &Node, content_start: usize, frames: &mut Vec<Frame>) {
        let mut pos = content_start;
        for (index, child) in node.children().enumerate() {
            self.visit(index, child, pos, frames);
            pos += child.node_size();
        }
    }

    /// Emit the lines of `child`, the `index`th child of its parent, which
    /// sits directly after `pos`.
    fn visit(&mut self, index: usize, child: &Node, pos: usize, frames: &mut Vec<Frame>) {
        let ty = self.schema.node_type(child.type_id());
        if !ty.is_block() {
            return;
        }
        frames.push(Frame {
            node_type: child.type_id(),
            attrs: child.attrs().clone(),
            index,
            before: pos,
        });
        if ty.is_textblock() {
            self.push_textblock(child, pos + 1, frames);
        } else if child.is_container() {
            self.walk(child, pos + 1, frames);
        } else {
            self.push_leaf_block(pos, frames);
        }
        frames.pop();
    }

    fn start_line(&mut self, pos: usize) -> LineStart {
        if !self.lines.is_empty() {
            self.text.push('\n');
            self.utf16_len += 1;
            self.char_len += 1;
        }
        LineStart {
            pos,
            byte: self.text.len(),
            char: self.char_len,
            utf16: self.utf16_len,
        }
    }

    fn push_leaf_block(&mut self, pos: usize, frames: &[Frame]) {
        let start = self.start_line(pos);
        self.lines.push(Line {
            start,
            body: Arc::new(LineBody {
                size: 0,
                kind: LineKind::LeafBlock,
                ancestors: frames.iter().map(|frame| frame.ancestor(pos)).collect(),
                runs: Vec::new(),
                rows: vec![Row {
                    start: 0,
                    end: 0,
                    char_from: 0,
                    char_to: 0,
                }],
                positions: vec![0],
                byte_len: 0,
                char_len: 0,
                utf16_len: 0,
            }),
        });
    }

    fn push_textblock(&mut self, block: &Node, from: usize, frames: &[Frame]) {
        let start = self.start_line(from);
        let mut runs = Vec::new();
        let mut positions = vec![0];
        self.inline_runs(block, 0, &MarkSet::empty(), &mut runs, &mut positions);
        let mut rows = Vec::new();
        let mut row_offset = 0;
        for run in &runs {
            if matches!(&run.content, RunContent::Atom(node)
                if self.schema.node_type(node.type_id()).break_kind() == Some(BreakKind::Hard))
            {
                rows.push(Row {
                    start: positions[row_offset],
                    end: positions[run.char_from],
                    char_from: row_offset,
                    char_to: run.char_from,
                });
                row_offset = run.char_to;
            }
        }
        rows.push(Row {
            start: positions[row_offset],
            end: *positions.last().expect("initial boundary"),
            char_from: row_offset,
            char_to: positions.len() - 1,
        });
        self.lines.push(Line {
            start,
            body: Arc::new(LineBody {
                size: block.content_size(),
                kind: LineKind::Textblock,
                ancestors: frames.iter().map(|frame| frame.ancestor(from)).collect(),
                runs,
                rows,
                positions,
                byte_len: self.text.len() - start.byte,
                char_len: self.char_len - start.char,
                utf16_len: self.utf16_len - start.utf16,
            }),
        });
    }

    /// Collect the runs of `parent`'s inline content, whose first token lies
    /// `from` tokens past the line's start.
    fn inline_runs(
        &mut self,
        parent: &Node,
        from: usize,
        inherited: &MarkSet,
        runs: &mut Vec<Run>,
        positions: &mut Vec<usize>,
    ) {
        let mut pos = from;
        for child in parent.children() {
            let size = child.node_size();
            let marks = child.marks().iter().fold(inherited.clone(), |marks, mark| {
                marks.add(self.schema, mark.clone())
            });
            if child.is_container() && !self.schema.node_type(child.type_id()).is_atom() {
                *positions.last_mut().expect("initial boundary") = pos + 1;
                self.inline_runs(child, pos + 1, &marks, runs, positions);
            } else {
                let offset = positions.len() - 1;
                let text = child.text().map(str::to_string).unwrap_or_else(|| {
                    let filler = atom_filler(self.schema, child.type_id());
                    std::iter::repeat_n(filler, size).collect()
                });
                self.utf16_len += text.encode_utf16().count();
                self.char_len += size;
                self.text.push_str(&text);
                *positions.last_mut().expect("initial boundary") = pos;
                positions.extend(pos + 1..=pos + size);
                runs.push(Run {
                    content: match child.text() {
                        Some(_) => RunContent::Text(text.into()),
                        None => RunContent::Atom(child.clone()),
                    },
                    marks,
                    start: pos,
                    end: pos + size,
                    char_from: offset,
                    char_to: offset + size,
                });
            }
            pos += size;
        }
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
    let start = tr.start_state();
    Arc::new(value.update(start.schema(), start.doc(), tr.new_doc()))
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
