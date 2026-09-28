//! Conservative, source-preserving writes for documents shared with other editors.
//!
//! A textblock's text is its inline source, so the serializer already writes
//! every inline spelling as it was read. What it does not keep is the block
//! level: container prefixes and indentation, setext underlines, table
//! padding, blank-line runs, line endings, front matter. This codec retains the
//! original source and patches only what an edit changed, by one rule:
//!
//! > A source line is a prefix the containers own, the text of one line of a
//! > leaf block, and a suffix the block's own syntax owns. The document owns
//! > the text; the file owns the prefix and the suffix.
//!
//! That is the rule the parser reads with — a paragraph's continuation line is
//! whatever follows its `>` markers and indentation — so an edit to a leaf's
//! text replaces the text of the lines it changed and leaves their prefixes,
//! and every other line, as they were. A line added takes the continuation
//! form of the prefix before it; a line removed goes whole. What lives in a
//! prefix and is owned by the document — a task box, a heading's `#`s, a
//! callout's marker line — is patched in the prefix the same way.
//!
//! Top-level blocks are paired by shape across the edit. A block whose shape
//! changed — a paragraph split in two, a list converted — is respelled the way
//! the writer spells it, and only that block. Every candidate
//! has to read back as the block it stands for, and the whole file as the
//! edited document; unmapped source is never silently replaced.

use std::ops::Range;

use comrak::Arena;
use markraft_core::ends::KeptEnds;
use markraft_core::kind::{HEADING_LEVEL_ATTR, TASK_CHECKED_ATTR};
use markraft_core::{ChangeRange, ChangeSet, Fragment, Node, Schema, Transaction};

use crate::derive::BlockKind;
use crate::textblock::{Item, Items, block_kind};
use crate::{ParseError, commonmark_options, from_markdown, to_markdown};

/// A parsed document and its immutable, byte-preserving source baseline.
#[derive(Clone, Debug)]
pub struct SourceDocument {
    source: String,
    /// What an edit is written against: the file's reading, or, once a
    /// [`SourceTrack`] wrote the file for the editor's document, that document.
    document: Node,
    /// The file's own reading, block for block with `blocks`.
    read: Node,
    body_start: usize,
    blocks: Vec<Range<usize>>,
    mapped: bool,
    newline: &'static str,
}

/// A source-preserving save could not safely represent an editor operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// Rewritten source did not reparse into the edited document — even with
    /// the blocks the edit touched respelled — or the source's blocks could
    /// not be mapped at all. Retain the editor buffer and offer a separate
    /// export instead of overwriting.
    UnsupportedEdit,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedEdit => {
                "This edit cannot be saved without rewriting Markdown source the editor does not represent. Your edits are retained; save a separate copy or use another editor."
            }
        })
    }
}

impl std::error::Error for SourceError {}

impl SourceDocument {
    /// Parse UTF-8 Markdown, retaining BOM, front matter, line endings and gaps.
    /// YAML front matter is opaque application-independent source, not metadata.
    pub fn parse(schema: &Schema, source: &str) -> Result<Self, ParseError> {
        let body_start = body_start(source);
        let body = &source[body_start..];
        let (document, blocks, mapped) = read_blocks(schema, body, body_start)?;
        let newline = if body.contains("\r\n") {
            "\r\n"
        } else if body.contains('\r') {
            "\r"
        } else {
            "\n"
        };
        Ok(Self {
            source: source.to_owned(),
            read: document.clone(),
            document,
            body_start,
            blocks,
            mapped,
            newline,
        })
    }

    /// The editable semantic document, excluding opaque front matter.
    pub fn document(&self) -> &Node {
        &self.document
    }

    /// The exact original UTF-8 source, before any parser normalization.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Render an edited snapshot without rewriting unrelated source.
    ///
    /// This baseline is immutable, so undoing back to its document restores its
    /// exact bytes. Keep it for the lifetime of an editing session if undo must
    /// also restore the spelling that preceded an intervening save.
    ///
    /// Choosing a spelling only reads the stretches an edit changed, which
    /// stands for the whole file by how CommonMark reads blocks; see
    /// [`SourceDocument::validate_near`]. What is written is read back whole
    /// first — a save is not a keystroke — and a file that would not read as
    /// `document` is refused rather than written.
    pub fn render(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        let rendered = self.render_patched(schema, document)?;
        let rendered = self.renumbered(schema, document, rendered);
        if rendered == self.source {
            return Ok(rendered);
        }
        self.validate(schema, document, rendered)
    }

    /// Whether [`SourceDocument::render`] can write `document`, without the
    /// ordinals it counts again: a list numbered as the file had it reads the
    /// same, so the answer is the same, and an editor asks on every keystroke.
    pub fn check(&self, schema: &Schema, document: &Node) -> Result<(), SourceError> {
        self.render_patched(schema, document).map(|_| ())
    }

