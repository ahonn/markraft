//! Reading the styles a textblock's inline source spells.
//!
//! In the inline source model a textblock's text *is* its Markdown inline
//! source — delimiters, backslash escapes, entities and HTML tags included —
//! and every style mark on it is a function of that text. [`derive()`] is that
//! function. It knows nothing about the document tree: it takes the block's
//! kind, its text and the context the text is read in, and answers which
//! characters carry which style and which characters are spelling a reader
//! does not see.
//!
//! # How it reads
//!
//! The text is [guarded](guard) and handed to comrak as a document of its own,
//! shaped so that comrak reads it as exactly one block of the given kind, and
//! the positions comrak reports for each inline node are mapped back to
//! character offsets in the original text. Reference definitions from the
//! context are appended after a blank line, where they resolve `[a][ref]`
//! without moving anything before them. Opaque atoms — an image, a wiki link,
//! a raw HTML tag — stand in the text as one U+FFFC each, a symbol that
//! CommonMark 0.31 classes as punctuation, like the `!`, `[`, `<` and `>` their
//! own spellings start and end with.
//!
//! # What it answers
//!
//! * [`Derived::styles`] — one [`StyleSpan`] per styled node, covering the
//!   node's whole source: `**a**` is strong over all five characters. A style
//!   therefore lands on its own delimiters, which is what keeps a span one
//!   contiguous mark range when it is applied to the tree.
//! * [`Derived::conceals`] — the runs a reader does not see: delimiters, the
//!   backslash of an escape, an entity's spelling, a hard break's spelling.
//!   The opening and closing runs of one span share a [`Conceal::span`] id, so
//!   a view can reveal them together.
//! * [`Derived::hard_breaks`] — the line endings that are hard breaks. Every
//!   other line ending is a soft break.
//! * [`Derived::atoms`] — the runs of text a reader takes for something the
//!   tree holds as an atom: an image, a wiki link (the `![[…]]` embed
//!   included), an `<img>` tag, a raw HTML tag read as nothing else. Typed as
//!   text, they are what the canonicalising correction turns into atoms. A
//!   `<u>` or `</u>` with no partner stays text: it is half of a style being
//!   typed.
//!
//! # Inline HTML
//!
//! A tag a reader renders as a style is that style: a `<u>`, `<em>`,
//! `<strong>`, `<del>`, `<mark>`, `<sup>` or `<a href>` paired with its closing
//! tag among the same node's children styles what lies between, and the two
//! tags are the span's concealed delimiters. A `<br>` a line ending follows is that line
//! ending's hard-break spelling. Every other tag — a style tag without its
//! partner included — is an atom.
//!
//! # Where comrak's positions need help
//!
//! comrak's inline positions are right almost everywhere; the differential
//! test in `tests/derive.rs` checks them against its HTML. Where they are not:
//!
//! * Inside a table cell comrak removes the backslash of every `\|` before it
//!   reads the cell's inline content, and reports inline positions in that
//!   shortened string. The mapping here replays the same removal, so positions
//!   after an escaped pipe land where they belong; each removed backslash is a
//!   concealed escape of its own.
//! * A code span that crosses a line ending has its end reported against the
//!   indentation of the paragraph's line numbered by how many line endings
//!   the span holds, not of the line it ends on, and sometimes past the end of
//!   that line. Its start is right, so the end is found the way a reader finds
//!   it: the next run of exactly as many backticks.
//! * A hard break spelled with more than two spaces is reported over its last
//!   two only. Every space and tab before the line ending is part of the
//!   spelling a reader does not see, so the conceal run covers all of them.
//! * comrak does not count a line ending inside what follows a link's or an
//!   image's text — its `(…)` or its `[…]` reference label — nor inside a wiki
//!   link. Every node after such a line ending is reported with the line
//!   numbers comrak's count gives it, one too early per line ending missed,
//!   and with columns counted on through the line ending it missed; a link
//!   whose `(…)` spans lines is reported as ending with its destination. The
//!   start of each of these nodes is right, so a hand-written scanner reads
//!   the tail the way a reader does, and positions are mapped through line
//!   starts that miss exactly the line endings comrak missed.
//! * Leading link reference definitions shift every inline position of the
//!   paragraph they start; [`guard`] escapes them, so this never meets a
//!   guarded text.

use std::ops::Range;

use std::collections::{HashMap, HashSet};

use comrak::nodes::{AstNode, NodeMath, NodeValue, Sourcepos};
use comrak::{Arena, Options, parse_document};
use markraft_core::{Attrs, attrs};

use crate::guard::leading_whitespace;
pub use crate::guard::{Guarded, guard};
use crate::schema as md;

/// The kinds of textblock whose text is inline Markdown source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// A paragraph. Its text may hold line endings.
    Paragraph,
    /// A heading. One line is an ATX heading's content; text holding a line
    /// ending is read as a setext heading's.
    Heading,
    /// A GFM table cell: one line, split from its row before it is read.
    TableCell,
}

/// What a textblock's text is read against besides itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeriveContext {
    definitions: String,
}

impl DeriveContext {
    /// A context with nothing in it: no reference link resolves.
    pub fn new() -> DeriveContext {
        DeriveContext::default()
    }

    /// Resolve reference links against `source`: the document's link reference
    /// definitions, as written, one or more lines of them.
    pub fn with_definitions(mut self, source: impl Into<String>) -> DeriveContext {
        self.definitions = source.into();
        self
    }

    /// The definitions this context resolves reference links against.
    pub fn definitions(&self) -> &str {
        &self.definitions
    }
}

/// A style a span of inline source spells.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Style {
    /// `**…**`, `__…__` or a paired `<strong>`…`</strong>`.
    Strong,
    /// `*…*`, `_…_` or a paired `<em>`…`</em>`.
    Emphasis,
    /// `~~…~~`, `~…~` or a paired `<del>`…`</del>`.
    Strikethrough,
    /// A code span.
    Code,
    /// An inline, reference or autolink link, or a paired `<a href>`…`</a>`,
    /// with its resolved destination and title.
    Link {
        /// The destination, with escapes and entities resolved.
        href: String,
        /// The title, empty when there is none.
        title: String,
    },
    /// A paired `<u>`…`</u>`.
    Underline,
    /// `==…==` or a paired `<mark>`…`</mark>`.
    Highlight,
    /// `^…^` or a paired `<sup>`…`</sup>`.
    Superscript,
    /// `~…~` or a paired `<sub>`…`</sub>`.
    Subscript,
    /// `[^label]`, where the document defines `label`.
    FootnoteReference {
        /// The label, as written.
        label: String,
    },
    /// A formula: `$…$` and `` $`…`$ `` inline, `$$…$$` display. Like a code
    /// span, nothing inside it is read.
    Math {
        /// Whether it is spelled `$$…$$`.
        display: bool,
    },
}

