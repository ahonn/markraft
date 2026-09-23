//! The history's stored state: two branches of invertible events.
//!
//! An event holds the **inverted** change set of the transaction it records —
//! applying it to the document that transaction produced gives the document
//! back — plus the selection from before the transaction, the document that
//! change set applies to, and any inverted effects.
//!
//! Keeping the document with the event is what lets a rebase repair itself:
//! [`ChangeSet::transform`] needs a document, and an event deeper in the branch
//! lives in a document frame that no longer exists. The document handle costs
//! one pointer, and the trees it keeps alive are the ones the event's inverted
//! content already refers to.

use crate::change::{ChangeRange, ChangeSet};
use crate::node::Node;
use crate::schema::Schema;
use crate::selection::Selection;
use crate::state::{StateEffect, Transaction, matches_user_event};

use super::inverted_effects;

/// How many selections one event remembers.
const MAX_SELECTIONS: usize = 200;

/// Why a transaction might join the entry above it, beyond the ordinary
/// time-and-adjacency rule.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MergeHints {
    /// A composition was folding into the top entry before this transaction.
    pub(crate) was_composing: bool,
    /// This transaction was produced by a transaction appender, or is
    /// annotated [`fold_into_previous`](super::fold_into_previous).
    pub(crate) appended: bool,
}

/// Which branch an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Branch {
    /// The undo stack.
    Done,
    /// The redo stack.
    Undone,
}

/// One undoable step.
#[derive(Clone)]
pub(crate) struct HistEvent {
    /// The inverted changes, in the coordinates of the document the recorded
    /// transaction produced.
    pub(crate) changes: Option<ChangeSet>,
    /// Effects to re-apply when this event is undone.
    pub(crate) effects: Vec<StateEffect>,
    /// A rebase still owed to the events below this one: the document they were
    /// recorded against, and the change that moves it into the current frame.
    pub(crate) mapped: Option<(Node, ChangeSet)>,
    /// The selection from before the recorded transaction.
    pub(crate) start_selection: Option<Selection>,
    /// Selections the user made after the recorded transaction, oldest first.
    pub(crate) selections_after: Vec<Selection>,
}

impl HistEvent {
    pub(crate) fn selection(selections: Vec<Selection>) -> HistEvent {
        HistEvent {
            changes: None,
            effects: Vec::new(),
            mapped: None,
            start_selection: None,
            selections_after: selections,
        }
    }

    /// Record `tr`, or `None` when there is nothing to undo.
    ///
    /// `selection` overrides the recorded "selection before", which is what an
    /// undo transaction uses to give the redo entry the selection the user had
    /// when they pressed undo.
    pub(crate) fn from_transaction(
        tr: &Transaction,
        selection: Option<Selection>,
    ) -> Option<HistEvent> {
        let mut effects: Vec<StateEffect> = Vec::new();
        for invert in tr.start_state().facet(inverted_effects()) {
            effects.extend(invert(tr));
        }
        if effects.is_empty() && !tr.doc_changed() {
            return None;
        }
        let changes = tr.changes().invert(tr.start_state().doc()).ok()?;
        Some(HistEvent {
            changes: Some(changes),
            effects,
            mapped: None,
            start_selection: Some(
                selection.unwrap_or_else(|| tr.start_state().selection().clone()),
            ),
            selections_after: Vec::new(),
        })
    }

    fn has_content(&self) -> bool {
        self.changes.as_ref().is_some_and(|c| !c.is_empty()) || !self.effects.is_empty()
    }
}

/// The value of the history state field.
#[derive(Clone)]
pub struct HistoryState {
    pub(crate) done: Vec<HistEvent>,
    pub(crate) undone: Vec<HistEvent>,
    /// When the last recorded transaction happened. `None` means a boundary
    /// was forced, so nothing may join the top entry.
    pub(crate) prev_time: Option<u64>,
    pub(crate) prev_user_event: Option<String>,
    /// How many [`begin_undo_group`](super::begin_undo_group) effects are open.
    pub(crate) group_depth: usize,
    /// Whether the open group already owns the top entry.
    pub(crate) group_started: bool,
    /// Whether an IME composition is folding into the top entry.
    pub(crate) composing: bool,
}

