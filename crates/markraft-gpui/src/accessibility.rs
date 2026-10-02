//! AccessKit text coordinates are selectable units, not UTF-16 offsets.
use crate::shown::{ShownPiece, line_pieces};
use crate::surface::LayoutLine;
use gpui::{A11ySubtreeBuilder, App, Bounds, Entity, Pixels, Role, Window, accesskit, px};
use markraft_core::kind::conceal::Reveal;
use markraft_core::projection::{Line, Projection};
use markraft_core::{EditorState, Selection};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub(crate) struct AccessibleText {
    messages: crate::EditorMessages,
    /// What the last update was built from; see [`AccessibleText::update`].
    built: Option<Built>,
    /// What each line no frame laid out reads as, for the projection they
    /// line up with; see [`AccessibleText::push_unlaid`].
    unlaid: Vec<Option<Unlaid>>,
    unlaid_of: Option<Arc<Projection>>,
    /// The id the next reading of an unlaid line takes.
    next_reading: u64,
    /// Far lines read together, by the index of the first; see
    /// [`AccessibleText::push_far`].
    far: HashMap<usize, Far>,
    /// What each laid line read as, by its index, for the projection
    /// `unlaid_of`; see [`AccessibleText::laid_reading`].
    laid: HashMap<usize, Laid>,
    /// Where the last update placed the content; see [`Space`].
    space: Space,
    runs: Vec<TextRun>,
    selection: (usize, usize),
    controls: Vec<AccessibleControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ControlAction {
    ToggleTask(usize),
    CodeLanguage(usize),
    CopyCode(usize),
    OpenWikiLink(usize),
    EnterCallout(usize),
}

struct AccessibleControl {
    node_id: Option<accesskit::NodeId>,
    action: ControlAction,
    label: String,
    checked: Option<bool>,
    bounds: accesskit::Rect,
}

/// Where the content's rows are told to stand: from the top of the content,
/// not of the window, so a frame that only scrolls moves the editor's node
/// alone. Every frame hands the whole tree over, and a row whose bounds moved
/// is copied whole, text and all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Space {
    /// Where the top of the content stands in the window.
    top: Pixels,
    scale: f32,
}

impl Space {
    pub(crate) fn new(top: Pixels, scale: f32) -> Self {
        Self { top, scale }
    }

    fn rect(self, bounds: Bounds<Pixels>) -> accesskit::Rect {
        // Rows stand where the window's top and their place in the content
        // add up to, so taking the top away again leaves rounding that a
        // scroll changes. An eighth of a pixel is finer than anything a
        // reader is shown, and keeps a row that did not move as it was.
        let snap = |at: Pixels| (f64::from(f32::from(at) * self.scale) * 8.).round() / 8.;
        accesskit::Rect {
            x0: snap(bounds.left()),
            y0: snap(bounds.top() - self.top),
            x1: snap(bounds.right()),
            y1: snap(bounds.bottom() - self.top),
        }
    }

    /// Where the content's top stands, in the editor node's own units.
    fn offset(self) -> f64 {
        f64::from(f32::from(self.top) * self.scale)
    }
}

/// What a screen reader is told a wiki link is, so it reads as a link to
/// somewhere rather than as the bare character the projection gives an atom.
fn wiki_link_label(label: &str, messages: &crate::EditorMessages) -> String {
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        messages.text(crate::EditorMessage::EmptyWikiLink)
    } else {
        messages.format(crate::EditorMessage::WikiLink, &[("label", &label)])
    }
}

impl AccessibleControl {
    fn node(&self) -> accesskit::Node {
        let mut node = accesskit::Node::new(if self.checked.is_some() {
            Role::CheckBox
        } else {
            Role::Button
        });
        node.set_label(self.label.clone());
        node.set_bounds(self.bounds);
        node.add_action(accesskit::Action::Click);
        node.add_action(accesskit::Action::Focus);
        if let Some(checked) = self.checked {
            node.set_toggled(if checked {
                accesskit::Toggled::True
            } else {
                accesskit::Toggled::False
            });
        }
        // A wiki link has no binding of its own: it is followed by clicking or
        // by activating it here, so there is no shortcut to announce.
        let shortcut = match self.action {
            ControlAction::ToggleTask(_) => Some("Command+Enter"),
            ControlAction::CodeLanguage(_) => Some("Command+Option+L"),
            ControlAction::CopyCode(_) => Some("Command+Option+Shift+C"),
            ControlAction::OpenWikiLink(_) | ControlAction::EnterCallout(_) => None,
        };
        if let Some(shortcut) = shortcut {
            node.set_keyboard_shortcut(shortcut);
        }
        node
    }
}

/// A line no frame laid out, as a reader is told it: kept from one update to
/// the next while the line keeps its body, since reading it means walking
/// its text grapheme by grapheme. Positions are the line's own, from where
/// it starts, so a line an edit only moved along keeps them.
struct Unlaid {
    /// Which reading this is, so lines read together can tell whether any of
    /// them was read again.
    id: u64,
    text: Arc<str>,
    lengths: Arc<[u8]>,
    positions: Arc<[usize]>,
    /// Whether the text ends in the break to the next line, which the last
    /// line has none of.
    newline: bool,
    cell: Option<(usize, usize)>,
}

/// Unlaid lines far from the screen, read as one run: a reader still reaches
/// them, a line at a time and each table cell with where it stands once the
/// screen comes near, without each costing a node of its own on every frame.
struct Far {
    /// The reading of each line, and where the line starts counted from where
    /// the first does: while both stay, so does what was read.
    lines: Vec<(u64, usize)>,
    text: Arc<str>,
    lengths: Arc<[u8]>,
    /// Counted from where the first line starts.
    positions: Arc<[usize]>,
}

/// A laid line's visual rows as a reader is told them, which only where the
/// rows stand changes while the line keeps its projection, its rows and what
/// the selection reveals of it.
struct Laid {
    rows: Vec<Range<usize>>,
    /// The selection and marked text, when they reveal any of the line.
    revealed: Option<Revealing>,
    reads: Vec<RowReading>,
}

/// The selection's range and the marked text's, which say what is revealed.
type Revealing = ((usize, usize), Option<(usize, usize)>);

struct RowReading {
    from: usize,
    content_end: usize,
    text: Arc<str>,
    lengths: Arc<[u8]>,
    positions: Arc<[usize]>,
}

/// How many unlaid lines either side of the laid ones are read a line at a
/// time. Past them, lines are read [`FAR_LINES`] at a time.
const NEAR_LINES: usize = 512;
/// How many far lines one run reads at most. Runs start at multiples of it, so
/// a scroll that moves the near lines along leaves the runs past them as they
/// were.
const FAR_LINES: usize = 128;

/// What an update read that changes what a reader is told.
#[derive(PartialEq)]
struct Built {
    projection: usize,
    selection: (usize, usize),
    composition: Option<(usize, usize)>,
    rows: usize,
    first: Option<(usize, gpui::Point<Pixels>)>,
    last: Option<(usize, gpui::Point<Pixels>)>,
    scale: f32,
}

