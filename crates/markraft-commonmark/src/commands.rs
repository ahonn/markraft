//! Formatting commands as edits to the source text.
//!
//! A style mark is what the text spells, so a command that formats text
//! rewrites the text and leaves the marks to the canonicalising correction.
//! Every command here works the same way:
//!
//! 1. Read each textblock the selection touches as what a reader sees — its
//!    characters, atoms and line breaks, each with the styles over it — and
//!    work out the styles the command wants instead.
//! 2. Take the smallest stretch of source that has to change: the selection,
//!    grown by every span whose delimiters it cuts, whose style changes
//!    inside it, or which is a code span (whose content cannot hold markup).
//!    Spans around that stretch stay as written and keep their styles.
//! 3. Spell that stretch from the wanted styles with
//!    [`spell`](crate::serialize::spell) and read the whole block back with
//!    [`derive`]. If a reader would not see the wanted styles — CommonMark
//!    reads `**"a"**b` as no strong at all — try once more with the
//!    selection shrunk past the whitespace and punctuation at its ends, so the
//!    delimiters land inside them: `"**a**"b`. If nothing is left once they
//!    are gone, or that is not read as wanted either, the command refuses
//!    with a [`CommandRefusal`] the host can show; it never writes something
//!    a reader would read differently.
//!
//! Only the characters that differ are replaced, so the edit is one undo step
//! and carets elsewhere in the block stay where they were. The selected
//! characters stay selected.
//!
//! A cursor has nothing to style: [`toggle_style`] writes an empty delimiter
//! pair and puts the caret between them, so what is typed next is styled.
//!
//! [`keeping_styles`] wraps a command that splits a block — Enter — so that a
//! split inside a style closes every span open at the cut and opens it again
//! after, instead of leaving `**ab` and `cd**`.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use markraft_core::commands::{Command, command, mark_applies};
use markraft_core::{
    Attrs, Change, ChangeSet, EditorState, Fragment, MarkSet, MarkTypeId, Node, Schema, Selection,
    Slice, TransactionSpec,
};

use crate::derive::{BlockKind, Conceal, DeriveContext, Derived, Style, StyleSpan, derive};
use crate::inline::style_delimiters;
use crate::preset::commonmark_serializer;
use crate::schema as md;
use crate::serialize::spell_run;
use crate::textblock::{Item, Items, block_kind, document_context, style_mark, syntax_mark};

/// Why a formatting command left the document alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandRefusal {
    /// The document kind has no way to write the result here: every spelling
    /// tried is read back as something else.
    NotExpressible {
        /// What stood in the way.
        reason: Inexpressible,
    },
}

/// What kept a formatting result from being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inexpressible {
    /// The delimiters of this style would sit where a reader cannot take
    /// them for opening or closing it: next to punctuation on one side and a
    /// letter on the other, as in `**"a"**b`. The name is the schema's mark
    /// type name.
    Delimiters {
        /// The mark type whose delimiters were not read.
        mark: &'static str,
    },
    /// Any other way the spelled result reads differently from what was
    /// asked for.
    Unreadable,
}

impl fmt::Display for CommandRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandRefusal::NotExpressible {
                reason: Inexpressible::Delimiters { mark },
            } => write!(
                f,
                "Markdown cannot write {mark} here: its delimiters would sit between punctuation \
                 and a letter"
            ),
            CommandRefusal::NotExpressible {
                reason: Inexpressible::Unreadable,
            } => f.write_str("Markdown cannot write this formatting here"),
        }
    }
}

impl std::error::Error for CommandRefusal {}

/// What a formatting command gives: `Ok(None)` where it does not apply,
/// `Ok(Some(spec))` for the edit, or why the document kind cannot make it.
pub type Formatted = Result<Option<TransactionSpec>, CommandRefusal>;

/// A formatting command over an editor state. See [`Formatted`].
pub type FormatCommand = Arc<dyn Fn(&EditorState) -> Formatted + Send + Sync>;

/// Toggle a style over the selection by editing its delimiters.
///
/// The selection's style is what its two ends carry: when the first and the
/// last selected characters both have it, the toggle takes it off the whole
/// selection, and otherwise puts it on the whole selection — so a selection
/// starting in bold and running into plain text becomes one bold span, and one
/// running from one bold span to another loses both.
///
/// With a cursor the style's empty delimiter pair is written and the caret put
/// between the two, where typing is styled.
///
/// Strong, emphasis, strikethrough, code and underline have delimiters; for
/// any other mark type the command does not apply.
pub fn toggle_style(mark_type: MarkTypeId) -> FormatCommand {
    Arc::new(move |state| {
        let schema = state.schema();
        let name = schema.try_mark_type(mark_type).map(|ty| ty.name());
        let Some(style) = name.and_then(style_of) else {
            return Ok(None);
        };
        let ranges = state.selection().ranges(state.doc());
        if !mark_applies(schema, state.doc(), &ranges, mark_type) {
            return Ok(None);
        }
        if state.selection().is_cursor() {
            return Ok(insert_pair(state, &style));
        }
        format(state, Op::Toggle(style), "format.mark")
    })
}

