//! Concrete editing scenarios, expressed as token-level changes.

use super::support::*;
use crate::change::{Change, ChangeSet};
use crate::fit::Fit;
use crate::fragment::Fragment;
use crate::mark::MarkSet;
use crate::node::{Markup, Node};
use crate::schema::Schema;
use crate::slice::{Slice, Token};

fn apply(schema: &Schema, d: &Node, changes: Vec<Change>) -> Node {
    let set = ChangeSet::create(schema, d, changes).expect("a valid change set");
    let out = set.apply(d).expect("applies");
    out.check(schema).expect("the result is a valid document");
    // Every change set round-trips through its inverse.
    let inverse = set.invert(d).expect("invertible");
    assert_eq!(inverse.apply(&out).expect("applies"), *d, "invert failed");
    out
}

/// A slice that opens a container: the token run `Open(ty)`.
fn open_of(schema: &Schema, name: &str) -> Slice {
    let ty = schema.node_id(name).expect("known type");
    let markup = Markup::with_attrs(ty, schema.node_type(ty).default_attrs().clone());
    Slice::from_tokens(&[Token::Open(markup)])
}

/// A slice that closes the innermost container: the token run `Close(ty)`.
fn close_of(schema: &Schema, name: &str) -> Slice {
    let ty = schema.node_id(name).expect("known type");
    let markup = Markup::with_attrs(ty, schema.node_type(ty).default_attrs().clone());
    Slice::from_tokens(&[Token::Close(markup)])
}

/// A slice that closes one container and opens another of the same type: the
/// token run a split inserts.
fn split_of(schema: &Schema, name: &str) -> Slice {
    let ty = schema.node_id(name).expect("known type");
    let markup = Markup::with_attrs(ty, schema.node_type(ty).default_attrs().clone());
    Slice::from_tokens(&[Token::Close(markup.clone()), Token::Open(markup)])
}

#[test]
fn split_a_paragraph_inside_a_nested_list_item() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "bullet_list",
            [n(
                &schema,
                "list_item",
                [n(&schema, "paragraph", [t(&schema, "abcd")])],
            )],
        )],
    );
    // bullet_list(1) list_item(1) paragraph(1) a b c d -> "ab" ends at 5.
    let split = split_of(&schema, "paragraph");
    assert_eq!(split.size(), 2, "a split inserts a close and an open token");
    let out = apply(&schema, &d, vec![Change::insert(5, split)]);
    assert_eq!(
        schema.describe(&out),
        r#"doc(bullet_list(list_item(paragraph("ab"), paragraph("cd"))))"#
    );
}

#[test]
fn split_a_list_item_into_two() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "bullet_list",
            [n(
                &schema,
                "list_item",
                [n(&schema, "paragraph", [t(&schema, "abcd")])],
            )],
        )],
    );
    // Close the paragraph and the item, then open a new item and paragraph.
    let paragraph = schema.node_id("paragraph").expect("known");
    let list_item = schema.node_id("list_item").expect("known");
    let slice = Slice::from_tokens(&[
        Token::Close(Markup::new(paragraph)),
        Token::Close(Markup::new(list_item)),
        Token::Open(Markup::new(list_item)),
        Token::Open(Markup::new(paragraph)),
    ]);
    let out = apply(&schema, &d, vec![Change::insert(5, slice)]);
    assert_eq!(
        schema.describe(&out),
        r#"doc(bullet_list(list_item(paragraph("ab")), list_item(paragraph("cd"))))"#
    );
}

#[test]
fn join_two_paragraphs() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ab")]),
            n(&schema, "paragraph", [t(&schema, "cd")]),
        ],
    );
    // The close token of the first paragraph and the open token of the second.
    let out = apply(&schema, &d, vec![Change::delete(3, 5)]);
    assert_eq!(schema.describe(&out), r#"doc(paragraph("abcd"))"#);
}

#[test]
fn wrap_paragraphs_in_a_blockquote() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ab")]),
            n(&schema, "paragraph", [t(&schema, "cd")]),
        ],
    );
    let end = d.content_size();
    let out = apply(
        &schema,
        &d,
        vec![
            Change::insert(0, open_of(&schema, "blockquote")),
            Change::insert(end, close_of(&schema, "blockquote")),
        ],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(blockquote(paragraph("ab"), paragraph("cd")))"#
    );
}

#[test]
fn lift_content_out_of_a_blockquote() {
    let schema = test_schema();
    let d = doc(
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
    let end = d.content_size();
    // Deleting the wrapper's two tokens is a lift. Neither deletion balances on
    // its own; together they do.
    let out = apply(
        &schema,
        &d,
        vec![Change::delete(0, 1), Change::delete(end - 1, end)],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("ab"), paragraph("cd"))"#
    );
}

