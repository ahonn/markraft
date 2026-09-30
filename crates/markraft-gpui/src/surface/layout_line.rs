//! The geometry a paint produces and every query on it: a line's rows,
//! where each character and caret sits, hit testing and the caret quads.
//! Shaping builds these; paint, hit testing and the caret read them.

use super::*;

/// Inline code, superscript or subscript on one visual row. Text runs share one font
/// size, so the main line only reserves the space and the text is painted again,
/// smaller, centred in that slot. Around code, what is left of the slot on either side
/// becomes the pill's padding; a script has no pill and is raised or lowered instead.
#[derive(Clone)]
pub(super) struct InlineCode {
    /// Byte range within the row's own text.
    pub(super) range: Range<usize>,
    /// Visual row within the whole block.
    pub(super) visual_row: usize,
    /// Relative to the block's origin.
    pub(super) left: Pixels,
    pub(super) slot: Pixels,
    pub(super) line: Rc<ShapedLine>,
    /// A script: no pill behind it, and painted `lift` above the row — below
    /// it for a subscript, whose lift is negative.
    pub(super) raised: bool,
    pub(super) lift: Pixels,
}

/// A span a row only reserves room for, painted again smaller: see [`InlineCode`].
#[derive(Clone)]
pub(super) struct Repaint {
    /// Byte range within the whole line text.
    pub(super) range: Range<usize>,
    /// The face and ink of each stretch of `range`, as byte lengths that sum
    /// to its length. One span can carry several: revealed backticks keep the
    /// quiet markup ink inside the one pill their code is drawn in.
    pub(super) runs: Vec<(usize, Font, Hsla)>,
    /// A script rather than code.
    pub(super) raised: bool,
    /// Of a script, whether it is subscript.
    pub(super) lowered: bool,
}

impl InlineCode {
    /// Code sits centred in its pill. A script sits at the start of its slot,
    /// against the word it belongs to, so the room the full-size text would
    /// have taken falls after it.
    pub(super) fn text_left(&self) -> Pixels {
        if self.raised {
            self.left
        } else {
            self.left + (self.slot - self.line.width) / 2.
        }
    }
}

/// What an inline atom is drawn as.
///
/// A [`AtomShape::Pill`] is chrome the row cannot shape, so it is painted over
/// an object with a measured advance. The others are text, so the row shapes
/// their label itself and punctuation sits against it as after any other word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AtomShape {
    /// An image's stand-in: a picture glyph and a label on a rounded fill.
    Pill,
    /// The source an atom under the caret was read from, shaped as the quiet
    /// code-font markup it is, as other revealed markup is.
    Source,
    /// Source the view does not render — an inline HTML primitive, a shortcode
    /// no emoji answers to — shaped as the prose around it.
    Text,
    /// A `<br>` in a table cell: the cell's line break, as a newline in the
    /// row, which starts a new row there.
    Break,
    /// A wiki link's label, shaped as the prose it stands in, in the link
    /// colour. Its brackets and its target are source the view does not show.
    Link,
    /// An emoji shortcode's emoji, shaped as a character of the prose around
    /// it. Its colons and its name are source the view does not show.
    Glyph,
}

impl AtomShape {
    /// What the shape draws around its label, which is all an atom reserves
    /// beyond the width of the text itself.
    pub(super) fn chrome(self) -> Pixels {
        match self {
            AtomShape::Pill => PILL_PADDING * 2. + PILL_ICON + PILL_ICON_GAP,
            AtomShape::Source
            | AtomShape::Text
            | AtomShape::Break
            | AtomShape::Link
            | AtomShape::Glyph => px(0.),
        }
    }

    /// Whether the row shapes the atom's own label in place of a placeholder,
    /// so it takes exactly the width its glyphs advance.
    pub(super) fn is_own_text(self) -> bool {
        matches!(
            self,
            AtomShape::Source
                | AtomShape::Text
                | AtomShape::Break
                | AtomShape::Link
                | AtomShape::Glyph
        )
    }
}

/// One painted inline atom, once shaping says which slot it landed in.
#[derive(Clone)]
pub(super) struct InlineAtom {
    /// Relative to the line's origin.
    pub(super) left: Pixels,
    pub(super) slot: Pixels,
    /// Visual row within the whole block.
    pub(super) visual_row: usize,
    pub(super) label: Rc<ShapedLine>,
    /// A decoded image drawn in place of the pill, at the size it was measured
    /// for.
    pub(super) image: Option<(Arc<crate::animation::Picture>, Size<Pixels>)>,
    /// A picture still being fetched: a frame of this size stands where it will
    /// be drawn, with the label in it.
    pub(super) frame: Option<Size<Pixels>>,
    /// The pill stands for a note rather than a picture, so it wears a page.
    pub(super) note: bool,
}

