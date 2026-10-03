//! Pointer context and capability queries shared by the host's editing menus.

use crate::{EditorEvent, EditorView, clipboard, links, wiki};
use gpui::{App, ClipboardEntry, ClipboardItem, Context, MouseDownEvent, Pixels, Point, Window};
use markraft_core::{Node, Selection, TransactionSpec, projection::Projection, protocol::event};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Reading text offered to native services, with explicit write-back capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextText {
    pub text: String,
    pub replaceable: bool,
    pub transformable: bool,
}

mod block_actions;
mod copy_formats;
mod transformations;
pub use transformations::TextTransformation;

/// Reading text with one source span per Unicode scalar. Nonliteral display
/// content has no span; paragraph separators can span structural tokens.
/// Consumers must retain the originating document snapshot and reject offsets
/// inside graphemes before using this mapping for a native write-back.
pub struct ContextTextMap {
    pub text: String,
    pub source: Vec<Option<Range<usize>>>,
    exact: bool,
}

impl ContextTextMap {
    fn replacement_range(&self) -> Option<Range<usize>> {
        if !self.exact || self.source.iter().any(Option::is_none) {
            return None;
        }
        let spans: Vec<_> = self.source.iter().flatten().collect();
        if spans.windows(2).any(|pair| pair[0].end != pair[1].start) {
            return None;
        }
        Some(spans.first()?.start..spans.last()?.end)
    }
}

/// The object actually drawn under the pointer, independent of the selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    Text,
    Link {
        url: String,
        range: Range<usize>,
    },
    WikiLink {
        pos: usize,
        target: String,
        embed: bool,
    },
    CodeBlock {
        pos: usize,
    },
    Table {
        pos: usize,
        cell: usize,
    },
    Image {
        pos: usize,
    },
    Math {
        pos: usize,
    },
}

/// Follow the clicked link through synchronous selection corrections. Changes
/// elsewhere may fold atoms and move it; changes to the link invalidate it.
pub(crate) struct ContextTargetBookmark {
    document: Node,
    target: ContextTarget,
}

impl ContextTargetBookmark {
    fn new(document: &Node, target: &ContextTarget) -> Option<Self> {
        matches!(
            target,
            ContextTarget::Link { .. } | ContextTarget::WikiLink { .. }
        )
        .then(|| Self {
            document: document.clone(),
            target: target.clone(),
        })
    }

    pub(crate) fn map(mut self, transactions: &[markraft_core::Transaction]) -> Option<Self> {
        for transaction in transactions {
            if !self.document.ptr_eq(transaction.start_state().doc()) {
                return None;
            }
            let range = match &self.target {
                ContextTarget::Link { range, .. } => range.clone(),
                ContextTarget::WikiLink { pos, .. } => *pos..*pos + 1,
                _ => return None,
            };
            if transaction.changes().iter_changes().iter().any(|change| {
                let (from, to) = match change {
                    markraft_core::ChangeRange::Replaced { from_a, to_a, .. }
                    | markraft_core::ChangeRange::Marked { from_a, to_a, .. } => (*from_a, *to_a),
                };
                from < range.end && to > range.start
                    || from == to && range.start < from && from < range.end
            }) {
                return None;
            }
            let mapped = transaction
                .changes()
                .desc()
                .map_range(range.start, range.end);
            let next = transaction.new_doc();
            if self.document.slice(range.start, range.end).ok()?
                != next.slice(mapped.from, mapped.to).ok()?
            {
                return None;
            }
            match &mut self.target {
                ContextTarget::Link { range, .. } => *range = mapped.from..mapped.to,
                ContextTarget::WikiLink { pos, .. } => *pos = mapped.from,
                _ => return None,
            }
            self.document = next.clone();
        }
        Some(self)
    }

    fn finish(self, document: &Node) -> Option<ContextTarget> {
        self.document.ptr_eq(document).then_some(self.target)
    }
}

/// A menu's immutable editing target. Hosts additionally validate editor and
/// document identity before executing a command against this snapshot.
#[derive(Clone, Debug)]
pub struct ContextRequest {
    pub position: Point<Pixels>,
    pub target: ContextTarget,
    document: Node,
    selection: Selection,
    epoch: u64,
    text_range: Option<Range<usize>>,
}

impl ContextRequest {
    pub fn selection_range(&self) -> Range<usize> {
        self.selection.from(&self.document)..self.selection.to(&self.document)
    }

    pub fn text_range(&self) -> Range<usize> {
        self.text_range
            .clone()
            .unwrap_or_else(|| self.selection_range())
    }
}

/// Structural capabilities, before the host applies its read-only policy.
/// Querying them neither dispatches transactions nor changes the clipboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditCapabilities {
    pub copy: bool,
    pub cut: bool,
    pub paste: bool,
    pub paste_plain: bool,
    pub paste_markdown: bool,
}

/// Selection commands executed synchronously by a validated context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextAction {
    Copy,
    Cut,
    Paste,
    PastePlain,
    PasteMatchStyle,
    PasteMarkdown,
}

enum ContextLinkChange {
    Existing,
    Add(TransactionSpec),
}

impl EditorView {
    /// Display host spelling/grammar diagnostics without changing document,
    /// selection, or history. Any subsequent document edit clears the ranges.
    pub fn set_text_diagnostics(&mut self, mut ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        let end = self.state.doc().content_size();
        ranges.retain(|range| !range.is_empty() && range.end <= end);
        ranges.sort_by_key(|range| (range.start, range.end));
        ranges.dedup();
        if ranges != self.text_diagnostics {
            self.text_diagnostics = ranges;
            cx.notify();
        }
    }

    /// Capture the whole reading document without moving the user's selection.
    pub fn context_document_snapshot(&self) -> ContextRequest {
        let mut request = self.context_snapshot(Point::default(), ContextTarget::Text);
        request.text_range = Some(0..self.state.doc().content_size());
        request
    }

    /// Capture a source range while retaining the selection used for stale
    /// result validation. Callers must supply positions in this document.
    pub fn context_source_range_snapshot(&self, range: Range<usize>) -> Option<ContextRequest> {
        if range.is_empty() || range.end > self.state.doc().content_size() || self.is_composing() {
            return None;
        }
        let mut request = self.context_snapshot(Point::default(), ContextTarget::Text);
        request.text_range = Some(range);
        Some(request)
    }

