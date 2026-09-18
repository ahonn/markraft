//! Content expressions and the finite automaton they compile to.
//!
//! The grammar is the one used by ProseMirror and Wordgard:
//!
//! ```text
//! expr   = seq ("|" seq)*
//! seq    = repeat+
//! repeat = atom ("*" | "+" | "?" | "{" n ("," m?)? "}")*
//! atom   = "(" expr ")" | name
//! ```
//!
//! A `name` refers to a node type or a node group. The expression is compiled
//! to an NFA which is then determinised; the resulting DFA is what
//! [`ContentMatch`] walks, so matching a fragment is a linear scan over its
//! children.

use std::collections::BTreeMap;

use super::NodeTypeId;

/// A compiled content expression: a deterministic automaton over node types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentExpr {
    states: Vec<State>,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    valid_end: bool,
    next: Vec<(NodeTypeId, usize)>,
}

/// A position inside a [`ContentExpr`]: the prefix of children matched so far.
///
/// `ContentMatch` is a cheap `Copy` handle into the automaton.
#[derive(Debug, Clone, Copy)]
pub struct ContentMatch<'a> {
    expr: &'a ContentExpr,
    state: usize,
}

impl PartialEq for ContentMatch<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.expr, other.expr) && self.state == other.state
    }
}

impl Eq for ContentMatch<'_> {}

impl ContentExpr {
    /// The expression that accepts no children at all, used by leaf types.
    pub fn empty() -> ContentExpr {
        ContentExpr {
            states: vec![State {
                valid_end: true,
                next: Vec::new(),
            }],
            source: String::new(),
        }
    }

    /// The expression source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether this expression accepts no children at all.
    pub fn is_empty(&self) -> bool {
        self.states.len() == 1 && self.states[0].next.is_empty()
    }

    /// The start state of the automaton.
    pub fn start(&self) -> ContentMatch<'_> {
        ContentMatch {
            expr: self,
            state: 0,
        }
    }

    /// Every node type that may appear anywhere in this expression, in the
    /// order the automaton first mentions them.
    pub fn mentioned_types(&self) -> Vec<NodeTypeId> {
        let mut out: Vec<NodeTypeId> = Vec::new();
        for state in &self.states {
            for (ty, _) in &state.next {
                if !out.contains(ty) {
                    out.push(*ty);
                }
            }
        }
        out
    }

    /// Compile `source`. `resolve` maps a name to the node types it stands for
    /// (a single type, or every member of a group).
    pub fn compile(
        source: &str,
        resolve: &mut dyn FnMut(&str) -> Option<Vec<NodeTypeId>>,
    ) -> Result<ContentExpr, String> {
        let tokens = tokenize(source)?;
        if tokens.is_empty() {
            return Ok(ContentExpr {
                states: vec![State {
                    valid_end: true,
                    next: Vec::new(),
                }],
                source: source.to_string(),
            });
        }
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
            resolve,
        };
        let expr = parser.parse_expr()?;
        if parser.pos != tokens.len() {
            return Err(format!("unexpected token `{}`", tokens[parser.pos]));
        }
        let nfa = Nfa::build(&expr);
        Ok(ContentExpr {
            states: nfa.determinise(),
            source: source.to_string(),
        })
    }
}

impl<'a> ContentMatch<'a> {
    /// Whether the content matched so far forms a complete, valid sequence.
    pub fn valid_end(&self) -> bool {
        self.expr.states[self.state].valid_end
    }

