use super::*;
use crate::Setup;
use gpui::{AppContext, Entity, MouseButton, TestAppContext, VisualTestContext, point, px};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::{commands, composition, history, kind::DocTypes};

fn setup(source: &str) -> Setup {
    let schema = commonmark_schema();
    Setup::new(schema.clone())
        .types(DocTypes::from_schema_names(
            &schema,
            &commonmark_doc_type_names(),
        ))
        .extensions(commonmark_extensions(&schema))
        .doc(from_markdown(&schema, source).unwrap())
}

struct ReadingKind;

impl crate::DocumentKind for ReadingKind {
    fn replace_reading(
        &self,
        range: Range<usize>,
        text: &str,
        policy: markraft_core::kind::ReadingReplacementPolicy,
    ) -> Option<crate::Formatting> {
        let command = markraft_commonmark::Formatter::new(Default::default())
            .replace_reading(range, text, policy);
        Some(std::sync::Arc::new(move |state| {
            command(state).map_err(|error| error.to_string())
        }))
    }
}

fn range(view: &EditorView) -> Range<usize> {
    view.state.selection().from(view.state.doc())..view.state.selection().to(view.state.doc())
}

fn click_character(
    view: &Entity<EditorView>,
    cx: &mut VisualTestContext,
    offset: usize,
    control: bool,
) {
    let position = view.read_with(cx, |view, _| {
        view.frame.rows()[0].rectangles(offset..offset + 1, false)[0].center()
    });
    cx.simulate_mouse_down(
        position,
        if control {
            MouseButton::Left
        } else {
            MouseButton::Right
        },
        gpui::Modifiers {
            control,
            ..Default::default()
        },
    );
    cx.run_until_parked();
}

#[gpui::test]
fn smart_clipboard_maps_prose_and_excludes_delimiters_and_code(cx: &mut TestAppContext) {
    let view =
        cx.new(|cx| EditorView::new(setup("one **two** three"), cx).with_smart_insert_delete(true));
    view.update(cx, |view, cx| {
        view.select_range(7, 10, cx);
        let context = view.smart_clipboard_context().unwrap();
        assert_eq!(context.text, "one two three");
        assert_eq!(context.selection, 4..7);
        view.select_range(5, 12, cx);
        assert!(view.smart_clipboard_context().is_none());
        view.set_smart_insert_delete(false);
        view.select_range(1, 4, cx);
        assert!(view.smart_clipboard_context().is_none());
    });
    let code =
        cx.new(|cx| EditorView::new(setup("one `two` three"), cx).with_smart_insert_delete(true));
    code.update(cx, |view, cx| {
        view.select_range(6, 9, cx);
        assert!(view.smart_clipboard_context().is_none());
    });
}

#[gpui::test]
fn only_a_selection_made_by_a_word_gesture_counts_as_a_word(cx: &mut TestAppContext) {
    let view =
        cx.new(|cx| EditorView::new(setup("one two three"), cx).with_smart_insert_delete(true));
    view.update(cx, |view, cx| {
        // The same range, extended by hand: deletion takes it and no more.
        view.select_range(5, 8, cx);
        assert!(!view.selected_by_word());
        assert!(view.smart_delete_spec().is_none());
        assert!(!view.smart_copy_eligible());

        view.select_context(6, Some(6), false, &ContextTarget::Text, cx);
        assert_eq!(view.state.selection(), &Selection::text(5, 8));
        assert!(view.selected_by_word());
        // Any other selection ends the gesture, and coming back to the
        // range does not restore it.
        view.select(6, false, cx);
        assert!(!view.selected_by_word());
        view.select_range(5, 8, cx);
        assert!(!view.selected_by_word());
        // A replaced document ends it too, whatever is selected afterwards.
        view.select_context(6, Some(6), false, &ContextTarget::Text, cx);
        assert!(view.selected_by_word());
        view.replace_doc(view.state.doc().clone(), cx);
        assert!(view.word_selection.is_none());
    });
}

#[gpui::test]
fn smart_padding_is_atomic_and_one_undo(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup("one three"), cx));
    view.update(cx, |view, cx| {
        view.select(4, false, cx);
        let spec = commands::insert_text("two")(&view.state).unwrap();
        let specs = view.padded_paste_specs(spec, 4..7, " ", "");
        assert!(view.dispatch_isolated(specs, cx));
        assert_eq!(view.projection().plain_text(), "one two three");
        assert_eq!(history::undo_depth(&view.state), 1);
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "one three");
        view.document_guard = Some(Box::new(|_| {
            Err(crate::EditRejection::Refused("read only".into()))
        }));
        let spec = commands::insert_text("two")(&view.state).unwrap();
        let specs = view.padded_paste_specs(spec, 4..7, " ", "");
        assert!(!view.dispatch_isolated(specs, cx));
        assert_eq!(view.projection().plain_text(), "one three");
    });
}

