//! The undo history, as an extension.
//!
//! Add [`history`] to a configuration and the state grows a field that records
//! every transaction it should be able to undo. An entry stores the
//! **inverted** change set and the selection from before the transaction, so
//! undoing is an ordinary transaction that happens to restore old content.
//!
//! # Grouping
//!
//! Consecutive transactions merge into one entry when all of the following
//! hold:
//!
//! * the previous recorded transaction was less than `new_group_delay`
//!   milliseconds ago,
//! * both carry a user event of the same class — one is the other or a dotted
//!   refinement of it, so `input.type` groups with `input.type.compose` but not
//!   with `input.paste`,
//! * `group_by_user_event` accepts the new user event (by default `input.type`
//!   and `delete` and their refinements),
//! * the two change sets touch adjacent or overlapping stretches of the
//!   document,
//! * and no selection was recorded on top of the previous entry.
//!
//! Three things override that rule:
//!
//! * [`isolate_history`] forces a boundary before, after, or on both sides of a
//!   transaction. Input rules should use it so that undoing an automatic
//!   conversion is a step of its own.
//! * An explicit group — [`begin_undo_group`] until [`end_undo_group`] — folds
//!   everything in between into one entry *whatever* happens to the document's
//!   structure. This is what a modal editor's insert session needs.
//! * A composition folds its successive replacements: a transaction with the
//!   `input.type.compose` user event joins the entry an earlier one started,
//!   until a transaction that is not a composition, or an
//!   [`end_composition`] effect, closes it.
//!
//! A transaction a [`transaction_appender`](crate::transaction_appender)
//! produced also folds into the entry of the transaction that triggered it, and
//! does not become the reference point for what comes after, so a typed
//! character and whatever an appender added to it undo as one step.
//!
//! Undo and redo close both an open group and an open composition.
//!
//! # Rolling back
//!
//! A transaction carrying [`restore_fields_from`] puts the history back to the
//! value it had in the carried state, open groups and composition grouping
//! included, instead of recording anything. Cancelling a composition uses it to
//! forget the entries the composition's own transactions grew.
//!
//! # Transactions the history should not record
//!
//! A transaction annotated
//! [`add_to_history(false)`](crate::protocol::add_to_history) is not recorded. When it changes the document, every stored entry is rebased
//! over it instead, so an undo after a remote edit still applies. Only the top
//! entry of each branch is rebased eagerly; what the rest still owe is carried
//! with that entry and paid when it is popped. An entry that cannot be rebased
//! empties both branches, and [`history_lost`] reports it for the state that
//! transaction produced.
//!
//! # Relation to the old core
//!
//! The previous implementation tagged edits with an `Origin` enum and grouped
//! by an explicit token plus "the selection and typing marks are unchanged".
//! That maps onto user events and adjacency: `Typed` is `input.type`,
//! `Composition` is `input.type.compose`, `Paste` is `input.paste`, `History`
//! is `undo`/`redo` with `add_to_history: false`, `Command` is no user event at
//! all, and `Extension(name)` is the [`origin`](crate::protocol::origin)
//! annotation. The old "a block kind changed, so start a new entry" guard
//! becomes [`isolate_history`] at the site that performs the conversion.

mod json;
mod state;

use std::sync::{Arc, LazyLock};

use crate::selection::Selection;
use crate::state::protocol::{
    COMPOSE_USER_EVENT, IsolateHistory, add_to_history, appended, end_composition,
    fold_into_previous, isolate_history, matches_user_event, restore_fields_from, time,
};
use crate::state::{
    AnnotationType, EditorState, Extension, Facet, FacetConfig, StateEffect, StateEffectType,
    StateField, StateFieldConfig, Transaction, TransactionSpec,
};

pub use state::HistoryState;

use state::{Branch, HistEvent, MergeHints, add_selection, pop_selection, updated_branch};

/// A function registered with [`inverted_effects`].
pub type InvertedEffectsFn = Arc<dyn Fn(&Transaction) -> Vec<StateEffect> + Send + Sync>;

/// How the history behaves.
#[derive(Clone)]
pub struct HistoryConfig {
    /// How many entries each branch keeps. Older entries are dropped.
    pub min_depth: usize,
    /// The largest gap, in milliseconds, across which two transactions may
    /// still join one entry.
    pub new_group_delay: u64,
    /// Whether transactions with this user event may join an entry at all.
    pub group_by_user_event: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl Default for HistoryConfig {
    fn default() -> HistoryConfig {
        HistoryConfig {
            min_depth: 100,
            new_group_delay: 500,
            group_by_user_event: Arc::new(|event| {
                matches_user_event(event, "input.type") || matches_user_event(event, "delete")
            }),
        }
    }
}

impl std::fmt::Debug for HistoryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryConfig")
            .field("min_depth", &self.min_depth)
            .field("new_group_delay", &self.new_group_delay)
            .finish()
    }
}