    /// `rendered` with the ordinals of each ordered list the edit touched
    /// counted again: a patch that adds an item leaves
    /// the lines after it with the numbers they had, `1.` `1.` `2.`, which a
    /// reader counts the same but a person reads as a mistake. A list written
    /// with one number throughout keeps it. Lists the edit did not reach keep
    /// whatever numbers the file gave them, and so does everything when the
    /// counted text would not read back as `document`.
    fn renumbered(&self, schema: &Schema, document: &Node, rendered: String) -> String {
        if rendered == self.source {
            return rendered;
        }
        let ordered = schema.node_id(crate::schema::ORDERED_LIST);
        let mut expected: Vec<Vec<i64>> = Vec::new();
        document.descendants(&mut |node, _, _, _| {
            if Some(node.type_id()) == ordered {
                let attrs = node.attrs();
                let start = attrs.get("start").and_then(|v| v.as_int()).unwrap_or(1);
                let same = attrs
                    .get("same_ordinal")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let step = i64::from(!same);
                expected.push(
                    (0..node.child_count() as i64)
                        .map(|i| start + i * step)
                        .collect(),
                );
            }
            true
        });
        if expected.is_empty() {
            return rendered;
        }
        // The lines of `rendered` the edit changed, between what it shares with
        // the source at either end.
        let old: Vec<&str> = self.source.split('\n').collect();
        let new: Vec<&str> = rendered.split('\n').collect();
        let same_start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let same_end = old[same_start..]
            .iter()
            .rev()
            .zip(new[same_start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let changed = same_start..new.len() - same_end;
        let body_line = self.source[..self.body_start].matches('\n').count();
        let body = rendered[self.body_start..]
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let arena = Arena::new();
        let root = crate::parse::parse_ast(&arena, &body, &commonmark_options());
        let mut lists = Vec::new();
        for node in root.descendants() {
            if let comrak::nodes::NodeValue::List(list) = &node.data.borrow().value
                && list.list_type == comrak::nodes::ListType::Ordered
            {
                lists.push(node);
            }
        }
        if lists.len() != expected.len() {
            return rendered;
        }
        // (line, byte column, digits, wanted) for every ordinal to change.
        let mut edits: Vec<(usize, usize, usize, i64)> = Vec::new();
        for (list, wanted) in lists.iter().zip(&expected) {
            let pos = list.data.borrow().sourcepos;
            let (first, last) = (body_line + pos.start.line - 1, body_line + pos.end.line - 1);
            if last < changed.start || first >= changed.end {
                continue;
            }
            for (item, wanted) in list.children().zip(wanted) {
                let at = item.data.borrow().sourcepos.start;
                let line = body_line + at.line - 1;
                let Some(text) = new.get(line) else {
                    return rendered;
                };
                let column = at.column - 1;
                let digits = text
                    .get(column..)
                    .unwrap_or_default()
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
                let written = text
                    .get(column..column + digits)
                    .and_then(|d| d.parse::<i64>().ok());
                if written != Some(*wanted) {
                    edits.push((line, column, digits, *wanted));
                }
            }
        }
        if edits.is_empty() {
            return rendered;
        }
        let mut lines: Vec<String> = new.iter().map(|line| (*line).to_owned()).collect();
        for (line, column, digits, wanted) in edits.into_iter().rev() {
            lines[line].replace_range(column..column + digits, &wanted.to_string());
        }
        // The counted lists lie in the blocks the edit changed, so reading
        // those is enough to tell, as it is for the edit itself.
        let old: Vec<Node> = self.document.children().cloned().collect();
        let new: Vec<Node> = document.children().cloned().collect();
        let kept = KeptEnds::of(&old, &new, |a, b| a == b);
        let changed = Changed {
            old: kept.old_middle(),
            new: kept.new_middle(),
        };
        self.validate_near(schema, document, lines.join("\n"), &changed)
            .unwrap_or(rendered)
    }

    /// The file for `document` as [`SourceDocument::render`] writes it, but
    /// read back only as far as the edit reached: the step a [`SourceTrack`]
    /// takes on every keystroke.
    fn step(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        let rendered = self.render_patched(schema, document)?;
        Ok(self.renumbered(schema, document, rendered))
    }

    /// This baseline moved on to `rendered`, a file [`SourceDocument::step`]
    /// wrote: its document and its blocks' bytes, read again only between the
    /// untouched blocks either side of what changed. `None` when the file
    /// cannot be read at all, which a file `step` wrote always can.
    fn advanced_in(
        &self,
        schema: &Schema,
        rendered: String,
        changed: Option<Range<usize>>,
    ) -> Option<Self> {
        if rendered == self.source {
            return Some(self.clone());
        }
        let near = match changed {
            Some(changed) => self.advanced_window(schema, &rendered, changed),
            None => self.advanced_near(schema, &rendered),
        };
        let next = match near {
            Some(next) => next,
            None => Self::parse(schema, &rendered).ok()?,
        };
        #[cfg(debug_assertions)]
        if let Ok(whole) = Self::parse(schema, &rendered) {
            debug_assert!(
                whole.read == next.read && whole.blocks == next.blocks,
                "a baseline read near its edit differs from the file read whole: {rendered:?}"
            );
        }
        Some(next)
    }

    /// This baseline standing for `document`, the editor's document it was
    /// just written for, when their blocks line up one to one.
    ///
    /// The file reads back as `document` up to what reading cannot give back —
    /// an empty paragraph typed into next, spaces a caret stands after — and
    /// the next edit is made to `document`, so that is what it is written
    /// against: a paragraph the file holds as a bare list marker is still the
    /// paragraph the next keystroke types into. An empty paragraph between
    /// blocks has no block in the file, so it stands aside; when the blocks
    /// still do not line up, the file's own reading stands. The blocks the
    /// edit left alone come over as the same nodes, which compare by identity.
    fn adopting(mut self, schema: &Schema, document: &Node) -> Self {
        let paragraph = schema.node_id(crate::schema::PARAGRAPH);
        let blocks: Vec<Node> = document
            .children()
            .filter(|block| Some(block.type_id()) != paragraph || block.child_count() > 0)
            .cloned()
            .collect();
        self.document = if blocks.len() == self.blocks.len() {
            if blocks.len() == document.child_count() {
                document.clone()
            } else {
                document.copy(Fragment::from_nodes(blocks))
            }
        } else {
            self.read.clone()
        };
        self
    }

    /// [`SourceDocument::advanced_in`] by reading the new bytes from the start of
    /// the untouched block before the change to the end of the one after it —
    /// `None` when that stretch cannot stand for the whole file: the change
    /// reached the front matter, the blocks either side read otherwise than
    /// they did, or a definition any link may read is in the stretch.
    fn advanced_near(&self, schema: &Schema, rendered: &str) -> Option<Self> {
        if !self.mapped || self.blocks.is_empty() {
            return None;
        }
        let (old, new) = (self.source.as_bytes(), rendered.as_bytes());
        let first = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let shorter = old.len().min(new.len());
        let last = old[first..]
            .iter()
            .rev()
            .zip(new[first..].iter().rev())
            .take(shorter - first)
            .take_while(|(a, b)| a == b)
            .count();
        if first < self.body_start {
            return None;
        }
        self.advanced_window(schema, rendered, first..old.len() - last)
    }

    // `changed` contains every byte the edit replaced in the previous source.
    // Transaction-aware writes already know this window and need no text diff.
    fn advanced_window(
        &self,
        schema: &Schema,
        rendered: &str,
        changed: Range<usize>,
    ) -> Option<Self> {
        let (old, new) = (self.source.as_bytes(), rendered.as_bytes());
        // The blocks the change touched are `touched`: a block that ends where
        // it starts, or starts where it ends, may have grown by it.
        let count = self.blocks.len();
        let touched_start = self
            .blocks
            .partition_point(|block| block.end < changed.start);
        let touched_end = self
            .blocks
            .partition_point(|block| block.start <= changed.end);
        let before = touched_start.checked_sub(1);
        let after = (touched_end < count).then_some(touched_end);
        let lo = before.unwrap_or(0);
        let hi = after.map_or(count, |after| after + 1);
        let start = before.map_or(self.body_start, |before| self.blocks[before].start);
        let old_end = after.map_or(old.len(), |after| self.blocks[after].end);
        let shift = |at: usize| (at + new.len()).checked_sub(old.len());
        let new_end = shift(old_end)?;
        let stretch = rendered.get(start..new_end)?;
        let (read, ranges, mapped) = read_blocks(schema, stretch, start).ok()?;
        if !mapped {
            return None;
        }
        let olds: Vec<Node> = self.read.children().cloned().collect();
        let defines = |nodes: &[Node]| {
            !crate::textblock::definition_candidates(
                schema,
                &self.read.copy(Fragment::from_nodes(nodes.iter().cloned())),
            )
            .is_empty()
        };
        let read_blocks: Vec<Node> = read.children().cloned().collect();
        if defines(&olds[lo..hi]) || defines(&read_blocks) {
            return None;
        }
        // Read on its own, the stretch has none of the note's definitions;
        // none of them changed, so its links resolve against the note's.
        let read = crate::textblock::resolve_references_from(schema, &read, &self.read);
        let mut read: Vec<Node> = read.children().cloned().collect();
        let anchors = usize::from(before.is_some()) + usize::from(after.is_some());
        if read.len() < anchors {
            return None;
        }
        if let Some(before) = before {
            if read[0] != olds[before] || ranges[0] != self.blocks[before] {
                return None;
            }
            read[0] = olds[before].clone();
        }
        if let Some(after) = after {
            let at = read.len() - 1;
            let was = &self.blocks[after];
            if read[at] != olds[after] || ranges[at] != (shift(was.start)?..shift(was.end)?) {
                return None;
            }
            read[at] = olds[after].clone();
        }
        let children = olds[..lo]
            .iter()
            .cloned()
            .chain(read)
            .chain(olds[hi..].iter().cloned());
        let blocks = self.blocks[..lo]
            .iter()
            .cloned()
            .chain(ranges)
            .chain(
                self.blocks[hi..]
                    .iter()
                    .map(|block| shift(block.start).unwrap_or(0)..shift(block.end).unwrap_or(0)),
            )
            .collect();
        let read = self.read.copy(Fragment::from_nodes(children));
        Some(Self {
            source: rendered.to_owned(),
            document: read.clone(),
            read,
            body_start: self.body_start,
            blocks,
            mapped: true,
            newline: self.newline,
        })
    }

    /// A transaction wholly inside one existing top-level block. The strict
    /// boundaries leave splits, joins and root insertions to the established
    /// snapshot writer; nested structure still uses the block's normal patcher.
    fn transaction_window(
        &self,
        before: &Node,
        after: &Node,
        changes: &ChangeSet,
    ) -> Option<Changed> {
        if !self.mapped || before != &self.document || before.child_count() != after.child_count() {
            return None;
        }
        let mut block = None;
        for change in changes.iter_changes() {
            let (from_a, to_a, from_b, to_b) = match change {
                ChangeRange::Replaced {
                    from_a,
                    to_a,
                    from_b,
                    to_b,
                    ..
                }
                | ChangeRange::Marked {
                    from_a,
                    to_a,
                    from_b,
                    to_b,
                    ..
                } => (from_a, to_a, from_b, to_b),
            };
            for (doc, from, to) in [(before, from_a, to_a), (after, from_b, to_b)] {
                let (index, start) = doc.content().find_index(from)?;
                let node = doc.maybe_child(index)?;
                if from <= start
                    || to >= start + node.node_size()
                    || block.is_some_and(|previous| previous != index)
                {
                    return None;
                }
                block = Some(index);
            }
        }
        let index = block?;
        self.blocks.get(index)?;
        Some(Changed {
            old: index..index + 1,
            new: index..index + 1,
        })
    }

    fn render_patched(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        self.render_changed(schema, document, None)
    }

    fn render_changed(
        &self,
        schema: &Schema,
        document: &Node,
        changed: Option<Changed>,
    ) -> Result<String, SourceError> {
        if document == &self.document {
            return Ok(self.source.clone());
        }
        // An empty Markdown file has an implicit editor paragraph but no AST
        // block. There is no existing syntax to disturb in this special case.
        if self.blocks.is_empty() {
            let expected = to_markdown(schema, document);
            if expected == to_markdown(schema, &self.document) {
                return Ok(self.source.clone());
            }
            let mut result = self.source.clone();
            if !self.source[self.body_start..].trim().is_empty() {
                // A reference-definition-only file has no semantic AST blocks.
                // Preserve those definitions before appending the new content.
                result.push_str(&self.newline.repeat(2));
            }
            result.push_str(&self.with_newlines(&expected));
            return self.validate(schema, document, result);
        }
        if !self.mapped {
            return Err(SourceError::UnsupportedEdit);
        }
        let old = self.document.content().as_slice();
        let new = document.content().as_slice();
        let changed = changed.unwrap_or_else(|| {
            let kept = KeptEnds::of(old, new, |a, b| a == b);
            Changed {
                old: kept.old_middle(),
                new: kept.new_middle(),
            }
        });
        let (prefix, suffix) = (changed.old.start, old.len() - changed.old.end);
        let (end, new_end) = (changed.old.end, changed.new.end);
        // Edits far apart are parsed and checked one by one, avoiding a parse
        // of the untouched blocks between the first change and the last.
        if let Some(done) = self.render_islands(
            schema,
            document,
            &old[prefix..end],
            &new[prefix..new_end],
            prefix,
        ) {
            return Ok(done);
        }
        let after = block_markdown(schema, document, &new[prefix..new_end]);
        // An edit the writer cannot see — the blocks it touched spell as they
        // did — leaves the file as it is. The blocks around it are the same
        // nodes, so only a whole spelling can tell whether they part the same.
        if after == block_markdown(schema, &self.document, &old[prefix..end])
            && to_markdown(schema, document) == to_markdown(schema, &self.document)
        {
            return Ok(self.source.clone());
        }
        let changed = Changed {
            old: prefix..end,
            new: prefix..new_end,
        };
        if prefix == end {
            return self.insert_blocks(
                schema,
                document,
                self.source.clone(),
                &changed,
                suffix,
                &after,
            );
        }
        if prefix == new_end {
            // Whole blocks went and nothing took their place: their lines go
            // with the gap before them. Patching them out would leave both
            // gaps, a blank line too many, where they stood.
            let mut result = self.source.clone();
            result.replace_range(self.dropped_range(prefix, end, suffix), "");
            return self.validate_near(schema, document, result, &changed);
        }
        let region = self.blocks[prefix].start..self.blocks[end - 1].end;
        let pieces = self.pieces(
            schema,
            document,
            &old[prefix..end],
            &new[prefix..new_end],
            prefix,
        );
        let mut result = self.source.clone();
        result.replace_range(region.clone(), &pieces);
        if let Ok(done) = self.validate_near(schema, document, result, &changed) {
            return Ok(done);
        }
        // The blocks the edit touched, written together the way the writer
        // writes them: what the file reads back as when their own spellings
        // cannot hold the edit — two lists that would run together, say.
        let mut result = self.source.clone();
        result.replace_range(region, &self.with_newlines(&after));
        self.validate_near(schema, document, result, &changed)
    }

    /// The source of the changed top-level blocks `new` in place of `old`,
    /// which begin at index `first` of the baseline.
    ///
    /// Each new block is paired with the old block of the same shape where
    /// there is one, and patched in that block's source — or, where its
    /// spelling cannot take the edit, respelled by the writer. A block paired
    /// with nothing is new and spelled by the writer; an old block nothing
    /// pairs with goes. Two blocks that both stand for old ones are parted by
    /// the gap the source has after the first; any other pair by the writer's
    /// blank line.
    fn pieces(
        &self,
        schema: &Schema,
        document: &Node,
        old: &[Node],
        new: &[Node],
        first: usize,
    ) -> String {
        let pairs = pair_in_order(old.len(), new.len(), |i, j| {
            if old[i] == new[j] {
                2
            } else {
                usize::from(same_shape(schema, &old[i], &new[j]))
            }
        });
        let partner = |j: usize| pairs.iter().find(|(_, b)| *b == j).map(|(a, _)| *a);
        let separator = self.newline.repeat(2);
        let mut out = String::new();
        let mut previous: Option<Option<usize>> = None;
        for (j, block) in new.iter().enumerate() {
            let paired = partner(j);
            let canonical = block_markdown(schema, document, std::slice::from_ref(block));
            let piece = paired
                .and_then(|i| {
                    let raw = &self.source[self.blocks[first + i].clone()];
                    let patched = patch_block(schema, &old[i], block, raw, self.newline)?;
                    reads_same(schema, &patched, &self.with_newlines(&canonical)).then_some(patched)
                })
                .unwrap_or_else(|| self.with_newlines(&canonical));
            // A block that spells nothing — an empty paragraph, typing in
            // progress — adds nothing, not even a gap.
            if piece.is_empty() {
                continue;
            }
            if let Some(before) = previous {
                match (before, paired) {
                    (Some(i), Some(_)) if first + i + 1 < self.blocks.len() => {
                        let gap = self.blocks[first + i].end..self.blocks[first + i + 1].start;
                        out.push_str(&self.source[gap]);
                    }
                    _ => out.push_str(&separator),
                }
            }
            out.push_str(&piece);
            previous = Some(paired);
        }
        out
    }

    /// The source top-level blocks `prefix..end` take, with the gap that parts
    /// them from the block before — or, first in the note, from the block
    /// after — so that dropping it leaves the one gap the blocks around need.
    fn dropped_range(&self, prefix: usize, end: usize, suffix: usize) -> Range<usize> {
        if prefix > 0 {
            self.blocks[prefix - 1].end..self.blocks[end - 1].end
        } else if suffix > 0 {
            self.blocks[prefix].start..self.blocks[end].start
        } else {
            self.blocks[prefix].start..self.blocks[end - 1].end
        }
    }

    /// Insert the Markdown of new top-level blocks between the untouched ones
    /// before `prefix` and the `suffix` untouched ones after it.
    ///
    /// The source already separates the blocks on either side, so new blocks go
    /// directly after the block before them with a separator of their own, and
    /// the existing gap is left to part them from the block after — adding a
    /// separator on both sides would double the gap that was already there.
    /// Blocks that spell nothing, such as an empty paragraph, add nothing.
    fn insert_blocks(
        &self,
        schema: &Schema,
        document: &Node,
        mut result: String,
        changed: &Changed,
        suffix: usize,
        after: &str,
    ) -> Result<String, SourceError> {
        let prefix = changed.old.start;
        if after.is_empty() {
            return self.validate_near(schema, document, result, changed);
        }
        let separator = self.newline.repeat(2);
        let blocks = self.with_newlines(after);
        let Some(before) = prefix.checked_sub(1).map(|index| &self.blocks[index]) else {
            let at = self
                .blocks
                .first()
                .map_or(self.body_start, |range| range.start);
            let mut insertion = blocks;
            if suffix > 0 {
                insertion.push_str(&separator);
            }
            result.insert_str(at, &insertion);
            return self.validate_near(schema, document, result, changed);
        };
        let mut candidate = result.clone();
        let following = self
            .following(schema, document, &changed.new)
            .unwrap_or_else(|| format!("{separator}{blocks}"));
        candidate.insert_str(before.end, &following);
        let placed = self.validate_near(schema, document, candidate, changed);
        if placed.is_ok() || suffix == 0 {
            return placed;
        }
        // The blocks either side of the insertion were not parted by a blank
        // line — a heading directly above a paragraph — so the new blocks take
        // the place of that gap with a separator of their own on both sides.
        // Anything but whitespace between them stays, with the new blocks
        // parted from it on both sides.
        let gap = before.end..self.blocks[prefix].start;
        let range = if result[gap.clone()].trim().is_empty() {
            gap
        } else {
            gap.end..gap.end
        };
        result.replace_range(range, &format!("{separator}{blocks}{separator}"));
        self.validate_near(schema, document, result, changed)
    }

    /// The writer's spelling of `document`'s blocks `new`, with the gap that
    /// parts them from the block before, spelled after that block as the
    /// writer would go on from it — `None` when there is no block before, or
    /// when spelling it with them changes its own spelling.
    ///
    /// How the writer parts blocks depends on the block before them: an
    /// empty paragraph after a divider writes as nothing, and after a
    /// paragraph as a blank line. Spelled on their own, the new blocks would
    /// not know which.
    fn following(&self, schema: &Schema, document: &Node, new: &Range<usize>) -> Option<String> {
        let before = document.children().nth(new.start.checked_sub(1)?)?.clone();
        let blocks = document.children().skip(new.start).take(new.len()).cloned();
        let alone = block_markdown(schema, document, std::slice::from_ref(&before));
        let together: Vec<Node> = std::iter::once(before).chain(blocks).collect();
        let together = block_markdown(schema, document, &together);
        let rest = together.strip_prefix(&alone)?;
        Some(self.with_newlines(rest))
    }

    fn with_newlines(&self, source: &str) -> String {
        source.replace('\n', self.newline)
    }

    fn validate(
        &self,
        schema: &Schema,
        target: &Node,
        source: String,
    ) -> Result<String, SourceError> {
        let parsed = from_markdown(schema, &source[self.body_start..])
            .map_err(|_| SourceError::UnsupportedEdit)?;
        if reads_as(schema, &parsed, target) {
            Ok(source)
        } else {
            Err(SourceError::UnsupportedEdit)
        }
    }

    /// [`SourceDocument::validate`], reading only as much of `source` as the
    /// edit can have changed when that is enough to tell.
    ///
    /// The blocks outside `changed` are the baseline's own, in its own bytes,
    /// and read back as they did — unless the edit reached past its blocks. It
    /// can do that at its edges, where an unclosed fence or a lazy line takes
    /// in the block beside it, and through the definitions a link anywhere
    /// resolves against. So the blocks it changed are read with an untouched
    /// block either side, against the whole document's definitions; when
    /// that stretch reads as the document's same stretch and the edit neither
    /// added nor removed a definition, the whole file reads back too. Anything
    /// else is read whole.
    fn validate_near(
        &self,
        schema: &Schema,
        target: &Node,
        source: String,
        changed: &Changed,
    ) -> Result<String, SourceError> {
        let window = self.window(changed);
        // The edit only moved what follows it by what it added or took away.
        let stop = (window.end + source.len()).checked_sub(self.source.len());
        let stretch = stop.and_then(|stop| source.get(window.start..stop));
        if stretch.is_some_and(|stretch| self.stretch_reads(schema, target, stretch, changed)) {
            return Ok(source);
        }
        self.validate(schema, target, source)
    }

    /// The baseline's bytes an edit of the blocks `changed` is checked in:
    /// from the start of the untouched block before them, or of the body, to
    /// the end of the untouched block after them, or of the file.
    fn window(&self, changed: &Changed) -> Range<usize> {
        let start = changed
            .old
            .start
            .checked_sub(1)
            .map_or(self.body_start, |before| self.blocks[before].start);
        let stop = self
            .blocks
            .get(changed.old.end)
            .map_or(self.source.len(), |after| after.end);
        start..stop
    }

    /// Whether `stretch`, the candidate's text in place of the baseline's
    /// [`SourceDocument::window`] of `changed`, reads as the edited document's
    /// same blocks. `false` too when the edit added, removed or changed a
    /// definition, which any link in the file may read.
    fn stretch_reads(
        &self,
        schema: &Schema,
        target: &Node,
        stretch: &str,
        changed: &Changed,
    ) -> bool {
        let old: Vec<&Node> = self.document.children().collect();
        let new: Vec<&Node> = target.children().collect();
        let defines = |block: &&Node| {
            !crate::textblock::definition_candidates(
                schema,
                &target.copy(Fragment::from_nodes([(*block).clone()])),
            )
            .is_empty()
        };
        if old[changed.old.clone()]
            .iter()
            .chain(&new[changed.new.clone()])
            .any(defines)
        {
            return false;
        }
        let from = changed.new.start - usize::from(changed.old.start > 0);
        let to = (changed.new.end + usize::from(changed.old.end < old.len())).min(new.len());
        let expected = target.copy(Fragment::from_nodes(
            new[from..to].iter().map(|&n| n.clone()),
        ));
        let Ok(read) = from_markdown(schema, stretch) else {
            return false;
        };
        reads_as(schema, &read, &expected)
            || reads_as(
                schema,
                &crate::textblock::resolve_references_from(schema, &read, target),
                &expected,
            )
    }

    /// The file for `document` when its changes are islands of blocks apart
    /// from each other, each spelled and checked on its own — `None` when there
    /// is one island, or when an island cannot be told apart from its
    /// surroundings that way, for the edit to be written as one.
    ///
    /// CommonMark reads blocks line by line against the blocks still open, and
    /// a block once closed never opens again. An untouched block therefore
    /// reads the same wherever the lines before it came from, once the block
    /// itself reads the same; so does everything after it. An island checked
    /// with an untouched block either side stands on its own, and islands are
    /// kept at least two untouched blocks apart, so no block is the edge of two.
    ///
    /// `old` and `new` are the blocks between the edit's first change and its
    /// last, which both documents hold from index `first`.
    fn render_islands(
        &self,
        schema: &Schema,
        document: &Node,
        old: &[Node],
        new: &[Node],
        first: usize,
    ) -> Option<String> {
        let islands = islands(old, new)?;
        if islands.len() < 2 {
            return None;
        }
        let old: Vec<Node> = self.document.children().cloned().collect();
        let new: Vec<Node> = document.children().cloned().collect();
        let mut patches = Vec::with_capacity(islands.len());
        for island in &islands {
            let island = Changed {
                old: first + island.old.start..first + island.old.end,
                new: first + island.new.start..first + island.new.end,
            };
            patches.push(self.patch_island(schema, document, &old, &new, &island)?);
        }
        let mut result = self.source.clone();
        for (range, text) in patches.into_iter().rev() {
            result.replace_range(range, &text);
        }
        Some(result)
    }

    /// The baseline's bytes to replace, and with what, for the island
    /// `changed`: the first of the spellings a single edit would try that
    /// reads back in the island's window.
    fn patch_island(
        &self,
        schema: &Schema,
        document: &Node,
        old: &[Node],
        new: &[Node],
        changed: &Changed,
    ) -> Option<(Range<usize>, String)> {
        let window = self.window(changed);
        let (first, end) = (changed.old.start, changed.old.end);
        let after = block_markdown(schema, document, &new[changed.new.clone()]);
        let separator = self.newline.repeat(2);
        let blocks = self.with_newlines(&after);
        let has_after = end < self.blocks.len();
        let mut candidates: Vec<(Range<usize>, String)> = Vec::new();
        let keep = window.start..window.start;
        if after == block_markdown(schema, &self.document, &old[first..end]) {
            candidates.push((keep.clone(), String::new()));
        }
        if first == end {
            // New blocks between untouched ones; see `insert_blocks`.
            match first.checked_sub(1) {
                _ if after.is_empty() => candidates.push((keep, String::new())),
                None => {
                    let at = self
                        .blocks
                        .first()
                        .map_or(self.body_start, |range| range.start);
                    let tail = if has_after { separator.as_str() } else { "" };
                    candidates.push((at..at, format!("{blocks}{tail}")));
                }
                Some(before) => {
                    let before = self.blocks[before].end;
                    let following = self
                        .following(schema, document, &changed.new)
                        .unwrap_or_else(|| format!("{separator}{blocks}"));
                    candidates.push((before..before, following));
                    if has_after {
                        let gap = before..self.blocks[first].start;
                        let range = if self.source[gap.clone()].trim().is_empty() {
                            gap
                        } else {
                            gap.end..gap.end
                        };
                        candidates.push((range, format!("{separator}{blocks}{separator}")));
                    }
                }
            }
        } else if changed.new.is_empty() {
            candidates.push((
                self.dropped_range(first, end, usize::from(has_after)),
                String::new(),
            ));
        } else {
            let region = self.blocks[first].start..self.blocks[end - 1].end;
            let pieces = self.pieces(
                schema,
                document,
                &old[first..end],
                &new[changed.new.clone()],
                first,
            );
            candidates.push((region.clone(), pieces));
            candidates.push((region, blocks));
        }
        candidates.into_iter().find(|(range, text)| {
            let stretch = format!(
                "{}{text}{}",
                &self.source[window.start..range.start],
                &self.source[range.end..window.end]
            );
            self.stretch_reads(schema, document, &stretch, changed)
        })
    }
}

/// A note's file across an editing session: the file as it was read, and the
/// file as the last write spelled it.
///
/// Each document is written against the last one written, limiting reparsing
/// to recent edits even after many changes. Source strings and block indexes
/// are still copied. A document that is the one read, as undoing everything
/// makes it, is written as the bytes read. The editor asks on every keystroke and
/// the save writes what it was answered, so a keystroke the track took is one
/// the save can write.
#[derive(Debug)]
pub struct SourceTrack {
    origin: std::sync::Arc<SourceDocument>,
    /// The file as last written; `None` while that is the file read, so a
    /// note never edited holds one copy of its text.
    current: std::sync::Mutex<Option<std::sync::Arc<SourceDocument>>>,
}

/// An immutable source baseline captured independently of subsequent edits.
#[derive(Clone, Debug)]
pub struct SourceSnapshot {
    origin: std::sync::Arc<SourceDocument>,
    current: Option<std::sync::Arc<SourceDocument>>,
}

impl SourceSnapshot {
    /// Render a committed document without advancing any editing track.
    pub fn render(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        if document == self.origin.document() {
            return Ok(self.origin.source().to_owned());
        }
        self.current
            .as_deref()
            .unwrap_or(&self.origin)
            .render(schema, document)
            .or_else(|_| self.origin.render(schema, document))
    }
}

impl SourceTrack {
    /// A track that starts at `origin`, the file as read.
    pub fn new(origin: SourceDocument) -> Self {
        Self {
            origin: std::sync::Arc::new(origin),
            current: std::sync::Mutex::new(None),
        }
    }

    /// Freeze the source under a short lock. Rendering happens after releasing it.
    pub fn snapshot(&self) -> SourceSnapshot {
        let current = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        SourceSnapshot {
            origin: self.origin.clone(),
            current,
        }
    }

    /// The file as it was read.
    pub fn origin(&self) -> &SourceDocument {
        &self.origin
    }

    /// The file for `document`, written against the last file this track
    /// wrote, and read back as far as the change reached.
    pub fn write(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        self.write_changed(schema, document, None)
    }

    /// Accept a complete transaction chain, advancing source only after its
    /// final document can be written. Intermediate appender states are not saved.
    /// When the current source matches the chain's starting document, token
    /// ranges locate local patches without diffing the whole document or file.
    /// Structural edits and unmatched baselines retain the snapshot fallback.
    pub fn apply_transactions(
        &self,
        schema: &Schema,
        transactions: &[Transaction],
    ) -> Result<(), SourceError> {
        let Some(first) = transactions.first() else {
            return Ok(());
        };
        if transactions
            .windows(2)
            .any(|pair| pair[0].new_doc() != pair[1].start_state().doc())
        {
            return Err(SourceError::UnsupportedEdit);
        }
        let document = transactions
            .last()
            .expect("nonempty transaction chain")
            .new_doc();
        let changes = transactions
            .iter()
            .skip(1)
            .try_fold(first.changes().clone(), |changes, transaction| {
                changes.compose(transaction.changes())
            })
            .ok();
        self.write_changed(
            schema,
            document,
            changes
                .as_ref()
                .map(|changes| (first.start_state().doc(), changes)),
        )
        .map(drop)
    }

    fn write_changed(
        &self,
        schema: &Schema,
        document: &Node,
        changes: Option<(&Node, &ChangeSet)>,
    ) -> Result<String, SourceError> {
        let mut held = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if document == &self.origin.document {
            *held = None;
            return Ok(self.origin.source.clone());
        }
        let current = held.as_deref().unwrap_or(&self.origin);
        if document == &current.document {
            return Ok(current.source.clone());
        }
        // What the file read could hold, the track holds too: an edit its
        // last file cannot take from where it stands is written against the
        // file read, as it would have been without the track.
        let local = changes
            .and_then(|(before, changes)| current.transaction_window(before, document, changes));
        let prepared = local.and_then(|changed| {
            let bytes =
                current.blocks[changed.old.start].start..current.blocks[changed.old.end - 1].end;
            let rendered = current
                .render_changed(schema, document, Some(changed))
                .ok()?;
            let rendered = current.renumbered(schema, document, rendered);
            Some((rendered, bytes))
        });
        let (base, rendered, bytes) = match prepared {
            Some((rendered, bytes)) => (current, rendered, Some(bytes)),
            None => match current.step(schema, document) {
                Ok(rendered) => (current, rendered, None),
                Err(_) => (
                    self.origin.as_ref(),
                    self.origin.step(schema, document)?,
                    None,
                ),
            },
        };
        // Build the next baseline before publishing it. A failed preparation
        // leaves the source that the caller's editor state still describes.
        let next = base
            .advanced_in(schema, rendered.clone(), bytes)
            .ok_or(SourceError::UnsupportedEdit)?;
        *held = Some(std::sync::Arc::new(next.adopting(schema, document)));
        Ok(rendered)
    }

    /// [`SourceTrack::write`], for the file a save puts on disk: read back
    /// whole, and refused rather than written when it would not read as
    /// `document`.
    pub fn save(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        let rendered = self.write(schema, document)?;
        if rendered == self.origin.source {
            return Ok(rendered);
        }
        self.origin.validate(schema, document, rendered)
    }
}

/// How many blocks, old and new together, one island may span. Finding where
/// an island ends compares every pair of blocks within this reach, so the
/// search costs its square; an edit that changes more blocks at once is
/// written as one, as a keystroke never does.
const ISLAND_REACH: usize = 64;

/// The islands of top-level blocks `new` changed from `old`, in order: each
/// the blocks between two runs of equal blocks, joined with the next when
/// fewer than two equal blocks part them. `None` when an island reaches
/// further than [`ISLAND_REACH`].
fn islands(old: &[Node], new: &[Node]) -> Option<Vec<Changed>> {
    let (mut i, mut j) = (0, 0);
    let mut out: Vec<Changed> = Vec::new();
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            i += 1;
            j += 1;
            continue;
        }
        // The nearest pair of equal blocks past here, fewest blocks skipped;
        // with none before the end, the island runs to it.
        let left = (old.len() - i) + (new.len() - j);
        let (a, b) =
            match (1..=left.min(ISLAND_REACH)).find_map(|reach| equal_at(old, new, i, j, reach)) {
                Some(skipped) => skipped,
                None if left <= ISLAND_REACH => (old.len() - i, new.len() - j),
                None => return None,
            };
        let island = Changed {
            old: i..i + a,
            new: j..j + b,
        };
        match out.last_mut() {
            Some(last) if island.old.start - last.old.end < 2 => {
                last.old.end = island.old.end;
                last.new.end = island.new.end;
            }
            _ => out.push(island),
        }
        i += a;
        j += b;
    }
    Some(out)
}

