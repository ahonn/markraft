//! `parse(serialize(doc)) == doc` for random documents on the CommonMark
//! schema.
//!
//! The generator builds *semantic* documents — text carrying style marks, the
//! way a host or a clipboard describes content — and brings each into the
//! inline source model with [`spell_document`], which is where its marks get
//! their spelling. That document has to survive being written and read back
//! exactly: text, marks, atoms, attributes and structure.
//!
//! The generator stays inside what CommonMark can write down, because the codec
//! cannot promise more than the format does. It therefore avoids:
//!
//! * a line break at the end of a textblock, or inside a heading — CommonMark
//!   cannot end a block with a line break;
//! * whitespace at the edges of a marked run, or of a textblock — a reader
//!   strips the first and refuses to read the second as emphasis at all;
//! * a `tight` list whose items hold blocks that need a blank line between
//!   them, and a loose list with nowhere to put a blank line — one item holding
//!   one block always reads back tight;
//! * two lists of the same kind side by side, which a reader joins;
//! * a fence character run inside a code block, which forces the fence to grow;
//! * a line ending inside a code span, which CommonMark turns into a space;
//! * a line break inside a table cell, which a row of one source line cannot
//!   hold, and a table or a raw block inside a list item, whose blank lines
//!   decide the list's tightness for it — `cases.rs` pins all three.
//!
//! It does generate the two shapes GFM's autolink extension reads: a link
//! whose text is its own URL, which is spelled bare, and plain text that only
//! looks like one, which has to be kept from becoming a link.

mod common;

use common::{Codec, Rng};
use markraft_commonmark::html::{HtmlParser, HtmlSerializer};
use markraft_commonmark::schema as md;
use markraft_commonmark::serialize::spell_document;
use markraft_core::{Attrs, Mark, MarkSet, Node, Schema, attrs};

/// Words with no whitespace at their edges, covering the characters a
/// serialiser has to think about.
const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gamma",
    "R&D",
    "5.0",
    "(approx.)",
    "a-b",
    "C#",
    "a*b",
    "a_b",
    "a`b",
    "a[b]c",
    "a<b",
    "a&amp;b",
    "a\\b",
    "#hash",
    "1.item",
    "-dash",
    "x|y",
    "{z}",
    "a\nb",
    "中文",
    "😀",
    "e\u{301}x",
    "👨‍👩‍👧‍👦",
    "#",
    "-",
    "+",
    ">",
    "=",
    "1.",
    "!",
    "~",
    "`",
    "*",
    "_",
    "[",
    "]",
    "<",
    "&",
    "\\",
    // Text a reader would autolink if the serialiser let it.
    "https://example.com",
    "a@b.example",
];

const LANGUAGES: &[&str] = &["", "rust", "js", "text"];
/// A bare URL and the href a reader gives it, which is the link the serialiser
/// has to be able to write back without brackets.
const AUTOLINKS: &[(&str, &str)] = &[
    ("https://example.com", "https://example.com"),
    ("https://example.com/a/b", "https://example.com/a/b"),
    ("www.example.com", "http://www.example.com"),
    ("someone@example.com", "mailto:someone@example.com"),
];
const HREFS: &[&str] = &[
    "https://example.com",
    "/relative",
    "https://example.com/a_(b)",
    "https://example.com/?q=&copy;",
    "a b",
    "",
];
const RAW_SOURCES: &[&str] = &["<div>\nraw\n</div>", "<!-- a comment -->"];
/// What a callout's marker may spell. A type holds no bracket and is never
/// empty; a title is one line and may hold anything a line may.
const CALLOUT_TYPES: &[&str] = &["note", "tip", "NOTE", "custom-type", "警告"];
const CALLOUT_TITLES: &[&str] = &["", "Title", "a|b **c**", "two  spaces", "trailing "];
/// What a wiki link's target and alias may spell: anything but `[`, `]`, an
/// unescaped `|` and a line ending, which is what the recogniser reads. An
/// empty alias is the link having none.
const WIKI_PARTS: &[&str] = &[
    "",
    "Note",
    "folder/note.md",
    "Note#Heading",
    "Note^block-id",
    " spaced ",
    "中文 😀",
    "a\\]b",
    "100",
];
const ALIGNMENTS: &[&str] = &["none", "left", "center", "right"];