struct FromHistory {
    side: Branch,
    rest: Vec<HistEvent>,
    selection: Selection,
}

static HISTORY_CONFIG: LazyLock<Facet<HistoryConfig, HistoryConfig>> = LazyLock::new(|| {
    Facet::define(FacetConfig::new(|inputs: &[HistoryConfig]| {
        inputs.first().cloned().unwrap_or_default()
    }))
});
static INVERTED_EFFECTS: LazyLock<Facet<InvertedEffectsFn>> = LazyLock::new(Facet::list);
static FROM_HISTORY: LazyLock<AnnotationType<FromHistory>> = LazyLock::new(AnnotationType::define);
static BEGIN_GROUP: LazyLock<StateEffectType<()>> = LazyLock::new(StateEffectType::define);
static END_GROUP: LazyLock<StateEffectType<()>> = LazyLock::new(StateEffectType::define);
static HISTORY_FIELD: LazyLock<StateField<HistoryState>> = LazyLock::new(|| {
    StateField::define(
        StateFieldConfig::new(|_| HistoryState::empty(), update_history)
            .to_json(json::history_to_json)
            .from_json(json::history_from_json),
    )
});

/// The facet the [`history`] extension reads its configuration from.
///
/// The highest-precedence input wins.
pub fn history_config() -> &'static Facet<HistoryConfig, HistoryConfig> {
    &HISTORY_CONFIG
}

/// Register a function that turns a transaction's effects into the effects the
/// history should replay when that transaction is undone.
pub fn inverted_effects() -> &'static Facet<InvertedEffectsFn> {
    &INVERTED_EFFECTS
}

/// Open an explicit undo group. Every entry made until the matching
/// [`end_undo_group`] folds into one.
pub fn begin_undo_group() -> &'static StateEffectType<()> {
    &BEGIN_GROUP
}

/// Close the innermost explicit undo group.
pub fn end_undo_group() -> &'static StateEffectType<()> {
    &END_GROUP
}

/// The field the history stores its branches in.
///
/// Useful for inspection and for serialising a state together with its history.
pub fn history_field() -> &'static StateField<HistoryState> {
    &HISTORY_FIELD
}

/// The history extension.
pub fn history(config: HistoryConfig) -> Extension {
    Extension::all([history_config().of(config), history_field().extension()])
}

/// The number of undoable events in `state`.
pub fn undo_depth(state: &EditorState) -> usize {
    state
        .field(history_field())
        .map(HistoryState::undo_depth)
        .unwrap_or(0)
}

/// The number of redoable events in `state`.
pub fn redo_depth(state: &EditorState) -> usize {
    state
        .field(history_field())
        .map(HistoryState::redo_depth)
        .unwrap_or(0)
}

/// Whether the transaction that produced `state` cost the history its entries.
///
/// A transaction the history does not record (annotated
/// [`add_to_history(false)`](crate::protocol::add_to_history), such as a remote
/// edit) has every stored entry rebased over it. When an entry cannot be
/// rebased, both branches are cleared — an entry that no longer applies must
/// not be undone — and this reports `true` for the resulting state, so a host
/// can tell the user their undo history is gone. The next transaction resets
/// it. Always `false` without the [`history`] extension, and not serialised.
pub fn history_lost(state: &EditorState) -> bool {
    state
        .field(history_field())
        .is_some_and(|history| history.lost)
}

/// A spec that undoes the most recent event, or `None` when there is nothing to
/// undo.
pub fn undo(state: &EditorState) -> Option<TransactionSpec> {
    pop(state, Branch::Done, false)
}

/// A spec that redoes the most recently undone event.
pub fn redo(state: &EditorState) -> Option<TransactionSpec> {
    pop(state, Branch::Undone, false)
}

/// A spec that restores the previous selection without touching the document.
pub fn undo_selection(state: &EditorState) -> Option<TransactionSpec> {
    pop(state, Branch::Done, true)
}

/// A spec that re-applies a selection undone by [`undo_selection`].
pub fn redo_selection(state: &EditorState) -> Option<TransactionSpec> {
    pop(state, Branch::Undone, true)
}

