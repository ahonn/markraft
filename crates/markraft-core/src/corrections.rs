//! Corrections: per-node-type observers that repair a transaction's result.
//!
//! The content rules a schema can express are deliberately simple, and an
//! editing command is allowed to produce an intermediate shape that breaks a
//! richer constraint. A correction watches nodes of one type, is called when a
//! transaction touches one, and may return changes that fix it up.
//!
//! Corrections are registered in the [`correction`] facet and run by a single
//! [`transaction_extender`](crate::transaction_extender), whatever number of
//! [`corrections`] extensions a configuration holds. Their changes are merged
//! sequentially, so the positions they return refer to the document the
//! transaction produces ([`Transaction::new_doc`]) and compose exactly with the
//! transaction's own changes — undoing the transaction undoes its corrections
//! with it.
//!
//! # Running to a fixed point
//!
//! One correction's output is another's input: filling in a required child can
//! be what makes a wrapper's child list wrong. Corrections are therefore run
//! repeatedly against the document produced so far, and the rounds are composed
//! into one change set, until a round asks for nothing.
//!
//! A pair of corrections that undo each other would never settle, so the loop
//! stops after [`MAX_CORRECTION_ROUNDS`] rounds. What it produced is still
//! applied — the alternative is refusing the user's edit — and the transaction
//! is annotated [`corrections_diverged`], which is how a caller notices.
//!
//! They do **not** run for a transaction annotated
//! [`remote(true)`](crate::remote): correcting another peer's edit makes every
//! peer correct the same thing, which cascades.
//!
//! ```
//! # use markraft_core::*;
//! # let schema = Schema::new(SchemaSpec::new()
//! #     .node(NodeTypeSpec::new("doc", "block+"))
//! #     .node(NodeTypeSpec::new("paragraph", "inline*").group("block"))
//! #     .node(NodeTypeSpec::text("text").group("inline"))).unwrap();
//! let doc_type = schema.node_id("doc").unwrap();
//! let extension = corrections([fill_required_content(doc_type)]);
//! # let _ = extension;
//! ```

use std::sync::{Arc, LazyLock};

use crate::change::{Change, ChangeRange, ChangeSet};
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::schema::NodeTypeId;
use crate::slice::Slice;
use crate::state::{
    AnnotationType, EditorState, Extension, Facet, Transaction, TransactionExtenderFn,
    TransactionSpec, remote, transaction_extender,
};

/// How many times corrections are re-run against their own output before the
/// loop gives up. See the module documentation.
pub const MAX_CORRECTION_ROUNDS: usize = 8;

/// When a [`Correction`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionTrigger {
    /// The node's direct child list changed, or the node was inserted.
    ChildList,
    /// Anything inside the node changed, however deep.
    Content,
    /// The marks on content directly inside the node changed.
    Marks,
}

/// What a correction is told about the node it was called for.
pub struct CorrectionContext<'a> {
    /// The state the transaction started from — the transaction's *result*
    /// state does not exist yet, and asking for it here would build a state
    /// that is then thrown away.
    pub start_state: &'a EditorState,
    /// The transaction being extended.
    pub tr: &'a Transaction,
    /// The document the transaction produces. Every position the correction
    /// returns refers to this document.
    pub doc: &'a Node,
    /// The matched node.
    pub node: &'a Node,
    /// The position of the first token inside `node`.
    pub content_start: usize,
    /// The position directly before `node`, or `None` for the top node.
    pub before: Option<usize>,
}

type CorrectFn = Arc<dyn Fn(&CorrectionContext<'_>) -> Vec<Change> + Send + Sync>;

/// An observer on one node type.
#[derive(Clone)]
pub struct Correction {
    node_type: NodeTypeId,
    trigger: CorrectionTrigger,
    correct: CorrectFn,
}

impl std::fmt::Debug for Correction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Correction")
            .field("node_type", &self.node_type)
            .field("trigger", &self.trigger)
            .finish()
    }
}