impl std::fmt::Debug for HistoryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryState")
            .field("done", &self.done.len())
            .field("undone", &self.undone.len())
            .field("group_depth", &self.group_depth)
            .field("composing", &self.composing)
            .finish()
    }
}

impl HistoryState {
    /// An empty history.
    pub(crate) fn empty() -> HistoryState {
        HistoryState {
            done: Vec::new(),
            undone: Vec::new(),
            prev_time: None,
            prev_user_event: None,
            group_depth: 0,
            group_started: false,
            composing: false,
        }
    }

    /// The number of undoable events.
    pub fn undo_depth(&self) -> usize {
        self.done.iter().filter(|e| e.changes.is_some()).count()
    }

    /// The number of redoable events.
    pub fn redo_depth(&self) -> usize {
        self.undone.iter().filter(|e| e.changes.is_some()).count()
    }

    /// Force a boundary: the next edit starts a new event.
    ///
    /// An explicit group is deliberately left alone — it folds everything it
    /// contains, which is the whole point of opening one.
    pub(crate) fn isolate(&self) -> HistoryState {
        HistoryState {
            prev_time: None,
            prev_user_event: None,
            ..self.clone()
        }
    }

    /// Record `event`, merging it into the previous one when the grouping rules
    /// say so.
    pub(crate) fn add_changes(
        &self,
        event: HistEvent,
        time: u64,
        user_event: Option<&str>,
        config: &super::HistoryConfig,
        hints: MergeHints,
    ) -> HistoryState {
        let merge = self.should_merge(&event, time, user_event, config, hints);
        let mut next = self.clone();
        if merge && let Some(last) = self.done.last() {
            let merged = merge_events(&event, last);
            next.done = updated_branch(self.done.clone(), true, config.min_depth, merged);
        } else {
            next.done = updated_branch(self.done.clone(), false, config.min_depth, event);
            if next.group_depth > 0 {
                next.group_started = true;
            }
        }
        next.undone = Vec::new();
        // An appended transaction belongs to the transaction that triggered it,
        // so it must not become the reference point for what comes next either.
        if !hints.appended {
            next.prev_time = Some(time);
            next.prev_user_event = user_event.map(str::to_string);
        }
        next
    }

    fn should_merge(
        &self,
        event: &HistEvent,
        time: u64,
        user_event: Option<&str>,
        config: &super::HistoryConfig,
        hints: MergeHints,
    ) -> bool {
        let Some(last) = self.done.last() else {
            return false;
        };
        let Some(last_changes) = &last.changes else {
            return false;
        };
        if last_changes.is_empty() {
            return false;
        }
        // An explicit group folds everything it contains, whatever happens to
        // the document's structure in between.
        if self.group_depth > 0 && self.group_started {
            return true;
        }
        // So does a transaction an appender produced: it is part of the edit
        // that triggered it, not an edit of its own.
        if hints.appended {
            return true;
        }
        // A composition folds its successive replacements.
        if hints.was_composing && user_event == Some(crate::composition::COMPOSE_USER_EVENT) {
            return true;
        }
        if !last.selections_after.is_empty() {
            return false;
        }
        let Some(previous_time) = self.prev_time else {
            return false;
        };
        if time.saturating_sub(previous_time) >= config.new_group_delay {
            return false;
        }
        let (Some(user_event), Some(previous)) = (user_event, self.prev_user_event.as_deref())
        else {
            return false;
        };
        if !(config.group_by_user_event)(user_event) || !same_user_event_class(previous, user_event)
        {
            return false;
        }
        let Some(changes) = &event.changes else {
            return false;
        };
        is_adjacent(last_changes, changes)
    }