fn pop(state: &EditorState, side: Branch, only_selection: bool) -> Option<TransactionSpec> {
    let history = state.field(history_field())?;
    let branch = match side {
        Branch::Done => &history.done,
        Branch::Undone => &history.undone,
    };
    let event = branch.last()?;
    let selection = event
        .selections_after
        .first()
        .cloned()
        .unwrap_or_else(|| state.selection().clone());
    let undo_side = side == Branch::Done;

    if only_selection && !event.selections_after.is_empty() {
        let restore = event
            .selections_after
            .last()
            .cloned()
            .expect("checked above");
        return Some(
            TransactionSpec::new()
                .selection(restore)
                .annotate(FROM_HISTORY.of(FromHistory {
                    side,
                    rest: pop_selection(branch),
                    selection,
                }))
                .user_event(if undo_side {
                    "select.undo"
                } else {
                    "select.redo"
                })
                .scroll_into_view(),
        );
    }

    let changes = event.changes.clone()?;
    let mut rest = branch[..branch.len() - 1].to_vec();
    if let Some((doc, mapping)) = &event.mapped {
        rest = state::map_branch(state.schema(), rest, doc, mapping)?;
    }
    let mut spec = TransactionSpec::new()
        .change_set(changes)
        .effects(event.effects.clone())
        .effect(end_composition().of(()))
        .annotate(FROM_HISTORY.of(FromHistory {
            side,
            rest,
            selection,
        }))
        .user_event(if undo_side { "undo" } else { "redo" })
        .add_to_history(false)
        .no_filter()
        .scroll_into_view();
    if let Some(start) = &event.start_selection {
        spec = spec.selection(start.clone());
    }
    Some(spec)
}

fn update_history(value: &HistoryState, tr: &Transaction) -> HistoryState {
    if let Some(from) = tr
        .effects()
        .iter()
        .find_map(|effect| effect.value(restore_fields_from()))
    {
        return from
            .field(history_field())
            .cloned()
            .unwrap_or_else(|| value.clone());
    }
    // The configuration is read from the *start* state: reading it from the
    // resulting state would ask for the state this update is producing.
    let config = tr.start_state().facet(history_config()).clone();

    if let Some(from_history) = tr.annotation(&FROM_HISTORY) {
        let item = HistEvent::from_transaction(tr, Some(from_history.selection.clone()));
        let mut other = match from_history.side {
            Branch::Done => value.undone.clone(),
            Branch::Undone => value.done.clone(),
        };
        match item {
            Some(item) => other = updated_branch(other, false, config.min_depth, item),
            None => add_selection(&mut other, tr.start_state().selection().clone()),
        }
        let (done, undone) = match from_history.side {
            Branch::Done => (from_history.rest.clone(), other),
            Branch::Undone => (other, from_history.rest.clone()),
        };
        return HistoryState {
            done,
            undone,
            prev_time: None,
            prev_user_event: None,
            group_depth: 0,
            group_started: false,
            composing: false,
            lost: false,
        };
    }

    let was_composing = value.composing;
    let mut state = HistoryState {
        lost: false,
        ..value.clone()
    };
    for effect in tr.effects() {
        if effect.is(begin_undo_group()) {
            state.group_depth += 1;
            state.group_started = false;
        } else if effect.is(end_undo_group()) {
            state.group_depth = state.group_depth.saturating_sub(1);
            if state.group_depth == 0 {
                state.group_started = false;
                state = state.isolate();
            }
        } else if effect.is(end_composition()) {
            state.composing = false;
        }
    }

    let isolate = tr.annotation(isolate_history()).copied();
    if matches!(isolate, Some(IsolateHistory::Before | IsolateHistory::Both)) {
        state = state.isolate();
    }

    let user_event = tr.user_event_name().map(str::to_string);
    // Closing a composition is a boundary: the entry the composition folded
    // into is complete, whatever follows it.
    if was_composing && user_event.as_deref() != Some(COMPOSE_USER_EVENT) {
        state = state.isolate();
    }
    if tr.annotation(add_to_history()) == Some(&false) {
        let mut next = if tr.doc_changed() {
            state.add_mapping(
                tr.start_state().schema(),
                tr.start_state().doc(),
                tr.changes(),
            )
        } else {
            state
        };
        next.composing = composing_after(&next, user_event.as_deref(), tr);
        return next;
    }

    let at = tr.annotation(time()).copied().unwrap_or(0);
    let hints = MergeHints {
        was_composing,
        appended: tr.annotation(appended()).is_some()
            || tr.annotation(fold_into_previous()) == Some(&true),
    };
    match HistEvent::from_transaction(tr, None) {
        Some(event) => state = state.add_changes(event, at, user_event.as_deref(), &config, hints),
        None => {
            if tr.selection().is_some() {
                state = state.add_selection(
                    tr.start_state().selection().clone(),
                    at,
                    user_event.as_deref(),
                    &config,
                );
            }
        }
    }
    if matches!(isolate, Some(IsolateHistory::After | IsolateHistory::Both)) {
        state = state.isolate();
    }
    state.composing = composing_after(&state, user_event.as_deref(), tr);
    state
}

/// Whether a composition is still folding after this transaction.
fn composing_after(state: &HistoryState, user_event: Option<&str>, tr: &Transaction) -> bool {
    if tr.has_effect(end_composition()) {
        return false;
    }
    match user_event {
        // A composition folds only once it has written something: its first
        // replacement is grouped like any other typing, so one that starts
        // after a pause, or elsewhere, is an entry of its own.
        Some(COMPOSE_USER_EVENT) => tr.doc_changed() || state.composing,
        Some(_) => false,
        None => state.composing && !tr.doc_changed(),
    }
}
