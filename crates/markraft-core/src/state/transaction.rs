//! Transactions: a described edit and the state it produces.
//!
//! A [`TransactionSpec`] describes what to do. [`EditorState::update`] turns
//! one or more specs into a [`Transaction`], running the configured filters and
//! extenders, and the transaction's [`Transaction::state`] is the result.
//!
//! # Combining specs
//!
//! The first spec's changes are in start-document coordinates. So are the next
//! spec's, unless it is marked [`TransactionSpec::sequential`], in which case
//! its positions refer to the document the previous specs produce. Combining
//! two non-sequential specs rebases each set over the other
//! ([`ChangeSet::transform`]) and composes; combining a sequential one simply
//! composes. Selections and effects are mapped through whichever side they did
//! not travel with, so a spec's own positions always mean what it wrote.
//!
//! # Annotations
//!
//! A later spec's annotation replaces an earlier spec's annotation of the same
//! type, exactly as its selection replaces the earlier selection. An annotation
//! therefore describes the *transaction*, not the spec it was written on:
//! [`TransactionSpec::add_to_history`] keeps a whole transaction out of the
//! undo history and must only be set by a caller that means all of it. A spec
//! that merely establishes the range a later one edits leaves it alone.

use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::change::{Change, ChangeDesc, ChangeSet};
use crate::node::Node;
use crate::selection::Selection;
use crate::slice::Slice;

use super::annotation::{self, Annotation, matches_user_event};
use super::effect::{StateEffect, append_config, compartment_reconfigure, reconfigure};
use super::filters::{self, ChangeFilterResult};
use super::{AnnotationType, EditorState, StateEffectType, StateError};

#[derive(Clone)]
enum SpecChanges {
    Set(ChangeSet),
    List(Vec<Change>),
}

/// A description of an edit.
///
/// Every field is optional: a spec with only a selection moves the cursor, a
/// spec with only annotations records metadata.
#[derive(Clone)]
pub struct TransactionSpec {
    changes: Option<SpecChanges>,
    selection: Option<Selection>,
    effects: Vec<StateEffect>,
    annotations: Vec<Annotation>,
    scroll_into_view: bool,
    sequential: bool,
    filter: bool,
}

impl Default for TransactionSpec {
    fn default() -> TransactionSpec {
        TransactionSpec {
            changes: None,
            selection: None,
            effects: Vec::new(),
            annotations: Vec::new(),
            scroll_into_view: false,
            sequential: false,
            filter: true,
        }
    }
}

impl std::fmt::Debug for TransactionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionSpec")
            .field("has_changes", &self.changes.is_some())
            .field("selection", &self.selection)
            .field("effects", &self.effects.len())
            .field("annotations", &self.annotations.len())
            .field("sequential", &self.sequential)
            .finish()
    }
}

impl TransactionSpec {
    /// An empty spec.
    pub fn new() -> TransactionSpec {
        TransactionSpec::default()
    }

    /// Change the document. Positions refer to the document this spec is
    /// resolved against.
    pub fn changes(mut self, changes: impl IntoIterator<Item = Change>) -> Self {
        self.changes = Some(SpecChanges::List(changes.into_iter().collect()));
        self
    }

    /// Change the document with an already-built change set.
    pub fn change_set(mut self, changes: ChangeSet) -> Self {
        self.changes = Some(SpecChanges::Set(changes));
        self
    }

    /// Set the selection explicitly. Positions refer to the document *after*
    /// the transaction.
    pub fn selection(mut self, selection: Selection) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Add an effect.
    pub fn effect(mut self, effect: StateEffect) -> Self {
        self.effects.push(effect);
        self
    }

    /// Add several effects.
    pub fn effects(mut self, effects: impl IntoIterator<Item = StateEffect>) -> Self {
        self.effects.extend(effects);
        self
    }

    /// Add an annotation.
    pub fn annotate(mut self, annotation: Annotation) -> Self {
        self.annotations.push(annotation);
        self
    }

    /// Shorthand for annotating with [`user_event`](super::user_event).
    pub fn user_event(self, event: &str) -> Self {
        self.annotate(annotation::user_event().of(event.to_string()))
    }

    /// Shorthand for annotating with
    /// [`add_to_history`](super::add_to_history).
    pub fn add_to_history(self, add: bool) -> Self {
        self.annotate(annotation::add_to_history().of(add))
    }

    /// Shorthand for annotating with [`time`](super::time). Without this the
    /// transaction is stamped with the current wall-clock time.
    pub fn time(self, millis: u64) -> Self {
        self.annotate(annotation::time().of(millis))
    }

    /// Shorthand for annotating with [`remote`](super::remote).
    pub fn remote(self, remote: bool) -> Self {
        self.annotate(annotation::remote().of(remote))
    }

