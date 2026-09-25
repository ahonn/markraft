//! Shaping: one projection line into one [`LayoutLine`], reusing the rows
//! of the previous frame where nothing about a line changed. Takes the
//! [`ShapeInput`] the view assembles; hands [`LayoutLine`]s to tables and paint.

use super::*;

/// Everything shaping needs that is not the window.
pub(crate) struct ShapeInput<'a> {
    pub images: &'a crate::images::Images,
    pub doc: &'a Node,
    pub types: &'a DocTypes,
    pub projection: &'a Projection,
    pub style: &'a EditorStyle,
    pub single_line: bool,
    /// Whether a wiki link target names something the host can open. Only the host
    /// knows, and one that has not said treats every link as followable.
    pub wiki: Option<&'a crate::WikiResolver>,
    /// How the host's kind spells the parts of itself a focused line shows as
    /// source. Without it a line is drawn the same focused or not.
    pub spelling: Option<&'a dyn markraft_core::kind::SourceSpelling>,
    /// Document selection range, which is what reveals a syntax run: a caret
    /// reveals a run it touches at either edge, a range one it overlaps.
    pub selection: Range<usize>,
    /// An input method's marked range, which reveals delimiters the way the
    /// selection does.
    pub composition: Option<Range<usize>>,
}

/// How wide a table cell is shaped.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum CellWidth {
    /// The measuring pass: nothing constrains the text, so the line's width
    /// comes out as the width its content wants and the grid can size the
    /// column from it.
    Natural,
    /// The laying-out pass: the content box the cell's column gave it, so text
    /// too long for its column wraps inside the cell.
    Column(Pixels),
}

/// The document's rows, shaped again only when shaping would answer differently.
///
/// Every frame asks for these twice — once to measure the note's height, once to
/// draw it — and a redraw is usually about something shaping never reads: the
/// caret blinked, the selection moved, a popup opened above. Shaping walks every
/// line of the document and asks the platform to lay out its text, so repeating
/// it for those frames is the single largest avoidable cost in the view.
///
/// What it reads is the projection (which stands for the document), the width,
/// and the inputs [`Shaping`](crate::shaping::Shaping) holds. An image file that
/// changed under the editor reaches those through
/// [`EditorView::refresh_images`], which the host polls.
///
/// A redraw after an edit does shape again, but only the lines the edit
/// reached: the rest are kept from the last shaping at this width; see
/// [`shape_reusing`].
pub(crate) fn shape_cached(
    view: &crate::EditorView,
    width: Pixels,
    text_system: &WindowTextSystem,
) -> Vec<LayoutLine> {
    let projection = view.projection_arc();
    let input = view.shape_input();
    let reveal = reveal_key(&input);
    let shaping = view.shaping();
    if let Some(lines) = shaping.rows(projection, width, reveal) {
        return lines;
    }
    let lines = {
        let previous = shaping.previous(width);
        shape_reusing(
            &input,
            width,
            previous.as_deref().unwrap_or(&[]),
            text_system,
        )
    };
    shaping.keep(projection, width, reveal, &lines);
    lines
}

/// The width the text is laid out in: the view's content width, or
/// [`EditorStyle::max_line_width`] where that is narrower.
pub(crate) fn column_width(width: Pixels, max: Option<Pixels>) -> Pixels {
    max.map_or(width, |max| width.min(max.max(px(0.))))
}

/// The column the text stands in inside the content box `bounds`: as wide as
/// [`column_width`] says and centred across the box, over its whole height.
pub(crate) fn column_bounds(bounds: Bounds<Pixels>, max: Option<Pixels>) -> Bounds<Pixels> {
    let width = column_width(bounds.size.width, max);
    let inset = ((bounds.size.width - width) / 2.).round();
    Bounds::new(
        point(bounds.left() + inset, bounds.top()),
        size(width, bounds.size.height),
    )
}

/// Every line of the projection, shaped afresh.
#[cfg(test)]
pub(crate) fn shape(
    input: &ShapeInput<'_>,
    width: Pixels,
    text_system: &WindowTextSystem,
) -> Vec<LayoutLine> {
    shape_reusing(input, width, &[], text_system)
}

