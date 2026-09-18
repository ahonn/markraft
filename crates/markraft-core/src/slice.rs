//! Token runs: the content that changes insert and that copy/paste carries.

use crate::fragment::Fragment;
use crate::node::{Markup, Node};

/// One token in a document's token stream.
///
/// `Close` carries the markup of the container it closes. That makes a token
/// run self-describing, so a run that closes containers it did not open can
/// still be turned back into a [`Slice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Opens a container.
    Open(Markup),
    /// Closes the innermost open container.
    Close(Markup),
    /// A complete node: a leaf, a text leaf, or a whole untouched subtree.
    Node(Node),
}

impl Token {
    /// The number of positions this token occupies.
    pub fn size(&self) -> usize {
        match self {
            Token::Open(_) | Token::Close(_) => 1,
            Token::Node(node) => node.node_size(),
        }
    }

    /// The markup this token carries.
    pub fn markup(&self) -> &Markup {
        match self {
            Token::Open(markup) | Token::Close(markup) => markup,
            Token::Node(node) => node.markup(),
        }
    }
}

/// Flatten a node into `Open`, content tokens and `Close`.
pub fn node_tokens(node: &Node) -> Vec<Token> {
    if !node.is_container() {
        return vec![Token::Node(node.clone())];
    }
    let mut out = vec![Token::Open(node.markup().clone())];
    for child in node.children() {
        out.push(Token::Node(child.clone()));
    }
    out.push(Token::Close(node.markup().clone()));
    out
}

/// The total size of a token run.
pub fn tokens_size(tokens: &[Token]) -> usize {
    tokens.iter().map(Token::size).sum()
}

/// How far below its starting depth a token run dips.
///
/// The result is zero or negative. A run that closes containers it did not open
/// can only be spliced into a subtree that is at least that many levels deeper
/// than the splice point's parent.
pub fn min_prefix_delta(tokens: &[Token]) -> isize {
    let mut delta = 0isize;
    let mut min = 0isize;
    for token in tokens {
        match token {
            Token::Open(_) => delta += 1,
            Token::Close(_) => delta -= 1,
            Token::Node(_) => {}
        }
        min = min.min(delta);
    }
    min
}

/// Cut a token run down to the positions in `from..to`, splitting text leaves
/// and descending into containers that straddle a boundary.
pub fn tokens_cut(tokens: &[Token], from: usize, to: usize) -> Vec<Token> {
    let mut out = Vec::new();
    let mut pos = 0;
    for token in tokens {
        if pos >= to {
            break;
        }
        let end = pos + token.size();
        if end > from {
            if pos >= from && end <= to {
                out.push(token.clone());
            } else {
                match token {
                    Token::Node(node) if node.is_text() => {
                        let start = from.saturating_sub(pos);
                        let stop = (to - pos).min(node.text_len());
                        if stop > start {
                            out.push(Token::Node(node.cut_text(start, stop)));
                        }
                    }
                    Token::Node(node) if node.is_container() => {
                        let inner = node_tokens(node);
                        out.extend(tokens_cut(
                            &inner,
                            from.saturating_sub(pos),
                            (to - pos).min(end - pos),
                        ));
                    }
                    other => out.push(other.clone()),
                }
            }
        }
        pos = end;
    }
    out
}

/// A run of tokens, expressed as a fragment with open sides.
///
/// `open_start` and `open_end` say how many of the outermost containers on each
/// side are *not* part of the run: their content merges with whatever surrounds
/// the insertion point. [`Slice::size`] is the number of tokens the run
/// occupies, which is what a change's inserted length counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Slice {
    content: Fragment,
    open_start: usize,
    open_end: usize,
}

impl Slice {
    /// The empty slice.
    pub fn empty() -> Slice {
        Slice {
            content: Fragment::empty(),
            open_start: 0,
            open_end: 0,
        }
    }

    /// Build a slice. Open depths are clamped to the depth the content
    /// actually provides.
    pub fn new(content: Fragment, open_start: usize, open_end: usize) -> Slice {
        let max_start = open_depth(&content, true);
        let max_end = open_depth(&content, false);
        Slice {
            content,
            open_start: open_start.min(max_start),
            open_end: open_end.min(max_end),
        }
    }

    /// A slice that inserts the given nodes as siblings.
    pub fn from_fragment(content: Fragment) -> Slice {
        Slice::new(content, 0, 0)
    }

    /// The slice's content, including the nodes that are open on either side.
    pub fn content(&self) -> &Fragment {
        &self.content
    }

    /// How many containers the run starts inside of.
    pub fn open_start(&self) -> usize {
        self.open_start
    }

    /// How many containers the run ends inside of.
    pub fn open_end(&self) -> usize {
        self.open_end
    }

    /// The number of positions the run occupies.
    pub fn size(&self) -> usize {
        self.content
            .size()
            .saturating_sub(self.open_start + self.open_end)
    }

    /// Whether the run holds no tokens.
    pub fn is_empty(&self) -> bool {
        self.size() == 0
    }

