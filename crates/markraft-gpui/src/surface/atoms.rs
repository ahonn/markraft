//! Display text and atoms: what a line shows in place of its source —
//! concealed markup, images, wiki links, pills — and where each atom sits.
//! Shaping asks it for the text to shape and the atoms to place.

use super::*;

/// Give every placeholder the slot the shaped rows put it in. A placeholder is
/// non-breaking, so it always lands on exactly one visual row.
pub(super) fn place_atoms(layout: &mut LayoutLine, pending: Vec<PendingAtom>) {
    for atom in pending {
        let Some(slot) = layout
            .display_rectangles(atom.chars.clone(), false)
            .into_iter()
            .next()
        else {
            continue;
        };
        layout.atoms.push(InlineAtom {
            left: slot.origin.x - layout.origin.x,
            slot: atom.width,
            visual_row: layout.visual_at(slot.origin.y - layout.origin.y),
            label: atom.label,
            image: atom.image,
            frame: atom.frame,
            note: atom.note,
        });
    }
}

/// A line's display text, and whether it stands in for content the caret cannot
/// enter.
pub(super) struct DisplayText {
    pub(super) text: String,
    /// True when the text is a stand-in — a placeholder space for an empty line
    /// — so no run may be derived from the content.
    pub(super) synthetic: bool,
    /// The display byte length of each of the line's projection runs, which is
    /// the run's own length except where an atom's placeholder widened it.
    pub(super) run_bytes: Vec<usize>,
    pub(super) widenings: Vec<Widening>,
    pub(super) atoms: Vec<PendingAtom>,
    pub(super) formulas: Vec<PendingFormula>,
    pub(super) math_previews: Vec<MathPreview>,
    pub(super) objects: Vec<InlineObject>,
    /// A formula's render was still outstanding, so its source stood in.
    pub(super) math_pending: bool,
}

impl DisplayText {
    /// A stand-in for content no run may be derived from.
    pub(super) fn stand_in(text: String) -> DisplayText {
        DisplayText {
            text,
            synthetic: true,
            run_bytes: Vec::new(),
            widenings: Vec::new(),
            atoms: Vec::new(),
            formulas: Vec::new(),
            math_previews: Vec::new(),
            objects: Vec::new(),
            math_pending: false,
        }
    }
}

pub(super) struct PendingFormula {
    pub(super) chars: Range<usize>,
    pub(super) rendered: crate::math::RenderedMath,
    pub(super) tag: Option<crate::math::RenderedMath>,
    pub(super) target: Option<usize>,
    metrics: FormulaMetrics,
}

pub(super) enum MathPreview {
    Formula {
        rendered: crate::math::RenderedMath,
        tag: Option<crate::math::RenderedMath>,
        target: Option<usize>,
        display: bool,
    },
    Error(Rc<ShapedLine>),
}

/// The same geometry reserves the object slot and positions its decorations.
/// Keeping the number separate preserves its font size when a formula shrinks.
#[derive(Clone, Copy, Debug)]
pub(super) struct FormulaMetrics {
    body: Bounds<Pixels>,
    tag: Option<Bounds<Pixels>>,
    ascent: Pixels,
    descent: Pixels,
    width: Pixels,
}

/// The space kept between a formula and a number beside it, on both sides so
/// the formula stays centered.
const TAG_GAP: Pixels = px(12.);

pub(super) fn formula_metrics(
    body: (f32, f32, f32),
    tag: Option<(f32, f32, f32)>,
    column: Pixels,
    centered: bool,
) -> FormulaMetrics {
    let column = column.max(px(1.));
    let (body_width, body_ascent, body_descent) = body;
    let scale = (f32::from(column) / body_width.max(1.)).min(1.);
    let width = px(body_width * scale);
    let height = px((body_ascent + body_descent) * scale);
    let mut metrics = FormulaMetrics {
        body: Bounds::new(
            point(
                if centered {
                    (column - width) / 2.
                } else {
                    px(0.)
                },
                px(0.),
            ),
            size(width, height),
        ),
        tag: None,
        ascent: px(body_ascent * scale),
        descent: px(body_descent * scale),
        width: if centered { column } else { width },
    };
    if let Some((tag_width, tag_ascent, tag_descent)) = tag {
        let tag_scale = (f32::from(column) / tag_width.max(1.)).min(1.);
        let tag_size = size(
            px(tag_width * tag_scale),
            px((tag_ascent + tag_descent) * tag_scale),
        );
        let fits = px(body_width) + (tag_size.width + TAG_GAP) * 2. <= column;
        let top = if fits {
            // Baseline alignment keeps a text tag natural beside a tall fraction.
            let ascent = metrics.ascent.max(px(tag_ascent * tag_scale));
            metrics.body.origin.y = ascent - metrics.ascent;
            metrics.ascent = ascent;
            metrics.descent = metrics.descent.max(px(tag_descent * tag_scale));
            ascent - px(tag_ascent * tag_scale)
        } else {
            let top = height + px(8.);
            metrics.descent += px(8.) + tag_size.height;
            top
        };
        metrics.tag = Some(Bounds::new(point(column - tag_size.width, top), tag_size));
        metrics.width = column;
    }
    metrics
}

