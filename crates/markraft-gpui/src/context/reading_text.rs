//! Reading text for host text services: what a request reads as, where each
//! character is spelled in source, and the guarded edits that write back.

use super::*;

impl EditorView {
    /// Automated spelling and substitutions must not interpret code or formulas
    /// as prose. Hosts decide whether the particular check applies to URLs.
    pub fn context_is_prose(&self, request: &ContextRequest) -> bool {
        if !self.context_is_current(request) || self.is_composing() {
            return false;
        }
        let range = request.text_range();
        for position in range {
            let Ok(resolved) = self.state.doc().resolve(position) else {
                return false;
            };
            if self.types.is_verbatim(resolved.parent().type_id()) {
                return false;
            }
            let Some(node) = self.state.doc().node_at(position) else {
                continue;
            };
            if [self.types.code, self.types.math]
                .into_iter()
                .flatten()
                .any(|ty| node.marks().contains_type(ty))
            {
                return false;
            }
        }
        self.context_text(request).is_some()
    }

    /// Narrow a service snapshot by Unicode scalar offsets in its reading text.
    /// Callers using Cocoa ranges must convert UTF-16 offsets first.
    pub fn context_text_range(
        &self,
        request: &ContextRequest,
        range: Range<usize>,
    ) -> Option<ContextRequest> {
        let mapped = self.context_text_mapping(request)?;
        if range.is_empty() || range.end > mapped.source.len() {
            return None;
        }
        let text: String = mapped
            .text
            .chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect();
        let source = mapped.source[range].to_vec();
        let narrowed = ContextTextMap {
            text,
            source,
            exact: true,
        };
        let mut request = request.clone();
        request.text_range = Some(narrowed.replacement_range()?);
        // Rebuild to enforce grapheme and nonliteral source boundaries too.
        self.context_text_mapping(&request)?.replacement_range()?;
        Some(request)
    }

    /// Read the selected rendered text independently of transient syntax reveal.
    /// A stale menu or a live input-method composition cannot start a service.
    pub fn context_text(&self, request: &ContextRequest) -> Option<ContextText> {
        if request.text_range().is_empty() && self.context_insert_position(request).is_some() {
            return Some(ContextText {
                text: String::new(),
                replaceable: true,
                transformable: false,
            });
        }
        let mapped = self.context_text_mapping(request)?;
        let replaceable = if let Some(command) =
            self.kind
                .replace_reading(request.text_range(), &mapped.text, Default::default())
        {
            match command(&self.state) {
                Ok(Some(_)) => true,
                Ok(None) => mapped.replacement_range().is_some(),
                Err(_) => false,
            }
        } else {
            mapped.replacement_range().is_some()
        };
        Some(ContextText {
            replaceable,
            transformable: mapped.exact,
            text: mapped.text,
        })
    }

