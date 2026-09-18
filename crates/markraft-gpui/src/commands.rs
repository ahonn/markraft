//! The editor's own key-binding behaviour, as commands a host or an extension
//! can run.
//!
//! Each of these is exactly what the matching key does, so a modal extension
//! that wants "what Enter would produce here" asks for it rather than
//! reimplementing it. Every one takes the [`DocTypes`] it should read its node
//! types from, which is [`EditorCx::types`](crate::EditorCx::types) for an
//! extension and the host's own table otherwise.

use crate::keymap;
use crate::types::DocTypes;
use markraft_core::commands::{Command, Direction};
use markraft_core::{Attrs, NodeTypeId};

/// Enter: split a list item, leave an empty one, break a code line, or split
/// the block.
pub fn enter(types: &DocTypes) -> Command {
    keymap::enter(types)
}

/// Backspace: take back an input rule, delete the selection or one grapheme,
/// outdent a list item at its start, then join or select backwards.
pub fn backspace(types: &DocTypes) -> Command {
    keymap::backspace(types)
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
pub fn indent(types: &DocTypes) -> Command {
    keymap::indent(types)
}

/// Shift-Tab: lift a list item, or lift a block out of its wrapper.
pub fn outdent(types: &DocTypes) -> Command {
    keymap::outdent(types)
}

/// Set a textblock type, or return to a paragraph when it is already that type.
pub fn toggle_block(types: &DocTypes, ty: NodeTypeId, attrs: Attrs) -> Command {
    keymap::toggle_block(types, ty, attrs)
}

/// Wrap in a block quote, or lift out of the one the cursor sits in.
pub fn toggle_quote(types: &DocTypes) -> Command {
    keymap::toggle_quote(types)
}

/// Wrap in a list of `ty` holding items of `item`, or leave it.
pub fn toggle_list(types: &DocTypes, ty: NodeTypeId, item: NodeTypeId) -> Command {
    keymap::toggle_list(types, ty, item)
}

/// ⌘⏎: tick or clear a task box, else leave a code block.
pub fn toggle_task(types: &DocTypes) -> Command {
    keymap::toggle_task(types)
}

/// Insert literal text, turning its line endings into block breaks except in a
/// code block.
pub fn insert_plain(types: &DocTypes, text: &str) -> Command {
    keymap::insert_plain(types, text)
}

/// ⌘A: the code block the cursor sits in first, then the whole document.
pub fn select_all(types: &DocTypes) -> Command {
    keymap::select_all(types)
}
