//! What the selection is formatted as, for a host drawing a toolbar.

use crate::conceal::{Reveal, Shown};
use markraft_core::projection::{Line, Projection};
use markraft_core::{Attrs, EditorState, MarkSet, MarkTypeId, NodeTypeId};

/// The marks every part of the selection carries, or the marks new text would
/// get at a cursor.
///
/// A cursor answers the state's stored marks when it has them and the marks of the
/// content it sits in otherwise, which is what makes a toolbar light up the
/// moment ⌘B is pressed in an empty paragraph. A range answers the intersection
/// of the mark sets of the inline content it actually covers: a block whose
/// covered stretch is empty contributes nothing, so selecting to the start of
/// the next block does not clear the toolbar.
///
/// What a reader sees is what counts. A run the kind conceals with nothing to
/// display — a delimiter, an escape's backslash — is spelling, not content,
/// so it is left out of the intersection; and the conceal role `syntax` is
/// never reported as a format of its own.
pub(crate) fn active_marks(state: &EditorState, syntax: Option<MarkTypeId>) -> MarkSet {
    let without_role = |marks: MarkSet| match syntax {
        Some(role) => marks.filter(|mark| mark.ty != role),
        None => marks,
    };
    let doc = state.doc();
    let selection = state.selection();
    if selection.is_cursor() {
        if let Some(marks) = state.stored_marks() {
            return without_role(marks.clone());
        }
        return doc
            .resolve(selection.head(doc))
            .map(|resolved| without_role(resolved.marks(state.schema())))
            .unwrap_or_else(|_| MarkSet::empty());
    }
    let (from, to) = (selection.from(doc), selection.to(doc));
    let mut common: Option<MarkSet> = None;
    let projection = markraft_core::projection::projection_of(state);
    for line in projection.lines() {
        if line.to() <= from || line.from() >= to {
            continue;
        }
        let shown = crate::conceal::shown(syntax, line, &Reveal::nothing());
        for (run, shown) in line.runs().iter().zip(shown) {
            if line.abs(run.start).max(from) >= line.abs(run.end).min(to) || shown == Shown::Hidden
            {
                continue;
            }
            common = Some(match common.take() {
                None => run.marks.clone(),
                Some(previous) => previous.filter(|mark| run.marks.contains(mark)),
            });
        }
    }
    common.map(without_role).unwrap_or_else(MarkSet::empty)
}

/// The type and attributes every textblock the selection touches shares, or
/// `None` when they differ.
///
/// A selection ending exactly at the start of a following block does not count
/// that block, matching what [`set_block_type`](markraft_core::commands::set_block_type)
/// would change.
pub(crate) fn active_block_type(
    state: &EditorState,
    projection: &Projection,
) -> Option<(NodeTypeId, Attrs)> {
    let mut lines = touched_lines(state, projection).peekable();
    let first = lines.next()?;
    let own = first.ancestors().last()?;
    let (ty, attrs) = (own.node_type, own.attrs.clone());
    lines
        .all(|line| {
            line.ancestors()
                .last()
                .is_some_and(|other| other.node_type == ty && other.attrs == attrs)
        })
        .then_some((ty, attrs))
}

