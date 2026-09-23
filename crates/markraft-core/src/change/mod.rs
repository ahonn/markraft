//! Changes in original-document coordinates.
//!
//! A [`ChangeSet`] divides the starting document into consecutive sections that
//! are kept, have marks modified, or are replaced by a run of tokens. Every
//! position in a [`Change`] refers to the *starting* document: changes in one
//! set never compensate for each other, so a caller can describe several edits
//! without doing position arithmetic.
//!
//! Structural edits are token edits. Splitting a block inserts a close and an
//! open token, joining deletes them, wrapping inserts an open token before and
//! a close token after a range, and lifting deletes the wrapper's two tokens.
//! When a caller cannot vouch for the result, [`Fit`] makes
//! [`ChangeSet::create`] repair the replacement.

pub(crate) mod apply;
mod compose;
mod json;
mod repair;
mod transform;

use crate::error::ChangeError;
use crate::fit::{Fit, fit_replacement};
use crate::mark::{Mark, MarkSet};
use crate::node::Node;
use crate::schema::{MarkTypeId, Schema};
use crate::slice::{Slice, Token, tokens_size};

/// A change to a document's marks over a range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkChange {
    /// Add this mark.
    Add(Mark),
    /// Remove this exact mark.
    Remove(Mark),
    /// Remove any mark of this type.
    RemoveType(MarkTypeId),
}

/// What a [`Change`] does to its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// Replace the range with a token run.
    Replace(Slice),
    /// Add a mark over the range.
    AddMark(Mark),
    /// Remove an exact mark over the range.
    RemoveMark(Mark),
    /// Remove every mark of a type over the range.
    RemoveMarkType(MarkTypeId),
    /// Make the range carry exactly this mark set.
    SetMarks(MarkSet),
}

/// One change, in starting-document coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Start of the affected range.
    pub from: usize,
    /// End of the affected range.
    pub to: usize,
    /// What to do with the range.
    pub kind: ChangeKind,
    /// Whether the model may repair the result.
    pub fit: Fit,
}

impl Change {
    /// Replace `from..to` with `slice`.
    pub fn replace(from: usize, to: usize, slice: Slice) -> Change {
        Change {
            from,
            to,
            kind: ChangeKind::Replace(slice),
            fit: Fit::No,
        }
    }

    /// Delete `from..to`.
    pub fn delete(from: usize, to: usize) -> Change {
        Change::replace(from, to, Slice::empty())
    }

    /// Insert `slice` at `pos`.
    pub fn insert(pos: usize, slice: Slice) -> Change {
        Change::replace(pos, pos, slice)
    }

    /// Add `mark` over `from..to`.
    pub fn add_mark(from: usize, to: usize, mark: Mark) -> Change {
        Change {
            from,
            to,
            kind: ChangeKind::AddMark(mark),
            fit: Fit::No,
        }
    }

    /// Remove `mark` over `from..to`.
    pub fn remove_mark(from: usize, to: usize, mark: Mark) -> Change {
        Change {
            from,
            to,
            kind: ChangeKind::RemoveMark(mark),
            fit: Fit::No,
        }
    }

    /// Remove every mark of type `ty` over `from..to`.
    pub fn remove_mark_type(from: usize, to: usize, ty: MarkTypeId) -> Change {
        Change {
            from,
            to,
            kind: ChangeKind::RemoveMarkType(ty),
            fit: Fit::No,
        }
    }

    /// Make every inline node in `from..to` carry exactly `marks`.
    ///
    /// Like the other mark changes this is resolved against the document when
    /// the set is created: each inline run records the removals and additions
    /// that take it from its current marks to `marks` (restricted to the mark
    /// types its parent allows), so inversion, composition and mapping treat
    /// it exactly like a sequence of [`Change::add_mark`] and
    /// [`Change::remove_mark`] calls — but a caller that knows the whole
    /// target set can express it as one change over one range, instead of
    /// several overlapping ones.
    pub fn set_marks(from: usize, to: usize, marks: MarkSet) -> Change {
        Change {
            from,
            to,
            kind: ChangeKind::SetMarks(marks),
            fit: Fit::No,
        }
    }

    /// Ask the model to repair this change so it produces a valid document.
    pub fn with_fit(mut self, fit: Fit) -> Change {
        self.fit = fit;
        self
    }