#[gpui::test]
fn return_only_services_support_empty_paragraphs_and_preserve_enclosing_style(
    cx: &mut TestAppContext,
) {
    for (source, position, expected) in [("", 1, "inserted"), ("**word**", 5, "**woinsertedrd**")] {
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.update(cx, |view, cx| {
            view.codecs = Some(std::sync::Arc::new(
                markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    Default::default(),
                ),
            ));
            view.select(position, false, cx);
            let request = view.context_snapshot(Point::default(), ContextTarget::Text);
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(view.replace_context_text(&request, "inserted", cx));
            assert_eq!(view.projection().plain_text(), expected);
        });
    }
}

#[gpui::test]
fn return_only_services_insert_literal_text_at_caret_and_reject_stale_or_readonly(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| EditorView::new(setup("before after"), cx));
    view.update(cx, |view, cx| {
        view.codecs = Some(std::sync::Arc::new(
            markraft_commonmark::CommonMarkCodecs::new(
                view.state.schema().clone(),
                Default::default(),
            ),
        ));
        view.select(8, false, cx);
        let request = view.context_snapshot(Point::default(), ContextTarget::Text);
        assert_eq!(view.context_text(&request).unwrap().text, "");
        assert!(view.replace_context_text(&request, "*literal* ", cx));
        assert_eq!(
            view.context_text(&view.context_document_snapshot())
                .unwrap()
                .text,
            "before *literal* after"
        );
        assert!(!view.replace_context_text(&request, "stale", cx));
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "before after");
        let request = view.context_snapshot(Point::default(), ContextTarget::Text);
        view.document_guard = Some(Box::new(|_| {
            Err(crate::EditRejection::Refused("read only".into()))
        }));
        assert!(!view.replace_context_text(&request, "blocked", cx));
        assert_eq!(view.projection().plain_text(), "before after");
    });
}

#[gpui::test]
fn right_click_preserves_selection_or_selects_clicked_word(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("one two three"), cx));
    cx.run_until_parked();
    view.update(cx, |view, cx| view.select_range(1, 8, cx));
    cx.run_until_parked();
    click_character(&view, cx, 5, false);
    view.read_with(cx, |view, _| {
        assert_eq!(range(view), 1..8);
        assert!(!view.selecting);
    });
    click_character(&view, cx, 10, true);
    view.read_with(cx, |view, _| {
        assert_eq!(range(view), 9..14);
        assert!(!view.selecting);
    });
    let blank = view.read_with(cx, |view, _| {
        let row = &view.frame.rows()[0];
        let last = row.rectangles(row.char_len - 1..row.char_len, false)[0];
        point(last.right() + px(15.), last.center().y)
    });
    cx.simulate_mouse_down(blank, MouseButton::Right, Default::default());
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert_eq!(range(view), 14..14));
}

#[gpui::test]
fn right_click_word_ranges_keep_contextual_dictionary_and_whitespace(cx: &mut TestAppContext) {
    for (source, offset, expected) in [
        ("one   two", 4, "   "),
        ("one...two", 4, "."),
        #[cfg(target_os = "macos")]
        ("中文编辑器测试", 2, "编辑"),
    ] {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(source), cx));
        cx.run_until_parked();
        click_character(&view, cx, offset, false);
        view.read_with(cx, |view, _| {
            let range = range(view);
            assert_eq!(
                view.projection().text_between(range.start, range.end),
                Some(expected)
            );
        });
    }
}

#[gpui::test]
fn right_click_selects_the_entire_link_label_but_preserves_existing_selection(
    cx: &mut TestAppContext,
) {
    let source = "[sample **bold** link](https://example.com) tail";
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(source), cx));
    cx.run_until_parked();
    let expected = 2..source.find(']').unwrap() + 1;
    click_character(&view, cx, 3, false);
    view.read_with(cx, |view, _| assert_eq!(range(view), expected));

    // Right-clicking within a partial label selection must not expand it.
    view.update(cx, |view, cx| view.select_range(2, 8, cx));
    cx.run_until_parked();
    click_character(&view, cx, 3, false);
    view.read_with(cx, |view, _| assert_eq!(range(view), 2..8));

    // A reversed selection spanning the link and surrounding text is also
    // preserved, including its anchor direction.
    let end = source.chars().count() + 1;
    view.update(cx, |view, cx| view.select_range(end, 2, cx));
    cx.run_until_parked();
    let selected = view.read_with(cx, |view, _| view.state.selection().clone());
    click_character(&view, cx, 3, false);
    view.read_with(cx, |view, _| assert_eq!(view.state.selection(), &selected));

    // Control-click outside the old selection uses the same link policy.
    view.update(cx, |view, cx| view.select_range(end - 4, end, cx));
    cx.run_until_parked();
    click_character(&view, cx, 3, true);
    view.read_with(cx, |view, _| assert_eq!(range(view), expected));
}

