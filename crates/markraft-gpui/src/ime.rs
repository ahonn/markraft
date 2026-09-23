//! The platform text protocol: marked text, selected text, and the two
//! replacement calls, all over the projection's UTF-16 conversions.
//!
//! A composition lives in the document as ordinary content with a marked range
//! the [`composition`](markraft_core::composition::composition) extension keeps; nothing here
//! holds an uncommitted buffer of its own.

use crate::types::DocTypes;
use crate::{EditorView, keymap, single_line};
use gpui::{prelude::*, *};
use markraft_core::{EditorState, Selection, TransactionSpec, composition::CompositionRange};
use std::borrow::Cow;
use std::ops::Range;

/// `text` with the control characters a document cannot hold removed.
///
/// A platform delivers more than characters through its text protocol: an input
/// method with an empty composition hands Escape on as `insertText("\u{1b}")`,
/// and a stray control character typed into a document is invisible, survives
/// every round trip and reaches the file on disk. Only the two C0 codes that
/// mean something in text — a tab and a line ending — are kept; the rest of C0
/// and DEL are dropped.
pub fn printable(text: &str) -> Cow<'_, str> {
    let drop = |c: char| c.is_control() && c != '\t' && c != '\n';
    if !text.chars().any(drop) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().filter(|c| !drop(*c)).collect())
}

/// What an input method's commit does: select the range the candidate occupies,
/// type `text` over it, and end the composition.
///
/// The selection spec only establishes what the insertion replaces, so it says
/// nothing about the history: an earlier version annotated it
/// `add_to_history(false)`, which the merge then applied to the whole
/// transaction and so kept every committed candidate out of the history.
///
/// Closing a composition is the *last step of that composition*, so the
/// transaction carries the compose user event and folds into the entry the
/// candidate's own replacements made — one undo takes back the whole word. That
/// event is a refinement of `input.type`, so the entry still merges into the
/// typing run around it and into an open undo group. A commit with no live
/// composition — the accessibility paths — is ordinary typing and says so.
pub fn commit_specs(
    state: &EditorState,
    types: &DocTypes,
    range: Option<(usize, usize)>,
    text: &str,
) -> Vec<TransactionSpec> {
    let text = printable(text);
    let doc = state.doc();
    let (from, to) = range
        .or_else(|| markraft_core::composition::composition_range(state).map(|r| (r.from, r.to)))
        .unwrap_or_else(|| {
            let replacement = state.selection().replacement_range(doc);
            (replacement.from, replacement.to)
        });
    let select = TransactionSpec::new()
        .selection(Selection::text(from, to))
        .stored_marks(state.stored_marks().cloned());
    // The insertion is computed against the selection it replaces, which is
    // what the first spec establishes; the two travel as one transaction.
    let insert = state
        .update([select.clone()])
        .ok()
        .and_then(|tr| keymap::insert_plain(types, &text)(tr.state()));
    let mut specs = vec![select];
    if let Some(insert) = insert {
        specs.push(insert.sequential());
    }
    let mut finish = markraft_core::composition::finish_composition().sequential();
    if markraft_core::composition::is_composing(state) {
        // The last spec has the last word on the user event.
        finish = finish.user_event(markraft_core::protocol::COMPOSE_USER_EVENT);
    }
    specs.push(finish);
    specs
}

impl EditorView {
    /// A UTF-16 range over the whole document's text as a position range.
    fn positions_of(&self, range: &Range<usize>) -> Option<(usize, usize)> {
        self.projection()
            .utf16_range_to_pos_range(range.start, range.end)
    }

    /// A position range as a UTF-16 range over the whole document's text.
    fn utf16_of(&self, from: usize, to: usize) -> Option<Range<usize>> {
        let (start, end) = self.projection().pos_range_to_utf16_range(from, to)?;
        Some(start..end)
    }

