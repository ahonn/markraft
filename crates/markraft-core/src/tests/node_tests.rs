//! Node, fragment, position and slice behaviour.

use super::support::*;
use crate::attrs;
use crate::error::NodeError;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::Node;
use crate::slice::{Slice, Token};

fn sample(schema: &crate::schema::Schema) -> Node {
    doc(
        schema,
        [
            n(schema, "paragraph", [t(schema, "hi"), img(schema, "a.png")]),
            n(
                schema,
                "blockquote",
                [n(schema, "paragraph", [t(schema, "deep")])],
            ),
        ],
    )
}

#[test]
fn token_sizes_follow_the_coordinate_frame() {
    let schema = test_schema();
    assert_eq!(t(&schema, "hi").node_size(), 2);
    assert_eq!(img(&schema, "a.png").node_size(), 1);
    let p = n(&schema, "paragraph", [t(&schema, "hi")]);
    assert_eq!(p.content_size(), 2);
    assert_eq!(p.node_size(), 4);
    let d = sample(&schema);
    // paragraph(2 + "hi" + image) = 5, blockquote(2 + paragraph(2 + 4)) = 8
    assert_eq!(d.child(0).node_size(), 5);
    assert_eq!(d.child(1).node_size(), 8);
    assert_eq!(d.content_size(), 13);
}

#[test]
fn removing_a_mark_type_keeps_the_other_marks() {
    let schema = shared_schema();
    let both = MarkSet::from_marks(&schema, [m(&schema, "strong"), m(&schema, "em")]);
    let strong = schema.mark_id("strong").expect("known");
    let rest = both.remove_type(strong);
    assert!(!rest.contains(&m(&schema, "strong")));
    assert!(rest.contains(&m(&schema, "em")));
}

#[test]
fn has_markup_compares_type_attributes_and_marks() {
    let schema = shared_schema();
    let heading = na(&schema, "heading", attrs! {"level" => 2}, [t(&schema, "a")]);
    let heading_type = schema.node_id("heading").expect("known");
    let paragraph_type = schema.node_id("paragraph").expect("known");
    assert!(heading.has_markup(heading_type, heading.attrs(), &MarkSet::empty()));
    assert!(!heading.has_markup(paragraph_type, heading.attrs(), &MarkSet::empty()));
    assert!(!heading.has_markup(heading_type, &attrs! {"level" => 3}, &MarkSet::empty()));
}

#[test]
fn text_counts_unicode_scalar_values() {
    let schema = test_schema();
    let text = t(&schema, "aé漢🙂");
    assert_eq!(text.node_size(), 4);
    assert_eq!(text.cut_text(1, 3).unwrap().text(), Some("é漢"));
    let p = n(&schema, "paragraph", [text]);
    assert_eq!(p.content_size(), 4);
    assert_eq!(p.text_between(&schema, 1, 3, None, None), "é漢".to_string());
}

#[test]
fn cuts_inside_the_range_return_the_covered_content() {
    let schema = test_schema();
    let d = sample(&schema);
    let expected = Fragment::from_node(n(&schema, "paragraph", [t(&schema, "hi")]));
    assert_eq!(d.content().cut(1, 3), Ok(expected.clone()));
    assert_eq!(d.cut(1, 3).unwrap().content(), &expected);
    assert_eq!(d.cut(0, 13).unwrap(), d);
    let text = t(&schema, "abc");
    assert_eq!(text.cut(1, 3).unwrap().text(), Some("bc"));
    assert_eq!(text.cut_text(3, 3).unwrap().text(), Some(""));
    // A non-text leaf has no content: only the empty range is in bounds.
    let image = img(&schema, "a.png");
    assert_eq!(image.cut(0, 0), Ok(image.clone()));
}

fn out_of_range<T>(pos: usize, size: usize) -> Result<T, NodeError> {
    Err(NodeError::PosOutOfRange { pos, size })
}

#[test]
fn cuts_past_the_end_report_the_end() {
    let schema = test_schema();
    let d = sample(&schema);
    assert_eq!(d.content().cut(0, 14), out_of_range(14, 13));
    assert_eq!(d.cut(2, 20), out_of_range(20, 13));
    let text = t(&schema, "abc");
    assert_eq!(text.cut_text(1, 4), out_of_range(4, 3));
    assert_eq!(text.cut(0, 9), out_of_range(9, 3));
    assert_eq!(img(&schema, "a.png").cut(0, 1), out_of_range(1, 0));
}