/// [`toggle_style`] as a plain [`Command`], for a caller that cannot show a
/// refusal: a refused toggle does not apply. `attrs` is unused — every mark
/// this toggles is spelled without attributes — and kept for the shape of
/// the model's own `toggle_mark`.
pub fn toggle_style_mark(mark_type: MarkTypeId, attrs: Attrs) -> Command {
    let _ = attrs;
    let toggle = toggle_style(mark_type);
    command(move |state| toggle(state).ok().flatten())
}

/// Link the selection to `href`, with `title` (empty for none).
///
/// A caret inside a link, or a selection inside one link, changes that whole
/// link's destination and keeps its text. A caret anywhere else inserts `href` as its own linked
/// text, which Markdown writes as a bare URL where a reader links it back.
pub fn set_link(href: impl Into<String>, title: impl Into<String>) -> FormatCommand {
    let link = Style::Link {
        href: href.into(),
        title: title.into(),
    };
    Arc::new(move |state| {
        let Style::Link { href, .. } = &link else {
            unreachable!("built as a link")
        };
        if state.selection().is_cursor() && link_around_cursor(state).is_none() {
            return insert_linked(state, &link, href);
        }
        format(state, Op::SetLink(link.clone()), "format.link")
    })
}

/// Take the link off the selection, or off the whole link a caret or a
/// selection is inside.
pub fn unlink() -> FormatCommand {
    Arc::new(|state| {
        if state.selection().is_cursor() && link_around_cursor(state).is_none() {
            return Ok(None);
        }
        format(state, Op::Unlink, "format.link")
    })
}

/// Take every style off the selection.
pub fn clear_formatting() -> FormatCommand {
    Arc::new(|state| {
        if state.selection().is_cursor() {
            return Ok(None);
        }
        format(state, Op::Clear, "format.clear")
    })
}

/// Enter, keeping the styles open at the caret on both sides of the split.
pub fn split_block_keeping_styles() -> Command {
    keeping_styles(markraft_core::commands::split_block())
}

/// `split`, with every span open at a cursor closed before the cut and opened
/// again after it, so `**ab|cd**` splits into `**ab**` and `**cd**` with the
/// caret inside the second.
///
/// The cut moves off any delimiter it falls on — a split right after `**`
/// leaves the whole span to the second block. Where closing and reopening a
/// span would be read differently, the spans are closed before the whitespace
/// at the cut instead; where even that is not read back, `split` runs on its
/// own, since Enter has to do something. `split` is any command that splits
/// the textblock at a cursor — a list item's split as well as a plain one — and
/// where it does something else at the cut, it runs on its own as well.
pub fn keeping_styles(split: Command) -> Command {
    command(move |state| split_keeping(state, &split).or_else(|| split(state)))
}

// -- reading a block ----------------------------------------------------------

/// What a reader sees at one place in a block.
#[derive(Clone, Debug, PartialEq)]
enum Content {
    Char(char),
    Atom(Node),
    Break { hard: bool },
}

/// One visible unit of a block, with the styles over it and the source
/// characters that spell it.
#[derive(Clone, Debug)]
struct Unit {
    content: Content,
    styles: Vec<Style>,
    /// Item indexes: the character itself, with the backslash of an escape or
    /// the whole spelling of an entity or a hard break.
    src: Range<usize>,
}

/// A textblock read for formatting.
struct Block {
    node: Node,
    /// Document position of the first item.
    start: usize,
    kind: BlockKind,
    items: Items,
    derived: Derived,
    units: Vec<Unit>,
    /// What the block is read against: the document's definitions.
    ctx: DeriveContext,
}

impl Block {
    fn read(schema: &Schema, ctx: &DeriveContext, node: &Node, start: usize) -> Option<Block> {
        let kind = block_kind(schema, node.type_id())?;
        let items = Items::from_nodes(schema, node.children());
        let derived = derive(kind, &items.text(), ctx);
        let units = units_of(&items, &derived);
        Some(Block {
            node: node.clone(),
            start,
            kind,
            items,
            derived,
            units,
            ctx: ctx.clone(),
        })
    }

    /// The indexes of the units that overlap the item range `from..to`.
    fn units_in(&self, from: usize, to: usize) -> Option<Range<usize>> {
        let first = self
            .units
            .iter()
            .position(|unit| unit.src.start < to && unit.src.end > from)?;
        let last = self
            .units
            .iter()
            .rposition(|unit| unit.src.start < to && unit.src.end > from)?;
        Some(first..last + 1)
    }
}

/// Whether a concealed run is a style's own delimiter rather than an escape,
/// an entity or a hard break's spelling.
fn is_delimiter(derived: &Derived, conceal: &Conceal) -> bool {
    derived
        .styles
        .iter()
        .any(|span| span.range.start == conceal.range.start || span.range.end == conceal.range.end)
}