#[gpui::test]
fn link_labels_exclude_destinations_and_resolve_character_positions(cx: &mut TestAppContext) {
    for (source, position, expected) in [
        ("[中文 标签](https://example.com)", 3, 2..7),
        ("[**bold**](https://example.com)", 5, 4..8),
        ("<https://example.com>", 5, 2..21),
        ("https://example.com", 5, 1..20),
        ("[a \\* b](https://example.com)", 2, 2..8),
        ("[sample link][ref]\n\n[ref]: https://example.com", 3, 2..13),
        (
            "[one](https://example.com)[two](https://example.com)",
            3,
            2..5,
        ),
        ("[one](https://example.com)https://example.com", 3, 2..5),
        ("[one](https://example.com)https://example.com", 27, 27..46),
    ] {
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.read_with(cx, |view, _| {
            let (range, _) = links::link_at(view.state.doc(), view.types.link.unwrap(), position)
                .expect("a link at the test position");
            let range = view.context_link_range(range, position);
            assert_eq!(view.context_link_label(&range), Some(expected), "{source}");
        });
    }
}

#[gpui::test]
fn right_click_on_task_marker_does_not_toggle_or_start_drag(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("- [ ] task"), cx));
    cx.run_until_parked();
    let (position, doc) = view.read_with(cx, |view, _| {
        (
            view.frame.rows()[0].task_marker().unwrap().1.center(),
            view.state.doc().clone(),
        )
    });
    cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.state.doc(), &doc);
        assert!(!view.selecting);
    });
}

#[gpui::test]
fn right_click_on_link_does_not_emit_navigation(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|_, cx| EditorView::new(setup("[link](https://example.com) tail"), cx));
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            seen.borrow_mut().push(event.clone())
        })
        .detach()
    });
    cx.run_until_parked();
    let position = view.read_with(cx, |view, _| {
        let row = &view.frame.rows()[0];
        row.rectangles(2..3, false)[0].center()
    });
    cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
    cx.run_until_parked();
    assert!(!events.borrow().iter().any(|event| matches!(
        event,
        EditorEvent::LinkClicked | EditorEvent::WikiLinkClicked { .. }
    )));
    assert!(events.borrow().iter().any(|event| matches!(
        event,
        EditorEvent::ContextMenuRequested(ContextRequest {
            target: ContextTarget::Link { .. },
            ..
        })
    )));
}

#[gpui::test]
fn table_context_keeps_clicked_cell_distinct_from_selection_head(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| {
        EditorView::new(setup("| first | second |\n| --- | --- |\n| a | b |"), cx)
    });
    cx.run_until_parked();
    let (position, expected_cell) = view.update(cx, |view, cx| {
        let rows = view.frame.rows();
        let first = rows
            .iter()
            .find(|row| {
                row.table
                    .is_some_and(|cell| cell.row == 0 && cell.column == 0)
            })
            .unwrap();
        let second = rows
            .iter()
            .find(|row| {
                row.table
                    .is_some_and(|cell| cell.row == 0 && cell.column == 1)
            })
            .unwrap();
        let from = first.from;
        let to = second.offset_to_pos(second.char_len);
        let position = first.rectangles(1..2, false)[0].center();
        let target = view.context_target_at(position, first.offset_to_pos(1));
        let ContextTarget::Table { cell, .. } = target else {
            panic!("expected a table target")
        };
        view.select_range(from, to, cx);
        (position, cell)
    });
    cx.run_until_parked();
    let before = view.read_with(cx, |view, _| range(view));
    let received = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen = received.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            if let EditorEvent::ContextMenuRequested(request) = event {
                *seen.borrow_mut() = Some(request.clone());
            }
        })
        .detach()
    });
    cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
    cx.run_until_parked();
    let request = received.borrow().clone().unwrap();
    assert_eq!(request.selection_range(), before);
    assert!(matches!(request.target, ContextTarget::Table { cell, .. } if cell == expected_cell));
    view.read_with(cx, |view, _| {
        let head = commands::cell_at(view.types.table_types().unwrap(), view.state()).unwrap();
        assert_ne!(head.cell, expected_cell);
    });
}

