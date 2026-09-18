//! CommonMark import. comrak parses the source and this adapter flattens its tree onto
//! the flat block model: one physical source line is one block, blank lines are kept as
//! empty blocks, and anything the model cannot hold stays literal source text. comrak
//! types do not leave this module.

use comrak::nodes::{AstNode, ListType, NodeCodeBlock, NodeValue, Sourcepos};
use comrak::{Arena, Options, parse_document};

use crate::{Block, BlockKind, Document, Mark, Marks, Span, push_linked_span, push_span};

pub(super) fn document(source: &str) -> Document {
    import(source, false)
}

pub(super) fn fragment(source: &str) -> Document {
    import(source, true)
}

fn import(source: &str, preserve_trailing_space: bool) -> Document {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let arena = Arena::new();
    let root = parse_document(&arena, &normalized, &options());
    let lines: Vec<&str> = normalized.split('\n').collect();
    let end = lines.len();
    let mut import = Import {
        lines,
        blocks: Vec::new(),
        next_line: 1,
        preserve_trailing_space,
    };
    import.children(root, &Context::root(), end);
    let mut document = Document {
        blocks: import.blocks,
    };
    document.normalize();
    document
}

/// Only the extensions the model has a representation for. Setext headings are ignored
/// because the model cannot write one back and `---` under a paragraph is a divider here;
/// front matter is stripped before the codec sees the text.
fn options<'c>() -> Options<'c> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.parse.ignore_setext = true;
    options
}

struct Import<'s> {
    lines: Vec<&'s str>,
    blocks: Vec<Block>,
    /// The first source line, counting from one, that no block covers yet.
    next_line: usize,
    preserve_trailing_space: bool,
}

/// What the containers around a leaf make of it. The model carries a single `depth`, so a
/// stack mixing quotes and lists has no representation: `literal_from` then holds the
/// source column where the first unrepresentable container opened, and every leaf below it
/// keeps the source text from that column on.
#[derive(Clone)]
struct Context {
    family: Family,
    /// The kind the containers give a paragraph inside them.
    kind: BlockKind,
    depth: u8,
    literal_from: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Root,
    Quote,
    List,
}

impl Context {
    fn root() -> Self {
        Self {
            family: Family::Root,
            kind: BlockKind::Paragraph,
            depth: 0,
            literal_from: None,
        }
    }

    fn block(&self) -> (BlockKind, u8) {
        (self.kind.clone(), self.depth)
    }

    /// Descend into a container: one level deeper for a container of the same family, and
    /// literal source text for one the model cannot nest inside the current stack.
    fn enter(&self, family: Family, kind: BlockKind, column: usize) -> Self {
        if self.literal_from.is_some() {
            return self.clone();
        }
        match self.family {
            Family::Root => Self {
                family,
                kind,
                depth: 0,
                literal_from: None,
            },
            current if current == family => Self {
                family,
                kind,
                depth: self.depth.saturating_add(1),
                literal_from: None,
            },
            _ => Self {
                literal_from: Some(column),
                ..self.clone()
            },
        }
    }

    /// Re-kind a list item without changing the nesting its list established.
    fn with_kind(&self, kind: BlockKind) -> Self {
        if self.literal_from.is_some() {
            return self.clone();
        }
        Self {
            kind,
            ..self.clone()
        }
    }
}

