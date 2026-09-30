//! Shared test schema, builders and a deterministic random generator.

use crate::attr::{AttrKind, AttrSpec, AttrValue, Attrs};
use crate::change::Change;
use crate::fragment::Fragment;
use crate::mark::{Mark, MarkSet};
use crate::node::{Markup, Node};
use crate::schema::{BreakKind, MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec};
use crate::slice::{Slice, Token};

/// A schema with the building blocks the tests exercise: paragraphs, headings,
/// blockquotes, lists, code blocks, images, hard breaks and four mark types.
pub fn test_schema() -> Schema {
    Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "block+"))
            .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
            .node(
                NodeTypeSpec::new("heading", "inline*")
                    .group("block")
                    .defining(true)
                    .attr(AttrSpec::new("level", AttrKind::Int, AttrValue::Int(1))),
            )
            .node(
                NodeTypeSpec::new("blockquote", "block+")
                    .group("block")
                    .defining(true),
            )
            .node(
                NodeTypeSpec::new("code_block", "text*")
                    .group("block")
                    .code(true)
                    .defining(true)
                    .marks(""),
            )
            .node(NodeTypeSpec::new("bullet_list", "item+").group("block"))
            .node(NodeTypeSpec::new("ordered_list", "item+").group("block"))
            .node(
                NodeTypeSpec::new("list_item", "paragraph block*")
                    .group("item")
                    .defining(true),
            )
            .node(
                NodeTypeSpec::new("task_item", "paragraph block*")
                    .group("item")
                    .defining(true)
                    .attr(AttrSpec::new(
                        "checked",
                        AttrKind::Bool,
                        AttrValue::Bool(false),
                    )),
            )
            .node(
                NodeTypeSpec::leaf("horizontal_rule")
                    .group("block")
                    .selectable(true),
            )
            .node(NodeTypeSpec::text("text").group("inline"))
            .node(
                NodeTypeSpec::leaf("image")
                    .inline(true)
                    .group("inline")
                    .selectable(true)
                    .attr(AttrSpec::required("src", AttrKind::Str)),
            )
            .node(
                NodeTypeSpec::leaf("hard_break")
                    .inline(true)
                    .group("inline")
                    .break_kind(BreakKind::Hard),
            )
            .node(
                NodeTypeSpec::new("inline_span", "inline*")
                    .inline(true)
                    .group("inline"),
            )
            .mark(MarkTypeSpec::new("strong").rank(20).group("style"))
            .mark(MarkTypeSpec::new("em").rank(30).group("style"))
            .mark(MarkTypeSpec::new("code").rank(40).excludes("_"))
            .mark(
                MarkTypeSpec::new("link")
                    .rank(60)
                    .inclusive(false)
                    .attr(AttrSpec::required("href", AttrKind::Str)),
            ),
    )
    .expect("the test schema is valid")
}

/// Build a node by type name.
pub fn n(schema: &Schema, name: &str, content: impl IntoIterator<Item = Node>) -> Node {
    schema.node(name, content).expect("valid node")
}

/// Build a node by type name with attributes.
pub fn na(
    schema: &Schema,
    name: &str,
    attrs: Attrs,
    content: impl IntoIterator<Item = Node>,
) -> Node {
    schema.node_with(name, attrs, content).expect("valid node")
}

pub fn doc(schema: &Schema, content: impl IntoIterator<Item = Node>) -> Node {
    schema.doc(content).expect("valid document")
}

/// Build a plain text leaf.
pub fn t(schema: &Schema, text: &str) -> Node {
    schema.text(text)
}

/// Build a text leaf carrying the named marks.
pub fn tm(schema: &Schema, text: &str, marks: &[&str]) -> Node {
    let set = MarkSet::from_marks(schema, marks.iter().map(|name| m(schema, name)));
    schema.text_marked(text, set)
}

/// A mark without attributes.
pub fn m(schema: &Schema, name: &str) -> Mark {
    schema.mark(name, Attrs::empty()).expect("known mark")
}

pub fn link(schema: &Schema, href: &str) -> Mark {
    schema
        .mark("link", crate::attrs! {"href" => href})
        .expect("known mark")
}

pub fn img(schema: &Schema, src: &str) -> Node {
    na(schema, "image", crate::attrs! {"src" => src}, [])
}

