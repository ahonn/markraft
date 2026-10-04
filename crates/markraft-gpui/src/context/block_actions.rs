//! Block insertion for context menus, independent of pointer selection changes.

use std::ops::Range;

use gpui::Context;
use markraft_core::{
    Attrs, Change, Fragment, MarkSet, Selection, Slice, TransactionSpec,
    commands::{changes_spec, structure::can_replace_with},
};

use super::{ContextRequest, ContextTarget};
use crate::EditorView;

impl EditorView {
    /// Whether a blank paragraph fits beside the clicked object or selected block.
    /// The host additionally applies its read-only policy.
    pub fn can_insert_context_paragraph(&self, request: &ContextRequest, before: bool) -> bool {
        self.context_paragraph_spec(request, before).is_some()
    }

    /// Insert without replacing the selected text, then put the caret in the new
    /// paragraph. The complete operation is one isolated undo event.
    pub fn insert_context_paragraph(
        &mut self,
        request: &ContextRequest,
        before: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(spec) = self.context_paragraph_spec(request, before) else {
            return false;
        };
        self.dispatch_isolated([spec], cx)
    }

    fn context_paragraph_spec(
        &self,
        request: &ContextRequest,
        before: bool,
    ) -> Option<TransactionSpec> {
        if self.single_line || self.is_composing() || !self.context_is_current(request) {
            return None;
        }
        let range = self.context_block_range(request)?;
        let position = if before { range.start } else { range.end };
        let schema = self.state.schema();
        let paragraph = self.types.paragraph?;
        let resolved = self.state.doc().resolve(position).ok()?;
        let index = resolved.index(resolved.depth());
        if !can_replace_with(schema, resolved.parent(), index, index, &[paragraph]) {
            return None;
        }
        let node = schema
            .create(
                paragraph,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
            )
            .ok()?;
        changes_spec(
            &self.state,
            vec![Change::insert(
                position,
                Slice::from_fragment(Fragment::from_node(node)),
            )],
            "input.block",
        )
        .map(|spec| spec.selection(Selection::cursor(position + 1)))
    }

    fn context_block_range(&self, request: &ContextRequest) -> Option<Range<usize>> {
        let doc = self.state.doc();
        // A clicked block wins over a selection elsewhere. Inline objects are
        // handled through their containing textblock, never by splitting it.
        let position = match &request.target {
            ContextTarget::Table { pos, .. } => {
                let node = doc.node_at(*pos)?;
                return (Some(node.type_id()) == self.types.table)
                    .then_some(*pos..*pos + node.node_size());
            }
            ContextTarget::CodeBlock { pos } => {
                let node = doc.node_at(*pos)?;
                return (Some(node.type_id()) == self.types.code_block)
                    .then_some(*pos..*pos + node.node_size());
            }
            ContextTarget::Image { pos }
            | ContextTarget::WikiLink { pos, .. }
            | ContextTarget::Math { pos } => *pos,
            ContextTarget::Link { range, .. } => range.start,
            ContextTarget::Text => {
                let range = request.selection_range();
                let first = self.containing_context_block(range.start)?;
                if !range.is_empty()
                    && first != self.containing_context_block(range.end.saturating_sub(1))?
                {
                    return None;
                }
                range.start
            }
        };
        let range = self.containing_context_block(position)?;
        let resolved = doc.resolve(range.start).ok()?;
        // Paragraph insertion in a table always goes beside the entire table;
        // inserting paragraphs in rows or cells would corrupt its structure.
        for depth in (1..=resolved.depth()).rev() {
            if Some(resolved.node(depth).type_id()) == self.types.table {
                return Some(resolved.before(depth)..resolved.after(depth));
            }
        }
        Some(range)
    }

