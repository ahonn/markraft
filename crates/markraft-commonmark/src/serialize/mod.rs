//! Writing a document tree back out as Markdown.
//!
//! The design is ProseMirror's: a rule per node type and a rule per mark type,
//! driven by a [`SerializerState`] that owns the output string and knows how
//! blocks are separated and how nested containers prefix their lines.
//!
//! # Block separation
//!
//! A node rule ends by calling [`SerializerState::close_block`], which only
//! *records* that a block ended. The next write flushes it, emitting a newline
//! and then one blank line — so exactly one blank line separates two blocks,
//! and a list that wants its items on consecutive lines asks for a shorter
//! flush instead. Nothing has to look ahead.
//!
//! # Prefixes
//!
//! [`SerializerState::wrap_block`] pushes a prefix onto a stack: `> ` for a
//! quote, `- ` and then two spaces for a list item. Every line the enclosed
//! content starts is written with the stack's prefix, including the blank lines
//! between blocks, which is what keeps a nested structure inside its container.
//!
//! # Escaping
//!
//! Text is escaped by [`crate::escape`], which only escapes what would be read
//! as syntax in the position it lands in. The state tracks whether the next
//! character starts a line's *content*, after any prefix, because that is where
//! the block-opening characters mean something.

use std::collections::HashMap;
use std::sync::Arc;

use markraft_core::{Mark, MarkTypeId, Node, NodeTypeId, Schema, Slice};

mod inline;

use crate::escape::{escape_text, escape_unlinked_text, protect_indent};

/// Writes one node. Receives the state, the node, its parent and its index in
/// that parent.
pub type NodeRule =
    Arc<dyn Fn(&mut SerializerState<'_>, &Node, Option<&Node>, usize) + Send + Sync>;

/// Produces the string that opens or closes one mark.
pub type MarkStringFn =
    Arc<dyn Fn(&mut SerializerState<'_>, &MarkTarget<'_>) -> String + Send + Sync>;

/// What a mark rule is told about the delimiter it is writing.
pub struct MarkTarget<'a> {
    /// The mark being opened or closed.
    pub mark: &'a Mark,
    /// The textblock holding the marked content.
    pub parent: &'a Node,
    /// The index in `parent` of the node the delimiter is adjacent to: the
    /// first node of the run when opening, the last when closing.
    pub index: usize,
    /// Whether the delimiter opens the mark.
    pub opening: bool,
}

/// How one mark type is written.
#[derive(Clone)]
pub struct MarkRule {
    /// The string that opens the mark.
    pub open: MarkStringFn,
    /// The string that closes it.
    pub close: MarkStringFn,
    /// Whether the mark may be reordered against other mixable marks, so that
    /// `**a _b_** _c_` does not close and reopen the emphasis.
    pub mixable: bool,
    /// Whether whitespace at the edges of the marked text moves outside the
    /// mark. CommonMark refuses to read `* a *` as emphasis at all.
    pub expel_enclosing_whitespace: bool,
    /// Whether the marked text is escaped. A code span's content is literal.
    pub escape: bool,
    /// The first character the opening delimiter writes, which is what a
    /// neighbouring mark needs to know to decide whether its own delimiter can
    /// flank where it sits.
    pub lead: Option<char>,
    /// The last character the closing delimiter writes, for the same reason.
    pub trail: Option<char>,
}

impl MarkRule {
    /// A mark written as a fixed pair of strings.
    pub fn fixed(open: &'static str, close: &'static str) -> MarkRule {
        MarkRule {
            open: Arc::new(move |_, _| open.to_string()),
            close: Arc::new(move |_, _| close.to_string()),
            mixable: false,
            expel_enclosing_whitespace: false,
            escape: true,
            lead: open.chars().next(),
            trail: close.chars().next_back(),
        }
    }
}

/// Node rules keyed by schema type name.
pub type NodeRules = HashMap<String, NodeRule>;
/// Mark rules keyed by schema type name.
pub type MarkRules = HashMap<String, MarkRule>;

