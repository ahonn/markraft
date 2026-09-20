//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::surface::LayoutLine;
use gpui::{A11ySubtreeBuilder, App, Bounds, Entity, Pixels, Role, Window, accesskit};
use markraft_core::projection::Projection;
use markraft_core::{EditorState, Selection};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(crate) struct AccessibleText {
    runs: Vec<TextRun>,
    selection: (usize, usize),
    controls: Vec<AccessibleControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ControlAction {
    ToggleTask(usize),
    CodeLanguage(usize),
    CopyCode(usize),
    EditHtml(usize),
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

fn html_label(source: &str) -> String {
    let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = source.graphemes(true);
    let mut preview = graphemes.by_ref().take(60).collect::<String>();
    if graphemes.next().is_some() {
        preview.push('…');
    }
    if preview.is_empty() {
        "Edit empty HTML source".into()
    } else {
        format!("Edit HTML source: {preview}")
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
            ControlAction::EditHtml(_) => Some("Command+Option+R"),
            ControlAction::OpenWikiLink(_) | ControlAction::EnterCallout(_) => None,
        };
        if let Some(shortcut) = shortcut {
            node.set_keyboard_shortcut(shortcut);
        }
        node
    }
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

impl AccessibleText {
    pub(crate) fn update(
        &mut self,
        projection: &Projection,
        state: &EditorState,
        types: &crate::DocTypes,
        rows: &[LayoutLine],
        scale: f32,
    ) {
        let doc = state.doc();
        self.selection = (state.selection().anchor(doc), state.selection().head(doc));
        self.runs.clear();
        self.controls.clear();
        let last_line = projection.line_count().saturating_sub(1);
        for row in rows {
            let Some(line) = projection.line(row.index) else {
                continue;
            };
            let text = projection.line_text(row.index).unwrap_or_default();
            for run in &line.runs {
                let markraft_core::projection::RunContent::Atom(node) = &run.content else {
                    continue;
                };
                let control = if Some(node.type_id()) == types.raw_inline {
                    let source = node
                        .attrs()
                        .get("source")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    Some((ControlAction::EditHtml(run.from), html_label(source)))
                } else if Some(node.type_id()) == types.wiki_link {
                    let label = crate::wiki::wiki_link_label(node);
                    Some((
                        ControlAction::OpenWikiLink(run.from),
                        wiki_link_label(label),
                    ))
                } else {
                    None
                };
                if let Some((action, label)) = control
                    && let Some(bounds) = row
                        .rectangles(
                            row.pos_to_offset(run.from)..row.pos_to_offset(run.to),
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
                    label: if text.is_empty() {
                        "Task".into()
                    } else {
                        text.into()
                    },
                    checked: Some(checked),
                    bounds: accessible_bounds(bounds, scale),
                });
            }
            if let Some(pos) = row.code_pos {
                if let Some(bounds) = row.code_language_bounds() {
                    let language = doc
                        .node_at(pos)
                        .and_then(|node| {
                            node.attrs()
                                .get("language")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned)
                        })
                        .unwrap_or_default();
                    self.controls.push(AccessibleControl {
                        node_id: None,
                        action: ControlAction::CodeLanguage(pos),
                        label: format!(
                            "Code language: {}",
                            crate::syntax::language_label(&language)
                        ),
                        checked: None,
                        bounds: accessible_bounds(bounds, scale),
                    });
                }
                if let Some(bounds) = row.code_copy_bounds() {
                    self.controls.push(AccessibleControl {
                        node_id: None,
                        action: ControlAction::CopyCode(pos),
                        label: "Copy code".into(),
                        checked: None,
                        bounds: accessible_bounds(bounds, scale),
                    });
                }
            }
            for (visual, inner) in row.accessible_rows().into_iter().enumerate() {
                let mut value: String = text
                    .chars()
                    .skip(inner.start)
                    .take(inner.end - inner.start)
                    .collect();
                if inner.end == line.len() && row.index < last_line {
                    value.push('\n');
                }
                let x = f32::from(row.origin.x) * scale;
                let y = f32::from(row.origin.y + row.line_height * visual as f32) * scale;
                let offsets = character_offsets(&value);
                let positions = offsets
                    .iter()
                    .map(|&byte| {
                        let offset = (inner.start + value[..byte].chars().count()).min(inner.end);
                        line.offset_to_pos(offset)
                            .expect("a visible offset inside the line")
                    })
                    .collect();
                self.runs.push(TextRun {
                    node_id: None,
                    from: if inner.start == 0 {
                        line.from
                    } else {
                        line.offset_to_pos(inner.start)
                            .expect("a row starts in its line")
                    },
                    content_end: if inner.end == line.len() {
                        line.to
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
            .then(|| line.ancestors.last().map(|a| a.before))
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
            ControlAction::EditHtml(position) => {
                if self.raw_html_at(position).is_none() {
                    return;
                }
                let Some((index, _)) = self.projection.pos_to_line_offset(position) else {
                    return;
                };
                (index, position)
            }
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
                            self.types.is_code_block(line)
                                && line.ancestors.last().is_some_and(|a| a.before == pos)
                        })
                else {
                    return;
                };
                (index, line.from)
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
            ControlAction::EditHtml(pos) => cx.emit(crate::EditorEvent::RawHtmlRequested { pos }),
            ControlAction::OpenWikiLink(pos) => {
                if let Some(node) = self.wiki_link_at(pos) {
                    cx.emit(crate::EditorEvent::WikiLinkClicked {
                        target: crate::wiki::wiki_link_target(&node),
                        embed: crate::wiki::wiki_link_embed(&node),
                    });
                }
            }
            ControlAction::ToggleTask(_) => {
                self.run_command(&crate::keymap::toggle_task(&self.types), cx);
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
mod tests {
    use super::*;

    #[test]
    fn html_control_labels_identify_source_without_splitting_graphemes() {
        assert_eq!(
            html_label("<span\n title=\"hello\">"),
            "Edit HTML source: <span title=\"hello\">"
        );
        assert_ne!(html_label("<span>"), html_label("</span>"));
        let cluster = "👩🏽‍💻";
        assert_eq!(
            html_label(&cluster.repeat(61)),
            format!("Edit HTML source: {}…", cluster.repeat(60))
        );
        assert_eq!(html_label(" \n "), "Edit empty HTML source");
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
            },
            gpui::px(400.),
            &text_system,
        );
        let mut text = AccessibleText::default();
        text.update(&projection, &state, &types, &rows, 2.);
        assert_eq!(text.controls.len(), 6);
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
        let code_pos = rows[2].code_pos.unwrap();
        assert_eq!(
            text.controls[2].action,
            ControlAction::CodeLanguage(code_pos)
        );
        assert_eq!(text.controls[3].action, ControlAction::CopyCode(code_pos));
        for control in &text.controls[2..] {
            assert_eq!(control.node().role(), Role::Button);
            assert!(control.node().supports_action(accesskit::Action::Click));
        }
        assert_eq!(text.controls[2].label, "Code language: Rust");
        assert_eq!(text.controls[3].label, "Copy code");
        for control in &text.controls[4..] {
            let ControlAction::EditHtml(pos) = control.action else {
                panic!("HTML control")
            };
            assert_eq!(
                Some(state.doc().node_at(pos).unwrap().type_id()),
                types.raw_inline
            );
            let node = state.doc().node_at(pos).unwrap();
            assert_eq!(
                control.label,
                html_label(node.attrs().get("source").unwrap().as_str().unwrap())
            );
        }
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
            runs: vec![TextRun {
                node_id: Some(accesskit::NodeId(1)),
                from: line.from,
                content_end: line.to,
                text: value.into(),
                offsets,
                positions: positions.clone(),
                bounds: accesskit::Rect::ZERO,
                cell: None,
            }],
            selection: (line.from, line.to),
            controls: Vec::new(),
        };
        for (index, pos) in positions.into_iter().enumerate() {
            let accessible = text.text_position(pos).unwrap();
            assert_eq!(accessible.character_index, index);
            assert_eq!(text.position(accessible), Some(pos));
        }
        assert_eq!(text.text_position(line.from).unwrap().character_index, 0);
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
