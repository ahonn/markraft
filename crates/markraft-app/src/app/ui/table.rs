use super::*;

/// The pill's height; its radius is the one that height gives it, as the link pill's is.
const HEIGHT: Pixels = px(32.);
/// The pill's width, which it has to be anchored by before it has been laid out: its
/// ten 28 px controls, the 1 px gap between each pair of neighbours, two dividers with
/// their margins, and 5 px of padding and border at each end.
const WIDTH: Pixels = px(10. * 28. + 11. * 1. + 2. * 7. + 2. * 5.);
/// How far the pill floats clear of the grid it belongs to, and of the window's chrome.
const GAP: Pixels = px(6.);
/// Every toolbar control's id begins with this, so the ring can tell a stop of its own
/// from a row of the panel it shares its commands with.
const STOP: &str = "table-bar-";

/// The toolbar's controls, in the order it draws them and the ring walks them: adding
/// to the grid, aligning its column, then taking from it.
const CONTROLS: [(&str, &str, TableEdit); 10] = [
    (
        "table-bar-row-add-before",
        "Add Row Above",
        TableEdit::RowBefore,
    ),
    (
        "table-bar-row-add",
        "Add Row Below · ⌘↩",
        TableEdit::RowAfter,
    ),
    (
        "table-bar-column-add-before",
        "Add Column Left",
        TableEdit::ColumnBefore,
    ),
    (
        "table-bar-column-add",
        "Add Column Right",
        TableEdit::ColumnAfter,
    ),
    (
        "table-bar-align-left",
        "Align Column Left",
        TableEdit::Align(ColumnAlignment::Left),
    ),
    (
        "table-bar-align-center",
        "Align Column Center",
        TableEdit::Align(ColumnAlignment::Center),
    ),
    (
        "table-bar-align-right",
        "Align Column Right",
        TableEdit::Align(ColumnAlignment::Right),
    ),
    ("table-bar-row-delete", "Delete Row", TableEdit::DeleteRow),
    (
        "table-bar-column-delete",
        "Delete Column",
        TableEdit::DeleteColumn,
    ),
    (
        "table-bar-table-delete",
        "Delete Table",
        TableEdit::DeleteTable,
    ),
];
/// Where the pill's two dividers fall: before the alignment trio, and before the
/// controls that take something away.
const DIVIDERS: [usize; 2] = [4, 7];

/// One edit of the table the caret is in. The ⌘K panel names them and the toolbar draws
/// them; each resolves to a command of the editor's table catalogue.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TableEdit {
    RowBefore,
    RowAfter,
    ColumnBefore,
    ColumnAfter,
    DeleteRow,
    DeleteColumn,
    DeleteTable,
    Align(ColumnAlignment),
}

impl TableEdit {
    pub(super) fn run(self, editor: &mut EditorView, cx: &mut Context<EditorView>) -> bool {
        match self {
            TableEdit::RowBefore => editor.table_add_row_before(cx),
            TableEdit::RowAfter => editor.table_add_row_after(cx),
            TableEdit::ColumnBefore => editor.table_add_column_before(cx),
            TableEdit::ColumnAfter => editor.table_add_column_after(cx),
            TableEdit::DeleteRow => editor.table_delete_row(cx),
            TableEdit::DeleteColumn => editor.table_delete_column(cx),
            TableEdit::DeleteTable => editor.table_delete_table(cx),
            TableEdit::Align(alignment) => editor.table_set_alignment(alignment, cx),
        }
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            TableEdit::RowBefore => Icon::RowAbove,
            TableEdit::RowAfter => Icon::RowBelow,
            TableEdit::ColumnBefore => Icon::ColumnLeft,
            TableEdit::ColumnAfter => Icon::ColumnRight,
            TableEdit::DeleteRow => Icon::RowDelete,
            TableEdit::DeleteColumn => Icon::ColumnDelete,
            TableEdit::DeleteTable => Icon::Trash,
            TableEdit::Align(ColumnAlignment::Center) => Icon::AlignCenter,
            TableEdit::Align(ColumnAlignment::Right) => Icon::AlignRight,
            TableEdit::Align(_) => Icon::AlignLeft,
        }
    }
}

impl NotesApp {
    /// Whether the ring rests on the table toolbar rather than on the surface below it.
    fn table_ringed(&self) -> bool {
        self.ring.at().is_some_and(|id| id.starts_with(STOP))
    }

    /// Every control of the toolbar, in the order it is drawn, so the ring walks the
    /// pill in the order the eye reads it.
    pub(super) fn table_stops(&self) -> impl Iterator<Item = (&'static str, Intent)> {
        CONTROLS
            .into_iter()
            .map(|(id, _, edit)| (id, Intent::Table(edit)))
    }

    /// The pill floating over the grid the caret is in: adding to it, aligning its
    /// column, and taking from it. It is not a popover the user opened, so nothing
    /// dismisses it — it follows the caret, and leaving the table is what puts it away.
    pub(super) fn table_toolbar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        // The ring can step off the note onto the pill, which keeps the note's claim on
        // the toolbar while the keyboard is up there.
        let ringed = self.table_ringed();
        let open = self.interaction.panel() == Panel::Editor
            && self.interaction.popover().is_none()
            && self.interaction.html().is_none()
            && (ringed || self.editor().focus_handle(cx).is_focused(window));
        self.toolbar.set_table(
            open.then(|| self.editor().read(cx).table_at_caret())
                .flatten(),
        );
        let Some(table) = self.toolbar.table().copied() else {
            // The ring cannot rest on a toolbar that is no longer drawn.
            if ringed {
                self.ring.release();
            }
            return None;
        };
        let anchor = table.bounds;
        let viewport = window.bounds().size;
        // Above the grid, unless that would leave the pill in the band the title and the
        // action capsule float in, where it would sit on top of them; then it goes below.
        let above = anchor.top() - HEIGHT - GAP;
        let top = if above < TOOLBAR_HEIGHT + GAP {
            anchor.bottom() + GAP
        } else {
            above
        };
        // Right-aligned to the grid, or left-aligned to it when the pill is the wider of
        // the two, and shifted rather than clipped at either edge of the window.
        let left = if WIDTH < anchor.size.width {
            anchor.right() - WIDTH
        } else {
            anchor.left()
        };
        let mut pill = self
            .capsule()
            .id("table-toolbar")
            .absolute()
            .top(top)
            .left((left.min(viewport.width - WIDTH - px(8.))).max(px(8.)))
            .h(HEIGHT)
            .px(px(4.))
            .gap(px(1.))
            .shadow(popover_shadow())
            // Clicks on the pill must not reach the note underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, (id, label, edit)) in CONTROLS.into_iter().enumerate() {
            if DIVIDERS.contains(&index) {
                pill = pill.child(
                    div()
                        .w(px(1.))
                        .h(px(16.))
                        .mx(px(3.))
                        .bg(self.border_color()),
                );
            }
            // Only the alignment trio carries a state: it is segmented, and the caret's
            // column wears its own. The rest only act, so they stay plain buttons.
            let toggled = matches!(edit, TableEdit::Align(_))
                .then(|| edit == TableEdit::Align(table.alignment));
            pill = pill.child(self.format_button(
                id,
                label,
                edit.icon(),
                Intent::Table(edit),
                toggled,
                cx,
            ));
        }
        Some(pill)
    }
}
