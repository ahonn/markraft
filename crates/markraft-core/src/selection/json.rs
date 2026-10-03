//! JSON serialisation of selections.
//!
//! ```json
//! {"type": "text", "anchor": 1, "head": 4}
//! {"type": "node", "pos": 0}
//! {"type": "all"}
//! ```
//!
//! A custom kind serialises under its own
//! [`SelectionKind::tag`](super::SelectionKind::tag). Reading one
//! back needs the extension that defines it, so [`Selection::from_json`] only
//! understands the built-in kinds (including `reading-text`) and reports
//! anything else as unknown.

use serde_json::{Value, json};

use crate::error::NodeError;
use crate::schema::Schema;

use super::Selection;

impl Selection {
    /// Serialise this selection.
    pub fn to_json(&self, schema: &Schema) -> Value {
        match self {
            Selection::Text { anchor, head } => {
                json!({"type": "text", "anchor": anchor, "head": head})
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
    ///
    /// Reading selections resolve their syntax mark by schema name; other
    /// built-in kinds hold only document positions.
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
            "reading-text" => {
                let syntax = object
                    .get("syntax")
                    .and_then(Value::as_str)
                    .and_then(|name| schema.mark_id(name))
                    .ok_or_else(|| {
                        NodeError::Json("reading-text needs a known syntax mark".into())
                    })?;
                Ok(Selection::reading(
                    number("anchor")?,
                    number("head")?,
                    syntax,
                ))
            }
            "text" => {
                let anchor = number("anchor")?;
                let head = object
                    .get("head")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(anchor);
                Ok(Selection::Text { anchor, head })
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
