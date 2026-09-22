//! Turning text back into Markdown that reads as itself.
//!
//! Everything here answers one question: which characters would a CommonMark
//! reader take for syntax *in this position*? A serialiser that escapes every
//! punctuation character produces correct but unreadable output, so each rule
//! below is as narrow as the grammar allows.
//!
//! * Always escaped: `` \ ` * [ ] ~ `` — these open a construct wherever they
//!   appear.
//! * `_` is escaped unless it sits between two alphanumerics, where CommonMark
//!   refuses to read it as emphasis anyway.
//! * `<` is escaped only when what follows could start a tag or an autolink.
//! * `&` is escaped only when it would be read as a character reference, so
//!   `R&D` stays `R&D` and `&amp;` comes back as itself.
//! * `#`, `>`, `-`, `+`, `=` and `1.` are escaped only at the start of a line,
//!   and only in the shapes that actually open a block there.
//! * `.`, `(`, `)`, `!`, `|`, `{` and `}` are never escaped: none of them opens
//!   anything on its own, and `![` cannot form because `[` is escaped already.
//!   A `|` inside a table cell is the exception, and has [`escape_pipes`] of
//!   its own because it is resolved before the cell's content is read at all.
//! * Text that no link encloses has the one character that would start a GFM
//!   autolink escaped as well, so a URL a reader would link does not gain a
//!   link the document does not have. See [`escape_unlinked_text`].

use finl_unicode::categories::CharacterCategories;

/// Characters that open an inline construct wherever they appear.
const ALWAYS: &[char] = &['\\', '`', '*', '[', ']', '~'];

/// Escape `text` so a CommonMark reader gives it back unchanged.
///
/// `at_line_start` says whether the text begins a line's *content* — after any
/// block prefix such as `> ` or `- `, where the block-opening characters mean
/// something.
pub fn escape_text(text: &str, at_line_start: bool) -> String {
    escape(text, at_line_start, false)
}

/// [`escape_text`] for text that no link encloses, which also keeps a URL in
/// it from being read as an autolink.
///
/// GFM links a bare `https://…`, `www.…` or e-mail address, so text that only
/// looks like one would come back carrying a link the document never had.
/// Inside a link there is nothing to protect: a reader reads no autolink in a
/// label, and the URL of a link written bare *is* that link.
pub fn escape_unlinked_text(text: &str, at_line_start: bool) -> String {
    escape(text, at_line_start, true)
}

