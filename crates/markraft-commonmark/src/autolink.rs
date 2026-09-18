//! Whether a link can be written as the bare URL a reader links back.
//!
//! GFM's autolink extension turns a plain `https://…`, `www.…` or e-mail
//! address into a link, so an author who typed one plain gets a link mark back
//! and must not get `[url](url)` written into their file. Whether the bare
//! spelling comes back *as the same link* depends on what surrounds it: the
//! characters before it join the URL or keep it from being one, and the
//! punctuation after it is trimmed or swallowed by rules — a domain that has
//! to hold a dot, parentheses that have to balance — this crate has no reason
//! to re-implement. So the question is put to comrak: the candidate line is
//! parsed and the bare spelling used only when exactly this link comes back.

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, parse_document};

use crate::escape::escape_text;
use crate::parse::commonmark_options;

/// Whether a link over `text` with this `href` and `title` is what a reader
/// builds from the bare `text` written between `before` and `after`.
///
/// `before` is the character the URL follows on its line, `None` at the start
/// of one. `after` is the text written directly behind it, already escaped and
/// cut at the first whitespace — everything a reader could still pull in.
pub(crate) fn writes_bare(
    text: &str,
    href: &str,
    title: &str,
    before: Option<char>,
    after: &str,
) -> bool {
    // A bare URL carries no title, and the cheap test keeps the parse below
    // for the few links that could pass it at all.
    if !title.is_empty() || !autolink_href(text, href) {
        return false;
    }
    // The URL travels as ordinary text, and a backslash the escaper adds would
    // land inside the URL a reader reads back.
    if escape_text(text, true) != text {
        return false;
    }

    // A paragraph strips the whitespace at the start of its first line, and a
    // `>` or a `-` there opens a block rather than sitting in front of a URL,
    // so the probe puts a letter before whatever the URL actually follows. It
    // changes nothing the autolink rules look at: they read one character back
    // from the URL, and the scheme they read back over is broken by the letter
    // just as it is by the text the character really belongs to.
    let head = match before {
        Some(before) => format!("x{before}"),
        None => String::new(),
    };
    let mut probe = head.clone();
    probe.push_str(text);
    probe.push_str(after);
    let arena = Arena::new();
    let root = parse_document(&arena, &probe, &commonmark_options());
    let mut blocks = root.children();
    let (Some(paragraph), None) = (blocks.next(), blocks.next()) else {
        return false;
    };
    if !matches!(paragraph.data.borrow().value, NodeValue::Paragraph) {
        return false;
    }

    let mut leading = String::new();
    let mut trailing = String::new();
    let mut linked = false;
    for child in paragraph.children() {
        match piece(child) {
            Piece::Text(literal) if linked => trailing.push_str(&literal),
            Piece::Text(literal) => leading.push_str(&literal),
            Piece::Link { url, title } if !linked => {
                linked = true;
                if url != href || !title.is_empty() || only_text(child).as_deref() != Some(text) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    linked && leading == head && trailing == after
}

/// Whether `href` is the URL the autolink extension builds from `text`: the
/// text itself, the `mailto:` an e-mail address gets, or the `http://` a
/// `www.` address gets. The parse above is what settles which of the three it
/// is; this only keeps the obvious misses out of it.
fn autolink_href(text: &str, href: &str) -> bool {
    href == text
        || href.strip_prefix("mailto:") == Some(text)
        || href.strip_prefix("http://") == Some(text)
}

/// What one inline of the probe is, as far as this check cares.
enum Piece {
    Text(String),
    Link { url: String, title: String },
    Other,
}

fn piece<'a>(node: &'a AstNode<'a>) -> Piece {
    match &node.data.borrow().value {
        NodeValue::Text(literal) => Piece::Text(literal.to_string()),
        NodeValue::Link(link) => Piece::Link {
            url: link.url.clone(),
            title: link.title.clone(),
        },
        _ => Piece::Other,
    }
}

/// The text of a node holding one text leaf and nothing else.
fn only_text<'a>(node: &'a AstNode<'a>) -> Option<String> {
    let mut children = node.children();
    let (Some(only), None) = (children.next(), children.next()) else {
        return None;
    };
    match &only.data.borrow().value {
        NodeValue::Text(literal) => Some(literal.to_string()),
        _ => None,
    }
}
