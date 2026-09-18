//! Fitting: repairing a replacement so that it produces a valid document.
//!
//! A replacement is a token run spliced into the document's token stream. The
//! run may open containers it does not close, close containers it did not open,
//! or place content where the schema forbids it. Fitting simulates the splice
//! against the schema and repairs it by
//!
//! * closing containers the run left open and opening ones it left closed,
//!   reusing the markup of the containers that surround the replacement,
//! * wrapping content that does not fit (stray inline content in the default
//!   textblock, stray blocks in whatever [`Schema::find_wrapping`] finds),
//! * dropping containers that would be left empty when their content rule
//!   forbids that, and
//! * dropping content it cannot place at all.
//!
//! The repair is returned as an adjusted change — a list of token-level
//! replacements — rather than applied as a side effect, so that inversion and
//! position mapping stay exact.

use crate::error::ChangeError;
use crate::node::{Markup, Node};
use crate::schema::{ContentMatch, NodeTypeId, Schema};
use crate::slice::{Slice, Token, tokens_cut};

use crate::change::apply::{content_tokens, fragment_from_tokens};

/// Whether and how a change may be repaired.
///
/// Repairing can widen the change: the recorded replacement may cover a larger
/// range than asked for, and may add a second replacement further along where
/// containers the change left open have to be closed. That is deliberate — the
/// repair is part of the change set, so position mapping and inversion stay
/// exact — but it means a caller cannot assume its range is preserved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fit {
    /// The caller vouches for the change; it is recorded as given.
    #[default]
    No,
    /// Repair the change using the document around it.
    Auto,
    /// Repair the change, preferring these container types as wrappers.
    /// Innermost first, as in a context stack.
    Context(Vec<NodeTypeId>),
}

/// How many times the placement loop may retry before giving up on a token.
const PLACE_GUARD: usize = 64;

#[derive(Clone)]
struct Frame<'a> {
    markup: Markup,
    m: ContentMatch<'a>,
    /// Index of this frame's open token in the output, when the repaired run
    /// opened it. `None` for frames that were already open in the document.
    open_at: Option<usize>,
    /// The parent's content match before this frame's open token, so the frame
    /// can be undone.
    parent_match: Option<ContentMatch<'a>>,
    /// Set for an open token that could not be placed: its content flows into
    /// the parent and its close token is swallowed.
    dropped: bool,
}

struct Fitter<'a> {
    schema: &'a Schema,
    fit: &'a Fit,
    frames: Vec<Frame<'a>>,
    out: Vec<Token>,
}