    /// Advance over one child of type `ty`, or `None` if it is not allowed
    /// here.
    pub fn match_type(&self, ty: NodeTypeId) -> Option<ContentMatch<'a>> {
        self.expr.states[self.state]
            .next
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, next)| ContentMatch {
                expr: self.expr,
                state: *next,
            })
    }

    /// Advance over a sequence of child types, failing at the first one that
    /// does not fit.
    pub fn match_types(
        &self,
        types: impl IntoIterator<Item = NodeTypeId>,
    ) -> Option<ContentMatch<'a>> {
        let mut cur = *self;
        for ty in types {
            cur = cur.match_type(ty)?;
        }
        Some(cur)
    }

    /// The node types that may directly follow this position.
    pub fn next_types(&self) -> impl Iterator<Item = NodeTypeId> + '_ {
        self.expr.states[self.state].next.iter().map(|(t, _)| *t)
    }

    /// The first type that may follow here and that `creatable` accepts, used
    /// when the model has to invent a node.
    pub fn default_type(&self, creatable: &dyn Fn(NodeTypeId) -> bool) -> Option<NodeTypeId> {
        self.next_types().find(|ty| creatable(*ty))
    }

    /// Find the shortest chain of container types that `target` must be wrapped
    /// in to be allowed here.
    ///
    /// Returns an empty vector when `target` fits directly and `None` when no
    /// wrapping helps. The chain is outermost-first.
    pub fn find_wrapping(
        &self,
        target: NodeTypeId,
        creatable: &dyn Fn(NodeTypeId) -> bool,
        content_of: &dyn Fn(NodeTypeId) -> Option<&'a ContentExpr>,
    ) -> Option<Vec<NodeTypeId>> {
        // Breadth-first search over wrapper chains. A candidate wrapper must be
        // allowed at the current position and, since exactly one node is
        // inserted, the outer content must be able to end right after it.
        let mut entries: Vec<(ContentMatch<'a>, Option<NodeTypeId>, Option<usize>)> =
            vec![(*self, None, None)];
        let mut seen: Vec<NodeTypeId> = Vec::new();
        let mut head = 0;
        while head < entries.len() {
            let current = head;
            head += 1;
            let m = entries[current].0;
            if m.match_type(target).is_some() {
                let mut chain = Vec::new();
                let mut cursor = Some(current);
                while let Some(i) = cursor {
                    match entries[i].1 {
                        Some(ty) => {
                            chain.push(ty);
                            cursor = entries[i].2;
                        }
                        None => break,
                    }
                }
                chain.reverse();
                return Some(chain);
            }
            let outer = entries[current].1.is_some();
            for (candidate, next_state) in m.expr.states[m.state].next.clone() {
                if seen.contains(&candidate) || !creatable(candidate) {
                    continue;
                }
                let next = ContentMatch {
                    expr: m.expr,
                    state: next_state,
                };
                if outer && !next.valid_end() {
                    continue;
                }
                let Some(inner) = content_of(candidate) else {
                    continue;
                };
                if inner.is_empty() {
                    continue;
                }
                seen.push(candidate);
                entries.push((inner.start(), Some(candidate), Some(current)));
            }
        }
        None
    }

    /// The node types that have to be inserted at this position so that the
    /// sequence `after` becomes valid content.
    ///
    /// With `to_end` set, the result must also be a valid end of the content.
    /// Returns `None` when no chain of `creatable` types works.
    pub fn fill_before(
        &self,
        after: &[NodeTypeId],
        to_end: bool,
        creatable: &dyn Fn(NodeTypeId) -> bool,
    ) -> Option<Vec<NodeTypeId>> {
        let mut acc = Vec::new();
        let mut seen = vec![self.state];
        if fill_search(*self, after, to_end, creatable, &mut acc, &mut seen) {
            Some(acc)
        } else {
            None
        }
    }
}