#[test]
fn lift_a_list_item_out_of_its_list() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "bullet_list",
            [n(
                &schema,
                "list_item",
                [n(&schema, "paragraph", [t(&schema, "ab")])],
            )],
        )],
    );
    let end = d.content_size();
    let out = apply(
        &schema,
        &d,
        vec![Change::delete(0, 2), Change::delete(end - 2, end)],
    );
    assert_eq!(schema.describe(&out), r#"doc(paragraph("ab"))"#);
}

#[test]
fn delete_across_a_list_item_and_a_blockquote() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(
                &schema,
                "bullet_list",
                [n(
                    &schema,
                    "list_item",
                    [n(&schema, "paragraph", [t(&schema, "abc")])],
                )],
            ),
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "def")])],
            ),
        ],
    );
    // 0 list 1 item 2 para 3 a 4 b 5 c 6 /para 7 /item 8 /list
    // 9 quote 10 para 11 d 12 e 13 f 14 /para 15 /quote 16
    assert_eq!(d.content_size(), 16);
    let set = ChangeSet::create(&schema, &d, vec![Change::delete(5, 12).with_fit(Fit::Auto)])
        .expect("fits");
    let out = set.apply(&d).expect("applies");
    out.check(&schema).expect("valid");
    assert_eq!(
        schema.describe(&out),
        r#"doc(bullet_list(list_item(paragraph("abef"))))"#
    );
    let inverse = set.invert(&d).expect("invertible");
    assert_eq!(inverse.apply(&out).expect("applies"), d);
}

#[test]
fn an_unfitted_cross_container_delete_is_unbalanced() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(
                &schema,
                "bullet_list",
                [n(
                    &schema,
                    "list_item",
                    [n(&schema, "paragraph", [t(&schema, "abc")])],
                )],
            ),
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "def")])],
            ),
        ],
    );
    let set = ChangeSet::create(&schema, &d, vec![Change::delete(5, 12)]).expect("created");
    assert!(
        matches!(set.apply(&d), Err(crate::error::ChangeError::Unbalanced(_))),
        "without fitting the caller vouches for the change"
    );
}

#[test]
fn paste_a_slice_with_open_sides_into_a_paragraph() {
    let schema = test_schema();
    let source = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "hello")]),
            n(&schema, "paragraph", [t(&schema, "world")]),
        ],
    );
    // A cut through both paragraphs: open on both sides.
    let slice = source.slice(3, 11).expect("in range");
    assert_eq!((slice.open_start(), slice.open_end()), (1, 1));

    let target = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]);
    let out = apply(&schema, &target, vec![Change::insert(3, slice)]);
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("abllo"), paragraph("worcd"))"#,
        "the open sides merge with the text on either side of the insertion"
    );

    // A slice that is closed on both sides inserts whole blocks.
    let blocks = source.slice(0, 7).expect("in range");
    assert_eq!((blocks.open_start(), blocks.open_end()), (0, 0));
    let out = apply(
        &schema,
        &target,
        vec![Change::insert(0, blocks).with_fit(Fit::Auto)],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("hello"), paragraph("abcd"))"#
    );
}

#[test]
fn fitting_wraps_stray_inline_content() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]);
    // Inserting bare inline content at the top level is invalid on its own.
    let stray = Slice::from_fragment(Fragment::from_nodes([t(&schema, "loose")]));
    let out = apply(
        &schema,
        &d,
        vec![Change::insert(4, stray).with_fit(Fit::Auto)],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("ab"), paragraph("loose"))"#
    );
}

#[test]
fn fitting_wraps_a_block_that_does_not_fit() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "bullet_list",
            [n(
                &schema,
                "list_item",
                [n(&schema, "paragraph", [t(&schema, "ab")])],
            )],
        )],
    );
    // A bare list item at the top level needs a list around it.
    let item = n(
        &schema,
        "list_item",
        [n(&schema, "paragraph", [t(&schema, "new")])],
    );
    let slice = Slice::from_fragment(Fragment::from_node(item));
    let out = apply(
        &schema,
        &d,
        vec![Change::insert(d.content_size(), slice).with_fit(Fit::Auto)],
    );
    assert!(
        schema
            .describe(&out)
            .ends_with(r#"bullet_list(list_item(paragraph("new"))))"#),
        "got {}",
        schema.describe(&out)
    );
}