impl<'a> Fitter<'a> {
    fn top(&self) -> &Frame<'a> {
        self.frames.last().expect("the container frame remains")
    }

    /// The type that really holds the content being placed.
    ///
    /// A dropped frame emits no open token, so the content inside it ends up in
    /// the nearest frame that did — and it is that node's rules, not the
    /// dropped one's, that decide which marks may stay.
    fn content_parent(&self) -> NodeTypeId {
        self.frames
            .iter()
            .rev()
            .find(|frame| !frame.dropped)
            .map(|frame| frame.markup.ty)
            .expect("the container frame is never dropped")
    }

    fn set_top_match(&mut self, m: ContentMatch<'a>) {
        self.frames
            .last_mut()
            .expect("the container frame remains")
            .m = m;
    }

    /// Replay a valid token run to bring the frame stack up to date.
    fn simulate(&mut self, tokens: &[Token]) -> Result<(), ChangeError> {
        for token in tokens {
            match token {
                Token::Open(markup) => {
                    let next = self.top().m.match_type(markup.ty).ok_or_else(|| {
                        ChangeError::Unfittable("document context does not match its schema".into())
                    })?;
                    self.set_top_match(next);
                    self.push_frame(markup.clone(), None, None);
                }
                Token::Close(_) => {
                    if self.frames.len() <= 1 {
                        return Err(ChangeError::Unbalanced(
                            "context closes the enclosing node".into(),
                        ));
                    }
                    self.frames.pop();
                }
                Token::Node(node) => {
                    let next = self.top().m.match_type(node.type_id()).ok_or_else(|| {
                        ChangeError::Unfittable("document context does not match its schema".into())
                    })?;
                    self.set_top_match(next);
                }
            }
        }
        Ok(())
    }

    fn push_frame(
        &mut self,
        markup: Markup,
        open_at: Option<usize>,
        parent_match: Option<ContentMatch<'a>>,
    ) {
        let m = self.schema.content_match(markup.ty);
        self.frames.push(Frame {
            markup,
            m,
            open_at,
            parent_match,
            dropped: false,
        });
    }

    /// Drop the marks `parent` does not allow in its content, recursively.
    ///
    /// Pasting bold text into a code block is the motivating case: the content
    /// is kept, the marks the destination forbids are not.
    fn strip_marks(&self, parent: NodeTypeId, node: &Node) -> Node {
        let parent_ty = self.schema.node_type(parent);
        let node_ty = self.schema.node_type(node.type_id());
        let marks = if node_ty.is_inline() {
            node.marks()
                .filter(|mark| parent_ty.allows_mark_in_content(mark.ty))
        } else {
            node.marks().clone()
        };
        let stripped = if marks == *node.marks() {
            node.clone()
        } else {
            node.mark(marks)
        };
        if !stripped.is_container() || stripped.content_size() == 0 {
            return stripped;
        }
        let inner = stripped.type_id();
        let content: crate::fragment::Fragment = stripped
            .children()
            .map(|child| self.strip_marks(inner, child))
            .collect();
        if content == *stripped.content() {
            stripped
        } else {
            stripped.copy(content)
        }
    }

    /// Whether a node's own content satisfies its content rule, recursively.
    fn content_ok(&self, node: &Node) -> bool {
        if !node.is_container() {
            return true;
        }
        let mut m = self.schema.content_match(node.type_id());
        for child in node.children() {
            match m.match_type(child.type_id()) {
                Some(next) => m = next,
                None => return false,
            }
        }
        m.valid_end() && node.children().all(|child| self.content_ok(child))
    }

    /// Wrapper chain that makes `target` fit at the current position.
    fn wrapping(&self, target: NodeTypeId) -> Option<Vec<NodeTypeId>> {
        if let Fit::Context(context) = self.fit {
            for candidate in context {
                if self.top().m.match_type(*candidate).is_some()
                    && self
                        .schema
                        .content_match(*candidate)
                        .match_type(target)
                        .is_some()
                {
                    return Some(vec![*candidate]);
                }
            }
        }
        self.schema.find_wrapping(self.top().m, target)
    }

    fn open(&mut self, markup: Markup) -> bool {
        let Some(next) = self.top().m.match_type(markup.ty) else {
            return false;
        };
        let parent_match = self.top().m;
        self.set_top_match(next);
        let open_at = self.out.len();
        self.out.push(Token::Open(markup.clone()));
        self.push_frame(markup, Some(open_at), Some(parent_match));
        true
    }

    /// Close the innermost frame, filling or dropping it as the content rule
    /// requires.
    fn close(&mut self) {
        let frame = self.frames.pop().expect("caller checked the frame count");
        if frame.dropped {
            // The open token was never emitted; carry its progress outwards.
            self.set_top_match(frame.m);
            return;
        }
        if !frame.m.valid_end() {
            let empty = frame
                .open_at
                .is_some_and(|open_at| self.out.len() == open_at + 1);
            if empty {
                let open_at = frame.open_at.expect("checked by `empty`");
                self.out.truncate(open_at);
                if let Some(parent) = frame.parent_match {
                    self.set_top_match(parent);
                }
                return;
            }
            match self.schema.fill_before(frame.m, &[], true) {
                Some(types) => {
                    for ty in types {
                        if let Some(node) = self.schema.create_and_fill(
                            ty,
                            self.schema.node_type(ty).default_attrs().clone(),
                            crate::mark::MarkSet::empty(),
                            crate::fragment::Fragment::empty(),
                        ) {
                            self.out.push(Token::Node(node));
                        }
                    }
                }
                None => {
                    if let Some(open_at) = frame.open_at {
                        self.out.truncate(open_at);
                        if let Some(parent) = frame.parent_match {
                            self.set_top_match(parent);
                        }
                        return;
                    }
                }
            }
        }
        self.out.push(Token::Close(frame.markup));
    }

    /// Complete the enclosing node's content when the repaired run leaves it
    /// short. Its frame is never closed, so nothing else fills it.
    fn fill_root(&mut self) {
        let root = self.frames.first().expect("the container frame remains");
        if root.m.valid_end() {
            return;
        }
        let Some(types) = self.schema.fill_before(root.m, &[], true) else {
            return;
        };
        for ty in types {
            if let Some(node) = self.schema.create_and_fill(
                ty,
                self.schema.node_type(ty).default_attrs().clone(),
                crate::mark::MarkSet::empty(),
                crate::fragment::Fragment::empty(),
            ) {
                self.out.push(Token::Node(node));
            }
        }
    }

    fn place_node(&mut self, node: &Node, depth: usize) {
        // A container whose own content breaks the schema is taken apart so the
        // repair loop can fix it from the inside.
        if node.is_container() && depth < PLACE_GUARD && !self.content_ok(node) {
            for token in crate::slice::node_tokens(node) {
                match token {
                    Token::Open(markup) => self.place_open(&markup),
                    Token::Close(_) => {
                        if self.frames.len() > 1 {
                            self.close();
                        }
                    }
                    Token::Node(child) => self.place_node(&child, depth + 1),
                }
            }
            return;
        }
        for _ in 0..PLACE_GUARD {
            if let Some(next) = self.top().m.match_type(node.type_id()) {
                self.set_top_match(next);
                let placed = self.strip_marks(self.content_parent(), node);
                self.out.push(Token::Node(placed));
                self.set_top_match(next);
                return;
            }
            if let Some(chain) = self.wrapping(node.type_id())
                && !chain.is_empty()
            {
                let mut opened = true;
                for ty in chain {
                    let markup =
                        Markup::with_attrs(ty, self.schema.node_type(ty).default_attrs().clone());
                    if !self.open(markup) {
                        opened = false;
                        break;
                    }
                }
                if opened && self.top().m.match_type(node.type_id()).is_some() {
                    continue;
                }
            }
            // Closing a dropped frame here would let its close token close the
            // real parent instead, so stop rather than mis-nest.
            if self.frames.len() > 1 && !self.top().dropped {
                self.close();
                continue;
            }
            break;
        }
        // The node cannot be placed. Salvage its content if it has any.
        if node.is_container() && node.child_count() > 0 && depth < PLACE_GUARD {
            for child in node.children() {
                self.place_node(child, depth + 1);
            }
        }
    }

    fn place_open(&mut self, markup: &Markup) {
        for _ in 0..PLACE_GUARD {
            if self.open(markup.clone()) {
                return;
            }
            if let Some(chain) = self.wrapping(markup.ty)
                && !chain.is_empty()
            {
                let mut opened = true;
                for ty in chain {
                    let wrapper =
                        Markup::with_attrs(ty, self.schema.node_type(ty).default_attrs().clone());
                    if !self.open(wrapper) {
                        opened = false;
                        break;
                    }
                }
                if opened {
                    continue;
                }
            }
            if self.frames.len() > 1 && !self.top().dropped {
                self.close();
                continue;
            }
            break;
        }
        // Keep the content but forget the container.
        let m = self.top().m;
        self.frames.push(Frame {
            markup: markup.clone(),
            m,
            open_at: None,
            parent_match: None,
            dropped: true,
        });
    }

    fn repair(&mut self, tokens: &[Token]) {
        for token in tokens {
            match token {
                Token::Node(node) => self.place_node(node, 0),
                Token::Open(markup) => self.place_open(markup),
                Token::Close(_) => {
                    if self.frames.len() > 1 {
                        self.close();
                    }
                }
            }
        }
    }
}