impl<'s> Import<'s> {
    /// A source line by its one-based number; the borrow outlives `self` so callers can
    /// keep slicing while they push blocks.
    fn line(&self, number: usize) -> &'s str {
        self.lines
            .get(number.wrapping_sub(1))
            .copied()
            .unwrap_or("")
    }

    /// Emit every child of a container, then the lines it holds that no child covered.
    fn children<'a>(&mut self, node: &'a AstNode<'a>, context: &Context, end_line: usize) {
        for child in node.children() {
            let start = child.data.borrow().sourcepos.start.line;
            self.fill(start, context);
            self.block(child, context);
        }
        self.fill(end_line + 1, context);
    }

    fn block<'a>(&mut self, node: &'a AstNode<'a>, context: &Context) {
        let ast = node.data.borrow();
        let sourcepos = ast.sourcepos;
        match &ast.value {
            NodeValue::Paragraph => self.inline_block(node, context, None),
            NodeValue::Heading(heading) => {
                self.inline_block(node, context, Some(BlockKind::Heading(heading.level)));
            }
            NodeValue::CodeBlock(code) => self.code(code, sourcepos, context),
            NodeValue::ThematicBreak => {
                if context.literal_from.is_some() {
                    self.literal_source(sourcepos, context);
                } else {
                    self.blocks.push(Block {
                        kind: BlockKind::Divider,
                        depth: 0,
                        spans: Vec::new(),
                    });
                    self.consume(sourcepos.end.line);
                }
            }
            NodeValue::BlockQuote => {
                let inner = context.enter(Family::Quote, BlockKind::Quote, sourcepos.start.column);
                self.children(node, &inner, sourcepos.end.line);
            }
            NodeValue::List(list) => {
                let inner = context.enter(
                    Family::List,
                    list_kind(list.list_type),
                    sourcepos.start.column,
                );
                self.children(node, &inner, sourcepos.end.line);
            }
            NodeValue::Item(list) => self.item(node, context, list_kind(list.list_type), sourcepos),
            NodeValue::TaskItem(task) => {
                let kind = BlockKind::Task {
                    checked: task.symbol.is_some(),
                };
                self.item(node, context, kind, sourcepos);
            }
            // The literal is already free of the indentation its containers added.
            NodeValue::HtmlBlock(html) => self.literal_text(&html.literal, sourcepos, context),
            // Constructs the enabled options cannot produce, and any later addition: keep
            // the source so nothing is silently dropped.
            _ => self.literal_source(sourcepos, context),
        }
    }

    /// A list item keeps its list's nesting and lends its own kind to the lines inside it.
    /// An item with no content is still a line of the document.
    fn item<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        context: &Context,
        kind: BlockKind,
        sourcepos: Sourcepos,
    ) {
        let inner = context.with_kind(kind);
        let content = node
            .first_child()
            .map(|child| child.data.borrow().sourcepos.start.line);
        // Reference definitions are absent from the AST. Preserve any source on the
        // marker line before consuming it, even when no child covers that line.
        if content != Some(sourcepos.start.line) {
            let (kind, depth) = inner.block();
            let text = slice(
                self.line(sourcepos.start.line),
                inner.literal_from.unwrap_or(sourcepos.start.column),
                usize::MAX,
            );
            let text = if inner.literal_from.is_some() {
                text
            } else {
                item_text(text, matches!(kind, BlockKind::Task { .. }))
            };
            self.plain(kind, depth, text);
            self.consume(sourcepos.start.line);
        }
        self.children(node, &inner, sourcepos.end.line);
    }

    /// A leaf holding inlines, split into one block per source line. Its own kind wins over
    /// the containers', which is why a heading inside a quote loses the quote.
    fn inline_block<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        context: &Context,
        own_kind: Option<BlockKind>,
    ) {
        let sourcepos = node.data.borrow().sourcepos;
        if context.literal_from.is_some() && own_kind.is_some() {
            return self.literal_source(sourcepos, context);
        }
        // Comrak removes a leading reference definition from a paragraph without
        // updating its inline source positions. Preserve that paragraph as source rather
        // than attach its remaining inlines to the definition's line and discard text.
        let first_line = slice(
            self.line(sourcepos.start.line),
            sourcepos.start.column,
            usize::MAX,
        );
        if own_kind.is_none() && is_reference_definition(first_line) {
            return self.literal_paragraph(sourcepos, context);
        }
        let mut segments = inlines(&self.lines, node);
        // A paragraph at the top level keeps the indentation of each of its lines: the
        // model has nowhere else to put it and the editor lets the user type it. Inside a
        // container the indentation belongs to the container, not to the text.
        let indented =
            own_kind.is_none() && context.family == Family::Root && context.literal_from.is_none();
        for (offset, spans) in segments.iter_mut().enumerate() {
            let text = self.line((sourcepos.start.line + offset).min(sourcepos.end.line));
            let prefix = match context.literal_from {
                // The markers of the containers the model cannot nest stay as text.
                Some(column) if offset == 0 => slice(text, column, sourcepos.start.column),
                Some(_) => "",
                None if indented => {
                    &text[..text.len() - text.trim_start_matches([' ', '\t']).len()]
                }
                None => "",
            };
            if !prefix.is_empty() {
                let mut prefixed = Vec::new();
                push_span(&mut prefixed, prefix, Marks::default());
                prefixed.append(spans);
                *spans = prefixed;
            }
        }
        // Pasted text must keep a final separator before existing text. Only paragraph
        // content loses these spaces: heading markers and fence padding are syntax.
        if self.preserve_trailing_space
            && own_kind.is_none()
            && sourcepos.end.line == self.lines.len()
        {
            let text = self.line(sourcepos.end.line);
            let trailing = &text[text.trim_end_matches([' ', '\t']).len()..];
            if let Some(spans) = segments.last_mut() {
                push_span(spans, trailing, Marks::default());
            }
        }
        for (offset, spans) in segments.into_iter().enumerate() {
            let line = (sourcepos.start.line + offset).min(sourcepos.end.line);
            let continued = offset > 0 && own_kind.is_none();
            if continued
                && let Some((kind, depth)) = self.empty_item(line, context, sourcepos.start.column)
            {
                self.blocks.push(Block {
                    kind,
                    depth,
                    spans: Vec::new(),
                });
                continue;
            }
            let (kind, depth) = match &own_kind {
                Some(kind) => (kind.clone(), 0),
                None if continued => self.continuation(line, context, sourcepos.start.column),
                None => context.block(),
            };
            self.blocks.push(Block { kind, depth, spans });
        }
        self.consume(sourcepos.end.line);
    }

    /// A continuation line holding nothing but a list marker. CommonMark will not let an
    /// empty item interrupt a paragraph, but one line is one block here and an empty item
    /// is how the editor writes a list item nobody has typed into yet.
    fn empty_item(
        &self,
        line: usize,
        context: &Context,
        content_column: usize,
    ) -> Option<(BlockKind, u8)> {
        if context.family == Family::Quote || context.literal_from.is_some() {
            return None;
        }
        let text = self.line(line);
        let kind = marker_kind(text.trim_start())?;
        let depth = match context.family {
            Family::List if indent_width(text) + 1 >= content_column => {
                context.depth.saturating_add(1)
            }
            Family::List => context.depth,
            _ => 0,
        };
        Some((kind, depth))
    }

    /// The context a continuation line belongs to. CommonMark reads a line that repeats
    /// none of its container's markers as part of the block above it (lazy continuation);
    /// one line is one block here, so the line's own prefix decides what it is.
    fn continuation(
        &self,
        line: usize,
        context: &Context,
        content_column: usize,
    ) -> (BlockKind, u8) {
        let text = self.line(line);
        match context.family {
            Family::Root => (BlockKind::Paragraph, 0),
            Family::Quote => match quote_markers(text).0 {
                0 => (BlockKind::Paragraph, 0),
                markers => (BlockKind::Quote, context.depth.min(markers as u8 - 1)),
            },
            Family::List if indent_width(text) + 1 >= content_column => context.block(),
            Family::List => (BlockKind::Paragraph, 0),
        }
    }

    /// One block per code line. An empty fenced block is a single empty code line.
    fn code(&mut self, code: &NodeCodeBlock, sourcepos: Sourcepos, context: &Context) {
        if context.literal_from.is_some() {
            return self.literal_source(sourcepos, context);
        }
        let kind = BlockKind::Code {
            language: code.info.split_whitespace().next().unwrap_or("").to_owned(),
        };
        let literal = code.literal.strip_suffix('\n').unwrap_or(&code.literal);
        for text in literal.split('\n') {
            self.plain(kind.clone(), 0, text);
        }
        self.consume(sourcepos.end.line);
    }

    /// Text the model has no structure for, already free of container indentation.
    fn literal_text(&mut self, text: &str, sourcepos: Sourcepos, context: &Context) {
        let (kind, depth) = context.block();
        let text = text.strip_suffix('\n').unwrap_or(text);
        for line in text.split('\n') {
            self.plain(kind.clone(), depth, line);
        }
        self.consume(sourcepos.end.line);
    }

    /// The source a node covers, from the column its first unrepresentable container
    /// opened at, one block per line.
    fn literal_source(&mut self, sourcepos: Sourcepos, context: &Context) {
        let (kind, depth) = context.block();
        let from = context.literal_from.unwrap_or(sourcepos.start.column);
        for line in sourcepos.start.line..=sourcepos.end.line {
            let text = slice(self.line(line), from, usize::MAX);
            self.plain(kind.clone(), depth, text);
        }
        self.consume(sourcepos.end.line);
    }

    /// A paragraph may have lazy continuation lines without its opening indentation or
    /// container markers. Remove only prefixes actually present on each source line.
    fn literal_paragraph(&mut self, sourcepos: Sourcepos, context: &Context) {
        let from = context.literal_from.unwrap_or(sourcepos.start.column);
        for line in sourcepos.start.line..=sourcepos.end.line {
            let source = self.line(line);
            let first = line == sourcepos.start.line;
            let (kind, depth) = if first {
                context.block()
            } else {
                self.continuation(line, context, sourcepos.start.column)
            };
            let text = match context.family {
                Family::Root => source,
                _ if first => slice(source, from, usize::MAX),
                Family::Quote => &source[quote_markers(source).1..],
                Family::List if kind != BlockKind::Paragraph => {
                    let padding = source
                        .bytes()
                        .take(from.saturating_sub(1))
                        .take_while(|byte| matches!(byte, b' ' | b'\t'))
                        .count();
                    &source[padding..]
                }
                Family::List => source,
            };
            self.plain(kind, depth, text);
        }
        self.consume(sourcepos.end.line);
    }

    /// Lines no node covers: blank ones, which stay as empty blocks, and whatever the
    /// parser drops, such as a link reference definition, which stays literal.
    fn fill(&mut self, up_to: usize, context: &Context) {
        while self.next_line < up_to.min(self.lines.len() + 1) {
            let source = self.line(self.next_line);
            let (kind, depth, text) = match context.family {
                Family::Quote => match quote_markers(source) {
                    (0, _) => (BlockKind::Paragraph, 0, source),
                    (markers, offset) => (
                        BlockKind::Quote,
                        context.depth.min(markers as u8 - 1),
                        &source[offset..],
                    ),
                },
                _ => (BlockKind::Paragraph, 0, source.trim_start()),
            };
            let text = if self.preserve_trailing_space
                && self.next_line == self.lines.len()
                && context.family == Family::Root
                && source.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
            {
                source
            } else if text.trim().is_empty() {
                ""
            } else {
                text
            };
            self.plain(kind, depth, text);
            self.next_line += 1;
        }
    }

    fn plain(&mut self, kind: BlockKind, depth: u8, text: &str) {
        let mut spans = Vec::new();
        push_span(&mut spans, text, Marks::default());
        self.blocks.push(Block { kind, depth, spans });
    }

    fn consume(&mut self, end_line: usize) {
        self.next_line = self.next_line.max(end_line + 1);
    }
}