    /// The prose line currently receiving ordinary typing. A service can read
    /// it without moving the caret and narrow it before applying a correction.
    pub fn context_paragraph_snapshot_at_caret(&self) -> Option<ContextRequest> {
        if self.is_composing() || !self.state.selection().is_empty(self.state.doc()) {
            return None;
        }
        let projection = self.analysis.projection();
        let (index, _) = projection.pos_to_line_offset(self.head())?;
        let line = projection.line(index)?;
        if self.types.is_verbatim_block(line) {
            return None;
        }
        let mut request = self.context_snapshot(Point::default(), ContextTarget::Text);
        request.text_range = Some(line.from()..line.to());
        Some(request)
    }

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
        let replaceable = if let Some(command) = self
            .kind
            .replace_reading(request.text_range(), &mapped.text)
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

    fn dispatch_context_check_specs(
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

    fn context_link_spec(&self, request: &ContextRequest, url: &str) -> Option<ContextLinkChange> {
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
        if let Some(command) =
            self.kind
                .replace_reading_with_policy(request.text_range(), replacement, policy)
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

    fn context_insert_position(&self, request: &ContextRequest) -> Option<usize> {
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

    fn context_replacement_changes(
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

    fn context_text_change(
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

    /// Native smart spacing sees the rendered paragraph, while every boundary
    /// must map to contiguous literal source. Delimiters and atoms are barriers.
    fn smart_clipboard_context(&self) -> Option<crate::smart_clipboard::Context> {
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

    pub(crate) fn smart_copy_eligible(&self) -> bool {
        self.smart_clipboard_context()
            .is_some_and(|context| crate::smart_clipboard::whole_words(&context))
    }

    pub(crate) fn smart_delete_spec(&self) -> Option<TransactionSpec> {
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

    fn padded_paste_specs(
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

    /// Keep a menu edit separate from typing, including an extension's explicit
    /// undo group. The regular edit funnel still checks guards atomically.
    pub fn with_context_edit<R>(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> R {
        let previous = self.context_edit;
        self.context_edit = true;
        let result = edit(self, cx);
        self.context_edit = previous;
        result
    }

    pub fn execute_context_action(&mut self, action: ContextAction, cx: &mut Context<Self>) {
        self.with_context_edit(cx, |view, cx| match action {
            ContextAction::Copy => view.copy(cx),
            ContextAction::Cut => view.cut(cx),
            ContextAction::Paste => view.paste(clipboard::PasteMode::Formatted, cx),
            ContextAction::PastePlain => view.paste(clipboard::PasteMode::Plain, cx),
            ContextAction::PasteMatchStyle => view.paste_match_style(cx),
            ContextAction::PasteMarkdown => view.paste(clipboard::PasteMode::Markdown, cx),
        });
    }

    fn paste_match_style(&mut self, cx: &mut Context<Self>) {
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

    fn matching_style_slice(&self, text: &str) -> Option<markraft_core::Slice> {
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

    pub fn can_toggle_mark(&self, ty: markraft_core::MarkTypeId) -> bool {
        !self.single_line
            && !self.is_composing()
            && self.mark_command(ty, markraft_core::Attrs::empty())(&self.state)
                .is_ok_and(|spec| spec.is_some())
    }

    pub fn can_table(&self, op: crate::TableOp) -> bool {
        !self.is_composing()
            && self
                .table_operation(op)
                .is_some_and(|command| command(&self.state).is_some())
    }

    /// Capture a menu target after all pointer-selection extensions have run.
    pub fn context_snapshot(
        &self,
        position: Point<Pixels>,
        target: ContextTarget,
    ) -> ContextRequest {
        ContextRequest {
            position,
            target,
            document: self.state.doc().clone(),
            selection: self.state.selection().clone(),
            epoch: self.context_epoch,
            text_range: None,
        }
    }

    /// A selection move, composition update, document replacement or edit
    /// invalidates a menu, including changes subsequently undone.
    pub fn context_is_current(&self, request: &ContextRequest) -> bool {
        request.epoch == self.context_epoch
            && request.document.ptr_eq(self.state.doc())
            && request.selection == *self.state.selection()
    }

    /// Diagnostic work may survive selection movement, but never a document
    /// change or an active composition. Hosts still validate editor identity.
    pub fn context_document_is_current(&self, request: &ContextRequest) -> bool {
        request.document.ptr_eq(self.state.doc()) && !self.is_composing()
    }

    pub fn edit_capabilities(&self, cx: &App) -> EditCapabilities {
        let item = cx
            .read_from_clipboard()
            .unwrap_or_else(|| ClipboardItem::new_string(String::new()));
        let text = item.text().is_some_and(|text| !text.is_empty());
        let files = self.file_paste
            && item.entries().iter().any(|entry| {
                matches!(
                    entry,
                    ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
                )
            });
        let rich = !self.single_line && !self.types.in_verbatim_block_at(&self.state);
        let formatted = rich
            && self.codecs.as_ref().is_some_and(|codecs| {
                clipboard::read_fragment(
                    self.state.schema(),
                    codecs.as_ref(),
                    &item,
                    clipboard::PasteMode::Formatted,
                    cx,
                )
                .is_some_and(|slice| !slice.is_empty())
            });
        let selected = !self.selection_slice().is_empty();
        EditCapabilities {
            copy: selected,
            cut: selected,
            paste: text || files || formatted,
            paste_plain: text,
            paste_markdown: text || files,
        }
    }

    /// The literal text of a particular code block, without relocating the caret.
    pub fn code_text_at(&self, pos: usize) -> Option<&str> {
        let projection = self.analysis.projection();
        let index = projection
            .lines()
            .iter()
            .position(|line| self.types.is_code_block(line) && line.block_before() == Some(pos))?;
        projection.line_text(index)
    }

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

    fn select_context(
        &mut self,
        position: usize,
        character: Option<usize>,
        inside: bool,
        target: &ContextTarget,
        cx: &mut Context<Self>,
    ) {
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
    }

    fn context_link_label(&self, range: &Range<usize>) -> Option<Range<usize>> {
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

    fn context_link_range(&self, range: Range<usize>, position: usize) -> Range<usize> {
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
    pub(super) fn pointer_word_at(&self, point: Point<Pixels>) -> Option<Selection> {
        self.pointer_character_at(point).and_then(|position| {
            self.word_at_character(position, crate::word_boundary::Intent::Pointer)
        })
    }

    fn word_at_character(
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
                Selection::reading(range.start, range.end, syntax)
            } else {
                Selection::text(range.start, range.end)
            },
        )
    }

    fn pointer_character_at(&self, point: Point<Pixels>) -> Option<usize> {
        if self.frame.rows().is_empty() {
            return None;
        }
        let (row, local) = self.row_under(point);
        let offset = row.char_at(local);
        pointer_character_in_row(row, self.analysis.projection(), point, offset)
    }

    fn context_target_at(&self, point: Point<Pixels>, position: usize) -> ContextTarget {
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

#[cfg(test)]
mod pointer_targets;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::Setup;
    use gpui::{AppContext, Entity, MouseButton, TestAppContext, VisualTestContext, point, px};
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    };
    use markraft_core::{commands, composition, history, kind::DocTypes};

    fn setup(source: &str) -> Setup {
        let schema = commonmark_schema();
        Setup::new(schema.clone())
            .types(DocTypes::from_schema_names(
                &schema,
                &commonmark_doc_type_names(),
            ))
            .extensions(commonmark_extensions(&schema))
            .doc(from_markdown(&schema, source).unwrap())
    }

    struct ReadingKind;

    impl crate::DocumentKind for ReadingKind {
        fn replace_reading(&self, range: Range<usize>, text: &str) -> Option<crate::Formatting> {
            let command = markraft_commonmark::Formatter::new(Default::default())
                .replace_reading(range, text);
            Some(std::sync::Arc::new(move |state| {
                command(state).map_err(|error| error.to_string())
            }))
        }

        fn replace_reading_with_policy(
            &self,
            range: Range<usize>,
            text: &str,
            policy: markraft_core::kind::ReadingReplacementPolicy,
        ) -> Option<crate::Formatting> {
            let command = markraft_commonmark::Formatter::new(Default::default())
                .replace_reading_with_policy(range, text, policy);
            Some(std::sync::Arc::new(move |state| {
                command(state).map_err(|error| error.to_string())
            }))
        }
    }

    fn range(view: &EditorView) -> Range<usize> {
        view.state.selection().from(view.state.doc())..view.state.selection().to(view.state.doc())
    }

    fn click_character(
        view: &Entity<EditorView>,
        cx: &mut VisualTestContext,
        offset: usize,
        control: bool,
    ) {
        let position = view.read_with(cx, |view, _| {
            view.frame.rows()[0].rectangles(offset..offset + 1, false)[0].center()
        });
        cx.simulate_mouse_down(
            position,
            if control {
                MouseButton::Left
            } else {
                MouseButton::Right
            },
            gpui::Modifiers {
                control,
                ..Default::default()
            },
        );
        cx.run_until_parked();
    }

    #[gpui::test]
    fn smart_clipboard_maps_prose_and_excludes_delimiters_and_code(cx: &mut TestAppContext) {
        let view = cx.new(|cx| {
            EditorView::new(setup("one **two** three"), cx).with_smart_insert_delete(true)
        });
        view.update(cx, |view, cx| {
            view.select_range(7, 10, cx);
            let context = view.smart_clipboard_context().unwrap();
            assert_eq!(context.text, "one two three");
            assert_eq!(context.selection, 4..7);
            view.select_range(5, 12, cx);
            assert!(view.smart_clipboard_context().is_none());
            view.set_smart_insert_delete(false);
            view.select_range(1, 4, cx);
            assert!(view.smart_clipboard_context().is_none());
        });
        let code = cx
            .new(|cx| EditorView::new(setup("one `two` three"), cx).with_smart_insert_delete(true));
        code.update(cx, |view, cx| {
            view.select_range(6, 9, cx);
            assert!(view.smart_clipboard_context().is_none());
        });
    }

    #[gpui::test]
    fn smart_padding_is_atomic_and_one_undo(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup("one three"), cx));
        view.update(cx, |view, cx| {
            view.select(4, false, cx);
            let spec = commands::insert_text("two")(&view.state).unwrap();
            let specs = view.padded_paste_specs(spec, 4..7, " ", "");
            assert!(view.dispatch_isolated(specs, cx));
            assert_eq!(view.projection().plain_text(), "one two three");
            assert_eq!(history::undo_depth(&view.state), 1);
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "one three");
            view.document_guard = Some(Box::new(|_| {
                Err(crate::EditRejection::Refused("read only".into()))
            }));
            let spec = commands::insert_text("two")(&view.state).unwrap();
            let specs = view.padded_paste_specs(spec, 4..7, " ", "");
            assert!(!view.dispatch_isolated(specs, cx));
            assert_eq!(view.projection().plain_text(), "one three");
        });
    }

    #[gpui::test]
    fn return_only_services_support_empty_paragraphs_and_preserve_enclosing_style(
        cx: &mut TestAppContext,
    ) {
        for (source, position, expected) in
            [("", 1, "inserted"), ("**word**", 5, "**woinsertedrd**")]
        {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.update(cx, |view, cx| {
                view.codecs = Some(std::sync::Arc::new(
                    markraft_commonmark::CommonMarkCodecs::new(
                        view.state.schema().clone(),
                        Default::default(),
                    ),
                ));
                view.select(position, false, cx);
                let request = view.context_snapshot(Point::default(), ContextTarget::Text);
                assert!(view.context_text(&request).unwrap().replaceable);
                assert!(view.replace_context_text(&request, "inserted", cx));
                assert_eq!(view.projection().plain_text(), expected);
            });
        }
    }

    #[gpui::test]
    fn return_only_services_insert_literal_text_at_caret_and_reject_stale_or_readonly(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| EditorView::new(setup("before after"), cx));
        view.update(cx, |view, cx| {
            view.codecs = Some(std::sync::Arc::new(
                markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    Default::default(),
                ),
            ));
            view.select(8, false, cx);
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            assert_eq!(view.context_text(&request).unwrap().text, "");
            assert!(view.replace_context_text(&request, "*literal* ", cx));
            assert_eq!(
                view.context_text(&view.context_document_snapshot())
                    .unwrap()
                    .text,
                "before *literal* after"
            );
            assert!(!view.replace_context_text(&request, "stale", cx));
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "before after");
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            view.document_guard = Some(Box::new(|_| {
                Err(crate::EditRejection::Refused("read only".into()))
            }));
            assert!(!view.replace_context_text(&request, "blocked", cx));
            assert_eq!(view.projection().plain_text(), "before after");
        });
    }

    #[gpui::test]
    fn right_click_preserves_selection_or_selects_clicked_word(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("one two three"), cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| view.select_range(1, 8, cx));
        cx.run_until_parked();
        click_character(&view, cx, 5, false);
        view.read_with(cx, |view, _| {
            assert_eq!(range(view), 1..8);
            assert!(!view.selecting);
        });
        click_character(&view, cx, 10, true);
        view.read_with(cx, |view, _| {
            assert_eq!(range(view), 9..14);
            assert!(!view.selecting);
        });
        let blank = view.read_with(cx, |view, _| {
            let row = &view.frame.rows()[0];
            let last = row.rectangles(row.char_len - 1..row.char_len, false)[0];
            point(last.right() + px(15.), last.center().y)
        });
        cx.simulate_mouse_down(blank, MouseButton::Right, Default::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(range(view), 14..14));
    }

    #[gpui::test]
    fn right_click_word_ranges_keep_contextual_dictionary_and_whitespace(cx: &mut TestAppContext) {
        for (source, offset, expected) in [
            ("one   two", 4, "   "),
            ("one...two", 4, "."),
            #[cfg(target_os = "macos")]
            ("中文编辑器测试", 2, "编辑"),
        ] {
            let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(source), cx));
            cx.run_until_parked();
            click_character(&view, cx, offset, false);
            view.read_with(cx, |view, _| {
                let range = range(view);
                assert_eq!(
                    view.projection().text_between(range.start, range.end),
                    Some(expected)
                );
            });
        }
    }

    #[gpui::test]
    fn right_click_selects_the_entire_link_label_but_preserves_existing_selection(
        cx: &mut TestAppContext,
    ) {
        let source = "[sample **bold** link](https://example.com) tail";
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(source), cx));
        cx.run_until_parked();
        let expected = 2..source.find(']').unwrap() + 1;
        click_character(&view, cx, 3, false);
        view.read_with(cx, |view, _| assert_eq!(range(view), expected));

        // Right-clicking within a partial label selection must not expand it.
        view.update(cx, |view, cx| view.select_range(2, 8, cx));
        cx.run_until_parked();
        click_character(&view, cx, 3, false);
        view.read_with(cx, |view, _| assert_eq!(range(view), 2..8));

        // A reversed selection spanning the link and surrounding text is also
        // preserved, including its anchor direction.
        let end = source.chars().count() + 1;
        view.update(cx, |view, cx| view.select_range(end, 2, cx));
        cx.run_until_parked();
        let selected = view.read_with(cx, |view, _| view.state.selection().clone());
        click_character(&view, cx, 3, false);
        view.read_with(cx, |view, _| assert_eq!(view.state.selection(), &selected));

        // Control-click outside the old selection uses the same link policy.
        view.update(cx, |view, cx| view.select_range(end - 4, end, cx));
        cx.run_until_parked();
        click_character(&view, cx, 3, true);
        view.read_with(cx, |view, _| assert_eq!(range(view), expected));
    }