/// Every line of the projection, keeping the rows of `previous` wherever they
/// are what shaping the line again would produce.
///
/// `previous` has to have been shaped at `width` from the same inputs
/// [`Shaping`](crate::shaping::Shaping) holds; the document and the selection
/// it was shaped with may be any. A line of it is kept for a line of this
/// projection when the two share a body ([`Line::same_body`]) and were shaped
/// with the same [`LineKey`], and is then only moved to where the line now
/// starts ([`LayoutLine::moved_to`]).
///
/// Lines are paired by position from either end: an edit rebuilds one run of
/// lines and hands those before and after it over with their bodies, so the
/// kept prefix and suffix are exactly what can be kept. A pairing that is wrong
/// costs a reshape, never a stale row, since a kept row is checked against the
/// line it is kept for.
pub(crate) fn shape_reusing(
    input: &ShapeInput<'_>,
    width: Pixels,
    previous: &[LayoutLine],
    text_system: &WindowTextSystem,
) -> Vec<LayoutLine> {
    // The pictures the document shows: its atoms', and those of a picture the
    // caret has spelled out, which is text now but still drawn under its
    // source. Dropping the latter would throw away a remote fetch as soon as
    // it started, and decode a local file again on every frame.
    let spelled: Vec<Node> = input
        .spelling
        .map(|spelling| {
            input
                .projection
                .lines()
                .iter()
                .filter(|line| line_focused(input, line))
                .flat_map(|line| spelling.spelled_atoms(line))
                .map(|(_, node)| node)
                .collect()
        })
        .unwrap_or_default();
    input.images.retain_sources(
        input
            .projection
            .lines()
            .iter()
            .flat_map(|line| line.runs())
            .filter_map(|run| match &run.content {
                RunContent::Atom(node) => picture_source(input.types, node),
                _ => None,
            })
            .chain(
                spelled
                    .iter()
                    .filter_map(|node| picture_source(input.types, node)),
            ),
    );
    let current = input.projection.lines();
    let (prefix, suffix) = kept_ends(previous, current);
    let mut lines: Vec<LayoutLine> = current
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let key = line_key(input, index);
            let kept = if index < prefix {
                previous.get(index)
            } else if current.len() - index <= suffix {
                previous.get(previous.len() - (current.len() - index))
            } else {
                None
            };
            if let Some(kept) = kept
                .filter(|kept| key.is_some() && kept.reuse == key && kept.source.same_body(line))
            {
                return kept.moved_to(line, index);
            }
            let cell = table_cell(input, index).map(|_| CellWidth::Natural);
            let mut layout = shape_line(input, index, width, cell, text_system);
            layout.reuse = key;
            layout
        })
        .collect();
    shape_tables(input, &mut lines, width, text_system);
    lines
}

/// How many lines at the start and at the end of `current` have a line with
/// the same body at the same place from that end of `previous`. The two never
/// overlap in either list.
pub(super) fn kept_ends(previous: &[LayoutLine], current: &[Line]) -> (usize, usize) {
    let shorter = previous.len().min(current.len());
    let prefix = previous
        .iter()
        .zip(current)
        .take_while(|(kept, line)| kept.source.same_body(line))
        .count();
    let suffix = previous
        .iter()
        .rev()
        .zip(current.iter().rev())
        .take(shorter - prefix)
        .take_while(|(kept, line)| kept.source.same_body(line))
        .count();
    (prefix, suffix)
}

/// What shaping one line reads besides its own body, the width and the inputs
/// [`Shaping`](crate::shaping::Shaping) revises.
///
/// A body ([`Line::same_body`]) fixes the line's content, marks, ancestors and
/// their indices and attributes. What shaping reads past that is the lines
/// around it, the list it numbers in, and the host's answer about each wiki
/// link — and those are what this holds, as the values shaping reads rather
/// than as the neighbours they come from, so a neighbour that changed in a way
/// the line never looks at does not cost it a reshape.
///
/// A line gets no key, and is always shaped afresh, where shaping reads
/// something this cannot hold cheaply: a line the selection or marked text
/// touches (what it reveals, and the host's source spelling of it), a table
/// cell (whose size is settled by the whole grid), and a line with a picture
/// (whose file is read off the disk on every shaping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LineKey {
    /// The first line has no gap above a heading.
    pub(super) first: bool,
    /// How many items the ordered list the line numbers in holds, which sets
    /// the widest number and so the indent of every item.
    pub(super) list_len: Option<usize>,
    /// How many of the line's quote bars join the line below.
    pub(super) joined_quotes: usize,
    /// Whether the line below goes on with this line's list, which sets the
    /// gap below this one.
    pub(super) list_continues: bool,
    /// Whether a table opens below the line, which keeps its toolbar's row in
    /// the gap below this one.
    pub(super) table_below: bool,
    /// Whether the line opens a callout, which depends on the line above.
    pub(super) callout_header: bool,
    /// Whether the host can open each wiki link on the line, in order.
    pub(super) links: Vec<bool>,
}