impl Style {
    /// The name of the schema mark this style is.
    pub fn mark_name(&self) -> &'static str {
        match self {
            Style::Strong => md::STRONG,
            Style::Emphasis => md::EM,
            Style::Strikethrough => md::STRIKETHROUGH,
            Style::Code => md::CODE,
            Style::Link { .. } => md::LINK,
            Style::Underline => md::UNDERLINE,
            Style::Highlight => md::HIGHLIGHT,
            Style::Superscript => md::SUPERSCRIPT,
            Style::Subscript => md::SUBSCRIPT,
            Style::FootnoteReference { .. } => md::FOOTNOTE_REFERENCE,
            Style::Math { .. } => md::MATH,
        }
    }
}

/// One styled node: the characters it spans, delimiters included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSpan {
    /// Character offsets into the text.
    pub range: Range<usize>,
    /// The style.
    pub style: Style,
}

/// A run of characters that is spelling rather than content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conceal {
    /// Character offsets into the text. Never empty.
    pub range: Range<usize>,
    /// Shared by the opening and the closing run of one span; an escape, an
    /// entity and a hard break each have one of their own. Ids are dense and
    /// numbered in the order their first run appears.
    pub span: u32,
    /// What a reader sees in the run's place: the decoded character(s) of an
    /// entity, nothing for every other run.
    pub display: String,
}

/// A run of text a reader takes for something the tree holds as an atom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomSpan {
    /// Character offsets into the text. Never holds an atom placeholder: an
    /// atom already in the tree is never folded into another.
    pub range: Range<usize>,
    /// The schema node type the run stands for.
    pub node_type: &'static str,
    /// The atom's attributes, read as the parser reads them.
    pub attrs: Attrs,
}

/// The styles and spelling of a textblock's text. See the module
/// documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Derived {
    /// Styled nodes, ordered by where they start, outer before inner.
    pub styles: Vec<StyleSpan>,
    /// Concealed runs, ordered by where they start.
    pub conceals: Vec<Conceal>,
    /// Character offsets of the `\n`s that are hard breaks, ascending.
    pub hard_breaks: Vec<usize>,
    /// Text spelling an atom, ordered by where it starts. Styles and conceals
    /// are read with the text as it is; inside one of these nothing is.
    pub atoms: Vec<AtomSpan>,
}

impl Derived {
    /// The styles over the character at `offset`, outermost first.
    pub fn styles_at(&self, offset: usize) -> Vec<&Style> {
        self.styles
            .iter()
            .filter(|span| span.range.contains(&offset))
            .map(|span| &span.style)
            .collect()
    }

    /// The concealed run holding the character at `offset`, if any.
    pub fn conceal_at(&self, offset: usize) -> Option<&Conceal> {
        self.conceals
            .iter()
            .find(|conceal| conceal.range.contains(&offset))
    }
}

/// The styles `text` spells as the inline content of a block of `kind`.
///
/// `text` is the block's inline source with U+FFFC for each opaque atom and
/// `\n` for each line ending. It need not be guarded: the backslashes
/// [`guard`] would add are parsed but never reported, since they are not in
/// `text`.
pub fn derive(kind: BlockKind, text: &str, ctx: &DeriveContext) -> Derived {
    if text.is_empty() {
        return Derived::default();
    }
    if kind == BlockKind::TableCell && text.contains('\n') {
        // A cell is one line of its row; see the guard module. Read each line
        // as a cell of its own rather than as a broken table.
        return derive_lines(text, ctx);
    }
    let guarded = guard(kind, text);
    let mut source = Source::new(kind, &guarded.text, ctx);
    let arena = Arena::new();
    let root = parse_document(&arena, &source.document, &parse_options());
    let mut tails = HashMap::new();
    if source.starts.is_none() {
        source.repair_tails(root, &mut tails);
    }
    let mut reader = Reader {
        source: &source,
        tails,
        text: text.chars().collect(),
        original: original_offsets(&guarded),
        out: Derived::default(),
        spans: 0,
        read_html: HashSet::new(),
    };
    reader.blocks(root);
    if let Some(removed) = &source.removed_backslashes {
        for &byte in removed {
            let range = reader.guarded_range(byte, byte + 1);
            reader.conceal(range, None, String::new());
        }
    }
    let mut out = reader.out;
    out.styles
        .sort_by(|a, b| (a.range.start, b.range.end).cmp(&(b.range.start, a.range.end)));
    out.conceals.sort_by_key(|conceal| conceal.range.start);
    renumber_spans(&mut out.conceals);
    out.hard_breaks.sort_unstable();
    out.atoms.sort_by_key(|atom| atom.range.start);
    out
}

/// Number span ids in the order their first run appears.
fn renumber_spans(conceals: &mut [Conceal]) {
    let mut seen: Vec<u32> = Vec::new();
    for conceal in conceals {
        let id = match seen.iter().position(|old| *old == conceal.span) {
            Some(index) => index,
            None => {
                seen.push(conceal.span);
                seen.len() - 1
            }
        };
        conceal.span = id as u32;
    }
}

fn derive_lines(text: &str, ctx: &DeriveContext) -> Derived {
    let mut out = Derived::default();
    let mut offset = 0;
    for line in text.split('\n') {
        let part = derive(BlockKind::TableCell, line, ctx);
        let base = out.conceals.iter().map(|c| c.span + 1).max().unwrap_or(0);
        let shift = |range: Range<usize>| range.start + offset..range.end + offset;
        out.styles
            .extend(part.styles.into_iter().map(|span| StyleSpan {
                range: shift(span.range),
                style: span.style,
            }));
        out.conceals
            .extend(part.conceals.into_iter().map(|conceal| Conceal {
                range: shift(conceal.range),
                span: conceal.span + base,
                display: conceal.display,
            }));
        out.atoms
            .extend(part.atoms.into_iter().map(|atom| AtomSpan {
                range: shift(atom.range),
                ..atom
            }));
        offset += line.chars().count() + 1;
    }
    out
}

/// The comrak options inline source is read with: the application's GFM
/// preset, with escapes kept as nodes of their own so their positions are
/// known.
pub(crate) fn parse_options() -> Options<'static> {
    let mut options = crate::commonmark_options();
    options.parse.escaped_char_spans = true;
    options
}

