//! The platform text protocol: marked text, selected text, and the two
//! replacement calls, all over the projection's UTF-16 conversions.
//!
//! A composition lives in the document as ordinary content with a marked range
//! the [`composition`](markraft_doc::composition) extension keeps; nothing here
//! holds an uncommitted buffer of its own.

use crate::{EditorView, keymap, single_line};
use gpui::{prelude::*, *};
use markraft_doc::{CompositionRange, Selection, TransactionSpec};
use std::ops::Range;

impl EditorView {
    /// A UTF-16 range over the whole document's text as a position range.
    fn positions_of(&self, range: &Range<usize>) -> Option<(usize, usize)> {
        self.projection()
            .utf16_range_to_pos_range(range.start, range.end)
    }

    /// A position range as a UTF-16 range over the whole document's text.
    fn utf16_of(&self, from: usize, to: usize) -> Option<Range<usize>> {
        let (start, end) = self.projection().pos_range_to_utf16_range(from, to)?;
        Some(start..end)
    }

    /// Replace `range` with `text` and end any composition.
    fn commit(&mut self, range: Option<(usize, usize)>, text: &str, cx: &mut Context<Self>) {
        let doc = self.state().doc();
        let (from, to) = range
            .or_else(|| markraft_doc::composition_range(self.state()).map(|r| (r.from, r.to)))
            .unwrap_or_else(|| {
                let replacement = self.state().selection().replacement_range(doc);
                (replacement.from, replacement.to)
            });
        let select = TransactionSpec::new()
            .selection(Selection::text(from, to))
            .add_to_history(false);
        // The insertion is computed against the selection it replaces, which is
        // what the first spec establishes; the two travel as one transaction.
        let insert = self
            .state()
            .update([select.clone()])
            .ok()
            .and_then(|tr| keymap::insert_plain(&self.types, text)(tr.state()));
        let mut specs = vec![select];
        if let Some(insert) = insert {
            specs.push(insert.sequential());
        }
        specs.push(markraft_doc::finish_composition().sequential());
        self.dispatch(specs, cx);
    }

    /// Bind the three accessibility actions that write text.
    pub(crate) fn bind_accessibility_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selection_editor = cx.entity();
        let replacement_editor = cx.entity();
        let value_editor = cx.entity();
        root.on_a11y_action(
            AccessibleAction::SetTextSelection,
            move |data, window, cx| {
                if let Some(accesskit::ActionData::SetTextSelection(selection)) = data {
                    selection_editor.update(cx, |this, cx| {
                        let selection = this.accessible_text.borrow().selection(selection);
                        if let Some(selection) = selection {
                            window.focus(&this.focus, cx);
                            this.dispatch([TransactionSpec::new().selection(selection)], cx);
                        }
                    });
                }
            },
        )
        .on_a11y_action(
            AccessibleAction::ReplaceSelectedText,
            move |data, window, cx| {
                if let Some(accesskit::ActionData::Value(value)) = data {
                    replacement_editor.update(cx, |this, cx| {
                        window.focus(&this.focus, cx);
                        let value = single_line::text(value, this.single_line).into_owned();
                        this.commit(None, &value, cx);
                    });
                }
            },
        )
        .on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
            if let Some(accesskit::ActionData::Value(value)) = data {
                value_editor.update(cx, |this, cx| {
                    window.focus(&this.focus, cx);
                    let value = single_line::text(value, this.single_line).into_owned();
                    let end = this.state().doc().content_size();
                    this.commit(Some((0, end)), &value, cx);
                });
            }
        })
    }
}

impl EntityInputHandler for EditorView {
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        EditorView::accepts_text_input(self)
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let projection = self.projection();
        let (from, to) = projection.utf16_range_to_pos_range(range.start, range.end)?;
        let (start, end) = projection.pos_range_to_utf16_range(from, to)?;
        *actual = Some(start..end);
        let units: Vec<u16> = projection.plain_text().encode_utf16().collect();
        Some(String::from_utf16_lossy(units.get(start..end.max(start))?))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let doc = self.state().doc();
        let selection = self.state().selection();
        let (anchor, head) = (selection.anchor(doc), selection.head(doc));
        Some(UTF16Selection {
            range: self.utf16_of(anchor.min(head), anchor.max(head))?,
            reversed: anchor > head,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let range = markraft_doc::composition_range(self.state())?;
        self.utf16_of(range.from, range.to)
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.dispatch([markraft_doc::finish_composition()], cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Belt and braces: an unbound printable key can still reach here through a
        // platform path that does not consult `accepts_text_input`.
        if !EditorView::accepts_text_input(self) {
            return;
        }
        let text = single_line::text(text, self.single_line).into_owned();
        let positions = range.as_ref().and_then(|range| self.positions_of(range));
        if range.is_none() && !self.is_composing() {
            // Ordinary typing: the history groups consecutive characters itself.
            let command = keymap::insert_plain(&self.types, &text);
            self.run_command(&command, cx);
            return;
        }
        self.commit(positions, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if self.single_line {
            single_line::selected_range(text, selected)
        } else {
            selected
        };
        let text = single_line::text(text, self.single_line).into_owned();
        let mut specs = Vec::new();
        if let Some(range) = range.as_ref()
            && let Some((from, to)) = self.positions_of(range)
        {
            specs.push(markraft_doc::start_composition(CompositionRange::new(
                from, to,
            )));
        }
        // A caret expressed in UTF-16 units of the candidate, as `char`s of it.
        let caret = selected
            .map(|selected| {
                let mut units = 0usize;
                let mut chars = 0usize;
                for character in text.chars() {
                    if units >= selected.end {
                        break;
                    }
                    units += character.len_utf16();
                    chars += 1;
                }
                chars
            })
            .unwrap_or_else(|| text.chars().count());
        let marked = if specs.is_empty() {
            None
        } else {
            match self.state().update(specs.clone()) {
                Ok(tr) => Some(tr),
                Err(_) => return,
            }
        };
        let base = marked
            .as_ref()
            .map(|tr| tr.state())
            .unwrap_or_else(|| self.state());
        let Ok(spec) = markraft_doc::update_composition(base, &text, caret) else {
            return;
        };
        let spec = if specs.is_empty() {
            spec
        } else {
            spec.sequential()
        };
        drop(marked);
        specs.push(spec);
        self.dispatch(specs, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (from, to) = self.positions_of(&range)?;
        let projection = self.projection();
        let (index, offset) = projection.pos_to_line_offset(from)?;
        let row = self.layout.get(index)?;
        let a = row.caret(offset, self.upstream);
        let b = match projection.pos_to_line_offset(to) {
            Some((end_index, end_offset)) if end_index == index => {
                row.caret(end_offset, self.upstream)
            }
            _ => a,
        };
        let width = if a.y == b.y {
            (b.x - a.x).max(px(2.))
        } else {
            px(2.)
        };
        Some(Bounds::new(a, size(width, row.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.projection().pos_to_utf16(self.hit(point))
    }
}