#[test]
fn reversed_cuts_report_their_start() {
    let schema = test_schema();
    let d = sample(&schema);
    assert_eq!(d.content().cut(3, 1), out_of_range(3, 13));
    assert_eq!(d.cut(5, 4), out_of_range(5, 13));
    // Reversed and past the end at once still reports the start.
    assert_eq!(d.cut(30, 20), out_of_range(30, 13));
    let text = t(&schema, "abc");
    assert_eq!(text.cut_text(2, 1), out_of_range(2, 3));
    assert_eq!(text.cut(3, 0), out_of_range(3, 3));
}

#[test]
fn find_index_is_none_past_the_end() {
    let schema = test_schema();
    let content = sample(&schema).content().clone();
    assert_eq!(content.find_index(0), Some((0, 0)));
    assert_eq!(content.find_index(3), Some((0, 0)));
    assert_eq!(content.find_index(5), Some((1, 5)));
    assert_eq!(content.find_index(13), Some((2, 13)));
    assert_eq!(content.find_index(14), None);
    assert_eq!(Fragment::empty().find_index(0), Some((0, 0)));
    assert_eq!(Fragment::empty().find_index(1), None);
    for pos in 0..=content.size() {
        assert_eq!(
            content.find_index(pos),
            Some(content.find_index_unchecked(pos))
        );
    }
}

#[test]
fn unchecked_cuts_agree_with_checked_ones_in_range() {
    let schema = test_schema();
    let d = sample(&schema);
    let size = d.content_size();
    for from in 0..=size {
        for to in from..=size {
            assert_eq!(
                d.content().cut(from, to),
                Ok(d.content().cut_unchecked(from, to))
            );
            assert_eq!(d.cut(from, to), Ok(d.cut_unchecked(from, to)));
        }
    }
    let text = t(&schema, "aé漢");
    for from in 0..=3 {
        for to in from..=3 {
            assert_eq!(
                text.cut_text(from, to),
                Ok(text.cut_text_unchecked(from, to))
            );
        }
    }
}

#[test]
fn unchecked_text_cuts_clamp_to_the_text() {
    let schema = test_schema();
    let text = t(&schema, "abc");
    assert_eq!(text.cut_text_unchecked(1, 10).text(), Some("bc"));
    assert_eq!(text.cut_text_unchecked(0, 10), text);
    assert_eq!(text.cut_text_unchecked(7, 9).text(), Some(""));
    // A reversed range collapses at its start instead of running backwards.
    assert_eq!(text.cut_text_unchecked(2, 1).text(), Some(""));
}

#[test]
fn fragments_merge_adjacent_text_with_equal_markup() {
    let schema = test_schema();
    let fragment = Fragment::from_nodes([
        t(&schema, "ab"),
        t(&schema, "cd"),
        tm(&schema, "ef", &["strong"]),
        tm(&schema, "gh", &["strong"]),
        t(&schema, "ij"),
    ]);
    assert_eq!(fragment.child_count(), 3);
    assert_eq!(fragment.child(0).text(), Some("abcd"));
    assert_eq!(fragment.child(1).text(), Some("efgh"));
    assert_eq!(fragment.size(), 10);
    // Empty text is dropped outright.
    assert!(Fragment::from_nodes([t(&schema, "")]).is_empty());
}

#[test]
fn structural_sharing_keeps_untouched_subtrees() {
    let schema = test_schema();
    let d = sample(&schema);
    let replaced = d.copy(
        d.content()
            .replace_child(0, n(&schema, "paragraph", [t(&schema, "new")])),
    );
    assert!(d.child(1).ptr_eq(replaced.child(1)));
    assert!(!d.child(0).ptr_eq(replaced.child(0)));
    assert_eq!(d.child(1), replaced.child(1));
}

#[test]
fn node_at_and_neighbours() {
    let schema = test_schema();
    let d = sample(&schema);
    assert_eq!(
        d.node_at(0).map(|n| n.type_id()),
        schema.node_id("paragraph")
    );
    assert_eq!(
        d.node_at(1).and_then(|n| n.text().map(String::from)),
        Some("hi".into())
    );
    assert_eq!(
        d.node_at(5).map(|n| n.type_id()),
        schema.node_id("blockquote")
    );
    assert_eq!(
        d.node_before(5).map(|n| n.type_id()),
        schema.node_id("paragraph")
    );
    assert_eq!(
        d.node_after(5).map(|n| n.type_id()),
        schema.node_id("blockquote")
    );
    assert!(d.node_before(0).is_none());
    assert!(d.node_after(13).is_none());
}