/// The key the line at `index` is shaped under, or `None` where it has to be
/// shaped afresh every time; see [`LineKey`].
pub(super) fn line_key(input: &ShapeInput<'_>, index: usize) -> Option<LineKey> {
    let ShapeInput {
        types, projection, ..
    } = *input;
    let line = &projection.lines()[index];
    if line_focused(input, line) || types.table_cell_of(line).is_some() {
        return None;
    }
    let mut links = Vec::new();
    for run in line.runs() {
        let RunContent::Atom(node) = &run.content else {
            continue;
        };
        if picture_source(types, node).is_some() {
            return None;
        }
        if Some(node.type_id()) == types.wiki_link {
            let target = crate::wiki::wiki_link_target(node);
            links.push(input.wiki.is_none_or(|resolves| resolves(&target)));
        }
    }
    let next = projection.line(index + 1);
    let above = index.checked_sub(1).map(|above| &projection.lines()[above]);
    Some(LineKey {
        first: index == 0,
        list_len: ordered_list_len(input.doc, types, line),
        joined_quotes: joined_quote_levels(projection, index, types),
        list_continues: list_continues(types, line, next),
        table_below: opens_table(types, next),
        callout_header: crate::callout::header_of(types, line, above).is_some(),
        links,
    })
}

/// Which table, row and column a projection line is a cell of.
pub(super) fn table_cell(input: &ShapeInput<'_>, index: usize) -> Option<(usize, usize, usize)> {
    input.types.table_cell_of(input.projection.line(index)?)
}

