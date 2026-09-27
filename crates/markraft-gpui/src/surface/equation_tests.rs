use super::{LayoutLine, MathContent, ShapeInput, shape};
use crate::style::EditorStyle;
use crate::typeahead::tests::state_of;
use gpui::{NoopTextSystem, TextSystem};
use gpui::{WindowTextSystem, point, px};
use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema};
use markraft_core::kind::DocTypes;
use markraft_core::kind::equations::EquationIndex;
use markraft_core::projection::projection_of;
use std::{ops::Range, sync::Arc};

fn shaped_equations(
    source: &str,
    auto_number: bool,
    selection: Range<usize>,
    width: f32,
) -> Vec<LayoutLine> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let types = DocTypes::from_schema_names(&commonmark_schema(), &commonmark_doc_type_names());
    let equations = EquationIndex::build(&projection, &types, auto_number);
    let style = EditorStyle::notes();
    let maths = crate::maths::Maths::default();
    for (index, line) in projection.lines().iter().enumerate() {
        for span in
            crate::math_spans::formula_spans(line, projection.line_text(index).unwrap(), &types)
        {
            let equation = equations.get(index, span.source.start).unwrap();
            for (source, display) in
                std::iter::once((equation.render_source.as_str(), span.display))
                    .chain(equation.tag.as_deref().map(|tag| (tag, false)))
            {
                let request = crate::math::MathRequest::new(
                    source,
                    display,
                    style.font_size(types.heading_level(line), false).into(),
                    1.,
                    if equation.target.is_some() {
                        style.link
                    } else {
                        style.text
                    },
                );
                maths.finish(vec![(request.clone(), crate::math::render_math(&request))]);
            }
        }
    }
    let images = crate::images::Images::default();
    let input = ShapeInput {
        images: &images,
        maths: Some(&maths),
        equations: Some(&equations),
        scale_factor: 1.,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        wiki: None,
        spelling: None,
        selection,
        composition: None,
    };
    let text_system =
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))));
    shape(&input, px(width), &text_system)
}

#[test]
fn automatic_number_has_separate_right_aligned_bounds() {
    let rows = shaped_equations("$$\nx=1\n$$", true, 0..0, 400.);
    let row = &rows[0];
    assert_eq!(row.formulas.len(), 2);
    let body = row.formulas[0].bounds;
    let tag = row.formulas[1].bounds;
    assert_eq!(tag.right(), row.width);
    assert!(body.right() < tag.left());
    assert!((body.center().x - row.width / 2.).abs() < px(0.01));
    assert!(tag.bottom() <= row.text_height());
}

#[test]
fn narrow_formula_stacks_number_without_overlap_or_font_shrink() {
    let source = "$$\nabcdefghijk=123456789\n$$";
    let wide = shaped_equations(source, true, 0..0, 600.);
    let narrow = shaped_equations(source, true, 0..0, 130.);
    let row = &narrow[0];
    let body = row.formulas[0].bounds;
    let tag = row.formulas[1].bounds;
    assert!(tag.top() >= body.bottom() + px(7.9));
    assert_eq!(tag.size, wide[0].formulas[1].bounds.size);
    assert!(tag.bottom() <= row.text_height());
}

#[test]
fn edited_formula_preview_retains_its_number() {
    let rows = shaped_equations("$$\nx=1\n$$", true, 5..5, 400.);
    let row = &rows[0];
    assert_eq!(row.formulas.len(), 2);
    assert!(row.formulas[0].bounds.top() >= row.text_height());
    assert!(row.formulas[1].bounds.bottom() <= row.height);
}

#[test]
fn manual_tag_is_visible_when_auto_numbering_is_disabled() {
    let rows = shaped_equations("$$\nx=1 \\tag{A}\n$$", false, 0..0, 400.);
    assert_eq!(rows[0].formulas.len(), 2);
    assert!(
        rows[0]
            .formulas
            .iter()
            .all(|formula| matches!(formula.content, MathContent::Formula(_)))
    );
}

#[test]
fn pure_reference_uses_painted_bounds_for_navigation() {
    let rows = shaped_equations(
        "See $\\eqref{answer}$.\n\n$$\nx=1 \\label{answer}\n$$",
        true,
        0..0,
        400.,
    );
    let reference = &rows[0].formulas[0];
    let target = reference.target.expect("resolved reference target");
    assert!(rows[1].contains(target));
    assert_eq!(
        rows[0].equation_target_at(rows[0].origin + reference.bounds.center()),
        Some(target)
    );
    assert_eq!(
        rows[0].equation_target_at(rows[0].origin + point(px(-1.), px(-1.))),
        None
    );
    assert!(
        rows[1]
            .formulas
            .iter()
            .all(|formula| formula.target.is_none())
    );
}

#[test]
fn unresolved_reference_keeps_a_visible_diagnostic() {
    let rows = shaped_equations("See $\\eqref{missing}$.", true, 0..0, 400.);
    assert!(
        rows[0]
            .formulas
            .iter()
            .any(|formula| matches!(formula.content, MathContent::Error(_)))
    );
    assert!(
        rows[0]
            .formulas
            .iter()
            .all(|formula| formula.target.is_none())
    );
}

#[test]
fn tall_manual_tag_fits_the_reserved_visual_row() {
    let rows = shaped_equations("$$\nx=1 \\tag{\\frac{a}{b}}\n$$", false, 0..0, 400.);
    let row = &rows[0];
    assert_eq!(row.formulas.len(), 2);
    for decoration in &row.formulas {
        assert!(decoration.bounds.top() >= px(0.));
        assert!(decoration.bounds.bottom() <= row.text_height());
    }
}

#[test]
fn oversized_tag_stays_within_a_narrow_column() {
    let rows = shaped_equations(
        "$$\nx=1 \\tag{123456789012345678901234567890}\n$$",
        false,
        0..0,
        90.,
    );
    let row = &rows[0];
    assert_eq!(row.formulas.len(), 2);
    let body = row.formulas[0].bounds;
    let tag = row.formulas[1].bounds;
    assert!(tag.left() >= px(0.));
    assert!(tag.right() <= row.width);
    assert!(tag.top() > body.bottom());
    assert!(tag.bottom() <= row.text_height() + px(0.001));
}
