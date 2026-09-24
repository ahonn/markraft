//! Editor state, configuration, transactions, filters and JSON.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use super::support::*;
use crate::error::NodeError;
use crate::mark::MarkSet;
use crate::selection::Selection;
use crate::state::protocol::{append_config, reconfigure, user_event};
use crate::state::{
    ChangeFilterFn, ChangeFilterResult, Compartment, Dep, EditorState, EditorStateConfig,
    Extension, Facet, FacetConfig, Prec, StateField, StateFieldConfig, StateJsonFields,
    TransactionExtenderFn, TransactionFilterFn, TransactionSpec, change_filter,
    transaction_extender, transaction_filter,
};

fn counter() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let value = Arc::new(AtomicUsize::new(0));
    (value.clone(), value)
}

fn sample_doc(schema: &crate::schema::Schema) -> crate::node::Node {
    doc(schema, [n(schema, "paragraph", [t(schema, "hello")])])
}

#[test]
fn filter_ranges_are_sorted_and_merged_where_they_touch() {
    use crate::state::filters::{normalise_ranges, union_ranges};
    // Reversed ends are put in order; touching and overlapping ranges merge.
    assert_eq!(
        normalise_ranges(&[(5, 7), (4, 3), (1, 3), (10, 12)]),
        [(1, 4), (5, 7), (10, 12)]
    );
    assert_eq!(union_ranges(&[(1, 2)], &[(8, 9), (2, 5)]), [(1, 5), (8, 9)]);
}

#[test]
fn a_transaction_without_a_time_is_stamped_with_the_clock() {
    let schema = shared_schema();
    let start = state(sample_doc(&schema), Extension::none());
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let tr = start
        .update([TransactionSpec::new().changes([insert_text(&schema, 1, "x")])])
        .unwrap();
    let stamped = *tr
        .annotation(crate::state::protocol::time())
        .expect("a time");
    assert!(stamped >= before, "{stamped} is before {before}");
    let given = start.update([TransactionSpec::new().time(7)]).unwrap();
    assert_eq!(given.annotation(crate::state::protocol::time()), Some(&7));
}

#[test]
fn create_fills_in_a_document_and_a_cursor() {
    let schema = shared_schema();
    let state = EditorState::create(EditorStateConfig::new(schema.clone())).unwrap();
    assert_eq!(schema.describe(state.doc()), "doc(paragraph())");
    assert_eq!(state.selection(), &Selection::cursor(1));
    state.doc().check(&schema).unwrap();
}

#[test]
fn a_transaction_produces_a_new_state_and_maps_the_selection() {
    let schema = shared_schema();
    let state = state(sample_doc(&schema), Extension::none());
    let tr = state
        .update([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "XY")])
            .user_event("input.type")])
        .unwrap();
    assert!(tr.doc_changed());
    assert!(tr.is_user_event("input"));
    assert!(tr.is_user_event("input.type"));
    assert!(!tr.is_user_event("input.typex"));
    let next = state.apply(&tr);
    assert_eq!(schema.describe(next.doc()), r#"doc(paragraph("heXYllo"))"#);
    // The start cursor sat at 1 and stays before the insertion.
    assert_eq!(next.selection(), &Selection::cursor(1));
    next.doc().check(&schema).unwrap();
}

#[test]
fn facets_combine_their_inputs_in_precedence_then_tree_order() {
    let schema = shared_schema();
    let facet: Facet<&'static str> = Facet::list();
    let state = state(
        sample_doc(&schema),
        Extension::all([
            facet.of("default-first"),
            Prec::high(facet.of("high")),
            Prec::lowest(facet.of("lowest")),
            facet.of("default-second"),
            Prec::highest(facet.of("highest")),
        ]),
    );
    assert_eq!(
        state.facet(&facet),
        &vec![
            "highest",
            "high",
            "default-first",
            "default-second",
            "lowest"
        ]
    );
}

#[test]
fn an_unconfigured_facet_reads_its_default() {
    let schema = shared_schema();
    let facet: Facet<usize, usize> =
        Facet::define(FacetConfig::new(|inputs: &[usize]| inputs.len() + 7));
    let state = state(sample_doc(&schema), Extension::none());
    assert_eq!(state.facet(&facet), &7);
}