/// A formula image or diagnostic, positioned relative to its paragraph.
#[derive(Clone)]
pub(super) struct MathDecoration {
    pub(super) bounds: Bounds<Pixels>,
    pub(super) content: MathContent,
    /// Absolute document position for a resolved equation reference.
    pub(super) target: Option<usize>,
    /// The visual row an inline formula sits in, which an aligned row moves;
    /// previews under the text have none.
    pub(super) visual: Option<usize>,
}

#[derive(Clone)]
pub(super) enum MathContent {
    Formula(Arc<RenderImage>),
    Error(Rc<ShapedLine>),
}

/// Where a line's display text holds a different number of characters than the
/// projection does.
///
/// Every geometry query the view answers is in projection offsets, and the rows
/// are shaped from the display text, so the two spaces have to be mapped onto
/// each other. An atom *widens* one projected character into a label; a
/// concealed run — characters that spell rather than say, see
/// [`crate::conceal`] — *collapses* to nothing, or is *substituted* by what it
/// displays, which is as long as it happens to be.
#[derive(Clone, Copy)]
pub(super) struct Widening {
    /// `char` offset of the remapped span within the projection line.
    pub(super) source: usize,
    /// How many projection `char`s the span covers. Atoms are one; a
    /// concealed run is the length of that run.
    pub(super) source_len: usize,
    /// `char` offset of the replacement within the display text.
    pub(super) display: usize,
    /// How many `char`s the replacement takes. Zero when the span is hidden.
    pub(super) len: usize,
    /// What an atom's placeholder holds, which is what says whether its own
    /// characters reach the screen. `None` for a concealed run, whose
    /// replacement is drawn as the text around it is.
    pub(super) shape: Option<AtomShape>,
    /// A wiki link the host says it cannot open. Decided while the atom is shaped,
    /// because that is where the node is; read where the row's text is coloured,
    /// because a link's label is the row's own text rather than the atom's.
    pub(super) broken: bool,
}

/// How many quote levels can carry a tone of their own; deeper ones fall back to
/// the ordinary bar.
pub(super) const QUOTE_TONES: usize = 8;

#[derive(Clone, Copy)]
pub(super) enum Decoration {
    /// One bar per block quote the line sits in; a level joins with the line below
    /// when that line sits in the same quote.
    Quote {
        levels: usize,
        joined: usize,
        /// The accent of each drawn level's callout, outermost first. `None` for
        /// an ordinary quote, and for levels past what the array holds.
        tones: [Option<Hsla>; QUOTE_TONES],
    },
    Divider,
    /// A code block's rounded background, behind the whole line, and the bars
    /// of the quotes it sits in beside it, as a quote's would be.
    Code {
        levels: usize,
        joined: usize,
        tones: [Option<Hsla>; QUOTE_TONES],
    },
}

/// A callout's header: the line drawn above its first block, saying what kind
/// of note it is. It is chrome, not content — no caret ever lands in it.
#[derive(Clone)]
pub(super) struct CalloutHeader {
    pub(super) label: Rc<ShapedLine>,
    /// What the label says, for a reader that cannot see it.
    pub(super) text: String,
}

/// The room a callout's header takes above the block it opens.
pub(super) const CALLOUT_HEADER_HEIGHT: Pixels = px(22.);

#[derive(Clone)]
pub(super) enum Marker {
    /// An ordered-list number, right-aligned against the text.
    Number(Rc<ShapedLine>),
    Bullet {
        depth: usize,
    },
    Task {
        checked: bool,
        number: Option<Rc<ShapedLine>>,
    },
    /// A footnote definition's label, right-aligned against its first line.
    /// Clicking it goes back to the first reference.
    Footnote(Rc<ShapedLine>),
}

/// Where a table cell sits in its grid, once the table pass has placed it.
///
/// The box is kept as an offset from the line's own origin, which `prepaint`
/// translates into the window, so a cell's chrome follows its text wherever
/// the frame puts it.
#[derive(Clone, Copy)]
pub(crate) struct TableCell {
    /// The position before the table node, which is what tells two adjacent
    /// grids apart.
    pub(crate) table: usize,
    /// The cell's row; row 0 is the header.
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    /// The alignment of the cell's own column.
    pub(crate) alignment: ColumnAlignment,
    /// How many block quotes the grid sits in. A cell carries no decoration of
    /// its own, so the grid draws those bars for all of its cells at once.
    pub(super) quotes: usize,
    /// The cell box's top-left corner, relative to the line's origin.
    pub(super) offset: Point<Pixels>,
    /// The cell box, padding included.
    pub(super) size: Size<Pixels>,
}