/// Build a state on the test schema.
pub fn state(document: Node, extensions: crate::state::Extension) -> crate::state::EditorState {
    let schema = document_schema(&document);
    crate::state::EditorState::create(
        crate::state::EditorStateConfig::new(schema)
            .doc(document)
            .extensions(extensions),
    )
    .expect("a valid starting state")
}

/// The test schema, cached so every state in one test shares it.
pub fn shared_schema() -> Schema {
    use std::sync::OnceLock;
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(test_schema).clone()
}

fn document_schema(_doc: &Node) -> Schema {
    shared_schema()
}

/// A change that inserts plain text at `pos`.
pub fn insert_text(schema: &Schema, pos: usize, text: &str) -> Change {
    Change::insert(
        pos,
        Slice::from_fragment(Fragment::from_node(schema.text(text))),
    )
}

/// A tiny xorshift-style generator, so property tests are reproducible without
/// pulling in a dependency.
pub struct Rng(u64);

impl Rng {
    /// Seed the generator. Any non-zero seed works.
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(6364136223846793005).wrapping_add(1) | 1)
    }

    /// The next raw value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// A value in `from..=to`.
    pub fn range(&mut self, from: usize, to: usize) -> usize {
        from + self.below(to - from + 1)
    }

    /// True with probability `1 / n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Words for generated text. Besides ASCII: a precomposed letter, CJK, an
/// emoji outside the Basic Multilingual Plane, and a ZWJ sequence that is one
/// grapheme over several scalars, so boundaries and offsets meet multi-byte text.
const WORDS: &[&str] = &["alpha", "beta", "gamma", "delta", "é", "中文", "😀", "👩‍👩‍👧"];

/// Random inline content for a textblock, empty one time in eight.
pub fn random_inline(schema: &Schema, rng: &mut Rng, allow_marks: bool) -> Vec<Node> {
    let words = WORDS;
    if rng.one_in(8) {
        return Vec::new();
    }
    let count = rng.range(1, 4);
    let mut out = Vec::new();
    for _ in 0..count {
        if allow_marks && rng.one_in(8) {
            out.push(img(schema, "pic.png"));
            continue;
        }
        if allow_marks && rng.one_in(10) {
            out.push(n(schema, "hard_break", []));
            continue;
        }
        let word = *rng.pick(words);
        if !allow_marks {
            out.push(t(schema, word));
            continue;
        }
        // Code excludes every other mark, so it comes alone.
        if rng.one_in(8) {
            out.push(tm(schema, word, &["code"]));
            continue;
        }
        let mut marks = Vec::new();
        if rng.one_in(3) {
            marks.push("strong");
        }
        if rng.one_in(4) {
            marks.push("em");
        }
        if rng.one_in(6) {
            out.push(
                schema.text_marked(
                    word,
                    MarkSet::from_marks(
                        schema,
                        marks
                            .iter()
                            .map(|name| m(schema, name))
                            .chain([link(schema, "https://example.com")]),
                    ),
                ),
            );
        } else {
            out.push(tm(schema, word, &marks));
        }
    }
    out
}

/// A random block node, nesting up to `depth` levels.
pub fn random_block(schema: &Schema, rng: &mut Rng, depth: usize) -> Node {
    if rng.one_in(10) {
        return n(schema, "horizontal_rule", []);
    }
    match rng.below(if depth == 0 { 3 } else { 6 }) {
        0 => n(schema, "paragraph", random_inline(schema, rng, true)),
        1 => na(
            schema,
            "heading",
            crate::attrs! {"level" => rng.range(1, 3) as i64},
            random_inline(schema, rng, true),
        ),
        2 => n(schema, "code_block", random_inline(schema, rng, false)),
        3 => {
            let count = rng.range(1, 2);
            let blocks: Vec<Node> = (0..count)
                .map(|_| random_block(schema, rng, depth - 1))
                .collect();
            n(schema, "blockquote", blocks)
        }
        _ => {
            let kind = if rng.one_in(2) {
                "bullet_list"
            } else {
                "ordered_list"
            };
            let count = rng.range(1, 3);
            let items: Vec<Node> = (0..count)
                .map(|_| {
                    let mut content =
                        vec![n(schema, "paragraph", random_inline(schema, rng, true))];
                    // `depth - 1`, not less, so a list can hold a list.
                    if depth > 1 && rng.one_in(3) {
                        content.push(random_block(schema, rng, depth - 1));
                    }
                    if rng.one_in(3) {
                        na(
                            schema,
                            "task_item",
                            crate::attrs! {"checked" => rng.one_in(2)},
                            content,
                        )
                    } else {
                        n(schema, "list_item", content)
                    }
                })
                .collect();
            n(schema, kind, items)
        }
    }
}