#[test]
fn a_computed_facet_is_re_evaluated_only_when_a_dependency_changes() {
    let schema = shared_schema();
    let (count, probe) = counter();
    let facet: Facet<usize, usize> = Facet::define(
        FacetConfig::new(|inputs: &[usize]| inputs.iter().sum()).compare(|a, b| a == b),
    );
    let state = state(
        sample_doc(&schema),
        facet.compute([Dep::Doc], move |state| {
            count.fetch_add(1, Ordering::SeqCst);
            state.doc().content_size()
        }),
    );
    assert_eq!(probe.load(Ordering::SeqCst), 1);
    assert_eq!(state.facet(&facet), &7);

    let moved = state
        .update([TransactionSpec::new().selection(Selection::cursor(3))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        probe.load(Ordering::SeqCst),
        1,
        "a selection is not the doc"
    );

    let typed = moved
        .update([TransactionSpec::new().changes([insert_text(&schema, 3, "!")])])
        .unwrap()
        .state()
        .clone();
    assert_eq!(probe.load(Ordering::SeqCst), 2);
    assert_eq!(typed.facet(&facet), &8);
}

#[test]
fn a_facet_keeps_its_value_when_the_combined_value_compares_equal() {
    let schema = shared_schema();
    let (count, probe) = counter();
    // The output only depends on whether the document is empty, so typing
    // recomputes the input but leaves the output equal.
    let facet: Facet<bool, bool> = Facet::define(
        FacetConfig::new(|inputs: &[bool]| inputs.iter().any(|v| *v)).compare(|a, b| a == b),
    );
    let derived: Facet<usize, usize> =
        Facet::define(FacetConfig::new(|inputs: &[usize]| inputs.len()));
    let state = state(
        sample_doc(&schema),
        Extension::all([
            facet.compute([Dep::Doc], |state| state.doc().content_size() > 0),
            derived.compute([Dep::facet(&facet)], move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                0
            }),
        ]),
    );
    assert_eq!(probe.load(Ordering::SeqCst), 1);
    let typed = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 3, "!")])])
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        probe.load(Ordering::SeqCst),
        1,
        "the facet output did not change, so its dependent was not recomputed"
    );
    assert!(*typed.facet(&facet));
}

#[test]
fn a_field_is_folded_forward_and_can_provide_a_facet_input() {
    let schema = shared_schema();
    let facet: Facet<usize> = Facet::list();
    let field = StateField::define(
        StateFieldConfig::new(
            |_| 0usize,
            |value, tr| value + usize::from(tr.doc_changed()),
        )
        .compare(|a, b| a == b)
        .provide({
            let facet = facet.clone();
            move |field| facet.from_field(field, |value| *value)
        }),
    );
    let state = state(sample_doc(&schema), field.extension());
    assert_eq!(state.field(&field), Some(&0));
    assert_eq!(state.facet(&facet), &vec![0]);

    let typed = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 3, "!")])])
        .unwrap()
        .state()
        .clone();
    assert_eq!(typed.field(&field), Some(&1));
    assert_eq!(typed.facet(&facet), &vec![1]);
}

#[test]
fn duplicate_extensions_take_part_once_at_the_highest_precedence() {
    let schema = shared_schema();
    let facet: Facet<&'static str> = Facet::list();
    let shared = facet.of("shared");
    let state = state(
        sample_doc(&schema),
        Extension::all([
            shared.clone(),
            facet.of("plain"),
            Prec::highest(shared.clone()),
        ]),
    );
    assert_eq!(state.facet(&facet), &vec!["shared", "plain"]);
}

