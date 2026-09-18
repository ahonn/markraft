//! Schema compilation and content-expression behaviour.

use super::support::*;
use crate::attr::{AttrKind, AttrSpec, AttrValue, Attrs};
use crate::error::SchemaError;
use crate::schema::{MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec};

fn types(schema: &Schema, names: &[&str]) -> Vec<crate::schema::NodeTypeId> {
    names
        .iter()
        .map(|n| schema.node_id(n).expect("known type"))
        .collect()
}

fn accepts(schema: &Schema, parent: &str, children: &[&str]) -> bool {
    let ty = schema.node_id(parent).expect("known type");
    match schema
        .content_match(ty)
        .match_types(types(schema, children))
    {
        Some(m) => m.valid_end(),
        None => false,
    }
}

#[test]
fn compiles_the_test_schema() {
    let schema = test_schema();
    assert_eq!(schema.node_types().len(), 11);
    assert_eq!(schema.mark_types().len(), 3);
    let paragraph = schema.node_id("paragraph").expect("known");
    assert!(schema.node_type(paragraph).is_textblock());
    assert!(!schema.node_type(paragraph).is_leaf());
    let text = schema.text_type().expect("text type");
    assert!(schema.node_type(text).is_leaf());
    assert!(schema.node_type(text).is_inline());
    let blockquote = schema.node_id("blockquote").expect("known");
    assert!(!schema.node_type(blockquote).is_textblock());
    assert!(schema.node_type(blockquote).is_defining());
}

#[test]
fn rejects_duplicate_names() {
    let err = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "para+"))
            .node(NodeTypeSpec::new("para", "").group("para"))
            .node(NodeTypeSpec::new("para", "").group("para")),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SchemaError::DuplicateName { kind: "node", .. }
    ));
}

#[test]
fn rejects_unknown_content_names() {
    let err =
        Schema::new(SchemaSpec::new().node(NodeTypeSpec::new("doc", "nonsense+"))).unwrap_err();
    assert!(matches!(err, SchemaError::ContentExpr { .. }), "{err:?}");
}

#[test]
fn rejects_mixed_inline_and_block_content() {
    let err = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "para text"))
            .node(NodeTypeSpec::new("para", ""))
            .node(NodeTypeSpec::text("text")),
    )
    .unwrap_err();
    assert!(matches!(err, SchemaError::InvalidSpec { .. }), "{err:?}");
}

#[test]
fn content_expressions_accept_and_reject() {
    let schema = test_schema();
    assert!(accepts(&schema, "doc", &["paragraph"]));
    assert!(accepts(
        &schema,
        "doc",
        &["paragraph", "heading", "blockquote"]
    ));
    assert!(!accepts(&schema, "doc", &[]), "block+ needs one child");
    assert!(!accepts(&schema, "doc", &["text"]));
    assert!(accepts(&schema, "paragraph", &[]));
    assert!(accepts(
        &schema,
        "paragraph",
        &["text", "image", "hard_break"]
    ));
    assert!(!accepts(&schema, "paragraph", &["paragraph"]));
    assert!(accepts(&schema, "list_item", &["paragraph"]));
    assert!(accepts(
        &schema,
        "list_item",
        &["paragraph", "bullet_list", "paragraph"]
    ));
    assert!(
        !accepts(&schema, "list_item", &["bullet_list"]),
        "a list item has to start with a paragraph"
    );
    assert!(accepts(&schema, "code_block", &["text"]));
    assert!(!accepts(&schema, "code_block", &["image"]));
    assert!(accepts(&schema, "bullet_list", &["list_item", "list_item"]));
    assert!(!accepts(&schema, "bullet_list", &["paragraph"]));
}

