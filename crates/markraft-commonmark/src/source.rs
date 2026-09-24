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
                // A task box ticked or unticked: its one character changes,
                // however the rest of the item is spelled — `[X]`, a `+`
                // marker, extra spaces — none of which the writer keeps.
                if let Some((nth, checked)) = toggled_box(schema, &old[index], &new[index])
                    && let Ok(patched) = self.flip_box(
                        schema,
                        &target,
                        result.clone(),
                        self.blocks[index].clone(),
                        nth,
                        checked,
                    )
                {
                    result = patched;
                    continue;
                }
                let before = block_markdown(schema, &self.document, &old[index..=index]);
                let after = block_markdown(schema, document, &new[index..=index]);
                let range = self.blocks[index].clone();
                // A table whose rows were added, removed or edited, its columns
                // as they were: each row it kept keeps its line, an edited row
                // keeps its own spacing around the text that changed, and a new
                // row is spelled the way the table's header is. See
                // `table_rows`.
                if let Some(patched) = table_rows(
                    schema,
                    &old[index],
                    &new[index],
                    &result[range.clone()],
                    self.newline,
                )
                .and_then(|rows| {
                    let mut candidate = result.clone();
                    candidate.replace_range(range.clone(), &rows);
                    self.validate(schema, &target, candidate).ok()
                }) {
                    result = patched;
                    continue;
                }
                // A table's body row added goes in as a line of its own. A diff of
                // the two tables' canonical spellings could place it anywhere their
                // padding happens to agree, inside a hand-written row, and the
                // table would still read the same.
                if let Some(patched) = added_row(schema, &old[index], &new[index]).and_then(|row| {
                    let line = after.split('\n').nth(row + 1)?;
                    let raw = &result[range.clone()];
                    let at = match row_line(raw, self.newline, row + 1) {
                        Some(existing) => existing.start + self.newline.len(),
                        None => raw.len(),
                    };
                    let text = if at == raw.len() {
                        format!("{}{line}", self.newline)
                    } else {
                        format!("{line}{}", self.newline)
                    };
                    let mut candidate = result.clone();
                    candidate.insert_str(range.start + at, &text);
                    self.validate(schema, &target, candidate).ok()
                }) {
                    result = patched;
                    continue;
                }
                result = match self.patch(
                    schema,
                    &target,
                    result.clone(),
                    range.clone(),
                    &before,
                    &after,
                ) {
                    Ok(patched) => patched,
                    // The block's hand-written spelling cannot take the
                    // change in place — a heading's level, a list's type, a
                    // table's columns, or text its spelling cannot hold — so
                    // it is respelled; see `respell`. A table's body row
                    // deleted takes just its line with it rather than the
                    // whole table.
                    Err(_) => {
                        let dropped = removed_row(schema, &old[index], &new[index])
                            .and_then(|row| row_line(&result[range.clone()], self.newline, row + 1))
                            .and_then(|line| {
                                let mut candidate = result.clone();
                                candidate.replace_range(
                                    range.start + line.start..range.start + line.end,
                                    "",
                                );
                                self.validate(schema, &target, candidate).ok()
                            });
                        match dropped {
                            Some(patched) => patched,
                            None => self.respell(schema, &target, result, range, &after)?,
                        }
                    }
                };
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
                // Whole blocks went and nothing took their place — a divider
                // deleted: their lines go with the gap before them. Patching
                // them out would leave both gaps, a blank line too many, where
                // they stood.
                if prefix + suffix == new.len() {
                    let mut candidate = result.clone();
                    candidate.replace_range(self.dropped_range(prefix, end, suffix), "");
                    if let Ok(done) = self.validate(schema, document, candidate) {
                        return Ok(done);
                    }
                }
                // A table that took in the text of the blocks after it —
                // Backspace after a table joins its last cell — keeps its
                // rows' spelling, and the blocks go with the gap before them.
                if new.len() - suffix == prefix + 1
                    && let Some(rows) = table_rows(
                        schema,
                        &old[prefix],
                        &new[prefix],
                        &result[self.blocks[prefix].clone()],
                        self.newline,
                    )
                {
                    let mut candidate = result.clone();
                    candidate.replace_range(self.blocks[prefix].end..self.blocks[end - 1].end, "");
                    candidate.replace_range(self.blocks[prefix].clone(), &rows);
                    if let Ok(done) = self.validate(schema, document, candidate) {
                        return Ok(done);
                    }
                }
                let range = self.blocks[prefix].start..self.blocks[end - 1].end;
                result = match self.patch(schema, document, result.clone(), range, &before, &after)
                {
                    Ok(patched) => patched,
                    // Whole blocks went and nothing took their place, but their
                    // source is not spelled the way the writer would spell them —
                    // a table padded otherwise. Their spelling does not matter to
                    // a deletion: drop their lines and the gap before them.
                    Err(_) if prefix + suffix == new.len() => {
                        result.replace_range(self.dropped_range(prefix, end, suffix), "");
                        result
                    }
                    // Blocks were joined, split, wrapped or lifted out: the
                    // structure changed, so the blocks it touched are
                    // respelled whole.
                    Err(_) => {
                        let range = self.blocks[prefix].start..self.blocks[end - 1].end;
                        return self.respell(schema, document, result, range, &after);
                    }
                };
            }
        }
        self.validate(schema, document, result)
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

    /// Replace `range` — whole top-level blocks — with `after`, their Markdown
    /// as the writer spells it.
    ///
    /// This is the last resort, once no patch keeps the blocks' spelling: an
    /// edit to a block's structure — a heading made a paragraph, a list
    /// converted or lifted, blocks joined, a table's columns changed — or text
    /// the spelling cannot hold, such as a setext heading whose text now opens
    /// with `~~~`, which would read as a code fence. The blocks the edit
    /// touched are written the way the writer writes them, as Typora does,
    /// rather than the edit being refused: a setext heading becomes an ATX
    /// one, a list's markers are re-spaced. Every other block keeps its bytes,
    /// and the whole document still has to read back as `target`. Typing in a
    /// block never gets this far while a patch can place it, so a hand-written
    /// block keeps its spelling for as long as it can.
    fn respell(
        &self,
        schema: &Schema,
        target: &Node,
        mut source: String,
        range: Range<usize>,
        after: &str,
    ) -> Result<String, SourceError> {
        source.replace_range(range, &self.with_newlines(after));
        self.validate(schema, target, source)
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

    /// Tick (`checked`) or untick the `nth` task box of the block in `range`,
    /// changing only the character inside it.
    ///
    /// A box is a `[ ]`, `[x]` or `[X]` that follows nothing but list markers
    /// and quote prefixes on its line. The `nth` of those is tried first — the
    /// source order of the boxes is the order of the task items — and the
    /// others after it, for a line that only looks like an item, such as one
    /// inside an indented code block. Whichever is taken, the whole document
    /// has to parse back to `target`.
    fn flip_box(
        &self,
        schema: &Schema,
        target: &Node,
        source: String,
        range: Range<usize>,
        nth: usize,
        checked: bool,
    ) -> Result<String, SourceError> {
        let raw = &source[range.clone()];
        let mut boxes: Vec<usize> = raw
            .match_indices('[')
            .map(|(offset, _)| offset + 1)
            .filter(|&inner| {
                matches!(raw.as_bytes().get(inner), Some(b' ' | b'x' | b'X'))
                    && raw.as_bytes().get(inner + 1) == Some(&b']')
                    && after_item_markers(
                        &raw[raw[..inner - 1].rfind('\n').map_or(0, |at| at + 1)..inner - 1],
                    )
            })
            .collect();
        if nth < boxes.len() {
            let first = boxes.remove(nth);
            boxes.insert(0, first);
        }
        boxes.truncate(64);
        let mark = if checked { "x" } else { " " };
        for inner in boxes {
            if (raw.as_bytes()[inner] != b' ') == checked {
                continue;
            }
            let at = range.start + inner;
            let mut candidate = source.clone();
            candidate.replace_range(at..at + 1, mark);
            if let Ok(valid) = self.validate(schema, target, candidate) {
                return Ok(valid);
            }
        }
        Err(SourceError::UnsupportedEdit)
    }
}