    fn containing_context_block(&self, position: usize) -> Option<Range<usize>> {
        let doc = self.state.doc();
        let schema = self.state.schema();
        let resolved = doc.resolve(position).ok()?;
        for depth in (1..=resolved.depth()).rev() {
            let node = resolved.node(depth);
            if node.is_textblock(schema) {
                return Some(resolved.before(depth)..resolved.after(depth));
            }
        }
        let node = doc.node_at(position)?;
        node.is_block(schema)
            .then_some(position..position + node.node_size())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocTypes, Setup};
    use gpui::{AppContext, Point, TestAppContext};
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    };
    use markraft_core::history;

    fn setup(source: &str) -> Setup {
        let schema = commonmark_schema();
        Setup::new(schema.clone())
            .types(DocTypes::from_schema_names(
                &schema,
                &commonmark_doc_type_names(),
            ))
            .extensions(commonmark_extensions(&schema))
            .doc(from_markdown(&schema, source).unwrap())
    }

    #[gpui::test]
    fn context_paragraph_insertion_preserves_objects_and_undo_selection(cx: &mut TestAppContext) {
        for (source, table) in [
            ("| 中文 | `a\\|b` |\n| :- | -: |\n| 😀 | z |", true),
            ("```rust\nlet 原文 = 42;\n```", false),
        ] {
            for before in [true, false] {
                let view = cx.new(|cx| EditorView::new(setup(source), cx));
                view.update(cx, |view, cx| {
                    let original = view.state.doc().clone();
                    let selection = view.state.selection().clone();
                    let target = if table {
                        ContextTarget::Table { pos: 0, cell: 3 }
                    } else {
                        ContextTarget::CodeBlock { pos: 0 }
                    };
                    let request = view.context_snapshot(Point::default(), target);
                    assert!(view.can_insert_context_paragraph(&request, before));
                    assert!(view.insert_context_paragraph(&request, before, cx));
                    assert_eq!(view.state.doc().child_count(), 2);
                    assert_eq!(
                        view.state.doc().child(usize::from(before)),
                        original.child(0)
                    );
                    let cursor = if before {
                        1
                    } else {
                        original.content_size() + 1
                    };
                    assert_eq!(view.state.selection(), &Selection::cursor(cursor));
                    assert!(!view.insert_context_paragraph(&request, before, cx));
                    let undo = history::undo(&view.state).unwrap();
                    view.dispatch([undo], cx);
                    assert_eq!(view.state.doc(), &original);
                    assert_eq!(view.state.selection(), &selection);
                });
            }
        }
    }

    #[gpui::test]
    fn context_paragraph_insertion_stays_inside_quote_and_list_item(cx: &mut TestAppContext) {
        for source in ["> ```rust\n> a\n> ```", "- item\n\n  ```rust\n  a\n  ```"] {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.update(cx, |view, cx| {
                let code = view.types.code_block.unwrap();
                let pos = (0..view.state.doc().content_size())
                    .find(|pos| {
                        view.state
                            .doc()
                            .node_at(*pos)
                            .is_some_and(|n| n.type_id() == code)
                    })
                    .unwrap();
                let original = view.state.doc().clone();
                let request =
                    view.context_snapshot(Point::default(), ContextTarget::CodeBlock { pos });
                assert!(view.insert_context_paragraph(&request, false, cx));
                assert_eq!(view.state.doc().child_count(), 1);
                assert_eq!(view.state.doc().node_at(pos), original.node_at(pos));
                assert_eq!(view.state.doc().content_size(), original.content_size() + 2);
                let undo = history::undo(&view.state).unwrap();
                view.dispatch([undo], cx);
                assert_eq!(view.state.doc(), &original);
            });
        }
    }

    #[gpui::test]
    fn context_paragraph_insertion_keeps_selected_text_and_escapes_table_cells(
        cx: &mut TestAppContext,
    ) {
        for (source, from, to) in [
            ("**selected** text", 3, 7),
            ("| a | b |\n| - | - |\n| x | y |", 3, 4),
        ] {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.update(cx, |view, cx| {
                view.select_range(from, to, cx);
                let original = view.state.doc().clone();
                let request = view.context_snapshot(Point::default(), ContextTarget::Text);
                assert!(view.insert_context_paragraph(&request, false, cx));
                assert_eq!(view.state.doc().child_count(), 2);
                assert_eq!(view.state.doc().child(0), original.child(0));
                assert_eq!(
                    view.state.selection(),
                    &Selection::cursor(original.content_size() + 1)
                );
            });
        }
    }

    #[gpui::test]
    fn context_paragraph_insertion_rejects_cross_block_and_stale_requests(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup("first\n\nsecond"), cx));
        view.update(cx, |view, cx| {
            view.select_range(2, 10, cx);
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            assert!(!view.can_insert_context_paragraph(&request, true));
            view.select_range(2, 4, cx);
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            assert!(view.can_insert_context_paragraph(&request, true));
            view.select(3, false, cx);
            assert!(!view.insert_context_paragraph(&request, true, cx));
            assert_eq!(history::undo_depth(&view.state), 0);
        });
    }
}
