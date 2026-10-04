//! What a pointer names: the contextual target under it, the word it picks,
//! and the selection a contextual click settles on.

use super::*;

impl EditorView {
    pub(crate) fn context_mouse_down(
        &mut self,
        mouse: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.selecting = false;
        self.pointer_word_gesture = None;
        self.caret.forget_column();
        self.lay_out_at(mouse.position);
        let position = self.hit(mouse.position);
        let character = self.pointer_character_at(mouse.position);
        let target = self.context_target_at(mouse.position, character.unwrap_or(position));
        let target_document = self.state.doc().clone();
        self.context_target_bookmark = ContextTargetBookmark::new(&target_document, &target);
        let selected = self.state.selection();
        let doc = self.state.doc();
        let inside = character.is_some_and(|pos| {
            selected
                .ranges(doc)
                .iter()
                .any(|range| (range.from..range.to).contains(&pos))
        });
        self.select_context(position, character, inside, &target, cx);
        // Follow the original object, never the old screen coordinates, when
        // selection corrections fold atoms elsewhere. A rewritten target or an
        // untracked document replacement still loses its object commands.
        let bookmark = self.context_target_bookmark.take();
        let target = if target_document.ptr_eq(self.state.doc()) {
            target
        } else {
            bookmark
                .and_then(|bookmark| bookmark.finish(self.state.doc()))
                .unwrap_or(ContextTarget::Text)
        };
        // The edit funnel runs extensions synchronously, including Vim's
        // selection normalization, before a snapshot becomes visible to hosts.
        cx.emit(EditorEvent::ContextMenuRequested(
            self.context_snapshot(mouse.position, target),
        ));
        self.reset_caret_blink(cx);
        cx.notify();
    }

    pub(super) fn select_context(
        &mut self,
        position: usize,
        character: Option<usize>,
        inside: bool,
        target: &ContextTarget,
        cx: &mut Context<Self>,
    ) {
        let mut by_word = false;
        let selection = if inside {
            self.state.selection().clone()
        } else if let ContextTarget::WikiLink { pos, .. } = target {
            // A text caret beside an atom unfolds its source. A contextual
            // selection owns the link as an object and keeps it folded.
            Selection::node(*pos)
        } else if let ContextTarget::Link { range, .. } = target
            && let Some(label) = self.context_link_label(range)
        {
            // Match native text views: a link is one contextual target even
            // when its label contains several words or inline formatting.
            Selection::text(label.start, label.end)
        } else if let Some(selection) = character
            .and_then(|pos| self.word_at_character(pos, crate::word_boundary::Intent::Context))
        {
            by_word = true;
            selection
        } else {
            Selection::near(self.state.schema(), self.state.doc(), position, 1)
        };
        let mut specs = vec![
            TransactionSpec::new()
                .selection(selection)
                .user_event(event::SELECT_POINTER),
        ];
        if self.is_composing() {
            specs.push(markraft_core::composition::finish_composition().sequential());
        }
        self.edit(cx, false, specs);
        if by_word {
            self.word_selection = Some(self.state.selection().clone());
        }
    }

    pub(super) fn context_link_label(&self, range: &Range<usize>) -> Option<Range<usize>> {
        use markraft_core::kind::{conceal::Reveal, reading};

        let projection = self.analysis.projection();
        let (first, _) = projection.pos_to_line_offset(range.start)?;
        let (last, _) = projection.pos_to_line_offset(range.end)?;
        let mut label: Option<Range<usize>> = None;
        // Link marks can cover the source delimiters and destination. Read
        // through the same concealment mapping as search, independently of
        // whether the caret currently reveals that syntax on screen.
        for index in first..=last {
            let line = projection.line(index)?;
            for piece in reading::line_pieces(projection, &self.types, index, &Reveal::nothing()) {
                let start = line.offset_to_pos(piece.source.start)?.max(range.start);
                let end = line.offset_to_pos(piece.source.end)?.min(range.end);
                if start < end {
                    match &mut label {
                        Some(label) => label.end = end,
                        None => label = Some(start..end),
                    }
                }
            }
        }
        label
    }

