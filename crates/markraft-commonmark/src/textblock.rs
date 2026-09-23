//! A textblock's inline content as the source it is.
//!
//! In this document kind a paragraph's, a heading's or a table cell's text is
//! its Markdown inline source, and every style mark on it is what
//! [`derive`](crate::derive::derive) reads from that text. This module is where
//! the two meet: it lays a textblock's content out as the text `derive` reads —
//! each character, one U+FFFC per atom, one `\n` per line break — and builds
//! content back from such a text with the marks derived for it.
//!
//! The parser, the HTML importer and the canonicalising correction all go
//! through here, so a freshly read block and a freshly corrected one are built
//! by the same function and cannot disagree.

use markraft_core::projection::OBJECT_REPLACEMENT;
use markraft_core::{
    AttrValue, Attrs, Fragment, Mark, MarkSet, MarkTypeId, Node, NodeTypeId, Schema, attrs,
};

use crate::derive::{AtomSpan, BlockKind, DeriveContext, Derived, Style, derive, guard};
use crate::schema as md;

/// The kind of inline source a textblock of type `ty` holds, or `None` for a
/// block whose text is not inline Markdown — a code block, a raw block.
pub(crate) fn block_kind(schema: &Schema, ty: NodeTypeId) -> Option<BlockKind> {
    match schema.node_type(ty).name() {
        md::PARAGRAPH => Some(BlockKind::Paragraph),
        md::HEADING => Some(BlockKind::Heading),
        md::TABLE_CELL => Some(BlockKind::TableCell),
        _ => None,
    }
}

/// One position of a textblock's content.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// A character of the source.
    Char(char),
    /// An inline atom, without marks.
    Atom(Node),
    /// A line ending.
    Break,
}

/// A textblock's content, one [`Item`] per document position.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Items(pub(crate) Vec<Item>);

impl Items {
    /// The items of source text that holds no atom: `\n` is a line break.
    pub(crate) fn from_source(text: &str) -> Items {
        Items(
            text.chars()
                .map(|c| {
                    if c == '\n' {
                        Item::Break
                    } else {
                        Item::Char(c)
                    }
                })
                .collect(),
        )
    }