#[gpui::test]
fn menu_snapshot_captures_selection_after_extensions_settle(cx: &mut TestAppContext) {
    struct NormalizePointer;
    impl crate::Extension for NormalizePointer {
        fn id(&self) -> &'static str {
            "normalize-pointer"
        }
        fn update(&mut self, update: &crate::Update, cx: &mut crate::EditorCx<'_>) {
            if update.is_user_event(event::SELECT_POINTER) {
                cx.select(Selection::text(1, 4), false);
            }
        }
    }
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup("one two three"), cx));
    let _extension = view.update(cx, |view, cx| view.add_extension(NormalizePointer, cx));
    let received = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen = received.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            if let EditorEvent::ContextMenuRequested(request) = event {
                *seen.borrow_mut() = Some(request.clone());
            }
        })
        .detach()
    });
    cx.run_until_parked();
    click_character(&view, cx, 10, false);
    let request = received.borrow().clone().unwrap();
    assert_eq!(request.selection_range(), 1..4);
    view.read_with(cx, |view, _| assert!(view.context_is_current(&request)));
}

#[test]
fn word_ranges_map_display_scalars_and_include_whitespace() {
    let source = "naïve café 中文 👩‍💻";
    let setup = setup(source);
    let projection = Projection::of(setup.doc.as_ref().unwrap(), &setup.schema);
    let pieces = crate::shown::line_pieces(
        &projection,
        &setup.types,
        0,
        &markraft_core::kind::conceal::Reveal::nothing(),
    );
    assert_eq!(
        word_at(
            &projection,
            3,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(1..6)
    );
    assert_eq!(
        word_at(
            &projection,
            8,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(7..11)
    );
    assert_eq!(
        word_at(
            &projection,
            6,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(6..7)
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        word_at(
            &projection,
            12,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(12..14)
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        word_at(
            &projection,
            12,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(12..13)
    );
    assert_eq!(
        word_at(
            &projection,
            16,
            &pieces,
            crate::word_boundary::Intent::Context
        ),
        Some(15..18)
    );
}

#[test]
fn pointer_words_follow_concealment_and_preserve_source_provenance() {
    use markraft_core::kind::conceal::Reveal;

    let resolve = |source: &str, target: &str, revealed: bool| {
        let setup = setup(source);
        let projection = Projection::of(setup.doc.as_ref().unwrap(), &setup.schema);
        let line = &projection.lines()[0];
        let source = projection.line_text(0).unwrap();
        let offset = source[..source.find(target).unwrap()].chars().count();
        let position = line.offset_to_pos(offset).unwrap();
        let reveal = if revealed {
            Reveal::at(line.from()..line.to(), None)
        } else {
            Reveal::nothing()
        };
        let pieces = crate::shown::line_pieces(&projection, &setup.types, 0, &reveal);
        word_at(
            &projection,
            position,
            &pieces,
            crate::word_boundary::Intent::Pointer,
        )
        .map(|range| {
            projection
                .text_between(range.start, range.end)
                .unwrap()
                .to_owned()
        })
    };
    assert_eq!(resolve("a&#98;c", "a", false).as_deref(), Some("a&#98;c"));
    assert_eq!(resolve("a&#98;c", "&", false).as_deref(), Some("a&#98;c"));
    assert_eq!(resolve("a&#98;c", "&", true).as_deref(), Some("&"));
    assert_eq!(resolve("**word**", "*", false), None);
    assert_eq!(resolve("**word**", "*", true).as_deref(), Some("*"));
    assert_eq!(resolve("a[[Notes]]b", "a", false).as_deref(), Some("a"));
    assert_eq!(resolve("a[[Notes]]b", "b", false).as_deref(), Some("b"));
    assert_eq!(resolve("a[[Notes]]b", "\u{fffc}", false), None);
    #[cfg(target_os = "macos")]
    assert_eq!(resolve("宇**宙**", "宙", false).as_deref(), Some("宇**宙"));
}

#[gpui::test]
fn context_commits_composition_without_discarding_candidate(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup(""), cx));
    view.update(cx, |view, cx| {
        let candidate = composition::update_composition(&view.state, "中文", 2).unwrap();
        view.dispatch([candidate], cx);
        assert!(view.is_composing());
        let candidate_doc = view.state.doc().clone();
        view.select_context(1, Some(1), false, &ContextTarget::Text, cx);
        assert!(!view.is_composing());
        assert_eq!(view.state.doc(), &candidate_doc);
        assert_eq!(view.committed_document(), &candidate_doc);
        assert!(view.context_is_current(
            &view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text)
        ));
    });
}

#[gpui::test]
fn menu_snapshot_expires_after_selection_composition_undo_and_replacement(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup("hello"), cx));
    view.update(cx, |view, cx| {
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        view.select(2, false, cx);
        view.select(1, false, cx);
        assert!(!view.context_is_current(&snapshot));
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        view.run_command(&commands::insert_text("x"), cx);
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert!(!view.context_is_current(&snapshot));
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
        view.dispatch([candidate], cx);
        assert!(!view.context_is_current(&snapshot));
        view.cancel_composition(cx);
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        view.replace_doc(view.state.doc().clone(), cx);
        assert!(!view.context_is_current(&snapshot));
    });
}

