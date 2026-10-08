//! The lists the window scrolls, and the two rules each of them keeps.
//!
//! A selected row and the scroll that shows it are one fact, not two: a row
//! selected off-screen that the list never scrolled to is a row the user cannot
//! see they selected. Every list here moves both together, so a new caller
//! cannot move one and forget the other.

use gpui::ScrollHandle;

/// One list's selected row and the scroll that follows it.
#[derive(Default)]
pub(super) struct Cursor {
    row: usize,
    scroll: ScrollHandle,
    /// Whether the list still owes the keyboard its focus: a menu that opened
    /// this frame has not been drawn yet, so there is nothing to focus until it
    /// has been.
    focus_pending: bool,
}

impl Cursor {
    pub(super) fn row(&self) -> usize {
        self.row
    }

    pub(super) fn scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Select `row` and bring it into view.
    pub(super) fn select(&mut self, row: usize) {
        self.row = row;
        self.scroll.scroll_to_item(row);
    }

    /// Select `row` without scrolling: the pointer is already on it, and a
    /// list that moved under the pointer would take the row with it.
    pub(super) fn point_at(&mut self, row: usize) {
        self.row = row;
    }

    pub(super) fn up(&mut self) {
        self.select(self.row.saturating_sub(1));
    }

    /// Down one, stopping at the last of `len` rows. Lists here do not wrap.
    pub(super) fn down(&mut self, len: usize) {
        self.select((self.row + 1).min(len.saturating_sub(1)));
    }

    /// Open the list at `row`, with the keyboard still to be handed over.
    pub(super) fn open_at(&mut self, row: usize) {
        self.select(row);
        self.focus_pending = true;
    }

    pub(super) fn reopen(&mut self) {
        self.select(0);
    }

    pub(super) fn focus_pending(&self) -> bool {
        self.focus_pending
    }

    /// Whether the keyboard is still owed, and no longer is.
    pub(super) fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus_pending)
    }

    /// The list is closed; it owes the keyboard nothing.
    pub(super) fn released(&mut self) {
        self.focus_pending = false;
    }
}

/// Browse and the command list, which share a selected row — only one of them
/// is ever open — and scroll separately, since each keeps its own place.
#[derive(Default)]
pub(super) struct Picker {
    row: usize,
    browse_scroll: ScrollHandle,
    actions_scroll: ScrollHandle,
}

impl Picker {
    pub(super) fn row(&self) -> usize {
        self.row
    }

    pub(super) fn browse_scroll(&self) -> &ScrollHandle {
        &self.browse_scroll
    }

    pub(super) fn actions_scroll(&self) -> &ScrollHandle {
        &self.actions_scroll
    }

    /// Select `row` in Browse, keeping it in view.
    pub(super) fn select_in_browse(&mut self, row: usize) {
        self.row = row;
        self.browse_scroll.scroll_to_item(row);
    }

    /// Select `row` in the command list, keeping it in view.
    pub(super) fn select_in_actions(&mut self, row: usize) {
        self.row = row;
        self.actions_scroll.scroll_to_item(row);
    }

    /// Select `row` without scrolling: the caller is following the list rather
    /// than moving through it — the pointer is already on the row it names.
    pub(super) fn point_at(&mut self, row: usize) {
        self.row = row;
    }

    /// Keep the selection inside a list that just became `len` rows long.
    pub(super) fn clamp_to(&mut self, len: usize) {
        self.row = self.row.min(len.saturating_sub(1));
        self.browse_scroll.scroll_to_item(self.row);
    }

    pub(super) fn reopen_browse(&mut self) {
        self.select_in_browse(0);
    }

    pub(super) fn reopen_actions(&mut self) {
        self.select_in_actions(0);
    }
}
