//! The delimiter pair a cursor toggle writes, until something is typed in it.
//!
//! [`Formatter::toggle_style`](crate::Formatter::toggle_style) at a caret writes delimiters and puts
//! the caret between them: a style's empty pair — `**|**` — so that what is
//! typed next is styled, or a span's closing and opening runs — `**ab**|**c**`
//! — so that it is not. Until something is typed there, those runs spell
//! nothing a reader wants: left behind, `****` is four literal asterisks in
//! the file. The pair is therefore remembered in a state field as *pending*,
//! with the layers it is made of, and settled by the transactions that follow:
//!
//! * One that takes the caret out from between the runs while nothing is
//!   between them deletes the pair. When that transaction does not change the
//!   document itself — an arrow key, a click — the deletion is annotated
//!   [`fold_into_previous`], so undoing the toggle takes both back.
//! * One that deletes into an empty pair and nothing else — Backspace or
//!   Delete at the caret — deletes the rest of it with it.
//! * One that breaks an empty pair apart — Enter between the runs, which
//!   leaves one at the end of a block and the other at the start of the
//!   next — deletes both runs with it, so the split is the one Enter makes
//!   without the pair, and undoing it gives the pair back with the caret in it.
//! * Typing between the runs makes the pair a span. It stays remembered while
//!   the caret is inside, so undoing the typing gives back a pair that still
//!   goes when the caret leaves; the caret leaving a pair that holds something
//!   forgets it — unless it does not read as its styles, as `**ni **` does
//!   not: then the runs move inside the whitespace, `**ni** `, or go when
//!   there is nothing else between them. That change is folded like the
//!   deletion of an empty pair.
//!
//! Nothing is settled in a transaction the history does not record — an undo,
//! a redo — or one from another peer: they restore or relay a selection
//! rather than move it. A pair left empty by one stays pending, and the next
//! transaction that is the writer's own settles it.
//!
//! # Undo
//!
//! Undoing a transaction that settled a pair — Backspace in it, Enter between
//! its runs — gives the runs back with the caret between them, spelling
//! nothing again. So the field remembers the last pair it deleted, as the
//! points its runs were deleted down to, and an undo or a redo that writes
//! those runs back at exactly those points, over nothing but the structure
//! between them, with the caret between them, makes the pair pending again.
//! Only a pair the field itself deleted comes back this way: runs typed by
//! hand never become pending, and a new toggle forgets the memory.
//!
//! Undoing the toggle itself deletes the pending pair, and the field
//! remembers it the same way, so the redo that writes it back makes it
//! pending again. And a pair typed into and then left is followed, settled
//! by nothing, so that undoing the typing — which empties it with the caret
//! between the runs — makes it pending again too.
//!
//! # Why a filter
//!
//! The deletion is folded into the transaction that moves the caret by a
//! [`transaction_filter`], so the corrections see the document without the
//! pair. A transaction extender only sees the transaction it extends, not
//! the corrections before it, and its positions would have to address a
//! document it cannot see: the canonicalising correction settles the line the
//! caret left in that same transaction, and `****` alone on a line is a
//! thematic break it guards with a backslash, which would shift the pair.

use std::ops::Range;
use std::sync::{Arc, LazyLock};

use markraft_core::protocol::fold_into_previous;
use markraft_core::{
    Annotation, AnnotationType, Change, ChangeRange, ChangeSet, EditorState, Extension, Fragment,
    Node, Schema, Slice, StateField, StateFieldConfig, TrackMode, Transaction, TransactionFilterFn,
    TransactionSpec, transaction_filter,
};

use crate::derive::{BlockKind, DeriveContext, Style, derive};
use crate::textblock::{Items, block_kind, document_context};

/// One style's runs in a pending pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    /// The style the layer is about.
    pub(crate) style: Style,
    /// Whether typing between the runs takes the style on (`**|**`) or
    /// leaves the span that has it (`**ab**|**c**`).
    pub(crate) adds: bool,
    /// The run before the caret: an opening delimiter where the layer adds
    /// the style, the span's closing one where it leaves it.
    pub(crate) open: String,
    /// The run after the caret.
    pub(crate) close: String,
}

