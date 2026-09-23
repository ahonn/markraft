//! Composing two change sets that run one after the other.

use crate::error::ChangeError;
use crate::slice::{Token, tokens_cut, tokens_size};

use super::apply::mark_tokens;
use super::{ChangeSet, MarkChange, SectionBuilder, SectionOp};

/// One stretch of the intermediate document produced by the first change.
enum Item {
    /// Content carried over from the starting document.
    Pass { x_len: usize, mods: Vec<MarkChange> },
    /// Content the first change inserted, replacing `x_len` starting tokens.
    Ins { x_len: usize, tokens: Vec<Token> },
}

impl Item {
    fn y_len(&self) -> usize {
        match self {
            Item::Pass { x_len, .. } => *x_len,
            Item::Ins { tokens, .. } => tokens_size(tokens),
        }
    }
}

impl ChangeSet {
    /// Compose two change sets, where `other` starts from the document this one
    /// produces.
    ///
    /// `a.compose(b).apply(doc) == b.apply(a.apply(doc))`.
    #[allow(unused_assignments)]
    pub fn compose(&self, other: &ChangeSet) -> Result<ChangeSet, ChangeError> {
        if !self.schema.same(&other.schema) {
            return Err(ChangeError::SchemaMismatch);
        }
        if self.length_after() != other.length_before() {
            return Err(ChangeError::LengthMismatch {
                expected: self.length_after(),
                actual: other.length_before(),
            });
        }
        let items: Vec<Item> = self
            .sections
            .iter()
            .map(|section| match &section.op {
                SectionOp::Keep => Item::Pass {
                    x_len: section.len,
                    mods: Vec::new(),
                },
                SectionOp::Mark(mods) => Item::Pass {
                    x_len: section.len,
                    mods: mods.clone(),
                },
                SectionOp::Replace(tokens) => Item::Ins {
                    x_len: section.len,
                    tokens: tokens.clone(),
                },
            })
            .collect();

        let mut out = SectionBuilder::new();
        let mut pending_x = 0usize;
        let mut pending_tokens: Vec<Token> = Vec::new();
        let mut pending = false;
        let mut ai = 0usize;
        let mut a_off = 0usize;
        let mut ins_x_taken = false;
        let mut bi = 0usize;
        let mut b_off = 0usize;
        let mut b_pushed = false;

        macro_rules! flush {
            () => {
                if pending {
                    out.replace(pending_x, std::mem::take(&mut pending_tokens));
                    pending_x = 0;
                    pending = false;
                }
            };
        }

        loop {
            let a_done = ai >= items.len();
            let b_done = bi >= other.sections.len();
            if a_done && b_done {
                break;
            }
            // A stretch the first change deleted outright carries only length.
            if !a_done && items[ai].y_len() == 0 {
                if let Item::Ins { x_len, .. } = &items[ai] {
                    pending_x += x_len;
                    pending = true;
                }
                ai += 1;
                a_off = 0;
                ins_x_taken = false;
                continue;
            }
            // A pure insertion by the second change.
            if !b_done && other.sections[bi].len == 0 {
                if let SectionOp::Replace(tokens) = &other.sections[bi].op {
                    pending_tokens.extend(tokens.iter().cloned());
                    pending = true;
                }
                bi += 1;
                b_off = 0;
                b_pushed = false;
                continue;
            }
            if a_done || b_done {
                return Err(ChangeError::LengthMismatch {
                    expected: self.length_after(),
                    actual: other.length_before(),
                });
            }
            // The second change's replacement content enters the buffer once,
            // at the start of its section, so it lands before any later
            // surviving content of the first change.
            if b_off == 0 && !b_pushed {
                if let SectionOp::Replace(tokens) = &other.sections[bi].op {
                    pending_tokens.extend(tokens.iter().cloned());
                    pending = true;
                }
                b_pushed = true;
            }

            let a_len = items[ai].y_len();
            let n = (a_len - a_off).min(other.sections[bi].len - b_off);
            match (&items[ai], &other.sections[bi].op) {
                (Item::Pass { mods, .. }, SectionOp::Keep) => {
                    flush!();
                    out.mark(n, mods.clone());
                }
                (Item::Pass { mods, .. }, SectionOp::Mark(mb)) => {
                    flush!();
                    let mut combined = mods.clone();
                    combined.extend(mb.iter().cloned());
                    out.mark(n, combined);
                }
                (Item::Pass { .. }, SectionOp::Replace(_)) => {
                    pending_x += n;
                    pending = true;
                }
                (Item::Ins { x_len, tokens }, op_b) => {
                    if !ins_x_taken {
                        pending_x += x_len;
                        ins_x_taken = true;
                        pending = true;
                    }
                    match op_b {
                        SectionOp::Keep => {
                            pending_tokens.extend(tokens_cut(tokens, a_off, a_off + n));
                            pending = true;
                        }
                        SectionOp::Mark(mb) => {
                            let sub = tokens_cut(tokens, a_off, a_off + n);
                            pending_tokens.extend(mark_tokens(&self.schema, &sub, mb));
                            pending = true;
                        }
                        SectionOp::Replace(_) => {}
                    }
                }
            }
            a_off += n;
            b_off += n;
            if a_off == a_len {
                ai += 1;
                a_off = 0;
                ins_x_taken = false;
            }
            if b_off == other.sections[bi].len {
                bi += 1;
                b_off = 0;
                b_pushed = false;
            }
        }
        flush!();
        Ok(out
            .finish(&self.schema, self.length_before())
            .with_dropped_tokens(self.dropped_tokens() + other.dropped_tokens()))
    }
}
