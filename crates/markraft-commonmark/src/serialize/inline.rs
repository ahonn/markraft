//! The inline half of the serialiser.
//!
//! [`SerializerState::render_inline`] writes a textblock whose text is its own
//! inline source: as it stands, with the guard's backslashes, one output line
//! per line break.
//!
//! [`SerializerState::spell_inline`] spells *semantic* inline content — text
//! with style marks and nothing else — as Markdown, which is how pasted HTML
//! becomes source. Its algorithm is ProseMirror's. It keeps a stack of the
//! marks that are currently open, and for each inline node works out the
//! longest prefix of that stack it can keep, closing the rest and opening
//! whatever the node adds. Two marks that may be written in either order are
//! reordered rather than closed and reopened, and whitespace at the edges of a
//! run moves out from under the marks that could not carry it.

use markraft_core::projection::is_line_break;
use markraft_core::{Mark, Node};

use super::{MarkTarget, SerializerState};
use crate::derive::BlockKind;
use crate::textblock::{Item, Items, block_kind};

impl SerializerState<'_> {
    /// Write a textblock's text, which is its inline source.
    ///
    /// The text goes out as it stands, with the backslashes
    /// [`guard`](crate::derive::guard) names so it reads back as the same
    /// block. A few shapes the canonicalising correction leaves alone while the
    /// caret is on them are settled here instead, since no reader could give
    /// them back: whitespace starting a line is dropped, and so is a line
    /// break at the start or end of the block. On a line that cannot end — an
    /// ATX heading, a table row — a break is written as
    /// [`SerializerState::line_break`].
    pub fn render_inline(&mut self, parent: &Node) {
        let schema = self.schema();
        let kind = block_kind(schema, parent.type_id()).unwrap_or(BlockKind::Paragraph);
        let items = canonical_lines(Items::from_nodes(schema, parent.children()));
        let insertions = items.guard_insertions(schema, kind);
        let mut out = String::new();
        let mut next = insertions.iter().peekable();
        for (index, item) in items.0.iter().enumerate() {
            while next.next_if(|at| **at == index).is_some() {
                out.push('\\');
            }
            match item {
                Item::Char(c) => out.push(*c),
                Item::Break if self.single_line => out.push_str(self.line_break),
                Item::Break => out.push('\n'),
                Item::Atom(node) => {
                    let spelled = self.capture_node(node);
                    // A `!` before a link's `[[` would make it an embed.
                    if spelled.starts_with('[') && out.ends_with('!') {
                        out.pop();
                        out.push_str("\\!");
                    }
                    out.push_str(&spelled);
                }
            }
        }
        if !out.is_empty() {
            self.text(&out, false);
        }
    }

    /// An inline atom as its rule writes it, on its own.
    fn capture_node(&mut self, node: &Node) -> String {
        self.capture(self.line_break, |state| state.render(node, None, 0))
    }

    /// Spell `parent`'s semantic inline content as Markdown, opening and
    /// closing marks around the runs that carry them.
    pub(crate) fn spell_inline(&mut self, parent: &Node) {
        let mut active: Vec<Mark> = Vec::new();
        let mut trailing = String::new();
        for index in 0..=parent.child_count() {
            self.inline_step(parent, index, &mut active, &mut trailing);
        }
    }

    fn inline_step(
        &mut self,
        parent: &Node,
        index: usize,
        active: &mut Vec<Mark>,
        trailing: &mut String,
    ) {
        let mut node = parent.maybe_child(index).cloned();
        let mut marks = self.marks_of(node.as_ref(), parent, index);
        let mut leading = std::mem::take(trailing);
        if let Some(current) = node.clone()
            && current.is_text()
            && marks.iter().any(|mark| {
                self.serializer
                    .mark_rule(mark.ty)
                    .is_some_and(|rule| rule.expel_enclosing_whitespace)
                    && !active.contains(mark)
            })
        {
            let text = current.text().unwrap_or_default();
            let start = text.len() - text.trim_start_matches([' ', '\t']).len();
            let end = text.trim_end_matches([' ', '\t']).len();
            if start > 0 || end < text.len() {
                leading.push_str(&text[..start]);
                *trailing = text[end.max(start)..].to_string();
                let inner = &text[start..end.max(start)];
                node = (!inner.is_empty()).then(|| current.with_text(inner));
                if node.is_none() {
                    marks = active.clone();
                }
            }
        }

        let inner = marks.last().cloned();
        let no_escape = inner.as_ref().is_some_and(|mark| {
            self.serializer
                .mark_rule(mark.ty)
                .is_some_and(|r| !r.escape)
        });
        let len = marks.len() - usize::from(no_escape);
        let ordered = self.mix(&marks[..len], active);

        let mut keep = 0;
        while keep < active.len().min(len) && ordered[keep] == active[keep] {
            keep += 1;
        }
        while active.len() > keep {
            let mark = active.pop().expect("keep is below the length");
            // A delimiter directly after a space cannot close emphasis, so the
            // space moves out from under the mark, as it does when one opens.
            let expels = self
                .serializer
                .mark_rule(mark.ty)
                .is_some_and(|rule| rule.expel_enclosing_whitespace);
            let moved = if expels {
                self.take_trailing_space()
            } else {
                String::new()
            };
            let closing = self.mark_string(&mark, false, parent, index.saturating_sub(1));
            self.text(&closing, false);
            self.after_mark_close = moved.is_empty();
            if !moved.is_empty() {
                self.out.push_str(&moved);
            }
        }
        if !leading.is_empty() {
            self.text(&leading, true);
        }
        let Some(current) = node else { return };
        while active.len() < len {
            let mark = ordered[active.len()].clone();
            active.push(mark.clone());
            let opening = self.mark_string(&mark, true, parent, index);
            self.text(&opening, false);
        }
        match (no_escape, current.text()) {
            (true, Some(text)) => {
                let mark = inner.expect("a mark that forbids escaping exists");
                let open = self.mark_string(&mark, true, parent, index);
                let close = self.mark_string(&mark, false, parent, index);
                self.text(&format!("{open}{text}{close}"), false);
            }
            // A URL in text that no link encloses has to stay text: GFM reads
            // a bare one back as a link the document never had.
            (_, Some(text)) if !self.inside_link(&current) => self.unlinked_text(text),
            (_, Some(text)) => self.text(text, true),
            (_, None) => self.render(&current, Some(parent), index),
        }
    }

    /// Whether a link already encloses the node, so a reader reads no autolink
    /// out of a URL in its text.
    fn inside_link(&self, node: &Node) -> bool {
        node.marks()
            .iter()
            .any(|mark| self.schema().mark_type(mark.ty).name() == crate::schema::LINK)
    }

    /// The marks of an inline node that this serialiser can write.
    ///
    /// A line break at the end of a marked run would leave the closing
    /// delimiter alone on the next line, so it keeps only the marks that carry
    /// on past it.
    fn marks_of(&self, node: Option<&Node>, parent: &Node, index: usize) -> Vec<Mark> {
        let Some(node) = node else {
            return Vec::new();
        };
        let mut marks: Vec<Mark> = node
            .marks()
            .iter()
            .filter(|mark| self.serializer.mark_rule(mark.ty).is_some())
            .cloned()
            .collect();
        if is_line_break(self.schema(), node.type_id()) {
            marks.retain(|mark| {
                parent.maybe_child(index + 1).is_some_and(|next| {
                    next.marks().contains(mark)
                        && next
                            .text()
                            .is_none_or(|text| text.chars().any(|c| !c.is_whitespace()))
                })
            });
        }
        marks
    }

    /// Reorder mixable marks so a run that is already open stays open.
    fn mix(&self, marks: &[Mark], active: &[Mark]) -> Vec<Mark> {
        let mut ordered = marks.to_vec();
        let mixable = |mark: &Mark| {
            self.serializer
                .mark_rule(mark.ty)
                .is_some_and(|rule| rule.mixable)
        };
        for i in 0..ordered.len() {
            if !mixable(&ordered[i]) {
                break;
            }
            let Some(j) = active
                .iter()
                .take_while(|other| mixable(other))
                .position(|other| *other == ordered[i])
            else {
                continue;
            };
            if j < ordered.len() && j != i {
                let mark = ordered.remove(i);
                ordered.insert(if j > i { j - 1 } else { j }, mark);
            }
        }
        ordered
    }

    /// Take the spaces and tabs off the end of the current line, never reaching
    /// into the line's prefix.
    fn take_trailing_space(&mut self) -> String {
        let line_begin = self.out.rfind('\n').map_or(0, |index| index + 1);
        let floor = (line_begin + self.delim.len()).min(self.out.len());
        let mut cut = self.out.len();
        while cut > floor && matches!(self.out.as_bytes()[cut - 1], b' ' | b'\t') {
            cut -= 1;
        }
        let moved = self.out[cut..].to_string();
        self.out.truncate(cut);
        moved
    }

    fn mark_string(&mut self, mark: &Mark, opening: bool, parent: &Node, index: usize) -> String {
        let serializer = self.serializer;
        let Some(rule) = serializer.mark_rule(mark.ty) else {
            return String::new();
        };
        let f = if opening {
            rule.open.clone()
        } else {
            rule.close.clone()
        };
        let target = MarkTarget {
            mark,
            parent,
            index,
            opening,
        };
        f(self, &target)
    }
}

/// `items` without the whitespace that starts each line and the line breaks
/// at either end of the block.
fn canonical_lines(items: Items) -> Items {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut line_start = true;
    for item in items.0 {
        match item {
            Item::Char(' ' | '\t') if line_start => {}
            Item::Break => {
                // A break at the start of the block would leave an empty line
                // the block does not have.
                if !out.is_empty() {
                    out.push(Item::Break);
                }
                line_start = true;
            }
            item => {
                out.push(item);
                line_start = false;
            }
        }
    }
    while out.last() == Some(&Item::Break) {
        out.pop();
    }
    Items(out)
}
