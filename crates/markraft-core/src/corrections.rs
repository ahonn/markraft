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
//! # Settling what a selection left behind
//!
//! A correction may deliberately leave the content around a caret alone — a
//! half-typed marker the writer is still completing. Such a correction is
//! registered with [`Correction::when_selection_leaves`]: it then also runs,
//! in the first round, for every node of its type that held an end of the
//! selection before a transaction that moves the selection, even when the
//! transaction changes nothing else — but not for one the history does not
//! record, such as an undo, which restores a selection rather than moving it.
//! [`CorrectionContext::selection_left`] says that is why it runs. What it asks for joins the transaction that moved
//! the selection; when that transaction did not change the document itself,
//! it is annotated [`fold_into_previous`](crate::fold_into_previous), so the
//! history keeps the settling with the edit that left the content unsettled.
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
    /// Whether the node held an end of the selection before the transaction
    /// and the transaction moved the selection. Only ever set in the first
    /// round, and only for a correction registered with
    /// [`Correction::when_selection_leaves`].
    pub selection_left: bool,
    /// The ranges of `doc` this round's input changed.
    touched: &'a [(usize, usize)],
    /// The positions of `doc` the selection left, in the first round.
    left: &'a [usize],
}

impl CorrectionContext<'_> {
    /// Whether this round's input reached `from..=to` of [`CorrectionContext::doc`]:
    /// a change touched it, or an end of the selection left it.
    ///
    /// This is exactly when a [`CorrectionTrigger::Content`] correction
    /// registered with [`Correction::when_selection_leaves`] is called for a
    /// node whose content is `from..to`. A correction on an ancestor that
    /// repairs such nodes itself uses it to leave to their own correction the
    /// ones it is being called for anyway, so the two never ask for
    /// overlapping changes.
    pub fn touches(&self, from: usize, to: usize) -> bool {
        overlaps(self.touched, from, to) || self.left.iter().any(|pos| (from..=to).contains(pos))
    }
}

type CorrectFn = Arc<dyn Fn(&CorrectionContext<'_>) -> Vec<Change> + Send + Sync>;

/// An observer on one node type.
#[derive(Clone)]
pub struct Correction {
    node_type: NodeTypeId,
    trigger: CorrectionTrigger,
    on_selection_leave: bool,
    correct: CorrectFn,
}

impl std::fmt::Debug for Correction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Correction")
            .field("node_type", &self.node_type)
            .field("trigger", &self.trigger)
            .field("on_selection_leave", &self.on_selection_leave)
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
            on_selection_leave: false,
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
            on_selection_leave: false,
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
            on_selection_leave: false,
            correct: Arc::new(correct),
        }
    }

    /// Also run this correction for a node the selection leaves.
    ///
    /// For a correction that leaves the content around a caret unsettled: the
    /// transaction that moves the selection away — including one that changes
    /// nothing else — is when to settle it. See the module documentation.
    pub fn when_selection_leaves(mut self) -> Correction {
        self.on_selection_leave = true;
        self
    }

    /// Whether this correction also runs for a node the selection leaves.
    pub fn runs_when_selection_leaves(&self) -> bool {
        self.on_selection_leave
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
    let mut left = selection_left(&list, tr);
    if !corrections_apply(&list, tr) && left.is_empty() {
        return None;
    }
    let schema = tr.start_state().schema().clone();
    let mut doc = tr.new_doc().clone();
    let (mut replaced, mut marked) = changed_ranges(tr.changes());
    let mut accumulated: Option<ChangeSet> = None;
    let mut settled = false;
    for _ in 0..MAX_CORRECTION_ROUNDS {
        let changes = corrections_for(&list, tr, &doc, &replaced, &marked, &left);
        left.clear();
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
    if !tr.doc_changed() {
        spec = spec.annotate(crate::history::fold_into_previous().of(true));
    }
    Some(spec)
}

/// Where the ends of the selection stood before `tr`, in the coordinates of
/// the document it produces, when `tr` moves the selection and some
/// correction wants to hear about it. Empty otherwise.
///
/// A transaction the history does not record — an undo, a redo, a remote
/// edit — restores or relays a selection rather than moving it, and settling
/// in it would make undoing one thing do another.
fn selection_left(corrections: &[Correction], tr: &Transaction) -> Vec<usize> {
    if tr.selection().is_none()
        || tr.annotation(remote()) == Some(&true)
        || tr.annotation(crate::state::add_to_history()) == Some(&false)
        || !corrections.iter().any(|c| c.on_selection_leave)
    {
        return Vec::new();
    }
    let start = tr.start_state();
    let before = start
        .selection()
        .map(start.schema(), tr.new_doc(), &tr.changes().desc());
    if before == tr.new_selection() {
        return Vec::new();
    }
    before
        .ranges(tr.new_doc())
        .iter()
        .flat_map(|range| [range.from, range.to])
        .collect()
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
    corrections_for(corrections, tr, tr.new_doc(), &replaced, &marked, &[])
}

/// One round: the changes `corrections` want to make to `doc`, given the ranges
/// of `doc` that just changed and the positions the selection left.
fn corrections_for(
    corrections: &[Correction],
    tr: &Transaction,
    doc: &Node,
    replaced: &[(usize, usize)],
    marked: &[(usize, usize)],
    left: &[usize],
) -> Vec<Change> {
    let touched: Vec<(usize, usize)> = replaced.iter().chain(marked.iter()).copied().collect();
    let visited: Vec<(usize, usize)> = touched
        .iter()
        .copied()
        .chain(left.iter().map(|pos| (*pos, *pos)))
        .collect();
    let mut out: Vec<Change> = Vec::new();

    let mut visit = |node: &Node, content_start: usize, before: Option<usize>| {
        let end = content_start + node.content_size();
        for correction in corrections {
            if correction.node_type != node.type_id() {
                continue;
            }
            let selection_left = correction.on_selection_leave
                && left.iter().any(|pos| (content_start..=end).contains(pos));
            let fires = selection_left
                || match correction.trigger {
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
                selection_left,
                touched: &touched,
                left,
            };
            out.extend((correction.correct)(&cx));
        }
    };

    visit(doc, 0, None);
    doc.descendants(&mut |node, pos, _, _| {
        let start = pos;
        let end = pos + node.node_size();
        if !overlaps(&visited, start, end) {
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