fn list_kind(list_type: ListType) -> BlockKind {
    match list_type {
        ListType::Ordered => BlockKind::Ordered,
        _ => BlockKind::Bullet,
    }
}

fn is_reference_definition(line: &str) -> bool {
    if !line.starts_with('[') {
        return false;
    }
    let arena = Arena::new();
    parse_document(&arena, line, &options())
        .first_child()
        .is_none()
}

/// The content after the marker of an already parsed list item.
fn item_text(text: &str, task: bool) -> &str {
    let marker_end = if text.starts_with(['-', '*', '+']) {
        1
    } else {
        text.bytes().take_while(u8::is_ascii_digit).count() + 1
    };
    let content = text
        .get(marker_end..)
        .unwrap_or("")
        .trim_start_matches([' ', '\t']);
    if task {
        content
            .get(3..)
            .unwrap_or("")
            .trim_start_matches([' ', '\t'])
    } else {
        content
    }
}

/// The kind of a line that is a list marker and nothing else, such as `- `, `2. ` or
/// `- [x] `. The space after the marker is what tells one from a dash somebody typed.
fn marker_kind(text: &str) -> Option<BlockKind> {
    let (kind, rest) = match text.strip_prefix(['-', '*', '+']) {
        Some(rest) => (BlockKind::Bullet, rest),
        None => {
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            let rest = (1..=9)
                .contains(&digits)
                .then(|| text[digits..].strip_prefix(['.', ')']))
                .flatten()?;
            (BlockKind::Ordered, rest)
        }
    };
    match rest.strip_prefix(' ')?.trim_end() {
        "" => Some(kind),
        "[ ]" if kind == BlockKind::Bullet => Some(BlockKind::Task { checked: false }),
        "[x]" | "[X]" if kind == BlockKind::Bullet => Some(BlockKind::Task { checked: true }),
        _ => None,
    }
}

