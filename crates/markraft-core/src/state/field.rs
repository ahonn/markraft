//! State fields: values that live in the state and are updated by a reducer.
//!
//! A field is created once per configuration and then folded forward: every
//! transaction runs `update(previous, &tr)`. The value must be immutable — the
//! old state keeps the old value — so `update` returns a fresh value or the one
//! it was given.
//!
//! A field can feed facets through [`StateFieldConfig::provide`], and can opt
//! into [`EditorState::to_json`](super::EditorState::to_json) by supplying JSON
//! hooks.

use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::error::NodeError;
use crate::schema::Schema;

use super::EditorState;
use super::extension::Extension;
use super::facet::{AnyValue, downcast, next_id};
use super::transaction::Transaction;

type CreateFn<T> = Box<dyn Fn(&EditorState) -> T + Send + Sync>;
type UpdateFn<T> = Box<dyn Fn(&T, &Transaction) -> T + Send + Sync>;
type CompareFn<T> = Box<dyn Fn(&T, &T) -> bool + Send + Sync>;
type ProvideFn<T> = Box<dyn FnOnce(&StateField<T>) -> Extension>;
type ToJsonFn<T> = Box<dyn Fn(&T, &Schema) -> Value + Send + Sync>;
type FromJsonFn<T> = Box<dyn Fn(&Value, &Schema) -> Result<T, NodeError> + Send + Sync>;

/// How to define a [`StateField`].
pub struct StateFieldConfig<T> {
    create: CreateFn<T>,
    update: UpdateFn<T>,
    compare: Option<CompareFn<T>>,
    provide: Vec<ProvideFn<T>>,
    to_json: Option<ToJsonFn<T>>,
    from_json: Option<FromJsonFn<T>>,
}

impl<T> StateFieldConfig<T> {
    /// A configuration with an initial value and a reducer.
    ///
    /// `create` may read facets and fields that the configuration places before
    /// this field; reading a field that depends on this one is a cycle and
    /// panics.
    pub fn new(
        create: impl Fn(&EditorState) -> T + Send + Sync + 'static,
        update: impl Fn(&T, &Transaction) -> T + Send + Sync + 'static,
    ) -> StateFieldConfig<T> {
        StateFieldConfig {
            create: Box::new(create),
            update: Box::new(update),
            compare: None,
            provide: Vec::new(),
            to_json: None,
            from_json: None,
        }
    }

    /// How to tell two values apart, so facets that depend on the field are not
    /// recomputed when it did not really change. Without this, every
    /// transaction counts as a change.
    pub fn compare(mut self, f: impl Fn(&T, &T) -> bool + Send + Sync + 'static) -> Self {
        self.compare = Some(Box::new(f));
        self
    }

    /// Extensions to enable alongside the field.
    ///
    /// The callback receives the finished field, which is what makes
    /// [`Facet::from_field`](super::Facet::from_field) usable here.
    pub fn provide(mut self, f: impl FnOnce(&StateField<T>) -> Extension + 'static) -> Self {
        self.provide.push(Box::new(f));
        self
    }

    /// How to serialise the field's value.
    ///
    /// The schema is passed along because document values — change sets,
    /// selections, nodes — need it to name their types.
    pub fn to_json(mut self, f: impl Fn(&T, &Schema) -> Value + Send + Sync + 'static) -> Self {
        self.to_json = Some(Box::new(f));
        self
    }

    /// How to read the field's value back.
    pub fn from_json(
        mut self,
        f: impl Fn(&Value, &Schema) -> Result<T, NodeError> + Send + Sync + 'static,
    ) -> Self {
        self.from_json = Some(Box::new(f));
        self
    }
}

struct FieldData<T> {
    id: u64,
    create: CreateFn<T>,
    update: UpdateFn<T>,
    compare: Option<CompareFn<T>>,
    provides: OnceLock<Vec<Extension>>,
    to_json: Option<ToJsonFn<T>>,
    from_json: Option<FromJsonFn<T>>,
}

/// A value stored in the editor state and updated by every transaction.
///
/// Cloning is a reference-count bump; every clone denotes the same field.
pub struct StateField<T> {
    data: Arc<FieldData<T>>,
}