#[gpui::test]
fn capabilities_are_read_only_and_keep_file_pastes_available(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup("hello"), cx));
    view.update(cx, |view, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        assert_eq!(view.edit_capabilities(cx), EditCapabilities::default());
        assert!(view.context_is_current(&snapshot));
        view.begin_undo_group();
        view.end_undo_group();
        assert!(
            view.context_is_current(&snapshot),
            "history boundaries do not move a menu's editing target"
        );
        view.select_range(1, 6, cx);
        cx.write_to_clipboard(ClipboardItem::new_string("text".into()));
        let snapshot = view.context_snapshot(point(px(0.), px(0.)), ContextTarget::Text);
        let capabilities = view.edit_capabilities(cx);
        assert!(
            capabilities.copy
                && capabilities.cut
                && capabilities.paste
                && capabilities.paste_plain
                && capabilities.paste_markdown
        );
        assert!(view.context_is_current(&snapshot));
        view.file_paste = true;
        cx.write_to_clipboard(ClipboardItem::from(ClipboardEntry::ExternalPaths(
            gpui::ExternalPaths(vec!["/tmp/image.png".into()].into()),
        )));
        let capabilities = view.edit_capabilities(cx);
        assert!(capabilities.paste && capabilities.paste_markdown);
        assert!(
            capabilities.paste_plain,
            "external paths also carry their path as text"
        );
        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::empty()));
        let capabilities = view.edit_capabilities(cx);
        assert!(capabilities.paste && capabilities.paste_markdown);
        assert!(!capabilities.paste_plain);
    });
}

#[gpui::test]
fn context_paste_is_its_own_undo_step_inside_explicit_typing_group(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup(""), cx));
    view.update(cx, |view, cx| {
        view.begin_undo_group();
        view.run_command(&commands::insert_text("before"), cx);
        cx.write_to_clipboard(ClipboardItem::new_string("paste".into()));
        view.execute_context_action(ContextAction::PastePlain, cx);
        view.run_command(&commands::insert_text("after"), cx);
        view.end_undo_group();
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "beforepaste");
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "before");
    });
}

#[gpui::test]
fn source_preserving_services_support_rich_multiline_replacement_and_atomic_undo(
    cx: &mut TestAppContext,
) {
    let source = "**hello** _world_\n\nSecond paragraph.";
    let view =
        cx.new(|cx| EditorView::new(setup(source).kind(std::sync::Arc::new(ReadingKind)), cx));
    view.update(cx, |view, cx| {
        let before = view.state.doc().clone();
        let request = view.context_document_snapshot();
        assert!(view.context_text(&request).unwrap().replaceable);
        let result = "greetings world\nNew 😀 paragraph\nSecond paragraph.";
        assert!(view.replace_context_text(&request, result, cx));
        assert_eq!(
            view.context_text(&view.context_document_snapshot())
                .unwrap()
                .text,
            result
        );
        assert!(!view.replace_context_text(&request, "stale", cx));
        assert_eq!(history::undo_depth(&view.state), 1);
        view.dispatch([history::undo(&view.state).unwrap()], cx);
        assert_eq!(*view.state.doc(), before);
        let request = view.context_document_snapshot();
        view.document_guard = Some(Box::new(|_| {
            Err(crate::EditRejection::Protected("Locked".into()))
        }));
        assert!(!view.replace_context_text(&request, result, cx));
        assert_eq!(*view.state.doc(), before);
    });
}

#[gpui::test]
fn source_preserving_services_preserve_unselected_atoms_for_selection_and_caret(
    cx: &mut TestAppContext,
) {
    let source = "hello ![image](x.png)";
    for (from, to, replacement, expected) in [
        (1, 6, "hi", "hi ![image](x.png)"),
        (1, 1, "before ", "before hello ![image](x.png)"),
        (6, 6, " after", "hello after ![image](x.png)"),
    ] {
        let view =
            cx.new(|cx| EditorView::new(setup(source).kind(std::sync::Arc::new(ReadingKind)), cx));
        view.update(cx, |view, cx| {
            view.select_range(from, to, cx);
            let request = view.context_snapshot(Default::default(), ContextTarget::Text);
            assert!(view.context_text(&request).unwrap().replaceable);
            assert!(view.replace_context_text(&request, replacement, cx));
            let codecs = markraft_commonmark::CommonMarkCodecs::new(
                view.state.schema().clone(),
                Default::default(),
            );
            let written = markraft_core::kind::Codecs::to_markup(
                &codecs,
                &markraft_core::Slice::from_fragment(view.state.doc().content().clone()),
            )
            .unwrap();
            assert_eq!(written, expected);
            let request = view.context_document_snapshot();
            assert!(!view.context_text(&request).unwrap().replaceable);
            assert!(!view.replace_context_text(&request, "cannot drop image", cx));
        });
    }
}