pub(super) fn shape_line(
    input: &ShapeInput<'_>,
    index: usize,
    width: Pixels,
    cell: Option<CellWidth>,
    text_system: &WindowTextSystem,
) -> LayoutLine {
    #[cfg(test)]
    tests::SHAPED_LINES.with(|count| count.set(count.get() + 1));
    let ShapeInput {
        doc,
        types,
        projection,
        style,
        single_line,
        ..
    } = *input;
    let line = &projection.lines()[index];
    let heading = types.heading_level(line);
    let code = types.is_code_block(line);
    let font_size = style.font_size(heading, code);
    let max_indent = max_indent(style, width);
    let number = ordered_marker(doc, types, line, style, font_size, text_system);
    // A footnote's label takes the place an ordinal would, unless a list inside
    // the definition already numbers the line.
    let footnote = number
        .is_none()
        .then(|| footnote_marker(types, line, style, font_size, text_system))
        .flatten();
    let focused = line_focused(input, line);
    let marker = chrome_marker(
        types,
        line,
        number.as_ref().map(|(shaped, _)| shaped.clone()),
    )
    .or_else(|| {
        footnote
            .as_ref()
            .filter(|_| types.starts_footnote(line))
            .map(|(label, _)| Marker::Footnote(label.clone()))
    });
    let decoration = decoration_of(input, index, line, cell.is_some(), max_indent);
    // The room left of the text a drawn ordinal needs, standing off the text
    // by a gap.
    let marker_reserve = number
        .as_ref()
        .or(footnote.as_ref())
        .map(|(_, width)| *width + NUMBER_GAP);
    let indent = indent_of(types, line, style, marker_reserve).min(max_indent);
    let quote_bars = quote_bar_distances(types, line, style, marker_reserve);
    let marker_inset = if matches!(marker, Some(Marker::Footnote(_))) {
        px(0.)
    } else {
        inside_item_indent(types, line, style)
    };

    let wrap_width = match cell {
        Some(CellWidth::Column(content)) => content.max(px(16.)),
        // The measuring pass is unconstrained, but an atom still needs a nominal
        // column to size itself against.
        Some(CellWidth::Natural) => (width - indent).max(px(40.)),
        None => (width - indent - if code { CODE_PADDING } else { px(0.) }).max(px(40.)),
    };
    let unwrapped = single_line || cell == Some(CellWidth::Natural);
    let text = display_text(input, line, index, font_size, wrap_width, text_system);
    // A drawn image needs the whole row it was measured for.
    let line_height = text
        .line_height
        .unwrap_or(font_size * style.line_height_ratio);
    let runs = text_runs(input, line, &text, heading, code, font_size, style);
    let shaped = text_system
        .shape_text(
            text.text.clone().into(),
            font_size,
            &runs.runs,
            (!unwrapped).then_some(wrap_width),
            None,
        )
        .expect("valid UTF-8 text can be shaped");

    let mut rows = Vec::with_capacity(shaped.len());
    let mut char_start = 0usize;
    let mut visual_start = 0usize;
    for mut wrapped in shaped {
        if !unwrapped {
            keep_line_breaking_rules(&mut wrapped, wrap_width);
        }
        let row = LayoutRow {
            char_start,
            visual_start,
            inline_code: Vec::new(),
            line: Rc::new(wrapped),
        };
        char_start += row.char_len() + 1;
        visual_start += row.visual_rows();
        rows.push(row);
    }

    let gap = gap_below(input, index, line, heading, code, &marker);
    // A code block's panel reaches above its text, at the top of the document
    // as anywhere else. Everything else starts flush and only a heading claims
    // space.
    let code_inset = if code { CODE_INSET } else { px(0.) };
    let top_gap = if code {
        code_inset
    } else if index == 0 {
        px(0.)
    } else if let Some(level) = heading {
        style.heading_top_gap(level)
    } else {
        px(0.)
    };
    // The language box, as a tag: the block's language as written, or
    // what the language picker calls none.
    let code_language = (code && focused).then(|| {
        let language = line
            .ancestors()
            .last()
            .and_then(|block| block.attrs.get("language"))
            .and_then(|value| value.as_str())
            .filter(|language| !language.is_empty())
            .unwrap_or("Plain Text");
        shape_source_label(
            language,
            font(CODE_FONT),
            font_size - px(1.),
            style.muted_text,
            text_system,
        )
    });
    // A callout says what kind of note it is on a line of its own above the
    // block it opens. The line is chrome: it holds no caret stop, so it lives
    // in the room the block reserves above itself rather than in the text.
    // A picture whose source the caret was let into: the source is the line's
    // text now, and the picture stays in view under it — where the picture had
    // the line to itself, as it is drawn full size only there.
    let preview = (focused && cell.is_none() && !single_line)
        .then(|| {
            let atoms = input.spelling?.spelled_atoms(line);
            let [(range, node)] = atoms.as_slice() else {
                return None;
            };
            let text = projection.line_text(index)?;
            let alone = text
                .chars()
                .enumerate()
                .all(|(at, c)| range.contains(&at) || c.is_whitespace());
            let source = picture_source(types, node).filter(|_| alone)?;
            drawn_image(input.images, source, wrap_width)
        })
        .flatten();
    let above = index.checked_sub(1).map(|above| &projection.lines()[above]);
    let callout_header = crate::callout::header_of(types, line, above).map(|head| CalloutHeader {
        label: callout_label(
            &head.label,
            style.callout_tone(head.tone),
            style,
            text_system,
        ),
        text: head.label,
    });
    let top_gap = top_gap
        + if callout_header.is_some() {
            CALLOUT_HEADER_HEIGHT
        } else {
            px(0.)
        };

    let mut layout = LayoutLine {
        source: line.clone(),
        index,
        from: line.from(),
        char_len: line.len(),
        rows,
        origin: point(indent, px(0.)),
        line_height,
        height: px(0.),
        width: if single_line {
            wrap_width.max(px(0.))
        } else {
            wrap_width
        },
        min_width: px(0.),
        top_gap,
        code_pos: code.then(|| line.block_before()).flatten(),
        marker,
        decoration,
        code_language,
        code_inset,
        marker_inset,
        preview,
        quote_bars,
        callout_header,
        widenings: text.widenings,
        atoms: Vec::new(),
        table: None,
        reuse: None,
    };
    if single_line && let Some(row) = layout.rows.first() {
        layout.width = row.line.size(line_height).width.max(wrap_width);
    }
    if cell == Some(CellWidth::Natural) {
        // What the content wants, not what it was given: the grid reads each
        // column's preferred width off this, and how far the column may shrink
        // off the min-content width beside it.
        layout.width = layout
            .rows
            .iter()
            .map(|row| row.line.size(line_height).width)
            .fold(px(0.), |widest, width| widest.max(width));
        layout.min_width = min_content_width(&layout, &runs.code);
    }
    layout.height = layout.text_height()
        + layout.code_inset
        + layout
            .preview
            .as_ref()
            .map_or(px(0.), |(_, size)| PREVIEW_GAP + size.height)
        + gap;
    shape_inline_code(&mut layout, &runs.code, font_size, text_system);
    place_atoms(&mut layout, text.atoms);
    layout
}