/// Turn a replacement into concrete token-level replacements.
///
/// Returns a sorted, non-overlapping list of `(from, to, tokens)` parts. With
/// [`Fit::No`] that is always the change as given. Otherwise the range may be
/// widened and a second part may be added further along, where the repair
/// needs to close containers the replacement left open.
pub(crate) fn fit_replacement(
    schema: &Schema,
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    fit: &Fit,
) -> Result<Vec<(usize, usize, Vec<Token>)>, ChangeError> {
    if *fit == Fit::No {
        return Ok(vec![(from, to, slice.tokens())]);
    }
    let resolved_from = doc.resolve(from)?;
    let resolved_to = doc.resolve(to)?;
    // The repaired run has to balance inside the region, so the region must be
    // deep enough to absorb every container the run closes without opening.
    let dip = crate::slice::min_prefix_delta(&slice.tokens());
    let shared = resolved_from
        .shared_depth(to)
        .min((resolved_from.depth() as isize + dip).max(0) as usize);
    let base = resolved_from.start(shared);
    let container = resolved_from.node(shared).clone();
    let region_end = base + container.content_size();
    let container_tokens = content_tokens(&container);
    let left = tokens_cut(&container_tokens, 0, from - base);
    let right = tokens_cut(&container_tokens, to - base, region_end - base);
    let depth_right = resolved_to.depth() - shared;

    // First attempt: repair only the replacement itself.
    let mut fitter = Fitter {
        schema,
        fit,
        frames: vec![Frame {
            markup: container.markup().clone(),
            m: schema.content_match(container.type_id()),
            open_at: None,
            parent_match: None,
            dropped: false,
        }],
        out: Vec::new(),
    };
    fitter.simulate(&left)?;
    let left_frames = fitter.frames.len();
    fitter.repair(&slice.tokens());
    let mut middle = std::mem::take(&mut fitter.out);
    // Only frames whose open token was actually emitted count towards the
    // depth. A frame the repair dropped contributes no token, so closing it
    // here would emit a close with nothing to match.
    let open_frames: Vec<usize> = (1..fitter.frames.len())
        .filter(|i| !fitter.frames[*i].dropped)
        .collect();
    let depth_now = open_frames.len();

    let mut tail: Vec<Token> = Vec::new();
    if depth_now < depth_right {
        // Re-open the containers the replacement left closed, reusing the
        // markup of the context after the change.
        for k in depth_now + 1..=depth_right {
            middle.push(Token::Open(resolved_to.node(shared + k).markup().clone()));
        }
    } else if depth_now > depth_right {
        // Close the containers the replacement left open. They can only be
        // closed after the context that follows the change has closed its own,
        // so the outermost `extra` frames are closed innermost-first.
        let extra = depth_now - depth_right;
        for i in open_frames[..extra].iter().rev() {
            tail.push(Token::Close(fitter.frames[*i].markup.clone()));
        }
    }

    let tail_pos = if depth_right == 0 {
        to
    } else {
        resolved_to.after(shared + 1)
    };
    let mut parts: Vec<(usize, usize, Vec<Token>)> = vec![(from, to, middle.clone())];
    if !tail.is_empty() {
        parts.push((tail_pos, tail_pos, tail.clone()));
    }

    if validates(
        schema, &container, &left, &middle, &right, to, tail_pos, &tail,
    ) {
        return Ok(parts);
    }

    // Second attempt: pull the rest of the enclosing node into the change and
    // repair that too.
    let mut fitter = Fitter {
        schema,
        fit,
        frames: vec![Frame {
            markup: container.markup().clone(),
            m: schema.content_match(container.type_id()),
            open_at: None,
            parent_match: None,
            dropped: false,
        }],
        out: Vec::new(),
    };
    fitter.simulate(&left)?;
    debug_assert_eq!(fitter.frames.len(), left_frames);
    let mut run = slice.tokens();
    run.extend(right.iter().cloned());
    fitter.repair(&run);
    while fitter.frames.len() > 1 {
        fitter.close();
    }
    // The change may have emptied the enclosing node, whose own frame is never
    // closed. Fill in whatever its content rule still requires.
    fitter.fill_root();
    let wide = std::mem::take(&mut fitter.out);
    if validates(
        schema,
        &container,
        &left,
        &wide,
        &[],
        region_end,
        region_end,
        &[],
    ) {
        return Ok(vec![(from, region_end, wide)]);
    }
    Err(ChangeError::Unfittable(
        "the replacement cannot be made to fit the schema".into(),
    ))
}

/// Whether the repaired stream produces a valid enclosing node.
#[allow(clippy::too_many_arguments)]
fn validates(
    schema: &Schema,
    container: &Node,
    left: &[Token],
    middle: &[Token],
    right: &[Token],
    right_start: usize,
    tail_pos: usize,
    tail: &[Token],
) -> bool {
    let mut full: Vec<Token> = Vec::with_capacity(left.len() + middle.len() + right.len());
    full.extend(left.iter().cloned());
    full.extend(middle.iter().cloned());
    if tail.is_empty() {
        full.extend(right.iter().cloned());
    } else {
        let split = tail_pos - right_start;
        let total = crate::slice::tokens_size(right);
        full.extend(tokens_cut(right, 0, split));
        full.extend(tail.iter().cloned());
        full.extend(tokens_cut(right, split, total));
    }
    match fragment_from_tokens(&full) {
        Ok(content) => container.copy(content).check(schema).is_ok(),
        Err(_) => false,
    }
}
