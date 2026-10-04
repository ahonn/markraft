//! Word-aware clipboard edits: smart deletion, padded paste, and pasting
//! text in the style of where it lands.

use super::*;

impl EditorView {
    /// Native smart spacing sees the rendered paragraph, while every boundary
    /// must map to contiguous literal source. Delimiters and atoms are barriers.
    pub(super) fn smart_clipboard_context(&self) -> Option<crate::smart_clipboard::Context> {
        if !self.smart_insert_delete
            || self.single_line
            || self.is_composing()
            || self.state.selection().ranges(self.state.doc()).len() > 1
        {
            return None;
        }
        let selection = self.state.selection().from(self.state.doc())
            ..self.state.selection().to(self.state.doc());
        let projection = self.analysis.projection();
        let (index, _) = projection.pos_to_line_offset(selection.start)?;
        let line = projection.line(index)?;
        if selection.end > line.to() || self.types.is_verbatim_block(line) {
            return None;
        }
        let request = self.context_source_range_snapshot(line.from()..line.to())?;
        let mapped = self.context_text_mapping(&request)?;
        if !mapped.exact || mapped.source.iter().any(Option::is_none) {
            return None;
        }
        let spans: Vec<_> = mapped.source.iter().flatten().collect();
        let offset = |position| {
            spans
                .iter()
                .position(|span| span.start == position)
                .or_else(|| {
                    spans
                        .iter()
                        .position(|span| span.end == position)
                        .map(|index| index + 1)
                })
        };
        let start = offset(selection.start)?;
        let end = offset(selection.end)?;
        let mut boundaries: Vec<_> = spans.iter().map(|span| span.start).collect();
        boundaries.push(spans.last()?.end);
        boundaries[start] = selection.start;
        boundaries[end] = selection.end;
        let prose_range = if selection.is_empty() {
            if start < spans.len() {
                (*spans[start]).clone()
            } else {
                (*spans.last()?).clone()
            }
        } else {
            selection.clone()
        };
        let prose = self.context_source_range_snapshot(prose_range)?;
        if !self.context_is_prose(&prose)
            || self.context_text_mapping(&prose)?.replacement_range()? != prose.text_range()
        {
            return None;
        }
        Some(crate::smart_clipboard::Context {
            text: mapped.text,
            selection: start..end,
            boundaries,
        })
    }

    pub(super) fn selected_by_word(&self) -> bool {
        self.word_selection.as_ref() == Some(self.state.selection())
    }

    pub(crate) fn smart_copy_eligible(&self) -> bool {
        self.selected_by_word()
            && self
                .smart_clipboard_context()
                .is_some_and(|context| crate::smart_clipboard::whole_words(&context))
    }

    pub(crate) fn smart_delete_spec(&self) -> Option<TransactionSpec> {
        // Only a word picked as a word takes its space along; a caret or a
        // range extended character by character deletes what it covers.
        if !self.selected_by_word() {
            return None;
        }
        let context = self.smart_clipboard_context()?;
        if !crate::smart_clipboard::whole_words(&context) {
            return None;
        }
        let range = crate::smart_clipboard::delete(&context)?;
        let from = *context.boundaries.get(range.start)?;
        let to = *context.boundaries.get(range.end)?;
        let request = self.context_source_range_snapshot(from..to)?;
        if self.context_text_mapping(&request)?.replacement_range()? != (from..to)
            || !self.context_is_prose(&request)
        {
            return None;
        }
        Some(
            TransactionSpec::new()
                .changes([markraft_core::Change::replace(
                    from,
                    to,
                    markraft_core::Slice::empty(),
                )])
                .user_event(event::DELETE),
        )
    }

