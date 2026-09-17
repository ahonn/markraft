//! Platform-independent structured rich text editing with explicit coordinate conversion.
//! Document positions use UTF-8 byte offsets at extended grapheme boundaries. Platform
//! input offsets use UTF-16 code units over the document's newline-separated plain text.

mod fragment;
mod html;
mod markdown;

use serde::{Deserialize, Serialize};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

impl Marks {
    fn toggle(&mut self, mark: Mark) {
        match mark {
            Mark::Bold => self.bold = !self.bold,
            Mark::Italic => self.italic = !self.italic,
            Mark::Code => self.code = !self.code,
            Mark::Strikethrough => self.strikethrough = !self.strikethrough,
            Mark::Underline => self.underline = !self.underline,
        }
    }

    pub fn has(self, mark: Mark) -> bool {
        match mark {
            Mark::Bold => self.bold,
            Mark::Italic => self.italic,
            Mark::Code => self.code,
            Mark::Strikethrough => self.strikethrough,
            Mark::Underline => self.underline,
        }
    }

    fn set(&mut self, mark: Mark, value: bool) {
        match mark {
            Mark::Bold => self.bold = value,
            Mark::Italic => self.italic = value,
            Mark::Code => self.code = value,
            Mark::Strikethrough => self.strikethrough = value,
            Mark::Underline => self.underline = value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Code,
    Strikethrough,
    Underline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading(u8),
    Bullet,
    /// Numbered from its position in a run of ordered blocks; see
    /// [`Document::ordinal`].
    Ordered,
    Task {
        checked: bool,
    },
    Quote,
    /// One line of a code block. Adjacent lines with the same language form one
    /// block; they hold plain text, without marks or input rules.
    Code {
        language: String,
    },
    /// A horizontal rule. It never holds text: typing into one turns it back into a
    /// paragraph, so it stays an ordinary, empty line for selection and navigation.
    Divider,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub marks: Marks,
    /// The URL this text links to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    /// Zero-based nesting level for lists and block quotes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth: u8,
    pub spans: Vec<Span>,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

impl BlockKind {
    pub fn is_list(&self) -> bool {
        matches!(self, Self::Bullet | Self::Ordered | Self::Task { .. })
    }

    fn supports_depth(&self) -> bool {
        self.is_list() || *self == Self::Quote
    }
}

impl Block {
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.spans.iter().map(|span| span.text.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.text.is_empty())
    }

    pub fn marks_at(&self, byte: usize) -> Marks {
        let mut offset = 0;
        for span in &self.spans {
            offset += span.text.len();
            if byte <= offset {
                return span.marks;
            }
        }
        self.spans.last().map_or(Marks::default(), |s| s.marks)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            blocks: vec![Block::default()],
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SerializedDocument {
    version: u32,
    document: Document,
}

impl Document {
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The 1-based number within the same nesting level, skipping child items.
    pub fn ordinal(&self, index: usize) -> Option<usize> {
        let block = self.blocks.get(index)?;
        if block.kind != BlockKind::Ordered {
            return None;
        }
        let mut ordinal = 1;
        for previous in self.blocks[..index].iter().rev() {
            if !previous.kind.is_list() || previous.depth < block.depth {
                break;
            }
            if previous.depth == block.depth {
                if previous.kind != BlockKind::Ordered {
                    break;
                }
                ordinal += 1;
            }
        }
        Some(ordinal)
    }

    /// Adjacent code lines with the same language form one code block.
    pub fn code_block_range(&self, index: usize) -> Option<Range<usize>> {
        let kind = &self.blocks.get(index)?.kind;
        if !matches!(kind, BlockKind::Code { .. }) {
            return None;
        }
        let start = self.blocks[..index]
            .iter()
            .rposition(|block| block.kind != *kind)
            .map_or(0, |index| index + 1);
        let end = index
            + self.blocks[index..]
                .iter()
                .take_while(|block| block.kind == *kind)
                .count();
        Some(start..end)
    }

    /// Normalize empty documents, adjacent runs and heading levels. Newlines in supplied
    /// spans become structural paragraph boundaries; text is never silently discarded.
    pub fn normalize(&mut self) {
        let mut blocks = Vec::new();
        for block in std::mem::take(&mut self.blocks) {
            let kind = match block.kind {
                BlockKind::Heading(level) => BlockKind::Heading(level.clamp(1, 6)),
                BlockKind::Divider if !block.is_empty() => BlockKind::Paragraph,
                kind => kind,
            };
            let code = matches!(kind, BlockKind::Code { .. });
            let mut current = Block {
                kind: kind.clone(),
                depth: if kind.supports_depth() {
                    block.depth
                } else {
                    0
                },
                spans: Vec::new(),
            };
            for span in block.spans {
                let marks = if code { Marks::default() } else { span.marks };
                let link = span.link.as_deref().filter(|_| !code);
                for (index, part) in span.text.split('\n').enumerate() {
                    if index > 0 {
                        blocks.push(current);
                        current = Block {
                            depth: 0,
                            kind: if code {
                                kind.clone()
                            } else {
                                BlockKind::Paragraph
                            },
                            spans: Vec::new(),
                        };
                    }
                    push_linked_span(&mut current.spans, part, marks, link);
                }
            }
            blocks.push(current);
        }
        if blocks.is_empty() {
            blocks.push(Block::default());
        }
        let mut list_depths = Vec::new();
        for block in &mut blocks {
            if block.kind.is_list() {
                while list_depths
                    .last()
                    .is_some_and(|depth| *depth >= block.depth)
                {
                    list_depths.pop();
                }
                let original_depth = block.depth;
                block.depth = list_depths.len().min(usize::from(u8::MAX)) as u8;
                list_depths.push(original_depth);
            } else {
                list_depths.clear();
            }
        }
        self.blocks = blocks;
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&SerializedDocument {
            version: 1,
            document: self.clone(),
        })
    }

    pub fn from_json(source: &str) -> Result<Self, serde_json::Error> {
        let serialized: SerializedDocument = serde_json::from_str(source)?;
        if serialized.version != 1 {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unsupported document version: {}",
                serialized.version
            )));
        }
        let mut document = serialized.document;
        document.normalize();
        Ok(document)
    }

    pub fn clamp_position(&self, position: Position) -> Position {
        if self.blocks.is_empty() {
            return Position::default();
        }
        let block = position.block.min(self.blocks.len() - 1);
        let text = self.blocks[block].text();
        Position {
            block,
            byte: floor_grapheme(&text, position.byte.min(text.len())),
        }
    }

    pub fn position_to_utf16(&self, position: Position) -> usize {
        self.position_to_utf16_raw(self.clamp_position(position))
    }

    fn position_to_utf16_raw(&self, position: Position) -> usize {
        self.blocks
            .iter()
            .take(position.block)
            .map(|b| b.text().encode_utf16().count() + 1)
            .sum::<usize>()
            + self.blocks[position.block].text()[..position.byte]
                .encode_utf16()
                .count()
    }

    pub fn utf16_to_position(&self, offset: usize) -> Position {
        self.clamp_position(self.utf16_to_scalar_position(offset))
    }

    fn utf16_to_scalar_position(&self, offset: usize) -> Position {
        let mut remaining = offset;
        for (index, block) in self.blocks.iter().enumerate() {
            let text = block.text();
            let length = text.encode_utf16().count();
            if remaining <= length {
                return Position {
                    block: index,
                    byte: utf16_to_byte(&text, remaining),
                };
            }
            remaining -= length + 1;
        }
        let block = self.blocks.len().saturating_sub(1);
        Position {
            block,
            byte: self.blocks.get(block).map_or(0, Block::len),
        }
    }