#[test]
fn fit_context_steers_the_wrapper_choice() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]);
    let heading = schema.node_id("heading").expect("known");
    let stray = Slice::from_fragment(Fragment::from_nodes([t(&schema, "loose")]));
    let out = apply(
        &schema,
        &d,
        vec![Change::insert(4, stray).with_fit(Fit::Context(vec![heading]))],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("ab"), heading[level=Int(1)]("loose"))"#
    );
}

#[test]
fn fitting_completes_a_container_left_open() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]);
    // Only the paragraph's open token is deleted; fitting restores it from the
    // context after the change.
    let out = apply(&schema, &d, vec![Change::delete(0, 1).with_fit(Fit::Auto)]);
    assert_eq!(schema.describe(&out), r#"doc(paragraph("abcd"))"#);
}

#[test]
fn fitting_drops_a_container_it_cannot_complete() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]);
    // An empty list is not valid content; fitting drops it rather than keeping
    // an empty node around.
    let empty_list = Slice::from_tokens(&[
        Token::Open(Markup::new(schema.node_id("bullet_list").expect("known"))),
        Token::Close(Markup::new(schema.node_id("bullet_list").expect("known"))),
    ]);
    let out = apply(
        &schema,
        &d,
        vec![Change::insert(4, empty_list).with_fit(Fit::Auto)],
    );
    assert_eq!(schema.describe(&out), r#"doc(paragraph("ab"))"#);
}

#[test]
fn add_and_remove_a_mark_across_leaf_boundaries() {
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
    let out = apply(&schema, &d, vec![Change::add_mark(2, 6, strong.clone())]);
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("a", "b"{strong}, "cd"{strong,em}, "e"{strong}, "f"))"#
    );
    let back = apply(&schema, &out, vec![Change::remove_mark(2, 6, strong)]);
    assert_eq!(back, d);
}

#[test]
fn a_mark_change_stops_at_a_parent_that_forbids_the_mark() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "ab")]),
            n(&schema, "code_block", [t(&schema, "cd")]),
        ],
    );
    let strong = m(&schema, "strong");
    let set = ChangeSet::create(
        &schema,
        &d,
        vec![Change::add_mark(0, d.content_size(), strong.clone())],
    )
    .expect("valid");
    let out = apply(
        &schema,
        &d,
        vec![Change::add_mark(0, d.content_size(), strong)],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("ab"{strong}), code_block("cd"))"#,
        "the code block declares `marks: \"\"`, so its text keeps no marks"
    );
    // The forbidden part is not recorded at all, so the change set only claims
    // the range it really modifies.
    let marked: Vec<(usize, usize)> = set
        .iter_changes()
        .into_iter()
        .filter_map(|change| match change {
            crate::change::ChangeRange::Marked { from_a, to_a, .. } => Some((from_a, to_a)),
            _ => None,
        })
        .collect();
    assert_eq!(
        marked,
        vec![(1, 3)],
        "only the paragraph's inline content is recorded"
    );
}

#[test]
fn mark_exclusion_replaces_the_previous_link() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [schema.text_marked(
                "abcd",
                MarkSet::from_marks(&schema, [link(&schema, "https://old")]),
            )],
        )],
    );
    let out = apply(
        &schema,
        &d,
        vec![Change::add_mark(2, 4, link(&schema, "https://new"))],
    );
    let paragraph = out.child(0);
    assert_eq!(paragraph.child_count(), 3);
    assert_eq!(
        paragraph
            .child(1)
            .marks()
            .get(schema.mark_id("link").expect("known"))
            .and_then(|m| m.attrs.get("href"))
            .and_then(|v| v.as_str()),
        Some("https://new")
    );
    assert_eq!(
        paragraph
            .child(0)
            .marks()
            .get(schema.mark_id("link").expect("known"))
            .and_then(|m| m.attrs.get("href"))
            .and_then(|v| v.as_str()),
        Some("https://old")
    );
}

#[test]
fn removing_a_mark_type_clears_every_variant() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [
                schema.text_marked("ab", MarkSet::from_marks(&schema, [link(&schema, "a")])),
                schema.text_marked("cd", MarkSet::from_marks(&schema, [link(&schema, "b")])),
            ],
        )],
    );
    let link_ty = schema.mark_id("link").expect("known");
    let out = apply(
        &schema,
        &d,
        vec![Change::remove_mark_type(0, d.content_size(), link_ty)],
    );
    assert_eq!(schema.describe(&out), r#"doc(paragraph("abcd"))"#);
}