    /// Link a precise service range without relocating the current selection.
    /// CommonMark already links bare URLs; never wrap an existing link again.
    pub fn set_context_link(
        &mut self,
        request: &ContextRequest,
        url: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ContextLinkChange::Add(spec)) = self.context_link_spec(request, url) else {
            return false;
        };
        self.dispatch_context_check_specs(vec![spec], cx)
    }

    /// Add links using reading scalar ranges, with one atomic guard and undo.
    pub fn set_context_links(
        &mut self,
        request: &ContextRequest,
        mut links: Vec<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        links.sort_by_key(|(range, _)| range.start);
        if links.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
            return false;
        }
        let mut specs = Vec::new();
        for (range, url) in links {
            let Some(narrowed) = self.context_text_range(request, range) else {
                return false;
            };
            let Some(change) = self.context_link_spec(&narrowed, &url) else {
                return false;
            };
            if let ContextLinkChange::Add(spec) = change {
                specs.push(spec);
            }
        }
        self.dispatch_context_check_specs(specs, cx)
    }

    pub(super) fn dispatch_context_check_specs(
        &mut self,
        mut specs: Vec<TransactionSpec>,
        cx: &mut Context<Self>,
    ) -> bool {
        if specs.is_empty() {
            return false;
        }
        let Ok(candidate) = self.state.update(specs.clone()) else {
            return false;
        };
        let selection = self.state.selection().map(
            self.state.schema(),
            candidate.new_doc(),
            candidate.changes().desc(),
        );
        specs.push(
            TransactionSpec::new()
                .selection(selection)
                .sequential()
                .user_event(event::INPUT_REPLACE),
        );
        self.dispatch_isolated(specs, cx)
    }

    pub(super) fn context_link_spec(
        &self,
        request: &ContextRequest,
        url: &str,
    ) -> Option<ContextLinkChange> {
        let mapped = self.context_text_mapping(request)?;
        let range = mapped.replacement_range()?;
        let link = self.types.link?;
        let existing = links::link_at(self.state.doc(), link, range.start)
            .is_some_and(|(span, _)| span.start <= range.start && range.end <= span.end);
        for position in range.clone() {
            let node = self.state.doc().node_at(position)?;
            if (!existing && node.marks().contains_type(link))
                || [self.types.code, self.types.math]
                    .into_iter()
                    .flatten()
                    .any(|ty| node.marks().contains_type(ty))
            {
                return None;
            }
            if self
                .state
                .doc()
                .resolve(position)
                .is_ok_and(|resolved| self.types.is_verbatim(resolved.parent().type_id()))
            {
                return None;
            }
        }
        // Detectors also report URLs that CommonMark already linked. Keep
        // their existing destination, without cancelling unrelated checks.
        if existing {
            return Some(ContextLinkChange::Existing);
        }
        let Ok(selected) = self
            .state
            .update([TransactionSpec::new().selection(Selection::text(range.start, range.end))])
        else {
            return None;
        };
        let Ok(Some(spec)) = self.link_command(link, Some(url))(selected.state()) else {
            return None;
        };
        Some(ContextLinkChange::Add(spec))
    }

    /// Replace reading text through the document kind's source-preserving
    /// planner, or a precise literal span when the kind has no such planner.
    pub fn replace_context_text(
        &mut self,
        request: &ContextRequest,
        replacement: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.replace_context_text_with_policy(
            request,
            replacement,
            markraft_core::kind::ReadingReplacementPolicy::PreserveUnchanged,
            cx,
        )
    }

    /// Replace a system result with its explicit presentation policy. Unlike
    /// proofreading, a plain-text Services result may change styles even when
    /// its characters are identical to the selected reading text.
    pub fn replace_context_text_with_policy(
        &mut self,
        request: &ContextRequest,
        replacement: &str,
        policy: markraft_core::kind::ReadingReplacementPolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.context_is_current(request) || self.is_composing() {
            return false;
        }
        if let Some(command) = self
            .kind
            .replace_reading(request.text_range(), replacement, policy)
        {
            match command(&self.state) {
                Ok(Some(spec)) => return self.dispatch_isolated([spec], cx),
                Ok(None) => {}
                Err(_) => return false,
            }
        }
        let Some(changes) = self.context_replacement_changes(request, replacement) else {
            return false;
        };
        self.dispatch_isolated(
            [TransactionSpec::new()
                .changes(changes)
                .user_event(event::INPUT_REPLACE)],
            cx,
        )
    }

    /// Apply nonoverlapping replacements expressed in the snapshot's reading
    /// scalar offsets. Validate every source span before committing any change.
    pub fn replace_context_text_edits(
        &mut self,
        request: &ContextRequest,
        mut replacements: Vec<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        replacements.sort_by_key(|(range, _)| range.start);
        if replacements
            .windows(2)
            .any(|pair| pair[0].0.end > pair[1].0.start)
        {
            return false;
        }
        let mut changes = Vec::new();
        for (range, replacement) in replacements {
            let Some(narrowed) = self.context_text_range(request, range) else {
                return false;
            };
            if self
                .context_text(&narrowed)
                .is_some_and(|text| text.text == replacement)
            {
                continue;
            }
            let Some(edit) = self.context_replacement_changes(&narrowed, &replacement) else {
                return false;
            };
            changes.extend(edit);
        }
        !changes.is_empty()
            && self.dispatch_isolated(
                [TransactionSpec::new()
                    .changes(changes)
                    .user_event(event::INPUT_REPLACE)],
                cx,
            )
    }

    /// Apply substitutions and detected links from the same checking snapshot.
    /// Their reading ranges must not overlap; all edits share one guard and undo.
    pub fn apply_context_text_checks(
        &mut self,
        request: &ContextRequest,
        replacements: Vec<(Range<usize>, String)>,
        links: Vec<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut ranges: Vec<_> = replacements
            .iter()
            .chain(links.iter())
            .map(|(range, _)| range.clone())
            .collect();
        ranges.sort_by_key(|range| range.start);
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return false;
        }
        let mut specs = Vec::new();
        for (range, replacement) in replacements {
            let Some(narrowed) = self.context_text_range(request, range) else {
                return false;
            };
            if self
                .context_text(&narrowed)
                .is_some_and(|text| text.text == replacement)
            {
                continue;
            }
            let Some(changes) = self.context_replacement_changes(&narrowed, &replacement) else {
                return false;
            };
            specs.push(TransactionSpec::new().changes(changes));
        }
        for (range, url) in links {
            let Some(narrowed) = self.context_text_range(request, range) else {
                return false;
            };
            let Some(change) = self.context_link_spec(&narrowed, &url) else {
                return false;
            };
            if let ContextLinkChange::Add(spec) = change {
                specs.push(spec);
            }
        }
        self.dispatch_context_check_specs(specs, cx)
    }

    pub(super) fn context_insert_position(&self, request: &ContextRequest) -> Option<usize> {
        if !self.context_is_current(request)
            || self.is_composing()
            || !request.text_range().is_empty()
        {
            return None;
        }
        let position = request.text_range().start;
        let resolved = self.state.doc().resolve(position).ok()?;
        if resolved.depth() != 1 || Some(resolved.parent().type_id()) != self.types.paragraph {
            return None;
        }
        let node = self.state.doc().node_at(position);
        if node.as_ref().is_some_and(|node| {
            [self.types.code, self.types.math]
                .into_iter()
                .flatten()
                .any(|ty| node.marks().contains_type(ty))
        }) {
            return None;
        }
        if node.as_ref().is_some_and(|node| {
            self.types
                .syntax
                .is_some_and(|syntax| node.marks().contains_type(syntax))
        }) {
            return None;
        }
        Some(position)
    }

    pub(super) fn context_replacement_changes(
        &self,
        request: &ContextRequest,
        replacement: &str,
    ) -> Option<Vec<markraft_core::Change>> {
        if let Some(position) = self.context_insert_position(request) {
            if replacement.is_empty() {
                return None;
            }
            if replacement.contains(['\n', '\r']) && !self.active_marks().is_empty() {
                return None;
            }
            let slice = if let Some(slice) = self.matching_style_slice(replacement) {
                slice
            } else {
                markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(
                    self.state
                        .schema()
                        .text_marked(replacement, self.active_marks()),
                ))
            };
            return Some(markraft_core::commands::replace_selection_changes(
                self.state.schema(),
                self.state.doc(),
                position,
                position,
                &slice,
            ));
        }
        let mapped = self.context_text_mapping(request)?;
        let range = mapped.replacement_range()?;
        if replacement == mapped.text {
            return None;
        }
        let verbatim = self
            .state
            .doc()
            .resolve(range.start)
            .is_ok_and(|resolved| self.types.is_verbatim(resolved.parent().type_id()));
        // A line break inside source-delimited emphasis/link text could strand
        // its closing delimiter in another paragraph. Only ordinary top-level
        // prose or verbatim blocks accept arbitrary new paragraphs here.
        if !verbatim
            && replacement.contains(['\n', '\r'])
            && mapped
                .source
                .iter()
                .flatten()
                .filter(|span| span.end == span.start + 1)
                .any(|span| {
                    let doc = self.state.doc();
                    !doc.resolve(span.start).is_ok_and(|resolved| {
                        resolved.depth() == 1
                            && Some(resolved.parent().type_id()) == self.types.paragraph
                    }) || doc
                        .node_at(span.start)
                        .is_none_or(|node| !node.marks().is_empty())
                })
        {
            return None;
        }
        Some(
            if let Some(codecs) = &self.codecs
                && !verbatim
            {
                markraft_core::commands::replace_selection_changes(
                    self.state.schema(),
                    self.state.doc(),
                    range.start,
                    range.end,
                    &codecs.from_text(replacement),
                )
            } else {
                let change = self.context_text_change(range, replacement)?;
                vec![change]
            },
        )
    }

    /// Transform reading characters in one guarded transaction while retaining
    /// each character's marks and every concealed source delimiter/destination.
    pub fn transform_context_text(
        &mut self,
        request: &ContextRequest,
        transformation: TextTransformation,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut changes = Vec::new();
        let Some(mapped) = self
            .context_text_mapping(request)
            .filter(|mapped| mapped.exact)
        else {
            return false;
        };
        let Some(replacements) = transformation.characters(&mapped.text) else {
            return false;
        };
        for ((original, source), replacement) in
            mapped.text.chars().zip(mapped.source).zip(replacements)
        {
            let Some(source) = source else { continue };
            if replacement == original.to_string() {
                continue;
            }
            let Some(change) = self.context_text_change(source, &replacement) else {
                return false;
            };
            changes.push(change);
        }
        !changes.is_empty()
            && self.dispatch_isolated(
                [TransactionSpec::new()
                    .changes(changes)
                    .user_event(event::INPUT_REPLACE)],
                cx,
            )
    }

    pub(super) fn context_text_change(
        &self,
        range: Range<usize>,
        text: &str,
    ) -> Option<markraft_core::Change> {
        use markraft_core::{Change, Fragment, Slice};
        let node = self.state.doc().node_at(range.start)?;
        if !node.is_text() {
            return None;
        }
        let slice = if text.is_empty() {
            Slice::empty()
        } else {
            Slice::from_fragment(Fragment::from_node(
                self.state.schema().text_marked(text, node.marks().clone()),
            ))
        };
        Some(Change::replace(range.start, range.end, slice))
    }

    /// Read text and its source provenance in one pass for native document
    /// mirrors. This uses the same concealment rules as text services.
    pub fn context_text_mapping(&self, request: &ContextRequest) -> Option<ContextTextMap> {
        use markraft_core::kind::{conceal::Reveal, reading};
        if !self.context_is_current(request) || self.is_composing() {
            return None;
        }
        let selected = request.text_range();
        if selected.is_empty() {
            return None;
        }
        let projection = self.analysis.projection();
        let mut mapped = ContextTextMap {
            text: String::new(),
            source: Vec::new(),
            exact: true,
        };
        let first = projection
            .lines()
            .partition_point(|line| line.to() < selected.start);
        let last = projection
            .lines()
            .partition_point(|line| line.from() <= selected.end);
        for (index, line) in projection.lines().iter().enumerate().take(last).skip(first) {
            for piece in reading::line_pieces(projection, &self.types, index, &Reveal::nothing()) {
                let from = line.offset_to_pos(piece.source.start)?;
                let to = line.offset_to_pos(piece.source.end)?;
                if to <= selected.start || from >= selected.end {
                    continue;
                }
                if !piece.own {
                    mapped.exact = false;
                    if from >= selected.start && to <= selected.end {
                        mapped
                            .source
                            .extend(std::iter::repeat_n(None, piece.text.chars().count()));
                        mapped.text.push_str(&piece.text);
                    }
                    continue;
                }
                let start = selected.start.saturating_sub(from);
                let end = (selected.end.min(to) - from).min(piece.text.chars().count());
                let mut boundary = 0;
                let mut boundaries = vec![0];
                for grapheme in piece.text.graphemes(true) {
                    boundary += grapheme.chars().count();
                    boundaries.push(boundary);
                }
                mapped.exact &= boundaries.contains(&start) && boundaries.contains(&end);
                for (offset, character) in piece.text.chars().enumerate().take(end).skip(start) {
                    let at = line.offset_to_pos(piece.source.start + offset)?;
                    let after = line.offset_to_pos(piece.source.start + offset + 1)?;
                    mapped.text.push(character);
                    mapped.source.push(Some(at..after));
                }
            }
            if let Some(next) = projection.line(index + 1)
                && line.to() >= selected.start
                && next.from() <= selected.end
            {
                mapped.text.push('\n');
                let plain_paragraph = |position| {
                    self.state.doc().resolve(position).is_ok_and(|resolved| {
                        resolved.depth() == 1
                            && Some(resolved.parent().type_id()) == self.types.paragraph
                    })
                };
                mapped.source.push(
                    (plain_paragraph(line.from()) && plain_paragraph(next.from()))
                        .then_some(line.to()..next.from()),
                );
            }
        }
        (!mapped.text.is_empty()).then_some(mapped)
    }
}