/// The document comrak parses for one textblock, and how its positions map
/// back to the guarded text.
struct Source {
    /// What comrak parses.
    document: String,
    /// The string comrak's inline positions index. The same as `document`
    /// except in a table cell, where it holds the cell with its escaped pipes
    /// unescaped, as comrak reads it.
    coordinates: String,
    /// The byte offset in `coordinates` of each line's start.
    line_starts: Vec<usize>,
    /// For each byte of `coordinates`, the byte of the guarded text it is.
    to_guarded: Vec<Option<usize>>,
    /// In a table cell, the guarded bytes of the backslashes comrak removes
    /// before it reads the cell.
    removed_backslashes: Option<Vec<usize>>,
    /// In a table cell, where a node starting at each byte of `coordinates`
    /// starts in the guarded text, when that differs from `to_guarded`.
    starts: Option<Vec<Option<usize>>>,
}

impl Source {
    fn new(kind: BlockKind, guarded: &str, ctx: &DeriveContext) -> Source {
        // comrak does not read a definition whose destination is `<>` when
        // nothing follows it, so the definitions end with a line ending.
        let definitions = if ctx.definitions.trim().is_empty() {
            String::new()
        } else {
            format!("\n\n{}\n", ctx.definitions)
        };
        match kind {
            BlockKind::Paragraph => Source::shifted(guarded, "", "", &definitions),
            BlockKind::Heading if guarded.contains('\n') => {
                Source::shifted(guarded, "", "\n===", &definitions)
            }
            BlockKind::Heading => Source::shifted(guarded, "# ", "", &definitions),
            BlockKind::TableCell => Source::cell(guarded, &definitions),
        }
    }

    /// `prefix`, then the guarded text less the whitespace a reader strips
    /// from its start, then `suffix`.
    fn shifted(guarded: &str, prefix: &str, suffix: &str, definitions: &str) -> Source {
        let lead = leading_whitespace(guarded);
        let document = format!("{prefix}{}{suffix}{definitions}", &guarded[lead..]);
        let mut to_guarded = vec![None; document.len() + 1];
        for byte in lead..guarded.len() {
            to_guarded[byte - lead + prefix.len()] = Some(byte);
        }
        Source {
            line_starts: line_starts(&document),
            coordinates: document.clone(),
            document,
            to_guarded,
            removed_backslashes: None,
            starts: None,
        }
    }

    fn cell(guarded: &str, definitions: &str) -> Source {
        let document = format!("| {guarded} |\n|-|{definitions}");
        let start = guarded.len() - guarded.trim_start_matches(is_cell_space).len();
        let end = guarded.trim_end_matches(is_cell_space).len().max(start);
        let (unescaped, map, removed) = unescape_pipes(&guarded[start..end]);
        // The cell's content starts after `| ` and the whitespace comrak trims.
        let offset = 2 + start;
        let coordinates = format!("{}{unescaped}", " ".repeat(offset));
        let mut to_guarded = vec![None; coordinates.len() + 1];
        let mut starts = vec![None; coordinates.len() + 1];
        for (byte, content) in map.into_iter().enumerate() {
            to_guarded[offset + byte] = Some(start + content);
            // A node that starts at an unescaped pipe starts at its backslash:
            // the two are one escape, and the backslash is its spelling.
            let backslash = content.checked_sub(1).filter(|b| removed.contains(b));
            starts[offset + byte] = Some(start + backslash.unwrap_or(content));
        }
        Source {
            line_starts: vec![0],
            coordinates,
            document,
            to_guarded,
            removed_backslashes: Some(removed.into_iter().map(|byte| start + byte).collect()),
            starts: Some(starts),
        }
    }

    /// The byte of `coordinates` at a comrak line and column.
    fn byte(&self, line: usize, column: usize) -> Option<usize> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        Some(start + column.checked_sub(1)?)
    }

    /// The guarded byte range of a node, from its first byte to one past its
    /// last.
    fn guarded(&self, pos: Sourcepos) -> Option<(usize, usize)> {
        let first = self.byte(pos.start.line, pos.start.column)?;
        let last = self.byte(pos.end.line, pos.end.column)?;
        let starts = self.starts.as_ref().unwrap_or(&self.to_guarded);
        let from = (*starts.get(first)?)?;
        // comrak's end column is the last *byte* of the node.
        let to = (*self.to_guarded.get(last)?)?;
        (to >= from).then_some((from, to + 1))
    }

    /// Where a node starting at byte `at` of `coordinates` starts in the
    /// guarded text.
    fn guarded_start(&self, at: usize) -> Option<usize> {
        let starts = self.starts.as_ref().unwrap_or(&self.to_guarded);
        *starts.get(at)?
    }

    /// The source of a node as comrak read it.
    fn slice(&self, pos: Sourcepos) -> Option<&str> {
        let first = self.byte(pos.start.line, pos.start.column)?;
        let last = self.byte(pos.end.line, pos.end.column)?;
        self.coordinates.get(first..=last)
    }

    /// Find where every link, image and wiki link under `node` really ends,
    /// and bring [`Source::line_starts`] in line with comrak's count of line
    /// endings. See the module documentation.
    ///
    /// Nodes are visited in the order their ends appear — children before
    /// their parent, earlier siblings first — so every position this reads was
    /// already mapped through the line endings comrak missed before it.
    /// `ends` receives the end (one past the last byte of `coordinates`) of
    /// each node whose reported end is wrong, keyed by [`node_key`].
    fn repair_tails<'a>(&mut self, node: &'a AstNode<'a>, ends: &mut HashMap<usize, usize>) {
        for child in node.children() {
            self.repair_tails(child, ends);
            let (value, pos) = {
                let data = child.data.borrow();
                (data.value.clone(), data.sourcepos)
            };
            let swallowed = match value {
                NodeValue::Link(_) | NodeValue::Image(_) => {
                    let image = matches!(value, NodeValue::Image(_));
                    self.link_tail(child, pos, image, ends)
                        .filter(|(_, crossed)| !crossed.is_empty())
                        .map(|(end, crossed)| {
                            ends.insert(node_key(child), end);
                            crossed
                        })
                }
                // A wiki link's own end is reported through the line ending,
                // counted as a column; only what follows it is off.
                NodeValue::WikiLink(_) => {
                    self.byte(pos.start.line, pos.start.column)
                        .and_then(|first| {
                            let last = self.byte(pos.end.line, pos.end.column)?;
                            Some(newlines(&self.coordinates, first, last + 1))
                        })
                }
                _ => None,
            };
            for newline in swallowed.unwrap_or_default() {
                self.line_starts.retain(|start| *start != newline + 1);
            }
        }
    }

    /// One past the last byte of `node` in `coordinates`.
    fn node_end<'a>(&self, node: &'a AstNode<'a>, ends: &HashMap<usize, usize>) -> Option<usize> {
        if let Some(end) = ends.get(&node_key(node)) {
            return Some(*end);
        }
        let data = node.data.borrow();
        let pos = data.sourcepos;
        if let NodeValue::Code(code) = &data.value {
            let start = self.byte(pos.start.line, pos.start.column)?;
            let close = closing_run(
                &self.coordinates,
                start + code.num_backticks,
                code.num_backticks,
            )?;
            return Some(close + code.num_backticks);
        }
        Some(self.byte(pos.end.line, pos.end.column)? + 1)
    }

    /// Where a link's or an image's source really ends, read from the `]`
    /// that closes its text, with the line endings inside what follows it.
    fn link_tail<'a>(
        &self,
        node: &'a AstNode<'a>,
        pos: Sourcepos,
        image: bool,
        ends: &HashMap<usize, usize>,
    ) -> Option<(usize, Vec<usize>)> {
        let start = self.byte(pos.start.line, pos.start.column)?;
        let label_end = match node.last_child() {
            Some(last) => self.node_end(last, ends)?,
            None => start + if image { 2 } else { 1 },
        };
        let bytes = self.coordinates.as_bytes();
        if bytes.get(label_end) != Some(&b']') {
            return None;
        }
        let end = match bytes.get(label_end + 1) {
            Some(b'(') => inline_tail_end(&self.coordinates, label_end + 1)?,
            Some(b'[') => reference_label_end(&self.coordinates, label_end + 1)?,
            _ => label_end + 1,
        };
        Some((end, newlines(&self.coordinates, label_end + 1, end)))
    }
}

