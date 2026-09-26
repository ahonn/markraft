//! Finding text in the note the reader sees.
//!
//! The query lives in a state field, and every other hit is an inline
//! decoration. Ordinary find selects the current hit, while cursor-based find
//! decorates every hit and places the caret at the current hit's start.
//! The field is not part of the undo history:
//! a find transaction is marked out of it, and an edit recomputes the hits
//! from the document it produced, keeping the current one when the edit only
//! moved it.

use std::ops::Range;
use std::sync::LazyLock;

use markraft_core::decorations::{
    Decoration, DecorationSet, DecorationSource, DecorationSpec, decorations,
};
use markraft_core::kind::DocTypes;
use markraft_core::kind::conceal::Reveal;
use markraft_core::projection::{Projection, projection_of};
use markraft_core::{
    Attrs, EditorState, Extension, Node, Schema, Selection, StateEffect, StateEffectType,
    StateField, StateFieldConfig, Transaction,
};

use crate::shown::ShownText;

/// The attribute a find decoration carries. The view paints one and ignores
/// every other role, so a later spelling mark can use the same path.
pub(crate) const ROLE: &str = "find";

static COMMAND: LazyLock<StateEffectType<Command>> = LazyLock::new(StateEffectType::define);

/// What a find transaction asks the field to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Replace the query and select the first hit at or after the caret.
    Query(String),
    /// The next hit, wrapping.
    Next,
    /// The previous hit, wrapping.
    Previous,
    BeginPreview,
    Preview(String),
    AcceptPreview,
    CancelPreview,
    FromCursor {
        query: String,
        forward: bool,
    },
}

/// The query and the hits it has in the current document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Find {
    query: String,
    hits: Vec<Range<usize>>,
    /// Index into [`Find::hits`].
    current: Option<usize>,
    cursor_mode: bool,
    preview: Option<Preview>,
}

/// A non-recursive snapshot of the state to restore on cancellation. Positions
/// track edits while the preview is open; hits are rebuilt from the live doc.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Preview {
    selection: Selection,
    query: String,
    current: Option<Range<usize>>,
    cursor_mode: bool,
}

impl Find {
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.current
    }

    pub(crate) fn total(&self) -> usize {
        self.hits.len()
    }

    pub(crate) fn has_preview(&self) -> bool {
        self.preview.is_some()
    }

    fn current_range(&self) -> Option<Range<usize>> {
        self.current.and_then(|index| self.hits.get(index).cloned())
    }
}

/// Where the find stands, for the bar that shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindStatus {
    pub query: String,
    /// The current hit, counting from zero. `None` when nothing matches.
    pub current: Option<usize>,
    pub total: usize,
}

pub(crate) fn effect(command: Command) -> StateEffect {
    COMMAND.of(command)
}

/// The field and the decoration source, closed over `types` because only the
/// view knows which nodes are atoms.
pub(crate) fn extension(types: DocTypes) -> (Extension, StateField<Find>) {
    let field = StateField::define(
        StateFieldConfig::new(
            |_| Find::default(),
            move |previous, transaction| update(previous, transaction, &types),
        )
        .compare(|left, right| left == right)
        .provide(|field| decorations().from_field(field, decoration_source)),
    );
    (field.extension(), field)
}

