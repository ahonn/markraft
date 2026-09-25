//! Line breaking: where a row may wrap, with the kinsoku rules CJK text
//! needs. Shaping asks it before it hands text to the text system.

use super::*;

/// The byte ranges of `text` the line wrapper never breaks inside, each with
/// its surrounding whitespace trimmed off.
///
/// A break opportunity opens before a word character that follows a space, and
/// before any character that is neither a space nor a word character — so CJK
/// text breaks between any two characters, and `/`, `?` and `&` break a path or
/// a query apart. An opportunity strictly inside a range of `glue` is ignored.
pub(super) fn unbreakable_units(text: &str, glue: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut starts = vec![0usize];
    let mut previous = '\0';
    let mut seen = false;
    for (byte, c) in text.char_indices() {
        let opportunity = seen && may_break(previous, c);
        if opportunity
            && !glue
                .iter()
                .any(|range| range.start < byte && byte < range.end)
        {
            starts.push(byte);
        }
        seen |= c != ' ';
        previous = c;
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(index, &start)| {
            let end = starts.get(index + 1).copied().unwrap_or(text.len());
            let unit = &text[start..end];
            let lead = unit.len() - unit.trim_start().len();
            let trimmed = unit.trim();
            (!trimmed.is_empty()).then(|| start + lead..start + lead + trimmed.len())
        })
        .collect()
}

/// Whether a line may wrap between `previous` and `c`.
///
/// gpui's wrapper breaks before a word character only after a space, and
/// before anything else that is not a space — so between any two CJK
/// characters. Less what CJK line breaking forbids: a line never starts with
/// closing punctuation such as `，` or `」`, nor ends with opening punctuation
/// such as `（` or `「`.
pub(super) fn may_break(previous: char, c: char) -> bool {
    if c == ' ' || never_starts_line(c) || never_ends_line(previous) {
        return false;
    }
    !is_word_char(c) || previous == ' '
}

/// The full-width closing marks, and the like, that never start a line (UAX #14
/// classes CL, CP, EX, IS and NS). gpui's wrapper knows only their ASCII
/// counterparts.
pub(super) fn never_starts_line(c: char) -> bool {
    matches!(
        c,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '）'
            | '］'
            | '｝'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '〕'
            | '〗'
            | '〙'
            | '〛'
            | '”'
            | '’'
            | '…'
            | '‥'
            | '・'
            | 'ー'
            | '～'
            | '％'
            | '．'
            | '｡'
            | '､'
    )
}

/// The opening marks that never end a line (UAX #14 class OP and opening
/// quotes).
pub(super) fn never_ends_line(c: char) -> bool {
    matches!(
        c,
        '（' | '［'
            | '｛'
            | '【'
            | '《'
            | '〈'
            | '「'
            | '『'
            | '〔'
            | '〖'
            | '〘'
            | '〚'
            | '“'
            | '‘'
    )
}

/// Wrap `line` again where gpui's wrapper broke it before closing punctuation
/// or after opening punctuation.
///
/// gpui's `LineWrapper` is not configurable, so its boundaries are checked
/// rather than replaced: a line it wrapped well keeps them. One that breaks a
/// rule is wrapped afresh, greedily, over the shaped glyphs' own positions,
/// with the opportunities [`may_break`] allows, and a line with none that fits
/// breaks at the glyph that overflows, as gpui's does. Spaces hang past the
/// edge, as they do there.
pub(super) fn keep_line_breaking_rules(line: &mut WrappedLine, wrap_width: Pixels) {
    let text = line.text.clone();
    let layout = line.unwrapped_layout.clone();
    let char_at = |byte: usize| text.get(byte..).and_then(|rest| rest.chars().next());
    let char_before = |byte: usize| text.get(..byte).and_then(|head| head.chars().next_back());
    let glyph_byte = |boundary: &WrapBoundary| {
        layout
            .runs
            .get(boundary.run_ix)
            .and_then(|run| run.glyphs.get(boundary.glyph_ix))
            .map(|glyph| glyph.index)
    };
    let broken = line.wrap_boundaries.iter().any(|boundary| {
        glyph_byte(boundary).is_some_and(|byte| {
            char_at(byte).is_some_and(never_starts_line)
                || char_before(byte).is_some_and(never_ends_line)
        })
    });
    if !broken {
        return;
    }
    // In text order: a line shaped with fallback fonts holds one run per font,
    // not one per stretch of text.
    let mut glyphs: Vec<(WrapBoundary, usize, Pixels)> = layout
        .runs
        .iter()
        .enumerate()
        .flat_map(|(run_ix, run)| {
            run.glyphs.iter().enumerate().map(move |(glyph_ix, glyph)| {
                (
                    WrapBoundary { run_ix, glyph_ix },
                    glyph.index,
                    glyph.position.x,
                )
            })
        })
        .collect();
    glyphs.sort_by_key(|glyph| glyph.1);
    let breakable = |at: usize| {
        let byte = glyphs[at].1;
        match (char_before(byte), char_at(byte)) {
            (Some(previous), Some(c)) => may_break(previous, c),
            _ => false,
        }
    };
    let mut boundaries = Vec::new();
    let mut row_start = 0usize;
    let mut candidate: Option<usize> = None;
    for at in 0..glyphs.len() {
        if at > row_start && breakable(at) {
            candidate = Some(at);
        }
        if char_at(glyphs[at].1) == Some(' ') {
            continue;
        }
        let right = glyphs.get(at + 1).map_or(layout.width, |glyph| glyph.2);
        if at > row_start && right - glyphs[row_start].2 > wrap_width {
            let wrap = candidate.unwrap_or(at);
            boundaries.push(glyphs[wrap].0);
            row_start = wrap;
            candidate = (wrap + 1..=at).rev().find(|&later| breakable(later));
        }
    }
    **line = Arc::new(WrappedLineLayout {
        unwrapped_layout: layout,
        wrap_boundaries: boundaries.into_iter().collect(),
        wrap_width: Some(wrap_width),
    });
}

/// Whether the line wrapper treats `c` as part of a word, which is what decides
/// where a cell's text may break.
///
/// Mirrors gpui's own `LineWrapper::is_word_char`, which is not public: Latin,
/// Cyrillic, Vietnamese and Bengali letters, digits, the punctuation that binds
/// to a word and the closing punctuation that never starts a line. Everything
/// else — CJK above all — is a break opportunity of its own.
pub(super) fn is_word_char(c: char) -> bool {
    // The punctuation that binds to a word — `a-b`, `var_name`, `3.14`,
    // `Self::new` — together with the closing marks that never start a line and
    // the glue characters that never break at all.
    const BINDING: &str = "-_.'\u{2019}\u{2018}$%@#^~,=:;!)]}\"\u{201d}\u{00bb}\u{2026}\u{22ef}\u{202f}\u{00a0}\u{2011}";
    // Latin-1 Supplement through Latin Extended-B, combining diacritics,
    // Cyrillic, Bengali, and Latin Extended Additional for Vietnamese.
    const SCRIPTS: [std::ops::RangeInclusive<char>; 5] = [
        '\u{00c0}'..='\u{024f}',
        '\u{0300}'..='\u{036f}',
        '\u{0400}'..='\u{04ff}',
        '\u{0980}'..='\u{09ff}',
        '\u{1e00}'..='\u{1eff}',
    ];
    c.is_ascii_alphanumeric()
        || BINDING.contains(c)
        || SCRIPTS.iter().any(|range| range.contains(&c))
}
