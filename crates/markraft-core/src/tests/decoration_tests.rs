//! Decorations and the range/point sets they are built from.

use std::sync::Arc;

use crate::change::{Change, ChangeSet};
use crate::decorations::*;
use crate::slice::Slice;
use crate::state::{Extension, StateField, StateFieldConfig};

use super::support::*;

fn spec(name: &str) -> DecorationSpec {
    DecorationSpec::new(crate::attrs! {"class" => name})
}

fn set_of(items: &[(usize, usize)]) -> RangeSet<&'static str> {
    RangeSet::from_items(items.iter().map(|(a, b)| RangeItem::new(*a, *b, "x")))
}

/// A change set over `doc` that replaces `from..to` with `text`.
fn edit(doc: &crate::node::Node, from: usize, to: usize, text: &str) -> ChangeSet {
    let schema = shared_schema();
    let slice = if text.is_empty() {
        Slice::empty()
    } else {
        Slice::from_fragment(crate::fragment::Fragment::from_node(schema.text(text)))
    };
    ChangeSet::create(&schema, doc, [Change::replace(from, to, slice)]).expect("valid changes")
}

#[test]
fn range_sets_are_sorted_and_queryable() {
    let set = set_of(&[(5, 8), (1, 3), (2, 9)]);
    assert_eq!(
        set.iter()
            .map(|item| (item.from, item.to))
            .collect::<Vec<_>>(),
        vec![(1, 3), (2, 9), (5, 8)]
    );
    assert_eq!(set.find(4, 6).len(), 2);
    assert_eq!(
        set.between(1, 8)
            .iter()
            .map(|item| (item.from, item.to))
            .collect::<Vec<_>>(),
        vec![(1, 3), (5, 8)]
    );
    assert_eq!(set.remove(|item| item.from == 2).len(), 2);
}

#[test]
fn mapping_honours_the_inclusivity_flags() {
    let schema = shared_schema();
    let document = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcdef")])]);
    // Insert "XY" exactly at position 3, the start of one range and the end of
    // another.
    let changes = edit(&document, 3, 3, "XY").desc().clone();

    let exclusive = RangeSet::from_items([RangeItem::new(3, 5, "a")]);
    let inclusive = RangeSet::from_items([RangeItem::new(3, 5, "a").inclusive(true, true)]);
    assert_eq!(exclusive.map(&changes).as_slice()[0].from, 5);
    assert_eq!(inclusive.map(&changes).as_slice()[0].from, 3);

    let ending = RangeSet::from_items([RangeItem::new(1, 3, "a")]);
    let ending_inclusive = RangeSet::from_items([RangeItem::new(1, 3, "a").inclusive(true, true)]);
    assert_eq!(ending.map(&changes).as_slice()[0].to, 3);
    assert_eq!(ending_inclusive.map(&changes).as_slice()[0].to, 5);
}

#[test]
fn a_range_whose_content_is_deleted_is_dropped() {
    let schema = shared_schema();
    let document = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcdef")])]);
    let changes = edit(&document, 2, 5, "").desc().clone();
    let set = RangeSet::from_items([
        RangeItem::new(2, 5, "gone"),
        RangeItem::new(1, 6, "kept"),
        RangeItem::new(3, 3, "empty"),
    ]);
    let mapped = set.map(&changes);
    let values: Vec<&str> = mapped.iter().map(|item| item.value).collect();
    assert_eq!(values, vec!["kept", "empty"]);
}

#[test]
fn points_map_with_their_own_side() {
    let schema = shared_schema();
    let document = doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcdef")])]);
    let changes = edit(&document, 3, 3, "XY").desc().clone();
    let set = PointSet::from_items([PointItem::new(3, -1, "left"), PointItem::new(3, 1, "right")]);
    let mapped = set.map(&changes);
    assert_eq!(mapped.as_slice()[0].pos, 3);
    assert_eq!(mapped.as_slice()[1].pos, 5);
}

#[test]
fn a_decoration_set_maps_every_kind() {
    let schema = shared_schema();
    let hr = schema.node_id("horizontal_rule").expect("known");
    let document = doc(
        &schema,
        [
            n(&schema, "paragraph", [t(&schema, "abcdef")]),
            n(&schema, "horizontal_rule", []),
        ],
    );
    let set = DecorationSet::from_decorations([
        Decoration::inline(2, 5, spec("hit")),
        Decoration::node(8, 9, spec("rule")),
        Decoration::widget(3, 1, spec("caret")),
        Decoration::tag(hr, spec("divider")),
    ]);
    assert!(!set.is_empty());
    assert_eq!(set.find(2, 3).len(), 3); // inline, widget, tag

    let changes = edit(&document, 1, 1, "XY").desc().clone();
    let mapped = set.map(&changes);
    assert_eq!(mapped.inline().as_slice()[0].from, 4);
    assert_eq!(mapped.nodes().as_slice()[0].from, 10);
    assert_eq!(mapped.widgets().as_slice()[0].pos, 5);
    assert_eq!(mapped.tags().len(), 1);
}

#[test]
fn inline_pieces_split_at_node_boundaries() {
    let schema = shared_schema();
    let document = doc(
        &schema,
        [
            n(
                &schema,
                "paragraph",
                [t(&schema, "ab"), tm(&schema, "cd", &["strong"])],
            ),
            n(&schema, "paragraph", [t(&schema, "ef")]),
        ],
    );
    let decoration = Decoration::inline(2, 8, spec("hit"));
    assert_eq!(
        decoration.inline_pieces(&document, &schema),
        vec![(2, 3), (3, 5), (7, 8)]
    );
}

#[test]
fn the_payload_survives_and_downcasts() {
    let spec = DecorationSpec::new(crate::attrs! {"class" => "hit"})
        .with_payload(Arc::new(42u32) as Arc<dyn std::any::Any + Send + Sync>);
    assert_eq!(spec.payload::<u32>(), Some(&42));
    assert_eq!(spec.payload::<String>(), None);
}

#[test]
fn the_facet_gathers_static_and_computed_sources() {
    let schema = shared_schema();
    let field: StateField<DecorationSet> = StateField::define(StateFieldConfig::new(
        |_| DecorationSet::from_decorations([Decoration::inline(1, 3, spec("field"))]),
        |value, tr| {
            if tr.doc_changed() {
                value.map(tr.changes().desc())
            } else {
                value.clone()
            }
        },
    ));
    let from_field = field.clone();
    let extensions = Extension::all([
        field.extension(),
        decorations().of(DecorationSource::computed(move |state| {
            state
                .field(&from_field)
                .cloned()
                .unwrap_or_else(DecorationSet::new)
        })),
        decorations().of(DecorationSource::Static(DecorationSet::from_decorations([
            Decoration::widget(0, 1, spec("static")),
        ]))),
    ]);
    let start = state(
        doc(&schema, [n(&schema, "paragraph", [t(&schema, "abcd")])]),
        extensions,
    );
    let collected = collect_decorations(&start);
    assert_eq!(collected.inline().len(), 1);
    assert_eq!(collected.widgets().len(), 1);

    // The field maps itself, so a later state reports the moved range.
    let typed = start
        .update([crate::state::TransactionSpec::new()
            .changes([super::support::insert_text(&schema, 1, "XY")])])
        .expect("resolves")
        .state()
        .clone();
    let moved = collect_decorations(&typed);
    assert_eq!(moved.inline().as_slice()[0].from, 3);
}
