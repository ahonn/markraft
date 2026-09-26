//! Where a caret goes when the note under it is replaced by the version on disk.
//!
//! A file changed by another editor, a sync or a pull is read back whole, and
//! the editor starts over on the new document. What stays the same at either end
//! of the two documents is taken as untouched, so a caret before the change
//! keeps its place and one after it moves with the text around it; a caret in
//! the part that changed goes to where that part starts.

use markraft_core::ends::KeptEnds;
use markraft_core::{Node, Schema, Selection};

/// `selection` in `old`, carried into `new`.
pub(super) fn carry(schema: &Schema, old: &Node, new: &Node, selection: &Selection) -> Selection {
    let (before, after) = (keys(old), keys(new));
    let kept = KeptEnds::of(&before, &after, |a, b| a == b);
    let (prefix, suffix) = (kept.prefix(), kept.suffix());
    let map = |pos: usize| {
        if pos <= prefix {
            pos
        } else if pos >= before.len() - suffix {
            pos + after.len() - before.len()
        } else {
            prefix
        }
    };
    let doc = old;
    match selection {
        Selection::Text { .. } => {
            let (anchor, head) = (map(selection.anchor(doc)), map(selection.head(doc)));
            let text = Selection::text(anchor, head);
            if text.check(new, schema).is_ok() {
                text
            } else {
                Selection::near(schema, new, head, 1)
            }
        }
        _ => Selection::near(schema, new, map(selection.from(doc)), 1),
    }
}

/// What stands at each position of `doc`'s content: a node's opening and
/// closing, a character, or a leaf.
fn keys(doc: &Node) -> Vec<Key> {
    fn walk(node: &Node, out: &mut Vec<Key>) {
        if let Some(text) = node.text() {
            out.extend(text.chars().map(Key::Char));
        } else if node.is_leaf() {
            out.push(Key::Leaf(format!("{:?}", node.type_id())));
        } else {
            out.push(Key::Open(format!("{:?}", node.type_id())));
            for child in node.children() {
                walk(child, out);
            }
            out.push(Key::Close);
        }
    }
    let mut out = Vec::with_capacity(doc.content_size());
    for child in doc.children() {
        walk(child, &mut out);
    }
    out
}

#[derive(PartialEq)]
enum Key {
    Open(String),
    Close,
    Char(char),
    Leaf(String),
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::doc;

    fn carried(old: &str, new: &str, pos: usize) -> usize {
        let (old, new) = (doc::from_markdown(old), doc::from_markdown(new));
        let selection = carry(doc::schema(), &old, &new, &Selection::cursor(pos));
        selection.head(&new)
    }

    #[test]
    fn a_caret_keeps_its_place_before_a_change_and_moves_with_the_text_after_one() {
        // `hello world`: the caret after `hello` is position 6.
        assert_eq!(
            carried("hello world\n", "hello world!\n", 6),
            6,
            "a change after"
        );
        assert_eq!(
            carried("hello world\n", "oh hello world\n", 6),
            9,
            "a change before"
        );
        assert_eq!(
            carried("one\n\nhello world\n", "hello world\n", 11),
            6,
            "a block removed before"
        );
        // Inside what changed: where the change starts.
        assert_eq!(carried("hello world\n", "hello there\n", 9), 7);
    }
}