    /// Whether this change modifies marks rather than content.
    pub fn is_mark_change(&self) -> bool {
        !matches!(self.kind, ChangeKind::Replace(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SectionOp {
    Keep,
    Mark(Vec<MarkChange>),
    Replace(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// Length of the section in the starting document.
    pub(crate) len: usize,
    pub(crate) op: SectionOp,
}

impl Section {
    /// Length of the section in the resulting document.
    pub(crate) fn len_after(&self) -> usize {
        match &self.op {
            SectionOp::Keep | SectionOp::Mark(_) => self.len,
            SectionOp::Replace(tokens) => tokens_size(tokens),
        }
    }
}

/// Builds a canonical section list: no empty sections and no two adjacent
/// sections that could be merged.
#[derive(Debug, Default)]
pub(crate) struct SectionBuilder {
    sections: Vec<Section>,
}

impl SectionBuilder {
    pub(crate) fn new() -> SectionBuilder {
        SectionBuilder {
            sections: Vec::new(),
        }
    }

    pub(crate) fn keep(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        if let Some(last) = self.sections.last_mut()
            && last.op == SectionOp::Keep
        {
            last.len += len;
            return;
        }
        self.sections.push(Section {
            len,
            op: SectionOp::Keep,
        });
    }

    pub(crate) fn mark(&mut self, len: usize, mods: Vec<MarkChange>) {
        if len == 0 {
            return;
        }
        if mods.is_empty() {
            self.keep(len);
            return;
        }
        if let Some(last) = self.sections.last_mut()
            && let SectionOp::Mark(existing) = &last.op
            && *existing == mods
        {
            last.len += len;
            return;
        }
        self.sections.push(Section {
            len,
            op: SectionOp::Mark(mods),
        });
    }

    pub(crate) fn replace(&mut self, len: usize, tokens: Vec<Token>) {
        if len == 0 && tokens.is_empty() {
            return;
        }
        if let Some(last) = self.sections.last_mut()
            && let SectionOp::Replace(existing) = &mut last.op
        {
            last.len += len;
            existing.extend(tokens);
            return;
        }
        self.sections.push(Section {
            len,
            op: SectionOp::Replace(tokens),
        });
    }

    pub(crate) fn finish(mut self, schema: &Schema, len_before: usize) -> ChangeSet {
        let total: usize = self.sections.iter().map(|s| s.len).sum();
        debug_assert_eq!(total, len_before, "sections must cover the whole document");
        // Canonicalise inserted runs so that equal change sets compare equal
        // however they were built. Two runs that describe the same tokens can
        // differ in shape -- an explicit open/close pair around a node, or two
        // text nodes that should have been merged -- and round-tripping through
        // `Slice` picks one of them. The token count, and hence every length in
        // the set, is unaffected.
        for section in &mut self.sections {
            if let SectionOp::Replace(tokens) = &mut section.op
                && !tokens.is_empty()
            {
                let canonical = Slice::from_tokens(tokens).tokens();
                debug_assert_eq!(tokens_size(&canonical), tokens_size(tokens));
                *tokens = canonical;
            }
        }
        let len_after = self.sections.iter().map(Section::len_after).sum();
        ChangeSet {
            schema: schema.clone(),
            sections: self.sections,
            len_before,
            len_after,
        }
    }
}

/// How a position sticks to its surroundings when it is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackMode {
    /// Always return a position.
    #[default]
    Simple,
    /// Return `None` when the token before the position was deleted.
    Before,
    /// Return `None` when the token after the position was deleted.
    After,
    /// Return `None` when the tokens on both sides were deleted.
    Around,
}

/// The shape of a change set: positions and lengths only.
///
/// A `ChangeDesc` is enough to map positions, which is what selection,
/// decorations and the undo history need. It is cheap to keep around because
/// it carries no content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeDesc {
    /// `(length before, length after)`; `None` as the second element marks a
    /// section whose content is preserved.
    sections: Vec<(usize, Option<usize>)>,
    len_before: usize,
    len_after: usize,
}

impl ChangeDesc {
    /// A description that changes nothing in a document of `len` tokens.
    pub fn empty(len: usize) -> ChangeDesc {
        ChangeDesc {
            sections: if len == 0 {
                Vec::new()
            } else {
                vec![(len, None)]
            },
            len_before: len,
            len_after: len,
        }
    }

    /// Size of the starting document.
    pub fn length_before(&self) -> usize {
        self.len_before
    }

    /// Size of the resulting document.
    pub fn length_after(&self) -> usize {
        self.len_after
    }

    /// Whether nothing is replaced.
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|(_, ins)| ins.is_none())
    }

    /// Map a position from the starting document into the result.
    ///
    /// `assoc` decides which side the position sticks to when content is
    /// inserted exactly at it: `-1` keeps it before the new content, `1` moves
    /// it after. `track` makes the method report a deleted position as `None`.
    pub fn map_pos(&self, pos: usize, assoc: i32, track: TrackMode) -> Option<usize> {
        let mut pos_a = 0usize;
        let mut pos_b = 0usize;
        for (len, ins) in &self.sections {
            let end_a = pos_a + len;
            match ins {
                None => {
                    if end_a > pos {
                        return Some(pos_b + (pos - pos_a));
                    }
                    pos_b += len;
                }
                Some(ins) => {
                    if track != TrackMode::Simple
                        && end_a >= pos
                        && match track {
                            TrackMode::Around => pos_a < pos && end_a > pos,
                            TrackMode::Before => pos_a < pos,
                            TrackMode::After => end_a > pos,
                            TrackMode::Simple => false,
                        }
                    {
                        return None;
                    }
                    if end_a > pos || (end_a == pos && assoc < 0 && *len == 0) {
                        return Some(if pos == pos_a || assoc < 0 {
                            pos_b
                        } else {
                            pos_b + ins
                        });
                    }
                    pos_b += ins;
                }
            }
            pos_a = end_a;
        }
        if pos > pos_a { None } else { Some(pos_b) }
    }

    /// Map a range so that it never grows past the content it covered.
    ///
    /// The start sticks forward and the end sticks backward, so content
    /// inserted at either edge stays outside the range. When that makes the
    /// ends cross — the range's content is gone — the result collapses to an
    /// empty range at the start. An empty range in, such as a cursor, always
    /// gives an empty range out.
    pub fn map_range(&self, from: usize, to: usize) -> MappedRange {
        let deleted = self.touches(from, to);
        if from >= to {
            let at = self
                .map_pos(from, -1, TrackMode::Simple)
                .unwrap_or(self.len_after);
            return MappedRange {
                from: at,
                to: at,
                deleted,
            };
        }
        let mapped_from = self.map_pos(from, 1, TrackMode::Simple).unwrap_or(0);
        let mapped_to = self
            .map_pos(to, -1, TrackMode::Simple)
            .unwrap_or(self.len_after);
        MappedRange {
            from: mapped_from,
            to: mapped_to.max(mapped_from),
            deleted,
        }
    }

    /// Whether any replaced range overlaps or is adjacent to `from..to`.
    pub fn touches(&self, from: usize, to: usize) -> bool {
        let mut pos_a = 0usize;
        for (len, ins) in &self.sections {
            let end_a = pos_a + len;
            if ins.is_some() && end_a >= from && pos_a <= to {
                return true;
            }
            pos_a = end_a;
        }
        false
    }
}

/// A range mapped through a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedRange {
    /// Mapped start.
    pub from: usize,
    /// Mapped end.
    pub to: usize,
    /// Whether the change touched the range.
    pub deleted: bool,
}

