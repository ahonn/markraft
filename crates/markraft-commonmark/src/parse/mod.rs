//! Reading Markdown into a document tree.
//!
//! comrak does the CommonMark parsing; this module maps its AST onto the
//! schema with a [`ParseRules`] table and repairs whatever the two disagree
//! about, so every document it returns passes [`Node::check`].
//!
//! # What the mapping decides
//!
//! * A paragraph's, a heading's and a table cell's content is its **inline
//!   source**: the text a reader sees once the block's own syntax is stripped,
//!   one `line_break` atom per line ending, and an atom for each image, wiki
//!   link and raw HTML tag. Every style mark is derived from that text; see
//!   `textblock` and [`derive`](mod@crate::derive). comrak's inline tree is not
//!   consulted for it, so the inline rules of a [`ParseRules`] table only
//!   apply where an inline construct turns up in block position.
//! * An **HTML block holding only `<br>`** becomes an empty paragraph. Empty
//!   paragraphs have no CommonMark write-back; they become blank separators. Runs of blank
//!   lines in the source are separators, as CommonMark says, and produce
//!   nothing.
//! * **Inline HTML** is a `raw_inline` atom holding the tag as written, except
//!   what [`derive`](crate::derive) reads as something else: a paired `<u>`,
//!   `<em>`, `<strong>`, `<del>`, `<mark>`, `<sup>` or `<a href>` is that style
//!   spelled in the text, a `<br>` ending a line spells that line's hard break,
//!   an `<img>` is an `image` atom that writes its tag back, and a `<u>` or
//!   `</u>` without its partner stays text.
//! * A **wiki link** — `[[target]]`, `[[target|alias]]` or the embed
//!   `![[target]]` — becomes a `wiki_link` atom holding the bytes the source
//!   spelled. A spelling [`crate::wiki`] refuses stays the text a reader sees.
//! * An **indented code block** becomes an ordinary `code_block` and is written
//!   back fenced. The two render identically.
//! * **Link reference definitions** are kept as they were written, in a
//!   `raw_block` where they stood — between blocks, or at the start of the
//!   paragraph they opened — so the reference links in the text still resolve
//!   once the file is written back. Every textblock is read against them, so
//!   `[a][ref]`, `[ref][]` and `[ref]` are links wherever they stand.
//! * A **GFM table** becomes a `table` of `table_row`s of `table_cell`s, with
//!   the delimiter row's alignments on the table. The first row is the header
//!   row, and every row is squared off to the column count the alignments
//!   declare — see [`crate::table`].
//! * Anything else — footnote definitions, HTML blocks, whatever a comrak
//!   extension produces — is kept as source text: a `raw_block` whose text is
//!   that source where a block is expected. Inline HTML has its own raw
//!   primitive.
//! * A container nested deeper than [`MAX_BLOCK_DEPTH`] is kept as source
//!   text too, in a `raw_block`: every pass over the tree after this one
//!   recurses once per level, and a file of nothing but `>` would otherwise
//!   take the stack with it.
//!
//! # Repair
//!
//! comrak's tree and the schema need not agree: a list item may hold blocks the
//! schema's content rule forbids, and a consumer's rule set may be looser still.
//! Each container's children are matched against its content automaton; a child
//! that does not fit is wrapped in whatever the schema says would make it fit,
//! failing that preceded by the children the rule requires, and only dropped
//! when neither works.

mod inline;
pub(crate) use inline::is_break_tag;

use std::cell::Ref;

use comrak::nodes::{AstNode, LineColumn, NodeTaskItem, NodeValue};
use comrak::{Arena, Options, parse_document};
use markraft_core::{Attrs, Fragment, Node, NodeError, NodeTypeId, Schema, Slice};

use crate::rules::{ParseCx, ParseRule, ParseRules, ParseTarget, commonmark_rules};

/// How many containers deep the tree is built. The first container past it
/// that a `raw_block` can stand in for is kept as its source in one.
///
/// comrak stops opening lists at 100; quotes it nests without limit. A list
/// item counts twice, once for the list and once for the item.
pub(crate) const MAX_BLOCK_DEPTH: usize = 128;