impl<T> Clone for StateField<T> {
    fn clone(&self) -> StateField<T> {
        StateField {
            data: self.data.clone(),
        }
    }
}

impl<T> std::fmt::Debug for StateField<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateField")
            .field("id", &self.data.id)
            .finish()
    }
}

impl<T: Send + Sync + 'static> StateField<T> {
    /// Define a field.
    pub fn define(config: StateFieldConfig<T>) -> StateField<T> {
        let StateFieldConfig {
            create,
            update,
            compare,
            provide,
            to_json,
            from_json,
        } = config;
        let field = StateField {
            data: Arc::new(FieldData {
                id: next_id(),
                create,
                update,
                compare,
                provides: OnceLock::new(),
                to_json,
                from_json,
            }),
        };
        let provides: Vec<Extension> = provide.into_iter().map(|f| f(&field)).collect();
        field
            .data
            .provides
            .set(provides)
            .map_err(|_| ())
            .expect("a fresh field has no provides yet");
        field
    }

    /// The field's unique id.
    pub fn id(&self) -> u64 {
        self.data.id
    }

    /// The extension that puts this field in a configuration.
    pub fn extension(&self) -> Extension {
        Extension::field(self.erased())
    }

    pub(crate) fn erased(&self) -> Arc<dyn AnyField> {
        self.data.clone()
    }
}

/// The type-erased half of a field definition.
pub(crate) trait AnyField: Send + Sync {
    fn id(&self) -> u64;
    fn create_any(&self, state: &EditorState) -> AnyValue;
    fn update_any(&self, value: &AnyValue, tr: &Transaction) -> AnyValue;
    fn compare_any(&self, a: &AnyValue, b: &AnyValue) -> bool;
    fn provides(&self) -> &[Extension];
    fn to_json_any(&self, value: &AnyValue, schema: &Schema) -> Option<Value>;
    fn read_json_any(&self, value: &Value, schema: &Schema) -> Option<Result<AnyValue, NodeError>>;
}

impl<T: Send + Sync + 'static> AnyField for FieldData<T> {
    fn id(&self) -> u64 {
        self.id
    }

    fn create_any(&self, state: &EditorState) -> AnyValue {
        Arc::new((self.create)(state))
    }

    fn update_any(&self, value: &AnyValue, tr: &Transaction) -> AnyValue {
        Arc::new((self.update)(downcast::<T>(value), tr))
    }

    fn compare_any(&self, a: &AnyValue, b: &AnyValue) -> bool {
        match &self.compare {
            Some(compare) => compare(downcast::<T>(a), downcast::<T>(b)),
            None => false,
        }
    }

    fn provides(&self) -> &[Extension] {
        self.provides.get().map(Vec::as_slice).unwrap_or(&[])
    }

    fn to_json_any(&self, value: &AnyValue, schema: &Schema) -> Option<Value> {
        self.to_json
            .as_ref()
            .map(|f| f(downcast::<T>(value), schema))
    }

    fn read_json_any(&self, value: &Value, schema: &Schema) -> Option<Result<AnyValue, NodeError>> {
        self.from_json.as_ref().map(|f| {
            f(value, schema).map(|value| {
                let value: AnyValue = Arc::new(value);
                value
            })
        })
    }
}

/// A field paired with the JSON key it serialises under.
///
/// Type erasure is what lets one call serialise fields of different types; see
/// [`EditorState::to_json`](super::EditorState::to_json).
#[derive(Clone, Default)]
pub struct StateJsonFields {
    pub(crate) entries: Vec<(String, Arc<dyn AnyField>)>,
}

impl StateJsonFields {
    /// An empty set.
    pub fn new() -> StateJsonFields {
        StateJsonFields::default()
    }

    /// Serialise `field` under `key`.
    ///
    /// `key` must not be `"doc"` or `"selection"`, which the state uses itself.
    pub fn add<T: Send + Sync + 'static>(mut self, key: &str, field: &StateField<T>) -> Self {
        assert!(
            key != "doc" && key != "selection",
            "`doc` and `selection` are reserved state JSON keys"
        );
        self.entries.push((key.to_string(), field.erased()));
        self
    }

    /// Whether any field is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::fmt::Debug for StateJsonFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateJsonFields")
            .field(
                "keys",
                &self.entries.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .finish()
    }
}
