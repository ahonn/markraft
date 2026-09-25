//! Where Tab has walked the window's chrome, and who holds the keyboard.
//!
//! The chrome's controls — the corner buttons, a row's actions, the toolbar's
//! pills — have no focus handles of their own: the panel holds one for all of
//! them and the ring is drawn around whichever control this says it rests on.
//! That makes "nothing is ringed" and "the keyboard is back with the note or
//! the query field" the same fact, and the whole window has to agree on it.
//!
//! Every path that hands the keyboard back therefore says so once, through
//! [`FocusRing::release`], rather than clearing a field — there are more than a
//! dozen such paths, and one that forgot would leave a ring drawn around a
//! control the keys no longer reach.

use gpui::{FocusHandle, SharedString};

pub(super) struct FocusRing {
    panel: FocusHandle,
    at: Option<SharedString>,
}

impl FocusRing {
    pub(super) fn new(panel: FocusHandle) -> FocusRing {
        FocusRing { panel, at: None }
    }

    /// The handle every chrome control shares.
    pub(super) fn panel(&self) -> &FocusHandle {
        &self.panel
    }

    /// Which control the ring rests on, if any.
    pub(super) fn at(&self) -> Option<&SharedString> {
        self.at.as_ref()
    }

    pub(super) fn move_to(&mut self, id: SharedString) {
        self.at = Some(id);
    }

    /// The keyboard goes back to the note or the query field, so nothing wears
    /// the ring.
    pub(super) fn release(&mut self) {
        self.at = None;
    }
}