/// The blocks skipped, `(old, new)`, `reach` blocks in all past `i` and `j`,
/// to a pair of equal blocks.
fn equal_at(
    old: &[Node],
    new: &[Node],
    i: usize,
    j: usize,
    reach: usize,
) -> Option<(usize, usize)> {
    (0..=reach).find_map(|a| {
        let b = reach - a;
        (i + a < old.len() && j + b < new.len() && old[i + a] == new[j + b]).then_some((a, b))
    })
}

/// Blocks an edit changed: the baseline's top-level blocks `old` became the
/// edited document's `new`, and the blocks either side are equal in both.
#[derive(Clone, Debug, PartialEq)]
struct Changed {
    old: Range<usize>,
    new: Range<usize>,
}

/// Whether `parsed`, read from a file, is `target`, the editor's document, up
/// to what reading a file cannot give back.
fn reads_as(schema: &Schema, parsed: &Node, target: &Node) -> bool {
    if parsed == target || to_markdown(schema, parsed) == to_markdown(schema, target) {
        return true;
    }
    // CommonMark discards spaces at a paragraph/heading's end. During
    // typing those spaces are real editor content (the next keystroke can
    // make them internal). Keep their bytes in the candidate, but compare
    // the target using only this specific parser normalization. Code and
    // unsupported syntax still require the original strict comparison.
    let trimmed = without_trailing_spaces(schema, target);
    if trimmed != *target && to_markdown(schema, parsed) == to_markdown(schema, &trimmed) {
        return true;
    }
    // An empty paragraph is typing in progress — Return at the start of a
    // block, with nothing yet on the new line. The file holds nothing for
    // it, so a document that differs from its reading only by empty
    // paragraphs is the same note. Only the ones whose going changes
    // nothing else are let go; see `without_empty_paragraphs`.
    let bare = without_empty_paragraphs(schema, &trimmed);
    let read = without_empty_paragraphs(schema, parsed);
    bare != trimmed && (read == bare || to_markdown(schema, &read) == to_markdown(schema, &bare))
}

