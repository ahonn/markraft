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
//! [`cancel_composition`] restores the document, selection and history from
//! before the composition. [`committed_document`] excludes uncommitted text
//! for persistence. A document edit without the composition user event commits
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
use crate::state::{
    EditorState, Extension, StateEffectType, StateError, StateField, StateFieldConfig, Transaction,
    TransactionSpec,
};

/// The user event every composition update carries.
///
/// The history groups transactions with this event into one undo entry.
pub const COMPOSE_USER_EVENT: &str = "input.type.compose";

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
static END: LazyLock<StateEffectType<()>> = LazyLock::new(StateEffectType::define);
static FIELD: LazyLock<StateField<Option<CompositionRange>>> = LazyLock::new(|| {
    StateField::define(
        StateFieldConfig::new(|_| None, update_composition_field).compare(|a, b| a == b),
    )
});

#[derive(Clone)]
struct CompositionSnapshot {
    doc: Node,
    selection: Selection,
    changes: ChangeSet,
    history: Option<crate::history::HistoryState>,
}

static CANCEL: LazyLock<StateEffectType<CompositionSnapshot>> =
    LazyLock::new(StateEffectType::define);
static SNAPSHOT: LazyLock<StateField<Option<CompositionSnapshot>>> = LazyLock::new(|| {
    StateField::define(StateFieldConfig::new(
        |_| None,
        |value: &Option<CompositionSnapshot>, tr| {
            if composition_ends(tr) {
                return None;
            }
            if let Some(snapshot) = value {
                let mut snapshot = snapshot.clone();
                snapshot.changes = snapshot
                    .changes
                    .compose(tr.changes())
                    .expect("composition changes share a document boundary");
                Some(snapshot)
            } else if tr.has_effect(set_composition_range()) {
                Some(CompositionSnapshot {
                    doc: tr.start_state().doc().clone(),
                    selection: tr.start_state().selection().clone(),
                    changes: tr.changes().clone(),
                    history: tr
                        .start_state()
                        .field(crate::history::history_field())
                        .cloned(),
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
        .map_or_else(|| state.doc(), |snapshot| &snapshot.doc)
}

/// Restore the content and selection from before the active composition.
///
/// The inverse changes preserve unaffected positions and extension state. The
/// history returns to its pre-composition value, including any open undo group.
pub fn cancel_composition(state: &EditorState) -> Option<TransactionSpec> {
    let snapshot = state.field(&SNAPSHOT)?.as_ref()?;
    Some(
        TransactionSpec::new()
            .change_set(snapshot.changes.invert(&snapshot.doc).ok()?)
            .selection(snapshot.selection.clone())
            .effect(CANCEL.of(snapshot.clone()))
            .effect(end_composition().of(()))
            .add_to_history(false),
    )
}

pub(crate) fn cancelled_history(tr: &Transaction) -> Option<&crate::history::HistoryState> {
    tr.effects()
        .iter()
        .find_map(|effect| effect.value(&CANCEL))
        .and_then(|snapshot| snapshot.history.as_ref())
}

/// Mark a range as composing, or re-mark it after a replacement.
pub fn set_composition_range() -> &'static StateEffectType<CompositionRange> {
    &SET_RANGE
}

/// Clear the composition.
///
/// The history also treats this as the end of a composition, so the next edit
/// starts a new undo entry.
pub fn end_composition() -> &'static StateEffectType<()> {
    &END
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
                .update([TransactionSpec::new().selection(Selection::Text {
                    anchor: from,
                    head: to,
                    marks: state.selection().stored_marks().cloned(),
                })])?
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
        .selection(Selection::cursor_with_marks(
            from + caret.min(length),
            marks,
        ))
        .effect(set_composition_range().of(range))
        .user_event(COMPOSE_USER_EVENT)
        .scroll_into_view())
}
