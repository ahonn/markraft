//! Conservative, source-preserving writes for documents shared with other editors.
//!
//! A textblock's text is its inline source, so the serializer already writes
//! every inline spelling as it was read. What it does not keep is the block
//! level: container prefixes and indentation, setext underlines, table
//! padding, blank-line runs, line endings, front matter. This codec retains the
//! original source, maps each top-level block to the lines it came from, and
//! patches only the blocks an edit changed — accepting a patch only when
//! reparsing the result produces the requested document. Unmapped source is
//! never silently replaced by canonical Markdown.

use std::ops::Range;

use comrak::Arena;
use markraft_core::{Fragment, Node, Schema};

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
    /// Rewritten source did not reparse into the edited document, or the block
    /// the change needs rewritten whole is not spelled the way the codec would
    /// write it. Retain the editor buffer and offer a separate export instead
    /// of overwriting.
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
        let mut result = self.source.clone();
        if old.len() == new.len() {
            let mut partial = old.clone();
            for index in (0..old.len()).rev() {
                if old[index] == new[index] {
                    continue;
                }
                partial[index] = new[index].clone();
                let target = document.copy(Fragment::from_nodes(partial.clone()));
                // Prefer semantic text deltas: a longer table cell changes the
                // canonical table's padding, but existing column whitespace is
                // unrelated to the user's text edit and must remain untouched.
                let mut text = Vec::new();
                if text_changes(&old[index], &new[index], &mut text)
                    && let [(before, after)] = text.as_slice()
                    && let Ok(patched) = self.patch(
                        schema,
                        &target,
                        result.clone(),
                        self.blocks[index].clone(),
                        before,
                        after,
                    )
                {
                    result = patched;
                    continue;
                }
                // The same, spelled as the writer spells the one textblock that
                // changed: a character its guard escapes — a `|` in a table
                // cell — goes in with its backslash.
                if let Some((before, after)) = changed_textblock(schema, &old[index], &new[index])
                    .and_then(|(before, after)| {
                        Some((
                            guarded_source(schema, before)?,
                            guarded_source(schema, after)?,
                        ))
                    })
                    && let Ok(patched) = self.patch(
                        schema,
                        &target,
                        result.clone(),
                        self.blocks[index].clone(),
                        &before,
                        &after,
                    )
                {
                    result = patched;
                    continue;
                }
                let before = block_markdown(schema, &self.document, &old[index..=index]);
                let after = block_markdown(schema, document, &new[index..=index]);
                result = self.patch(
                    schema,
                    &target,
                    result,
                    self.blocks[index].clone(),
                    &before,
                    &after,
                )?;
            }
        } else {
            let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let end = old.len() - suffix;
            let before = block_markdown(schema, &self.document, &old[prefix..end]);
            let after = block_markdown(schema, document, &new[prefix..new.len() - suffix]);
            if prefix == end {
                return self.insert_blocks(schema, document, result, prefix, suffix, &after);
            } else {
                let range = self.blocks[prefix].start..self.blocks[end - 1].end;
                result = self.patch(schema, document, result, range, &before, &after)?;
            }
        }
        self.validate(schema, document, result)
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
            Ok(source)
        } else {
            Err(SourceError::UnsupportedEdit)
        }
    }

    fn patch(
        &self,
        schema: &Schema,
        target: &Node,
        source: String,
        range: Range<usize>,
        before: &str,
        after: &str,
    ) -> Result<String, SourceError> {
        let (removed, inserted, prefix, suffix) = difference(before, after);
        let raw = &source[range.clone()];
        let removed = self.with_newlines(removed);
        let inserted = self.with_newlines(inserted);
        let prefix = self.with_newlines(prefix);
        let suffix = self.with_newlines(suffix);
        // Source and canonical Markdown can differ around the edit — a setext
        // underline, a continuation line's prefix, a table's padding. Prefer
        // matching local context, then prove the chosen location by parsing
        // the entire resulting document.
        for offset in candidate_offsets(raw, &removed, &prefix, &suffix) {
            let changed = offset..offset + removed.len();
            let mut candidate = source.clone();
            candidate.replace_range(
                range.start + changed.start..range.start + changed.end,
                &inserted,
            );
            if let Ok(valid) = self.validate(schema, target, candidate) {
                return Ok(valid);
            }
        }
        // A save can cover several keystrokes separated by an untouched link
        // or opaque token. Diff those independently rather than serializing
        // everything between the first and last change.
        if let Some(hunks) = token_hunks(before, after)
            && hunks.len() > 1
        {
            let mut patches = Vec::new();
            for (old, new) in hunks {
                let (removed, inserted, left, right) =
                    difference(&before[old.clone()], &after[new]);
                let removed = self.with_newlines(removed);
                let prefix = self.with_newlines(&format!("{}{left}", &before[..old.start]));
                let suffix = self.with_newlines(&format!("{right}{}", &before[old.end..]));
                let found = candidate_offsets(raw, &removed, &prefix, &suffix)
                    .into_iter()
                    .next()
                    .map(|offset| offset..offset + removed.len());
                let Some(changed) = found else {
                    patches.clear();
                    break;
                };
                patches.push((changed, self.with_newlines(inserted)));
            }
            patches.sort_by_key(|(span, _)| span.start);
            if !patches.is_empty()
                && patches.windows(2).all(|pair| {
                    pair[0].0.end <= pair[1].0.start && pair[0].0.start != pair[1].0.start
                })
            {
                let mut candidate = source.clone();
                for (changed, inserted) in patches.into_iter().rev() {
                    candidate.replace_range(
                        range.start + changed.start..range.start + changed.end,
                        &inserted,
                    );
                }
                if let Ok(valid) = self.validate(schema, target, candidate) {
                    return Ok(valid);
                }
            }
        }
        // A structural operation may require replacing its containing block.
        // Only canonical original source is eligible: block-level trivia the
        // writer would respell makes this fallback unsafe rather than
        // expendable. Inline source needs no such care — a textblock's text is
        // its source, so the writer puts every inline spelling back as it was —
        // and a callout's marker line lives in its quote's attributes, which
        // the writer spells again exactly.
        if raw == self.with_newlines(before) {
            let mut candidate = source;
            candidate.replace_range(range, &self.with_newlines(after));
            return self.validate(schema, target, candidate);
        }
        Err(SourceError::UnsupportedEdit)
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
        out.push((pos.start.line.max(next), pos.end.line));
        next = pos.end.line + 1;
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

fn text_changes<'a>(old: &'a Node, new: &'a Node, changes: &mut Vec<(&'a str, &'a str)>) -> bool {
    if !old.same_markup(new) || old.child_count() != new.child_count() {
        return false;
    }
    match (old.text(), new.text()) {
        (Some(before), Some(after)) => {
            if before != after {
                changes.push((before, after));
            }
            true
        }
        (None, None) => old
            .children()
            .zip(new.children())
            .all(|(a, b)| text_changes(a, b, changes)),
        _ => false,
    }
}

