//! Conservative, source-preserving writes for documents shared with other editors.
//!
//! The canonical serializer remains useful for newly authored fragments. This
//! codec retains the original source and only accepts a patch when reparsing it
//! produces the requested document. Unmapped or opaque source is never silently
//! replaced by canonical Markdown.

use std::ops::Range;

use comrak::{Arena, nodes::NodeValue, parse_document};
use markraft_core::{Fragment, Node, Schema};

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

/// A source-preserving save could not safely represent an editor operation. The
/// variants say which part of the source stood in the way, so a host can tell the
/// user what to do about it rather than only that something failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// The change lands inside source kept verbatim: math, a block anchor, a
    /// link reference definition, a callout's marker line or a `[[…]]`
    /// spelling this codec does not read as a wiki link.
    ProtectedSpan,
    /// Writing the change needs its whole block replaced, and that block carries
    /// source the semantic document does not, such as a reference definition.
    ProtectedBlock,
    /// Rewritten source did not reparse into the edited document. Retain the editor
    /// buffer and offer a separate export instead of overwriting.
    UnsupportedEdit,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ProtectedSpan => {
                "This edit falls inside Markdown that is kept exactly as written, such as math, a block anchor or a callout's marker line."
            }
            Self::ProtectedBlock => {
                "This edit would have to replace a block that also carries source the document does not, such as a link reference definition."
            }
            Self::UnsupportedEdit => {
                "This edit cannot be saved without changing protected Markdown source. Your edits are retained; save a separate copy or use another editor."
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
        let root = parse_document(&arena, &normalized, &commonmark_options());
        let lines = line_ranges(body);
        let blocks: Vec<_> = root
            .children()
            .filter_map(|node| {
                let pos = node.data.borrow().sourcepos;
                let first = lines.get(pos.start.line.checked_sub(1)?)?;
                let last = lines.get(pos.end.line.checked_sub(1)?)?;
                Some(body_start + first.start..body_start + last.end)
            })
            .collect();
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
                let at = self.blocks.get(prefix).map_or_else(
                    || {
                        self.blocks
                            .last()
                            .map_or(self.body_start, |range| range.end)
                    },
                    |range| range.start,
                );
                let mut insertion = self.with_newlines(&after);
                if prefix > 0 {
                    insertion.insert_str(0, &self.newline.repeat(2));
                }
                if suffix > 0 {
                    insertion.push_str(&self.newline.repeat(2));
                }
                result.insert_str(at, &insertion);
            } else {
                let range = self.blocks[prefix].start..self.blocks[end - 1].end;
                result = self.patch(schema, document, result, range, &before, &after)?;
            }
        }
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
        let protected = protected_ranges(raw);
        // Whether a location the edit wanted was refused for overlapping protected
        // source, which is what separates "not here" from "not like this".
        let mut blocked = false;
        // Source and canonical Markdown can differ around the edit (reference
        // links, Setext headings, escapes). Prefer matching local context, then
        // prove the chosen location by parsing the entire resulting document.
        for spelling in emphasis_spellings(&removed) {
            for offset in candidate_offsets(raw, &spelling, &prefix, &suffix) {
                let changed = offset..offset + spelling.len();
                if protected.overlaps(&changed) {
                    blocked = true;
                    continue;
                }
                let mut candidate = source.clone();
                candidate.replace_range(
                    range.start + changed.start..range.start + changed.end,
                    &inserted,
                );
                if let Ok(valid) = self.validate(schema, target, candidate) {
                    return Ok(valid);
                }
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
                let found = emphasis_spellings(&removed)
                    .into_iter()
                    .find_map(|spelling| {
                        candidate_offsets(raw, &spelling, &prefix, &suffix)
                            .into_iter()
                            .find_map(|offset| {
                                let changed = offset..offset + spelling.len();
                                (!protected.overlaps(&changed)).then_some(changed)
                            })
                    });
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
        // Only canonical original source is eligible: any unknown spelling,
        // definition or trivia makes this fallback unsafe rather than
        // expendable. A callout's marker line is the exception — the document
        // holds its bytes in the quote's attributes, so writing the block again
        // writes the marker again.
        if raw == self.with_newlines(before) && protected.all_rebuildable() {
            let mut candidate = source;
            candidate.replace_range(range, &self.with_newlines(after));
            return self.validate(schema, target, candidate);
        }
        if blocked {
            Err(SourceError::ProtectedSpan)
        } else if !protected.is_empty() {
            Err(SourceError::ProtectedBlock)
        } else {
            Err(SourceError::UnsupportedEdit)
        }
    }
}

