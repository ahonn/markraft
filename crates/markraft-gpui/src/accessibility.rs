//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::surface::LayoutLine;
use gpui::{A11ySubtreeBuilder, App, Bounds, Entity, Pixels, Role, Window, accesskit};
use markraft_core::kind::conceal::{self, Reveal};
use markraft_core::projection::{Line, Projection};
use markraft_core::{EditorState, Selection};
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(crate) struct AccessibleText {
    /// What the last update was built from; see [`AccessibleText::update`].
    built: Option<Built>,
    /// What each line no frame laid out reads as, for the projection they
    /// line up with; see [`AccessibleText::push_unlaid`].
    unlaid: Vec<Option<Unlaid>>,
    unlaid_of: Option<Arc<Projection>>,
    runs: Vec<TextRun>,
    selection: (usize, usize),
    controls: Vec<AccessibleControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ControlAction {
    ToggleTask(usize),
    CodeLanguage(usize),
    CopyCode(usize),
    OpenWikiLink(usize),
    EnterCallout(usize),
}

struct AccessibleControl {
    node_id: Option<accesskit::NodeId>,
    action: ControlAction,
    label: String,
    checked: Option<bool>,
    bounds: accesskit::Rect,
}

fn accessible_bounds(bounds: Bounds<Pixels>, scale: f32) -> accesskit::Rect {
    accesskit::Rect {
        x0: f64::from(f32::from(bounds.left()) * scale),
        y0: f64::from(f32::from(bounds.top()) * scale),
        x1: f64::from(f32::from(bounds.right()) * scale),
        y1: f64::from(f32::from(bounds.bottom()) * scale),
    }
}

/// What a screen reader is told a wiki link is, so it reads as a link to
/// somewhere rather than as the bare character the projection gives an atom.
fn wiki_link_label(label: &str) -> String {
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        "Empty wiki link".into()
    } else {
        format!("Wiki link: {label}")
    }
}

impl AccessibleControl {
    fn node(&self) -> accesskit::Node {
        let mut node = accesskit::Node::new(if self.checked.is_some() {
            Role::CheckBox
        } else {
            Role::Button
        });
        node.set_label(self.label.clone());
        node.set_bounds(self.bounds);
        node.add_action(accesskit::Action::Click);
        node.add_action(accesskit::Action::Focus);
        if let Some(checked) = self.checked {
            node.set_toggled(if checked {
                accesskit::Toggled::True
            } else {
                accesskit::Toggled::False
            });
        }
        // A wiki link has no binding of its own: it is followed by clicking or
        // by activating it here, so there is no shortcut to announce.
        let shortcut = match self.action {
            ControlAction::ToggleTask(_) => Some("Command+Enter"),
            ControlAction::CodeLanguage(_) => Some("Command+Option+L"),
            ControlAction::CopyCode(_) => Some("Command+Option+Shift+C"),
            ControlAction::OpenWikiLink(_) | ControlAction::EnterCallout(_) => None,
        };
        if let Some(shortcut) = shortcut {
            node.set_keyboard_shortcut(shortcut);
        }
        node
    }
}

/// A line no frame laid out, as a reader is told it: kept from one update to
/// the next while the line keeps its body, since reading it means walking
/// its text grapheme by grapheme. Positions are the line's own, from where
/// it starts, so a line an edit only moved along keeps them.
struct Unlaid {
    text: String,
    offsets: Vec<usize>,
    positions: Vec<usize>,
    /// Whether the text ends in the break to the next line, which the last
    /// line has none of.
    newline: bool,
    cell: Option<(usize, usize)>,
}

/// What an update read that changes what a reader is told.
#[derive(PartialEq)]
struct Built {
    projection: usize,
    selection: (usize, usize),
    composition: Option<(usize, usize)>,
    rows: usize,
    first: Option<(usize, gpui::Point<Pixels>)>,
    last: Option<(usize, gpui::Point<Pixels>)>,
    scale: f32,
}