/// The `>` markers a line opens with, and the byte offset of the text after them.
fn quote_markers(line: &str) -> (usize, usize) {
    let bytes = line.as_bytes();
    let (mut offset, mut markers) = (0, 0);
    loop {
        let mut probe = offset;
        while probe < bytes.len() && matches!(bytes[probe], b' ' | b'\t') && probe - offset < 3 {
            probe += 1;
        }
        if bytes.get(probe) != Some(&b'>') {
            return (markers, offset);
        }
        offset = probe + 1;
        if matches!(bytes.get(offset), Some(b' ' | b'\t')) {
            offset += 1;
        }
        markers += 1;
    }
}

/// The indentation of a line in columns, with tab stops every four.
pub(super) fn indent_width(line: &str) -> usize {
    let mut width = 0;
    for byte in line.bytes() {
        match byte {
            b' ' => width += 1,
            b'\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// `line` between two one-based, inclusive-exclusive columns, clamped to its bounds.
fn slice(line: &str, from: usize, to: usize) -> &str {
    let start = from.saturating_sub(1).min(line.len());
    let end = to.saturating_sub(1).clamp(start, line.len());
    line.get(start..end).unwrap_or("")
}

/// The source text a node covers, for constructs that stay literal.
fn source(lines: &[&str], sourcepos: Sourcepos) -> String {
    let mut text = String::new();
    for number in sourcepos.start.line..=sourcepos.end.line {
        if number > sourcepos.start.line {
            text.push('\n');
        }
        let line = lines.get(number - 1).copied().unwrap_or("");
        let from = if number == sourcepos.start.line {
            sourcepos.start.column
        } else {
            1
        };
        let to = if number == sourcepos.end.line {
            sourcepos.end.column + 1
        } else {
            usize::MAX
        };
        text.push_str(slice(line, from, to));
    }
    text
}

/// Inline conversion of one leaf block into its source lines' spans.
struct Inlines<'a> {
    lines: &'a [&'a str],
    segments: Vec<Vec<Span>>,
    /// What each inline HTML tag of this block does, in document order.
    tags: Vec<Tag>,
    seen: usize,
    /// The marks the tags seen so far have opened.
    html: Marks,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Open(Mark),
    Close(Mark),
    Literal,
}

fn inlines<'a>(lines: &[&str], node: &'a AstNode<'a>) -> Vec<Vec<Span>> {
    let mut state = Inlines {
        lines,
        segments: vec![Vec::new()],
        tags: mark_tags(node),
        seen: 0,
        html: Marks::default(),
    };
    state.walk(node, Marks::default(), None);
    state.segments
}

