//! Derived, bounded syntax highlighting. Document spans remain plain code text.
use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::LazyLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color, FontStyle, ScopeSelectors, Style, StyleModifier, Theme, ThemeItem, ThemeSettings,
    },
    parsing::{SyntaxReference, SyntaxSet},
};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static THEMES: LazyLock<[Theme; 2]> = LazyLock::new(|| [theme(false), theme(true)]);

/// A restrained palette: keywords, names, strings and comments get a colour and
/// everything else keeps the text colour, so short snippets stay calm.
fn theme(dark: bool) -> Theme {
    let color = |hex: u32| Color {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
        a: 0xff,
    };
    let (text, keyword, name, string, comment) = if dark {
        (0xe6e7e9, 0xa78bfa, 0x6cb2ff, 0x7bd88f, 0x8b8f98)
    } else {
        (0x1c1d21, 0x7a5af8, 0x2f7cf6, 0x1f8a4c, 0x8a8d93)
    };
    let item = |scopes: &str, hex: u32, font_style: Option<FontStyle>| ThemeItem {
        scope: scopes
            .parse::<ScopeSelectors>()
            .expect("valid scope selectors"),
        style: StyleModifier {
            foreground: Some(color(hex)),
            background: None,
            font_style,
        },
    };
    Theme {
        settings: ThemeSettings {
            foreground: Some(color(text)),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("keyword, storage", keyword, None),
            // Operators are keywords to most grammars but read better as plain text.
            item("keyword.operator", text, None),
            item(
                "entity.name, support.function, variable.function, variable.other.readwrite.declaration",
                name,
                None,
            ),
            item("string", string, None),
            item("comment", comment, Some(FontStyle::ITALIC)),
        ],
        ..Theme::default()
    }
}

/// Language identifiers stored in documents and their menu labels.
pub fn code_languages() -> &'static [(&'static str, &'static str)] {
    &[
        ("", "Plain Text"),
        ("rust", "Rust"),
        ("javascript", "JavaScript"),
        ("typescript", "TypeScript"),
        ("jsx", "JSX"),
        ("tsx", "TSX"),
        ("json", "JSON"),
        ("html", "HTML"),
        ("css", "CSS"),
        ("python", "Python"),
        ("ruby", "Ruby"),
        ("go", "Go"),
        ("java", "Java"),
        ("swift", "Swift"),
        ("kotlin", "Kotlin"),
        ("c", "C"),
        ("cpp", "C++"),
        ("cs", "C#"),
        ("bash", "Shell"),
        ("sql", "SQL"),
        ("yaml", "YAML"),
        ("toml", "TOML"),
        ("xml", "XML"),
        ("markdown", "Markdown"),
    ]
}

/// The menu label a code block's `language` attribute is drawn as. A fence
/// alias reads as the language it names; anything the table does not list is
/// shown exactly as the document spells it.
pub fn language_label(language: &str) -> &str {
    let canonical = canonical_language(language);
    code_languages()
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(canonical))
        .map_or(language, |(_, label)| *label)
}

/// The [`code_languages`] entry a fence alias names, or the alias unchanged.
fn canonical_language(language: &str) -> &str {
    let lower = language.to_ascii_lowercase();
    match lower.as_str() {
        "text" | "txt" | "plaintext" | "plain" => "",
        "rs" => "rust",
        "js" | "mjs" | "cjs" | "node" | "nodejs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "rb" => "ruby",
        "golang" => "go",
        "kt" | "kts" => "kotlin",
        "c++" | "cxx" | "hpp" => "cpp",
        "c#" | "csharp" => "cs",
        "sh" | "shell" | "zsh" => "bash",
        "yml" => "yaml",
        "md" => "markdown",
        _ => language,
    }
}

pub(crate) type HighlightedLines = Vec<Vec<(usize, Style)>>;

struct CacheEntry {
    text: String,
    language: String,
    dark: bool,
    lines: Rc<HighlightedLines>,
}

thread_local! {
    static CACHE: RefCell<VecDeque<CacheEntry>> = const { RefCell::new(VecDeque::new()) };
}

fn syntax_for_language(language: &str) -> &'static SyntaxReference {
    let token = match language.to_ascii_lowercase().as_str() {
        "typescript" => "ts",
        // TSX includes JSX grammar; the bundled Babel grammar needs Oniguruma.
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

/// Keep the parser state across lines, including multiline strings and comments.
pub(crate) fn highlight(text: &str, language: &str, dark: bool) -> Rc<HighlightedLines> {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|entry| {
            entry.text == text && entry.language == language && entry.dark == dark
        }) {
            let entry = cache.remove(index).expect("cache index exists");
            let result = entry.lines.clone();
            cache.push_back(entry);
            return result;
        }
        let syntax = syntax_for_language(language);
        let theme = &THEMES[usize::from(dark)];
        let mut parser = HighlightLines::new(syntax, theme);
        let lines: HighlightedLines = text
            .split('\n')
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
                        (len > 0).then_some((len, style))
                    })
                    .collect()
            })
            .collect();
        let lines = Rc::new(lines);
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
                dark,
                lines: lines.clone(),
            });
        }
        lines
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_language_has_a_grammar() {
        for (id, label) in code_languages().iter().filter(|(id, _)| !id.is_empty()) {
            assert_ne!(
                syntax_for_language(id).name,
                "Plain Text",
                "missing grammar for {label}"
            );
        }
    }

    /// A fence carries whatever the author typed. The chip reads as the
    /// language, not as the abbreviation, and never hides an unknown one.
    #[test]
    fn a_fence_alias_reads_as_the_language_it_names() {
        for (attr, label) in [
            ("", "Plain Text"),
            ("js", "JavaScript"),
            ("JS", "JavaScript"),
            ("ts", "TypeScript"),
            ("py", "Python"),
            ("rs", "Rust"),
            ("zsh", "Shell"),
            ("yml", "YAML"),
            ("md", "Markdown"),
            ("txt", "Plain Text"),
            ("rust", "Rust"),
            ("wgsl", "wgsl"),
        ] {
            assert_eq!(language_label(attr), label, "for {attr:?}");
        }
    }

    #[test]
    fn multiline_comments_preserve_parser_state_and_utf8_lengths() {
        let highlighted = highlight("/* comment\n你好 */ let x = 1;", "rust", false);
        assert_eq!(highlighted.len(), 2);
        assert_eq!(
            highlighted[1].iter().map(|(len, _)| len).sum::<usize>(),
            "你好 */ let x = 1;".len()
        );
        assert_eq!(
            highlighted[0][0].1.foreground,
            highlighted[1][0].1.foreground
        );
        assert!(
            highlighted[1]
                .iter()
                .any(|(_, style)| style.foreground != highlighted[1][0].1.foreground)
        );
    }

    #[test]
    fn cache_reuses_unchanged_code_and_separates_themes() {
        let first = highlight("let x = 1;", "rust", false);
        assert!(Rc::ptr_eq(&first, &highlight("let x = 1;", "rust", false)));
        assert!(!Rc::ptr_eq(&first, &highlight("let x = 1;", "rust", true)));
    }

    #[test]
    fn unknown_language_keeps_every_byte() {
        let lines = highlight("你好\n\ntext", "unsupported-language", false);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.iter().map(|(len, _)| len).sum::<usize>())
                .collect::<Vec<_>>(),
            vec![6, 0, 4]
        );
    }
}