    pub(super) fn context_link_range(&self, range: Range<usize>, position: usize) -> Range<usize> {
        use markraft_core::kind::conceal;

        let projection = self.analysis.projection();
        let Some((index, _)) = projection.pos_to_line_offset(range.start) else {
            return range;
        };
        let line = &projection.lines()[index];
        let spans: Vec<_> = conceal::markup_spans(self.types.syntax, line)
            .into_iter()
            .filter(|runs| runs.len() > 1)
            .filter_map(|runs| Some(runs.first()?.start..runs.last()?.end))
            .filter(|span| span.start >= range.start && span.end <= range.end)
            .collect();
        // Equal link marks can merge across adjacent source links. The widest
        // paired syntax span owns its nested formatting; single-run escapes
        // cannot define a link. A bare URL has no paired syntax, so keep only
        // the gap containing the pointer when it neighbours an explicit link.
        if let Some(span) = spans
            .iter()
            .filter(|span| span.contains(&position))
            .max_by_key(|span| span.end - span.start)
        {
            return span.clone();
        }
        let start = spans
            .iter()
            .filter(|span| span.end <= position)
            .map(|span| span.end)
            .max()
            .unwrap_or(range.start);
        let end = spans
            .iter()
            .filter(|span| span.start > position)
            .map(|span| span.start)
            .min()
            .unwrap_or(range.end);
        start..end
    }

    /// Hit testing returns the nearest insertion boundary; find the drawn
    /// character on either side, so a word's right half still selects that word.
    pub(crate) fn pointer_word_at(&self, point: Point<Pixels>) -> Option<Selection> {
        self.pointer_character_at(point).and_then(|position| {
            self.word_at_character(position, crate::word_boundary::Intent::Pointer)
        })
    }

    pub(super) fn word_at_character(
        &self,
        position: usize,
        intent: crate::word_boundary::Intent,
    ) -> Option<Selection> {
        use markraft_core::kind::{conceal::Reveal, reading};

        let projection = self.analysis.projection();
        let (index, _) = projection.pos_to_line_offset(position)?;
        let selection = self.state.selection();
        let reveal = Reveal::at(
            selection.from(self.state.doc())..selection.to(self.state.doc()),
            markraft_core::composition::composition_range(&self.state)
                .map(|range| range.from..range.to),
        );
        // This runs before the first click moves the caret and reveals markup.
        let pieces = reading::line_pieces(projection, &self.types, index, &reveal);
        let range = word_at(projection, position, &pieces, intent)?;
        let line = projection.line(index)?;
        let from = line.pos_to_offset(range.start)?;
        let to = line.pos_to_offset(range.end)?;
        let mut covered = from;
        let mut reading = false;
        for piece in &pieces {
            let start = piece.source.start.max(from);
            let end = piece.source.end.min(to);
            if start < end {
                reading |= start > covered || !piece.own;
                covered = covered.max(end);
            }
        }
        reading |= covered < to;
        Some(
            if let Some(syntax) = self.types.syntax.filter(|_| reading) {
                markraft_core::kind::ReadingSelection::selection(range.start, range.end, syntax)
            } else {
                Selection::text(range.start, range.end)
            },
        )
    }

    pub(super) fn pointer_character_at(&self, point: Point<Pixels>) -> Option<usize> {
        if self.frame.rows().is_empty() {
            return None;
        }
        let (row, local) = self.row_under(point);
        let offset = row.char_at(local);
        pointer_character_in_row(row, self.analysis.projection(), point, offset)
    }

