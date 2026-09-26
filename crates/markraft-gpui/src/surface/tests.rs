use super::{
    AtomShape, CELL_MIN_WIDTH, CELL_PADDING_X, CELL_PADDING_Y, CODE_FONT, CODE_INSET, Decoration,
    LayoutLine, LayoutRow, Marker, PREVIEW_GAP, QUOTE_BAR, ROUNDED_FONT, Runs, ShapeInput,
    TABLE_LINE, TableCell, TableScroll, UI_FONT, Widening, atom_label, caret_cell_frame,
    cell_under, chrome_marker, column_demands, column_widths, decoration_of, display_text,
    drawn_image, file_name, gap_below, max_indent, merge_row_centers, picture_source, place_table,
    quote_bars, reveal_offset, shape, table_overflows, text_runs, unbreakable_units,
    visible_strips,
};
use crate::style::EditorStyle;
use crate::typeahead::tests::{at, run, state_of};
use gpui::{Bounds, NoopTextSystem, Pixels, TextSystem, WindowTextSystem, point, px, size};
use gpui::{Hsla, TextRun, font};
use markraft_commonmark::{commonmark_doc_type_names, commonmark_schema};
use markraft_core::EditorState;
use markraft_core::commands::ColumnAlignment;
use markraft_core::commands::insert_text;
use markraft_core::kind::DocTypes;
use markraft_core::projection::{Line, RunContent, projection_of};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

/// A row carrying only the token range it stands for, which is all the
/// caret and selection geometry decides with.
fn probe(index: usize, line: &markraft_core::projection::Line) -> LayoutLine {
    LayoutLine {
        source: line.clone(),
        index,
        from: line.from(),
        char_len: line.len(),
        rows: Vec::new(),
        origin: point(px(0.), px(0.)),
        line_height: px(10.),
        height: px(10.),
        width: px(100.),
        min_width: px(0.),
        top_gap: Pixels::ZERO,
        code_pos: None,
        marker: None,
        decoration: None,
        code_language: None,
        callout_header: None,
        code_inset: px(0.),
        marker_inset: px(0.),
        preview: None,
        quote_bars: Vec::new(),
        widenings: Vec::new(),
        atoms: Vec::new(),
        table: None,
        reuse: None,
        rendered: None,
        row_shifts: Vec::new(),
        align: markraft_core::kind::Align::Start,
    }
}

/// The rows a document's projection would be laid out into, as ranges.
fn rows_of(source: &str) -> Vec<LayoutLine> {
    let state = state_of(source);
    let projection = projection_of(&state);
    projection
        .lines()
        .iter()
        .enumerate()
        .map(|(index, line)| probe(index, line))
        .collect()
}

#[test]
fn popovers_anchor_all_selection_at_the_first_content_row() {
    for source in ["Markraft interface check", "- Parent\n  - Child"] {
        let state = state_of(source);
        let selection = markraft_core::Selection::All;
        let rows = rows_of(source);
        let start = selection.from(state.doc());
        let end = selection.to(state.doc());
        assert!(
            !rows[0].contains(start),
            "the document starts before its text"
        );
        let anchor = super::selection_anchor_row(&rows, start, end).unwrap();
        assert_eq!(anchor.index, 0);
        assert_eq!(anchor.pos_to_offset(start), 0);
    }

    let rows = rows_of("First\n\nSecond");
    let start = rows[1].from + 1;
    let anchor = super::selection_anchor_row(&rows, start, start + 2).unwrap();
    assert_eq!(anchor.index, 1, "a partial selection stays on its own row");
    assert_eq!(anchor.pos_to_offset(start), 1);
    assert!(super::selection_anchor_row(&rows, 0, 0).is_none());
    let outside = rows.last().unwrap().to() + 1;
    assert!(super::selection_anchor_row(&rows, outside, outside + 2).is_none());
}

/// A fixture holding every shape that makes a row
/// stand for something other than one plain paragraph — a multi-row code
/// block, a leaf block with no text, an empty paragraph — followed by the
/// line holding the caret.
const SHAPE: &str = "# Head\n\npara\n\n- a\n- b\n- [ ] c\n- [x] d\n\n1. x\n1. y\n\n\
                     > - q\n\n```rust\na\nb\nc\n```\n\n---\n\nLast paragraph\n\n\
                     h\n\nh\n\nh\n\nh\n\n<br>\n\nh";

#[test]
fn a_hit_on_a_divider_stays_on_that_leaf() {
    let state = state_of("text\n\n***");
    let projection = projection_of(&state);
    let divider = &projection.lines()[1];
    let row = probe(1, divider);
    assert_eq!(row.hit_position(0, &projection), divider.from());
}

#[test]
fn layout_coordinates_skip_inline_container_boundaries() {
    let rows = shaped("*a **b** c*");
    let row = &rows[0];
    // The delimiter characters are real projection offsets. With the
    // caret outside the span they are collapsed in the display map.
    assert_eq!(row.char_len, 11);
    let display_shift: isize = row
        .widenings
        .iter()
        .map(|w| w.len as isize - w.source_len as isize)
        .sum();
    assert_eq!(
        row.char_len as isize + display_shift,
        5,
        "six delimiter chars collapse, five prose chars remain"
    );
    assert!(
        row.widenings.iter().any(|w| w.len == 0 && w.source_len > 0),
        "at least one delimiter run is collapsed"
    );
    for offset in 0..=row.char_len {
        let pos = row.offset_to_pos(offset);
        assert_eq!(row.pos_to_offset(pos), offset);
        assert!(row.contains(pos));
    }
    assert_eq!(row.pos_to_offset(row.from), 0);
    assert_eq!(row.pos_to_offset(row.to()), row.char_len);
}

#[test]
fn exactly_one_row_holds_any_caret_position() {
    let rows = rows_of(SHAPE);
    assert!(rows.len() > 15, "the shape lays out as many rows");
    let last = rows.last().expect("a last row");
    for pos in 0..=last.to() {
        let holding: Vec<usize> = rows
            .iter()
            .filter(|row| row.contains(pos))
            .map(|row| row.index)
            .collect();
        assert!(
            holding.len() <= 1,
            "position {pos} is claimed by rows {holding:?}"
        );
    }
}

/// The row that draws the caret on the document's last line is the one whose
/// own range holds it, never a row further up.
#[test]
fn the_caret_on_the_last_line_belongs_to_the_last_row() {
    let rows = rows_of(SHAPE);
    let last = rows.last().expect("a last row").clone();
    assert_eq!(last.char_len, 1, "the last line holds one character");
    for caret in [last.from, last.to()] {
        let drawn: Vec<usize> = rows
            .iter()
            .filter(|row| row.contains(caret))
            .map(|row| row.index)
            .collect();
        assert_eq!(drawn, vec![last.index], "caret {caret}");
    }
}

#[test]
fn a_readable_line_width_narrows_the_column_and_centres_it() {
    let view = Bounds::new(point(px(40.), px(10.)), size(px(1000.), px(600.)));
    // No limit, or one wider than the view: the text fills the content box.
    assert_eq!(super::column_bounds(view, None), view);
    assert_eq!(super::column_bounds(view, Some(px(1200.))), view);
    let column = super::column_bounds(view, Some(px(700.)));
    assert_eq!(column.size.width, px(700.));
    assert_eq!(column.size.height, view.size.height);
    assert_eq!(column.top(), view.top());
    assert_eq!(column.left() - view.left(), view.right() - column.right());
    // Shaping wraps at the same width the column is placed at.
    assert_eq!(super::column_width(px(1000.), Some(px(700.))), px(700.));
    assert_eq!(super::column_width(px(500.), Some(px(700.))), px(500.));
}

/// Gaps far enough apart that the number a line carries says which spacing
/// rule produced it.
fn spaced_style() -> EditorStyle {
    EditorStyle {
        paragraph_gap: px(11.),
        list_gap: px(3.),
        heading_bottom_gap: px(5.),
        ..EditorStyle::notes()
    }
}

/// Run `each` over every line of `source`, with the shaping input the layout
/// pass would have built for it.
fn per_line<T>(
    source: &str,
    style: &EditorStyle,
    each: impl Fn(&ShapeInput<'_>, usize, &Line) -> T,
) -> Vec<T> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style,
        single_line: false,
        selection: 0..0,
        composition: None,
    };
    projection
        .lines()
        .iter()
        .enumerate()
        .map(|(index, line)| each(&input, index, line))
        .collect()
}

/// The gap every line of a document carries below it.
fn gaps_of(source: &str, style: &EditorStyle) -> Vec<Pixels> {
    per_line(source, style, |input, index, line| {
        gap_below(
            input,
            index,
            line,
            input.types.heading_level(line),
            input.types.is_code_block(line),
            &chrome_marker(input.types, line, None),
        )
    })
}

/// The decoration every line of a document carries, at a note's width.
fn decorations_of(source: &str, style: &EditorStyle) -> Vec<Option<Decoration>> {
    per_line(source, style, |input, index, line| {
        decoration_of(input, index, line, false, max_indent(input.style, px(600.)))
    })
}

/// Whatever kind of block follows a list, it is spaced off the last item by
/// the ordinary gap.
#[test]
fn a_list_closes_with_the_ordinary_block_gap() {
    let style = spaced_style();
    for following in ["> quote", "para", "```\nx\n```"] {
        let gaps = gaps_of(&format!("- a\n- b\n\n{following}"), &style);
        assert_eq!(gaps[0], style.list_gap, "between two items");
        assert_eq!(
            gaps[1], style.paragraph_gap,
            "below the item that closes the list, above {following:?}"
        );
    }
}

#[test]
fn a_nested_item_still_belongs_to_the_list_above_it() {
    let style = spaced_style();
    let gaps = gaps_of("- a\n  - b\n\npara", &style);
    assert_eq!(gaps[0], style.list_gap, "above the nested item");
    assert_eq!(gaps[1], style.paragraph_gap, "below the nested item");
}

/// A heading is spaced off whatever follows it by the same amount, so a list
/// under one is not tighter than a paragraph under one.
#[test]
fn a_heading_keeps_its_own_gap_above_any_block() {
    let style = spaced_style();
    for following in ["- a", "para"] {
        let gaps = gaps_of(&format!("# Head\n\n{following}"), &style);
        assert_eq!(gaps[0], style.heading_bottom_gap, "above {following:?}");
    }
}

/// The code fill is drawn past the last row, and the line's height holds it,
/// so the gap below it is the note's own even where a quote would otherwise
/// sit tight.
#[test]
fn a_code_block_keeps_its_bottom_padding_above_a_quote() {
    let style = spaced_style();
    let gaps = gaps_of("```\nx\n```\n\n> quote", &style);
    assert_eq!(gaps[0], style.paragraph_gap);
}

/// A quote spaces the blocks it holds as the note does: off what comes
/// before and after it, between its own paragraphs, and around a quote
/// opening inside it.
#[test]
fn a_quote_spaces_its_blocks_as_the_note_does() {
    let style = spaced_style();
    let gaps = gaps_of("para\n\n> a\n>\n> b\n\npara", &style);
    assert_eq!(gaps[0], style.paragraph_gap, "above the quote");
    assert_eq!(
        gaps[1], style.paragraph_gap,
        "between the quote's paragraphs"
    );
    assert_eq!(gaps[2], style.paragraph_gap, "below the quote");
    let gaps = gaps_of("> a\n>\n> > b\n\npara", &style);
    assert_eq!(gaps[0], style.paragraph_gap, "above the nested quote");
    assert_eq!(gaps[1], style.paragraph_gap, "leaving both quotes");
}

/// Two lists one after the other are two blocks, spaced apart as blocks
/// are, while a nested list's lines stay as close as its parent's.
#[test]
fn a_list_right_after_another_is_spaced_off_it() {
    let style = spaced_style();
    let gaps = gaps_of("- a\n  - b\n- c\n\n1. d\n2. e", &style);
    assert_eq!(gaps[0], style.list_gap, "into the nested list");
    assert_eq!(gaps[1], style.list_gap, "back out of it");
    assert_eq!(gaps[2], style.paragraph_gap, "between the two lists");
    assert_eq!(gaps[3], style.list_gap, "within the second list");
}

/// A list inside a quote draws the quote's bar at the quote's edge, where
/// the quote's paragraphs have it, not at the list's indent through its
/// bullets.
#[test]
fn a_list_in_a_quote_keeps_the_quote_bar_at_its_edge() {
    let source = "> para\n>\n> - item\n>\n>   ```\n>   x\n>   ```";
    let projection = projection_of(&state_of(source));
    let away = projection.lines()[0].from();
    let rows = shaped_revealing(source, away..away, None);
    let style = EditorStyle::notes();
    let bar = |row: &LayoutLine| row.quote_bar_x(1, 0, &style);
    assert_eq!(bar(&rows[1]), bar(&rows[0]), "the item's bar");
    assert_eq!(bar(&rows[2]), bar(&rows[0]), "the code block's bar");
}

/// The view keeps HTML verbatim and shows it as source, so a raw block is
/// drawn like a paragraph of monospace text: nothing behind it, and the
/// ordinary gap below.
#[test]
fn a_raw_block_is_drawn_as_a_plain_block() {
    let style = spaced_style();
    let source = "<div>\n  x\n</div>\n\npara";
    assert!(
        per_line(source, &style, |input, _, line| input
            .types
            .is_raw_block(line))[0],
        "the HTML block is the one kept verbatim"
    );
    assert_eq!(gaps_of(source, &style)[0], style.paragraph_gap);
    assert!(
        decorations_of(source, &style)[0].is_none(),
        "no panel behind a raw block"
    );
    assert!(
        matches!(
            decorations_of("```\nx\n```\n\npara", &style)[0],
            Some(Decoration::Code { levels: 0, .. })
        ),
        "a code block keeps its own",
    );
}

