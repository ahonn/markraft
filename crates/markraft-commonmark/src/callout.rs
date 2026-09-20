//! Reading and writing Obsidian/GitHub callouts, byte for byte.
//!
//! A callout is a block quote whose first line is a marker — `[!note]`,
//! `[!tip]- Folded`, `[!custom] Any title` — so it is the
//! [`BLOCKQUOTE`](crate::schema::BLOCKQUOTE) node with its marker in
//! attributes rather than a node type of its own: every structural command,
//! key binding, correction and test that works on a block quote keeps working
//! on a callout.
//!
//! comrak's `alerts` extension is deliberately **not** used. It knows the five
//! GitHub types and nothing else, while Obsidian's type is any identifier at
//! all, may carry a `-`/`+` fold marker and may be followed by a title; a
//! parser that recognises only the five would read the rest as body text and
//! lose the marker.
//!
//! What counts as a marker:
//!
//! * `[!` immediately at the start of the quote's first line — after the `>`
//!   and the one optional space a reader strips, and *not* after any further
//!   indentation, which is what Obsidian requires too;
//! * a type of at least one character holding no bracket;
//! * an optional `-` or `+` directly after the `]`;
//! * either the end of the line, or one space and the title, which runs to the
//!   end of that line and is kept exactly as written — a second space belongs
//!   to the title, so the marker writes itself back byte for byte.
//!
//! Anything else is an ordinary block quote: `> \[!note]` is escaped, `>  [!x]`
//! is indented past the content column, and `> text [!x]` does not begin one.

/// The parts of a callout marker, spelled as the source spells them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Callout {
    /// The type between the brackets, in the case it was written in. Never
    /// empty: an empty type is what says a block quote is an ordinary one.
    pub kind: String,
    /// `"-"`, `"+"` or empty. Obsidian folds on the first and expands on the
    /// second; this codec keeps the byte and shows the content either way.
    pub fold: String,
    /// The raw title after the marker, empty where the line ends at it.
    pub title: String,
}

impl Callout {
    /// The marker line this writes back, with no normalisation of either part.
    pub fn marker(&self) -> String {
        let mut out = format!("[!{}]{}", self.kind, self.fold);
        if !self.title.is_empty() {
            out.push(' ');
            out.push_str(&self.title);
        }
        out
    }
}

/// Read the callout marker `content` is, where `content` is a block quote's
/// first line with its `>` markers already removed.
///
/// The whole line is the marker, so there is nothing left over to report.
pub fn read_callout(content: &str) -> Option<Callout> {
    let rest = content.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = &rest[..end];
    if kind.is_empty() || kind.contains('[') {
        return None;
    }
    let rest = &rest[end + 1..];
    let (fold, rest) = match rest.as_bytes().first() {
        Some(b'-' | b'+') => rest.split_at(1),
        _ => ("", rest),
    };
    let title = if rest.is_empty() {
        ""
    } else {
        // One space separates the marker from the title; a second one is the
        // title's own, so `]  Title` comes back as it was written.
        rest.strip_prefix(' ')?
    };
    Some(Callout {
        kind: kind.to_string(),
        fold: fold.to_string(),
        title: title.to_string(),
    })
}

/// A block quote line's content: what is left once its `>` markers and the one
/// optional space after each are stripped, or `None` for a line that is not in
/// a quote at all.
pub fn quote_content(line: &str) -> Option<&str> {
    let mut rest = line.trim_start_matches([' ', '\t']);
    let mut quoted = false;
    while let Some(tail) = rest.strip_prefix('>') {
        quoted = true;
        rest = tail.strip_prefix(' ').unwrap_or(tail);
    }
    quoted.then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(content: &str) -> Option<Callout> {
        read_callout(content)
    }

    fn callout(kind: &str, fold: &str, title: &str) -> Option<Callout> {
        Some(Callout {
            kind: kind.to_string(),
            fold: fold.to_string(),
            title: title.to_string(),
        })
    }

    #[test]
    fn every_marker_writes_its_own_source_back() {
        for content in [
            "[!note]",
            "[!tip] Custom title",
            "[!faq]- Folded by default",
            "[!warning]+ Expanded by default",
            "[!custom-type] Any type is legal",
            "[!NOTE]",
            "[!note]  two spaces",
            "[!note] a|b **c**",
            "[!note] trailing  ",
            "[!note]-",
        ] {
            let callout = read_callout(content).expect(content);
            assert_eq!(callout.marker(), content, "{content:?}");
        }
    }

    #[test]
    fn the_parts_keep_the_bytes_the_source_spelled() {
        assert_eq!(read("[!note]"), callout("note", "", ""));
        assert_eq!(read("[!tip] Title"), callout("tip", "", "Title"));
        assert_eq!(read("[!faq]- Folded"), callout("faq", "-", "Folded"));
        assert_eq!(read("[!x]+"), callout("x", "+", ""));
        assert_eq!(read("[!x]  spaced"), callout("x", "", " spaced"));
        assert_eq!(read("[!Note]"), callout("Note", "", ""));
    }

    #[test]
    fn what_is_not_a_marker_is_an_ordinary_quote() {
        for content in [
            "[!]",
            "[!note",
            "text [!note]",
            " [!note]",
            "[!note]x",
            "[!a[b]",
            "\\[!note]",
            "",
        ] {
            assert_eq!(read_callout(content), None, "{content:?}");
        }
    }

    #[test]
    fn a_quote_line_loses_its_markers_and_one_space_after_each() {
        assert_eq!(quote_content("> [!note]"), Some("[!note]"));
        assert_eq!(quote_content(">[!note]"), Some("[!note]"));
        assert_eq!(quote_content(">  [!note]"), Some(" [!note]"));
        assert_eq!(quote_content("> > [!tip] a"), Some("[!tip] a"));
        assert_eq!(quote_content("  > text"), Some("text"));
        assert_eq!(quote_content("plain"), None);
    }
}