fn without_trailing_spaces(schema: &Schema, node: &Node) -> Node {
    if node.is_text() || node.child_count() == 0 {
        return node.clone();
    }
    let mut children: Vec<_> = node
        .children()
        .map(|child| without_trailing_spaces(schema, child))
        .collect();
    if [crate::schema::PARAGRAPH, crate::schema::HEADING]
        .iter()
        .any(|name| schema.node_id(name) == Some(node.type_id()))
    {
        while let Some(last) = children.last() {
            // Emphasis rules expel whitespace outside their delimiters. Link,
            // code, HTML and custom marks may retain meaningful internal
            // whitespace, so they must continue to pass strict validation.
            if last.marks().iter().any(|mark| {
                ![
                    crate::schema::EM,
                    crate::schema::STRONG,
                    crate::schema::STRIKETHROUGH,
                ]
                .iter()
                .any(|name| schema.mark_id(name) == Some(mark.ty))
            }) {
                break;
            }
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

fn block_markdown(schema: &Schema, document: &Node, nodes: &[Node]) -> String {
    if nodes.is_empty() {
        String::new()
    } else if nodes.len() == 1
        && schema.node_id(crate::schema::PARAGRAPH) == Some(nodes[0].type_id())
        && nodes[0].content_size() == 0
    {
        // An isolated empty paragraph serializes as an empty *document*. Here
        // it is a block within an existing document: keep its explicit marker
        // so Enter, list exit, and subsequent typing remain representable.
        "<br>".into()
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

fn emphasis_spellings(source: &str) -> Vec<String> {
    let mut spellings = vec![source.to_owned()];
    if source.contains('*') {
        spellings.push(source.replace('*', "_"));
    }
    spellings
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

fn overlaps(change: &Range<usize>, protected: &Range<usize>) -> bool {
    if change.is_empty() {
        protected.start < change.start && change.start < protected.end
    } else {
        change.start < protected.end && protected.start < change.end
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

/// Where an edit may not land, and which of those spans the document itself can
/// put back.
#[derive(Debug, Default)]
struct Protected {
    /// Every span an edit must not overlap.
    spans: Vec<Range<usize>>,
    /// The spans whose bytes the *document* carries — a callout's marker line,
    /// which lives in its quote's attributes — so replacing the whole block
    /// writes them again exactly as they were. Everything else here is source
    /// the tree has no record of, and a rewrite would lose or respell it.
    rebuildable: Vec<Range<usize>>,
}

impl Protected {
    fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    fn overlaps(&self, change: &Range<usize>) -> bool {
        self.spans.iter().any(|span| overlaps(change, span))
    }

    /// Whether a rewrite of the whole block puts every protected span back.
    fn all_rebuildable(&self) -> bool {
        self.spans
            .iter()
            .all(|span| self.rebuildable.contains(span))
    }
}

fn protected_ranges(source: &str) -> Protected {
    let mut rebuildable = Vec::new();
    let mut protected = Vec::new();
    let code = code_ranges(source);
    for (open, close) in [("%%", "%%")] {
        let mut offset = 0;
        while let Some(start) = source[offset..].find(open).map(|at| offset + at) {
            if let Some(span) = code.iter().find(|span| span.contains(&start)) {
                offset = span.end;
                continue;
            }
            if let Some(end) = source[start + open.len()..].find(close) {
                let end = start + open.len() + end + close.len();
                protected.push(start..end);
                offset = end;
            } else {
                protected.push(start..source.len());
                break;
            }
        }
    }
    protected.extend(math_ranges(source, &code));
    protected.extend(unread_wiki_links(source, &code));
    for range in line_ranges(source) {
        let line = &source[range.clone()];
        let trimmed = line.trim_start();
        // A callout's marker line is in the tree as attributes rather than as
        // text, so nothing an edit says can rebuild it; the body it opened is
        // ordinary content. `[!…]` anywhere else is the plain text it has
        // always been and is not guarded at all.
        if crate::callout::quote_content(line)
            .and_then(crate::callout::read_callout)
            .is_some()
            && !code
                .iter()
                .any(|span| span.start <= range.start && range.end <= span.end)
        {
            protected.push(range.clone());
            rebuildable.push(range.clone());
        }
        // Reference definitions can disappear from the semantic tree, so never
        // treat them as replaceable whitespace inside a structural range.
        if trimmed.starts_with('[')
            && trimmed.contains("]:")
            && !code
                .iter()
                .any(|span| span.start <= range.start && range.end <= span.end)
        {
            protected.push(range.clone());
        }
        if let Some(at) = line.rfind(" ^")
            && !code
                .iter()
                .any(|span| span.contains(&(range.start + at + 1)))
        {
            protected.push(range.start + at + 1..range.end);
        }
    }
    Protected {
        spans: protected,
        rebuildable,
    }
}

/// The `[[…]]` spans this codec does *not* read as a
/// [`WIKI_LINK`](crate::schema::WIKI_LINK) atom.
///
/// A recognised one is a node of its own now: its source is ordinary content
/// that an edit may replace, insert or delete, and the guard would otherwise
/// refuse the very operations the atom exists for. Everything else — an empty
/// alias, a `[` inside the brackets, an unterminated `[[` — is still source
/// this codec cannot rebuild, so it stays untouchable.
fn unread_wiki_links(source: &str, code: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut protected = Vec::new();
    let mut offset = 0;
    while let Some(start) = source[offset..].find("[[").map(|at| offset + at) {
        if let Some(span) = code.iter().find(|span| span.contains(&start)) {
            offset = span.end;
            continue;
        }
        // An embed's `!` is part of its source, and an escaped one is not an
        // embed at all.
        let from = if source[..start].ends_with('!') && !source[..start].ends_with("\\!") {
            start - 1
        } else {
            start
        };
        if let Some((_, len)) = crate::wiki::read_wiki_link(&source[from..]) {
            offset = from + len;
            continue;
        }
        match source[start + 2..].find("]]") {
            Some(end) => {
                let end = start + 2 + end + 2;
                protected.push(start..end);
                offset = end;
            }
            None => {
                protected.push(start..source.len());
                break;
            }
        }
    }
    protected
}

/// The `$`-delimited math spans in `source`.
///
/// Math is unsupported and has to stay byte-preserved, but a `$` is also an
/// ordinary character: `costs $5 and $10` is prose, not a formula between two
/// prices. The usual dollar-math delimiter rules separate the two — an opening
/// `$` is not followed by whitespace, a closing one is not preceded by
/// whitespace and not followed by a digit, `$$…$$` is display math, neither
/// kind spans a blank line, and `\$` is not a delimiter at all.
fn math_ranges(source: &str, code: &[Range<usize>]) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'\\' {
            offset += 2;
            continue;
        }
        if bytes[offset] != b'$' || code.iter().any(|span| span.contains(&offset)) {
            offset += 1;
            continue;
        }
        let display = bytes.get(offset + 1) == Some(&b'$');
        let open = if display { 2 } else { 1 };
        // Inline math opens only on a `$` with something other than whitespace
        // after it, which is what keeps `costs $5 and $10` prose.
        if !display && bytes.get(offset + 1).is_none_or(u8::is_ascii_whitespace) {
            offset += 1;
            continue;
        }
        match math_end(bytes, offset + open, display) {
            Some(end) => {
                ranges.push(offset..end);
                offset = end;
            }
            None => offset += open,
        }
    }
    ranges
}

/// Where the math opened before `from` closes, or `None` where nothing closes
/// it before a blank line or the end of the source.
fn math_end(bytes: &[u8], from: usize, display: bool) -> Option<usize> {
    let mut offset = from;
    while offset < bytes.len() {
        match bytes[offset] {
            b'\\' => offset += 2,
            b'\n' => {
                let mut ahead = offset + 1;
                while bytes.get(ahead).is_some_and(|b| matches!(b, b' ' | b'\t')) {
                    ahead += 1;
                }
                if bytes.get(ahead).is_none_or(|b| *b == b'\n') {
                    return None;
                }
                offset = ahead;
            }
            b'$' if display => {
                if bytes.get(offset + 1) == Some(&b'$') {
                    return Some(offset + 2);
                }
                offset += 1;
            }
            b'$' => {
                let closes = offset > from
                    && !bytes[offset - 1].is_ascii_whitespace()
                    && !bytes.get(offset + 1).is_some_and(u8::is_ascii_digit);
                if closes {
                    return Some(offset + 1);
                }
                offset += 1;
            }
            _ => offset += 1,
        }
    }
    None
}

/// Unknown-syntax guards do not apply inside ordinary Markdown code literals.
fn code_ranges(source: &str) -> Vec<Range<usize>> {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let arena = Arena::new();
    let root = parse_document(&arena, &normalized, &commonmark_options());
    let lines = line_ranges(source);
    root.descendants()
        .filter_map(|node| {
            let data = node.data.borrow();
            let block = matches!(data.value, NodeValue::CodeBlock(_));
            if !block && !matches!(data.value, NodeValue::Code(_)) {
                return None;
            }
            let first = lines.get(data.sourcepos.start.line.checked_sub(1)?)?;
            let last = lines.get(data.sourcepos.end.line.checked_sub(1)?)?;
            if block {
                Some(first.start..last.end)
            } else {
                let start = first.start + data.sourcepos.start.column.saturating_sub(1);
                let end = (last.start + data.sourcepos.end.column).min(last.end);
                (start <= end).then_some(start..end)
            }
        })
        .collect()
}