/// Why a document could not be built.
///
/// Parsing Markdown itself never fails — CommonMark has no invalid input — so
/// every variant is a disagreement between the rule set and the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A rule named a node or mark type the schema does not declare.
    UnknownType {
        /// `"node"` or `"mark"`.
        kind: &'static str,
        /// The name the rule asked for.
        name: String,
    },
    /// A node could not be built: wrong attributes, or content the schema
    /// rejects even after repair.
    Schema(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnknownType { kind, name } => {
                write!(f, "the schema declares no {kind} type `{name}`")
            }
            ParseError::Schema(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<NodeError> for ParseError {
    fn from(error: NodeError) -> ParseError {
        ParseError::Schema(error.to_string())
    }
}

/// The comrak options this codec parses with.
///
/// `strikethrough`, `tasklist` and `table` back schema features; with `table`
/// off a table would arrive as a paragraph full of pipes and be reflowed into
/// one line. `autolink` is on so the URL an author typed plain becomes a link mark
/// rather than text that only *looks* like one; the serialiser writes such a
/// link back as the bare URL. `relaxed_autolinks` stays off: it reads a URL
/// inside brackets as a link too, which is not what GFM does.
///
/// `footnotes` is on, and every whole document is read through `parse_ast`,
/// which undoes the two things comrak does to footnote definitions that would
/// lose or move text: it drops a definition nothing refers to, and it moves
/// every definition to the end of the document.
///
/// `wikilinks_title_after_pipe` is on for the `[[target|alias]]` order,
/// which is what the files this editor shares are written in. comrak only
/// *finds* the construct: what it reads is normalised — the destination is
/// trimmed, unescaped and entity-resolved — so the [`WIKI_LINK`] atom takes its
/// parts from the source instead, and [`crate::wiki`] decides which spellings
/// count. The embed form `![[…]]` is not comrak's at all and is recognised in
/// the conversion layer.
///
/// `highlight` (`==…==`), `superscript` (`^…^`), `subscript` (`~…~`),
/// `math_dollars` (`$…$` and `$$…$$`) and `math_code` (`` $`…`$ ``) are on for
/// the spellings the notes this editor shares use. Math content is literal, so
/// a `^` or a `*` inside a formula is never read as a style. `subscript` takes
/// the single `~` GFM reads as strikethrough, leaving strikethrough to `~~`:
/// the notes this editor opens mean subscript by it far more often.
/// `underline` stays off, since it would take `__` from strong emphasis.
///
/// `cjk_friendly_emphasis` is on because CommonMark's flanking rules refuse a
/// delimiter run between CJK punctuation and a letter, so `**注意：**这里` would
/// stay literal asterisks. It adds no syntax: it only lets emphasis that is
/// already spelled be read.
///
/// Front matter is off: the host strips it before the codec sees the text.
/// Setext headings are *not* ignored, so `Title\n=====` imports as a heading.
///
/// comrak records source positions in the AST unconditionally; its `sourcepos`
/// option only adds attributes to rendered HTML, so it stays off.
///
/// [`WIKI_LINK`]: crate::schema::WIKI_LINK
pub fn commonmark_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.extension.highlight = true;
    options.extension.superscript = true;
    options.extension.subscript = true;
    options.extension.footnotes = true;
    options.extension.math_dollars = true;
    options.extension.math_code = true;
    options.extension.cjk_friendly_emphasis = true;
    options
}

/// Parse a whole document with comrak, its footnote definitions where the
/// source has them.
///
/// comrak drops a footnote definition nothing refers to and moves the rest to
/// the end of the document. So the source is read with a paragraph after it
/// that refers to every label a definition could have, which keeps them all,
/// and each definition is then put back into the container and at the place
/// its lines are in. The extra paragraph starts after the last line of the
/// source, so every position in the tree is the source's own; it is removed
/// again, and the document's end is the source's. Where that paragraph would
/// not stand on its own — the source ends in an open fence, say — the source
/// is read without footnotes instead: a definition is then the paragraph a
/// plain CommonMark reader sees, which loses nothing.
///
/// A task's check box before a heading or a quote, `- [ ] # title` or
/// `- [ ] > quote`, is read as two parts: the box, and the block after
/// it on its line. comrak only knows a box before a paragraph and reads the
/// rest of that line as the paragraph's text, so the source is read again
/// once with every such box taken out — the item's content column does not
/// count the box, so the item holds the same lines — and the positions after
/// them on their lines are moved back to where the source has them. A quote
/// such a box opens may hold, on the same line, an item whose own box opens
/// a block; those are found along the line from the first box, so one
/// reading finds them all.
pub(crate) fn parse_ast<'a>(
    arena: &'a Arena<'a>,
    source: &str,
    options: &Options<'static>,
) -> &'a AstNode<'a> {
    let root = parse_with_math(arena, source, options);
    let mut boxes = boxes_before_blocks(root, source);
    if boxes.is_empty() {
        return root;
    }
    boxes.sort_by_key(|found| (found.line, found.column));
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    for found in boxes.iter().rev() {
        let line = &mut lines[found.line - 1];
        line.replace_range(found.column - 1..found.column - 1 + found.len, "");
    }
    let root = parse_with_math(arena, &lines.join("\n"), options);
    let restore = |point: &mut LineColumn| {
        let mut shift = 0;
        for found in boxes.iter().filter(|found| found.line == point.line) {
            if point.column + shift >= found.column {
                shift += found.len;
            }
        }
        point.column += shift;
    };
    for node in root.descendants() {
        let mut data = node.data.borrow_mut();
        restore(&mut data.sourcepos.start);
        restore(&mut data.sourcepos.end);
        if let NodeValue::TaskItem(task) = &mut data.value {
            restore(&mut task.symbol_sourcepos.start);
            restore(&mut task.symbol_sourcepos.end);
        }
    }
    for found in &boxes {
        let item = root.descendants().find(|node| {
            matches!(node.data.borrow().value, NodeValue::Item(_))
                && node.first_child().is_some_and(|block| {
                    let start = block.data.borrow().sourcepos.start;
                    start.line == found.line && start.column == found.column + found.len
                })
        });
        if let Some(item) = item {
            item.data.borrow_mut().value = NodeValue::TaskItem(found.task);
        }
    }
    root
}

