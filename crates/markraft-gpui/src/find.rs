//! Finding text in the note the reader sees.
//!
//! The query lives in a state field, and every other hit is an inline
//! decoration. The current hit is the selection, so it is drawn once, by the
//! selection, above the other hits. The field is not part of the undo history:
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
    Attrs, EditorState, Extension, Node, Schema, StateEffect, StateEffectType, StateField,
    StateFieldConfig, Transaction,
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
}

/// The query and the hits it has in the current document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Find {
    query: String,
    hits: Vec<Range<usize>>,
    /// Index into [`Find::hits`].
    current: Option<usize>,
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
            }
        }
        Command::Next => step(previous, 1),
        Command::Previous => step(previous, -1),
    }
}

/// The document range `command` selects, when it selects one. An empty query
/// clears the hits and leaves the caret where it is.
pub(crate) fn selection_for(next: &Find, command: &Command) -> Option<Range<usize>> {
    match command {
        Command::Query(query) if query.is_empty() => None,
        _ => next.current_range(),
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
    if transaction.doc_changed() && !previous.query.is_empty() {
        return follow(previous, transaction, types);
    }
    previous.clone()
}

fn follow(previous: &Find, transaction: &Transaction, types: &DocTypes) -> Find {
    let mapped = previous.current_range().and_then(|range| {
        let desc = transaction.changes().desc();
        let start = desc.map_pos(range.start, -1, Default::default())?;
        let end = desc.map_pos(range.end, 1, Default::default())?;
        (start < end).then_some(start..end)
    });
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
        ..previous.clone()
    }
}

fn hits_of(state: &EditorState, types: &DocTypes, query: &str) -> Vec<Range<usize>> {
    let projection = projection_of(state);
    ShownText::build(&projection, types, &Reveal::nothing()).matches(query)
}

fn hits_in(doc: &Node, schema: &Schema, types: &DocTypes, query: &str) -> Vec<Range<usize>> {
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

fn decoration_source(find: &Find) -> DecorationSource {
    let skip = find.current;
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
}