struct TextRun {
    node_id: Option<accesskit::NodeId>,
    /// The line the run reads, and which of its visual rows; a run of far
    /// lines is keyed by the first with no row.
    key: (usize, usize),
    /// Document position of the run's first character.
    from: usize,
    /// Document position just past the run's own content.
    content_end: usize,
    text: Arc<str>,
    /// The UTF-8 length of each selectable unit.
    lengths: Arc<[u8]>,
    /// Document positions for each selectable unit boundary, counted from
    /// `base`.
    positions: Arc<[usize]>,
    base: usize,
    bounds: accesskit::Rect,
    /// The run's row and column, when it sits in a table cell.
    ///
    /// Announced on the run itself rather than through a `Role::Cell`
    /// container: the subtree builder appends leaves under the editor's own
    /// node and has no way to nest one, and a container between the editor and
    /// its runs would break the flat run sequence its text selection is
    /// expressed in.
    cell: Option<(usize, usize)>,
}

/// What one visual row shows of a line, given as `pieces` over its projected
/// text and the `char` range `inner` of it the row holds, with the projection
/// offset before each shown `char`.
///
/// A concealed run the row hides adds nothing; one it substitutes adds what it
/// displays, every character of which stands before the whole run, so a
/// selection can only take or leave it whole.
fn shown_row(pieces: &[ShownPiece], inner: Range<usize>) -> (String, Vec<usize>) {
    ShownRows::new(pieces).row(inner)
}

/// [`shown_row`] for the visual rows of one line, taken in order: where one
/// row stops is where the next starts looking, so a line of many rows — a
/// long code block is one line — is walked once rather than once a row.
struct ShownRows<'a> {
    pieces: &'a [ShownPiece],
    /// The piece the last row stopped in.
    piece: usize,
    /// How far into that piece the last row read, in `char`s and in bytes.
    chars: usize,
    bytes: usize,
    /// Where the last row ended. A row starting before it reads afresh.
    end: usize,
}

impl<'a> ShownRows<'a> {
    fn new(pieces: &'a [ShownPiece]) -> Self {
        Self {
            pieces,
            piece: 0,
            chars: 0,
            bytes: 0,
            end: 0,
        }
    }

    fn row(&mut self, inner: Range<usize>) -> (String, Vec<usize>) {
        if inner.start < self.end {
            *self = Self::new(self.pieces);
        }
        self.end = inner.end;
        let mut value = String::new();
        let mut before = Vec::new();
        // Pieces run in line order and do not overlap, so everything before
        // the first piece reaching into the row is in none of it.
        while let Some(piece) = self.pieces.get(self.piece) {
            let past = if piece.own {
                piece.source.end <= inner.start
            } else {
                piece.source.start < inner.start
            };
            if !past {
                break;
            }
            self.next_piece();
        }
        while let Some(piece) = self.pieces.get(self.piece) {
            if piece.source.start >= inner.end {
                break;
            }
            if !piece.own {
                for c in piece.text.chars() {
                    value.push(c);
                    before.push(piece.source.start);
                }
                self.next_piece();
                continue;
            }
            let from = piece.source.start.max(inner.start);
            let to = piece.source.end.min(inner.end);
            let mut chars = piece.text[self.bytes..].chars();
            while self.chars < to - piece.source.start {
                let Some(c) = chars.next() else {
                    break;
                };
                let at = piece.source.start + self.chars;
                if at >= from {
                    value.push(c);
                    before.push(at);
                }
                self.chars += 1;
                self.bytes += c.len_utf8();
            }
            if to < piece.source.end {
                break;
            }
            self.next_piece();
        }
        (value, before)
    }

    fn next_piece(&mut self) {
        self.piece += 1;
        self.chars = 0;
        self.bytes = 0;
    }
}

// AccessKit uses u8 for each selectable unit's UTF-8 length. An unusually long
// combining sequence cannot fit; expose its scalars and clamp incoming requests
// through the projection so editing still cannot split the grapheme.
fn character_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (start, grapheme) in text.grapheme_indices(true) {
        if grapheme.len() <= usize::from(u8::MAX) {
            offsets.push(start + grapheme.len());
        } else {
            offsets.extend(
                grapheme
                    .char_indices()
                    .map(|(index, c)| start + index + c.len_utf8()),
            );
        }
    }
    offsets
}

/// The UTF-8 length of each unit between `offsets`.
fn unit_lengths(offsets: &[usize]) -> Vec<u8> {
    offsets
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as u8)
        .collect()
}

/// The document position before each unit boundary in `offsets`, of text
/// `value` whose characters stand at the line offsets `before`: walked once,
/// since a long line has many boundaries. Past the text is `end`.
fn positions_of(
    line: &Line,
    value: &str,
    before: &[usize],
    offsets: &[usize],
    end: usize,
) -> Vec<usize> {
    let mut chars = value
        .char_indices()
        .map(|(byte, _)| byte)
        .enumerate()
        .peekable();
    offsets
        .iter()
        .map(|&byte| {
            let mut at = before.len();
            while let Some(&(index, start)) = chars.peek() {
                if start >= byte {
                    at = index;
                    break;
                }
                chars.next();
            }
            let offset = before.get(at).copied().unwrap_or(end).min(end);
            line.offset_to_pos(offset)
                .expect("a visible offset inside the line")
        })
        .collect()
}

/// The run `read` stands for, with `line` starting where it starts now.
fn unlaid_run(
    read: &Unlaid,
    index: usize,
    line: &Line,
    bounds: Bounds<Pixels>,
    space: Space,
) -> TextRun {
    TextRun {
        node_id: None,
        key: (index, 0),
        from: line.from(),
        content_end: line.to(),
        lengths: read.lengths.clone(),
        positions: read.positions.clone(),
        base: line.from(),
        text: read.text.clone(),
        bounds: space.rect(bounds),
        cell: read.cell,
    }
}

/// Line `index`, laid out as one run, as a reader is told it.
fn read_unlaid(
    id: u64,
    projection: &Projection,
    types: &crate::DocTypes,
    reveal: &Reveal,
    index: usize,
    line: &Line,
    newline: bool,
) -> Unlaid {
    let pieces = line_pieces(projection, types, index, reveal);
    let (mut text, mut before) = shown_row(&pieces, 0..line.len());
    if newline {
        text.push('\n');
        before.push(line.len());
    }
    let offsets = character_offsets(&text);
    let positions = positions_of(line, &text, &before, &offsets, line.len())
        .into_iter()
        .map(|pos| pos - line.from())
        .collect();
    Unlaid {
        id,
        text: text.into(),
        lengths: unit_lengths(&offsets).into(),
        positions,
        newline,
        cell: types
            .table_cell_of(line)
            .map(|(_, row, column)| (row, column)),
    }
}