/// A key identifying one comrak node for as long as its arena lives.
fn node_key<'a>(node: &'a AstNode<'a>) -> usize {
    std::ptr::from_ref(node) as usize
}

/// The byte offsets of the line endings in `text[from..to]`.
fn newlines(text: &str, from: usize, to: usize) -> Vec<usize> {
    text.get(from..to)
        .map(|slice| slice.match_indices('\n').map(|(at, _)| from + at).collect())
        .unwrap_or_default()
}

/// Skip spaces, tabs and line endings from `at`.
fn skip_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
    {
        at += 1;
    }
    at
}

/// One past the `)` that closes an inline link's `(…)` opening at `open`: a
/// destination, in pointy brackets or with balanced parentheses, then an
/// optional title, with whitespace around each. `None` when the source there
/// is not shaped like one, which leaves comrak's own end in force.
fn inline_tail_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = skip_whitespace(bytes, open + 1);
    if bytes.get(at) == Some(&b'<') {
        at += 1;
        loop {
            match bytes.get(at)? {
                b'\\' => at += 2,
                b'>' => {
                    at += 1;
                    break;
                }
                b'\n' | b'<' => return None,
                _ => at += 1,
            }
        }
    } else {
        let mut depth = 0usize;
        while let Some(&byte) = bytes.get(at) {
            match byte {
                b'\\' => at += 2,
                b' ' | b'\t' | b'\n' => break,
                b'(' => {
                    depth += 1;
                    at += 1;
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    at += 1;
                }
                _ => at += 1,
            }
        }
    }
    at = skip_whitespace(bytes, at);
    if let Some(&quote) = bytes.get(at).filter(|b| matches!(b, b'"' | b'\'' | b'(')) {
        let close = if quote == b'(' { b')' } else { quote };
        at += 1;
        loop {
            match *bytes.get(at)? {
                b'\\' => at += 2,
                byte if byte == close => {
                    at += 1;
                    break;
                }
                _ => at += 1,
            }
        }
        at = skip_whitespace(bytes, at);
    }
    (bytes.get(at) == Some(&b')')).then_some(at + 1)
}

/// One past the `]` that closes a reference label opening at `open`.
fn reference_label_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = open + 1;
    loop {
        match *bytes.get(at)? {
            b'\\' => at += 2,
            b']' => return Some(at + 1),
            b'[' => return None,
            _ => at += 1,
        }
    }
}

fn is_cell_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{b}' | '\u{c}' | '\r' | '\n')
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// comrak's `unescape_pipes`: drop the backslash of every `\|` whose backslash
/// is not itself escaped — the last of an odd run. Returns the result, the byte
/// of `text` each of its bytes came from, and the bytes of `text` dropped.
fn unescape_pipes(text: &str) -> (String, Vec<usize>, Vec<usize>) {
    let mut removed = Vec::new();
    let mut run = 0;
    for (at, character) in text.char_indices() {
        if character == '|' && run % 2 == 1 {
            removed.push(at - 1);
        }
        run = if character == '\\' { run + 1 } else { 0 };
    }
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    for (at, character) in text.char_indices() {
        if removed.contains(&at) {
            continue;
        }
        out.push(character);
        map.extend(at..at + character.len_utf8());
    }
    (out, map, removed)
}

/// For each byte of the guarded text, how many original characters precede
/// it; one more entry for the end.
fn original_offsets(guarded: &Guarded) -> Vec<usize> {
    let mut out = Vec::with_capacity(guarded.text.len() + 1);
    let mut inserted = guarded
        .insertions
        .iter()
        .enumerate()
        .map(|(index, at)| at + index)
        .peekable();
    let mut original = 0;
    for (index, character) in guarded.text.chars().enumerate() {
        let is_inserted = inserted.next_if(|at| *at == index).is_some();
        for _ in 0..character.len_utf8() {
            out.push(original);
        }
        if !is_inserted {
            original += 1;
        }
    }
    out.push(original);
    out
}

struct Reader<'s> {
    source: &'s Source,
    /// Where each link and image comrak reports a wrong end for really ends;
    /// see [`Source::repair_tails`].
    tails: HashMap<usize, usize>,
    /// The original, unguarded text.
    text: Vec<char>,
    /// For each guarded byte, how many original characters precede it.
    original: Vec<usize>,
    out: Derived,
    spans: u32,
    /// The inline HTML nodes read as something other than an atom: a paired
    /// style tag, or a `<br>` spelling the hard break after it. Keyed by
    /// [`node_key`].
    read_html: HashSet<usize>,
}

