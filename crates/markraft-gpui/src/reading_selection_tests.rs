//! Visible-selection edits share the core selection contract across input paths.
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::{
    Change, EditorState, EditorStateConfig, Extension, Fragment, ReplacementStyle, Selection,
    Slice, TransactionSpec, commands, composition, history,
    kind::{DocTypes, ReadingSelection, chains, conceal::Reveal},
    projection::projection_of,
};

fn state(source: &str, target: &str) -> (EditorState, DocTypes) {
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let state = EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(from_markdown(&schema, source).unwrap())
            .extensions(Extension::all([
                markraft_core::projection::projection(),
                commonmark_extensions(&schema),
                history::history(Default::default()),
                composition::composition(),
            ])),
    )
    .unwrap();
    let projection = projection_of(&state);
    let text = projection.line_text(0).unwrap();
    let start = text[..text.find(target).unwrap()].chars().count();
    let line = &projection.lines()[0];
    let selected = ReadingSelection::selection(
        line.offset_to_pos(start).unwrap(),
        line.offset_to_pos(start + target.chars().count()).unwrap(),
        types.syntax.unwrap(),
    );
    (
        apply(&state, vec![TransactionSpec::new().selection(selected)]),
        types,
    )
}

fn apply(state: &EditorState, specs: Vec<TransactionSpec>) -> EditorState {
    state
        .update_with_appended(specs)
        .unwrap()
        .last()
        .unwrap()
        .state()
        .clone()
}

fn reading(state: &EditorState, types: &DocTypes) -> String {
    crate::shown::ShownText::build(&projection_of(state), types, &Reveal::nothing())
        .text()
        .to_owned()
}

#[test]
fn reading_selection_replacement_deletion_paste_and_undo_balance_markup() {
    for source in ["宇**宙**洪荒"] {
        let (state, types) = state(source, "宇**宙");
        for (command, expected) in [
            (commands::insert_text("X"), "X洪荒"),
            (chains::insert_plain(&types, "X"), "X洪荒"),
            (commands::delete_selection(), "洪荒"),
            (chains::backspace(&types), "洪荒"),
            (chains::delete_forward(&types), "洪荒"),
            (
                commands::replace_selection(Slice::from_fragment(Fragment::from_node(
                    state.schema().text("X"),
                ))),
                "X洪荒",
            ),
        ] {
            let edited = apply(&state, vec![command(&state).unwrap()]);
            assert_eq!(reading(&edited, &types), expected, "{source}");
            assert!(edited.selection().is_cursor());
            let restored = apply(&edited, vec![history::undo(&edited).unwrap()]);
            assert_eq!(restored.doc(), state.doc());
            assert_eq!(restored.selection(), state.selection());
        }
    }
}

#[test]
fn reading_selection_mapping_json_and_source_edits_remain_distinct() {
    let (state, types) = state("宇**宙**洪荒", "宇**宙");
    let selected = state.selection();
    assert_eq!(
        ReadingSelection::from_json(state.schema(), &selected.to_json(state.schema())).unwrap(),
        *selected
    );
    let range = selected.replacement_range(state.doc());
    assert_eq!(
        selected.content_with_schema(state.doc(), state.schema()),
        Selection::text(range.from, range.to).content_with_schema(state.doc(), state.schema())
    );
    let moved = apply(
        &state,
        vec![TransactionSpec::new().changes([Change::insert(
            1,
            Slice::from_fragment(Fragment::from_node(state.schema().text("pre "))),
        )])],
    );
    assert!(ReadingSelection::is(moved.selection()));
    assert_eq!(moved.selection().from(moved.doc()), range.from + 4);
    let navigation = commands::move_by_grapheme(commands::Direction::Forward, false);
    let navigated = apply(&state, vec![navigation(&state).unwrap()]);
    assert!(navigated.selection().is_cursor());
    let source = apply(
        &state,
        vec![TransactionSpec::new().selection(Selection::text(range.from, range.to))],
    );
    let literal = apply(
        &source,
        vec![chains::insert_plain(&types, "X")(&source).unwrap()],
    );
    assert_eq!(
        projection_of(&literal).plain_text(),
        "X**洪荒",
        "explicit source editing stays literal"
    );
}

