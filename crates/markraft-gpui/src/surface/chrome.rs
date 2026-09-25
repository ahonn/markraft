//! Block chrome: indents, list markers, quote bars, block gaps and callout
//! headers — what stands around a line's text. Shaping asks it for the
//! insets and gaps; paint draws what it describes.

use super::*;

/// How far a line may be indented before its text would be squeezed away.
///
/// Keeps deeply nested imported content editable in a narrow note. Only the
/// visual indentation is capped; document depth is preserved.
pub(super) fn max_indent(style: &EditorStyle, width: Pixels) -> Pixels {
    (width - px(80.)).max(style.quote_indent)
}

/// How many indentation levels fit before the text would be squeezed away.
pub(super) fn visible_levels(style: &EditorStyle, max_indent: Pixels) -> usize {
    ((max_indent / style.quote_indent.max(px(1.))) as usize).max(1)
}

pub(super) fn indent_of(
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    marker_reserve: Option<Pixels>,
) -> Pixels {
    let mut indent = (0..line.ancestors().len())
        .map(|index| ancestor_indent(types, line, index, style))
        .fold(px(0.), |sum, step| sum + step);
    if let Some(reserve) = marker_reserve {
        // An ordered list's items share the indent its widest number needs.
        indent += (reserve - style.list_indent).max(px(0.));
    }
    indent
}

/// The indent the line's `index`th ancestor adds to everything inside it.
pub(super) fn ancestor_indent(
    types: &DocTypes,
    line: &Line,
    index: usize,
    style: &EditorStyle,
) -> Pixels {
    let ancestors = line.ancestors();
    let ty = ancestors[index].node_type;
    if Some(ty) == types.blockquote {
        style.quote_indent
    } else if types.is_list(ty) {
        style.list_indent
    } else if Some(ty) == types.task_item
        && index > 0
        && Some(ancestors[index - 1].node_type) == types.ordered_list
    {
        // Nested blocks and lists keep the checkbox slot of every enclosing item.
        px(22.)
    } else if Some(ty) == types.code_block {
        CODE_PADDING
    } else if Some(ty) == types.footnote_definition {
        style.list_indent
    } else {
        px(0.)
    }
}

/// The indent the containers inside the line's innermost item add, which the
/// item's marker is drawn to the left of.
pub(super) fn inside_item_indent(types: &DocTypes, line: &Line, style: &EditorStyle) -> Pixels {
    let Some(item) = line
        .ancestors()
        .iter()
        .rposition(|ancestor| types.is_item(ancestor.node_type))
    else {
        return px(0.);
    };
    (item + 1..line.ancestors().len())
        .map(|index| ancestor_indent(types, line, index, style))
        .fold(px(0.), |sum, step| sum + step)
}

/// How far left of the line's text each quote it sits in draws its bar,
/// outermost first: the quote's own indent and all the indent nested inside
/// it — a list's, a code panel's — so a bar stays at its
/// quote's edge rather than at the text's, wherever in the quote the line is.
pub(super) fn quote_bar_distances(
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    marker_reserve: Option<Pixels>,
) -> Vec<Pixels> {
    let ancestors = line.ancestors();
    let reserve =
        marker_reserve.map_or(px(0.), |reserve| (reserve - style.list_indent).max(px(0.)));
    (0..ancestors.len())
        .filter(|&index| Some(ancestors[index].node_type) == types.blockquote)
        .map(|quote| {
            let inside = (quote..ancestors.len())
                .map(|index| ancestor_indent(types, line, index, style))
                .fold(px(0.), |sum, step| sum + step);
            // An ordinal's extra room belongs to the item it numbers, which is
            // inside this quote when a list or a note is.
            let numbered = ancestors[quote + 1..].iter().any(|ancestor| {
                types.is_list(ancestor.node_type)
                    || Some(ancestor.node_type) == types.footnote_definition
            });
            inside + if numbered { reserve } else { px(0.) }
        })
        .collect()
}

