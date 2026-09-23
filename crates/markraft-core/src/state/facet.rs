//! Facets: extension points whose inputs are combined into one value.
//!
//! A facet is defined once, with a function that folds the list of inputs the
//! configuration provides into an output value. Inputs come from extensions —
//! either constants ([`Facet::of`]) or values computed from the state
//! ([`Facet::compute`], [`Facet::compute_n`], [`Facet::from_field`]).
//!
//! Input order is the configuration's extension order: explicit precedence
//! first, then position in the flattened extension tree. Facets that take the
//! "first input wins" shape therefore read the highest-precedence value.
//!
//! # Recomputation
//!
//! A computed input declares what it depends on. When a transaction changes
//! none of its dependencies, the previous input value is kept and the facet is
//! not recombined at all. When it is recombined, the facet's `compare` decides
//! whether the new output replaces the old one, so a facet value can be
//! compared by identity to detect change.
//!
//! Wordgard tracks those dependencies automatically by observing what a
//! `compute` function reads. This crate asks for them explicitly instead: doing
//! it by observation needs interior mutability in a value that must stay
//! `Send + Sync`, and the explicit list also documents the dependency at the
//! definition site.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::EditorState;
use super::extension::Extension;
use super::field::StateField;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A process-wide unique id for a facet, field, provider or extension node.
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A type-erased configuration value.
pub(crate) type AnyValue = Arc<dyn Any + Send + Sync>;

/// Something a computed facet input can depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dep {
    /// The document.
    Doc,
    /// The selection, including the state's stored marks.
    Selection,
    /// A state field, by id.
    Field(u64),
    /// A facet, by id.
    Facet(u64),
}

impl Dep {
    /// Depend on a state field's value.
    pub fn field<T: Send + Sync + 'static>(field: &StateField<T>) -> Dep {
        Dep::Field(field.id())
    }

    /// Depend on a facet's output.
    pub fn facet<I, O>(facet: &Facet<I, O>) -> Dep {
        Dep::Facet(facet.id())
    }
}

type CombineFn<I, O> = Box<dyn Fn(&[I]) -> O + Send + Sync>;
type CompareFn<T> = Option<Box<dyn Fn(&T, &T) -> bool + Send + Sync>>;

/// How to define a [`Facet`].
///
/// Only `combine` is required. The builder methods mirror Wordgard's
/// `Facet.Spec` fields; `static` is spelled [`FacetConfig::static_only`]
/// because it is a Rust keyword.
pub struct FacetConfig<I, O> {
    combine: CombineFn<I, O>,
    compare: CompareFn<O>,
    compare_input: CompareFn<I>,
    is_static: bool,
    enables: Option<Extension>,
}

impl<I, O> FacetConfig<I, O> {
    /// A configuration that folds the inputs with `combine`.
    ///
    /// `combine` is called once at definition time with an empty slice to
    /// produce the facet's default value, which a state that does not
    /// configure the facet reads.
    pub fn new(combine: impl Fn(&[I]) -> O + Send + Sync + 'static) -> FacetConfig<I, O> {
        FacetConfig {
            combine: Box::new(combine),
            compare: None,
            compare_input: None,
            is_static: false,
            enables: None,
        }
    }

    /// How to tell two output values apart.
    ///
    /// When a recombined value compares equal to the old one, the old one is
    /// kept, so dependents do not see a change. Without this, every
    /// recombination counts as a change.
    pub fn compare(mut self, f: impl Fn(&O, &O) -> bool + Send + Sync + 'static) -> Self {
        self.compare = Some(Box::new(f));
        self
    }

    /// How to tell two input values apart, to skip recombining after a
    /// computed input was re-evaluated to the same value.
    pub fn compare_input(mut self, f: impl Fn(&I, &I) -> bool + Send + Sync + 'static) -> Self {
        self.compare_input = Some(Box::new(f));
        self
    }

    /// Forbid computed inputs, so the value can be read from a
    /// [`Configuration`](super::Configuration) alone.
    pub fn static_only(mut self) -> Self {
        self.is_static = true;
        self
    }

    /// Extensions to enable in any state that provides an input to this facet.
    ///
    /// They are *not* enabled in a state that merely reads the facet's default.
    pub fn enables(mut self, extension: Extension) -> Self {
        self.enables = Some(extension);
        self
    }
}

struct FacetData<I, O> {
    id: u64,
    combine: CombineFn<I, O>,
    compare: CompareFn<O>,
    compare_input: CompareFn<I>,
    is_static: bool,
    enables: Option<Extension>,
    default: AnyValue,
}

/// An extension point with many inputs and one combined output.
///
/// Cloning is a reference-count bump; every clone denotes the same facet.
pub struct Facet<I, O = Vec<I>> {
    data: Arc<FacetData<I, O>>,
}

impl<I, O> Clone for Facet<I, O> {
    fn clone(&self) -> Facet<I, O> {
        Facet {
            data: self.data.clone(),
        }
    }
}

impl<I, O> std::fmt::Debug for Facet<I, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Facet").field("id", &self.data.id).finish()
    }
}

impl<I, O> Facet<I, O> {
    /// The facet's unique id.
    pub fn id(&self) -> u64 {
        self.data.id
    }

    /// Whether the facet refuses computed inputs.
    pub fn is_static(&self) -> bool {
        self.data.is_static
    }
}

impl<I, O: Send + Sync + 'static> Facet<I, O> {
    /// The value a state that configures no input reads.
    pub fn default_value(&self) -> &O {
        downcast(&self.data.default)
    }
}