    pub(crate) fn smart_paste_specs(
        &self,
        spec: TransactionSpec,
        item: &ClipboardItem,
        cx: &App,
    ) -> Vec<TransactionSpec> {
        let Some(context) = self
            .smart_clipboard_context()
            .filter(|_| clipboard::smart_item(item, cx))
        else {
            return vec![spec];
        };
        let Ok(candidate) = self.state.update([spec.clone()]) else {
            return vec![spec];
        };
        let start = context.boundaries[context.selection.start];
        let end = context.boundaries[context.selection.end];
        let from = candidate
            .changes()
            .desc()
            .map_pos(start, -1, markraft_core::TrackMode::Simple)
            .unwrap_or(start);
        let to = candidate
            .changes()
            .desc()
            .map_pos(end, 1, markraft_core::TrackMode::Simple)
            .unwrap_or(end);
        let selection = Selection::text(from, to);
        let slice = selection.content_with_schema(candidate.new_doc(), self.state.schema());
        let text = markraft_core::kind::conceal::slice_text(
            self.state.schema(),
            self.types.syntax,
            &slice,
        );
        let Some((before, after)) = crate::smart_clipboard::padding(&context, &text) else {
            return vec![spec];
        };
        self.padded_paste_specs(spec, from..to, &before, &after)
    }

    pub(super) fn padded_paste_specs(
        &self,
        spec: TransactionSpec,
        inserted: Range<usize>,
        before: &str,
        after: &str,
    ) -> Vec<TransactionSpec> {
        use markraft_core::{Change, Fragment, Slice};
        let mut changes = Vec::new();
        for (position, text) in [(inserted.start, before), (inserted.end, after)] {
            if !text.is_empty() {
                changes.push(Change::replace(
                    position,
                    position,
                    Slice::from_fragment(Fragment::from_node(self.state.schema().text(text))),
                ));
            }
        }
        if changes.is_empty() {
            vec![spec]
        } else {
            vec![
                spec,
                TransactionSpec::new()
                    .changes(changes)
                    .sequential()
                    .user_event(event::INPUT_PASTE),
            ]
        }
    }

    pub(super) fn paste_match_style(&mut self, cx: &mut Context<Self>) {
        if self.single_line || self.types.in_verbatim_block_at(&self.state) || self.codecs.is_none()
        {
            self.paste(clipboard::PasteMode::Plain, cx);
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        if let Some(spec) = clipboard::math_source_paste(&self.state, &self.types, &text) {
            self.edit(cx, false, vec![spec.user_event(event::INPUT_PASTE)]);
            return;
        }
        if text.contains('\n')
            && self
                .types
                .table_types()
                .and_then(|types| markraft_core::commands::cell_at(types, &self.state))
                .is_some()
        {
            self.paste(clipboard::PasteMode::Plain, cx);
            return;
        }
        let Some(slice) = self.matching_style_slice(&text) else {
            return;
        };
        if let Some(spec) = markraft_core::commands::replace_selection(slice)(&self.state) {
            let specs = self.smart_paste_specs(spec.user_event(event::INPUT_PASTE), &item, cx);
            self.edit(cx, false, specs);
        }
    }

    pub(super) fn matching_style_slice(&self, text: &str) -> Option<markraft_core::Slice> {
        use markraft_core::{Fragment, Slice};
        let codecs = self.codecs.as_ref()?;
        let selected = self.state.selection();
        let doc = self.state.doc();
        let (from, to) = (selected.from(doc), selected.to(doc));
        let mut marks = self.active_marks();
        // Source delimiters outside the replaced range will continue to provide
        // these styles. Spelling them again would nest duplicate delimiters.
        if self.types.syntax.is_some() {
            let before = from.checked_sub(1).and_then(|pos| doc.node_at(pos));
            let after = doc.node_at(to);
            marks = marks.filter(|mark| {
                !(before
                    .as_ref()
                    .is_some_and(|node| node.marks().contains(mark))
                    && after
                        .as_ref()
                        .is_some_and(|node| node.marks().contains(mark)))
            });
        }
        fn styled(
            node: &Node,
            schema: &markraft_core::Schema,
            marks: &markraft_core::MarkSet,
        ) -> Node {
            if node.is_text() {
                let mut combined = node.marks().clone();
                for mark in marks.iter() {
                    combined = combined.add(schema, mark.clone());
                }
                node.mark(combined)
            } else {
                node.copy(Fragment::from_nodes(
                    node.children().map(|child| styled(child, schema, marks)),
                ))
            }
        }
        let plain = codecs.from_text(text);
        let styled = Slice::new(
            Fragment::from_nodes(
                plain
                    .content()
                    .iter()
                    .map(|node| styled(node, self.state.schema(), &marks)),
            ),
            plain.open_start(),
            plain.open_end(),
        );
        Some(codecs.copied(&styled))
    }
}