    /// Ask the view to scroll the selection into view.
    pub fn scroll_into_view(mut self) -> Self {
        self.scroll_into_view = true;
        self
    }

    /// Resolve this spec against the document the previous specs produce,
    /// rather than against the start document.
    pub fn sequential(mut self) -> Self {
        self.sequential = true;
        self
    }

    /// Skip the configured transaction and change filters.
    ///
    /// Setting this on any spec disables filtering for the whole transaction,
    /// which is what lets a history command restore a document a filter would
    /// otherwise refuse.
    pub fn no_filter(mut self) -> Self {
        self.filter = false;
        self
    }
}

impl Selection {
    /// Add a change to `spec` replacing this selection with `slice`.
    pub fn replace(&self, spec: TransactionSpec, doc: &Node, slice: Slice) -> TransactionSpec {
        match self {
            Selection::Custom(kind) => kind.replace(spec, doc, slice),
            _ => {
                let range = self.replacement_range(doc);
                spec.changes([Change::replace(range.from, range.to, slice)])
            }
        }
    }
}

pub(crate) struct Resolved {
    pub(crate) changes: ChangeSet,
    pub(crate) selection: Option<Selection>,
    pub(crate) effects: Vec<StateEffect>,
    pub(crate) annotations: Vec<Annotation>,
    pub(crate) scroll_into_view: bool,
}

struct TransactionData {
    start_state: EditorState,
    changes: ChangeSet,
    new_doc: Node,
    selection: Option<Selection>,
    effects: Vec<StateEffect>,
    annotations: Vec<Annotation>,
    scroll_into_view: bool,
    reconfigured: bool,
    state: OnceLock<EditorState>,
}

/// A resolved edit, together with the state it produces.
///
/// Cloning is a reference-count bump.
#[derive(Clone)]
pub struct Transaction(Arc<TransactionData>);

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("doc_changed", &self.doc_changed())
            .field("effects", &self.0.effects.len())
            .field("reconfigured", &self.0.reconfigured)
            .finish()
    }
}

impl Transaction {
    pub(crate) fn create(
        state: &EditorState,
        resolved: Resolved,
    ) -> Result<Transaction, StateError> {
        let new_doc = resolved.changes.apply(state.doc())?;
        let mut annotations = resolved.annotations;
        if !annotations.iter().any(|a| a.is(annotation::time())) {
            annotations.push(annotation::time().of(now_millis()));
        }
        let reconfigured = resolved.effects.iter().any(|effect| {
            effect.is(reconfigure())
                || effect.is(append_config())
                || effect.is(compartment_reconfigure())
        });
        Ok(Transaction(Arc::new(TransactionData {
            start_state: state.clone(),
            changes: resolved.changes,
            new_doc,
            selection: resolved.selection,
            effects: resolved.effects,
            annotations,
            scroll_into_view: resolved.scroll_into_view,
            reconfigured,
            state: OnceLock::new(),
        })))
    }

    /// The state this transaction starts from.
    pub fn start_state(&self) -> &EditorState {
        &self.0.start_state
    }

    /// The changes this transaction makes.
    pub fn changes(&self) -> &ChangeSet {
        &self.0.changes
    }

    /// Whether the document changed.
    pub fn doc_changed(&self) -> bool {
        !self.0.changes.is_empty()
    }

    /// The resulting document.
    ///
    /// Reading this does not force the rest of the new state to be computed,
    /// which is what makes it the right thing for a transaction extender to
    /// look at.
    pub fn new_doc(&self) -> &Node {
        &self.0.new_doc
    }

    /// The selection the transaction set explicitly, if any.
    pub fn selection(&self) -> Option<&Selection> {
        self.0.selection.as_ref()
    }

    /// The resulting selection: the explicit one, or the start selection mapped
    /// through the changes.
    pub fn new_selection(&self) -> Selection {
        match &self.0.selection {
            Some(selection) => selection.clone(),
            None => self.0.start_state.selection().map(
                self.0.start_state.schema(),
                &self.0.new_doc,
                &self.0.changes.desc(),
            ),
        }
    }

    /// The effects attached to this transaction.
    pub fn effects(&self) -> &[StateEffect] {
        &self.0.effects
    }

    /// The annotations attached to this transaction.
    pub fn annotations(&self) -> &[Annotation] {
        &self.0.annotations
    }

