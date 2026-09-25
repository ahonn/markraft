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
            slot: slot.size.width,
            visual_row: ((slot.origin.y - layout.origin.y) / layout.line_height).round() as usize,
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
    /// The height a drawn image needs, where it is taller than a text row.
    pub(super) line_height: Option<Pixels>,
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
            line_height: None,
        }
    }
}

/// A painted atom the display text has reserved room for, before shaping says
/// where on the block it landed.
pub(super) struct PendingAtom {
    /// `char` range of the placeholder within the display text.
    pub(super) chars: Range<usize>,
    pub(super) label: Rc<ShapedLine>,
    pub(super) image: Option<(Arc<RenderImage>, Size<Pixels>)>,
    pub(super) frame: Option<Size<Pixels>>,
    pub(super) note: bool,
}

pub(super) fn display_text(
    input: &ShapeInput<'_>,
    line: &Line,
    index: usize,
    font_size: Pixels,
    column: Pixels,
    text_system: &WindowTextSystem,
) -> DisplayText {
    let projection = input.projection;
    if line.kind() == LineKind::LeafBlock {
        return DisplayText::stand_in(" ".to_owned());
    }
    let source = projection.line_text(index).unwrap_or_default();
    if source.is_empty() {
        return DisplayText::stand_in(" ".to_owned());
    }
    // An image that has its line to itself is drawn at full size; one sharing
    // its line with text has to stay within a row.
    let alone = line.runs().len() == 1 && line.len() == 1;
    let shown = markraft_core::kind::conceal::shown(input.types.syntax, line, &reveal_of(input));
    let mut text = String::with_capacity(source.len());
    let mut run_bytes = Vec::with_capacity(line.runs().len());
    let mut widenings = Vec::new();
    let mut atoms = Vec::new();
    let mut byte = 0usize;
    let mut display = 0usize;
    let mut filler: Option<Pixels> = None;
    let mut line_height = None;
    for (index_in_line, run) in line.runs().iter().enumerate() {
        let chars = run.char_to - run.char_from;
        let len: usize = source[byte..].chars().take(chars).map(char::len_utf8).sum();
        let slice = &source[byte..byte + len];
        byte += len;
        match atom_of(input, line, run, font_size, column, alone, text_system) {
            Some(atom) => {
                // An atom the row can shape *is* its text: writing the label
                // into the display text gives it exactly the width its glyphs
                // advance, so what follows sits against it. A placeholder
                // rounded up to whole fillers would leave a gap behind.
                let count = if atom.shape.is_own_text() && !atom.text.is_empty() {
                    text.push_str(&atom.text);
                    run_bytes.push(atom.text.len());
                    atom.text.chars().count()
                } else {
                    // One atom is one character, and a pill advances nowhere
                    // near far enough for what is drawn over it, so the row
                    // reserves the width in fillers. An atom with nothing to
                    // shape keeps one, which is its caret stop.
                    let unit = *filler.get_or_insert_with(|| filler_width(font_size, text_system));
                    let count = (atom.width / unit).ceil().max(1.) as usize;
                    text.extend(std::iter::repeat_n(PILL_FILLER, count));
                    run_bytes.push(count * PILL_FILLER.len_utf8());
                    // A picture with the line to itself sets the row's height;
                    // one sharing it with text fits inside the row it is in.
                    if alone
                        && let Some(size) =
                            atom.image.as_ref().map(|(_, size)| *size).or(atom.frame)
                    {
                        line_height = Some(size.height);
                    }
                    atoms.push(PendingAtom {
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
        line_height,
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

/// The advance of one [`PILL_FILLER`], which is the unit a placeholder reserves
/// its width in.
pub(super) fn filler_width(font_size: Pixels, text_system: &WindowTextSystem) -> Pixels {
    const SAMPLE: usize = 8;
    let text: String = std::iter::repeat_n(PILL_FILLER, SAMPLE).collect();
    let shaped = text_system.shape_line(
        text.clone().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: font(UI_FONT),
            color: gpui::transparent_black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    (shaped.width / SAMPLE as f32).max(px(1.))
}

/// An inline atom's content and the width it needs.
pub(super) struct Atom {
    pub(super) shape: AtomShape,
    /// The label as it reaches the screen, shortened where it would not fit the
    /// column. The row shapes this itself where the shape is its own text.
    pub(super) text: String,
    pub(super) label: Rc<ShapedLine>,
    pub(super) width: Pixels,
    pub(super) image: Option<(Arc<RenderImage>, Size<Pixels>)>,
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

/// What an inline atom is drawn as, for the atoms the view draws itself: an
/// image's label, the verbatim source of an inline HTML primitive, a wiki
/// link's label, or the emoji a shortcode names. Every other atom keeps the object-replacement character the
/// projection gave it, which is blank.
pub(super) fn atom_label<'a>(types: &DocTypes, node: &'a Node) -> Option<(AtomShape, &'a str)> {
    let ty = node.type_id();
    if Some(ty) == types.image {
        let label = match (attr(node, "alt"), file_name(attr(node, "src"))) {
            ("", Some(name)) => name,
            ("", None) => "Image",
            (alt, _) => alt,
        };
        Some((AtomShape::Pill, label))
    } else if Some(ty) == types.raw_inline {
        // HTML is kept verbatim and shown as source, so the tag reads exactly as
        // it was written — a closing tag included.
        Some((AtomShape::Text, attr(node, "source")))
    } else if Some(ty) == types.wiki_link {
        if crate::wiki::wiki_link_embed(node) {
            // `![[…]]` puts a file in the note rather than pointing at a page, so
            // it reads as the picture it is: its alias, or the file it names.
            let label = match (attr(node, "alias"), file_name(attr(node, "target"))) {
                ("", Some(name)) => name,
                ("", None) => "Embed",
                (alias, _) => alias,
            };
            return Some((AtomShape::Pill, label));
        }
        // The alias is what the author wrote it to read as; without one the
        // target stands in, with whatever `#heading` or `^block` it names,
        // because that is what the link says.
        Some((AtomShape::Link, crate::wiki::wiki_link_label(node)))
    } else if Some(ty) == types.emoji {
        // A code the table does not know was never read as one, but an atom
        // built by hand could hold one: it reads as its name.
        let code = attr(node, "code");
        match emojis::get_by_shortcode(code) {
            Some(emoji) => Some((AtomShape::Glyph, emoji.as_str())),
            None => Some((AtomShape::Text, code)),
        }
    } else {
        None
    }
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
            Err(error) => format!("{}: {original}", error.label()),
            Ok(_) if !alone => format!("Inline image: {original}"),
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
            drawn_image(images, source, column)
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

/// A decoded image and the size it is drawn at: its own, or the column's width
/// where it is wider. A tall picture is drawn tall rather
/// than shrunk into a thumbnail no one can read.
pub(super) fn drawn_image(
    images: &crate::images::Images,
    src: &str,
    column: Pixels,
) -> Option<(Arc<RenderImage>, Size<Pixels>)> {
    let image = images.load(src).ok()?;
    let intrinsic = image.size(0);
    let (native_width, native_height) = (intrinsic.width.0 as f32, intrinsic.height.0 as f32);
    if native_width <= 0. || native_height <= 0. {
        return None;
    }
    let width = column.min(px(native_width)).max(px(1.));
    let height = width * (native_height / native_width);
    Some((image, size(width, height)))
}

/// A decoded image fitted to a row: `height` tall, as wide as its proportions
/// make it, and no wider than the column.
pub(super) fn inline_image(
    images: &crate::images::Images,
    src: &str,
    height: Pixels,
    column: Pixels,
) -> Option<(Arc<RenderImage>, Size<Pixels>)> {
    let image = images.load(src).ok()?;
    let intrinsic = image.size(0);
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

/// The file name an image source ends in, for a placeholder with no alt text.
pub(super) fn file_name(src: &str) -> Option<&str> {
    let path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .trim_end_matches('/');
    let name = path.rsplit(['/', '\\']).next()?;
    (!name.is_empty()).then_some(name)
}