    /// Replace `range` with `text` and end any composition.
    fn commit(&mut self, range: Option<(usize, usize)>, text: &str, cx: &mut Context<Self>) {
        let specs = commit_specs(self.state(), &self.types, range, text);
        self.dispatch(specs, cx);
    }

    /// Bind the three accessibility actions that write text.
    pub(crate) fn bind_accessibility_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selection_editor = cx.entity();
        let replacement_editor = cx.entity();
        let value_editor = cx.entity();
        root.on_a11y_action(
            AccessibleAction::SetTextSelection,
            move |data, window, cx| {
                if let Some(accesskit::ActionData::SetTextSelection(selection)) = data {
                    selection_editor.update(cx, |this, cx| {
                        let selection = this.accessible_text.borrow().selection(selection);
                        if let Some(selection) = selection {
                            window.focus(&this.focus, cx);
                            this.dispatch([TransactionSpec::new().selection(selection)], cx);
                        }
                    });
                }
            },
        )
        .on_a11y_action(
            AccessibleAction::ReplaceSelectedText,
            move |data, window, cx| {
                if let Some(accesskit::ActionData::Value(value)) = data {
                    replacement_editor.update(cx, |this, cx| {
                        window.focus(&this.focus, cx);
                        let value = single_line::text(value, this.single_line).into_owned();
                        this.commit(None, &value, cx);
                    });
                }
            },
        )
        .on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
            if let Some(accesskit::ActionData::Value(value)) = data {
                value_editor.update(cx, |this, cx| {
                    window.focus(&this.focus, cx);
                    let value = single_line::text(value, this.single_line).into_owned();
                    let end = this.state().doc().content_size();
                    this.commit(Some((0, end)), &value, cx);
                });
            }
        })
    }
}