    /// Record a selection change on top of the current event.
    pub(crate) fn add_selection(
        &self,
        selection: Selection,
        time: u64,
        user_event: Option<&str>,
        config: &super::HistoryConfig,
    ) -> HistoryState {
        if let Some(last) = self.done.last()
            && let Some(previous) = last.selections_after.last()
            && self
                .prev_time
                .is_some_and(|prev| time.saturating_sub(prev) < config.new_group_delay)
            && user_event.is_some()
            && user_event == self.prev_user_event.as_deref()
            && *previous == selection
        {
            return self.clone();
        }
        let mut next = self.clone();
        add_selection(&mut next.done, selection);
        next.prev_time = Some(time);
        next.prev_user_event = user_event.map(str::to_string);
        next
    }

    /// Move every event into the frame `changes` produces.
    ///
    /// Returns an empty history when a rebase fails, which is the only safe
    /// answer: an event that cannot be rebased cannot be applied either.
    pub(crate) fn add_mapping(
        &self,
        schema: &Schema,
        doc: &Node,
        changes: &ChangeSet,
    ) -> HistoryState {
        let done = map_branch(schema, self.done.clone(), doc, changes);
        let undone = map_branch(schema, self.undone.clone(), doc, changes);
        match (done, undone) {
            (Some(done), Some(undone)) => HistoryState {
                done,
                undone,
                ..self.clone()
            },
            _ => HistoryState {
                done: Vec::new(),
                undone: Vec::new(),
                ..self.clone()
            },
        }
    }
}

/// Merge `event` (the newer one) into `last`.
///
/// Undo runs newest-first, so the newer inverted change set comes first and the
/// older one is composed onto it. The restore point stays the older event's, so
/// undoing a merged run lands where the first keystroke started.
fn merge_events(event: &HistEvent, last: &HistEvent) -> HistEvent {
    let changes = match (&event.changes, &last.changes) {
        (Some(new), Some(old)) => new.compose(old).ok(),
        (new, _) => new.clone(),
    };
    let mut effects = StateEffect::map_all(
        &event.effects,
        &last
            .changes
            .as_ref()
            .map(ChangeSet::desc)
            .unwrap_or_else(|| crate::change::ChangeDesc::empty(0)),
    );
    effects.extend(last.effects.iter().cloned());
    HistEvent {
        changes,
        effects,
        mapped: last.mapped.clone(),
        start_selection: last.start_selection.clone(),
        selections_after: Vec::new(),
    }
}

/// Append `event` to `branch`, optionally replacing the last entry, and trim to
/// `max_len`.
pub(crate) fn updated_branch(
    mut branch: Vec<HistEvent>,
    replace_last: bool,
    max_len: usize,
    event: HistEvent,
) -> Vec<HistEvent> {
    if replace_last {
        branch.pop();
    }
    branch.push(event);
    if max_len > 0 && branch.len() > max_len {
        branch.drain(0..branch.len() - max_len);
    }
    branch
}

pub(crate) fn add_selection(branch: &mut Vec<HistEvent>, selection: Selection) {
    match branch.last_mut() {
        Some(last) => {
            if last.selections_after.last() != Some(&selection) {
                last.selections_after.push(selection);
                if last.selections_after.len() > MAX_SELECTIONS {
                    last.selections_after.remove(0);
                }
            }
        }
        None => branch.push(HistEvent::selection(vec![selection])),
    }
}

/// Drop the newest remembered selection from the top event.
pub(crate) fn pop_selection(branch: &[HistEvent]) -> Vec<HistEvent> {
    let mut branch = branch.to_vec();
    if let Some(last) = branch.last_mut() {
        last.selections_after.pop();
    }
    branch
}