impl<I: Clone + Send + Sync + 'static, O: Send + Sync + 'static> Facet<I, O> {
    /// Define a facet.
    pub fn define(config: FacetConfig<I, O>) -> Facet<I, O> {
        let default: O = (config.combine)(&[]);
        Facet {
            data: Arc::new(FacetData {
                id: next_id(),
                combine: config.combine,
                compare: config.compare,
                compare_input: config.compare_input,
                is_static: config.is_static,
                enables: config.enables,
                default: Arc::new(default),
            }),
        }
    }

    /// An extension providing a constant input.
    pub fn of(&self, value: I) -> Extension {
        self.provider(Vec::new(), ProviderKind::Static(Arc::new(vec![value])))
    }

    /// An extension providing one input computed from the state.
    ///
    /// # Panics
    ///
    /// Panics when the facet was defined with
    /// [`FacetConfig::static_only`].
    pub fn compute(
        &self,
        deps: impl IntoIterator<Item = Dep>,
        get: impl Fn(&EditorState) -> I + Send + Sync + 'static,
    ) -> Extension {
        self.assert_dynamic();
        self.provider(
            deps.into_iter().collect(),
            ProviderKind::Computed(Arc::new(move |state| {
                let value: AnyValue = Arc::new(vec![get(state)]);
                value
            })),
        )
    }

    /// An extension providing any number of inputs computed from the state.
    ///
    /// # Panics
    ///
    /// Panics when the facet was defined with
    /// [`FacetConfig::static_only`].
    pub fn compute_n(
        &self,
        deps: impl IntoIterator<Item = Dep>,
        get: impl Fn(&EditorState) -> Vec<I> + Send + Sync + 'static,
    ) -> Extension {
        self.assert_dynamic();
        self.provider(
            deps.into_iter().collect(),
            ProviderKind::Computed(Arc::new(move |state| {
                let value: AnyValue = Arc::new(get(state));
                value
            })),
        )
    }

    /// An extension deriving one input from a state field's value.
    ///
    /// # Panics
    ///
    /// Panics when the facet was defined with
    /// [`FacetConfig::static_only`].
    pub fn from_field<T: Send + Sync + 'static>(
        &self,
        field: &StateField<T>,
        get: impl Fn(&T) -> I + Send + Sync + 'static,
    ) -> Extension {
        let field = field.clone();
        self.compute([Dep::field(&field)], move |state| {
            get(state
                .field(&field)
                .expect("a field that provides a facet input is part of the configuration"))
        })
    }

    fn assert_dynamic(&self) {
        assert!(
            !self.data.is_static,
            "a static facet does not accept computed inputs"
        );
    }

    fn provider(&self, deps: Vec<Dep>, kind: ProviderKind) -> Extension {
        Extension::provider(Arc::new(FacetProvider {
            id: next_id(),
            facet: self.erased(),
            deps,
            kind,
        }))
    }

    pub(crate) fn erased(&self) -> Arc<dyn AnyFacet> {
        self.data.clone()
    }
}

impl<I: Clone + Send + Sync + 'static> Facet<I, Vec<I>> {
    /// A facet whose output is simply the list of its inputs, in configuration
    /// order.
    pub fn list() -> Facet<I, Vec<I>> {
        Facet::define(FacetConfig::new(<[I]>::to_vec))
    }
}

pub(crate) fn downcast<T: 'static>(value: &AnyValue) -> &T {
    value
        .downcast_ref::<T>()
        .expect("a configuration slot always holds the type its definition declares")
}

/// The type-erased half of a facet definition.
pub(crate) trait AnyFacet: Send + Sync {
    fn id(&self) -> u64;
    /// Combine provider outputs (each an `Arc<Vec<I>>`) into an `Arc<O>`.
    fn combine_any(&self, inputs: &[AnyValue]) -> AnyValue;
    fn compare_any(&self, a: &AnyValue, b: &AnyValue) -> bool;
    fn compare_inputs_any(&self, a: &AnyValue, b: &AnyValue) -> bool;
    fn enables(&self) -> Option<&Extension>;
}

impl<I: Clone + Send + Sync + 'static, O: Send + Sync + 'static> AnyFacet for FacetData<I, O> {
    fn id(&self) -> u64 {
        self.id
    }

    fn combine_any(&self, inputs: &[AnyValue]) -> AnyValue {
        let mut all: Vec<I> = Vec::new();
        for input in inputs {
            all.extend(downcast::<Vec<I>>(input).iter().cloned());
        }
        Arc::new((self.combine)(&all))
    }

    fn compare_any(&self, a: &AnyValue, b: &AnyValue) -> bool {
        match &self.compare {
            Some(compare) => compare(downcast::<O>(a), downcast::<O>(b)),
            None => false,
        }
    }

    fn compare_inputs_any(&self, a: &AnyValue, b: &AnyValue) -> bool {
        let Some(compare) = &self.compare_input else {
            return false;
        };
        let (a, b) = (downcast::<Vec<I>>(a), downcast::<Vec<I>>(b));
        a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| compare(x, y))
    }

    fn enables(&self) -> Option<&Extension> {
        self.enables.as_ref()
    }
}

/// One input to a facet, as the configuration stores it.
pub(crate) struct FacetProvider {
    pub(crate) id: u64,
    pub(crate) facet: Arc<dyn AnyFacet>,
    pub(crate) deps: Vec<Dep>,
    pub(crate) kind: ProviderKind,
}

pub(crate) enum ProviderKind {
    /// A constant `Arc<Vec<I>>`.
    Static(AnyValue),
    /// A function producing `Arc<Vec<I>>` from the state.
    Computed(Arc<dyn Fn(&EditorState) -> AnyValue + Send + Sync>),
}

impl FacetProvider {
    pub(crate) fn is_static(&self) -> bool {
        matches!(self.kind, ProviderKind::Static(_))
    }
}