struct TextRun {
    node_id: Option<accesskit::NodeId>,
    /// Document position of the run's first character.
    from: usize,
    /// Document position just past the run's own content.
    content_end: usize,
    text: String,
    offsets: Vec<usize>,
    /// Document positions for each selectable unit boundary in `offsets`.
    positions: Vec<usize>,
    bounds: accesskit::Rect,
    /// The run's row and column, when it sits in a table cell.
    ///
    /// Announced on the run itself rather than through a `Role::Cell`
    /// container: the subtree builder appends leaves under the editor's own
    /// node and has no way to nest one, and a container between the editor and
    /// its runs would break the flat run sequence its text selection is
    /// expressed in.
    cell: Option<(usize, usize)>,
}

/// What one visual row shows of a line, given as `pieces` over its projected
/// text and the `char` range `inner` of it the row holds, with the projection
/// offset before each shown `char`.
///
/// A concealed run the row hides adds nothing; one it substitutes adds what it
/// displays, every character of which stands before the whole run, so a
/// selection can only take or leave it whole.
fn shown_row(pieces: &[conceal::Piece<'_>], inner: Range<usize>) -> (String, Vec<usize>) {
    let mut value = String::new();
    let mut before = Vec::new();
    for piece in pieces {
        if piece.own {
            let from = piece.source.start.max(inner.start);
            let to = piece.source.end.min(inner.end);
            for (index, c) in piece.text.chars().enumerate() {
                let at = piece.source.start + index;
                if (from..to).contains(&at) {
                    value.push(c);
                    before.push(at);
                }
            }
        } else if inner.contains(&piece.source.start) {
            for c in piece.text.chars() {
                value.push(c);
                before.push(piece.source.start);
            }
        }
    }
    (value, before)
}

// AccessKit uses u8 for each selectable unit's UTF-8 length. An unusually long
// combining sequence cannot fit; expose its scalars and clamp incoming requests
// through the projection so editing still cannot split the grapheme.
fn character_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (start, grapheme) in text.grapheme_indices(true) {
        if grapheme.len() <= usize::from(u8::MAX) {
            offsets.push(start + grapheme.len());
        } else {
            offsets.extend(
                grapheme
                    .char_indices()
                    .map(|(index, c)| start + index + c.len_utf8()),
            );
        }
    }
    offsets
}

/// The document position before each unit boundary in `offsets`, of text
/// `value` whose characters stand at the line offsets `before`: walked once,
/// since a long line has many boundaries. Past the text is `end`.
fn positions_of(
    line: &Line,
    value: &str,
    before: &[usize],
    offsets: &[usize],
    end: usize,
) -> Vec<usize> {
    let mut chars = value
        .char_indices()
        .map(|(byte, _)| byte)
        .enumerate()
        .peekable();
    offsets
        .iter()
        .map(|&byte| {
            let mut at = before.len();
            while let Some(&(index, start)) = chars.peek() {
                if start >= byte {
                    at = index;
                    break;
                }
                chars.next();
            }
            let offset = before.get(at).copied().unwrap_or(end).min(end);
            line.offset_to_pos(offset)
                .expect("a visible offset inside the line")
        })
        .collect()
}

/// The run `read` stands for, with `line` starting where it starts now.
fn unlaid_run(read: &Unlaid, line: &Line, bounds: Bounds<Pixels>, scale: f32) -> TextRun {
    TextRun {
        node_id: None,
        from: line.from(),
        content_end: line.to(),
        offsets: read.offsets.clone(),
        positions: read.positions.iter().map(|&at| line.abs(at)).collect(),
        text: read.text.clone(),
        bounds: accessible_bounds(bounds, scale),
        cell: read.cell,
    }
}

