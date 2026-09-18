//! JSON serialisation of a whole editor state.
//!
//! ```json
//! {"doc": {"type": "doc", "content": []},
//!  "selection": {"type": "text", "anchor": 0, "head": 0},
//!  "history": {"done": [], "undone": []}}
//! ```
//!
//! `doc` and `selection` are always written. Any other key comes from a field
//! that opted in, both by defining JSON hooks
//! ([`StateFieldConfig::to_json`](super::StateFieldConfig::to_json)) and by
//! being listed in the [`StateJsonFields`] passed to the call.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::node::Node;
use crate::selection::Selection;

use super::config::Address;
use super::{EditorState, EditorStateConfig, StateError, StateJsonFields};

impl EditorState {
    /// Serialise the document, the selection and the listed fields.
    pub fn to_json(&self, fields: &StateJsonFields) -> Value {
        let mut map = Map::new();
        map.insert("doc".into(), self.doc().to_json(self.schema()));
        map.insert("selection".into(), self.selection().to_json(self.schema()));
        for (key, field) in &fields.entries {
            let Some(Address::Dynamic(index)) = self.config().address_of(field.id()) else {
                continue;
            };
            if let Some(value) = field.to_json_any(&self.ensure(index).value, self.schema()) {
                map.insert(key.clone(), value);
            }
        }
        Value::Object(map)
    }

    /// Read a state back.
    ///
    /// `config` supplies the schema and the extensions; the document, the
    /// selection and the listed fields come from `value`. A field that is
    /// listed but absent from the JSON, or whose definition has no
    /// `from_json`, starts from its `create` value.
    pub fn from_json(
        value: &Value,
        config: EditorStateConfig,
        fields: &StateJsonFields,
    ) -> Result<EditorState, StateError> {
        let object = value
            .as_object()
            .ok_or_else(|| StateError::Json("a state must be an object".into()))?;
        let doc = object
            .get("doc")
            .ok_or_else(|| StateError::Json("a state needs a `doc`".into()))?;
        let mut config = config;
        config.doc = Some(Node::from_json(&config.schema, doc)?);
        if let Some(selection) = object.get("selection") {
            config.selection = Some(Selection::from_json(&config.schema, selection)?);
        }
        let mut overrides = HashMap::new();
        for (key, field) in &fields.entries {
            let Some(stored) = object.get(key) else {
                continue;
            };
            if let Some(result) = field.read_json_any(stored, &config.schema) {
                overrides.insert(field.id(), result?);
            }
        }
        EditorState::create_with(config, overrides)
    }
}