    pub(super) fn context_target_at(&self, point: Point<Pixels>, position: usize) -> ContextTarget {
        if let Some(pos) = self.wiki_link_under(point)
            && let Some(node) = self.wiki_link_at(pos)
        {
            return ContextTarget::WikiLink {
                pos,
                target: wiki::wiki_link_target(&node),
                embed: wiki::wiki_link_embed(&node),
            };
        }
        if let Some(url) = self.link_under(point)
            && let Some((range, _)) = self
                .types
                .link
                .and_then(|ty| links::link_at(self.state.doc(), ty, position))
        {
            return ContextTarget::Link {
                url,
                range: self.context_link_range(range, position),
            };
        }
        let Some((row, _)) = self.row_at(position) else {
            return ContextTarget::Text;
        };
        let projection = self.analysis.projection();
        if let Some(line) = projection.line(row.index)
            && let Some(text) = projection.line_text(row.index)
            && let Some(span) = crate::math_spans::formula_spans(line, text, &self.types)
                .into_iter()
                .find(|span| {
                    row.rectangles(span.source.clone(), false)
                        .iter()
                        .any(|bounds| bounds.contains(&point))
                })
            && let Some(pos) = line.offset_to_pos(span.source.start)
        {
            return ContextTarget::Math { pos };
        }
        if let Some(pos) = row.code_pos {
            return ContextTarget::CodeBlock { pos };
        }
        if let Some(cell) = row.table
            && let Some(types) = self.types.table_types()
            && let Ok(resolved) = self.state.doc().resolve(row.from)
            && let Some(depth) = (1..=resolved.depth())
                .rev()
                .find(|&depth| resolved.node(depth).type_id() == types.cell)
        {
            return ContextTarget::Table {
                pos: cell.table,
                cell: resolved.before(depth),
            };
        }
        if let Some(node) = self.state.doc().node_at(position)
            && Some(node.type_id()) == self.types.image
        {
            return ContextTarget::Image { pos: position };
        }
        ContextTarget::Text
    }
}

/// Pointer targets use grapheme selection geometry, not insertion affinity.
/// A bidi primary caret can name a logically distant grapheme; try adjacent
/// graphemes first, then the remaining displayed row. This runs on pointer down.
pub(crate) fn pointer_character_in_row(
    row: &crate::surface::LayoutLine,
    projection: &Projection,
    point: Point<Pixels>,
    insertion: usize,
) -> Option<usize> {
    for adjacent in [true, false] {
        for (position, grapheme) in projection.graphemes(row.index) {
            let start = row.pos_to_offset(position);
            let range = start..start + grapheme.chars().count();
            if (range.contains(&insertion) || range.end == insertion) != adjacent {
                continue;
            }
            if row.rectangles(range, false).iter().any(|bounds| {
                bounds.size.width > gpui::px(0.)
                    && bounds.size.height > gpui::px(0.)
                    && bounds.contains(&point)
            }) {
                return Some(position);
            }
        }
    }
    None
}

pub(crate) fn word_at(
    projection: &Projection,
    position: usize,
    pieces: &[crate::shown::ShownPiece],
    intent: crate::word_boundary::Intent,
) -> Option<Range<usize>> {
    let (index, offset) = projection.pos_to_line_offset(position)?;
    let line = projection.line(index)?;
    let source = projection.line_text(index)?.chars().collect::<Vec<_>>();
    if source.get(offset) == Some(&'\u{fffc}') {
        return None;
    }
    let mut text = String::new();
    let mut spans = Vec::new();
    for piece in pieces {
        if source.get(piece.source.start) == Some(&'\u{fffc}') {
            // Labels are one object, not adjacent prose. Preserve the barrier
            // even when its displayed label happens to contain word characters.
            text.push('\u{fffc}');
            spans.push(piece.source.clone());
        } else {
            text.push_str(&piece.text);
            spans.extend(piece.text.chars().enumerate().map(|(index, _)| {
                if piece.own {
                    let start = piece.source.start + index;
                    start..start + 1
                } else {
                    // An entity or another concealed substitution is atomic in
                    // source; never return an interior of its spelling.
                    piece.source.clone()
                }
            }));
        }
    }
    let character = spans.iter().position(|span| span.contains(&offset))?;
    let selected = crate::word_boundary::at(&text, character, intent)?;
    let start = line.offset_to_pos(spans.get(selected.start)?.start)?;
    let end = line.offset_to_pos(spans.get(selected.end.checked_sub(1)?)?.end)?;
    Some(start..end)
}