#[gpui::test]
fn plain_service_policy_applies_same_text_style_changes_with_guarded_atomic_undo(
    cx: &mut TestAppContext,
) {
    use markraft_core::kind::ReadingReplacementPolicy::InheritSelectionStart;
    let view = cx.new(|cx| {
        EditorView::new(
            setup("**red** _blue_").kind(std::sync::Arc::new(ReadingKind)),
            cx,
        )
    });
    view.update(cx, |view, cx| {
        let before = view.state.doc().clone();
        let request = view.context_document_snapshot();
        assert!(view.replace_context_text_with_policy(
            &request,
            "red blue",
            InheritSelectionStart,
            cx
        ));
        assert_eq!(view.projection().plain_text(), "**red blue**");
        assert_eq!(history::undo_depth(&view.state), 1);
        assert!(!view.replace_context_text_with_policy(
            &request,
            "stale",
            InheritSelectionStart,
            cx
        ));
        view.dispatch([history::undo(&view.state).unwrap()], cx);
        assert_eq!(*view.state.doc(), before);
        let request = view.context_document_snapshot();
        view.document_guard = Some(Box::new(|_| {
            Err(crate::EditRejection::Protected("Locked".into()))
        }));
        assert!(!view.replace_context_text_with_policy(
            &request,
            "red blue",
            InheritSelectionStart,
            cx
        ));
        assert_eq!(*view.state.doc(), before);
    });
}

#[gpui::test]
fn source_preserving_services_keep_the_existing_verbatim_replacement_path(cx: &mut TestAppContext) {
    let view = cx.new(|cx| {
        EditorView::new(
            setup("```\nfirst line\nsecond line\n```").kind(std::sync::Arc::new(ReadingKind)),
            cx,
        )
    });
    view.update(cx, |view, cx| {
        let request = view.context_document_snapshot();
        assert!(view.context_text(&request).unwrap().replaceable);
        assert!(view.replace_context_text_with_policy(
            &request,
            "literal **code**\nwith 😀",
            markraft_core::kind::ReadingReplacementPolicy::InheritSelectionStart,
            cx,
        ));
        assert_eq!(
            view.context_text(&view.context_document_snapshot())
                .unwrap()
                .text,
            "literal **code**\nwith 😀"
        );
    });
}

#[gpui::test]
fn source_preserving_service_capability_rejects_protected_code_and_composition(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| {
        EditorView::new(
            setup("plain `code` text").kind(std::sync::Arc::new(ReadingKind)),
            cx,
        )
    });
    view.update(cx, |view, cx| {
        let request = view.context_document_snapshot();
        assert!(!view.context_text(&request).unwrap().replaceable);
        assert!(!view.replace_context_text(&request, "replacement", cx));
        view.select_range(1, 6, cx);
        let request = view.context_snapshot(Default::default(), ContextTarget::Text);
        assert!(view.context_text(&request).unwrap().replaceable);
        assert!(view.replace_context_text(&request, "updated", cx));
        assert!(view.projection().plain_text().contains("`code`"));
        view.select(1, false, cx);
        let request = view.context_document_snapshot();
        view.dispatch(
            [composition::update_composition(&view.state, "中", 1).unwrap()],
            cx,
        );
        assert!(!view.replace_context_text(&request, "blocked", cx));
    });
}

#[gpui::test]
fn text_service_reading_hides_markdown_and_maps_document_subranges(cx: &mut TestAppContext) {
    let source = "A **bold** [label](https://example.com)\n\n中文 café";
    let view = cx.new(|cx| EditorView::new(setup(source), cx));
    view.update(cx, |view, cx| {
        let request = view.context_document_snapshot();
        let text = view.context_text(&request).unwrap();
        assert_eq!(text.text, "A bold label\n中文 café");
        assert!(!text.replaceable);
        assert!(text.transformable);
        let label = view.context_text_range(&request, 7..12).unwrap();
        assert_eq!(view.context_text(&label).unwrap().text, "label");
        assert!(view.replace_context_text(&label, "title", cx));
        assert!(
            view.projection()
                .plain_text()
                .contains("[title](https://example.com)")
        );
        assert!(!view.replace_context_text(&label, "stale", cx));
    });
}

#[gpui::test]
fn text_service_reading_preserves_inline_breaks_before_styles(cx: &mut TestAppContext) {
    for source in ["one\n**two**", "one  \ntwo", "one\r\ntwo"] {
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.update(cx, |view, _| {
            let request = view.context_document_snapshot();
            let mapped = view.context_text_mapping(&request).unwrap();
            assert_eq!(mapped.text, "one\ntwo", "{source:?}");
            let newline = source.replace("\r\n", "\n").find('\n').unwrap() + 1;
            assert_eq!(mapped.source[3], Some(newline..newline + 1));
            let word = view.context_text_range(&request, 4..7).unwrap();
            assert_eq!(view.context_text(&word).unwrap().text, "two");
        });
    }
}