#[test]
fn resolve_reports_the_ancestor_chain() {
    let schema = test_schema();
    let d = sample(&schema);
    // Position 8 sits between "d" and "eep" inside blockquote > paragraph.
    let r = d.resolve(8).expect("in range");
    assert_eq!(r.depth(), 2);
    assert_eq!(r.node(0).type_id(), schema.node_id("doc").expect("known"));
    assert_eq!(
        r.node(1).type_id(),
        schema.node_id("blockquote").expect("known")
    );
    assert_eq!(
        r.parent().type_id(),
        schema.node_id("paragraph").expect("known")
    );
    assert_eq!(r.text_offset(), 1);
    assert_eq!(r.start(2), 7);
    assert_eq!(r.end(2), 11);
    assert_eq!(r.before(2), 6);
    assert_eq!(r.after(2), 12);
    assert_eq!(r.before(1), 5);
    assert_eq!(r.after(1), 13);
    assert_eq!(
        r.node_before().and_then(|n| n.text().map(String::from)),
        Some("d".into())
    );
    assert_eq!(
        r.node_after().and_then(|n| n.text().map(String::from)),
        Some("eep".into())
    );
    assert_eq!(r.shared_depth(9), 2);
    assert_eq!(r.shared_depth(2), 0);
    assert!(d.resolve(14).is_err());
}

#[test]
fn resolve_at_container_boundaries() {
    let schema = test_schema();
    let d = sample(&schema);
    let r = d.resolve(0).expect("in range");
    assert_eq!(r.depth(), 0);
    assert_eq!(r.index(0), 0);
    assert!(r.node_before().is_none());
    let r = d.resolve(5).expect("in range");
    assert_eq!(r.depth(), 0);
    assert_eq!(r.index(0), 1);
    let r = d.resolve(6).expect("in range");
    assert_eq!(r.depth(), 1);
    assert_eq!(r.start(1), 6);
}

#[test]
fn marks_at_a_position_respect_inclusivity() {
    let schema = test_schema();
    let p = n(
        &schema,
        "paragraph",
        [tm(&schema, "ab", &["strong"]), t(&schema, "cd")],
    );
    let d = doc(&schema, [p]);
    // Right after the strong run: strong is inclusive so it carries over.
    let r = d.resolve(3).expect("in range");
    assert!(
        r.marks(&schema)
            .contains_type(schema.mark_id("strong").expect("known"))
    );

    // A non-inclusive mark does not.
    let p = n(
        &schema,
        "paragraph",
        [
            schema.text_marked("ab", MarkSet::from_marks(&schema, [link(&schema, "u")])),
            t(&schema, "cd"),
        ],
    );
    let d = doc(&schema, [p]);
    let r = d.resolve(3).expect("in range");
    assert!(
        !r.marks(&schema)
            .contains_type(schema.mark_id("link").expect("known"))
    );
    // Inside the run it still applies.
    let r = d.resolve(2).expect("in range");
    assert!(
        r.marks(&schema)
            .contains_type(schema.mark_id("link").expect("known"))
    );
}

#[test]
fn text_between_separates_blocks() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "one")]),
            n(&schema, "paragraph", [t(&schema, "two")]),
        ],
    );
    assert_eq!(
        d.text_between(&schema, 0, d.content_size(), Some("\n"), None),
        "one\ntwo"
    );
    assert_eq!(d.text_between(&schema, 2, 7, Some("\n"), None), "ne\nt");
    let with_image = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [t(&schema, "a"), img(&schema, "x.png"), t(&schema, "b")],
        )],
    );
    let leaf = |_: &Node| "[img]".to_string();
    assert_eq!(
        with_image.text_between(&schema, 0, with_image.content_size(), None, Some(&leaf)),
        "a[img]b"
    );
}