impl Reader<'_> {
    /// Character offsets in the original text for a guarded byte range.
    fn guarded_range(&self, from: usize, to: usize) -> Range<usize> {
        self.original[from]..self.original[to]
    }

    /// A node's characters in the original text.
    fn range(&self, pos: Sourcepos) -> Option<Range<usize>> {
        let (from, to) = self.source.guarded(pos)?;
        Some(self.guarded_range(from, to))
    }

    fn new_span(&mut self) -> u32 {
        self.spans += 1;
        self.spans - 1
    }

    /// Record a concealed run, under `span` or an id of its own. Runs that
    /// hold no original character — a backslash only the guard added — are
    /// not recorded.
    fn conceal(&mut self, range: Range<usize>, span: Option<u32>, display: String) -> Option<u32> {
        if range.is_empty() {
            return span;
        }
        let span = span.unwrap_or_else(|| self.new_span());
        self.out.conceals.push(Conceal {
            range,
            span,
            display,
        });
        Some(span)
    }

    fn style(&mut self, range: Range<usize>, style: Style) {
        if !range.is_empty() {
            self.out.styles.push(StyleSpan { range, style });
        }
    }

    /// Conceal a span's opening run, from its start to where its content
    /// starts, and its closing run, from where its content ends to its end.
    fn delimiters(&mut self, whole: Range<usize>, content: Range<usize>) {
        let span = self.conceal(whole.start..content.start, None, String::new());
        self.conceal(content.end..whole.end, span, String::new());
    }

    fn blocks<'a>(&mut self, node: &'a AstNode<'a>) {
        for child in node.children() {
            let value = child.data.borrow().value.clone();
            match value {
                NodeValue::Paragraph | NodeValue::Heading(_) | NodeValue::TableCell => {
                    self.inlines(child)
                }
                // The definitions the context adds are not the block's text.
                NodeValue::FootnoteDefinition(_) => {}
                // A code block, an HTML block or a thematic break holds no
                // inline content; a table or a container holds blocks that may.
                _ if value.block() => self.blocks(child),
                _ => {}
            }
        }
    }

    fn inlines<'a>(&mut self, parent: &'a AstNode<'a>) {
        self.html_styles(parent);
        self.html_breaks(parent);
        self.embeds(parent);
        for child in parent.children() {
            self.inline(child);
        }
    }

    fn inline<'a>(&mut self, node: &'a AstNode<'a>) {
        let (value, pos) = {
            let data = node.data.borrow();
            (data.value.clone(), data.sourcepos)
        };
        // A code span's reported end cannot be trusted; see `code`.
        if let NodeValue::Code(code) = &value {
            self.code(pos, code.num_backticks);
            return;
        }
        let Some((from, to)) = self.guarded_node(node, pos) else {
            return;
        };
        let whole = self.guarded_range(from, to);
        match value {
            NodeValue::Text(literal) => self.entities(pos, &literal),
            // The backslash alone; the character it escapes is content. A
            // backslash the guard added is not in the text and conceals
            // nothing.
            NodeValue::Escaped => {
                let backslash = self.guarded_range(from, from + 1);
                self.conceal(backslash, None, String::new());
            }
            NodeValue::LineBreak => self.hard_break(whole),
            NodeValue::Emph => self.styled(node, whole, Style::Emphasis),
            NodeValue::Strong => self.styled(node, whole, Style::Strong),
            NodeValue::Strikethrough => self.styled(node, whole, Style::Strikethrough),
            NodeValue::Highlight => self.styled(node, whole, Style::Highlight),
            NodeValue::Superscript => self.styled(node, whole, Style::Superscript),
            // comrak lets a subscript run across spaces, so the tildes of
            // `~5 to ~10` would make one. Typora and Pandoc need a space in one
            // escaped, and a note's `~` means "about" far more often than it
            // opens a subscript, so such a run is plain text.
            NodeValue::Subscript if spans_whitespace(node) => self.inlines(node),
            NodeValue::Subscript => self.styled(node, whole, Style::Subscript),
            NodeValue::Math(math) => self.math(whole, &math),
            // The `[^` and `]` are spelling; the label is what a reader sees.
            NodeValue::FootnoteReference(reference) if whole.len() >= 3 => {
                let label = reference.name.clone();
                self.style(whole.clone(), Style::FootnoteReference { label });
                self.delimiters(whole.clone(), whole.start + 2..whole.end - 1);
            }
            NodeValue::Link(link) => {
                let style = Style::Link {
                    href: link.url.clone(),
                    title: link.title.clone(),
                };
                self.styled(node, whole, style);
            }
            // An image and a wiki link are atoms in the tree. One spelled in
            // the text is reported as the atom it spells, with nothing styled
            // inside.
            // An image by reference stays the text it is: the atom would write
            // its destination inline and lose the reference.
            NodeValue::Image(_) if self.text.get(whole.end - 1) != Some(&')') => {}
            NodeValue::Image(link) => {
                let attrs = attrs! {
                    "src" => link.url.clone(),
                    "alt" => crate::rules::inline_text(node),
                    "title" => link.title.clone(),
                };
                self.atom(whole, md::IMAGE, attrs);
            }
            NodeValue::WikiLink(_) => {
                let spelled: String = self.text[whole.clone()].iter().collect();
                // A spelling the wiki reader refuses stays the text it is.
                if let Some(link) = crate::wiki::whole_wiki_link(&spelled) {
                    self.atom(whole, md::WIKI_LINK, wiki_attrs(link));
                }
            }
            // Paired style tags and a `<br>` ending its line were read with
            // their parent, and a `<u>` or `</u>` without a partner is half of
            // a style being typed. An `<img>` is the image atom; any other tag
            // is an atom in the tree, holding its source.
            NodeValue::HtmlInline(html) => {
                if self.read_html.contains(&node_key(node)) {
                    return;
                }
                let tag = HtmlTag::read(&html);
                if tag.as_ref().is_some_and(|tag| tag.name == "u") {
                    return;
                }
                let source: String = self.text[whole.clone()].iter().collect();
                match tag.and_then(|tag| tag.image(&source)) {
                    Some(attrs) => self.atom(whole, md::IMAGE, attrs),
                    None => self.atom(whole, md::RAW_INLINE, attrs! {"source" => source}),
                }
            }
            NodeValue::SoftBreak => {}
            _ => {
                for child in node.children() {
                    self.inline(child);
                }
            }
        }
    }

    /// The guarded byte range of a node, with a link's or an image's end
    /// taken from [`Source::repair_tails`] where comrak's is wrong.
    fn guarded_node<'a>(&self, node: &'a AstNode<'a>, pos: Sourcepos) -> Option<(usize, usize)> {
        let Some(&end) = self.tails.get(&node_key(node)) else {
            return self.source.guarded(pos);
        };
        let first = self.source.byte(pos.start.line, pos.start.column)?;
        let from = self.source.guarded_start(first)?;
        let to = (*self.source.to_guarded.get(end - 1)?)?;
        (to >= from).then_some((from, to + 1))
    }

    /// Record text that spells an atom, unless it holds one already.
    fn atom(&mut self, range: Range<usize>, node_type: &'static str, attrs: Attrs) {
        let holds_atom = self.text[range.clone()].contains(&ATOM_PLACEHOLDER);
        if !range.is_empty() && !holds_atom {
            self.out.atoms.push(AtomSpan {
                range,
                node_type,
                attrs,
            });
        }
    }

    /// Record the `![[…]]` embeds and the `:shortcode:` emoji among
    /// `parent`'s children.
    ///
    /// comrak has no embed syntax: the `!` opens an image label, which stops
    /// the wiki link inside from being seen, so the whole run arrives as text —
    /// split wherever an escape stands. Nor does it read shortcodes here; see
    /// [`crate::shortcode`]. Each run of text and escapes is read as the source
    /// it covers, and only an embed or a shortcode that lies wholly inside one
    /// is one. The text of an autolink is its destination, where a colon is
    /// the URL's.
    fn embeds<'a>(&mut self, parent: &'a AstNode<'a>) {
        let shortcodes = !self.is_autolink(parent);
        let mut run: Option<(usize, usize)> = None;
        for child in parent.children() {
            let (textual, pos) = {
                let data = child.data.borrow();
                (
                    matches!(data.value, NodeValue::Text(_) | NodeValue::Escaped),
                    data.sourcepos,
                )
            };
            let bytes = self
                .source
                .byte(pos.start.line, pos.start.column)
                .and_then(|first| {
                    let last = self.source.byte(pos.end.line, pos.end.column)?;
                    Some((first, last + 1))
                });
            match (textual, bytes, run) {
                (true, Some((first, end)), Some((from, to))) if first == to => {
                    run = Some((from, end));
                }
                (true, Some(bytes), _) => {
                    if let Some(run) = run {
                        self.embeds_in(run, shortcodes);
                    }
                    run = Some(bytes);
                }
                _ => {
                    if let Some(run) = run.take() {
                        self.embeds_in(run, shortcodes);
                    }
                }
            }
        }
        if let Some(run) = run {
            self.embeds_in(run, shortcodes);
        }
    }

    /// Whether `node` is a link written as its own destination: a GFM or an
    /// angle-bracket autolink rather than one with a `[label]`.
    fn is_autolink<'a>(&self, node: &'a AstNode<'a>) -> bool {
        let data = node.data.borrow();
        matches!(data.value, NodeValue::Link(_))
            && self
                .source
                .byte(data.sourcepos.start.line, data.sourcepos.start.column)
                .and_then(|byte| self.source.coordinates.as_bytes().get(byte))
                .is_some_and(|byte| *byte != b'[')
    }

    fn embeds_in(&mut self, (first, end): (usize, usize), shortcodes: bool) {
        let Some(slice) = self.source.coordinates.get(first..end) else {
            return;
        };
        let opens = |c: char| c == '!' || (shortcodes && c == ':');
        let wanted = slice.contains("![[") || (shortcodes && slice.contains(':'));
        if !wanted {
            return;
        }
        let slice = slice.to_string();
        let mut at = 0;
        while let Some(offset) = slice[at..].find(opens).map(|index| at + index) {
            // An escaped `\!` is not an embed's, nor an escaped `\:` a
            // shortcode's.
            let escaped = slice[..offset]
                .chars()
                .rev()
                .take_while(|c| *c == '\\')
                .count()
                % 2
                == 1;
            let rest = &slice[offset..];
            let found = if escaped {
                None
            } else if rest.starts_with("![[") {
                crate::wiki::read_wiki_link(rest)
                    .map(|(link, len)| (len, md::WIKI_LINK, wiki_attrs(link)))
            } else if rest.starts_with(':') {
                crate::shortcode::read_shortcode(rest)
                    .map(|(name, len)| (len, md::EMOJI, attrs! {"code" => name.to_string()}))
            } else {
                None
            };
            match found {
                Some((len, node_type, attrs)) => {
                    if let Some((from, to)) =
                        self.coordinate_range(first + offset, first + offset + len)
                    {
                        let range = self.guarded_range(from, to);
                        self.atom(range, node_type, attrs);
                    }
                    at = offset + len;
                }
                None => at = offset + 1,
            }
        }
    }

    /// A node whose content is inline children between two delimiter runs.
    fn styled<'a>(&mut self, node: &'a AstNode<'a>, whole: Range<usize>, style: Style) {
        let content = self.children_range(node).unwrap_or_else(|| {
            // Only a link can be empty, `[](…)`: its opening run is the `[`.
            whole.start + 1..whole.start + 1
        });
        self.style(whole.clone(), style);
        self.delimiters(whole, content);
        self.inlines(node);
    }

    /// The characters from the start of `node`'s first child to the end of
    /// its last.
    fn children_range<'a>(&self, node: &'a AstNode<'a>) -> Option<Range<usize>> {
        let first = self.node_range(node.first_child()?)?;
        let last = self.node_range(node.last_child()?)?;
        Some(first.start..last.end)
    }

    /// A node's characters in the original text, a code span's found the way
    /// [`Reader::code`] finds it.
    fn node_range<'a>(&self, node: &'a AstNode<'a>) -> Option<Range<usize>> {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Code(code) => {
                let (_, from, to) = self.code_bytes(data.sourcepos, code.num_backticks)?;
                Some(self.guarded_range(from, to))
            }
            _ => {
                let (from, to) = self.guarded_node(node, data.sourcepos)?;
                Some(self.guarded_range(from, to))
            }
        }
    }

    /// Where a code span's closing backticks start in `coordinates`, and its
    /// guarded byte range.
    ///
    /// comrak reports the end of a code span that crosses a line ending
    /// against the indentation of the wrong line (see the module
    /// documentation), so the end is found the way a reader finds it: the
    /// next run of exactly as many backticks after the start, which is right.
    fn code_bytes(&self, pos: Sourcepos, ticks: usize) -> Option<(usize, usize, usize)> {
        let start = self.source.byte(pos.start.line, pos.start.column)?;
        let close = closing_run(&self.source.coordinates, start + ticks, ticks)?;
        let from = self.source.guarded_start(start)?;
        let to = (*self.source.to_guarded.get(close + ticks - 1)?)?;
        Some((close, from, to + 1))
    }

    fn code(&mut self, pos: Sourcepos, ticks: usize) {
        let Some(start) = self.source.byte(pos.start.line, pos.start.column) else {
            return;
        };
        let Some((close, from, to)) = self.code_bytes(pos, ticks) else {
            return;
        };
        let end = close + ticks;
        let text = &self.source.coordinates;
        self.style(self.guarded_range(from, to), Style::Code);
        // A reader removes the indentation of each paragraph line, turns line
        // endings into spaces, and then strips one space from each side when
        // both sides have one and the content is not all spaces. Those spaces
        // are spelling, like the backticks, and so is indentation next to one.
        let content = start + ticks..close;
        let raw: String = strip_indentation(&text[content.clone()]).replace('\n', " ");
        let padded = raw.len() >= 2
            && raw.starts_with(' ')
            && raw.ends_with(' ')
            && raw.chars().any(|c| c != ' ');
        let (mut open_end, mut close_start) = (content.start, content.end);
        if padded {
            open_end += 1;
            if text.as_bytes()[content.start] == b'\n' {
                open_end += leading_whitespace(&text[open_end..content.end]);
            }
            let indented = text[..content.end].trim_end_matches([' ', '\t']).len();
            close_start = if indented > content.start && text.as_bytes()[indented - 1] == b'\n' {
                indented - 1
            } else {
                content.end - 1
            };
        }
        // Backticks, padding and indentation are single bytes, and no
        // backslash the guard or a cell's pipes add sits among them.
        let open = self.guarded_range(from, from + (open_end - start));
        let close = self.guarded_range(to - (end - close_start), to);
        let span = self.conceal(open, None, String::new());
        self.conceal(close, span, String::new());
    }

    /// A formula: styled over its whole source, its fences concealed as one
    /// span, its content left as the TeX it is.
    ///
    /// The fences are `$` or `$$` for dollar math and `` $` `` and `` `$ `` for
    /// code math. They are single-byte characters no guard backslash sits
    /// among, and comrak's end for a formula across lines is right.
    fn math(&mut self, whole: Range<usize>, math: &NodeMath) {
        let fence = if math.display_math || !math.dollar_math {
            2
        } else {
            1
        };
        if whole.len() < fence * 2 {
            return;
        }
        let display = math.display_math;
        self.style(whole.clone(), Style::Math { display });
        let content = whole.start + fence..whole.end - fence;
        self.delimiters(whole, content);
    }

    fn hard_break(&mut self, whole: Range<usize>) {
        // The node runs to the line ending, and for a spelling of more than
        // two spaces starts on the last two: every space before the line
        // ending is spelling. See the module documentation.
        let newline = whole.end - 1;
        let mut start = whole.start;
        while start > 0 && matches!(self.text[start - 1], ' ' | '\t') {
            start -= 1;
        }
        self.conceal(start..newline, None, String::new());
        self.out.hard_breaks.push(newline);
    }

    /// Conceal the character references in a text node's source.
    ///
    /// comrak has already resolved them into `literal`; walking the source and
    /// the literal together finds where. Backslash escapes are nodes of their
    /// own and never meet this.
    fn entities(&mut self, pos: Sourcepos, literal: &str) {
        let Some(slice) = self.source.slice(pos) else {
            return;
        };
        if slice == literal {
            return;
        }
        let Some(first) = self.source.byte(pos.start.line, pos.start.column) else {
            return;
        };
        let (mut read, mut written) = (0, 0);
        while read < slice.len() {
            if let Some(len) = entity_len(&slice[read..])
                && let Some(decoded) = decode_entity(&slice[read..read + len])
                && literal[written..].starts_with(&decoded)
            {
                if let Some((from, to)) = self.coordinate_range(first + read, first + read + len) {
                    let range = self.guarded_range(from, to);
                    self.conceal(range, None, decoded.clone());
                }
                read += len;
                written += decoded.len();
                continue;
            }
            let Some(character) = slice[read..].chars().next() else {
                break;
            };
            if !literal[written..].starts_with(character) {
                // The two no longer line up; nothing after this is certain.
                break;
            }
            read += character.len_utf8();
            written += character.len_utf8();
        }
    }

    /// The guarded byte range for a range of `coordinates` bytes.
    fn coordinate_range(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let start = (*self.source.to_guarded.get(from)?)?;
        let last = (*self.source.to_guarded.get(to - 1)?)?;
        Some((start, last + 1))
    }

    /// Style every pair of style tags among `parent`'s children: `<u>`,
    /// `<em>`, `<strong>`, `<del>` and `<a href>`, each closed by its own
    /// closing tag.
    ///
    /// Only tags that pair up within one parent are a style: `<u>*a</u>*` has
    /// its tags in different nodes, and guessing a tree for them would change
    /// what the source says. The tags themselves are the span's delimiters. A
    /// closing tag pairs with the latest unpaired opening tag of its name.
    fn html_styles<'a>(&mut self, parent: &'a AstNode<'a>) {
        let mut open: Vec<(String, Style, Range<usize>)> = Vec::new();
        let mut paired: Vec<usize> = Vec::new();
        let mut openers: Vec<usize> = Vec::new();
        for child in parent.children() {
            let (value, pos) = {
                let data = child.data.borrow();
                (data.value.clone(), data.sourcepos)
            };
            let NodeValue::HtmlInline(html) = value else {
                continue;
            };
            let (Some(tag), Some(range)) = (HtmlTag::read(&html), self.range(pos)) else {
                continue;
            };
            if tag.closing {
                let Some(at) = open.iter().rposition(|(name, ..)| *name == tag.name) else {
                    continue;
                };
                let (_, style, opening) = open.remove(at);
                paired.push(openers.remove(at));
                paired.push(node_key(child));
                self.style(opening.start..range.end, style);
                let span = self.conceal(opening, None, String::new());
                self.conceal(range, span, String::new());
            } else if let Some(style) = tag.style() {
                open.push((tag.name, style, range));
                openers.push(node_key(child));
            }
        }
        self.read_html.extend(paired);
    }

    /// Read every `<br>` among `parent`'s children that ends its line as the
    /// spelling of that line's hard break, the way a backslash would be.
    ///
    /// One in the middle of a line has no line ending in the text to spell,
    /// and stays the atom it is.
    fn html_breaks<'a>(&mut self, parent: &'a AstNode<'a>) {
        for child in parent.children() {
            let (value, pos) = {
                let data = child.data.borrow();
                (data.value.clone(), data.sourcepos)
            };
            let NodeValue::HtmlInline(html) = value else {
                continue;
            };
            let is_break = HtmlTag::read(&html).is_some_and(|tag| tag.name == "br" && !tag.closing);
            let soft_break_follows = child
                .next_sibling()
                .is_some_and(|next| matches!(next.data.borrow().value, NodeValue::SoftBreak));
            let Some(range) = self.range(pos).filter(|_| is_break && soft_break_follows) else {
                continue;
            };
            let mut newline = range.end;
            while matches!(self.text.get(newline), Some(' ' | '\t')) {
                newline += 1;
            }
            if self.text.get(newline) != Some(&'\n') {
                continue;
            }
            self.conceal(range.start..newline, None, String::new());
            self.out.hard_breaks.push(newline);
            self.read_html.insert(node_key(child));
        }
    }
}

