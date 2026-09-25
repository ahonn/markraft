//! The editor state: a document, a selection, and everything extensions put
//! next to them.
//!
//! [`EditorState`] is an immutable value. An edit is a [`Transaction`], built
//! from one or more [`TransactionSpec`]s, which produces the next state:
//!
//! ```
//! use markraft_core::{EditorState, EditorStateConfig, Extension, NodeTypeSpec, Schema, SchemaSpec, Selection, TransactionSpec};
//!
//! let schema = Schema::new(
//!     SchemaSpec::new()
//!         .node(NodeTypeSpec::new("doc", "block+"))
//!         .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
//!         .node(NodeTypeSpec::text("text").group("inline")),
//! )
//! .unwrap();
//! let state = EditorState::create(EditorStateConfig::new(schema)).unwrap();
//! let tr = state.update([TransactionSpec::new().selection(Selection::cursor(0))]).unwrap();
//! assert_eq!(tr.state().selection(), &Selection::cursor(0));
//! # let _ = Extension::none();
//! ```
//!
//! # How values are stored
//!
//! Extensions contribute [`StateField`]s (values folded forward by a reducer)
//! and facet inputs ([`Facet`]). Both are type-erased behind
//! `Arc<dyn Any + Send + Sync>` and addressed by a process-wide unique id, so
//! one state can hold values of arbitrary types and still be
//! `Clone + Send + Sync`. Accessors downcast back to the type the definition
//! declares, which cannot fail because only the definition can produce the
//! value.
//!
//! Slots are computed eagerly when a state is built, in dependency order: a
//! slot that reads another resolves it first. A dependency cycle is a
//! programming error and panics with the slot involved.

mod annotation;
mod appender;
mod config;
mod effect;
mod extension;
mod facet;
mod field;
pub(crate) mod filters;
mod json;
pub mod protocol;
mod transaction;

pub use annotation::{Annotation, AnnotationType};
pub use appender::MAX_APPENDED_TRANSACTIONS;
pub use config::Configuration;
pub use effect::{StateEffect, StateEffectType};
pub use extension::{Compartment, Extension, Prec};
pub use facet::{Dep, Facet, FacetConfig};
pub use field::{StateField, StateFieldConfig, StateJsonFields};
pub use filters::{
    ChangeFilterFn, ChangeFilterResult, TransactionAppenderFn, TransactionExtenderFn,
    TransactionFilterFn, change_filter, transaction_appender, transaction_extender,
    transaction_filter,
};
pub use transaction::{Transaction, TransactionSpec};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;

use crate::attr::Attrs;
use crate::error::{ChangeError, NodeError};
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::pos::ResolvedPos;
use crate::schema::Schema;
use crate::selection::Selection;

use config::{Address, Slot};
use facet::{AnyValue, ProviderKind};
use protocol::{append_config, compartment_reconfigure, reconfigure};

/// Failure while creating or updating an [`EditorState`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StateError {
    /// A transaction's changes could not be created or applied.
    #[error(transparent)]
    Change(#[from] ChangeError),
    /// The document or selection is not valid.
    #[error(transparent)]
    Node(#[from] NodeError),
    /// State JSON did not describe a state for this configuration.
    #[error("invalid state JSON: {0}")]
    Json(String),
}

/// How to create an [`EditorState`].
pub struct EditorStateConfig {
    /// The schema the state's documents belong to. Shared, so cloning it is a
    /// reference-count bump.
    pub schema: Schema,
    /// The starting document. Defaults to the smallest valid document the
    /// schema allows.
    pub doc: Option<Node>,
    /// The starting selection. Defaults to a cursor at the start of the
    /// document.
    pub selection: Option<Selection>,
    /// The starting stored marks. Defaults to none.
    pub stored_marks: Option<MarkSet>,
    /// The extensions to configure.
    pub extensions: Extension,
}

impl EditorStateConfig {
    /// A configuration with no extensions and a default document.
    pub fn new(schema: Schema) -> EditorStateConfig {
        EditorStateConfig {
            schema,
            doc: None,
            selection: None,
            stored_marks: None,
            extensions: Extension::none(),
        }
    }

    /// Start from `doc`.
    pub fn doc(mut self, doc: Node) -> Self {
        self.doc = Some(doc);
        self
    }

    /// Start with `selection`.
    pub fn selection(mut self, selection: Selection) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Start with `marks` stored for the next insertion.
    pub fn stored_marks(mut self, marks: MarkSet) -> Self {
        self.stored_marks = Some(marks);
        self
    }

    /// Configure `extensions`.
    pub fn extensions(mut self, extensions: Extension) -> Self {
        self.extensions = extensions;
        self
    }
}

#[derive(Clone)]
pub(crate) struct SlotValue {
    value: AnyValue,
    changed: bool,
}

impl SlotValue {
    fn changed(value: AnyValue) -> SlotValue {
        SlotValue {
            value,
            changed: true,
        }
    }

    fn kept(value: AnyValue) -> SlotValue {
        SlotValue {
            value,
            changed: false,
        }
    }
}

enum ResolveSource {
    /// A fresh state. The map holds values for fields whose starting value is
    /// supplied from outside — which is how `from_json` restores a field.
    Create(std::collections::HashMap<u64, AnyValue>),
    Reconfigure(EditorState),
    Update {
        tr: Transaction,
        start: Vec<SlotValue>,
    },
}

struct StateData {
    schema: Schema,
    config: Arc<Configuration>,
    doc: Node,
    selection: Selection,
    stored_marks: Option<MarkSet>,
    slots: Vec<OnceLock<SlotValue>>,
    /// What the slots are computed from. Dropped once every slot is resolved,
    /// so a state never keeps the transaction that produced it alive.
    source: Mutex<Option<Arc<ResolveSource>>>,
}

thread_local! {
    static RESOLVING: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// A complete editor state.
///
/// Cloning is a reference-count bump. Two states produced by different
/// transactions share every field value that did not change.
#[derive(Clone)]
pub struct EditorState(Arc<StateData>);

impl std::fmt::Debug for EditorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorState")
            .field("doc_size", &self.0.doc.content_size())
            .field("selection", &self.0.selection)
            .field("stored_marks", &self.0.stored_marks)
            .finish()
    }
}