/// One newline-separated row of a line, shaped on its own.
#[derive(Clone)]
pub(crate) struct LayoutRow {
    pub(super) line: Rc<WrappedLine>,
    pub(super) paint_rows: Vec<ShapedLine>,
    /// `char` offset of the row's start within the projection line.
    pub(super) char_start: usize,
    /// The first visual row this row occupies within the block.
    pub(super) visual_start: usize,
    pub(super) inline_code: Vec<InlineCode>,
}

impl LayoutRow {
    pub(super) fn text(&self) -> &str {
        &self.line.text
    }
    pub(super) fn char_len(&self) -> usize {
        self.text().chars().count()
    }
    pub(super) fn visual_rows(&self) -> usize {
        self.line.wrap_boundaries().len() + 1
    }
    /// Byte offsets at which each of this row's visual rows starts.
    pub(super) fn wrap_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(
            self.line
                .wrap_boundaries()
                .iter()
                .map(|b| self.line.runs()[b.run_ix].glyphs[b.glyph_ix].index),
        );
        starts
    }
}

/// One projection line, laid out.
#[derive(Clone)]
pub(crate) struct LayoutLine {
    /// The projection line used to shape this frame, including inline position maps.
    pub(super) source: Line,
    /// The projection line this was built from.
    pub index: usize,
    /// Document position of the line's first content token.
    pub from: usize,
    /// How many visible `char`s the line holds.
    pub char_len: usize,
    pub rows: Vec<LayoutRow>,
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
    pub(super) visuals: Vec<VisualRow>,
    /// Text height plus the gap below the line.
    pub height: Pixels,
    pub width: Pixels,
    /// The narrowest the content can be drawn at without splitting a unit the
    /// line wrapper keeps together. Measured on the table pass only, which is
    /// the one caller that has to know how far a box may shrink.
    pub(crate) min_width: Pixels,
    pub(crate) top_gap: Pixels,
    /// Position directly before a code block's node, for the host's language picker.
    pub(crate) code_pos: Option<usize>,
    pub(super) marker: Option<Marker>,
    pub(super) decoration: Option<Decoration>,
    /// The language tag a focused code block shows under its panel.
    pub(super) code_language: Option<Rc<ShapedLine>>,
    /// How far a code block's panel reaches above and below its text; zero on
    /// any other line.
    pub(super) code_inset: Pixels,
    /// How far the text of an item's first line is indented by the blocks
    /// inside the item it opens — a quote, a code panel — so the item's marker
    /// stands left of them rather than on them.
    pub(super) marker_inset: Pixels,
    /// The picture a line spelling one out draws under its text, keeping
    /// the picture in view while the caret edits its source.
    pub(super) preview: Option<(Arc<crate::animation::Picture>, Size<Pixels>)>,
    /// How far left of the text each quote the line sits in draws its bar,
    /// outermost first. See [`quote_bar_distances`].
    pub(super) quote_bars: Vec<Pixels>,
    /// The shaped header of a callout, on the line that opens it.
    pub(super) callout_header: Option<CalloutHeader>,
    /// Sorted by `source`; see [`Widening`].
    pub(super) widenings: Vec<Widening>,
    pub(super) atoms: Vec<InlineAtom>,
    pub(super) formulas: Vec<MathDecoration>,
    /// Where the line sits in a table, when it is a cell of one.
    pub(crate) table: Option<TableCell>,
    /// What the line was shaped from beyond its own body, when a later shaping
    /// may keep it; see [`LineKey`].
    pub(super) reuse: Option<LineKey>,
    /// A formula's render was outstanding when the line was shaped. Such a
    /// line is never kept: a render the bounded queue turned away is asked
    /// for again only by shaping the line again.
    pub(super) math_pending: bool,
    /// What a verbatim line draws of its source rendered: an HTML block's
    /// page. While the caret is away the page stands in for the source, whose
    /// rows stay unpainted so a click still lands on a position of the line;
    /// with the caret in it the source is drawn and the page stays under it.
    pub(super) rendered: Option<Rc<RenderedLayout>>,
    /// How far each visual row is moved across the line to sit as a rendered
    /// block's alignment asks; empty for a line that starts at its start.
    pub(super) row_shifts: Vec<Pixels>,
    /// The alignment `row_shifts` were measured for.
    pub(super) align: Align,
}

/// A verbatim line's source rendered and laid out; see [`LayoutLine::rendered`].
pub(crate) struct RenderedLayout {
    /// Its lines, each placed relative to the page's top.
    pub(super) lines: Vec<(Point<Pixels>, LayoutLine)>,
    /// How far they reach below that top.
    pub(super) height: Pixels,
    /// Where the page's top sits below the verbatim line's origin: zero where
    /// it stands in for the source, and past the source rows where it is drawn
    /// under them.
    pub(super) top: Pixels,
}

