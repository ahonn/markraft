//! Semantic inline content, as the HTML importer builds it up.
//!
//! Adjacent text with equal marks has to end up as one leaf, because
//! [`Node::check`] rejects anything else, and empty text has to be dropped,
//! because a text leaf is never empty. The content built here carries marks
//! but no spelling; it becomes a textblock's source through
//! [`spell`](crate::serialize::spell).

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

    /// The last node built so far.
    pub(crate) fn last(&self) -> Option<&Node> {
        self.nodes.last()
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
        if let Some(last) = self.nodes.last_mut()
            && last.is_text()
            && *last.marks() == marks
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

/// `children` with `mark` added to each. A mark of the same type already on
/// a child is replaced: semantic content has no nesting of its own, which is
/// what spelling it as Markdown adds.
pub(crate) fn wrap_mark(schema: &Schema, mark: Mark, children: Vec<Node>) -> Vec<Node> {
    children
        .into_iter()
        .map(|node| {
            let marks = node.marks().add(schema, mark.clone());
            node.mark(marks)
        })
        .collect()
}

/// The delimiter pair a style mark is spelled with, if it has one.
pub(crate) fn style_delimiters(mark_name: &str) -> Option<(&'static str, &'static str)> {
    match mark_name {
        md::STRONG => Some(("**", "**")),
        md::EM => Some(("*", "*")),
        md::STRIKETHROUGH => Some(("~~", "~~")),
        md::UNDERLINE => Some(("<u>", "</u>")),
        md::HIGHLIGHT => Some(("==", "==")),
        md::SUPERSCRIPT => Some(("^", "^")),
        _ => None,
    }
}

/// The closing `](…)` spelling for a link mark's destination and title.
pub(crate) fn link_closing(href: &str, title: &str) -> String {
    format!(
        "]({}{})",
        crate::escape::link_destination(href),
        crate::escape::link_title(title)
    )
}
