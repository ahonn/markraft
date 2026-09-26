//! The one table of the paired inline styles: what each is called, how it is
//! spelled in Markdown and in HTML, and the order they are given up in.
//!
//! Everything that has to agree about a style reads it here — the delimiters
//! a command writes, the rule the serialiser spells it with, the tag the HTML
//! codec reads and writes, the reverse lookup from a mark's name — so adding
//! one is one row, and no two lists can drift apart. The styles with content
//! of their own — a link's destination, a footnote's label, a formula, a code
//! span's variable fence — keep their bespoke rules and are not in the table.

use crate::derive::Style;
use crate::schema as md;

/// One paired inline style.
#[derive(Debug)]
pub(crate) struct StyleSpec {
    /// The schema mark this style is.
    pub mark: &'static str,
    /// The style [`derive`](crate::derive::derive) reports it as.
    pub style: Style,
    /// The Markdown delimiter run in the `*` house style, or `None` for a
    /// style Markdown has no delimiter for and spells with its HTML tags.
    pub run: Option<&'static str>,
    /// The run in the `_` house style, where it differs.
    pub underscore_run: Option<&'static str>,
    /// The tags the HTML codec writes it with, and reads it from.
    pub tags: (&'static str, &'static str),
    /// Other elements the HTML importer reads as this style.
    pub aliases: &'static [&'static str],
}

/// The paired styles, in the order they are given up when their spelling does
/// not read back: the ones Markdown has least room for first.
pub(crate) static STYLES: &[StyleSpec] = &[
    StyleSpec {
        mark: md::KEYBOARD,
        style: Style::Keyboard,
        run: None,
        underscore_run: None,
        tags: ("<kbd>", "</kbd>"),
        aliases: &[],
    },
    StyleSpec {
        mark: md::HIGHLIGHT,
        style: Style::Highlight,
        run: Some("=="),
        underscore_run: None,
        tags: ("<mark>", "</mark>"),
        aliases: &[],
    },
    StyleSpec {
        mark: md::SUPERSCRIPT,
        style: Style::Superscript,
        run: Some("^"),
        underscore_run: None,
        tags: ("<sup>", "</sup>"),
        aliases: &[],
    },
    StyleSpec {
        mark: md::SUBSCRIPT,
        style: Style::Subscript,
        run: Some("~"),
        underscore_run: None,
        tags: ("<sub>", "</sub>"),
        aliases: &[],
    },
    StyleSpec {
        mark: md::UNDERLINE,
        style: Style::Underline,
        run: None,
        underscore_run: None,
        tags: ("<u>", "</u>"),
        aliases: &["ins"],
    },
    StyleSpec {
        mark: md::STRIKETHROUGH,
        style: Style::Strikethrough,
        run: Some("~~"),
        underscore_run: None,
        tags: ("<del>", "</del>"),
        aliases: &["s", "strike"],
    },
    StyleSpec {
        mark: md::EM,
        style: Style::Emphasis,
        run: Some("*"),
        underscore_run: Some("_"),
        tags: ("<em>", "</em>"),
        aliases: &["i"],
    },
    StyleSpec {
        mark: md::STRONG,
        style: Style::Strong,
        run: Some("**"),
        underscore_run: Some("__"),
        tags: ("<strong>", "</strong>"),
        aliases: &["b"],
    },
];

impl StyleSpec {
    /// The delimiter pair the style is spelled with when emphasis is written
    /// in `emphasis` — `*` or `_`.
    pub fn delimiters(&self, emphasis: char) -> (&'static str, &'static str) {
        let run = match (emphasis, self.underscore_run) {
            ('_', Some(run)) => Some(run),
            _ => self.run,
        };
        run.map_or(self.tags, |run| (run, run))
    }

    /// The HTML element name the style is written as: `strong` for
    /// `<strong>`…`</strong>`.
    pub fn tag(&self) -> &'static str {
        self.tags.0.trim_start_matches('<').trim_end_matches('>')
    }

    /// Every element name the HTML importer reads as this style, the written
    /// one first.
    pub fn html_names(&self) -> Vec<&'static str> {
        std::iter::once(self.tag())
            .chain(self.aliases.iter().copied())
            .collect()
    }
}

/// The paired style a mark name is, if it is one.
pub(crate) fn by_mark(mark: &str) -> Option<&'static StyleSpec> {
    STYLES.iter().find(|spec| spec.mark == mark)
}

/// The row of a paired style.
pub(crate) fn by_style(style: &Style) -> Option<&'static StyleSpec> {
    STYLES.iter().find(|spec| spec.style == *style)
}

/// The paired style an HTML element name reads as, its written name or an
/// alias: `<b>` typed into the source is strong as `<strong>` is, and stays
/// spelled `<b>`, since the text is the source.
pub(crate) fn by_html_name(name: &str) -> Option<&'static StyleSpec> {
    STYLES.iter().find(|spec| spec.html_names().contains(&name))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_found_by_each_of_its_keys() {
        for spec in STYLES {
            assert!(std::ptr::eq(by_mark(spec.mark).unwrap(), spec));
            assert!(std::ptr::eq(by_style(&spec.style).unwrap(), spec));
            for name in spec.html_names() {
                assert!(std::ptr::eq(by_html_name(name).unwrap(), spec));
            }
            assert_eq!(spec.style.mark_name(), spec.mark);
        }
    }

    #[test]
    fn the_underscore_style_only_changes_emphasis_and_strong() {
        assert_eq!(by_mark(md::EM).unwrap().delimiters('_'), ("_", "_"));
        assert_eq!(by_mark(md::STRONG).unwrap().delimiters('_'), ("__", "__"));
        assert_eq!(by_mark(md::STRONG).unwrap().delimiters('*'), ("**", "**"));
        assert_eq!(
            by_mark(md::STRIKETHROUGH).unwrap().delimiters('_'),
            ("~~", "~~")
        );
        assert_eq!(
            by_mark(md::UNDERLINE).unwrap().delimiters('_'),
            ("<u>", "</u>")
        );
    }
}