#[test]
fn counted_repetition_and_grouping() {
    let schema = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "(a | b){2,3}"))
            .node(NodeTypeSpec::new("a", "").group("x"))
            .node(NodeTypeSpec::new("b", "").group("x"))
            .node(NodeTypeSpec::new("c", "a{2}")),
    )
    .expect("valid");
    assert!(!accepts(&schema, "doc", &["a"]));
    assert!(accepts(&schema, "doc", &["a", "b"]));
    assert!(accepts(&schema, "doc", &["a", "b", "a"]));
    assert!(!accepts(&schema, "doc", &["a", "b", "a", "b"]));
    assert!(accepts(&schema, "c", &["a", "a"]));
    assert!(!accepts(&schema, "c", &["a"]));
}

#[test]
fn sequences_with_optional_parts() {
    let schema = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "head? body* tail"))
            .node(NodeTypeSpec::new("head", ""))
            .node(NodeTypeSpec::new("body", ""))
            .node(NodeTypeSpec::new("tail", "")),
    )
    .expect("valid");
    assert!(accepts(&schema, "doc", &["tail"]));
    assert!(accepts(&schema, "doc", &["head", "tail"]));
    assert!(accepts(&schema, "doc", &["head", "body", "body", "tail"]));
    assert!(!accepts(&schema, "doc", &["body", "head", "tail"]));
    assert!(!accepts(&schema, "doc", &["head"]));
}

#[test]
fn find_wrapping_reaches_through_containers() {
    let schema = test_schema();
    let doc_ty = schema.node_id("doc").expect("known");
    let list_item = schema.node_id("list_item").expect("known");
    let paragraph = schema.node_id("paragraph").expect("known");
    let text = schema.text_type().expect("known");

    // A paragraph fits directly at the top level.
    assert_eq!(
        schema.find_wrapping(schema.content_match(doc_ty), paragraph),
        Some(Vec::new())
    );
    // A list item needs a list around it.
    let wrapping = schema
        .find_wrapping(schema.content_match(doc_ty), list_item)
        .expect("a wrapping exists");
    assert_eq!(wrapping.len(), 1);
    assert!(schema.node_type(wrapping[0]).name().ends_with("_list"));
    // Inline content at the top level goes into the default textblock.
    assert_eq!(
        schema.find_wrapping(schema.content_match(doc_ty), text),
        Some(vec![paragraph])
    );
    // Nothing can wrap a paragraph so that it fits inside another paragraph.
    assert_eq!(
        schema.find_wrapping(schema.content_match(paragraph), paragraph),
        None
    );
}

#[test]
fn fill_before_completes_required_content() {
    let schema = test_schema();
    let list_item = schema.node_id("list_item").expect("known");
    let paragraph = schema.node_id("paragraph").expect("known");
    let bullet_list = schema.node_id("bullet_list").expect("known");

    // A list item that starts with a nested list needs a paragraph in front.
    assert_eq!(
        schema.fill_before(schema.content_match(list_item), &[bullet_list], false),
        Some(vec![paragraph])
    );
    // Closing an empty list item requires a paragraph.
    assert_eq!(
        schema.fill_before(schema.content_match(list_item), &[], true),
        Some(vec![paragraph])
    );
    // A paragraph may end empty.
    assert_eq!(
        schema.fill_before(schema.content_match(paragraph), &[], true),
        Some(Vec::new())
    );
    // A list needs at least one item; filling it invents one.
    assert_eq!(
        schema.fill_before(schema.content_match(bullet_list), &[], true),
        Some(vec![list_item])
    );
}

#[test]
fn default_type_follows_declaration_order() {
    let schema = test_schema();
    let doc_ty = schema.node_id("doc").expect("known");
    assert_eq!(
        schema.default_type(schema.content_match(doc_ty)),
        schema.node_id("paragraph")
    );
}

#[test]
fn create_and_fill_builds_required_children() {
    let schema = test_schema();
    let list_item = schema.node_id("list_item").expect("known");
    let filled = schema
        .create_and_fill(
            list_item,
            Attrs::empty(),
            crate::mark::MarkSet::empty(),
            crate::fragment::Fragment::empty(),
        )
        .expect("a list item can be filled");
    assert_eq!(schema.describe(&filled), "list_item(paragraph())");
}