#[test]
fn block_range_covers_sibling_blocks() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "one")]),
            n(&schema, "paragraph", [t(&schema, "two")]),
        ],
    );
    let from = d.resolve(2).expect("in range");
    let to = d.resolve(7).expect("in range");
    let range = from.block_range(&schema, &to, None).expect("a range");
    assert_eq!(range.depth(), 0);
    assert_eq!(range.start(), 0);
    assert_eq!(range.end(), 10);
    assert_eq!(range.start_index(), 0);
    assert_eq!(range.end_index(), 2);
    assert_eq!(range.content().child_count(), 2);
}

#[test]
fn slices_record_open_depths() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcd")]),
            n(&schema, "paragraph", [t(&schema, "efgh")]),
        ],
    );
    // Inside one paragraph the cut does not cross any boundary, so the slice is
    // the bare inline content.
    let slice = d.slice(2, 4).expect("in range");
    assert_eq!(slice.open_start(), 0);
    assert_eq!(slice.open_end(), 0);
    assert_eq!(slice.size(), 2);
    assert_eq!(slice.text_content(None), "bc");

    // Across two paragraphs.
    let slice = d.slice(3, 9).expect("in range");
    assert_eq!(slice.open_start(), 1);
    assert_eq!(slice.open_end(), 1);
    assert_eq!(slice.size(), 6);
    assert_eq!(slice.content().child_count(), 2);

    // Whole blocks: closed on both sides.
    let slice = d.slice(0, 6).expect("in range");
    assert_eq!(slice.open_start(), 0);
    assert_eq!(slice.open_end(), 0);
    assert_eq!(slice.size(), 6);
}

#[test]
fn slice_tokens_round_trip() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(
                &schema,
                "bullet_list",
                [
                    n(
                        &schema,
                        "list_item",
                        [n(&schema, "paragraph", [t(&schema, "one")])],
                    ),
                    n(
                        &schema,
                        "list_item",
                        [n(&schema, "paragraph", [t(&schema, "two")])],
                    ),
                ],
            ),
            n(&schema, "paragraph", [t(&schema, "tail")]),
        ],
    );
    for from in 0..=d.content_size() {
        for to in from..=d.content_size() {
            let slice = d.slice(from, to).expect("in range");
            assert_eq!(
                slice.size(),
                to - from,
                "slice {from}..{to} has the wrong size"
            );
            let tokens = slice.tokens();
            assert_eq!(crate::slice::tokens_size(&tokens), to - from);
            let rebuilt = Slice::from_tokens(&tokens);
            assert_eq!(rebuilt.tokens(), tokens, "slice {from}..{to} lost tokens");
            assert_eq!(rebuilt.size(), to - from);
        }
    }
}

#[test]
fn slice_cut_and_concat() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcd")]),
            n(&schema, "paragraph", [t(&schema, "efgh")]),
        ],
    );
    let slice = d.slice(1, 11).expect("in range");
    assert_eq!(slice.size(), 10);
    let head = slice.cut(0, 4);
    let tail = slice.cut(4, 10);
    assert_eq!(head.size(), 4);
    assert_eq!(tail.size(), 6);
    let joined = head.concat(&tail);
    assert_eq!(joined.tokens(), slice.tokens());
}

#[test]
fn check_validates_content_marks_and_text() {
    let schema = test_schema();
    let good = sample(&schema);
    good.check(&schema).expect("valid");

    // A paragraph inside a paragraph is not allowed.
    let bad = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [n(&schema, "paragraph", [t(&schema, "x")])],
        )],
    );
    assert!(matches!(
        bad.check(&schema),
        Err(NodeError::InvalidContent { .. })
    ));

    // An empty document violates `block+`.
    let empty = doc(&schema, []);
    assert!(matches!(
        empty.check(&schema),
        Err(NodeError::InvalidContent { .. })
    ));

    // Marks the type forbids are rejected.
    let marked_code = doc(&schema, [n(&schema, "code_block", [tm(&schema, "x", &[])])]);
    marked_code.check(&schema).expect("plain code text is fine");
    let code_ty = schema.node_id("code_block").expect("known");
    let bad_marks = doc(
        &schema,
        [schema
            .create(
                code_ty,
                crate::attr::Attrs::empty(),
                MarkSet::from_marks(&schema, [m(&schema, "strong")]),
                Fragment::empty(),
            )
            .expect("built")],
    );
    assert!(matches!(
        bad_marks.check(&schema),
        Err(NodeError::MarkNotAllowed { .. })
    ));
}

