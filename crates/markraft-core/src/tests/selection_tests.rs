//! Selection kinds, mapping, normalisation and JSON.

use std::any::Any;

use serde_json::{Value, json};

use super::support::*;
use crate::change::{Change, ChangeDesc, ChangeSet};
use crate::fragment::Fragment;
use crate::node::Node;
use crate::schema::Schema;
use crate::selection::{Selection, SelectionKind, SelectionRange};
use crate::slice::Slice;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockSelection {
    from: usize,
    to: usize,
}

impl SelectionKind for BlockSelection {
    fn tag(&self) -> &str {
        "block"
    }

    fn clone_box(&self) -> Box<dyn SelectionKind> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_kind(&self, other: &dyn SelectionKind) -> bool {
        other.as_any().downcast_ref::<BlockSelection>() == Some(self)
    }

    fn anchor(&self, _doc: &Node) -> usize {
        self.from
    }

    fn head(&self, _doc: &Node) -> usize {
        self.to
    }

    fn map(&self, _doc: &Node, changes: &ChangeDesc) -> Selection {
        let mapped = changes.map_range(self.from, self.to);
        Selection::custom(Box::new(BlockSelection {
            from: mapped.from,
            to: mapped.to,
        }))
    }

    fn to_json(&self, _schema: &Schema) -> Value {
        json!({ "from": self.from, "to": self.to })
    }
}

fn mixed_doc(schema: &Schema) -> Node {
    doc(
        schema,
        [n(
            schema,
            "paragraph",
            [t(schema, "a"), img(schema, "x.png"), t(schema, "b")],
        )],
    )
}

fn changes(schema: &Schema, document: &Node, list: Vec<Change>) -> ChangeSet {
    ChangeSet::create(schema, document, list).expect("a valid change set")
}

#[test]
fn accessors_order_the_two_ends() {
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    let selection = Selection::text(4, 1);
    assert_eq!(selection.anchor(&document), 4);
    assert_eq!(selection.head(&document), 1);
    assert_eq!(selection.from(&document), 1);
    assert_eq!(selection.to(&document), 4);
    assert!(!selection.is_empty(&document));
    assert_eq!(
        selection.ranges(&document),
        vec![SelectionRange { from: 1, to: 4 }]
    );
    assert!(Selection::cursor(2).is_cursor());
}

#[test]
fn a_node_selection_covers_its_node_and_an_all_selection_the_document() {
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    let node = Selection::node(2);
    assert_eq!(node.from(&document), 2);
    assert_eq!(node.to(&document), 3);
    assert_eq!(
        schema.describe(node.content(&document).content().child(0)),
        "image[src=Str(\"x.png\")]"
    );

    let all = Selection::All;
    assert_eq!(all.from(&document), 0);
    assert_eq!(all.to(&document), document.content_size());
}

#[test]
fn is_selectable_only_accepts_types_that_say_so() {
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    assert!(Selection::is_selectable(&schema, &document, 2));
    assert!(!Selection::is_selectable(&schema, &document, 0));
    Selection::node(2).check(&document, &schema).unwrap();
    assert!(Selection::node(1).check(&document, &schema).is_ok());
    assert!(
        Selection::node(document.content_size())
            .check(&document, &schema)
            .is_err()
    );
}

#[test]
fn near_at_start_and_at_end_land_in_inline_content() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(
                &schema,
                "blockquote",
                [n(&schema, "paragraph", [t(&schema, "hi")])],
            ),
            n(&schema, "paragraph", [t(&schema, "bye")]),
        ],
    );
    assert_eq!(
        Selection::at_start(&schema, &document),
        Selection::cursor(2)
    );
    assert_eq!(
        Selection::at_end(&schema, &document),
        Selection::cursor(document.content_size() - 1)
    );
    // Position 0 sits between blocks; the search moves inwards.
    assert_eq!(
        Selection::near(&schema, &document, 0, 1),
        Selection::cursor(2)
    );
    assert_eq!(
        Selection::find_from(&schema, &document, 0, -1, true),
        None,
        "there is no inline position at or before the very start"
    );
}

#[test]
fn a_text_selection_stays_in_inline_content_when_mapped() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "one")]),
            n(&schema, "paragraph", [t(&schema, "two")]),
        ],
    );
    // A cursor in the second paragraph; deleting that paragraph would strand it.
    let selection = Selection::cursor(7);
    let set = changes(&schema, &document, vec![Change::delete(5, 10)]);
    let after = set.apply(&document).unwrap();
    let mapped = selection.map(&schema, &after, &set.desc());
    mapped.check(&after, &schema).unwrap();
    let head = mapped.head(&after);
    assert!(
        after
            .resolve(head)
            .map(|r| schema.node_type(r.parent().type_id()).has_inline_content())
            .unwrap_or(false),
        "the mapped selection is inside inline content"
    );
}

#[test]
fn a_node_selection_follows_its_node_and_drops_when_it_is_deleted() {
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    let selection = Selection::node(2);

    let inserted = changes(
        &schema,
        &document,
        vec![Change::insert(
            1,
            Slice::from_fragment(Fragment::from_node(t(&schema, "Z"))),
        )],
    );
    let after = inserted.apply(&document).unwrap();
    assert_eq!(
        selection.map(&schema, &after, &inserted.desc()),
        Selection::node(3)
    );

    let deleted = changes(&schema, &document, vec![Change::delete(2, 3)]);
    let after = deleted.apply(&document).unwrap();
    let mapped = selection.map(&schema, &after, &deleted.desc());
    assert!(
        matches!(mapped, Selection::Text { .. }),
        "a deleted node falls back to a text selection, got {mapped:?}"
    );
    mapped.check(&after, &schema).unwrap();
}

#[test]
fn a_custom_kind_maps_compares_and_serialises() {
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    let selection = Selection::custom(Box::new(BlockSelection { from: 1, to: 3 }));
    assert_eq!(
        selection,
        Selection::custom(Box::new(BlockSelection { from: 1, to: 3 }))
    );
    assert_ne!(selection, Selection::text(1, 3));
    assert_eq!(selection.from(&document), 1);

    let set = changes(
        &schema,
        &document,
        vec![Change::insert(
            1,
            Slice::from_fragment(Fragment::from_node(t(&schema, "ZZ"))),
        )],
    );
    let after = set.apply(&document).unwrap();
    let mapped = selection.map(&schema, &after, &set.desc());
    assert_eq!(
        mapped,
        Selection::custom(Box::new(BlockSelection { from: 3, to: 5 }))
    );
    assert_eq!(
        mapped.to_json(&schema),
        json!({"type": "block", "from": 3, "to": 5})
    );
    assert!(Selection::from_json(&schema, &mapped.to_json(&schema)).is_err());
}

#[test]
fn built_in_selections_round_trip_through_json() {
    let schema = shared_schema();
    for selection in [
        Selection::text(1, 3),
        Selection::cursor(2),
        Selection::node(2),
        Selection::All,
    ] {
        let json = selection.to_json(&schema);
        assert_eq!(Selection::from_json(&schema, &json).unwrap(), selection);
    }
}

#[test]
fn replace_turns_a_selection_into_a_change() {
    use crate::state::TransactionSpec;
    let schema = shared_schema();
    let document = mixed_doc(&schema);
    let state = state(document.clone(), crate::state::Extension::none());
    let spec = Selection::node(2).replace(
        TransactionSpec::new(),
        &document,
        Slice::from_fragment(Fragment::from_node(t(&schema, "Q"))),
    );
    let tr = state.update([spec]).unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("aQb"))"#);
}
