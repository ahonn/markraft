//! Code languages offered for fences. Highlighting itself is `markraft_syntax`.

/// Language identifiers stored in documents and their menu labels.
pub fn code_languages() -> &'static [(&'static str, &'static str)] {
    const LANGUAGES: &[(&str, &str)] = &[
        ("", crate::EditorMessage::PlainText.english()),
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
    ];
    LANGUAGES
}

/// The [`code_languages`] entry a fence alias names, or the alias unchanged.
pub fn canonical_language(language: &str) -> &str {
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_menu_language_has_a_grammar() {
        for (id, label) in code_languages().iter().filter(|(id, _)| !id.is_empty()) {
            assert!(
                markraft_syntax::has_grammar(id),
                "missing grammar for {label}"
            );
        }
    }
}
