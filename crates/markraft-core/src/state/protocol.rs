//! The vocabulary extensions share: annotations, effect types and constants
//! that more than one part of the editor reads or writes.
//!
//! The state layer defines *how* annotations and effects work
//! ([`AnnotationType`], [`StateEffectType`]); this module defines the
//! particular ones that form a contract between extensions. Something lives
//! here when the state machine itself writes or reads it, or when two or more
//! extensions read it. An annotation or effect only one extension cares about
//! stays in that extension — [`begin_undo_group`](crate::history::begin_undo_group)
//! is the history's own business, so it lives in [`history`](crate::history).
//!
//! Keeping the shared vocabulary in one place is what lets the extensions
//! ([`history`](crate::history), [`composition`](crate::composition),
//! [`corrections`](crate::corrections), the input rules in
//! [`commands`](crate::commands)) depend only on the state layer and this
//! module, never on one another. The composition, for example, closes the
//! history's composition grouping with [`end_composition`] and puts the
//! history back with [`restore_fields_from`] without either knowing the
//! other exists.
//!
//! # What is here
//!
//! * Describing a transaction: [`user_event`] (with [`matches_user_event`]),
//!   [`time`], [`remote`], [`origin`], [`add_to_history`].
//! * Shaping the undo history: [`isolate_history`] (and [`isolate`]),
//!   [`fold_into_previous`].
//! * Reporting what a hook or the model did: [`appended`],
//!   [`appenders_diverged`], [`corrections_diverged`], [`content_dropped`].
//! * Compositions: [`COMPOSE_USER_EVENT`], [`end_composition`].
//! * Rolling fields back: [`restore_fields_from`].
//! * Changing the configuration: [`reconfigure`], [`append_config`],
//!   [`compartment_reconfigure`].

use std::sync::LazyLock;

use super::EditorState;
use super::annotation::{Annotation, AnnotationType};
use super::effect::StateEffectType;
use super::extension::{Compartment, Extension};

static USER_EVENT: LazyLock<AnnotationType<String>> = LazyLock::new(AnnotationType::define);
static ADD_TO_HISTORY: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static TIME: LazyLock<AnnotationType<u64>> = LazyLock::new(AnnotationType::define);
static REMOTE: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static ORIGIN: LazyLock<AnnotationType<String>> = LazyLock::new(AnnotationType::define);
static APPENDED: LazyLock<AnnotationType<Appended>> = LazyLock::new(AnnotationType::define);
static APPENDERS_DIVERGED: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static ISOLATE: LazyLock<AnnotationType<IsolateHistory>> = LazyLock::new(AnnotationType::define);
static FOLD: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static CORRECTIONS_DIVERGED: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static CONTENT_DROPPED: LazyLock<AnnotationType<usize>> = LazyLock::new(AnnotationType::define);
static END_COMPOSITION: LazyLock<StateEffectType<()>> = LazyLock::new(StateEffectType::define);
static RESTORE_FIELDS_FROM: LazyLock<StateEffectType<EditorState>> =
    LazyLock::new(StateEffectType::define);
static RECONFIGURE: LazyLock<StateEffectType<Extension>> = LazyLock::new(StateEffectType::define);
static APPEND_CONFIG: LazyLock<StateEffectType<Extension>> = LazyLock::new(StateEffectType::define);
static COMPARTMENT: LazyLock<StateEffectType<(Compartment, Extension)>> =
    LazyLock::new(StateEffectType::define);

/// What the user did, as a dotted hierarchy.
///
/// The vocabulary the rest of this crate uses: `input`, `input.type`,
/// `input.type.compose`, `input.paste`, `input.drop`, `delete`,
/// `delete.selection`, `delete.forward`, `delete.backward`, `delete.cut`,
/// `move`, `move.drop`, `select`, `select.pointer`, `select.all`, `undo`,
/// `redo`, `insert`, `mark`, `mark.add`, `mark.remove`, `split`, `wrap`,
/// `unwrap`, `settype`.
///
/// [`Transaction::is_user_event`](crate::Transaction::is_user_event) matches on
/// dotted prefixes, so `select` matches `select.pointer`.
pub fn user_event() -> &'static AnnotationType<String> {
    &USER_EVENT
}

/// Whether `event` is `prefix` or a dotted refinement of it, so `select`
/// matches `select.pointer` but not `selection`.
///
/// This is the rule [`Transaction::is_user_event`](crate::Transaction::is_user_event)
/// applies; an extension that holds a user event as a string — a history
/// deciding whether two events belong to one class — uses it directly.
pub fn matches_user_event(event: &str, prefix: &str) -> bool {
    event == prefix
        || (event.len() > prefix.len()
            && event.starts_with(prefix)
            && event.as_bytes()[prefix.len()] == b'.')
}

/// Whether the undo history should record this transaction.
///
/// Absent means "yes". With `false`, the history maps its entries through the
/// change instead of recording it, which is how a remote or programmatic edit
/// keeps the local history valid.
pub fn add_to_history() -> &'static AnnotationType<bool> {
    &ADD_TO_HISTORY
}

/// When the transaction was created, in milliseconds.
///
/// Added automatically by [`EditorState::update`] when a spec does not supply
/// it. Tests that care about grouping supply it.
pub fn time() -> &'static AnnotationType<u64> {
    &TIME
}

/// Whether the transaction represents another actor's edit.
///
/// Corrections and other transaction extenders that add changes skip remote
/// transactions, because acting on them makes collaborating peers correct the
/// same thing over and over.
pub fn remote() -> &'static AnnotationType<bool> {
    &REMOTE
}

