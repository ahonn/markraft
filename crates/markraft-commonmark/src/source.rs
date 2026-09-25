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
//! the writer spells it, as Typora does, and only that block. Every candidate
//! has to read back as the block it stands for, and the whole file as the
//! edited document; unmapped source is never silently replaced.

use std::ops::Range;

use comrak::Arena;
use markraft_core::kind::{HEADING_LEVEL_ATTR, TASK_CHECKED_ATTR};
use markraft_core::{Fragment, Node, Schema};

use crate::derive::BlockKind;
use crate::textblock::{Item, Items, block_kind};
use crate::{ParseError, commonmark_options, from_markdown, to_markdown};

/// A parsed document and its immutable, byte-preserving source baseline.
#[derive(Clone, Debug)]
pub struct SourceDocument {
    source: String,
    document: Node,
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
                Some(body_start + first.start..body_start + last.end)
            })
            .collect::<Vec<_>>();
        let mapped = blocks.len() == document.child_count()
            && blocks.windows(2).all(|pair| pair[0].end <= pair[1].start);
        let newline = if body.contains("\r\n") {
            "\r\n"
        } else if body.contains('\r') {
            "\r"
        } else {
            "\n"
        };
        Ok(Self {
            source: source.to_owned(),
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
    pub fn render(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        let rendered = self.render_patched(schema, document)?;
        Ok(self.renumbered(schema, document, rendered))
    }

    /// `rendered` with the ordinals of each ordered list the edit touched
    /// counted again, as Typora writes them: a patch that adds an item leaves
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
        self.validate(schema, document, lines.join("\n"))
            .unwrap_or(rendered)
    }

    fn render_patched(&self, schema: &Schema, document: &Node) -> Result<String, SourceError> {
        if document == &self.document {
            return Ok(self.source.clone());
        }
        let expected = to_markdown(schema, document);
        if expected == to_markdown(schema, &self.document) {
            return Ok(self.source.clone());
        }
        // An empty Markdown file has an implicit editor paragraph but no AST
        // block. There is no existing syntax to disturb in this special case.
        if self.blocks.is_empty() {
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
        let old: Vec<_> = self.document.children().cloned().collect();
        let new: Vec<_> = document.children().cloned().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let (end, new_end) = (old.len() - suffix, new.len() - suffix);
        let after = block_markdown(schema, document, &new[prefix..new_end]);
        if prefix == end {
            return self.insert_blocks(
                schema,
                document,
                self.source.clone(),
                prefix,
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
            return self.validate(schema, document, result);
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
        if let Ok(done) = self.validate(schema, document, result) {
            return Ok(done);
        }
        // The blocks the edit touched, written together the way the writer
        // writes them: what the file reads back as when their own spellings
        // cannot hold the edit — two lists that would run together, say.
        let mut result = self.source.clone();
        result.replace_range(region, &self.with_newlines(&after));
        self.validate(schema, document, result)
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
        prefix: usize,
        suffix: usize,
        after: &str,
    ) -> Result<String, SourceError> {
        if after.is_empty() {
            return self.validate(schema, document, result);
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
            return self.validate(schema, document, result);
        };
        let mut candidate = result.clone();
        candidate.insert_str(before.end, &format!("{separator}{blocks}"));
        let placed = self.validate(schema, document, candidate);
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
        self.validate(schema, document, result)
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
        if parsed == *target || to_markdown(schema, &parsed) == to_markdown(schema, target) {
            return Ok(source);
        }
        // CommonMark discards spaces at a paragraph/heading's end. During
        // typing those spaces are real editor content (the next keystroke can
        // make them internal). Keep their bytes in the candidate, but compare
        // the target using only this specific parser normalization. Code and
        // unsupported syntax still require the original strict comparison.
        let trimmed = without_trailing_spaces(schema, target);
        if trimmed != *target && to_markdown(schema, &parsed) == to_markdown(schema, &trimmed) {
            return Ok(source);
        }
        // An empty paragraph is typing in progress — Return at the start of a
        // block, with nothing yet on the new line. The file holds nothing for
        // it, so a document that differs from its reading only by empty
        // paragraphs is the same note. Only the ones whose going changes
        // nothing else are let go; see `without_empty_paragraphs`.
        let bare = without_empty_paragraphs(schema, &trimmed);
        let read = without_empty_paragraphs(schema, &parsed);
        if bare != trimmed
            && (read == bare || to_markdown(schema, &read) == to_markdown(schema, &bare))
        {
            Ok(source)
        } else {
            Err(SourceError::UnsupportedEdit)
        }
    }
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
                return Some(None);
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
            let kept =
                after_paired && paired.is_some_and(|(was, _)| was.opens_item == leaf.opens_item);
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
                let (old_lines, new_lines) = (&was.lines, &leaf.lines);
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
                let texts: &[String] = if marker_only {
                    &[String::new()]
                } else {
                    new_lines
                };
                for (k, text) in texts.iter().enumerate() {
                    let from_old = if marker_only {
                        None
                    } else if k < head {
                        Some(k)
                    } else if k >= new_lines.len() - tail {
                        Some(k + old_lines.len() - new_lines.len())
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