#[test]
fn json_round_trip() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            na(
                &schema,
                "heading",
                attrs! {"level" => 2i64},
                [tm(&schema, "title", &["strong", "em"])],
            ),
            n(
                &schema,
                "paragraph",
                [
                    schema.text_marked(
                        "link",
                        MarkSet::from_marks(&schema, [link(&schema, "https://x")]),
                    ),
                    img(&schema, "p.png"),
                    n(&schema, "hard_break", []),
                ],
            ),
        ],
    );
    d.check(&schema).expect("valid");
    let json = d.to_json(&schema);
    let back = Node::from_json(&schema, &json).expect("round trips");
    assert_eq!(back, d);
    assert_eq!(back.to_json(&schema), json);

    let slice = d.slice(2, 9).expect("in range");
    let slice_json = slice.to_json(&schema);
    assert_eq!(
        Slice::from_json(&schema, &slice_json).expect("round trips"),
        slice
    );
}

#[test]
fn add_and_remove_marks_across_leaf_boundaries() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [
                t(&schema, "ab"),
                tm(&schema, "cd", &["em"]),
                t(&schema, "ef"),
            ],
        )],
    );
    let strong = m(&schema, "strong");
    let marked = d.add_mark(&schema, 2, 6, &strong);
    marked.check(&schema).expect("valid");
    assert_eq!(
        schema.describe(&marked),
        r#"doc(paragraph("a", "b"{strong}, "cd"{strong,em}, "e"{strong}, "f"))"#
    );
    let cleaned = marked.remove_mark(&schema, 2, 6, &strong);
    assert_eq!(cleaned, d, "removing restores the merged runs");
}

#[test]
fn nodes_between_visits_outer_nodes_first() {
    let schema = test_schema();
    let d = sample(&schema);
    let mut seen = Vec::new();
    d.nodes_between(0, d.content_size(), &mut |node, pos, _, _| {
        seen.push((schema.node_type(node.type_id()).name().to_string(), pos));
        true
    });
    assert_eq!(
        seen,
        vec![
            ("paragraph".to_string(), 0),
            ("text".to_string(), 1),
            ("image".to_string(), 3),
            ("blockquote".to_string(), 5),
            ("paragraph".to_string(), 6),
            ("text".to_string(), 7),
        ]
    );
}

#[test]
fn tokens_describe_the_document_stream() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "hi")])]);
    let tokens = crate::slice::node_tokens(d.child(0));
    assert!(matches!(tokens[0], Token::Open(_)));
    assert!(matches!(tokens[1], Token::Node(_)));
    assert!(matches!(tokens[2], Token::Close(_)));
    assert_eq!(crate::slice::tokens_size(&tokens), 4);
}

#[test]
fn text_nodes_are_built_through_the_schema() {
    let schema = test_schema();
    let text_ty = schema.text_type().expect("known");
    // `create` builds content-carrying nodes; text carries a string instead.
    assert!(matches!(
        schema.create(
            text_ty,
            crate::attr::Attrs::empty(),
            MarkSet::empty(),
            Fragment::empty()
        ),
        Err(NodeError::InvalidText(_))
    ));
    // A container that claims the text type is rejected by `check`.
    let fake = Node::container(crate::node::Markup::new(text_ty), Fragment::empty());
    assert!(matches!(
        fake.check(&schema),
        Err(NodeError::InvalidText(_))
    ));
    // An empty text leaf is rejected too.
    let empty = Node::text_leaf(crate::node::Markup::new(text_ty), "");
    assert!(matches!(
        empty.check(&schema),
        Err(NodeError::InvalidText(_))
    ));
}

#[test]
fn an_empty_slice_holds_no_tokens() {
    let schema = test_schema();
    assert!(Slice::empty().is_empty());
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]);
    // A zero-width cut yields the empty slice.
    assert!(d.slice(2, 2).expect("in range").is_empty());
    // A slice that is open on both sides through the same node carries no
    // tokens either, even though its content is not empty.
    let hollow = Slice::new(Fragment::from_node(n(&schema, "paragraph", [])), 1, 1);
    assert!(hollow.is_empty());
    assert!(hollow.tokens().is_empty());
}