/// A set of changes over one starting document.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    schema: Schema,
    pub(crate) sections: Vec<Section>,
    len_before: usize,
    len_after: usize,
}

impl PartialEq for ChangeSet {
    fn eq(&self, other: &Self) -> bool {
        self.len_before == other.len_before && self.sections == other.sections
    }
}

impl Eq for ChangeSet {}

impl ChangeSet {
    /// A change set that changes nothing in a document of `len` tokens.
    pub fn empty(schema: &Schema, len: usize) -> ChangeSet {
        let mut builder = SectionBuilder::new();
        builder.keep(len);
        builder.finish(schema, len)
    }

    /// Create a change set over `doc`.
    ///
    /// Changes are sorted by position and must not overlap. Changes carrying a
    /// [`Fit`] other than [`Fit::No`] are repaired against the document, which
    /// may widen their range or add tokens elsewhere; the repair is recorded as
    /// part of the change set, so mapping and inversion stay exact.
    ///
    /// Changes with [`Fit::No`] are taken at face value. Creating a set that
    /// would not produce a well-formed document succeeds; [`ChangeSet::apply`]
    /// reports the problem.
    ///
    /// Mark changes are resolved against `doc` here: a modification is recorded
    /// only over the inline content whose parent allows that mark type, and
    /// **dropped** everywhere else — adding emphasis across a paragraph and a
    /// code block marks the paragraph's text and leaves the code alone. The set
    /// therefore describes exactly what it does, which is what keeps
    /// [`ChangeSet::invert`] exact and lets [`ChangeSet::apply`] and
    /// [`ChangeSet::compose`] work without re-deriving each node's parent.
    pub fn create(
        schema: &Schema,
        doc: &Node,
        changes: impl IntoIterator<Item = Change>,
    ) -> Result<ChangeSet, ChangeError> {
        let size = doc.content_size();
        let mut changes: Vec<Change> = changes.into_iter().collect();
        for change in &changes {
            if change.from > change.to || change.to > size {
                return Err(ChangeError::BadRange {
                    from: change.from,
                    to: change.to,
                    size,
                });
            }
        }
        changes.sort_by_key(|c| (c.from, c.to));
        for pair in changes.windows(2) {
            if pair[0].to > pair[1].from {
                return Err(ChangeError::Overlapping {
                    a_from: pair[0].from,
                    a_to: pair[0].to,
                    b_from: pair[1].from,
                    b_to: pair[1].to,
                });
            }
        }

        // Turn every change into concrete parts in document coordinates. A
        // part remembers the change it came from, so a repair that widens one
        // change over another can say so instead of blaming the caller's
        // ranges.
        enum Part {
            Replace(usize, usize, Vec<Token>, (usize, usize)),
            Mark(usize, usize, Vec<MarkChange>),
        }
        /// Record a mark change, split into the runs whose parent allows it.
        fn push_mark_parts(
            schema: &Schema,
            doc: &Node,
            from: usize,
            to: usize,
            modification: MarkChange,
            parts: &mut Vec<Part>,
        ) -> Result<(), ChangeError> {
            let mods = [modification];
            let runs = crate::change::apply::split_mark_change(schema, doc, from, to, &mods)?;
            push_mark_runs(from, runs, parts);
            Ok(())
        }
        /// Record resolved runs, merging neighbours that do the same thing.
        fn push_mark_runs(from: usize, runs: Vec<(usize, Vec<MarkChange>)>, parts: &mut Vec<Part>) {
            let mut pos = from;
            for (len, mods) in runs {
                if !mods.is_empty() {
                    if let Some(Part::Mark(_, end, last)) = parts.last_mut()
                        && *end == pos
                        && *last == mods
                    {
                        *end = pos + len;
                    } else {
                        parts.push(Part::Mark(pos, pos + len, mods));
                    }
                }
                pos += len;
            }
        }

        let mut parts: Vec<Part> = Vec::new();
        for change in changes {
            let (from, to) = (change.from, change.to);
            match change.kind {
                ChangeKind::Replace(slice) => {
                    let asked = (change.from, change.to);
                    let fitted =
                        fit_replacement(schema, doc, change.from, change.to, &slice, &change.fit)?;
                    for (from, to, tokens) in fitted {
                        parts.push(Part::Replace(from, to, tokens, asked));
                    }
                }
                ChangeKind::AddMark(mark) => {
                    push_mark_parts(schema, doc, from, to, MarkChange::Add(mark), &mut parts)?;
                }
                ChangeKind::RemoveMark(mark) => {
                    push_mark_parts(schema, doc, from, to, MarkChange::Remove(mark), &mut parts)?;
                }
                ChangeKind::RemoveMarkType(ty) => {
                    push_mark_parts(
                        schema,
                        doc,
                        from,
                        to,
                        MarkChange::RemoveType(ty),
                        &mut parts,
                    )?;
                }
                ChangeKind::SetMarks(marks) => {
                    let runs =
                        crate::change::apply::split_set_marks(schema, doc, from, to, &marks)?;
                    push_mark_runs(from, runs, &mut parts);
                }
            }
        }
        parts.sort_by_key(|part| match part {
            Part::Replace(from, to, _, _) | Part::Mark(from, to, _) => (*from, *to),
        });
        for pair in parts.windows(2) {
            let (a_from, a_to, a_asked) = match &pair[0] {
                Part::Replace(from, to, _, asked) => (*from, *to, Some(*asked)),
                Part::Mark(from, to, _) => (*from, *to, None),
            };
            let (b_from, b_to, b_asked) = match &pair[1] {
                Part::Replace(from, to, _, asked) => (*from, *to, Some(*asked)),
                Part::Mark(from, to, _) => (*from, *to, None),
            };
            if a_to <= b_from {
                continue;
            }
            // Only a repair can produce a part wider than the change it came
            // from; report that rather than an overlap the caller never wrote.
            let widened = [(a_from, a_to, a_asked), (b_from, b_to, b_asked)]
                .into_iter()
                .find_map(|(from, to, asked)| {
                    asked.filter(|(af, at)| *af != from || *at != to).map(
                        |(asked_from, asked_to)| ChangeError::FitConflict {
                            from: asked_from,
                            to: asked_to,
                            fitted_from: from,
                            fitted_to: to,
                        },
                    )
                });
            return Err(widened.unwrap_or(ChangeError::Overlapping {
                a_from,
                a_to,
                b_from,
                b_to,
            }));
        }

        let mut builder = SectionBuilder::new();
        let mut pos = 0usize;
        for part in parts {
            let (from, to) = match &part {
                Part::Replace(from, to, _, _) | Part::Mark(from, to, _) => (*from, *to),
            };
            builder.keep(from - pos);
            match part {
                Part::Replace(_, _, tokens, _) => builder.replace(to - from, tokens),
                Part::Mark(_, _, mods) => builder.mark(to - from, mods),
            }
            pos = to;
        }
        builder.keep(size - pos);
        Ok(builder.finish(schema, size))
    }

