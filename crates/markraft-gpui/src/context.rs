//! Pointer context and capability queries shared by the host's editing menus.

use crate::{EditorEvent, EditorView, clipboard, links, wiki};
use gpui::{
    App, ClipboardEntry, ClipboardItem, Context, Font, MouseDownEvent, Pixels, Point, Window,
};
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

/// Painted text metrics for a native service that redraws the selected text.
pub struct ContextTextPresentation {
    pub text: String,
    /// First character's baseline origin in window-local, top-down points.
    pub baseline: Point<Pixels>,
    pub runs: Vec<ContextFontRun>,
}

pub struct ContextFontRun {
    /// UTF-16 range within the reading text, not the Markdown source.
    pub range: Range<usize>,
    pub font: Font,
    pub font_size: Pixels,
}

mod block_actions;
mod copy_formats;
mod pointer;
mod presentation;
mod reading_text;
mod smart_edit;
mod transformations;
#[cfg(test)]
pub(crate) use pointer::{pointer_character_in_row, word_at};
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
    /// selection, or history. A later edit moves the ranges beside it and
    /// drops the ones it touches.
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

    /// Open or close the scope [`Self::with_context_edit`] keeps, for a host
    /// whose edit runs through its own dispatch rather than a closure here.
    /// Returns the previous value, which the host restores afterwards.
    pub fn set_context_edit(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.context_edit, on)
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
}

#[cfg(test)]
mod pointer_targets;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