/// The one textblock `old` and `new` differ in, when they differ in nothing
/// else.
fn changed_textblock<'a>(
    schema: &Schema,
    old: &'a Node,
    new: &'a Node,
) -> Option<(&'a Node, &'a Node)> {
    if old == new || !old.same_markup(new) {
        return None;
    }
    if block_kind(schema, old.type_id()).is_some() {
        return Some((old, new));
    }
    if old.child_count() != new.child_count() {
        return None;
    }
    let mut changed = old.children().zip(new.children()).filter(|(a, b)| a != b);
    let (a, b) = changed.next()?;
    if changed.next().is_some() {
        return None;
    }
    changed_textblock(schema, a, b)
}

/// A textblock's text as the writer puts it in the file, with its guard's
/// backslashes, or `None` when it holds an atom, which has a spelling of its
/// own.
fn guarded_source(schema: &Schema, block: &Node) -> Option<String> {
    let kind = block_kind(schema, block.type_id())?;
    let items = Items::from_nodes(schema, block.children());
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
            Item::Atom(_) => return None,
        }
    }
    Some(out)
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

fn difference<'a>(before: &'a str, after: &'a str) -> (&'a str, &'a str, &'a str, &'a str) {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    (
        &before[prefix..before.len() - suffix],
        &after[prefix..after.len() - suffix],
        &before[..prefix],
        &before[before.len() - suffix..],
    )
}

fn candidate_offsets(raw: &str, removed: &str, prefix: &str, suffix: &str) -> Vec<usize> {
    let mut offsets: Vec<_> = if removed.is_empty() {
        raw.char_indices()
            .map(|(offset, _)| offset)
            .chain([raw.len()])
            .collect()
    } else {
        raw.match_indices(removed)
            .map(|(offset, _)| offset)
            .collect()
    };
    offsets.sort_by_key(|&offset| {
        std::cmp::Reverse(context_score(
            &raw[..offset],
            prefix,
            &raw[offset + removed.len()..],
            suffix,
        ))
    });
    offsets.truncate(64);
    offsets
}

/// Bounded word/punctuation diff, used only when a single contiguous patch fails.
/// Large divergent blocks are rejected instead of allocating quadratic memory.
fn token_hunks(before: &str, after: &str) -> Option<Vec<(Range<usize>, Range<usize>)>> {
    fn tokens(source: &str) -> Vec<Range<usize>> {
        let mut result = Vec::new();
        let mut start = 0;
        let mut previous_word = false;
        for (offset, ch) in source.char_indices() {
            let word = ch.is_alphanumeric();
            if offset > start && !(word && previous_word) {
                result.push(start..offset);
                start = offset;
            }
            previous_word = word;
        }
        if start < source.len() {
            result.push(start..source.len());
        }
        result
    }
    let old = tokens(before);
    let new = tokens(after);
    let width = new.len() + 1;
    let cells = (old.len() + 1).checked_mul(width)?;
    if cells > 250_000 {
        return None;
    }
    let mut lengths = vec![0_u32; cells];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lengths[i * width + j] = if before[old[i].clone()] == after[new[j].clone()] {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let (mut old_start, mut new_start) = (0, 0);
    let mut hunks = Vec::new();
    while i < old.len() && j < new.len() {
        if before[old[i].clone()] == after[new[j].clone()] {
            if old_start < old[i].start || new_start < new[j].start {
                hunks.push((old_start..old[i].start, new_start..new[j].start));
            }
            old_start = old[i].end;
            new_start = new[j].end;
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if old_start < before.len() || new_start < after.len() {
        hunks.push((old_start..before.len(), new_start..after.len()));
    }
    Some(hunks)
}

fn context_score(left: &str, prefix: &str, right: &str, suffix: &str) -> usize {
    usize::from(left == prefix) * 256
        + usize::from(right == suffix) * 256
        + left
            .chars()
            .rev()
            .zip(prefix.chars().rev())
            .take(64)
            .take_while(|(a, b)| a == b)
            .count()
        + right
            .chars()
            .zip(suffix.chars())
            .take(64)
            .take_while(|(a, b)| a == b)
            .count()
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