/// Whether `prefix`, the start of a line up to a `[`, is nothing but quote
/// prefixes and list markers, each marker followed by the space or tab that
/// makes it one — at least one marker, as a task box needs an item.
fn after_item_markers(prefix: &str) -> bool {
    let mut rest = prefix;
    let mut marker = false;
    loop {
        rest = rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() {
            return marker && prefix.ends_with([' ', '\t']);
        }
        if let Some(after) = rest.strip_prefix('>') {
            rest = after;
            marker = false;
            continue;
        }
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let length = match rest.as_bytes()[0] {
            b'-' | b'+' | b'*' => 1,
            _ if (1..=9).contains(&digits)
                && matches!(rest.as_bytes().get(digits), Some(b'.' | b')')) =>
            {
                digits + 1
            }
            _ => return false,
        };
        if !rest[length..].starts_with([' ', '\t']) {
            return false;
        }
        rest = &rest[length..];
        marker = true;
    }
}

/// The source-order index among `old`'s task items of the one whose box `new`
/// ticks or unticks, and whether it ends up ticked, when the two differ in
/// nothing else.
fn toggled_box(schema: &Schema, old: &Node, new: &Node) -> Option<(usize, bool)> {
    fn walk(
        task: markraft_core::NodeTypeId,
        old: &Node,
        new: &Node,
        seen: &mut usize,
        found: &mut Option<(usize, bool)>,
    ) -> bool {
        if old.type_id() != new.type_id()
            || old.marks() != new.marks()
            || old.text() != new.text()
            || old.child_count() != new.child_count()
        {
            return false;
        }
        if old.type_id() == task {
            if old.attrs() != new.attrs() {
                let Some(checked) = new
                    .attrs()
                    .get(markraft_core::kind::TASK_CHECKED_ATTR)
                    .and_then(|value| value.as_bool())
                else {
                    return false;
                };
                let only_the_box = old
                    .attrs()
                    .with(markraft_core::kind::TASK_CHECKED_ATTR, checked)
                    == *new.attrs();
                if !only_the_box || found.replace((*seen, checked)).is_some() {
                    return false;
                }
            }
            *seen += 1;
        } else if old.attrs() != new.attrs() {
            return false;
        }
        old.children()
            .zip(new.children())
            .all(|(a, b)| walk(task, a, b, seen, found))
    }
    let task = schema.node_id(crate::schema::TASK_ITEM)?;
    let mut found = None;
    walk(task, old, new, &mut 0, &mut found).then_some(found)?
}

