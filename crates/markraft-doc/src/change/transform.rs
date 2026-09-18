//! Operational transformation of two change sets over the same document.

use crate::error::ChangeError;
use crate::node::Node;
use crate::schema::MarkTypeId;
use crate::slice::{Token, tokens_size};

use super::apply::mark_change_type;
use super::{ChangeSet, MarkChange, SectionBuilder, SectionOp};

impl ChangeSet {
    /// Rebase this change set onto the document `other` produces.
    ///
    /// All three of `doc`, this set and `other` describe the same starting
    /// document. `before` decides whose content comes first when both insert at
    /// the same position: with `true`, this set's insertions land before
    /// `other`'s.
    ///
    /// Content `other` inserted is never deleted by the result, so a deletion
    /// that spans an insertion point is split around it. This is what makes the
    /// two directions converge.
    ///
    /// The rebased set is then repaired against the document `other` produces,
    /// so that [`ChangeSet::apply`] never rejects it: mark changes are resolved
    /// against the parents the content now has, and a token run that no longer
    /// balances is refitted. See [`ChangeSet::transform`] for what that costs
    /// when the two changes genuinely conflict.
    pub fn transform_over(
        &self,
        doc: &Node,
        other: &ChangeSet,
        before: bool,
    ) -> Result<ChangeSet, ChangeError> {
        if doc.content_size() != self.len_before {
            return Err(ChangeError::LengthMismatch {
                expected: self.len_before,
                actual: doc.content_size(),
            });
        }
        let rebased = self.rebase(other, before)?;
        let target = other.apply(doc)?;
        rebased.repair_against(&target)
    }

    /// The raw rebase, before any repair.
    #[allow(unused_assignments)]
    fn rebase(&self, other: &ChangeSet, before: bool) -> Result<ChangeSet, ChangeError> {
        if !self.schema.same(&other.schema) {
            return Err(ChangeError::SchemaMismatch);
        }
        if self.len_before != other.len_before {
            return Err(ChangeError::LengthMismatch {
                expected: self.len_before,
                actual: other.len_before,
            });
        }
        let mut out = SectionBuilder::new();
        let mut pending: Vec<Token> = Vec::new();
        let mut has_pending = false;
        let mut ai = 0usize;
        let mut a_off = 0usize;
        let mut a_staged = false;
        let mut bi = 0usize;
        let mut b_off = 0usize;
        let mut b_kept = false;

        macro_rules! flush {
            () => {
                if has_pending {
                    out.replace(0, std::mem::take(&mut pending));
                    has_pending = false;
                }
            };
        }

        loop {
            let a_done = ai >= self.sections.len();
            let b_done = bi >= other.sections.len();
            if a_done && b_done {
                break;
            }
            // Stage this set's inserted content at the position it belongs to.
            if !a_done && a_off == 0 && !a_staged {
                if let SectionOp::Replace(tokens) = &self.sections[ai].op
                    && !tokens.is_empty()
                {
                    pending.extend(tokens.iter().cloned());
                    has_pending = true;
                }
                a_staged = true;
            }
            // Content the other change inserted has to be skipped over.
            if !b_done && b_off == 0 && !b_kept {
                if let SectionOp::Replace(tokens) = &other.sections[bi].op {
                    let len = tokens_size(tokens);
                    if len > 0 {
                        if before {
                            flush!();
                        }
                        out.keep(len);
                    }
                }
                b_kept = true;
            }
            if !b_done && other.sections[bi].len == 0 {
                bi += 1;
                b_off = 0;
                b_kept = false;
                continue;
            }
            if !a_done && self.sections[ai].len == 0 {
                flush!();
                ai += 1;
                a_off = 0;
                a_staged = false;
                continue;
            }
            if a_done || b_done {
                return Err(ChangeError::LengthMismatch {
                    expected: self.len_before,
                    actual: other.len_before,
                });
            }

            let a_len = self.sections[ai].len;
            let b_len = other.sections[bi].len;
            let n = (a_len - a_off).min(b_len - b_off);
            if matches!(other.sections[bi].op, SectionOp::Replace(_)) {
                // The other change deleted this range; nothing of ours survives
                // here, but staged insertions stay pending.
            } else {
                match &self.sections[ai].op {
                    SectionOp::Keep => {
                        flush!();
                        out.keep(n);
                    }
                    SectionOp::Mark(mods) => {
                        flush!();
                        let mods = match (&other.sections[bi].op, before) {
                            // Both sides modify the same marks here. The set
                            // that is ordered last wins, so the other one drops
                            // the conflicting modifications; unrelated mark
                            // types survive on both sides.
                            (SectionOp::Mark(theirs), true) => self.drop_conflicting(mods, theirs),
                            _ => mods.clone(),
                        };
                        out.mark(n, mods);
                    }
                    SectionOp::Replace(_) => {
                        out.replace(n, std::mem::take(&mut pending));
                        has_pending = false;
                    }
                }
            }
            a_off += n;
            b_off += n;
            if a_off == a_len {
                ai += 1;
                a_off = 0;
                a_staged = false;
            }
            if b_off == b_len {
                bi += 1;
                b_off = 0;
                b_kept = false;
            }
        }
        flush!();
        Ok(out.finish(&self.schema, other.len_after))
    }

    /// Keep only the modifications that do not fight with `theirs`.
    ///
    /// Two modifications fight when they touch the same mark type, or when one
    /// mark type excludes the other, because then the order in which they are
    /// applied decides the result.
    fn drop_conflicting(&self, ours: &[MarkChange], theirs: &[MarkChange]) -> Vec<MarkChange> {
        let their_types: Vec<MarkTypeId> = theirs.iter().map(mark_change_type).collect();
        ours.iter()
            .filter(|change| {
                let ty = mark_change_type(change);
                !their_types.iter().any(|other| {
                    *other == ty
                        || self.schema.mark_type(*other).excludes(ty)
                        || self.schema.mark_type(ty).excludes(*other)
                })
            })
            .cloned()
            .collect()
    }

    /// Transform two change sets that start from the same document over each
    /// other.
    ///
    /// Returns `(this over other, other over this)`. Both results apply
    /// successfully and produce valid documents. `before` decides which set's
    /// insertions come first when the two insert at the same position.
    ///
    /// For changes that do not conflict structurally the two orders converge on
    /// the same document. They can differ when both sides edit the same node
    /// boundaries in incompatible ways — see the crate documentation's
    /// "Known limitations" for the exact class.
    pub fn transform(
        &self,
        doc: &Node,
        other: &ChangeSet,
        before: bool,
    ) -> Result<(ChangeSet, ChangeSet), ChangeError> {
        Ok((
            self.transform_over(doc, other, before)?,
            other.transform_over(doc, self, !before)?,
        ))
    }
}
