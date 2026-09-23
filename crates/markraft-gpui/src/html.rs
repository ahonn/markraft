//! Host-owned editing of opaque inline HTML without reparsing its contents.

use crate::EditorView;
use gpui::{Context, Pixels, Point};
use markraft_core::{Change, EditorState, Fragment, Node, NodeTypeId, Slice, TransactionSpec};

fn replace_source(
    state: &EditorState,
    ty: NodeTypeId,
    pos: usize,
    expected: &Node,
    source: &str,
) -> Option<TransactionSpec> {
    let node = state.doc().node_at(pos)?;
    // A stale popover must not overwrite a different atom after another edit.
    if node.type_id() != ty || &node != expected {
        return None;
    }
    let updated = node.with_attrs(node.attrs().with("source", source));
    let change = Change::replace(
        pos,
        pos + node.node_size(),
        Slice::from_fragment(Fragment::from_node(updated)),
    );
    markraft_core::commands::changes_spec(state, vec![change], "format.html")
        .map(|spec| spec.selection(state.selection().clone()))
}

impl EditorView {
    /// Prefer the HTML atom after the caret, then the one immediately before it.
    pub fn raw_html_at_caret(&self) -> Option<usize> {
        let ty = self.types.raw_inline?;
        let head = self.state.selection().head(self.state.doc());
        raw_html_near(&self.state, ty, head)
    }

    /// The opaque HTML primitive beginning at `pos`, for a host's source editor.
    pub fn raw_html_at(&self, pos: usize) -> Option<Node> {
        let node = self.state.doc().node_at(pos)?;
        (Some(node.type_id()) == self.types.raw_inline).then_some(node)
    }

    /// Commit the source as one undoable edit, retaining the atom's type and marks.
    pub fn set_raw_html_at(
        &mut self,
        pos: usize,
        expected: &Node,
        source: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ty) = self.types.raw_inline else {
            return false;
        };
        let Some(spec) = replace_source(&self.state, ty, pos, expected, source) else {
            return false;
        };
        if expected
            .attrs()
            .get("source")
            .and_then(|value| value.as_str())
            == Some(source)
        {
            return true;
        }
        self.edit(cx, false, vec![spec]).unwrap_or(false)
    }

    pub(crate) fn raw_html_under(&self, point: Point<Pixels>) -> Option<usize> {
        use markraft_core::projection::RunContent;
        for row in &self.layout {
            let line = self.projection.line(row.index)?;
            for run in line.runs() {
                if let RunContent::Atom(node) = &run.content
                    && Some(node.type_id()) == self.types.raw_inline
                    && row
                        .rectangles(
                            row.pos_to_offset(line.abs(run.start))
                                ..row.pos_to_offset(line.abs(run.end)),
                            false,
                        )
                        .iter()
                        .any(|bounds| bounds.contains(&point))
                {
                    return Some(line.abs(run.start));
                }
            }
        }
        None
    }
}

fn raw_html_near(state: &EditorState, ty: NodeTypeId, head: usize) -> Option<usize> {
    let matches = |pos| {
        state
            .doc()
            .node_at(pos)
            .is_some_and(|node| node.type_id() == ty)
    };
    if matches(head) {
        Some(head)
    } else {
        head.checked_sub(1).filter(|pos| matches(*pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_source_edits_preserve_the_atom_and_reject_stale_targets() {
        let state = crate::typeahead::tests::state_of("before <span title=\"old\">after</span>");
        let ty = state.schema().node_id("raw_inline").unwrap();
        let pos = 8;
        let node = state.doc().node_at(pos).unwrap();
        assert_eq!(node.type_id(), ty);
        assert_eq!(raw_html_near(&state, ty, pos), Some(pos));
        assert_eq!(raw_html_near(&state, ty, pos + 1), Some(pos));
        assert_eq!(raw_html_near(&state, ty, pos + 2), None);
        let source = "<span\n title=\"a & b\" data-value=\"**literal**\">";
        let spec = replace_source(&state, ty, pos, &node, source).unwrap();
        let changed = state.update([spec]).unwrap();
        let updated = changed.new_doc().node_at(pos).unwrap();
        assert_eq!(updated.type_id(), node.type_id());
        assert_eq!(updated.marks(), node.marks());
        assert_eq!(
            updated.attrs().get("source").unwrap().as_str(),
            Some(source)
        );
        assert!(replace_source(changed.state(), ty, pos, &node, "lost").is_none());
        assert!(replace_source(&state, ty, 1, &node, "lost").is_none());
        let undo = markraft_core::history::undo(changed.state()).unwrap();
        let restored = changed.state().update([undo]).unwrap();
        assert_eq!(restored.new_doc(), state.doc());
        let redone = restored
            .state()
            .update([markraft_core::history::redo(restored.state()).unwrap()])
            .unwrap();
        assert_eq!(redone.new_doc(), changed.new_doc());
        assert_eq!(
            markraft_commonmark::to_markdown(state.schema(), changed.new_doc()),
            format!("before {source}after</span>"),
        );
    }
}