/// Whether two spellings of a block read as the same block, each read on its
/// own — without the definitions the rest of the note holds, which neither
/// has, so a reference link reads the same way in both.
fn reads_same(schema: &Schema, a: &str, b: &str) -> bool {
    match (from_markdown(schema, a), from_markdown(schema, b)) {
        (Ok(a), Ok(b)) => a == b || to_markdown(schema, &a) == to_markdown(schema, &b),
        _ => false,
    }
}

/// Whether `old` and `new` are the same block but for what the line model
/// patches in place: the text of each leaf, a paragraph or heading added or
/// dropped, a task box, a heading's level, a callout's marker. A table is one
/// shape whatever its rows: its rows are lines of their own, patched by
/// [`table_rows`].
fn same_shape(schema: &Schema, old: &Node, new: &Node) -> bool {
    if old.type_id() != new.type_id() || old.marks() != new.marks() {
        return false;
    }
    if old.attrs() != new.attrs() {
        let carried = prefix_attrs(schema, old)
            .iter()
            .fold(old.attrs().clone(), |attrs, name| {
                match new.attrs().get(name) {
                    Some(value) => attrs.with(*name, value.clone()),
                    None => attrs,
                }
            });
        if carried != *new.attrs() {
            return false;
        }
    }
    // A leaf is its text; a table is its rows; a list is its items — all
    // lines the line model patches, adds and drops on its own.
    let ty = old.type_id();
    if block_kind(schema, ty).is_some()
        || is_verbatim(schema, old)
        || schema.node_type(ty).is_leaf()
        || [
            crate::schema::TABLE,
            crate::schema::BULLET_LIST,
            crate::schema::ORDERED_LIST,
        ]
        .iter()
        .any(|name| schema.node_id(name) == Some(ty))
    {
        return true;
    }
    // Paragraphs and headings are lines the line model adds and drops; the
    // rest of the children are the shape.
    fn structure<'a>(schema: &Schema, node: &'a Node) -> Vec<&'a Node> {
        node.children()
            .filter(|child| block_kind(schema, child.type_id()).is_none())
            .collect()
    }
    let (before, after) = (structure(schema, old), structure(schema, new));
    before.len() == after.len()
        && before
            .into_iter()
            .zip(after)
            .all(|(a, b)| same_shape(schema, a, b))
}

/// The attributes of `node`'s type that are spelled in a line's prefix, on a
/// marker line of its own, or in the blank lines between items, and so can
/// change without the block's shape changing.
fn prefix_attrs(schema: &Schema, node: &Node) -> &'static [&'static str] {
    let ty = Some(node.type_id());
    if schema.node_id(crate::schema::TASK_ITEM) == ty {
        &[TASK_CHECKED_ATTR]
    } else if schema.node_id(crate::schema::HEADING) == ty {
        &[HEADING_LEVEL_ATTR]
    } else if schema.node_id(crate::schema::BLOCKQUOTE) == ty {
        &["callout", "fold", "title"]
    } else if [crate::schema::BULLET_LIST, crate::schema::ORDERED_LIST]
        .iter()
        .any(|name| schema.node_id(name) == ty)
    {
        &["tight"]
    } else {
        &[]
    }
}

