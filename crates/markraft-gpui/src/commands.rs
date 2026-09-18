//! The editor's own key-binding behaviour, as commands a host or an extension
//! can run.
//!
//! Each of these is exactly what the matching key does, so a modal extension
//! that wants "what Enter would produce here" asks for it rather than
//! reimplementing it. Every one takes the schema it should read its node types
//! from, which is [`EditorView::schema`](crate::EditorView::schema).

use crate::keymap;
use crate::types::DocTypes;
use markraft_doc::commands::{Command, Direction};
use markraft_doc::{Attrs, NodeTypeId, Schema};

/// Enter: split a list item, leave an empty one, break a code line, or split
/// the block.
pub fn enter(schema: &Schema) -> Command {
    keymap::enter(&DocTypes::of(schema))
}

/// Backspace: take back an input rule, delete the selection or one grapheme,
/// outdent a list item at its start, then join or select backwards.
pub fn backspace(schema: &Schema) -> Command {
    keymap::backspace(&DocTypes::of(schema))
}

/// Forward delete.
pub fn delete_forward() -> Command {
    keymap::delete_forward()
}

/// Delete one word in `dir`, or the selection.
pub fn delete_word(dir: Direction) -> Command {
    keymap::delete_word(dir)
}

/// Tab: sink a list item, or indent inside a code block.
pub fn indent(schema: &Schema) -> Command {
    keymap::indent(&DocTypes::of(schema))
}

/// Shift-Tab: lift a list item, or lift a block out of its wrapper.
pub fn outdent(schema: &Schema) -> Command {
    keymap::outdent(&DocTypes::of(schema))
}

/// Set a textblock type, or return to a paragraph when it is already that type.
pub fn toggle_block(schema: &Schema, ty: NodeTypeId, attrs: Attrs) -> Command {
    keymap::toggle_block(&DocTypes::of(schema), ty, attrs)
}

/// Wrap in a block quote, or lift out of the one the cursor sits in.
pub fn toggle_quote(schema: &Schema) -> Command {
    keymap::toggle_quote(&DocTypes::of(schema))
}

/// Wrap in a list of `ty` holding items of `item`, or leave it.
pub fn toggle_list(schema: &Schema, ty: NodeTypeId, item: NodeTypeId) -> Command {
    keymap::toggle_list(&DocTypes::of(schema), ty, item)
}

/// ⌘⏎: tick or clear a task box, else leave a code block.
pub fn toggle_task(schema: &Schema) -> Command {
    keymap::toggle_task(&DocTypes::of(schema))
}

/// Insert literal text, turning its line endings into block breaks except in a
/// code block.
pub fn insert_plain(schema: &Schema, text: &str) -> Command {
    keymap::insert_plain(&DocTypes::of(schema), text)
}

/// ⌘A: the code block the cursor sits in first, then the whole document.
pub fn select_all(schema: &Schema) -> Command {
    keymap::select_all(&DocTypes::of(schema))
}
