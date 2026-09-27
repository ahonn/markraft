//! Commands for leaving a display formula without splitting its source.

use markraft_core::commands::structure::can_replace_with;
use markraft_core::commands::{Command, command};
use markraft_core::kind::DocTypes;
use markraft_core::projection::Projection;
use markraft_core::protocol::event;
use markraft_core::{Attrs, Change, Fragment, MarkSet, Selection, Slice, TransactionSpec};

use crate::math_spans::formula_spans;

/// Move below a standalone display formula, creating a sibling paragraph only
/// when needed. Keeping the insertion inside its container preserves list and
/// blockquote structure, and leaves the formula's source exactly as written.
pub(crate) fn finish(types: &DocTypes) -> Command {
    let types = types.clone();
    command(move |state| {
        if !state.selection().is_cursor() || types.math.is_none() {
            return None;
        }
        let paragraph_type = types.paragraph?;
        let resolved = state.resolved_head()?;
        let depth = resolved.depth();
        if resolved.parent().type_id() != paragraph_type {
            return None;
        }
        let container_depth = depth.checked_sub(1)?;
        let projection = Projection::of(state.doc(), state.schema());
        let index = projection.line_at(state.head())?;
        let line = projection.line(index)?;
        let source = projection.line_text(index)?;
        if !formula_spans(line, source, &types)
            .iter()
            .any(|span| span.is_standalone(source))
        {
            return None;
        }
        let schema = state.schema();
        let container = resolved.node(container_depth);
        let next_index = resolved.index(container_depth) + 1;
        let after = resolved.after(depth);
        let selection = Selection::cursor(after + 1);
        if next_index < container.child_count()
            && container.child(next_index).type_id() == paragraph_type
            && !projection.line_at(after + 1).is_some_and(|index| {
                let next_line = &projection.lines()[index];
                let next_source = projection.line_text(index).unwrap_or_default();
                formula_spans(next_line, next_source, &types)
                    .iter()
                    .any(|span| span.is_standalone(next_source))
            })
        {
            selection.check(state.doc(), schema).ok()?;
            return Some(
                TransactionSpec::new()
                    .selection(selection)
                    .stored_marks(Some(MarkSet::empty()))
                    .user_event(event::SELECT)
                    .scroll_into_view(),
            );
        }
        if !can_replace_with(schema, container, next_index, next_index, &[paragraph_type]) {
            return None;
        }
        let paragraph = schema
            .create(
                paragraph_type,
                Attrs::empty(),
                MarkSet::empty(),
                Fragment::empty(),
            )
            .ok()?;
        let spec = TransactionSpec::new().changes(vec![Change::replace(
            after,
            after,
            Slice::from_fragment(Fragment::from_node(paragraph)),
        )]);
        let applied = state.update([spec]).ok()?;
        applied.new_doc().check(schema).ok()?;
        selection.check(applied.new_doc(), schema).ok()?;
        Some(
            TransactionSpec::new()
                .change_set(applied.changes().clone())
                .selection(selection)
                .stored_marks(Some(MarkSet::empty()))
                .user_event(event::INPUT)
                .scroll_into_view(),
        )
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    };
    use markraft_core::{EditorState, EditorStateConfig};

    fn open(source: &str, line_index: usize, offset: usize) -> (EditorState, DocTypes) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).unwrap();
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let projection = Projection::of(&doc, &schema);
        let cursor = projection.line_offset_to_pos(line_index, offset).unwrap();
        let state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(doc)
                .extensions(commonmark_extensions(&schema))
                .selection(Selection::cursor(cursor)),
        )
        .unwrap();
        (state, types)
    }

    fn apply(state: &EditorState, types: &DocTypes) -> EditorState {
        state
            .update([finish(types)(state).expect("command applies")])
            .unwrap()
            .state()
            .clone()
    }

    #[test]
    fn existing_following_paragraph_is_reused_without_changing_formula() {
        let (state, types) = open("$$\nE=mc^2\n$$\n\nFollowing text", 0, 5);
        let result = apply(&state, &types);
        assert_eq!(result.doc(), state.doc());
        assert_eq!(result.head(), state.doc().child(0).node_size() + 1);
    }

    #[test]
    fn final_formula_gets_an_empty_paragraph_without_source_changes() {
        let (state, types) = open("$$\n\\frac{α}{2}\n$$", 0, 6);
        let formula = state.doc().child(0).clone();
        let result = apply(&state, &types);
        assert_eq!(result.doc().child_count(), 2);
        assert_eq!(result.doc().child(0), &formula);
        assert_eq!(result.doc().child(1).content_size(), 0);
        assert_eq!(result.head(), formula.node_size() + 1);
    }

    #[test]
    fn adjacent_display_formulas_get_an_empty_paragraph_between_them() {
        let (state, types) = open("$$x$$\n\n$$y$$", 0, 3);
        let result = apply(&state, &types);
        assert_eq!(result.doc().child_count(), 3);
        assert_eq!(result.doc().child(0), state.doc().child(0));
        assert_eq!(result.doc().child(1).content_size(), 0);
        assert_eq!(result.doc().child(2), state.doc().child(1));
        assert_eq!(result.head(), state.doc().child(0).node_size() + 1);
    }

    #[test]
    fn newly_opened_empty_formula_can_be_finished() {
        let (state, types) = open("$$", 0, 2);
        let entered = markraft_commonmark::block_from_line()(&state).unwrap();
        let state = state.update([entered]).unwrap().state().clone();
        let result = apply(&state, &types);
        assert_eq!(result.doc().child(0), state.doc().child(0));
        assert_eq!(result.doc().child_count(), 2);
    }

    #[test]
    fn newly_typed_multiline_formula_can_be_finished_and_followed_by_text() {
        let (mut state, types) = open("$$", 0, 2);
        for command in [
            markraft_commonmark::block_from_line(),
            markraft_core::commands::insert_text("x^2"),
            markraft_commonmark::block_from_line(),
            markraft_core::commands::insert_text("+ y^2"),
        ] {
            state = state
                .update([command(&state).expect("editing applies")])
                .unwrap()
                .state()
                .clone();
        }
        let before = Projection::of(state.doc(), state.schema())
            .plain_text()
            .to_owned();
        let finished = apply(&state, &types);
        let typed = markraft_core::commands::insert_text("After")(&finished).unwrap();
        let result = finished.update([typed]).unwrap();
        let projected = Projection::of(result.new_doc(), state.schema());
        assert_eq!(projected.line_text(0), Some(before.as_str()));
        assert_eq!(projected.line_text(1), Some("After"));
        assert_eq!(
            markraft_commonmark::to_markdown(state.schema(), result.new_doc()),
            "$$\nx^2\n+ y^2\n$$\n\nAfter"
        );
    }

    #[test]
    fn inline_code_currency_and_noncollapsed_selection_are_ignored() {
        for source in [
            "before $x$ after",
            "`$$x$$`",
            "```tex\n$$x$$\n```",
            "costs $5 and $10",
            "$$unfinished",
            "$$x$$ and $$y$$",
        ] {
            let (state, types) = open(source, 0, 1);
            assert!(finish(&types)(&state).is_none(), "{source}");
        }
        let (state, types) = open("$$x$$", 0, 2);
        let selected = state
            .update([TransactionSpec::new().selection(Selection::text(2, 4))])
            .unwrap();
        assert!(finish(&types)(selected.state()).is_none());
    }

    #[test]
    fn finishing_inside_quote_stays_inside_quote() {
        let (state, types) = open("> $$x$$\n\nOutside", 0, 3);
        let result = apply(&state, &types);
        let quote = result.doc().child(0);
        assert_eq!(result.doc().child_count(), 2);
        assert_eq!(quote.child_count(), 2);
        assert_eq!(quote.child(0), state.doc().child(0).child(0));
        assert_eq!(quote.child(1).type_id(), types.paragraph.unwrap());
        assert_eq!(result.resolved_head().unwrap().depth(), 2);
    }
}