/// Rebase a branch over `changes`, which applies to `doc`.
///
/// Only the top event is rebased eagerly; the rebase the events below it still
/// owe is recorded in that event's `mapped` and paid when it is popped. When
/// the top event is emptied by the rebase the walk continues downwards.
pub(crate) fn map_branch(
    schema: &Schema,
    branch: Vec<HistEvent>,
    doc: &Node,
    changes: &ChangeSet,
) -> Option<Vec<HistEvent>> {
    if branch.is_empty() {
        return Some(branch);
    }
    let mut branch = branch;
    let mut doc = doc.clone();
    let mut changes = changes.clone();
    let mut selections: Vec<Selection> = Vec::new();
    let mut length = branch.len();
    while length > 0 {
        let (event, next) = map_event(schema, &branch[length - 1], &doc, &changes, selections)?;
        if event.has_content() {
            branch.truncate(length);
            branch[length - 1] = event;
            return Some(branch);
        }
        let (next_doc, next_changes) = next.unwrap_or((doc.clone(), changes.clone()));
        doc = next_doc;
        changes = next_changes;
        selections = event.selections_after;
        length -= 1;
    }
    Some(if selections.is_empty() {
        Vec::new()
    } else {
        vec![HistEvent::selection(selections)]
    })
}

type MapResult = (HistEvent, Option<(Node, ChangeSet)>);

fn map_event(
    schema: &Schema,
    event: &HistEvent,
    doc: &Node,
    changes: &ChangeSet,
    partial: Vec<Selection>,
) -> Option<MapResult> {
    let mapped_doc = changes.apply(doc).ok()?;
    let desc = changes.desc();
    let mut selections: Vec<Selection> = event
        .selections_after
        .iter()
        .map(|selection| selection.map(schema, &mapped_doc, &desc))
        .collect();
    selections.extend(partial);

    let Some(event_changes) = &event.changes else {
        let mut mapped = HistEvent::selection(selections);
        mapped.effects = StateEffect::map_all(&event.effects, &desc);
        return Some((mapped, None));
    };

    // `changes` and `event_changes` both start from `doc`; rebasing each over
    // the other gives the event in the new frame, and the change in the frame
    // the events below this one live in.
    let (changes_over_event, event_over_changes) =
        changes.transform(doc, event_changes, true).ok()?;
    let lower_doc = event_changes.apply(doc).ok()?;
    let new_lower_doc = changes_over_event.apply(&lower_doc).ok()?;
    let owed = match &event.mapped {
        Some((original, existing)) => (
            original.clone(),
            existing.compose(&changes_over_event).ok()?,
        ),
        None => (lower_doc, changes_over_event.clone()),
    };
    let start_selection = event
        .start_selection
        .as_ref()
        .map(|selection| selection.map(schema, &new_lower_doc, &changes_over_event.desc()));
    let mapped = HistEvent {
        changes: Some(event_over_changes),
        effects: StateEffect::map_all(&event.effects, &desc),
        mapped: Some(owed.clone()),
        start_selection,
        selections_after: selections,
    };
    Some((mapped, Some(owed)))
}

/// Whether the two inverted change sets touch the same stretch of the document
/// they are both expressed against.
pub(crate) fn is_adjacent(previous: &ChangeSet, next: &ChangeSet) -> bool {
    let before: Vec<(usize, usize)> = previous
        .iter_changes()
        .iter()
        .map(|range| match range {
            ChangeRange::Replaced { from_a, to_a, .. }
            | ChangeRange::Marked { from_a, to_a, .. } => (*from_a, *to_a),
        })
        .collect();
    next.iter_changes().iter().any(|range| {
        let (from, to) = match range {
            ChangeRange::Replaced { from_b, to_b, .. }
            | ChangeRange::Marked { from_b, to_b, .. } => (*from_b, *to_b),
        };
        before
            .iter()
            .any(|(start, end)| to >= *start && from <= *end)
    })
}

/// Whether two user events belong to the same class, meaning one is the other
/// or a dotted refinement of it.
///
/// `input.type` groups with `input.type.compose` but not with `input.paste`,
/// which is what keeps a paste out of a run of typing.
pub(crate) fn same_user_event_class(a: &str, b: &str) -> bool {
    matches_user_event(a, b) || matches_user_event(b, a)
}