    /// The plain text of a range, walking only the blocks it covers. The range is ordered
    /// and clamped.
    pub fn text_in(&self, range: Range<Position>) -> String {
        let (start, end) = self.ordered_range(range);
        let last = end.block - start.block;
        let mut text = String::new();
        for (offset, block) in self.blocks[start.block..=end.block].iter().enumerate() {
            if offset > 0 {
                text.push('\n');
            }
            let from = if offset == 0 { start.byte } else { 0 };
            let to = if offset == last {
                end.byte
            } else {
                block.len()
            };
            push_span_text(&mut text, &block.spans, from..to);
        }
        text
    }

    /// Order a range and clamp both ends into the document.
    fn ordered_range(&self, range: Range<Position>) -> (Position, Position) {
        (
            self.clamp_position(range.start.min(range.end)),
            self.clamp_position(range.start.max(range.end)),
        )
    }

    fn global_byte(&self, position: Position) -> usize {
        self.global_byte_raw(self.clamp_position(position))
    }

    fn global_byte_raw(&self, position: Position) -> usize {
        self.blocks
            .iter()
            .take(position.block)
            .map(|b| b.len() + 1)
            .sum::<usize>()
            + position.byte
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub block: usize,
    pub byte: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
}

impl Selection {
    pub fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    pub fn ordered(self) -> (Position, Position) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

/// One side of a [`PositionMap`]: the prefix sums of `block.len() + 1`, enough to convert
/// between a [`Position`] and a global byte offset without retaining the text. A
/// normalized document always holds at least one block, so `starts` holds at least two
/// entries.
#[derive(Clone, Debug)]
struct BlockOffsets {
    /// The global start offset of every block, plus a terminator past the last one.
    starts: Vec<usize>,
}

impl BlockOffsets {
    fn of(document: &Document) -> Self {
        let mut starts = Vec::with_capacity(document.blocks.len() + 1);
        let mut offset = 0;
        for block in &document.blocks {
            starts.push(offset);
            offset += block.len() + 1;
        }
        starts.push(offset);
        Self { starts }
    }

    fn blocks(&self) -> usize {
        self.starts.len() - 1
    }

    fn block_len(&self, block: usize) -> usize {
        self.starts[block + 1] - self.starts[block] - 1
    }

    /// Clamp `position` into range and convert it to a global byte offset.
    fn global_byte(&self, position: Position) -> usize {
        let block = position.block.min(self.blocks() - 1);
        self.starts[block] + position.byte.min(self.block_len(block))
    }

    fn position(&self, offset: usize) -> Position {
        let block = self
            .starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
            .min(self.blocks() - 1);
        Position {
            block,
            byte: (offset - self.starts[block]).min(self.block_len(block)),
        }
    }
}

/// A replacement map in logical document coordinates. Formatting-only changes preserve
/// all positions.
#[derive(Clone, Debug)]
pub struct PositionMap {
    before: BlockOffsets,
    after: BlockOffsets,
    steps: Vec<Replacement>,
}

#[derive(Clone, Debug)]
struct Replacement {
    range: Range<usize>,
    inserted_len: usize,
}

impl Replacement {
    fn map(&self, offset: usize, affinity: Affinity) -> usize {
        let Range { start, end } = self.range;
        if offset < start {
            offset
        } else if offset > end {
            offset - (end - start) + self.inserted_len
        } else if offset == start && (start != end || affinity == Affinity::Before) {
            start
        } else if offset == end {
            start + self.inserted_len
        } else {
            start
                + if affinity == Affinity::After {
                    self.inserted_len
                } else {
                    0
                }
        }
    }

    /// Whether `offset` sits strictly inside the replaced text, so the content on both
    /// sides of it is gone.
    fn deletes(&self, offset: usize) -> bool {
        self.range.start < offset && offset < self.range.end
    }
}

impl PositionMap {
    /// Map a position from the old document into the new one. A position at the start of
    /// a replaced range stays at the start of the replacement and one at its end moves to
    /// the end of the inserted text; only a position strictly inside collapses to the edge
    /// `affinity` picks. A pure insertion at the position moves it with
    /// [`Affinity::After`] and leaves it alone with [`Affinity::Before`].
    ///
    /// The map works in raw bytes: the result is clamped into the new document but is not
    /// snapped onto a grapheme boundary, so callers placing a caret or selection pass it
    /// through [`Document::clamp_position`] against the document the change produced.
    pub fn map(&self, position: Position, affinity: Affinity) -> Position {
        let offset = self
            .steps
            .iter()
            .fold(self.before.global_byte(position), |offset, step| {
                step.map(offset, affinity)
            });
        self.after.position(offset)
    }

    /// [`Self::map`], but `None` once the content at `position` has been deleted: some
    /// step replaced a non-empty range that the position lay strictly inside. A position
    /// exactly at either edge of a replaced range survives, because the deleted text is
    /// then wholly on one side of it.
    pub fn map_tracked(&self, position: Position, affinity: Affinity) -> Option<Position> {
        let mut offset = self.before.global_byte(position);
        for step in &self.steps {
            if step.deletes(offset) {
                return None;
            }
            offset = step.map(offset, affinity);
        }
        Some(self.after.position(offset))
    }

    /// Both ends take [`Affinity::After`], so a caret follows text typed at it.
    pub fn map_selection(&self, selection: Selection) -> Selection {
        Selection {
            anchor: self.map(selection.anchor, Affinity::After),
            head: self.map(selection.head, Affinity::After),
        }
    }

    fn reversed(&self) -> Self {
        Self {
            before: self.after.clone(),
            after: self.before.clone(),
            steps: self
                .steps
                .iter()
                .rev()
                .map(|step| Replacement {
                    range: step.range.start..step.range.start + step.inserted_len,
                    inserted_len: step.range.len(),
                })
                .collect(),
        }
    }

    fn retained_bytes(&self) -> usize {
        (self.before.starts.capacity() + self.after.starts.capacity())
            * std::mem::size_of::<usize>()
            + self.steps.capacity() * std::mem::size_of::<Replacement>()
    }
}

/// What produced a change. Hosts use it to tell their own edits apart from the
/// user's; `Command` is the generic provenance of a programmatic edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    Typed,
    Composition,
    Paste,
    #[default]
    Command,
    History,
    Extension(&'static str),
}

#[derive(Clone, Debug)]
pub struct Change {
    pub revision: u64,
    pub mapping: PositionMap,
    pub origin: Origin,
}

/// Options for [`Editor::transact`]. `group` has the same meaning as the typing group
/// of [`Editor::insert_text_grouped`]: it may merge into the previous undo entry, but
/// never across a change of block count or block kind.
#[derive(Clone, Copy, Debug, Default)]
pub struct TransactionOptions {
    pub group: Option<u64>,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
struct Snapshot {
    document: Document,
    selection: Selection,
    typing_marks: Marks,
}

#[derive(Clone, Debug)]
struct Composition {
    before: Snapshot,
    marked: Range<usize>,
    steps: Vec<Replacement>,
}

#[derive(Clone, Debug)]
struct HistoryEntry {
    state: Snapshot,
    mapping: PositionMap,
}

/// Bounds retained undo and redo snapshots. Oversized edits remain applied but may
/// not be undoable; the live document and an active composition are not included.
#[derive(Clone, Copy, Debug)]
pub struct HistoryLimits {
    pub entries: usize,
    pub bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            entries: 256,
            bytes: 16 * 1024 * 1024,
        }
    }
}

/// The heap a document holds, for the byte-based history limit.
fn document_bytes(document: &Document) -> usize {
    document.blocks.capacity() * std::mem::size_of::<Block>()
        + document
            .blocks
            .iter()
            .map(|block| {
                block.spans.capacity() * std::mem::size_of::<Span>()
                    + block
                        .spans
                        .iter()
                        .map(|span| span.text.capacity())
                        .sum::<usize>()
            })
            .sum::<usize>()
}

impl HistoryEntry {
    /// An entry retains one document, its snapshot; the mapping only holds block lengths.
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + document_bytes(&self.state.document)
            + self.mapping.retained_bytes()
    }
}

