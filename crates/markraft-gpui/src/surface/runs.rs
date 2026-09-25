//! Text runs: the font, colour and decoration of every stretch of a line,
//! syntax highlighting included. Shaping hands them to the text system.

use super::*;

pub(super) struct Runs {
    pub(super) runs: Vec<TextRun>,
    /// Inline code and superscript within the whole line text, with their face.
    pub(super) code: Vec<Repaint>,
}

pub(super) fn text_runs(
    input: &ShapeInput<'_>,
    line: &Line,
    text: &DisplayText,
    heading: Option<u8>,
    code_block: bool,
    font_size: Pixels,
    style: &EditorStyle,
) -> Runs {
    let types = input.types;
    // A header cell is bold as a heading is: the weight is what says the row
    // names the columns rather than holding data.
    let header = types.is_table_header(line);
    let checked_item = types.in_checked_item(line);
    // A raw block's source is shown as source: monospaced and quiet, so it reads
    // as the markup it is rather than as prose. It carries no chrome, so the
    // face and the colour are all that say so.
    let raw = types.is_raw_block(line);
    let text_color = if checked_item || raw {
        style.muted_text
    } else {
        style.text
    };
    // A ticked item is greyed and struck through as a whole, but an atom and a
    // link keep the colours that say what they are.
    let done = checked_item.then_some(StrikethroughStyle {
        thickness: px(1.),
        color: Some(style.muted_text),
    });
    if text.synthetic {
        let mut face = font(style.font_family.clone());
        if heading.is_some() {
            face.weight = FontWeight::BOLD;
        }
        if raw {
            face = font(CODE_FONT);
        }
        return Runs {
            runs: vec![TextRun {
                len: text.text.len(),
                font: face,
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            code: Vec::new(),
        };
    }
    let mut runs = Vec::with_capacity(line.runs().len());
    let mut code = Vec::new();
    let mut byte = 0usize;
    for (index, run) in line.runs().iter().enumerate() {
        let len = text.run_bytes.get(index).copied().unwrap_or(0);
        let range = byte..byte + len;
        byte += len;
        if len == 0 {
            continue;
        }
        let marks = &run.marks;
        let is_math = has(types.math, marks) && !code_block;
        let is_code = code_block || raw || has(types.code, marks) || is_math;
        // A footnote reference is drawn in the link colour, raised, and without
        // the underline a link has.
        let footnote = has(types.footnote_reference, marks);
        let is_link = has(types.link, marks) || footnote;
        let mut face = if is_code {
            font(CODE_FONT)
        } else {
            font(style.font_family.clone())
        };
        if has(types.strong, marks) || heading.is_some() || header {
            face.weight = FontWeight::BOLD;
        }
        if has(types.em, marks) {
            face.style = FontStyle::Italic;
            // Asked for an italic it does not have, the rounded design would set the
            // emphasis upright; the system face it is drawn from has one.
            if face.family.as_ref() == ROUNDED_FONT {
                face.family = UI_FONT.into();
            }
        }
        let atom = matches!(run.content, RunContent::Atom(_));
        // What an atom's placeholder holds: a pill is painted over its fillers,
        // and the other shapes are the row's own text. A wiki link's label is
        // prose, so it keeps the face this run already decided on — the
        // heading's weight, the emphasis around it — in the link colour an atom
        // draws in anyway; an HTML primitive is source, and reads as the quiet
        // monospaced markup it is wherever it sits.
        let widening = text
            .widenings
            .iter()
            .find(|widening| widening.source == run.char_from);
        let placeholder = widening.and_then(|widening| widening.shape);
        let source_atom = placeholder == Some(AtomShape::Source);
        if source_atom {
            face = font(CODE_FONT);
        }
        let widened = placeholder == Some(AtomShape::Pill);
        // A pill's fillers reserve the width `filler_width` measured in the
        // chrome face, so they are shaped in it whatever the note's face is.
        if widened && !is_code {
            face.family = UI_FONT.into();
        }
        // An emoji is a character of the sentence, not something to follow, and
        // source shown as text is the sentence's own text.
        let glyph = matches!(
            placeholder,
            Some(AtomShape::Glyph | AtomShape::Text | AtomShape::Break)
        );
        // Revealed markup — a `**`, a link's `](…)`, an escape's `\` — is
        // quieter than the text it styles, so the words still read first.
        let markup = has(types.syntax, marks) && !code_block && !raw;
        let ink = if source_atom || markup {
            style.muted_text
        } else if widening.is_some_and(|widening| widening.broken) {
            style.broken_link
        } else if is_link || (atom && !code_block && !glyph) {
            style.link
        } else if has(types.code, marks) || is_math {
            style.inline_code_text
        } else {
            text_color
        };
        // Inline code and scripts only reserve their space here; see `InlineCode`.
        let inline_code = has(types.code, marks) && !code_block;
        let script = !code_block && !raw && !atom;
        let lowered = has(types.subscript, marks) && script;
        let raised = ((has(types.superscript, marks) || footnote) && script) || lowered;
        let color = if widened {
            gpui::transparent_black()
        } else if inline_code || raised {
            code.push(Repaint {
                runs: vec![(range.len(), face.clone(), ink)],
                range,
                raised: raised && !inline_code,
                lowered: lowered && !inline_code,
            });
            gpui::transparent_black()
        } else {
            ink
        };
        runs.push(TextRun {
            len,
            font: face,
            color,
            // A code span inside a highlight takes the fill too, across its
            // pill's whole slot, so the band is unbroken; the pill is painted
            // over the fill.
            background_color: (has(types.highlight, marks) && !code_block)
                .then_some(style.highlight),
            underline: (!widened
                && !markup
                && (has(types.underline, marks) || has(types.link, marks)))
            .then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(ink),
                wavy: false,
            }),
            strikethrough: (has(types.strikethrough, marks) && !markup)
                .then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(ink),
                })
                .or(done),
        });
    }
    if code_block {
        let language = types.code_language(line).unwrap_or("");
        let highlighted = crate::syntax::highlight(&text.text, language, style.background.l < 0.5);
        let total: usize = highlighted
            .iter()
            .map(|row| row.iter().map(|(len, _)| len).sum::<usize>())
            .sum::<usize>()
            + highlighted.len().saturating_sub(1);
        if total == text.text.len() {
            runs = highlight_runs(&highlighted, font_size);
        }
    }
    // A raw block the kind reads more in — a run of link definitions — is drawn
    // as the prose it describes; every character stays.
    if raw && let Some(spelling) = input.spelling {
        let highlights = spelling.source_highlights(line);
        let chars = text.text.chars().count();
        if !highlights.is_empty() && highlights.iter().all(|(range, _)| range.end <= chars) {
            runs = source_highlight_runs(&text.text, &highlights, style);
        }
    }
    // A revealed syntax run is a run of its own, in the quieter markup ink;
    // without merging, each backtick would paint a separate pill.
    Runs {
        runs,
        code: merge_adjacent_code(code),
    }
}