impl RenderedLayout {
    /// Whether the page is drawn under the source rather than in their place.
    pub(super) fn under_source(&self) -> bool {
        self.top > px(0.)
    }
}

/// Host popovers also anchor document and node selections, whose opening token can
/// precede the first text row. A caret outside the layout still has no anchor.
pub(crate) fn selection_anchor_row(
    layout: &[LayoutLine],
    start: usize,
    end: usize,
) -> Option<&LayoutLine> {
    layout.iter().find(|row| row.contains(start)).or_else(|| {
        (start < end)
            .then(|| {
                layout
                    .iter()
                    .find(|row| row.from >= start && row.from < end)
            })
            .flatten()
    })
}

impl LayoutLine {
    /// Resolved references use their painted bounds, including in live previews.
    pub(crate) fn equation_target_at(&self, position: Point<Pixels>) -> Option<usize> {
        let local = position - self.origin;
        self.formulas.iter().find_map(|formula| {
            formula
                .target
                .filter(|_| self.formula_bounds(formula).contains(&local))
        })
    }

    /// Where a formula decoration is drawn, relative to the line's origin,
    /// moved with its row when the line is aligned.
    pub(super) fn formula_bounds(&self, formula: &MathDecoration) -> Bounds<Pixels> {
        let shift = formula
            .visual
            .map_or(px(0.), |visual| self.row_shift(visual));
        Bounds::new(
            formula.bounds.origin + point(shift, px(0.)),
            formula.bounds.size,
        )
    }

    /// This line's rows, for `line`, which shares its body and sits at `index`.
    ///
    /// Everything a shaped line holds is relative to its own start except
    /// these fields, so they are all a kept line has to be told. The rest —
    /// rows, widenings, atoms, markers, the origin's indent — is measured from
    /// the line's own start. A table cell also holds where the grid placed it,
    /// which is why a cell is never kept.
    pub(super) fn moved_to(&self, line: &Line, index: usize) -> LayoutLine {
        debug_assert!(self.source.same_body(line) && self.table.is_none());
        LayoutLine {
            source: line.clone(),
            index,
            from: line.from(),
            code_pos: self.code_pos.and(line.block_before()),
            ..self.clone()
        }
    }

    /// How far visual row `visual` is moved across the line; see `row_shifts`.
    pub(super) fn row_shift(&self, visual: usize) -> Pixels {
        self.row_shifts.get(visual).copied().unwrap_or_default()
    }

    /// Align every visual row across the line's width, measuring each row as
    /// gpui does when it paints a wrapped line aligned — from the glyph a row
    /// starts at to the one the next starts at — so what is placed by hand,
    /// pills and pictures, moves exactly as the text does.
    pub(super) fn align_rows(&mut self, align: Align) {
        self.align = align;
        self.row_shifts.clear();
        if align == Align::Start {
            return;
        }
        for row in &self.rows {
            let layout = &row.line.unwrapped_layout;
            let mut starts: Vec<Pixels> = row
                .line
                .wrap_boundaries()
                .iter()
                .map(|b| layout.runs[b.run_ix].glyphs[b.glyph_ix].position.x)
                .collect();
            starts.insert(0, px(0.));
            for (visual, start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(layout.width);
                let room = self.width - (end - *start);
                self.row_shifts.push(match align {
                    Align::Center => room / 2.,
                    _ => room,
                });
            }
        }
    }

    /// The document position just past the line's own content.
    pub(crate) fn to(&self) -> usize {
        self.source.to()
    }

    pub(crate) fn pos_to_offset(&self, pos: usize) -> usize {
        self.source
            .pos_to_offset(pos.clamp(self.from, self.to()))
            .unwrap_or(0)
    }

    pub(crate) fn offset_to_pos(&self, offset: usize) -> usize {
        self.source
            .offset_to_pos(offset.min(self.char_len))
            .unwrap_or(self.from)
    }

    pub(crate) fn hit_position(&self, offset: usize, projection: &Projection) -> usize {
        let pos = self.offset_to_pos(offset);
        // A leaf block has no grapheme to snap to. Searching backwards would
        // move a hit on a divider into the preceding paragraph.
        if self.char_len == 0 {
            pos
        } else {
            projection.floor_grapheme(pos)
        }
    }