/// An inline HTML tag, read far enough to tell what it stands for.
struct HtmlTag {
    /// The element name, in lower case.
    name: String,
    closing: bool,
    /// Whether the tag closes itself, `<br/>`.
    empty: bool,
    /// Its attributes as written, values with their character references
    /// resolved. A name without a value has an empty one.
    attrs: Vec<(String, String)>,
}

impl HtmlTag {
    /// `html` read as an opening or closing tag. A comment, a processing
    /// instruction or a declaration is none.
    fn read(html: &str) -> Option<HtmlTag> {
        let body = html.strip_prefix('<')?.strip_suffix('>')?;
        let (body, closing) = match body.strip_prefix('/') {
            Some(rest) => (rest, true),
            None => (body, false),
        };
        let (body, empty) = match body.strip_suffix('/') {
            Some(rest) => (rest, true),
            None => (body, false),
        };
        let name_len = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(body.len());
        let name = &body[..name_len];
        if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let attrs = read_attributes(&body[name_len..])?;
        if closing && (empty || !attrs.is_empty()) {
            return None;
        }
        Some(HtmlTag {
            name: name.to_ascii_lowercase(),
            closing,
            empty,
            attrs,
        })
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(attr, _)| attr.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The style an opening tag starts, when it is a style tag.
    fn style(&self) -> Option<Style> {
        if self.closing || self.empty {
            return None;
        }
        Some(match self.name.as_str() {
            "u" => Style::Underline,
            "em" => Style::Emphasis,
            "strong" => Style::Strong,
            "del" => Style::Strikethrough,
            "mark" => Style::Highlight,
            "sup" => Style::Superscript,
            "sub" => Style::Subscript,
            "a" => Style::Link {
                href: self.attr("href")?.to_string(),
                title: self.attr("title").unwrap_or_default().to_string(),
            },
            _ => return None,
        })
    }

    /// The image atom's attributes, when this is an `<img>` with a source.
    /// `source` is the tag as written, which the atom writes back.
    fn image(&self, source: &str) -> Option<Attrs> {
        if self.name != "img" || self.closing {
            return None;
        }
        Some(attrs! {
            "src" => self.attr("src")?.to_string(),
            "alt" => self.attr("alt").unwrap_or_default().to_string(),
            "title" => self.attr("title").unwrap_or_default().to_string(),
            "source" => source.to_string(),
        })
    }
}

/// The attributes of a tag, from what follows its name. `None` when that is
/// not a run of attributes.
fn read_attributes(mut rest: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', '\n']);
        if trimmed.is_empty() {
            return Some(out);
        }
        // An attribute is separated from what precedes it by whitespace.
        if trimmed.len() == rest.len() {
            return None;
        }
        let name_len = trimmed
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')))
            .unwrap_or(trimmed.len());
        if name_len == 0 {
            return None;
        }
        let name = trimmed[..name_len].to_string();
        let after = trimmed[name_len..].trim_start_matches([' ', '\t', '\n']);
        let Some(value) = after.strip_prefix('=') else {
            out.push((name, String::new()));
            rest = &trimmed[name_len..];
            continue;
        };
        let value = value.trim_start_matches([' ', '\t', '\n']);
        let (raw, remainder) = match value.chars().next()? {
            quote @ ('"' | '\'') => {
                let close = value[1..].find(quote)? + 1;
                (&value[1..close], &value[close + 1..])
            }
            _ => {
                let end = value
                    .find(|c: char| c.is_whitespace() || "\"'=<>`".contains(c))
                    .unwrap_or(value.len());
                (&value[..end], &value[end..])
            }
        };
        out.push((name, decode_entities(raw)));
        rest = remainder;
    }
}