    /// The run's tokens.
    pub fn tokens(&self) -> Vec<Token> {
        let mut out = Vec::new();
        emit_tokens(&self.content, self.open_start, self.open_end, &mut out);
        out
    }

    /// Rebuild a slice from a token run.
    ///
    /// Unmatched `Close` tokens at the start become `open_start` levels (their
    /// markup is taken from the tokens themselves) and unmatched `Open` tokens
    /// at the end become `open_end` levels.
    ///
    /// This is a *normalising* conversion, not an exact inverse of
    /// [`Slice::tokens`]. Runs that describe the same content collapse to one
    /// form: a balanced `Open`/`Close` pair becomes a single node token, and
    /// adjacent text with identical markup is merged. The total size, the open
    /// depths and the resulting document are unchanged, and the result is a
    /// fixed point — `Slice::from_tokens(s.tokens()).tokens() == s.tokens()`
    /// holds for any slice `s` built this way. [`ChangeSet`](crate::ChangeSet)
    /// stores inserted runs in this form so that equal change sets compare
    /// equal.
    pub fn from_tokens(tokens: &[Token]) -> Slice {
        let mut stack: Vec<(Option<Markup>, Vec<Node>)> = vec![(None, Vec::new())];
        let mut open_start = 0;
        for token in tokens {
            match token {
                Token::Open(markup) => stack.push((Some(markup.clone()), Vec::new())),
                Token::Close(markup) => {
                    if stack.len() == 1 {
                        let content = std::mem::take(&mut stack[0].1);
                        stack[0].1 = vec![Node::container(
                            markup.clone(),
                            Fragment::from_nodes(content),
                        )];
                        open_start += 1;
                    } else {
                        let (markup, content) = stack.pop().expect("stack is not empty");
                        let node = Node::container(
                            markup.expect("only the root frame has no markup"),
                            Fragment::from_nodes(content),
                        );
                        stack.last_mut().expect("root frame remains").1.push(node);
                    }
                }
                Token::Node(node) => stack
                    .last_mut()
                    .expect("root frame remains")
                    .1
                    .push(node.clone()),
            }
        }
        let mut open_end = 0;
        while stack.len() > 1 {
            let (markup, content) = stack.pop().expect("stack is not empty");
            let node = Node::container(
                markup.expect("only the root frame has no markup"),
                Fragment::from_nodes(content),
            );
            stack.last_mut().expect("root frame remains").1.push(node);
            open_end += 1;
        }
        let content = Fragment::from_nodes(std::mem::take(&mut stack[0].1));
        Slice {
            content,
            open_start,
            open_end,
        }
    }

    /// The sub-run between two token offsets.
    pub fn cut(&self, from: usize, to: usize) -> Slice {
        Slice::from_tokens(&tokens_cut(&self.tokens(), from, to))
    }

    /// Concatenate two runs.
    pub fn concat(&self, other: &Slice) -> Slice {
        let mut tokens = self.tokens();
        tokens.extend(other.tokens());
        Slice::from_tokens(&tokens)
    }

    /// The text in this run, using the same rules as
    /// [`Node::text_between`](crate::Node::text_between) for leaves.
    pub fn text_content(&self, leaf_text: Option<&dyn Fn(&Node) -> String>) -> String {
        fn walk(node: &Node, leaf_text: Option<&dyn Fn(&Node) -> String>, out: &mut String) {
            if let Some(text) = node.text() {
                out.push_str(text);
            } else if node.is_leaf() {
                if let Some(f) = leaf_text {
                    out.push_str(&f(node));
                }
            } else {
                for child in node.children() {
                    walk(child, leaf_text, out);
                }
            }
        }
        let mut out = String::new();
        for node in self.content.iter() {
            walk(node, leaf_text, &mut out);
        }
        out
    }
}

fn open_depth(content: &Fragment, start: bool) -> usize {
    let mut depth = 0;
    let mut current = if start {
        content.first_child()
    } else {
        content.last_child()
    };
    while let Some(node) = current {
        if !node.is_container() {
            break;
        }
        depth += 1;
        current = if start {
            node.content().first_child()
        } else {
            node.content().last_child()
        };
    }
    depth
}

fn emit_tokens(content: &Fragment, open_start: usize, open_end: usize, out: &mut Vec<Token>) {
    let last = content.child_count().saturating_sub(1);
    for (i, child) in content.iter().enumerate() {
        let child_start = if i == 0 { open_start } else { 0 };
        let child_end = if i == last { open_end } else { 0 };
        if child_start == 0 && child_end == 0 {
            out.push(Token::Node(child.clone()));
            continue;
        }
        if child_start == 0 {
            out.push(Token::Open(child.markup().clone()));
        }
        emit_tokens(
            child.content(),
            child_start.saturating_sub(1),
            child_end.saturating_sub(1),
            out,
        );
        if child_end == 0 {
            out.push(Token::Close(child.markup().clone()));
        }
    }
}