#[test]
fn block_range_works_from_positions_between_blocks() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ab")]),
            n(&schema, "paragraph", [t(&schema, "cd")]),
            n(&schema, "paragraph", [t(&schema, "ef")]),
        ],
    );
    // Both ends sit at block boundaries, so neither has a textblock parent.
    let from = d.resolve(0).expect("in range");
    let to = d.resolve(8).expect("in range");
    let range = from.block_range(&schema, &to, None).expect("a range");
    assert_eq!(range.depth(), 0);
    assert_eq!((range.start_index(), range.end_index()), (0, 2));
    assert_eq!((range.start(), range.end()), (0, 8));
    assert_eq!(range.content().child_count(), 2);

    // A range that starts between blocks and ends inside one.
    let to = d.resolve(9).expect("in range");
    let range = from.block_range(&schema, &to, None).expect("a range");
    assert_eq!((range.start(), range.end()), (0, 12));

    // A predicate can restrict which ancestor is chosen.
    let nested = doc(
        &schema,
        [n(
            &schema,
            "blockquote",
            [
                n(&schema, "paragraph", [t(&schema, "ab")]),
                n(&schema, "paragraph", [t(&schema, "cd")]),
            ],
        )],
    );
    let from = nested.resolve(3).expect("in range");
    let to = nested.resolve(8).expect("in range");
    let quote = schema.node_id("blockquote").expect("known");
    let range = from
        .block_range(&schema, &to, Some(&|node: &Node| node.type_id() == quote))
        .expect("a range");
    assert_eq!(range.depth(), 1);
    assert_eq!((range.start(), range.end()), (1, 9));
}

/// A document whose subtrees can be edited one at a time: a paragraph, a
/// blockquote around a paragraph, and a bullet list whose item holds a
/// paragraph.
fn layered(schema: &crate::schema::Schema) -> Node {
    doc(
        schema,
        [
            n(schema, "paragraph", [t(schema, "top")]),
            n(
                schema,
                "blockquote",
                [n(schema, "paragraph", [t(schema, "quoted")])],
            ),
            n(
                schema,
                "bullet_list",
                [n(
                    schema,
                    "list_item",
                    [n(schema, "paragraph", [t(schema, "item")])],
                )],
            ),
        ],
    )
}

/// `node` with its child at `index` replaced, sharing every other child.
fn with_child(node: &Node, index: usize, child: Node) -> Node {
    node.copy(node.content().replace_child(index, child))
}

#[test]
fn check_from_only_visits_what_changed_but_agrees_with_check() {
    let schema = test_schema();
    let old = layered(&schema);
    old.check(&schema).expect("valid");
    assert_eq!(old.check_from(&old, &schema), Ok(()));

    // A valid edit deep inside the blockquote.
    let quote = old.child(1);
    let edited = with_child(
        &old,
        1,
        with_child(quote, 0, n(&schema, "paragraph", [t(&schema, "quoted!")])),
    );
    assert_eq!(edited.check_from(&old, &schema), Ok(()));
    assert_eq!(edited.check(&schema), Ok(()));

    // An invalid child somewhere the edit reached is still found: the list
    // item loses the paragraph its `paragraph block*` rule requires.
    let list = old.child(2);
    let emptied = with_child(
        &old,
        2,
        with_child(list, 0, list.child(0).copy(Fragment::empty())),
    );
    assert!(emptied.check(&schema).is_err());
    assert!(emptied.check_from(&old, &schema).is_err());

    // The node's own rules cover its whole child list, shared children
    // included: moving a strong text node, shared as it is, into a code block
    // breaks the code block's mark rule.
    let paragraph = doc(
        &schema,
        [n(&schema, "paragraph", [tm(&schema, "a", &["strong"])])],
    );
    let code = paragraph.copy(Fragment::from_node(Node::container(
        crate::node::Markup::new(schema.node_id("code_block").unwrap()),
        paragraph.child(0).content().clone(),
    )));
    assert!(code.child(0).child(0).ptr_eq(paragraph.child(0).child(0)));
    assert!(code.check(&schema).is_err());
    assert!(code.check_from(&paragraph, &schema).is_err());

    // Children beyond the old list are checked in full: an appended
    // blockquote whose paragraph holds a block.
    let broken = Node::container(
        crate::node::Markup::new(schema.node_id("blockquote").unwrap()),
        Fragment::from_node(Node::container(
            crate::node::Markup::new(schema.node_id("paragraph").unwrap()),
            Fragment::from_node(n(&schema, "horizontal_rule", [])),
        )),
    );
    let grown = old.copy(old.content().append(&Fragment::from_node(broken)));
    assert!(grown.check(&schema).is_err());
    assert!(grown.check_from(&old, &schema).is_err());
}