fn fill_search(
    m: ContentMatch<'_>,
    after: &[NodeTypeId],
    to_end: bool,
    creatable: &dyn Fn(NodeTypeId) -> bool,
    acc: &mut Vec<NodeTypeId>,
    seen: &mut Vec<usize>,
) -> bool {
    if let Some(end) = m.match_types(after.iter().copied())
        && (!to_end || end.valid_end())
    {
        return true;
    }
    for (ty, next) in m.expr.states[m.state].next.clone() {
        if !creatable(ty) || seen.contains(&next) {
            continue;
        }
        seen.push(next);
        acc.push(ty);
        let found = fill_search(
            ContentMatch {
                expr: m.expr,
                state: next,
            },
            after,
            to_end,
            creatable,
            acc,
            seen,
        );
        if found {
            return true;
        }
        acc.pop();
    }
    false
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn tokenize(source: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if "|()*+?{},".contains(c) {
            chars.next();
            out.push(c.to_string());
        } else if c.is_alphanumeric() || c == '_' {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    word.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(word);
        } else {
            return Err(format!("unexpected character `{c}`"));
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Expr {
    /// Matches one node whose type is any of these.
    Types(Vec<NodeTypeId>),
    Seq(Vec<Expr>),
    Choice(Vec<Expr>),
    /// `{min,max}`; `max == None` is unbounded.
    Range {
        min: u32,
        max: Option<u32>,
        inner: Box<Expr>,
    },
}

struct Parser<'a, 'r> {
    tokens: &'a [String],
    pos: usize,
    resolve: &'r mut dyn FnMut(&str) -> Option<Vec<NodeTypeId>>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| s.as_str())
    }

    fn eat(&mut self, tok: &str) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        let first = self.parse_seq()?;
        if self.peek() != Some("|") {
            return Ok(first);
        }
        let mut options = vec![first];
        while self.eat("|") {
            options.push(self.parse_seq()?);
        }
        Ok(Expr::Choice(options))
    }

    fn parse_seq(&mut self) -> Result<Expr, String> {
        let mut items = Vec::new();
        while self.peek().is_some_and(|t| t != "|" && t != ")") {
            items.push(self.parse_repeat()?);
        }
        match items.len() {
            0 => Err("empty sequence".to_string()),
            1 => Ok(items.remove(0)),
            _ => Ok(Expr::Seq(items)),
        }
    }

    fn parse_repeat(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_atom()?;
        loop {
            if self.eat("*") {
                expr = Expr::Range {
                    min: 0,
                    max: None,
                    inner: Box::new(expr),
                };
            } else if self.eat("+") {
                expr = Expr::Range {
                    min: 1,
                    max: None,
                    inner: Box::new(expr),
                };
            } else if self.eat("?") {
                expr = Expr::Range {
                    min: 0,
                    max: Some(1),
                    inner: Box::new(expr),
                };
            } else if self.eat("{") {
                let min = self.parse_number()?;
                let max = if self.eat(",") {
                    if self.peek() == Some("}") {
                        None
                    } else {
                        Some(self.parse_number()?)
                    }
                } else {
                    Some(min)
                };
                if !self.eat("}") {
                    return Err("expected `}`".to_string());
                }
                if let Some(max) = max
                    && max < min
                {
                    return Err(format!("invalid range {{{min},{max}}}"));
                }
                expr = Expr::Range {
                    min,
                    max,
                    inner: Box::new(expr),
                };
            } else {
                return Ok(expr);
            }
        }
    }

    fn parse_number(&mut self) -> Result<u32, String> {
        let tok = self.peek().ok_or_else(|| "expected a number".to_string())?;
        let n: u32 = tok
            .parse()
            .map_err(|_| format!("expected a number, got `{tok}`"))?;
        self.pos += 1;
        Ok(n)
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        if self.eat("(") {
            let inner = self.parse_expr()?;
            if !self.eat(")") {
                return Err("expected `)`".to_string());
            }
            return Ok(inner);
        }
        let tok = self
            .peek()
            .ok_or_else(|| "unexpected end of expression".to_string())?
            .to_string();
        if !tok
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            return Err(format!("unexpected token `{tok}`"));
        }
        self.pos += 1;
        let types = (self.resolve)(&tok).ok_or_else(|| format!("unknown node or group `{tok}`"))?;
        if types.is_empty() {
            return Err(format!("`{tok}` does not match any node type"));
        }
        Ok(Expr::Types(types))
    }
}

// ---------------------------------------------------------------------------
// NFA construction and determinisation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Edge {
    /// `None` marks an epsilon edge.
    ty: Option<NodeTypeId>,
    to: usize,
}