/// A random document of one to four top-level blocks.
pub fn random_doc(schema: &Schema, rng: &mut Rng) -> Node {
    let count = rng.range(1, 4);
    let blocks: Vec<Node> = (0..count).map(|_| random_block(schema, rng, 2)).collect();
    doc(schema, blocks)
}

/// One of `items`, or `None` when there are none.
fn pick_some<'a, T>(rng: &mut Rng, items: &'a [T]) -> Option<&'a T> {
    (!items.is_empty()).then(|| rng.pick(items))
}

/// Every position inside a textblock's inline content, as `(block start, pos)`.
pub fn textblock_positions(schema: &Schema, d: &Node) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    d.descendants(&mut |node, pos, _, _| {
        if node.is_textblock(schema) {
            let start = pos + 1;
            for offset in 0..=node.content_size() {
                out.push((start, start + offset));
            }
        }
        true
    });
    out
}

/// Every block node in the document, as `(before, after, depth)`.
pub fn block_spans(schema: &Schema, d: &Node) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    d.descendants(&mut |node, pos, _, _| {
        if node.is_block(schema) && node.is_container() {
            out.push((pos, pos + node.node_size()));
        }
        true
    });
    out
}

/// A random structural edit — a split, join, wrap or lift — expressed as the
/// token-level changes that perform it.
///
/// The changes ask to be repaired, because a lift can put content where the
/// schema does not allow it and an editor command would fit it too.
///
/// Returns `None` when the document offers no position for the chosen edit.
pub fn random_structural_change(schema: &Schema, rng: &mut Rng, d: &Node) -> Option<Vec<Change>> {
    let changes = random_structural_edit(schema, rng, d)?;
    Some(
        changes
            .into_iter()
            .map(|change| change.with_fit(crate::fit::Fit::Auto))
            .collect(),
    )
}

fn random_structural_edit(schema: &Schema, rng: &mut Rng, d: &Node) -> Option<Vec<Change>> {
    match rng.below(4) {
        // Split: close the textblock and open a fresh one of the same type.
        0 => {
            // Anywhere in a textblock, its edges included.
            let spots = textblock_positions(schema, d);
            let (_, at) = *pick_some(rng, &spots)?;
            let node = d.resolve(at).ok()?;
            let markup = Markup::with_attrs(node.parent().type_id(), node.parent().attrs().clone());
            Some(vec![Change::insert(
                at,
                Slice::from_tokens(&[Token::Close(markup.clone()), Token::Open(markup)]),
            )])
        }
        // Join: delete the close and open tokens between two adjacent blocks.
        1 => {
            let spans: Vec<(usize, usize)> = block_spans(schema, d)
                .into_iter()
                .filter(|(_, end)| {
                    d.resolve(*end)
                        .map(|r| r.node_after().is_some())
                        .unwrap_or(false)
                })
                .collect();
            let (_, end) = *pick_some(rng, &spans)?;
            Some(vec![Change::delete(end - 1, end + 1)])
        }
        // Wrap: an open token before a block and a close token after it.
        2 => {
            let spans = block_spans(schema, d);
            let (before, after) = *pick_some(rng, &spans)?;
            let quote = schema.node_id("blockquote")?;
            let markup = Markup::new(quote);
            Some(vec![
                Change::insert(before, Slice::from_tokens(&[Token::Open(markup.clone())])),
                Change::insert(after, Slice::from_tokens(&[Token::Close(markup)])),
            ])
        }
        // Lift: delete a container's own two tokens.
        _ => {
            let spans: Vec<(usize, usize)> = block_spans(schema, d)
                .into_iter()
                .filter(|(before, after)| {
                    d.node_at(*before)
                        .map(|n| n.is_container() && n.content_size() > 0)
                        .unwrap_or(false)
                        && after - before > 2
                })
                .collect();
            let (before, after) = *pick_some(rng, &spans)?;
            Some(vec![
                Change::delete(before, before + 1),
                Change::delete(after - 1, after),
            ])
        }
    }
}