/// Whether `node` is a block whose text is written line for line as it
/// stands — a code block, an HTML block, a raw block — rather than inline
/// source with atoms and marks.
fn is_verbatim(schema: &Schema, node: &Node) -> bool {
    block_kind(schema, node.type_id()).is_none()
        && schema.node_type(node.type_id()).has_inline_content()
        && node.children().all(Node::is_text)
}

/// What a leaf's lines hold.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LeafKind {
    /// Inline source: a paragraph's, a heading's.
    Text(BlockKind),
    /// A callout's marker, the first line of its quote.
    Marker,
    /// A code block's lines, between fences or indented.
    Code,
    /// An HTML or raw block's lines.
    Verbatim,
}

/// One run of lines a block owns the text of, in document order.
struct Leaf<'a> {
    kind: LeafKind,
    /// The node the lines belong to: the textblock, the code block, or for a
    /// marker the quote.
    node: &'a Node,
    /// The text of each line, as the file holds it.
    lines: Vec<String>,
    /// The task items whose box stands in the prefix of the first line —
    /// each item this leaf opens, outermost first.
    boxes: Vec<&'a Node>,
    /// Whether this leaf is the first block of a list item, and if so whether
    /// that list is loose. One with no text still takes the item's marker
    /// line, which holds the item open.
    opens_item: Option<bool>,
}

/// The leaves of `block` in document order, their text as the file holds it
/// (`as_written`) or as the writer would write it now. `None` where the block
/// holds something the line model does not cover: a table inside it, or an
/// atom the writer has no spelling for.
fn leaves<'a>(schema: &Schema, block: &'a Node, as_written: bool) -> Option<Vec<Leaf<'a>>> {
    fn walk<'a>(
        schema: &Schema,
        node: &'a Node,
        as_written: bool,
        parent: Option<&'a Node>,
        boxes: &mut Vec<&'a Node>,
        opens_item: &mut Option<bool>,
        out: &mut Vec<Leaf<'a>>,
    ) -> Option<()> {
        let ty = node.type_id();
        if schema.node_id(crate::schema::TABLE) == Some(ty) {
            return None;
        }
        if let Some(kind) = block_kind(schema, ty) {
            let text = if as_written {
                written_text(schema, node)
            } else {
                guarded_text(schema, node, kind)?
            };
            out.push(Leaf {
                kind: LeafKind::Text(kind),
                node,
                lines: text_lines(&text),
                boxes: std::mem::take(boxes),
                opens_item: std::mem::take(opens_item),
            });
            return Some(());
        }
        if is_verbatim(schema, node) {
            let text: String = node.children().filter_map(Node::text).collect();
            let kind = if schema.node_id(crate::schema::CODE_BLOCK) == Some(ty) {
                LeafKind::Code
            } else {
                LeafKind::Verbatim
            };
            out.push(Leaf {
                kind,
                node,
                lines: text_lines(&text),
                boxes: std::mem::take(boxes),
                opens_item: std::mem::take(opens_item),
            });
            return Some(());
        }
        if let Some(marker) = callout_marker(schema, node) {
            out.push(Leaf {
                kind: LeafKind::Marker,
                node,
                lines: vec![marker],
                boxes: std::mem::take(boxes),
                opens_item: std::mem::take(opens_item),
            });
        }
        if schema.node_id(crate::schema::TASK_ITEM) == Some(ty) {
            boxes.push(node);
        }
        if [crate::schema::LIST_ITEM, crate::schema::TASK_ITEM]
            .iter()
            .any(|name| schema.node_id(name) == Some(ty))
        {
            let loose = parent.is_some_and(|list| !crate::preset::written_tight(schema, list));
            *opens_item = Some(loose);
        }
        for child in node.children() {
            walk(
                schema,
                child,
                as_written,
                Some(node),
                boxes,
                opens_item,
                out,
            )?;
        }
        Some(())
    }
    let mut out = Vec::new();
    walk(
        schema,
        block,
        as_written,
        None,
        &mut Vec::new(),
        &mut None,
        &mut out,
    )?;
    Some(out)
}

/// The lines of a leaf's text: none for no text at all.
fn text_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_owned).collect()
    }
}

/// The marker line a callout quote opens with, if `node` is one.
fn callout_marker(schema: &Schema, node: &Node) -> Option<String> {
    if schema.node_id(crate::schema::BLOCKQUOTE) != Some(node.type_id()) {
        return None;
    }
    let attr = |name: &str| node.attrs().get(name).and_then(|value| value.as_str());
    let kind = attr("callout").filter(|kind| !kind.is_empty())?;
    let callout = crate::callout::Callout {
        kind: kind.to_owned(),
        fold: attr("fold").unwrap_or_default().to_owned(),
        title: attr("title").unwrap_or_default().to_owned(),
    };
    Some(callout.marker())
}

/// A textblock's text as the file holds it: its inline source, with each
/// atom's own spelling and `\n` for a line break.
fn written_text(schema: &Schema, block: &Node) -> String {
    let mut out = String::new();
    for item in &Items::from_nodes(schema, block.children()).0 {
        match item {
            Item::Char(c) => out.push(*c),
            Item::Break => out.push('\n'),
            Item::Atom(atom) => out.push_str(&crate::textblock::atom_spelling(schema, atom)),
        }
    }
    out
}

/// A textblock's text as the writer puts it in the file: its lines settled,
/// with its guard's backslashes and each atom's own spelling — or `None` when
/// it holds an atom the writer has no spelling for.
fn guarded_text(schema: &Schema, block: &Node, kind: BlockKind) -> Option<String> {
    let items = crate::serialize::canonical_lines(Items::from_nodes(schema, block.children()));
    let insertions = items.guard_insertions(schema, kind, None);
    let mut out = String::new();
    let mut next = insertions.iter().peekable();
    for (index, item) in items.0.iter().enumerate() {
        while next.next_if(|at| **at == index).is_some() {
            out.push('\\');
        }
        match item {
            Item::Char(c) => out.push(*c),
            Item::Break => out.push('\n'),
            Item::Atom(atom) => {
                let spelling = crate::textblock::atom_spelling(schema, atom);
                if spelling == markraft_core::projection::OBJECT_REPLACEMENT.to_string() {
                    return None;
                }
                out.push_str(&spelling);
            }
        }
    }
    Some(out)
}

/// A block's source cut into lines, each with the line ending that closes it
/// — none for a last line the source ends without one.
fn source_lines(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = raw.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if matches!(bytes[offset], b'\r' | b'\n') {
            let mut end = offset + 1;
            if bytes[offset] == b'\r' && bytes.get(offset + 1) == Some(&b'\n') {
                end += 1;
            }
            out.push((raw[start..offset].to_owned(), raw[offset..end].to_owned()));
            start = end;
            offset = end;
        } else {
            offset += 1;
        }
    }
    if start < raw.len() || out.is_empty() {
        out.push((raw[start..].to_owned(), String::new()));
    }
    out
}

/// Where a leaf's lines stand in a block's source.
struct Located {
    /// The index of its first line, and how many lines it has.
    first: usize,
    count: usize,
    /// What each line holds before the leaf's text.
    prefixes: Vec<String>,
    /// The line ending that closes each line.
    ends: Vec<String>,
    /// What the last line holds after it: whitespace the reader drops, an ATX
    /// heading's closing sequence.
    suffix: String,
    /// The prefix a line added to a leaf with no lines takes.
    base: String,
    /// The line that closes a fenced code block, after its lines.
    close: Option<(String, String)>,
}

/// The characters a continuation line's prefix is made of: what the parser
/// strips from one.
fn is_prefix_char(c: char) -> bool {
    matches!(c, ' ' | '\t' | '>')
}

/// Whether `line` ends with `text` after a prefix of nothing but `>` markers
/// and whitespace: what a continuation line is to the parser.
fn continues(line: &str, text: &str) -> bool {
    line.ends_with(text) && line[..line.len() - text.len()].chars().all(is_prefix_char)
}

/// Whether `line` is a list item's marker and nothing else after the
/// container prefix: `-`, `+`, `*`, `1.` or `1)`, with a task box or not.
fn is_bare_marker(line: &str) -> bool {
    let content = line.trim_start_matches(is_prefix_char).trim_end();
    let rest = match content.strip_prefix(['-', '+', '*']) {
        Some(rest) => rest,
        None => {
            let digits = content.len()
                - content
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .len();
            if digits == 0 {
                return false;
            }
            match content[digits..].strip_prefix(['.', ')']) {
                Some(rest) => rest,
                None => return false,
            }
        }
    };
    let rest = rest.trim_start();
    rest.is_empty() || ["[ ]", "[x]", "[X]"].contains(&rest)
}

/// Whether `line` is a fence line: its content, after any container prefix,
/// opens with a run of at least three backticks or tildes. Answers the run.
fn fence_of(line: &str) -> Option<&str> {
    let content = line.trim_start_matches(is_prefix_char);
    let run = content.len() - content.trim_start_matches(['`', '~']).len();
    let first = content.chars().next()?;
    (run >= 3 && content[..run].chars().all(|c| c == first)).then(|| &content[..run])
}

/// Find where `leaf`'s lines stand in `lines` from `cursor` on, moving the
/// cursor past them. `Some(None)` is a leaf with no lines of its own — an
/// empty paragraph, an empty code block — and `None` a leaf whose lines could
/// not be found, so the block cannot be patched.
fn locate(
    leaf: &Leaf<'_>,
    lines: &[(String, String)],
    cursor: &mut usize,
) -> Option<Option<Located>> {
    let texts = &leaf.lines;
    let n = texts.len();
    match leaf.kind {
        LeafKind::Text(_) | LeafKind::Marker => {
            let kind = match leaf.kind {
                LeafKind::Text(kind) => Some(kind),
                _ => None,
            };
            if n == 0 {
                // An item whose first block is empty is its marker alone on
                // its line: that line is the leaf's, and text typed into the
                // leaf goes there.
                let next = (*cursor..lines.len())
                    .find(|&index| !lines[index].0.chars().all(is_prefix_char));
                return Some(
                    next.filter(|&index| {
                        leaf.opens_item.is_some() && is_bare_marker(&lines[index].0)
                    })
                    .map(|index| {
                        *cursor = index + 1;
                        let (line, end) = &lines[index];
                        Located {
                            first: index,
                            count: 1,
                            prefixes: vec![line.clone()],
                            ends: vec![end.clone()],
                            suffix: String::new(),
                            base: line.clone(),
                            close: None,
                        }
                    }),
                );
            }
            for first in *cursor..lines.len() {
                let Some(located) = locate_text(texts, kind, lines, first) else {
                    continue;
                };
                *cursor = first + n;
                return Some(Some(located));
            }
            None
        }
        LeafKind::Code | LeafKind::Verbatim => {
            let start = (*cursor..lines.len())
                .find(|&index| !lines[index].0.chars().all(is_prefix_char))?;
            let fence = (leaf.kind == LeafKind::Code)
                .then(|| fence_of(&lines[start].0))
                .flatten();
            let first = if fence.is_some() { start + 1 } else { start };
            if first + n > lines.len() {
                return None;
            }
            let prefixes: Vec<String> = (0..n)
                .map(|j| {
                    let line = &lines[first + j].0;
                    continues(line, &texts[j])
                        .then(|| line[..line.len() - texts[j].len()].to_owned())
                })
                .collect::<Option<_>>()?;
            let base = match fence {
                Some(_) => {
                    let line = &lines[start].0;
                    line[..line.len() - line.trim_start_matches(is_prefix_char).len()].to_owned()
                }
                None => prefixes.first().cloned().unwrap_or_default(),
            };
            *cursor = first + n;
            let close = fence.and_then(|run| {
                lines
                    .get(first + n)
                    .filter(|(line, _)| fence_of(line).is_some_and(|close| close.starts_with(run)))
                    .cloned()
            });
            if close.is_some() {
                *cursor += 1;
            }
            Some(Some(Located {
                first,
                count: n,
                prefixes,
                ends: lines[first..first + n]
                    .iter()
                    .map(|(_, end)| end.clone())
                    .collect(),
                suffix: String::new(),
                base,
                close,
            }))
        }
    }
}

