//! Markdown for the flat block model: [`import`] reads CommonMark and
//! [`Document::to_markdown`] writes the subset the model can express.

mod import;

use finl_unicode::categories::CharacterCategories;

use crate::{Block, BlockKind, Document, Span};

impl Document {
    /// Import CommonMark. Each physical line is one block and blank lines are retained as
    /// empty paragraphs. Syntax the model cannot hold stays literal.
    pub fn from_markdown(source: &str) -> Self {
        import::document(source)
    }

    /// Import Markdown for insertion into existing text. Unlike document import, this
    /// keeps trailing spaces and tabs in the final paragraph so pasted words stay apart.
    pub fn from_markdown_fragment(source: &str) -> Self {
        import::fragment(source)
    }

    pub fn to_markdown(&self) -> String {
        let mut lines = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if let BlockKind::Code { language } = &block.kind {
                let continues = |other: Option<&Block>| other.is_some_and(|b| b.kind == block.kind);
                let run_start = self.blocks[..=index]
                    .iter()
                    .rposition(|b| b.kind != block.kind)
                    .map_or(0, |i| i + 1);
                let run_len = self.blocks[run_start..]
                    .iter()
                    .take_while(|b| b.kind == block.kind)
                    .count();
                // The fence outgrows any backtick run that starts a line of the block.
                let fence = "`".repeat(
                    self.blocks[run_start..run_start + run_len]
                        .iter()
                        .map(|b| {
                            b.text()
                                .trim_start()
                                .bytes()
                                .take_while(|c| *c == b'`')
                                .count()
                                + 1
                        })
                        .max()
                        .unwrap_or(0)
                        .max(3),
                );
                if !continues(index.checked_sub(1).and_then(|i| self.blocks.get(i))) {
                    lines.push(format!("{fence}{language}"));
                }
                lines.push(block.text());
                if !continues(self.blocks.get(index + 1)) {
                    lines.push(fence);
                }
                continue;
            }
            let prefix = match block.kind {
                BlockKind::Paragraph | BlockKind::Code { .. } => String::new(),
                BlockKind::Heading(level) => {
                    format!("{} ", "#".repeat(usize::from(level.clamp(1, 6))))
                }
                BlockKind::Bullet => "- ".to_owned(),
                BlockKind::Ordered => format!("{}. ", self.ordinal(index).unwrap_or(1)),
                BlockKind::Task { checked: false } => "- [ ] ".to_owned(),
                BlockKind::Task { checked: true } => "- [x] ".to_owned(),
                BlockKind::Quote => "> ".repeat(usize::from(block.depth) + 1),
                BlockKind::Divider => {
                    lines.push("---".to_owned());
                    continue;
                }
            };
            let mut content = String::new();
            let mut link: Option<&str> = None;
            for (position, span) in block.spans.iter().enumerate() {
                if link != span.link.as_deref() {
                    if let Some(url) = link {
                        content.push_str(&link_close(url));
                    }
                    link = span.link.as_deref();
                    if link.is_some() {
                        content.push('[');
                    }
                }
                let mut text = if span.marks.code {
                    code_text(&span.text)
                } else {
                    escape(&span.text)
                };
                let edges = (
                    content.chars().next_back(),
                    lead(span, block.spans.get(position + 1)),
                );
                // A mark written as a tag puts punctuation around the marks inside it,
                // whatever the span's own neighbours are.
                let wrapped = |outer: bool| if outer { (Some('>'), Some('<')) } else { edges };
                let emphasis = usize::from(span.marks.italic) + 2 * usize::from(span.marks.bold);
                if emphasis > 0 {
                    // One run of asterisks, so bold and italic together read as `***`.
                    let run = "*".repeat(emphasis);
                    let (before, after) = wrapped(span.marks.strikethrough || span.marks.underline);
                    text = if flanks(&text, before, after, '*') {
                        format!("{run}{text}{run}")
                    } else {
                        let text = tagged(&text, "em", span.marks.italic);
                        tagged(&text, "strong", span.marks.bold)
                    };
                }
                if span.marks.strikethrough {
                    let (before, after) = wrapped(span.marks.underline);
                    text = if flanks(&text, before, after, '~') {
                        format!("~~{text}~~")
                    } else {
                        tagged(&text, "del", true)
                    };
                }
                if span.marks.underline {
                    text = tagged(&text, "u", true);
                }
                content.push_str(&text);
            }
            if let Some(url) = link {
                content.push_str(&link_close(url));
            }
            let mut line = if block.kind.is_list() {
                format!("{}{prefix}", "    ".repeat(usize::from(block.depth)))
            } else {
                prefix
            };
            line.push_str(&protect_indent(&content));
            lines.push(line);
        }
        lines.join("\n")
    }
}