#[test]
fn several_changes_use_starting_document_coordinates() {
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "word")])]);
    let open = Slice::from_fragment(Fragment::from_node(t(&schema, "(")));
    let close = Slice::from_fragment(Fragment::from_node(t(&schema, ")")));
    let out = apply(
        &schema,
        &d,
        vec![Change::insert(1, open), Change::insert(5, close)],
    );
    assert_eq!(schema.describe(&out), r#"doc(paragraph("(word)"))"#);
}

#[test]
fn structural_sharing_survives_a_change() {
    let schema = test_schema();
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "first")]),
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "second")])],
            ),
        ],
    );
    let set = ChangeSet::create(
        &schema,
        &d,
        vec![Change::insert(
            2,
            Slice::from_fragment(Fragment::from_node(t(&schema, "X"))),
        )],
    )
    .expect("valid");
    let out = set.apply(&d).expect("applies");
    assert!(
        d.child(1).ptr_eq(out.child(1)),
        "the untouched blockquote is shared, not rebuilt"
    );
}

#[test]
fn fitting_an_unplaceable_open_token_is_a_no_op() {
    // Regression: a container the repair could not place still counted towards
    // the open depth, so the fitter emitted a close token with nothing to match
    // and fell back to rewriting the whole enclosing node.
    let schema = test_schema();
    let d = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]);
    let paragraph = schema.node_id("paragraph").expect("known");
    let stray_open = Slice::from_tokens(&[Token::Open(Markup::new(paragraph))]);
    let set = ChangeSet::create(
        &schema,
        &d,
        vec![Change::insert(3, stray_open).with_fit(Fit::Auto)],
    )
    .expect("fits");
    assert_eq!(
        schema.describe(&set.apply(&d).expect("applies")),
        r#"doc(paragraph("abcd"))"#
    );
    assert!(
        set.is_empty(),
        "a repair with no effect should not be recorded as a change: {:?}",
        set.iter_changes()
    );
    assert!(!set.touches(3, 3));
    let mapped = set.map_range(3, 3);
    assert!(!mapped.deleted);
    assert_eq!((mapped.from, mapped.to), (3, 3));
}

#[test]
fn check_rejects_marks_the_parent_forbids() {
    let schema = test_schema();
    let ok = doc(
        &schema,
        [n(&schema, "paragraph", [tm(&schema, "bold", &["strong"])])],
    );
    ok.check(&schema)
        .expect("a paragraph allows marks on its text");

    let bad = doc(
        &schema,
        [n(&schema, "code_block", [tm(&schema, "bold", &["strong"])])],
    );
    match bad.check(&schema) {
        Err(crate::error::NodeError::MarkNotAllowed { mark, node }) => {
            assert_eq!(mark, "strong");
            assert_eq!(node, "code_block");
        }
        other => panic!("expected the code block to reject the mark, got {other:?}"),
    }
}

#[test]
fn fitting_strips_marks_the_destination_forbids() {
    let schema = test_schema();
    let source = doc(
        &schema,
        [n(
            &schema,
            "paragraph",
            [tm(&schema, "bold", &["strong", "em"])],
        )],
    );
    let copied = source.slice(1, 5).expect("in range");
    assert_eq!(copied.text_content(None), "bold");

    // Into a paragraph the marks survive.
    let target = doc(&schema, [n(&schema, "paragraph", [t(&schema, "ab")])]);
    let out = apply(
        &schema,
        &target,
        vec![Change::insert(2, copied.clone()).with_fit(Fit::Auto)],
    );
    assert_eq!(
        schema.describe(&out),
        r#"doc(paragraph("a", "bold"{strong,em}, "b"))"#
    );

    // Into a code block they do not, but the text does.
    let target = doc(&schema, [n(&schema, "code_block", [t(&schema, "ab")])]);
    let out = apply(
        &schema,
        &target,
        vec![Change::insert(2, copied).with_fit(Fit::Auto)],
    );
    assert_eq!(schema.describe(&out), r#"doc(code_block("aboldb"))"#);
}

#[test]
fn marks_at_a_position_follow_the_parent() {
    let schema = test_schema();
    // The same text, with the same marks, in two different parents.
    let d = doc(
        &schema,
        [
            n(&schema, "paragraph", [tm(&schema, "ab", &["strong"])]),
            n(&schema, "code_block", [t(&schema, "cd")]),
        ],
    );
    let strong = schema.mark_id("strong").expect("known");
    assert!(
        d.resolve(3)
            .expect("in range")
            .marks(&schema)
            .contains_type(strong),
        "typing after bold text in a paragraph continues the mark"
    );
    assert!(
        !d.resolve(7)
            .expect("in range")
            .marks(&schema)
            .contains_type(strong),
        "a code block never offers marks for content typed into it"
    );
}
