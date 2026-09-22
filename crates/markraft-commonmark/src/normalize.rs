//! Re-derive Method-B inline structure after local edits.
//!
//! Style marks live as delimiter characters in the text, so an edit to those
//! characters can leave a mark that no longer matches its spelling: delete the
//! `*` that opens `*em*` and the emphasis has to go with it. This correction
//! re-serialises a dirty textblock and parses it back to find out what the
//! characters now say.
//!
//! It only ever changes *marks*. Characters a writer typed are turned into
//! delimiters by the input rules in [`crate::extensions`], which read the text
//! before the caret rather than the whole block — the tree cannot tell a `*`
//! that was typed as emphasis from one written `\*` in the source, and a round
//! trip that reinterpreted the second would quietly rewrite the file.

use markraft_core::projection::OBJECT_REPLACEMENT;
use markraft_core::{
    Change, ChangeRange, Correction, CorrectionContext, Fragment, Mark, MarkSet, MarkTypeId, Node,
    Schema, Slice,
};

use crate::from_markdown;
use crate::inline::is_syntax;
use crate::schema as md;

/// Corrections that keep Method-B delimiter leaves in sync with the text.
pub fn method_b_normalize_corrections(schema: &Schema) -> Vec<Correction> {
    let blocks = [md::PARAGRAPH, md::HEADING, md::TABLE_CELL];
    let mut out = Vec::new();
    for name in blocks {
        let Some(ty) = schema.node_id(name) else {
            continue;
        };
        out.push(Correction::on_content(ty, normalize_textblock));
    }
    out
}

fn normalize_textblock(cx: &CorrectionContext<'_>) -> Vec<Change> {
    let schema = cx.start_state.schema();
    if !edits_characters(cx) || !can_reparse(schema, cx.node) || !needs_normalize(schema, cx.node) {
        return Vec::new();
    }
    let Some(replacement) = reparsed_content(schema, cx.node) else {
        return Vec::new();
    };
    let before = shape_of(schema, cx.node.content());
    let after = shape_of(schema, &replacement);
    // A reparse that only takes the syntax mark off an unpaired delimiter is not
    // a repair: `a****b` — the empty pair a cursor toggle inserts — reads back as
    // plain text, and replacing it would throw away the leaves the next keystroke
    // is about to fill. Only a changed *style* shape is worth a change.
    if before.style == after.style {
        return Vec::new();
    }
    // This correction re-derives style marks; it never edits prose and never
    // touches another kind of mark. A round trip that came back with different
    // characters lost something Markdown cannot hold — a space at the end of a
    // paragraph, which is what a writer has under the caret mid-sentence — and
    // one that moved a link rewrote more than it was asked to. Neither result is
    // usable here.
    if before.text != after.text || before.other != after.other {
        return Vec::new();
    }
    let from = cx.content_start;
    let to = from + cx.node.content_size();
    vec![Change::replace(from, to, Slice::from_fragment(replacement))]
}

/// Whether this transaction changed characters *in this block*.
///
/// Only characters can change what a delimiter spells, so a transaction that
/// only moved marks around has nothing here to re-derive — and re-deriving
/// anyway would let the round trip rewrite a shape Markdown cannot hold, such as
/// a link that now ends inside a bold span. It also keeps the reparse off the
/// blocks an edit never reached.
fn edits_characters(cx: &CorrectionContext<'_>) -> bool {
    let schema = cx.start_state.schema();
    let start = cx.content_start;
    let end = start + cx.node.content_size();
    let text_of = |slice: &Slice| markraft_core::projection::slice_to_plain_text(schema, slice);
    cx.tr
        .changes()
        .iter_changes()
        .iter()
        .any(|change| match change {
            ChangeRange::Marked { .. } => false,
            ChangeRange::Replaced {
                from_a,
                to_a,
                from_b,
                to_b,
                inserted,
            } => {
                if *from_b > end || *to_b < start {
                    return false;
                }
                // A mark command rewrites the nodes it covers, so a replacement
                // is not proof that any character moved. Compare the text.
                match cx.start_state.doc().slice(*from_a, *to_a) {
                    Ok(replaced) => text_of(&replaced) != text_of(inserted),
                    Err(_) => true,
                }
            }
        })
}

/// What a block reads as, split into the parts this correction may change and
/// the parts it may not.
///
/// One entry per character in each list, so two shapes line up character for
/// character whatever their delimiter leaves are marked as.
#[derive(PartialEq, Eq)]
struct Shape {
    /// Every character, delimiters included.
    text: Vec<char>,
    /// The style marks over each character: what a delimiter spells, and so the
    /// only thing a reparse is allowed to decide.
    style: Vec<Vec<MarkTypeId>>,
    /// Every other mark over each character — a link, an underline — which a
    /// reparse must give back exactly as it found them.
    other: Vec<Vec<Mark>>,
}