    /// The items of a textblock's children.
    pub(crate) fn from_nodes<'a>(
        schema: &Schema,
        nodes: impl IntoIterator<Item = &'a Node>,
    ) -> Items {
        let mut out = Vec::new();
        for node in nodes {
            push_node(schema, node, &mut out);
        }
        Items(out)
    }

    /// The text [`derive`] reads: an atom is U+FFFC and a break is `\n`.
    pub(crate) fn text(&self) -> String {
        self.0
            .iter()
            .map(|item| match item {
                Item::Char(c) => *c,
                Item::Atom(_) => OBJECT_REPLACEMENT,
                Item::Break => '\n',
            })
            .collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// Replace every run of text a reader takes for an atom with that atom,
    /// until none is left. Answers whether anything changed.
    pub(crate) fn atomize(
        &mut self,
        schema: &Schema,
        kind: BlockKind,
        ctx: &DeriveContext,
    ) -> bool {
        let mut changed = false;
        // Each round removes at least one character, and folding atoms can
        // reveal another only rarely; the bound only guards against a reader
        // that keeps reporting what cannot be built.
        for _ in 0..8 {
            let derived = derive(kind, &self.text(), ctx);
            let atoms = atom_nodes(schema, &derived.atoms);
            if atoms.is_empty() {
                break;
            }
            for (range, node) in atoms.into_iter().rev() {
                self.0.splice(range, [Item::Atom(node)]);
            }
            changed = true;
        }
        changed
    }

    /// Insert the backslashes [`guard`] asks for.
    pub(crate) fn guard(&mut self, schema: &Schema, kind: BlockKind) {
        for at in self.guard_insertions(schema, kind).into_iter().rev() {
            self.0.insert(at, Item::Char('\\'));
        }
    }

    /// The item indexes [`guard`] puts a backslash before.
    ///
    /// The guard reads the text as the file will hold it, each atom spelled
    /// out: what a line opens can hang on an atom's spelling — `[a]: <b>(c)`
    /// is no link reference definition, but with its tag as one placeholder
    /// character it would be. A backslash the guard would put inside an atom's
    /// spelling cannot go there, and is left out.
    pub(crate) fn guard_insertions(&self, schema: &Schema, kind: BlockKind) -> Vec<usize> {
        let mut spelled = String::new();
        let mut owner: Vec<Option<usize>> = Vec::new();
        for (index, item) in self.0.iter().enumerate() {
            match item {
                Item::Char(c) => {
                    spelled.push(*c);
                    owner.push(Some(index));
                }
                Item::Break => {
                    spelled.push('\n');
                    owner.push(Some(index));
                }
                Item::Atom(node) => {
                    for c in atom_spelling(schema, node).chars() {
                        spelled.push(c);
                        owner.push(None);
                    }
                }
            }
        }
        owner.push(Some(self.len()));
        guard(kind, &spelled)
            .insertions
            .into_iter()
            .filter_map(|at| owner.get(at).copied().flatten())
            .collect()
    }

    /// The content these items make, with the marks derived for them. Adjacent
    /// characters with equal marks are one text leaf.
    pub(crate) fn nodes(&self, schema: &Schema, derived: &Derived) -> Vec<Node> {
        let marks = derived_marks(schema, derived, self.len());
        let line_break = schema.node_id(md::LINE_BREAK);
        let mut out: Vec<Node> = Vec::new();
        let mut run = String::new();
        let mut run_marks = MarkSet::empty();
        let flush = |run: &mut String, marks: &MarkSet, out: &mut Vec<Node>| {
            if !run.is_empty() {
                out.push(schema.text_marked(run, marks.clone()));
                run.clear();
            }
        };
        for (item, set) in self.0.iter().zip(marks) {
            match item {
                Item::Char(c) => {
                    if set != run_marks {
                        flush(&mut run, &run_marks, &mut out);
                        run_marks = set;
                    }
                    run.push(*c);
                }
                Item::Atom(node) => {
                    flush(&mut run, &run_marks, &mut out);
                    out.push(node.mark(set));
                }
                Item::Break => {
                    flush(&mut run, &run_marks, &mut out);
                    if let Some(ty) = line_break
                        && let Ok(node) = schema.create(ty, Attrs::empty(), set, Fragment::empty())
                    {
                        out.push(node);
                    }
                }
            }
        }
        flush(&mut run, &run_marks, &mut out);
        out
    }
}

fn push_node(schema: &Schema, node: &Node, out: &mut Vec<Item>) {
    if let Some(text) = node.text() {
        out.extend(text.chars().map(Item::Char));
    } else if schema.node_id(md::LINE_BREAK) == Some(node.type_id()) {
        out.push(Item::Break);
    } else if node.is_container() && !schema.node_type(node.type_id()).is_atom() {
        for child in node.children() {
            push_node(schema, child, out);
        }
    } else {
        out.push(Item::Atom(node.mark(MarkSet::empty())));
    }
}

