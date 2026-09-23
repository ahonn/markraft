//! JSON serialisation of change sets.
//!
//! A change set becomes its starting length plus its sections:
//!
//! ```json
//! {"length": 8,
//!  "sections": [{"len": 3},
//!               {"len": 2, "insert": {"content": [], "openStart": 0, "openEnd": 0}},
//!               {"len": 3, "marks": [{"add": {"type": "strong"}}]}]}
//! ```
//!
//! A section with neither `insert` nor `marks` is preserved as-is. This is what
//! an undo history or a collaboration channel persists; the document itself is
//! serialised separately.

use serde_json::{Map, Value};

use crate::error::NodeError;
use crate::mark::Mark;
use crate::schema::Schema;
use crate::slice::Slice;

use super::{ChangeSet, MarkChange, SectionBuilder, SectionOp};

impl MarkChange {
    fn to_json(&self, schema: &Schema) -> Value {
        let mut map = Map::new();
        match self {
            MarkChange::Add(mark) => {
                map.insert("add".into(), mark.to_json(schema));
            }
            MarkChange::Remove(mark) => {
                map.insert("remove".into(), mark.to_json(schema));
            }
            MarkChange::RemoveType(ty) => {
                map.insert(
                    "removeType".into(),
                    Value::String(schema.mark_type(*ty).name().to_string()),
                );
            }
        }
        Value::Object(map)
    }

    fn from_json(schema: &Schema, value: &Value) -> Result<MarkChange, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a mark change must be an object".into()))?;
        if let Some(mark) = object.get("add") {
            return Ok(MarkChange::Add(Mark::from_json(schema, mark)?));
        }
        if let Some(mark) = object.get("remove") {
            return Ok(MarkChange::Remove(Mark::from_json(schema, mark)?));
        }
        if let Some(name) = object.get("removeType").and_then(Value::as_str) {
            let ty = schema
                .mark_id(name)
                .ok_or_else(|| NodeError::Json(format!("unknown mark type `{name}`")))?;
            return Ok(MarkChange::RemoveType(ty));
        }
        Err(NodeError::Json(
            "a mark change needs `add`, `remove` or `removeType`".into(),
        ))
    }
}

impl ChangeSet {
    /// Serialise this change set.
    pub fn to_json(&self) -> Value {
        let sections: Vec<Value> = self
            .sections
            .iter()
            .map(|section| {
                let mut map = Map::new();
                map.insert("len".into(), Value::from(section.len));
                match &section.op {
                    SectionOp::Keep => {}
                    SectionOp::Mark(mods) => {
                        map.insert(
                            "marks".into(),
                            Value::Array(mods.iter().map(|m| m.to_json(&self.schema)).collect()),
                        );
                    }
                    SectionOp::Replace(tokens) => {
                        map.insert(
                            "insert".into(),
                            Slice::from_tokens(tokens).to_json(&self.schema),
                        );
                    }
                }
                Value::Object(map)
            })
            .collect();
        let mut map = Map::new();
        map.insert("length".into(), Value::from(self.length_before()));
        map.insert("sections".into(), Value::Array(sections));
        Value::Object(map)
    }

    /// Read a change set from its JSON representation.
    ///
    /// Fails when the sections do not add up to the recorded starting length,
    /// so a truncated or tampered payload cannot produce a set that would
    /// misbehave on [`ChangeSet::apply`].
    pub fn from_json(schema: &Schema, value: &Value) -> Result<ChangeSet, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a change set must be an object".into()))?;
        let length = usize::try_from(
            object
                .get("length")
                .and_then(Value::as_u64)
                .ok_or_else(|| NodeError::Json("a change set needs a `length`".into()))?,
        )
        .map_err(|_| NodeError::Json("change set length is out of range".into()))?;
        let sections = object
            .get("sections")
            .and_then(Value::as_array)
            .ok_or_else(|| NodeError::Json("a change set needs `sections`".into()))?;
        let mut builder = SectionBuilder::new();
        let mut total = 0usize;
        for section in sections {
            let section = section
                .as_object()
                .ok_or_else(|| NodeError::Json("a section must be an object".into()))?;
            let len = usize::try_from(
                section
                    .get("len")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| NodeError::Json("a section needs a `len`".into()))?,
            )
            .map_err(|_| NodeError::Json("section length is out of range".into()))?;
            total = total
                .checked_add(len)
                .filter(|total| *total <= length)
                .ok_or_else(|| {
                    NodeError::Json(format!(
                        "sections cover more than the declared length {length}"
                    ))
                })?;
            if let Some(insert) = section.get("insert") {
                builder.replace(len, Slice::from_json(schema, insert)?.tokens());
            } else if let Some(Value::Array(mods)) = section.get("marks") {
                let mut changes = Vec::with_capacity(mods.len());
                for item in mods {
                    changes.push(MarkChange::from_json(schema, item)?);
                }
                builder.mark(len, changes);
            } else {
                builder.keep(len);
            }
        }
        if total != length {
            return Err(NodeError::Json(format!(
                "sections cover {total} tokens but the change set declares {length}"
            )));
        }
        Ok(builder.finish(schema, length))
    }
}