    /// Whether `pos` falls inside this line.
    ///
    /// This is what decides which row draws the caret. Asking the row rather
    /// than asking the projection for a line index and comparing it to
    /// [`LayoutLine::index`] is deliberate: a layout is shaped from one
    /// projection and painted against whatever the view holds when the frame
    /// comes, and if those two ever disagree — a row list left over from a
    /// document with fewer lines — an index comparison attributes the caret to
    /// a row that stands for a different line and draws it there. A row that
    /// only ever answers for its own token range cannot.
    pub(crate) fn contains(&self, pos: usize) -> bool {
        pos >= self.from && pos <= self.to()
    }

    /// How many of the line's visual rows an arrow can stop on. A line drawn as
    /// its rendered page draws none of its source rows, and they need not fit
    /// in the page — read as rows they could lie inside the next line — so it
    /// offers the one row the caret enters it by; once in, the source is drawn
    /// and every row of it counts again.
    pub(crate) fn navigable_rows(&self) -> usize {
        match &self.rendered {
            Some(page) if !page.under_source() => 1,
            _ => self.visual_rows(),
        }
    }

    /// How many visual rows the line occupies.
    pub(crate) fn visual_rows(&self) -> usize {
        self.rows.iter().map(LayoutRow::visual_rows).sum()
    }

    pub(super) fn text_height(&self) -> Pixels {
        self.visuals
            .last()
            .map_or(self.line_height * self.visual_rows() as f32, |row| {
                row.top + row.height
            })
    }

    pub(crate) fn visual_top(&self, visual: usize) -> Pixels {
        self.visuals
            .get(visual)
            .map_or(self.line_height * visual as f32, |row| row.top)
    }

    pub(crate) fn visual_height(&self, visual: usize) -> Pixels {
        self.visuals
            .get(visual)
            .map_or(self.line_height, |row| row.height)
    }

    pub(crate) fn visual_text_top(&self, visual: usize) -> Pixels {
        self.visuals
            .get(visual)
            .map_or(self.line_height * visual as f32, |row| row.text_top)
    }

    pub(super) fn visual_baseline(&self, visual: usize) -> Pixels {
        self.visuals
            .get(visual)
            .map_or(self.visual_top(visual) + self.line_height, |row| {
                row.baseline
            })
    }

    pub(crate) fn visual_at(&self, local_y: Pixels) -> usize {
        if self.visuals.is_empty() {
            return ((local_y / self.line_height).floor().max(0.) as usize)
                .min(self.visual_rows().saturating_sub(1));
        }
        self.visuals
            .partition_point(|row| row.top <= local_y)
            .saturating_sub(1)
    }

