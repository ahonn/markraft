//! Equation indexing follows the same edit/undo funnel as the document.

use crate::{EditorView, Setup};
use gpui::{AppContext, TestAppContext};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown, to_markdown,
};
use markraft_core::kind::{DocTypes, equations::Equation};
use markraft_core::{Change, Slice, TransactionSpec};

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

fn equation<'a>(view: &'a EditorView, needle: &str) -> &'a Equation {
    let index = view
        .projection
        .lines()
        .iter()
        .enumerate()
        .find(|(index, _)| view.projection.line_text(*index).unwrap().contains(needle))
        .map(|(index, _)| index)
        .unwrap();
    let span = crate::math_spans::formula_spans(
        &view.projection.lines()[index],
        view.projection.line_text(index).unwrap(),
        &view.types,
    )
    .remove(0);
    view.shaping
        .equations()
        .get(index, span.source.start)
        .unwrap()
}

#[gpui::test]
fn equation_index_tracks_insertions_undo_and_reference_targets(cx: &mut TestAppContext) {
    let source = "See $\\eqref{energy}$.\n\n$$\nE=mc^2\\label{energy}\n$$";
    let view = cx.new(|cx| EditorView::new(setup(source), cx));
    view.update(cx, |view, cx| {
        view.set_auto_number_equations(true, cx);
        assert_eq!(equation(view, "E=mc").tag.as_deref(), Some("(1)"));
        let target = equation(view, "See").target.unwrap();
        let prefix = from_markdown(view.state.schema(), "$$\nx=1\n$$").unwrap();
        let added = prefix.content_size();
        assert!(view.dispatch(
            [TransactionSpec::new().changes([Change::insert(
                0,
                Slice::from_fragment(prefix.content().clone())
            )])],
            cx
        ));
        assert_eq!(equation(view, "E=mc").tag.as_deref(), Some("(2)"));
        assert_eq!(equation(view, "See").target, Some(target + added));
        assert!(equation(view, "See").render_source.contains('2'));
        let undo = markraft_core::history::undo(&view.state).unwrap();
        view.dispatch([undo], cx);
        assert_eq!(equation(view, "E=mc").tag.as_deref(), Some("(1)"));
        assert_eq!(equation(view, "See").target, Some(target));
        assert_eq!(to_markdown(view.state.schema(), view.state.doc()), source);
    });
}

#[gpui::test]
fn numbering_preference_and_replacement_preserve_source_and_refresh_references(
    cx: &mut TestAppContext,
) {
    let source = "$$\nx=1\\label{first}\n$$\n\nSee $\\ref{first}$.";
    let view = cx.new(|cx| EditorView::new(setup(source), cx));
    view.update(cx, |view, cx| {
        assert!(!view.auto_number_equations());
        assert!(equation(view, "x=1").tag.is_none());
        view.set_auto_number_equations(true, cx);
        assert!(equation(view, "See").target.is_some());
        view.set_auto_number_equations(false, cx);
        assert!(equation(view, "x=1").tag.is_none());
        assert!(equation(view, "See").target.is_none());
        assert_eq!(to_markdown(view.state.schema(), view.state.doc()), source);
        let replacement = from_markdown(
            view.state.schema(),
            "$$\ny=2\\tag{A}\\label{second}\n$$\n\nSee $\\ref{second}$.",
        )
        .unwrap();
        view.replace_doc(replacement, cx);
        assert_eq!(equation(view, "y=2").tag.as_deref(), Some("(A)"));
        assert!(equation(view, "See").target.is_some());
    });
}

#[gpui::test]
fn command_click_on_rendered_equation_reference_navigates_without_editing(cx: &mut TestAppContext) {
    let source = "See $\\eqref{energy}$.\n\n$$\nE=mc^2\\label{energy}\n$$";
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = EditorView::new(setup(source), cx);
        view.set_auto_number_equations(true, cx);
        // Complete the actual render requests before the first frame, keeping
        // the event-path test independent of background task scheduling.
        let mut results = Vec::new();
        for (index, line) in view.projection.lines().iter().enumerate() {
            for span in crate::math_spans::formula_spans(
                line,
                view.projection.line_text(index).unwrap(),
                &view.types,
            ) {
                let equation = view
                    .shaping
                    .equations()
                    .get(index, span.source.start)
                    .unwrap();
                let style = view.shaping.style();
                for (tex, display, color) in std::iter::once((
                    equation.render_source.as_str(),
                    span.display,
                    if equation.target.is_some() {
                        style.link
                    } else {
                        style.text
                    },
                ))
                .chain(equation.tag.as_deref().map(|tag| (tag, false, style.text)))
                {
                    let request = crate::math::MathRequest::new(
                        tex,
                        display,
                        style
                            .font_size(view.types.heading_level(line), false)
                            .into(),
                        window.scale_factor(),
                        color,
                    );
                    let result = crate::math::render_math(&request);
                    assert!(result.is_ok(), "fixture formula must render: {tex}");
                    results.push((request, result));
                }
            }
        }
        view.shaping.finish_math(&view.types, results);
        view
    });
    cx.run_until_parked();
    let (point, target) = view.read_with(cx, |view, _| {
        let equation = equation(view, "See");
        let target = equation.target.unwrap();
        let row = view.frame.rows().iter().find(|row| row.index == 0).unwrap();
        let point = row.rectangles(equation.source.clone(), false)[0].center();
        assert_eq!(
            row.equation_target_at(point),
            Some(target),
            "click lands on the rendered reference"
        );
        assert_ne!(view.head(), target, "navigation must change the caret");
        (point, target)
    });
    cx.simulate_mouse_down(
        point,
        gpui::MouseButton::Left,
        gpui::Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    cx.simulate_mouse_up(
        point,
        gpui::MouseButton::Left,
        gpui::Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.head(), target);
        assert!(
            !view.selecting,
            "following a reference must not begin a drag selection"
        );
        assert_eq!(to_markdown(view.state.schema(), view.state.doc()), source);
    });
}