impl EntityInputHandler for EditorView {
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        EditorView::accepts_text_input(self)
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let projection = self.projection();
        let (from, to) = projection.utf16_range_to_pos_range(range.start, range.end)?;
        let (start, end) = projection.pos_range_to_utf16_range(from, to)?;
        *actual = Some(start..end);
        let units: Vec<u16> = projection.plain_text().encode_utf16().collect();
        Some(String::from_utf16_lossy(units.get(start..end.max(start))?))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let doc = self.state().doc();
        let selection = self.state().selection();
        let (anchor, head) = (selection.anchor(doc), selection.head(doc));
        Some(UTF16Selection {
            range: self.utf16_of(anchor.min(head), anchor.max(head))?,
            reversed: anchor > head,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let range = markraft_core::composition::composition_range(self.state())?;
        self.utf16_of(range.from, range.to)
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch([markraft_core::composition::finish_composition()], cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Belt and braces: an unbound printable key can still reach here through a
        // platform path that does not consult `accepts_text_input`.
        if !EditorView::accepts_text_input(self) {
            return;
        }
        let text = printable(text);
        let text = single_line::text(&text, self.single_line).into_owned();
        let positions = range.as_ref().and_then(|range| self.positions_of(range));
        if range.is_none() && !self.is_composing() {
            // Ordinary typing: the history groups consecutive characters itself.
            let command = keymap::insert_plain(&self.types, &text);
            self.run_command(&command, cx);
            return;
        }
        self.commit(positions, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if self.single_line {
            single_line::selected_range(text, selected)
        } else {
            selected
        };
        let text = printable(text);
        let text = single_line::text(&text, self.single_line).into_owned();
        // macOS reports Escape (and deleting the final candidate character)
        // as empty marked text. Restore the preedit snapshot, including the
        // original replacement selection, before another candidate can start.
        if text.is_empty() && self.is_composing() {
            self.cancel_composition(cx);
            return;
        }
        let mut specs = Vec::new();
        if let Some(range) = range.as_ref()
            && let Some((from, to)) = self.positions_of(range)
        {
            specs.push(markraft_core::composition::start_composition(
                CompositionRange::new(from, to),
            ));
        }
        // A caret expressed in UTF-16 units of the candidate, as `char`s of it.
        let caret = selected
            .map(|selected| {
                let mut units = 0usize;
                let mut chars = 0usize;
                for character in text.chars() {
                    if units >= selected.end {
                        break;
                    }
                    units += character.len_utf16();
                    chars += 1;
                }
                chars
            })
            .unwrap_or_else(|| text.chars().count());
        let marked = if specs.is_empty() {
            None
        } else {
            match self.state().update(specs.clone()) {
                Ok(tr) => Some(tr),
                Err(_) => return,
            }
        };
        let base = marked
            .as_ref()
            .map(|tr| tr.state())
            .unwrap_or_else(|| self.state());
        let Ok(spec) = markraft_core::composition::update_composition(base, &text, caret) else {
            return;
        };
        let spec = if specs.is_empty() {
            spec
        } else {
            spec.sequential()
        };
        drop(marked);
        specs.push(spec);
        self.dispatch(specs, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (from, to) = self.positions_of(&range)?;
        let (row, offset) = self.row_at(from)?;
        let a = row.caret(offset, self.upstream);
        let b = if row.contains(to) {
            row.caret(row.pos_to_offset(to), self.upstream)
        } else {
            a
        };
        let width = if a.y == b.y {
            (b.x - a.x).max(px(2.))
        } else {
            px(2.)
        };
        Some(Bounds::new(a, size(width, row.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.projection().pos_to_utf16(self.hit(point))
    }
}

#[cfg(test)]
mod tests {
    use super::commit_specs;
    use crate::typeahead::tests::{at, state_of, types_of};
    use crate::types::DocTypes;
    use markraft_core::projection::projection_of;
    use markraft_core::{
        EditorState, TransactionSpec,
        composition::CompositionRange,
        history::{begin_undo_group, end_undo_group},
    };

    fn apply(state: &EditorState, specs: Vec<TransactionSpec>) -> EditorState {
        state
            .update_with_appended(specs)
            .expect("the edit applies")
            .last()
            .expect("a transaction")
            .state()
            .clone()
    }

    fn text_of(state: &EditorState) -> String {
        projection_of(state).plain_text().to_owned()
    }

    fn group(state: &EditorState, open: bool) -> EditorState {
        let effect = if open {
            begin_undo_group().of(())
        } else {
            end_undo_group().of(())
        };
        apply(
            state,
            vec![TransactionSpec::new().effect(effect).add_to_history(false)],
        )
    }

    fn undo(state: &EditorState) -> EditorState {
        match markraft_core::history::undo(state) {
            Some(spec) => apply(state, vec![spec]),
            None => state.clone(),
        }
    }

    /// One `setMarkedText`, as the platform sends it.
    fn mark(state: &EditorState, types: &DocTypes, text: &str) -> EditorState {
        let _ = types;
        let range = markraft_core::composition::composition_range(state);
        let mut specs = Vec::new();
        if range.is_none() {
            let head = state.selection().head(state.doc());
            specs.push(markraft_core::composition::start_composition(
                CompositionRange::new(head, head),
            ));
        }
        let base = if specs.is_empty() {
            state.clone()
        } else {
            apply(state, specs.clone())
        };
        let spec =
            markraft_core::composition::update_composition(&base, text, text.chars().count())
                .expect("a composition update");
        if specs.is_empty() {
            apply(state, vec![spec])
        } else {
            specs.push(spec.sequential());
            apply(state, specs)
        }
    }

    /// The whole of an input method's session: mark, refine, then commit.
    fn compose_and_commit(state: &EditorState, types: &DocTypes, candidate: &str) -> EditorState {
        let marked = mark(state, types, "h");
        let marked = mark(&marked, types, candidate);
        assert!(
            markraft_core::composition::is_composing(&marked),
            "the candidate is live"
        );
        let specs = commit_specs(&marked, types, None, candidate);
        let committed = apply(&marked, specs);
        assert!(!markraft_core::composition::is_composing(&committed));
        committed
    }

    /// The caret as a line index and a `char` offset into that line.
    fn caret(state: &EditorState) -> (usize, usize) {
        let head = state.selection().head(state.doc());
        projection_of(state)
            .pos_to_line_offset(head)
            .expect("the caret sits in a line")
    }

    #[test]
    fn control_characters_never_reach_the_document() {
        use super::printable;
        // Escape delivered as text by an input method with nothing composed,
        // which is how one reached a saved note.
        assert_eq!(printable("end.hia\u{1b}"), "end.hia");
        assert_eq!(printable("a\u{0}b\u{7f}c"), "abc");
        // A tab and a line ending mean something in text and are kept.
        assert_eq!(printable("a\tb\nc"), "a\tb\nc");
        // Ordinary text is passed through untouched, without allocating.
        assert!(matches!(
            printable("plain 中文 😀"),
            std::borrow::Cow::Borrowed(_)
        ));
        // And the commit path drops them, so no spec ever carries one.
        let state = state_of("");
        let types = types_of(&state);
        let committed = apply(&state, commit_specs(&state, &types, None, "hi\u{1b}"));
        assert_eq!(text_of(&committed), "hi");
    }

    #[test]
    fn a_committed_candidate_is_recorded_like_typed_text() {
        let state = state_of("a");
        let types = types_of(&state);
        let start = at(&state, 2);
        let committed = compose_and_commit(&start, &types, "hi");
        assert_eq!(text_of(&committed), "ahi");
        // The commit is an ordinary typing transaction, so undo takes it back.
        assert!(markraft_core::history::undo_depth(&committed) > 0);
        // The caret rests after the committed text, which is where a modal
        // editor's Escape steps back from onto its last grapheme.
        assert_eq!(caret(&committed), (0, 3));
        let back = undo(&committed);
        assert_eq!(text_of(&back), "a", "the committed text survived the undo");
    }

    /// The bug the live smoke test found: a commit inside an explicit group has
    /// to belong to that group, not bypass the history and leave its text
    /// behind when the group is undone.
    #[test]
    fn a_commit_inside_an_undo_group_undoes_with_the_rest_of_the_session() {
        let state = state_of("end");
        let types = types_of(&state);
        let start = group(&at(&state, 4), true);
        // The session: a block split, a committed candidate, then more typing.
        let split = crate::commands::enter(&types);
        let opened = apply(&start, vec![split(&start).expect("Enter applies")]);
        let committed = compose_and_commit(&opened, &types, "hi");
        let typed = markraft_core::commands::insert_text("- /");
        let session = apply(&committed, vec![typed(&committed).expect("typing applies")]);
        assert_eq!(text_of(&session), "end\nhi- /");
        let closed = group(&session, false);
        assert_eq!(
            markraft_core::history::undo_depth(&closed),
            1,
            "one entry for the session"
        );
        let back = undo(&closed);
        assert_eq!(text_of(&back), "end");
    }

    #[test]
    fn a_cancelled_composition_leaves_an_open_group_consistent() {
        let state = state_of("end");
        let types = types_of(&state);
        let start = group(&at(&state, 4), true);
        let typed = markraft_core::commands::insert_text("x");
        let session = apply(&start, vec![typed(&start).expect("typing applies")]);
        let marked = mark(&session, &types, "hi");
        assert_eq!(text_of(&marked), "endxhi");
        // Cancelling takes back only the uncommitted candidate.
        let cancelled = apply(
            &marked,
            vec![
                markraft_core::composition::cancel_composition(&marked)
                    .expect("an active composition"),
            ],
        );
        assert_eq!(text_of(&cancelled), "endx");
        let closed = group(&cancelled, false);
        let back = undo(&closed);
        assert_eq!(text_of(&back), "end", "the group still undoes as a whole");
    }
}