#[test]
fn required_attributes_block_automatic_creation() {
    let schema = test_schema();
    let image = schema.node_id("image").expect("known");
    assert!(schema.node_type(image).has_required_attrs());
    assert!(!schema.is_creatable(image));
    assert!(schema.build_node_attrs(image, &Attrs::empty()).is_err());
    assert!(
        schema
            .build_node_attrs(image, &crate::attrs! {"src" => "a.png"})
            .is_ok()
    );
}

#[test]
fn attribute_defaults_and_kinds() {
    let schema = test_schema();
    let heading = schema.node_id("heading").expect("known");
    let attrs = schema
        .build_node_attrs(heading, &Attrs::empty())
        .expect("defaults apply");
    assert_eq!(attrs.get("level"), Some(&AttrValue::Int(1)));
    assert!(
        schema
            .build_node_attrs(heading, &crate::attrs! {"level" => "two"})
            .is_err(),
        "a string is not an int"
    );
    assert!(
        schema
            .build_node_attrs(heading, &crate::attrs! {"other" => 1i64})
            .is_err(),
        "unknown attributes are rejected"
    );
}

#[test]
fn mark_exclusion_and_ordering() {
    let schema = test_schema();
    let strong = m(&schema, "strong");
    let em = m(&schema, "em");
    let a = link(&schema, "https://a.example");
    let b = link(&schema, "https://b.example");

    let set = crate::mark::MarkSet::empty()
        .add(&schema, em.clone())
        .add(&schema, strong.clone());
    assert_eq!(
        set.iter().map(|m| m.ty).collect::<Vec<_>>(),
        vec![strong.ty, em.ty],
        "marks sort by rank, not insertion order"
    );

    // A mark type excludes itself by default, so a second link replaces the
    // first.
    let links = crate::mark::MarkSet::empty()
        .add(&schema, a)
        .add(&schema, b.clone());
    assert_eq!(links.len(), 1);
    assert_eq!(links.get(b.ty), Some(&b));

    // Explicit exclusion in both directions.
    let schema2 = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "text*"))
            .node(NodeTypeSpec::text("text"))
            .mark(MarkTypeSpec::new("sub").excludes("sub sup"))
            .mark(MarkTypeSpec::new("sup").excludes("sub sup")),
    )
    .expect("valid");
    let sub = schema2.mark("sub", Attrs::empty()).expect("known");
    let sup = schema2.mark("sup", Attrs::empty()).expect("known");
    let set = crate::mark::MarkSet::empty()
        .add(&schema2, sub.clone())
        .add(&schema2, sup.clone());
    assert_eq!(set.as_slice(), std::slice::from_ref(&sup));
    let set = set.add(&schema2, sub.clone());
    assert_eq!(set.as_slice(), &[sub]);
    let _ = sup;
}

#[test]
fn marks_are_permitted_by_the_parent() {
    let schema = test_schema();
    let code_block = schema.node_id("code_block").expect("known");
    let paragraph = schema.node_id("paragraph").expect("known");
    let doc_ty = schema.node_id("doc").expect("known");
    let text = schema.text_type().expect("known");
    let strong = schema.mark_id("strong").expect("known");

    // `marks: ""` on the code block forbids marks on the text inside it, even
    // though that is the same text type paragraphs use.
    assert!(!schema.node_type(code_block).allows_mark_in_content(strong));
    assert!(schema.node_type(paragraph).allows_mark_in_content(strong));
    // Types with block content allow no marks in it by default.
    assert!(!schema.node_type(doc_ty).allows_mark_in_content(strong));
    // The text type itself is a leaf, so it holds no content to permit marks in.
    assert!(!schema.node_type(text).allows_mark_in_content(strong));
}

