//! Making a change set that was built for one document work on another.
//!
//! Rebasing a change over a concurrent one can invalidate it: the content a
//! mark change covers may have moved into a parent that forbids the mark, and a
//! token run that balanced against the original document may no longer balance
//! once the other change has opened or closed containers around it. Repairing
//! resolves both, so that applying a transformed change set always produces a
//! valid document.

use crate::error::ChangeError;
use crate::fit::Fit;
use crate::node::Node;
use crate::slice::{Slice, Token};

use super::apply::{doc_token_run, mark_tokens, split_mark_change};
use super::{Change, ChangeSet, SectionBuilder, SectionOp};

impl ChangeSet {
    /// Return a version of this set that applies to `doc` and leaves a valid
    /// document behind.
    ///
    /// `doc` must have the size this set starts from. When the set is already
    /// sound the sections are returned as they are, so mapping stays precise.
    /// Otherwise the touched span is collapsed into a single replacement and
    /// repaired with [`Fit::Auto`]: the content the set wanted to produce is
    /// kept, the structure around it is made valid, and position mapping inside
    /// the span becomes coarse.
    ///
    /// Verifying soundness costs one application plus one validation of the
    /// result, so this is meant for rebasing, not for every edit.
    pub fn repair_against(&self, doc: &Node) -> Result<ChangeSet, ChangeError> {
        if doc.content_size() != self.len_before {
            return Err(ChangeError::LengthMismatch {
                expected: self.len_before,
                actual: doc.content_size(),
            });
        }
        let filtered = self.refilter_marks(doc)?;
        if let Ok(result) = filtered.apply(doc)
            && result.check(&self.schema).is_ok()
        {
            return Ok(filtered);
        }
        let Some((lo, hi)) = filtered.changed_span() else {
            return Ok(filtered);
        };
        let run = filtered.result_run(doc, lo, hi)?;
        ChangeSet::create(
            &self.schema,
            doc,
            [Change::replace(lo, hi, Slice::from_tokens(&run)).with_fit(Fit::Auto)],
        )
    }

    /// Re-resolve every mark section against the parents the content has in
    /// `doc`, dropping modifications those parents do not allow.
    fn refilter_marks(&self, doc: &Node) -> Result<ChangeSet, ChangeError> {
        if !self
            .sections
            .iter()
            .any(|section| matches!(section.op, SectionOp::Mark(_)))
        {
            return Ok(self.clone());
        }
        let mut builder = SectionBuilder::new();
        let mut pos = 0usize;
        for section in &self.sections {
            let end = pos + section.len;
            match &section.op {
                SectionOp::Keep => builder.keep(section.len),
                SectionOp::Replace(tokens) => builder.replace(section.len, tokens.clone()),
                SectionOp::Mark(mods) => {
                    for (len, allowed) in split_mark_change(&self.schema, doc, pos, end, mods)? {
                        builder.mark(len, allowed);
                    }
                }
            }
            pos = end;
        }
        Ok(builder.finish(&self.schema, self.len_before))
    }

    /// The tokens this set produces for the span `lo..hi` of `doc`.
    ///
    /// The run may not balance — that is exactly the case the caller is about
    /// to hand to the fitter.
    fn result_run(&self, doc: &Node, lo: usize, hi: usize) -> Result<Vec<Token>, ChangeError> {
        let mut out: Vec<Token> = Vec::new();
        let mut pos = 0usize;
        for section in &self.sections {
            let end = pos + section.len;
            match &section.op {
                SectionOp::Keep | SectionOp::Mark(_) => {
                    let start = pos.max(lo);
                    let stop = end.min(hi);
                    if start < stop {
                        let run = doc_token_run(doc, start, stop)?;
                        match &section.op {
                            SectionOp::Mark(mods) => {
                                out.extend(mark_tokens(&self.schema, &run, mods))
                            }
                            _ => out.extend(run),
                        }
                    }
                }
                SectionOp::Replace(tokens) => out.extend(tokens.iter().cloned()),
            }
            pos = end;
        }
        Ok(out)
    }
}