    /// The value of an annotation of the given type, if present.
    ///
    /// Within one spec the first of several annotations of a type wins. Across
    /// specs the later spec's annotation replaced the earlier one when they
    /// were merged, so what is left here is already the winner.
    pub fn annotation<T: Send + Sync + 'static>(&self, ty: &AnnotationType<T>) -> Option<&T> {
        self.0.annotations.iter().find_map(|a| a.value(ty))
    }

    /// Whether an effect of the given type is attached.
    pub fn has_effect<T: Send + Sync + 'static>(&self, ty: &StateEffectType<T>) -> bool {
        self.0.effects.iter().any(|effect| effect.is(ty))
    }

    /// The transaction's user event, if it has one.
    pub fn user_event_name(&self) -> Option<&str> {
        self.annotation(annotation::user_event())
            .map(String::as_str)
    }

    /// Whether the transaction's user event is `prefix` or a dotted refinement
    /// of it, so `select` matches `select.pointer`.
    pub fn is_user_event(&self, prefix: &str) -> bool {
        self.user_event_name()
            .is_some_and(|event| matches_user_event(event, prefix))
    }

    /// Whether the view should scroll the selection into view.
    pub fn scroll_into_view(&self) -> bool {
        self.0.scroll_into_view
    }

    /// Whether this transaction changes the configuration.
    pub fn reconfigured(&self) -> bool {
        self.0.reconfigured
    }

    /// The state this transaction produces.
    ///
    /// Computed on first use and cached. A state field's `update` must not call
    /// this: the state it would ask for is the one being built.
    pub fn state(&self) -> &EditorState {
        self.0
            .state
            .get_or_init(|| self.0.start_state.apply_transaction(self))
    }

    /// This transaction as a spec, in start-state coordinates.
    ///
    /// A [`transaction_filter`](super::transaction_filter) uses this to amend a
    /// transaction rather than rewrite it from scratch.
    pub fn as_spec(&self) -> TransactionSpec {
        let mut spec = TransactionSpec::new()
            .change_set(self.0.changes.clone())
            .effects(self.0.effects.clone());
        if let Some(selection) = &self.0.selection {
            spec = spec.selection(selection.clone());
        }
        for annotation in &self.0.annotations {
            spec = spec.annotate(annotation.clone());
        }
        if self.0.scroll_into_view {
            spec = spec.scroll_into_view();
        }
        spec
    }

    fn resolved(&self) -> Resolved {
        Resolved {
            changes: self.0.changes.clone(),
            selection: self.0.selection.clone(),
            effects: self.0.effects.clone(),
            annotations: self.0.annotations.clone(),
            scroll_into_view: self.0.scroll_into_view,
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn resolve_inner(
    state: &EditorState,
    spec: &TransactionSpec,
    doc: &Node,
) -> Result<Resolved, StateError> {
    let changes = match &spec.changes {
        None => ChangeSet::empty(state.schema(), doc.content_size()),
        Some(SpecChanges::Set(set)) => {
            // A prebuilt set names a document; resolving it against a different
            // one would silently mis-position every change.
            if set.length_before() != doc.content_size() {
                return Err(crate::error::ChangeError::LengthMismatch {
                    expected: set.length_before(),
                    actual: doc.content_size(),
                }
                .into());
            }
            set.clone()
        }
        Some(SpecChanges::List(list)) => {
            ChangeSet::create(state.schema(), doc, list.iter().cloned())?
        }
    };
    Ok(Resolved {
        changes,
        selection: spec.selection.clone(),
        effects: spec.effects.clone(),
        annotations: spec.annotations.clone(),
        scroll_into_view: spec.scroll_into_view,
    })
}

fn merge(
    state: &EditorState,
    a: Resolved,
    b: Resolved,
    sequential: bool,
) -> Result<Resolved, StateError> {
    let (changes, map_for_a, map_for_b) = if sequential {
        let map_for_a = b.changes.desc();
        let map_for_b = ChangeDesc::empty(b.changes.length_after());
        (a.changes.compose(&b.changes)?, map_for_a, map_for_b)
    } else {
        let (a_over_b, b_over_a) = a.changes.transform(state.doc(), &b.changes, true)?;
        let map_for_a = b_over_a.desc();
        let map_for_b = a_over_b.desc();
        (a.changes.compose(&b_over_a)?, map_for_a, map_for_b)
    };
    let doc = changes.apply(state.doc())?;
    let selection = match b.selection {
        Some(selection) => Some(selection.map(state.schema(), &doc, &map_for_b)),
        None => a
            .selection
            .map(|selection| selection.map(state.schema(), &doc, &map_for_a)),
    };
    let mut effects = StateEffect::map_all(&a.effects, &map_for_a);
    effects.extend(StateEffect::map_all(&b.effects, &map_for_b));
    // A later spec has the last word on any annotation it sets, so a spec can
    // amend what an earlier one said rather than being shadowed by it. This is
    // what keeps `add_to_history(false)` on a spec that only establishes a
    // selection from deciding the whole transaction.
    let mut annotations = a.annotations;
    annotations.retain(|earlier| {
        !b.annotations
            .iter()
            .any(|later| later.type_id() == earlier.type_id())
    });
    annotations.extend(b.annotations);
    Ok(Resolved {
        changes,
        selection,
        effects,
        annotations,
        scroll_into_view: a.scroll_into_view || b.scroll_into_view,
    })
}

pub(crate) fn resolve(
    state: &EditorState,
    specs: Vec<TransactionSpec>,
    mut filter: bool,
    extend: bool,
) -> Result<Transaction, StateError> {
    let mut iter = specs.into_iter();
    let first = iter.next().unwrap_or_default();
    filter &= first.filter;
    let mut acc = resolve_inner(state, &first, state.doc())?;
    for spec in iter {
        filter &= spec.filter;
        let sequential = spec.sequential;
        let base = if sequential {
            acc.changes.apply(state.doc())?
        } else {
            state.doc().clone()
        };
        let next = resolve_inner(state, &spec, &base)?;
        acc = merge(state, acc, next, sequential)?;
    }
    let tr = Transaction::create(state, acc)?;
    let tr = if filter { filter_transaction(tr)? } else { tr };
    if extend {
        extend_transaction(tr)
    } else {
        Ok(tr)
    }
}

/// Run the change filters and then the transaction filters.
///
/// Change filters run first and in configuration order; a `Block` result stops
/// the scan. Transaction filters run in *reverse* configuration order, so the
/// highest-precedence filter sees the result of the lower-precedence ones and
/// therefore has the last word.
fn filter_transaction(tr: Transaction) -> Result<Transaction, StateError> {
    let tr = apply_change_filters(tr)?;
    let state = tr.start_state().clone();
    let filters = state.facet(filters::transaction_filter()).clone();
    let mut tr = tr;
    for filter in filters.iter().rev() {
        if let Some(specs) = filter(&tr) {
            tr = resolve(&state, specs, false, false)?;
        }
    }
    Ok(tr)
}

fn apply_change_filters(tr: Transaction) -> Result<Transaction, StateError> {
    let state = tr.start_state().clone();
    let change_filters = state.facet(filters::change_filter()).clone();
    if change_filters.is_empty() || !tr.doc_changed() {
        return Ok(tr);
    }
    let mut allowed: Option<Vec<(usize, usize)>> = None;
    let mut blocked = false;
    for filter in &change_filters {
        match filter(&tr) {
            ChangeFilterResult::Allow => {}
            ChangeFilterResult::Block => {
                blocked = true;
                break;
            }
            ChangeFilterResult::AllowOnly(ranges) => {
                allowed = Some(match allowed {
                    Some(existing) => filters::union_ranges(&existing, &ranges),
                    None => filters::normalise_ranges(&ranges),
                });
            }
        }
    }
    let filtered = match (blocked, &allowed) {
        (true, _) => ChangeSet::empty(state.schema(), state.doc().content_size()),
        (false, Some(ranges)) => filters::restrict_changes(tr.changes(), ranges),
        (false, None) => return Ok(tr),
    };
    if filtered == *tr.changes() {
        return Ok(tr);
    }
    // `back` maps the document the unfiltered transaction would have produced
    // onto the one the filtered transaction produces, so the selection and the
    // effects keep pointing at the content they were written for.
    let back = tr.changes().invert(state.doc())?.compose(&filtered)?;
    let back_desc = back.desc();
    let new_doc = filtered.apply(state.doc())?;
    let selection = tr
        .selection()
        .map(|selection| selection.map(state.schema(), &new_doc, &back_desc));
    let effects = StateEffect::map_all(tr.effects(), &back_desc);
    Transaction::create(
        &state,
        Resolved {
            changes: filtered,
            selection,
            effects,
            annotations: tr.annotations().to_vec(),
            scroll_into_view: tr.scroll_into_view(),
        },
    )
}

/// Let the configured extenders add to the transaction.
///
/// Extenders run in reverse configuration order, each seeing the *original*
/// transaction, and what they return is merged sequentially: their positions
/// refer to the document produced so far, and the changes they add are composed
/// exactly. Wordgard's extenders may only add effects and annotations; allowing
/// changes is what lets [`Correction`](crate::Correction) be one.
fn extend_transaction(tr: Transaction) -> Result<Transaction, StateError> {
    let state = tr.start_state().clone();
    let extenders = state.facet(filters::transaction_extender()).clone();
    if extenders.is_empty() {
        return Ok(tr);
    }
    let mut acc = tr.resolved();
    let mut extended = false;
    for extender in extenders.iter().rev() {
        let Some(spec) = extender(&tr) else { continue };
        let base = acc.changes.apply(state.doc())?;
        let next = resolve_inner(&state, &spec, &base)?;
        acc = merge(&state, acc, next, true)?;
        extended = true;
    }
    if !extended {
        return Ok(tr);
    }
    Transaction::create(&state, acc)
}