/// A free-form provenance tag, for a host that needs to tell its own edits
/// apart beyond what [`user_event`] expresses.
pub fn origin() -> &'static AnnotationType<String> {
    &ORIGIN
}

/// What an appended transaction is reacting to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    /// The user event of the transaction that triggered this one.
    pub trigger_user_event: Option<String>,
    /// The [`origin`] of the transaction that triggered this one.
    pub trigger_origin: Option<String>,
    /// How far this transaction is from the primary one: `1` for a transaction
    /// appended directly to it.
    pub depth: usize,
}

/// Set on every transaction a
/// [`transaction_appender`](crate::transaction_appender) produced, naming what
/// it reacts to.
///
/// The undo history folds an appended transaction into the entry of the
/// transaction that triggered it, so the pair undoes as one step.
pub fn appended() -> &'static AnnotationType<Appended> {
    &APPENDED
}

/// Set on the last appended transaction when the chain was cut at
/// [`MAX_APPENDED_TRANSACTIONS`](crate::MAX_APPENDED_TRANSACTIONS).
pub fn appenders_diverged() -> &'static AnnotationType<bool> {
    &APPENDERS_DIVERGED
}

/// Which side of a transaction an undo boundary is forced on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolateHistory {
    /// Do not merge with earlier transactions.
    Before,
    /// Do not let later transactions merge with this one.
    After,
    /// Both.
    Both,
}

/// Force an undo boundary around a transaction.
///
/// Input rules use it so that undoing an automatic conversion is a step of its
/// own.
pub fn isolate_history() -> &'static AnnotationType<IsolateHistory> {
    &ISOLATE
}

/// An annotation shorthand for [`isolate_history`].
pub fn isolate(kind: IsolateHistory) -> Annotation {
    isolate_history().of(kind)
}

/// Fold a transaction into the undo entry below it, the way an appended one
/// is.
///
/// For a change that finishes the edit recorded there rather than being an
/// edit of its own — a correction that a selection change set off, settling
/// what the previous edit left unsettled. Undoing the entry undoes both.
pub fn fold_into_previous() -> &'static AnnotationType<bool> {
    &FOLD
}

/// Set on a transaction whose [`corrections`](crate::corrections) still wanted
/// to change something after
/// [`MAX_CORRECTION_ROUNDS`](crate::corrections::MAX_CORRECTION_ROUNDS) rounds.
pub fn corrections_diverged() -> &'static AnnotationType<bool> {
    &CORRECTIONS_DIVERGED
}

/// Set on a transaction whose changes lost content to a repair: how many
/// tokens were dropped, as [`ChangeSet::dropped_tokens`](crate::ChangeSet::dropped_tokens)
/// reports.
///
/// Added automatically when a transaction is built, unless a spec already
/// supplies it. A [`Fit`](crate::Fit) repair drops what it can place nowhere —
/// a paste of blocks into a node that only takes text, say — rather than
/// refusing the edit; this is how a host finds out and tells the user.
pub fn content_dropped() -> &'static AnnotationType<usize> {
    &CONTENT_DROPPED
}

/// The user event every composition update carries.
///
/// The history groups transactions with this event into one undo entry.
pub const COMPOSE_USER_EVENT: &str = "input.type.compose";

/// End the active IME composition.
///
/// The composition clears its marked range, and the history treats this as
/// the end of a composition, so the next edit starts a new undo entry.
pub fn end_composition() -> &'static StateEffectType<()> {
    &END_COMPOSITION
}

/// Put fields back to the values they had in an earlier state.
///
/// A state field whose reducer sees this effect reads its own value out of the
/// carried state and returns that, instead of folding the transaction into its
/// current value; a field the carried state does not hold keeps its current
/// value. Honouring it is up to each field — the state machine does not do it
/// for them, because a field derived from the document (rather than from
/// history) must follow the document the transaction actually produces.
///
/// This is how an operation that rolls the document back also rolls back what
/// other extensions recorded about the steps it undoes, without naming them:
/// [`cancel_composition`](crate::composition::cancel_composition) carries the
/// state from before the composition, and the undo history returns to the
/// value it had there, open undo groups included.
///
/// ```
/// use std::sync::LazyLock;
/// use markraft_core::protocol::restore_fields_from;
/// use markraft_core::{StateField, StateFieldConfig, Transaction};
///
/// /// How many transactions changed the document.
/// static EDITS: LazyLock<StateField<u32>> = LazyLock::new(|| {
///     StateField::define(StateFieldConfig::new(|_| 0, |count: &u32, tr: &Transaction| {
///         if let Some(from) = tr.effects().iter().find_map(|e| e.value(restore_fields_from())) {
///             return from.field(&EDITS).copied().unwrap_or(*count);
///         }
///         if tr.doc_changed() { count + 1 } else { *count }
///     }))
/// });
/// # let _ = &*EDITS;
/// ```
pub fn restore_fields_from() -> &'static StateEffectType<EditorState> {
    &RESTORE_FIELDS_FROM
}

/// Replace the root extensions of the configuration.
///
/// Extensions added with [`append_config`] are discarded; the content of
/// compartments is kept, so a compartment that still appears in the new tree
/// keeps whatever it was last reconfigured to.
pub fn reconfigure() -> &'static StateEffectType<Extension> {
    &RECONFIGURE
}

/// Append extensions to the root configuration.
pub fn append_config() -> &'static StateEffectType<Extension> {
    &APPEND_CONFIG
}

/// Replace the content of one compartment. Produced by
/// [`Compartment::reconfigure`].
pub fn compartment_reconfigure() -> &'static StateEffectType<(Compartment, Extension)> {
    &COMPARTMENT
}
