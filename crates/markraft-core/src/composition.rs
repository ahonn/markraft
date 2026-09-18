//! IME composition, expressed as state.
//!
//! A platform layer that has to implement "marked text" needs one thing from
//! the model: a range that survives edits and can be read back. This extension
//! stores that range in a state field, maps it through every change, and gives
//! the three transaction shapes a composition goes through:
//!
//! * [`start_composition`] marks a range without changing the document,
//! * [`update_composition`] replaces the marked range with the current
//!   candidate text and re-marks it, annotated `input.type.compose` so the
//!   history folds the whole composition into one entry,
//! * [`finish_composition`] clears the mark, which also closes the history's
//!   composition grouping.
//!
//! Nothing here knows about a platform: `marked_text_range` is
//! [`composition_range`], and `set_marked_text` is
//! [`update_composition`] dispatched as a transaction.

use std::sync::LazyLock;

use crate::change::Change;
use crate::error::NodeError;
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::selection::Selection;
use crate::slice::Slice;
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
    composition_field().extension()
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
    let marks = marks_at(state, doc, from);
    let length = text.chars().count();
    let slice = if text.is_empty() {
        Slice::empty()
    } else {
        let schema = state.schema();
        if schema.text_type().is_none() {
            return Err(StateError::Node(NodeError::InvalidText(
                "the schema declares no text type, so a composition has nothing to insert".into(),
            )));
        }
        Slice::from_fragment(Fragment::from_node(schema.text_marked(text, marks)))
    };
    let range = CompositionRange::new(from, from + length);
    Ok(TransactionSpec::new()
        .changes([Change::replace(from, to, slice).with_fit(Fit::Auto)])
        .selection(Selection::cursor(from + caret.min(length)))
        .effect(set_composition_range().of(range))
        .user_event(COMPOSE_USER_EVENT)
        .scroll_into_view())
}

/// The marks candidate text should carry: the selection's stored marks, or the
/// marks the content at the insertion point has.
fn marks_at(state: &EditorState, doc: &Node, pos: usize) -> MarkSet {
    if let Some(marks) = state.selection().stored_marks() {
        return marks.clone();
    }
    doc.resolve(pos)
        .map(|resolved| resolved.marks(state.schema()))
        .unwrap_or_else(|_| MarkSet::empty())
}