/// The lines of a textblock's text laid over `lines` from `first`: the first
/// line ends with the first text after whatever prefix, the rest are
/// continuation lines, and the last drops the whitespace — and, for a heading,
/// the closing `#`s — the reader drops.
fn locate_text(
    texts: &[String],
    kind: Option<BlockKind>,
    lines: &[(String, String)],
    first: usize,
) -> Option<Located> {
    let n = texts.len();
    if first + n > lines.len() {
        return None;
    }
    let mut prefixes = Vec::with_capacity(n);
    let mut suffix = String::new();
    for j in 0..n {
        let line = &lines[first + j].0;
        let text = &texts[j];
        let bodies: Vec<&str> = if j + 1 == n {
            let trimmed = line.trim_end_matches([' ', '\t']);
            let mut bodies = vec![trimmed];
            if kind == Some(BlockKind::Heading) {
                let hashes = trimmed.trim_end_matches('#');
                if hashes.len() < trimmed.len() && hashes.ends_with([' ', '\t']) {
                    bodies.insert(0, hashes.trim_end_matches([' ', '\t']));
                }
            }
            bodies
        } else {
            vec![line.as_str()]
        };
        let body = bodies.into_iter().find(|body| {
            body.ends_with(text.as_str())
                && (j == 0 || body[..body.len() - text.len()].chars().all(is_prefix_char))
        })?;
        prefixes.push(body[..body.len() - text.len()].to_owned());
        if j + 1 == n {
            suffix = line[body.len()..].to_owned();
        }
    }
    let base = prefixes[0].clone();
    Some(Located {
        first,
        count: n,
        prefixes,
        ends: lines[first..first + n]
            .iter()
            .map(|(_, end)| end.clone())
            .collect(),
        suffix,
        base,
        close: None,
    })
}

/// The prefix a line continuing the one with `prefix` takes: its markers and
/// boxes as the spaces they stand over, its `>` markers and whitespace as they
/// are.
fn continuation(prefix: &str) -> String {
    prefix
        .chars()
        .map(|c| if is_prefix_char(c) { c } else { ' ' })
        .collect()
}

/// The lines of a block written out so far.
struct Emitted {
    out: Vec<(String, String)>,
    /// How many of them are lines of a leaf.
    leaf_lines: usize,
    /// The prefix of the last leaf line, which a new line continues, and of
    /// the last one that opens a list item, which a new item's takes.
    last_prefix: Option<String>,
    last_opener: Option<String>,
}