/// Apply `command` to `previous` against `state`, which is the document
/// before the find transaction.
pub(crate) fn apply(
    previous: &Find,
    state: &EditorState,
    types: &DocTypes,
    command: &Command,
) -> Find {
    match command {
        Command::Query(query) => {
            let query = query.clone();
            let hits = hits_of(state, types, &query);
            let current = place(&hits, caret_of(state));
            Find {
                query,
                hits,
                current,
                ..Find::default()
            }
        }
        Command::Next => step(previous, 1),
        Command::Previous => step(previous, -1),
        Command::BeginPreview => {
            let mut next = previous.clone();
            next.preview.get_or_insert_with(|| Preview {
                selection: state.selection().clone(),
                query: previous.query.clone(),
                current: previous.current_range(),
                cursor_mode: previous.cursor_mode,
            });
            next
        }
        Command::Preview(query) => {
            let Some(preview) = &previous.preview else {
                return previous.clone();
            };
            let hits = hits_of(state, types, query);
            let current = from_cursor(&hits, preview.selection.head(state.doc()), true);
            Find {
                query: query.clone(),
                hits,
                current,
                cursor_mode: true,
                preview: previous.preview.clone(),
            }
        }
        Command::AcceptPreview => Find {
            preview: None,
            ..previous.clone()
        },
        Command::CancelPreview => {
            let Some(preview) = &previous.preview else {
                return previous.clone();
            };
            let hits = hits_of(state, types, &preview.query);
            let current = preview.current.as_ref().and_then(|range| {
                hits.iter()
                    .position(|hit| hit == range)
                    .or_else(|| place(&hits, preview.selection.head(state.doc())))
            });
            Find {
                query: preview.query.clone(),
                hits,
                current,
                cursor_mode: preview.cursor_mode,
                preview: None,
            }
        }
        Command::FromCursor { query, forward } => {
            let hits = hits_of(state, types, query);
            // Ordinary find leaves a selected range behind when its bar closes.
            // Treat that range's start as the hit's caret in either direction.
            let current = from_cursor(&hits, state.selection().from(state.doc()), *forward);
            Find {
                query: query.clone(),
                hits,
                current,
                cursor_mode: true,
                preview: previous.preview.clone(),
            }
        }
    }
}

/// The selection change requested by the command. Preview cancellation also
/// restores non-text selections instead of reducing them to a character range.
pub(crate) fn selection_for(previous: &Find, next: &Find, command: &Command) -> Option<Selection> {
    match command {
        Command::Query(query) if query.is_empty() => None,
        Command::BeginPreview | Command::AcceptPreview => None,
        Command::CancelPreview => previous.preview.as_ref().map(|p| p.selection.clone()),
        Command::Preview(_) if previous.preview.is_none() => None,
        Command::Preview(_) => next
            .current_range()
            .map(|hit| Selection::cursor(hit.start))
            .or_else(|| previous.preview.as_ref().map(|p| p.selection.clone())),
        Command::FromCursor { .. } => next.current_range().map(|hit| Selection::cursor(hit.start)),
        _ => next
            .current_range()
            .map(|hit| Selection::text(hit.start, hit.end)),
    }
}

fn update(previous: &Find, transaction: &Transaction, types: &DocTypes) -> Find {
    if let Some(command) = transaction
        .effects()
        .iter()
        .find_map(|effect| effect.value(&COMMAND))
    {
        return apply(previous, transaction.start_state(), types, command);
    }
    if transaction.doc_changed() && (!previous.query.is_empty() || previous.has_preview()) {
        return follow(previous, transaction, types);
    }
    previous.clone()
}

fn follow(previous: &Find, transaction: &Transaction, types: &DocTypes) -> Find {
    let map_range = |range: Range<usize>| {
        let desc = transaction.changes().desc();
        let start = desc.map_pos(range.start, -1, Default::default())?;
        let end = desc.map_pos(range.end, 1, Default::default())?;
        (start < end).then_some(start..end)
    };
    let mapped = previous.current_range().and_then(map_range);
    // The new state is the one this update is building, so the hits are read
    // from the document the transaction already produced.
    let doc = transaction.new_doc();
    let hits = hits_in(
        doc,
        transaction.start_state().schema(),
        types,
        &previous.query,
    );
    let caret = transaction.new_selection().from(doc);
    let current = mapped
        .and_then(|range| {
            hits.iter().position(|hit| hit == &range).or_else(|| {
                hits.iter()
                    .position(|hit| hit.start < range.end && range.start < hit.end)
            })
        })
        .or_else(|| place(&hits, caret));
    Find {
        query: previous.query.clone(),
        hits,
        current,
        cursor_mode: previous.cursor_mode,
        preview: previous.preview.as_ref().map(|preview| Preview {
            selection: preview.selection.map(
                transaction.start_state().schema(),
                doc,
                transaction.changes().desc(),
            ),
            current: preview.current.clone().and_then(map_range),
            ..preview.clone()
        }),
    }
}

fn step(previous: &Find, delta: isize) -> Find {
    let len = previous.hits.len();
    let current = (len > 0).then(|| {
        let index = previous.current.unwrap_or(0) as isize;
        (index + delta).rem_euclid(len as isize) as usize
    });
    Find {
        current,
        cursor_mode: false,
        preview: None,
        ..previous.clone()
    }
}