/// The runs before and after the caret that `layers`, outermost first, spell.
pub(crate) fn runs(layers: &[Layer]) -> (String, String) {
    let left = layers.iter().map(|layer| layer.open.as_str()).collect();
    let right = layers
        .iter()
        .rev()
        .map(|layer| layer.close.as_str())
        .collect();
    (left, right)
}

/// A pair the caret was put between, as document positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    /// Its layers, outermost first.
    pub(crate) layers: Vec<Layer>,
    /// The runs before the caret.
    pub(crate) open: Range<usize>,
    /// The runs after the caret.
    pub(crate) close: Range<usize>,
}

impl Pending {
    /// Whether nothing stands between the two runs.
    pub(crate) fn is_empty(&self) -> bool {
        self.open.end == self.close.start
    }

    /// Where the caret goes in an empty pair.
    pub(crate) fn caret(&self) -> Option<usize> {
        self.is_empty().then_some(self.open.end)
    }

    /// The whole pair, and whatever was typed in it.
    pub(crate) fn range(&self) -> Range<usize> {
        self.open.start..self.close.end
    }

    /// The pair of `layers` around a caret in `doc`, when its runs are there.
    fn around(schema: &Schema, doc: &Node, layers: &[Layer], caret: usize) -> Option<Pending> {
        let (left, right) = runs(layers);
        let open = caret.checked_sub(left.chars().count())?..caret;
        let close = caret..caret + right.chars().count();
        let pending = Pending {
            layers: layers.to_vec(),
            open,
            close,
        };
        pending.reads_in(schema, doc).then_some(pending)
    }

    /// Where the pair is after `changes`, while both of its runs are still
    /// there as they were.
    fn map(&self, schema: &Schema, doc: &Node, changes: &ChangeSet) -> Option<Pending> {
        // Text typed at either end of a run is outside it: at the caret it is
        // between the runs, next to the pair it is outside the pair.
        let map = |pos: usize, assoc: i32| changes.map_pos(pos, assoc, TrackMode::Simple);
        let pending = Pending {
            layers: self.layers.clone(),
            open: map(self.open.start, 1)?..map(self.open.end, -1)?,
            close: map(self.close.start, 1)?..map(self.close.end, -1)?,
        };
        pending.reads_in(schema, doc).then_some(pending)
    }

    /// Whether `doc` spells the pair's runs where it says they are, in one
    /// textblock of inline source.
    fn reads_in(&self, schema: &Schema, doc: &Node) -> bool {
        let (left, right) = runs(&self.layers);
        self.open.end <= self.close.start
            && source(schema, doc, self.range()).is_some()
            && source(schema, doc, self.open.clone()).as_deref() == Some(left.as_str())
            && source(schema, doc, self.close.clone()).as_deref() == Some(right.as_str())
    }
}

/// The source text over `range`, when it lies inside one textblock whose text
/// is inline Markdown.
fn source(schema: &Schema, doc: &Node, range: Range<usize>) -> Option<String> {
    let resolved = doc.resolve(range.start).ok()?;
    let block = resolved.parent();
    block_kind(schema, block.type_id())?;
    let start = resolved.start(resolved.depth());
    if range.end > start + block.content_size() {
        return None;
    }
    let text = Items::from_nodes(schema, block.children()).text();
    Some(
        text.chars()
            .skip(range.start - start)
            .take(range.len())
            .collect(),
    )
}

/// What a formatting transaction says about the pending pair.
enum Mark {
    /// Its caret is between the runs of these layers.
    Set(Vec<Layer>),
    /// It leaves no pair pending.
    Clear,
}

/// An empty pair the field deleted, as the points in the document its runs
/// were deleted down to: `from` where the opening runs started, `to` where
/// the closing runs ended. They are one point when the pair went whole, and
/// the ends of two textblocks when Enter broke it apart.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Settled {
    layers: Vec<Layer>,
    from: usize,
    to: usize,
}