/// The patch of `old`'s source `raw` that holds `new`, or `None` where its
/// spelling cannot take the edit and the writer spells the block instead.
///
/// The block is patched as its lines: each leaf of `new` is paired with the
/// leaf of `old` it is an edit of, a paired leaf keeps every line whose text
/// it keeps and gets the text of the lines that changed, and the lines
/// between leaves — blank lines, fences, a setext underline — stay with the
/// leaf before them. A leaf with no partner is a paragraph or heading added
/// or removed whole: any other kind brings syntax of its own that only the
/// writer spells.
fn patch_block(
    schema: &Schema,
    old: &Node,
    new: &Node,
    raw: &str,
    newline: &str,
) -> Option<String> {
    if old == new {
        return Some(raw.to_owned());
    }
    if schema.node_id(crate::schema::TABLE) == Some(old.type_id()) {
        return table_rows(schema, old, new, raw, newline);
    }
    if !same_shape(schema, old, new) {
        return None;
    }
    let before = leaves(schema, old, true)?;
    let after = leaves(schema, new, false)?;
    let lines = source_lines(raw);
    let mut cursor = 0;
    let located: Vec<Option<Located>> = before
        .iter()
        .map(|leaf| locate(leaf, &lines, &mut cursor))
        .collect::<Option<_>>()?;
    let starts: Vec<usize> = located.iter().flatten().map(|place| place.first).collect();
    let opener_index = located.iter().position(Option::is_some)?;
    let opener = located[opener_index].as_ref()?;
    let opener_line = opener.first;
    let opener_prefix = opener
        .prefixes
        .first()
        .cloned()
        .unwrap_or_else(|| opener.base.clone());
    // The lines after a leaf's own up to the next leaf's: what parts them.
    let trivia_after = |place: &Located| -> &[(String, String)] {
        let end = place.first + place.count + usize::from(place.close.is_some());
        let next = starts
            .iter()
            .copied()
            .find(|&start| start >= end)
            .unwrap_or(lines.len());
        &lines[end..next]
    };
    let pairs = pair_in_order(before.len(), after.len(), |i, j| {
        let (a, b) = (&before[i], &after[j]);
        if a.kind != b.kind || a.node.type_id() != b.node.type_id() {
            0
        } else {
            1 + usize::from(a.lines == b.lines)
        }
    });
    let partner = |j: usize| pairs.iter().find(|(_, b)| *b == j).map(|(a, _)| *a);
    let mut cur = Emitted {
        out: lines[..opener_line].to_vec(),
        leaf_lines: 0,
        last_prefix: None,
        last_opener: None,
    };
    // One line of a leaf, the `k`th of its lines, kept from old line
    // `from_old` of the leaf `was` at `place` where it is one the leaf had.
    //
    // The block's first line keeps the prefix the block opens with. A line
    // kept from an old one keeps that line's prefix — but for the old first
    // line, which takes the continuation form once another line stands
    // before it, unless it still opens an item and so keeps its marker. A
    // line that opens a new item takes the prefix of the last line that
    // opened one; any other new line continues the last leaf line. The boxes
    // in a prefix that opens items are patched where the prefix is kept.
    let push = |cur: &mut Emitted,
                text: &str,
                k: usize,
                from_old: Option<usize>,
                leaf: &Leaf<'_>,
                was: Option<&Leaf<'_>>,
                place: Option<&Located>|
     -> Option<()> {
        let mut boxes: Option<(&[&Node], &[&Node])> = None;
        let mut prefix = if cur.leaf_lines == 0 {
            boxes = Some((&before[opener_index].boxes, &leaf.boxes));
            opener_prefix.clone()
        } else {
            match (from_old, place) {
                (Some(0), Some(place))
                    if place.first == opener_line && leaf.opens_item.is_none() =>
                {
                    continuation(&opener_prefix)
                }
                (Some(index), Some(place)) => {
                    if index == 0 {
                        boxes = Some((&was?.boxes, &leaf.boxes));
                    }
                    place.prefixes[index].clone()
                }
                _ if k == 0 && leaf.opens_item.is_some() => {
                    // An ordered item would carry the number of the one
                    // before it; the writer numbers the list afresh.
                    let opener = cur.last_opener.clone()?;
                    if !leaf.boxes.is_empty() || opener.chars().any(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    opener
                }
                _ => continuation(cur.last_prefix.as_deref()?),
            }
        };
        if let Some((opened, opens)) = boxes {
            prefix = patch_boxes(prefix, opened, opens)?;
        }
        if k == 0
            && let (LeafKind::Text(BlockKind::Heading), Some(was)) = (leaf.kind, was)
        {
            prefix = patch_level(prefix, was.node, leaf.node)?;
        }
        if k == 0 && leaf.opens_item.is_some() {
            cur.last_opener = Some(prefix.clone());
        }
        cur.last_prefix = Some(prefix.clone());
        let end = match from_old {
            Some(index) => place?.ends[index].clone(),
            None => newline.to_owned(),
        };
        cur.out.push((format!("{prefix}{text}"), end));
        cur.leaf_lines += 1;
        Some(())
    };
    // The lines that parted the last leaf emitted from the next one in the
    // source, not yet written out; and whether that leaf was a marker line
    // alone, which nothing parts from the item's next block.
    let mut pending: &[(String, String)] = &[];
    let mut after_marker = false;
    let mut after_paired = false;
    // Whether the last leaf emitted stands for a marker alone on its line in
    // the source, which nothing parted from the item's next block.
    let mut after_bare = false;
    let mut first = true;
    for (j, leaf) in after.iter().enumerate() {
        let paired = partner(j).map(|i| (&before[i], located[i].as_ref()));
        let marker_only = leaf.lines.is_empty() && leaf.opens_item.is_some();
        if leaf.lines.is_empty() && !marker_only {
            continue;
        }
        // What parts this leaf from the one before it. Two leaves that both
        // stand for old ones, in a list as loose or as tight as it was, keep
        // the lines the source had between them. Otherwise the source's own
        // lines that are not blank — a setext underline — stay, and a blank
        // line parts the leaves where the writer would put one: before an
        // item of a loose list, and before any other block but the one a
        // marker line alone opens.
        if !first {
            // Text typed onto a marker that stood alone needs the blank line
            // its source never had before the item's next block.
            let kept = after_paired
                && (after_marker || !after_bare)
                && paired.is_some_and(|(was, _)| was.opens_item == leaf.opens_item);
            if kept {
                cur.out.extend_from_slice(pending);
            } else {
                cur.out.extend(
                    pending
                        .iter()
                        .filter(|(line, _)| !line.chars().all(is_prefix_char))
                        .cloned(),
                );
                let blank = match leaf.opens_item {
                    Some(loose) => loose,
                    None => !after_marker,
                };
                if blank {
                    let markers: String = cur
                        .last_prefix
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| *c == '>')
                        .collect();
                    cur.out.push((markers, newline.to_owned()));
                }
            }
        }
        first = false;
        pending = &[];
        match paired {
            Some((was, Some(place))) => {
                // A bare marker line is the one line of a leaf with no text.
                let bare = [String::new()];
                let old_lines: &[String] = if was.lines.is_empty() && place.count == 1 {
                    &bare
                } else {
                    &was.lines
                };
                let new_lines = &leaf.lines;
                let head = old_lines
                    .iter()
                    .zip(new_lines)
                    .take_while(|(a, b)| a == b)
                    .count();
                let tail = old_lines[head..]
                    .iter()
                    .rev()
                    .zip(new_lines[head..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                let texts: &[String] = if marker_only { &bare[..] } else { new_lines };
                let new_count = texts.len();
                for (k, text) in texts.iter().enumerate() {
                    let from_old = if k < head {
                        Some(k)
                    } else if k >= new_count - tail {
                        Some(k + old_lines.len() - new_count)
                    } else if k < old_lines.len() - tail {
                        Some(k)
                    } else {
                        None
                    };
                    push(&mut cur, text, k, from_old, leaf, Some(was), Some(place))?;
                }
                if !new_lines.is_empty()
                    && let Some((line, _)) = cur.out.last_mut()
                {
                    line.push_str(&place.suffix);
                }
                if let Some(close) = &place.close {
                    cur.out.push(close.clone());
                }
                pending = trivia_after(place);
            }
            _ => {
                // A paragraph or heading added whole; any other kind brings
                // syntax of its own that only the writer spells.
                if !matches!(leaf.kind, LeafKind::Text(_)) {
                    return None;
                }
                let texts: &[String] = if marker_only {
                    &[String::new()]
                } else {
                    &leaf.lines
                };
                for (k, text) in texts.iter().enumerate() {
                    push(&mut cur, text, k, None, leaf, None, None)?;
                }
            }
        }
        after_marker = marker_only;
        after_paired = matches!(paired, Some((_, Some(_))));
        after_bare =
            matches!(paired, Some((was, Some(place))) if was.lines.is_empty() && place.count == 1);
    }
    // What followed the last leaf stays but for its blank lines: a setext
    // underline, not the gap a dropped leaf left.
    cur.out.extend(
        pending
            .iter()
            .filter(|(line, _)| !line.chars().all(is_prefix_char))
            .cloned(),
    );
    // A leaf of any other kind dropped would leave its fences behind.
    if before.iter().enumerate().any(|(index, leaf)| {
        !matches!(leaf.kind, LeafKind::Text(_)) && !pairs.iter().any(|(i, _)| *i == index)
    }) {
        return None;
    }
    let last_end = lines.last().map(|(_, end)| end.clone()).unwrap_or_default();
    let count = cur.out.len();
    Some(
        cur.out
            .into_iter()
            .enumerate()
            .map(|(index, (line, end))| {
                let end = if index + 1 == count {
                    last_end.clone()
                } else if end.is_empty() {
                    newline.to_owned()
                } else {
                    end
                };
                line + &end
            })
            .collect(),
    )
}

/// `prefix`, the block's first line's, with the box of each task item the
/// block opens with ticked or unticked as `now` has it.
fn patch_boxes(mut prefix: String, was: &[&Node], now: &[&Node]) -> Option<String> {
    let checked = |item: &Node| {
        item.attrs()
            .get(TASK_CHECKED_ATTR)
            .and_then(|value| value.as_bool())
    };
    if was.len() != now.len() {
        return None;
    }
    let boxes: Vec<usize> = prefix
        .match_indices('[')
        .filter(|(at, _)| matches!(prefix.get(at + 1..at + 3), Some(" ]" | "x]" | "X]")))
        .map(|(at, _)| at)
        .collect();
    if boxes.len() < was.len() {
        return None;
    }
    for (nth, (a, b)) in was.iter().zip(now).enumerate() {
        if checked(a) != checked(b) {
            let at = boxes[nth] + 1;
            prefix.replace_range(at..at + 1, if checked(b) == Some(true) { "x" } else { " " });
        }
    }
    Some(prefix)
}

/// `prefix`, a heading's first line's, with its `#`s as many as `now`'s
/// level — or `None` for a setext heading, which has none to change.
fn patch_level(prefix: String, was: &Node, now: &Node) -> Option<String> {
    let level = |node: &Node| {
        node.attrs()
            .get(HEADING_LEVEL_ATTR)
            .and_then(|value| value.as_int())
    };
    if level(was) == level(now) {
        return Some(prefix);
    }
    let trimmed = prefix.trim_end_matches([' ', '\t']);
    let hashes = trimmed.trim_end_matches('#');
    if hashes.len() == trimmed.len() {
        return None;
    }
    let level = usize::try_from(level(now)?).ok()?;
    Some(format!(
        "{hashes}{}{}",
        "#".repeat(level),
        &prefix[trimmed.len()..]
    ))
}

/// A top-level table's source, `raw`, rewritten row by row from `old` to
/// `new`, or `None` when the two differ other than in their rows — a column
/// added or aligned — or `raw` is not one line per row, and the caller falls
/// back to patching the table whole.
///
/// The rows are matched as a diff would match lines: those equal at either end
/// keep their lines byte for byte, and those between pair up in order, the
/// surplus added or dropped. A paired row keeps its line's pipes and the
/// spaces around each cell, only the text of a changed cell replaced; an added
/// row takes its spacing from the header — `|a|b|` makes `|z| |`, `| a | b |`
/// makes `| z |  |` — rather than the padding the writer would give a table
/// of its own, which would sit oddly among rows written by hand.
fn table_rows(schema: &Schema, old: &Node, new: &Node, raw: &str, newline: &str) -> Option<String> {
    if schema.node_id(crate::schema::TABLE) != Some(old.type_id()) || !old.same_markup(new) {
        return None;
    }
    let columns = old.child(0).child_count();
    if new.children().any(|row| row.child_count() != columns)
        || old.children().any(|row| row.child_count() != columns)
    {
        return None;
    }
    let lines: Vec<&str> = raw.split(newline).collect();
    if lines.len() != old.child_count() + 1 {
        return None;
    }
    // Row `index`'s line: the header's is the first, the delimiter row sits
    // between it and the body.
    let line_of = |index: usize| lines[if index == 0 { 0 } else { index + 1 }];
    let header = RowLine::parse(line_of(0))?;
    let (o, n) = (old.child_count(), new.child_count());
    let head = old
        .children()
        .zip(new.children())
        .take_while(|(a, b)| a == b)
        .count();
    let tail = old
        .children()
        .rev()
        .zip(new.children().rev())
        .take(o.min(n) - head)
        .take_while(|(a, b)| a == b)
        .count();
    // The old row each new row keeps the line of, if any.
    let mut source_of: Vec<Option<usize>> = (0..n)
        .map(|index| {
            if index < head {
                Some(index)
            } else if index >= n - tail {
                Some(index + o - n)
            } else {
                None
            }
        })
        .collect();
    // Two rows paired count for one more than their equal cells, so an edited
    // row pairs rather than reading as one row dropped and another added.
    let pairs = pair_in_order(o - tail - head, n - tail - head, |i, j| {
        1 + old
            .child(head + i)
            .children()
            .zip(new.child(head + j).children())
            .filter(|(p, q)| p == q)
            .count()
    });
    for (old_index, new_index) in pairs {
        source_of[head + new_index] = Some(head + old_index);
    }
    let mut out: Vec<String> = Vec::with_capacity(n + 1);
    for (index, source) in source_of.into_iter().enumerate() {
        let row = new.child(index);
        let line = match source {
            Some(from) if old.child(from) == row => line_of(from).to_owned(),
            Some(from) => {
                let mut line = RowLine::parse(line_of(from))?;
                if line.cells.len() != columns {
                    return None;
                }
                let was = old.child(from);
                for column in 0..columns {
                    if was.child(column) != row.child(column) {
                        line.set(column, cell_text(schema, row.child(column))?, &header);
                    }
                }
                line.to_string()
            }
            None => {
                let mut line = header.emptied();
                for column in 0..columns {
                    line.set(column, cell_text(schema, row.child(column))?, &header);
                }
                line.to_string()
            }
        };
        if line.contains('\n') {
            return None;
        }
        out.push(line);
        if index == 0 {
            out.push(lines[1].to_owned());
        }
    }
    Some(out.join(newline))
}

/// A cell's text as the writer puts it in its row.
fn cell_text(schema: &Schema, cell: &Node) -> Option<String> {
    guarded_text(schema, cell, block_kind(schema, cell.type_id())?)
}

/// Which of `a` items the `b` items are edits of, as index pairs in order: the
/// pairing whose scores add up to the most, an item with no partner being one
/// added or removed. A pair scoring nothing is no pair.
fn pair_in_order(a: usize, b: usize, score: impl Fn(usize, usize) -> usize) -> Vec<(usize, usize)> {
    // best[i][j]: the most a pairing of items i.. with items j.. scores.
    let mut best = vec![vec![0usize; b + 1]; a + 1];
    for i in (0..a).rev() {
        for j in (0..b).rev() {
            best[i][j] = (score(i, j) + best[i + 1][j + 1])
                .max(best[i + 1][j])
                .max(best[i][j + 1]);
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < a && j < b {
        let here = score(i, j);
        if here > 0 && best[i][j] == here + best[i + 1][j + 1] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if best[i][j] == best[i + 1][j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// One table row's line, cut at its pipes: what stands before the first cell,
/// each cell's text with the spaces either side of it, and what follows the
/// last.
#[derive(Clone)]
struct RowLine {
    lead: String,
    cells: Vec<(String, String, String)>,
    trail: String,
}

impl RowLine {
    fn parse(line: &str) -> Option<RowLine> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut escaped = false;
        for c in line.chars() {
            if c == '|' && !escaped {
                parts.push(std::mem::take(&mut current));
            } else {
                current.push(c);
            }
            escaped = c == '\\' && !escaped;
        }
        parts.push(current);
        // Pipes at the edges are optional: with one, what lies outside it is
        // indentation or trailing space rather than a cell.
        let trimmed = line.trim();
        let lead = if trimmed.starts_with('|') {
            Some(parts.remove(0))
        } else {
            None
        };
        let trail = if trimmed.ends_with('|') && trimmed.len() > 1 {
            parts.pop()
        } else {
            None
        };
        if parts.is_empty() {
            return None;
        }
        let cells = parts
            .into_iter()
            .map(|part| {
                let text = part.trim();
                let start = part.find(text).unwrap_or(part.len());
                let (before, rest) = part.split_at(start);
                let (text, after) = rest.split_at(text.len());
                (before.to_owned(), text.to_owned(), after.to_owned())
            })
            .collect();
        Some(RowLine {
            lead: lead.map(|lead| format!("{lead}|")).unwrap_or_default(),
            cells,
            trail: trail.map(|trail| format!("|{trail}")).unwrap_or_default(),
        })
    }

    /// This line's shape with every cell empty: how a new row is spelled.
    fn emptied(&self) -> RowLine {
        RowLine {
            cells: vec![(String::new(), String::new(), String::new()); self.cells.len()],
            ..self.clone()
        }
    }

    /// Put `text` in cell `column`. A cell with text keeps the spaces around
    /// it; an empty one — all space, or new — takes the header's first cell's.
    fn set(&mut self, column: usize, text: String, header: &RowLine) {
        let (before, old, after) = &mut self.cells[column];
        if old.is_empty() {
            let (pad_before, _, pad_after) = &header.cells[0];
            let pad = |side: &str| if side.is_empty() { "" } else { " " };
            *before = pad(pad_before).to_owned();
            *after = pad(pad_after).to_owned();
        }
        // A cell with nothing in it between two pipes still needs room to read
        // as a cell to a person.
        if text.is_empty() && before.is_empty() && after.is_empty() {
            *before = " ".to_owned();
        }
        *old = text;
    }
}

impl std::fmt::Display for RowLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.lead)?;
        for (index, (before, text, after)) in self.cells.iter().enumerate() {
            if index > 0 {
                f.write_str("|")?;
            }
            write!(f, "{before}{text}{after}")?;
        }
        f.write_str(&self.trail)
    }
}

/// The one-based first and last line of each of the document's top-level
/// blocks.
///
/// comrak leaves no node for link reference definitions, and the parser keeps
/// each run of them as a raw block of its own — between blocks, or split off
/// the start of the paragraph they opened. Such a block takes the first lines
/// not yet accounted for that are not blank, as many as its text has; every
/// other block is the next of comrak's.
fn block_lines<'a>(
    schema: &Schema,
    document: &Node,
    root: &'a comrak::nodes::AstNode<'a>,
    source: &str,
) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = source.split('\n').collect();
    let raw = schema.node_id(crate::schema::RAW_BLOCK);
    let mut nodes = root.children();
    let mut next = 1;
    let mut out = Vec::new();
    for child in document.children() {
        let text: String = child.children().filter_map(|leaf| leaf.text()).collect();
        let definitions =
            Some(child.type_id()) == raw && crate::textblock::reads_as_definitions(&text);
        if definitions {
            while lines
                .get(next - 1)
                .is_some_and(|line| line.trim().is_empty())
            {
                next += 1;
            }
            let end = next + text.split('\n').count() - 1;
            out.push((next, end));
            next = end + 1;
            continue;
        }
        let Some(node) = nodes.next() else {
            break;
        };
        let pos = node.data.borrow().sourcepos;
        let start = pos.start.line.max(next);
        // comrak ends an indented code block after the blank lines that follow
        // it, which belong to the gap before the next block: a block respelled
        // over them would lose that gap and run into its neighbour.
        let mut end = pos.end.line;
        while end > start
            && lines
                .get(end - 1)
                .is_some_and(|line| line.trim().is_empty())
        {
            end -= 1;
        }
        out.push((start, end));
        next = end + 1;
    }
    out
}

/// `node` without the spaces and tabs ending each paragraph's and heading's
/// text, which a reader drops.
///
/// While a caret stands at the end of a block they are real content — the
/// next keystroke can make them internal — and the correction only settles
/// them once it leaves, so a save in between writes them and reads back
/// without them. Nothing styled can end in one: a style's span ends with its
/// closing delimiter.
fn without_trailing_spaces(schema: &Schema, node: &Node) -> Node {
    if node.is_text() || node.child_count() == 0 {
        return node.clone();
    }
    let mut children: Vec<_> = node
        .children()
        .map(|child| without_trailing_spaces(schema, child))
        .collect();
    if [
        crate::schema::PARAGRAPH,
        crate::schema::HEADING,
        crate::schema::TABLE_CELL,
    ]
    .iter()
    .any(|name| schema.node_id(name) == Some(node.type_id()))
    {
        while let Some(last) = children.last() {
            let Some(text) = last.text() else { break };
            let trimmed = text.trim_end_matches([' ', '\t']);
            if trimmed.len() == text.len() {
                break;
            }
            if trimmed.is_empty() {
                children.pop();
            } else {
                let last = last.with_text(trimmed);
                *children.last_mut().expect("last text child") = last;
                break;
            }
        }
    }
    node.copy(Fragment::from_nodes(children))
}

/// `node` without the empty paragraphs a reader would not give back.
///
/// Such a paragraph writes as nothing, or as the blank line it stands for, so
/// the file reads back without it. That includes the first block of a list
/// item that goes on: the writer leaves the marker alone on its line and the
/// next block on the line after it, which reads back as the item without the
/// paragraph. The one kept is an empty item's only block, which the item
/// cannot do without.
pub fn without_empty_paragraphs(schema: &Schema, node: &Node) -> Node {
    if node.is_text() || node.child_count() == 0 {
        return node.clone();
    }
    let paragraph = schema.node_id(crate::schema::PARAGRAPH);
    let item = [crate::schema::LIST_ITEM, crate::schema::TASK_ITEM]
        .iter()
        .any(|name| schema.node_id(name) == Some(node.type_id()));
    let children: Vec<_> = node
        .children()
        .enumerate()
        .filter(|(index, child)| {
            Some(child.type_id()) != paragraph
                || child.child_count() > 0
                || (item && *index == 0 && node.child_count() == 1)
        })
        .map(|(_, child)| without_empty_paragraphs(schema, child))
        .collect();
    node.copy(Fragment::from_nodes(children))
}

/// `body` read as a document, with the bytes each of its top-level blocks
/// takes, counted from `offset`, and whether every block found its bytes.
fn read_blocks(
    schema: &Schema,
    body: &str,
    offset: usize,
) -> Result<(Node, Vec<Range<usize>>, bool), ParseError> {
    let document = from_markdown(schema, body)?;
    let arena = Arena::new();
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let root = crate::parse::parse_ast(&arena, &normalized, &commonmark_options());
    let lines = line_ranges(body);
    let blocks = block_lines(schema, &document, root, &normalized)
        .into_iter()
        .filter_map(|(start, end)| {
            let first = lines.get(start.checked_sub(1)?)?;
            let last = lines.get(end.checked_sub(1)?)?;
            Some(offset + first.start..offset + last.end)
        })
        .collect::<Vec<_>>();
    let mapped = blocks.len() == document.child_count()
        && blocks.windows(2).all(|pair| pair[0].end <= pair[1].start);
    Ok((document, blocks, mapped))
}

fn block_markdown(schema: &Schema, document: &Node, nodes: &[Node]) -> String {
    if nodes.is_empty() {
        String::new()
    } else {
        to_markdown(
            schema,
            &document.copy(Fragment::from_nodes(nodes.iter().cloned())),
        )
    }
}

fn body_start(source: &str) -> usize {
    let bom = if source.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let body = &source[bom..];
    let lines = line_ranges(body);
    if lines
        .first()
        .is_none_or(|range| &body[range.clone()] != "---")
    {
        return bom;
    }
    for (index, range) in lines.iter().enumerate().skip(1) {
        if matches!(&body[range.clone()], "---" | "...") {
            return bom + lines.get(index + 1).map_or(body.len(), |next| next.start);
        }
    }
    // A missing closing delimiter is ordinary Markdown, not inferred YAML.
    bom
}

fn line_ranges(source: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let bytes = source.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if matches!(bytes[offset], b'\r' | b'\n') {
            lines.push(start..offset);
            if bytes[offset] == b'\r' && bytes.get(offset + 1) == Some(&b'\n') {
                offset += 1;
            }
            start = offset + 1;
        }
        offset += 1;
    }
    if start < source.len() {
        lines.push(start..source.len());
    }
    lines
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::SourceDocument;
    use crate::commonmark_schema;

    /// An edit inside one block, or between two, is read again only near
    /// itself; one that reaches the front matter or a definition is not.
    #[test]
    fn a_baseline_moves_on_by_reading_near_its_edit() {
        let schema = commonmark_schema();
        let filler: String = (0..30).map(|i| format!("filler {i}\n\n")).collect();
        let original = format!("---\na: b\n---\n{filler}# end\n");
        let baseline = SourceDocument::parse(&schema, &original).unwrap();
        for (edited, near) in [
            (original.replace("filler 12", "filler twelve"), true),
            (original.replace("filler 0\n", "filler 0 first\n"), true),
            (original.replace("# end", "# the end"), true),
            (
                original.replace("filler 5\n\n", "filler 5\n\nnew\n\n"),
                true,
            ),
            (original.replace("filler 5\n\n", ""), true),
            (
                original.replace("filler 5\n\n", "filler 5\n\n[r]: /x\n\n"),
                false,
            ),
            (original.replace("a: b", "a: c"), false),
        ] {
            let next = baseline.advanced_near(&schema, &edited);
            assert_eq!(next.is_some(), near, "{edited:?}");
            if let Some(next) = next {
                let whole = SourceDocument::parse(&schema, &edited).unwrap();
                assert_eq!(next.document, whole.document, "{edited:?}");
                assert_eq!(next.blocks, whole.blocks, "{edited:?}");
            }
        }
    }
    #[test]
    fn transaction_window_locates_only_the_edited_block_in_a_large_file() {
        use markraft_core::{EditorState, EditorStateConfig, Selection, commands};
        let schema = commonmark_schema();
        let text: String = (0..1_000)
            .map(|index| format!("paragraph {index}\n\n"))
            .collect();
        let source = SourceDocument::parse(&schema, &text).unwrap();
        let block = 500;
        let position = source
            .document
            .children()
            .take(block)
            .map(|node| node.node_size())
            .sum::<usize>()
            + 3;
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(source.document.clone())
                .selection(Selection::cursor(position)),
        )
        .unwrap();
        let transaction = state
            .update([commands::insert_text("中")(&state).unwrap()])
            .unwrap();
        let window = source
            .transaction_window(state.doc(), transaction.new_doc(), transaction.changes())
            .unwrap();
        assert_eq!(window.old, block..block + 1);
        assert_eq!(window.new, block..block + 1);
        let patched = source
            .render_changed(&schema, transaction.new_doc(), Some(window))
            .unwrap();
        let bytes = source.blocks[block].clone();
        let next = source.advanced_window(&schema, &patched, bytes).unwrap();
        assert_eq!(
            next.source,
            source.render(&schema, transaction.new_doc()).unwrap()
        );
        assert_eq!(
            next.read,
            SourceDocument::parse(&schema, &patched).unwrap().read
        );
    }

    #[test]
    fn a_frozen_snapshot_keeps_exact_bytes_without_advancing_the_live_track() {
        use markraft_core::{EditorState, EditorStateConfig, Selection, commands};
        let schema = commonmark_schema();
        let text = "---\r\ntitle: exact\r\n---\r\n\r\noriginal\r\n";
        let source = SourceDocument::parse(&schema, text).unwrap();
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(source.document.clone())
                .selection(Selection::cursor(2)),
        )
        .unwrap();
        let track = super::SourceTrack::new(source);
        let frozen = track.snapshot();
        let change = state
            .update([commands::insert_text("a")(&state).unwrap()])
            .unwrap();
        // Even rendering a future document only mutates the returned string.
        assert!(
            frozen
                .render(&schema, change.new_doc())
                .unwrap()
                .contains("oariginal")
        );
        assert!(track.current.lock().unwrap().is_none());
        track
            .apply_transactions(&schema, std::slice::from_ref(&change))
            .unwrap();
        assert_eq!(frozen.render(&schema, state.doc()).unwrap(), text);
        assert!(
            track
                .current
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .document
                .ptr_eq(change.new_doc())
        );
        assert_eq!(
            track.snapshot().render(&schema, change.new_doc()).unwrap(),
            frozen.render(&schema, change.new_doc()).unwrap()
        );
    }

    #[test]
    fn a_disconnected_transaction_chain_does_not_advance_source() {
        use markraft_core::{EditorState, EditorStateConfig, Selection, commands};
        let schema = commonmark_schema();
        let source = SourceDocument::parse(&schema, "original").unwrap();
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(source.document.clone())
                .selection(Selection::cursor(2)),
        )
        .unwrap();
        let first = state
            .update([commands::insert_text("a")(&state).unwrap()])
            .unwrap();
        let second = state
            .update([commands::insert_text("b")(&state).unwrap()])
            .unwrap();
        let track = super::SourceTrack::new(source);
        assert!(
            track
                .apply_transactions(&schema, &[first.clone(), second])
                .is_err()
        );
        assert!(track.current.lock().unwrap().is_none());
        track
            .apply_transactions(&schema, std::slice::from_ref(&first))
            .unwrap();
        assert_eq!(track.save(&schema, first.new_doc()).unwrap(), "oariginal");
        assert!(
            track
                .current
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .document
                .ptr_eq(first.new_doc())
        );
        let undo = first
            .state()
            .update([markraft_core::TransactionSpec::new()
                .change_set(first.changes().invert(state.doc()).unwrap())])
            .unwrap();
        track
            .apply_transactions(&schema, std::slice::from_ref(&undo))
            .unwrap();
        assert_eq!(track.save(&schema, undo.new_doc()).unwrap(), "original");
        assert!(track.current.lock().unwrap().is_none());
    }
}