fn hits_of(state: &EditorState, types: &DocTypes, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let projection = projection_of(state);
    ShownText::build(&projection, types, &Reveal::nothing()).matches(query)
}

fn hits_in(doc: &Node, schema: &Schema, types: &DocTypes, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let projection = Projection::of(doc, schema);
    ShownText::build(&projection, types, &Reveal::nothing()).matches(query)
}

fn caret_of(state: &EditorState) -> usize {
    state.selection().from(state.doc())
}

/// The first hit that still reaches `caret`, or the first hit when the caret
/// is past all of them.
fn place(hits: &[Range<usize>], caret: usize) -> Option<usize> {
    if hits.is_empty() {
        return None;
    }
    Some(hits.iter().position(|hit| hit.end > caret).unwrap_or(0))
}

fn from_cursor(hits: &[Range<usize>], caret: usize, forward: bool) -> Option<usize> {
    if hits.is_empty() {
        return None;
    }
    Some(if forward {
        hits.iter().position(|hit| hit.start > caret).unwrap_or(0)
    } else {
        hits.iter()
            .rposition(|hit| hit.start < caret)
            .unwrap_or(hits.len() - 1)
    })
}

fn decoration_source(find: &Find) -> DecorationSource {
    let skip = if find.cursor_mode { None } else { find.current };
    let spec = DecorationSpec::new(Attrs::from_pairs([(ROLE, "hit")]));
    DecorationSource::Static(DecorationSet::from_decorations(
        find.hits
            .iter()
            .enumerate()
            .filter(|(index, range)| Some(*index) != skip && range.start < range.end)
            .map(|(_, range)| Decoration::inline(range.start, range.end, spec.clone())),
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    };
    use markraft_core::commands::Command as EditCommand;
    use markraft_core::decorations::collect_decorations;
    use markraft_core::{EditorStateConfig, TransactionSpec};

    fn open(source: &str) -> (EditorState, StateField<Find>, DocTypes) {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
        let (find_ext, field) = extension(types.clone());
        let state =
            EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
                Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::history::history(Default::default()),
                    commonmark_extensions(&schema),
                    find_ext,
                ]),
            ))
            .expect("a valid state");
        (state, field, types)
    }

    fn run(state: &EditorState, _field: &StateField<Find>, command: Command) -> EditorState {
        let spec = TransactionSpec::new()
            .effect(effect(command))
            .add_to_history(false);
        state.update([spec]).unwrap().state().clone()
    }

    fn run_selected(
        state: &EditorState,
        field: &StateField<Find>,
        types: &DocTypes,
        command: Command,
    ) -> EditorState {
        let previous = state.field(field).unwrap();
        let next = apply(previous, state, types, &command);
        let selection = selection_for(previous, &next, &command);
        let mut spec = TransactionSpec::new()
            .effect(effect(command))
            .add_to_history(false);
        if let Some(selection) = selection {
            spec = spec.selection(selection);
        }
        state.update([spec]).unwrap().state().clone()
    }

    fn move_to(state: &EditorState, pos: usize) -> EditorState {
        state
            .update([TransactionSpec::new()
                .selection(Selection::cursor(pos))
                .add_to_history(false)])
            .unwrap()
            .state()
            .clone()
    }

    #[test]
    fn a_query_records_every_hit_and_decorates_all_but_the_current() {
        let (state, field, _) = open("one bold two bold");
        let state = run(&state, &field, Command::Query("bold".into()));
        let find = state.field(&field).unwrap();
        assert_eq!(find.total(), 2);
        assert_eq!(find.current(), Some(0));
        assert_eq!(collect_decorations(&state).all().len(), 1);
    }

    #[test]
    fn an_empty_query_clears_the_hits() {
        let (state, field, _) = open("bold");
        let state = run(&state, &field, Command::Query("bold".into()));
        let state = run(&state, &field, Command::Query(String::new()));
        let find = state.field(&field).unwrap();
        assert!(find.query().is_empty());
        assert_eq!(find.total(), 0);
        assert!(collect_decorations(&state).is_empty());
    }

    #[test]
    fn an_edit_keeps_the_query_and_moves_the_hit_with_the_text() {
        let (state, field, types) = open("say bold");
        let state = run(&state, &field, Command::Query("bold".into()));
        let before = state.field(&field).unwrap().current_range().unwrap();
        let insert: EditCommand = markraft_core::commands::insert_text("X");
        // The caret is at the start, so the insertion lands before the word.
        let state = state
            .update([insert(&state).unwrap()])
            .unwrap()
            .state()
            .clone();
        let find = state.field(&field).unwrap();
        assert_eq!(find.query(), "bold");
        let after = find.current_range().unwrap();
        assert!(after.start > before.start);
        let fresh = hits_of(&state, &types, "bold");
        assert_eq!(fresh, find.hits);
    }

    #[test]
    fn next_wraps_and_does_not_enter_the_undo_history() {
        let (state, field, _) = open("a a");
        let depth = |state: &EditorState| markraft_core::history::undo_depth(state);
        let before = depth(&state);
        let state = run(&state, &field, Command::Query("a".into()));
        assert_eq!(state.field(&field).unwrap().current(), Some(0));
        assert_eq!(depth(&state), before);
        let state = run(&state, &field, Command::Next);
        assert_eq!(state.field(&field).unwrap().current(), Some(1));
        let state = run(&state, &field, Command::Next);
        assert_eq!(state.field(&field).unwrap().current(), Some(0));
        let state = run(&state, &field, Command::Previous);
        assert_eq!(state.field(&field).unwrap().current(), Some(1));
    }

    #[test]
    fn cursor_search_uses_the_live_caret_and_wraps_in_both_directions() {
        let (state, field, types) = open("你好 one 你好 one 你好");
        let hits = hits_of(&state, &types, "你好");
        let search = |state: &EditorState, forward| {
            run_selected(
                state,
                &field,
                &types,
                Command::FromCursor {
                    query: "你好".into(),
                    forward,
                },
            )
        };
        let state = move_to(&state, hits[0].start);
        let state = search(&state, true);
        assert_eq!(state.selection(), &Selection::cursor(hits[1].start));
        assert_eq!(collect_decorations(&state).all().len(), hits.len());
        let state = search(&state, false);
        assert_eq!(state.selection(), &Selection::cursor(hits[0].start));
        let state = search(&state, false);
        assert_eq!(state.selection(), &Selection::cursor(hits[2].start));
        let state = search(&state, true);
        assert_eq!(state.selection(), &Selection::cursor(hits[0].start));
        // Moving manually invalidates the cached index as a navigation origin.
        let state = move_to(&state, hits[2].start);
        let state = search(&state, false);
        assert_eq!(state.selection(), &Selection::cursor(hits[1].start));
        assert_eq!(markraft_core::history::undo_depth(&state), 0);
    }

    #[test]
    fn cursor_search_steps_past_the_range_left_by_ordinary_find() {
        let (state, field, types) = open("needle one needle two needle");
        let hits = hits_of(&state, &types, "needle");
        let run = |state: &EditorState, command| run_selected(state, &field, &types, command);
        let state = run(&state, Command::Query("needle".into()));
        let state = run(&state, Command::Next);
        let state = run(&state, Command::Query(String::new()));
        assert_eq!(
            state.selection(),
            &Selection::text(hits[1].start, hits[1].end)
        );
        for (forward, expected) in [(false, 0), (true, 2)] {
            let stepped = run(
                &state,
                Command::FromCursor {
                    query: "needle".into(),
                    forward,
                },
            );
            assert_eq!(
                stepped.selection(),
                &Selection::cursor(hits[expected].start)
            );
        }
    }

    #[test]
    fn preview_uses_a_fixed_origin_and_accept_does_not_advance() {
        let (state, field, types) = open("😀 alpha alpha");
        let hits = hits_of(&state, &types, "alpha");
        let origin = state.selection().clone();
        let run = |state: &EditorState, command| run_selected(state, &field, &types, command);
        let state = run(&state, Command::BeginPreview);
        let state = run(&state, Command::Preview("a".into()));
        let state = run(&state, Command::Preview("alpha".into()));
        assert_eq!(state.selection(), &Selection::cursor(hits[0].start));
        let state = run(&state, Command::Preview("missing".into()));
        assert_eq!(state.selection(), &origin);
        assert_eq!(state.field(&field).unwrap().total(), 0);
        let state = run(&state, Command::Preview("alpha".into()));
        let state = run(&state, Command::Preview(String::new()));
        assert_eq!(state.selection(), &origin);
        let state = run(&state, Command::Preview("alpha".into()));
        let state = run(&state, Command::AcceptPreview);
        assert_eq!(state.selection(), &Selection::cursor(hits[0].start));
        assert!(!state.field(&field).unwrap().has_preview());
        assert_eq!(state.field(&field).unwrap().query(), "alpha");
        assert_eq!(collect_decorations(&state).all().len(), hits.len());
        assert_eq!(markraft_core::history::undo_depth(&state), 0);
    }

    #[test]
    fn cancel_restores_selection_query_and_decoration_mode_after_preview_steps() {
        let (state, field, types) = open("old new old new");
        let run = |state: &EditorState, command| run_selected(state, &field, &types, command);
        let state = run(&state, Command::Query("old".into()));
        let original_find = state.field(&field).unwrap().clone();
        let original_selection = state.selection().clone();
        let state = run(&state, Command::BeginPreview);
        let state = run(&state, Command::Preview("new".into()));
        // A repeated open must not overwrite the cancellation snapshot.
        let state = run(&state, Command::BeginPreview);
        let state = run(
            &state,
            Command::FromCursor {
                query: "new".into(),
                forward: true,
            },
        );
        assert!(state.field(&field).unwrap().has_preview());
        let state = run(&state, Command::CancelPreview);
        assert_eq!(state.selection(), &original_selection);
        assert_eq!(state.field(&field).unwrap(), &original_find);
        assert_eq!(collect_decorations(&state).all().len(), 1);
        // Closed-session operations are harmless, including a stale input event.
        let state = run(&state, Command::CancelPreview);
        let state = run(&state, Command::AcceptPreview);
        let state = run(&state, Command::Preview("new".into()));
        assert_eq!(state.selection(), &original_selection);
        assert_eq!(state.field(&field).unwrap(), &original_find);
    }

    #[test]
    fn edits_map_the_preview_origin_and_saved_search_even_with_an_empty_query() {
        let (state, field, types) = open("old new old");
        let run = |state: &EditorState, command| run_selected(state, &field, &types, command);
        let state = run(&state, Command::Query("old".into()));
        let state = run(&state, Command::Next);
        let original_selection = state.selection().clone();
        let state = run(&state, Command::BeginPreview);
        let state = run(&state, Command::Preview(String::new()));
        let state = move_to(&state, 1);
        let insert: EditCommand = markraft_core::commands::insert_text("😀 ");
        let transaction = state.update([insert(&state).unwrap()]).unwrap();
        let mapped = original_selection.map(
            state.schema(),
            transaction.new_doc(),
            transaction.changes().desc(),
        );
        let state = transaction.state().clone();
        let state = run(&state, Command::Preview("missing".into()));
        assert_eq!(state.selection(), &mapped);
        let state = run(&state, Command::CancelPreview);
        assert_eq!(state.selection(), &mapped);
        let find = state.field(&field).unwrap();
        assert_eq!(find.query(), "old");
        assert_eq!(find.current(), Some(1));
        assert_eq!(find.hits, hits_of(&state, &types, "old"));
        assert!(!find.has_preview());
    }

    #[test]
    fn cursor_search_without_matches_keeps_the_caret_and_single_hits_wrap() {
        let (state, field, types) = open("one 😀");
        let run = |state: &EditorState, query: &str, forward| {
            run_selected(
                state,
                &field,
                &types,
                Command::FromCursor {
                    query: query.into(),
                    forward,
                },
            )
        };
        let state = run(&state, "😀", true);
        let at_hit = state.selection().clone();
        let state = run(&state, "😀", true);
        assert_eq!(state.selection(), &at_hit);
        let state = run(&state, "😀", false);
        assert_eq!(state.selection(), &at_hit);
        let state = run(&state, "missing", true);
        assert_eq!(state.selection(), &at_hit);
        let state = run(&state, "", false);
        assert_eq!(state.selection(), &at_hit);
    }
}