impl Correction {
    /// Run `correct` when a node of this type has its direct child list changed
    /// or is inserted.
    pub fn on_child_list(
        node_type: NodeTypeId,
        correct: impl Fn(&CorrectionContext<'_>) -> Vec<Change> + Send + Sync + 'static,
    ) -> Correction {
        Correction {
            node_type,
            trigger: CorrectionTrigger::ChildList,
            correct: Arc::new(correct),
        }
    }

    /// Run `correct` when anything inside a node of this type changes.
    pub fn on_content(
        node_type: NodeTypeId,
        correct: impl Fn(&CorrectionContext<'_>) -> Vec<Change> + Send + Sync + 'static,
    ) -> Correction {
        Correction {
            node_type,
            trigger: CorrectionTrigger::Content,
            correct: Arc::new(correct),
        }
    }

    /// Run `correct` when the marks on content directly inside a node of this
    /// type change.
    pub fn on_marks(
        node_type: NodeTypeId,
        correct: impl Fn(&CorrectionContext<'_>) -> Vec<Change> + Send + Sync + 'static,
    ) -> Correction {
        Correction {
            node_type,
            trigger: CorrectionTrigger::Marks,
            correct: Arc::new(correct),
        }
    }

    /// The type this correction watches.
    pub fn node_type(&self) -> NodeTypeId {
        self.node_type
    }

    /// When this correction runs.
    pub fn trigger(&self) -> CorrectionTrigger {
        self.trigger
    }
}

static CORRECTION: LazyLock<Facet<Correction>> = LazyLock::new(Facet::list);
static DIVERGED: LazyLock<AnnotationType<bool>> = LazyLock::new(AnnotationType::define);
static RUNNER: LazyLock<Extension> = LazyLock::new(|| {
    let run: TransactionExtenderFn = Arc::new(run_corrections);
    transaction_extender().of(run)
});

/// The facet corrections are registered in.
///
/// [`corrections`] is the usual way to add them; this is here for a consumer
/// that wants to place one at a specific precedence.
pub fn correction() -> &'static Facet<Correction> {
    &CORRECTION
}

/// Set on a transaction whose corrections still wanted to change something
/// after [`MAX_CORRECTION_ROUNDS`] rounds.
pub fn corrections_diverged() -> &'static AnnotationType<bool> {
    &DIVERGED
}

/// An extension that runs `corrections` on every local transaction.
///
/// Including this more than once adds more corrections but still only one
/// extender, so the rounds below stay a single fixed-point loop over all of
/// them.
///
/// The changes it contributes address [`Transaction::new_doc`]. A configuration
/// that also registers a [`transaction_extender`](crate::transaction_extender)
/// which changes the document must give that extender a *higher* precedence, so
/// corrections run first; otherwise the transaction is rejected with a length
/// mismatch rather than silently mis-positioned.
pub fn corrections(corrections: impl IntoIterator<Item = Correction>) -> Extension {
    let mut items: Vec<Extension> = corrections
        .into_iter()
        .map(|correction| CORRECTION.of(correction))
        .collect();
    items.push(RUNNER.clone());
    Extension::all(items)
}

/// Run every registered correction against the transaction's result, then
/// against its own output, until nothing more is asked for.
fn run_corrections(tr: &Transaction) -> Option<TransactionSpec> {
    let list = tr.start_state().facet(correction()).clone();
    if !corrections_apply(&list, tr) {
        return None;
    }
    let schema = tr.start_state().schema().clone();
    let mut doc = tr.new_doc().clone();
    let (mut replaced, mut marked) = changed_ranges(tr.changes());
    let mut accumulated: Option<ChangeSet> = None;
    let mut settled = false;
    for _ in 0..MAX_CORRECTION_ROUNDS {
        let changes = corrections_for(&list, tr, &doc, &replaced, &marked);
        if changes.is_empty() {
            settled = true;
            break;
        }
        // Two corrections asking for overlapping changes cannot be expressed as
        // one set. Keep what settled so far and report the rest as divergence.
        let Ok(round) = ChangeSet::create(&schema, &doc, changes) else {
            break;
        };
        let Ok(next) = round.apply(&doc) else {
            break;
        };
        if round.is_empty() {
            settled = true;
            break;
        }
        let ranges = changed_ranges(&round);
        replaced = ranges.0;
        marked = ranges.1;
        accumulated = Some(match accumulated {
            Some(previous) => previous.compose(&round).ok()?,
            None => round,
        });
        doc = next;
    }
    let accumulated = accumulated?;
    let mut spec = TransactionSpec::new().change_set(accumulated).sequential();
    if !settled {
        spec = spec.annotate(corrections_diverged().of(true));
    }
    Some(spec)
}

fn corrections_apply(corrections: &[Correction], tr: &Transaction) -> bool {
    !corrections.is_empty() && tr.doc_changed() && tr.annotation(remote()) != Some(&true)
}

/// Replaced and mark-modified ranges of one change set.
type TouchedRanges = (Vec<(usize, usize)>, Vec<(usize, usize)>);

/// The ranges a change set touches, in the coordinates of the document it
/// produces.
fn changed_ranges(changes: &ChangeSet) -> TouchedRanges {
    let mut replaced = Vec::new();
    let mut marked = Vec::new();
    for range in changes.iter_changes() {
        match range {
            ChangeRange::Replaced { from_b, to_b, .. } => replaced.push((from_b, to_b)),
            ChangeRange::Marked { from_b, to_b, .. } => marked.push((from_b, to_b)),
        }
    }
    (replaced, marked)
}

/// The changes `corrections` want to make to the result of `tr`, in one round.
///
/// Exposed for a caller that has to run corrections outside a transaction, for
/// instance when checking a document that was loaded from elsewhere. The
/// [`corrections`] extension runs this repeatedly; a caller that wants the same
/// fixed point has to loop itself.
pub fn collect_corrections(corrections: &[Correction], tr: &Transaction) -> Vec<Change> {
    if !corrections_apply(corrections, tr) {
        return Vec::new();
    }
    let (replaced, marked) = changed_ranges(tr.changes());
    corrections_for(corrections, tr, tr.new_doc(), &replaced, &marked)
}

/// One round: the changes `corrections` want to make to `doc`, given the ranges
/// of `doc` that just changed.
fn corrections_for(
    corrections: &[Correction],
    tr: &Transaction,
    doc: &Node,
    replaced: &[(usize, usize)],
    marked: &[(usize, usize)],
) -> Vec<Change> {
    let touched: Vec<(usize, usize)> = replaced.iter().chain(marked.iter()).copied().collect();
    let mut out: Vec<Change> = Vec::new();

    let mut visit = |node: &Node, content_start: usize, before: Option<usize>| {
        let end = content_start + node.content_size();
        for correction in corrections {
            if correction.node_type != node.type_id() {
                continue;
            }
            let fires = match correction.trigger {
                CorrectionTrigger::Content => {
                    overlaps(&touched, content_start, end) || inserted(replaced, before, node)
                }
                CorrectionTrigger::ChildList => {
                    child_list_changed(node, content_start, replaced)
                        || inserted(replaced, before, node)
                }
                CorrectionTrigger::Marks => overlaps(marked, content_start, end),
            };
            if !fires {
                continue;
            }
            let cx = CorrectionContext {
                start_state: tr.start_state(),
                tr,
                doc,
                node,
                content_start,
                before,
            };
            out.extend((correction.correct)(&cx));
        }
    };

    visit(doc, 0, None);
    doc.descendants(&mut |node, pos, _, _| {
        let start = pos;
        let end = pos + node.node_size();
        if !overlaps(&touched, start, end) {
            return false;
        }
        if node.is_container() {
            visit(node, pos + 1, Some(pos));
        }
        true
    });
    out.sort_by_key(|change| (change.from, change.to));
    out
}

fn overlaps(ranges: &[(usize, usize)], from: usize, to: usize) -> bool {
    ranges.iter().any(|(a, b)| *b >= from && *a <= to)
}

/// Whether the node itself was part of inserted content.
fn inserted(replaced: &[(usize, usize)], before: Option<usize>, node: &Node) -> bool {
    let Some(before) = before else {
        return false;
    };
    replaced
        .iter()
        .any(|(a, b)| *a <= before && *b >= before + node.node_size())
}

/// Whether a change crossed a direct-child boundary of `node`.
///
/// A change that stays strictly inside one child only changed that child's
/// content, not this node's child list.
fn child_list_changed(node: &Node, content_start: usize, replaced: &[(usize, usize)]) -> bool {
    let end = content_start + node.content_size();
    replaced.iter().any(|(from, to)| {
        if *to < content_start || *from > end {
            return false;
        }
        let mut pos = content_start;
        for child in node.children() {
            let child_end = pos + child.node_size();
            if child.is_container() && *from > pos && *to < child_end {
                return false;
            }
            pos = child_end;
        }
        true
    })
}

/// A correction that fills in the children a node's content rule requires.
///
/// Emptying a `list_item` whose rule is `paragraph block*` leaves a document
/// that [`Node::check`] rejects but that a change with [`Fit::No`](crate::Fit)
/// will happily produce. This puts the missing child back.
pub fn fill_required_content(node_type: NodeTypeId) -> Correction {
    Correction::on_child_list(node_type, move |cx| {
        let schema = cx.start_state.schema();
        let mut matched = schema.content_match(cx.node.type_id());
        for child in cx.node.children() {
            match matched.match_type(child.type_id()) {
                Some(next) => matched = next,
                None => return Vec::new(),
            }
        }
        if matched.valid_end() {
            return Vec::new();
        }
        let Some(types) = schema.fill_before(matched, &[], true) else {
            return Vec::new();
        };
        let mut nodes = Vec::with_capacity(types.len());
        for ty in types {
            let Some(node) = schema.create_and_fill(
                ty,
                schema.node_type(ty).default_attrs().clone(),
                MarkSet::empty(),
                Fragment::empty(),
            ) else {
                return Vec::new();
            };
            nodes.push(node);
        }
        if nodes.is_empty() {
            return Vec::new();
        }
        let at = cx.content_start + cx.node.content_size();
        vec![Change::insert(
            at,
            Slice::from_fragment(Fragment::from_nodes(nodes)),
        )]
    })
}