/// A code block in a quote keeps the quote's bar beside its panel, joined
/// to the paragraph above it, and the panel stands where that paragraph's
/// text does, so the bars of both lines fall in one place.
#[test]
fn a_code_block_in_a_quote_keeps_the_quote_bar() {
    let style = spaced_style();
    let source = "> para\n>\n> ```\n> x\n> ```";
    let decorations = decorations_of(source, &style);
    assert!(matches!(
        decorations[0],
        Some(Decoration::Quote {
            levels: 1,
            joined: 1,
            ..
        })
    ));
    assert!(
        matches!(
            decorations[1],
            Some(Decoration::Code {
                levels: 1,
                joined: 0,
                ..
            })
        ),
        "the bar beside the panel"
    );
    let lines = shaped(source);
    assert_eq!(lines[1].origin.x - super::CODE_PADDING, lines[0].origin.x);
}

/// A window-free text system, so the shaping pass can be run in a test.
/// Every character comes out the same width, which is all a row count and a
/// caret stop ask of it.
fn text_system() -> WindowTextSystem {
    WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(NoopTextSystem::new()))))
}

/// Every line of `source`, laid out at a note's width.
fn shaped(source: &str) -> Vec<LayoutLine> {
    shaped_with(source, callout_types())
}

/// What a CommonMark host hands the view: the preset's roles, plus the two
/// block-quote attributes that spell a callout, which no role table names.
fn callout_types() -> DocTypes {
    let schema = commonmark_schema();
    DocTypes {
        callout: Some(crate::CalloutAttrs {
            kind: "callout",
            title: "title",
        }),
        ..DocTypes::from_schema_names(&schema, &commonmark_doc_type_names())
    }
}

fn shaped_with(source: &str, types: DocTypes) -> Vec<LayoutLine> {
    shaped_in(source, types, px(600.), 0..0)
}

/// Every line of `source`, laid out `width` wide with the document
/// `selection`, which is what reveals a syntax run.
fn shaped_in(
    source: &str,
    types: DocTypes,
    width: Pixels,
    selection: Range<usize>,
) -> Vec<LayoutLine> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let style = EditorStyle::notes();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection,
        composition: None,
    };
    shape(&input, width, &text_system())
}

/// The label each atom pill of `source`'s first line reaches the screen with, the
/// host's index holding every note `resolves` accepts.
fn pill_labels(source: &str, resolves: fn(&str) -> bool) -> Vec<String> {
    let state = state_of(source);
    let projection = projection_of(&state);
    // A directory with nothing in it, so an embed that is no note finds no file.
    let images = crate::images::Images::new(Some(std::env::temp_dir().join("markraft-none")));
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let types = callout_types();
    let style = EditorStyle::notes();
    let wiki: crate::WikiResolver = Box::new(resolves);
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: Some(&wiki),
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection: 0..0,
        composition: None,
    };
    shape(&input, px(600.), &text_system())[0]
        .atoms
        .iter()
        .map(|atom| atom.label.text.to_string())
        .collect()
}

/// The atoms of `source`'s first line, remote images being fetched.
fn fetching_atoms(source: &str) -> Vec<(String, Option<gpui::Size<Pixels>>)> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let mut images = crate::images::Images::default();
    images.set_remote_enabled(true);
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let types = callout_types();
    let style = EditorStyle::notes();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection: 0..0,
        composition: None,
    };
    let line = &shape(&input, px(600.), &text_system())[0];
    line.atoms
        .iter()
        .map(|atom| (atom.label.text.to_string(), atom.frame))
        .collect()
}

/// A picture still on its way holds the room of one where it stands alone, and
/// stays a pill beside text, where a picture would be a pill too.
#[test]
fn a_remote_image_being_fetched_holds_a_pictures_place() {
    let alone = fetching_atoms("![a cat](https://example.com/cat.png)");
    assert_eq!(alone.len(), 1);
    assert_eq!(alone[0].0, "Loading image: a cat");
    let frame = alone[0].1.expect("a frame, not a pill");
    assert_eq!(frame.width, px(480.));
    assert!(frame.height > px(200.), "a picture's height, not a row's");
    let inline = fetching_atoms("see ![a cat](https://example.com/cat.png) here");
    assert_eq!(inline, [("Loading image: a cat".to_owned(), None)]);
}

#[test]
fn an_embed_is_labelled_with_its_name_alone() {
    assert_eq!(
        pill_labels("x ![[Second Note]] y", |_| true),
        ["Second Note"]
    );
    assert_eq!(
        pill_labels("x ![[Missing Note]] y", |_| false),
        ["Missing Note"]
    );
}

/// The display text of `source`'s first line and the runs it is shaped with.
fn runs_of(source: &str) -> (String, Runs, EditorStyle) {
    runs_with(source, 0..0)
}

/// [`runs_of`] with the document `selection`, which reveals syntax runs.
fn runs_with(source: &str, selection: Range<usize>) -> (String, Runs, EditorStyle) {
    runs_styled(source, selection, EditorStyle::notes())
}

/// [`runs_with`] in `style`.
fn runs_styled(
    source: &str,
    selection: Range<usize>,
    style: EditorStyle,
) -> (String, Runs, EditorStyle) {
    let state = state_of(source);
    let projection = projection_of(&state);
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let types = callout_types();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection,
        composition: None,
    };
    let line = &projection.lines()[0];
    let font_size = style.font_size(None, false);
    let text = display_text(&input, line, 0, font_size, px(600.), &text_system());
    let runs = text_runs(&input, line, &text, None, false, font_size, &style);
    (text.text.to_string(), runs, style.clone())
}

/// The run that draws the first occurrence of `needle` in `text`.
fn run_over<'r>(text: &str, runs: &'r Runs, needle: char) -> &'r TextRun {
    let at = text.find(needle).expect("the text holds the needle");
    let mut byte = 0;
    runs.runs
        .iter()
        .find(|run| {
            byte += run.len;
            at < byte
        })
        .expect("a run covers every byte")
}

/// The note's text is set in the style's family; code keeps its own face.
#[test]
fn prose_takes_the_style_font_family_and_code_keeps_its_face() {
    let style = EditorStyle {
        font_family: "Georgia".into(),
        ..EditorStyle::notes()
    };
    let (text, runs, _) = runs_styled("a **b** `c`", 0..0, style);
    assert_eq!(run_over(&text, &runs, 'a').font.family.as_ref(), "Georgia");
    assert_eq!(run_over(&text, &runs, 'b').font.family.as_ref(), "Georgia");
    assert_eq!(run_over(&text, &runs, 'c').font.family.as_ref(), CODE_FONT);
}

/// The rounded system design has no italic, so emphasis set in it falls back to
/// the system face, which has; the rest of the prose stays rounded.
#[test]
fn emphasis_in_the_rounded_face_is_set_in_one_with_an_italic() {
    let style = EditorStyle {
        font_family: ROUNDED_FONT.into(),
        ..EditorStyle::notes()
    };
    let (text, runs, _) = runs_styled("a *b*", 0..0, style);
    assert_eq!(
        run_over(&text, &runs, 'a').font.family.as_ref(),
        ROUNDED_FONT
    );
    let emphasis = run_over(&text, &runs, 'b');
    assert_eq!(emphasis.font.family.as_ref(), UI_FONT);
    assert_eq!(emphasis.font.style, gpui::FontStyle::Italic);
}

/// Link definitions are drawn in the prose face, the
/// label bold and the destination underlined. Other raw source stays
/// monospaced.
#[test]
fn link_definitions_read_as_prose_and_other_raw_source_as_code() {
    let (text, runs, style) = runs_of("[ref]: https://e.com \"T\"\n[b]: /u\n\nafter");
    assert_eq!(text, "[ref]: https://e.com \"T\"\n[b]: /u");
    let label = run_over(&text, &runs, 'r');
    assert_eq!(label.font.family, style.font_family);
    assert_eq!(label.font.weight, gpui::FontWeight::BOLD);
    assert_eq!(label.color, style.text);
    let destination = run_over(&text, &runs, 'h');
    assert!(destination.underline.is_some());
    assert_eq!(destination.font.weight, gpui::FontWeight::default());
    assert!(run_over(&text, &runs, '[').underline.is_none());

    let (text, runs, _) = runs_of("<div>\nx\n</div>");
    assert_eq!(run_over(&text, &runs, 'x').font.family.as_ref(), CODE_FONT);
}

#[test]
fn highlighted_text_is_drawn_over_the_highlight_fill() {
    let (text, runs, style) = runs_of("a ==b== c");
    assert_eq!(
        run_over(&text, &runs, 'b').background_color,
        Some(style.highlight)
    );
    assert_eq!(run_over(&text, &runs, 'c').background_color, None);
}

/// The fill is painted per run and joined where neighbouring runs carry the
/// same colour, so a highlight with other marks inside it must hand every
/// one of its runs the one fill, or it would be drawn broken into pieces.
#[test]
fn a_highlight_keeps_one_fill_across_the_marks_inside_it() {
    let (text, runs, style) = runs_of("a ==b **c** d== e");
    for needle in ['b', 'c', 'd'] {
        assert_eq!(
            run_over(&text, &runs, needle).background_color,
            Some(style.highlight),
            "{needle} sits in the highlight"
        );
    }
    assert_eq!(run_over(&text, &runs, 'a').background_color, None);
    assert_eq!(run_over(&text, &runs, 'e').background_color, None);
}

/// A code span inside a highlight shares the fill of the text around it,
/// so the band runs unbroken through the pill's slot, padding included;
/// the pill itself is painted over the fill.
#[test]
fn a_highlight_keeps_one_fill_through_the_code_pill_inside_it() {
    let (text, runs, style) = runs_of("x ==a `c` b== y");
    for needle in ['a', 'c', 'b'] {
        assert_eq!(
            run_over(&text, &runs, needle).background_color,
            Some(style.highlight),
            "{needle} sits in the highlight"
        );
    }
    // Every run from `a` to `b` carries it — the code's delimiters and the
    // spaces beside the pill included — so the fill never breaks.
    let (from, to) = (text.find('a').unwrap(), text.find('b').unwrap());
    let mut start = 0;
    for run in &runs.runs {
        let end = start + run.len;
        if end > from && start <= to {
            assert_eq!(
                run.background_color,
                Some(style.highlight),
                "{:?}",
                &text[start..end]
            );
        }
        start = end;
    }
    assert_eq!(run_over(&text, &runs, 'x').background_color, None);
    assert_eq!(run_over(&text, &runs, 'y').background_color, None);
    assert!(!runs.code.is_empty(), "the code keeps its pill");
}

/// The code spans drawn as pills on each visual row of `line`, as the
/// text each one holds.
fn pills(line: &LayoutLine) -> Vec<(usize, String)> {
    line.rows
        .iter()
        .flat_map(|row| {
            row.inline_code
                .iter()
                .filter(|code| !code.raised)
                .map(|code| (code.visual_row, row.text()[code.range.clone()].to_owned()))
        })
        .collect()
}

/// A code span wrapped across two rows is drawn as a pill on each. The
/// row it wraps from ends in the space it wraps at, which is not drawn:
/// the pill stops at the last glyph instead of running on to the row's
/// end, and the smaller text centred in it holds no trailing space.
#[test]
fn a_wrapped_code_span_is_pilled_to_its_glyphs() {
    // Every character is as wide as every other, so at this width the row
    // wraps after `inline code `.
    let lines = shaped_in(
        "alpha beta `inline code span here` gamma",
        callout_types(),
        px(200.),
        0..0,
    );
    let line = &lines[0];
    let row = &line.rows[0];
    assert_eq!(row.wrap_starts(), [0, 23]);
    assert_eq!(
        pills(line),
        [(0, "inline code".to_owned()), (1, "span here".to_owned())]
    );
    let x = |byte| {
        row.line
            .position_for_index(byte, line.line_height)
            .unwrap()
            .x
    };
    let first = &row.inline_code[0];
    assert_eq!(first.left, x(11));
    assert_eq!(first.slot, x(22) - x(11), "as wide as its glyphs");
    assert!(
        first.left + first.slot < line.width,
        "short of the row's end"
    );
    let second = &row.inline_code[1];
    assert_eq!(second.left, px(0.), "the next row starts with the pill");
    assert_eq!(second.line.text.as_ref(), "span here");
}

/// An HTML block is drawn as its page while the caret is elsewhere: its own
/// lines, aligned as the block asks, stand in for its source, and the line is as
/// tall as the page. The caret reaching it brings the source back, with the
/// page kept under it.
#[test]
fn an_html_block_is_drawn_as_its_page_and_under_its_source_with_the_caret_in_it() {
    let source = "intro\n\n<p align=\"center\">\n  <b>Hi</b> there\n</p>\n\n[a]: /u";
    let away = shaped_in(source, callout_types(), px(600.), 0..0);
    let html = &away[1];
    let page = html.rendered.as_ref().expect("drawn as its page");
    assert_eq!(page.lines.len(), 1);
    let (_, line) = &page.lines[0];
    assert_eq!(line.align, markraft_core::kind::Align::Center);
    let row = &line.rows[0];
    assert_eq!(
        line.row_shifts,
        [(line.width - row.line.unwrapped_layout.width) / 2.],
        "the row moves by the room gpui centres it in"
    );
    assert!(!page.under_source(), "the page stands in for the source");
    assert!(
        html.height >= page.height,
        "the line is as tall as its page"
    );
    assert!(away[2].rendered.is_none(), "a link definition stays source");

    let at = projection_of(&state_of(source)).lines()[1].from() + 1;
    let near = shaped_in(source, callout_types(), px(600.), at..at);
    let html = &near[1];
    let page = html.rendered.as_ref().expect("the page stays in view");
    assert_eq!(
        page.top,
        html.text_height() + super::PREVIEW_GAP,
        "under the source rows"
    );
    assert!(html.height >= page.top + page.height);
}