impl Settled {
    /// Where `pending`, deleted by `changes`, went.
    fn after(pending: &Pending, changes: &ChangeSet) -> Option<Settled> {
        Some(Settled {
            layers: pending.layers.clone(),
            from: changes.map_pos(pending.open.start, -1, TrackMode::Simple)?,
            to: changes.map_pos(pending.close.end, 1, TrackMode::Simple)?,
        })
    }

    /// The same points after `changes`: what is inserted at either keeps
    /// outside them, as it would be outside the runs.
    fn map(&self, changes: &ChangeSet) -> Option<Settled> {
        Some(Settled {
            layers: self.layers.clone(),
            from: changes.map_pos(self.from, -1, TrackMode::Simple)?,
            to: changes.map_pos(self.to, 1, TrackMode::Simple)?,
        })
    }

    /// The pair again, when `tr` — an undo or a redo — wrote its runs back
    /// where they were deleted from and left the caret between them.
    fn restored(&self, tr: &Transaction) -> Option<Pending> {
        let schema = tr.start_state().schema();
        let before = tr.start_state().doc();
        // Only structure — a block boundary Enter made — may stand where the
        // runs were: text there is something written since, which may spell
        // the same runs without being the pair.
        let leaf = |_: &Node| "\u{fffc}".to_string();
        if !before
            .text_between(schema, self.from, self.to, None, Some(&leaf))
            .is_empty()
        {
            return None;
        }
        let doc = tr.new_doc();
        let selection = tr.new_selection();
        if !selection.is_cursor() {
            return None;
        }
        let restored = Pending::around(schema, doc, &self.layers, selection.head(doc))?;
        let mapped = self.map(tr.changes())?;
        (restored.open.start == mapped.from && restored.close.end == mapped.to).then_some(restored)
    }
}

/// What the field keeps: the pair pending, a pair that holds something the
/// caret left, or the last one it deleted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Pair {
    #[default]
    None,
    Pending(Pending),
    /// A pair typed into and left: a span, settled by nothing, followed
    /// only so that an undo emptying it again with the caret inside gives
    /// back the pending pair.
    Left(Pending),
    Settled(Settled),
}

static MARK: LazyLock<AnnotationType<Mark>> = LazyLock::new(AnnotationType::define);
/// Set on a transaction the filter extended to delete the pending pair.
static SETTLES: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
/// Set on a transaction the filter extended to move the runs of a pair that
/// holds something in around its whitespace: the pair where it ends up.
static RESHAPED: LazyLock<AnnotationType<Pending>> = LazyLock::new(AnnotationType::define);
static FIELD: LazyLock<StateField<Pair>> =
    LazyLock::new(|| StateField::define(StateFieldConfig::new(|_| Pair::None, update)));
static EXTENSION: LazyLock<Extension> = LazyLock::new(|| {
    let settle: TransactionFilterFn = Arc::new(settle);
    Extension::all([transaction_filter().of(settle), FIELD.extension()])
});

/// The field and the filter that keep and settle the pending pair.
pub(crate) fn pending_pairs() -> Extension {
    EXTENSION.clone()
}

/// The pair pending in `state`, if its configuration keeps one.
pub(crate) fn pending(state: &EditorState) -> Option<&Pending> {
    match state.field(&FIELD)? {
        Pair::Pending(pending) => Some(pending),
        _ => None,
    }
}

/// The annotation for a transaction that leaves the caret between the runs of
/// `layers`, or leaves no pair pending when there are none.
pub(crate) fn pending_after(layers: Vec<Layer>) -> Annotation {
    if layers.is_empty() {
        MARK.of(Mark::Clear)
    } else {
        MARK.of(Mark::Set(layers))
    }
}