/// Line `index`, laid out as one run, as a reader is told it.
fn read_unlaid(
    projection: &Projection,
    types: &crate::DocTypes,
    reveal: &Reveal,
    index: usize,
    line: &Line,
    newline: bool,
) -> Unlaid {
    let source = projection.line_text(index).unwrap_or_default();
    let shown = conceal::shown(types.syntax, line, reveal);
    let pieces = conceal::pieces(line, source, &shown);
    let (mut text, mut before) = shown_row(&pieces, 0..line.len());
    if newline {
        text.push('\n');
        before.push(line.len());
    }
    let offsets = character_offsets(&text);
    let positions = positions_of(line, &text, &before, &offsets, line.len())
        .into_iter()
        .map(|pos| pos - line.from())
        .collect();
    Unlaid {
        text,
        offsets,
        positions,
        newline,
        cell: types
            .table_cell_of(line)
            .map(|(_, row, column)| (row, column)),
    }
}

impl AccessibleText {
    /// Give back what the last update built, for an editor that is not being
    /// drawn; the next update rebuilds it all anyway.
    pub(crate) fn release(&mut self) {
        self.built = None;
        self.unlaid = Vec::new();
        self.unlaid_of = None;
        self.runs = Vec::new();
        self.controls = Vec::new();
    }

    /// Rebuild what a reader is told from the document and the frame: every
    /// line, as the visual rows it was laid out in where the frame laid it
    /// out, and as one run standing where `place` says otherwise, so a reader
    /// reaches the whole note however little of it is on screen.
    pub(crate) fn update(
        &mut self,
        projection: &Arc<Projection>,
        state: &EditorState,
        types: &crate::DocTypes,
        rows: &[LayoutLine],
        place: &dyn Fn(usize) -> Bounds<Pixels>,
        scale: f32,
    ) {
        let doc = state.doc();
        // Every frame asks, and most change nothing a reader is told: a caret
        // blink, a line off screen measured. A line the frame did not lay out
        // keeps the place it was given until something here moves, which only
        // costs a reader how precisely an unseen line is outlined.
        let built = Built {
            projection: std::ptr::from_ref(projection) as usize,
            selection: (state.selection().anchor(doc), state.selection().head(doc)),
            composition: markraft_core::composition::composition_range(state)
                .map(|range| (range.from, range.to)),
            rows: rows.len(),
            first: rows.first().map(|row| (row.index, row.origin)),
            last: rows.last().map(|row| (row.index, row.origin)),
            scale,
        };
        if self.built.as_ref() == Some(&built) {
            return;
        }
        self.built = Some(built);
        self.line_up_unlaid(projection);
        self.selection = (state.selection().anchor(doc), state.selection().head(doc));
        // What a reader hears is what the screen shows: a concealed span reads
        // as what it displays, and as its source while the caret reveals it.
        let reveal = Reveal::at(
            state.selection().from(doc)..state.selection().to(doc),
            markraft_core::composition::composition_range(state).map(|range| range.from..range.to),
        );
        self.runs.clear();
        self.controls.clear();
        let last_line = projection.line_count().saturating_sub(1);
        let mut laid = rows.iter().peekable();
        for index in 0..projection.line_count() {
            let Some(line) = projection.line(index) else {
                continue;
            };
            while laid.next_if(|row| row.index < index).is_some() {}
            let Some(row) = laid.next_if(|row| row.index == index) else {
                self.push_unlaid(projection, types, &reveal, index, place(index), scale);
                continue;
            };
            let source = projection.line_text(row.index).unwrap_or_default();
            let shown = conceal::shown(types.syntax, line, &reveal);
            let pieces = conceal::pieces(line, source, &shown);
            let prose: String = {
                let shown = conceal::shown(types.syntax, line, &Reveal::nothing());
                conceal::pieces(line, source, &shown)
                    .iter()
                    .map(|piece| piece.text)
                    .collect()
            };
            for run in line.runs() {
                let markraft_core::projection::RunContent::Atom(node) = &run.content else {
                    continue;
                };
                // Inline HTML is text the caret edits in place, so it is no
                // control of its own.
                let control = if Some(node.type_id()) == types.wiki_link {
                    let label = crate::wiki::wiki_link_label(node);
                    Some((
                        ControlAction::OpenWikiLink(line.abs(run.start)),
                        wiki_link_label(label),
                    ))
                } else {
                    None
                };
                if let Some((action, label)) = control
                    && let Some(bounds) = row
                        .rectangles(
                            row.pos_to_offset(line.abs(run.start))
                                ..row.pos_to_offset(line.abs(run.end)),
                            false,
                        )
                        .first()
                {
                    self.controls.push(AccessibleControl {
                        node_id: None,
                        action,
                        label,
                        checked: None,
                        bounds: accessible_bounds(*bounds, scale),
                    });
                }
            }
            // A callout's header is not text anyone can reach with the caret,
            // so what it says reaches a screen reader as a control that puts
            // the caret where the callout's own content starts.
            if let Some((label, bounds)) = row.callout_header() {
                self.controls.push(AccessibleControl {
                    node_id: None,
                    action: ControlAction::EnterCallout(row.from),
                    label: format!("Callout: {label}"),
                    checked: None,
                    bounds: accessible_bounds(bounds, scale),
                });
            }
            if let Some((checked, bounds)) = row.task_marker() {
                self.controls.push(AccessibleControl {
                    node_id: None,
                    action: ControlAction::ToggleTask(row.from),
                    label: if prose.is_empty() {
                        "Task".into()
                    } else {
                        prose.clone()
                    },
                    checked: Some(checked),
                    bounds: accessible_bounds(bounds, scale),
                });
            }
            for (visual, inner) in row.accessible_rows().into_iter().enumerate() {
                let (mut value, mut before) = shown_row(&pieces, inner.clone());
                if inner.end == line.len() && row.index < last_line {
                    value.push('\n');
                    before.push(inner.end);
                }
                let x = f32::from(row.origin.x) * scale;
                let y = f32::from(row.origin.y + row.line_height * visual as f32) * scale;
                let offsets = character_offsets(&value);
                let positions = positions_of(line, &value, &before, &offsets, inner.end);
                self.runs.push(TextRun {
                    node_id: None,
                    from: if inner.start == 0 {
                        line.from()
                    } else {
                        line.offset_to_pos(inner.start)
                            .expect("a row starts in its line")
                    },
                    content_end: if inner.end == line.len() {
                        line.to()
                    } else {
                        line.offset_to_pos(inner.end)
                            .expect("a row ends in its line")
                    },
                    offsets,
                    positions,
                    text: value,
                    bounds: accesskit::Rect {
                        x0: f64::from(x),
                        y0: f64::from(y),
                        x1: f64::from(x + f32::from(row.width) * scale),
                        y1: f64::from(y + f32::from(row.line_height) * scale),
                    },
                    cell: row.table.map(|cell| (cell.row, cell.column)),
                });
            }
        }
    }