/// What the page cannot hold whole keeps the block as source.
#[test]
fn an_html_block_the_page_cannot_hold_stays_source() {
    let lines = shaped_in(
        "intro\n\n<table><tr><td>a</td></tr></table>",
        callout_types(),
        px(600.),
        0..0,
    );
    assert!(lines[1].rendered.is_none());
}

/// A key in `<kbd>` is drawn in a pill as a code span is.
#[test]
fn a_key_is_drawn_in_a_pill() {
    let lines = shaped("press <kbd>K</kbd> and `c`");
    assert_eq!(pills(&lines[0]), [(0, "K".to_owned()), (0, "c".to_owned())]);
}

/// A paragraph's line breaks start rows of their own, and a code span after
/// one is drawn on the visual row its text sits on, not one row lower per
/// break before it.
#[test]
fn code_spans_after_a_line_break_keep_their_row() {
    let lines = shaped("one `a`\ntwo `b`\nthree `c`");
    assert_eq!(lines[0].rows.len(), 3, "one row per line");
    assert_eq!(
        pills(&lines[0]),
        [
            (0, "a".to_owned()),
            (1, "b".to_owned()),
            (2, "c".to_owned())
        ]
    );
}

/// With the caret inside a code span its backticks are revealed as runs
/// of their own, in the quieter markup ink. They still sit in the one pill
/// the code is drawn in, rather than in pills of their own abutting it.
#[test]
fn revealed_backticks_share_the_code_pill() {
    // The caret between `c` and `o`: doc position 1 opens the paragraph.
    let (text, runs, style) = runs_with("x `code` y", 5..5);
    assert_eq!(text, "x `code` y", "the backticks are revealed");
    assert_eq!(runs.code.len(), 1, "one span for the backticks and code");
    let inks: Vec<(usize, Hsla)> = runs.code[0]
        .runs
        .iter()
        .map(|(len, _, ink)| (*len, *ink))
        .collect();
    assert_eq!(
        inks,
        [
            (1, style.muted_text),
            (4, style.inline_code_text),
            (1, style.muted_text)
        ],
        "the backticks keep the markup ink"
    );
    let lines = shaped_in("x `code` y", callout_types(), px(600.), 5..5);
    assert_eq!(pills(&lines[0]), [(0, "`code`".to_owned())]);
}

/// Revealed inside a highlight, the span is still one pill over one fill.
#[test]
fn revealed_backticks_in_a_highlight_share_the_pill_and_the_fill() {
    // The caret on `c`: `x ==a ` takes doc positions 1 to 7.
    let source = "x ==a `c` b== y";
    let (text, runs, style) = runs_with(source, 8..8);
    assert!(text.contains("`c`"), "the backticks are revealed: {text}");
    assert_eq!(runs.code.len(), 1, "one span for the backticks and code");
    let (from, to) = (text.find('a').unwrap(), text.find('b').unwrap());
    let mut start = 0;
    for run in &runs.runs {
        let end = start + run.len;
        if end > from && start <= to {
            assert_eq!(
                run.background_color,
                Some(style.highlight),
                "{:?}",
                &text[start..end]
            );
        }
        start = end;
    }
    let lines = shaped_in(source, callout_types(), px(600.), 8..8);
    assert_eq!(pills(&lines[0]), [(0, "`c`".to_owned())]);
}

#[test]
fn a_formula_is_drawn_as_its_source_in_the_code_font() {
    let (text, runs, style) = runs_of("x $a+b$ y");
    let formula = run_over(&text, &runs, '+');
    assert_eq!(formula.font.family, font(CODE_FONT).family);
    assert_eq!(formula.color, style.inline_code_text);
    assert!(runs.code.is_empty(), "no pill is reserved for a formula");
    assert_ne!(run_over(&text, &runs, 'y').font.family, formula.font.family);
}

#[test]
fn superscript_is_painted_again_smaller_and_raised() {
    let lines = shaped("x^2^ and `c`");
    let row = &lines[0].rows[0];
    let raised: Vec<&str> = row
        .inline_code
        .iter()
        .filter(|code| code.raised)
        .map(|code| &row.text()[code.range.clone()])
        .collect();
    assert_eq!(raised, ["2"]);
    let code = row
        .inline_code
        .iter()
        .find(|code| !code.raised)
        .expect("the code pill");
    assert_eq!(code.lift, px(0.));
    let two = row
        .inline_code
        .iter()
        .find(|code| code.raised)
        .expect("the superscript");
    assert!(two.lift > px(0.), "raised above the row");
}

#[test]
fn a_footnote_reference_is_raised_and_its_definition_labelled() {
    let lines = shaped("see[^n]\n\n[^n]: the note\n    more");
    let row = &lines[0].rows[0];
    let raised: Vec<&str> = row
        .inline_code
        .iter()
        .filter(|code| code.raised && code.lift > px(0.))
        .map(|code| &row.text()[code.range.clone()])
        .collect();
    assert_eq!(raised, ["n"]);
    let Some(Marker::Footnote(label)) = &lines[1].marker else {
        panic!("the definition's first line carries its label");
    };
    assert_eq!(label.text.as_ref(), "[n]");
    assert!(lines[1].footnote_marker().is_some());
    // Its later lines are indented with it, and carry no label.
    assert!(lines[1].origin.x > lines[0].origin.x);
}

#[test]
fn subscript_is_painted_again_smaller_and_lowered() {
    let lines = shaped("H~2~O");
    let row = &lines[0].rows[0];
    let script = row
        .inline_code
        .iter()
        .find(|code| code.raised)
        .expect("the subscript");
    assert_eq!(&row.text()[script.range.clone()], "2");
    assert!(script.lift < px(0.), "lowered below the row");
}

/// A raw block's source is its own text, so it goes through the rows a code
/// block goes through: one per `\n`, each holding the caret stops of its own
/// line and reported to AccessKit as the text it is rather than as an atom.
#[test]
fn a_raw_blocks_rows_follow_its_newlines() {
    let source = "<div>\n  x\n</div>";
    let lines = shaped(&format!("{source}\n\npara"));
    let raw = &lines[0];
    assert_eq!(
        raw.rows.iter().map(LayoutRow::text).collect::<Vec<_>>(),
        vec!["<div>", "  x", "</div>"],
        "one row per source line"
    );
    assert_eq!(raw.visual_rows(), 3, "and none of them wrapped");
    assert_eq!(raw.char_len, source.chars().count());
    // Caret movement, selection and hit testing all locate an offset by its
    // row, so every row has to answer for its own stretch of the block.
    for (offset, row) in [(0, 0), (5, 0), (6, 1), (9, 1), (10, 2), (raw.char_len, 2)] {
        assert_eq!(raw.locate(offset).0, row, "offset {offset}");
    }
    assert_eq!(
        raw.accessible_rows(),
        vec![0..5, 6..9, 10..raw.char_len],
        "every row is selectable text"
    );
}

/// A two-by-one RGBA PNG, so a decode can be checked against a known
/// aspect ratio without shipping a fixture file.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0xf4, 0x22, 0x7f,
    0x8a, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x42, 0xff, 0x01, 0x0f, 0xf9, 0x03, 0xfd, 0x85, 0x11, 0x99, 0x76, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// An `<img>` tag's `width` and `height` size the picture as a browser would:
/// one side keeps its proportions on the other, and the column still bounds it.
#[test]
fn a_picture_takes_the_size_its_tag_asks_for() {
    let path = std::env::temp_dir().join("markraft-surface-declared.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let src = path.to_str().expect("a UTF-8 temp path");
    let images = crate::images::Images::default();
    let size = |declared| {
        let (_, drawn) = drawn_image(&images, src, declared, px(100.)).expect("decodes");
        (drawn.width, drawn.height)
    };
    assert_eq!(size((Some(40.), None)), (px(40.), px(20.)));
    assert_eq!(size((None, Some(10.))), (px(20.), px(10.)));
    assert_eq!(size((Some(30.), Some(30.))), (px(30.), px(30.)));
    assert_eq!(
        size((Some(400.), None)),
        (px(100.), px(50.)),
        "the column bounds it"
    );
    let _ = std::fs::remove_file(&path);
}

/// Pictures with a line to themselves are drawn at their own size, side by
/// side, however many there are; beside text they shrink to the row.
#[test]
fn pictures_alone_on_a_line_keep_their_size_side_by_side() {
    let path = std::env::temp_dir().join("markraft-surface-gallery.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let src = path.to_str().expect("a UTF-8 temp path");
    let sizes = |source: &str| -> Vec<(Pixels, Pixels)> {
        shaped(source)[0]
            .atoms
            .iter()
            .filter_map(|atom| {
                atom.image
                    .as_ref()
                    .map(|(_, size)| (size.width, size.height))
            })
            .collect()
    };
    assert_eq!(
        sizes(&format!("![a]({src}) ![b]({src})")),
        [(px(2.), px(1.)), (px(2.), px(1.))]
    );
    let beside_text = sizes(&format!("see ![a]({src}) here"));
    assert!(
        beside_text[0].1 > px(1.),
        "beside text it is drawn to the row"
    );
    let _ = std::fs::remove_file(&path);
}

/// A picture as wide as the column reserves no more than the column: rounding
/// its reserved width up to whole fillers would push the last of them onto a
/// row of its own, as tall as the picture and empty.
#[test]
fn a_picture_as_wide_as_the_column_keeps_to_one_row() {
    let path = std::env::temp_dir().join("markraft-surface-wide.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let src = path.to_str().expect("a UTF-8 temp path");
    let source = format!(
        "intro\n\n<p align=\"center\">\n  <img src=\"{src}\" alt=\"shot\" width=\"480\">\n</p>"
    );
    let lines = shaped_in(&source, callout_types(), px(432.), 0..0);
    let page = lines[1].rendered.as_ref().expect("a page");
    let (_, picture) = &page.lines[0];
    assert_eq!(picture.visual_rows(), 1);
    assert_eq!(
        picture.atoms[0].image.as_ref().map(|(_, size)| size.width),
        Some(px(432.))
    );
    let _ = std::fs::remove_file(&path);
}

/// The size an `<img>` tag asks for is read from its attributes, as numbers,
/// pixels or a share of the column; nonsense asks for nothing.
#[test]
fn a_tag_asks_for_a_size_in_numbers_or_pixels() {
    let state = state_of(
        "<img src=\"a.png\" width=\"96\" height=\"48px\"> <img src=\"b.png\" width=\"50%\"> <img src=\"c.png\" width=\"wide\">",
    );
    let image = state.schema().node_id("image").expect("an image type");
    let mut images = Vec::new();
    state.doc().descendants(&mut |node, _, _, _| {
        if node.type_id() == image {
            images.push(node.clone());
        }
        true
    });
    assert_eq!(images.len(), 3);
    assert_eq!(
        super::declared_size(&images[0], px(400.)),
        (Some(96.), Some(48.))
    );
    assert_eq!(
        super::declared_size(&images[1], px(400.)),
        (Some(200.), None),
        "a share of the column"
    );
    assert_eq!(super::declared_size(&images[2], px(400.)), (None, None));
}

/// Decoding runs on the layout pass, outside any window or app context, so
/// it has to stand on its own. Only a file the note can actually read is
/// drawn; everything else keeps the placeholder.
#[test]
fn a_local_file_is_decoded_and_fitted_to_the_column() {
    let path = std::env::temp_dir().join("markraft-surface-test.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let src = path.to_str().expect("a UTF-8 temp path");
    let images = crate::images::Images::default();
    let (_, drawn) = drawn_image(&images, src, (None, None), px(100.)).expect("the PNG decodes");
    assert_eq!(drawn.width, px(2.), "a small image is not blown up");
    assert_eq!(drawn.height, px(1.));
    assert!(drawn_image(&images, "https://host/a.png", (None, None), px(100.)).is_none());
    assert!(drawn_image(&images, "/no/such/file.png", (None, None), px(100.)).is_none());
    let _ = std::fs::remove_file(&path);
}

/// A caret let into a picture's source finds it spelled out as the line's
/// text, with the picture still drawn under it, and the
/// line tall enough for both.
#[test]
fn a_picture_stays_in_view_under_its_spelled_out_source() {
    let path = std::env::temp_dir().join("markraft-preview-test.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let source = format!("![a]({})\n\nafter", path.display());
    let state = state_of(&source);
    let reached = state
        .update([
            markraft_core::TransactionSpec::new().selection(markraft_core::Selection::cursor(1))
        ])
        .expect("the caret moves")
        .state()
        .clone();
    let projection = projection_of(&reached);
    assert_eq!(
        projection.line_text(0),
        Some(format!("![a]({})", path.display()).as_str()),
        "the caret found the source"
    );
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(reached.schema().clone());
    let style = EditorStyle::notes();
    let types = callout_types();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: reached.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection: 1..1,
        composition: None,
    };
    let lines = shape(&input, px(600.), &text_system());
    let (_, drawn) = lines[0].preview.as_ref().expect("the picture under it");
    assert_eq!(drawn.width, px(2.));
    assert_eq!(
        lines[0].height,
        lines[0].text_height() + PREVIEW_GAP + drawn.height + style.paragraph_gap,
        "the line holds the source and the picture"
    );
    // Away from it, the picture is the line again, and nothing is drawn
    // under anything.
    let after = projection.lines()[1].from();
    let lines = shape(
        &ShapeInput {
            selection: after..after,
            composition: None,
            ..input
        },
        px(600.),
        &text_system(),
    );
    assert!(lines[0].preview.is_none());
    let _ = std::fs::remove_file(&path);
}

/// A remote picture spelled out under the caret is still one the document
/// shows: its fetch is kept rather than dropped as soon as it starts, and
/// the picture is drawn under its source once it arrives.
#[test]
fn a_remote_picture_stays_in_view_under_its_spelled_out_source() {
    let source = "https://example.com/a.png";
    let state = state_of(&format!("![a]({source})\n\nafter"));
    let reached = state
        .update([
            markraft_core::TransactionSpec::new().selection(markraft_core::Selection::cursor(1))
        ])
        .expect("the caret moves")
        .state()
        .clone();
    let projection = projection_of(&reached);
    let mut images = crate::images::Images::default();
    images.set_remote_enabled(true);
    let spelling = markraft_commonmark::CommonMarkSpelling::new(reached.schema().clone());
    let style = EditorStyle::notes();
    let types = callout_types();
    let shaped = |images: &crate::images::Images| {
        let input = ShapeInput {
            images,
            spelling: Some(&spelling),
            wiki: None,
            doc: reached.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            selection: 1..1,
            composition: None,
        };
        shape(&input, px(600.), &text_system())
    };
    assert!(shaped(&images)[0].preview.is_none(), "still on its way");
    assert_eq!(images.take_requests(), [source]);
    let fetcher: crate::RemoteImageFetcher = Arc::new(|_| Ok(PNG.to_vec()));
    assert!(
        images.finish_remote(source, crate::images::fetch(&fetcher, source)),
        "the fetch was still wanted when it arrived"
    );
    assert!(
        shaped(&images)[0].preview.is_some(),
        "drawn once it arrives"
    );
}

/// A picture sharing its line with text is drawn, as tall as the row
/// allows, rather than named in a pill; the row keeps the text's height.
#[test]
fn a_picture_sharing_its_line_is_drawn_as_tall_as_the_row() {
    let path = std::env::temp_dir().join("markraft-inline-test.png");
    std::fs::write(&path, PNG).expect("a writable temp directory");
    let source = format!("see ![a]({}) here", path.display());
    let lines = shaped_revealing(&source, 0..0, None);
    let line = &lines[0];
    let style = EditorStyle::notes();
    let row = style.body_size * style.line_height_ratio;
    assert_eq!(line.line_height, row, "the row keeps the text's height");
    let (_, drawn) = line.atoms[0].image.as_ref().expect("the picture is drawn");
    assert_eq!(drawn.height, row - super::INLINE_IMAGE_INSET * 2.);
    assert_eq!(
        drawn.width,
        drawn.height * 2.,
        "at the picture's proportions"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_image_falls_back_to_the_name_of_its_file() {
    assert_eq!(file_name("images/shot.png"), Some("shot.png"));
    assert_eq!(file_name("/a/b/shot.png?v=2#x"), Some("shot.png"));
    assert_eq!(file_name("https://host/"), Some("host"));
    assert_eq!(file_name(""), None);
}

/// A drawn atom needs far more room than the one character it is, so the
/// display text holds a run of fillers in its place. Every geometry query
/// still speaks in projection offsets, which the two maps have to preserve.
#[test]
fn an_atom_placeholder_stays_one_caret_stop() {
    let mut rows = rows_of("ab![alt](x.png)cd");
    let row = &mut rows[0];
    assert_eq!(row.char_len, 5, "the atom is one visible character");
    row.widenings = vec![Widening {
        source: 2,
        source_len: 1,
        display: 2,
        len: 5,
        shape: Some(AtomShape::Pill),
        broken: false,
    }];
    for offset in 0..=row.char_len {
        assert_eq!(row.to_source(row.to_display(offset)), offset, "at {offset}");
    }
    assert_eq!(row.to_display(2), 2, "the atom starts where its slot does");
    assert_eq!(row.to_display(3), 7, "and ends where its slot does");
    assert_eq!(row.to_source(3), 2, "the atom's left half is before it");
    assert_eq!(row.to_source(6), 3, "and its right half is after it");
    assert_eq!(row.to_display(5), 9, "text past the atom keeps its order");
}

/// Inline HTML nothing reads is never rendered: the primitive is drawn as the
/// source it stands for, as the prose around it, so the row reserves the width of
/// exactly that text and nothing around it.
#[test]
fn a_raw_inline_atom_reserves_the_width_of_its_own_source() {
    let state = state_of("press <var>K</var> twice");
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let drawn: Vec<(AtomShape, &str)> = projection.lines()[0]
        .runs()
        .iter()
        .filter_map(|run| match &run.content {
            RunContent::Atom(node) => atom_label(&types, node),
            _ => None,
        })
        .collect();
    assert_eq!(
        drawn,
        vec![(AtomShape::Text, "<var>"), (AtomShape::Text, "</var>")],
        "both tags read as they were written"
    );
    assert_eq!(
        AtomShape::Text.chrome(),
        px(0.),
        "source text reserves its own width and no padding"
    );
    // And the row shapes that source itself, so the text after a tag sits
    // against it exactly as it does after a wiki link.
    let row = &shaped("press <var>K</var> twice")[0];
    assert_eq!(row.rows[0].text(), "press <var>K</var> twice");
    assert!(!row.rows[0].text().contains(super::PILL_FILLER));
}

/// A point inside a tag shown as source finds the tag and how far into it
/// the point is, so a click puts the caret there. Its edges are the
/// atom's own caret stops, and find nothing.
#[test]
fn a_point_inside_source_shown_as_text_finds_how_far_in_it_is() {
    let line = &shaped("press <var>K</var> now")[0];
    // `press ` is six characters; `<var>` is shown from there.
    assert_eq!(line.source_text_in(8), Some((6, "<var>".to_owned(), 2)));
    assert_eq!(line.source_text_in(10), Some((6, "<var>".to_owned(), 4)));
    assert_eq!(line.source_text_in(6), None, "the tag's left edge");
    assert_eq!(line.source_text_in(11), None, "the tag's right edge");
    assert_eq!(line.source_text_in(3), None, "plain text");
    // `</var>` follows `K`: shown from 12, it is the third character's atom.
    assert_eq!(line.source_text_in(14), Some((8, "</var>".to_owned(), 2)));
}

/// A `<br>` in a table cell is the cell's line break:
/// the cell's text goes on in a row of its own. Anywhere else the tag is
/// source like any other inline HTML.
#[test]
fn a_break_tag_in_a_table_cell_starts_a_new_row() {
    let lines = shaped("| a |\n| - |\n| 1<br/>2 |\n\nx<br/>y");
    let cell: Vec<&str> = lines
        .iter()
        .find(|line| line.rows.len() == 2)
        .expect("a cell of two rows")
        .rows
        .iter()
        .map(|row| row.text())
        .collect();
    assert_eq!(cell, ["1", "2"]);
    let paragraph = lines.last().expect("the paragraph");
    assert_eq!(paragraph.rows.len(), 1);
    assert_eq!(paragraph.rows[0].text(), "x<br/>y");
}

/// A wiki link is drawn as the prose it stands in: its alias, or its
/// target with whatever it names, and none of its brackets. An embed is not a
/// link at all — it names a file the note shows — so it reads as an image does.
#[test]
fn a_wiki_link_atom_is_drawn_as_its_label_and_nothing_around_it() {
    let state =
        state_of("see [[Note#Top]] and [[a/b|Alias]] and ![[pics/x.png]] and ![[y.png|Cover]]");
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let drawn: Vec<(AtomShape, &str)> = projection.lines()[0]
        .runs()
        .iter()
        .filter_map(|run| match &run.content {
            RunContent::Atom(node) => atom_label(&types, node),
            _ => None,
        })
        .collect();
    assert_eq!(
        drawn,
        vec![
            (AtomShape::Link, "Note#Top"),
            (AtomShape::Link, "Alias"),
            (AtomShape::Pill, "x.png"),
            (AtomShape::Pill, "Cover"),
        ]
    );
    assert_eq!(AtomShape::Link.chrome(), px(0.));
    assert!(AtomShape::Link.is_own_text());
    // An embed is the same picture as `![](…)` written another way, so both name
    // the same file to load; a plain link names no picture at all.
    let sources: Vec<Option<&str>> = projection.lines()[0]
        .runs()
        .iter()
        .filter_map(|run| match &run.content {
            RunContent::Atom(node) => Some(picture_source(&types, node)),
            _ => None,
        })
        .collect();
    assert_eq!(sources, vec![None, None, Some("pics/x.png"), Some("y.png")]);
}

/// A wiki link is shaped as the text it reads as, so the punctuation after
/// it sits against it rather than after a placeholder rounded up to whole
/// fillers.
#[test]
fn text_after_a_wiki_link_starts_at_the_labels_right_edge() {
    let lines = shaped("see [[Missing Page]]. end");
    let row = &lines[0];
    // The display text holds the label itself; nothing stands in for it.
    assert_eq!(row.rows[0].text(), "see Missing Page. end");
    assert!(!row.rows[0].text().contains(super::PILL_FILLER));
    // Four projection characters — `see ` — then the atom, then `. end`.
    assert_eq!(row.char_len, 4 + 1 + 5);
    let atom = row.rectangles(4..5, false);
    let after = row.rectangles(5..6, false);
    assert_eq!(atom.len(), 1);
    assert_eq!(after.len(), 1);
    assert_eq!(atom[0].right(), after[0].left());
}

/// A callout says what it is on a line of its own above its first block,
/// which is chrome: it takes room, not caret stops.
#[test]
fn a_callout_reserves_a_header_row_above_its_first_block_only() {
    let lines = shaped("> [!tip] Custom title\n> First\n>\n> Second\n\nafter");
    let (first, second, after) = (&lines[0], &lines[1], &lines[2]);
    assert_eq!(
        first.callout_header().map(|(label, _)| label),
        Some("Custom title")
    );
    assert_eq!(second.callout_header().map(|(label, _)| label), None);
    assert_eq!(after.callout_header().map(|(label, _)| label), None);
    // The room is above the block, so no caret stop moved.
    assert!(first.top_gap >= super::CALLOUT_HEADER_HEIGHT);
    let (_, box_) = first.callout_header().expect("a header");
    assert_eq!(box_.bottom(), first.origin.y);
    // The marker line is not in the text, so the first line is the body's.
    assert_eq!(first.rows[0].text(), "First");
    // A callout with no title reads as its type, capitalised.
    let lines = shaped("> [!warning]\n> Body");
    assert_eq!(
        lines[0].callout_header().map(|(label, _)| label),
        Some("Warning")
    );
    // Where the callout sits in the note does not matter, and a first block
    // holding a hard break is still one line with one header.
    let lines = shaped("before\n\n> [!note]\n> One  \n> two\n>\n> > [!tip]\n> > Inner");
    let labels: Vec<_> = lines
        .iter()
        .map(|line| line.callout_header().map(|(label, _)| label.to_owned()))
        .collect();
    assert_eq!(
        labels,
        [None, Some("Note".to_owned()), Some("Tip".to_owned())]
    );
    // Each bar keeps its own callout's tone: beside the nested tip the outer
    // bar is still the note's.
    let Some(Decoration::Quote { levels, tones, .. }) = lines[2].decoration else {
        panic!("a nested callout line is decorated as a quote");
    };
    assert_eq!(levels, 2);
    assert!(tones[0].is_some() && tones[1].is_some());
    assert_ne!(tones[0], tones[1]);
    let Some(Decoration::Quote { tones: outer, .. }) = lines[1].decoration else {
        panic!("a callout line is decorated as a quote");
    };
    assert_eq!(outer[0], tones[0]);
    // The band is the header's, and nothing below or above it is.
    let band = lines[1].origin.y - super::CALLOUT_HEADER_HEIGHT / 2.;
    assert!(lines[1].in_callout_header(band));
    assert!(!lines[1].in_callout_header(lines[1].origin.y));
    assert!(!lines[0].in_callout_header(band));
    // An ordinary quote has no header and no extra room.
    let lines = shaped("> plain");
    assert!(lines[0].callout_header().is_none());
    assert_eq!(lines[0].top_gap, Pixels::ZERO);
}

/// A callout that opens on a code block draws its header in a band of its
/// own above the panel, not inside the panel's padding: the line keeps the
/// room a code block in a plain quote keeps, plus the header's band on top.
#[test]
fn a_callout_opening_on_a_code_block_draws_its_header_above_the_panel() {
    let mut callout = shaped("> [!warning] Warn\n> ```\n> warn code\n> ```");
    let mut plain = shaped("> ```\n> warn code\n> ```");
    stack(&mut callout, Pixels::ZERO);
    stack(&mut plain, Pixels::ZERO);
    let (code, bare) = (&callout[0], &plain[0]);
    assert!(code.code_inset > Pixels::ZERO, "the first line is code");
    assert!(bare.callout_header().is_none());
    assert_eq!(code.top_gap, bare.top_gap + super::CALLOUT_HEADER_HEIGHT);
    assert_eq!(code.height, bare.height);
    let (label, header) = code.callout_header().expect("a header");
    assert_eq!(label, "Warn");
    // The band starts where the line's room starts and ends where the panel
    // begins, flush with the panel's left edge.
    let panel_top = code.origin.y - code.code_panel_room();
    assert_eq!(header.top(), code.origin.y - code.top_gap);
    assert_eq!(header.bottom(), panel_top);
    assert_eq!(header.left(), code.origin.x - super::CODE_PADDING);
    assert!(code.in_callout_header(header.top()));
    assert!(!code.in_callout_header(panel_top));
    // The panel itself is where it is in a plain quote, one band lower.
    assert_eq!(code.origin.y - bare.origin.y, super::CALLOUT_HEADER_HEIGHT);
    assert_eq!(code.origin.x, bare.origin.x);

    // A callout opening on a paragraph keeps its header right above the text.
    let mut prose = shaped("> [!warning] Warn\n> warn text");
    stack(&mut prose, Pixels::ZERO);
    let first = &prose[0];
    assert_eq!(first.top_gap, super::CALLOUT_HEADER_HEIGHT);
    let (_, header) = first.callout_header().expect("a header");
    assert_eq!(header.bottom(), first.origin.y);
    assert_eq!(header.top(), first.origin.y - super::CALLOUT_HEADER_HEIGHT);
    // Both headers start at the same x: where the paragraph's text does.
    assert_eq!(
        header.left(),
        code.callout_header().expect("a header").1.left()
    );
}

/// Callouts are the host's to ask for: the attributes that spell one are not
/// in any role table, so a host that names none gets the block quotes its
/// schema declares and nothing drawn around them — however those attributes
/// happen to be spelled.
#[test]
fn a_host_that_names_no_callout_attributes_draws_plain_quotes() {
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    assert!(types.callout.is_none(), "no role table names them");
    let lines = shaped_with("> [!tip] Custom title\n> Body", types);
    assert!(lines[0].callout_header().is_none());
    assert_eq!(lines[0].top_gap, Pixels::ZERO);
    // The quote is still a quote, with a bar of the ordinary tone.
    let Some(Decoration::Quote { levels, tones, .. }) = lines[0].decoration else {
        panic!("a quote line is decorated as a quote");
    };
    assert_eq!(levels, 1);
    assert_eq!(tones[0], None);
}

/// A table of two rows of two cells, as the measuring pass leaves them:
/// projection lines with a natural width and nothing placed yet.
fn table_cells(natural: Pixels) -> Vec<LayoutLine> {
    let mut cells = rows_of("| a | b |\n| - | - |\n| c | d |");
    assert_eq!(cells.len(), 4, "two rows of two cells");
    for cell in &mut cells {
        cell.width = natural;
    }
    cells
}

/// The table `table_cells` builds, placed on a 100 by 80 grid.
fn placed_table(alignment: ColumnAlignment, natural: Pixels) -> Vec<LayoutLine> {
    let mut cells = table_cells(natural);
    let natural = vec![natural; cells.len()];
    place_table(
        &mut cells,
        6,
        &[(0, 0), (0, 1), (1, 0), (1, 1)],
        &[px(100.), px(80.)],
        &[px(30.), px(40.)],
        &natural,
        &[alignment; 2],
        px(4.),
        px(7.),
    );
    cells
}

/// What `prepaint` does with a shaped line list: stack the lines by their
/// own gap and height, from `top`.
fn stack(lines: &mut [LayoutLine], top: Pixels) {
    let mut y = top;
    for line in lines {
        y += line.top_gap;
        line.origin.y += y;
        y += line.height;
    }
}

/// A content-sized table is not stretched to the editor's width; one that
/// does not fit gives up width in proportion to what each column asked for,
/// and never goes below the minimum.
#[test]
fn a_table_keeps_its_content_width_until_the_grid_has_to_shrink() {
    let preferred = [px(100.), px(200.), px(100.)];
    let floor = [CELL_MIN_WIDTH; 3];
    assert_eq!(
        column_widths(&preferred, &floor, px(400.)),
        preferred.to_vec(),
        "room to spare leaves every column as it asked"
    );
    assert_eq!(
        column_widths(&preferred, &floor, px(200.)),
        vec![CELL_MIN_WIDTH, px(88.), CELL_MIN_WIDTH],
        "the two narrow columns stop at the floor and the wide one takes the rest"
    );
    // A grid that will not fit even at the floor keeps its width, and the
    // view scrolls it sideways rather than squeezing its text away.
    let tight = [CELL_MIN_WIDTH; 3];
    assert_eq!(column_widths(&tight, &floor, px(100.)), tight.to_vec());
    // A column whose widest word needs more than the flat floor keeps it:
    // shrinking may wrap a cell, never split a word.
    let minimum = [px(120.), CELL_MIN_WIDTH, CELL_MIN_WIDTH];
    assert_eq!(
        column_widths(&preferred, &minimum, px(200.)),
        vec![px(120.), CELL_MIN_WIDTH, CELL_MIN_WIDTH],
        "the word's column holds its width and the grid overflows instead"
    );
}

/// A column is floored by the widest word of the widest cell in it, as that
/// cell will be painted — a header cell is bold, so the floor is the bold
/// width, not the regular one.
///
/// The live bug this pins: the bold header "Second" measured 52.0004px, the
/// column reserved `52.0004 + 16`, and handing the cell back `that - 16`
/// returned 52.000397px — a fraction short in `f32`, which is all the
/// wrapper needs to break on strictly greater and split the word as
/// `Secon / d`. Whole pixels at both ends make the round trip exact.
#[test]
fn a_bold_header_floors_its_column_at_the_width_the_bold_word_needs() {
    // What the measuring pass leaves behind: a bold one-word header and the
    // narrow body cell under it. One word, so content and min-content are
    // the same width.
    let bold = px(52.0004);
    let body = px(9.1);
    let mut cells = table_cells(px(0.));
    for (index, cell) in cells.iter_mut().enumerate() {
        let measured = if index < 2 { bold } else { body };
        cell.width = measured;
        cell.min_width = measured;
    }
    let grid = [(0, 0), (0, 1), (1, 0), (1, 1)];
    let (preferred, minimum) = column_demands(&cells, &grid, 2);
    assert_eq!(
        preferred, minimum,
        "a one-word column cannot give anything up"
    );
    for (column, floor) in minimum.iter().enumerate() {
        assert!(
            *floor >= bold + CELL_PADDING_X * 2.,
            "column {column} floors below the bold header it holds"
        );
        // The width the cell is actually reshaped at, which is what the
        // wrapper compares the word against.
        let content = *floor - CELL_PADDING_X * 2.;
        assert!(
            content >= bold,
            "column {column} hands back {content:?}, short of the {bold:?} word"
        );
    }
    // And it holds through a grid far too narrow for those columns, which
    // is exactly when the shrinking runs.
    for content in column_widths(&preferred, &minimum, px(80.))
        .iter()
        .map(|width| *width - CELL_PADDING_X * 2.)
    {
        assert!(content >= bold, "shrinking split the word after all");
    }
}

/// A code span wholly on the first visual row of a wrapped line gets its
/// one pill there, and the rows after it none.
#[test]
fn a_code_span_before_a_wrap_is_pilled_on_its_own_row() {
    let lines = shaped_in(
        "alpha `code` beta gamma delta epsilon zeta eta theta iota kappa lambda mu",
        callout_types(),
        px(200.),
        0..0,
    );
    assert!(lines[0].rows[0].visual_rows() > 1, "the line wraps");
    assert_eq!(pills(&lines[0]), [(0, "code".to_owned())]);
}

/// Where the wrapper would break before a full-width comma, the character
/// before it goes down with it: no row starts with closing punctuation,
/// and none ends with opening punctuation.
#[test]
fn a_row_never_starts_with_closing_punctuation() {
    let starts = |source: &str| {
        let lines = shaped_in(source, callout_types(), px(200.), 0..0);
        let row = &lines[0].rows[0];
        row.wrap_starts()
            .into_iter()
            .map(|byte| row.text()[..byte].chars().count())
            .collect::<Vec<_>>()
    };
    let plain = "一".repeat(60);
    let first = starts(&plain)[1];
    assert!(first > 1, "the test width holds more than one character");

    let mut chars: Vec<char> = plain.chars().collect();
    chars[first] = '，';
    let comma: String = chars.iter().collect();
    assert_eq!(
        starts(&comma)[1],
        first - 1,
        "the comma keeps its character"
    );

    let mut chars: Vec<char> = plain.chars().collect();
    chars[first - 1] = '「';
    let quote: String = chars.iter().collect();
    assert_eq!(
        starts(&quote)[1],
        first - 1,
        "the quote goes down with its text"
    );
}

/// What a column may never be narrower than: the widest run of text the
/// line wrapper would not break. These ranges have to agree with where gpui
/// actually wraps, or a column floors at a width that still splits a word.
#[test]
fn a_cells_unbreakable_units_are_the_runs_the_wrapper_keeps_together() {
    let units = |text: &str, glue: &[std::ops::Range<usize>]| -> Vec<String> {
        unbreakable_units(text, glue)
            .into_iter()
            .map(|range| text[range].to_owned())
            .collect()
    };
    assert_eq!(units("gamma delta", &[]), ["gamma", "delta"]);
    assert_eq!(units("  leading  spaces ", &[]), ["leading", "spaces"]);
    // A hyphen and an underscore bind a word together; a path separator and
    // a query do not, so a long URL still has somewhere to break.
    assert_eq!(units("well-known_name", &[]), ["well-known_name"]);
    assert_eq!(units("a/b?c&d", &[]), ["a", "/b", "?c", "&d"]);
    // CJK breaks between any two characters, so one character is all a
    // column has to reserve room for.
    assert_eq!(units("表格标题", &[]), ["表", "格", "标", "题"]);
    // Except where a mark may not start or end a line.
    assert_eq!(units("表，格「标」题", &[]), ["表，", "格", "「标」", "题"]);
    // A code span is drawn as one pill, so it counts as one unit however
    // many words it holds.
    let span = std::slice::from_ref(&(4usize..14usize));
    assert_eq!(
        units("say cargo test now", span),
        ["say", "cargo test", "now"]
    );
}

/// The grid is expressed in the terms `prepaint` stacks lines in: the cells
/// of one row keep one y and only the last of them advances it, so the run
/// of lines lands as a rectangle of boxes rather than a column of them.
#[test]
fn a_tables_cells_share_one_band_of_y_and_only_the_last_of_a_row_advances() {
    let mut cells = placed_table(ColumnAlignment::None, px(10.));
    assert!(cells.iter().all(|cell| cell.top_gap == Pixels::ZERO));
    assert!(cells.iter().all(|cell| cell.origin.y == CELL_PADDING_Y));
    assert_eq!(cells[0].height, Pixels::ZERO, "a cell mid-row advances not");
    assert_eq!(cells[1].height, px(30.), "the last of a row carries it");
    assert_eq!(cells[2].height, Pixels::ZERO);
    assert_eq!(
        cells[3].height,
        px(40.) + px(7.),
        "and the last of the table carries the gap below it too"
    );
    stack(&mut cells, px(0.));
    let boxes: Vec<Bounds<Pixels>> = cells
        .iter()
        .map(|cell| cell.cell_bounds().expect("a placed cell has a box"))
        .collect();
    assert_eq!(
        boxes[0],
        Bounds::new(point(px(4.), px(0.)), size(px(100.), px(30.)))
    );
    assert_eq!(
        boxes[1],
        Bounds::new(point(px(104.), px(0.)), size(px(80.), px(30.)))
    );
    assert_eq!(
        boxes[2],
        Bounds::new(point(px(4.), px(30.)), size(px(100.), px(40.)))
    );
    assert_eq!(
        boxes[3],
        Bounds::new(point(px(104.), px(30.)), size(px(80.), px(40.)))
    );
    // The text sits inside its own box, clear of the padding.
    assert_eq!(
        cells[2].origin,
        point(px(4.) + CELL_PADDING_X, px(30.) + CELL_PADDING_Y)
    );
}

/// An alignment moves the text inside the column and leaves the box where
/// it is, so the grid stays rectangular whatever the columns declare.
#[test]
fn a_column_alignment_offsets_the_text_within_the_cell() {
    // The first column's content box is 100 less its two paddings.
    let content = px(100.) - CELL_PADDING_X * 2.;
    for (alignment, shift) in [
        (ColumnAlignment::None, px(0.)),
        (ColumnAlignment::Left, px(0.)),
        (ColumnAlignment::Center, (content - px(24.)) * 0.5),
        (ColumnAlignment::Right, content - px(24.)),
    ] {
        let cells = placed_table(alignment, px(24.));
        assert_eq!(
            cells[0].origin.x,
            px(4.) + CELL_PADDING_X + shift,
            "{alignment:?} starts the text here"
        );
        assert_eq!(
            cells[0].origin.x + cells[0].width,
            px(4.) + px(100.) - CELL_PADDING_X,
            "{alignment:?} still ends at the column's own edge"
        );
        assert_eq!(
            cells[0].cell_bounds().expect("a box").left(),
            px(4.),
            "{alignment:?} leaves the box alone"
        );
    }
    // Text too wide for its column has no slack to be offset by.
    let cells = placed_table(ColumnAlignment::Right, px(400.));
    assert_eq!(cells[0].origin.x, px(4.) + CELL_PADDING_X);
}

/// Walking the lines by y lands on whichever cell closes the row, so the
/// grid has to say which column a point was in.
#[test]
fn a_point_over_a_grid_resolves_to_the_cell_it_is_in() {
    let mut cells = placed_table(ColumnAlignment::None, px(10.));
    stack(&mut cells, px(0.));
    let at = |x: f32, y: f32| {
        cell_under(&cells, 6, point(px(x), px(y)))
            .expect("a table has a nearest cell")
            .index
    };
    assert_eq!(at(20., 10.), 0);
    assert_eq!(at(120., 10.), 1);
    assert_eq!(at(20., 50.), 2);
    assert_eq!(at(120., 50.), 3);
    // Beside the grid, the nearest cell of the row the point is level with.
    assert_eq!(at(500., 50.), 3, "past the right edge");
    assert_eq!(at(-50., 50.), 2, "before the left edge");
    assert_eq!(at(120., -80.), 1, "above the grid");
    assert!(cell_under(&cells, 99, point(px(20.), px(10.))).is_none());
}

/// Every cell of a table row occupies the same band, so without merging
/// them one press of Down would step through that row once per column.
#[test]
fn the_cells_of_one_row_offer_the_arrow_keys_a_single_visual_row() {
    let centers = merge_row_centers(vec![px(20.), px(8.), px(20.2), px(8.), px(40.)]);
    assert_eq!(centers, vec![px(8.), px(20.), px(40.)]);
    assert_eq!(merge_row_centers(Vec::new()), Vec::<Pixels>::new());
}

/// The gap above a table also holds the row the table keeps for its toolbar.
#[test]
fn a_table_is_one_block_whose_cells_sit_tight() {
    let style = spaced_style();
    let gaps = gaps_of("para\n\n| a | b |\n| - | - |\n| c | d |\n\npara", &style);
    assert_eq!(
        gaps[0],
        style.paragraph_gap + style.table_toolbar_room,
        "above the table"
    );
    assert_eq!(&gaps[1..4], [Pixels::ZERO; 3], "between its cells");
    assert_eq!(gaps[4], style.paragraph_gap, "below the table");
}

/// What marker, if any, each line of a document draws.
fn markers_of(source: &str) -> Vec<Option<&'static str>> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    projection
        .lines()
        .iter()
        .map(|line| {
            chrome_marker(&types, line, None).map(|marker| match marker {
                Marker::Number(_) => "number",
                Marker::Bullet { .. } => "bullet",
                Marker::Task { .. } => "task",
                Marker::Footnote(_) => "footnote",
            })
        })
        .collect()
}

/// A container inside a list item is not a second start of that item: neither
/// a nested quote nor the rows of a nested table draw the item's marker.
#[test]
fn only_an_items_very_first_line_carries_its_marker() {
    assert_eq!(
        markers_of("- [ ] task\n\n  > quoted\n"),
        vec![Some("task"), None],
        "the quote inside the item starts nothing"
    );
    assert_eq!(
        markers_of("- item\n\n  | a | b |\n  | - | - |\n  | c | d |\n"),
        vec![Some("bullet"), None, None, None, None],
        "no cell of the nested table starts the item again"
    );
    // Two items still get one marker each, and a nested list its own.
    assert_eq!(
        markers_of("- a\n- [x] b\n  - c\n"),
        vec![Some("bullet"), Some("task"), Some("bullet")]
    );
}

#[test]
fn ordered_tasks_draw_separate_checkbox_and_keep_continuation_indent() {
    let rows = shaped("7. [ ] open\n\n   continuation\n\n8. [x] done\n9. plain");
    let first = &rows[0];
    let (checked, checkbox) = first.task_marker().expect("ordered task has checkbox");
    assert!(!checked);
    let Some(Marker::Task {
        number: Some(number),
        ..
    }) = &first.marker
    else {
        panic!("ordered task also keeps its number");
    };
    let ordinal_center = point(
        checkbox.left() - super::NUMBER_GAP - number.width / 2.,
        checkbox.center().y,
    );
    assert!(
        !checkbox.contains(&ordinal_center),
        "the number is not a checkbox hit target"
    );
    assert_eq!(
        first.origin.x, rows[1].origin.x,
        "continuation text keeps its alignment"
    );
    assert!(
        rows[1].task_marker().is_none(),
        "only the task's first line has a checkbox"
    );
    assert!(rows[2].task_marker().unwrap().0);
    assert!(
        rows[3].task_marker().is_none(),
        "plain ordered items never toggle tasks"
    );
}

/// An item that opens with a quote or a fence draws its marker where every
/// item of its list does, left of the quote's bar and the code panel.
#[test]
fn an_items_marker_stands_left_of_the_blocks_it_opens_with() {
    let rows = shaped("- [ ] a\n- [x] > b\n- > c\n- ```\n  d\n  ```");
    let plain = rows[0].task_marker().expect("a check box").1;
    assert_eq!(
        plain.left(),
        rows[0].origin.x - px(22.),
        "a plain item's box"
    );
    let quoted = rows[1].task_marker().expect("a check box").1;
    assert_eq!(quoted.left(), plain.left());
    let bar = rows[1].quote_bars[0];
    assert!(
        quoted.right() < rows[1].origin.x - bar,
        "the box is left of the bar"
    );
    let bullet = rows[2].marker_bounds().expect("a bullet");
    assert!(bullet.right() < rows[2].origin.x - rows[2].quote_bars[0]);
    let code = rows[3].marker_bounds().expect("a bullet");
    assert_eq!(code.left(), bullet.left());
    // An ordered task's box has room of its own, which the quote after it
    // does not move.
    let rows = shaped("1. [ ] a\n2. [ ] > b");
    let plain = rows[0].task_marker().expect("a check box").1;
    assert_eq!(
        plain.left(),
        rows[0].origin.x - px(22.),
        "a plain item's box"
    );
    let quoted = rows[1].task_marker().expect("a check box").1;
    assert_eq!(quoted.left(), plain.left());
}

/// A cell's own height is zero except on the last of its row, so the quote
/// a grid sits in has nothing to draw a bar against; the grid draws them
/// itself, over its whole height.
#[test]
fn a_grid_inside_a_quote_draws_its_own_bars() {
    let mut cells = placed_table(ColumnAlignment::None, px(10.));
    stack(&mut cells, px(0.));
    let grid = cells
        .iter()
        .filter_map(LayoutLine::cell_bounds)
        .reduce(|all, bounds| all.union(&bounds))
        .expect("a placed table has a grid");
    assert_eq!(grid.size.height, px(70.), "rows of 30 and 40");
    assert_eq!(
        quote_bars(grid, 2, px(0.), px(12.)),
        vec![
            Bounds::new(point(px(-20.), px(0.)), size(QUOTE_BAR, px(70.))),
            Bounds::new(point(px(-8.), px(0.)), size(QUOTE_BAR, px(70.))),
        ],
        "one bar per level, at the indents the ordinary painter uses"
    );
    // A scrolling grid slides under its bars rather than taking them along.
    assert_eq!(
        quote_bars(grid, 1, px(30.), px(12.))[0].left(),
        px(4.) + px(30.) - px(12.)
    );
    assert!(quote_bars(grid, 0, px(0.), px(12.)).is_empty());
}

/// The caret's cell is brought into the visible strip, as the vertical
/// reveal brings its line into the viewport.
#[test]
fn the_caret_pulls_its_own_cell_into_the_visible_strip() {
    let strip = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(50.)));
    let cell = |left: f32| Bounds::new(point(px(left), px(0.)), size(px(40.), px(20.)));
    assert_eq!(
        reveal_offset(cell(10.), strip, px(0.), px(200.)),
        px(0.),
        "a cell already in view keeps the reader's place"
    );
    assert_eq!(
        reveal_offset(cell(150.), strip, px(0.), px(200.)),
        px(90.),
        "one off the right edge is pulled just inside it"
    );
    assert_eq!(
        reveal_offset(cell(10.), strip, px(80.), px(200.)),
        px(10.),
        "and one off the left edge of a scrolled grid is pulled back"
    );
    assert_eq!(
        reveal_offset(cell(500.), strip, px(0.), px(200.)),
        px(200.),
        "never past what there is to scroll"
    );
}

/// A grid too wide for the note keeps its width and is drawn scrolled. Every
/// geometry query reads the moved origins, so a click lands in the cell the
/// reader sees under the pointer.
#[test]
fn a_grid_wider_than_the_note_scrolls_as_one_and_takes_its_geometry_along() {
    let mut cells = placed_table(ColumnAlignment::None, px(10.));
    stack(&mut cells, px(0.));
    let content = Bounds::new(point(px(0.), px(0.)), size(px(120.), px(200.)));
    // The grid runs from 4 to 184 and the note's content ends at 120.
    assert_eq!(
        table_overflows(&cells, content).get(&6).copied(),
        Some(px(64.))
    );
    assert!(
        table_overflows(
            &cells,
            Bounds::new(point(px(0.), px(0.)), size(px(400.), px(200.)))
        )
        .values()
        .all(|overflow| *overflow == Pixels::ZERO),
        "a grid that fits has nothing to scroll"
    );
    for cell in &mut cells {
        cell.origin.x -= px(64.);
    }
    assert_eq!(
        cell_under(&cells, 6, point(px(60.), px(10.)))
            .expect("a table has a nearest cell")
            .index,
        1,
        "the second column is what sits under that point now"
    );
    let scroll = HashMap::from([(
        6usize,
        TableScroll {
            offset: px(64.),
            overflow: px(64.),
        },
    )]);
    assert_eq!(
        visible_strips(&cells, &scroll, content),
        vec![(
            6,
            Bounds::new(point(px(0.), px(0.)), size(px(120.), px(70.)))
        )],
        "the strip the wheel and the toolbar answer for is the visible part"
    );
    assert!(
        visible_strips(&cells, &HashMap::new(), content).is_empty(),
        "a grid with nothing to scroll takes no wheel"
    );
}

/// A layout left over from a document with fewer lines draws no caret at
/// all rather than drawing it on whichever row happens to share an index.
#[test]
fn a_stale_layout_draws_no_caret_rather_than_the_wrong_one() {
    let rows = rows_of(SHAPE);
    let caret = rows.last().expect("a last row").to();
    let stale = &rows[..rows.len() - 5];
    assert!(
        stale.iter().all(|row| !row.contains(caret)),
        "a row that does not hold the caret never draws it"
    );
}

/// Past the last word of a line, a point lands after the markup that closes
/// the line's last span, not inside it; elsewhere a collapsed run keeps the
/// caret on its near side.
#[test]
fn the_end_of_a_line_lies_past_its_closing_markup() {
    for source in ["x **bold**", "x `code`", "x *it*", "x ~~s~~"] {
        let rows = shaped_revealing(source, 0..0, None);
        let row = &rows[0];
        let end = row.to_display(row.char_len);
        assert_eq!(row.source_at(end), row.char_len, "{source:?}");
        assert_eq!(row.source_at(2), 2, "{source:?}: before the span opens");
    }
}

/// One line may hold several spans of one style. The caret opens the span it
/// is in and leaves the others spelled the way a reader wants them.
#[test]
fn only_the_span_the_caret_is_in_opens_its_delimiters() {
    let source = "**aa** plain **bb**";
    let state = state_of(source);
    let projection = projection_of(&state);
    let plain = projection.plain_text();
    let caret = projection
        .line_offset_to_pos(0, plain.find("bb").expect("bb") + 1)
        .expect("a position");
    let rows = shaped_revealing(source, caret..caret, None);
    let hidden: Vec<_> = rows[0]
        .widenings
        .iter()
        .filter(|widening| widening.len == 0)
        .collect();
    assert_eq!(
        hidden.len(),
        2,
        "the other span's pair stays collapsed: {:?}",
        rows[0]
            .widenings
            .iter()
            .map(|w| (w.source, w.len))
            .collect::<Vec<_>>()
    );
    // With the caret outside both, all four delimiter runs collapse.
    let away = shaped_revealing(source, 0..0, None);
    assert_eq!(
        away[0].widenings.iter().filter(|w| w.len == 0).count(),
        4,
        "every pair collapses when the caret is elsewhere"
    );
}

#[test]
fn the_caret_never_swaps_a_marker_for_its_spelling() {
    let text = text_system();
    let state = state_of("# Hello\n\n- item\n\n1. numbered\n\n- [ ] task");
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let style = EditorStyle::default();
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    // Caret in the heading.
    let heading_pos = projection.lines()[0].from();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        wiki: None,
        selection: heading_pos..heading_pos,
        composition: None,
    };
    let rows = shape(&input, px(400.), &text);
    assert!(
        rows[0].marker.is_none(),
        "a focused heading shows no hashes"
    );
    // Caret in the bullet item.
    let bullet_pos = projection.lines()[1].from();
    let input = ShapeInput {
        selection: bullet_pos..bullet_pos,
        ..input
    };
    let rows = shape(&input, px(400.), &text);
    assert!(
        matches!(rows[1].marker, Some(Marker::Bullet { .. })),
        "a focused bullet stays a drawn marker"
    );
    let ordered_pos = projection.lines()[2].from();
    let rows = shape(
        &ShapeInput {
            selection: ordered_pos..ordered_pos,
            composition: None,
            ..input
        },
        px(400.),
        &text,
    );
    assert!(
        matches!(rows[2].marker, Some(Marker::Number(_))),
        "a focused ordinal stays drawn"
    );
    let task_pos = projection.lines()[3].from();
    let rows = shape(
        &ShapeInput {
            selection: task_pos..task_pos,
            composition: None,
            ..input
        },
        px(400.),
        &text,
    );
    assert!(
        matches!(rows[3].marker, Some(Marker::Task { .. })),
        "a focused task keeps its check box"
    );
    // Unfocused list keeps chrome.
    let input = ShapeInput {
        selection: heading_pos..heading_pos,
        ..input
    };
    let rows = shape(&input, px(400.), &text);
    assert!(
        matches!(rows[1].marker, Some(Marker::Bullet { .. })),
        "unfocused bullet stays a drawn marker"
    );
    let state = state_of("- # Item heading");
    let projection = projection_of(&state);
    let pos = projection.lines()[0].from();
    let rows = shape(
        &ShapeInput {
            images: &images,
            spelling: Some(&spelling),
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            wiki: None,
            selection: pos..pos,
            composition: None,
        },
        px(400.),
        &text,
    );
    assert!(
        matches!(rows[0].marker, Some(Marker::Bullet { .. })),
        "a heading opening a list item keeps the item's bullet"
    );
}

/// A code block spells no fences, focused or not: its panel reaches only
/// its padding past its text, and focusing it adds a language tag that
/// hangs over what follows. Nothing below it moves.
#[test]
fn a_code_block_is_as_tall_focused_as_it_is_unfocused() {
    let source = "```rust\nfn main() {}\n```\n\nafter";
    let projection = projection_of(&state_of(source));
    let inside = projection.lines()[0].from() + 1;
    let outside = projection.lines()[1].from() + 1;
    let focused = shaped_revealing(source, inside..inside, None);
    let unfocused = shaped_revealing(source, outside..outside, None);
    let (code, plain) = (&focused[0], &unfocused[0]);
    assert_eq!(code.code_inset, CODE_INSET);
    assert_eq!(code.top_gap, CODE_INSET, "no fence row above the text");
    assert_eq!(code.top_gap, plain.top_gap);
    assert_eq!(code.height, plain.height);
    assert_eq!(
        code.height,
        code.text_height() + CODE_INSET + EditorStyle::notes().paragraph_gap,
        "no fence row below it either"
    );
    let total = |lines: &[LayoutLine]| {
        lines
            .iter()
            .fold(px(0.), |sum, line| sum + line.top_gap + line.height)
    };
    assert_eq!(total(&focused), total(&unfocused));
}

/// A focused code block shows its language as a tag in its panel's
/// top-right corner, and a block with none shows what the language
/// picker calls none; an unfocused block shows no tag.
#[test]
fn a_focused_code_block_tags_its_language_inside_its_panel() {
    let tag = |source: &str, focused: bool| {
        let projection = projection_of(&state_of(source));
        let pos = if focused {
            projection.lines()[0].from() + 1
        } else {
            projection.lines()[1].from() + 1
        };
        let lines = shaped_revealing(source, pos..pos, None);
        let code = &lines[0];
        let label = code
            .code_language
            .as_ref()
            .map(|label| label.text.to_string());
        (label, code.code_language_bounds(), code.clone())
    };
    let (label, bounds, code) = tag("```rust\nfn main() {}\n```\n\nafter", true);
    assert_eq!(label.as_deref(), Some("rust"));
    let bounds = bounds.expect("a focused block's tag has bounds");
    assert_eq!(
        bounds.right(),
        code.origin.x + code.width + super::CODE_PADDING,
        "flush with the panel's right edge"
    );
    assert_eq!(
        bounds.top(),
        code.origin.y - CODE_INSET,
        "flush with the panel's top edge"
    );
    let (label, _, _) = tag("```\nplain\n```\n\nafter", true);
    assert_eq!(label.as_deref(), Some("Plain Text"));
    let (label, bounds, _) = tag("```rust\nfn main() {}\n```\n\nafter", false);
    assert!(label.is_none() && bounds.is_none(), "no tag unfocused");
}

/// A quote draws its bar and nothing else wherever the caret is: no `>`
/// beside it.
#[test]
fn a_focused_quote_draws_as_it_does_unfocused() {
    let text = text_system();
    let state = state_of("> quoted\n\npara");
    let projection = projection_of(&state);
    let schema = commonmark_schema();
    let types = DocTypes::from_schema_names(&schema, &commonmark_doc_type_names());
    let style = EditorStyle::default();
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let quote_pos = projection.lines()[0].from();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        wiki: None,
        selection: quote_pos..quote_pos,
        composition: None,
    };
    let focused = shape(&input, px(400.), &text);
    let para_pos = projection.lines()[1].from();
    let unfocused = shape(
        &ShapeInput {
            selection: para_pos..para_pos,
            composition: None,
            ..input
        },
        px(400.),
        &text,
    );
    assert_eq!(focused[0].origin, unfocused[0].origin);
    assert_eq!(focused[0].quote_bars, unfocused[0].quote_bars);
}

/// An input method's marked text opens the span it is being typed into, and
/// only that one.
#[test]
fn marked_text_opens_the_span_it_sits_in() {
    let source = "**aa** plain **bb**";
    let state = state_of(source);
    let projection = projection_of(&state);
    let plain = projection.plain_text();
    let at = |needle: &str| {
        projection
            .line_offset_to_pos(0, plain.find(needle).expect("needle") + 1)
            .expect("a position")
    };
    let marked = at("bb");
    let rows = shaped_revealing(source, 0..0, Some(marked..marked));
    assert_eq!(
        rows[0].widenings.iter().filter(|w| w.len == 0).count(),
        2,
        "the marked span opens even though the selection is elsewhere"
    );
}

/// What a laid-out line shows, row by row.
fn shown_text(line: &LayoutLine) -> Vec<&str> {
    line.rows.iter().map(LayoutRow::text).collect()
}

/// A caret at the `offset`th source character of the first line.
fn caret_at(source: &str, offset: usize) -> std::ops::Range<usize> {
    let state = state_of(source);
    let pos = projection_of(&state).lines()[0]
        .offset_to_pos(offset)
        .expect("a position in the line");
    pos..pos
}

/// An entity away from the caret is drawn as the character it stands for,
/// and every offset around it still maps to a caret stop: a click on the
/// `&` lands before or after the whole entity, never inside it.
#[test]
fn a_concealed_entity_is_drawn_as_what_it_displays() {
    let source = "a &amp; b";
    let rows = shaped_revealing(source, 0..0, None);
    let row = &rows[0];
    assert_eq!(shown_text(row), vec!["a & b"]);
    let entity: Vec<_> = row
        .widenings
        .iter()
        .map(|w| (w.source, w.source_len, w.display, w.len, w.shape.is_none()))
        .collect();
    assert_eq!(entity, vec![(2, 5, 2, 1, true)]);
    assert_eq!(row.to_display(2), 2, "the entity starts where its `&` does");
    assert_eq!(row.to_display(7), 3, "and ends where it does");
    assert_eq!(row.to_display(9), 5, "the text after it follows on");
    assert_eq!(row.to_source(2), 2);
    assert_eq!(row.to_source(3), 7, "past the `&` is past the whole entity");
    assert_eq!(row.to_source(5), 9);
    for offset in [0, 1, 2, 7, 8, 9] {
        assert_eq!(row.to_source(row.to_display(offset)), offset, "at {offset}");
    }
    // With the caret on it the entity is its source again.
    let revealed = shaped_revealing(source, caret_at(source, 4), None);
    assert_eq!(shown_text(&revealed[0]), vec!["a &amp; b"]);
    assert!(revealed[0].widenings.is_empty());
}

/// A hard break's backslash is hidden until the caret ends its row, and
/// shown then.
#[test]
fn a_hard_break_spelling_shows_only_at_the_caret() {
    let source = "ab\\\ncd";
    let away = shaped_revealing(source, 0..0, None);
    assert_eq!(shown_text(&away[0]), vec!["ab", "cd"]);
    let at_end = shaped_revealing(source, caret_at(source, 3), None);
    assert_eq!(shown_text(&at_end[0]), vec!["ab\\", "cd"]);
    let next_row = shaped_revealing(source, caret_at(source, 4), None);
    assert_eq!(shown_text(&next_row[0]), vec!["ab", "cd"]);
}

/// A caret beside an escape opens that escape and nothing else; a caret in
/// the span beside it opens the span and leaves the escape concealed.
#[test]
fn an_escape_and_the_span_beside_it_reveal_apart() {
    let source = "\\***a**";
    let escape = shaped_revealing(source, caret_at(source, 1), None);
    assert_eq!(shown_text(&escape[0]), vec!["\\*a"]);
    let span = shaped_revealing(source, caret_at(source, 5), None);
    assert_eq!(shown_text(&span[0]), vec!["***a**"]);
}

/// Nested spans open by their own extent: a caret in the inner one opens
/// both, one only in the outer one opens just the outer pair.
#[test]
fn nested_spans_reveal_by_their_own_extent() {
    let source = "*a **b** c*";
    let inner = shaped_revealing(source, caret_at(source, 5), None);
    assert_eq!(shown_text(&inner[0]), vec!["*a **b** c*"]);
    let outer = shaped_revealing(source, caret_at(source, 9), None);
    assert_eq!(shown_text(&outer[0]), vec!["*a b c*"]);
}

/// Marked text inside an entity's span opens it, the way it opens a style.
#[test]
fn marked_text_opens_an_entity() {
    let source = "a &amp; b";
    let marked = caret_at(source, 4);
    let rows = shaped_revealing(source, 0..0, Some(marked.start..marked.end + 1));
    assert_eq!(shown_text(&rows[0]), vec!["a &amp; b"]);
}

/// The shaping cache's key moves when what is revealed moves, and only
/// then: a caret walking within one span, or outside every span, keeps the
/// rows it was shaped with.
#[test]
fn the_reveal_key_changes_only_with_the_revealed_set() {
    let source = "x **ab** y &amp; z";
    let state = state_of(source);
    let projection = projection_of(&state);
    let images = crate::images::Images::default();
    let style = EditorStyle::notes();
    let types = callout_types();
    let key = |offset: usize, composition: Option<usize>| {
        let pos = |offset| projection.lines()[0].offset_to_pos(offset).unwrap();
        let input = ShapeInput {
            images: &images,
            spelling: None,
            wiki: None,
            doc: state.doc(),
            types: &types,
            projection: &projection,
            style: &style,
            single_line: false,
            selection: pos(offset)..pos(offset),
            composition: composition.map(|offset| pos(offset)..pos(offset) + 1),
        };
        super::reveal_key(&input)
    };
    assert_eq!(key(0, None), key(1, None), "outside every span");
    assert_eq!(key(3, None), key(5, None), "inside the same span");
    assert_eq!(key(2, None), key(8, None), "at either edge of it");
    assert_ne!(key(1, None), key(2, None), "onto the span's edge");
    assert_ne!(key(5, None), key(12, None), "from the span to the entity");
    assert_ne!(
        key(0, None),
        key(0, Some(12)),
        "marked text opens the entity"
    );
    assert_eq!(
        key(0, Some(9)),
        key(1, None),
        "marked text outside every span"
    );
}

thread_local! {
    /// How many lines this thread has run through `shape_line`.
    pub(super) static SHAPED_LINES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// A document with most of what a line's shaping reads from around it: a
/// heading at the top, a wiki link the host can and one it cannot open, an
/// ordered list one item short of a wider number, a callout, a quote and a
/// code block.
const REUSED: &str = "# Title\n\nA [[Target]] and a [[Missing]] link, `code` and ^sup^.\n\nSecond paragraph to edit.\n\n1. one\n2. two\n3. three\n4. four\n5. five\n6. six\n7. seven\n8. eight\n9. nine\n\n> [!note] Heads up\n> Callout body\n\n> quote one\n>\n> quote two\n\nOutside\n\n```rust\nlet x = 1;\n```\n\nLast paragraph.";

/// `state` laid out through `lines`, which keeps what it can of the document
/// it laid out before, and how many lines were shaped to do it.
fn shape_state(state: &EditorState, lines: &mut super::Lines) -> (Vec<LayoutLine>, usize) {
    sync_state(state, lines, true)
}

/// `state` lined up in `lines`, and every line laid out when `lay_out` says.
fn sync_state(
    state: &EditorState,
    lines: &mut super::Lines,
    lay_out: bool,
) -> (Vec<LayoutLine>, usize) {
    let projection = projection_of(state);
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    // The two gaps differ, so a gap kept from the wrong neighbour shows.
    let style = EditorStyle {
        list_gap: px(3.),
        ..EditorStyle::notes()
    };
    let types = callout_types();
    let wiki: crate::WikiResolver = Box::new(|target| target != "Missing");
    let doc = state.doc();
    let selection = state.selection();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: Some(&wiki),
        doc,
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection: selection.from(doc)..selection.to(doc),
        composition: None,
    };
    let before = SHAPED_LINES.with(|count| count.get());
    lines.sync(&input, &projection, px(600.), 0);
    if !lay_out {
        return (Vec::new(), 0);
    }
    lines.lay_out_range(&input, 0..lines.len(), &text_system());
    (lines.all(), SHAPED_LINES.with(|count| count.get()) - before)
}

/// A grid reads nothing outside itself, so typing beside a table leaves its
/// cells' heights measured, and only the line typed into is estimated again.
#[test]
fn an_edit_beside_a_table_keeps_its_cells_measured() {
    let state = state_of("| a | b |\n|---|---|\n| 1 | 2 |\n\nOutside");
    let state = at(&state, end_of(&state, "Outside"));
    let mut lines = super::Lines::default();
    shape_state(&state, &mut lines);
    assert_eq!(lines.estimated(), 0);
    let typed = run(&state, &insert_text("!"));
    sync_state(&typed, &mut lines, false);
    assert_eq!(lines.estimated(), 1, "only the line typed into");
}

/// Everything a laid-out line holds that can be compared, as text.
fn fingerprint(line: &LayoutLine) -> String {
    use std::fmt::Write;
    let text = |shaped: &Option<std::rc::Rc<gpui::ShapedLine>>| {
        shaped
            .as_ref()
            .map(|shaped| (shaped.text.to_string(), shaped.width))
    };
    let mut out = format!(
        "{} {:?} {}..{} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
        line.index,
        line.source.start(),
        line.from,
        line.to(),
        line.char_len,
        line.origin,
        line.line_height,
        line.height,
        line.width,
        line.min_width,
        line.top_gap,
        line.code_pos,
    );
    for row in &line.rows {
        write!(
            out,
            "\n row {:?} {} {} {} {:?}",
            row.text(),
            row.char_start,
            row.visual_start,
            row.visual_rows(),
            row.line.size(line.line_height),
        )
        .unwrap();
        for code in &row.inline_code {
            write!(
                out,
                " code {:?} {} {:?} {:?} {:?} {} {:?}",
                code.range,
                code.visual_row,
                code.left,
                code.slot,
                code.line.text,
                code.raised,
                code.lift,
            )
            .unwrap();
        }
    }
    let marker = match &line.marker {
        None => "none".to_owned(),
        Some(Marker::Number(label)) => format!("number {:?}", label.text),
        Some(Marker::Bullet { depth }) => format!("bullet {depth}"),
        Some(Marker::Task { checked, number }) => {
            format!("task {checked} {:?}", text(number))
        }
        Some(Marker::Footnote(label)) => format!("footnote {:?}", label.text),
    };
    let decoration = match &line.decoration {
        None => "none".to_owned(),
        Some(Decoration::Quote {
            levels,
            joined,
            tones,
        }) => format!("quote {levels} {joined} {tones:?}"),
        Some(Decoration::Divider) => "divider".to_owned(),
        Some(Decoration::Code { levels: 0, .. }) => "code".to_owned(),
        Some(Decoration::Code {
            levels,
            joined,
            tones,
        }) => format!("code quote {levels} {joined} {tones:?}"),
    };
    write!(
        out,
        "\n marker {marker}\n decoration {decoration}\n chrome {:?} {:?}",
        text(&line.code_language),
        line.callout_header
            .as_ref()
            .map(|header| (header.text.clone(), header.label.text.to_string())),
    )
    .unwrap();
    for widening in &line.widenings {
        write!(
            out,
            "\n widening {} {} {} {} {:?} {}",
            widening.source,
            widening.source_len,
            widening.display,
            widening.len,
            widening.shape,
            widening.broken,
        )
        .unwrap();
    }
    for atom in &line.atoms {
        write!(
            out,
            "\n atom {:?} {:?} {} {:?} {} {}",
            atom.left,
            atom.slot,
            atom.visual_row,
            atom.label.text,
            atom.image.is_some(),
            atom.note,
        )
        .unwrap();
    }
    if let Some(cell) = &line.table {
        write!(
            out,
            "\n cell {} {} {} {} {} {:?} {} {:?} {:?}",
            cell.table,
            cell.row,
            cell.column,
            cell.rows,
            cell.columns,
            cell.alignment,
            cell.quotes,
            cell.offset,
            cell.size,
        )
        .unwrap();
    }
    out
}

/// Shape `state` through `lines`, keeping what it can, check the result is
/// what shaping it from nothing gives, and say how many lines it shaped.
fn reshaped(state: &EditorState, lines: &mut super::Lines) -> (Vec<LayoutLine>, usize) {
    let (kept, shaped) = shape_state(state, lines);
    let (fresh, _) = shape_state(state, &mut super::Lines::default());
    assert_eq!(kept.len(), fresh.len());
    for (kept, fresh) in kept.iter().zip(&fresh) {
        assert!(kept.source == fresh.source, "line {}", fresh.index);
        assert_eq!(fingerprint(kept), fingerprint(fresh));
    }
    (kept, shaped)
}

/// The position at the end of the line whose text is `text`.
fn end_of(state: &EditorState, text: &str) -> usize {
    let projection = projection_of(state);
    let index = (0..projection.line_count())
        .find(|&index| projection.line_text(index) == Some(text))
        .expect("a line with that text");
    projection.lines()[index].to()
}

/// Typing a character reshapes the line it went into and nothing else, and
/// what it keeps is what shaping the whole document again would draw.
#[test]
fn typing_reshapes_only_the_line_it_types_into() {
    let state = state_of(REUSED);
    let state = at(&state, end_of(&state, "Outside"));
    let mut lines = super::Lines::default();
    let (first, shaped) = reshaped(&state, &mut lines);
    assert_eq!(shaped, first.len(), "nothing to keep yet");

    let caret = end_of(&state, "Second paragraph to edit.");
    let state = at(&state, caret);
    let (moved, shaped) = reshaped(&state, &mut lines);
    assert_eq!(shaped, 2, "the line the caret left, and the one it entered");

    let state = run(&state, &insert_text("!"));
    let (typed, shaped) = reshaped(&state, &mut lines);
    assert_eq!(shaped, 1, "only the line typed into");
    // Every line after it moved one position along and was still kept.
    let edited = typed
        .iter()
        .position(|line| line.to() == caret + 1)
        .expect("the edited line");
    assert_eq!(typed[edited + 1].from, moved[edited + 1].from + 1);

    // Deleting it again moves them all back.
    let state = run(
        &state,
        &markraft_core::commands::delete_range(caret, caret + 1),
    );
    let (_, shaped) = reshaped(&state, &mut lines);
    assert_eq!(shaped, 1);
}

/// What a line reads from around it is part of what it is kept under: a
/// tenth item widens every number of the list, a list's last item gets the
/// list gap when the list goes on below it, and a table and a picture are
/// always shaped again.
#[test]
fn a_line_is_reshaped_when_what_it_reads_around_it_changes() {
    let source = format!("{REUSED}\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![alt](missing.png)");
    let state = state_of(&source);
    let mut lines = super::Lines::default();
    reshaped(&state, &mut lines);

    // Enter at the end of the ninth item makes a tenth.
    let types = callout_types();
    let state = at(&state, end_of(&state, "nine"));
    let (before, _) = reshaped(&state, &mut lines);
    let item = types.list_item.expect("a list item type");
    let state = run(&state, &markraft_core::commands::split_list_item(item));
    let (after, shaped) = reshaped(&state, &mut lines);
    // The two halves of the split item, the eight items before it whose
    // number widened, four cells shaped twice each and the picture.
    assert_eq!(shaped, 2 + 8 + 8 + 1);
    assert_ne!(
        row_of(&before, "one").map(|line| line.origin.x),
        row_of(&after, "one").map(|line| line.origin.x),
        "the list's indent follows its widest number"
    );

    // Listing the paragraph under a list: the gap under the list's last
    // item becomes the list's own.
    let state = state_of("- a\n- b\n\npara");
    let state = at(&state, end_of(&state, "para"));
    let mut lines = super::Lines::default();
    let (before, _) = reshaped(&state, &mut lines);
    let list = state
        .schema()
        .node_id("bullet_list")
        .expect("a bullet list type");
    let state = run(
        &state,
        &markraft_core::commands::wrap_in(list, Default::default()),
    );
    let (after, _) = reshaped(&state, &mut lines);
    let above = |lines: &[LayoutLine]| row_of(lines, "b").map(|line| line.height);
    assert!(above(&before).is_some());
    assert_ne!(above(&before), above(&after));
}

/// The laid-out line whose first row reads `text`.
fn row_of<'a>(lines: &'a [LayoutLine], text: &str) -> Option<&'a LayoutLine> {
    lines
        .iter()
        .find(|line| line.rows.first().is_some_and(|row| row.text() == text))
}