#[test]
fn diff_region_is_empty_for_identical_trees() {
    let schema = test_schema();
    let d = layered(&schema);
    let region = crate::node::diff_region(&d, &d, &schema);
    assert!(region.path.is_empty());
    assert!(region.old.is_empty() && region.new.is_empty());
}

#[test]
fn diff_region_descends_to_the_changed_textblock() {
    let schema = test_schema();
    let old = layered(&schema);
    let list = old.child(2);
    let item = list.child(0);
    let paragraph = n(&schema, "paragraph", [t(&schema, "items")]);
    let new_item = with_child(item, 0, paragraph);
    let new_list = with_child(list, 0, new_item.clone());
    let new = with_child(&old, 2, new_list.clone());

    let region = crate::node::diff_region(&old, &new, &schema);
    let indexes: Vec<usize> = region.path.iter().map(|step| step.index).collect();
    assert_eq!(indexes, [2, 0]);
    let path = &region.path;
    assert!(path[0].old.ptr_eq(list) && path[0].new.ptr_eq(&new_list));
    assert!(path[1].old.ptr_eq(item) && path[1].new.ptr_eq(&new_item));
    // The walk stops at the textblock: its inline content is one region.
    assert_eq!((region.old, region.new), (0..1, 0..1));
}

#[test]
fn diff_region_stops_where_the_difference_is_not_one_child() {
    let schema = test_schema();
    let old = layered(&schema);
    let count = old.child_count();

    // A block appended at the top: the region is empty in the old tree.
    let grown = old.copy(old.content().append(&Fragment::from_node(n(
        &schema,
        "horizontal_rule",
        [],
    ))));
    let region = crate::node::diff_region(&old, &grown, &schema);
    assert!(region.path.is_empty());
    assert_eq!((region.old, region.new), (count..count, count..count + 1));

    // Two top-level blocks edited at once: the region covers both.
    let both = with_child(
        &with_child(&old, 0, n(&schema, "paragraph", [t(&schema, "x")])),
        1,
        n(
            &schema,
            "blockquote",
            [n(&schema, "paragraph", [t(&schema, "y")])],
        ),
    );
    let region = crate::node::diff_region(&old, &both, &schema);
    assert!(region.path.is_empty());
    assert_eq!((region.old, region.new), (0..2, 0..2));

    // One changed child at the top, but two inside it: the walk enters the
    // blockquote and the region is its whole content.
    let quote = old.child(1);
    let doubled = quote.copy(Fragment::from_nodes([
        n(&schema, "paragraph", [t(&schema, "one")]),
        n(&schema, "paragraph", [t(&schema, "two")]),
    ]));
    let new = with_child(&old, 1, doubled.clone());
    let region = crate::node::diff_region(&old, &new, &schema);
    assert_eq!(region.path.len(), 1);
    assert_eq!(region.path[0].index, 1);
    assert!(region.path[0].old.ptr_eq(quote) && region.path[0].new.ptr_eq(&doubled));
    assert_eq!((region.old, region.new), (0..quote.child_count(), 0..2));
}

#[test]
fn diff_region_does_not_enter_a_container_whose_markup_changed() {
    let schema = test_schema();
    let old = layered(&schema);
    let list = old.child(2);
    // Same items, different list type: everything inside is affected.
    let ordered = Node::container(
        crate::node::Markup::new(schema.node_id("ordered_list").unwrap()),
        list.content().clone(),
    );
    let new = with_child(&old, 2, ordered.clone());
    let region = crate::node::diff_region(&old, &new, &schema);
    assert!(region.path.is_empty());
    assert_eq!((region.old, region.new), (2..3, 2..3));
}

#[test]
fn diff_region_stops_at_a_leaf_block() {
    let schema = test_schema();
    let old = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "a")]),
            n(&schema, "horizontal_rule", []),
        ],
    );
    let rule = n(&schema, "horizontal_rule", []);
    let new = with_child(&old, 1, rule.clone());
    let region = crate::node::diff_region(&old, &new, &schema);
    assert!(region.path.is_empty());
    assert_eq!((region.old, region.new), (1..2, 1..2));
}