    /// Line the kept readings of unlaid lines up with `projection`: a line the
    /// edit handed over with its body keeps its reading, and every other line
    /// is read again when it is asked for.
    fn line_up_unlaid(&mut self, projection: &Arc<Projection>) {
        let count = projection.line_count();
        match self.unlaid_of.take() {
            Some(held) if Arc::ptr_eq(&held, projection) => {}
            Some(held) => {
                self.unlaid = crate::surface::kept_ends(held.lines(), projection.lines())
                    .carry(std::mem::take(&mut self.unlaid))
                    .into_iter()
                    .map(Option::flatten)
                    .collect();
            }
            None => self.unlaid = (0..count).map(|_| None).collect(),
        }
        self.unlaid_of = Some(projection.clone());
    }

    /// Line `index`, which no frame laid out, as one run over `bounds`.
    ///
    /// A line the selection or the marked text touches shows its source as
    /// they reveal it, so it is read afresh; any other line reads the same
    /// until its body changes, and is read once.
    fn push_unlaid(
        &mut self,
        projection: &Projection,
        types: &crate::DocTypes,
        reveal: &Reveal,
        index: usize,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) {
        let Some(line) = projection.line(index) else {
            return;
        };
        let newline = index + 1 < projection.line_count();
        let touched = reveal.touches(line.from(), line.to());
        if touched {
            let read = read_unlaid(projection, types, reveal, index, line, newline);
            self.runs.push(unlaid_run(&read, line, bounds, scale));
            return;
        }
        let Some(slot) = self.unlaid.get_mut(index) else {
            return;
        };
        if slot.as_ref().is_none_or(|kept| kept.newline != newline) {
            *slot = Some(read_unlaid(projection, types, reveal, index, line, newline));
        }
        if let Some(read) = slot.as_ref() {
            self.runs.push(unlaid_run(read, line, bounds, scale));
        }
    }