fn shape_of(schema: &Schema, content: &Fragment) -> Shape {
    fn walk(schema: &Schema, content: &Fragment, inherited: &MarkSet, out: &mut Shape) {
        let syntax = schema.mark_id(md::SYNTAX);
        let styles: Vec<MarkTypeId> = [md::STRONG, md::EM, md::STRIKETHROUGH, md::CODE]
            .into_iter()
            .filter_map(|name| schema.mark_id(name))
            .collect();
        for node in content.iter() {
            let marks = node
                .marks()
                .iter()
                .fold(inherited.clone(), |set, mark| set.add(schema, mark.clone()));
            if node.is_container() {
                walk(schema, node.content(), &marks, out);
                continue;
            }
            let mut style: Vec<MarkTypeId> = marks
                .iter()
                .map(|mark| mark.ty)
                .filter(|ty| styles.contains(ty))
                .collect();
            style.sort_unstable();
            let mut other: Vec<Mark> = marks
                .iter()
                .filter(|mark| Some(mark.ty) != syntax && !styles.contains(&mark.ty))
                .cloned()
                .collect();
            other.sort_by_key(|mark| mark.ty);
            let text: Vec<char> = match node.text() {
                Some(text) => text.chars().collect(),
                None => vec![OBJECT_REPLACEMENT],
            };
            for character in text {
                out.text.push(character);
                out.style.push(style.clone());
                out.other.push(other.clone());
            }
        }
    }
    let mut out = Shape {
        text: Vec::new(),
        style: Vec::new(),
        other: Vec::new(),
    };
    walk(schema, content, &MarkSet::empty(), &mut out);
    out
}

/// Whether this textblock is safe to round-trip through Markdown for Method-B
/// repair. Atoms and other opaque inlines do not survive an unescaped serialize
/// + parse without corruption (HTML source whitespace, wiki targets, …).
fn can_reparse(schema: &Schema, block: &Node) -> bool {
    fn ok(schema: &Schema, node: &Node) -> bool {
        if node.is_text() {
            return true;
        }
        let name = schema.node_type(node.type_id()).name();
        if name == md::SOFT_BREAK || name == md::HARD_BREAK {
            return true;
        }
        if name == md::INLINE_SPAN {
            return node.children().all(|child| ok(schema, child));
        }
        false
    }
    block.children().all(|child| ok(schema, child))
}

/// Whether a reparse would do useful Method-B work, rather than disturb
/// ordinary typing (ATX markers, list prefixes, trailing spaces).
fn needs_normalize(schema: &Schema, block: &Node) -> bool {
    let style = [md::STRONG, md::EM, md::STRIKETHROUGH, md::CODE];
    let mut has_syntax = false;
    let mut has_bare_style = false;
    let mut text = String::new();
    for child in block.children() {
        if is_syntax(schema, child) {
            has_syntax = true;
        }
        if let Some(piece) = child.text() {
            text.push_str(piece);
            if !is_syntax(schema, child) {
                for name in style {
                    if schema
                        .mark_id(name)
                        .is_some_and(|ty| child.marks().contains_type(ty))
                    {
                        has_bare_style = true;
                    }
                }
            }
        }
    }
    if has_syntax || has_bare_style {
        return true;
    }
    // Freshly inserted delimiter pairs (toggle_style_mark) still have no marks.
    text.contains("**")
        || text.contains("~~")
        || text.contains('`')
        || text.chars().filter(|&c| c == '*').count() >= 2
}

/// Serialize `block`'s inline content as a one-paragraph document and parse it
/// back, returning the new inline content when that round trip succeeds.
fn reparsed_content(schema: &Schema, block: &Node) -> Option<Fragment> {
    let children: Vec<_> = block.children().cloned().collect();
    let wrapper = schema.node(md::PARAGRAPH, children).ok()?;
    let doc = schema.doc([wrapper]).ok()?;
    // Escaped exactly as a save would write it: text that only *looks* like a
    // delimiter (`a \*not em\* b`) has to stay literal through the round trip,
    // or every edit to the block would reinterpret it. Characters the writer
    // means as delimiters are syntax leaves, which the serializer writes raw.
    let source = crate::commonmark_serializer(schema).serialize(&doc);
    let reparsed = from_markdown(schema, &source).ok()?;
    let para = reparsed.child(0);
    if schema.node_type(para.type_id()).name() != md::PARAGRAPH {
        return None;
    }
    Some(para.content().clone())
}
