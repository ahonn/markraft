//! The inline content of a textblock, as it is built up.
//!
//! Both importers need the same two things of it: adjacent text with equal
//! marks has to end up as one leaf, because [`Node::check`] rejects anything
//! else, and empty text has to be dropped, because a text leaf is never empty.
//!
//! Style marks that have a Markdown spelling are Method-B: the delimiter
//! characters sit in the tree as text carrying [`crate::schema::SYNTAX`], and
//! the style mark covers only the inner content.

use crate::schema as md;
use markraft_core::{Mark, MarkSet, Node, Schema};

pub(crate) struct InlineContent<'s> {
    schema: &'s Schema,
    nodes: Vec<Node>,
}

impl<'s> InlineContent<'s> {
    pub(crate) fn new(schema: &'s Schema) -> InlineContent<'s> {
        InlineContent {
            schema,
            nodes: Vec::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The last node that stands for prose rather than for spelling.
    ///
    /// Whitespace collapsing asks what the text so far ends with, and a
    /// Method-B delimiter leaf is not text so far — `<strong>b </strong> c`
    /// still has a space before the `c` to collapse against the one inside.
    pub(crate) fn last_prose(&self) -> Option<&Node> {
        self.nodes
            .iter()
            .rev()
            .find(|node| !is_syntax(self.schema, node))
    }

    /// The content built so far, leaving the builder empty.
    pub(crate) fn take(&mut self) -> Vec<Node> {
        std::mem::take(&mut self.nodes)
    }

    /// Restore a suspended outer scope without cloning its preceding siblings.
    pub(crate) fn restore(&mut self, nodes: Vec<Node>) {
        self.nodes = nodes;
    }

    pub(crate) fn push_text(&mut self, text: &str, marks: MarkSet) {
        if text.is_empty() {
            return;
        }
        // Method-B delimiter leaves must not merge with each other — `**` next
        // to `` ` `` are two pairs, not one `**`` run.
        let syntax = self.schema.mark_id(md::SYNTAX);
        let mergeable = syntax.is_none_or(|ty| marks.get(ty).is_none());
        if mergeable
            && let Some(last) = self.nodes.last_mut()
            && last.is_text()
            && *last.marks() == marks
            && syntax.is_none_or(|ty| last.marks().get(ty).is_none())
        {
            let joined = format!("{}{text}", last.text().unwrap_or_default());
            *last = last.with_text(&joined);
            return;
        }
        self.nodes.push(self.schema.text_marked(text, marks));
    }

    pub(crate) fn push_node(&mut self, node: Node, marks: MarkSet) {
        self.nodes.push(node.mark(marks));
    }

    /// Build a mark set from a list, letting the schema's exclusion rules apply.
    pub(crate) fn mark_set(&self, marks: impl IntoIterator<Item = Mark>) -> MarkSet {
        MarkSet::from_marks(self.schema, marks)
    }

    /// Drop the spaces and tabs at the end of the content, and any text leaf
    /// they leave empty. A block's trailing whitespace is not content.
    pub(crate) fn trim_end(&mut self) {
        while let Some(last) = self.nodes.last_mut() {
            let Some(text) = last.text() else { break };
            let trimmed = text.trim_end_matches([' ', '\t']);
            if trimmed.len() == text.len() {
                break;
            }
            if trimmed.is_empty() {
                self.nodes.pop();
                continue;
            }
            *last = last.with_text(trimmed);
            break;
        }
    }
}

/// Whether `node` is a Method-B delimiter leaf.
pub(crate) fn is_syntax(schema: &Schema, node: &Node) -> bool {
    schema
        .mark_id(md::SYNTAX)
        .is_some_and(|ty| node.marks().get(ty).is_some())
}

/// A text leaf that holds a Method-B delimiter run.
pub(crate) fn syntax_text(schema: &Schema, text: &str) -> Result<Node, markraft_core::NodeError> {
    let Some(ty) = schema.mark_id(md::SYNTAX) else {
        return Ok(schema.text(text));
    };
    let mark = Mark::with_attrs(ty, markraft_core::attrs! {"delim" => text});
    Ok(schema.text_marked(text, MarkSet::from_marks(schema, [mark])))
}

/// Keep flat marks when every child can carry `mark` on the same leaf. A
/// repeated mark, a container child, or an empty label needs a real
/// [`INLINE_SPAN`](crate::schema::INLINE_SPAN).
///
/// Method-B stores nesting in delimiter leaves, so an inner style with a
/// *lower* rank (strong inside em) is still flat: both marks sit on the inner
/// text and the delimiters spell the order. Style marks also land on delimiter
/// leaves so serialize closes wrapping marks (e.g. a link) before writing the
/// outer style's closing characters.
pub(crate) fn wrap_mark(
    schema: &Schema,
    mark: Mark,
    children: Vec<Node>,
) -> Result<Vec<Node>, markraft_core::NodeError> {
    let flat = !children.is_empty()
        && children.iter().all(|node| {
            is_syntax(schema, node)
                || (!node.is_container() && node.marks().iter().all(|inner| inner.ty != mark.ty))
        });
    if flat || schema.node_id(crate::schema::INLINE_SPAN).is_none() {
        return Ok(children
            .into_iter()
            .map(|node| {
                let marks = node.marks().add(schema, mark.clone());
                node.mark(marks)
            })
            .collect());
    }
    let ty = schema
        .node_id(crate::schema::INLINE_SPAN)
        .expect("checked above");
    Ok(vec![schema.create(
        ty,
        markraft_core::Attrs::empty(),
        MarkSet::from_marks(schema, [mark]),
        markraft_core::Fragment::from_nodes(children),
    )?])
}

/// Wrap `children` in Method-B delimiters for a style mark: open syntax, marked
/// content, close syntax. The style mark is added to the delimiter leaves too,
/// so serialize keeps it open across them and closes inner marks first.
///
/// When nesting cannot be flat and an [`INLINE_SPAN`](md::INLINE_SPAN) owns the
/// mark, that span already writes delimiters on serialize — do not sandwich a
/// second pair.
pub(crate) fn wrap_mark_method_b(
    schema: &Schema,
    mark: Mark,
    open: &str,
    close: &str,
    children: Vec<Node>,
) -> Result<Vec<Node>, markraft_core::NodeError> {
    let wrapped = wrap_mark(schema, mark.clone(), children)?;
    if wrapped.len() == 1
        && schema.node_type(wrapped[0].type_id()).name() == md::INLINE_SPAN
        && wrapped[0].marks().get(mark.ty).is_some()
    {
        return Ok(wrapped);
    }
    let open_leaf = syntax_text(schema, open)?;
    let close_leaf = syntax_text(schema, close)?;
    let open_leaf = open_leaf.mark(open_leaf.marks().add(schema, mark.clone()));
    let close_leaf = close_leaf.mark(close_leaf.marks().add(schema, mark));
    let mut out = Vec::with_capacity(wrapped.len() + 2);
    out.push(open_leaf);
    out.extend(wrapped);
    out.push(close_leaf);
    Ok(out)
}

/// The fixed Method-B delimiter pair for a style mark name, if it has one.
pub(crate) fn style_delimiters(mark_name: &str) -> Option<(&'static str, &'static str)> {
    match mark_name {
        md::STRONG => Some(("**", "**")),
        md::EM => Some(("*", "*")),
        md::STRIKETHROUGH => Some(("~~", "~~")),
        _ => None,
    }
}