/// The decoration drawn behind or beside a line: a code block's panel, the rule
/// of a thematic break, or one bar per block quote the line sits in. A raw block
/// has none — its source is shown as source, not fenced off as a panel.
///
/// A cell carries no block decoration of its own: the grid is the table's, and a
/// cell's own height is zero except on the last of its row, which a quote bar or
/// a panel has no way to draw against.
pub(super) fn decoration_of(
    input: &ShapeInput<'_>,
    index: usize,
    line: &Line,
    in_cell: bool,
    max_indent: Pixels,
) -> Option<Decoration> {
    let ShapeInput {
        types,
        projection,
        style,
        ..
    } = *input;
    let quote_levels = types.quote_depth(line);
    let levels = quote_levels.min(visible_levels(style, max_indent));
    // The bars drawn are the innermost `levels`, so the tones are too.
    let beside = crate::callout::tones_beside(types, line);
    let mut tones = [None; QUOTE_TONES];
    for (slot, tone) in tones
        .iter_mut()
        .zip(beside.iter().skip(beside.len().saturating_sub(levels)))
    {
        *slot = tone.map(|tone| style.callout_tone(tone));
    }
    let joined = || joined_quote_levels(projection, index, types).min(quote_levels);
    if in_cell {
        None
    } else if types.is_code_block(line) {
        Some(Decoration::Code {
            levels,
            joined: joined(),
            tones,
        })
    } else if types.horizontal_rule.is_some() && line.node_type() == types.horizontal_rule {
        Some(Decoration::Divider)
    } else if quote_levels > 0 {
        Some(Decoration::Quote {
            levels,
            joined: joined(),
            tones,
        })
    } else {
        None
    }
}

/// The narrowest a line's content can be drawn at without splitting a unit the
/// line wrapper keeps together.
///
/// The line has to have been shaped unwrapped, which is what the measuring pass
/// gives a cell: every row then sits on one visual row, so the advance between
/// two byte offsets is the width of the text between them. A unit wider than
/// [`CELL_MAX_MIN_CONTENT`] only counts for that much, so one very long word
/// cannot widen a column without bound.
pub(super) fn min_content_width(layout: &LayoutLine, code: &[Repaint]) -> Pixels {
    let mut widest = px(0.);
    for (index, row) in layout.rows.iter().enumerate() {
        let start = row_byte_start(layout, index);
        let text = row.text();
        // A code span is drawn as one pill; split over two rows of a cell it
        // reads as two spans, so it is held together as one unit.
        let glue: Vec<Range<usize>> = code
            .iter()
            .map(|repaint| &repaint.range)
            .filter(|range| range.end > start && range.start < start + text.len())
            .map(|range| range.start.saturating_sub(start)..range.end - start)
            .collect();
        let x = |byte: usize| {
            row.line
                .position_for_index(byte, layout.line_height)
                .map_or(px(0.), |position| position.x)
        };
        for unit in unbreakable_units(text, &glue) {
            widest = widest.max((x(unit.end) - x(unit.start)).min(CELL_MAX_MIN_CONTENT));
        }
    }
    widest
}

