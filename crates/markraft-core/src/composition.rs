//! IME composition, expressed as state.
//!
//! A platform layer that has to implement "marked text" needs one thing from
//! the model: a range that survives edits and can be read back. This extension
//! stores that range and the committed document in state fields, and gives
//! the three transaction shapes a composition goes through:
//!
//! * [`start_composition`] marks a range without changing the document,
//! * [`update_composition`] replaces the marked range with the current
//!   candidate text and re-marks it, annotated `input.type.compose` so the
//!   history folds the whole composition into one entry,
//! * [`finish_composition`] clears the mark, which also closes the history's
//!   composition grouping.
//!
//! [`cancel_composition`] restores the document, selection and undo history
//! from before the composition. [`committed_document`] excludes uncommitted
//! text for persistence. A document edit without the composition user event commits
//! the current candidate and ends the composition before that edit takes over.
//!
//! Nothing here knows about a platform: `marked_text_range` is
//! [`composition_range`], and `set_marked_text` is
//! [`update_composition`] dispatched as a transaction.

use std::sync::LazyLock;

use crate::change::{Change, ChangeSet, TrackMode};
use crate::error::NodeError;
use crate::fit::Fit;
use crate::node::Node;
use crate::selection::Selection;
use crate::state::protocol::{COMPOSE_USER_EVENT, end_composition, restore_fields_from};
use crate::state::{
    EditorState, Extension, StateEffectType, StateError, StateField, StateFieldConfig, Transaction,
    TransactionSpec,
};

/// The range of the document that is currently marked as composing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionRange {
    /// Start of the marked text.
    pub from: usize,
    /// End of the marked text.
    pub to: usize,
}

impl CompositionRange {
    /// A range.
    pub fn new(from: usize, to: usize) -> CompositionRange {
        CompositionRange {
            from: from.min(to),
            to: from.max(to),
        }
    }
}

static SET_RANGE: LazyLock<StateEffectType<CompositionRange>> = LazyLock::new(|| {
    StateEffectType::mapped(|range: &CompositionRange, changes| {
        let mapped = changes.map_range(range.from, range.to);
        Some(CompositionRange::new(mapped.from, mapped.to))
    })
});
static FIELD: LazyLock<StateField<Option<CompositionRange>>> = LazyLock::new(|| {
    StateField::define(
        StateFieldConfig::new(|_| None, update_composition_field).compare(|a, b| a == b),
    )
});

/// What a composition started from, and how far it has moved away from it.
#[derive(Clone)]
struct CompositionSnapshot {
    /// The state before the composition's first transaction.
    state: EditorState,
    /// Every change the composition made since, composed, in the coordinates
    /// of `state`'s document.
    changes: ChangeSet,
}
static SNAPSHOT: LazyLock<StateField<Option<CompositionSnapshot>>> = LazyLock::new(|| {
    StateField::define(StateFieldConfig::new(
        |_| None,
        |value: &Option<CompositionSnapshot>, tr| {
            if composition_ends(tr) {
                return None;
            }
            if let Some(snapshot) = value {
                // A change that does not continue from the snapshot's document
                // makes the way back unknown. Forget the snapshot rather than
                // restore something else; see `cancel_composition`.
                let changes = snapshot.changes.compose(tr.changes()).ok()?;
                Some(CompositionSnapshot {
                    state: snapshot.state.clone(),
                    changes,
                })
            } else if tr.has_effect(set_composition_range()) && !is_composing(tr.start_state()) {
                // Only the composition's first transaction takes a snapshot: a
                // later one would record a state that already holds
                // uncommitted text.
                Some(CompositionSnapshot {
                    state: tr.start_state().clone(),
                    changes: tr.changes().clone(),
                })
            } else {
                None
            }
        },
    ))
});

fn composition_ends(tr: &Transaction) -> bool {
    tr.has_effect(end_composition())
        || (tr.doc_changed() && tr.user_event_name() != Some(COMPOSE_USER_EVENT))
}

/// The document with any uncommitted input-method candidate excluded.
///
/// Use this for persistence; [`EditorState::doc`] is the live editing preview.
pub fn committed_document(state: &EditorState) -> &Node {
    state
        .field(&SNAPSHOT)
        .and_then(Option::as_ref)
        .map_or_else(|| state.doc(), |snapshot| snapshot.state.doc())
}