#[test]
fn attr_specs_are_validated() {
    let err = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "text*"))
            .node(
                NodeTypeSpec::text("text")
                    .attr(AttrSpec::new("a", AttrKind::Int, AttrValue::Int(0)))
                    .attr(AttrSpec::new("a", AttrKind::Int, AttrValue::Int(1))),
            ),
    )
    .unwrap_err();
    assert!(matches!(err, SchemaError::InvalidSpec { .. }), "{err:?}");
}

#[test]
fn a_star_branch_does_not_leak_into_its_alternatives() {
    // Regression: compiling `*` used to loop back to the state the expression
    // started from. Because every option of a choice compiles from that same
    // state, an iteration of the star re-enabled the other options.
    let schema = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "a*|b"))
            .node(NodeTypeSpec::new("a", "").group("g"))
            .node(NodeTypeSpec::new("b", "").group("g"))
            .node(NodeTypeSpec::new("c", "").group("g")),
    )
    .expect("valid");
    assert!(accepts(&schema, "doc", &[]));
    assert!(accepts(&schema, "doc", &["a"]));
    assert!(accepts(&schema, "doc", &["a", "a"]));
    assert!(accepts(&schema, "doc", &["b"]));
    assert!(!accepts(&schema, "doc", &["a", "b"]));
    assert!(!accepts(&schema, "doc", &["b", "a"]));
    assert!(!accepts(&schema, "doc", &["b", "b"]));

    type Case<'a> = (&'a str, Vec<(&'a [&'a str], bool)>);
    let cases: Vec<Case<'_>> = vec![
        (
            "b|a*",
            vec![
                (&["a", "b"], false),
                (&["b"], true),
                (&["a", "a"], true),
                (&[], true),
            ],
        ),
        (
            "(a* c)|b",
            vec![
                (&["a", "b"], false),
                (&["c"], true),
                (&["a", "a", "c"], true),
                (&["b"], true),
            ],
        ),
        (
            "a{0,}|b",
            vec![(&["a", "b"], false), (&["a"], true), (&["b"], true)],
        ),
        (
            "(a* b)+",
            vec![
                (&["b", "a"], false),
                (&["b"], true),
                (&["a", "b"], true),
                (&["a", "b", "b"], true),
            ],
        ),
        (
            "(a* b)*",
            vec![(&["a"], false), (&[], true), (&["a", "b", "b"], true)],
        ),
    ];
    for (expr, expectations) in cases {
        let schema = Schema::new(
            SchemaSpec::new()
                .node(NodeTypeSpec::new("doc", expr))
                .node(NodeTypeSpec::new("a", "").group("g"))
                .node(NodeTypeSpec::new("b", "").group("g"))
                .node(NodeTypeSpec::new("c", "").group("g")),
        )
        .unwrap_or_else(|err| panic!("`{expr}` should compile: {err}"));
        for (input, expected) in expectations {
            assert_eq!(
                accepts(&schema, "doc", input),
                expected,
                "`{expr}` on {input:?}"
            );
        }
    }
}

#[test]
fn check_rejects_content_a_choice_forbids() {
    let schema = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "paragraph*|heading"))
            .node(NodeTypeSpec::new("paragraph", "").group("block"))
            .node(NodeTypeSpec::new("heading", "").group("block")),
    )
    .expect("valid");
    let paragraph = schema.node("paragraph", []).expect("built");
    let heading = schema.node("heading", []).expect("built");
    schema
        .doc([paragraph.clone(), paragraph.clone()])
        .expect("built")
        .check(&schema)
        .expect("paragraphs only is valid");
    schema
        .doc([heading.clone()])
        .expect("built")
        .check(&schema)
        .expect("a lone heading is valid");
    assert!(
        schema
            .doc([paragraph, heading])
            .expect("built")
            .check(&schema)
            .is_err(),
        "the choice forbids mixing the two branches"
    );
}
