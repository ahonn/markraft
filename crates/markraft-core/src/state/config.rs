//! Resolving an extension tree into a configuration.
//!
//! Resolution flattens the tree (see [`Extension`]), sorts it by precedence and
//! then by position, and lays the result out as slots:
//!
//! * a **static** slot holds a value that only depends on the configuration —
//!   a constant facet input, or the output of a facet all of whose inputs are
//!   constant. It is computed once, here.
//! * a **dynamic** slot holds a state field, a computed facet input, or the
//!   output of a facet that has at least one computed input. It is computed per
//!   state.
//!
//! Slots are ordered fields first, then facets in the order they are first
//! mentioned, with a facet's computed inputs immediately before it.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use super::EditorState;
use super::extension::{ExtKind, Extension, Prec};
use super::facet::{AnyFacet, AnyValue, FacetProvider, ProviderKind};
use super::field::AnyField;

/// Where a configuration value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Address {
    /// Index into the configuration's static values.
    Static(usize),
    /// Index into the state's dynamic slots.
    Dynamic(usize),
}

pub(crate) enum Slot {
    Field(Arc<dyn AnyField>),
    Provider(Arc<FacetProvider>),
    Facet {
        facet: Arc<dyn AnyFacet>,
        inputs: Vec<Address>,
    },
}

/// A resolved set of extensions.
///
/// A configuration is shared by every state that uses it, so reading a static
/// facet from one costs nothing.
pub struct Configuration {
    base: Extension,
    compartments: BTreeMap<u64, Extension>,
    address: HashMap<u64, Address>,
    static_values: Vec<AnyValue>,
    slots: Vec<Slot>,
}

impl std::fmt::Debug for Configuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Configuration")
            .field("slots", &self.slots.len())
            .field("static_values", &self.static_values.len())
            .field("compartments", &self.compartments.len())
            .finish()
    }
}

impl Configuration {
    /// Resolve `base`, honouring the compartment contents in `requested` and
    /// reusing `old`'s static facet values where they compare equal.
    ///
    /// # Panics
    ///
    /// Panics when the same compartment appears twice in the tree.
    pub(crate) fn resolve(
        base: Extension,
        requested: BTreeMap<u64, Extension>,
        old: Option<&EditorState>,
    ) -> Configuration {
        let mut compartments = BTreeMap::new();
        let flat = flatten(&base, &requested, &mut compartments);

        let mut fields: Vec<Arc<dyn AnyField>> = Vec::new();
        let mut facet_order: Vec<u64> = Vec::new();
        let mut facet_inputs: HashMap<u64, Vec<Arc<FacetProvider>>> = HashMap::new();
        let mut facets: HashMap<u64, Arc<dyn AnyFacet>> = HashMap::new();
        for extension in &flat {
            match &extension.0.kind {
                ExtKind::Field(field) => fields.push(field.clone()),
                ExtKind::Provider(provider) => {
                    let id = provider.facet.id();
                    if !facet_inputs.contains_key(&id) {
                        facet_order.push(id);
                        facets.insert(id, provider.facet.clone());
                    }
                    facet_inputs.entry(id).or_default().push(provider.clone());
                }
                _ => unreachable!("flatten only yields fields and facet inputs"),
            }
        }

        let mut address: HashMap<u64, Address> = HashMap::new();
        let mut static_values: Vec<AnyValue> = Vec::new();
        let mut slots: Vec<Slot> = Vec::new();

        for field in &fields {
            address.insert(field.id(), Address::Dynamic(slots.len()));
            slots.push(Slot::Field(field.clone()));
        }

        for id in &facet_order {
            let facet = facets[id].clone();
            let providers = &facet_inputs[id];
            if providers.iter().all(|p| p.is_static()) {
                let inputs: Vec<AnyValue> = providers
                    .iter()
                    .map(|p| match &p.kind {
                        ProviderKind::Static(value) => value.clone(),
                        ProviderKind::Computed(_) => unreachable!("checked above"),
                    })
                    .collect();
                let mut value = facet.combine_any(&inputs);
                if let Some(old) = old
                    && let Some(previous) = old.raw_facet(*id)
                    && facet.compare_any(&value, &previous)
                {
                    value = previous;
                }
                address.insert(*id, Address::Static(static_values.len()));
                static_values.push(value);
                continue;
            }
            let mut inputs = Vec::with_capacity(providers.len());
            for provider in providers {
                let at = match &provider.kind {
                    ProviderKind::Static(value) => {
                        let at = Address::Static(static_values.len());
                        static_values.push(value.clone());
                        at
                    }
                    ProviderKind::Computed(_) => {
                        let at = Address::Dynamic(slots.len());
                        slots.push(Slot::Provider(provider.clone()));
                        at
                    }
                };
                address.insert(provider.id, at);
                inputs.push(at);
            }
            address.insert(*id, Address::Dynamic(slots.len()));
            slots.push(Slot::Facet { facet, inputs });
        }

        Configuration {
            base,
            compartments,
            address,
            static_values,
            slots,
        }
    }