/// Protect standalone display formulas from CommonMark block parsing. Use
/// temporary code fences so blank lines, indentation and Markdown-looking
/// TeX stay literal, then restore paragraph nodes at the original positions.
/// The conversion layer reads their inline content from the original source.
fn parse_with_math<'a>(
    arena: &'a Arena<'a>,
    source: &str,
    options: &Options<'static>,
) -> &'a AstNode<'a> {
    let root = parse_with_footnotes(arena, source, options);
    if !options.extension.math_dollars || !source.contains("$$") {
        return root;
    }
    let lines: Vec<&str> = source.split('\n').collect();
    let mut blocks = crate::math::SourceBlocks::default();
    for node in root.descendants() {
        let data = node.data.borrow();
        let lines = data.sourcepos.start.line.saturating_sub(1)..data.sourcepos.end.line;
        match data.value {
            NodeValue::CodeBlock(_) | NodeValue::HtmlBlock(_) => blocks.add_literal(lines),
            NodeValue::Paragraph => blocks.add_paragraph(lines),
            _ => {}
        }
    }
    let mut spans = Vec::new();
    let mut tasks = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        let start = data.sourcepos.start.line.saturating_sub(1);
        let column = data.sourcepos.start.column.saturating_sub(1);
        // A setext underline in the TeX turns the opening lines into a heading.
        let leaf = match &data.value {
            NodeValue::Paragraph => true,
            NodeValue::Heading(heading) => heading.setext,
            _ => false,
        };
        if !leaf
            || spans
                .last()
                .is_some_and(|span: &crate::math::DisplayBlock| start < span.lines.end)
        {
            continue;
        }
        if let Some(span) = crate::math::DisplayBlock::starting_at(&lines, start, column, &blocks) {
            if let Some(parent) = node.parent()
                && let NodeValue::TaskItem(task) = parent.data.borrow().value
                && span.continuation().len() < column
            {
                tasks.push((start, task));
            }
            spans.push(span);
        }
    }
    if spans.is_empty() {
        return root;
    }
    let mut protected: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    for span in &spans {
        let start = span.lines.start;
        let end = span.lines.end - 1;
        let column = span.column;
        let length = lines[start + 1..end]
            .iter()
            .map(|line| line.chars().filter(|c| *c == '~').count() + 1)
            .max()
            .unwrap_or(3)
            .max(3);
        let fence = "~".repeat(length);
        let prefix = span.continuation();
        let opening_column = if tasks.iter().any(|&(line, _)| line == start) {
            prefix.len()
        } else {
            column
        };
        protected[start] = format!("{}{fence}", &lines[start][..opening_column]);
        protected[end] = format!("{prefix}{fence}");
    }
    let protected = parse_with_footnotes(arena, &protected.join("\n"), options);
    let mut restored = 0;
    for node in protected.descendants() {
        let mut data = node.data.borrow_mut();
        if matches!(data.value, NodeValue::CodeBlock(_))
            && let Some(span) = spans
                .iter()
                .find(|span| data.sourcepos.start.line == span.lines.start + 1)
        {
            let start = span.lines.start;
            let end = span.lines.end - 1;
            let column = span.column;
            // Never attach a later container's source to a code block that
            // CommonMark actually ended earlier.
            if data.sourcepos.end.line != end + 1 {
                return root;
            }
            data.value = NodeValue::Paragraph;
            restored += 1;
            data.sourcepos.start.column = column + 1;
            data.sourcepos.end = LineColumn {
                line: end + 1,
                column: lines[end].len(),
            };
            if let Some(&(_, task)) = tasks.iter().find(|&&(line, _)| line == start)
                && let Some(parent) = node.parent()
            {
                parent.data.borrow_mut().value = NodeValue::TaskItem(task);
            }
        }
    }
    if restored != spans.len() {
        return root;
    }
    protected.data.borrow_mut().sourcepos.end = source_end(source);
    protected
}