/// The shaped ordinal and width every line of an ordered-list item reserves.
/// Only its first line paints the ordinal, but continuation lines align with it.
pub(super) fn ordered_marker(
    doc: &Node,
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    font_size: Pixels,
    text_system: &WindowTextSystem,
) -> Option<(Rc<ShapedLine>, Pixels)> {
    let (item, list) = types.item_of(line)?;
    let count = ordered_list_len(doc, types, line)?;
    let start = list
        .attrs
        .get("start")
        .and_then(|value| value.as_int())
        .unwrap_or(1);
    let shape_number = |ordinal: i64| {
        let text = format!("{ordinal}.");
        Rc::new(text_system.shape_line(
            text.clone().into(),
            font_size,
            &[TextRun {
                len: text.len(),
                font: font(style.font_family.clone()),
                color: style.marker,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ))
    };
    let widest = shape_number(start + count.saturating_sub(1) as i64).width;
    Some((shape_number(start + item.index as i64), widest))
}

/// The shaped label of the footnote definition a line sits in, and the width
/// every line of it reserves. Only its first line paints the label, but the
/// others align with it, as an ordered item's do with its number.
pub(super) fn footnote_marker(
    types: &DocTypes,
    line: &Line,
    style: &EditorStyle,
    font_size: Pixels,
    text_system: &WindowTextSystem,
) -> Option<(Rc<ShapedLine>, Pixels)> {
    let definition = types.footnote_of(line)?;
    let label = definition
        .attrs
        .get(markraft_core::kind::FOOTNOTE_LABEL_ATTR)
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let text = format!("[{label}]");
    let shaped = Rc::new(text_system.shape_line(
        text.clone().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: font(style.font_family.clone()),
            color: style.link,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    ));
    let width = shaped.width;
    Some((shaped, width))
}

/// How many items the ordered list a line's item sits in holds, which is the
/// one thing about the line's numbering its own ancestors do not say.
pub(super) fn ordered_list_len(doc: &Node, types: &DocTypes, line: &Line) -> Option<usize> {
    let (item, list) = types.item_of(line)?;
    if Some(list.node_type) != types.ordered_list {
        return None;
    }
    let list_before = line.ancestor_before(types.item_index(line)? - 1);
    Some(
        doc.node_at(list_before)
            .map_or(item.index + 1, |node| node.child_count()),
    )
}

/// Whether a line is its list item's very first line, which is where the marker
/// is drawn.
///
/// Every ancestor between the item and the line has to be the first child of
/// the one above it, not just the line's own block: a table or a quote inside an
/// item is a container whose own first block starts it, and asking only about
/// the immediate parent drew a bullet beside every row of such a table and a
/// second check box beside such a quote.
pub(super) fn starts_item(types: &DocTypes, line: &Line) -> bool {
    let Some(item) = line
        .ancestors()
        .iter()
        .rposition(|ancestor| types.is_item(ancestor.node_type))
    else {
        return false;
    };
    item + 1 < line.ancestors().len()
        && line.ancestors()[item + 1..]
            .iter()
            .all(|ancestor| ancestor.index == 0)
}

/// Whether this line has anything to draw differently while it is focused.
///
/// A code block does, showing its language tag, and a line spelling out a
/// picture, drawing it under its source: every other block draws its marker,
/// bar or heading the same wherever the caret is, as Typora does. So the
/// caret passing through any other line does not invalidate the shaped rows.
pub(super) fn focus_chrome(input: &ShapeInput<'_>, line: &Line) -> bool {
    input.types.is_code_block(line)
        || input
            .spelling
            .is_some_and(|spelling| !spelling.spelled_atoms(line).is_empty())
}

/// Whether the selection or composition touches this projection line.
pub(super) fn line_focused(input: &ShapeInput<'_>, line: &Line) -> bool {
    affinity_touches(input, line.from(), line.to())
}

pub(super) fn affinity_touches(input: &ShapeInput<'_>, from: usize, to: usize) -> bool {
    let touches = |range: &Range<usize>| {
        if range.start == range.end {
            range.start >= from && range.start <= to
        } else {
            range.start < to && range.end > from
        }
    };
    touches(&input.selection) || input.composition.as_ref().is_some_and(touches)
}

/// Shapes a label the view draws beside the text — a code block's language
/// tag — in `face` and `color`.
pub(super) fn shape_source_label(
    text: &str,
    face: Font,
    font_size: Pixels,
    color: Hsla,
    text_system: &WindowTextSystem,
) -> Rc<ShapedLine> {
    Rc::new(text_system.shape_line(
        text.to_owned().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: face,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    ))
}

/// The gutter marker a line draws: the bullet, ordinal or check box of the
/// list item it opens.
///
/// It is the same wherever the caret is. As in Typora, a list item never shows
/// the `- `, `1. ` or `- [ ] ` it is spelled with, nor a heading its hashes:
/// they are not text the caret can reach, so showing them would only suggest
/// an edit that cannot be made, and swapping a drawn marker for its spelling
/// as the caret came and went moved the line's text under it.
pub(super) fn chrome_marker(
    types: &DocTypes,
    line: &Line,
    number: Option<Rc<ShapedLine>>,
) -> Option<Marker> {
    let (item, list) = types.item_of(line)?;
    if !starts_item(types, line) {
        return None;
    }
    if Some(item.node_type) == types.task_item {
        return Some(Marker::Task {
            checked: DocTypes::task_checked(&item.attrs),
            number,
        });
    }
    if let Some(number) = number {
        return Some(Marker::Number(number));
    }
    if Some(list.node_type) != types.bullet_list {
        return None;
    }
    Some(Marker::Bullet {
        depth: types.list_depth(line),
    })
}

/// How many of a line's quote levels continue into the line below.
pub(super) fn joined_quote_levels(
    projection: &Projection,
    index: usize,
    types: &DocTypes,
) -> usize {
    let Some(next) = projection.line(index + 1) else {
        return 0;
    };
    let line = &projection.lines()[index];
    line.ancestors()
        .iter()
        .zip(next.ancestors().iter())
        .enumerate()
        .take_while(|(depth, (a, b))| {
            line.ancestor_before(*depth) == next.ancestor_before(*depth)
                && a.node_type == b.node_type
        })
        .filter(|(_, (a, _))| Some(a.node_type) == types.blockquote)
        .count()
}

/// Whether `next` is a line of the same list as `line` — of its outermost
/// list, so a nested list's lines go on with their parent's.
pub(super) fn list_continues(types: &DocTypes, line: &Line, next: Option<&Line>) -> bool {
    let outermost = |line: &Line| {
        line.ancestors()
            .iter()
            .position(|ancestor| types.is_list(ancestor.node_type))
            .map(|depth| line.ancestor_before(depth))
    };
    next.is_some_and(|next| types.item_of(next).is_some() && outermost(next) == outermost(line))
}

/// Whether `next` opens a table, which keeps its toolbar's row above itself.
pub(super) fn opens_table(types: &DocTypes, next: Option<&Line>) -> bool {
    next.and_then(|next| types.table_cell_of(next))
        .is_some_and(|(_, row, column)| row == 0 && column == 0)
}

pub(super) fn gap_below(
    input: &ShapeInput<'_>,
    index: usize,
    line: &Line,
    heading: Option<u8>,
    code: bool,
    marker: &Option<Marker>,
) -> Pixels {
    if input.single_line {
        return px(0.);
    }
    let next = input.projection.line(index + 1);
    // The row a table keeps for its toolbar is part of the gap above it, so
    // the bars of a quote around both reach over it as over any gap.
    let toolbar = if opens_table(input.types, next) {
        input.style.table_toolbar_room
    } else {
        px(0.)
    };
    block_gap(input, line, next, heading, code, marker) + toolbar
}

/// The space a block keeps below itself before the next one.
pub(super) fn block_gap(
    input: &ShapeInput<'_>,
    line: &Line,
    next: Option<&Line>,
    heading: Option<u8>,
    code: bool,
    marker: &Option<Marker>,
) -> Pixels {
    let style = input.style;
    // The code fill reaches CODE_INSET past the text (`shape_line` adds it on
    // top of this gap), so a code block keeps its own bottom padding whatever
    // block follows it.
    if code {
        return style.paragraph_gap;
    }
    // A table is one block: its cells sit tight against each other, and only
    // the cell that closes the grid is spaced off what follows it.
    if let Some((table, _, _)) = input.types.table_cell_of(line) {
        let continues = next
            .and_then(|next| input.types.table_cell_of(next))
            .is_some_and(|(below, _, _)| below == table);
        return if continues {
            px(0.)
        } else {
            style.paragraph_gap
        };
    }
    // The tighter list gap holds between the lines of a list. The block that
    // closes one is spaced off it like any other pair of blocks, and so is a
    // second list that starts right after it.
    if marker.is_some() || input.types.item_of(line).is_some() {
        return if list_continues(input.types, line, next) {
            style.list_gap
        } else {
            style.paragraph_gap
        };
    }
    if heading.is_some() {
        return style.heading_bottom_gap;
    }
    // A quote holds blocks like the note does, spaced the same: its bars reach
    // over the gaps between them, so the spacing never breaks a bar.
    style.paragraph_gap
}

/// A callout's header label, shaped in the accent of its tone: the sentence
/// face at the body size, in the weight that says it names the note rather
/// than being part of it.
pub(super) fn callout_label(
    label: &str,
    tone: Hsla,
    style: &EditorStyle,
    text_system: &WindowTextSystem,
) -> Rc<ShapedLine> {
    let mut face = font(style.font_family.clone());
    face.weight = FontWeight::BOLD;
    let text: SharedString = label.to_owned().into();
    Rc::new(text_system.shape_line(
        text.clone(),
        style.body_size,
        &[TextRun {
            len: text.len(),
            font: face,
            color: tone,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    ))
}
