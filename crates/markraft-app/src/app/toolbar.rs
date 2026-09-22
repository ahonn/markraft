//! The formatting toolbar over the note, and what the footer counts.

use super::doc;
use markraft_core::MarkSet;
use markraft_gpui::TableInfo;

/// Whether the toolbar is up, and what it was last drawn for.
///
/// The snapshot is how a redraw is told from a change: the editor reports every
/// selection move, and redrawing the toolbar for each of them would be a frame
/// spent on a toolbar that looks exactly the same. It is only kept while the
/// toolbar is up, so putting it away forgets it — otherwise the first change
/// after it comes back would be compared against a snapshot from before it went
/// away.
#[derive(Default)]
pub(super) struct Toolbar {
    shown: bool,
    format: Option<(MarkSet, Option<doc::Block>)>,
    /// The table the toolbar was last drawn for. It is paint geometry, so it is
    /// only ever as fresh as the last frame — which is also the frame the
    /// keyboard walks.
    table: Option<TableInfo>,
    /// Whether the footer counts words rather than characters.
    words: bool,
}

impl Toolbar {
    pub(super) fn shown(&self) -> bool {
        self.shown
    }

    pub(super) fn toggle(&mut self) {
        self.shown = !self.shown;
        if !self.shown {
            self.format = None;
        }
    }

    /// Put the toolbar away, and say whether it was up — which is what Escape
    /// needs to know, since it dismisses one thing at a time.
    pub(super) fn dismiss(&mut self) -> bool {
        let was = self.shown;
        self.toggle_off();
        was
    }

    fn toggle_off(&mut self) {
        self.shown = false;
        self.format = None;
    }

    /// The formats under the caret have changed while the toolbar is up, so it
    /// has to be drawn again. `false` for a report that changes nothing it
    /// draws.
    ///
    /// The formats are read through a closure because the editor reports every
    /// selection move, and working out which marks and which block the caret is
    /// in is not free: a toolbar that is not up never pays for it.
    pub(super) fn formats_changed(
        &mut self,
        formats: impl FnOnce() -> (MarkSet, Option<doc::Block>),
    ) -> bool {
        if !self.shown {
            return false;
        }
        let formats = formats();
        if self.format.as_ref() == Some(&formats) {
            return false;
        }
        self.format = Some(formats);
        true
    }

    /// The grid the caret is in, as the last frame drew it.
    pub(super) fn table(&self) -> Option<&TableInfo> {
        self.table.as_ref()
    }

    pub(super) fn set_table(&mut self, table: Option<TableInfo>) {
        self.table = table;
    }

    /// Whether the footer counts words rather than characters.
    pub(super) fn counts_words(&self) -> bool {
        self.words
    }

    pub(super) fn toggle_counting(&mut self) {
        self.words = !self.words;
    }
}