fn update(value: &Pair, tr: &Transaction) -> Pair {
    let schema = tr.start_state().schema();
    let doc = tr.new_doc();
    let selection = tr.new_selection();
    if let Some(mark) = tr.annotation(&MARK) {
        return match mark {
            Mark::Set(layers) if selection.is_cursor() => {
                Pending::around(schema, doc, layers, selection.head(doc))
                    .map_or(Pair::None, Pair::Pending)
            }
            _ => Pair::None,
        };
    }
    if let Some(reshaped) = tr.annotation(&RESHAPED) {
        return Pair::Left(reshaped.clone());
    }
    match value {
        Pair::None => Pair::None,
        Pair::Pending(pending) if tr.annotation(&SETTLES) == Some(&true) => {
            Settled::after(pending, tr.changes()).map_or(Pair::None, Pair::Settled)
        }
        Pair::Pending(pending) => match follow(schema, pending, tr) {
            // A pair that holds something is a span: once the caret leaves
            // it, it is the file's like any other. An empty one the caret
            // left in a transaction that settles nothing stays, for the next
            // one to settle.
            Some(pending) if pending.is_empty() || caret_inside(&pending, tr) => {
                Pair::Pending(pending)
            }
            Some(span) => Pair::Left(span),
            // An undo or a redo that takes an empty pair out — the toggle's
            // own undo — remembers it as a deleted one, so the replay that
            // writes it back makes it pending again.
            None if pending.is_empty() && tr.replays_history() => {
                Settled::after(pending, tr.changes()).map_or(Pair::None, Pair::Settled)
            }
            None => Pair::None,
        },
        Pair::Left(span) => {
            // Undoing what was typed in it gives the empty pair back with the
            // caret between its runs, as it was before the typing.
            let undoing = tr.replays_history();
            match follow(schema, span, tr) {
                Some(pair) if undoing && pair.is_empty() && caret_inside(&pair, tr) => {
                    Pair::Pending(pair)
                }
                Some(span) => Pair::Left(span),
                None if undoing => reopened(span, tr).map_or(Pair::None, Pair::Pending),
                None => Pair::None,
            }
        }
        Pair::Settled(settled) => {
            if tr.replays_history()
                && let Some(restored) = settled.restored(tr)
            {
                return Pair::Pending(restored);
            }
            if !tr.doc_changed() {
                return value.clone();
            }
            settled.map(tr.changes()).map_or(Pair::None, Pair::Settled)
        }
    }
}

/// The pair after `tr`, while both of its runs are still there.
fn follow(schema: &Schema, pending: &Pending, tr: &Transaction) -> Option<Pending> {
    if tr.doc_changed() {
        pending.map(schema, tr.new_doc(), tr.changes())
    } else {
        Some(pending.clone())
    }
}

/// The empty pair of `span`'s layers an undo put back where `span` opens,
/// with the caret between its runs — when the undo also took back the runs
/// the filter moved, so the span itself could not be followed.
fn reopened(span: &Pending, tr: &Transaction) -> Option<Pending> {
    let schema = tr.start_state().schema();
    let doc = tr.new_doc();
    let selection = tr.new_selection();
    if !selection.is_cursor() {
        return None;
    }
    let pair = Pending::around(schema, doc, &span.layers, selection.head(doc))?;
    let start = tr
        .changes()
        .map_pos(span.open.start, -1, TrackMode::Simple)?;
    (pair.open.start == start).then_some(pair)
}

/// Whether `tr` leaves the caret between the runs of `pair`.
fn caret_inside(pair: &Pending, tr: &Transaction) -> bool {
    let selection = tr.new_selection();
    selection.is_cursor()
        && (pair.open.end..=pair.close.start).contains(&selection.head(tr.new_doc()))
}