impl EditorState {
    /// Create a state.
    ///
    /// Validates the document against the schema and the selection against the
    /// document, then builds every configured field and facet.
    pub fn create(config: EditorStateConfig) -> Result<EditorState, StateError> {
        EditorState::create_with(config, std::collections::HashMap::new())
    }

    pub(crate) fn create_with(
        config: EditorStateConfig,
        overrides: std::collections::HashMap<u64, AnyValue>,
    ) -> Result<EditorState, StateError> {
        let EditorStateConfig {
            schema,
            doc,
            selection,
            stored_marks,
            extensions,
        } = config;
        let doc = match doc {
            Some(doc) => doc,
            None => schema
                .create_and_fill(
                    schema.top_type(),
                    Attrs::empty(),
                    MarkSet::empty(),
                    Fragment::empty(),
                )
                .ok_or_else(|| StateError::Json("the schema has no valid empty document".into()))?,
        };
        doc.check(&schema)?;
        let selection = match selection {
            Some(selection) => selection,
            None => Selection::at_start(&schema, &doc),
        };
        selection.check(&doc, &schema)?;
        let configuration = Arc::new(Configuration::resolve(extensions, BTreeMap::new(), None));
        Ok(EditorState::build(
            schema,
            configuration,
            doc,
            selection,
            stored_marks,
            ResolveSource::Create(overrides),
        ))
    }

    fn build(
        schema: Schema,
        config: Arc<Configuration>,
        doc: Node,
        selection: Selection,
        stored_marks: Option<MarkSet>,
        source: ResolveSource,
    ) -> EditorState {
        let slots = (0..config.slot_count()).map(|_| OnceLock::new()).collect();
        let state = EditorState(Arc::new(StateData {
            schema,
            config,
            doc,
            selection,
            stored_marks,
            slots,
            source: Mutex::new(Some(Arc::new(source))),
        }));
        for index in 0..state.0.config.slot_count() {
            state.ensure(index);
        }
        *state.source_lock() = None;
        state
    }

    fn source_lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<ResolveSource>>> {
        self.0
            .source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The schema every document in this state belongs to.
    pub fn schema(&self) -> &Schema {
        &self.0.schema
    }

    /// The document.
    pub fn doc(&self) -> &Node {
        &self.0.doc
    }

    /// The selection.
    pub fn selection(&self) -> &Selection {
        &self.0.selection
    }

    /// The moving end of the selection.
    pub fn head(&self) -> usize {
        self.selection().head(self.doc())
    }

    /// The selection's head resolved against the document, with its ancestor
    /// chain — what nearly every command asks first.
    pub fn resolved_head(&self) -> Option<ResolvedPos> {
        self.doc().resolve(self.head()).ok()
    }

    /// The marks content typed next should get, overriding the marks the
    /// surrounding content would give it.
    ///
    /// Set by a transaction through [`TransactionSpec::stored_marks`]. A
    /// transaction that does not set them keeps them only when it neither
    /// changes the document nor sets a selection; see
    /// [`Transaction::new_stored_marks`].
    pub fn stored_marks(&self) -> Option<&MarkSet> {
        self.0.stored_marks.as_ref()
    }

    /// The resolved configuration.
    pub fn config(&self) -> &Configuration {
        &self.0.config
    }

    /// A field's value, when the field is configured.
    pub fn field<T: Send + Sync + 'static>(&self, field: &StateField<T>) -> Option<&T> {
        match self.0.config.address_of(field.id())? {
            Address::Dynamic(index) => Some(facet::downcast(&self.ensure(index).value)),
            Address::Static(_) => None,
        }
    }