fn escape(text: &str, at_line_start: bool, unlinked: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let line_escapes = if at_line_start {
        line_start_escapes(&chars)
    } else {
        Vec::new()
    };
    for (index, ch) in chars.iter().copied().enumerate() {
        // A line ending inside inline content is not a break the model asked
        // for — those are `hard_break` nodes — so it travels as a reference
        // rather than ending the block.
        if matches!(ch, '\n' | '\r') {
            out.push_str(if ch == '\n' { "&#10;" } else { "&#13;" });
            continue;
        }
        let escape = line_escapes.contains(&index)
            || ALWAYS.contains(&ch)
            || (ch == '_' && !intraword(&chars, index))
            || (ch == '<' && opens_tag(&chars, index))
            || (ch == '&' && opens_reference(&chars, index))
            || (unlinked && opens_autolink(&chars, index));
        if escape {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Whether the character at `index` is the one that starts a GFM autolink,
/// which a backslash before it is the narrowest way to prevent.
///
/// There are three: the `:` of an `http://`, `https://` or `ftp://` URL, the
/// `.` of a `www.` address, and the `@` of an e-mail address. Each needs a host
/// with a dot in it, which is what keeps ordinary prose — `see: //x`, `a@b` —
/// out of this. The test is a little wider than the extension's, because a
/// backslash before a `:` a reader would not have linked anyway costs one
/// character and changes nothing it renders.
fn opens_autolink(chars: &[char], index: usize) -> bool {
    match chars[index] {
        ':' => {
            scheme_before(chars, index)
                && chars[index + 1..].starts_with(&['/', '/'])
                && dotted_host(&chars[index + 3..])
        }
        '.' => www_before(chars, index) && dotted_host(&chars[index + 1..]),
        '@' => local_part_before(chars, index) && dotted_host(&chars[index + 1..]),
        _ => false,
    }
}

/// Whether the whole run of letters before `index` is a scheme the extension
/// links. A reader reads the run back to its start, so `xhttp://a.b` is no URL.
fn scheme_before(chars: &[char], index: usize) -> bool {
    let start = chars[..index]
        .iter()
        .rposition(|c| !c.is_ascii_alphabetic())
        .map_or(0, |at| at + 1);
    let scheme: String = chars[start..index].iter().collect();
    matches!(scheme.as_str(), "http" | "https" | "ftp")
}

/// Whether `index` is the dot of a `www.` address: a `www` that starts the
/// text or follows whitespace or one of the delimiters a reader allows there.
fn www_before(chars: &[char], index: usize) -> bool {
    let Some(start) = index.checked_sub(3) else {
        return false;
    };
    chars[start..index] == ['w', 'w', 'w']
        && start.checked_sub(1).is_none_or(|at| {
            chars[at].is_whitespace() || matches!(chars[at], '*' | '_' | '~' | '(' | '[')
        })
}

/// Whether an e-mail local part sits directly before `index`.
fn local_part_before(chars: &[char], index: usize) -> bool {
    index.checked_sub(1).is_some_and(|at| {
        chars[at].is_ascii_alphanumeric() || matches!(chars[at], '.' | '+' | '-' | '_')
    })
}

/// Whether `rest` opens with a host name holding a dot, which every autolink
/// the extension reads has to have.
fn dotted_host(rest: &[char]) -> bool {
    let mut dotted = false;
    let mut length = 0;
    for (offset, c) in rest.iter().copied().enumerate() {
        if c == '.' {
            // A dot only separates labels when another one follows it.
            if offset == 0 || !rest.get(offset + 1).is_some_and(|next| host_char(*next)) {
                break;
            }
            dotted = true;
        } else if !host_char(c) && c != '-' && c != '_' {
            break;
        }
        length = offset + 1;
    }
    dotted && length > 0
}

/// What a reader takes for part of a host name.
fn host_char(c: char) -> bool {
    !(c.is_whitespace() || c.is_punctuation() || c.is_symbol())
}

/// Whether `_` at `index` sits between two alphanumerics, where CommonMark's
/// flanking rules forbid emphasis.
fn intraword(chars: &[char], index: usize) -> bool {
    let before = index.checked_sub(1).and_then(|i| chars.get(i));
    let after = chars.get(index + 1);
    matches!((before, after), (Some(b), Some(a)) if b.is_alphanumeric() && a.is_alphanumeric())
}

/// Whether `<` at `index` could open an HTML tag, a comment or an autolink.
fn opens_tag(chars: &[char], index: usize) -> bool {
    matches!(chars.get(index + 1), Some(c) if c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
}

/// Whether `&` at `index` would be read as a character reference: a name, a
/// decimal or a hexadecimal escape, each closed by `;`.
fn opens_reference(chars: &[char], index: usize) -> bool {
    let rest = &chars[index + 1..];
    let (digits, body): (fn(&char) -> bool, &[char]) = match rest.first() {
        Some('#') => match rest.get(1) {
            Some('x') | Some('X') => (|c| c.is_ascii_hexdigit(), &rest[2..]),
            _ => (|c| c.is_ascii_digit(), &rest[1..]),
        },
        Some(c) if c.is_ascii_alphabetic() => (|c| c.is_ascii_alphanumeric(), rest),
        _ => return false,
    };
    let taken = body.iter().take_while(|c| digits(c)).count();
    taken > 0 && body.get(taken) == Some(&';')
}

/// Indices of characters that only open a block when they start a line.
fn line_start_escapes(chars: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    match chars.first() {
        // `>` opens a block quote with or without a following space.
        Some('>') => out.push(0),
        // `#` up to six times, then a space or the end of the line.
        Some('#') => {
            let hashes = chars.iter().take_while(|c| **c == '#').count();
            if hashes <= 6 && chars.get(hashes).is_none_or(|c| c.is_whitespace()) {
                out.push(0);
            }
        }
        // `-` and `+` open a list item when a space follows, and `-` also opens
        // a thematic break or a setext underline on a line of its own.
        Some(c @ ('-' | '+'))
            if chars.get(1).is_none_or(|c| c.is_whitespace())
                || (*c == '-' && only_of(chars, '-')) =>
        {
            out.push(0)
        }
        // `=` underlines the paragraph above it as a setext heading. A line of
        // ours only follows a paragraph line after a hard break.
        Some('=') if only_of(chars, '=') => out.push(0),
        _ => {}
    }
    // `1.` and `1)` open an ordered list; escaping the delimiter is enough.
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && matches!(chars.get(digits), Some('.' | ')'))
        && chars.get(digits + 1).is_none_or(|c| c.is_whitespace())
    {
        out.push(digits);
    }
    out
}

/// Whether the line holds nothing but `character`, spaces and tabs, with at
/// least one `character` — the shape of a thematic break or setext underline.
fn only_of(chars: &[char], character: char) -> bool {
    chars
        .iter()
        .all(|c| *c == character || *c == ' ' || *c == '\t')
        && chars.iter().filter(|c| **c == character).count() >= 1
}

/// Four columns of indentation would make a reader take the line for an
/// indented code block, so the first space or tab travels as a character
/// reference instead.
///
/// Lesser indentation and trailing whitespace are left alone: a reader may
/// strip them, which changes no structure.
pub fn protect_indent(line: &str) -> String {
    if line.trim().is_empty() || indent_width(line) < 4 {
        return line.to_string();
    }
    let entity = if line.starts_with('\t') {
        "&#9;"
    } else {
        "&#32;"
    };
    format!("{entity}{}", &line[1..])
}

/// The indentation of a line in columns, with tab stops every four.
pub fn indent_width(line: &str) -> usize {
    let mut width = 0;
    for byte in line.bytes() {
        match byte {
            b' ' => width += 1,
            b'\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// The `](destination)` that closes a link, with the destination protected.
///
/// A reader resolves backslash escapes and character references inside a
/// destination, so their literal characters have to travel escaped for every
/// save/load cycle to resolve to the same URL. A destination that is empty,
/// holds whitespace or has unbalanced parentheses goes in pointy brackets.
pub fn link_destination(url: &str) -> String {
    let balanced = url.chars().try_fold(0i32, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' => (depth > 0).then_some(depth - 1),
        _ => Some(depth),
    }) == Some(0);
    let mut escaped = String::new();
    for (index, character) in url.char_indices() {
        match character {
            '&' => escaped.push_str("&amp;"),
            c if c.is_ascii_control() || (c == ' ' && (index == 0 || index + 1 == url.len())) => {
                escaped.push_str(&format!("&#{};", u32::from(c)));
            }
            '\\' | '<' | '>' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    if url.is_empty() || url.contains(char::is_whitespace) || !balanced {
        format!("<{escaped}>")
    } else {
        escaped
    }
}

/// The ` "title"` that follows a link or image destination, or the empty string.
pub fn link_title(title: &str) -> String {
    if title.is_empty() {
        return String::new();
    }
    format!(" \"{}\"", escape_inside(title, &['"', '\\']))
}

/// Protect the pipes of a table cell, whose text is otherwise escaped already.
///
/// GFM splits a row on its unescaped pipes *before* it reads the cell's inline
/// content, so a `|` has to be escaped wherever it sits — inside a code span,
/// inside raw HTML — and a reader resolves the escape everywhere too. That is
/// why this runs over the written text rather than over the source characters.
pub fn escape_pipes(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Escape the label of a link or an image.
///
/// A label is ordinary inline content, so everything [`escape_text`] protects
/// has to be protected here too: a backtick left alone in an image's `alt`
/// opens a code span that swallows the rest of the paragraph.
pub fn escape_label(text: &str) -> String {
    escape_text(text, false)
}

/// Backslash-escape `special` and any `&` that would be read as a character
/// reference, and send line endings as character references: a raw one would
/// end the construct the text sits in.
fn escape_inside(text: &str, special: &[char]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (index, c) in chars.iter().copied().enumerate() {
        match c {
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => {
                if special.contains(&c) || (c == '&' && opens_reference(&chars, index)) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
    }
    out
}

/// The backtick run and padding that delimit `text` as a code span.
///
/// The run outgrows any run inside the text, and a space is added on both sides
/// when the text starts or ends with a backtick, or is entirely padded with
/// spaces, because a reader strips exactly one such space.
pub fn code_span_delimiters(text: &str) -> (String, String) {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let ticks = "`".repeat(longest + 1);
    let padded = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' ') && text.chars().any(|c| c != ' '));
    if padded {
        (format!("{ticks} "), format!(" {ticks}"))
    } else {
        (ticks.clone(), ticks)
    }
}