#[test]
fn reading_selection_typing_keeps_the_starting_style_in_source() {
    for (source, target, expected) in [
        ("**宇**宙洪荒", "宇**宙", "**X**洪荒"),
        ("***宇***宙洪荒", "宇***宙", "***X***洪荒"),
        ("宇**宙洪**荒", "宇**宙", "X**洪**荒"),
        ("&#x5b87;**宙**洪荒", "&#x5b87;**宙", "X洪荒"),
    ] {
        let (state, types) = state(source, target);
        let edited = apply(
            &state,
            vec![chains::insert_plain(&types, "X")(&state).unwrap()],
        );
        assert_eq!(projection_of(&edited).plain_text(), expected);
        let restored = apply(&edited, vec![history::undo(&edited).unwrap()]);
        assert_eq!(restored.doc(), state.doc());
        assert_eq!(restored.selection(), state.selection());
    }
}

#[test]
fn reading_selection_distinguishes_plain_style_inheritance_from_rich_paste() {
    use markraft_core::kind::Codecs;
    let (state, types) = state("**宇**宙洪荒", "宇**宙");
    let codecs =
        markraft_commonmark::CommonMarkCodecs::new(state.schema().clone(), Default::default());
    let plain = commands::replace_selection_as(
        codecs.from_text("X"),
        ReplacementStyle::Receiving,
        markraft_core::protocol::event::INPUT_PASTE,
    )(&state)
    .unwrap();
    let pasted = state.update([plain]).unwrap();
    assert!(pasted.is_user_event(markraft_core::protocol::event::INPUT_PASTE));
    assert_eq!(projection_of(pasted.state()).plain_text(), "**X**洪荒");
    let plain = apply(
        &state,
        vec![
            commands::replace_selection_as(
                codecs.from_text("X"),
                ReplacementStyle::Receiving,
                markraft_core::protocol::event::INPUT_TYPE,
            )(&state)
            .unwrap(),
        ],
    );
    assert_eq!(projection_of(&plain).plain_text(), "**X**洪荒");
    let unstyled = commands::replace_selection(codecs.from_text("X"))(&state).unwrap();
    assert_eq!(
        projection_of(&apply(&state, vec![unstyled])).plain_text(),
        "X洪荒"
    );
    let rich = from_markdown(state.schema(), "*X*").unwrap();
    let rich =
        commands::replace_selection(Slice::new(rich.content().clone(), 1, 1))(&state).unwrap();
    assert_eq!(
        projection_of(&apply(&state, vec![rich])).plain_text(),
        "*X*洪荒"
    );
    let multiline = chains::insert_plain(&types, "X\nY")(&state).unwrap();
    let multiline = apply(&state, vec![multiline]);
    assert_eq!(reading(&multiline, &types), "X\nY洪荒");
}

#[test]
fn reading_selection_composition_and_commit_use_actual_inserted_range() {
    for (source, target) in [("宇**宙**洪荒", "宇**宙"), ("**宇**宙洪荒", "宇**宙")] {
        let (state, types) = state(source, target);
        let committed = apply(&state, crate::ime::commit_specs(&state, &types, None, "X"));
        assert_eq!(reading(&committed, &types), "X洪荒");
        let first = composition::update_composition_with(
            &state,
            "候选",
            2,
            &chains::insert_plain(&types, "候选"),
        )
        .unwrap();
        let composing = apply(&state, vec![first]);
        assert_eq!(reading(&composing, &types), "候选洪荒");
        let range = composition::composition_range(&composing).unwrap();
        assert_eq!(
            projection_of(&composing).text_between(range.from, range.to),
            Some("候选")
        );
        let committed = apply(
            &composing,
            crate::ime::commit_specs(&composing, &types, None, "最终"),
        );
        assert_eq!(reading(&committed, &types), "最终洪荒");
        let restored = apply(&committed, vec![history::undo(&committed).unwrap()]);
        assert_eq!(restored.doc(), state.doc());
        assert_eq!(restored.selection(), state.selection());
        let empty = composition::update_composition(&state, "", 0).unwrap();
        let deleted = apply(&state, vec![empty]);
        assert_eq!(reading(&deleted, &types), "洪荒");
    }
}