    /// The schema this set was created against.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Size of the starting document.
    pub fn length_before(&self) -> usize {
        self.len_before
    }

    /// Size of the resulting document.
    pub fn length_after(&self) -> usize {
        self.len_after
    }

    /// Whether the set changes nothing.
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.op == SectionOp::Keep)
    }

    /// The set's shape, without content.
    pub fn desc(&self) -> ChangeDesc {
        ChangeDesc {
            sections: self
                .sections
                .iter()
                .map(|s| match &s.op {
                    SectionOp::Keep | SectionOp::Mark(_) => (s.len, None),
                    SectionOp::Replace(tokens) => (s.len, Some(tokens_size(tokens))),
                })
                .collect(),
            len_before: self.len_before,
            len_after: self.len_after,
        }
    }

    /// Map a position through this change. See [`ChangeDesc::map_pos`].
    pub fn map_pos(&self, pos: usize, assoc: i32, track: TrackMode) -> Option<usize> {
        self.desc().map_pos(pos, assoc, track)
    }

    /// Map a range through this change. See [`ChangeDesc::map_range`].
    pub fn map_range(&self, from: usize, to: usize) -> MappedRange {
        self.desc().map_range(from, to)
    }

    /// Whether any replaced range overlaps or is adjacent to `from..to`.
    pub fn touches(&self, from: usize, to: usize) -> bool {
        self.desc().touches(from, to)
    }

    /// The ranges this set changes, in starting-document order.
    pub fn iter_changes(&self) -> Vec<ChangeRange> {
        let mut out = Vec::new();
        let mut pos_a = 0usize;
        let mut pos_b = 0usize;
        for section in &self.sections {
            let end_a = pos_a + section.len;
            let end_b = pos_b + section.len_after();
            match &section.op {
                SectionOp::Keep => {}
                SectionOp::Mark(mods) => out.push(ChangeRange::Marked {
                    from_a: pos_a,
                    to_a: end_a,
                    from_b: pos_b,
                    to_b: end_b,
                    mods: mods.clone(),
                }),
                SectionOp::Replace(tokens) => out.push(ChangeRange::Replaced {
                    from_a: pos_a,
                    to_a: end_a,
                    from_b: pos_b,
                    to_b: end_b,
                    inserted: Slice::from_tokens(tokens),
                }),
            }
            pos_a = end_a;
            pos_b = end_b;
        }
        out
    }
}

/// One changed range, as reported by [`ChangeSet::iter_changes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeRange {
    /// A range replaced by new content.
    Replaced {
        /// Start in the starting document.
        from_a: usize,
        /// End in the starting document.
        to_a: usize,
        /// Start in the resulting document.
        from_b: usize,
        /// End in the resulting document.
        to_b: usize,
        /// The inserted token run.
        inserted: Slice,
    },
    /// A range whose marks were modified.
    Marked {
        /// Start in the starting document.
        from_a: usize,
        /// End in the starting document.
        to_a: usize,
        /// Start in the resulting document.
        from_b: usize,
        /// End in the resulting document.
        to_b: usize,
        /// The modifications, applied in order.
        mods: Vec<MarkChange>,
    },
}