/// Two ends of a list of lines kept from a shaping: the lines before an
/// edit and the lines after it, never overlapping.
#[test]
fn kept_ends_meet_but_never_overlap() {
    let state = state_of("a\n\nb\n\nc");
    let projection = projection_of(&state);
    let lines = projection.lines();
    let kept_ends = |before, now| {
        let kept = super::lines::kept_ends(before, now);
        (kept.prefix(), kept.suffix())
    };
    assert_eq!(kept_ends(lines, projection.lines()), (3, 0));
    assert_eq!(kept_ends(&[], projection.lines()), (0, 0));
    // A document of other lines altogether shares nothing.
    let other = projection_of(&state_of("a\n\nb\n\nc"));
    assert_eq!(kept_ends(lines, other.lines()), (0, 0));
    // Typing into the middle line keeps one line at either end.
    let typed = run(&at(&state, end_of(&state, "b")), &insert_text("!"));
    assert_eq!(kept_ends(lines, projection_of(&typed).lines()), (1, 1));
}

fn shaped_revealing(
    source: &str,
    selection: std::ops::Range<usize>,
    composition: Option<std::ops::Range<usize>>,
) -> Vec<LayoutLine> {
    let state = state_of(source);
    let projection = projection_of(&state);
    let images = crate::images::Images::default();
    let spelling = markraft_commonmark::CommonMarkSpelling::new(state.schema().clone());
    let style = EditorStyle::notes();
    let types = callout_types();
    let input = ShapeInput {
        images: &images,
        spelling: Some(&spelling),
        wiki: None,
        doc: state.doc(),
        types: &types,
        projection: &projection,
        style: &style,
        single_line: false,
        selection,
        composition,
    };
    shape(&input, px(600.), &text_system())
}

