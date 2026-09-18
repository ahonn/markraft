//! The inline content of a textblock, as it is built up.
//!
//! Both importers need the same two things of it: adjacent text with equal
//! marks has to end up as one leaf, because [`Node::check`] rejects anything
//! else, and empty text has to be dropped, because a text leaf is never empty.

use markraft_doc::{Mark, MarkSet, Node, Schema};

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

    pub(crate) fn last(&self) -> Option<&Node> {
        self.nodes.last()
    }

    /// The content built so far, leaving the builder empty.
    pub(crate) fn take(&mut self) -> Vec<Node> {
        std::mem::take(&mut self.nodes)
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
