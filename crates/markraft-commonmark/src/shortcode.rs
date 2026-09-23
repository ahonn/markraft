//! Reading GitHub emoji shortcodes, `:smile:`.
//!
//! A shortcode is a colon, a name of ASCII letters, digits, `+`, `_` and `-`,
//! and a colon, where the name is one of the [`emojis`] table's GitHub
//! shortcodes exactly as written — the grammar comrak's own `shortcodes`
//! extension reads. comrak's extension is not used: it resolves names against
//! an older table than the editor's `:` menu offers, and a name the menu
//! inserts has to be one the reader takes for an emoji.
//!
//! A shortcode is an [`EMOJI`](crate::schema::EMOJI) atom in the tree, written
//! back as the `:name:` it was read from. What is not a shortcode — an unknown
//! name, `10:30:`, a colon a backslash escapes — stays the text it is.

/// The emoji `name` names, when it is a GitHub shortcode as written.
pub fn emoji(name: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(name).map(|emoji| emoji.as_str())
}

/// The shortcode `text` starts with: its name and its length in bytes, both
/// colons included.
pub fn read_shortcode(text: &str) -> Option<(&str, usize)> {
    let rest = text.strip_prefix(':')?;
    let len = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'_' | b'-'))
        .count();
    let name = &rest[..len];
    (len > 0 && rest[len..].starts_with(':') && emoji(name).is_some()).then_some((name, len + 2))
}

/// How a shortcode is written.
pub fn spelling(name: &str) -> String {
    format!(":{name}:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_name_between_colons_is_a_shortcode() {
        assert_eq!(read_shortcode(":smile: x"), Some(("smile", 7)));
        assert_eq!(read_shortcode(":+1:"), Some(("+1", 4)));
        assert_eq!(read_shortcode(":-1:"), Some(("-1", 4)));
    }

    #[test]
    fn anything_else_is_not() {
        for text in [
            ":nope_not_one:",
            ":smile",
            "smile:",
            "::",
            ":30:",
            ":Smile:",
            ":smi le:",
        ] {
            assert_eq!(read_shortcode(text), None, "{text:?}");
        }
    }
}