struct Gen<'a> {
    schema: &'a Schema,
    rng: Rng,
    /// Whether to generate underline, spelled `<u>…</u>`.
    allow_underline: bool,
    /// Whether to generate code spans.
    allow_code: bool,
}

impl Gen<'_> {
    fn mark(&mut self, name: &str, attrs: Attrs) -> Mark {
        self.schema.mark(name, attrs).expect("a preset mark")
    }

    fn marks(&mut self) -> MarkSet {
        let mut picked: Vec<Mark> = Vec::new();
        if self.rng.one_in(4) {
            let href = *self.rng.pick(HREFS);
            let title = if self.rng.one_in(3) { "a title" } else { "" };
            picked.push(self.mark(md::LINK, attrs! {"href" => href, "title" => title}));
        }
        let style: &[&str] = if self.allow_underline {
            &[
                md::UNDERLINE,
                md::STRIKETHROUGH,
                md::STRONG,
                md::EM,
                md::HIGHLIGHT,
                md::SUPERSCRIPT,
                md::SUBSCRIPT,
            ]
        } else {
            &[
                md::STRIKETHROUGH,
                md::STRONG,
                md::EM,
                md::HIGHLIGHT,
                md::SUPERSCRIPT,
                md::SUBSCRIPT,
            ]
        };
        for name in style {
            if self.rng.one_in(4) {
                picked.push(self.mark(name, Attrs::empty()));
            }
        }
        if self.rng.one_in(5) && self.allow_code {
            picked.push(self.mark(md::CODE, Attrs::empty()));
        }
        MarkSet::from_marks(self.schema, picked)
    }

    fn word(&mut self) -> String {
        let count = self.rng.range(1, 3);
        let mut out = String::new();
        for index in 0..count {
            if index > 0 {
                out.push(' ');
            }
            let word = self.rng.pick(WORDS);
            out.push_str(word);
        }
        out
    }

    /// Inline content with no whitespace at its edges, no trailing break, and
    /// no whitespace directly after a break — a reader strips the indentation of
    /// the line a break starts.
    fn inline(&mut self, breaks: bool) -> Vec<Node> {
        let count = self.rng.range(1, 4);
        let mut out: Vec<Node> = Vec::new();
        let mut after_break = false;
        for index in 0..count {
            let make_break =
                breaks && index > 0 && index + 1 < count && !after_break && self.rng.one_in(6);
            // Now and then no space, so styled runs meet and emphasis falls
            // inside words.
            if index > 0 && !after_break && !make_break && !self.rng.one_in(4) {
                out.push(self.schema.text(" "));
            }
            after_break = make_break;
            if make_break {
                out.push(self.schema.node(md::LINE_BREAK, []).expect("a hard break"));
                continue;
            }
            if self.rng.one_in(9) {
                let (text, href) = *self.rng.pick(AUTOLINKS);
                let mark = self.mark(md::LINK, attrs! {"href" => href, "title" => ""});
                let marks = MarkSet::from_marks(self.schema, [mark]);
                out.push(self.schema.text_marked(text, marks));
                continue;
            }
            // A table row is one source line, where GFM escapes the `|` that
            // separates a wiki link's alias from its target, so an aliased link
            // has no spelling there at all.
            if breaks && self.rng.one_in(8) {
                let target = *self.rng.pick(WIKI_PARTS);
                let alias = *self.rng.pick(WIKI_PARTS);
                out.push(
                    self.schema
                        .node_with(
                            md::WIKI_LINK,
                            attrs! {
                                "target" => target,
                                "alias" => alias,
                                "embed" => self.rng.one_in(3),
                            },
                            [],
                        )
                        .expect("a wiki link"),
                );
                continue;
            }
            if breaks && self.rng.one_in(8) {
                let src = *self.rng.pick(HREFS);
                out.push(
                    self.schema
                        .node_with(
                            md::IMAGE,
                            attrs! {"src" => src, "alt" => (*self.rng.pick(WORDS)).to_string(), "title" => ""},
                            [],
                        )
                        .expect("an image"),
                );
                continue;
            }
            let mut word = self.word();
            let marks = self.marks();
            if self
                .schema
                .mark_id(md::CODE)
                .is_some_and(|code| marks.contains_type(code))
            {
                // CommonMark turns a line ending inside a code span into a
                // space, so there is nothing to round trip.
                word = word.replace('\n', " ");
            }
            out.push(self.schema.text_marked(&word, marks));
        }
        merge(out)
    }

    fn code_text(&mut self, fence_char: char) -> String {
        let lines = self.rng.range(0, 3);
        let mut out = String::new();
        for index in 0..lines {
            if index > 0 {
                out.push('\n');
            }
            // A line may not open with the fence character, or the fence would
            // have to grow past it.
            out.push_str(&format!("code {}", *self.rng.pick(WORDS)).replace(fence_char, "x"));
        }
        out
    }

    fn block(&mut self, depth: usize, in_item: bool) -> Node {
        let choices = if depth == 0 { 4 } else { 9 };
        match self.rng.below(choices) {
            0 | 1 => self
                .schema
                .node(md::PARAGRAPH, self.inline(true))
                .expect("a paragraph"),
            2 => {
                let level = self.rng.range(1, 6) as i64;
                let content = self.inline(false);
                self.schema
                    .node_with(md::HEADING, attrs! {"level" => level}, content)
                    .expect("a heading")
            }
            3 => {
                let fence_char = if self.rng.one_in(3) { '~' } else { '`' };
                let fence_length = self.rng.range(3, 5) as i64;
                let language = *self.rng.pick(LANGUAGES);
                let text = self.code_text(fence_char);
                let content = (!text.is_empty())
                    .then(|| self.schema.text(&text))
                    .into_iter();
                self.schema
                    .node_with(
                        md::CODE_BLOCK,
                        attrs! {
                            "language" => language,
                            "fence_char" => fence_char.to_string(),
                            "fence_length" => fence_length,
                        },
                        content,
                    )
                    .expect("a code block")
            }
            4 => self
                .schema
                .node(md::HORIZONTAL_RULE, [])
                .expect("a thematic break"),
            5 if !in_item => {
                let source = *self.rng.pick(RAW_SOURCES);
                self.schema
                    .node(md::RAW_BLOCK, [self.schema.text(source)])
                    .expect("a raw block")
            }
            6 if !in_item => self.table(),
            7 => {
                let blocks = self.blocks(depth - 1, false, 1, 2);
                // Every third quote is a callout, which is the same node
                // carrying the marker its first line writes.
                let (callout, fold, title) = if self.rng.one_in(3) {
                    (
                        *self.rng.pick(CALLOUT_TYPES),
                        *self.rng.pick(&["", "-", "+"]),
                        *self.rng.pick(CALLOUT_TITLES),
                    )
                } else {
                    ("", "", "")
                };
                self.schema
                    .node_with(
                        md::BLOCKQUOTE,
                        attrs! {"callout" => callout, "fold" => fold, "title" => title},
                        blocks,
                    )
                    .expect("a block quote")
            }
            _ => self.list(depth),
        }
    }

    /// A small table, honouring the column invariant: the `alignments` attribute
    /// has one entry per column and every row holds exactly that many cells.
    fn table(&mut self) -> Node {
        let columns = self.rng.range(1, 3);
        let alignments: Vec<&str> = (0..columns).map(|_| *self.rng.pick(ALIGNMENTS)).collect();
        let rows = self.rng.range(1, 3);
        let mut children = Vec::with_capacity(rows);
        for _ in 0..rows {
            let cells: Vec<Node> = (0..columns)
                .map(|_| {
                    // A row is one source line, so a hard break cannot travel
                    // in a cell; an empty cell is a shape of its own.
                    let content = if self.rng.one_in(5) {
                        Vec::new()
                    } else {
                        self.inline(false)
                    };
                    self.schema
                        .node(md::TABLE_CELL, content)
                        .expect("a table cell")
                })
                .collect();
            children.push(self.schema.node(md::TABLE_ROW, cells).expect("a table row"));
        }
        self.schema
            .node_with(
                md::TABLE,
                attrs! {"alignments" => alignments.join(",")},
                children,
            )
            .expect("a table")
    }

    fn list(&mut self, depth: usize) -> Node {
        let ordered = self.rng.one_in(2);
        let tight = self.rng.one_in(2);
        let count = self.rng.range(1, 3);
        let mut items = Vec::new();
        for _ in 0..count {
            // A tight list only survives when every item holds a single block,
            // and looseness is only visible when an item directly holds a
            // paragraph, so a loose item starts with one.
            let blocks = if tight {
                vec![self.block(depth.saturating_sub(1), true)]
            } else {
                let mut blocks = vec![
                    self.schema
                        .node(md::PARAGRAPH, self.inline(true))
                        .expect("a paragraph"),
                ];
                if self.rng.one_in(2) {
                    blocks.push(self.block(depth.saturating_sub(1), true));
                }
                blocks
            };
            // A task item's check box lives in its first paragraph, so only an
            // item that starts with one can be a task.
            let starts_with_paragraph = blocks
                .first()
                .is_some_and(|block| self.schema.node_id(md::PARAGRAPH) == Some(block.type_id()));
            let item = if starts_with_paragraph && self.rng.one_in(3) {
                let checked = self.rng.one_in(2);
                self.schema
                    .node_with(md::TASK_ITEM, attrs! {"checked" => checked}, blocks)
                    .expect("a task item")
            } else {
                self.schema.node(md::LIST_ITEM, blocks).expect("an item")
            };
            items.push(item);
        }
        // Looseness is blank lines, so a list with nowhere to put one — a
        // single item holding a single block — always reads back tight.
        let tight = tight
            || (items.len() == 1 && items.first().is_some_and(|item| item.child_count() == 1));
        if ordered {
            let start = self.rng.range(0, 12) as i64;
            let delimiter = if self.rng.one_in(3) { ")" } else { "." };
            self.schema
                .node_with(
                    md::ORDERED_LIST,
                    attrs! {"tight" => tight, "start" => start, "delimiter" => delimiter},
                    items,
                )
                .expect("an ordered list")
        } else {
            let bullet = *self.rng.pick(&["-", "*", "+"]);
            self.schema
                .node_with(
                    md::BULLET_LIST,
                    attrs! {"tight" => tight, "bullet_char" => bullet},
                    items,
                )
                .expect("a bullet list")
        }
    }

    /// A run of blocks, never putting two lists of the same kind side by side.
    fn blocks(&mut self, depth: usize, in_item: bool, least: usize, most: usize) -> Vec<Node> {
        let count = self.rng.range(least, most);
        let mut out: Vec<Node> = Vec::new();
        for _ in 0..count {
            let mut block = self.block(depth, in_item);
            let mut tries = 0;
            while tries < 4
                && out.last().is_some_and(|previous| {
                    previous.type_id() == block.type_id() && is_list(self.schema, &block)
                })
            {
                block = self.block(depth, in_item);
                tries += 1;
            }
            if out.last().is_some_and(|previous| {
                previous.type_id() == block.type_id() && is_list(self.schema, &block)
            }) {
                continue;
            }
            out.push(block);
        }
        if out.is_empty() {
            out.push(
                self.schema
                    .node(md::PARAGRAPH, self.inline(true))
                    .expect("a paragraph"),
            );
        }
        out
    }
}