/// The tags [`Document::to_markdown`] writes a mark as when no delimiter run would do:
/// always for underline, which has no Markdown syntax, and for a span whose edges keep a
/// delimiter from opening or closing where it sits.
fn mark_tag(html: &str) -> Option<(Mark, bool)> {
    let (name, open) = match html.strip_prefix("</") {
        Some(name) => (name, false),
        None => (html.strip_prefix('<')?, true),
    };
    let mark = match name.strip_suffix('>')? {
        "u" => Mark::Underline,
        "em" => Mark::Italic,
        "strong" => Mark::Bold,
        "del" => Mark::Strikethrough,
        _ => return None,
    };
    Some((mark, open))
}

/// Only tags that pair up within one block carry a mark; a stray one stays literal text,
/// as it reads.
fn mark_tags<'a>(node: &'a AstNode<'a>) -> Vec<Tag> {
    fn collect<'a>(node: &'a AstNode<'a>, tags: &mut Vec<Tag>) {
        for child in node.children() {
            if let NodeValue::HtmlInline(html) = &child.data.borrow().value {
                tags.push(match mark_tag(html) {
                    Some((mark, true)) => Tag::Open(mark),
                    Some((mark, false)) => Tag::Close(mark),
                    None => Tag::Literal,
                });
            }
            // Match `Inlines::walk`: opaque nodes such as images keep their source and
            // never visit their descendants, including HTML inside an image label.
            if matches!(
                child.data.borrow().value,
                NodeValue::Emph | NodeValue::Strong | NodeValue::Strikethrough | NodeValue::Link(_)
            ) {
                collect(child, tags);
            }
        }
    }
    let mut tags = Vec::new();
    collect(node, &mut tags);
    let mut open: Vec<usize> = Vec::new();
    for index in 0..tags.len() {
        match tags[index] {
            Tag::Open(_) => open.push(index),
            Tag::Close(mark) => match open.iter().rposition(|s| tags[*s] == Tag::Open(mark)) {
                Some(position) => {
                    let nested: Vec<usize> = open.drain(position + 1..).collect();
                    open.pop();
                    for start in nested {
                        tags[start] = Tag::Literal;
                    }
                }
                None => tags[index] = Tag::Literal,
            },
            Tag::Literal => {}
        }
    }
    for index in open {
        tags[index] = Tag::Literal;
    }
    tags
}