/// Delete the empty pending pair a transaction leaves behind. See the module
/// documentation.
fn settle(tr: &Transaction) -> Option<Vec<TransactionSpec>> {
    if tr.annotation(&MARK).is_some() || !tr.recorded_in_history() {
        return None;
    }
    let state = tr.start_state();
    let pending = pending(state)?;
    if !pending.is_empty() {
        return reshape(tr, pending);
    }
    let schema = state.schema();
    let doc = tr.new_doc();
    let leftover = match pending.map(schema, doc, tr.changes()) {
        Some(mapped) => {
            let selection = tr.new_selection();
            let inside = selection.is_cursor() && mapped.caret() == Some(selection.head(doc));
            if inside || !mapped.is_empty() {
                return None;
            }
            vec![mapped.range()]
        }
        None => match eaten_into(tr, pending) {
            Some(rest) => vec![rest],
            None => {
                let (open, close) = split_apart(schema, doc, tr.changes(), pending)?;
                vec![open, close]
            }
        },
    };
    let deletions = leftover
        .into_iter()
        .map(|range| Change::delete(range.start, range.end));
    let mut cleanup = TransactionSpec::new()
        .changes(deletions)
        .sequential()
        .annotate(SETTLES.of(true));
    if !tr.doc_changed() {
        cleanup = cleanup.annotate(fold_into_previous().of(true));
    }
    Some(vec![tr.as_spec(), cleanup])
}

/// What is left of an empty pair a transaction did nothing but delete from,
/// in the document it produces.
fn eaten_into(tr: &Transaction, pending: &Pending) -> Option<Range<usize>> {
    let range = pending.range();
    let changes = tr.changes();
    let only_inside = changes.iter_changes().iter().all(|change| match change {
        ChangeRange::Replaced {
            from_a,
            to_a,
            inserted,
            ..
        } => range.start <= *from_a && *to_a <= range.end && inserted.is_empty(),
        ChangeRange::Marked { .. } => true,
    });
    if !tr.doc_changed() || !only_inside {
        return None;
    }
    let from = changes.map_pos(range.start, 1, TrackMode::Simple)?;
    let to = changes.map_pos(range.end, -1, TrackMode::Simple)?;
    (from < to).then_some(from..to)
}

/// Where the runs of an empty pair a transaction broke apart are, in the
/// document it produces: each run still spells what it did, but the two no
/// longer read as one pair — Enter between them put them in two textblocks.
fn split_apart(
    schema: &Schema,
    doc: &Node,
    changes: &ChangeSet,
    pending: &Pending,
) -> Option<(Range<usize>, Range<usize>)> {
    let (left, right) = runs(&pending.layers);
    // As in `Pending::map`: what is inserted at either end of a run is
    // outside it.
    let map = |range: &Range<usize>| -> Option<Range<usize>> {
        let from = changes.map_pos(range.start, 1, TrackMode::Simple)?;
        let to = changes.map_pos(range.end, -1, TrackMode::Simple)?;
        (from <= to).then_some(from..to)
    };
    let open = map(&pending.open)?;
    let close = map(&pending.close)?;
    let spells = |range: &Range<usize>, run: &str| {
        source(schema, doc, range.clone()).as_deref() == Some(run)
    };
    (open.end <= close.start && spells(&open, &left) && spells(&close, &right))
        .then_some((open, close))
}

/// Settle a pair that holds something as the caret leaves it, when it does
/// not read as its styles: its content ends — or starts — with whitespace,
/// where a reader takes no delimiter for a closing — or opening — one, so
/// `**ni **` is four literal asterisks. The runs move inside the whitespace,
/// `**ni** `, or go when that does not read either — only whitespace between
/// them — leaving the content.
fn reshape(tr: &Transaction, pending: &Pending) -> Option<Vec<TransactionSpec>> {
    let schema = tr.start_state().schema();
    let doc = tr.new_doc();
    let mapped = pending.map(schema, doc, tr.changes())?;
    if caret_inside(&mapped, tr) {
        return None;
    }
    let (changes, reshaped) = reshaped(schema, doc, &mapped)?;
    let mut cleanup = TransactionSpec::new().changes(changes).sequential();
    if let Some(pair) = reshaped {
        cleanup = cleanup.annotate(RESHAPED.of(pair));
    }
    if !tr.doc_changed() {
        cleanup = cleanup.annotate(fold_into_previous().of(true));
    }
    Some(vec![tr.as_spec(), cleanup])
}

/// Whether a pair that holds something reads as the styles its layers add.
pub(crate) fn reads_back(schema: &Schema, doc: &Node, pair: &Pending) -> bool {
    let Some((kind, start, text)) = block_text(schema, doc, pair.open.start) else {
        return true;
    };
    let local = |pos: usize| pos - start;
    let ctx = document_context(schema, doc);
    let open = local(pair.open.start);
    let content = local(pair.open.end)..local(pair.close.start);
    let close = local(pair.close.end);
    reads_as(kind, &text, &ctx, &pair.layers, open, content, close)
}