fn tagged(text: &str, tag: &str, wrap: bool) -> String {
    if wrap {
        format!("<{tag}>{text}</{tag}>")
    } else {
        text.to_owned()
    }
}

/// Whether a run of `delimiter` wrapped around `text` would both open and close emphasis
/// where it sits. CommonMark only lets a run open when nothing but a word follows it, or
/// the punctuation that follows is matched by whitespace or punctuation before it, and
/// mirrors that for closing. A run that touches another delimiter run merges with it, so
/// neither may be read as emphasis. A span the rule turns down is written as inline HTML.
fn flanks(text: &str, before: Option<char>, after: Option<char>, delimiter: char) -> bool {
    let punctuation = |c: char| c.is_ascii_punctuation() || c.is_punctuation() || c.is_symbol();
    let boundary = |edge: Option<char>| {
        edge.is_none_or(|c| (c.is_whitespace() || punctuation(c)) && !matches!(c, '*' | '~'))
    };
    let opens = text
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && (!punctuation(c) || boundary(before)));
    let closes = text
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && (!punctuation(c) || boundary(after)));
    opens && closes && before != Some(delimiter)
}

/// The first character the span after this one writes, which is all [`flanks`] needs of it.
/// Every mark and every link edge writes punctuation first.
fn lead(span: &Span, next: Option<&Span>) -> Option<char> {
    let next = next?;
    if next.link != span.link {
        return Some('[');
    }
    let marked = [
        (next.marks.underline, '<'),
        (next.marks.strikethrough, '~'),
        (next.marks.bold || next.marks.italic, '*'),
        (next.marks.code, '`'),
    ];
    if let Some((_, character)) = marked.iter().find(|(set, _)| *set) {
        return Some(*character);
    }
    let first = next.text.chars().next()?;
    Some(if ESCAPED.contains(first) { '\\' } else { first })
}

/// Four columns of indentation would make a reader take the content for an indented code
/// block, so its first space or tab travels as an entity instead. Lesser indentation and
/// trailing whitespace are left alone: a reader may strip them, which changes no structure.
fn protect_indent(content: &str) -> String {
    if content.trim().is_empty() || import::indent_width(content) < 4 {
        return content.to_owned();
    }
    let entity = if content.starts_with('\t') {
        "&#9;"
    } else {
        "&#32;"
    };
    format!("{entity}{}", &content[1..])
}

fn link_close(url: &str) -> String {
    let balanced = url.chars().try_fold(0i32, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' => (depth > 0).then_some(depth - 1),
        _ => Some(depth),
    }) == Some(0);
    // The parser decodes escapes and entities in destinations. Protect their literal
    // characters so each save/load cycle resolves to the same URL.
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
        format!("](<{escaped}>)")
    } else {
        format!("]({escaped})")
    }
}

fn code_text(source: &str) -> String {
    let longest = source.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let delimiter = "`".repeat(longest + 1);
    let padding = source.starts_with('`')
        || source.ends_with('`')
        || (source.starts_with(' ') && source.ends_with(' ') && source.chars().any(|c| c != ' '));
    if padding {
        format!("{delimiter} {source} {delimiter}")
    } else {
        format!("{delimiter}{source}{delimiter}")
    }
}

/// Every character a reader could take for syntax. `&` is included because a reader
/// resolves character references, so `&amp;` has to come back as itself.
const ESCAPED: &str = "\\`*_{}[]<>()#+-.!>|~&";

fn escape(source: &str) -> String {
    let mut result = String::new();
    for character in source.chars() {
        if ESCAPED.contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}