/// Join contiguous inline-code byte ranges into one pill, and contiguous superscript
/// into one raised run. Each part keeps its own face and ink as a run of the
/// merged span.
pub(super) fn merge_adjacent_code(code: Vec<Repaint>) -> Vec<Repaint> {
    let mut merged: Vec<Repaint> = Vec::with_capacity(code.len());
    for repaint in code {
        match merged.last_mut() {
            Some(last)
                if last.range.end == repaint.range.start
                    && last.raised == repaint.raised
                    && last.lowered == repaint.lowered =>
            {
                last.range.end = repaint.range.end;
                for (len, face, ink) in repaint.runs {
                    match last.runs.last_mut() {
                        Some((last_len, last_face, last_ink))
                            if *last_face == face && *last_ink == ink =>
                        {
                            *last_len += len;
                        }
                        _ => last.runs.push((len, face, ink)),
                    }
                }
            }
            _ => merged.push(repaint),
        }
    }
    merged
}

/// The runs of a verbatim line drawn by the parts its kind reads in it: the
/// label bold, the destination underlined, the rest quiet. `highlights` are
/// `char` ranges of `text`.
pub(super) fn source_highlight_runs(
    text: &str,
    highlights: &[(Range<usize>, SourceHighlight)],
    style: &EditorStyle,
) -> Vec<TextRun> {
    let part_at = |index: usize| {
        highlights
            .iter()
            .find(|(range, _)| range.contains(&index))
            .map(|(_, part)| *part)
    };
    let run = |len: usize, part: Option<SourceHighlight>| {
        let mut face = font(style.font_family.clone());
        let (color, underline) = match part {
            Some(SourceHighlight::Label) => {
                face.weight = FontWeight::BOLD;
                (style.text, false)
            }
            Some(SourceHighlight::Destination) => (style.muted_text, true),
            Some(SourceHighlight::Punctuation) => (style.muted_text.opacity(0.6), false),
            Some(SourceHighlight::Title) | None => (style.muted_text, false),
        };
        TextRun {
            len,
            font: face,
            color,
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        }
    };
    let mut runs: Vec<(usize, Option<SourceHighlight>)> = Vec::new();
    for (index, c) in text.chars().enumerate() {
        let part = part_at(index);
        match runs.last_mut() {
            Some((len, last)) if *last == part => *len += c.len_utf8(),
            _ => runs.push((c.len_utf8(), part)),
        }
    }
    runs.into_iter().map(|(len, part)| run(len, part)).collect()
}

pub(super) fn highlight_runs(
    highlighted: &crate::syntax::HighlightedLines,
    _font_size: Pixels,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    for (index, row) in highlighted.iter().enumerate() {
        if index > 0 {
            runs.push(TextRun {
                len: 1,
                font: font(CODE_FONT),
                color: gpui::transparent_black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        for (len, syntax) in row {
            let mut face = font(CODE_FONT);
            // Themes embolden keywords; at note size colour alone reads calmer.
            if syntax
                .font_style
                .contains(syntect::highlighting::FontStyle::ITALIC)
            {
                face.style = FontStyle::Italic;
            }
            let color = syntax.foreground;
            runs.push(TextRun {
                len: *len,
                font: face,
                color: rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32)
                    .into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    runs
}

pub(super) fn has(ty: Option<markraft_core::MarkTypeId>, marks: &MarkSet) -> bool {
    ty.is_some_and(|ty| marks.contains_type(ty))
}