fn is_list(schema: &Schema, node: &Node) -> bool {
    [md::BULLET_LIST, md::ORDERED_LIST]
        .iter()
        .any(|name| schema.node_id(name) == Some(node.type_id()))
}

/// Adjacent text leaves with equal marks have to be one leaf.
fn merge(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for node in nodes {
        match out.last_mut() {
            Some(last) if last.is_text() && node.is_text() && last.marks() == node.marks() => {
                let joined = format!(
                    "{}{}",
                    last.text().unwrap_or_default(),
                    node.text().unwrap_or_default()
                );
                *last = last.with_text(&joined);
            }
            _ => out.push(node),
        }
    }
    out
}

/// What a document reads as: each textblock's characters with the style marks
/// on them, the spelling marked `syntax` left out, and every other node by its
/// type. Runs are merged, so how the text is split into nodes does not count.
/// With `fold`, each run of whitespace reads as one space, as HTML reads it.
fn reading(schema: &Schema, doc: &Node, fold: bool) -> Vec<String> {
    let syntax = schema.mark_id(markraft_commonmark::schema::SYNTAX);
    let mut out = Vec::new();
    doc.descendants(&mut |node, _, _, _| {
        if node.is_textblock(schema) {
            let mut runs: Vec<(String, String)> = Vec::new();
            for child in node.children() {
                let marks = child.marks();
                let spelling = syntax.is_some_and(|syntax| marks.contains_type(syntax));
                // Spelling reads as nothing, except a character reference, which
                // reads as the character it names.
                let text = match (spelling, child.text()) {
                    (true, Some(text)) => match character_reference(text) {
                        Some(ch) => ch.to_string(),
                        None => continue,
                    },
                    (true, None) => continue,
                    (false, Some(text)) => text.to_string(),
                    (false, None) => format!("<{}>", schema.node_type(child.type_id()).name()),
                };
                let names = marks
                    .iter()
                    .filter(|mark| Some(mark.ty) != syntax)
                    .map(|mark| schema.mark_type(mark.ty).name().to_string())
                    .collect::<Vec<_>>()
                    .join("+");
                match runs.last_mut() {
                    Some((last, on)) if *on == names => last.push_str(&text),
                    _ => runs.push((text, names)),
                }
            }
            out.push(
                runs.iter()
                    .map(|(text, on)| {
                        let text = if fold {
                            fold_whitespace(text)
                        } else {
                            text.clone()
                        };
                        format!("{text:?}{{{on}}}")
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            return false;
        }
        if !node.is_text() {
            out.push(schema.node_type(node.type_id()).name().to_string());
        }
        true
    });
    out
}

/// `text` with each run of whitespace made one space.
fn fold_whitespace(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    if space {
        out.push(' ');
    }
    out
}

/// Each textblock's characters and atoms, with the styles on each by name and
/// attributes. Spelling marked `syntax` reads as nothing, except a numeric
/// character reference, which reads as the character it names.
fn styled_units(schema: &Schema, doc: &Node) -> Vec<Vec<(Option<char>, Vec<String>)>> {
    let syntax = schema.mark_id(markraft_commonmark::schema::SYNTAX);
    let mut out = Vec::new();
    doc.descendants(&mut |node, _, _, _| {
        if !node.is_textblock(schema) {
            return true;
        }
        let mut units = Vec::new();
        for child in node.children() {
            let marks = child.marks();
            let styles: Vec<String> = marks
                .iter()
                .filter(|mark| Some(mark.ty) != syntax)
                .map(|mark| format!("{}{:?}", schema.mark_type(mark.ty).name(), mark.attrs))
                .collect();
            let spelling = syntax.is_some_and(|syntax| marks.contains_type(syntax));
            match child.text() {
                Some(text) if spelling => {
                    if let Some(ch) = character_reference(text) {
                        units.push((Some(ch), styles));
                    }
                }
                Some(text) => units.extend(text.chars().map(|ch| (Some(ch), styles.clone()))),
                None if spelling => {}
                None => units.push((None, styles)),
            }
        }
        out.push(units);
        false
    });
    out
}

/// The character a numeric reference such as `&#10;` or `&#x2a;` names.
fn character_reference(text: &str) -> Option<char> {
    let number = text.strip_prefix("&#")?.strip_suffix(';')?;
    let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code)
}

#[test]
fn random_documents_survive_a_round_trip() {
    let codec = Codec::new();
    let mut tally = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for seed in 1..2000u64 {
        let mut generator = Gen {
            schema: &codec.schema,
            rng: Rng::new(seed),
            allow_underline: seed % 2 == 0,
            allow_code: seed % 3 == 0,
        };
        let blocks = generator.blocks(2, false, 1, 4);
        let semantic = codec.schema.doc(blocks).expect("a document");
        semantic
            .check(&codec.schema)
            .expect("the generator is valid");
        let doc = spell_document(&codec.serializer, &semantic);
        doc.check(&codec.schema)
            .expect("spelling keeps the document valid");
        // `write` echoes inline source, so the round trip alone would pass a
        // `spell` that dropped every delimiter: what the spelled text reads as
        // must be what the semantic document said, less the styles Markdown
        // cannot say there, which are given up rather than left as delimiters.
        let spelled = styled_units(&codec.schema, &doc);
        let meant = styled_units(&codec.schema, &semantic);
        // Each block's text, an atom as U+FFFC.
        let text = |blocks: &[Vec<(Option<char>, Vec<String>)>]| -> Vec<String> {
            blocks
                .iter()
                .map(|units| {
                    units
                        .iter()
                        .map(|unit| unit.0.unwrap_or('\u{fffc}'))
                        .collect()
                })
                .collect()
        };
        let (got, want) = (text(&spelled), text(&meant));
        if let Some((got, want)) = got.iter().zip(&want).find(|(got, want)| got != want) {
            panic!("seed {seed}: the text changed\n  read  {got:?}\n  meant {want:?}");
        }
        assert_eq!(got.len(), want.len(), "seed {seed}: blocks changed");
        for (got, want) in spelled.iter().flatten().zip(meant.iter().flatten()) {
            if got.0.is_some_and(char::is_whitespace) {
                continue;
            }
            assert!(
                got.1.iter().all(|style| want.1.contains(style)),
                "seed {seed}: {:?} gained a style, spelled as {:?}",
                got,
                codec.write(&doc)
            );
            for style in &want.1 {
                let name = style
                    .split(['A', '('])
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let entry = tally.entry(name).or_insert((0usize, 0usize));
                entry.0 += 1;
                entry.1 += usize::from(!got.1.contains(style));
            }
        }
        let written = codec.write(&doc);
        let again = codec.parse(&written);
        assert_eq!(
            codec.describe(&again),
            codec.describe(&doc),
            "seed {seed} did not survive:\n{written:?}"
        );
        assert_eq!(again, doc, "seed {seed}: attributes differ\n{written}");
        assert_eq!(
            codec.write(&again),
            written,
            "seed {seed} is not a fixed point"
        );
    }
    // Giving a style up must stay the exception, or a `spell` that wrote no
    // delimiters at all would pass. What each style may lose, in percent, a
    // little above what was measured when this was set: a subscript cannot hold
    // a space and the generated runs often do (82%); a superscript is lost in
    // a link's text and where runs meet (29%); the rest only where runs meet
    // (under 3%).
    let ceilings = [
        ("code", 1),
        ("link", 1),
        ("underline", 5),
        ("strong", 5),
        ("em", 5),
        ("strikethrough", 5),
        ("highlight", 5),
        ("superscript", 40),
        ("subscript", 90),
    ];
    for (name, ceiling) in ceilings {
        let (all, lost) = tally.get(name).copied().unwrap_or_default();
        assert!(all > 1000, "only {all} {name} runs were generated");
        assert!(
            lost * 100 <= all * ceiling,
            "{lost} of {all} {name} styles were given up, over {ceiling}%"
        );
    }
}

#[test]
fn random_documents_survive_an_html_round_trip() {
    let codec = Codec::new();
    let parser = HtmlParser::commonmark(codec.schema.clone());
    let serializer = HtmlSerializer::commonmark(&codec.schema);
    for seed in 1..2000u64 {
        let mut generator = Gen {
            schema: &codec.schema,
            rng: Rng::new(seed),
            allow_underline: true,
            allow_code: true,
        };
        let blocks = generator.blocks(2, false, 1, 4);
        let semantic = codec.schema.doc(blocks).expect("a document");
        let doc = spell_document(&codec.serializer, &semantic);
        let written = serializer.serialize(&doc);
        let back = parser
            .parse(&written)
            .unwrap_or_else(|error| panic!("seed {seed} did not parse: {error}\n{written}"));
        back.check(&codec.schema).expect("a valid document");
        // A fixed point alone would pass an importer that dropped a mark both
        // times: what comes back must read as what went out.
        // HTML folds a run of whitespace into one space, so that much is not a
        // difference.
        assert_eq!(
            reading(&codec.schema, &back, true),
            reading(&codec.schema, &doc, true),
            "seed {seed}: {written}"
        );
        // HTML spells a soft break and the whitespace around a marked run
        // however it likes; the judge collapses that noise.
        assert_eq!(
            common::normalize_html(&serializer.serialize(&back)),
            common::normalize_html(&written),
            "seed {seed} is not an HTML fixed point"
        );
    }
}
