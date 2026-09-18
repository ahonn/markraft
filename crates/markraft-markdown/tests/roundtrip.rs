//! `parse(serialize(doc)) == doc` for random documents on the CommonMark
//! schema.
//!
//! The generator stays inside what CommonMark can write down, because the codec
//! cannot promise more than the format does. It therefore avoids:
//!
//! * a `hard_break` at the end of a textblock, or inside a heading — CommonMark
//!   cannot end a block with a line break;
//! * whitespace at the edges of a marked run, or of a textblock — a reader
//!   strips the first and refuses to read the second as emphasis at all;
//! * a `tight` list whose items hold blocks that need a blank line between
//!   them, and a loose list with nowhere to put a blank line — one item holding
//!   one block always reads back tight;
//! * two lists of the same kind side by side, which a reader joins;
//! * a fence character run inside a code block, which forces the fence to grow;
//! * a line ending inside a code span, which CommonMark turns into a space.
//!
//! Every one of those is pinned by a test of its own in `cases.rs`, with the
//! behaviour the codec falls back to. Nothing else is excepted: attributes,
//! marks and structure come back exactly.

mod common;

use common::{Codec, Rng};
use markraft_doc::{Attrs, Mark, MarkSet, Node, Schema, attrs};
use markraft_markdown::schema as md;

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
];

const LANGUAGES: &[&str] = &["", "rust", "js", "text"];
const HREFS: &[&str] = &[
    "https://example.com",
    "/relative",
    "https://example.com/a_(b)",
    "https://example.com/?q=&copy;",
    "a b",
    "",
];
const RAW_SOURCES: &[&str] = &[
    "| a | b |\n| --- | --- |\n| 1 | 2 |",
    "<div>\nraw\n</div>",
    "<!-- a comment -->",
];

struct Gen<'a> {
    schema: &'a Schema,
    rng: Rng,
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
        for name in [md::UNDERLINE, md::STRIKETHROUGH, md::STRONG, md::EM] {
            if self.rng.one_in(4) {
                picked.push(self.mark(name, Attrs::empty()));
            }
        }
        if self.rng.one_in(5) {
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
            if index > 0 && !after_break && !make_break {
                out.push(self.schema.text(" "));
            }
            after_break = make_break;
            if make_break {
                out.push(self.schema.node(md::HARD_BREAK, []).expect("a hard break"));
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
        let choices = if depth == 0 { 4 } else { 8 };
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
                    .node_with(md::RAW_BLOCK, attrs! {"source" => source}, [])
                    .expect("a raw block")
            }
            6 => {
                let blocks = self.blocks(depth - 1, false, 1, 2);
                self.schema
                    .node(md::BLOCKQUOTE, blocks)
                    .expect("a block quote")
            }
            _ => self.list(depth),
        }
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

#[test]
fn random_documents_survive_a_round_trip() {
    let codec = Codec::new();
    for seed in 1..2000u64 {
        let mut generator = Gen {
            schema: &codec.schema,
            rng: Rng::new(seed),
        };
        let blocks = generator.blocks(2, false, 1, 4);
        let doc = codec.schema.doc(blocks).expect("a document");
        doc.check(&codec.schema).expect("the generator is valid");
        let written = codec.write(&doc);
        let back = codec.parse(&written);
        assert_eq!(
            codec.describe(&back),
            codec.describe(&doc),
            "seed {seed} did not survive:\n{written:?}\n{doc:?}"
        );
        assert_eq!(back, doc, "seed {seed}: attributes differ\n{written}");
        assert_eq!(
            codec.write(&back),
            written,
            "seed {seed} is not a fixed point"
        );
    }
}