#[test]
fn facet_inputs_can_enable_extensions() {
    let schema = shared_schema();
    let extra: Facet<&'static str> = Facet::list();
    let gate: Facet<&'static str> =
        Facet::define(FacetConfig::new(<[&'static str]>::to_vec).enables(extra.of("enabled")));
    let without = state(sample_doc(&schema), Extension::none());
    assert!(without.facet(&extra).is_empty());
    let with = state(sample_doc(&schema), gate.of("on"));
    assert_eq!(with.facet(&extra), &vec!["enabled"]);
}

#[test]
fn a_compartment_reconfigures_without_losing_unrelated_fields() {
    let schema = shared_schema();
    let facet: Facet<usize> = Facet::list();
    let field = StateField::define(StateFieldConfig::new(
        |_| 0usize,
        |value, tr| value + usize::from(tr.doc_changed()),
    ));
    let compartment = Compartment::new();
    let state = state(
        sample_doc(&schema),
        Extension::all([compartment.of(facet.of(1)), field.extension()]),
    );
    let typed = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 3, "!")])])
        .unwrap()
        .state()
        .clone();
    assert_eq!(typed.field(&field), Some(&1));

    let tr = typed
        .update([TransactionSpec::new().effect(compartment.reconfigure(facet.of(2)))])
        .unwrap();
    assert!(tr.reconfigured());
    let next = tr.state();
    assert_eq!(next.facet(&facet), &vec![2]);
    assert_eq!(
        next.field(&field),
        Some(&1),
        "a field that survives reconfiguration keeps its value"
    );
    assert!(compartment.get(next).is_some());
}

#[test]
fn reconfigure_recreates_fields_the_new_configuration_introduces() {
    let schema = shared_schema();
    let kept = StateField::define(StateFieldConfig::new(
        |_| 10usize,
        |value, tr| value + usize::from(tr.doc_changed()),
    ));
    let added = StateField::define(StateFieldConfig::new(|_| 99usize, |value, _| *value));
    let state = state(sample_doc(&schema), kept.extension());
    let typed = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 3, "!")])])
        .unwrap()
        .state()
        .clone();
    assert_eq!(typed.field(&kept), Some(&11));
    assert_eq!(typed.field(&added), None);

    let next = typed
        .update([TransactionSpec::new()
            .effect(reconfigure().of(Extension::all([kept.extension(), added.extension()])))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(next.field(&kept), Some(&11));
    assert_eq!(next.field(&added), Some(&99));
}

#[test]
fn append_config_extends_the_root_configuration() {
    let schema = shared_schema();
    let facet: Facet<&'static str> = Facet::list();
    let state = state(sample_doc(&schema), facet.of("base"));
    let next = state
        .update([TransactionSpec::new().effect(append_config().of(facet.of("added")))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(next.facet(&facet), &vec!["base", "added"]);
}

#[test]
fn two_specs_are_rebased_over_each_other() {
    let schema = shared_schema();
    let state = state(sample_doc(&schema), Extension::none());
    // Both specs address the starting document.
    let tr = state
        .update([
            TransactionSpec::new().changes([insert_text(&schema, 1, "A")]),
            TransactionSpec::new().changes([insert_text(&schema, 6, "B")]),
        ])
        .unwrap();
    assert_eq!(
        schema.describe(tr.new_doc()),
        r#"doc(paragraph("AhelloB"))"#
    );
}

#[test]
fn a_sequential_spec_addresses_the_intermediate_document() {
    let schema = shared_schema();
    let state = state(sample_doc(&schema), Extension::none());
    let tr = state
        .update([
            TransactionSpec::new().changes([insert_text(&schema, 1, "A")]),
            // Position 6 in the document the first spec produces.
            TransactionSpec::new()
                .changes([insert_text(&schema, 6, "B")])
                .sequential(),
        ])
        .unwrap();
    assert_eq!(
        schema.describe(tr.new_doc()),
        r#"doc(paragraph("AhellBo"))"#
    );
}

#[test]
fn a_change_filter_protects_a_range() {
    let schema = shared_schema();
    let guard: ChangeFilterFn = Arc::new(|_| ChangeFilterResult::AllowOnly(vec![(4, 6)]));
    let state = state(sample_doc(&schema), change_filter().of(guard));
    let tr = state
        .update([TransactionSpec::new()
            .changes([insert_text(&schema, 2, "A"), insert_text(&schema, 5, "B")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("hellBo"))"#);
}

#[test]
fn a_change_filter_can_block_everything() {
    let schema = shared_schema();
    let guard: ChangeFilterFn = Arc::new(|_| ChangeFilterResult::Block);
    let state = state(sample_doc(&schema), change_filter().of(guard));
    let tr = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert!(!tr.doc_changed());
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("hello"))"#);
}

#[test]
fn a_transaction_filter_can_block_and_replace() {
    let schema = shared_schema();
    let block: TransactionFilterFn = Arc::new(|_| Some(Vec::new()));
    let blocked = state(sample_doc(&schema), transaction_filter().of(block));
    let tr = blocked
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert!(!tr.doc_changed());

    let replace: TransactionFilterFn = Arc::new(|tr| {
        let schema = tr.start_state().schema().clone();
        Some(vec![
            TransactionSpec::new().changes([insert_text(&schema, 6, "Z")]),
        ])
    });
    let replaced = state(sample_doc(&schema), transaction_filter().of(replace));
    let tr = replaced
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("helloZ"))"#);

    // A filter that has no opinion leaves the transaction exactly as it was.
    let pass: TransactionFilterFn = Arc::new(|_| None);
    let untouched = state(sample_doc(&schema), transaction_filter().of(pass));
    let tr = untouched
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("hAello"))"#);

    // Amending through `as_spec` keeps the original change and adds to it.
    let amend: TransactionFilterFn = Arc::new(|tr| {
        Some(vec![
            tr.as_spec(),
            TransactionSpec::new()
                .annotate(user_event().of("input.paste".into()))
                .sequential(),
        ])
    });
    let amended = state(sample_doc(&schema), transaction_filter().of(amend));
    let tr = amended
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("hAello"))"#);
    assert!(tr.is_user_event("input.paste"));
}

#[test]
fn a_transaction_extender_adds_changes_and_annotations() {
    let schema = shared_schema();
    let extender: TransactionExtenderFn = Arc::new(|tr| {
        if !tr.doc_changed() {
            return None;
        }
        let schema = tr.start_state().schema().clone();
        let end = tr.new_doc().content_size() - 1;
        Some(
            TransactionSpec::new()
                .changes([insert_text(&schema, end, "!")])
                .annotate(user_event().of("input.type".into())),
        )
    });
    let state = state(sample_doc(&schema), transaction_extender().of(extender));
    let tr = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 1, "A")])])
        .unwrap();
    assert_eq!(
        schema.describe(tr.new_doc()),
        r#"doc(paragraph("Ahello!"))"#
    );
    assert!(tr.is_user_event("input.type"));
}

