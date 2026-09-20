//! Following a wiki link, which only the host can do.
//!
//! The view knows a wiki link is an atom carrying a `target`, an `alias` and an
//! `embed` flag, and it knows what to draw for one. What a target *names* is a
//! question about files, which this crate has no business answering: a click
//! emits [`EditorEvent::WikiLinkClicked`](crate::EditorEvent::WikiLinkClicked)
//! and the host resolves it.

use crate::EditorView;
use gpui::{Pixels, Point};
use markraft_core::Node;

/// A node's attribute, trimmed, or the empty string where it has none.
fn attr<'a>(node: &'a Node, name: &str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
}

/// What a wiki link reads as: its alias, or its target where it has none.
pub(crate) fn wiki_link_label(node: &Node) -> &str {
    match attr(node, "alias") {
        "" => attr(node, "target"),
        alias => alias,
    }
}

/// The target of a wiki link, as the host is asked to follow it.
pub(crate) fn wiki_link_target(node: &Node) -> String {
    attr(node, "target").to_owned()
}

impl EditorView {
    /// The wiki link atom beginning at `pos`.
    pub fn wiki_link_at(&self, pos: usize) -> Option<Node> {
        let node = self.state.doc().node_at(pos)?;
        (Some(node.type_id()) == self.types.wiki_link).then_some(node)
    }

    pub(crate) fn wiki_link_under(&self, point: Point<Pixels>) -> Option<usize> {
        use markraft_core::projection::RunContent;
        for row in &self.layout {
            let line = self.projection.line(row.index)?;
            for run in &line.runs {
                if let RunContent::Atom(node) = &run.content
                    && Some(node.type_id()) == self.types.wiki_link
                    && row
                        .rectangles(
                            row.pos_to_offset(run.from)..row.pos_to_offset(run.to),
                            false,
                        )
                        .iter()
                        .any(|bounds| bounds.contains(&point))
                {
                    return Some(run.from);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wiki_link_reads_as_its_alias_and_falls_back_to_its_target() {
        let state = crate::typeahead::tests::state_of("a [[ Note#H ]] b ![[x.png|Cover]] c");
        let ty = state.schema().node_id("wiki_link").unwrap();
        let mut links = Vec::new();
        state.doc().descendants(&mut |node, _, _, _| {
            if node.type_id() == ty {
                links.push(node.clone());
            }
            true
        });
        assert_eq!(links.len(), 2);
        assert_eq!(wiki_link_label(&links[0]), "Note#H");
        assert_eq!(wiki_link_target(&links[0]), "Note#H");
        assert_eq!(wiki_link_label(&links[1]), "Cover");
        assert_eq!(wiki_link_target(&links[1]), "x.png");
    }
}