#[gpui::test]
fn text_service_replacement_preserves_formatting_and_has_its_own_undo_step(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
    view.update(cx, |view, cx| {
        view.begin_undo_group();
        view.select_range(3, 7, cx);
        let request = view.context_snapshot(Point::default(), ContextTarget::Text);
        assert!(view.context_text(&request).unwrap().replaceable);
        assert!(!view.replace_context_text(&request, "broken\nformatting", cx));
        assert!(view.context_is_current(&request));
        assert!(view.replace_context_text(&request, "replacement", cx));
        assert_eq!(view.projection().plain_text(), "**replacement**");
        view.select(view.state.doc().content_size() - 1, false, cx);
        view.run_command(&commands::insert_text(" later"), cx);
        view.end_undo_group();
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "**replacement**");
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.projection().plain_text(), "**word**");
    });
}

#[gpui::test]
fn text_service_transform_is_atomic_and_preserves_source_syntax(cx: &mut TestAppContext) {
    let source = "**straße** [mixed CASE](https://example.com/KeepCase)\n\nélÈVE ΟΣ";
    let view = cx.new(|cx| EditorView::new(setup(source), cx));
    view.update(cx, |view, cx| {
        let before = view.state.doc().clone();
        let request = view.context_document_snapshot();
        assert!(view.transform_context_text(&request, TextTransformation::Uppercase, cx));
        let text = view.projection().plain_text().to_owned();
        assert!(
            text.contains("**STRASSE** [MIXED CASE](https://example.com/KeepCase)"),
            "{text}"
        );
        assert!(text.contains("ÉLÈVE ΟΣ"));
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.state.doc(), &before);
        let request = view.context_document_snapshot();
        assert!(view.transform_context_text(&request, TextTransformation::Capitalize, cx));
        let text = view.projection().plain_text().to_owned();
        assert!(
            text.contains("**Straße** [Mixed Case](https://example.com/KeepCase)"),
            "{text}"
        );
        assert!(text.contains("Élève Ος"), "{text}");
    });
}

#[gpui::test]
fn text_service_refuses_composite_grapheme_and_guarded_replacement(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup("a **b** [c](https://example.com)"), cx));
    view.update(cx, |view, cx| {
        let before = view.state.doc().clone();
        let request = view.context_document_snapshot();
        assert!(!view.replace_context_text(&request, "replacement", cx));
        view.document_guard = Some(Box::new(|_| {
            Err(crate::EditRejection::Protected("Read only".into()))
        }));
        assert!(!view.transform_context_text(&request, TextTransformation::Uppercase, cx));
        assert_eq!(view.state.doc(), &before);
        assert!(view.context_is_current(&request));
        assert_eq!(history::undo_depth(&view.state), 0);
    });
    let view = cx.new(|cx| EditorView::new(setup("e\u{301} &amp; ![picture](image.png)"), cx));
    view.update(cx, |view, cx| {
        let request = view.context_document_snapshot();
        let text = view.context_text(&request).unwrap();
        assert_eq!(text.text, "e\u{301} & picture");
        assert!(!text.transformable);
        assert!(view.context_text_range(&request, 0..1).is_none());
        assert!(view.context_text_range(&request, 3..4).is_none());
        assert!(view.context_text_range(&request, 5..12).is_none());
        let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
        view.dispatch([candidate], cx);
        assert!(
            view.context_text(&view.context_document_snapshot())
                .is_none()
        );
    });
}
#[gpui::test]
fn text_service_rewrites_plain_paragraphs_with_literal_newlines(cx: &mut TestAppContext) {
    let view = cx.new(|cx| EditorView::new(setup("first\n\nsecond"), cx));
    view.update(cx, |view, cx| {
        view.codecs = Some(std::sync::Arc::new(
            markraft_commonmark::CommonMarkCodecs::new(
                view.state.schema().clone(),
                markraft_commonmark::HouseStyleHandle::default(),
            ),
        ));
        let before = view.state.doc().clone();
        let request = view.context_document_snapshot();
        assert!(view.context_text(&request).unwrap().replaceable);
        assert!(view.replace_context_text(&request, "*literal*\n# text", cx));
        assert_eq!(
            view.context_text(&view.context_document_snapshot())
                .unwrap()
                .text,
            "*literal*\n# text"
        );
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(view.state.doc(), &before);
    });
}

