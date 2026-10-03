//! Pointer-word behavior through the shared shaped selection geometry.
use crate::{EditorStyle, context, surface};
use gpui::{NoopTextSystem, TextSystem, WindowTextSystem, px};
use markraft_core::{
    EditorState,
    kind::{DocTypes, SourceSpelling},
    projection::projection_of,
};
use std::sync::Arc;

pub(crate) fn verify(
    state: &EditorState,
    types: &DocTypes,
    style: &EditorStyle,
    spelling: &dyn SourceSpelling,
    targets: &[(&str, &str)],
) {
    let projection = projection_of(state);
    let images = crate::images::Images::default();
    let text_system =
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))));
    for revealed in [false, true] {
        let input = surface::ShapeInput {
            messages: &crate::EditorMessages::ENGLISH,
            images: &images,
            maths: None,
            equations: None,
            scale_factor: 1.,
            doc: state.doc(),
            types,
            projection: &projection,
            style,
            single_line: false,
            wiki: None,
            spelling: Some(spelling),
            selection: if revealed {
                projection.lines()[0].from()..projection.lines()[0].to()
            } else {
                usize::MAX..usize::MAX
            },
            composition: None,
        };
        let mut lines = surface::Lines::default();
        lines.sync(&input, &projection, px(1600.), 0);
        lines.lay_out_range(&input, 0..projection.line_count(), &text_system);
        lines.set_shown(std::iter::once(0..projection.line_count()).collect());
        let rows: Vec<_> = lines.shown().into_iter().map(|(_, row)| row).collect();
        let row = &rows[0];
        let text = projection.line_text(0).unwrap();
        for &(target, expected) in targets {
            let start = text[..text.find(target).unwrap()].chars().count();
            let end = start + target.chars().count();
            let rects = row.rectangles(start..end, false);
            assert!(
                !rects.is_empty(),
                "{target} must have visible selection geometry"
            );
            for rect in rects {
                for fraction in [0.25, 0.75] {
                    let mut point = rect.center();
                    point.x = rect.left() + rect.size.width * fraction;
                    let insertion = row.char_at(point - row.origin);
                    let character = context::pointer_character_in_row(
                        row,
                        &projection,
                        point,
                        insertion,
                    )
                    .unwrap_or_else(|| {
                        panic!(
                            "no character for {target}, revealed={revealed}, fraction={fraction}"
                        )
                    });
                    let reveal = markraft_core::kind::conceal::Reveal::at(
                        input.selection.clone(),
                        input.composition.clone(),
                    );
                    let pieces = crate::shown::line_pieces(&projection, types, row.index, &reveal);
                    let word = context::word_at(
                        &projection,
                        character,
                        &pieces,
                        crate::word_boundary::Intent::Pointer,
                    )
                    .unwrap();
                    assert_eq!(
                        projection.text_between(word.start, word.end),
                        Some(expected),
                        "target={target}, revealed={revealed}, fraction={fraction}"
                    );
                    assert!(
                        projection
                            .graphemes(0)
                            .any(|(position, _)| position == character)
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod unit {
    use super::verify;
    use crate::EditorStyle;
    use markraft_commonmark::{
        CommonMarkSpelling, commonmark_doc_type_names, commonmark_extensions, commonmark_schema,
        from_markdown,
    };
    use markraft_core::{EditorState, EditorStateConfig, Extension, kind::DocTypes};

    #[test]
    fn non_native_pointer_words_respect_both_halves_graphemes_and_markup() {
        for (source, targets) in [
            ("a <u>x</u> b", vec![("x", "x")]),
            ("a **word** b", vec![("w", "word"), ("d", "word")]),
            (
                "left (naïve) e\u{301} 👩🏽‍💻 אבג right",
                vec![
                    ("(", "("),
                    ("ï", "naïve"),
                    ("e\u{301}", "e\u{301}"),
                    ("👩🏽‍💻", "👩🏽‍💻"),
                    ("ג", "אבג"),
                ],
            ),
        ] {
            let schema = commonmark_schema();
            let state = EditorState::create(
                EditorStateConfig::new(schema.clone())
                    .doc(from_markdown(&schema, source).unwrap())
                    .extensions(Extension::all([
                        markraft_core::projection::projection(),
                        commonmark_extensions(&schema),
                    ])),
            )
            .unwrap();
            let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
            verify(
                &state,
                &types,
                &EditorStyle::default(),
                &CommonMarkSpelling::new(schema),
                &targets,
            );
        }
    }
    #[gpui::test]
    fn double_click_uses_the_drawn_word_before_caret_reveals_markup(cx: &mut gpui::TestAppContext) {
        use crate::{EditorView, Setup};
        use gpui::{MouseButton, MouseDownEvent};
        for (source, target, expected) in [
            ("a <u>x</u> b\n\nother", "x", "x"),
            ("a **word** b\n\nother", "w", "word"),
            ("(naïve) e\u{301} 👩🏽‍💻\n\nother", "e\u{301}", "e\u{301}"),
            ("one   two\n\nother", " ", "   "),
            ("one...two\n\nother", ".", "."),
            #[cfg(target_os = "macos")]
            ("中文编辑器测试\n\nother", "编", "编辑器"),
            #[cfg(target_os = "macos")]
            ("中文编**辑**器测试\n\nother", "编", "编**辑**器"),
            #[cfg(target_os = "macos")]
            ("宇**宙**洪荒\n\nother", "宇", "宇**宙"),
            ("a&#98;c tail\n\nother", "a", "a&#98;c"),
        ] {
            for (fraction, sequence) in [(0.25, false), (0.75, false), (0.25, true), (0.75, true)] {
                let (view, cx) = cx.add_window_view(|_, cx| {
                    let schema = commonmark_schema();
                    let setup = Setup::new(schema.clone())
                        .types(DocTypes::from_schema_names(
                            &schema,
                            &commonmark_doc_type_names(),
                        ))
                        .extensions(commonmark_extensions(&schema))
                        .doc(from_markdown(&schema, source).unwrap());
                    let mut view = EditorView::new(setup, cx);
                    view.spelling = Some(std::sync::Arc::new(CommonMarkSpelling::new(schema)));
                    view.select(view.state.doc().content().size() - 1, false, cx);
                    view
                });
                cx.run_until_parked();
                let point = view.read_with(cx, |view, _| {
                    let projection = view.projection();
                    let text = projection.line_text(0).unwrap();
                    let start = text[..text.find(target).unwrap()].chars().count();
                    let rect = view.frame.rows()[0]
                        .rectangles(start..start + target.chars().count(), false)[0];
                    gpui::point(rect.left() + rect.size.width * fraction, rect.center().y)
                });
                if sequence {
                    cx.simulate_event(MouseDownEvent {
                        position: point,
                        button: MouseButton::Left,
                        modifiers: Default::default(),
                        click_count: 1,
                        first_mouse: false,
                    });
                    cx.simulate_mouse_up(point, MouseButton::Left, Default::default());
                    cx.run_until_parked();
                }
                cx.simulate_event(MouseDownEvent {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Default::default(),
                    click_count: 2,
                    first_mouse: false,
                });
                cx.run_until_parked();
                view.read_with(cx, |view, _| {
                    let selection = view.state.selection();
                    assert_eq!(
                        view.projection().text_between(
                            selection.from(view.state.doc()),
                            selection.to(view.state.doc())
                        ),
                        Some(expected)
                    );
                });
                if let Some(edited) = match source {
                    "宇**宙**洪荒\n\nother" => Some("X洪荒\nother"),
                    "a&#98;c tail\n\nother" => Some("X tail\nother"),
                    _ => None,
                } {
                    view.update(cx, |view, cx| {
                        let original = view.state.doc().clone();
                        let command = markraft_core::kind::chains::insert_plain(&view.types, "X");
                        assert!(view.run_command(&command, cx));
                        let reading = crate::shown::ShownText::build(
                            &view.projection(),
                            &view.types,
                            &markraft_core::kind::conceal::Reveal::nothing(),
                        );
                        assert_eq!(reading.text(), edited);
                        let undo = markraft_core::history::undo(&view.state).unwrap();
                        assert!(view.dispatch([undo], cx));
                        assert_eq!(view.state.doc(), &original);
                    });
                }
            }
        }
    }
    #[gpui::test]
    fn multi_click_target_expires_after_intervening_selection_edit_or_context_click(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::{EditorView, Setup};
        use gpui::{MouseButton, MouseDownEvent};
        for interruption in ["selection", "edit", "context"] {
            let (view, cx) = cx.add_window_view(|_, cx| {
                let schema = commonmark_schema();
                EditorView::new(
                    Setup::new(schema.clone())
                        .types(DocTypes::from_schema_names(
                            &schema,
                            &commonmark_doc_type_names(),
                        ))
                        .extensions(commonmark_extensions(&schema))
                        .doc(from_markdown(&schema, "first second third").unwrap()),
                    cx,
                )
            });
            cx.run_until_parked();
            let first = view.read_with(cx, |view, _| {
                view.frame.rows()[0].rectangles(1..2, false)[0].center()
            });
            cx.simulate_mouse_down(first, MouseButton::Left, Default::default());
            cx.simulate_mouse_up(first, MouseButton::Left, Default::default());
            cx.run_until_parked();
            view.update(cx, |view, cx| match interruption {
                "selection" => view.select(view.state.doc().content().size() - 1, false, cx),
                "edit" => {
                    view.run_command(&markraft_core::commands::insert_text("changed"), cx);
                }
                _ => {}
            });
            cx.run_until_parked();
            let second = view.read_with(cx, |view, _| {
                let projection = view.projection();
                let text = projection.line_text(0).unwrap();
                let offset = text[..text.find("second").unwrap()].chars().count() + 2;
                view.frame.rows()[0].rectangles(offset..offset + 1, false)[0].center()
            });
            if interruption == "context" {
                cx.simulate_mouse_down(second, MouseButton::Right, Default::default());
                cx.simulate_mouse_up(second, MouseButton::Right, Default::default());
                cx.run_until_parked();
            }
            cx.simulate_event(MouseDownEvent {
                position: second,
                button: MouseButton::Left,
                modifiers: Default::default(),
                click_count: 2,
                first_mouse: false,
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                let selection = view.state.selection();
                assert_eq!(
                    view.projection().text_between(
                        selection.from(view.state.doc()),
                        selection.to(view.state.doc())
                    ),
                    Some("second"),
                    "{interruption}"
                );
                assert!(
                    view.pointer_word_gesture.is_none(),
                    "second press consumes the target"
                );
            });
        }
    }
}
