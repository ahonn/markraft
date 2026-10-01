//! Code highlighting as colour roles, independent of any renderer.
//!
//! [`highlight`] splits code into [`Span`]s that each carry a [`Tone`]: the role a
//! run plays, not its colour. A renderer picks the colour with [`Tone::rgb`] for
//! its appearance, so one pass serves light and dark alike, and a stylesheet can
//! switch between them without highlighting again.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::LazyLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings},
    parsing::{SyntaxReference, SyntaxSet},
};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static ROLES: LazyLock<Theme> = LazyLock::new(roles);

/// The role a run of code plays. The palette is restrained: keywords, names,
/// strings and comments stand out and everything else keeps the text colour, so
/// short snippets stay calm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Operators, punctuation and anything no other role claims.
    Text,
    /// Keywords and storage modifiers.
    Keyword,
    /// Declared and called names.
    Name,
    /// String literals.
    String,
    /// Comments, drawn in italics.
    Comment,
}

impl Tone {
    /// Every tone, in declaration order.
    pub const ALL: [Tone; 5] = [
        Tone::Text,
        Tone::Keyword,
        Tone::Name,
        Tone::String,
        Tone::Comment,
    ];

    /// The colour as `0xRRGGBB` on a light or a dark background.
    pub fn rgb(self, dark: bool) -> u32 {
        match (self, dark) {
            (Tone::Text, false) => 0x1c1d21,
            (Tone::Keyword, false) => 0x7a5af8,
            (Tone::Name, false) => 0x2f7cf6,
            (Tone::String, false) => 0x1f8a4c,
            (Tone::Comment, false) => 0x8a8d93,
            (Tone::Text, true) => 0xe6e7e9,
            (Tone::Keyword, true) => 0xa78bfa,
            (Tone::Name, true) => 0x6cb2ff,
            (Tone::String, true) => 0x7bd88f,
            (Tone::Comment, true) => 0x8b8f98,
        }
    }

    /// Whether the run is set in italics.
    pub fn italic(self) -> bool {
        self == Tone::Comment
    }

    /// A stable lowercase name, for class names and the like.
    pub fn name(self) -> &'static str {
        match self {
            Tone::Text => "text",
            Tone::Keyword => "keyword",
            Tone::Name => "name",
            Tone::String => "string",
            Tone::Comment => "comment",
        }
    }

    // The role theme marks each tone with its index in the red channel; the
    // colour itself is never shown.
    fn marker(self) -> Color {
        Color {
            r: self as u8,
            g: 0,
            b: 0,
            a: 0xff,
        }
    }

    fn from_marker(color: Color) -> Tone {
        Tone::ALL
            .get(usize::from(color.r))
            .copied()
            .unwrap_or(Tone::Text)
    }
}

/// A run of `len` bytes of one line in one tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Length in bytes of UTF-8.
    pub len: usize,
    /// The run's role.
    pub tone: Tone,
}

/// Highlighted code, one entry per line split on `\n`. The spans of a line cover
/// its bytes exactly; the line ending belongs to no span.
pub type Lines = Vec<Vec<Span>>;

/// A theme whose "colours" name tones, so matching scopes to roles stays with
/// syntect's selector rules.
fn roles() -> Theme {
    let item = |scopes: &str, tone: Tone| ThemeItem {
        scope: scopes
            .parse::<ScopeSelectors>()
            .expect("valid scope selectors"),
        style: StyleModifier {
            foreground: Some(tone.marker()),
            background: None,
            font_style: None,
        },
    };
    Theme {
        settings: ThemeSettings {
            foreground: Some(Tone::Text.marker()),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("keyword, storage", Tone::Keyword),
            // Operators are keywords to most grammars but read better as plain text.
            item("keyword.operator", Tone::Text),
            item(
                "entity.name, support.function, variable.function, variable.other.readwrite.declaration",
                Tone::Name,
            ),
            item("string", Tone::String),
            item("comment", Tone::Comment),
        ],
        ..Theme::default()
    }
}

fn syntax_for_language(language: &str) -> &'static SyntaxReference {
    let token = match language.to_ascii_lowercase().as_str() {
        "typescript" => "ts",
        // TSX includes JSX grammar. The bundled Babel grammar scopes import and
        // export as operators, which this palette leaves uncoloured.
        "jsx" => "tsx",
        "shell" | "sh" | "zsh" => "bash",
        "c++" => "cpp",
        "c#" => "cs",
        "plain text" | "text" | "plaintext" => "txt",
        _ => language,
    };
    SYNTAXES
        .find_syntax_by_token(token)
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

/// Whether `language` has a grammar of its own, rather than falling back to
/// plain text.
pub fn has_grammar(language: &str) -> bool {
    !std::ptr::eq(
        syntax_for_language(language),
        SYNTAXES.find_syntax_plain_text(),
    )
}