fn rendered_metrics(
    body: &crate::math::RenderedMath,
    tag: Option<&crate::math::RenderedMath>,
    column: Pixels,
    centered: bool,
) -> FormulaMetrics {
    formula_metrics(
        (body.width, body.ascent, body.descent),
        tag.map(|tag| (tag.width, tag.ascent, tag.descent)),
        column,
        centered,
    )
}

/// The narrowest column that shows a formula at full size with its number
/// beside it, as [`formula_metrics`] places one.
fn natural_formula_width(
    body: &crate::math::RenderedMath,
    tag: Option<&crate::math::RenderedMath>,
) -> Pixels {
    px(body.width) + tag.map_or(px(0.), |tag| (px(tag.width) + TAG_GAP) * 2.)
}

fn push_formula(
    layout: &mut LayoutLine,
    rendered: crate::math::RenderedMath,
    tag: Option<crate::math::RenderedMath>,
    target: Option<usize>,
    metrics: FormulaMetrics,
    origin: Point<Pixels>,
    visual: Option<usize>,
) {
    layout.formulas.push(MathDecoration {
        bounds: Bounds::new(origin + metrics.body.origin, metrics.body.size),
        content: MathContent::Formula(rendered.image),
        target,
        visual,
    });
    if let Some((tag, bounds)) = tag.zip(metrics.tag) {
        layout.formulas.push(MathDecoration {
            bounds: Bounds::new(origin + bounds.origin, bounds.size),
            content: MathContent::Formula(tag.image),
            target: None,
            visual,
        });
    }
}

/// Place formulas at the text baseline, and keep a live result underneath
/// formulas whose source is being edited. Decorations never change source
/// coordinates or introduce additional caret stops.
pub(super) fn place_formulas(
    layout: &mut LayoutLine,
    pending: Vec<PendingFormula>,
    previews: Vec<MathPreview>,
    font_size: Pixels,
) {
    for formula in pending {
        let Some(slot) = layout
            .display_rectangles(formula.chars.clone(), false)
            .into_iter()
            .next()
        else {
            continue;
        };
        let visual = layout.visual_at(slot.origin.y - layout.origin.y);
        let top = layout.visual_baseline(visual) - formula.metrics.ascent;
        let origin = point(slot.origin.x - layout.origin.x, top);
        push_formula(
            layout,
            formula.rendered,
            formula.tag,
            formula.target,
            formula.metrics,
            origin,
            Some(visual),
        );
    }
    let mut top = layout.text_height()
        + layout
            .preview
            .as_ref()
            .map_or(px(0.), |(_, size)| PREVIEW_GAP + size.height);
    let before = top;
    for preview in previews {
        top += PREVIEW_GAP;
        match preview {
            MathPreview::Formula {
                rendered,
                tag,
                target,
                display,
            } => {
                let metrics = rendered_metrics(&rendered, tag.as_ref(), layout.width, display);
                push_formula(
                    layout,
                    rendered,
                    tag,
                    target,
                    metrics,
                    point(px(0.), top),
                    None,
                );
                top += metrics.ascent + metrics.descent;
            }
            MathPreview::Error(label) => {
                let height = font_size * 1.4;
                layout.formulas.push(MathDecoration {
                    bounds: Bounds::new(
                        point(px(0.), top),
                        size(label.width.min(layout.width), height),
                    ),
                    content: MathContent::Error(label),
                    target: None,
                    visual: None,
                });
                top += height;
            }
        }
    }
    layout.height += top - before;
}