/// The index of the one body row the table `new` lacks, when it is the table
/// `old` without it.
fn removed_row(schema: &Schema, old: &Node, new: &Node) -> Option<usize> {
    if schema.node_id(crate::schema::TABLE) != Some(old.type_id())
        || !old.same_markup(new)
        || old.child_count() != new.child_count() + 1
    {
        return None;
    }
    let row = old
        .children()
        .zip(new.children())
        .position(|(a, b)| a != b)
        .unwrap_or(new.child_count());
    let rest_equal = old.children().skip(row + 1).eq(new.children().skip(row));
    (row > 0 && rest_equal).then_some(row)
}

/// The index of the one body row the table `new` has that `old` lacks, when it
/// is the table `old` with it.
fn added_row(schema: &Schema, old: &Node, new: &Node) -> Option<usize> {
    removed_row(schema, new, old)
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
    for (old_index, new_index) in pair_rows(old, new, head..o - tail, head..n - tail) {
        source_of[new_index] = Some(old_index);
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
                        line.set(column, guarded_source(schema, row.child(column))?, &header);
                    }
                }
                line.to_string()
            }
            None => {
                let mut line = header.emptied();
                for column in 0..columns {
                    line.set(column, guarded_source(schema, row.child(column))?, &header);
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

/// Which rows of `old` in `olds` the rows of `new` in `news` are edits of, as
/// pairs in order: the pairing that keeps the most cells as they were, a row
/// with no partner being one added or removed. Two rows paired count for one
/// more than their equal cells, so an edited row pairs rather than reading as
/// one row dropped and another added.
fn pair_rows(
    old: &Node,
    new: &Node,
    olds: Range<usize>,
    news: Range<usize>,
) -> Vec<(usize, usize)> {
    let (a, b) = (olds.len(), news.len());
    let score = |i: usize, j: usize| {
        let (x, y) = (old.child(olds.start + i), new.child(news.start + j));
        1 + x
            .children()
            .zip(y.children())
            .filter(|(p, q)| p == q)
            .count()
    };
    // best[i][j]: the most a pairing of old rows i.. with new rows j.. keeps.
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
        if best[i][j] == score(i, j) + best[i + 1][j + 1] {
            pairs.push((olds.start + i, news.start + j));
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

/// The byte range of `raw`'s line `index` with the line break before it — a
/// table's rows after the header are its lines from the third on.
fn row_line(raw: &str, newline: &str, index: usize) -> Option<Range<usize>> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(raw.match_indices(newline).map(|(at, _)| at + newline.len()))
        .collect();
    let start = *starts.get(index)?;
    let end = starts
        .get(index + 1)
        .map_or(raw.len(), |next| next - newline.len());
    Some(start.checked_sub(newline.len())?..end)
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
/// backslashes and each atom's own spelling — `<br/>` for the raw HTML a
/// paste puts in a table cell — or `None` when it holds an atom the writer
/// has no spelling for.
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