impl Inlines<'_> {
    fn walk<'a>(&mut self, node: &'a AstNode<'a>, marks: Marks, link: Option<&str>) {
        for child in node.children() {
            let ast = child.data.borrow();
            match &ast.value {
                NodeValue::Text(text) => self.push(text, marks, link),
                NodeValue::Code(code) => self.push(
                    &code.literal,
                    Marks {
                        code: true,
                        ..marks
                    },
                    link,
                ),
                NodeValue::Emph => self.walk(
                    child,
                    Marks {
                        italic: true,
                        ..marks
                    },
                    link,
                ),
                NodeValue::Strong => self.walk(
                    child,
                    Marks {
                        bold: true,
                        ..marks
                    },
                    link,
                ),
                NodeValue::Strikethrough => self.walk(
                    child,
                    Marks {
                        strikethrough: true,
                        ..marks
                    },
                    link,
                ),
                NodeValue::Link(target) if child.first_child().is_some() => {
                    self.walk(child, marks, Some(&target.url));
                }
                // A hard break is a line of its own here, like a soft one.
                NodeValue::SoftBreak | NodeValue::LineBreak => self.segments.push(Vec::new()),
                NodeValue::HtmlInline(html) => {
                    let tag = self.tags.get(self.seen).copied().unwrap_or(Tag::Literal);
                    self.seen += 1;
                    match tag {
                        Tag::Open(mark) => self.html.set(mark, true),
                        Tag::Close(mark) => self.html.set(mark, false),
                        Tag::Literal => self.push(html, marks, link),
                    }
                }
                // Images, footnote references and anything else with no model: source text.
                _ => {
                    let text = source(self.lines, ast.sourcepos);
                    self.push(&text, marks, link);
                }
            }
        }
    }

    fn push(&mut self, text: &str, marks: Marks, link: Option<&str>) {
        let marks = Marks {
            bold: marks.bold || self.html.bold,
            italic: marks.italic || self.html.italic,
            strikethrough: marks.strikethrough || self.html.strikethrough,
            underline: marks.underline || self.html.underline,
            ..marks
        };
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.segments.push(Vec::new());
            }
            let spans = self.segments.last_mut().expect("a segment is always open");
            push_linked_span(spans, part, marks, link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_unicode_whitespace_is_not_duplicated() {
        for source in [
            "\u{3000}你好",
            "\u{a0}hello",
            "  \u{3000}你好",
            "first\n\u{3000}second",
        ] {
            let imported = document(source);
            assert_eq!(imported.plain_text(), source);
            let mut reloaded = imported.clone();
            for _ in 0..3 {
                reloaded = document(&reloaded.to_markdown());
                assert_eq!(reloaded, imported);
            }
        }
    }

    #[test]
    fn reference_definitions_on_item_marker_lines_stay_literal() {
        for source in [
            "- [ref]: https://example.com",
            "1. [ref]: https://example.com",
            "- [ref]: https://example.com\n  text",
            "- parent\n  - [ref]: https://example.com",
        ] {
            let imported = document(source);
            let definition = imported
                .blocks
                .iter()
                .find(|block| block.text().starts_with("[ref]:"))
                .unwrap_or_else(|| {
                    panic!("the reference definition must survive import: {source:?}: {imported:?}")
                });
            assert_eq!(definition.text(), "[ref]: https://example.com");
            assert!(matches!(
                definition.kind,
                BlockKind::Bullet | BlockKind::Ordered
            ));
            assert_eq!(document(&imported.to_markdown()), imported);
        }
    }

    #[test]
    fn reference_definition_fallback_keeps_lazy_continuation_text() {
        for (source, expected, last_kind) in [
            (
                "- [ref]: url\ntext",
                "[ref]: url\ntext",
                BlockKind::Paragraph,
            ),
            (
                "  [ref]: url\ntext",
                "  [ref]: url\ntext",
                BlockKind::Paragraph,
            ),
            (
                "- [ref]: url\n  text",
                "[ref]: url\ntext",
                BlockKind::Bullet,
            ),
            (
                "> [ref]: url\ntext",
                "[ref]: url\ntext",
                BlockKind::Paragraph,
            ),
            ("> [ref]: url\n> text", "[ref]: url\ntext", BlockKind::Quote),
            ("- [ref]: url\n你", "[ref]: url\n你", BlockKind::Paragraph),
            ("- [ref]: url\n x", "[ref]: url\n x", BlockKind::Paragraph),
        ] {
            let imported = document(source);
            assert_eq!(imported.plain_text(), expected, "{source:?}");
            assert_eq!(
                imported.blocks.last().unwrap().kind,
                last_kind,
                "{source:?}"
            );
        }
    }

    #[test]
    fn html_tags_inside_opaque_images_do_not_change_following_marks() {
        for source in [
            "![<u>x</u>](url) <strong>z</strong>",
            "*![<u>x</u>](url)* <strong>z</strong>",
            "![<u>x](url) <strong>z</strong>",
        ] {
            let imported = document(source);
            let span = imported.blocks[0]
                .spans
                .iter()
                .find(|span| span.text == "z")
                .unwrap();
            assert!(span.marks.bold);
            assert!(!span.marks.underline);
            assert!(!span.marks.italic);
            assert_eq!(document(&imported.to_markdown()), imported);
        }
    }

    #[test]
    fn empty_links_stay_literal() {
        for source in [
            "before [](https://example.com) after",
            "before []() after",
            "[](https://example.com \"title\")",
        ] {
            let imported = document(source);
            assert_eq!(imported.plain_text(), source);
            assert_eq!(document(&imported.to_markdown()), imported);
        }
    }

    #[test]
    fn fragments_keep_trailing_paragraph_whitespace() {
        for (source, expected) in [
            ("hello ", "hello "),
            ("**hello** \t", "hello \t"),
            ("hello\n \t", "hello\n \t"),
            (" \t", " \t"),
            ("- hello ", "hello "),
            ("> hello ", "hello "),
        ] {
            assert_eq!(fragment(source).plain_text(), expected, "{source:?}");
        }
        assert_eq!(document("hello ").plain_text(), "hello");
    }

    #[test]
    fn fragments_do_not_duplicate_literal_whitespace_or_marker_padding() {
        for source in [
            "# heading #   ",
            "#   ",
            "-   ",
            "- [ ]   ",
            "```\ncode  \n```   ",
            "    code  ",
            "<div>literal  ",
            "<!-- literal -->  ",
            "[ref]: https://example.com  ",
        ] {
            assert_eq!(fragment(source), document(source), "{source:?}");
        }
    }
}