struct CacheEntry {
    text: String,
    language: String,
    lines: Rc<Lines>,
}

thread_local! {
    static CACHE: RefCell<VecDeque<CacheEntry>> = const { RefCell::new(VecDeque::new()) };
}

/// Highlight `text` as `language`, keeping the parser state across lines so
/// multiline strings and comments stay coloured. An unknown language comes back
/// as one [`Tone::Text`] span per line.
///
/// Recent results are cached per thread, bounded by entry count and bytes.
pub fn highlight(text: &str, language: &str) -> Rc<Lines> {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache
            .iter()
            .position(|entry| entry.text == text && entry.language == language)
        {
            let entry = cache.remove(index).expect("cache index exists");
            let result = entry.lines.clone();
            cache.push_back(entry);
            return result;
        }
        let lines = Rc::new(highlight_uncached(text, language));
        // Limit both entry count and retained source bytes for long-lived windows.
        if text.len() <= 256 * 1024 {
            while cache.len() >= 24
                || cache.iter().map(|entry| entry.text.len()).sum::<usize>() + text.len()
                    > 512 * 1024
            {
                cache.pop_front();
            }
            cache.push_back(CacheEntry {
                text: text.to_owned(),
                language: language.to_owned(),
                lines: lines.clone(),
            });
        }
        lines
    })
}

fn highlight_uncached(text: &str, language: &str) -> Lines {
    let mut parser = HighlightLines::new(syntax_for_language(language), &ROLES);
    text.split('\n')
        .map(|line| {
            let input = format!("{line}\n");
            let mut remaining = line.len();
            parser
                .highlight_line(&input, &SYNTAXES)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(style, value)| {
                    let len = value.len().min(remaining);
                    remaining -= len;
                    (len > 0).then_some(Span {
                        len,
                        tone: Tone::from_marker(style.foreground),
                    })
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn tones(line: &[Span], text: &str) -> Vec<(String, Tone)> {
        let mut at = 0;
        line.iter()
            .map(|span| {
                let part = text[at..at + span.len].to_owned();
                at += span.len;
                (part, span.tone)
            })
            .collect()
    }

    #[test]
    fn roles_follow_the_grammar() {
        let code = "fn main() { let s = \"hi\"; } // done";
        let lines = highlight(code, "rust");
        let parts = tones(&lines[0], code);
        let tone_of = |needle: &str| {
            parts
                .iter()
                .find(|(part, _)| part.contains(needle))
                .map(|(_, tone)| *tone)
        };
        assert_eq!(tone_of("fn"), Some(Tone::Keyword));
        assert_eq!(tone_of("main"), Some(Tone::Name));
        assert_eq!(tone_of("hi"), Some(Tone::String));
        assert_eq!(tone_of("done"), Some(Tone::Comment));
        assert_eq!(tone_of("="), Some(Tone::Text));
    }

    #[test]
    fn multiline_comments_preserve_parser_state_and_utf8_lengths() {
        let lines = highlight("/* comment\n你好 */ let x = 1;", "rust");
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1].iter().map(|span| span.len).sum::<usize>(),
            "你好 */ let x = 1;".len()
        );
        assert_eq!(lines[0][0].tone, Tone::Comment);
        assert_eq!(lines[1][0].tone, Tone::Comment);
        assert!(lines[1].iter().any(|span| span.tone != Tone::Comment));
    }

    #[test]
    fn cache_reuses_unchanged_code() {
        let first = highlight("let x = 1;", "rust");
        assert!(Rc::ptr_eq(&first, &highlight("let x = 1;", "rust")));
        assert!(!Rc::ptr_eq(&first, &highlight("let x = 1;", "go")));
    }

    #[test]
    fn unknown_language_keeps_every_byte_as_text() {
        let lines = highlight("你好\n\ntext", "unsupported-language");
        assert_eq!(
            lines
                .iter()
                .map(|line| line.iter().map(|span| span.len).sum::<usize>())
                .collect::<Vec<_>>(),
            vec![6, 0, 4]
        );
        assert!(lines.iter().flatten().all(|span| span.tone == Tone::Text));
        assert!(!has_grammar("unsupported-language"));
        assert!(has_grammar("rust"));
    }

    #[test]
    fn every_tone_has_distinct_colours_per_appearance() {
        for dark in [false, true] {
            let mut colours: Vec<u32> = Tone::ALL.iter().map(|tone| tone.rgb(dark)).collect();
            colours.dedup();
            assert_eq!(colours.len(), Tone::ALL.len());
        }
        assert!(Tone::Comment.italic() && !Tone::Keyword.italic());
    }
}