/// Writes documents of one schema as Markdown.
#[derive(Clone)]
pub struct MarkdownSerializer {
    schema: Schema,
    nodes: Vec<Option<NodeRule>>,
    marks: Vec<Option<MarkRule>>,
}

impl MarkdownSerializer {
    /// A serialiser binding name-keyed rules to `schema`.
    ///
    /// Rules naming a type the schema does not declare are ignored, so one rule
    /// set serves a schema and any trimmed-down variant of it. A type with no
    /// rule falls back to writing its content, which keeps text rather than
    /// losing it, but writes no syntax of its own.
    pub fn new(schema: Schema, nodes: NodeRules, marks: MarkRules) -> MarkdownSerializer {
        let mut node_rules = vec![None; schema.node_types().len()];
        for (name, rule) in nodes {
            if let Some(id) = schema.node_id(&name) {
                node_rules[id.index()] = Some(rule);
            }
        }
        let mut mark_rules = vec![None; schema.mark_types().len()];
        for (name, rule) in marks {
            if let Some(id) = schema.mark_id(&name) {
                mark_rules[id.index()] = Some(rule);
            }
        }
        MarkdownSerializer {
            schema,
            nodes: node_rules,
            marks: mark_rules,
        }
    }

    /// The schema this serialiser writes.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The rule for a node type, if any.
    pub fn node_rule(&self, ty: NodeTypeId) -> Option<&NodeRule> {
        self.nodes.get(ty.index()).and_then(Option::as_ref)
    }

    /// The rule for a mark type, if any.
    pub fn mark_rule(&self, ty: MarkTypeId) -> Option<&MarkRule> {
        self.marks.get(ty.index()).and_then(Option::as_ref)
    }

    /// Write a copied [`Slice`] as Markdown.
    ///
    /// The slice is closed first — its partial blocks are wrapped in whatever
    /// the schema needs to make a document of them — because Markdown has no
    /// way to say "a list item without its list". A cut taken inside one
    /// paragraph therefore writes as that text alone, with no block separation
    /// around it.
    pub fn serialize_fragment(&self, slice: &Slice) -> String {
        match crate::fragment::close(&self.schema, slice) {
            Some(doc) => self.serialize(&doc),
            None => String::new(),
        }
    }

    /// Write `doc` as Markdown.
    ///
    /// The result has no trailing newline; a host that writes files adds one.
    pub fn serialize(&self, doc: &Node) -> String {
        let mut state = SerializerState {
            serializer: self,
            out: String::new(),
            delim: String::new(),
            closed: None,
            in_tight_list: false,
            line_start: true,
            single_line: false,
            line_break: " ",
            after_mark_close: false,
            tagged: vec![false; self.marks.len()],
        };
        state.render_content(doc);
        state.out
    }
}

/// The output being built, and everything a rule needs to add to it.
pub struct SerializerState<'a> {
    serializer: &'a MarkdownSerializer,
    out: String,
    delim: String,
    closed: Option<Node>,
    in_tight_list: bool,
    line_start: bool,
    single_line: bool,
    line_break: &'static str,
    after_mark_close: bool,
    tagged: Vec<bool>,
}