/// The change that makes a pair that does not read back one that does, and
/// the pair it makes — or, where none would, the change that deletes its
/// runs. `None` when it reads back as it is.
fn reshaped(schema: &Schema, doc: &Node, pair: &Pending) -> Option<(Vec<Change>, Option<Pending>)> {
    // A layer that takes a style off — `**ab**|**c**` — is two spans'
    // delimiters, read or not whatever is typed between them.
    if pair.layers.iter().any(|layer| !layer.adds) || reads_back(schema, doc, pair) {
        return None;
    }
    let (kind, start, text) = block_text(schema, doc, pair.open.start)?;
    let chars: Vec<char> = text.chars().collect();
    let local = |pos: usize| pos - start;
    let content = &chars[local(pair.open.end)..local(pair.close.start)];
    let lead = content.iter().take_while(|c| c.is_whitespace()).count();
    let trail = content[lead..]
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let (left, right) = runs(&pair.layers);
    let text_of = |chars: &[char]| chars.iter().collect::<String>();
    let slice = |text: String| Slice::from_fragment(Fragment::from_nodes(vec![schema.text(&text)]));
    let strip = vec![
        Change::delete(pair.open.start, pair.open.end),
        Change::delete(pair.close.start, pair.close.end),
    ];
    if lead + trail == 0 || lead == content.len() {
        return Some((strip, None));
    }
    let moved = Pending {
        layers: pair.layers.clone(),
        open: pair.open.start + lead..pair.open.end + lead,
        close: pair.close.start - trail..pair.close.end - trail,
    };
    let lead_text = text_of(&content[..lead]);
    let trail_text = text_of(&content[content.len() - trail..]);
    let spelled: String = chars[..local(pair.open.start)].iter().collect::<String>()
        + &lead_text
        + &left
        + &text_of(&content[lead..content.len() - trail])
        + &right
        + &trail_text
        + &text_of(&chars[local(pair.close.end)..]);
    let ctx = document_context(schema, doc);
    let reads = reads_as(
        kind,
        &spelled,
        &ctx,
        &moved.layers,
        local(moved.open.start),
        local(moved.open.end)..local(moved.close.start),
        local(moved.close.end),
    );
    if !reads {
        return Some((strip, None));
    }
    let mut changes = Vec::new();
    if lead > 0 {
        changes.push(Change::replace(
            pair.open.start,
            pair.open.end + lead,
            slice(lead_text + &left),
        ));
    }
    if trail > 0 {
        changes.push(Change::replace(
            pair.close.start - trail,
            pair.close.end,
            slice(right + &trail_text),
        ));
    }
    Some((changes, Some(moved)))
}

/// The kind, start position and source text of the textblock of inline
/// source `pos` is in.
fn block_text(schema: &Schema, doc: &Node, pos: usize) -> Option<(BlockKind, usize, String)> {
    let resolved = doc.resolve(pos).ok()?;
    let block = resolved.parent();
    let kind = block_kind(schema, block.type_id())?;
    let text = Items::from_nodes(schema, block.children()).text();
    Some((kind, resolved.start(resolved.depth()), text))
}

/// Whether `text` reads a span of each style `layers` add over `content`,
/// inside `open..close`: the pair's own spans, not ones around it.
fn reads_as(
    kind: BlockKind,
    text: &str,
    ctx: &DeriveContext,
    layers: &[Layer],
    open: usize,
    content: Range<usize>,
    close: usize,
) -> bool {
    let derived = derive(kind, text, ctx);
    layers.iter().filter(|layer| layer.adds).all(|layer| {
        derived.styles.iter().any(|span| {
            span.style == layer.style
                && open <= span.range.start
                && span.range.start <= content.start
                && content.end <= span.range.end
                && span.range.end <= close
        })
    })
}