    /// A facet's output.
    ///
    /// A facet that no extension provides an input for reads its default value,
    /// which is why the facet has to outlive the borrow.
    pub fn facet<'a, I, O: Send + Sync + 'static>(&'a self, facet: &'a Facet<I, O>) -> &'a O {
        match self.0.config.address_of(facet.id()) {
            Some(Address::Static(index)) => facet::downcast(self.0.config.static_value(index)),
            Some(Address::Dynamic(index)) => facet::downcast(&self.ensure(index).value),
            None => facet.default_value(),
        }
    }

    /// Build a transaction from one or more specs.
    ///
    /// Specs are combined in order: by default a later spec's positions refer
    /// to the *starting* document and the two change sets are rebased over each
    /// other, while a spec marked [`TransactionSpec::sequential`] is taken to
    /// refer to the document the previous specs produce.
    ///
    /// This is fallible: turning a [`Change`](crate::Change)
    /// list into a [`ChangeSet`](crate::ChangeSet) and applying it can fail, and
    /// reporting that here is what makes every [`Transaction`] accessor
    /// infallible.
    pub fn update(
        &self,
        specs: impl IntoIterator<Item = TransactionSpec>,
    ) -> Result<Transaction, StateError> {
        transaction::resolve(self, specs.into_iter().collect(), true, true)
    }

    /// Build a transaction and run the configured
    /// [`transaction_appender`]s over it.
    ///
    /// The first element is the transaction `specs` describes; the rest are the
    /// transactions appenders produced, each already resolved against the state
    /// the one before it leaves behind. Applying them in order is what a view
    /// layer does; every one of them has a valid
    /// [`state`](Transaction::state).
    pub fn update_with_appended(
        &self,
        specs: impl IntoIterator<Item = TransactionSpec>,
    ) -> Result<Vec<Transaction>, StateError> {
        appender::append(self.update(specs)?)
    }

    /// The state a transaction produces. Same as [`Transaction::state`].
    pub fn apply(&self, tr: &Transaction) -> EditorState {
        debug_assert!(
            std::ptr::eq(Arc::as_ptr(&self.0), Arc::as_ptr(&tr.start_state().0)),
            "a transaction may only be applied to the state it was created from"
        );
        tr.state().clone()
    }

    pub(crate) fn apply_transaction(&self, tr: &Transaction) -> EditorState {
        let mut base = self.0.config.base().clone();
        let mut compartments = self.0.config.compartment_map().clone();
        let mut reconfigured = false;
        for effect in tr.effects() {
            if let Some(extension) = effect.value(reconfigure()) {
                base = extension.clone();
                reconfigured = true;
            } else if let Some(extension) = effect.value(append_config()) {
                base = Extension::all([base.clone(), extension.clone()]);
                reconfigured = true;
            } else if let Some((compartment, extension)) = effect.value(compartment_reconfigure()) {
                compartments.insert(compartment.id(), extension.clone());
                reconfigured = true;
            }
        }
        let (config, start) = if reconfigured {
            let config = Arc::new(Configuration::resolve(base, compartments, Some(self)));
            let intermediate = EditorState::build(
                self.0.schema.clone(),
                config.clone(),
                self.0.doc.clone(),
                self.0.selection.clone(),
                self.0.stored_marks.clone(),
                ResolveSource::Reconfigure(self.clone()),
            );
            let values = intermediate.slot_values();
            (config, values)
        } else {
            (self.0.config.clone(), self.slot_values())
        };
        EditorState::build(
            self.0.schema.clone(),
            config,
            tr.new_doc().clone(),
            tr.new_selection(),
            tr.new_stored_marks(),
            ResolveSource::Update {
                tr: tr.clone(),
                start,
            },
        )
    }

    fn slot_values(&self) -> Vec<SlotValue> {
        self.0
            .slots
            .iter()
            .map(|cell| {
                cell.get()
                    .cloned()
                    .expect("every slot is resolved when a state is built")
            })
            .collect()
    }

    pub(crate) fn raw_facet(&self, id: u64) -> Option<AnyValue> {
        match self.0.config.address_of(id)? {
            Address::Static(index) => Some(self.0.config.static_value(index).clone()),
            Address::Dynamic(index) => self.0.slots[index].get().map(|slot| slot.value.clone()),
        }
    }

    fn raw_slot(&self, id: u64) -> Option<AnyValue> {
        match self.0.config.address_of(id)? {
            Address::Dynamic(index) => self.0.slots[index].get().map(|slot| slot.value.clone()),
            Address::Static(index) => Some(self.0.config.static_value(index).clone()),
        }
    }

    fn ensure(&self, index: usize) -> &SlotValue {
        if let Some(value) = self.0.slots[index].get() {
            return value;
        }
        let source = self
            .source_lock()
            .clone()
            .expect("slots are only resolved while a state is being built");
        let key = (Arc::as_ptr(&self.0) as *const () as usize, index);
        RESOLVING.with(|stack| {
            let mut stack = stack.borrow_mut();
            assert!(
                !stack.contains(&key),
                "configuration slot {index} depends on itself"
            );
            stack.push(key);
        });
        let value = self.compute_slot(index, &source);
        RESOLVING.with(|stack| {
            stack.borrow_mut().pop();
        });
        self.0.slots[index].get_or_init(|| value)
    }

    fn compute_slot(&self, index: usize, source: &ResolveSource) -> SlotValue {
        match self.0.config.slot(index) {
            Slot::Field(field) => match source {
                ResolveSource::Create(overrides) => match overrides.get(&field.id()) {
                    Some(value) => SlotValue::changed(value.clone()),
                    None => SlotValue::changed(field.create_any(self)),
                },
                ResolveSource::Reconfigure(old) => match old.raw_slot(field.id()) {
                    Some(value) => SlotValue::kept(value),
                    None => SlotValue::changed(field.create_any(self)),
                },
                ResolveSource::Update { tr, start } => {
                    let previous = &start[index];
                    let next = field.update_any(&previous.value, tr);
                    if field.compare_any(&previous.value, &next) {
                        SlotValue::kept(previous.value.clone())
                    } else {
                        SlotValue::changed(next)
                    }
                }
            },
            Slot::Provider(provider) => {
                let ProviderKind::Computed(compute) = &provider.kind else {
                    unreachable!("a static input never gets a dynamic slot")
                };
                match source {
                    ResolveSource::Create(_) => SlotValue::changed(compute(self)),
                    ResolveSource::Reconfigure(old) => match old.raw_slot(provider.id) {
                        Some(value) => SlotValue::kept(value),
                        None => SlotValue::changed(compute(self)),
                    },
                    ResolveSource::Update { tr, start } => {
                        let previous = &start[index];
                        if !self.deps_changed(&provider.deps, tr) {
                            return SlotValue::kept(previous.value.clone());
                        }
                        let next = compute(self);
                        if provider.facet.compare_inputs_any(&previous.value, &next) {
                            SlotValue::kept(previous.value.clone())
                        } else {
                            SlotValue::changed(next)
                        }
                    }
                }
            }
            Slot::Facet { facet, inputs } => {
                let mut changed = matches!(source, ResolveSource::Create(_));
                let mut values = Vec::with_capacity(inputs.len());
                for input in inputs {
                    match input {
                        Address::Static(at) => values.push(self.0.config.static_value(*at).clone()),
                        Address::Dynamic(at) => {
                            let slot = self.ensure(*at);
                            changed |= slot.changed;
                            values.push(slot.value.clone());
                        }
                    }
                }
                let previous = match source {
                    ResolveSource::Create(_) => None,
                    ResolveSource::Reconfigure(old) => old.raw_facet(facet.id()),
                    ResolveSource::Update { start, .. } => Some(start[index].value.clone()),
                };
                if !changed
                    && let Some(previous) = &previous
                    && !matches!(source, ResolveSource::Reconfigure(_))
                {
                    return SlotValue::kept(previous.clone());
                }
                let value = facet.combine_any(&values);
                match previous {
                    Some(previous) if facet.compare_any(&value, &previous) => {
                        SlotValue::kept(previous)
                    }
                    _ => SlotValue::changed(value),
                }
            }
        }
    }

    fn deps_changed(&self, deps: &[Dep], tr: &Transaction) -> bool {
        deps.iter().any(|dep| match dep {
            Dep::Doc => tr.doc_changed(),
            // Stored marks are part of what a selection-dependent slot sees
            // (they decide what typing at the cursor produces).
            Dep::Selection => {
                tr.doc_changed() || tr.selection().is_some() || tr.stored_marks().is_some()
            }
            Dep::Field(id) | Dep::Facet(id) => {
                matches!(self.0.config.address_of(*id), Some(Address::Dynamic(at)) if self.ensure(at).changed)
            }
        })
    }
}