/// The visual rows of laid line `row` as a reader is told them, kept from
/// the last update while the line reads the same. A frame that only
/// scrolls moves where every row stands, and a long code block is one
/// line of thousands of rows.
fn laid_reading(
    held: &mut HashMap<usize, Laid>,
    projection: &Projection,
    types: &crate::DocTypes,
    (reveal, revealing): (&Reveal, Revealing),
    row: &LayoutLine,
    last_line: usize,
) -> Laid {
    let rows = row.accessible_rows();
    let line = projection
        .line(row.index)
        .expect("a laid line is in the projection");
    let revealed = reveal.touches(line.from(), line.to()).then_some(revealing);
    if let Some(kept) = held.remove(&row.index)
        && kept.rows == rows
        && kept.revealed == revealed
    {
        return kept;
    }
    let pieces = line_pieces(projection, types, row.index, reveal);
    let mut shown = ShownRows::new(&pieces);
    let reads = rows
        .iter()
        .map(|inner| {
            let (mut value, mut before) = shown.row(inner.clone());
            if inner.end == line.len() && row.index < last_line {
                value.push('\n');
                before.push(inner.end);
            }
            let offsets = character_offsets(&value);
            let positions = positions_of(line, &value, &before, &offsets, inner.end);
            RowReading {
                from: if inner.start == 0 {
                    line.from()
                } else {
                    line.offset_to_pos(inner.start)
                        .expect("a row starts in its line")
                },
                content_end: if inner.end == line.len() {
                    line.to()
                } else {
                    line.offset_to_pos(inner.end)
                        .expect("a row ends in its line")
                },
                lengths: unit_lengths(&offsets).into(),
                positions: positions.into(),
                text: value.into(),
            }
        })
        .collect();
    Laid {
        rows,
        revealed,
        reads,
    }
}

impl AccessibleText {
    pub(crate) fn set_messages(&mut self, messages: crate::EditorMessages) {
        self.messages = messages;
        self.release();
    }
    /// Give back what the last update built, for an editor that is not being
    /// drawn; the next update rebuilds it all anyway.
    pub(crate) fn release(&mut self) {
        self.built = None;
        self.unlaid = Vec::new();
        self.unlaid_of = None;
        self.far = HashMap::new();
        self.laid = HashMap::new();
        self.runs = Vec::new();
        self.controls = Vec::new();
    }

    /// Rebuild what a reader is told from the document and the frame: every
    /// line, as the visual rows it was laid out in where the frame laid it
    /// out, and as one run standing where `place` says otherwise, so a reader
    /// reaches the whole note however little of it is on screen. Lines far
    /// from the laid ones are read [`FAR_LINES`] to a run: every frame hands
    /// the whole tree over, so its size is paid for on each one.
    pub(crate) fn update(
        &mut self,
        projection: &Arc<Projection>,
        state: &EditorState,
        types: &crate::DocTypes,
        rows: &[LayoutLine],
        place: &dyn Fn(usize) -> Bounds<Pixels>,
        space: Space,
    ) {
        let doc = state.doc();
        let Space { top, scale } = space;
        // Every frame asks, and most change nothing a reader is told: a caret
        // blink, a line off screen measured, a scroll that lays out the same
        // lines. A line the frame did not lay out
        // keeps the place it was given until something here moves, which only
        // costs a reader how precisely an unseen line is outlined.
        let built = Built {
            projection: std::ptr::from_ref(projection) as usize,
            selection: (state.selection().anchor(doc), state.selection().head(doc)),
            composition: markraft_core::composition::composition_range(state)
                .map(|range| (range.from, range.to)),
            rows: rows.len(),
            first: rows
                .first()
                .map(|row| (row.index, row.origin - gpui::point(px(0.), top))),
            last: rows
                .last()
                .map(|row| (row.index, row.origin - gpui::point(px(0.), top))),
            scale,
        };
        self.space = space;
        if self.built.as_ref() == Some(&built) {
            return;
        }
        self.built = Some(built);
        // Only the lines laid this time are kept for the next.
        let mut laid_held = std::mem::take(&mut self.laid);
        if self
            .unlaid_of
            .as_ref()
            .is_none_or(|held| !Arc::ptr_eq(held, projection))
        {
            laid_held.clear();
        }
        self.line_up_unlaid(projection);
        self.selection = (state.selection().anchor(doc), state.selection().head(doc));
        // What a reader hears is what the screen shows: a concealed span reads
        // as what it displays, and as its source while the caret reveals it.
        let revealing = (
            (state.selection().from(doc), state.selection().to(doc)),
            markraft_core::composition::composition_range(state)
                .map(|range| (range.from, range.to)),
        );
        let reveal = Reveal::at(
            revealing.0.0..revealing.0.1,
            revealing.1.map(|(from, to)| from..to),
        );
        self.runs.clear();
        self.controls.clear();
        let last_line = projection.line_count().saturating_sub(1);
        let near = match (rows.first(), rows.last()) {
            (Some(first), Some(last)) => {
                first.index.saturating_sub(NEAR_LINES)..last.index + NEAR_LINES + 1
            }
            _ => 0..NEAR_LINES,
        };
        let mut far = std::mem::take(&mut self.far);
        // Far lines not yet in a run, which all fall in one stretch of
        // `FAR_LINES`.
        let mut pending: Option<Range<usize>> = None;
        let mut laid = rows.iter().peekable();
        for index in 0..projection.line_count() {
            let Some(line) = projection.line(index) else {
                continue;
            };
            while laid.next_if(|row| row.index < index).is_some() {}
            let Some(row) = laid.next_if(|row| row.index == index) else {
                // A line the selection reveals reads afresh, so it is not read
                // with others. A far table cell is, without saying where in
                // its table it stands: every cell is a line, and a long note
                // can hold thousands.
                let alone = near.contains(&index)
                    || reveal.touches(line.from(), line.to())
                    || self.reading(projection, types, &reveal, index).is_none();
                if alone {
                    self.push_far(&mut far, pending.take(), projection, place, space);
                    self.push_unlaid(projection, types, &reveal, index, place(index), space);
                    continue;
                }
                match &mut pending {
                    Some(lines) if index / FAR_LINES == lines.start / FAR_LINES => {
                        lines.end = index + 1;
                    }
                    _ => {
                        self.push_far(&mut far, pending.take(), projection, place, space);
                        pending = Some(index..index + 1);
                    }
                }
                continue;
            };
            self.push_far(&mut far, pending.take(), projection, place, space);
            for run in line.runs() {
                let markraft_core::projection::RunContent::Atom(node) = &run.content else {
                    continue;
                };
                // Inline HTML is text the caret edits in place, so it is no
                // control of its own.
                let control = if Some(node.type_id()) == types.wiki_link {
                    let label = crate::wiki::wiki_link_label(node);
                    Some((
                        ControlAction::OpenWikiLink(line.abs(run.start)),
                        wiki_link_label(label, &self.messages),
                    ))
                } else {
                    None
                };
                if let Some((action, label)) = control
                    && let Some(bounds) = row
                        .rectangles(
                            row.pos_to_offset(line.abs(run.start))
                                ..row.pos_to_offset(line.abs(run.end)),
                            false,
                        )
                        .first()
                {
                    self.controls.push(AccessibleControl {
                        node_id: None,
                        action,
                        label,
                        checked: None,
                        bounds: space.rect(*bounds),
                    });
                }
            }
            // A callout's header is not text anyone can reach with the caret,
            // so what it says reaches a screen reader as a control that puts
            // the caret where the callout's own content starts.
            if let Some((label, bounds)) = row.callout_header() {
                self.controls.push(AccessibleControl {
                    node_id: None,
                    action: ControlAction::EnterCallout(row.from),
                    label: self
                        .messages
                        .format(crate::EditorMessage::Callout, &[("label", label)]),
                    checked: None,
                    bounds: space.rect(bounds),
                });
            }
            if let Some((checked, bounds)) = row.task_marker() {
                let prose: String = line_pieces(projection, types, row.index, &Reveal::nothing())
                    .iter()
                    .map(|piece| piece.text.as_str())
                    .collect();
                self.controls.push(AccessibleControl {
                    node_id: None,
                    action: ControlAction::ToggleTask(row.from),
                    label: if prose.is_empty() {
                        self.messages.text(crate::EditorMessage::Task)
                    } else {
                        prose
                    },
                    checked: Some(checked),
                    bounds: space.rect(bounds),
                });
            }
            let laid = laid_reading(
                &mut laid_held,
                projection,
                types,
                (&reveal, revealing),
                row,
                last_line,
            );
            for (visual, read) in laid.reads.iter().enumerate() {
                let shown = Bounds::new(
                    gpui::point(row.origin.x, row.origin.y + row.visual_top(visual)),
                    gpui::size(row.width, row.visual_height(visual)),
                );
                self.runs.push(TextRun {
                    node_id: None,
                    key: (index, visual),
                    from: read.from,
                    content_end: read.content_end,
                    lengths: read.lengths.clone(),
                    positions: read.positions.clone(),
                    base: 0,
                    text: read.text.clone(),
                    bounds: space.rect(shown),
                    cell: row.table.map(|cell| (cell.row, cell.column)),
                });
            }
            self.laid.insert(index, laid);
        }
        self.push_far(&mut far, pending, projection, place, space);
    }

