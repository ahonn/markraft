//! Platform-independent structured rich text editing with explicit coordinate conversion.
//! Document positions use UTF-8 byte offsets at extended grapheme boundaries. Platform
//! input offsets use UTF-16 code units over the document's newline-separated plain text.

mod markdown;

use serde::{Deserialize, Serialize};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

impl Marks {
    fn toggle(&mut self, mark: Mark) {
        match mark {
            Mark::Bold => self.bold = !self.bold,
            Mark::Italic => self.italic = !self.italic,
            Mark::Code => self.code = !self.code,
        }
    }

    fn has(self, mark: Mark) -> bool {
        match mark {
            Mark::Bold => self.bold,
            Mark::Italic => self.italic,
            Mark::Code => self.code,
        }
    }

    fn set(&mut self, mark: Mark, value: bool) {
        match mark {
            Mark::Bold => self.bold = value,
            Mark::Italic => self.italic = value,
            Mark::Code => self.code = value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Code,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading(u8),
    Bullet,
    Task {
        checked: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub marks: Marks,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
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

    /// Normalize empty documents, adjacent runs and heading levels. Newlines in supplied
    /// spans become structural paragraph boundaries; text is never silently discarded.
    pub fn normalize(&mut self) {
        let mut blocks = Vec::new();
        for block in std::mem::take(&mut self.blocks) {
            let kind = match block.kind {
                BlockKind::Heading(level) => BlockKind::Heading(level.clamp(1, 6)),
                kind => kind,
            };
            let mut current = Block {
                kind,
                spans: Vec::new(),
            };
            for span in block.spans {
                for (index, part) in span.text.split('\n').enumerate() {
                    if index > 0 {
                        blocks.push(current);
                        current = Block::default();
                    }
                    push_span(&mut current.spans, part, span.marks);
                }
            }
            blocks.push(current);
        }
        if blocks.is_empty() {
            blocks.push(Block::default());
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

    fn position_at_global_byte(&self, offset: usize) -> Position {
        let mut remaining = offset;
        for (index, block) in self.blocks.iter().enumerate() {
            if remaining <= block.len() {
                return self.clamp_position(Position {
                    block: index,
                    byte: remaining,
                });
            }
            remaining -= block.len() + 1;
        }
        let block = self.blocks.len().saturating_sub(1);
        Position {
            block,
            byte: self.blocks.get(block).map_or(0, Block::len),
        }
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

/// A replacement map in logical document coordinates. Formatting-only changes preserve
/// all positions. Positions inside removed text map to the requested replacement edge.
#[derive(Clone, Debug)]
pub struct PositionMap {
    before: Document,
    after: Document,
    steps: Vec<Replacement>,
}

#[derive(Clone, Debug)]
struct Replacement {
    range: Range<usize>,
    inserted_len: usize,
}

impl PositionMap {
    pub fn map(&self, position: Position, affinity: Affinity) -> Position {
        let mut offset = self.before.global_byte(position);
        for step in &self.steps {
            offset = if offset < step.range.start {
                offset
            } else if offset > step.range.end {
                offset - step.range.len() + step.inserted_len
            } else {
                step.range.start
                    + if affinity == Affinity::After {
                        step.inserted_len
                    } else {
                        0
                    }
            };
        }
        self.after.position_at_global_byte(offset)
    }

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
}

#[derive(Clone, Debug)]
pub struct Change {
    pub revision: u64,
    pub mapping: PositionMap,
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

impl HistoryEntry {
    fn retained_bytes(&self) -> usize {
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
        std::mem::size_of::<Self>()
            + document_bytes(&self.state.document)
            + document_bytes(&self.mapping.before)
            + document_bytes(&self.mapping.after)
            + self.mapping.steps.capacity() * std::mem::size_of::<Replacement>()
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    InsertText(String),
    Backspace,
    DeleteForward,
    ToggleMark(Mark),
    SetBlockKind(BlockKind),
    Undo,
    Redo,
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
        self.state.selection = Selection {
            anchor: self.document().clamp_position(selection.anchor),
            head: self.document().clamp_position(selection.head),
        };
        let head = self.state.selection.head;
        self.state.typing_marks = self.document().blocks[head.block].marks_at(head.byte);
    }

    pub fn selection_text(&self) -> String {
        let (start, end) = self.selection().ordered();
        let plain = self.document().plain_text();
        plain[self.document().global_byte(start)..self.document().global_byte(end)].to_owned()
    }

    pub fn apply(&mut self, command: Command) -> Option<Change> {
        match command {
            Command::InsertText(text) => self.insert_text(&text),
            Command::Backspace => self.backspace(),
            Command::DeleteForward => self.delete_forward(),
            Command::ToggleMark(mark) => self.toggle_mark(mark),
            Command::SetBlockKind(kind) => self.set_block_kind(kind),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
        }
    }

    fn publish(&mut self, before: &Document) -> Option<Change> {
        let steps = std::mem::take(&mut self.pending_steps);
        if before == self.document() {
            return None;
        }
        self.revision += 1;
        Some(Change {
            revision: self.revision,
            mapping: PositionMap {
                before: before.clone(),
                after: self.document().clone(),
                steps,
            },
        })
    }

    fn transaction(&mut self, action: impl FnOnce(&mut Self)) -> Option<Change> {
        self.transaction_in_group(None, action)
    }

    fn transaction_in_group(
        &mut self,
        group: Option<u64>,
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
        let change = self.publish(&before.document);
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
        self.transaction(|editor| editor.replace_selection(text))
    }

    pub fn insert_text_plain_grouped(&mut self, text: &str, group: u64) -> Option<Change> {
        let group =
            (!text.is_empty() && !text.contains(['\n', '\r']) && self.selection().is_empty())
                .then_some(group);
        self.transaction_in_group(group, |editor| editor.replace_selection(text))
    }

    fn insert_text_in_group(&mut self, text: &str, group: Option<u64>) -> Option<Change> {
        let group = group.filter(|_| {
            !text.contains(['\n', '\r']) && self.selection().is_empty() && !text.is_empty()
        });
        self.transaction_in_group(group, |editor| {
            let selection = editor.selection();
            if text == "\n" && selection.is_empty() {
                let block = &mut editor.state.document.blocks[selection.head.block];
                if block.is_empty() && block.kind != BlockKind::Paragraph {
                    block.kind = BlockKind::Paragraph;
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
        self.transaction(|editor| {
            editor.state.selection = Selection {
                anchor: editor.utf16_to_position(range.start),
                head: editor.utf16_to_position(range.end),
            };
            editor.replace_selection(text);
        })
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
        let mut replacement = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let kind = if index == 0 {
                first.kind.clone()
            } else {
                match first.kind {
                    BlockKind::Bullet => BlockKind::Bullet,
                    BlockKind::Task { .. } => BlockKind::Task { checked: false },
                    _ => BlockKind::Paragraph,
                }
            };
            let mut spans = if index == 0 {
                prefix.clone()
            } else {
                Vec::new()
            };
            push_span(&mut spans, part, self.state.typing_marks);
            if index == parts.len() - 1 {
                for span in &suffix {
                    push_span(&mut spans, &span.text, span.marks);
                }
            }
            replacement.push(Block { kind, spans });
        }
        let caret_block = start.block + parts.len() - 1;
        let caret_byte = if parts.len() == 1 {
            start.byte + parts[0].len()
        } else {
            parts.last().unwrap().len()
        };
        document.blocks.splice(start.block..=end.block, replacement);
        // Inserting a joiner or combining scalar can merge with the following grapheme.
        let caret_text = document.blocks[caret_block].text();
        self.state.selection = Selection::caret(Position {
            block: caret_block,
            byte: ceil_grapheme(&caret_text, caret_byte),
        });
    }

    pub fn backspace(&mut self) -> Option<Change> {
        self.transaction(|editor| {
            let selection = editor.selection();
            if selection.is_empty() {
                let head = selection.head;
                if head.byte == 0
                    && editor.document().blocks[head.block].kind != BlockKind::Paragraph
                {
                    editor.state.document.blocks[head.block].kind = BlockKind::Paragraph;
                    return;
                }
                editor.state.selection.anchor = editor.previous_position(head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_forward(&mut self) -> Option<Change> {
        self.transaction(|editor| {
            if editor.selection().is_empty() {
                editor.state.selection.head = editor.next_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_word_backward(&mut self) -> Option<Change> {
        self.transaction(|editor| {
            if editor.selection().is_empty() {
                editor.state.selection.anchor =
                    editor.previous_word_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn delete_word_forward(&mut self) -> Option<Change> {
        self.transaction(|editor| {
            if editor.selection().is_empty() {
                editor.state.selection.head = editor.next_word_position(editor.selection().head);
            }
            editor.replace_selection("");
        })
    }

    pub fn toggle_mark(&mut self, mark: Mark) -> Option<Change> {
        self.transaction(|editor| {
            if editor.selection().is_empty() {
                editor.state.typing_marks.toggle(mark);
                return;
            }
            let (start, end) = editor.selection().ordered();
            let mut all_enabled = true;
            for index in start.block..=end.block {
                let block = &editor.document().blocks[index];
                let range = if index == start.block { start.byte } else { 0 }
                    ..if index == end.block {
                        end.byte
                    } else {
                        block.len()
                    };
                for span in slice_spans(&block.spans, range) {
                    all_enabled &= span.marks.has(mark);
                }
            }
            for index in start.block..=end.block {
                let block = &mut editor.state.document.blocks[index];
                let start_byte = if index == start.block { start.byte } else { 0 };
                let end_byte = if index == end.block {
                    end.byte
                } else {
                    block.len()
                };
                let mut spans = slice_spans(&block.spans, 0..start_byte);
                for mut span in slice_spans(&block.spans, start_byte..end_byte) {
                    span.marks.set(mark, !all_enabled);
                    push_span(&mut spans, &span.text, span.marks);
                }
                for span in slice_spans(&block.spans, end_byte..block.len()) {
                    push_span(&mut spans, &span.text, span.marks);
                }
                block.spans = spans;
            }
            editor.state.typing_marks.set(mark, !all_enabled);
        })
    }

    pub fn set_block_kind(&mut self, kind: BlockKind) -> Option<Change> {
        self.transaction(|editor| {
            let (start, end) = editor.selection().ordered();
            let last = if end.byte == 0 && end.block > start.block {
                end.block - 1
            } else {
                end.block
            };
            for block in &mut editor.state.document.blocks[start.block..=last] {
                block.kind = kind.clone();
            }
            editor.state.document.normalize();
        })
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
        self.publish(&before_document)
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

    /// Commit existing marked text without replacing it (platform unmarkText).
    pub fn finish_composition(&mut self) {
        if let Some(composition) = self.composition.take()
            && composition.before.document != self.state.document
        {
            let mapping = PositionMap {
                before: composition.before.document.clone(),
                after: self.document().clone(),
                steps: composition.steps,
            };
            self.undo.push(HistoryEntry {
                state: composition.before,
                mapping,
            });
            self.redo.clear();
            self.trim_history();
        }
    }

    pub fn cancel_composition(&mut self) -> Option<Change> {
        let composition = self.composition.take()?;
        let mapping = PositionMap {
            before: composition.before.document.clone(),
            after: self.document().clone(),
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
        })
    }

    fn input_rules(&mut self) {
        let position = self.selection().head;
        let text = self.document().blocks[position.block].text();
        let before = &text[..position.byte];
        let kind = match before {
            "- " | "* " | "+ " => Some(BlockKind::Bullet),
            "- [ ] " | "[ ] " | "- [] " | "[] " => Some(BlockKind::Task { checked: false }),
            "- [x] " | "- [X] " | "[x] " | "[X] " => Some(BlockKind::Task { checked: true }),
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
            self.replace_selection("");
            self.state.document.blocks[position.block].kind = kind;
            return;
        }
        for (delimiter, mark) in [
            ("**", Mark::Bold),
            ("__", Mark::Bold),
            ("*", Mark::Italic),
            ("_", Mark::Italic),
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
            let block_start = self.document().global_byte(Position {
                block: position.block,
                byte: 0,
            });
            // Apply the trailing delimiter removal before the leading removal so
            // both offsets refer to the original block's bytes.
            self.pending_steps.push(Replacement {
                range: block_start + content_end..block_start + position.byte,
                inserted_len: 0,
            });
            self.pending_steps.push(Replacement {
                range: block_start + open..block_start + content_start,
                inserted_len: 0,
            });
            let block = &mut self.state.document.blocks[position.block];
            let mut spans = slice_spans(&block.spans, 0..open);
            for mut span in slice_spans(&block.spans, content_start..content_end) {
                span.marks.set(mark, true);
                push_span(&mut spans, &span.text, span.marks);
            }
            for span in slice_spans(&block.spans, position.byte..block.len()) {
                push_span(&mut spans, &span.text, span.marks);
            }
            block.spans = spans;
            self.state.selection = Selection::caret(Position {
                block: position.block,
                byte: open + content_end - content_start,
            });
            self.state.typing_marks.set(mark, false);
            return;
        }
    }
}

fn push_span(spans: &mut Vec<Span>, text: &str, marks: Marks) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.marks == marks
    {
        last.text.push_str(text);
    } else {
        spans.push(Span {
            text: text.to_owned(),
            marks,
        });
    }
}

fn slice_spans(spans: &[Span], range: Range<usize>) -> Vec<Span> {
    let mut result = Vec::new();
    let mut offset = 0;
    for span in spans {
        let start = range.start.saturating_sub(offset).min(span.text.len());
        let end = range.end.saturating_sub(offset).min(span.text.len());
        if start < end {
            push_span(&mut result, &span.text[start..end], span.marks);
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
