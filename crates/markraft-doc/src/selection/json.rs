//! JSON serialisation of selections.
//!
//! ```json
//! {"type": "text", "anchor": 1, "head": 4, "marks": [{"type": "strong"}]}
//! {"type": "node", "pos": 0}
//! {"type": "all"}
//! ```
//!
//! A custom kind serialises under its own [`SelectionKind::tag`]. Reading one
//! back needs the extension that defines it, so [`Selection::from_json`] only
//! understands the built-in kinds and reports anything else as unknown.

use serde_json::{Map, Value, json};

use crate::error::NodeError;
use crate::mark::{Mark, MarkSet};
use crate::schema::Schema;

use super::Selection;

impl Selection {
    /// Serialise this selection.
    pub fn to_json(&self, schema: &Schema) -> Value {
        match self {
            Selection::Text {
                anchor,
                head,
                marks,
            } => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("text".into()));
                map.insert("anchor".into(), Value::from(*anchor));
                map.insert("head".into(), Value::from(*head));
                if let Some(marks) = marks {
                    map.insert(
                        "marks".into(),
                        Value::Array(marks.iter().map(|m| m.to_json(schema)).collect()),
                    );
                }
                Value::Object(map)
            }
            Selection::Node { pos } => json!({"type": "node", "pos": pos}),
            Selection::All => json!({"type": "all"}),
            Selection::Custom(kind) => {
                let mut value = kind.to_json(schema);
                if let Some(map) = value.as_object_mut() {
                    map.insert("type".into(), Value::String(kind.tag().to_string()));
                }
                value
            }
        }
    }

    /// Read a built-in selection from its JSON representation.
    pub fn from_json(schema: &Schema, value: &Value) -> Result<Selection, NodeError> {
        let object = value
            .as_object()
            .ok_or_else(|| NodeError::Json("a selection must be an object".into()))?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| NodeError::Json("a selection needs a `type`".into()))?;
        let number = |name: &str| -> Result<usize, NodeError> {
            object
                .get(name)
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| NodeError::Json(format!("a selection needs `{name}`")))
        };
        match kind {
            "text" => {
                let anchor = number("anchor")?;
                let head = object
                    .get("head")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(anchor);
                let marks = match object.get("marks") {
                    None => None,
                    Some(Value::Array(items)) => {
                        let mut marks = Vec::with_capacity(items.len());
                        for item in items {
                            marks.push(Mark::from_json(schema, item)?);
                        }
                        Some(MarkSet::from_marks(schema, marks))
                    }
                    Some(_) => return Err(NodeError::Json("`marks` must be an array".into())),
                };
                Ok(Selection::Text {
                    anchor,
                    head,
                    marks,
                })
            }
            "node" => Ok(Selection::Node {
                pos: number("pos")?,
            }),
            "all" => Ok(Selection::All),
            other => Err(NodeError::Json(format!(
                "unknown selection type `{other}`; custom kinds must be read by their extension"
            ))),
        }
    }
}