/// The caret's cell frame lies on the grid lines round the cell: its own
/// separators run inside its right and bottom edges, its neighbours' outside
/// its left and top, and the outer border inside the grid's first row and
/// column.
#[test]
fn the_caret_cell_frame_covers_the_grid_lines_it_borders() {
    let cell = |row, column| TableCell {
        table: 0,
        row,
        column,
        rows: 3,
        columns: 3,
        alignment: ColumnAlignment::None,
        quotes: 0,
        offset: point(px(0.), px(0.)),
        size: size(px(40.), px(20.)),
    };
    let bounds = Bounds::new(point(px(100.), px(50.)), size(px(40.), px(20.)));
    let frame = |row, column| {
        let frame = caret_cell_frame(cell(row, column), bounds);
        (frame.left(), frame.top(), frame.right(), frame.bottom())
    };
    let (left, top) = (px(100.) - TABLE_LINE, px(50.) - TABLE_LINE);
    assert_eq!(
        frame(0, 0),
        (px(100.), px(50.), px(140.), px(70.)),
        "on the outer border"
    );
    assert_eq!(
        frame(0, 1),
        (left, px(50.), px(140.), px(70.)),
        "over the line to its left"
    );
    assert_eq!(
        frame(1, 0),
        (px(100.), top, px(140.), px(70.)),
        "over the line above"
    );
    assert_eq!(frame(2, 2), (left, top, px(140.), px(70.)), "over both");
}

