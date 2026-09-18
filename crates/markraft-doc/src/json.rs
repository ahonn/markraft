//! JSON serialisation of nodes and slices against a schema.
//!
//! The format mirrors the document model directly:
//!
//! ```json
//! {"type": "paragraph",
//!  "attrs": {"align": "center"},
//!  "content": [{"type": "text", "text": "hi", "marks": [{"type": "strong"}]}]}
//! ```
//!
//! Absent `attrs`, `marks` and `content` keys mean "empty".

use serde_json::{Map, Value, json};

use crate::attr::{AttrValue, Attrs};
use crate::error::NodeError;
use crate::fragment::Fragment;
use crate::mark::{Mark, MarkSet};
use crate::node::Node;
use crate::schema::Schema;
use crate::slice::Slice;

fn attrs_to_json(attrs: &Attrs) -> Value {
    let mut map = Map::new();
    for (name, value) in attrs.iter() {
        map.insert(name.to_string(), attr_value_to_json(value));
    }
    Value::Object(map)
}

fn attr_value_to_json(value: &AttrValue) -> Value {
    match value {
        AttrValue::Null => Value::Null,
        AttrValue::Bool(b) => Value::Bool(*b),
        AttrValue::Int(i) => Value::from(*i),
        AttrValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        AttrValue::Str(s) => Value::String(s.clone()),
    }
}

fn attr_value_from_json(value: &Value) -> Result<AttrValue, NodeError> {
    Ok(match value {
        Value::Null => AttrValue::Null,
        Value::Bool(b) => AttrValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                AttrValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                AttrValue::Float(f)
            } else {
                return Err(NodeError::Json(format!("unsupported number {n}")));
            }
        }
        Value::String(s) => AttrValue::Str(s.clone()),
        other => {
            return Err(NodeError::Json(format!(
                "attribute values must be scalars, got {other}"
            )));
        }
    })
}

fn attrs_from_json(value: Option<&Value>) -> Result<Attrs, NodeError> {
    let Some(value) = value else {
        return Ok(Attrs::empty());
    };
    let object = value
        .as_object()
        .ok_or_else(|| NodeError::Json("`attrs` must be an object".into()))?;
    let mut pairs = Vec::with_capacity(object.len());
    for (name, value) in object {
        pairs.push((name.clone(), attr_value_from_json(value)?));
    }
    Ok(Attrs::from_pairs(pairs))
}

impl Mark {
    /// Serialise this mark.
    pub fn to_json(&self, schema: &Schema) -> Value {
        let mut map = Map::new();
        map.insert(
            "type".into(),
            Value::String(schema.mark_type(self.ty).name().to_string()),
        );
        if !self.attrs.is_empty() {
            map.insert("attrs".into(), attrs_to_json(&self.attrs));
        }
        Value::Object(map)
    }

    /// Read a mark from its JSON representation.
    pub fn from_json(schema: &Schema, value: &Value) -> Result<Mark, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a mark must be an object".into()))?;
        let name = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| NodeError::Json("a mark needs a `type`".into()))?;
        let ty = schema
            .mark_id(name)
            .ok_or_else(|| NodeError::Json(format!("unknown mark type `{name}`")))?;
        let attrs = schema.build_mark_attrs(ty, &attrs_from_json(object.get("attrs"))?)?;
        Ok(Mark { ty, attrs })
    }
}

impl Node {
    /// Serialise this node and its content.
    pub fn to_json(&self, schema: &Schema) -> Value {
        let mut map = Map::new();
        map.insert(
            "type".into(),
            Value::String(schema.node_type(self.type_id()).name().to_string()),
        );
        if let Some(text) = self.text() {
            map.insert("text".into(), Value::String(text.to_string()));
        }
        if !self.attrs().is_empty() {
            map.insert("attrs".into(), attrs_to_json(self.attrs()));
        }
        if !self.marks().is_empty() {
            map.insert(
                "marks".into(),
                Value::Array(self.marks().iter().map(|m| m.to_json(schema)).collect()),
            );
        }
        if self.is_container() && self.child_count() > 0 {
            map.insert(
                "content".into(),
                Value::Array(self.children().map(|c| c.to_json(schema)).collect()),
            );
        }
        Value::Object(map)
    }

    /// Read a node from its JSON representation, validating types, attributes
    /// and marks against `schema`.
    ///
    /// Content rules are *not* checked; call [`Node::check`] for that.
    pub fn from_json(schema: &Schema, value: &Value) -> Result<Node, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a node must be an object".into()))?;
        let name = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| NodeError::Json("a node needs a `type`".into()))?;
        let ty = schema
            .node_id(name)
            .ok_or_else(|| NodeError::Json(format!("unknown node type `{name}`")))?;
        let attrs = attrs_from_json(object.get("attrs"))?;
        let marks = match object.get("marks") {
            None => MarkSet::empty(),
            Some(Value::Array(items)) => {
                let mut marks = Vec::with_capacity(items.len());
                for item in items {
                    marks.push(Mark::from_json(schema, item)?);
                }
                MarkSet::from_marks(schema, marks)
            }
            Some(_) => return Err(NodeError::Json("`marks` must be an array".into())),
        };
        if schema.node_type(ty).is_text() {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| NodeError::Json("a text node needs `text`".into()))?;
            if text.is_empty() {
                return Err(NodeError::InvalidText(
                    "text nodes must not be empty".into(),
                ));
            }
            let attrs = schema.build_node_attrs(ty, &attrs)?;
            return Ok(Node::text_leaf(
                crate::node::Markup { ty, attrs, marks },
                text,
            ));
        }
        let content = match object.get("content") {
            None => Fragment::empty(),
            Some(Value::Array(items)) => {
                let mut nodes = Vec::with_capacity(items.len());
                for item in items {
                    nodes.push(Node::from_json(schema, item)?);
                }
                Fragment::from_nodes(nodes)
            }
            Some(_) => return Err(NodeError::Json("`content` must be an array".into())),
        };
        schema.create(ty, attrs, marks, content)
    }
}

impl Slice {
    /// Serialise this slice.
    pub fn to_json(&self, schema: &Schema) -> Value {
        json!({
            "content": Value::Array(self.content().iter().map(|n| n.to_json(schema)).collect()),
            "openStart": self.open_start(),
            "openEnd": self.open_end(),
        })
    }

    /// Read a slice from its JSON representation.
    pub fn from_json(schema: &Schema, value: &Value) -> Result<Slice, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a slice must be an object".into()))?;
        let content = match object.get("content") {
            None => Fragment::empty(),
            Some(Value::Array(items)) => {
                let mut nodes = Vec::with_capacity(items.len());
                for item in items {
                    nodes.push(Node::from_json(schema, item)?);
                }
                Fragment::from_nodes(nodes)
            }
            Some(_) => return Err(NodeError::Json("`content` must be an array".into())),
        };
        let open_start = object.get("openStart").and_then(Value::as_u64).unwrap_or(0) as usize;
        let open_end = object.get("openEnd").and_then(Value::as_u64).unwrap_or(0) as usize;
        Ok(Slice::new(content, open_start, open_end))
    }
}