/// Restore the content and selection from before the active composition.
///
/// Returns `None` when nothing is composing, and also when the composition's
/// record of where it started was lost — a transaction during the composition
/// whose changes could not be composed onto the ones before it. The
/// composition then has to be finished instead, and
/// [`committed_document`] reports the live document until it is.
///
/// The inverse changes preserve unaffected positions. The spec carries
/// [`restore_fields_from`] with the state from before the composition, so every
/// field that honours it — the undo history, including any open undo group —
/// returns to its pre-composition value; other fields see an ordinary
/// transaction.
pub fn cancel_composition(state: &EditorState) -> Option<TransactionSpec> {
    let snapshot = state.field(&SNAPSHOT)?.as_ref()?;
    let before = &snapshot.state;
    Some(
        TransactionSpec::new()
            .change_set(snapshot.changes.invert(before.doc()).ok()?)
            .selection(before.selection().clone())
            .stored_marks(before.stored_marks().cloned())
            .effect(restore_fields_from().of(before.clone()))
            .effect(end_composition().of(()))
            .add_to_history(false),
    )
}

/// Mark a range as composing, or re-mark it after a replacement.
pub fn set_composition_range() -> &'static StateEffectType<CompositionRange> {
    &SET_RANGE
}

/// The field holding the composition range.
pub fn composition_field() -> &'static StateField<Option<CompositionRange>> {
    &FIELD
}

/// The composition extension.
pub fn composition() -> Extension {
    Extension::all([composition_field().extension(), SNAPSHOT.extension()])
}

/// The range currently marked as composing, if any.
pub fn composition_range(state: &EditorState) -> Option<CompositionRange> {
    state.field(composition_field()).copied().flatten()
}

/// Whether a composition is active.
pub fn is_composing(state: &EditorState) -> bool {
    composition_range(state).is_some()
}

fn update_composition_field(
    value: &Option<CompositionRange>,
    tr: &Transaction,
) -> Option<CompositionRange> {
    if composition_ends(tr) {
        return None;
    }
    let mut range = value.map(|range| {
        let mapped = tr.changes().map_range(range.from, range.to);
        CompositionRange::new(mapped.from, mapped.to)
    });
    for effect in tr.effects() {
        if let Some(next) = effect.value(set_composition_range()) {
            range = Some(*next);
        } else if effect.is(end_composition()) {
            range = None;
        }
    }
    range
}

/// A spec that marks `range` as composing, without changing the document.
pub fn start_composition(range: CompositionRange) -> TransactionSpec {
    TransactionSpec::new()
        .effect(set_composition_range().of(range))
        .user_event(COMPOSE_USER_EVENT)
}

/// A spec that clears the composition.
pub fn finish_composition() -> TransactionSpec {
    TransactionSpec::new().effect(end_composition().of(()))
}

/// A spec that replaces the marked range with `text` and re-marks it.
///
/// `caret` is an offset in characters into `text`. When nothing is marked yet
/// the current selection's replacement range is used and marked, which is what
/// the first `set_marked_text` call of a composition does.
///
/// # Errors
///
/// Fails when the schema declares no text type, so the candidate text cannot
/// be expressed.
pub fn update_composition(
    state: &EditorState,
    text: &str,
    caret: usize,
) -> Result<TransactionSpec, StateError> {
    let doc = state.doc();
    let (from, to) = match composition_range(state) {
        Some(range) => (range.from, range.to),
        None => {
            let range = state.selection().replacement_range(doc);
            (range.from, range.to)
        }
    };
    let length = text.chars().count();
    let (changes, next_doc, end) = if text.is_empty() {
        let changes = ChangeSet::create(
            state.schema(),
            doc,
            [Change::delete(from, to).with_fit(Fit::Auto)],
        )?;
        let next_doc = changes.apply(doc)?;
        let mapped = changes
            .map_pos(to, 1, TrackMode::Simple)
            .unwrap_or(next_doc.content_size());
        let end = Selection::near(state.schema(), &next_doc, mapped, -1).head(&next_doc);
        (changes, next_doc, end)
    } else {
        // Share typing's fitted caret and inline-scope handling. Only its
        // document change is used; the composition remains one transaction.
        let selected = if composition_range(state).is_some() {
            state
                .update([TransactionSpec::new()
                    .selection(Selection::text(from, to))
                    .stored_marks(state.stored_marks().cloned())])?
                .state()
                .clone()
        } else {
            state.clone()
        };
        let spec = crate::commands::insert_text(text)(&selected).ok_or_else(|| {
            StateError::Node(NodeError::InvalidText(
                "the schema cannot insert the composition candidate".into(),
            ))
        })?;
        let inserted = selected.update([spec])?;
        let end = inserted.state().selection().head(inserted.new_doc());
        (inserted.changes().clone(), inserted.new_doc().clone(), end)
    };
    let marks = next_doc.resolve(end)?.marks(state.schema());
    let from = end.saturating_sub(length);
    let range = CompositionRange::new(from, end);
    Ok(TransactionSpec::new()
        .change_set(changes)
        .selection(Selection::cursor(from + caret.min(length)))
        .stored_marks(Some(marks))
        .effect(set_composition_range().of(range))
        .user_event(COMPOSE_USER_EVENT)
        .scroll_into_view())
}