/// The index search reads is the line the reader sees, once the fillers a
/// pill reserves are taken back out.
#[test]
fn shown_text_matches_the_shaped_line_apart_from_pill_fillers() {
    let source = "x **ab** &amp; [[a/b|Alias]] :smile:\n";
    let types = callout_types();
    let rows = shaped_in(source, types.clone(), px(600.), 0..0);
    let state = state_of(source);
    let projection = projection_of(&state);
    let shown = crate::shown::ShownText::build(
        &projection,
        &types,
        &markraft_core::kind::conceal::Reveal::nothing(),
    );
    let displayed = rows
        .iter()
        .map(|row| {
            row.rows
                .iter()
                .map(|inner| inner.text().replace('\u{00a0}', ""))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(displayed, shown.text());
}

/// A hit's rectangles land on the line that holds it, and a later cell's
/// hit sits to the right of an earlier one.
#[test]
fn a_find_hit_has_a_rectangle_on_its_line() {
    let source = "alpha **bold** and bold\n\n| left | right |\n| - | - |\n| bold | other |\n";
    let types = callout_types();
    let rows = shaped_in(source, types.clone(), px(420.), 0..0);
    let state = state_of(source);
    let projection = projection_of(&state);
    let hits = crate::shown::ShownText::build(
        &projection,
        &types,
        &markraft_core::kind::conceal::Reveal::nothing(),
    )
    .matches("bold");
    assert!(hits.len() >= 2);
    let mut origins = Vec::new();
    for hit in &hits {
        let row = rows
            .iter()
            .find(|row| hit.start < row.to() && hit.end > row.from)
            .expect("a row holds the hit");
        let from = row.pos_to_offset(hit.start);
        let to = row.pos_to_offset(hit.end);
        let rects = row.rectangles(from..to.min(row.char_len), hit.end > row.to());
        assert!(!rects.is_empty(), "the hit is drawn");
        origins.push(rects[0].origin);
    }
    let paragraph = origins[0];
    let cell = origins
        .iter()
        .find(|origin| origin.y > paragraph.y)
        .copied();
    if let Some(cell) = cell {
        assert!(cell.y > paragraph.y);
    }
    let others = crate::shown::ShownText::build(
        &projection,
        &types,
        &markraft_core::kind::conceal::Reveal::nothing(),
    )
    .matches("other");
    let other = others.first().expect("the other cell");
    let row = rows
        .iter()
        .find(|row| other.start < row.to() && other.end > row.from)
        .expect("a row holds the other cell");
    let from = row.pos_to_offset(other.start);
    let to = row.pos_to_offset(other.end);
    let rects = row.rectangles(from..to.min(row.char_len), other.end > row.to());
    let bold_cell = origins
        .iter()
        .find(|origin| (origin.y - rects[0].origin.y).abs() < px(1.))
        .copied();
    if let Some(bold_cell) = bold_cell {
        assert!(rects[0].origin.x > bold_cell.x);
    }
}