    /// Line the kept readings of unlaid lines up with `projection`: a line the
    /// edit handed over with its body keeps its reading, and every other line
    /// is read again when it is asked for.
    fn line_up_unlaid(&mut self, projection: &Arc<Projection>) {
        let count = projection.line_count();
        match self.unlaid_of.take() {
            Some(held) if Arc::ptr_eq(&held, projection) => {}
            Some(held) => {
                self.unlaid = crate::surface::kept_ends(held.lines(), projection.lines())
                    .carry(std::mem::take(&mut self.unlaid))
                    .into_iter()
                    .map(Option::flatten)
                    .collect();
            }
            None => self.unlaid = (0..count).map(|_| None).collect(),
        }
        self.unlaid_of = Some(projection.clone());
    }

    /// Line `index`, which no frame laid out, as one run over `bounds`.
    ///
    /// A line the selection or the marked text touches shows its source as
    /// they reveal it, so it is read afresh; any other line reads the same
    /// until its body changes, and is read once.
    fn push_unlaid(
        &mut self,
        projection: &Projection,
        types: &crate::DocTypes,
        reveal: &Reveal,
        index: usize,
        bounds: Bounds<Pixels>,
        space: Space,
    ) {
        let Some(line) = projection.line(index) else {
            return;
        };
        if reveal.touches(line.from(), line.to()) {
            let newline = index + 1 < projection.line_count();
            let read = read_unlaid(0, projection, types, reveal, index, line, newline);
            self.runs
                .push(unlaid_run(&read, index, line, bounds, space));
            return;
        }
        if let Some(read) = self.reading(projection, types, reveal, index) {
            let run = unlaid_run(read, index, line, bounds, space);
            self.runs.push(run);
        }
    }

    /// The kept reading of line `index`, which the selection does not touch,
    /// read now if it has none.
    fn reading(
        &mut self,
        projection: &Projection,
        types: &crate::DocTypes,
        reveal: &Reveal,
        index: usize,
    ) -> Option<&Unlaid> {
        let line = projection.line(index)?;
        let newline = index + 1 < projection.line_count();
        let slot = self.unlaid.get_mut(index)?;
        if slot.as_ref().is_none_or(|kept| kept.newline != newline) {
            self.next_reading += 1;
            let id = self.next_reading;
            *slot = Some(read_unlaid(
                id, projection, types, reveal, index, line, newline,
            ));
        }
        slot.as_ref()
    }

    /// Far `lines`, every one with a kept reading, as one run from the top of
    /// the first to the bottom of the last. What `held` read for the same
    /// lines is used again while each line keeps its reading and its place
    /// from the first.
    fn push_far(
        &mut self,
        held: &mut HashMap<usize, Far>,
        lines: Option<Range<usize>>,
        projection: &Projection,
        place: &dyn Fn(usize) -> Bounds<Pixels>,
        space: Space,
    ) {
        let Some(lines) = lines else {
            return;
        };
        let (Some(first), Some(last)) =
            (projection.line(lines.start), projection.line(lines.end - 1))
        else {
            return;
        };
        let readings: Vec<&Unlaid> = lines
            .clone()
            .filter_map(|index| self.unlaid.get(index)?.as_ref())
            .collect();
        if readings.len() != lines.len() {
            return;
        }
        let keys: Vec<(u64, usize)> = readings
            .iter()
            .zip(lines.clone())
            .map(|(read, index)| {
                let from = projection.line(index).map_or(first.from(), Line::from);
                (read.id, from - first.from())
            })
            .collect();
        let read = match held.remove(&lines.start) {
            Some(kept) if kept.lines == keys => kept,
            _ => {
                let mut text = String::new();
                let mut lengths = Vec::new();
                let mut positions = Vec::new();
                for (read, &(_, from)) in readings.iter().zip(&keys) {
                    // Each line's last boundary is the next one's first,
                    // which starts where the next line does.
                    positions.pop();
                    text.push_str(&read.text);
                    lengths.extend_from_slice(&read.lengths);
                    positions.extend(read.positions.iter().map(|at| at + from));
                }
                Far {
                    lines: keys,
                    text: text.into(),
                    lengths: lengths.into(),
                    positions: positions.into(),
                }
            }
        };
        let bounds = place(lines.start).union(&place(lines.end - 1));
        self.runs.push(TextRun {
            node_id: None,
            key: (lines.start, usize::MAX),
            from: first.from(),
            content_end: last.to(),
            text: read.text.clone(),
            lengths: read.lengths.clone(),
            positions: read.positions.clone(),
            base: first.from(),
            bounds: space.rect(bounds),
            cell: None,
        });
        self.far.insert(lines.start, read);
    }