    /// The root extension tree this configuration was resolved from.
    pub(crate) fn base(&self) -> &Extension {
        &self.base
    }

    /// The compartment contents this configuration resolved with.
    pub(crate) fn compartment_map(&self) -> &BTreeMap<u64, Extension> {
        &self.compartments
    }

    /// The content of one compartment, when it takes part in this
    /// configuration.
    pub fn compartment(&self, id: u64) -> Option<Extension> {
        self.compartments.get(&id).cloned()
    }

    /// The number of per-state slots.
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn slot(&self, index: usize) -> &Slot {
        &self.slots[index]
    }

    pub(crate) fn address_of(&self, id: u64) -> Option<Address> {
        self.address.get(&id).copied()
    }

    pub(crate) fn static_value(&self, index: usize) -> &AnyValue {
        &self.static_values[index]
    }

    /// The output of a facet all of whose inputs are constant.
    ///
    /// Returns `None` when the facet is absent from this configuration or has
    /// at least one computed input, which is why
    /// [`FacetConfig::static_only`](super::FacetConfig::static_only) exists.
    pub fn static_facet<I, O: Send + Sync + 'static>(
        &self,
        facet: &super::Facet<I, O>,
    ) -> Option<&O> {
        match self.address.get(&facet.id()) {
            Some(Address::Static(index)) => {
                Some(super::facet::downcast(&self.static_values[*index]))
            }
            _ => None,
        }
    }
}

/// Flatten the tree into fields and facet inputs, in configuration order.
fn flatten(
    base: &Extension,
    requested: &BTreeMap<u64, Extension>,
    compartments: &mut BTreeMap<u64, Extension>,
) -> Vec<Extension> {
    struct Walk<'a> {
        requested: &'a BTreeMap<u64, Extension>,
        compartments: &'a mut BTreeMap<u64, Extension>,
        seen: HashMap<u64, usize>,
        buckets: Vec<Vec<Extension>>,
    }

    impl Walk<'_> {
        fn visit(&mut self, extension: &Extension, prec: Prec) {
            let rank = prec.rank();
            if let Some(known) = self.seen.get(&extension.0.id).copied() {
                if known <= rank {
                    return;
                }
                // Seen at a lower precedence: take it out and place it again.
                if let Some(at) = self.buckets[known]
                    .iter()
                    .position(|e| e.0.id == extension.0.id)
                {
                    self.buckets[known].remove(at);
                }
                if let ExtKind::Compartment(compartment, _) = &extension.0.kind {
                    self.compartments.remove(&compartment.id());
                }
            }
            self.seen.insert(extension.0.id, rank);
            match &extension.0.kind {
                ExtKind::List(items) => {
                    for item in items {
                        self.visit(item, prec);
                    }
                }
                ExtKind::Prec(inner_prec, inner) => self.visit(inner, *inner_prec),
                ExtKind::Compartment(compartment, inner) => {
                    assert!(
                        !self.compartments.contains_key(&compartment.id()),
                        "a compartment may only be used once in a configuration"
                    );
                    let content = self
                        .requested
                        .get(&compartment.id())
                        .cloned()
                        .unwrap_or_else(|| inner.clone());
                    self.compartments.insert(compartment.id(), content.clone());
                    self.visit(&content, prec);
                }
                ExtKind::Field(field) => {
                    self.buckets[rank].push(extension.clone());
                    let provides: Vec<Extension> = field.provides().to_vec();
                    for provided in provides {
                        self.visit(&provided, prec);
                    }
                }
                ExtKind::Provider(provider) => {
                    self.buckets[rank].push(extension.clone());
                    if let Some(enables) = provider.facet.enables() {
                        let enables = enables.clone();
                        self.visit(&enables, Prec::Default);
                    }
                }
            }
        }
    }

    let mut walk = Walk {
        requested,
        compartments,
        seen: HashMap::new(),
        buckets: vec![Vec::new(); Prec::LEVELS.len()],
    };
    walk.visit(base, Prec::Default);
    walk.buckets.into_iter().flatten().collect()
}