/// A task's check box that a heading or a quote follows on its line: where
/// the box starts, as a one-based byte column, and how many bytes it and the
/// whitespace after it take.
struct BoxBeforeBlock {
    line: usize,
    column: usize,
    len: usize,
    task: NodeTaskItem,
}

/// The check boxes in `root` whose item's paragraph opens with what would be
/// a heading or a quote without them, and the boxes after each of those on
/// its line that open a block the same way, inside the quote it opens.
fn boxes_before_blocks<'a>(root: &'a AstNode<'a>, source: &str) -> Vec<BoxBeforeBlock> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = Vec::new();
    for node in root.descendants() {
        let NodeValue::TaskItem(task) = &node.data.borrow().value else {
            continue;
        };
        let symbol = task.symbol_sourcepos.start;
        let Some(line) = lines.get(symbol.line.wrapping_sub(1)) else {
            continue;
        };
        // The box is `[`, the symbol and `]`, the symbol one byte wide.
        let open = symbol.column.saturating_sub(1);
        if line.as_bytes().get(open.wrapping_sub(1)) != Some(&b'[') {
            continue;
        }
        let mut close = open + 1;
        let mut box_task = *task;
        while let Some((len, quote)) = box_before_opener(line, close) {
            out.push(BoxBeforeBlock {
                line: symbol.line,
                column: close - 1,
                len,
                task: box_task,
            });
            // Along the quote's line, past the container prefix inside it,
            // to the next box, if the line opens an item with one there.
            let Some((next, checked)) = quote.and_then(|content| box_in_prefix(line, content))
            else {
                break;
            };
            close = next;
            // comrak's symbol is the character of a ticked box, none for an
            // unticked one.
            box_task = NodeTaskItem {
                symbol: checked.then_some('x'),
                ..*task
            };
        }
    }
    out
}

/// Whether the box whose `]` sits at byte `close` of `line` opens a block:
/// the bytes the box and the whitespace after it take, and — when what
/// follows is a quote marker rather than a heading — the byte the quote's
/// content starts at.
fn box_before_opener(line: &str, close: usize) -> Option<(usize, Option<usize>)> {
    let after = line.get(close + 1..)?;
    let text = after.trim_start_matches([' ', '\t']);
    // comrak only makes a box that whitespace follows.
    if text.len() == after.len() && !text.is_empty() {
        return None;
    }
    let len = 3 + after.len() - text.len();
    if text.starts_with('>') {
        return Some((len, Some(line.len() - text.len() + 1)));
    }
    let hashes = text.len() - text.trim_start_matches('#').len();
    let heading = (1..=6).contains(&hashes)
        && matches!(text.as_bytes().get(hashes), None | Some(b' ' | b'\t'));
    heading.then_some((len, None))
}

/// A box that opens a list item in the container prefix of `line` from byte
/// `from`: past any further quote markers, a list marker and the whitespace
/// after it, a `[ ]`, `[x]` or `[X]`. Answers the byte of its `]` and whether
/// it is ticked.
fn box_in_prefix(line: &str, from: usize) -> Option<(usize, bool)> {
    let rest = |at: usize| line.get(at..).unwrap_or_default();
    let mut at = from;
    loop {
        at += rest(at).len() - rest(at).trim_start_matches([' ', '\t']).len();
        if rest(at).starts_with('>') {
            at += 1;
        } else {
            break;
        }
    }
    let marker = rest(at);
    let digits = marker.len()
        - marker
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let width = match marker.as_bytes().first()? {
        b'-' | b'+' | b'*' => 1,
        _ if (1..=9).contains(&digits)
            && matches!(marker.as_bytes().get(digits), Some(b'.' | b')')) =>
        {
            digits + 1
        }
        _ => return None,
    };
    at += width;
    let spaces = rest(at).len() - rest(at).trim_start_matches([' ', '\t']).len();
    if !(1..=4).contains(&spaces) {
        return None;
    }
    at += spaces;
    let checked = match rest(at).as_bytes() {
        [b'[', b' ', b']', ..] => false,
        [b'[', b'x' | b'X', b']', ..] => true,
        _ => return None,
    };
    Some((at + 2, checked))
}

