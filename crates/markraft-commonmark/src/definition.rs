//! The parts of link reference definitions, for a view to draw them by.
//!
//! A run of definitions is kept verbatim in a raw block, so nothing about it is
//! lost and every character stays editable. Typora draws one as the prose it
//! describes rather than as source: the label bold, the destination
//! underlined, the brackets, the colon and the title quiet. Every character
//! stays on screen; only its face says what it is.

use std::ops::Range;

use markraft_core::kind::SourceHighlight;

/// The parts of `text`, which [`reads_as_definitions`](crate::textblock::reads_as_definitions)
/// already said holds nothing else, as `char` ranges. What the scan cannot
/// place is left out, and drawn as the text around it is.
pub(crate) fn highlights(text: &str) -> Vec<(Range<usize>, SourceHighlight)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        at = skip_blank(&chars, at, true);
        match definition(&chars, at, &mut out) {
            Some(end) => at = end,
            // Past what could not be read, to the next line.
            None => {
                at = chars[at..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(chars.len(), |offset| at + offset + 1)
            }
        }
    }
    out
}

/// Read the definition that starts at `at`, pushing its parts, and answer where
/// it ends.
fn definition(
    chars: &[char],
    at: usize,
    out: &mut Vec<(Range<usize>, SourceHighlight)>,
) -> Option<usize> {
    use SourceHighlight::*;
    if chars.get(at) != Some(&'[') {
        return None;
    }
    let close = closing(chars, at + 1, ']')?;
    if chars.get(close + 1) != Some(&':') {
        return None;
    }
    let mut parts = vec![
        (at..at + 1, Punctuation),
        (at + 1..close, Label),
        (close..close + 2, Punctuation),
    ];
    let mut at = skip_blank(chars, close + 2, false);
    // The destination: `<…>`, which may hold spaces, or a run without any.
    if chars.get(at) == Some(&'<') {
        let end = closing(chars, at + 1, '>')?;
        parts.extend([
            (at..at + 1, Punctuation),
            (at + 1..end, Destination),
            (end..end + 1, Punctuation),
        ]);
        at = end + 1;
    } else {
        let end = chars[at..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| at + offset);
        if end == at {
            return None;
        }
        parts.push((at..end, Destination));
        at = end;
    }
    // An optional title, on the same line or the next.
    let before_title = skip_blank(chars, at, false);
    let opener = chars.get(before_title).copied();
    let closer = match opener {
        Some('"') => Some('"'),
        Some('\'') => Some('\''),
        Some('(') => Some(')'),
        _ => None,
    };
    if let Some(closer) = closer
        && let Some(end) = closing(chars, before_title + 1, closer)
    {
        parts.push((before_title..end + 1, Title));
        at = end + 1;
    }
    out.extend(parts.into_iter().filter(|(range, _)| !range.is_empty()));
    Some(at)
}

/// The first unescaped `close` from `at` on.
fn closing(chars: &[char], at: usize, close: char) -> Option<usize> {
    let mut index = at;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            c if c == close => return Some(index),
            _ => index += 1,
        }
    }
    None
}

/// Past spaces and tabs from `at`, and past one line ending among them — or
/// every one, where `lines` says so.
fn skip_blank(chars: &[char], mut at: usize, lines: bool) -> usize {
    let mut ended = false;
    while let Some(&c) = chars.get(at) {
        match c {
            ' ' | '\t' => at += 1,
            '\n' if lines || !ended => {
                ended = true;
                at += 1;
            }
            _ => break,
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use SourceHighlight::*;

    fn parts(text: &str) -> Vec<(String, SourceHighlight)> {
        let chars: Vec<char> = text.chars().collect();
        highlights(text)
            .into_iter()
            .map(|(range, part)| (chars[range].iter().collect(), part))
            .collect()
    }

    fn owned(parts: &[(&str, SourceHighlight)]) -> Vec<(String, SourceHighlight)> {
        parts
            .iter()
            .map(|(text, part)| (text.to_string(), *part))
            .collect()
    }

    #[test]
    fn a_definition_reads_as_its_label_destination_and_title() {
        assert_eq!(
            parts("[ref]: https://example.com/ref \"引用定义\"\n[折叠写法]: <a b.md>"),
            owned(&[
                ("[", Punctuation),
                ("ref", Label),
                ("]:", Punctuation),
                ("https://example.com/ref", Destination),
                ("\"引用定义\"", Title),
                ("[", Punctuation),
                ("折叠写法", Label),
                ("]:", Punctuation),
                ("<", Punctuation),
                ("a b.md", Destination),
                (">", Punctuation),
            ])
        );
    }

    #[test]
    fn a_title_may_stand_on_the_next_line_and_a_label_hold_an_escape() {
        assert_eq!(
            parts("[a\\]b]:\n  /u\n  'T'"),
            owned(&[
                ("[", Punctuation),
                ("a\\]b", Label),
                ("]:", Punctuation),
                ("/u", Destination),
                ("'T'", Title),
            ])
        );
    }
}