    pub(crate) fn visual_for_offset(&self, offset: usize, upstream: bool) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let (index, byte) = self.locate(offset);
        let row = &self.rows[index];
        let starts = row.wrap_starts();
        row.visual_start
            + starts
                .iter()
                .skip(1)
                .take_while(|&&start| {
                    if upstream {
                        start < byte
                    } else {
                        start <= byte
                    }
                })
                .count()
    }

    /// Every picture the line draws and where: its inline atoms', each centred
    /// in its row, then the preview under its text.
    pub(crate) fn pictures(
        &self,
    ) -> impl Iterator<Item = (Bounds<Pixels>, &Arc<crate::animation::Picture>)> {
        let atoms = self.atoms.iter().filter_map(|atom| {
            let (picture, drawn) = atom.image.as_ref()?;
            let top = self.origin.y
                + self.visual_top(atom.visual_row)
                + ((self.visual_height(atom.visual_row) - drawn.height) * 0.5).max(px(0.));
            let left = self.origin.x + atom.left + self.row_shift(atom.visual_row);
            Some((Bounds::new(point(left, top), *drawn), picture))
        });
        let preview = self.preview.as_ref().map(|(picture, drawn)| {
            let origin = self.origin + point(px(0.), self.text_height() + PREVIEW_GAP);
            (Bounds::new(origin, *drawn), picture)
        });
        atoms.chain(preview)
    }

    /// The x of the bar drawn for `level` of the innermost `levels` quotes the
    /// line sits in.
    pub(super) fn quote_bar_x(&self, levels: usize, level: usize, style: &EditorStyle) -> Pixels {
        let quotes = self.quote_bars.len();
        match self.quote_bars.get(quotes.saturating_sub(levels) + level) {
            Some(distance) => self.origin.x - *distance,
            // A line shaped without its quotes' geometry keeps the plain
            // spacing of one indent per level.
            None => self.origin.x - style.quote_indent * (levels - level) as f32,
        }
    }

    /// How far a code block's panel reaches above and below its text.
    pub(super) fn code_panel_room(&self) -> Pixels {
        self.code_inset
    }

    /// The display-text `char` offset a projection offset stands at.
    pub(super) fn to_display(&self, offset: usize) -> usize {
        let mut shift: isize = 0;
        for widening in &self.widenings {
            if offset <= widening.source {
                break;
            }
            if offset >= widening.source + widening.source_len {
                shift += widening.len as isize - widening.source_len as isize;
                continue;
            }
            // Inside a remapped span: a collapsed run parks at the display
            // point; an atom or a substituted run keeps its left edge.
            return widening.display;
        }
        offset
            .checked_add_signed(shift)
            .expect("display offset stays in range")
    }

    /// The projection `char` offset a display offset stands at. Inside a
    /// replacement the nearer of the span's two edges wins, so a click on the
    /// right half of an atom — or of the `&` an entity shows — puts the caret
    /// after the whole span. A collapsed span has no interior.
    pub(super) fn to_source(&self, display: usize) -> usize {
        let mut shift: isize = 0;
        for widening in &self.widenings {
            let delta = widening.len as isize - widening.source_len as isize;
            if widening.len == 0 {
                if display > widening.display {
                    shift += delta;
                } else {
                    break;
                }
                continue;
            }
            if display >= widening.display + widening.len {
                shift += delta;
            } else if display > widening.display {
                let past = display - widening.display >= widening.len.div_ceil(2);
                return widening.source + if past { widening.source_len } else { 0 };
            } else {
                break;
            }
        }
        display
            .checked_add_signed(-shift)
            .expect("source offset stays in range")
    }

    /// The row a `char` offset falls in, and the byte offset within it.
    pub(super) fn locate(&self, offset: usize) -> (usize, usize) {
        let offset = self.to_display(offset);
        let index = self
            .rows
            .iter()
            .rposition(|row| row.char_start <= offset)
            .unwrap_or(0);
        let row = &self.rows[index];
        let local = offset.saturating_sub(row.char_start).min(row.char_len());
        (index, char_to_byte(row.text(), local))
    }

    /// The top-left corner of the band a callout's header is drawn in. The
    /// header sits directly above the block it opens: above a paragraph's text,
    /// and above a code block's panel — whose padding is the block's own —
    /// rather than inside it, flush with the panel's left edge,
    /// which is where a paragraph's text would start.
    pub(super) fn callout_header_origin(&self) -> Point<Pixels> {
        if self.code_inset > Pixels::ZERO {
            point(
                self.origin.x - CODE_PADDING,
                self.origin.y - self.code_panel_room() - CALLOUT_HEADER_HEIGHT,
            )
        } else {
            point(self.origin.x, self.origin.y - CALLOUT_HEADER_HEIGHT)
        }
    }

    /// Whether `y` falls in the band a callout's header is drawn in. The band is
    /// chrome above the line, so a click there means the start of the body rather
    /// than whichever character happens to sit under it.
    pub(crate) fn in_callout_header(&self, y: Pixels) -> bool {
        let top = self.callout_header_origin().y;
        self.callout_header.is_some() && y >= top && y < top + CALLOUT_HEADER_HEIGHT
    }

    /// What a callout's header says and the box it is drawn in, in the same
    /// space as the line's origin — available only on the line that opens one.
    pub(crate) fn callout_header(&self) -> Option<(&str, Bounds<Pixels>)> {
        let header = self.callout_header.as_ref()?;
        Some((
            header.text.as_str(),
            Bounds::new(
                self.callout_header_origin(),
                size(header.label.width, CALLOUT_HEADER_HEIGHT),
            ),
        ))
    }

    /// Window-space bounds of a code block's language tag, in its panel's
    /// top-right corner: where the tag is drawn, what a click on it hits,
    /// and what the host's language picker anchors to. `None` while the block
    /// is not focused, when it shows no tag.
    pub(crate) fn code_language_bounds(&self) -> Option<Bounds<Pixels>> {
        self.code_pos?;
        let width = self.code_language.as_ref()?.width + CODE_LANGUAGE_PADDING * 2.;
        let right = self.origin.x + self.width + CODE_PADDING;
        let top = self.origin.y - self.code_inset;
        Some(Bounds::new(
            point(right - width, top),
            size(width, CODE_LANGUAGE_HEIGHT),
        ))
    }

    /// Window-space bounds of the whole cell box, padding included, available
    /// only on a table cell.
    pub(crate) fn cell_bounds(&self) -> Option<Bounds<Pixels>> {
        let cell = self.table.as_ref()?;
        Some(Bounds::new(self.origin + cell.offset, cell.size))
    }

    pub(crate) fn marker_bounds(&self) -> Option<Bounds<Pixels>> {
        let (offset, width, height) = match self.marker.as_ref()? {
            Marker::Number(line) | Marker::Footnote(line) => {
                (line.width + NUMBER_GAP, line.width, self.line_height)
            }
            // Drawn markers share one center, 15px left of the text.
            Marker::Bullet { .. } => (px(17.5), px(5.), px(5.)),
            Marker::Task { .. } => (px(22.), px(14.), px(14.)),
        };
        let offset = offset + self.marker_inset;
        Some(Bounds::new(
            self.origin
                + point(
                    -offset,
                    self.visual_text_top(0) + (self.line_height - height) * 0.5,
                ),
            size(width, height),
        ))
    }

    /// Where a footnote definition's label is drawn, on the definition's first
    /// line.
    pub(crate) fn footnote_marker(&self) -> Option<Bounds<Pixels>> {
        matches!(self.marker, Some(Marker::Footnote(_)))
            .then(|| self.marker_bounds())
            .flatten()
    }

    pub(crate) fn task_marker(&self) -> Option<(bool, Bounds<Pixels>)> {
        let Marker::Task { checked, .. } = self.marker.as_ref()? else {
            return None;
        };
        Some((*checked, self.marker_bounds()?))
    }

    /// The top-left of the caret at `offset`, a `char` offset into the line.
    pub(crate) fn caret(&self, offset: usize, upstream: bool) -> Point<Pixels> {
        if self.rows.is_empty() {
            return self.origin;
        }
        let (index, byte) = self.locate(offset);
        let row = &self.rows[index];
        if !upstream {
            for (visual, boundary) in row.line.wrap_boundaries().iter().enumerate() {
                if row.line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index == byte {
                    return self.origin
                        + point(px(0.), self.visual_text_top(row.visual_start + visual + 1));
                }
            }
        }
        let mut position = row
            .line
            .position_for_index(byte, self.line_height)
            .unwrap_or_default();
        let visual = row.visual_start + (position.y / self.line_height).round() as usize;
        if let Some(x) = self.inline_code_x(index, byte, visual) {
            position.x = x;
        }
        self.origin + point(position.x, self.visual_text_top(visual))
    }

    /// Where a position strictly inside inline code is drawn. Its edges keep the
    /// slot's own bounds, so the caret rests outside the pill there.
    pub(super) fn inline_code_x(&self, index: usize, byte: usize, visual: usize) -> Option<Pixels> {
        let row = &self.rows[index];
        let code = row.inline_code.iter().find(|code| {
            code.visual_row == visual && code.range.start < byte && byte < code.range.end
        })?;
        Some(code.text_left() + code.line.x_for_index(byte - code.range.start))
    }

    /// The `char` offset under `local`, a point relative to the line's origin.
    pub(crate) fn char_at(&self, local: Point<Pixels>) -> usize {
        self.source_at(self.display_at(local))
    }

    /// [`LayoutLine::to_source`], except at the very end of the line's text: a
    /// point there — a click past the last word, ⌘→ — goes past the markup
    /// that closes the line too, so what is typed next
    /// carries on after a bold or a code span rather than inside it.
    pub(super) fn source_at(&self, display: usize) -> usize {
        if display >= self.to_display(self.char_len) {
            self.char_len
        } else {
            self.to_source(display)
        }
    }

    /// The source shown as text that `local` falls inside — an inline HTML tag,
    /// an unknown shortcode — as the offset before its atom, the source, and how
    /// many of its characters lie before the point. `None` at either edge of it
    /// and anywhere else, where [`LayoutLine::char_at`] already says where the
    /// caret goes.
    pub(crate) fn source_text_at(&self, local: Point<Pixels>) -> Option<(usize, String, usize)> {
        self.source_text_in(self.display_at(local))
    }

    /// [`LayoutLine::source_text_at`] for a display-text `char` offset.
    pub(super) fn source_text_in(&self, display: usize) -> Option<(usize, String, usize)> {
        let widening = self.widenings.iter().find(|widening| {
            widening.shape == Some(AtomShape::Text)
                && display > widening.display
                && display < widening.display + widening.len
        })?;
        let text: String = self
            .rows
            .iter()
            .flat_map(|row| row.text().chars().chain(std::iter::once('\n')))
            .skip(widening.display)
            .take(widening.len)
            .collect();
        Some((widening.source, text, display - widening.display))
    }

    /// The display-text `char` offset under `local`.
    pub(super) fn display_at(&self, local: Point<Pixels>) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let visual = self.visual_at(local.y);
        let index = self
            .rows
            .iter()
            .rposition(|row| row.visual_start <= visual)
            .unwrap_or(0);
        let row = &self.rows[index];
        let inner = point(
            local.x.max(px(0.)),
            self.line_height * (visual - row.visual_start) as f32 + self.line_height * 0.5,
        );
        let byte = row.inline_code_index(visual, local.x).unwrap_or_else(|| {
            match row.line.closest_index_for_position(inner, self.line_height) {
                Ok(index) | Err(index) => index,
            }
        });
        row.char_start + byte_to_char(row.text(), byte)
    }

    /// The `char` range of each visual row, clamped to the line's own content so
    /// a placeholder reports nothing selectable.
    pub(crate) fn accessible_rows(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        for row in &self.rows {
            let text = row.text();
            let starts = row.wrap_starts();
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(text.len());
                let from = self
                    .to_source(row.char_start + byte_to_char(text, start))
                    .min(self.char_len);
                let to = self
                    .to_source(row.char_start + byte_to_char(text, end))
                    .min(self.char_len);
                ranges.push(from..to);
            }
        }
        ranges
    }

    /// The quads covering `chars`, one per visual row it touches. `newline` adds a
    /// stub past the end of the line, for a selection that spans into the next.
    pub(crate) fn rectangles(&self, chars: Range<usize>, newline: bool) -> Vec<Bounds<Pixels>> {
        let stub = newline && chars.end >= self.char_len;
        self.display_rectangles(
            self.to_display(chars.start)..self.to_display(chars.end),
            stub,
        )
    }

    /// The quads covering a range of the *display* text, which is what the rows
    /// were shaped from. `stub` marks the end of the last visual row.
    pub(super) fn display_rectangles(
        &self,
        chars: Range<usize>,
        stub: bool,
    ) -> Vec<Bounds<Pixels>> {
        let mut rectangles = vec![];
        let last_visual = self.visual_rows().saturating_sub(1);
        for (index, row) in self.rows.iter().enumerate() {
            let text = row.text();
            let starts = row.wrap_starts();
            let from = char_to_byte(text, chars.start.saturating_sub(row.char_start));
            let to = if chars.end < row.char_start {
                0
            } else {
                char_to_byte(text, chars.end - row.char_start)
            };
            for (visual, &start) in starts.iter().enumerate() {
                let end = starts.get(visual + 1).copied().unwrap_or(text.len());
                let a = from.max(start);
                let b = to.min(end);
                let absolute = row.visual_start + visual;
                let trailing = stub && absolute == last_visual;
                if a >= b && !trailing {
                    continue;
                }
                let y = self.origin.y + self.visual_top(absolute);
                let x_for = |byte: usize| {
                    if byte == start {
                        return px(0.);
                    }
                    let p = row
                        .line
                        .position_for_index(byte, self.line_height)
                        .unwrap_or_default();
                    if p.y < self.line_height * visual as f32 {
                        px(0.)
                    } else if p.y > self.line_height * visual as f32 {
                        self.width
                    } else {
                        self.inline_code_x(index, byte, absolute).unwrap_or(p.x)
                    }
                };
                let left = x_for(a);
                let right = if b == end && visual + 1 < starts.len() {
                    self.width
                } else {
                    x_for(b)
                };
                rectangles.push(Bounds::new(
                    point(self.origin.x + left, y),
                    size(
                        (right - left + if trailing { px(7.) } else { px(0.) }).max(px(2.)),
                        self.visual_height(absolute),
                    ),
                ));
            }
        }
        rectangles
    }
}