/// How an atom is written in a file: the spelling a reader takes for it.
pub(crate) fn atom_spelling(schema: &Schema, node: &Node) -> String {
    let attr = |name: &str| {
        node.attrs()
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    match schema.node_type(node.type_id()).name() {
        md::IMAGE => image_spelling(node.attrs()),
        md::WIKI_LINK => crate::wiki::WikiLink {
            target: attr("target"),
            alias: attr("alias"),
            embed: node
                .attrs()
                .get("embed")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }
        .source(),
        md::RAW_INLINE => attr("source"),
        _ => OBJECT_REPLACEMENT.to_string(),
    }
}

/// How an image atom is written: the `<img>` tag it was read from, or else a
/// Markdown image.
pub(crate) fn image_spelling(attrs: &Attrs) -> String {
    let attr = |name: &str| {
        attrs
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
    };
    if !attr("source").is_empty() {
        return attr("source").to_string();
    }
    format!(
        "![{}]({}{})",
        crate::escape::escape_label(attr("alt")),
        crate::escape::link_destination(attr("src")),
        crate::escape::link_title(attr("title")),
    )
}

/// The atom nodes the reported spans stand for, with the item ranges they
/// replace, in order and disjoint: a span overlapping an earlier one is left
/// for the next round. A span whose type the schema lacks, or whose
/// attributes it rejects, stays text.
fn atom_nodes(schema: &Schema, atoms: &[AtomSpan]) -> Vec<(std::ops::Range<usize>, Node)> {
    let mut folded_to = 0;
    atoms
        .iter()
        .filter(|atom| {
            let disjoint = atom.range.start >= folded_to;
            if disjoint {
                folded_to = atom.range.end;
            }
            disjoint
        })
        .filter_map(|atom| {
            let ty = schema.node_id(atom.node_type)?;
            let node = schema
                .create(ty, atom.attrs.clone(), MarkSet::empty(), Fragment::empty())
                .ok()?;
            Some((atom.range.clone(), node))
        })
        .collect()
}

/// The content a textblock of `kind` holding `source` is: its atoms folded,
/// its guard backslashes in place and its marks derived.
///
/// `source` is inline Markdown with `\n` line endings and no atoms; this is
/// what the parser reads a block into and what imported content is spelled
/// back through.
pub(crate) fn build(schema: &Schema, kind: BlockKind, source: &str) -> Vec<Node> {
    let ctx = DeriveContext::new();
    let mut items = Items::from_source(source);
    items.atomize(schema, kind, &ctx);
    items.guard(schema, kind);
    let derived = derive(kind, &items.text(), &ctx);
    items.nodes(schema, &derived)
}

/// The mark types this kind derives from the text. Every other mark type is
/// left as it is found.
pub(crate) fn derived_mark_types(schema: &Schema) -> Vec<MarkTypeId> {
    [
        md::LINK,
        md::UNDERLINE,
        md::STRIKETHROUGH,
        md::STRONG,
        md::EM,
        md::CODE,
        md::SYNTAX,
    ]
    .into_iter()
    .filter_map(|name| schema.mark_id(name))
    .collect()
}

/// The mark a style is.
pub(crate) fn style_mark(schema: &Schema, style: &Style) -> Option<Mark> {
    let ty = schema.mark_id(style.mark_name())?;
    let given = match style {
        Style::Link { href, title } => attrs! {"href" => href.clone(), "title" => title.clone()},
        _ => Attrs::empty(),
    };
    let attrs = schema.build_mark_attrs(ty, &given).ok()?;
    Some(Mark::with_attrs(ty, attrs))
}

/// The `syntax` mark on a concealed run: its span id and what it displays.
pub(crate) fn syntax_mark(schema: &Schema, span: u32, display: &str) -> Option<Mark> {
    let ty = schema.mark_id(md::SYNTAX)?;
    let given = attrs! {
        "span" => AttrValue::Int(i64::from(span)),
        "display" => display.to_string(),
    };
    let attrs = schema.build_mark_attrs(ty, &given).ok()?;
    Some(Mark::with_attrs(ty, attrs))
}

/// The derived marks over each of `len` positions.
pub(crate) fn derived_marks(schema: &Schema, derived: &Derived, len: usize) -> Vec<MarkSet> {
    let mut out = vec![MarkSet::empty(); len];
    for span in &derived.styles {
        let Some(mark) = style_mark(schema, &span.style) else {
            continue;
        };
        for set in &mut out[span.range.start.min(len)..span.range.end.min(len)] {
            *set = set.add(schema, mark.clone());
        }
    }
    for conceal in &derived.conceals {
        let Some(mark) = syntax_mark(schema, conceal.span, &conceal.display) else {
            continue;
        };
        for set in &mut out[conceal.range.start.min(len)..conceal.range.end.min(len)] {
            *set = set.add(schema, mark.clone());
        }
    }
    out
}

/// The text of every raw block in `doc` that could hold link reference
/// definitions, in document order: the ones that start with `[`.
///
/// Cheap to compare, so a caller can tell whether a transaction changed the
/// definitions before it pays for [`document_context`].
pub(crate) fn definition_candidates(schema: &Schema, doc: &Node) -> Vec<String> {
    let Some(raw) = schema.node_id(md::RAW_BLOCK) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    doc.descendants(&mut |node, _, _, _| {
        if node.type_id() == raw {
            let text: String = node.children().filter_map(|leaf| leaf.text()).collect();
            if text.trim_start().starts_with('[') {
                out.push(text);
            }
            return false;
        }
        // Nothing inside a textblock is a block.
        !node.is_textblock(schema)
    });
    out
}

/// What the textblocks of a document holding `candidates` — see
/// [`definition_candidates`] — are read against: the link reference
/// definitions among them, so `[a][ref]` resolves as it does in the file.
///
/// A definition may stand anywhere in a document and still apply to all of
/// it; the first of two with one label wins, which the order keeps.
pub(crate) fn definitions_context(candidates: &[String]) -> DeriveContext {
    let definitions: Vec<&str> = candidates
        .iter()
        .map(String::as_str)
        .filter(|text| reads_as_definitions(text))
        .collect();
    DeriveContext::new().with_definitions(definitions.join("\n\n"))
}

/// The context `doc`'s textblocks are read against.
pub(crate) fn document_context(schema: &Schema, doc: &Node) -> DeriveContext {
    definitions_context(&definition_candidates(schema, doc))
}

/// Whether `block` is a raw block holding nothing but link reference
/// definitions: source that tells links where they go, which a reader never
/// sees as text.
pub fn holds_definitions(schema: &Schema, block: &Node) -> bool {
    schema.node_id(md::RAW_BLOCK) == Some(block.type_id()) && {
        let text: String = block.children().filter_map(|leaf| leaf.text()).collect();
        reads_as_definitions(&text)
    }
}

/// Whether `text` reads as nothing but link reference definitions.
///
/// comrak does not read a definition whose destination is `<>` when nothing
/// follows it, so the text is given the line ending it had in the file.
pub(crate) fn reads_as_definitions(text: &str) -> bool {
    let arena = comrak::Arena::new();
    !text.trim().is_empty()
        && comrak::parse_document(&arena, &format!("{text}\n"), &crate::commonmark_options())
            .first_child()
            .is_none()
}

/// `doc` with every textblock's marks derived against the document's own
/// definitions, which [`build`] could not know while the document was being
/// read.
pub(crate) fn resolve_references(schema: &Schema, doc: &Node) -> Node {
    let ctx = document_context(schema, doc);
    if ctx.definitions().is_empty() {
        return doc.clone();
    }
    rederive(schema, doc, &ctx)
}

fn rederive(schema: &Schema, node: &Node, ctx: &DeriveContext) -> Node {
    if let Some(kind) = block_kind(schema, node.type_id()) {
        let items = Items::from_nodes(schema, node.children());
        let text = items.text();
        if !text.contains(']') {
            return node.clone();
        }
        let derived = derive(kind, &text, ctx);
        return node.copy(Fragment::from_nodes(items.nodes(schema, &derived)));
    }
    if node.is_leaf() || node.is_textblock(schema) {
        return node.clone();
    }
    let children: Vec<Node> = node
        .children()
        .map(|child| rederive(schema, child, ctx))
        .collect();
    node.copy(Fragment::from_nodes(children))
}

/// Whether `block` is a textblock whose first line reads as a callout marker.
pub(crate) fn looks_like_callout(schema: &Schema, block: &Node) -> bool {
    if block_kind(schema, block.type_id()) != Some(BlockKind::Paragraph) {
        return false;
    }
    let text = Items::from_nodes(schema, block.children()).text();
    let first = text.split('\n').next().unwrap_or_default();
    crate::callout::read_callout(first).is_some()
}

/// `block` with a backslash before a first line that would read as a callout
/// marker, rebuilt with its marks derived.
///
/// Only a quote's first line can be one, so this is what keeps an ordinary
/// quote ordinary — the quote's own guard, as [`guard`] is a block's.
pub(crate) fn escape_callout_lookalike(schema: &Schema, block: &Node) -> Node {
    if !looks_like_callout(schema, block) {
        return block.clone();
    }
    let mut items = Items::from_nodes(schema, block.children());
    items.0.insert(0, Item::Char('\\'));
    let derived = derive(BlockKind::Paragraph, &items.text(), &DeriveContext::new());
    block.copy(Fragment::from_nodes(items.nodes(schema, &derived)))
}