/// The lines a block-level command would act on.
pub(crate) fn touched_lines<'a>(
    state: &EditorState,
    projection: &'a Projection,
) -> impl Iterator<Item = &'a Line> {
    let doc = state.doc();
    let selection = state.selection();
    let (from, to) = (selection.from(doc), selection.to(doc));
    let first = projection.line_at(from).unwrap_or(0);
    let last = match projection.line_at(to) {
        // A range that stops at a later block's start leaves that block alone.
        Some(index) if index > first && projection.lines()[index].pos_to_offset(to) == Some(0) => {
            index - 1
        }
        Some(index) => index,
        None => projection.line_count().saturating_sub(1),
    };
    projection.lines()[first.min(last)..=last].iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use markraft_commonmark::{commonmark_schema, from_markdown, schema as md};
    use markraft_core::commands::{run_command, toggle_mark};
    use markraft_core::projection::projection_of;
    use markraft_core::{Attrs, EditorStateConfig, Extension, Schema, Selection};

    fn state_of(source: &str) -> (Schema, EditorState) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        let state =
            EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
                Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::history::history(Default::default()),
                ]),
            ))
            .expect("a valid state");
        (schema, state)
    }

    /// Two strong paragraphs with an empty one between them. The Markdown reader
    /// folds blank lines away, so this shape has to be built.
    fn empty_middle_state() -> (Schema, EditorState) {
        let schema = commonmark_schema();
        let strong = schema.mark_id(md::STRONG).unwrap();
        let bold = |text: &str| {
            let marks = MarkSet::from_marks(&schema, [markraft_core::Mark::new(strong)]);
            schema
                .node(md::PARAGRAPH, [schema.text_marked(text, marks)])
                .expect("a paragraph")
        };
        let doc = schema
            .doc([
                bold("first"),
                schema.node(md::PARAGRAPH, []).expect("an empty paragraph"),
                bold("last"),
            ])
            .expect("a valid document");
        let state =
            EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
                Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::history::history(Default::default()),
                ]),
            ))
            .expect("a valid state");
        (schema, state)
    }

    fn select(state: &EditorState, anchor: usize, head: usize) -> EditorState {
        state
            .update(
                [markraft_core::TransactionSpec::new().selection(Selection::text(anchor, head))],
            )
            .expect("a selection")
            .state()
            .clone()
    }

    fn syntax() -> Option<markraft_core::MarkTypeId> {
        commonmark_schema().mark_id(md::SYNTAX)
    }

    fn has(marks: &MarkSet, schema: &Schema, name: &str) -> bool {
        schema
            .mark_id(name)
            .is_some_and(|ty| marks.contains_type(ty))
    }

    #[test]
    fn a_cursor_reports_stored_marks_without_changing_the_document() {
        let (schema, state) = state_of("");
        let strong = schema.mark_id(md::STRONG).unwrap();
        let code = schema.mark_id(md::CODE).unwrap();
        let state = run_command(&state, &toggle_mark(strong, Attrs::empty()))
            .expect("strong applies")
            .expect("a transaction")
            .state()
            .clone();
        let state = run_command(&state, &toggle_mark(code, Attrs::empty()))
            .expect("code applies")
            .expect("a transaction")
            .state()
            .clone();
        let marks = active_marks(&state, syntax());
        assert!(has(&marks, &schema, md::STRONG));
        assert!(has(&marks, &schema, md::CODE));
        assert_eq!(state.doc().content_size(), 2);
    }

    #[test]
    fn a_range_intersects_only_the_content_it_covers_in_either_direction() {
        // The delimiters are in the projection; select by finding the
        // visible letter runs rather than assuming one token per character.
        let (schema, state) = state_of("***ab***cd*ef*");
        let em = schema.mark_id(md::EM).unwrap();
        let strong = schema.mark_id(md::STRONG).unwrap();
        let projection = projection_of(&state);
        let plain = projection.plain_text();
        let ab = plain.find("ab").expect("ab");
        let cd = plain.find("cd").expect("cd");
        let ef = plain.find("ef").expect("ef");
        let pos = |offset| projection.line_offset_to_pos(0, offset).unwrap();
        let both = select(&state, pos(ab), pos(ab + 2));
        assert!(active_marks(&both, syntax()).contains_type(em));
        assert!(active_marks(&both, syntax()).contains_type(strong));
        assert!(
            active_marks(&select(&state, pos(ab + 2), pos(ab)), syntax()).contains_type(strong)
        );
        assert!(
            !active_marks(&select(&state, pos(ab), pos(cd + 2)), syntax()).contains_type(strong)
        );
        assert!(active_marks(&select(&state, pos(ef), pos(ef + 2)), syntax()).contains_type(em));
        assert!(active_marks(&select(&state, pos(ef + 2), pos(ef)), syntax()).contains_type(em));
    }

    /// The characters that spell a style are not a format: a caret just past
    /// an opening delimiter, or a range over a span delimiters and all, reports
    /// the style and never the conceal role itself.
    #[test]
    fn concealed_spelling_is_not_a_format() {
        let (schema, state) = state_of("x **ab** y");
        let strong = schema.mark_id(md::STRONG).unwrap();
        let role = syntax().unwrap();
        let projection = projection_of(&state);
        let pos = |offset| projection.line_offset_to_pos(0, offset).unwrap();
        let caret = select(&state, pos(4), pos(4));
        let marks = active_marks(&caret, syntax());
        assert!(marks.contains_type(strong));
        assert!(!marks.contains_type(role));
        let span = select(&state, pos(2), pos(8));
        let marks = active_marks(&span, syntax());
        assert!(marks.contains_type(strong));
        assert!(!marks.contains_type(role));
        // Only the delimiters: nothing a reader sees is selected.
        let delimiters = select(&state, pos(2), pos(4));
        assert!(active_marks(&delimiters, syntax()).is_empty());
    }

    #[test]
    fn a_range_stopping_at_the_next_block_keeps_the_first_blocks_format() {
        let (schema, state) = state_of("# **你好**\n\nplain");
        let heading = schema.node_id(md::HEADING).unwrap();
        let strong = schema.mark_id(md::STRONG).unwrap();
        let projection = projection_of(&state);
        let para_start = projection.lines()[1].from();
        let to_next_block = select(&state, 1, para_start);
        assert!(active_marks(&to_next_block, syntax()).contains_type(strong));
        assert_eq!(
            active_block_type(&to_next_block, &projection).map(|(ty, _)| ty),
            Some(heading)
        );
        let backwards = select(&state, para_start, 1);
        assert_eq!(
            active_block_type(&backwards, &projection).map(|(ty, _)| ty),
            Some(heading)
        );
        // One character into the paragraph and the two formats differ.
        let into_next = select(&state, 1, para_start + 1);
        assert!(!active_marks(&into_next, syntax()).contains_type(strong));
        assert_eq!(active_block_type(&into_next, &projection), None);
    }

    #[test]
    fn a_selection_stopping_at_a_nested_inline_start_excludes_that_block() {
        let (schema, state) = state_of("# heading\n\n*text **bold***");
        let projection = projection_of(&state);
        let from = projection.line_offset_to_pos(0, 0).unwrap();
        let to = projection.line_offset_to_pos(1, 0).unwrap();
        let selected = select(&state, from, to);
        assert_eq!(
            active_block_type(&selected, &projection).map(|(ty, _)| ty),
            schema.node_id(md::HEADING)
        );
    }

    #[test]
    fn an_empty_block_invents_no_marks_but_still_counts_for_the_block_format() {
        // The Markdown reader folds blank lines away, so the empty paragraph in
        // the middle is built rather than parsed.
        let (schema, state) = empty_middle_state();
        let strong = schema.mark_id(md::STRONG).unwrap();
        let paragraph = schema.node_id(md::PARAGRAPH).unwrap();
        let projection = projection_of(&state);
        let all = select(&state, 0, state.doc().content_size());
        assert!(active_marks(&all, syntax()).contains_type(strong));
        assert_eq!(
            active_block_type(&all, &projection).map(|(ty, _)| ty),
            Some(paragraph)
        );
        // A cursor in the empty middle paragraph carries nothing.
        let empty_line = projection.lines()[1].from();
        let cursor = select(&state, empty_line, empty_line);
        assert!(active_marks(&cursor, syntax()).is_empty());
    }
}