#[test]
fn a_state_round_trips_through_json_with_a_field() {
    let schema = shared_schema();
    let field = StateField::define(
        StateFieldConfig::new(|_| 0i64, |value, tr| value + i64::from(tr.doc_changed()))
            .to_json(|value, _| Value::from(*value))
            .from_json(|value, _| {
                value
                    .as_i64()
                    .ok_or_else(|| NodeError::Json("expected an integer".into()))
            }),
    );
    let state = state(sample_doc(&schema), field.extension());
    let state = state
        .update([TransactionSpec::new()
            .changes([insert_text(&schema, 3, "!")])
            .selection(Selection::text(2, 5))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(state.field(&field), Some(&1));

    let fields = StateJsonFields::new().add("count", &field);
    let json = state.to_json(&fields);
    let restored = EditorState::from_json(
        &json,
        EditorStateConfig::new(schema.clone()).extensions(field.extension()),
        &fields,
    )
    .unwrap();
    assert_eq!(restored.doc(), state.doc());
    assert_eq!(restored.selection(), state.selection());
    assert_eq!(restored.field(&field), Some(&1));
}

#[test]
fn a_static_facet_is_readable_from_the_configuration() {
    let schema = shared_schema();
    let facet: Facet<usize, usize> = Facet::define(
        FacetConfig::new(|inputs: &[usize]| inputs.iter().sum::<usize>()).static_only(),
    );
    let state = state(
        sample_doc(&schema),
        Extension::all([facet.of(2), facet.of(3)]),
    );
    assert_eq!(state.config().static_facet(&facet), Some(&5));
    assert_eq!(state.facet(&facet), &5);
}

#[test]
fn the_core_types_are_shareable_values() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EditorState>();
    assert_send_sync::<crate::state::Transaction>();
    assert_send_sync::<Selection>();
    assert_send_sync::<Extension>();
    assert_send_sync::<crate::state::StateEffect>();
    assert_send_sync::<crate::state::Annotation>();
}

#[test]
fn reconfigure_discards_appended_extensions_but_keeps_compartment_contents() {
    let schema = shared_schema();
    let facet: Facet<&'static str> = Facet::list();
    let compartment = Compartment::new();
    let root = Extension::all([compartment.of(facet.of("start")), facet.of("root")]);
    let state = state(sample_doc(&schema), root.clone());
    let state = state
        .update([TransactionSpec::new()
            .effect(append_config().of(facet.of("appended")))
            .effect(compartment.reconfigure(facet.of("swapped")))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(state.facet(&facet), &vec!["swapped", "root", "appended"]);

    let next = state
        .update([TransactionSpec::new().effect(reconfigure().of(root))])
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        next.facet(&facet),
        &vec!["swapped", "root"],
        "the appended input is gone, the compartment keeps what it was set to"
    );
}

#[test]
fn no_filter_bypasses_the_filters() {
    let schema = shared_schema();
    let block: ChangeFilterFn = Arc::new(|_| ChangeFilterResult::Block);
    let state = state(sample_doc(&schema), change_filter().of(block));
    let tr = state
        .update([TransactionSpec::new()
            .changes([insert_text(&schema, 2, "A")])
            .no_filter()])
        .unwrap();
    assert_eq!(schema.describe(tr.new_doc()), r#"doc(paragraph("hAello"))"#);
}

#[test]
fn extenders_run_from_the_lowest_precedence_to_the_highest() {
    let schema = shared_schema();
    let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let make = |name: &'static str,
                log: Arc<std::sync::Mutex<Vec<&'static str>>>|
     -> TransactionExtenderFn {
        Arc::new(move |_| {
            log.lock().unwrap().push(name);
            None
        })
    };
    let state = state(
        sample_doc(&schema),
        Extension::all([
            Prec::high(transaction_extender().of(make("high", order.clone()))),
            transaction_extender().of(make("default", order.clone())),
            Prec::low(transaction_extender().of(make("low", order.clone()))),
        ]),
    );
    let _ = state
        .update([TransactionSpec::new().changes([insert_text(&schema, 2, "A")])])
        .unwrap();
    assert_eq!(&*order.lock().unwrap(), &["low", "default", "high"]);
}

fn strong_marks(schema: &crate::schema::Schema) -> MarkSet {
    MarkSet::from_marks(schema, [m(schema, "strong")])
}

fn after(state: &EditorState, spec: TransactionSpec) -> EditorState {
    state.update([spec]).unwrap().state().clone()
}

#[test]
fn stored_marks_are_kept_only_by_transactions_that_leave_doc_and_selection_alone() {
    let schema = shared_schema();
    let start = state(sample_doc(&schema), Extension::none());
    assert_eq!(start.stored_marks(), None);
    let armed = after(
        &start,
        TransactionSpec::new().stored_marks(Some(strong_marks(&schema))),
    );
    assert_eq!(armed.stored_marks(), Some(&strong_marks(&schema)));
    assert_eq!(armed.selection(), start.selection());

    // An annotation-only transaction keeps them.
    let annotated = after(&armed, TransactionSpec::new().user_event("noop"));
    assert_eq!(annotated.stored_marks(), Some(&strong_marks(&schema)));

    // A document change clears them.
    let edited = after(
        &armed,
        TransactionSpec::new().changes([insert_text(&schema, 6, "!")]),
    );
    assert_eq!(edited.stored_marks(), None);

    // An explicit selection clears them, even one equal to the current one.
    let reselected = after(
        &armed,
        TransactionSpec::new().selection(armed.selection().clone()),
    );
    assert_eq!(reselected.stored_marks(), None);

    // An explicit `None` clears them without any other change.
    let cleared = after(&armed, TransactionSpec::new().stored_marks(None));
    assert_eq!(cleared.stored_marks(), None);
    assert_eq!(cleared.doc(), armed.doc());

    // An explicit setting wins over the default clearing.
    let kept = after(
        &armed,
        TransactionSpec::new()
            .changes([insert_text(&schema, 6, "!")])
            .selection(Selection::cursor(7))
            .stored_marks(armed.stored_marks().cloned()),
    );
    assert_eq!(kept.stored_marks(), Some(&strong_marks(&schema)));
}

#[test]
fn a_later_spec_decides_the_stored_marks() {
    let schema = shared_schema();
    let start = state(sample_doc(&schema), Extension::none());
    let tr = start
        .update([
            TransactionSpec::new().stored_marks(Some(strong_marks(&schema))),
            TransactionSpec::new().user_event("noop"),
        ])
        .unwrap();
    assert_eq!(tr.stored_marks(), Some(&Some(strong_marks(&schema))));
    assert_eq!(tr.state().stored_marks(), Some(&strong_marks(&schema)));

    let tr = start
        .update([
            TransactionSpec::new().stored_marks(Some(strong_marks(&schema))),
            TransactionSpec::new().stored_marks(None),
        ])
        .unwrap();
    assert_eq!(tr.stored_marks(), Some(&None));
    assert_eq!(tr.state().stored_marks(), None);

    // `as_spec` carries the explicit setting, so a filter that amends the
    // transaction keeps it.
    let tr = start
        .update([TransactionSpec::new().stored_marks(Some(strong_marks(&schema)))])
        .unwrap();
    let again = start.update([tr.as_spec()]).unwrap();
    assert_eq!(again.state().stored_marks(), Some(&strong_marks(&schema)));
}

#[test]
fn setting_stored_marks_counts_as_a_selection_dependency() {
    let schema = shared_schema();
    let (count, probe) = counter();
    let facet: Facet<usize, usize> =
        Facet::define(FacetConfig::new(|inputs: &[usize]| inputs.iter().sum()));
    let start = state(
        sample_doc(&schema),
        facet.compute([Dep::Selection], move |state| {
            count.fetch_add(1, Ordering::SeqCst);
            state.stored_marks().map_or(0, |marks| marks.iter().count())
        }),
    );
    assert_eq!(probe.load(Ordering::SeqCst), 1);
    let annotated = after(&start, TransactionSpec::new().user_event("noop"));
    assert_eq!(probe.load(Ordering::SeqCst), 1);
    let armed = after(
        &annotated,
        TransactionSpec::new().stored_marks(Some(strong_marks(&schema))),
    );
    assert_eq!(probe.load(Ordering::SeqCst), 2);
    assert_eq!(armed.facet(&facet), &1);
}

#[test]
fn stored_marks_round_trip_through_json() {
    let schema = shared_schema();
    let fields = StateJsonFields::new();
    let plain = state(sample_doc(&schema), Extension::none());
    let json = plain.to_json(&fields);
    assert!(json.get("storedMarks").is_none());
    let restored =
        EditorState::from_json(&json, EditorStateConfig::new(schema.clone()), &fields).unwrap();
    assert_eq!(restored.stored_marks(), None);

    for marks in [strong_marks(&schema), MarkSet::empty()] {
        let armed = after(
            &plain,
            TransactionSpec::new().stored_marks(Some(marks.clone())),
        );
        let json = armed.to_json(&fields);
        assert!(json.get("storedMarks").is_some_and(Value::is_array));
        let restored =
            EditorState::from_json(&json, EditorStateConfig::new(schema.clone()), &fields).unwrap();
        assert_eq!(restored.stored_marks(), Some(&marks));
        assert_eq!(restored.selection(), armed.selection());
    }

    let mut bad = plain.to_json(&fields);
    bad["storedMarks"] = Value::from(1);
    assert!(EditorState::from_json(&bad, EditorStateConfig::new(schema.clone()), &fields).is_err());
}