    #[gpui::test]
    fn link_labels_exclude_destinations_and_resolve_character_positions(cx: &mut TestAppContext) {
        for (source, position, expected) in [
            ("[中文 标签](https://example.com)", 3, 2..7),
            ("[**bold**](https://example.com)", 5, 4..8),
            ("<https://example.com>", 5, 2..21),
            ("https://example.com", 5, 1..20),
            ("[a \\* b](https://example.com)", 2, 2..8),
            ("[sample link][ref]\n\n[ref]: https://example.com", 3, 2..13),
            (
                "[one](https://example.com)[two](https://example.com)",
                3,
                2..5,
            ),
            ("[one](https://example.com)https://example.com", 3, 2..5),
            ("[one](https://example.com)https://example.com", 27, 27..46),
        ] {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.read_with(cx, |view, _| {
                let (range, _) =
                    links::link_at(view.state.doc(), view.types.link.unwrap(), position)
                        .expect("a link at the test position");
                let range = view.context_link_range(range, position);
                assert_eq!(view.context_link_label(&range), Some(expected), "{source}");
            });
        }
    }

    #[gpui::test]
    fn right_click_on_task_marker_does_not_toggle_or_start_drag(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("- [ ] task"), cx));
        cx.run_until_parked();
        let (position, doc) = view.read_with(cx, |view, _| {
            (
                view.frame.rows()[0].task_marker().unwrap().1.center(),
                view.state.doc().clone(),
            )
        });
        cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.state.doc(), &doc);
            assert!(!view.selecting);
        });
    }

    #[gpui::test]
    fn right_click_on_link_does_not_emit_navigation(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::new(setup("[link](https://example.com) tail"), cx)
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event, _| {
                seen.borrow_mut().push(event.clone())
            })
            .detach()
        });
        cx.run_until_parked();
        let position = view.read_with(cx, |view, _| {
            let row = &view.frame.rows()[0];
            row.rectangles(2..3, false)[0].center()
        });
        cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
        cx.run_until_parked();
        assert!(!events.borrow().iter().any(|event| matches!(
            event,
            EditorEvent::LinkClicked | EditorEvent::WikiLinkClicked { .. }
        )));
        assert!(events.borrow().iter().any(|event| matches!(
            event,
            EditorEvent::ContextMenuRequested(ContextRequest {
                target: ContextTarget::Link { .. },
                ..
            })
        )));
    }

    #[gpui::test]
    fn table_context_keeps_clicked_cell_distinct_from_selection_head(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::new(setup("| first | second |\n| --- | --- |\n| a | b |"), cx)
        });
        cx.run_until_parked();
        let (position, expected_cell) = view.update(cx, |view, cx| {
            let rows = view.frame.rows();
            let first = rows
                .iter()
                .find(|row| {
                    row.table
                        .is_some_and(|cell| cell.row == 0 && cell.column == 0)
                })
                .unwrap();
            let second = rows
                .iter()
                .find(|row| {
                    row.table
                        .is_some_and(|cell| cell.row == 0 && cell.column == 1)
                })
                .unwrap();
            let from = first.from;
            let to = second.offset_to_pos(second.char_len);
            let position = first.rectangles(1..2, false)[0].center();
            let target = view.context_target_at(position, first.offset_to_pos(1));
            let ContextTarget::Table { cell, .. } = target else {
                panic!("expected a table target")
            };
            view.select_range(from, to, cx);
            (position, cell)
        });
        cx.run_until_parked();
        let before = view.read_with(cx, |view, _| range(view));
        let received = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = received.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let EditorEvent::ContextMenuRequested(request) = event {
                    *seen.borrow_mut() = Some(request.clone());
                }
            })
            .detach()
        });
        cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
        cx.run_until_parked();
        let request = received.borrow().clone().unwrap();
        assert_eq!(request.selection_range(), before);
        assert!(
            matches!(request.target, ContextTarget::Table { cell, .. } if cell == expected_cell)
        );
        view.read_with(cx, |view, _| {
            let head = commands::cell_at(view.types.table_types().unwrap(), view.state()).unwrap();
            assert_ne!(head.cell, expected_cell);
        });
    }

    #[gpui::test]
    fn menu_snapshot_captures_selection_after_extensions_settle(cx: &mut TestAppContext) {
        struct NormalizePointer;
        impl crate::Extension for NormalizePointer {
            fn id(&self) -> &'static str {
                "normalize-pointer"
            }
            fn update(&mut self, update: &crate::Update, cx: &mut crate::EditorCx<'_>) {
                if update.is_user_event(event::SELECT_POINTER) {
                    cx.select(Selection::text(1, 4), false);
                }
            }
        }
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("one two three"), cx));
        let _extension = view.update(cx, |view, cx| view.add_extension(NormalizePointer, cx));
        let received = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = received.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let EditorEvent::ContextMenuRequested(request) = event {
                    *seen.borrow_mut() = Some(request.clone());
                }
            })
            .detach()
        });
        cx.run_until_parked();
        click_character(&view, cx, 10, false);
        let request = received.borrow().clone().unwrap();
        assert_eq!(request.selection_range(), 1..4);
        view.read_with(cx, |view, _| assert!(view.context_is_current(&request)));
    }

    #[test]
    fn word_ranges_map_display_scalars_and_include_whitespace() {
        let source = "naïve café 中文 👩‍💻";
        let setup = setup(source);
        let projection = Projection::of(setup.doc.as_ref().unwrap(), &setup.schema);
        let pieces = crate::shown::line_pieces(
            &projection,
            &setup.types,
            0,
            &markraft_core::kind::conceal::Reveal::nothing(),
        );
        assert_eq!(
            word_at(
                &projection,
                3,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(1..6)
        );
        assert_eq!(
            word_at(
                &projection,
                8,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(7..11)
        );
        assert_eq!(
            word_at(
                &projection,
                6,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(6..7)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            word_at(
                &projection,
                12,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(12..14)
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            word_at(
                &projection,
                12,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(12..13)
        );
        assert_eq!(
            word_at(
                &projection,
                16,
                &pieces,
                crate::word_boundary::Intent::Context
            ),
            Some(15..18)
        );
    }

    #[test]
    fn pointer_words_follow_concealment_and_preserve_source_provenance() {
        use markraft_core::kind::conceal::Reveal;

        let resolve = |source: &str, target: &str, revealed: bool| {
            let setup = setup(source);
            let projection = Projection::of(setup.doc.as_ref().unwrap(), &setup.schema);
            let line = &projection.lines()[0];
            let source = projection.line_text(0).unwrap();
            let offset = source[..source.find(target).unwrap()].chars().count();
            let position = line.offset_to_pos(offset).unwrap();
            let reveal = if revealed {
                Reveal::at(line.from()..line.to(), None)
            } else {
                Reveal::nothing()
            };
            let pieces = crate::shown::line_pieces(&projection, &setup.types, 0, &reveal);
            word_at(
                &projection,
                position,
                &pieces,
                crate::word_boundary::Intent::Pointer,
            )
            .map(|range| {
                projection
                    .text_between(range.start, range.end)
                    .unwrap()
                    .to_owned()
            })
        };
        assert_eq!(resolve("a&#98;c", "a", false).as_deref(), Some("a&#98;c"));
        assert_eq!(resolve("a&#98;c", "&", false).as_deref(), Some("a&#98;c"));
        assert_eq!(resolve("a&#98;c", "&", true).as_deref(), Some("&"));
        assert_eq!(resolve("**word**", "*", false), None);
        assert_eq!(resolve("**word**", "*", true).as_deref(), Some("*"));
        assert_eq!(resolve("a[[Notes]]b", "a", false).as_deref(), Some("a"));
        assert_eq!(resolve("a[[Notes]]b", "b", false).as_deref(), Some("b"));
        assert_eq!(resolve("a[[Notes]]b", "\u{fffc}", false), None);
        #[cfg(target_os = "macos")]
        assert_eq!(resolve("宇**宙**", "宙", false).as_deref(), Some("宇**宙"));
    }

    #[gpui::test]
    fn context_commits_composition_without_discarding_candidate(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup(""), cx));
        view.update(cx, |view, cx| {
            let candidate = composition::update_composition(&view.state, "中文", 2).unwrap();
            view.dispatch([candidate], cx);
            assert!(view.is_composing());
            let candidate_doc = view.state.doc().clone();
            view.select_context(1, Some(1), false, &ContextTarget::Text, cx);
            assert!(!view.is_composing());
            assert_eq!(view.state.doc(), &candidate_doc);
            assert_eq!(view.committed_document(), &candidate_doc);
            assert!(view.context_is_current(
                &view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text)
            ));
        });
    }

    #[gpui::test]
    fn menu_snapshot_expires_after_selection_composition_undo_and_replacement(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| EditorView::new(setup("hello"), cx));
        view.update(cx, |view, cx| {
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            view.select(2, false, cx);
            view.select(1, false, cx);
            assert!(!view.context_is_current(&snapshot));
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            view.run_command(&commands::insert_text("x"), cx);
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert!(!view.context_is_current(&snapshot));
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
            view.dispatch([candidate], cx);
            assert!(!view.context_is_current(&snapshot));
            view.cancel_composition(cx);
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            view.replace_doc(view.state.doc().clone(), cx);
            assert!(!view.context_is_current(&snapshot));
        });
    }

    #[gpui::test]
    fn capabilities_are_read_only_and_keep_file_pastes_available(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup("hello"), cx));
        view.update(cx, |view, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            assert_eq!(view.edit_capabilities(cx), EditCapabilities::default());
            assert!(view.context_is_current(&snapshot));
            view.begin_undo_group();
            view.end_undo_group();
            assert!(
                view.context_is_current(&snapshot),
                "history boundaries do not move a menu's editing target"
            );
            view.select_range(1, 6, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("text".into()));
            let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
            let capabilities = view.edit_capabilities(cx);
            assert!(
                capabilities.copy
                    && capabilities.cut
                    && capabilities.paste
                    && capabilities.paste_plain
                    && capabilities.paste_markdown
            );
            assert!(view.context_is_current(&snapshot));
            view.file_paste = true;
            cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths(vec!["/tmp/image.png".into()].into()),
            )));
            let capabilities = view.edit_capabilities(cx);
            assert!(capabilities.paste && capabilities.paste_markdown);
            assert!(
                capabilities.paste_plain,
                "external paths also carry their path as text"
            );
            cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::empty()));
            let capabilities = view.edit_capabilities(cx);
            assert!(capabilities.paste && capabilities.paste_markdown);
            assert!(!capabilities.paste_plain);
        });
    }

    #[gpui::test]
    fn context_paste_is_its_own_undo_step_inside_explicit_typing_group(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup(""), cx));
        view.update(cx, |view, cx| {
            view.begin_undo_group();
            view.run_command(&commands::insert_text("before"), cx);
            cx.write_to_clipboard(ClipboardItem::new_string("paste".into()));
            view.execute_context_action(ContextAction::PastePlain, cx);
            view.run_command(&commands::insert_text("after"), cx);
            view.end_undo_group();
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "beforepaste");
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "before");
        });
    }

    #[gpui::test]
    fn source_preserving_services_support_rich_multiline_replacement_and_atomic_undo(
        cx: &mut TestAppContext,
    ) {
        let source = "**hello** _world_\n\nSecond paragraph.";
        let view =
            cx.new(|cx| EditorView::new(setup(source).kind(std::sync::Arc::new(ReadingKind)), cx));
        view.update(cx, |view, cx| {
            let before = view.state.doc().clone();
            let request = view.context_document_snapshot();
            assert!(view.context_text(&request).unwrap().replaceable);
            let result = "greetings world\nNew 😀 paragraph\nSecond paragraph.";
            assert!(view.replace_context_text(&request, result, cx));
            assert_eq!(
                view.context_text(&view.context_document_snapshot())
                    .unwrap()
                    .text,
                result
            );
            assert!(!view.replace_context_text(&request, "stale", cx));
            assert_eq!(history::undo_depth(&view.state), 1);
            view.dispatch([history::undo(&view.state).unwrap()], cx);
            assert_eq!(*view.state.doc(), before);
            let request = view.context_document_snapshot();
            view.document_guard = Some(Box::new(|_| {
                Err(crate::EditRejection::Protected("Locked".into()))
            }));
            assert!(!view.replace_context_text(&request, result, cx));
            assert_eq!(*view.state.doc(), before);
        });
    }

    #[gpui::test]
    fn source_preserving_services_preserve_unselected_atoms_for_selection_and_caret(
        cx: &mut TestAppContext,
    ) {
        let source = "hello ![image](x.png)";
        for (from, to, replacement, expected) in [
            (1, 6, "hi", "hi ![image](x.png)"),
            (1, 1, "before ", "before hello ![image](x.png)"),
            (6, 6, " after", "hello after ![image](x.png)"),
        ] {
            let view = cx.new(|cx| {
                EditorView::new(setup(source).kind(std::sync::Arc::new(ReadingKind)), cx)
            });
            view.update(cx, |view, cx| {
                view.select_range(from, to, cx);
                let request = view.context_snapshot(Default::default(), ContextTarget::Text);
                assert!(view.context_text(&request).unwrap().replaceable);
                assert!(view.replace_context_text(&request, replacement, cx));
                let codecs = markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    Default::default(),
                );
                let written = markraft_core::kind::Codecs::to_markup(
                    &codecs,
                    &markraft_core::Slice::from_fragment(view.state.doc().content().clone()),
                )
                .unwrap();
                assert_eq!(written, expected);
                let request = view.context_document_snapshot();
                assert!(!view.context_text(&request).unwrap().replaceable);
                assert!(!view.replace_context_text(&request, "cannot drop image", cx));
            });
        }
    }

    #[gpui::test]
    fn plain_service_policy_applies_same_text_style_changes_with_guarded_atomic_undo(
        cx: &mut TestAppContext,
    ) {
        use markraft_core::kind::ReadingReplacementPolicy::InheritSelectionStart;
        let view = cx.new(|cx| {
            EditorView::new(
                setup("**red** _blue_").kind(std::sync::Arc::new(ReadingKind)),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            let before = view.state.doc().clone();
            let request = view.context_document_snapshot();
            assert!(view.replace_context_text_with_policy(
                &request,
                "red blue",
                InheritSelectionStart,
                cx
            ));
            assert_eq!(view.projection().plain_text(), "**red blue**");
            assert_eq!(history::undo_depth(&view.state), 1);
            assert!(!view.replace_context_text_with_policy(
                &request,
                "stale",
                InheritSelectionStart,
                cx
            ));
            view.dispatch([history::undo(&view.state).unwrap()], cx);
            assert_eq!(*view.state.doc(), before);
            let request = view.context_document_snapshot();
            view.document_guard = Some(Box::new(|_| {
                Err(crate::EditRejection::Protected("Locked".into()))
            }));
            assert!(!view.replace_context_text_with_policy(
                &request,
                "red blue",
                InheritSelectionStart,
                cx
            ));
            assert_eq!(*view.state.doc(), before);
        });
    }

    #[gpui::test]
    fn source_preserving_services_keep_the_existing_verbatim_replacement_path(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| {
            EditorView::new(
                setup("```\nfirst line\nsecond line\n```").kind(std::sync::Arc::new(ReadingKind)),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            let request = view.context_document_snapshot();
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(view.replace_context_text_with_policy(
                &request,
                "literal **code**\nwith 😀",
                markraft_core::kind::ReadingReplacementPolicy::InheritSelectionStart,
                cx,
            ));
            assert_eq!(
                view.context_text(&view.context_document_snapshot())
                    .unwrap()
                    .text,
                "literal **code**\nwith 😀"
            );
        });
    }

    #[gpui::test]
    fn source_preserving_service_capability_rejects_protected_code_and_composition(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| {
            EditorView::new(
                setup("plain `code` text").kind(std::sync::Arc::new(ReadingKind)),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            let request = view.context_document_snapshot();
            assert!(!view.context_text(&request).unwrap().replaceable);
            assert!(!view.replace_context_text(&request, "replacement", cx));
            view.select_range(1, 6, cx);
            let request = view.context_snapshot(Default::default(), ContextTarget::Text);
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(view.replace_context_text(&request, "updated", cx));
            assert!(view.projection().plain_text().contains("`code`"));
            view.select(1, false, cx);
            let request = view.context_document_snapshot();
            view.dispatch(
                [composition::update_composition(&view.state, "中", 1).unwrap()],
                cx,
            );
            assert!(!view.replace_context_text(&request, "blocked", cx));
        });
    }

    #[gpui::test]
    fn text_service_reading_hides_markdown_and_maps_document_subranges(cx: &mut TestAppContext) {
        let source = "A **bold** [label](https://example.com)\n\n中文 café";
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.update(cx, |view, cx| {
            let request = view.context_document_snapshot();
            let text = view.context_text(&request).unwrap();
            assert_eq!(text.text, "A bold label\n中文 café");
            assert!(!text.replaceable);
            assert!(text.transformable);
            let label = view.context_text_range(&request, 7..12).unwrap();
            assert_eq!(view.context_text(&label).unwrap().text, "label");
            assert!(view.replace_context_text(&label, "title", cx));
            assert!(
                view.projection()
                    .plain_text()
                    .contains("[title](https://example.com)")
            );
            assert!(!view.replace_context_text(&label, "stale", cx));
        });
    }

    #[gpui::test]
    fn text_service_reading_preserves_inline_breaks_before_styles(cx: &mut TestAppContext) {
        for source in ["one\n**two**", "one  \ntwo", "one\r\ntwo"] {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.update(cx, |view, _| {
                let request = view.context_document_snapshot();
                let mapped = view.context_text_mapping(&request).unwrap();
                assert_eq!(mapped.text, "one\ntwo", "{source:?}");
                let newline = source.replace("\r\n", "\n").find('\n').unwrap() + 1;
                assert_eq!(mapped.source[3], Some(newline..newline + 1));
                let word = view.context_text_range(&request, 4..7).unwrap();
                assert_eq!(view.context_text(&word).unwrap().text, "two");
            });
        }
    }

    #[gpui::test]
    fn text_service_replacement_preserves_formatting_and_has_its_own_undo_step(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
        view.update(cx, |view, cx| {
            view.begin_undo_group();
            view.select_range(3, 7, cx);
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(!view.replace_context_text(&request, "broken\nformatting", cx));
            assert!(view.context_is_current(&request));
            assert!(view.replace_context_text(&request, "replacement", cx));
            assert_eq!(view.projection().plain_text(), "**replacement**");
            view.select(view.state.doc().content_size() - 1, false, cx);
            view.run_command(&commands::insert_text(" later"), cx);
            view.end_undo_group();
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "**replacement**");
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "**word**");
        });
    }

    #[gpui::test]
    fn text_service_transform_is_atomic_and_preserves_source_syntax(cx: &mut TestAppContext) {
        let source = "**straße** [mixed CASE](https://example.com/KeepCase)\n\nélÈVE ΟΣ";
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.update(cx, |view, cx| {
            let before = view.state.doc().clone();
            let request = view.context_document_snapshot();
            assert!(view.transform_context_text(&request, TextTransformation::Uppercase, cx));
            let text = view.projection().plain_text().to_owned();
            assert!(
                text.contains("**STRASSE** [MIXED CASE](https://example.com/KeepCase)"),
                "{text}"
            );
            assert!(text.contains("ÉLÈVE ΟΣ"));
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.state.doc(), &before);
            let request = view.context_document_snapshot();
            assert!(view.transform_context_text(&request, TextTransformation::Capitalize, cx));
            let text = view.projection().plain_text().to_owned();
            assert!(
                text.contains("**Straße** [Mixed Case](https://example.com/KeepCase)"),
                "{text}"
            );
            assert!(text.contains("Élève Ος"), "{text}");
        });
    }

    #[gpui::test]
    fn text_service_refuses_composite_grapheme_and_guarded_replacement(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup("a **b** [c](https://example.com)"), cx));
        view.update(cx, |view, cx| {
            let before = view.state.doc().clone();
            let request = view.context_document_snapshot();
            assert!(!view.replace_context_text(&request, "replacement", cx));
            view.document_guard = Some(Box::new(|_| {
                Err(crate::EditRejection::Protected("Read only".into()))
            }));
            assert!(!view.transform_context_text(&request, TextTransformation::Uppercase, cx));
            assert_eq!(view.state.doc(), &before);
            assert!(view.context_is_current(&request));
            assert_eq!(history::undo_depth(&view.state), 0);
        });
        let view = cx.new(|cx| EditorView::new(setup("e\u{301} &amp; ![picture](image.png)"), cx));
        view.update(cx, |view, cx| {
            let request = view.context_document_snapshot();
            let text = view.context_text(&request).unwrap();
            assert_eq!(text.text, "e\u{301} & picture");
            assert!(!text.transformable);
            assert!(view.context_text_range(&request, 0..1).is_none());
            assert!(view.context_text_range(&request, 3..4).is_none());
            assert!(view.context_text_range(&request, 5..12).is_none());
            let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
            view.dispatch([candidate], cx);
            assert!(
                view.context_text(&view.context_document_snapshot())
                    .is_none()
            );
        });
    }
    #[gpui::test]
    fn text_service_rewrites_plain_paragraphs_with_literal_newlines(cx: &mut TestAppContext) {
        let view = cx.new(|cx| EditorView::new(setup("first\n\nsecond"), cx));
        view.update(cx, |view, cx| {
            view.codecs = Some(std::sync::Arc::new(
                markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    markraft_commonmark::HouseStyleHandle::default(),
                ),
            ));
            let before = view.state.doc().clone();
            let request = view.context_document_snapshot();
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(view.replace_context_text(&request, "*literal*\n# text", cx));
            assert_eq!(
                view.context_text(&view.context_document_snapshot())
                    .unwrap()
                    .text,
                "*literal*\n# text"
            );
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.state.doc(), &before);
        });
    }

    #[gpui::test]
    fn text_diagnostics_survive_selection_but_clear_after_document_changes(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| EditorView::new(setup("hello world"), cx));
        view.update(cx, |view, cx| {
            let request = view.context_document_snapshot();
            view.set_text_diagnostics(vec![7..12, 1..6, 7..12, 0..999, 4..4], cx);
            assert_eq!(view.text_diagnostics, vec![1..6, 7..12]);
            assert_eq!(history::undo_depth(&view.state), 0);
            view.select(3, false, cx);
            assert!(view.context_document_is_current(&request));
            assert!(!view.context_is_current(&request));
            assert_eq!(view.text_diagnostics, vec![1..6, 7..12]);
            view.run_command(&commands::insert_text("x"), cx);
            assert!(!view.context_document_is_current(&request));
            assert!(view.text_diagnostics.is_empty());
            view.set_text_diagnostics(std::iter::once(1..4).collect(), cx);
            view.replace_doc(view.state.doc().clone(), cx);
            assert!(view.text_diagnostics.is_empty());
        });
    }
    #[gpui::test]
    fn prose_services_skip_code_math_and_leave_url_policy_to_host(cx: &mut TestAppContext) {
        for (source, word, expected) in [
            ("plain words", "plain", true),
            ("`misspelled`", "misspelled", false),
            ("```text\nmisspelled\n```", "misspelled", false),
            ("$misspelled$", "misspelled", false),
            ("https://example.com", "example", true),
            ("[misspelled](https://example.com)", "misspelled", true),
        ] {
            let view = cx.new(|cx| EditorView::new(setup(source), cx));
            view.read_with(cx, |view, _| {
                let whole = view.context_document_snapshot();
                let text = view.context_text(&whole).unwrap().text;
                let start = text[..text.find(word).unwrap()].chars().count();
                let request = view
                    .context_text_range(&whole, start..start + word.chars().count())
                    .unwrap();
                assert_eq!(view.context_is_prose(&request), expected, "{source}");
            });
        }
    }

    #[gpui::test]
    fn committed_typing_event_excludes_rewrites_paste_undo_and_uncommitted_ime(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| EditorView::new(setup(""), cx));
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events = seen.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let EditorEvent::Changed { text_input, .. } = event {
                    events.borrow_mut().push(*text_input);
                }
            })
            .detach()
        });
        view.update(cx, |view, cx| {
            view.run_command(&commands::insert_text("typed"), cx);
            let request = view.context_document_snapshot();
            view.replace_context_text(&request, "replaced", cx);
            cx.write_to_clipboard(ClipboardItem::new_string("paste".into()));
            view.execute_context_action(ContextAction::PastePlain, cx);
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
            view.dispatch([candidate], cx);
            view.dispatch([composition::finish_composition()], cx);
        });
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), vec![true, false, false, false, false, true]);
    }
    #[gpui::test]
    fn paste_match_style_keeps_destination_format_without_duplicate_source(
        cx: &mut TestAppContext,
    ) {
        for (from, to, expected) in [(1, 9, "**new**"), (5, 5, "**wonewrd**")] {
            let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
            view.update(cx, |view, cx| {
                view.codecs = Some(std::sync::Arc::new(
                    markraft_commonmark::CommonMarkCodecs::new(
                        view.state.schema().clone(),
                        Default::default(),
                    ),
                ));
                view.select_range(from, to, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
                view.execute_context_action(ContextAction::PasteMatchStyle, cx);
                assert_eq!(view.projection().plain_text(), expected);
                let undo = history::undo(&view.state).unwrap();
                view.dispatch([undo], cx);
                assert_eq!(view.projection().plain_text(), "**word**");
            });
        }
        let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
        view.update(cx, |view, cx| {
            view.codecs = Some(std::sync::Arc::new(
                markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    Default::default(),
                ),
            ));
            view.select_range(1, 9, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("*literal*\nnext".into()));
            view.execute_context_action(ContextAction::PasteMatchStyle, cx);
            let text = view
                .context_text(&view.context_document_snapshot())
                .unwrap()
                .text;
            assert_eq!(text, "*literal*\nnext");
            let strong = view.types.strong.unwrap();
            for (index, line) in view.analysis.projection().lines().iter().enumerate() {
                for piece in markraft_core::kind::reading::line_pieces(
                    view.analysis.projection(),
                    &view.types,
                    index,
                    &markraft_core::kind::conceal::Reveal::nothing(),
                ) {
                    let position = line.offset_to_pos(piece.source.start).unwrap();
                    assert!(
                        view.state
                            .doc()
                            .node_at(position)
                            .unwrap()
                            .marks()
                            .contains_type(strong)
                    );
                }
            }
        });
    }
}