    pub(crate) fn write(&mut self, builder: &mut A11ySubtreeBuilder) {
        for (index, run) in self.runs.iter_mut().enumerate() {
            // A row that stands in for content with no position of its own —
            // an empty line, a divider — shares its document position with the
            // row beside it. Each displayed row still needs its own AccessKit
            // ID, so the index is part of the key.
            let id = builder.synthetic_node_id(("text", run.from, index));
            run.node_id = Some(id);
            let mut node = accesskit::Node::new(Role::TextRun);
            node.set_value(run.text.clone());
            node.set_character_lengths(
                run.offsets
                    .windows(2)
                    .map(|pair| (pair[1] - pair[0]) as u8)
                    .collect::<Vec<_>>(),
            );
            node.set_bounds(run.bounds);
            if let Some((row, column)) = run.cell {
                node.set_row_index(row);
                node.set_column_index(column);
            }
            builder.push_child(id, node);
        }
        for control in &mut self.controls {
            let id = builder.synthetic_node_id(("control", control.action));
            control.node_id = Some(id);
            builder.push_child(id, control.node());
        }
        if let (Some(anchor), Some(focus)) = (
            self.text_position(self.selection.0),
            self.text_position(self.selection.1),
        ) {
            builder
                .parent_node()
                .set_text_selection(accesskit::TextSelection { anchor, focus });
        }
    }

    pub(crate) fn bind_controls(&self, editor: &Entity<crate::EditorView>, window: &mut Window) {
        for control in &self.controls {
            let Some(id) = control.node_id else { continue };
            for activate in [false, true] {
                let editor = editor.clone();
                let action = control.action;
                window.on_a11y_action(
                    id,
                    if activate {
                        accesskit::Action::Click
                    } else {
                        accesskit::Action::Focus
                    },
                    move |_, window: &mut Window, cx: &mut App| {
                        editor.update(cx, |editor, cx| {
                            editor.run_control(action, activate, window, cx);
                        });
                    },
                );
            }
        }
    }

    fn text_position(&self, pos: usize) -> Option<accesskit::TextPosition> {
        let run = self
            .runs
            .iter()
            .rev()
            .find(|run| run.from <= pos && pos <= run.content_end)?;
        let index = run.positions.partition_point(|&boundary| boundary < pos);
        let character_index = if run.positions.get(index) == Some(&pos) {
            index
        } else {
            index.saturating_sub(1)
        };
        Some(accesskit::TextPosition {
            node: run.node_id?,
            character_index,
        })
    }

    fn position(&self, position: accesskit::TextPosition) -> Option<usize> {
        let run = self
            .runs
            .iter()
            .find(|run| run.node_id == Some(position.node))?;
        run.positions.get(position.character_index).copied()
    }

    pub(crate) fn selection(&self, selection: &accesskit::TextSelection) -> Option<Selection> {
        Some(Selection::text(
            self.position(selection.anchor)?,
            self.position(selection.focus)?,
        ))
    }
}

impl crate::EditorView {
    pub(crate) fn active_code_pos(&self) -> Option<usize> {
        let (index, _) = self.projection.pos_to_line_offset(self.head())?;
        let line = self.projection.line(index)?;
        self.types
            .is_code_block(line)
            .then(|| line.block_before())
            .flatten()
    }