/// A painted atom the display text has reserved room for, before shaping says
/// where on the block it landed.
pub(super) struct PendingAtom {
    /// `char` range of the placeholder within the display text.
    pub(super) chars: Range<usize>,
    pub(super) width: Pixels,
    pub(super) label: Rc<ShapedLine>,
    pub(super) image: Option<(Arc<crate::animation::Picture>, Size<Pixels>)>,
    pub(super) frame: Option<Size<Pixels>>,
    pub(super) note: bool,
}

#[cfg(test)]
pub(super) fn display_text(
    input: &ShapeInput<'_>,
    line: &Line,
    index: usize,
    font_size: Pixels,
    column: Pixels,
    text_system: &WindowTextSystem,
) -> DisplayText {
    display_text_mode(
        input,
        line,
        index,
        font_size,
        column,
        None,
        text_system,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn display_text_mode(
    input: &ShapeInput<'_>,
    line: &Line,
    index: usize,
    font_size: Pixels,
    column: Pixels,
    cell: Option<super::shape::CellWidth>,
    text_system: &WindowTextSystem,
    render_objects: bool,
) -> DisplayText {
    // A table row is as tall as its cells' text, so nothing may hang below it.
    let previews = !input.single_line && cell.is_none();
    let projection = input.projection;
    if line.kind() == LineKind::LeafBlock {
        return DisplayText::stand_in(" ".to_owned());
    }
    let source = projection.line_text(index).unwrap_or_default();
    if source.is_empty() {
        return DisplayText::stand_in(" ".to_owned());
    }
    // Pictures with a line to themselves — one, or several with only blanks
    // between them, as a row of screenshots is written — are drawn at their
    // size, side by side where they fit; one sharing its line with text has to
    // stay within a row.
    let alone = pictures_only(input, line);
    let reveal = reveal_of(input);
    let shown = markraft_core::kind::conceal::shown(input.types.syntax, line, &reveal);
    let math_spans = crate::math_spans::formula_spans(line, source, input.types);
    let equations: Vec<_> = math_spans
        .iter()
        .map(|span| {
            input
                .equations
                .and_then(|equations| equations.get(index, span.source.start))
        })
        .collect();
    let math_results: Vec<_> = math_spans
        .iter()
        .zip(&equations)
        .map(|(span, equation)| {
            let source = equation.map_or(span.tex.as_str(), |equation| {
                equation.render_source.as_str()
            });
            input
                .maths
                .filter(|_| !source.trim().is_empty())
                .and_then(|maths| {
                    maths.get(&crate::math::MathRequest::new(
                        source,
                        span.display,
                        font_size.into(),
                        input.scale_factor,
                        if equation.is_some_and(|equation| equation.target.is_some()) {
                            input.style.link
                        } else {
                            input.style.text
                        },
                    ))
                })
        })
        .collect();
    let math_pending = input.maths.is_some()
        && math_spans.iter().zip(&math_results).zip(&equations).any(
            |((span, result), equation)| {
                let source = equation.map_or(span.tex.as_str(), |equation| {
                    equation.render_source.as_str()
                });
                result.is_none() && !source.trim().is_empty()
            },
        );
    let tag_results: Vec<_> = equations
        .iter()
        .map(|equation| {
            equation
                .and_then(|equation| equation.tag.as_deref())
                .and_then(|tag| {
                    input.maths.and_then(|maths| {
                        maths.get(&crate::math::MathRequest::new(
                            tag,
                            false,
                            font_size.into(),
                            input.scale_factor,
                            input.style.text,
                        ))
                    })
                })
        })
        .collect();
    let mut text = String::with_capacity(source.len());
    let mut run_bytes = Vec::with_capacity(line.runs().len());
    let mut widenings = Vec::new();
    let mut atoms = Vec::new();
    let mut formulas = Vec::new();
    let mut math_previews = Vec::new();
    let mut byte = 0usize;
    let mut display = 0usize;
    let mut objects = Vec::new();
    for (index_in_line, run) in line.runs().iter().enumerate() {
        let chars = run.char_to - run.char_from;
        let len: usize = source[byte..].chars().take(chars).map(char::len_utf8).sum();
        let slice = &source[byte..byte + len];
        byte += len;
        if let Some((math_index, span)) = math_spans
            .iter()
            .enumerate()
            .find(|(_, span)| span.source.contains(&run.char_from))
            && input.maths.is_some()
        {
            let result = &math_results[math_index];
            let revealed = !render_objects || span.revealed(line, &reveal);
            if run.char_from == span.source.start {
                let equation = equations[math_index];
                let tag = tag_results[math_index]
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .cloned();
                let target = equation.and_then(|equation| equation.target);
                if previews {
                    if let Some(diagnostic) =
                        equation.and_then(|equation| equation.diagnostic.as_ref())
                    {
                        math_previews.push(MathPreview::Error(shape_source_label(
                            &input.messages.equation_diagnostic(diagnostic),
                            font(UI_FONT),
                            font_size * 0.85,
                            input.style.muted_text,
                            text_system,
                        )));
                    }
                    if let Some(Err(error)) = &tag_results[math_index] {
                        math_previews.push(MathPreview::Error(shape_source_label(
                            &input.messages.format(
                                crate::EditorMessage::EquationError,
                                &[("error", &error.localized(input.messages))],
                            ),
                            font(UI_FONT),
                            font_size * 0.85,
                            input.style.muted_text,
                            text_system,
                        )));
                    }
                }
                match result {
                    Some(Ok(rendered)) if !revealed => {
                        let centered = span.display && span.is_standalone(source);
                        let metrics = if cell == Some(super::shape::CellWidth::Natural) {
                            // Measuring a cell: ask for the room the formula
                            // and its number need, not the whole editor.
                            rendered_metrics(
                                rendered,
                                tag.as_ref(),
                                natural_formula_width(rendered, tag.as_ref()),
                                centered,
                            )
                        } else {
                            rendered_metrics(rendered, tag.as_ref(), column, centered)
                        };
                        let count = 1;
                        text.push(OBJECT);
                        run_bytes.push(OBJECT.len_utf8());
                        objects.push(InlineObject {
                            display: display..display + 1,
                            width: metrics.width,
                            alignment: InlineAlignment::Baseline {
                                ascent: metrics.ascent,
                                descent: metrics.descent,
                            },
                        });
                        widenings.push(Widening {
                            source: span.source.start,
                            source_len: span.source.len(),
                            display,
                            len: count,
                            shape: Some(AtomShape::Pill),
                            broken: false,
                        });
                        formulas.push(PendingFormula {
                            chars: display..display + count,
                            rendered: rendered.clone(),
                            tag,
                            target,
                            metrics,
                        });
                        display += count;
                        continue;
                    }
                    Some(Ok(rendered)) if previews => {
                        math_previews.push(MathPreview::Formula {
                            rendered: rendered.clone(),
                            tag,
                            target,
                            display: span.display,
                        });
                    }
                    Some(Err(error)) if previews => {
                        let message: String = input
                            .messages
                            .format(
                                crate::EditorMessage::FormulaError,
                                &[("error", &error.localized(input.messages))],
                            )
                            .chars()
                            .take(96)
                            .collect();
                        math_previews.push(MathPreview::Error(shape_source_label(
                            &message,
                            font(UI_FONT),
                            font_size * 0.85,
                            input.style.muted_text,
                            text_system,
                        )));
                    }
                    _ => {}
                }
            } else if matches!(result, Some(Ok(_))) && !revealed {
                run_bytes.push(0);
                continue;
            }
            // Pending and failed renders retain all source, including fences.
            text.push_str(slice);
            run_bytes.push(len);
            display += chars;
            continue;
        }
        match atom_of(input, line, run, font_size, column, alone, text_system) {
            Some(mut atom) => {
                if !render_objects && !atom.shape.is_own_text() {
                    let source = match &run.content {
                        RunContent::Atom(node) => {
                            input.spelling.and_then(|kind| kind.atom_source(node))
                        }
                        _ => None,
                    };
                    atom.shape = if source.is_some() {
                        AtomShape::Source
                    } else {
                        AtomShape::Text
                    };
                    atom.text = source.unwrap_or(atom.text);
                    if atom.text.is_empty() {
                        atom.text = input.messages.text(crate::EditorMessage::ImagePlaceholder);
                    }
                }
                // An atom the row can shape *is* its text: writing the label
                // into the display text gives it exactly the width its glyphs
                // advance, so what follows sits against it.
                let count = if atom.shape.is_own_text() && !atom.text.is_empty() {
                    text.push_str(&atom.text);
                    run_bytes.push(atom.text.len());
                    atom.text.chars().count()
                } else {
                    // One source atom occupies one display object. Its advance
                    // is an exact layout metric, independent of font whitespace.
                    let count = 1;
                    text.push(OBJECT);
                    run_bytes.push(OBJECT.len_utf8());
                    let height = atom
                        .image
                        .as_ref()
                        .map(|(_, size)| size.height)
                        .or(atom.frame.map(|size| size.height))
                        .unwrap_or(font_size * input.style.line_height_ratio);
                    objects.push(InlineObject {
                        display: display..display + 1,
                        width: atom.width.min(column),
                        alignment: InlineAlignment::Center { height },
                    });
                    atoms.push(PendingAtom {
                        width: atom.width.min(column),
                        chars: display..display + count,
                        note: atom.note,
                        label: atom.label,
                        image: atom.image,
                        frame: atom.frame,
                    });
                    count
                };
                widenings.push(Widening {
                    source: run.char_from,
                    source_len: 1,
                    display,
                    len: count,
                    shape: Some(atom.shape),
                    broken: atom.broken,
                });
                display += count;
            }
            None => match shown.get(index_in_line).copied().unwrap_or(Shown::Source) {
                Shown::Hidden => {
                    widenings.push(Widening {
                        source: run.char_from,
                        source_len: chars,
                        display,
                        len: 0,
                        shape: None,
                        broken: false,
                    });
                    run_bytes.push(0);
                }
                Shown::Display(shows) => {
                    let count = shows.chars().count();
                    text.push_str(shows);
                    run_bytes.push(shows.len());
                    widenings.push(Widening {
                        source: run.char_from,
                        source_len: chars,
                        display,
                        len: count,
                        shape: None,
                        broken: false,
                    });
                    display += count;
                }
                Shown::Source | Shown::Revealed => {
                    text.push_str(slice);
                    run_bytes.push(len);
                    display += chars;
                }
            },
        }
    }
    DisplayText {
        text,
        synthetic: false,
        run_bytes,
        widenings,
        atoms,
        formulas,
        math_previews,
        objects,
        math_pending,
    }
}

/// What reveals a concealed span while this input is shaped.
pub(super) fn reveal_of(input: &ShapeInput<'_>) -> Reveal {
    Reveal::at(input.selection.clone(), input.composition.clone())
}

/// Which concealed spans and atoms stand revealed, as the shaping cache's own
/// key.
///
/// Shaping is the view's largest cost and it now depends on where the caret
/// stands, but almost every caret move leaves every span exactly as it was.
/// Walking the runs is orders of magnitude cheaper than laying the text out
/// again, so the rows are kept until the *revealed set* changes rather than
/// until the selection does.
pub(crate) fn reveal_key(input: &ShapeInput<'_>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let reveal = reveal_of(input);
    // Only the lines the selection or the marked text stand on, and one either
    // side of them, can be touched; a note's other lines never need a look.
    let lines = input.projection.lines();
    let index = |pos: usize| input.projection.line_at(pos).unwrap_or(0);
    let ends = std::iter::once(&input.selection)
        .chain(input.composition.as_ref())
        .flat_map(|range| [index(range.start), index(range.end)]);
    let (first, last) = ends.fold((usize::MAX, 0), |(first, last), at| {
        (first.min(at), last.max(at))
    });
    let near = first.saturating_sub(1)..(last + 2).min(lines.len());
    for line in &lines[near] {
        // Only a line the selection or the marked text reaches can have
        // anything revealed on it.
        if reveal.touches(line.from(), line.to()) {
            let shown = markraft_core::kind::conceal::shown(input.types.syntax, line, &reveal);
            for (run, shown) in line.runs().iter().zip(shown) {
                let (from, to) = (line.abs(run.start), line.abs(run.end));
                if shown == Shown::Revealed {
                    from.hash(&mut hasher);
                }
                if matches!(run.content, RunContent::Atom(_)) && reveal.touches(from, to) {
                    from.hash(&mut hasher);
                    1u8.hash(&mut hasher);
                }
            }
        }
        // Any line that draws differently while it has the caret: a list
        // marker's spelling, a fence, a quote's marker. Asking
        // the kind only for the lines the caret actually touches keeps this to
        // a couple of calls per move.
        if affinity_touches(input, line.from(), line.to()) && focus_chrome(input, line) {
            line.from().hash(&mut hasher);
            2u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// An inline atom's content and the width it needs.
pub(super) struct Atom {
    pub(super) shape: AtomShape,
    /// The label as it reaches the screen, shortened where it would not fit the
    /// column. The row shapes this itself where the shape is its own text.
    pub(super) text: String,
    pub(super) label: Rc<ShapedLine>,
    pub(super) width: Pixels,
    pub(super) image: Option<(Arc<crate::animation::Picture>, Size<Pixels>)>,
    /// A picture still being fetched, drawn as a frame of this size.
    pub(super) frame: Option<Size<Pixels>>,
    /// A wiki link leading nowhere; see [`Widening::broken`].
    pub(super) broken: bool,
    /// An `![[…]]` whose target is a note in the host's index rather than a file
    /// beside it. The note is not unfolded here, so the pill stands for it.
    pub(super) note: bool,
}

/// A node's attribute, trimmed, or the empty string where it has none.
pub(super) fn attr<'a>(node: &'a Node, name: &str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
}

/// Resolve reading text independently from its screen shape.
pub(super) fn atom_label<'a>(types: &DocTypes, node: &'a Node) -> Option<(AtomShape, &'a str)> {
    use markraft_core::kind::reading::{self, AtomTextRole};
    reading::atom_label(types, node).map(|(role, label)| {
        let shape = match role {
            AtomTextRole::Placeholder => AtomShape::Pill,
            AtomTextRole::Literal => AtomShape::Text,
            AtomTextRole::Link => AtomShape::Link,
            AtomTextRole::Glyph => AtomShape::Glyph,
        };
        (shape, label)
    })
}

/// The file an atom draws a picture of, where it draws one. `![](path)` and
/// `![[path]]` are the same picture written two ways, so the view loads, measures and
/// draws them alike; only the syntax they were written in differs.
pub(super) fn picture_source<'a>(types: &DocTypes, node: &'a Node) -> Option<&'a str> {
    let ty = node.type_id();
    if Some(ty) == types.image {
        Some(attr(node, "src"))
    } else if Some(ty) == types.wiki_link && crate::wiki::wiki_link_embed(node) {
        Some(attr(node, "target"))
    } else {
        None
    }
}

/// Whether `line` holds pictures and nothing else but the blanks between them.
fn pictures_only(input: &ShapeInput<'_>, line: &Line) -> bool {
    let mut pictures = 0;
    for run in line.runs() {
        match &run.content {
            RunContent::Atom(node) if picture_source(input.types, node).is_some() => pictures += 1,
            RunContent::Text(text) if text.trim().is_empty() => {}
            _ => return false,
        }
    }
    pictures > 0
}

/// The atom an inline run is drawn as, shaped and measured.
pub(super) fn atom_of(
    input: &ShapeInput<'_>,
    line: &Line,
    run: &Run,
    font_size: Pixels,
    column: Pixels,
    alone: bool,
    text_system: &WindowTextSystem,
) -> Option<Atom> {
    let RunContent::Atom(node) = &run.content else {
        return None;
    };
    let ShapeInput {
        types,
        style,
        images,
        wiki,
        ..
    } = *input;
    let revealed = {
        let (from, to) = (line.abs(run.start), line.abs(run.end));
        let touches = |range: &Range<usize>| {
            if range.start == range.end {
                range.start >= from && range.start <= to
            } else {
                range.start < to && range.end > from
            }
        };
        touches(&input.selection) || input.composition.as_ref().is_some_and(touches)
    };
    // A `<br>` in a table cell is the cell's line break.
    if line
        .node_type()
        .is_some_and(|parent| types.is_cell_break(parent, node))
    {
        let label = Rc::new(text_system.shape_line("".into(), font_size, &[], None));
        return Some(Atom {
            shape: AtomShape::Break,
            text: "\n".to_owned(),
            label,
            width: px(0.),
            image: None,
            frame: None,
            broken: false,
            note: false,
        });
    }
    // Source the view shows as it is — inline HTML, an unknown shortcode — is
    // already the text a save writes, under the caret or not.
    let plain = atom_label(types, node).is_some_and(|(shape, _)| shape == AtomShape::Text);
    // An atom under the caret shows the source it was read from, which only the
    // host's kind can spell — it is the same text a save writes.
    let source = (revealed && !plain)
        .then(|| {
            input
                .spelling
                .and_then(|spelling| spelling.atom_source(node))
        })
        .flatten();
    let (shape, original) = match source {
        Some(source) => (AtomShape::Source, source),
        None => {
            let (shape, label) = atom_label(types, node)?;
            (shape, label.to_owned())
        }
    };
    // A picture stands for the atom; where the atom is showing its source there
    // is nothing to stand for.
    let picture = match shape {
        AtomShape::Source => None,
        _ => picture_source(types, node),
    };
    // `![[…]]` is written for a file, but a note answers to the same spelling and
    // the host's index is what knows which this is. Reporting a missing picture
    // for a note that is plainly there tells the reader something untrue.
    let embed = Some(node.type_id()) == types.wiki_link && crate::wiki::wiki_link_embed(node);
    let note = embed && wiki.is_some_and(|resolves| resolves(attr(node, "target")));
    // An embed is labelled with the name it was written with and nothing else, found
    // or not: `![[…]]` never said the target was a picture, so an embed that finds
    // nothing is not reported as a missing one.
    let text = match picture {
        Some(_) if note => original,
        Some(source) => match images.load(source) {
            Err(crate::images::ImageError::Missing) if embed => original,
            Err(error) => input.messages.format(
                crate::EditorMessage::ImageStatus,
                &[
                    ("status", &input.messages.text(error.message())),
                    ("name", &original),
                ],
            ),
            Ok(_) if !alone => input
                .messages
                .format(crate::EditorMessage::InlineImage, &[("name", &original)]),
            Ok(_) => original.to_owned(),
        },
        None => original,
    };
    // A picture the note can show is drawn for real: at its own size where it
    // has the line to itself, and as tall as the row where it shares the line
    // with text. The placeholder is still built, and stands in wherever it is
    // not drawn.
    let row_height = font_size * style.line_height_ratio;
    let drawn = (!note).then_some(picture).flatten().and_then(|source| {
        if alone {
            drawn_image(images, source, declared_size(node, column), column)
        } else {
            inline_image(images, source, row_height - INLINE_IMAGE_INSET * 2., column)
        }
    });
    // A picture still on its way keeps the room a picture takes, so the line does
    // not start as one row and jump when it arrives.
    let frame = (alone && !note && drawn.is_none())
        .then_some(picture)
        .flatten()
        .filter(|source| images.load(source) == Err(crate::images::ImageError::Loading))
        .map(|_| loading_frame(column));
    // A pill's label is smaller than the text around it, as inline code is;
    // source text and a wiki link's label sit in the sentence at the
    // sentence's own size, the one in the code font it is and the other in the
    // link colour, which is what says it can be followed.
    let (face, size) = match shape {
        AtomShape::Pill => (font(UI_FONT), font_size * PILL_SCALE),
        AtomShape::Source => (font(CODE_FONT), font_size),
        // A wiki link's label and source shown as text are the row's own text,
        // so they are measured in the face the row sets them in.
        AtomShape::Link | AtomShape::Text | AtomShape::Break => {
            (font(style.font_family.clone()), font_size)
        }
        AtomShape::Glyph => (font(UI_FONT), font_size),
    };
    // A link the host says it cannot open is still drawn as a link, because that is
    // what the source says it is — but not in the colour that invites a click, since
    // clicking it only reports that there is nothing there. A link's label is the
    // row's own text, so the colour is applied where the row is coloured; this only
    // decides it, while the node is at hand.
    let broken = shape == AtomShape::Link
        && wiki.is_some_and(|resolves| !resolves(&crate::wiki::wiki_link_target(node)));
    let ink = match shape {
        AtomShape::Link if broken => style.broken_link,
        AtomShape::Link => style.link,
        AtomShape::Glyph | AtomShape::Text | AtomShape::Break => style.text,
        AtomShape::Pill | AtomShape::Source => style.muted_text,
    };
    let room = (column * PILL_MAX_RATIO - shape.chrome()).max(px(16.));
    let shaped = |label: String| {
        Rc::new(text_system.shape_line(
            label.clone().into(),
            size,
            &[TextRun {
                len: label.len(),
                font: face.clone(),
                color: ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let mut graphemes = text.graphemes(true).collect::<Vec<_>>();
    let mut shown = graphemes.concat();
    let mut label = shaped(shown.clone());
    // A label has to stay within the column. A pill's placeholder is one
    // unbreakable run, so nothing downstream could shorten it; a label the row
    // shapes itself could wrap, but a target or a tag long enough to need it is
    // better read short than spread over three lines.
    // Source shown as text is prose: it wraps as prose does rather than being cut.
    while shape != AtomShape::Text && label.width > room && graphemes.len() > 1 {
        graphemes.pop();
        shown = format!("{}…", graphemes.concat());
        label = shaped(shown.clone());
    }
    Some(Atom {
        width: match drawn.as_ref().map(|(_, size)| *size).or(frame) {
            Some(size) => size.width,
            None => label.width + shape.chrome(),
        },
        shape,
        text: shown,
        label,
        image: drawn,
        frame,
        broken,
        note,
    })
}

/// The size an image node asks for, from an `<img>` tag's `width` and `height`:
/// plain numbers or pixels, or a share of `column` in percent, as a browser
/// reads them. Anything else asks for nothing.
pub(super) fn declared_size(node: &Node, column: Pixels) -> (Option<f32>, Option<f32>) {
    let read = |name: &str, of: Option<f32>| {
        let value = attr(node, name).trim();
        let size = match value.strip_suffix('%') {
            Some(share) => share
                .trim()
                .parse::<f32>()
                .ok()
                .zip(of)
                .map(|(share, of)| of * share / 100.),
            None => value
                .strip_suffix("px")
                .unwrap_or(value)
                .trim()
                .parse::<f32>()
                .ok(),
        };
        size.filter(|size| *size > 0.)
    };
    // A height in percent is of a box whose height is its content's, which
    // gives it nothing to be a share of.
    (read("width", Some(f32::from(column))), read("height", None))
}

/// A decoded image and the size it is drawn at: the size its tag asks for, or
/// its own, but never wider than the column. A tall picture is drawn tall rather
/// than shrunk into a thumbnail no one can read.
pub(super) fn drawn_image(
    images: &crate::images::Images,
    src: &str,
    declared: (Option<f32>, Option<f32>),
    column: Pixels,
) -> Option<(Arc<crate::animation::Picture>, Size<Pixels>)> {
    let image = images.load(src).ok()?;
    let intrinsic = image.size();
    let (native_width, native_height) = (intrinsic.width.0 as f32, intrinsic.height.0 as f32);
    if native_width <= 0. || native_height <= 0. {
        return None;
    }
    let ratio = native_height / native_width;
    // A size the tag gives on one side keeps the picture's proportions on the
    // other, as a browser draws it.
    let (width, height) = match declared {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, width * ratio),
        (None, Some(height)) => (height / ratio, height),
        (None, None) => (native_width, native_height),
    };
    let drawn = column.min(px(width)).max(px(1.));
    Some((image, size(drawn, drawn * (height / width))))
}

/// A decoded image fitted to a row: `height` tall, as wide as its proportions
/// make it, and no wider than the column.
pub(super) fn inline_image(
    images: &crate::images::Images,
    src: &str,
    height: Pixels,
    column: Pixels,
) -> Option<(Arc<crate::animation::Picture>, Size<Pixels>)> {
    let image = images.load(src).ok()?;
    let intrinsic = image.size();
    let (native_width, native_height) = (intrinsic.width.0 as f32, intrinsic.height.0 as f32);
    if native_width <= 0. || native_height <= 0. || height <= px(0.) {
        return None;
    }
    let ratio = native_width / native_height;
    let width = (height * ratio).min(column).max(px(1.));
    Some((image, size(width, width / ratio)))
}

/// The frame a picture still being fetched is drawn as: the column's width up to
/// [`LOADING_FRAME_MAX_WIDTH`], at a photo's proportions.
pub(super) fn loading_frame(column: Pixels) -> Size<Pixels> {
    let width = column.min(LOADING_FRAME_MAX_WIDTH).max(px(1.));
    size(
        width,
        (width * 0.5625).min(LOADING_FRAME_MAX_HEIGHT).round(),
    )
}