fn parse_with_footnotes<'a>(
    arena: &'a Arena<'a>,
    source: &str,
    options: &Options<'static>,
) -> &'a AstNode<'a> {
    let labels = footnote_labels(source);
    if !options.extension.footnotes || labels.is_empty() {
        return parse_document(arena, source, options);
    }
    let end = source_end(source);
    let gap = if source.ends_with('\n') { "\n" } else { "\n\n" };
    let references: String = labels.iter().map(|label| format!("[^{label}]")).collect();
    let root = parse_document(arena, &format!("{source}{gap}{references}\n"), options);
    let appended = root.children().find(|child| {
        let data = child.data.borrow();
        matches!(data.value, NodeValue::Paragraph) && data.sourcepos.start.line > end.line
    });
    let Some(appended) = appended else {
        let mut plain = options.clone();
        plain.extension.footnotes = false;
        return parse_document(arena, source, &plain);
    };
    appended.detach();
    root.data.borrow_mut().sourcepos.end = end;
    let definitions: Vec<_> = root
        .children()
        .filter(|child| matches!(child.data.borrow().value, NodeValue::FootnoteDefinition(_)))
        .collect();
    for definition in definitions {
        definition.detach();
        place(root, definition);
    }
    root
}

/// Put a detached footnote definition back where its lines are: inside the
/// deepest container whose lines hold them, before the first child after them.
fn place<'a>(parent: &'a AstNode<'a>, definition: &'a AstNode<'a>) {
    let line = definition.data.borrow().sourcepos.start.line;
    for child in parent.children() {
        let pos = child.data.borrow().sourcepos;
        if pos.start.line > line {
            child.insert_before(definition);
            return;
        }
        let container = matches!(
            child.data.borrow().value,
            NodeValue::BlockQuote
                | NodeValue::List(_)
                | NodeValue::Item(_)
                | NodeValue::TaskItem(_)
                | NodeValue::FootnoteDefinition(_)
                | NodeValue::Alert(_)
        );
        if container && pos.end.line >= line {
            place(child, definition);
            return;
        }
    }
    parent.append(definition);
}