    pub(crate) fn write(&mut self, builder: &mut A11ySubtreeBuilder) {
        // The rows stand where they do in the content, which the editor's own
        // node moves to where the content stands in the window. That moves
        // the node's own bounds as well, so they are moved back.
        let offset = self.space.offset();
        let editor = builder.parent_node();
        if let Some(mut bounds) = editor.bounds() {
            bounds.y0 -= offset;
            bounds.y1 -= offset;
            editor.set_bounds(bounds);
        }
        editor.set_transform(accesskit::Affine::translate((0., offset)));
        for run in &mut self.runs {
            // Keyed by line and row rather than by document position: a row
            // that stands in for content with no position of its own — an
            // empty line, a divider — shares its position with the row beside
            // it, and typing moves the position of every line after the caret.
            let id = builder.synthetic_node_id(("text", run.key));
            run.node_id = Some(id);
            let mut node = accesskit::Node::new(Role::TextRun);
            node.set_value(&*run.text);
            node.set_character_lengths(&*run.lengths);
            node.set_bounds(run.bounds);
            if let Some((row, column)) = run.cell {
                node.set_row_index(row);
                node.set_column_index(column);
            }
            builder.push_child(id, node);
        }
        for control in &mut self.controls {
            let id = builder.synthetic_node_id(("control", control.action));
            control.node_id = Some(id);
            builder.push_child(id, control.node());
        }
        if let (Some(anchor), Some(focus)) = (
            self.text_position(self.selection.0),
            self.text_position(self.selection.1),
        ) {
            builder
                .parent_node()
                .set_text_selection(accesskit::TextSelection { anchor, focus });
        }
    }

    pub(crate) fn bind_controls(&self, editor: &Entity<crate::EditorView>, window: &mut Window) {
        for control in &self.controls {
            let Some(id) = control.node_id else { continue };
            for activate in [false, true] {
                let editor = editor.clone();
                let action = control.action;
                window.on_a11y_action(
                    id,
                    if activate {
                        accesskit::Action::Click
                    } else {
                        accesskit::Action::Focus
                    },
                    move |_, window: &mut Window, cx: &mut App| {
                        editor.update(cx, |editor, cx| {
                            editor.run_control(action, activate, window, cx);
                        });
                    },
                );
            }
        }
    }

    fn text_position(&self, pos: usize) -> Option<accesskit::TextPosition> {
        let run = self
            .runs
            .iter()
            .rev()
            .find(|run| run.from <= pos && pos <= run.content_end)?;
        let index = run
            .positions
            .partition_point(|&boundary| run.base + boundary < pos);
        let character_index = if run.positions.get(index).map(|at| run.base + at) == Some(pos) {
            index
        } else {
            index.saturating_sub(1)
        };
        Some(accesskit::TextPosition {
            node: run.node_id?,
            character_index,
        })
    }

    fn position(&self, position: accesskit::TextPosition) -> Option<usize> {
        let run = self
            .runs
            .iter()
            .find(|run| run.node_id == Some(position.node))?;
        run.positions
            .get(position.character_index)
            .map(|at| run.base + at)
    }

    pub(crate) fn selection(&self, selection: &accesskit::TextSelection) -> Option<Selection> {
        Some(Selection::text(
            self.position(selection.anchor)?,
            self.position(selection.focus)?,
        ))
    }
}

impl crate::EditorView {
    pub(crate) fn active_code_pos(&self) -> Option<usize> {
        let (index, _) = self.analysis.projection().pos_to_line_offset(self.head())?;
        let line = self.analysis.projection().line(index)?;
        self.types
            .is_code_block(line)
            .then(|| line.block_before())
            .flatten()
    }