/// Shape the smaller text drawn inside each inline-code pill and each superscript
/// slot, and record the slots it sits in.
pub(super) fn shape_inline_code(
    layout: &mut LayoutLine,
    code_ranges: &[Repaint],
    font_size: Pixels,
    text_system: &WindowTextSystem,
) {
    if code_ranges.is_empty() {
        return;
    }
    for Repaint {
        range,
        runs,
        raised,
        lowered,
    } in code_ranges
    {
        let mut pieces: Vec<(usize, Range<usize>, Bounds<Pixels>)> = Vec::new();
        for (index, row) in layout.rows.iter().enumerate() {
            let row_text = row.text();
            let from = range.start.saturating_sub(row_byte_start(layout, index));
            let to = range.end.saturating_sub(row_byte_start(layout, index));
            if to == 0 || from >= row_text.len() {
                continue;
            }
            let (from, to) = (from.min(row_text.len()), to.min(row_text.len()));
            let starts = row.wrap_starts();
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(row_text.len());
                let mut part = from.max(start)..to.min(end);
                // A visual row the span does not reach.
                if part.is_empty() {
                    continue;
                }
                // The space a row wraps at is not drawn, and a selection's
                // rectangle runs on to the row's end past it; a piece keeps to
                // its visible glyphs, so the pill hugs them and the text
                // centred in it carries no dangling space.
                if part.end == end && visual + 1 < starts.len() {
                    part.end = part.start + row_text[part.clone()].trim_end().len();
                }
                if part.start == start && visual > 0 {
                    part.start = part.end - row_text[part.clone()].trim_start().len();
                }
                if part.is_empty() {
                    continue;
                }
                let chars = row.char_start + byte_to_char(row_text, part.start)
                    ..row.char_start + byte_to_char(row_text, part.end);
                let Some(slot) = layout.display_rectangles(chars, false).into_iter().next() else {
                    continue;
                };
                pieces.push((index, part, slot));
            }
        }
        // The slot is as wide as full-size text. Shrinking code by a fixed ratio would
        // leave long spans mostly padding, so its size is chosen to leave about
        // `INLINE_CODE_PADDING` on either side of each visual row instead.
        let reserved: Pixels = pieces.iter().map(|(_, _, slot)| slot.size.width).sum();
        let padding = INLINE_CODE_PADDING * 2. * pieces.len() as f32;
        let (scale, lift) = if *raised && *lowered {
            (SUPERSCRIPT_SCALE, -(font_size * SUBSCRIPT_DROP).round())
        } else if *raised {
            (SUPERSCRIPT_SCALE, (font_size * SUPERSCRIPT_LIFT).round())
        } else {
            (
                ((reserved - padding) / reserved.max(px(1.))).clamp(INLINE_CODE_SCALE, 1.),
                px(0.),
            )
        };
        for (index, part, slot) in pieces {
            let row_text = layout.rows[index].text().to_owned();
            let absolute = row_byte_start(layout, index);
            let absolute = absolute + part.start..absolute + part.end;
            let line = text_system.shape_line(
                row_text[part.clone()].to_owned().into(),
                font_size * scale,
                &piece_runs(range.start, runs, absolute),
                None,
            );
            let visual = layout.rows[index].visual_start
                + ((slot.origin.y - layout.origin.y) / layout.line_height).round() as usize;
            layout.rows[index].inline_code.push(InlineCode {
                range: part,
                visual_row: visual,
                left: slot.origin.x - layout.origin.x,
                slot: slot.size.width,
                line: Rc::new(line),
                raised: *raised,
                lift,
            });
        }
    }
}

/// The runs of a span's `runs`, which start at byte `start` of the line text,
/// that fall within `piece`, a stretch of the same span.
pub(super) fn piece_runs(
    start: usize,
    runs: &[(usize, Font, Hsla)],
    piece: Range<usize>,
) -> Vec<TextRun> {
    let mut pieces = Vec::new();
    let mut from = start;
    for (len, face, ink) in runs {
        let to = from + len;
        let (a, b) = (from.max(piece.start), to.min(piece.end));
        if a < b {
            pieces.push(TextRun {
                len: b - a,
                font: face.clone(),
                color: *ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        from = to;
    }
    pieces
}

/// The byte offset at which a row's text starts within the line's text.
pub(super) fn row_byte_start(layout: &LayoutLine, index: usize) -> usize {
    layout.rows[..index]
        .iter()
        .map(|row| row.text().len() + 1)
        .sum()
}