/// Every label a footnote definition in `source` could have: each `[^label]:`.
fn footnote_labels(source: &str) -> Vec<&str> {
    let mut labels: Vec<&str> = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("[^") {
        rest = &rest[at + 2..];
        let Some(close) = rest.find("]:") else {
            break;
        };
        let label = &rest[..close];
        if !label.is_empty()
            && !label.contains(|c: char| c.is_whitespace() || matches!(c, '[' | ']'))
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    labels
}

/// Where comrak says a document of `source` ends: its last line, and the
/// byte length of that line.
fn source_end(source: &str) -> comrak::nodes::LineColumn {
    let lines: Vec<&str> = source.lines().collect();
    comrak::nodes::LineColumn {
        line: lines.len().max(1),
        column: lines.last().map_or(0, |line| line.len()),
    }
}

/// Reads Markdown into a [`Node`] tree.
#[derive(Clone)]
pub struct MarkdownParser {
    schema: Schema,
    rules: ParseRules,
    options: Options<'static>,
}

impl MarkdownParser {
    /// A parser driving `rules` against `schema`.
    pub fn new(schema: Schema, rules: ParseRules) -> MarkdownParser {
        MarkdownParser {
            schema,
            rules,
            options: commonmark_options(),
        }
    }

    /// A parser with the CommonMark/GFM rule set.
    pub fn commonmark(schema: Schema) -> MarkdownParser {
        MarkdownParser::new(schema, commonmark_rules())
    }

    /// Replace the comrak options. Enabling an extension is how a consumer gets
    /// its construct into the AST; a rule for that kind then decides what the
    /// construct becomes.
    pub fn with_options(mut self, options: Options<'static>) -> MarkdownParser {
        self.options = options;
        self
    }

    /// The comrak options in force.
    pub fn options(&self) -> &Options<'static> {
        &self.options
    }

    /// The schema documents are built against.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Read `source` as a fragment to paste into existing content.
    ///
    /// The result is a [`Slice`] opened by
    /// [`open_fragment`](crate::fragment::open_fragment), so a source that is
    /// one paragraph merges into the textblock the caret sits in, while a run
    /// of blocks lands as blocks.
    ///
    /// Unlike [`MarkdownParser::parse`], the spaces and tabs at either end of
    /// the source are kept where they are text: a reader strips a paragraph's
    /// leading and trailing whitespace, but a pasted `hello ` has to stay
    /// apart from the `tail` after the caret, and ` more` from the word
    /// before it.
    ///
    /// Text indented four spaces at the top level reads as prose, not as an
    /// indented code block: what another application puts
    /// on the clipboard is indented for a reader — a log, a terminal's output,
    /// a quoted mail — far more often than it is Markdown's older code syntax,
    /// and a fenced block still says code unmistakably.
    pub fn parse_fragment(&self, source: &str) -> Result<Slice, ParseError> {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        let kept = normalized[normalized.trim_end_matches([' ', '\t']).len()..].to_string();
        let normalized = self.without_indented_code(&normalized);
        let doc = self.parse(&normalized)?;
        let doc = crate::fragment::append_trailing(&self.schema, &doc, &kept);
        let leading = if normalized.trim().is_empty() {
            ""
        } else {
            &normalized[..normalized.len() - normalized.trim_start_matches([' ', '\t']).len()]
        };
        let doc = crate::fragment::prepend_leading(&self.schema, &doc, leading);
        Ok(crate::fragment::open_fragment(
            &self.schema,
            doc.content().clone(),
        ))
    }

    /// `source` with the lines of each top-level indented code block taken to
    /// the margin, so they read as the paragraphs they were meant as. Such a
    /// block nested in a list or a quote is the list's or the quote's own and
    /// is left as it is.
    fn without_indented_code(&self, source: &str) -> String {
        let arena = Arena::new();
        let root = parse_document(&arena, source, &self.options);
        let mut indented = Vec::new();
        for child in root.children() {
            let data = child.data.borrow();
            if let NodeValue::CodeBlock(code) = &data.value
                && !code.fenced
            {
                indented.push(data.sourcepos.start.line..=data.sourcepos.end.line);
            }
        }
        if indented.is_empty() {
            return source.to_owned();
        }
        source
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                if indented.iter().any(|lines| lines.contains(&(index + 1))) {
                    line.trim_start()
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Read `source` into a document.
    pub fn parse(&self, source: &str) -> Result<Node, ParseError> {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        let cx = ParseCx::new(&self.schema, &normalized);
        let arena = Arena::new();
        let root = parse_ast(&arena, &normalized, &self.options);
        let walk = Walk {
            schema: &self.schema,
            rules: &self.rules,
            cx: &cx,
            options: &self.options,
        };
        let blocks = walk.blocks(root, self.schema.top_type(), 0)?;
        let doc = walk.fit(self.schema.top_type(), Attrs::empty(), blocks)?;
        let doc = crate::table::normalize_tables(&self.schema, &doc).unwrap_or(doc);
        let doc = crate::textblock::resolve_references(&self.schema, &doc);
        doc.check(&self.schema)?;
        Ok(doc)
    }
}

pub(crate) struct Walk<'a> {
    pub(crate) schema: &'a Schema,
    pub(crate) rules: &'a ParseRules,
    pub(crate) cx: &'a ParseCx<'a>,
    pub(crate) options: &'a Options<'static>,
}

impl<'a> Walk<'a> {
    pub(crate) fn node_id(&self, name: &str) -> Result<NodeTypeId, ParseError> {
        self.schema
            .node_id(name)
            .ok_or_else(|| ParseError::UnknownType {
                kind: "node",
                name: name.to_string(),
            })
    }

    pub(crate) fn target(&self, node: &'a AstNode<'a>) -> ParseTarget<'a> {
        ParseTarget { node, cx: self.cx }
    }

    pub(crate) fn value(&self, node: &'a AstNode<'a>) -> Ref<'a, NodeValue> {
        self.target(node).value()
    }

    // -- blocks ------------------------------------------------------------

    /// The blocks inside `parent`, a `parent_ty` that sits `depth` containers
    /// deep.
    fn blocks(
        &self,
        parent: &'a AstNode<'a>,
        parent_ty: NodeTypeId,
        depth: usize,
    ) -> Result<Vec<Node>, ParseError> {
        let mut out = Vec::new();
        let pos = self.target(parent).sourcepos();
        let mut next = pos.start.line.max(1);
        for child in parent.children() {
            let child_pos = self.target(child).sourcepos();
            self.push_definitions(parent, next, child_pos.start.line, &mut out)?;
            self.block(child, parent_ty, depth, &mut out)?;
            next = next.max(child_pos.end.line + 1);
        }
        self.push_definitions(parent, next, pos.end.line + 1, &mut out)?;
        Ok(out)
    }

    /// The link reference definitions between `from` and `to`, each run as a
    /// raw block.
    fn push_definitions(
        &self,
        parent: &'a AstNode<'a>,
        from: usize,
        to: usize,
        out: &mut Vec<Node>,
    ) -> Result<(), ParseError> {
        if from >= to {
            return Ok(());
        }
        let in_list = matches!(&*self.value(parent), NodeValue::List(_));
        for definitions in self.definitions_between(parent, from, to) {
            let block = self.raw_block(crate::schema::RAW_BLOCK, &definitions)?;
            // Between two items, a definition indented under the first is its
            // content: comrak ends an item at its last block, not at the
            // definitions and blank lines after it.
            match out.last_mut() {
                Some(item) if in_list && item.is_container() => {
                    let mut children: Vec<Node> = item.children().cloned().collect();
                    children.push(block);
                    *item = item.copy(Fragment::from_nodes(children));
                }
                _ => out.push(block),
            }
        }
        Ok(())
    }

    fn block(
        &self,
        node: &'a AstNode<'a>,
        parent_ty: NodeTypeId,
        depth: usize,
        out: &mut Vec<Node>,
    ) -> Result<(), ParseError> {
        let target = self.target(node);
        if self.is_empty_paragraph_html(node)
            && let Some(name) = self.paragraph_type(target)
        {
            let ty = self.node_id(&name)?;
            out.push(self.fit(ty, Attrs::empty(), Vec::new())?);
            return Ok(());
        }
        let rule = self.rules.rule(&self.value(node)).clone();
        match rule {
            ParseRule::Ignore => {}
            ParseRule::Block { node_type, attrs } => {
                let ty = self.node_id(&node_type(target))?;
                let kind = self.schema.node_type(ty);
                let children = if kind.is_leaf() {
                    Vec::new()
                } else if kind.has_inline_content() {
                    let (children, definitions) = self.textblock(node)?;
                    if let Some(definitions) = definitions {
                        out.push(self.raw_block(crate::schema::RAW_BLOCK, &definitions)?);
                    }
                    children
                } else if depth >= MAX_BLOCK_DEPTH && self.holds_raw(parent_ty)? {
                    out.push(self.raw_block(crate::schema::RAW_BLOCK, &self.block_source(node))?);
                    return Ok(());
                } else {
                    self.blocks(node, ty, depth + 1)?
                };
                let (attrs, children) = self.callout(node, attrs(target), children);
                out.push(self.fit(ty, attrs, children)?);
            }
            ParseRule::TextBlock {
                node_type,
                attrs,
                text,
            } => {
                let ty = self.node_id(&node_type(target))?;
                let body = text(target);
                let children = if body.is_empty() {
                    Vec::new()
                } else {
                    vec![self.schema.text(&body)]
                };
                out.push(self.fit(ty, attrs(target), children)?);
            }
            ParseRule::Raw { node_type } => {
                out.push(self.raw_block(&node_type(target), &self.block_source(node))?);
            }
            // An inline rule met in block position: keep the source rather than
            // lose the text.
            ParseRule::Atom { .. } | ParseRule::Mark { .. } | ParseRule::Text { .. } => {
                if let Some(name) = self.rules.raw_block_type(target) {
                    out.push(self.raw_block(&name, &self.block_source(node))?);
                }
            }
        }
        Ok(())
    }

    /// Take a callout's marker line out of the body it opened.
    ///
    /// The marker is in the attributes now, and it is not text, so the
    /// paragraph it shares with the first body line has to lose its first
    /// line. A marker that is the whole paragraph takes the paragraph with it;
    /// [`crate::fit::fit`] puts the empty one back that the content rule asks
    /// for.
    ///
    /// The cut is the paragraph's first line break, and what follows it is
    /// read again as a paragraph of its own: `> [!note] **bold` with the
    /// emphasis closing on the line below leaves a body that is not bold, which
    /// is how the body reads once the marker is taken off it.
    fn callout(
        &self,
        node: &'a AstNode<'a>,
        attrs: Attrs,
        mut children: Vec<Node>,
    ) -> (Attrs, Vec<Node>) {
        let Some(kind) = attrs.get("callout").and_then(|value| value.as_str()) else {
            return (attrs, children);
        };
        if kind.is_empty() {
            // An ordinary quote whose first line only looks like a marker —
            // indented, say — keeps it text with a backslash, as it is
            // written back.
            if let Some(first) = children.first_mut() {
                *first = crate::textblock::escape_callout_lookalike(self.schema, first);
            }
            return (attrs, children);
        }
        if self.take_marker_line(node, &mut children) {
            (attrs, children)
        } else {
            let plain = attrs.with("callout", "").with("fold", "").with("title", "");
            (plain, children)
        }
    }

    /// Take the marker line out of the body, answering whether it could be
    /// taken out at all.
    fn take_marker_line(&self, node: &'a AstNode<'a>, children: &mut Vec<Node>) -> bool {
        if node.first_child().is_none() {
            return false;
        }
        let Some(paragraph) = children.first().cloned() else {
            return false;
        };
        let Some(kind) = crate::textblock::block_kind(self.schema, paragraph.type_id()) else {
            return false;
        };
        let items = crate::textblock::Items::from_nodes(self.schema, paragraph.children());
        let cut = items
            .0
            .iter()
            .position(|item| *item == crate::textblock::Item::Break);
        match cut {
            Some(cut) => {
                let body = crate::textblock::Items(items.0[cut + 1..].to_vec());
                let derived =
                    crate::derive::derive(kind, &body.text(), &crate::derive::DeriveContext::new());
                let body = body.nodes(self.schema, &derived);
                children[0] = paragraph.copy(Fragment::from_nodes(body));
                true
            }
            // No break, so the marker is the whole paragraph.
            None => {
                children.remove(0);
                true
            }
        }
    }

    /// The node type the rule set uses for paragraphs, which is also what an
    /// empty paragraph is built from.
    fn paragraph_type(&self, target: ParseTarget<'a>) -> Option<String> {
        match self.rules.rule(&NodeValue::Paragraph) {
            ParseRule::Block { node_type, .. } => Some(node_type(target)),
            _ => None,
        }
    }

    /// Whether the node is an HTML block holding nothing but a `<br>` tag.
    /// Older Markraft files used that spelling for empty paragraphs; we still
    /// read it, but no longer write it.
    fn is_empty_paragraph_html(&self, node: &'a AstNode<'a>) -> bool {
        matches!(&*self.value(node), NodeValue::HtmlBlock(html) if inline::is_break_tag(&html.literal))
    }

    /// The source of a block that is kept verbatim. comrak hands an HTML block
    /// its literal text already free of container indentation; everything else
    /// is recovered from the source lines.
    fn block_source(&self, node: &'a AstNode<'a>) -> String {
        if let NodeValue::HtmlBlock(html) = &*self.value(node) {
            return html
                .literal
                .strip_suffix('\n')
                .unwrap_or(&html.literal)
                .to_string();
        }
        // References live outside the raw block and are consumed by comrak.
        // Its formatter writes resolved link/image destinations inline, making
        // the preserved block self-contained when those definitions disappear.
        if node.descendants().any(|child| {
            matches!(
                &*self.value(child),
                NodeValue::Link(_) | NodeValue::Image(_)
            )
        }) {
            let mut source = String::new();
            comrak::format_commonmark(node, &commonmark_options(), &mut source)
                .expect("formatting into a String cannot fail");
            return source.trim_end_matches('\n').to_string();
        }
        self.cx.block_source(self.target(node).sourcepos())
    }

    /// Whether a `raw_block` can stand among `parent`'s children as it is.
    ///
    /// A list's children are items and a table's are rows: a raw block there
    /// would be wrapped in a new item, whose marker the source already spells,
    /// and every save would add one more. Such a container is kept and the cut
    /// falls a level further down.
    fn holds_raw(&self, parent: NodeTypeId) -> Result<bool, ParseError> {
        let raw = self.node_id(crate::schema::RAW_BLOCK)?;
        Ok(self.schema.can_contain(parent, raw))
    }

    /// A raw block holding `source` as its text. An empty source leaves the
    /// block empty, because a textblock has no child standing for no text.
    fn raw_block(&self, name: &str, source: &str) -> Result<Node, ParseError> {
        let ty = self.node_id(name)?;
        let children = if source.is_empty() {
            Vec::new()
        } else {
            vec![self.schema.text(source)]
        };
        self.fit(ty, Attrs::empty(), children)
    }

    /// Build a node of `ty` holding `children`, making the content fit.
    pub(crate) fn fit(
        &self,
        ty: NodeTypeId,
        attrs: Attrs,
        children: Vec<Node>,
    ) -> Result<Node, ParseError> {
        Ok(crate::fit::fit(self.schema, ty, attrs, children)?)
    }
}