struct Nfa {
    nodes: Vec<Vec<Edge>>,
    accept: usize,
}

impl Nfa {
    fn build(expr: &Expr) -> Nfa {
        let mut nfa = Nfa {
            nodes: vec![Vec::new()],
            accept: 0,
        };
        nfa.accept = nfa.compile(expr, 0);
        nfa
    }

    fn node(&mut self) -> usize {
        self.nodes.push(Vec::new());
        self.nodes.len() - 1
    }

    fn edge(&mut self, from: usize, ty: Option<NodeTypeId>, to: usize) {
        self.nodes[from].push(Edge { ty, to });
    }

    /// Compile `expr` starting at `from`, returning the state it ends in.
    fn compile(&mut self, expr: &Expr, from: usize) -> usize {
        match expr {
            Expr::Types(types) => {
                let to = self.node();
                for ty in types {
                    self.edge(from, Some(*ty), to);
                }
                to
            }
            Expr::Seq(items) => {
                let mut cur = from;
                for item in items {
                    cur = self.compile(item, cur);
                }
                cur
            }
            Expr::Choice(options) => {
                let to = self.node();
                for option in options {
                    let end = self.compile(option, from);
                    self.edge(end, None, to);
                }
                to
            }
            Expr::Range { min, max, inner } => {
                let mut cur = from;
                for _ in 0..*min {
                    cur = self.compile(inner, cur);
                }
                match max {
                    None => {
                        // The loop needs a state of its own. Looping back to
                        // `cur` would be wrong whenever `cur` is shared with
                        // another branch (every option of a choice compiles
                        // from the same state), because the branch's outgoing
                        // edges would stay reachable after an iteration.
                        let loop_start = self.node();
                        self.edge(cur, None, loop_start);
                        let end = self.compile(inner, loop_start);
                        self.edge(end, None, loop_start);
                        loop_start
                    }
                    Some(max) => {
                        let out = self.node();
                        self.edge(cur, None, out);
                        for _ in *min..*max {
                            cur = self.compile(inner, cur);
                            self.edge(cur, None, out);
                        }
                        out
                    }
                }
            }
        }
    }

    fn epsilon_closure(&self, start: &[usize]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut stack: Vec<usize> = start.to_vec();
        while let Some(n) = stack.pop() {
            if out.contains(&n) {
                continue;
            }
            out.push(n);
            for edge in &self.nodes[n] {
                if edge.ty.is_none() {
                    stack.push(edge.to);
                }
            }
        }
        out.sort_unstable();
        out
    }

    fn determinise(&self) -> Vec<State> {
        let mut sets: Vec<Vec<usize>> = vec![self.epsilon_closure(&[0])];
        let mut states: Vec<State> = Vec::new();
        let mut i = 0;
        while i < sets.len() {
            let set = sets[i].clone();
            let valid_end = set.contains(&self.accept);
            // Group labelled edges by node type, keeping the order in which the
            // types are first mentioned so `default_type` is deterministic.
            let mut order: Vec<NodeTypeId> = Vec::new();
            let mut targets: BTreeMap<NodeTypeId, Vec<usize>> = BTreeMap::new();
            for &n in &set {
                for edge in &self.nodes[n] {
                    if let Some(ty) = edge.ty {
                        let entry = targets.entry(ty).or_default();
                        if entry.is_empty() {
                            order.push(ty);
                        }
                        if !entry.contains(&edge.to) {
                            entry.push(edge.to);
                        }
                    }
                }
            }
            let mut next = Vec::new();
            for ty in order {
                let closure = self.epsilon_closure(&targets[&ty]);
                let index = match sets.iter().position(|s| *s == closure) {
                    Some(index) => index,
                    None => {
                        sets.push(closure);
                        sets.len() - 1
                    }
                };
                next.push((ty, index));
            }
            states.push(State { valid_end, next });
            i += 1;
        }
        states
    }
}