    pub(crate) fn run_control(
        &mut self,
        action: ControlAction,
        activate: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.single_line {
            return;
        }
        if self.is_composing() {
            // Cancelling may restore different positions; require a fresh control activation.
            self.cancel_composition(cx);
            return;
        }
        let (index, position) = match action {
            ControlAction::EnterCallout(position) => {
                let Some((index, _)) = self.analysis.projection().pos_to_line_offset(position)
                else {
                    return;
                };
                (index, position)
            }
            ControlAction::OpenWikiLink(position) => {
                if self.wiki_link_at(position).is_none() {
                    return;
                }
                let Some((index, _)) = self.analysis.projection().pos_to_line_offset(position)
                else {
                    return;
                };
                (index, position)
            }
            ControlAction::ToggleTask(position) => {
                let Some((index, _)) = self.analysis.projection().pos_to_line_offset(position)
                else {
                    return;
                };
                let Some(line) = self.analysis.projection().line(index) else {
                    return;
                };
                if self
                    .types
                    .item_of(line)
                    .is_none_or(|(item, _)| Some(item.node_type) != self.types.task_item)
                {
                    return;
                }
                (index, position)
            }
            ControlAction::CodeLanguage(pos) | ControlAction::CopyCode(pos) => {
                let Some((index, line)) = self
                    .analysis
                    .projection()
                    .lines()
                    .iter()
                    .enumerate()
                    .find(|(_, line)| {
                        self.types.is_code_block(line) && line.block_before() == Some(pos)
                    })
                else {
                    return;
                };
                (index, line.from())
            }
        };
        if !activate
            || matches!(
                action,
                ControlAction::ToggleTask(_) | ControlAction::EnterCallout(_)
            )
        {
            window.focus(&self.focus, cx);
            self.select(position, false, cx);
        }
        if !activate {
            return;
        }
        match action {
            ControlAction::OpenWikiLink(pos) => {
                if let Some(node) = self.wiki_link_at(pos) {
                    cx.emit(crate::EditorEvent::WikiLinkClicked {
                        target: crate::wiki::wiki_link_target(&node),
                        embed: crate::wiki::wiki_link_embed(&node),
                    });
                }
            }
            ControlAction::ToggleTask(_) => {
                self.run_command(&markraft_core::kind::chains::toggle_task(&self.types), cx);
            }
            ControlAction::CodeLanguage(pos) => {
                cx.emit(crate::EditorEvent::CodeLanguageRequested { pos })
            }
            // Selecting the position is the whole action: the header names the
            // callout, and reaching it means reaching its content.
            ControlAction::EnterCallout(_) => {}
            ControlAction::CopyCode(_) => {
                if let Some(text) = self.analysis.projection().line_text(index) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_owned()));
                    cx.emit(crate::EditorEvent::CodeCopied);
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// A reading kept from before an edit is what reading the edited note
    /// afresh gives: the lines the edit moved along start where they now
    /// start, and the line it changed reads as it now does.
    #[test]
    fn kept_readings_are_fresh_readings() {
        use markraft_core::{Selection, TransactionSpec};
        let source = "first **bold**\n\n| a | b |\n| - | - |\n| c | d |\n\n> quoted *text*\n\nlast";
        let state = crate::typeahead::tests::state_of(source);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let place = |_: usize| Bounds::default();
        let read = |text: &AccessibleText| {
            text.runs
                .iter()
                .map(|run| {
                    (
                        run.from,
                        run.content_end,
                        run.text.to_string(),
                        run.lengths.to_vec(),
                        run.positions
                            .iter()
                            .map(|at| run.base + at)
                            .collect::<Vec<_>>(),
                        run.cell,
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut kept = AccessibleText::default();
        let projection = markraft_core::projection::projection_of(&state);
        kept.update(
            &projection,
            &state,
            &types,
            &[],
            &place,
            Space::new(px(0.), 1.),
        );
        // Typed at the start, so every line after the first moves along.
        let edited = state
            .update([TransactionSpec::new().selection(Selection::cursor(1))])
            .expect("a caret")
            .state()
            .clone();
        let edited = edited
            .update([markraft_core::commands::insert_text("new ")(&edited).expect("an insertion")])
            .expect("an edit")
            .state()
            .clone();
        let projection = markraft_core::projection::projection_of(&edited);
        kept.update(
            &projection,
            &edited,
            &types,
            &[],
            &place,
            Space::new(px(0.), 1.),
        );
        let mut fresh = AccessibleText::default();
        fresh.update(
            &projection,
            &edited,
            &types,
            &[],
            &place,
            Space::new(px(0.), 1.),
        );
        assert_eq!(read(&kept), read(&fresh));
        assert!(read(&kept)[0].2.starts_with("new first"));
    }

    /// A line the frame did not lay out still reaches a reader, as one run
    /// standing where the lines say it is.
    #[test]
    fn lines_the_frame_did_not_lay_out_are_still_read() {
        let state = crate::typeahead::tests::state_of("first\n\n**second**\n\nthird");
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let style = crate::EditorStyle::notes();
        let images = crate::images::Images::default();
        let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(gpui::TextSystem::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
        )));
        let rows = crate::surface::shape(
            &crate::surface::ShapeInput {
                maths: None,
                equations: None,
                scale_factor: 1.0,
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                images: &images,
                messages: &crate::EditorMessages::ENGLISH,
                wiki: None,
                selection: 0..0,
                spelling: None,
                composition: None,
            },
            gpui::px(400.),
            &text_system,
        );
        // Only the first line was laid out.
        let place = |index: usize| {
            Bounds::new(
                gpui::point(gpui::px(0.), gpui::px(100. * index as f32)),
                gpui::size(gpui::px(400.), gpui::px(20.)),
            )
        };
        let mut text = AccessibleText::default();
        text.update(
            &projection,
            &state,
            &types,
            &rows[..1],
            &place,
            Space::new(px(0.), 1.),
        );
        let read: Vec<&str> = text.runs.iter().map(|run| &*run.text).collect();
        assert_eq!(read, ["first\n", "second\n", "third"]);
        assert_eq!(text.runs[2].bounds.y0, 200.);
    }

    #[test]
    fn controls_expose_states_actions_and_document_targets() {
        let state = crate::typeahead::tests::state_of(
            "7. [ ] open\n8. [x] done\n\n```rust\nlet x = 1;\n```\n\ntext <span>raw</span>",
        );
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let style = crate::EditorStyle::notes();
        let images = crate::images::Images::default();
        let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(gpui::TextSystem::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
        )));
        let rows = crate::surface::shape(
            &crate::surface::ShapeInput {
                maths: None,
                equations: None,
                scale_factor: 1.0,
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                images: &images,
                messages: &crate::EditorMessages::ENGLISH,
                wiki: None,
                selection: 0..0,
                spelling: None,
                composition: None,
            },
            gpui::px(400.),
            &text_system,
        );
        let mut text = AccessibleText::default();
        text.update(
            &projection,
            &state,
            &types,
            &rows,
            &|_| Bounds::default(),
            Space::new(px(0.), 2.),
        );
        // Inline HTML is edited as text in place, so only the task boxes are
        // controls.
        assert_eq!(text.controls.len(), 2);
        for (control, checked) in text.controls[..2].iter().zip([false, true]) {
            let node = control.node();
            assert_eq!(node.role(), Role::CheckBox);
            assert_eq!(
                node.toggled(),
                Some(if checked {
                    accesskit::Toggled::True
                } else {
                    accesskit::Toggled::False
                })
            );
            assert!(node.supports_action(accesskit::Action::Click));
            assert!(node.supports_action(accesskit::Action::Focus));
            assert!(matches!(control.action, ControlAction::ToggleTask(_)));
        }
        // A code block has no controls of its own on screen; its language and
        // copy are commands.
        assert!(rows[2].code_pos.is_some());
    }

    /// A screen reader reads what the screen shows: concealed delimiters are
    /// left out, an entity reads as its character and spans the whole entity,
    /// and the span the caret is in reads as its source.
    #[test]
    fn accessible_text_reads_concealed_runs_as_they_are_shown() {
        let state = crate::typeahead::tests::state_of("x **ab** &amp; y");
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let read = |state: &EditorState| {
            let projection = markraft_core::projection::projection_of(state);
            let style = crate::EditorStyle::notes();
            let images = crate::images::Images::default();
            let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(
                gpui::TextSystem::new(std::sync::Arc::new(gpui::NoopTextSystem::new())),
            ));
            let doc = state.doc();
            let rows = crate::surface::shape(
                &crate::surface::ShapeInput {
                    maths: None,
                    equations: None,
                    scale_factor: 1.0,
                    doc,
                    types: &types,
                    projection: &projection,
                    style: &style,
                    single_line: false,
                    images: &images,
                    messages: &crate::EditorMessages::ENGLISH,
                    wiki: None,
                    selection: state.selection().from(doc)..state.selection().to(doc),
                    spelling: None,
                    composition: None,
                },
                gpui::px(400.),
                &text_system,
            );
            let mut text = AccessibleText::default();
            text.update(
                &projection,
                state,
                &types,
                &rows,
                &|_| Bounds::default(),
                Space::new(px(0.), 1.),
            );
            let run = text.runs.remove(0);
            let line = &projection.lines()[0];
            let offsets: Vec<usize> = run
                .positions
                .iter()
                .map(|pos| line.pos_to_offset(run.base + pos).unwrap())
                .collect();
            (run.text.to_string(), offsets)
        };
        let projection = markraft_core::projection::projection_of(&state);
        let at = |offset| projection.lines()[0].offset_to_pos(offset).unwrap();
        let away = state
            .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(at(0)))])
            .unwrap()
            .state()
            .clone();
        let (text, offsets) = read(&away);
        assert_eq!(text, "x ab & y");
        // x, space, a, b, space, the entity, space, y — and the end.
        assert_eq!(offsets, vec![0, 1, 4, 5, 8, 9, 14, 15, 16]);
        let inside = state
            .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(at(5)))])
            .unwrap()
            .state()
            .clone();
        assert_eq!(read(&inside).0, "x **ab** & y");
    }

    #[test]
    fn accessible_units_preserve_graphemes_and_represent_every_byte() {
        let text = "你好 é 👨‍👩‍👧‍👦\n";
        let offsets = character_offsets(text);
        assert_eq!(offsets.len() - 1, text.graphemes(true).count());
        assert_eq!(offsets.last(), Some(&text.len()));
        let long = format!("e{}", "\u{301}".repeat(200));
        let offsets = character_offsets(&long);
        assert!(offsets.windows(2).all(|pair| pair[1] - pair[0] <= 255));
        assert_eq!(offsets.last(), Some(&long.len()));
    }

    #[test]
    fn accessible_positions_skip_hidden_inline_boundaries() {
        let state = crate::typeahead::tests::state_of("*你 **好** é*");
        let projection = markraft_core::projection::projection_of(&state);
        let line = &projection.lines()[0];
        let value = projection.line_text(0).unwrap();
        let offsets = character_offsets(value);
        let positions: Vec<_> = offsets
            .iter()
            .map(|&byte| line.offset_to_pos(value[..byte].chars().count()).unwrap())
            .collect();
        let text = AccessibleText {
            runs: vec![TextRun {
                node_id: Some(accesskit::NodeId(1)),
                key: (0, 0),
                from: line.from(),
                content_end: line.to(),
                text: value.into(),
                lengths: unit_lengths(&offsets).into(),
                positions: positions.clone().into(),
                base: 0,
                bounds: accesskit::Rect::ZERO,
                cell: None,
            }],
            selection: (line.from(), line.to()),
            ..AccessibleText::default()
        };
        for (index, pos) in positions.into_iter().enumerate() {
            let accessible = text.text_position(pos).unwrap();
            assert_eq!(accessible.character_index, index);
            assert_eq!(text.position(accessible), Some(pos));
        }
        assert_eq!(text.text_position(line.from()).unwrap().character_index, 0);
    }

    #[test]
    fn accessible_positions_roundtrip_wrapped_lines_and_block_boundaries() {
        let run = |id, from, end, text: &str| TextRun {
            node_id: Some(accesskit::NodeId(id)),
            key: (id as usize, 0),
            from,
            content_end: end,
            text: text.into(),
            lengths: unit_lengths(&character_offsets(text)).into(),
            positions: character_offsets(text)
                .into_iter()
                .map(|byte| (text[..byte].chars().count()).min(end - from))
                .collect(),
            base: from,
            bounds: accesskit::Rect::ZERO,
            cell: None,
        };
        // Two visual rows of one line holding "你好", then the next block.
        let text = AccessibleText {
            runs: vec![run(1, 1, 2, "你"), run(2, 2, 3, "好\n")],
            selection: (1, 1),
            ..AccessibleText::default()
        };
        let boundary = 2;
        let accessible = text.text_position(boundary).unwrap();
        assert_eq!(accessible.node, accesskit::NodeId(2));
        assert_eq!(accessible.character_index, 0);
        assert_eq!(text.position(accessible), Some(boundary));
        // The synthetic newline maps back to the end of the run's own content.
        assert_eq!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(2),
                character_index: 2
            }),
            Some(3)
        );
        assert!(
            text.position(accesskit::TextPosition {
                node: accesskit::NodeId(99),
                character_index: 0
            })
            .is_none()
        );
    }

    /// Reading a line's rows one after another gives what reading each row
    /// on its own gives, whatever order the rows come in.
    #[test]
    fn rows_read_in_turn_read_as_rows_read_alone() {
        fn alone(pieces: &[ShownPiece], inner: Range<usize>) -> (String, Vec<usize>) {
            let mut value = String::new();
            let mut before = Vec::new();
            for piece in pieces {
                if piece.own {
                    let from = piece.source.start.max(inner.start);
                    let to = piece.source.end.min(inner.end);
                    for (index, c) in piece.text.chars().enumerate() {
                        let at = piece.source.start + index;
                        if (from..to).contains(&at) {
                            value.push(c);
                            before.push(at);
                        }
                    }
                } else if inner.contains(&piece.source.start) {
                    for c in piece.text.chars() {
                        value.push(c);
                        before.push(piece.source.start);
                    }
                }
            }
            (value, before)
        }
        let piece = |source: Range<usize>, text: &str, own| ShownPiece {
            source,
            text: text.into(),
            own,
        };
        let pieces = [
            piece(0..4, "ab你é", true),
            piece(4..9, "&", false),
            piece(9..9, "🙂", false),
            piece(9..15, "cd 好ef", true),
            piece(15..20, "label", false),
            piece(20..23, "xyz", true),
        ];
        let rowings: [&[Range<usize>]; 5] = [
            &[0..3, 3..9, 9..12, 12..23],
            &[0..2, 5..10, 11..11, 14..21, 22..23],
            &[0..9, 9..9, 9..23],
            &[3..12, 1..4, 10..23, 0..23],
            &[0..23, 23..23],
        ];
        for rows in rowings {
            let mut shown = ShownRows::new(&pieces);
            for row in rows {
                assert_eq!(
                    shown.row(row.clone()),
                    alone(&pieces, row.clone()),
                    "{row:?} of {rows:?}"
                );
            }
        }
    }

    /// Lines far from the screen read together, each stretch starting at a
    /// multiple of `FAR_LINES`, and say what reading them one by one says.
    /// An edit far above them leaves what they read to be used again.
    #[test]
    fn far_lines_read_together_and_are_read_once() {
        use markraft_core::TransactionSpec;
        let count = NEAR_LINES + 2 * FAR_LINES + 40;
        let source = (0..count)
            .map(|i| format!("line {i} **bold** 你好"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let state = crate::typeahead::tests::state_of(&source);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let place = |index: usize| {
            Bounds::new(
                gpui::point(gpui::px(0.), gpui::px(10. * index as f32)),
                gpui::size(gpui::px(400.), gpui::px(10.)),
            )
        };
        let read = |text: &mut AccessibleText, state: &EditorState| {
            let projection = markraft_core::projection::projection_of(state);
            text.update(
                &projection,
                state,
                &types,
                &[],
                &place,
                Space::new(px(0.), 1.),
            );
            projection
        };
        let mut text = AccessibleText::default();
        let projection = read(&mut text, &state);
        assert_eq!(projection.line_count(), count);
        let far: Vec<&TextRun> = text
            .runs
            .iter()
            .filter(|run| run.key.1 == usize::MAX)
            .collect();
        assert_eq!(text.runs.len(), NEAR_LINES + 3);
        assert_eq!(
            far.iter().map(|run| run.key.0).collect::<Vec<_>>(),
            [
                NEAR_LINES,
                NEAR_LINES + FAR_LINES,
                NEAR_LINES + 2 * FAR_LINES
            ]
        );
        assert_eq!(far[2].bounds.y1, 10. * count as f64);
        // The whole note, as reading every line alone gives it.
        let mut whole = String::new();
        for index in 0..count {
            let line = projection.line(index).unwrap();
            let alone = read_unlaid(
                0,
                &projection,
                &types,
                &Reveal::nothing(),
                index,
                line,
                index + 1 < count,
            );
            whole.push_str(&alone.text);
            // Each line starts at its own unit of the run that reads it.
            let run = text
                .runs
                .iter()
                .find(|run| run.from <= line.from() && line.to() <= run.content_end)
                .unwrap();
            let unit = run
                .text
                .split_inclusive('\n')
                .take(index - run.key.0)
                .map(|piece| character_offsets(piece).len() - 1)
                .sum::<usize>();
            assert_eq!(run.base + run.positions[unit], line.from(), "line {index}");
        }
        let all: String = text.runs.iter().map(|run| &*run.text).collect();
        assert_eq!(all, whole);
        for run in &text.runs {
            assert_eq!(run.lengths.len() + 1, run.positions.len());
            assert_eq!(
                run.lengths
                    .iter()
                    .map(|&len| usize::from(len))
                    .sum::<usize>(),
                run.text.len()
            );
            assert_eq!(run.base + run.positions.last().unwrap(), run.content_end);
        }
        // Typed in the first line: every far line moves along and reads as it
        // did, from what was read before.
        let before: Vec<Arc<str>> = far.iter().map(|run| run.text.clone()).collect();
        let edited = state
            .update([TransactionSpec::new().selection(Selection::cursor(1))])
            .unwrap()
            .state()
            .clone();
        let edited = edited
            .update([markraft_core::commands::insert_text("new ")(&edited).unwrap()])
            .unwrap()
            .state()
            .clone();
        read(&mut text, &edited);
        let mut fresh = AccessibleText::default();
        read(&mut fresh, &edited);
        let runs = |text: &AccessibleText| {
            text.runs
                .iter()
                .map(|run| {
                    let positions: Vec<usize> =
                        run.positions.iter().map(|at| run.base + at).collect();
                    (
                        run.key,
                        run.from,
                        run.content_end,
                        run.text.to_string(),
                        positions,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(runs(&text), runs(&fresh));
        let after: Vec<&Arc<str>> = text
            .runs
            .iter()
            .filter(|run| run.key.1 == usize::MAX)
            .map(|run| &run.text)
            .collect();
        assert!(
            before.iter().zip(after).all(|(a, b)| Arc::ptr_eq(a, b)),
            "far lines are read again"
        );
    }

    /// A frame that only scrolls moves the rows a reader is told of without
    /// reading them again; a selection that reveals a line's source does.
    #[test]
    fn laid_lines_read_again_only_when_what_they_show_changes() {
        let state = crate::typeahead::tests::state_of("first\n\nsecond\n\nx **ab** y");
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let style = crate::EditorStyle::notes();
        let images = crate::images::Images::default();
        let text_system = gpui::WindowTextSystem::new(std::sync::Arc::new(gpui::TextSystem::new(
            std::sync::Arc::new(gpui::NoopTextSystem::new()),
        )));
        let mut rows = crate::surface::shape(
            &crate::surface::ShapeInput {
                maths: None,
                equations: None,
                scale_factor: 1.0,
                doc: state.doc(),
                types: &types,
                projection: &projection,
                style: &style,
                single_line: false,
                images: &images,
                messages: &crate::EditorMessages::ENGLISH,
                wiki: None,
                selection: 0..0,
                spelling: None,
                composition: None,
            },
            gpui::px(400.),
            &text_system,
        );
        let place = |_: usize| Bounds::default();
        let mut text = AccessibleText::default();
        text.update(
            &projection,
            &state,
            &types,
            &rows,
            &place,
            Space::new(px(0.), 1.),
        );
        let before: Vec<(Arc<str>, f64)> = text
            .runs
            .iter()
            .map(|run| (run.text.clone(), run.bounds.y0))
            .collect();
        assert_eq!(&*before[2].0, "x ab y");
        for row in &mut rows {
            row.origin.y += gpui::px(50.);
        }
        text.update(
            &projection,
            &state,
            &types,
            &rows,
            &place,
            Space::new(px(0.), 1.),
        );
        for (run, (read, y)) in text.runs.iter().zip(&before) {
            assert!(Arc::ptr_eq(&run.text, read), "{read}");
            assert_eq!(run.bounds.y0, y + 50.);
        }
        // Scrolled: the rows and the content's top move together, and what a
        // reader is told of the rows stays as it was.
        let moved: Vec<f64> = text.runs.iter().map(|run| run.bounds.y0).collect();
        for row in &mut rows {
            row.origin.y += gpui::px(30.);
        }
        text.update(
            &projection,
            &state,
            &types,
            &rows,
            &place,
            Space::new(px(30.), 1.),
        );
        assert_eq!(
            text.runs
                .iter()
                .map(|run| run.bounds.y0)
                .collect::<Vec<_>>(),
            moved
        );
        assert_eq!(text.space.offset(), 30.);
        let line = &projection.lines()[2];
        let inside = state
            .update([markraft_core::TransactionSpec::new()
                .selection(Selection::cursor(line.offset_to_pos(5).unwrap()))])
            .unwrap()
            .state()
            .clone();
        text.update(
            &projection,
            &inside,
            &types,
            &rows,
            &place,
            Space::new(px(30.), 1.),
        );
        // The selection never touched the middle line.
        assert!(Arc::ptr_eq(&text.runs[1].text, &before[1].0));
        assert_eq!(&*text.runs[2].text, "x **ab** y");
    }

    /// A far table's cells are read with the lines around them; near the
    /// screen each says where in its table it stands.
    #[test]
    fn far_table_cells_read_with_far_lines() {
        let mut source = (0..NEAR_LINES + 10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        source.push_str("\n\n| a | b |\n| - | - |\n| c | d |\n\nlast");
        let state = crate::typeahead::tests::state_of(&source);
        let projection = markraft_core::projection::projection_of(&state);
        let types = crate::DocTypes::from_schema_names(
            state.schema(),
            &markraft_commonmark::commonmark_doc_type_names(),
        );
        let mut text = AccessibleText::default();
        let place = |_: usize| Bounds::default();
        text.update(
            &projection,
            &state,
            &types,
            &[],
            &place,
            Space::new(px(0.), 1.),
        );
        let far = text.runs.last().unwrap();
        assert_eq!(far.key, (NEAR_LINES, usize::MAX));
        assert_eq!(far.cell, None);
        assert!(far.text.ends_with("a\nb\nc\nd\nlast"), "{}", far.text);
        assert!(text.runs[..NEAR_LINES].iter().all(|run| run.key.1 == 0));
    }
}