impl LayoutRow {
    /// The byte offset under `x` when it falls on inline code of `visual` row.
    pub(super) fn inline_code_index(&self, visual: usize, x: Pixels) -> Option<usize> {
        let code = self.inline_code.iter().find(|code| {
            code.visual_row == visual && code.left <= x && x <= code.left + code.slot
        })?;
        Some(code.range.start + code.line.closest_index_for_x(x - code.text_left()))
    }
}

/// The caret quad for `shape` at `offset`. `next` is the grapheme boundary after
/// the caret within the same line, when there is one.
pub(super) fn caret_quad(
    row: &LayoutLine,
    offset: usize,
    next: Option<usize>,
    upstream: bool,
    shape: CaretShape,
) -> Bounds<Pixels> {
    let origin = row.caret(offset, upstream);
    let bar = Bounds::new(origin, size(CARET_THICKNESS, row.line_height));
    match shape {
        CaretShape::Bar => bar,
        CaretShape::Block | CaretShape::Underline => {
            // A grapheme that wraps onto the next visual row leaves no width here, so
            // the caret falls back to its nominal one rather than spanning the row.
            let width = next
                .map(|next| row.caret(next, false))
                .filter(|end| end.y == origin.y && end.x > origin.x)
                .map(|end| end.x - origin.x)
                .unwrap_or(row.line_height * EMPTY_BLOCK_CARET_RATIO);
            if shape == CaretShape::Block {
                Bounds::new(origin, size(width, row.line_height))
            } else {
                Bounds::new(
                    origin + point(px(0.), row.line_height - CARET_THICKNESS),
                    size(width, CARET_THICKNESS),
                )
            }
        }
    }
}