fn sorted(mut styles: Vec<Style>) -> Vec<Style> {
    styles.sort();
    styles.dedup();
    styles
}

fn units_of(items: &Items, derived: &Derived) -> Vec<Unit> {
    let styles_at = |offset: usize| {
        sorted(
            derived
                .styles
                .iter()
                .filter(|span| span.range.contains(&offset))
                .map(|span| span.style.clone())
                .collect(),
        )
    };
    let mut out: Vec<Unit> = Vec::new();
    // Where the spelling of the next unit started: an escape's backslash, a
    // hard break's `\` or trailing spaces.
    let mut pending: Option<usize> = None;
    for (index, item) in items.0.iter().enumerate() {
        if let Some(conceal) = derived.conceal_at(index) {
            if !conceal.display.is_empty() {
                if index == conceal.range.start {
                    for c in conceal.display.chars() {
                        out.push(Unit {
                            content: Content::Char(c),
                            styles: styles_at(index),
                            src: conceal.range.clone(),
                        });
                    }
                }
            } else if !is_delimiter(derived, conceal) {
                pending.get_or_insert(index);
            } else {
                pending = None;
            }
            continue;
        }
        let content = match item {
            Item::Char(c) => Content::Char(*c),
            Item::Atom(node) => Content::Atom(node.clone()),
            Item::Break => Content::Break {
                hard: derived.hard_breaks.contains(&index),
            },
        };
        out.push(Unit {
            content,
            styles: styles_at(index),
            src: pending.take().unwrap_or(index)..index + 1,
        });
    }
    out
}

/// The opening and closing runs of a span, as item ranges. A span with no
/// delimiters — a bare autolink — has empty ones at its edges.
fn delimiters(derived: &Derived, span: &StyleSpan) -> (Range<usize>, Range<usize>) {
    let open = derived
        .conceals
        .iter()
        .find(|conceal| conceal.range.start == span.range.start)
        .map_or(span.range.start..span.range.start, |c| c.range.clone());
    let close = derived
        .conceals
        .iter()
        .rfind(|conceal| conceal.range.end == span.range.end && conceal.range.start >= open.end)
        .map_or(span.range.end..span.range.end, |c| c.range.clone());
    (open, close)
}

// -- what a command wants -----------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Toggle(Style),
    Add(Style),
    Remove(Style),
    SetLink(Style),
    Unlink,
    Clear,
}

impl Op {
    /// The style the command writes or takes away, if it is about one.
    fn style(&self) -> Option<&Style> {
        match self {
            Op::Toggle(style) | Op::Add(style) | Op::Remove(style) | Op::SetLink(style) => {
                Some(style)
            }
            Op::Unlink | Op::Clear => None,
        }
    }

    fn apply(&self, styles: &[Style]) -> Vec<Style> {
        let mut out = styles.to_vec();
        match self {
            Op::Toggle(_) => unreachable!("resolved before it is applied"),
            Op::Add(style) => out.push(style.clone()),
            Op::Remove(style) => out.retain(|s| s != style),
            Op::SetLink(link) => {
                out.retain(|s| !matches!(s, Style::Link { .. }));
                out.push(link.clone());
            }
            Op::Unlink => out.retain(|s| !matches!(s, Style::Link { .. })),
            Op::Clear => out.clear(),
        }
        sorted(out)
    }
}

/// The style a delimited mark type is, by its schema name.
fn style_of(name: &str) -> Option<Style> {
    match name {
        md::STRONG => Some(Style::Strong),
        md::EM => Some(Style::Emphasis),
        md::STRIKETHROUGH => Some(Style::Strikethrough),
        md::CODE => Some(Style::Code),
        md::UNDERLINE => Some(Style::Underline),
        _ => None,
    }
}

fn is_blank(content: &Content) -> bool {
    match content {
        Content::Char(c) => c.is_whitespace(),
        Content::Break { .. } => true,
        Content::Atom(_) => false,
    }
}

fn is_blank_or_punctuation(content: &Content) -> bool {
    match content {
        Content::Char(c) => c.is_whitespace() || is_punctuation(*c),
        _ => false,
    }
}

/// CommonMark's punctuation: ASCII punctuation and Unicode P and S classes,
/// approximated by what is neither alphanumeric nor whitespace.
fn is_punctuation(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace()
}

// -- formatting a selection ----------------------------------------------------

/// One block's share of a selection, as a range of its units.
struct Share {
    block: Block,
    selected: Range<usize>,
}

/// The blocks the selection touches, each with the units it selects.
fn shares(state: &EditorState) -> Vec<Share> {
    let schema = state.schema();
    let doc = state.doc();
    let ctx = document_context(schema, doc);
    let mut out = Vec::new();
    for range in state.selection().ranges(doc) {
        doc.nodes_between(range.from, range.to, &mut |node, pos, _, _| {
            if block_kind(schema, node.type_id()).is_none() {
                return true;
            }
            let start = pos + 1;
            let end = start + node.content_size();
            let from = range.from.clamp(start, end) - start;
            let to = range.to.clamp(start, end) - start;
            if let Some(block) = Block::read(schema, &ctx, node, start)
                && let Some(selected) = block.units_in(from, to)
            {
                out.push(Share { block, selected });
            }
            false
        });
    }
    out
}

