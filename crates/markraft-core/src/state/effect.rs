//! State effects: typed values that travel with a transaction's changes.
//!
//! An effect that carries document positions declares how to move them, so
//! that combining transaction specs and rebasing keep the effect pointing at
//! the right content. An effect without a `map` function is carried through
//! unchanged.

use std::sync::{Arc, LazyLock};

use crate::change::ChangeDesc;

use super::extension::{Compartment, Extension};
use super::facet::{AnyValue, next_id};

type MapFn<T> = Box<dyn Fn(&T, &ChangeDesc) -> Option<T> + Send + Sync>;

struct EffectTypeData<T> {
    id: u64,
    map: Option<MapFn<T>>,
}

/// The identity, value type and mapping rule of a kind of effect.
///
/// Cloning is a reference-count bump; every clone denotes the same type.
pub struct StateEffectType<T> {
    data: Arc<EffectTypeData<T>>,
}

impl<T> Clone for StateEffectType<T> {
    fn clone(&self) -> StateEffectType<T> {
        StateEffectType {
            data: self.data.clone(),
        }
    }
}

impl<T> std::fmt::Debug for StateEffectType<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateEffectType")
            .field("id", &self.data.id)
            .finish()
    }
}

impl<T: Send + Sync + 'static> StateEffectType<T> {
    /// Define a kind of effect that is carried through changes unchanged.
    pub fn define() -> StateEffectType<T> {
        StateEffectType {
            data: Arc::new(EffectTypeData {
                id: next_id(),
                map: None,
            }),
        }
    }

    /// Define a kind of effect that maps its value through changes.
    ///
    /// Returning `None` from `map` drops the effect, which is how an effect
    /// pointing at deleted content disappears.
    pub fn mapped(
        map: impl Fn(&T, &ChangeDesc) -> Option<T> + Send + Sync + 'static,
    ) -> StateEffectType<T> {
        StateEffectType {
            data: Arc::new(EffectTypeData {
                id: next_id(),
                map: Some(Box::new(map)),
            }),
        }
    }

    /// The type's unique id.
    pub fn id(&self) -> u64 {
        self.data.id
    }

    /// An effect of this type carrying `value`.
    pub fn of(&self, value: T) -> StateEffect {
        StateEffect {
            ty: self.data.clone(),
            value: Arc::new(value),
        }
    }
}

/// A typed value attached to a transaction, alongside its changes.
#[derive(Clone)]
pub struct StateEffect {
    ty: Arc<dyn AnyEffectType>,
    value: AnyValue,
}

impl StateEffect {
    /// Whether this effect has the given type.
    pub fn is<T: Send + Sync + 'static>(&self, ty: &StateEffectType<T>) -> bool {
        self.ty.id() == ty.id()
    }

    /// The value, when this effect has the given type.
    pub fn value<T: Send + Sync + 'static>(&self, ty: &StateEffectType<T>) -> Option<&T> {
        if self.ty.id() == ty.id() {
            self.value.downcast_ref::<T>()
        } else {
            None
        }
    }

    /// The effect's type id.
    pub fn type_id(&self) -> u64 {
        self.ty.id()
    }

    /// Move this effect through `changes`.
    ///
    /// Returns `None` when the mapping dropped it. An effect whose type has no
    /// mapping rule is returned unchanged.
    pub fn map(&self, changes: &ChangeDesc) -> Option<StateEffect> {
        self.ty
            .map_any(&self.value, changes)
            .map(|value| StateEffect {
                ty: self.ty.clone(),
                value,
            })
    }

    /// Move a list of effects through `changes`, dropping the ones the mapping
    /// deletes.
    pub fn map_all(effects: &[StateEffect], changes: &ChangeDesc) -> Vec<StateEffect> {
        effects.iter().filter_map(|e| e.map(changes)).collect()
    }
}

impl std::fmt::Debug for StateEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateEffect")
            .field("type", &self.ty.id())
            .finish()
    }
}

trait AnyEffectType: Send + Sync {
    fn id(&self) -> u64;
    fn map_any(&self, value: &AnyValue, changes: &ChangeDesc) -> Option<AnyValue>;
}

impl<T: Send + Sync + 'static> AnyEffectType for EffectTypeData<T> {
    fn id(&self) -> u64 {
        self.id
    }

    fn map_any(&self, value: &AnyValue, changes: &ChangeDesc) -> Option<AnyValue> {
        let Some(map) = &self.map else {
            return Some(value.clone());
        };
        let typed = value
            .downcast_ref::<T>()
            .expect("an effect always holds the type its definition declares");
        map(typed, changes).map(|mapped| {
            let mapped: AnyValue = Arc::new(mapped);
            mapped
        })
    }
}

static RECONFIGURE: LazyLock<StateEffectType<Extension>> = LazyLock::new(StateEffectType::define);
static APPEND_CONFIG: LazyLock<StateEffectType<Extension>> = LazyLock::new(StateEffectType::define);
static COMPARTMENT: LazyLock<StateEffectType<(Compartment, Extension)>> =
    LazyLock::new(StateEffectType::define);

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
