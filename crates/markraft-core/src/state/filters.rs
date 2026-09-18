//! Hooks that inspect a transaction before and after it is applied.
//!
//! Four facets, in the order they run:
//!
//! 1. [`change_filter`] — decides which parts of the document a transaction may
//!    touch. Filters run in configuration order; the first `Block` wins, and
//!    range results are unioned.
//! 2. [`transaction_filter`] — may replace a transaction with different specs,
//!    or block it by returning an empty list. Filters run in *reverse*
//!    configuration order, so the highest-precedence filter runs last and has
//!    the final say.
//! 3. [`transaction_extender`] — adds to a transaction. Extenders also run in
//!    reverse configuration order, each seeing the original transaction, and
//!    their specs are merged sequentially so the changes they add compose
//!    exactly.
//!
//! 4. [`transaction_appender`] — reacts to a finished transaction by producing
//!    *another* one. Appended transactions are separate transactions applied in
//!    order on the state each of them leaves behind, so a view layer sees every
//!    step. Only [`EditorState::update_with_appended`](super::EditorState::update_with_appended)
//!    runs them; plain `update` returns the primary transaction alone.
//!
//! A spec marked [`TransactionSpec::no_filter`] turns the first three off for
//! that transaction. Appenders are not a filter and always run.

use std::sync::{Arc, LazyLock};

use crate::change::{ChangeSet, SectionBuilder, SectionOp};

use super::annotation::AnnotationType;
use super::facet::Facet;
use super::transaction::{Transaction, TransactionSpec};

/// A function registered with [`transaction_filter`].
///
/// `None` leaves the transaction alone, a list of specs replaces it — build one
/// from [`Transaction::as_spec`] to amend rather than rewrite — and an empty
/// list blocks it.
pub type TransactionFilterFn =
    Arc<dyn Fn(&Transaction) -> Option<Vec<TransactionSpec>> + Send + Sync>;

/// A function registered with [`transaction_extender`].
pub type TransactionExtenderFn = Arc<dyn Fn(&Transaction) -> Option<TransactionSpec> + Send + Sync>;

/// A function registered with [`change_filter`].
pub type ChangeFilterFn = Arc<dyn Fn(&Transaction) -> ChangeFilterResult + Send + Sync>;

/// A function registered with [`transaction_appender`].
///
/// It receives a transaction that has already been resolved and returns the
/// spec of a transaction to apply after it, or `None` to stay out of the way.
/// An appender that reacts to every transaction must check
/// [`Transaction::annotation`] for [`appended`] so it does not react to its own
/// output.
pub type TransactionAppenderFn = Arc<dyn Fn(&Transaction) -> Option<TransactionSpec> + Send + Sync>;

/// What an appended transaction is reacting to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    /// The user event of the transaction that triggered this one.
    pub trigger_user_event: Option<String>,
    /// The [`origin`](super::origin) of the transaction that triggered this one.
    pub trigger_origin: Option<String>,
    /// How far this transaction is from the primary one: `1` for a transaction
    /// appended directly to it.
    pub depth: usize,
}

/// What a [`change_filter`] allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeFilterResult {
    /// Let every change through.
    Allow,
    /// Drop every change.
    Block,
    /// Only let changes through that fall entirely inside one of these ranges.
    AllowOnly(Vec<(usize, usize)>),
}

static TRANSACTION_FILTER: LazyLock<Facet<TransactionFilterFn>> = LazyLock::new(Facet::list);
static TRANSACTION_EXTENDER: LazyLock<Facet<TransactionExtenderFn>> = LazyLock::new(Facet::list);
static CHANGE_FILTER: LazyLock<Facet<ChangeFilterFn>> = LazyLock::new(Facet::list);
static TRANSACTION_APPENDER: LazyLock<Facet<TransactionAppenderFn>> = LazyLock::new(Facet::list);
static APPENDED: LazyLock<AnnotationType<Appended>> = LazyLock::new(AnnotationType::define);
static APPENDERS_DIVERGED: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);

/// Register a hook that may replace or block a transaction.
pub fn transaction_filter() -> &'static Facet<TransactionFilterFn> {
    &TRANSACTION_FILTER
}

/// Register a hook that may add changes, effects, annotations or a selection to
/// a transaction.
pub fn transaction_extender() -> &'static Facet<TransactionExtenderFn> {
    &TRANSACTION_EXTENDER
}

/// Register a hook that protects parts of the document from being changed.
pub fn change_filter() -> &'static Facet<ChangeFilterFn> {
    &CHANGE_FILTER
}

/// Register a hook that reacts to a transaction with another transaction.
pub fn transaction_appender() -> &'static Facet<TransactionAppenderFn> {
    &TRANSACTION_APPENDER
}

/// Set on every transaction an appender produced, naming what it reacts to.
///
/// The undo history folds an appended transaction into the entry of the
/// transaction that triggered it, so the pair undoes as one step.
pub fn appended() -> &'static AnnotationType<Appended> {
    &APPENDED
}

/// Set on the last appended transaction when the chain was cut at
/// [`MAX_APPENDED_TRANSACTIONS`](super::MAX_APPENDED_TRANSACTIONS).
pub fn appenders_diverged() -> &'static AnnotationType<bool> {
    &APPENDERS_DIVERGED
}

/// Sort and merge overlapping or touching ranges.
pub(crate) fn normalise_ranges(ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut sorted: Vec<(usize, usize)> = ranges
        .iter()
        .map(|(from, to)| (*from.min(to), *from.max(to)))
        .collect();
    sorted.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(sorted.len());
    for (from, to) in sorted {
        match out.last_mut() {
            Some(last) if last.1 >= from => last.1 = last.1.max(to),
            _ => out.push((from, to)),
        }
    }
    out
}

/// The union of two range lists.
pub(crate) fn union_ranges(a: &[(usize, usize)], b: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    normalise_ranges(&all)
}

/// A copy of `set` in which every change outside `allowed` is dropped.
///
/// A change is kept only when the whole stretch it covers in the starting
/// document lies inside one allowed range, so a protected range can never be
/// partly rewritten.
pub(crate) fn restrict_changes(set: &ChangeSet, allowed: &[(usize, usize)]) -> ChangeSet {
    let mut builder = SectionBuilder::new();
    let mut pos = 0usize;
    for section in &set.sections {
        let end = pos + section.len;
        let permitted = allowed.iter().any(|(from, to)| *from <= pos && end <= *to);
        match (&section.op, permitted) {
            (SectionOp::Keep, _) | (_, false) => builder.keep(section.len),
            (SectionOp::Mark(mods), true) => builder.mark(section.len, mods.clone()),
            (SectionOp::Replace(tokens), true) => builder.replace(section.len, tokens.clone()),
        }
        pos = end;
    }
    builder.finish(set.schema(), set.length_before())
}
