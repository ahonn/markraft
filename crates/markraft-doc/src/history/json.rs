//! Serialising the history field.
//!
//! ```json
//! {"done": [{"changes": {"length": 4, "sections": []}, "selection": {"type": "text", "anchor": 0, "head": 0}}],
//!  "undone": []}
//! ```
//!
//! Only the inverted changes and the selection before each event survive. A
//! rebase an event still owes to the events below it, the selections made after
//! it, and inverted effects are all dropped: they only mean something relative
//! to a live document that a reloaded state no longer has.

use serde_json::{Map, Value};

use crate::change::ChangeSet;
use crate::error::NodeError;
use crate::schema::Schema;
use crate::selection::Selection;

use super::state::{HistEvent, HistoryState};

fn events_to_json(events: &[HistEvent], schema: &Schema) -> Value {
    Value::Array(
        events
            .iter()
            .filter_map(|event| {
                let changes = event.changes.as_ref()?;
                let mut map = Map::new();
                map.insert("changes".into(), changes.to_json());
                if let Some(selection) = &event.start_selection {
                    map.insert("selection".into(), selection.to_json(schema));
                }
                Some(Value::Object(map))
            })
            .collect(),
    )
}

fn events_from_json(value: Option<&Value>, schema: &Schema) -> Result<Vec<HistEvent>, NodeError> {
    let Some(Value::Array(items)) = value else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| NodeError::Json("a history event must be an object".into()))?;
        let changes = object
            .get("changes")
            .ok_or_else(|| NodeError::Json("a history event needs `changes`".into()))?;
        let changes = ChangeSet::from_json(schema, changes)?;
        let selection = match object.get("selection") {
            Some(value) => Some(Selection::from_json(schema, value)?),
            None => None,
        };
        out.push(HistEvent {
            changes: Some(changes),
            effects: Vec::new(),
            mapped: None,
            start_selection: selection,
            selections_after: Vec::new(),
        });
    }
    Ok(out)
}

pub(crate) fn history_to_json(state: &HistoryState, schema: &Schema) -> Value {
    let mut map = Map::new();
    map.insert("done".into(), events_to_json(&state.done, schema));
    map.insert("undone".into(), events_to_json(&state.undone, schema));
    Value::Object(map)
}

pub(crate) fn history_from_json(value: &Value, schema: &Schema) -> Result<HistoryState, NodeError> {
    let object = value
        .as_object()
        .ok_or_else(|| NodeError::Json("a history must be an object".into()))?;
    Ok(HistoryState {
        done: events_from_json(object.get("done"), schema)?,
        undone: events_from_json(object.get("undone"), schema)?,
        prev_time: None,
        prev_user_event: None,
        group_depth: 0,
        group_started: false,
        composing: false,
    })
}
