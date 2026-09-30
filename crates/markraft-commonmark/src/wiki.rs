//! Reading and writing wiki links, byte for byte.
//!
//! comrak recognises `[[…]]`, but what it hands back is *interpreted*: its
//! `url` is trimmed, its HTML entities are resolved and its backslash escapes
//! are gone. A [`WIKI_LINK`](crate::schema::WIKI_LINK) atom has to put back the
//! bytes it took, so the parts are read from the source here instead, and this
//! recogniser — not comrak — decides what counts as a wiki link:
//!
//! * `![[…]]`, the embed form, which comrak does not read at all: its `!`
//!   opens an image label, which stops the wiki link from being seen.
//! * A component may not hold `[`, `]` or, before the first one, `|`, exactly
//!   as comrak's does; a backslash escapes the punctuation after it, and the
//!   escape stays in the attribute as the two bytes it is.
//! * An *empty* alias — `[[a|]]` — is refused, because `[[a]]` and `[[a|]]`
//!   would otherwise be the same atom and only one of them could be written
//!   back.
//! * A line ending inside one is refused too. No wiki link spans lines, and
//!   the atom's own source must be one line to be written where a paragraph's
//!   line breaks are nodes rather than characters.
//!
//! Whatever is refused stays the text a reader sees: ordinary characters of
//! its textblock, which the writer puts back exactly as they were.

/// The label length comrak stops a wiki link component at.
const MAX_COMPONENT: usize = 1000;

/// The parts of a wiki link, spelled exactly as the source spells them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WikiLink {
    /// The raw text before the first `|`, including any `#heading` or `^block`
    /// suffix and the spaces around it.
    pub target: String,
    /// The raw text after the first `|`, empty where the source has no `|`.
    pub alias: String,
    /// Whether the source spells it `![[…]]`.
    pub embed: bool,
}

/// What a wiki link reads as: the alias the author gave it, or the target it
/// names where there is none.
///
/// Free-standing because a [`WIKI_LINK`](crate::schema::WIKI_LINK) atom carries
/// the two parts as separate attributes, and the plain-text projection has to ask
/// the same question of those without rebuilding the link.
pub fn label<'a>(target: &'a str, alias: &'a str) -> &'a str {
    if alias.is_empty() { target } else { alias }
}

impl WikiLink {
    /// What the editor shows for it: the alias, or the target as written.
    pub fn label(&self) -> &str {
        label(&self.target, &self.alias)
    }

    /// The source of this link: `[[target]]`, `[[target|alias]]`, with a
    /// leading `!` for an embed, and no normalisation of either part.
    pub fn source(&self) -> String {
        let mut out = String::with_capacity(self.target.len() + self.alias.len() + 6);
        if self.embed {
            out.push('!');
        }
        out.push_str("[[");
        out.push_str(&self.target);
        if !self.alias.is_empty() {
            out.push('|');
            out.push_str(&self.alias);
        }
        out.push_str("]]");
        out
    }
}

/// Read the wiki link that `source` begins with, with the byte length it
/// covers, or `None` where it begins with something else.
pub fn read_wiki_link(source: &str) -> Option<(WikiLink, usize)> {
    let embed = source.starts_with('!');
    let mut at = usize::from(embed);
    if !source[at..].starts_with("[[") {
        return None;
    }
    at += 2;
    let end = component_end(source, at)?;
    let target = source[at..end].to_string();
    at = end;
    let alias = if source[at..].starts_with('|') {
        at += 1;
        let end = component_end(source, at)?;
        // `[[a]]` and `[[a|]]` would be the same atom, so only one of them can
        // be read as one at all.
        if end == at {
            return None;
        }
        let alias = source[at..end].to_string();
        at = end;
        alias
    } else {
        String::new()
    };
    if !source[at..].starts_with("]]") {
        return None;
    }
    at += 2;
    let link = WikiLink {
        target,
        alias,
        embed,
    };
    // A link spanning a line ending is not one; refusing it here keeps every
    // caller — the parse rule and `derive` — of one mind.
    (!link.target.contains('\n') && !link.alias.contains('\n')).then_some((link, at))
}

/// The wiki link `source` is in its entirety, or `None` where it is anything
/// else — including a wiki link with text after it.
pub fn whole_wiki_link(source: &str) -> Option<WikiLink> {
    let (link, len) = read_wiki_link(source)?;
    (len == source.len()).then_some(link)
}

/// Where the component starting at `from` ends: at `[`, `]` or `|`, with a
/// backslash escaping whatever punctuation follows it.
fn component_end(source: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'[' | b']' | b'|' => break,
            b'\\' => {
                at += 1;
                if bytes.get(at).is_some_and(u8::is_ascii_punctuation) {
                    at += 1;
                }
            }
            _ => at += 1,
        }
        if at - from > MAX_COMPONENT {
            return None;
        }
    }
    Some(at)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn read(source: &str) -> Option<WikiLink> {
        whole_wiki_link(source)
    }

    fn link(target: &str, alias: &str, embed: bool) -> Option<WikiLink> {
        Some(WikiLink {
            target: target.to_string(),
            alias: alias.to_string(),
            embed,
        })
    }

    #[test]
    fn every_accepted_form_writes_its_own_source_back() {
        for source in [
            "[[Note]]",
            "[[Note|Alias]]",
            "![[image.png]]",
            "![[Note|300]]",
            "[[a#heading]]",
            "[[a^block-id]]",
            "[[a#^block-id]]",
            "[[  spaced  ]]",
            "[[ a | b ]]",
            "[[]]",
            "[[|a]]",
            r"[[a\[b]]",
            r"[[a\|b]]",
            "[[folder/note.md]]",
        ] {
            let (link, len) = read_wiki_link(source).expect(source);
            assert_eq!(len, source.len(), "{source:?}");
            assert_eq!(link.source(), source, "{source:?}");
        }
    }

    #[test]
    fn the_parts_keep_the_bytes_the_source_spelled() {
        assert_eq!(read("[[Note]]"), link("Note", "", false));
        assert_eq!(read("[[ a | b ]]"), link(" a ", " b ", false));
        assert_eq!(read("![[a#b|c]]"), link("a#b", "c", true));
        assert_eq!(read(r"[[a\]b]]"), link(r"a\]b", "", false));
    }

    #[test]
    fn refused_forms_are_not_wiki_links() {
        for source in [
            "[[a|]]",
            "[[|]]",
            "[[a|b|c]]",
            "[[Note]",
            "[[a\nb]]",
            "[a]",
            "text",
            "[[a[b]]",
        ] {
            assert_eq!(read_wiki_link(source), None, "{source:?}");
        }
    }

    #[test]
    fn a_link_is_read_out_of_the_text_that_follows_it() {
        let (link, len) = read_wiki_link("[[a]] and more").expect("a wiki link");
        assert_eq!(len, 5);
        assert_eq!(link.label(), "a");
    }

    #[test]
    fn an_over_long_component_is_refused_as_comraks_is() {
        let long = format!("[[{}]]", "a".repeat(MAX_COMPONENT + 1));
        assert_eq!(read_wiki_link(&long), None);
    }
}