/// `text` with every character reference in it resolved.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some(len) = entity_len(&text[at..])
            && let Some(decoded) = decode_entity(&text[at..at + len])
        {
            out.push_str(&decoded);
            at += len;
            continue;
        }
        let character = text[at..].chars().next().unwrap_or_default();
        out.push(character);
        at += character.len_utf8();
    }
    out
}

/// The character an atom stands as in a textblock's text.
const ATOM_PLACEHOLDER: char = '\u{fffc}';

/// A wiki link atom's attributes.
fn wiki_attrs(link: crate::wiki::WikiLink) -> Attrs {
    attrs! {
        "target" => link.target,
        "alias" => link.alias,
        "embed" => link.embed,
    }
}

/// `text` without the spaces and tabs that start each of its lines after the
/// first.
fn strip_indentation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(line.trim_start_matches([' ', '\t']));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The byte offset of the first run of exactly `ticks` backticks in `text`
/// from `from` on.
fn closing_run(text: &str, from: usize, ticks: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let run = bytes[at..].iter().take_while(|byte| **byte == b'`').count();
        if run == ticks {
            return Some(at);
        }
        at += run;
    }
    None
}

/// The byte length of the character reference `text` starts with, by shape
/// alone: `&name;`, `&#digits;` or `&#xhex;`.
fn entity_len(text: &str) -> Option<usize> {
    let rest = text.strip_prefix('&')?;
    let (body, valid, max): (&str, fn(char) -> bool, usize) =
        if let Some(hex) = rest.strip_prefix("#x").or_else(|| rest.strip_prefix("#X")) {
            (hex, |c| c.is_ascii_hexdigit(), 6)
        } else if let Some(decimal) = rest.strip_prefix('#') {
            (decimal, |c| c.is_ascii_digit(), 7)
        } else {
            (rest, |c| c.is_ascii_alphanumeric(), 32)
        };
    let taken = body.chars().take_while(|c| valid(*c)).count();
    ((1..=max).contains(&taken) && body[taken..].starts_with(';'))
        .then(|| text.len() - body.len() + taken + 1)
}

/// What a character reference decodes to, when it is one.
///
/// comrak's entity table is private, so comrak is asked directly: a lone
/// reference is a paragraph whose text is what it decodes to, and a name that
/// is not in the table reads back as itself.
fn decode_entity(reference: &str) -> Option<String> {
    let arena = Arena::new();
    let root = parse_document(&arena, reference, &Options::default());
    let paragraph = root.first_child()?;
    let text = paragraph.first_child()?;
    let decoded = match &text.data.borrow().value {
        NodeValue::Text(literal) => literal.to_string(),
        _ => return None,
    };
    (decoded != reference).then_some(decoded)
}

#[cfg(test)]
mod tests;

/// Whether an inline node's content holds whitespace or a line ending.
fn spans_whitespace<'a>(node: &'a AstNode<'a>) -> bool {
    node.descendants()
        .any(|child| match &child.data.borrow().value {
            NodeValue::Text(text) => text.chars().any(char::is_whitespace),
            NodeValue::SoftBreak | NodeValue::LineBreak => true,
            _ => false,
        })
}