/// Each edit command is an atomic history transaction. Composition candidate changes
/// publish revisions immediately while retaining a single pre-composition undo snapshot.
#[derive(Clone, Debug)]
pub struct Editor {
    state: Snapshot,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    pending_steps: Vec<Replacement>,
    composition: Option<Composition>,
    revision: u64,
    history_limits: HistoryLimits,
    input_group: Option<(u64, Selection, Marks)>,
}

impl Editor {
    pub fn new(mut document: Document) -> Self {
        document.normalize();
        Self {
            state: Snapshot {
                document,
                selection: Selection::default(),
                typing_marks: Marks::default(),
            },
            undo: Vec::new(),
            redo: Vec::new(),
            pending_steps: Vec::new(),
            composition: None,
            revision: 0,
            history_limits: HistoryLimits::default(),
            input_group: None,
        }
    }

    pub fn document(&self) -> &Document {
        &self.state.document
    }
    /// A persistence snapshot excluding uncommitted input-method candidates.
    pub fn committed_document(&self) -> &Document {
        self.composition
            .as_ref()
            .map_or(self.document(), |composition| &composition.before.document)
    }
    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.history_limits = limits;
        self.trim_history();
    }
    fn trim_history(&mut self) {
        let mut bytes: usize = self
            .undo
            .iter()
            .chain(&self.redo)
            .map(HistoryEntry::retained_bytes)
            .sum();
        while self.undo.len() + self.redo.len() > self.history_limits.entries
            || bytes > self.history_limits.bytes
        {
            let removed = if !self.undo.is_empty() {
                self.undo.remove(0)
            } else if !self.redo.is_empty() {
                self.redo.remove(0)
            } else {
                break;
            };
            bytes = bytes.saturating_sub(removed.retained_bytes());
        }
        if self.undo.is_empty() {
            self.input_group = None;
        }
    }
    pub fn selection(&self) -> Selection {
        self.state.selection
    }
    pub fn typing_marks(&self) -> Marks {
        self.state.typing_marks
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.composition.is_some()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.composition.as_ref().map(|c| c.marked.clone())
    }
    pub fn position_to_utf16(&self, position: Position) -> usize {
        self.document().position_to_utf16(position)
    }
    pub fn utf16_to_position(&self, offset: usize) -> Position {
        self.document().utf16_to_position(offset)
    }

    pub fn set_selection(&mut self, selection: Selection) {
        self.input_group = None;
        self.finish_composition();
        self.select(selection);
    }

    fn select(&mut self, selection: Selection) {
        self.state.selection = selection;
        self.clamp_selection();
        let head = self.state.selection.head;
        self.state.typing_marks = self.document().blocks[head.block].marks_at(head.byte);
    }

    /// Bring both ends back into the document and onto grapheme boundaries, leaving
    /// the typing marks alone.
    fn clamp_selection(&mut self) {
        let selection = self.state.selection;
        self.state.selection = Selection {
            anchor: self.document().clamp_position(selection.anchor),
            head: self.document().clamp_position(selection.head),
        };
    }

    pub fn selection_text(&self) -> String {
        let (start, end) = self.selection().ordered();
        self.text_in(start..end)
    }

    /// See [`Document::text_in`].
    pub fn text_in(&self, range: Range<Position>) -> String {
        self.document().text_in(range)
    }

    /// Order a range onto grapheme boundaries. An empty range is an insertion point and
    /// stays empty; a non-empty one widens, so an edit neither splits a cluster nor
    /// silently collapses into a no-op.
    fn edit_range(&self, range: Range<Position>) -> (Position, Position) {
        let start = self.document().clamp_position(range.start.min(range.end));
        if range.start == range.end {
            return (start, start);
        }
        let end = range.start.max(range.end);
        let blocks = &self.document().blocks;
        let block = end.block.min(blocks.len() - 1);
        let text = blocks[block].text();
        let end = Position {
            block,
            byte: ceil_grapheme(&text, end.byte.min(text.len())),
        };
        (start, end)
    }

    /// Run several edits as one undo entry and one [`Change`]. An active composition is
    /// committed first; a transaction that leaves the document untouched publishes
    /// nothing and leaves the history, including redo, alone.
    pub fn transact(
        &mut self,
        options: TransactionOptions,
        action: impl FnOnce(&mut Transaction<'_>),
    ) -> Option<Change> {
        self.transaction_in_group(options.group, options.origin, |editor| {
            action(&mut Transaction { editor });
            editor.clamp_selection();
        })
    }

    fn publish(&mut self, before: &Document, origin: Origin) -> Option<Change> {
        let steps = std::mem::take(&mut self.pending_steps);
        if before == self.document() {
            return None;
        }
        self.revision += 1;
        Some(Change {
            revision: self.revision,
            mapping: PositionMap {
                before: BlockOffsets::of(before),
                after: BlockOffsets::of(self.document()),
                steps,
            },
            origin,
        })
    }

    fn transaction(&mut self, origin: Origin, action: impl FnOnce(&mut Self)) -> Option<Change> {
        self.transaction_in_group(None, origin, action)
    }

    fn transaction_in_group(
        &mut self,
        group: Option<u64>,
        origin: Origin,
        action: impl FnOnce(&mut Self),
    ) -> Option<Change> {
        self.finish_composition();
        let merge = group.is_some_and(|id| {
            self.input_group == Some((id, self.selection(), self.typing_marks()))
                && self.selection().is_empty()
        });
        self.input_group = None;
        let before = self.state.clone();
        action(self);
        let change = self.publish(&before.document, origin);
        if let Some(change) = &change {
            // Input-rule conversions own an undo boundary, so undoing a heading
            // or list conversion restores its literal marker before earlier typing.
            let group = group.filter(|_| {
                before.document.blocks.len() == self.document().blocks.len()
                    && before
                        .document
                        .blocks
                        .iter()
                        .zip(&self.document().blocks)
                        .all(|(a, b)| a.kind == b.kind)
            });
            if merge
                && group.is_some()
                && let Some(previous) = self.undo.last_mut()
            {
                previous.mapping.after = change.mapping.after.clone();
                previous
                    .mapping
                    .steps
                    .extend(change.mapping.steps.iter().cloned());
            } else {
                self.undo.push(HistoryEntry {
                    state: before,
                    mapping: change.mapping.clone(),
                });
            }
            self.redo.clear();
            self.input_group = group.map(|id| (id, self.selection(), self.typing_marks()));
            self.trim_history();
        }
        change
    }

    pub fn insert_text(&mut self, text: &str) -> Option<Change> {
        self.insert_text_in_group(text, None)
    }

    /// Group adjacent typing explicitly. Hosts choose a new token after a pause;
    /// selection changes, other commands, newlines, and composition break groups.
    pub fn insert_text_grouped(&mut self, text: &str, group: u64) -> Option<Change> {
        self.insert_text_in_group(text, Some(group))
    }

    /// Insert literal text without Markdown input rules or list-enter behavior.
    pub fn insert_text_plain(&mut self, text: &str) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| editor.replace_selection(text))
    }

    pub fn insert_text_plain_grouped(&mut self, text: &str, group: u64) -> Option<Change> {
        let group =
            (!text.is_empty() && !text.contains(['\n', '\r']) && self.selection().is_empty())
                .then_some(group);
        self.transaction_in_group(group, Origin::Typed, |editor| {
            editor.replace_selection(text)
        })
    }

    fn insert_text_in_group(&mut self, text: &str, group: Option<u64>) -> Option<Change> {
        let group = group.filter(|_| {
            !text.contains(['\n', '\r']) && self.selection().is_empty() && !text.is_empty()
        });
        self.transaction_in_group(group, Origin::Typed, |editor| {
            let selection = editor.selection();
            if text == "\n" && selection.is_empty() {
                let index = selection.head.block;
                let blocks = &mut editor.state.document.blocks;
                if let Some(language) = fence_language(&blocks[index]) {
                    editor.state.selection.anchor = Position {
                        block: index,
                        byte: 0,
                    };
                    editor.replace_selection("");
                    editor.state.document.blocks[index].kind = BlockKind::Code { language };
                    editor.state.document.blocks[index].depth = 0;
                    return;
                }
                let block = &mut blocks[index];
                if block.is_empty()
                    && !matches!(
                        block.kind,
                        BlockKind::Paragraph | BlockKind::Divider | BlockKind::Code { .. }
                    )
                {
                    if block.kind.supports_depth() {
                        editor.change_nesting(index..index + 1, false);
                    } else {
                        block.kind = BlockKind::Paragraph;
                        block.depth = 0;
                    }
                    return;
                }
            }
            editor.replace_selection(text);
            if !text.contains('\n') {
                editor.input_rules();
            }
        })
    }

    pub fn replace_utf16(&mut self, range: Range<usize>, text: &str) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| {
            editor.state.selection = Selection {
                anchor: editor.utf16_to_position(range.start),
                head: editor.utf16_to_position(range.end),
            };
            editor.replace_selection(text);
        })
    }

    /// Replace an arbitrary range, which may span blocks, with literal text and no
    /// Markdown input rules. The range is ordered and snapped onto grapheme boundaries;
    /// the inserted text takes the formatting at the range start and the caret lands at
    /// its end, as it does when replacing a selection.
    pub fn replace_range(&mut self, range: Range<Position>, text: &str) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            editor.apply_replace_range(range, text)
        })
    }

    pub fn delete_range(&mut self, range: Range<Position>) -> Option<Change> {
        self.replace_range(range, "")
    }

    fn apply_replace_range(&mut self, range: Range<Position>, text: &str) {
        let (start, end) = self.edit_range(range);
        self.state.selection = Selection {
            anchor: start,
            head: end,
        };
        self.state.typing_marks = self.document().blocks[start.block].marks_at(start.byte);
        self.replace_selection(text);
    }

    fn replace_selection(&mut self, text: &str) {
        let (start, end) = self.selection().ordered();
        let global_start = self.document().global_byte_raw(start);
        let global_end = self.document().global_byte_raw(end);
        let document = &mut self.state.document;
        let first = document.blocks[start.block].clone();
        let last = document.blocks[end.block].clone();
        let prefix = slice_spans(&first.spans, 0..start.byte);
        let suffix = slice_spans(&last.spans, end.byte..last.len());
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if global_start != global_end || !normalized.is_empty() {
            self.pending_steps.push(Replacement {
                range: global_start..global_end,
                inserted_len: normalized.len(),
            });
        }
        let parts: Vec<&str> = normalized.split('\n').collect();
        let marks = if matches!(first.kind, BlockKind::Code { .. }) {
            Marks::default()
        } else {
            self.state.typing_marks
        };
        // Text typed strictly inside a link stays part of it; at either edge it does not.
        let link = prefix
            .last()
            .and_then(|span| span.link.clone())
            .filter(|link| suffix.first().and_then(|span| span.link.as_ref()) == Some(link))
            .filter(|_| parts.len() == 1);
        let mut replacement = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let kind = if index == 0 {
                first.kind.clone()
            } else {
                match first.kind {
                    BlockKind::Bullet => BlockKind::Bullet,
                    BlockKind::Ordered => BlockKind::Ordered,
                    BlockKind::Task { .. } => BlockKind::Task { checked: false },
                    BlockKind::Quote => BlockKind::Quote,
                    BlockKind::Code { .. } => first.kind.clone(),
                    _ => BlockKind::Paragraph,
                }
            };
            let mut spans = if index == 0 {
                prefix.clone()
            } else {
                Vec::new()
            };
            push_linked_span(&mut spans, part, marks, link.as_deref());
            if index == parts.len() - 1 {
                for span in &suffix {
                    push_like(&mut spans, &span.text, span);
                }
            }
            if matches!(kind, BlockKind::Code { .. }) {
                for span in &mut spans {
                    span.marks = Marks::default();
                    span.link = None;
                }
            }
            let kind = if kind == BlockKind::Divider && !spans.is_empty() {
                BlockKind::Paragraph
            } else {
                kind
            };
            let depth = if kind.supports_depth() {
                first.depth
            } else {
                0
            };
            replacement.push(Block { kind, depth, spans });
        }
        let caret_block = start.block + parts.len() - 1;
        let caret_byte = if parts.len() == 1 {
            start.byte + parts[0].len()
        } else {
            parts.last().unwrap().len()
        };
        document.blocks.splice(start.block..=end.block, replacement);
        document.normalize();
        // Inserting a joiner or combining scalar can merge with the following grapheme.
        let caret_text = document.blocks[caret_block].text();
        self.state.selection = Selection::caret(Position {
            block: caret_block,
            byte: ceil_grapheme(&caret_text, caret_byte),
        });
    }

    pub fn backspace(&mut self) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| {
            let selection = editor.selection();
            if selection.is_empty() {
                let head = selection.head;
                if head.byte == 0
                    && let Some(range) = editor.document().code_block_range(head.block)
                    && head.block == range.start
                {
                    // A physical code line is not its own block. Only a wholly
                    // empty code block clears its format at the start boundary.
                    if range.len() == 1 && editor.document().blocks[head.block].is_empty() {
                        editor.state.document.blocks[head.block].kind = BlockKind::Paragraph;
                    }
                    return;
                }
                if head.byte == 0
                    && !matches!(
                        editor.document().blocks[head.block].kind,
                        BlockKind::Paragraph | BlockKind::Code { .. }
                    )
                {
                    let block = &mut editor.state.document.blocks[head.block];
                    if block.kind.supports_depth() {
                        editor.change_nesting(head.block..head.block + 1, false);
                    } else {
                        block.kind = BlockKind::Paragraph;
                        block.depth = 0;
                    }
                    return;
                }
                editor.state.selection.anchor = editor.previous_position(head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_forward(&mut self) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| {
            if editor.selection().is_empty() {
                editor.state.selection.head = editor.next_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_word_backward(&mut self) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| {
            if editor.selection().is_empty() {
                editor.state.selection.anchor =
                    editor.previous_word_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_word_forward(&mut self) -> Option<Change> {
        self.transaction(Origin::Typed, |editor| {
            if editor.selection().is_empty() {
                editor.state.selection.head = editor.next_word_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn toggle_mark(&mut self, mark: Mark) -> Option<Change> {
        self.transaction(Origin::Command, |editor| editor.apply_toggle_mark(mark))
    }

    fn apply_toggle_mark(&mut self, mark: Mark) {
        if self.selection().is_empty() {
            self.state.typing_marks.toggle(mark);
            return;
        }
        let (start, end) = self.selection().ordered();
        let mut all_enabled = true;
        for index in start.block..=end.block {
            let block = &self.document().blocks[index];
            let range = if index == start.block { start.byte } else { 0 }..if index == end.block {
                end.byte
            } else {
                block.len()
            };
            for span in slice_spans(&block.spans, range) {
                all_enabled &= span.marks.has(mark);
            }
        }
        for index in start.block..=end.block {
            let block = &mut self.state.document.blocks[index];
            let start_byte = if index == start.block { start.byte } else { 0 };
            let end_byte = if index == end.block {
                end.byte
            } else {
                block.len()
            };
            let mut spans = slice_spans(&block.spans, 0..start_byte);
            for mut span in slice_spans(&block.spans, start_byte..end_byte) {
                span.marks.set(mark, !all_enabled);
                push_like(&mut spans, &span.text, &span);
            }
            for span in slice_spans(&block.spans, end_byte..block.len()) {
                push_like(&mut spans, &span.text, &span);
            }
            block.spans = spans;
        }
        self.state.typing_marks.set(mark, !all_enabled);
        // Code lines drop the mark again.
        self.state.document.normalize();
    }

    pub fn set_block_kind(&mut self, kind: BlockKind) -> Option<Change> {
        self.transaction(Origin::Command, |editor| editor.apply_set_block_kind(kind))
    }

    /// Set one block's kind without moving the selection. An out-of-range index is a
    /// no-op.
    pub fn set_block_kind_at(&mut self, block: usize, kind: BlockKind) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            editor.apply_set_block_kind_at(block, kind)
        })
    }

    fn apply_set_block_kind(&mut self, kind: BlockKind) {
        let (start, end) = self.selection().ordered();
        let last = if end.byte == 0 && end.block > start.block {
            end.block - 1
        } else {
            end.block
        };
        self.set_kind_in(start.block..last + 1, kind);
    }

    fn apply_set_block_kind_at(&mut self, block: usize, kind: BlockKind) {
        if block < self.document().blocks.len() {
            self.set_kind_in(block..block + 1, kind);
        }
    }

    /// Re-kind a block range, dropping the depth of kinds that do not carry one and
    /// normalizing what the new kind forbids (marks and links in code, text in a divider).
    fn set_kind_in(&mut self, range: Range<usize>, kind: BlockKind) {
        for block in &mut self.state.document.blocks[range] {
            if !(block.kind.is_list() && kind.is_list()
                || block.kind == BlockKind::Quote && kind == BlockKind::Quote)
            {
                block.depth = 0;
            }
            block.kind = kind.clone();
        }
        self.state.document.normalize();
    }

    /// Toggle matching selected block formats off; task state and code language
    /// are attributes, so they do not distinguish the format being toggled.
    pub fn toggle_block_kind(&mut self, kind: BlockKind) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            let mut range = editor.selected_blocks();
            let matches = |current: &BlockKind| {
                *current == kind
                    || matches!(
                        (current, &kind),
                        (BlockKind::Task { .. }, BlockKind::Task { .. })
                            | (BlockKind::Code { .. }, BlockKind::Code { .. })
                    )
            };
            let clear = editor.document().blocks[range.clone()]
                .iter()
                .all(|block| matches(&block.kind));
            if clear && matches!(kind, BlockKind::Code { .. }) {
                range.start = editor
                    .document()
                    .code_block_range(range.start)
                    .unwrap()
                    .start;
                range.end = editor
                    .document()
                    .code_block_range(range.end - 1)
                    .unwrap()
                    .end;
            }
            let target = if clear { BlockKind::Paragraph } else { kind };
            editor.set_kind_in(range, target);
        })
    }

    /// Move to an ordinary paragraph after the current code block, creating one
    /// if necessary. The insertion is a single undoable document transaction.
    pub fn exit_code_block(&mut self) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            let Some(range) = editor
                .document()
                .code_block_range(editor.selection().head.block)
            else {
                return;
            };
            if !editor
                .document()
                .blocks
                .get(range.end)
                .is_some_and(|block| block.kind == BlockKind::Paragraph)
            {
                let last = range.end - 1;
                let offset = editor.document().global_byte(Position {
                    block: last,
                    byte: editor.document().blocks[last].len(),
                });
                editor.pending_steps.push(Replacement {
                    range: offset..offset,
                    inserted_len: 1,
                });
                editor
                    .state
                    .document
                    .blocks
                    .insert(range.end, Block::default());
            }
            editor.select(Selection::caret(Position {
                block: range.end,
                byte: 0,
            }));
        })
    }

    /// Indent selected list items (including their children), or selected quotes.
    pub fn indent(&mut self) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            let range = editor.selected_blocks();
            editor.indent_code_lines(range.clone(), true);
            editor.change_nesting(range, true);
        })
    }

    /// Outdent one level. Top-level items become ordinary paragraphs.
    pub fn outdent(&mut self) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            let range = editor.selected_blocks();
            editor.indent_code_lines(range.clone(), false);
            editor.change_nesting(range, false);
        })
    }

    fn indent_code_lines(&mut self, range: Range<usize>, indent: bool) {
        for index in range.rev() {
            let block = &self.document().blocks[index];
            if !matches!(block.kind, BlockKind::Code { .. }) {
                continue;
            }
            let text = block.text();
            let removed = if indent {
                0
            } else if text.starts_with('\t') {
                1
            } else {
                text.bytes()
                    .take_while(|byte| *byte == b' ')
                    .take(2)
                    .count()
            };
            if !indent && removed == 0 {
                continue;
            }
            let offset = self.document().global_byte(Position {
                block: index,
                byte: 0,
            });
            let mut spans = Vec::new();
            if indent {
                push_span(&mut spans, "\t", Marks::default());
            }
            for span in slice_spans(&block.spans, removed..block.len()) {
                push_like(&mut spans, &span.text, &span);
            }
            self.state.document.blocks[index].spans = spans;
            self.pending_steps.push(Replacement {
                range: offset..offset + removed,
                inserted_len: usize::from(indent),
            });
            for position in [
                &mut self.state.selection.anchor,
                &mut self.state.selection.head,
            ] {
                if position.block == index {
                    position.byte = if indent {
                        position.byte + 1
                    } else {
                        position.byte.saturating_sub(removed)
                    };
                }
            }
        }
    }

    fn selected_blocks(&self) -> Range<usize> {
        let (start, end) = self.selection().ordered();
        let end = if end.byte == 0 && end.block > start.block {
            end.block
        } else {
            end.block + 1
        };
        start.block..end
    }

    fn change_nesting(&mut self, range: Range<usize>, indent: bool) {
        let blocks = &mut self.state.document.blocks;
        let first = &blocks[range.start];
        if indent
            && first.kind.is_list()
            && !range.start.checked_sub(1).is_some_and(|previous| {
                blocks[previous].kind.is_list() && blocks[previous].depth >= first.depth
            })
        {
            return;
        }
        let mut index = range.start;
        while index < range.end {
            let kind = blocks[index].kind.clone();
            let depth = blocks[index].depth;
            if !kind.supports_depth() {
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while end < blocks.len()
                && blocks[end].depth > depth
                && ((kind.is_list() && blocks[end].kind.is_list())
                    || (kind == BlockKind::Quote && blocks[end].kind == BlockKind::Quote))
            {
                end += 1;
            }
            let can_indent = !indent
                || kind == BlockKind::Quote
                || index.checked_sub(1).is_some_and(|previous| {
                    blocks[previous].kind.is_list() && blocks[previous].depth >= depth
                });
            if can_indent
                && (!indent || blocks[index..end].iter().all(|block| block.depth < u8::MAX))
            {
                for block in &mut blocks[index..end] {
                    if indent {
                        block.depth += 1;
                    } else if block.depth > 0 {
                        block.depth -= 1;
                    } else {
                        block.kind = BlockKind::Paragraph;
                    }
                }
            }
            index = end;
        }
    }

    pub fn set_code_language(&mut self, language: &str) -> Option<Change> {
        self.set_code_language_at(self.selection().head.block, language)
    }

    /// Update a code block without changing the caret or its undo selection.
    pub fn set_code_language_at(&mut self, block: usize, language: &str) -> Option<Change> {
        let language = language.split_whitespace().next().unwrap_or("").to_owned();
        self.transaction(Origin::Command, |editor| {
            if let Some(range) = editor.document().code_block_range(block) {
                for block in &mut editor.state.document.blocks[range] {
                    block.kind = BlockKind::Code {
                        language: language.clone(),
                    };
                }
            }
        })
    }

    pub fn code_block_text(&self) -> Option<String> {
        let range = self
            .document()
            .code_block_range(self.selection().head.block)?;
        Some(
            self.document().blocks[range]
                .iter()
                .map(Block::text)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub fn undo(&mut self) -> Option<Change> {
        self.input_group = None;
        self.finish_composition();
        let previous = self.undo.pop()?;
        let current = std::mem::replace(&mut self.state, previous.state);
        self.revision += 1;
        let change = Change {
            revision: self.revision,
            mapping: previous.mapping.reversed(),
            origin: Origin::History,
        };
        self.redo.push(HistoryEntry {
            state: current,
            mapping: previous.mapping,
        });
        self.trim_history();
        Some(change)
    }

    pub fn redo(&mut self) -> Option<Change> {
        self.input_group = None;
        self.finish_composition();
        let next = self.redo.pop()?;
        let current = std::mem::replace(&mut self.state, next.state);
        self.revision += 1;
        let change = Change {
            revision: self.revision,
            mapping: next.mapping.clone(),
            origin: Origin::History,
        };
        self.undo.push(HistoryEntry {
            state: current,
            mapping: next.mapping,
        });
        self.trim_history();
        Some(change)
    }

    pub fn previous_position(&self, position: Position) -> Position {
        let position = self.document().clamp_position(position);
        if position.byte > 0 {
            let text = self.document().blocks[position.block].text();
            Position {
                block: position.block,
                byte: text
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .take_while(|i| *i < position.byte)
                    .last()
                    .unwrap_or(0),
            }
        } else if position.block > 0 {
            Position {
                block: position.block - 1,
                byte: self.document().blocks[position.block - 1].len(),
            }
        } else {
            position
        }
    }

    pub fn next_position(&self, position: Position) -> Position {
        let position = self.document().clamp_position(position);
        let text = self.document().blocks[position.block].text();
        if position.byte < text.len() {
            Position {
                block: position.block,
                byte: text
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .find(|i| *i > position.byte)
                    .unwrap_or(text.len()),
            }
        } else if position.block + 1 < self.document().blocks.len() {
            Position {
                block: position.block + 1,
                byte: 0,
            }
        } else {
            position
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        let position = if !extend && !self.selection().is_empty() {
            self.selection().ordered().0
        } else {
            self.previous_position(self.selection().head)
        };
        self.move_to(position, extend);
    }

    pub fn previous_word_position(&self, position: Position) -> Position {
        let position = self.document().clamp_position(position);
        if position.byte == 0 {
            return self.previous_position(position);
        }
        let text = self.document().blocks[position.block].text();
        let byte = text[..position.byte]
            .split_word_bound_indices()
            .rfind(|(_, segment)| !segment.chars().all(char::is_whitespace))
            .map_or(0, |(start, _)| start);
        self.document().clamp_position(Position {
            block: position.block,
            byte,
        })
    }

    pub fn next_word_position(&self, position: Position) -> Position {
        let position = self.document().clamp_position(position);
        let text = self.document().blocks[position.block].text();
        if position.byte == text.len() {
            return self.next_position(position);
        }
        let byte = text[position.byte..]
            .split_word_bound_indices()
            .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
            .map_or(text.len(), |(start, segment)| {
                position.byte + start + segment.len()
            });
        Position {
            block: position.block,
            byte: ceil_grapheme(&text, byte),
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let position = if !extend && !self.selection().is_empty() {
            self.selection().ordered().0
        } else {
            self.previous_word_position(self.selection().head)
        };
        self.move_to(position, extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let position = if !extend && !self.selection().is_empty() {
            self.selection().ordered().1
        } else {
            self.next_word_position(self.selection().head)
        };
        self.move_to(position, extend);
    }

    pub fn move_document_start(&mut self, extend: bool) {
        self.move_to(Position::default(), extend);
    }

    pub fn move_document_end(&mut self, extend: bool) {
        self.move_to(self.utf16_to_position(usize::MAX), extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        let position = if !extend && !self.selection().is_empty() {
            self.selection().ordered().1
        } else {
            self.next_position(self.selection().head)
        };
        self.move_to(position, extend);
    }

    pub fn move_to(&mut self, position: Position, extend: bool) {
        self.set_selection(Selection {
            anchor: if extend {
                self.selection().anchor
            } else {
                position
            },
            head: position,
        });
    }

    pub fn set_composition(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) -> Option<Change> {
        self.input_group = None;
        let before_document = self.document().clone();
        let replacement = range.or_else(|| self.marked_range()).unwrap_or_else(|| {
            let (start, end) = self.selection().ordered();
            self.position_to_utf16(start)..self.position_to_utf16(end)
        });
        if self.composition.is_none() {
            self.composition = Some(Composition {
                before: self.state.clone(),
                marked: replacement.clone(),
                steps: Vec::new(),
            });
        }
        // Marked text may begin inside an extended grapheme (e.g. an accent after
        // a Latin base). Platform replacement uses scalar boundaries internally;
        // user navigation and the exposed logical selection remain grapheme-based.
        self.state.selection = Selection {
            anchor: self.document().utf16_to_scalar_position(replacement.start),
            head: self.document().utf16_to_scalar_position(replacement.end),
        };
        let start = self
            .document()
            .position_to_utf16_raw(self.selection().ordered().0);
        self.replace_selection(text);
        let length = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .encode_utf16()
            .count();
        let marked = start..start + length;
        self.composition.as_mut().unwrap().marked = marked.clone();
        if let Some(selected) = selected {
            self.state.selection = Selection {
                anchor: self.utf16_to_position(start + selected.start.min(length)),
                head: self.utf16_to_position(start + selected.end.min(length)),
            };
        }
        self.composition
            .as_mut()
            .unwrap()
            .steps
            .extend(self.pending_steps.iter().cloned());
        self.publish(&before_document, Origin::Composition)
    }

    pub fn commit_composition(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
    ) -> Option<Change> {
        if self.composition.is_none() {
            return match range {
                Some(range) => self.replace_utf16(range, text),
                None => self.insert_text(text),
            };
        }
        let change = self.set_composition(range, text, None);
        self.finish_composition();
        change
    }

    /// Plain-text hosts share the native composition path but never interpret
    /// the fallback (non-composing) insertion as a Markdown input rule.
    pub fn commit_composition_plain(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
    ) -> Option<Change> {
        if self.is_composing() {
            self.commit_composition(range, text)
        } else if let Some(range) = range {
            self.replace_utf16(range, text)
        } else {
            self.insert_text_plain(text)
        }
    }

    /// Commit existing marked text without replacing it (platform unmarkText). Returns
    /// whether a live composition became an undo entry, that is whether
    /// [`Self::committed_document`] just changed. It publishes no [`Change`].
    pub fn finish_composition(&mut self) -> bool {
        let Some(composition) = self.composition.take() else {
            return false;
        };
        if composition.before.document == self.state.document {
            return false;
        }
        let mapping = PositionMap {
            before: BlockOffsets::of(&composition.before.document),
            after: BlockOffsets::of(self.document()),
            steps: composition.steps,
        };
        self.undo.push(HistoryEntry {
            state: composition.before,
            mapping,
        });
        self.redo.clear();
        self.trim_history();
        true
    }

    pub fn cancel_composition(&mut self) -> Option<Change> {
        let composition = self.composition.take()?;
        let mapping = PositionMap {
            before: BlockOffsets::of(&composition.before.document),
            after: BlockOffsets::of(self.document()),
            steps: composition.steps,
        }
        .reversed();
        let current = std::mem::replace(&mut self.state, composition.before);
        if current.document == self.state.document {
            return None;
        }
        self.revision += 1;
        Some(Change {
            revision: self.revision,
            mapping,
            origin: Origin::Composition,
        })
    }

    fn input_rules(&mut self) {
        let position = self.selection().head;
        if matches!(
            self.document().blocks[position.block].kind,
            BlockKind::Code { .. }
        ) {
            return;
        }
        let text = self.document().blocks[position.block].text();
        let before = &text[..position.byte];
        let kind = match before {
            "- " | "* " | "+ " => Some(BlockKind::Bullet),
            "- [ ] " | "[ ] " | "- [] " | "[] " => Some(BlockKind::Task { checked: false }),
            "- [x] " | "- [X] " | "[x] " | "[X] " => Some(BlockKind::Task { checked: true }),
            "> " => Some(BlockKind::Quote),
            _ if before.ends_with(' ') && fence_info(before.trim_end_matches(' ')).is_some() => {
                fence_info(before.trim_end_matches(' ')).map(|language| BlockKind::Code {
                    language: language.to_owned(),
                })
            }
            "---" | "___ " | "*** " => Some(BlockKind::Divider),
            _ if before.strip_suffix(". ").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit())
            }) =>
            {
                Some(BlockKind::Ordered)
            }
            _ => {
                let hashes = before.trim_end_matches(' ');
                if before.ends_with(' ')
                    && (1..=6).contains(&hashes.len())
                    && hashes.bytes().all(|c| c == b'#')
                {
                    Some(BlockKind::Heading(hashes.len() as u8))
                } else {
                    None
                }
            }
        };
        if let Some(kind) = kind {
            self.state.selection.anchor = Position {
                block: position.block,
                byte: 0,
            };
            if kind == BlockKind::Divider {
                // The rule takes the line; typing continues on a new one below it.
                self.replace_selection("\n");
                self.state.document.blocks[position.block + 1].kind = BlockKind::Paragraph;
            } else {
                self.replace_selection("");
            }
            let block = &mut self.state.document.blocks[position.block];
            if block.kind == BlockKind::Quote && kind == BlockKind::Quote {
                block.depth = block.depth.saturating_add(1);
            } else if !(block.kind.is_list() && kind.is_list()) {
                block.depth = 0;
            }
            block.kind = kind;
            self.state.document.normalize();
            return;
        }
        for (delimiter, mark) in [
            ("**", Mark::Bold),
            ("__", Mark::Bold),
            ("*", Mark::Italic),
            ("_", Mark::Italic),
            ("~~", Mark::Strikethrough),
            ("`", Mark::Code),
        ] {
            if before.len() <= delimiter.len() * 2 || !before.ends_with(delimiter) {
                continue;
            }
            let content_end = before.len() - delimiter.len();
            let Some(open) = before[..content_end].rfind(delimiter) else {
                continue;
            };
            let content_start = open + delimiter.len();
            if content_start == content_end || (open > 0 && before.as_bytes()[open - 1] == b'\\') {
                continue;
            }
            if matches!(delimiter, "*" | "_")
                && (before[..open].ends_with(delimiter)
                    || before[content_start..content_end].contains(delimiter))
            {
                continue;
            }
            // Underscores inside a word are identifiers, not emphasis.
            if delimiter.starts_with('_')
                && before[..open]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
            {
                continue;
            }
            self.unwrap_delimited(
                position.block,
                open..content_start,
                content_end..position.byte,
                |span| span.marks.set(mark, true),
            );
            self.state.typing_marks.set(mark, false);
            return;
        }
        // [text](url)
        if let Some(inner) = before.strip_suffix(')')
            && let Some(middle) = inner.rfind("](")
            && let Some(open) = before[..middle].rfind('[')
            && open + 1 < middle
            && !before[..open].ends_with(['!', '\\'])
        {
            let url = before[middle + 2..inner.len()].to_owned();
            if !url.is_empty() && !url.contains(char::is_whitespace) {
                self.unwrap_delimited(
                    position.block,
                    open..open + 1,
                    middle..position.byte,
                    |span| span.link = Some(url.clone()),
                );
            }
        }
    }

    /// Remove the `open` and `close` byte ranges of a block, format the text between
    /// them, and leave the caret after it.
    fn unwrap_delimited(
        &mut self,
        block: usize,
        open: Range<usize>,
        close: Range<usize>,
        apply: impl Fn(&mut Span),
    ) {
        let block_start = self.document().global_byte(Position { block, byte: 0 });
        // Apply the trailing removal before the leading removal so both offsets
        // refer to the original block's bytes.
        for range in [&close, &open] {
            self.pending_steps.push(Replacement {
                range: block_start + range.start..block_start + range.end,
                inserted_len: 0,
            });
        }
        let target = &mut self.state.document.blocks[block];
        let mut spans = slice_spans(&target.spans, 0..open.start);
        for mut span in slice_spans(&target.spans, open.end..close.start) {
            apply(&mut span);
            push_like(&mut spans, &span.text, &span);
        }
        for span in slice_spans(&target.spans, close.end..target.len()) {
            push_like(&mut spans, &span.text, &span);
        }
        target.spans = spans;
        self.state.selection = Selection::caret(Position {
            block,
            byte: open.start + close.start - open.end,
        });
    }

    /// The link containing the caret, or the one shared by the whole selection.
    pub fn active_link(&self) -> Option<&str> {
        let (start, end) = self.selection().ordered();
        if start == end {
            return self.link_at(start).map(|(_, link)| link);
        }
        let mut links = (start.block..=end.block).flat_map(|index| {
            let block = &self.document().blocks[index];
            let from = if index == start.block { start.byte } else { 0 };
            let to = if index == end.block {
                end.byte
            } else {
                block.len()
            };
            let mut offset = 0;
            block.spans.iter().filter_map(move |span| {
                let range = offset..offset + span.text.len();
                offset = range.end;
                (range.start.max(from) < range.end.min(to)).then_some(span.link.as_deref())
            })
        });
        let first = links.next()??;
        links.all(|link| link == Some(first)).then_some(first)
    }

    /// The byte range within its block of the link touching `position`.
    pub fn link_at(&self, position: Position) -> Option<(Range<usize>, &str)> {
        let touches =
            |range: &Range<usize>| range.start <= position.byte && position.byte <= range.end;
        let mut offset = 0;
        let mut extent: Option<(Range<usize>, &str)> = None;
        for span in &self.document().blocks[position.block].spans {
            let range = offset..offset + span.text.len();
            offset = range.end;
            extent = match (extent.take(), span.link.as_deref()) {
                (Some((current, link)), Some(next)) if link == next => {
                    Some((current.start..range.end, link))
                }
                (Some((current, link)), _) if touches(&current) => return Some((current, link)),
                (_, link) => link.map(|link| (range, link)),
            };
        }
        extent.filter(|(range, _)| touches(range))
    }

    /// Link the selection to `url`, or unlink it with `None`. A caret edits the link
    /// it touches; elsewhere it inserts the URL itself as linked text.
    pub fn set_link(&mut self, url: Option<&str>) -> Option<Change> {
        self.transaction(Origin::Command, |editor| {
            let selection = editor.selection();
            let (mut start, mut end) = selection.ordered();
            if start == end {
                if let Some((range, _)) = editor.link_at(start) {
                    start.byte = range.start;
                    end.byte = range.end;
                } else if let Some(url) = url {
                    editor.replace_selection(url);
                    end = editor.selection().head;
                }
            }
            for index in start.block..=end.block {
                let block = &mut editor.state.document.blocks[index];
                let from = if index == start.block { start.byte } else { 0 };
                let to = if index == end.block {
                    end.byte
                } else {
                    block.len()
                };
                let mut spans = slice_spans(&block.spans, 0..from);
                for mut span in slice_spans(&block.spans, from..to) {
                    span.link = url.map(str::to_owned);
                    push_like(&mut spans, &span.text, &span);
                }
                for span in slice_spans(&block.spans, to..block.len()) {
                    push_like(&mut spans, &span.text, &span);
                }
                block.spans = spans;
            }
            // Code lines drop the link again.
            editor.state.document.normalize();
        })
    }
}

/// The edits available while composing one undo entry inside [`Editor::transact`].
/// History, composition and nested transactions are deliberately out of reach.
pub struct Transaction<'a> {
    editor: &'a mut Editor,
}

impl Transaction<'_> {
    pub fn document(&self) -> &Document {
        self.editor.document()
    }

    pub fn selection(&self) -> Selection {
        self.editor.selection()
    }

    pub fn text_in(&self, range: Range<Position>) -> String {
        self.editor.text_in(range)
    }

    pub fn set_selection(&mut self, selection: Selection) {
        self.editor.select(selection);
    }

    /// Insert literal text over the selection, without Markdown input rules.
    pub fn insert_text(&mut self, text: &str) {
        self.editor.replace_selection(text);
    }

    /// See [`Editor::replace_range`].
    pub fn replace_range(&mut self, range: Range<Position>, text: &str) {
        self.editor.apply_replace_range(range, text);
    }

    pub fn delete_range(&mut self, range: Range<Position>) {
        self.replace_range(range, "");
    }

    pub fn toggle_mark(&mut self, mark: Mark) {
        self.editor.apply_toggle_mark(mark);
    }

    pub fn set_block_kind(&mut self, kind: BlockKind) {
        self.editor.apply_set_block_kind(kind);
    }

    /// See [`Editor::set_block_kind_at`].
    pub fn set_block_kind_at(&mut self, block: usize, kind: BlockKind) {
        self.editor.apply_set_block_kind_at(block, kind);
    }

    /// See [`Editor::insert_fragment`].
    pub fn insert_fragment(&mut self, fragment: Document) {
        self.editor.apply_insert_fragment(fragment);
    }

    /// See [`Editor::remove_blocks`].
    pub fn remove_blocks(&mut self, range: Range<usize>) {
        self.editor.apply_remove_blocks(range);
    }

    /// See [`Editor::insert_blocks`].
    pub fn insert_blocks(&mut self, index: usize, blocks: Vec<Block>) {
        self.editor.apply_insert_blocks(index, blocks);
    }
}

/// The info string of an opening code fence such as "```rust".
fn fence_info(line: &str) -> Option<&str> {
    let info = line
        .strip_prefix("```")
        .or_else(|| line.strip_prefix("~~~"))?;
    info.chars()
        .all(|c| c.is_alphanumeric() || "+-_#.".contains(c))
        .then_some(info)
}

fn fence_language(block: &Block) -> Option<String> {
    if block.kind != BlockKind::Paragraph {
        return None;
    }
    fence_info(&block.text()).map(str::to_owned)
}

fn push_span(spans: &mut Vec<Span>, text: &str, marks: Marks) {
    push_linked_span(spans, text, marks, None);
}

fn push_linked_span(spans: &mut Vec<Span>, text: &str, marks: Marks, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.marks == marks
        && last.link.as_deref() == link
    {
        last.text.push_str(text);
    } else {
        spans.push(Span {
            text: text.to_owned(),
            marks,
            link: link.map(str::to_owned),
        });
    }
}

/// Append a copy of `span`'s formatting over `text`.
fn push_like(spans: &mut Vec<Span>, text: &str, span: &Span) {
    push_linked_span(spans, text, span.marks, span.link.as_deref());
}

/// Append the `range` of the text `spans` concatenate, without slicing them.
fn push_span_text(out: &mut String, spans: &[Span], range: Range<usize>) {
    let mut offset = 0;
    for span in spans {
        let start = range.start.saturating_sub(offset).min(span.text.len());
        let end = range.end.saturating_sub(offset).min(span.text.len());
        out.push_str(&span.text[start..end]);
        offset += span.text.len();
    }
}

fn slice_spans(spans: &[Span], range: Range<usize>) -> Vec<Span> {
    let mut result = Vec::new();
    let mut offset = 0;
    for span in spans {
        let start = range.start.saturating_sub(offset).min(span.text.len());
        let end = range.end.saturating_sub(offset).min(span.text.len());
        if start < end {
            push_like(&mut result, &span.text[start..end], span);
        }
        offset += span.text.len();
    }
    result
}

fn floor_grapheme(text: &str, byte: usize) -> usize {
    if byte >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|i| *i <= byte)
        .last()
        .unwrap_or(0)
}

fn ceil_grapheme(text: &str, byte: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .find(|i| *i >= byte)
        .unwrap_or(text.len())
}

fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut consumed = 0;
    for (byte, character) in text.char_indices() {
        if consumed + character.len_utf16() > offset {
            return byte;
        }
        consumed += character.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests;