#[gpui::test]
fn text_diagnostics_survive_selection_and_follow_untouched_text_through_edits(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| EditorView::new(setup("hello world"), cx));
    view.update(cx, |view, cx| {
        let request = view.context_document_snapshot();
        view.set_text_diagnostics(vec![7..12, 1..6, 7..12, 0..999, 4..4], cx);
        assert_eq!(view.text_diagnostics, vec![1..6, 7..12]);
        assert_eq!(history::undo_depth(&view.state), 0);
        view.select(3, false, cx);
        assert!(view.context_document_is_current(&request));
        assert!(!view.context_is_current(&request));
        assert_eq!(view.text_diagnostics, vec![1..6, 7..12]);
        view.run_command(&commands::insert_text("x"), cx);
        assert!(!view.context_document_is_current(&request));
        assert_eq!(view.text_diagnostics, vec![8..13]);
        view.set_text_diagnostics(std::iter::once(1..4).collect(), cx);
        view.replace_doc(view.state.doc().clone(), cx);
        assert!(view.text_diagnostics.is_empty());
    });
}
#[gpui::test]
fn prose_services_skip_code_math_and_leave_url_policy_to_host(cx: &mut TestAppContext) {
    for (source, word, expected) in [
        ("plain words", "plain", true),
        ("`misspelled`", "misspelled", false),
        ("```text\nmisspelled\n```", "misspelled", false),
        ("$misspelled$", "misspelled", false),
        ("https://example.com", "example", true),
        ("[misspelled](https://example.com)", "misspelled", true),
    ] {
        let view = cx.new(|cx| EditorView::new(setup(source), cx));
        view.read_with(cx, |view, _| {
            let whole = view.context_document_snapshot();
            let text = view.context_text(&whole).unwrap().text;
            let start = text[..text.find(word).unwrap()].chars().count();
            let request = view
                .context_text_range(&whole, start..start + word.chars().count())
                .unwrap();
            assert_eq!(view.context_is_prose(&request), expected, "{source}");
        });
    }
}

#[gpui::test]
fn committed_typing_event_excludes_rewrites_paste_undo_and_uncommitted_ime(
    cx: &mut TestAppContext,
) {
    let view = cx.new(|cx| EditorView::new(setup(""), cx));
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event, _| {
            if let EditorEvent::Changed { text_input, .. } = event {
                events.borrow_mut().push(*text_input);
            }
        })
        .detach()
    });
    view.update(cx, |view, cx| {
        view.run_command(&commands::insert_text("typed"), cx);
        let request = view.context_document_snapshot();
        view.replace_context_text(&request, "replaced", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("paste".into()));
        view.execute_context_action(ContextAction::PastePlain, cx);
        let undo = history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        let candidate = composition::update_composition(&view.state, "中", 1).unwrap();
        view.dispatch([candidate], cx);
        view.dispatch([composition::finish_composition()], cx);
    });
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), vec![true, false, false, false, false, true]);
}
#[gpui::test]
fn paste_match_style_keeps_destination_format_without_duplicate_source(cx: &mut TestAppContext) {
    for (from, to, expected) in [(1, 9, "**new**"), (5, 5, "**wonewrd**")] {
        let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
        view.update(cx, |view, cx| {
            view.codecs = Some(std::sync::Arc::new(
                markraft_commonmark::CommonMarkCodecs::new(
                    view.state.schema().clone(),
                    Default::default(),
                ),
            ));
            view.select_range(from, to, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
            view.execute_context_action(ContextAction::PasteMatchStyle, cx);
            assert_eq!(view.projection().plain_text(), expected);
            let undo = history::undo(&view.state).unwrap();
            view.dispatch([undo], cx);
            assert_eq!(view.projection().plain_text(), "**word**");
        });
    }
    let view = cx.new(|cx| EditorView::new(setup("**word**"), cx));
    view.update(cx, |view, cx| {
        view.codecs = Some(std::sync::Arc::new(
            markraft_commonmark::CommonMarkCodecs::new(
                view.state.schema().clone(),
                Default::default(),
            ),
        ));
        view.select_range(1, 9, cx);
        cx.write_to_clipboard(ClipboardItem::new_string("*literal*\nnext".into()));
        view.execute_context_action(ContextAction::PasteMatchStyle, cx);
        let text = view
            .context_text(&view.context_document_snapshot())
            .unwrap()
            .text;
        assert_eq!(text, "*literal*\nnext");
        let strong = view.types.strong.unwrap();
        for (index, line) in view.analysis.projection().lines().iter().enumerate() {
            for piece in markraft_core::kind::reading::line_pieces(
                view.analysis.projection(),
                &view.types,
                index,
                &markraft_core::kind::conceal::Reveal::nothing(),
            ) {
                let position = line.offset_to_pos(piece.source.start).unwrap();
                assert!(
                    view.state
                        .doc()
                        .node_at(position)
                        .unwrap()
                        .marks()
                        .contains_type(strong)
                );
            }
        }
    });
}