impl<'a> SerializerState<'a> {
    /// The schema being written.
    pub fn schema(&self) -> &'a Schema {
        self.serializer.schema()
    }

    /// The serialiser, for a rule that needs to look up another rule.
    pub fn serializer(&self) -> &'a MarkdownSerializer {
        self.serializer
    }

    /// The output so far.
    pub fn out(&self) -> &str {
        &self.out
    }

    /// The output so far, for a rule that has to amend what it just wrote —
    /// an ATX heading escaping a trailing `#`, for instance.
    pub fn out_mut(&mut self) -> &mut String {
        &mut self.out
    }

    /// Whether the output is at the start of a line.
    pub fn at_blank(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n')
    }

    /// Whether the next character written starts a line's *content*, after any
    /// block prefix. This is where the block-opening characters mean something.
    pub fn at_line_start(&self) -> bool {
        self.line_start
    }

    /// Whether the current block must stay on one line, as an ATX heading must.
    pub fn is_single_line(&self) -> bool {
        self.single_line
    }

    /// Set the one-line constraint and return the previous value.
    pub fn set_single_line(&mut self, value: bool) -> bool {
        std::mem::replace(&mut self.single_line, value)
    }

    /// What a hard break writes while the one-line constraint is in force,
    /// where a line ending cannot go.
    ///
    /// A space in an ATX heading, which has no other spelling for it; `<br>` in
    /// a table cell, where GFM reads the tag back as the break the author made.
    pub fn line_break(&self) -> &'static str {
        self.line_break
    }

    /// Whether the content being written sits in a tight list, where blocks are
    /// separated by a single newline rather than a blank line.
    pub fn in_tight_list(&self) -> bool {
        self.in_tight_list
    }

    /// Record that the mark is currently open in its alternative spelling — an
    /// HTML tag rather than a delimiter run, a bare URL rather than brackets —
    /// so the closing rule matches.
    pub fn set_tagged(&mut self, ty: MarkTypeId, tagged: bool) {
        if let Some(slot) = self.tagged.get_mut(ty.index()) {
            *slot = tagged;
        }
    }

    /// Whether the mark was opened in its alternative spelling.
    pub fn tagged(&self, ty: MarkTypeId) -> bool {
        self.tagged.get(ty.index()).copied().unwrap_or(false)
    }

    /// The first character the opening delimiter of `ty` writes.
    pub fn mark_lead(&self, ty: MarkTypeId) -> Option<char> {
        self.serializer.mark_rule(ty).and_then(|rule| rule.lead)
    }

    /// The last character the closing delimiter of `ty` writes.
    pub fn mark_trail(&self, ty: MarkTypeId) -> Option<char> {
        self.serializer.mark_rule(ty).and_then(|rule| rule.trail)
    }

    /// The character that lands next to `outer`'s own delimiter because a mark
    /// written *inside* `outer` puts it there — a backtick for a code span, a
    /// bracket for a link.
    ///
    /// A delimiter spelled with the same character as `outer`'s merges into one
    /// run with it (`**` and `*` become `***`) rather than sitting next to it,
    /// so the search looks past those. `None` means the node's own text is what
    /// lands there.
    pub fn edge_inside(&self, node: &Node, outer: &Mark, leading: bool) -> Option<char> {
        let schema = self.schema();
        let rank = schema.mark_type(outer.ty).rank();
        let own = if leading {
            self.mark_lead(outer.ty)
        } else {
            self.mark_trail(outer.ty)
        };
        for mark in node.marks().iter() {
            if schema.mark_type(mark.ty).rank() <= rank {
                continue;
            }
            let Some(rule) = self.serializer.mark_rule(mark.ty) else {
                continue;
            };
            let edge = if leading { rule.lead } else { rule.trail };
            match edge {
                Some(character) if Some(character) != own => return Some(character),
                _ => continue,
            }
        }
        None
    }

    /// Whether the last thing written was a mark's closing delimiter.
    ///
    /// A delimiter run written straight after one that closed a mark merges
    /// with it into a longer run that means something else, so a rule in that
    /// position has to spell its mark another way.
    pub fn after_mark_close(&self) -> bool {
        self.after_mark_close
    }

    /// The last character written, skipping a trailing run of `delimiter`.
    ///
    /// A delimiter run that touches another run merges with it, so what decides
    /// whether a run can flank is the character before the whole run, not the
    /// run itself.
    pub fn char_before_run(&self, delimiter: char) -> Option<char> {
        self.out.trim_end_matches(delimiter).chars().next_back()
    }

    /// Start a new line unless the output is already at one.
    pub fn ensure_newline(&mut self) {
        if !self.at_blank() {
            self.newline();
        }
    }

    /// End the current line.
    ///
    /// Trailing spaces are left where they are. The space after a marker is
    /// part of it: `- [ ]` is only a task item when whitespace follows the box,
    /// and `- ` is only an empty item when the marker is complete.
    fn newline(&mut self) {
        self.out.push('\n');
        self.line_start = true;
    }

    fn write_delim(&mut self) {
        if self.at_blank() && !self.delim.is_empty() {
            self.out.push_str(&self.delim);
        }
    }

    /// Emit the pending block separation: a newline, then `size - 1` blank
    /// lines carrying the current prefix.
    pub fn flush_close(&mut self, size: usize) {
        if self.closed.take().is_none() {
            return;
        }
        self.ensure_newline();
        if size > 1 {
            let trimmed = self.delim.trim_end_matches([' ', '\t']).to_string();
            for _ in 1..size {
                self.out.push_str(&trimmed);
                self.newline();
            }
        }
    }

    /// How many newlines separate two blocks here: a blank line normally, a
    /// single break inside a tight list, where a blank line would make the list
    /// loose.
    pub fn flush_size(&self) -> usize {
        if self.in_tight_list { 1 } else { 2 }
    }

    /// Write content, flushing any pending block separation and the line prefix
    /// first.
    pub fn write(&mut self, content: &str) {
        self.flush_close(self.flush_size());
        self.write_delim();
        if !content.is_empty() {
            self.out.push_str(content);
            self.line_start = false;
        }
    }

    /// Write a block prefix — a list marker or a quote bar — which does not
    /// count as the line's content, so what follows it is still at a line
    /// start.
    pub fn write_prefix(&mut self, prefix: &str) {
        self.flush_close(self.flush_size());
        self.write_delim();
        self.out.push_str(prefix);
    }

    /// Write text.
    ///
    /// Escaped text is inline content and always one output line: a line ending
    /// inside it is written as a character reference, because a line break in a
    /// document is a `hard_break` node, not a character. Unescaped text is
    /// written as the caller spelled it, one output line per line, each with
    /// the current prefix — which is what a code block and a raw block need.
    pub fn text(&mut self, text: &str, escape: bool) {
        self.write_text(text, escape, false);
    }

    /// [`SerializerState::text`], escaped, for inline text that no link
    /// encloses: a URL in it is protected from being read back as an autolink.
    pub(crate) fn unlinked_text(&mut self, text: &str) {
        self.write_text(text, true, true);
    }

    fn write_text(&mut self, text: &str, escape: bool, unlinked: bool) {
        self.after_mark_close = false;
        if escape {
            self.flush_close(self.flush_size());
            self.write_delim();
            let at_start = self.line_start;
            let mut piece = if unlinked {
                escape_unlinked_text(text, at_start)
            } else {
                escape_text(text, at_start)
            };
            if at_start {
                piece = protect_indent(&piece);
            }
            if !piece.is_empty() {
                self.out.push_str(&piece);
                self.line_start = false;
            }
            return;
        }
        let mut first = true;
        for line in text.split('\n') {
            if !first {
                self.newline();
            }
            first = false;
            self.flush_close(self.flush_size());
            self.write_delim();
            if !line.is_empty() {
                // A `!` directly before a link's `[` would make the two an
                // image, so the text that wrote it gives it up now.
                if line.starts_with('[') && self.out.ends_with('!') && !self.out.ends_with("\\!") {
                    self.out.pop();
                    self.out.push_str("\\!");
                }
                self.out.push_str(line);
                self.line_start = false;
            }
        }
    }

    /// Record that a block ended. The separation is written when something else
    /// is.
    pub fn close_block(&mut self, node: &Node) {
        self.closed = Some(node.clone());
    }

    /// The node the last [`SerializerState::close_block`] named, while the
    /// separation is still pending.
    pub fn closed(&self) -> Option<&Node> {
        self.closed.as_ref()
    }

    /// Write a container: `first_delim` (or `delim`) starts its first line,
    /// `delim` every line after that, and the block is closed when `f` returns.
    pub fn wrap_block(
        &mut self,
        delim: &str,
        first_delim: Option<&str>,
        node: &Node,
        f: impl FnOnce(&mut Self),
    ) {
        let old = self.delim.clone();
        let tight = self.in_tight_list;
        // A prefix that puts characters on a line — a quote bar — means the
        // blank lines inside this container are not blank in the source, so an
        // enclosing tight list no longer constrains them. A prefix of spaces —
        // a list item's indent — leaves them blank, so it does.
        // The flush that separates this container from the block before it
        // still belongs to the enclosing list, so the prefix goes out first.
        self.write_prefix(first_delim.unwrap_or(delim));
        if !delim.trim().is_empty() {
            self.in_tight_list = false;
        }
        self.delim.push_str(delim);
        f(self);
        self.delim = old;
        self.in_tight_list = tight;
        self.close_block(node);
    }

    /// Write one node with its type's rule.
    pub fn render(&mut self, node: &Node, parent: Option<&Node>, index: usize) {
        match self.serializer.node_rule(node.type_id()) {
            Some(rule) => {
                let rule = rule.clone();
                rule(self, node, parent, index);
            }
            None => self.render_without_rule(node),
        }
    }

    /// Write the inline content of `node` into a string of its own, without
    /// touching the output.
    ///
    /// A rule that has to measure what it writes before it writes it — a table
    /// padding its columns — renders into a buffer with no line prefix and no
    /// pending block separation, on one line, with `line_break` standing in for
    /// a hard break. The buffer starts mid-line, because that is where a cell
    /// lands: the characters that only open a block at the start of one need no
    /// escaping there.
    pub fn capture_inline(&mut self, node: &Node, line_break: &'static str) -> String {
        let out = std::mem::take(&mut self.out);
        let delim = std::mem::take(&mut self.delim);
        let closed = self.closed.take();
        let tight = std::mem::replace(&mut self.in_tight_list, false);
        let line_start = std::mem::replace(&mut self.line_start, false);
        let single_line = std::mem::replace(&mut self.single_line, true);
        let previous_break = std::mem::replace(&mut self.line_break, line_break);
        let after_mark_close = std::mem::replace(&mut self.after_mark_close, false);
        let fresh = vec![false; self.tagged.len()];
        let tagged = std::mem::replace(&mut self.tagged, fresh);
        self.render_inline(node);
        let captured = std::mem::replace(&mut self.out, out);
        self.delim = delim;
        self.closed = closed;
        self.in_tight_list = tight;
        self.line_start = line_start;
        self.single_line = single_line;
        self.line_break = previous_break;
        self.after_mark_close = after_mark_close;
        self.tagged = tagged;
        captured
    }

    /// Write every child of `parent`.
    pub fn render_content(&mut self, parent: &Node) {
        for (index, child) in parent.children().enumerate() {
            self.render(child, Some(parent), index);
        }
    }

    /// The fallback for a type with no rule: keep the text, write no syntax.
    fn render_without_rule(&mut self, node: &Node) {
        if let Some(text) = node.text() {
            self.text(text, true);
        } else if node.is_container() {
            if node.is_textblock(self.schema()) {
                self.render_inline(node);
                self.close_block(node);
            } else {
                self.render_content(node);
            }
        }
    }

    /// Write a list, one [`SerializerState::wrap_block`] per item.
    ///
    /// `first_delim` is asked for each item's marker, so an ordered list can
    /// count and a task item can add its check box. `tight` decides whether the
    /// items are separated by a newline or by a blank line; what makes a list
    /// tight is the schema's business, not this state's.
    pub fn render_list(
        &mut self,
        node: &Node,
        delim: &str,
        tight: bool,
        first_delim: &dyn Fn(usize) -> String,
    ) {
        if self
            .closed
            .as_ref()
            .is_some_and(|c| c.type_id() == node.type_id())
        {
            // Two lists of the same type in a row: a blank line alone would not
            // keep a reader from joining them, so the rule that picked this
            // list's marker has to differ. See `distinct_marker` in the preset.
            self.flush_close(3);
        } else {
            // Separate the list from whatever came before it while the *outer*
            // tightness is still in force.
            self.flush_close(self.flush_size());
        }
        let previous = self.in_tight_list;
        self.in_tight_list = tight;
        for (index, child) in node.children().enumerate() {
            if index > 0 && tight {
                self.flush_close(1);
            }
            let marker = first_delim(index);
            self.wrap_block(delim, Some(&marker), node, |state| {
                state.render(child, Some(node), index)
            });
        }
        self.in_tight_list = previous;
    }
}