    pub(crate) fn run_control(
        &mut self,
        action: ControlAction,
        activate: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.single_line {
            return;
        }
        if self.is_composing() {
            // Cancelling may restore different positions; require a fresh control activation.
            self.cancel_composition(cx);
            return;
        }
        let (index, position) = match action {
            ControlAction::EnterCallout(position) => {
                let Some((index, _)) = self.projection.pos_to_line_offset(position) else {
                    return;
                };
                (index, position)
            }
            ControlAction::OpenWikiLink(position) => {
                if self.wiki_link_at(position).is_none() {
                    return;
                }
                let Some((index, _)) = self.projection.pos_to_line_offset(position) else {
                    return;
                };
                (index, position)
            }
            ControlAction::ToggleTask(position) => {
                let Some((index, _)) = self.projection.pos_to_line_offset(position) else {
                    return;
                };
                let Some(line) = self.projection.line(index) else {
                    return;
                };
                if self
                    .types
                    .item_of(line)
                    .is_none_or(|(item, _)| Some(item.node_type) != self.types.task_item)
                {
                    return;
                }
                (index, position)
            }
            ControlAction::CodeLanguage(pos) | ControlAction::CopyCode(pos) => {
                let Some((index, line)) =
                    self.projection
                        .lines()
                        .iter()
                        .enumerate()
                        .find(|(_, line)| {
                            self.types.is_code_block(line) && line.block_before() == Some(pos)
                        })
                else {
                    return;
                };
                (index, line.from())
            }
        };
        if !activate
            || matches!(
                action,
                ControlAction::ToggleTask(_) | ControlAction::EnterCallout(_)
            )
        {
            window.focus(&self.focus, cx);
            self.select(position, false, cx);
        }
        if !activate {
            return;
        }
        match action {
            ControlAction::OpenWikiLink(pos) => {
                if let Some(node) = self.wiki_link_at(pos) {
                    cx.emit(crate::EditorEvent::WikiLinkClicked {
                        target: crate::wiki::wiki_link_target(&node),
                        embed: crate::wiki::wiki_link_embed(&node),
                    });
                }
            }
            ControlAction::ToggleTask(_) => {
                self.run_command(&markraft_core::kind::chains::toggle_task(&self.types), cx);
            }
            ControlAction::CodeLanguage(pos) => {
                cx.emit(crate::EditorEvent::CodeLanguageRequested { pos })
            }
            // Selecting the position is the whole action: the header names the
            // callout, and reaching it means reaching its content.
            ControlAction::EnterCallout(_) => {}
            ControlAction::CopyCode(_) => {
                if let Some(text) = self.projection.line_text(index) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_owned()));
                    cx.emit(crate::EditorEvent::CodeCopied);
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// A reading kept from before an edit is what reading the edited note
    /// afresh gives: the lines the edit moved along start where they now
    /// start, and the line it changed reads as it now does.
    #[test]
    fn kept_readings_are_fresh_readings() {
        use markraft_core::{Selection, TransactionSpec};
        let source = "first **bold**\n\n| a | b |\n| - | - |\n| c | d |\n\n> quoted *text*\n\nlast";
        let state = crate::typeahead::tests::state_of(source);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let place = |_: usize| Bounds::default();
        let read = |text: &AccessibleText| {
            text.runs
                .iter()
                .map(|run| {
                    (
                        run.from,
                        run.content_end,
                        run.text.clone(),
                        run.offsets.clone(),
                        run.positions.clone(),
                        run.cell,
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut kept = AccessibleText::default();
        let projection = markraft_core::projection::projection_of(&state);
        kept.update(&projection, &state, &types, &[], &place, 1.);
        // Typed at the start, so every line after the first moves along.
        let edited = state
            .update([TransactionSpec::new().selection(Selection::cursor(1))])
            .expect("a caret")
            .state()
            .clone();
        let edited = edited
            .update([markraft_core::commands::insert_text("new ")(&edited).expect("an insertion")])
            .expect("an edit")
            .state()
            .clone();
        let projection = markraft_core::projection::projection_of(&edited);
        kept.update(&projection, &edited, &types, &[], &place, 1.);
        let mut fresh = AccessibleText::default();
        fresh.update(&projection, &edited, &types, &[], &place, 1.);
        assert_eq!(read(&kept), read(&fresh));
        assert!(read(&kept)[0].2.starts_with("new first"));
    }

    /// A line the frame did not lay out still reaches a reader, as one run
    /// standing where the lines say it is.
    #[test]
    fn lines_the_frame_did_not_lay_out_are_still_read() {
        let state = crate::typeahead::tests::state_of("first\n\n**second**\n\nthird");
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let style = crate::EditorStyle::notes();
        let images = crate::images::Images::default();
        let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(gpui::TextSystem::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
        )));
        let rows = crate::surface::shape(
            &crate::surface::ShapeInput {
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                images: &images,
                wiki: None,
                selection: 0..0,
                spelling: None,
                composition: None,
            },
            gpui::px(400.),
            &text_system,
        );
        // Only the first line was laid out.
        let place = |index: usize| {
            Bounds::new(
                gpui::point(gpui::px(0.), gpui::px(100. * index as f32)),
                gpui::size(gpui::px(400.), gpui::px(20.)),
            )
        };
        let mut text = AccessibleText::default();
        text.update(&projection, &state, &types, &rows[..1], &place, 1.);
        let read: Vec<&str> = text.runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(read, ["first\n", "second\n", "third"]);
        assert_eq!(text.runs[2].bounds.y0, 200.);
    }

    #[test]
    fn controls_expose_states_actions_and_document_targets() {
        let state = crate::typeahead::tests::state_of(
            "7. [ ] open\n8. [x] done\n\n```rust\nlet x = 1;\n```\n\ntext <span>raw</span>",
        );
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let style = crate::EditorStyle::notes();
        let images = crate::images::Images::default();
        let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(gpui::TextSystem::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
        )));
        let rows = crate::surface::shape(
            &crate::surface::ShapeInput {
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                images: &images,
                wiki: None,
                selection: 0..0,
                spelling: None,
                composition: None,
            },
            gpui::px(400.),
            &text_system,
        );
        let mut text = AccessibleText::default();
        text.update(
            &projection,
            &state,
            &types,
            &rows,
            &|_| Bounds::default(),
            2.,
        );
        // Inline HTML is edited as text in place, so only the task boxes are
        // controls.
        assert_eq!(text.controls.len(), 2);
        for (control, checked) in text.controls[..2].iter().zip([false, true]) {
            let node = control.node();
            assert_eq!(node.role(), Role::CheckBox);
            assert_eq!(
                node.toggled(),
                Some(if checked {
                    accesskit::Toggled::True
                } else {
                    accesskit::Toggled::False
                })
            );
            assert!(node.supports_action(accesskit::Action::Click));
            assert!(node.supports_action(accesskit::Action::Focus));
            assert!(matches!(control.action, ControlAction::ToggleTask(_)));
        }
        // A code block has no controls of its own on screen; its language and
        // copy are commands.
        assert!(rows[2].code_pos.is_some());
    }

    /// A screen reader reads what the screen shows: concealed delimiters are
    /// left out, an entity reads as its character and spans the whole entity,
    /// and the span the caret is in reads as its source.
    #[test]
    fn accessible_text_reads_concealed_runs_as_they_are_shown() {
        let state = crate::typeahead::tests::state_of("x **ab** &amp; y");
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let read = |state: &EditorState| {
            let projection = markraft_core::projection::projection_of(state);
            let style = crate::EditorStyle::notes();
            let images = crate::images::Images::default();
            let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(
                gpui::TextSystem::new(std::sync::Arc::new(gpui::NoopTextSystem::new())),
            ));
            let doc = state.doc();
            let rows = crate::surface::shape(
                &crate::surface::ShapeInput {
                    doc,
                    types: &types,
                    projection: &projection,
                    style: &style,
                    single_line: false,
                    images: &images,
                    wiki: None,
                    selection: state.selection().from(doc)..state.selection().to(doc),
                    spelling: None,
                    composition: None,
                },
                gpui::px(400.),
                &text_system,
            );
            let mut text = AccessibleText::default();
            text.update(
                &projection,
                state,
                &types,
                &rows,
                &|_| Bounds::default(),
                1.,
            );
            let run = text.runs.remove(0);
            let line = &projection.lines()[0];
            let offsets: Vec<usize> = run
                .positions
                .iter()
                .map(|pos| line.pos_to_offset(*pos).unwrap())
                .collect();
            (run.text, offsets)
        };
        let projection = markraft_core::projection::projection_of(&state);
        let at = |offset| projection.lines()[0].offset_to_pos(offset).unwrap();
        let away = state
            .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(at(0)))])
            .unwrap()
            .state()
            .clone();
        let (text, offsets) = read(&away);
        assert_eq!(text, "x ab & y");
        // x, space, a, b, space, the entity, space, y — and the end.
        assert_eq!(offsets, vec![0, 1, 4, 5, 8, 9, 14, 15, 16]);
        let inside = state
            .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(at(5)))])
            .unwrap()
            .state()
            .clone();
        assert_eq!(read(&inside).0, "x **ab** & y");
    }

    #[test]
    fn accessible_units_preserve_graphemes_and_represent_every_byte() {
        let text = "你好 é 👨‍👩‍👧‍👦\n";
        let offsets = character_offsets(text);
        assert_eq!(offsets.len() - 1, text.graphemes(true).count());
        assert_eq!(offsets.last(), Some(&text.len()));
        let long = format!("e{}", "\u{301}".repeat(200));
        let offsets = character_offsets(&long);
        assert!(offsets.windows(2).all(|pair| pair[1] - pair[0] <= 255));
        assert_eq!(offsets.last(), Some(&long.len()));
    }

    #[test]
    fn accessible_positions_skip_hidden_inline_boundaries() {
        let state = crate::typeahead::tests::state_of("*你 **好** é*");
        let projection = markraft_core::projection::projection_of(&state);
        let line = &projection.lines()[0];
        let value = projection.line_text(0).unwrap();
        let offsets = character_offsets(value);
        let positions: Vec<_> = offsets
            .iter()
            .map(|&byte| line.offset_to_pos(value[..byte].chars().count()).unwrap())
            .collect();
        let text = AccessibleText {
            built: None,
            unlaid: Vec::new(),
            unlaid_of: None,
            runs: vec![TextRun {
                node_id: Some(accesskit::NodeId(1)),
                from: line.from(),
                content_end: line.to(),
                text: value.into(),
                offsets,
                positions: positions.clone(),
                bounds: accesskit::Rect::ZERO,
                cell: None,
            }],
            selection: (line.from(), line.to()),
            controls: Vec::new(),
        };
        for (index, pos) in positions.into_iter().enumerate() {
            let accessible = text.text_position(pos).unwrap();
            assert_eq!(accessible.character_index, index);
            assert_eq!(text.position(accessible), Some(pos));
        }
        assert_eq!(text.text_position(line.from()).unwrap().character_index, 0);
    }

    #[test]
    fn accessible_positions_roundtrip_wrapped_lines_and_block_boundaries() {
        let run = |id, from, end, text: &str| TextRun {
            node_id: Some(accesskit::NodeId(id)),
            from,
            content_end: end,
            text: text.into(),
            offsets: character_offsets(text),
            positions: character_offsets(text)
                .into_iter()
                .map(|byte| (from + text[..byte].chars().count()).min(end))
                .collect(),
            bounds: accesskit::Rect::ZERO,
            cell: None,
        };
        // Two visual rows of one line holding "你好", then the next block.
        let text = AccessibleText {
            built: None,
            unlaid: Vec::new(),
            unlaid_of: None,
            runs: vec![run(1, 1, 2, "你"), run(2, 2, 3, "好\n")],
            selection: (1, 1),
            controls: Vec::new(),
        };
        let boundary = 2;
        let accessible = text.text_position(boundary).unwrap();
        assert_eq!(accessible.node, accesskit::NodeId(2));
        assert_eq!(accessible.character_index, 0);
        assert_eq!(text.position(accessible), Some(boundary));
        // The synthetic newline maps back to the end of the run's own content.
        assert_eq!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(2),
                character_index: 2
            }),
            Some(3)
        );
        assert!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(99),
                character_index: 0
            })
            .is_none()
        );
    }
}