/// The whole link a caret is in, or that a selection lies inside, as a
/// selection over its units.
fn link_around_cursor(state: &EditorState) -> Option<Share> {
    let doc = state.doc();
    let (from, to) = (state.selection().from(doc), state.selection().to(doc));
    let resolved = doc.resolve(from).ok()?;
    let node = resolved.parent();
    let start = resolved.start(resolved.depth());
    if to > start + node.content_size() {
        return None;
    }
    let ctx = document_context(state.schema(), doc);
    let block = Block::read(state.schema(), &ctx, node, start)?;
    let (from, to) = (from - start, to - start);
    let span = block
        .derived
        .styles
        .iter()
        .filter(|span| matches!(span.style, Style::Link { .. }))
        .find(|span| span.range.start < from && to < span.range.end)?;
    let selected = block.units_in(span.range.start, span.range.end)?;
    Some(Share { block, selected })
}

fn format(state: &EditorState, op: Op, event: &str) -> Formatted {
    // A link is edited whole from inside it: a new destination for half its
    // text is a second link nobody asked for.
    let whole_link = match op {
        Op::SetLink(_) | Op::Unlink => link_around_cursor(state),
        _ => None,
    };
    let shares = match whole_link {
        Some(link) => vec![link],
        None if state.selection().is_cursor() => Vec::new(),
        None => shares(state),
    };
    let (Some(first), Some(last)) = (shares.first(), shares.last()) else {
        return Ok(None);
    };
    let op = match op {
        Op::Toggle(style) => {
            let edge = |share: &Share, from_end: bool| {
                let units = &share.block.units[share.selected.clone()];
                let mut visible = units.iter().filter(|unit| !is_blank(&unit.content));
                let unit = if from_end {
                    visible.next_back()
                } else {
                    visible.next()
                };
                unit.or_else(|| {
                    if from_end {
                        units.last()
                    } else {
                        units.first()
                    }
                })
                .is_some_and(|unit| unit.styles.contains(&style))
            };
            if edge(first, false) && edge(last, true) {
                Op::Remove(style)
            } else {
                Op::Add(style)
            }
        }
        op => op,
    };

    let schema = state.schema();
    let mut changes = Vec::new();
    let mut edits = Vec::new();
    for share in &shares {
        let edit = rewrite(schema, &share.block, share.selected.clone(), &op)?;
        changes.extend(edit.change(schema, &share.block));
        edits.push(edit);
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let set = ChangeSet::create(schema, state.doc(), changes).map_err(|_| unreadable())?;
    let position = |share: &Share, edit: &Rewrite, unit: usize, end: bool| {
        let start = set.map_pos(share.block.start, -1, markraft_core::TrackMode::Simple)?;
        let src = &edit.units.get(unit)?.src;
        Some(start + if end { src.end } else { src.start })
    };
    let from = position(first, &edits[0], first.selected.start, false);
    let last_edit = edits.last().expect("one edit per share");
    let to = position(last, last_edit, last.selected.end - 1, true);
    let mut spec = TransactionSpec::new()
        .change_set(set)
        .user_event(event)
        .scroll_into_view();
    if let (Some(from), Some(to)) = (from, to) {
        spec = spec.selection(Selection::text(from, to));
    }
    Ok(Some(spec))
}

fn unreadable() -> CommandRefusal {
    CommandRefusal::NotExpressible {
        reason: Inexpressible::Unreadable,
    }
}

/// One block's new text.
struct Rewrite {
    /// The item range replaced, and what replaces it.
    range: Range<usize>,
    items: Vec<Item>,
    /// The block's units after the rewrite, index for index the units before.
    units: Vec<Unit>,
}

impl Rewrite {
    /// The change that makes the block's text the rewritten one, trimmed to
    /// the items that differ.
    fn change(&self, schema: &Schema, block: &Block) -> Option<Change> {
        let old = &block.items.0[self.range.clone()];
        let new = &self.items;
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let from = self.range.start + prefix;
        let to = self.range.end - suffix;
        let inserted = &new[prefix..new.len() - suffix];
        if from == to && inserted.is_empty() {
            return None;
        }
        Some(Change::replace(
            block.start + from,
            block.start + to,
            Slice::from_fragment(Fragment::from_nodes(items_to_nodes(schema, inserted))),
        ))
    }
}

/// Unmarked content for `items`: the correction derives its marks.
fn items_to_nodes(schema: &Schema, items: &[Item]) -> Vec<Node> {
    let mut out = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<Node>| {
        if !run.is_empty() {
            out.push(schema.text(run));
            run.clear();
        }
    };
    for item in items {
        match item {
            Item::Char(c) => run.push(*c),
            Item::Atom(node) => {
                flush(&mut run, &mut out);
                out.push(node.clone());
            }
            Item::Break => {
                flush(&mut run, &mut out);
                if let Some(ty) = schema.node_id(md::LINE_BREAK)
                    && let Ok(node) =
                        schema.create(ty, Attrs::empty(), MarkSet::empty(), Fragment::empty())
                {
                    out.push(node);
                }
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Rewrite `block` so the units `selected` carry what `op` asks for, trying
/// the selection as it is and then shrunk past the whitespace and punctuation
/// at its ends, which stay as they were. See the module documentation.
fn rewrite(
    schema: &Schema,
    block: &Block,
    selected: Range<usize>,
    op: &Op,
) -> Result<Rewrite, CommandRefusal> {
    let first = attempt(schema, block, selected.clone(), op);
    let refusal = match first {
        Ok(rewrite) => return Ok(rewrite),
        Err(refusal) => refusal,
    };
    let units = &block.units;
    let mut shrunk = selected.clone();
    while !shrunk.is_empty() && is_blank_or_punctuation(&units[shrunk.start].content) {
        shrunk.start += 1;
    }
    while !shrunk.is_empty() && is_blank_or_punctuation(&units[shrunk.end - 1].content) {
        shrunk.end -= 1;
    }
    if shrunk.is_empty() || shrunk == selected {
        return Err(refusal);
    }
    attempt(schema, block, shrunk, op).map_err(|_| refusal)
}

fn attempt(
    schema: &Schema,
    block: &Block,
    selected: Range<usize>,
    op: &Op,
) -> Result<Rewrite, CommandRefusal> {
    let units = &block.units;
    let target: Vec<Vec<Style>> = units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            if selected.contains(&index) {
                op.apply(&unit.styles)
            } else {
                unit.styles.clone()
            }
        })
        .collect();

    // The stretch of source to rewrite, grown to whole spans.
    let mut lo = units[selected.start].src.start;
    let mut hi = units[selected.end - 1].src.end;
    let mut context: Vec<Style>;
    loop {
        let mut grew = false;
        context = Vec::new();
        for span in &block.derived.styles {
            let r = &span.range;
            if r.end <= lo || r.start >= hi || (r.start >= lo && r.end <= hi) {
                continue;
            }
            let (open, close) = delimiters(&block.derived, span);
            let around = open.end <= lo && close.start >= hi;
            let changes_inside = around
                && units.iter().zip(&target).any(|(unit, wanted)| {
                    unit.src.start >= lo
                        && unit.src.end <= hi
                        && ((unit.styles.contains(&span.style) && !wanted.contains(&span.style))
                            || (span.style == Style::Code && unit.styles != *wanted))
                });
            if around && !changes_inside {
                context.push(span.style.clone());
                continue;
            }
            lo = lo.min(r.start);
            hi = hi.max(r.end);
            grew = true;
        }
        // A span's edge never cuts a unit's spelling, but keep it that way.
        for unit in units {
            if unit.src.start < lo && unit.src.end > lo {
                lo = unit.src.start;
                grew = true;
            }
            if unit.src.start < hi && unit.src.end > hi {
                hi = unit.src.end;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let inside: Vec<usize> = (0..units.len())
        .filter(|index| units[*index].src.start >= lo && units[*index].src.end <= hi)
        .collect();

    // Spell the stretch from the styles it should carry. A link written bare
    // can run on into the text after the stretch, which its spelling does not
    // see; the bracketed spelling cannot.
    let links = inside.iter().any(|index| {
        target[*index]
            .iter()
            .any(|s| matches!(s, Style::Link { .. }))
    });
    let mut outcome = Err(None);
    for bracketed in [false, true] {
        if bracketed && !links {
            break;
        }
        let (spelled, atoms) = spell_units(
            schema,
            block,
            inside
                .iter()
                .map(|index| (&units[*index].content, target[*index].as_slice())),
            &context,
            lo,
            bracketed,
        )?;
        let new_items = spelled_items(&spelled, atoms)?;

        // Read the whole block back and compare it with what was asked for.
        let mut all = block.items.0[..lo].to_vec();
        all.extend(new_items.iter().cloned());
        all.extend(block.items.0[hi..].iter().cloned());
        let items = Items(all);
        let derived = derive(block.kind, &items.text(), &block.ctx);
        let read = units_of(&items, &derived);
        outcome = check(units, &target, &read).map(|()| (new_items, read));
        if outcome.is_ok() {
            break;
        }
    }
    let (new_items, read) = outcome.map_err(|mark| {
        // Delimiters read as characters are the delimiters of what the
        // command writes.
        match mark.or_else(|| op.style().map(Style::mark_name)) {
            Some(mark) => CommandRefusal::NotExpressible {
                reason: Inexpressible::Delimiters { mark },
            },
            None => unreadable(),
        }
    })?;
    Ok(Rewrite {
        range: lo..hi,
        items: new_items,
        units: read,
    })
}

/// Spelled source as items: `\n` is a line break and U+FFFC the next of
/// `atoms`.
fn spelled_items(spelled: &str, atoms: Vec<Node>) -> Result<Vec<Item>, CommandRefusal> {
    let mut atoms = atoms.into_iter();
    spelled
        .chars()
        .map(|c| match c {
            '\n' => Ok(Item::Break),
            markraft_core::projection::OBJECT_REPLACEMENT => {
                atoms.next().map(Item::Atom).ok_or_else(unreadable)
            }
            c => Ok(Item::Char(c)),
        })
        .collect()
}

/// Whether `read` is `units` with the `target` styles. Where it is not, the
/// style that reads differently, when that is what went wrong; `None` where
/// the text reads differently — a delimiter read as a character.
fn check(units: &[Unit], target: &[Vec<Style>], read: &[Unit]) -> Result<(), Option<&'static str>> {
    if read.len() != units.len() {
        return Err(None);
    }
    for ((unit, wanted), got) in units.iter().zip(target).zip(read) {
        if unit.content != got.content {
            return Err(None);
        }
        // Whitespace carries no style a reader sees, except inside code.
        let significant = |styles: &[Style]| -> Vec<Style> {
            if is_blank(&unit.content) {
                styles
                    .iter()
                    .filter(|style| **style == Style::Code)
                    .cloned()
                    .collect()
            } else {
                styles.to_vec()
            }
        };
        let (wanted, got) = (significant(wanted), significant(&got.styles));
        if wanted != got {
            let differs = wanted
                .iter()
                .find(|style| !got.contains(style))
                .or_else(|| got.iter().find(|style| !wanted.contains(style)))
                .expect("the two differ");
            return Err(Some(differs.mark_name()));
        }
    }
    Ok(())
}

/// The source for the units at `inside`, carrying `target` styles less the
/// `context` a surrounding span already gives them, and the atoms it holds in
/// order: an atom is spelled as U+FFFC and a soft line break as `\n`.
fn spell_units<'u>(
    schema: &Schema,
    block: &Block,
    units: impl IntoIterator<Item = (&'u Content, &'u [Style])>,
    context: &[Style],
    at: usize,
    bracketed_links: bool,
) -> Result<(String, Vec<Node>), CommandRefusal> {
    let marks_of = |styles: &[Style]| {
        MarkSet::from_marks(
            schema,
            styles
                .iter()
                .filter(|style| !context.contains(style))
                .filter_map(|style| style_mark(schema, style)),
        )
    };
    let line_break = schema.node_id(md::LINE_BREAK);
    let mut children: Vec<Node> = Vec::new();
    let mut atoms = Vec::new();
    let push_text = |children: &mut Vec<Node>, text: &str, marks: MarkSet| {
        if let Some(last) = children.last_mut()
            && last.is_text()
            && *last.marks() == marks
        {
            let joined = format!("{}{text}", last.text().unwrap_or_default());
            *last = last.with_text(&joined);
        } else {
            children.push(schema.text_marked(text, marks));
        }
    };
    for (content, styles) in units {
        let marks = marks_of(styles);
        match content {
            Content::Char(c) => push_text(&mut children, &c.to_string(), marks),
            Content::Atom(node) => {
                atoms.push(node.clone());
                let placeholder = markraft_core::projection::OBJECT_REPLACEMENT.to_string();
                push_text(&mut children, &placeholder, marks);
            }
            Content::Break { hard: false } => {
                let syntax = syntax_mark(schema, 0, "").ok_or_else(unreadable)?;
                push_text(&mut children, "\n", marks.add(schema, syntax));
            }
            Content::Break { hard: true } => {
                let ty = line_break.ok_or_else(unreadable)?;
                let node = schema
                    .create(ty, Attrs::empty(), marks, Fragment::empty())
                    .map_err(|_| unreadable())?;
                children.push(node);
            }
        }
    }
    let temp = block.node.copy(Fragment::from_nodes(children));
    let serializer = if bracketed_links {
        let mut marks = crate::preset::commonmark_mark_rules();
        marks.insert(md::LINK.to_string(), crate::preset::inline_link_mark_rule());
        crate::serialize::MarkdownSerializer::new(
            schema.clone(),
            crate::preset::commonmark_node_rules(),
            marks,
        )
    } else {
        commonmark_serializer(schema)
    };
    let at_line_start = at == 0 || block.items.0.get(at - 1) == Some(&Item::Break);
    Ok((spell_run(&serializer, &temp, at_line_start), atoms))
}

// -- a cursor ---------------------------------------------------------------------

/// The empty delimiter pair of `style` at the cursor, with the caret inside.
fn insert_pair(state: &EditorState, style: &Style) -> Option<TransactionSpec> {
    let (open, close) = style_delimiters(style.mark_name())
        .or_else(|| (*style == Style::Code).then_some(("`", "`")))?;
    let pos = state.selection().head(state.doc());
    let text = Slice::from_fragment(Fragment::from_node(
        state.schema().text(&format!("{open}{close}")),
    ));
    Some(
        TransactionSpec::new()
            .changes([Change::insert(pos, text)])
            .selection(Selection::cursor(pos + open.chars().count()))
            .user_event("format.mark"),
    )
}

/// `href` inserted at the cursor as its own linked text, inside whatever
/// spans the cursor is in.
fn insert_linked(state: &EditorState, link: &Style, href: &str) -> Formatted {
    let schema = state.schema();
    let doc = state.doc();
    let pos = state.selection().head(doc);
    let resolved = doc.resolve(pos).map_err(|_| unreadable())?;
    let start = resolved.start(resolved.depth());
    let ctx = document_context(schema, doc);
    let Some(block) = Block::read(schema, &ctx, resolved.parent(), start) else {
        return Ok(None);
    };
    let mut offset = pos - start;
    if let Some(unit) = block
        .units
        .iter()
        .find(|unit| unit.src.start < offset && offset < unit.src.end)
    {
        offset = unit.src.start;
    }
    let before = block.units.iter().filter(|u| u.src.end <= offset).count();
    // The spans the URL lands inside give it their styles as they are.
    let context: Vec<Style> = block
        .derived
        .styles
        .iter()
        .filter(|span| {
            let (open, close) = delimiters(&block.derived, span);
            open.end <= offset && offset <= close.start
        })
        .map(|span| span.style.clone())
        .collect();
    let styles = sorted(context.iter().cloned().chain([link.clone()]).collect());
    let typed: Vec<Content> = href.chars().map(Content::Char).collect();
    let mut expected: Vec<Unit> = block.units[..before].to_vec();
    expected.extend(typed.iter().map(|content| Unit {
        content: content.clone(),
        styles: styles.clone(),
        src: 0..0,
    }));
    expected.extend(block.units[before..].iter().cloned());
    let target: Vec<Vec<Style>> = expected.iter().map(|unit| unit.styles.clone()).collect();
    let mut outcome = Err(None);
    for bracketed in [false, true] {
        let (spelled, _) = spell_units(
            schema,
            &block,
            typed.iter().map(|content| (content, styles.as_slice())),
            &context,
            offset,
            bracketed,
        )?;
        let new_items: Vec<Item> = spelled.chars().map(Item::Char).collect();
        let mut all = block.items.0[..offset].to_vec();
        all.extend(new_items.iter().cloned());
        all.extend(block.items.0[offset..].iter().cloned());
        let items = Items(all);
        let derived = derive(block.kind, &items.text(), &block.ctx);
        let read = units_of(&items, &derived);
        outcome = check(&expected, &target, &read).map(|()| (new_items, read));
        if outcome.is_ok() {
            break;
        }
    }
    let (new_items, _) = outcome.map_err(|mark| CommandRefusal::NotExpressible {
        reason: Inexpressible::Delimiters {
            mark: mark.unwrap_or(md::LINK),
        },
    })?;
    // After the whole link, delimiters and all: typing there is not linked.
    let caret = offset + new_items.len();
    Ok(Some(
        TransactionSpec::new()
            .changes([Change::insert(
                start + offset,
                Slice::from_fragment(Fragment::from_nodes(items_to_nodes(schema, &new_items))),
            )])
            .selection(Selection::cursor(start + caret))
            .user_event("format.link")
            .scroll_into_view(),
    ))
}

// -- Enter ------------------------------------------------------------------------

fn split_keeping(state: &EditorState, split: &Command) -> Option<TransactionSpec> {
    if !state.selection().is_cursor() {
        return None;
    }
    let schema = state.schema();
    let doc = state.doc();
    let head = state.selection().head(doc);
    let resolved = doc.resolve(head).ok()?;
    let node = resolved.parent();
    let start = resolved.start(resolved.depth());
    let block = Block::read(schema, &document_context(schema, doc), node, start)?;
    let derived = &block.derived;

    // Move the cut off delimiters and out of a unit's spelling.
    let mut cut = head - start;
    loop {
        let before = cut;
        for span in &derived.styles {
            let (open, close) = delimiters(derived, span);
            if open.start < cut && cut <= open.end && cut < close.start {
                cut = span.range.start;
            } else if close.start <= cut && cut < close.end && open.end < cut {
                cut = span.range.end;
            }
        }
        if let Some(unit) = block
            .units
            .iter()
            .find(|unit| unit.src.start < cut && cut < unit.src.end)
        {
            cut = unit.src.start;
        }
        if cut == before {
            break;
        }
    }
    let open_at: Vec<&StyleSpan> = derived
        .styles
        .iter()
        .filter(|span| {
            let (open, close) = delimiters(derived, span);
            open.end < cut && cut < close.start && open.start < open.end
        })
        .collect();
    // Nothing to close and nowhere else to cut: an ordinary split.
    if open_at.is_empty() && cut == head - start {
        return None;
    }
    let text = |range: Range<usize>| -> String {
        block.items.0[range]
            .iter()
            .map(|item| match item {
                Item::Char(c) => *c,
                _ => markraft_core::projection::OBJECT_REPLACEMENT,
            })
            .collect()
    };
    // Outer spans come first in `styles`: close inner first, open outer first.
    let closers: String = open_at
        .iter()
        .rev()
        .map(|span| text(delimiters(derived, span).1))
        .collect();
    let openers: String = open_at
        .iter()
        .map(|span| text(delimiters(derived, span).0))
        .collect();
    if closers.contains(markraft_core::projection::OBJECT_REPLACEMENT)
        || openers.contains(markraft_core::projection::OBJECT_REPLACEMENT)
    {
        return None;
    }

    // Where the closers and openers go: at the cut, or around the whitespace
    // there when a reader would not take them at the cut.
    let units_before = block.units.iter().filter(|u| u.src.end <= cut).count();
    let placements = {
        let items = &block.items.0;
        let mut left = cut;
        while left > 0 && matches!(items[left - 1], Item::Char(' ' | '\t')) {
            left -= 1;
        }
        let mut right = cut;
        while right < items.len() && matches!(items[right], Item::Char(' ' | '\t')) {
            right += 1;
        }
        let mut out = vec![(cut, cut)];
        if (left, right) != (cut, cut) {
            out.push((left, right));
        }
        out
    };
    let (close_at, open_at_pos) = placements.into_iter().find(|(close_at, open_at)| {
        let halves = split_halves(&block, &closers, &openers, *close_at, cut, *open_at);
        halves_read_back(&block, units_before, &halves)
    })?;

    // Write the spans' ends in, split between them, and put the caret after
    // the reopened delimiters.
    let text_slice = |s: &str| Slice::from_fragment(Fragment::from_node(schema.text(s)));
    let mut changes = Vec::new();
    let closers_len = closers.chars().count();
    if open_at.is_empty() {
        // Only the cut moved.
    } else if close_at == open_at_pos {
        changes.push(Change::insert(
            start + close_at,
            text_slice(&format!("{closers}{openers}")),
        ));
    } else {
        changes.push(Change::insert(start + close_at, text_slice(&closers)));
        changes.push(Change::insert(start + open_at_pos, text_slice(&openers)));
    }
    // The cut, between the two runs, in the document the insertions produce.
    let split_at = start + cut + closers_len;
    let first = state
        .update([TransactionSpec::new()
            .changes(changes)
            .selection(Selection::cursor(split_at))])
        .ok()?;
    let spec = split(first.state())?;
    let second = first.state().update([spec]).ok()?;
    // The split has to have cut there: the caret now opens a block whose text
    // starts with the reopened delimiters.
    let after = second.new_doc();
    let caret = second.new_selection();
    if !caret.is_cursor() {
        return None;
    }
    let caret = caret.head(after);
    let resolved = after.resolve(caret).ok()?;
    let parent = resolved.parent();
    if resolved.parent_offset() != 0 || block_kind(schema, parent.type_id()).is_none() {
        return None;
    }
    let reopened = Items::from_nodes(schema, parent.children()).text();
    let skip = if close_at == open_at_pos {
        0
    } else {
        open_at_pos - cut
    };
    let tail: String = reopened.chars().skip(skip).collect();
    if !tail.starts_with(&openers) {
        return None;
    }
    let inside = caret + skip + openers.chars().count();
    let set = first.changes().compose(second.changes()).ok()?;
    let mut spec = TransactionSpec::new()
        .change_set(set)
        .selection(Selection::cursor(inside))
        .scroll_into_view();
    if let Some(event) = second.user_event_name() {
        spec = spec.user_event(event);
    }
    Some(spec)
}

/// The two blocks' texts after closing at `close_at` and opening at
/// `open_at`, with the split at `cut` between.
fn split_halves(
    block: &Block,
    closers: &str,
    openers: &str,
    close_at: usize,
    cut: usize,
    open_at: usize,
) -> (Items, Items) {
    let items = &block.items.0;
    let mut left = items[..close_at].to_vec();
    left.extend(closers.chars().map(Item::Char));
    left.extend(items[close_at..cut].iter().cloned());
    let mut right = items[cut..open_at].to_vec();
    right.extend(openers.chars().map(Item::Char));
    right.extend(items[open_at..].iter().cloned());
    (Items(left), Items(right))
}

/// Whether each half reads as the units it holds did before the split.
fn halves_read_back(block: &Block, units_before: usize, halves: &(Items, Items)) -> bool {
    let (left, right) = halves;
    let (before, after) = block.units.split_at(units_before);
    let reads = |items: &Items, units: &[Unit]| {
        let derived = derive(block.kind, &items.text(), &block.ctx);
        let read = units_of(items, &derived);
        let target: Vec<Vec<Style>> = units.iter().map(|unit| unit.styles.clone()).collect();
        check(units, &target, &read).is_ok()
    };
    reads(left, before) && reads(right, after)
}
