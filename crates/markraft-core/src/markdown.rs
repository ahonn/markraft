use crate::{
    Block, BlockKind, Document, Mark, Marks, Span, push_like, push_linked_span, push_span,
};

impl Document {
    /// Import the supported Markdown subset. Each physical line is one block and blank
    /// lines are retained as empty paragraphs. Unsupported block syntax stays literal.
    pub fn from_markdown(source: &str) -> Self {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        // The open fence's marker and the index of the first block it produced.
        let mut fence: Option<(String, BlockKind, usize)> = None;
        let mut blocks = Vec::new();
        for line in normalized.split('\n') {
            if let Some((marker, kind, first)) = &fence {
                let trimmed = line.trim();
                if trimmed.starts_with(marker.as_str())
                    && trimmed.trim_start_matches(&marker[..1]).is_empty()
                {
                    if blocks.len() == *first {
                        blocks.push(Block {
                            kind: kind.clone(),
                            spans: vec![],
                        });
                    }
                    fence = None;
                    continue;
                }
                let mut spans = vec![];
                push_span(&mut spans, line, Marks::default());
                blocks.push(Block {
                    kind: kind.clone(),
                    spans,
                });
                continue;
            }
            if let Some((marker, language)) = parse_fence(line) {
                let kind = BlockKind::Code {
                    language: language.to_owned(),
                };
                fence = Some((marker.to_owned(), kind, blocks.len()));
                continue;
            }
            let (kind, text) = parse_block(line);
            blocks.push(Block {
                kind,
                spans: parse_inline(text, Marks::default()),
            });
        }
        if let Some((_, kind, first)) = fence
            && blocks.len() == first
        {
            blocks.push(Block {
                kind,
                spans: vec![],
            });
        }
        let mut document = Self { blocks };
        document.normalize();
        document
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
                BlockKind::Quote => "> ".to_owned(),
                BlockKind::Divider => {
                    lines.push("---".to_owned());
                    continue;
                }
            };
            let mut line = prefix;
            let mut link: Option<&str> = None;
            for span in &block.spans {
                if link != span.link.as_deref() {
                    if let Some(url) = link {
                        line.push_str(&link_close(url));
                    }
                    link = span.link.as_deref();
                    if link.is_some() {
                        line.push('[');
                    }
                }
                let mut text = if span.marks.code {
                    code_text(&span.text)
                } else {
                    escape(&span.text)
                };
                if span.marks.italic {
                    text = format!("*{text}*");
                }
                if span.marks.bold {
                    text = format!("**{text}**");
                }
                if span.marks.strikethrough {
                    text = format!("~~{text}~~");
                }
                if span.marks.underline {
                    text = format!("<u>{text}</u>");
                }
                line.push_str(&text);
            }
            if let Some(url) = link {
                line.push_str(&link_close(url));
            }
            lines.push(line);
        }
        lines.join("\n")
    }
}

/// An opening fence: its marker and info string. Inline code written as
/// "```text```" on one line is not a fence.
fn parse_fence(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let character = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = trimmed.chars().take_while(|c| *c == character).count();
    let info = trimmed[length..].trim();
    (length >= 3 && !(character == '`' && info.contains('`'))).then(|| {
        (
            &trimmed[..length],
            info.split_whitespace().next().unwrap_or(""),
        )
    })
}

fn parse_block(line: &str) -> (BlockKind, &str) {
    for (prefix, checked) in [
        ("- [ ] ", false),
        ("- [x] ", true),
        ("- [X] ", true),
        ("* [ ] ", false),
        ("* [x] ", true),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (BlockKind::Task { checked }, rest);
        }
    }
    let rule = line.trim();
    if rule.len() >= 3
        && ["-", "*", "_"]
            .iter()
            .any(|c| rule.trim_start_matches(c).is_empty())
    {
        return (BlockKind::Divider, "");
    }
    if let Some(rest) = line.strip_prefix("> ") {
        return (BlockKind::Quote, rest);
    }
    if line == ">" {
        return (BlockKind::Quote, "");
    }
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (BlockKind::Bullet, rest);
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && let Some(rest) = line[digits..]
            .strip_prefix(". ")
            .or_else(|| line[digits..].strip_prefix(") "))
    {
        return (BlockKind::Ordered, rest);
    }
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        return (BlockKind::Heading(hashes as u8), &line[hashes + 1..]);
    }
    (BlockKind::Paragraph, line)
}

fn parse_inline(source: &str, marks: Marks) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let rest = &source[index..];
        if rest.starts_with('\\')
            && let Some(next) = rest[1..].chars().next()
            && next.is_ascii_punctuation()
        {
            push_span(&mut spans, &next.to_string(), marks);
            index += 1 + next.len_utf8();
            continue;
        }
        if rest.starts_with('`') {
            let count = rest.bytes().take_while(|b| *b == b'`').count();
            let delimiter = "`".repeat(count);
            if let Some(close) = find_code_close(rest, count) {
                let mut content = &rest[count..close];
                if content.starts_with(' ')
                    && content.ends_with(' ')
                    && content.chars().any(|c| c != ' ')
                {
                    content = &content[1..content.len() - 1];
                }
                push_span(
                    &mut spans,
                    content,
                    Marks {
                        code: true,
                        ..marks
                    },
                );
                index += close + delimiter.len();
                continue;
            }
        }
        // Images have no model; keep them literal rather than reading a link after "!".
        if rest.starts_with("![")
            && let Some((_, _, length)) = parse_link(&rest[1..])
        {
            push_span(&mut spans, &rest[..1 + length], marks);
            index += 1 + length;
            continue;
        }
        if let Some((text, url, length)) = parse_link(rest) {
            for span in parse_inline(text, marks) {
                push_linked_span(&mut spans, &span.text, span.marks, Some(url));
            }
            index += length;
            continue;
        }
        let mut parsed = false;
        for (open, close, mark) in INLINE_DELIMITERS {
            if !rest.starts_with(open) {
                continue;
            }
            if let Some(end) = find_delimiter(rest, open.len(), close) {
                let mut inner = marks;
                for mark in *mark {
                    inner.set(*mark, true);
                }
                for span in parse_inline(&rest[open.len()..end], inner) {
                    push_like(&mut spans, &span.text, &span);
                }
                index += end + close.len();
                parsed = true;
                break;
            }
        }
        if parsed {
            continue;
        }
        let character = rest.chars().next().unwrap();
        push_span(&mut spans, &character.to_string(), marks);
        index += character.len_utf8();
    }
    spans
}

/// `[text](url)` or `[text](<url>)` at the start of `source`: the text, the URL and
/// the length of the whole construct.
fn parse_link(source: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0;
    let mut close = None;
    let mut characters = source.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                characters.next();
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(index);
                    break;
                }
            }
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    let close = close.filter(|close| *close > 1)?;
    let target = source[close + 1..].strip_prefix('(')?;
    let start = close + 2;
    if let Some(bracketed) = target.strip_prefix('<') {
        let end = bracketed.find('>')?;
        bracketed[end + 1..].starts_with(')').then_some(())?;
        return Some((&source[1..close], &bracketed[..end], start + end + 3));
    }
    let mut depth = 0;
    for (index, character) in target.char_indices() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => {
                return (index > 0).then_some((
                    &source[1..close],
                    &target[..index],
                    start + index + 1,
                ));
            }
            ')' => depth -= 1,
            _ if character.is_whitespace() => return None,
            _ => {}
        }
    }
    None
}

fn link_close(url: &str) -> String {
    let balanced = url.chars().try_fold(0i32, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' => (depth > 0).then_some(depth - 1),
        _ => Some(depth),
    }) == Some(0);
    if url.is_empty() || url.contains(char::is_whitespace) || !balanced {
        format!("](<{url}>)")
    } else {
        format!("]({url})")
    }
}

/// Longer delimiters come first so `***` is not read as `**` followed by `*`.
/// Underline has no Markdown syntax and round-trips as inline HTML.
const INLINE_DELIMITERS: &[(&str, &str, &[Mark])] = &[
    ("***", "***", &[Mark::Bold, Mark::Italic]),
    ("___", "___", &[Mark::Bold, Mark::Italic]),
    ("**", "**", &[Mark::Bold]),
    ("__", "__", &[Mark::Bold]),
    ("~~", "~~", &[Mark::Strikethrough]),
    ("<u>", "</u>", &[Mark::Underline]),
    ("*", "*", &[Mark::Italic]),
    ("_", "_", &[Mark::Italic]),
];

fn find_delimiter(source: &str, open_len: usize, delimiter: &str) -> Option<usize> {
    let mut index = open_len;
    while index < source.len() {
        let rest = &source[index..];
        if rest.starts_with('\\') {
            index += 1;
            if index < source.len() {
                index += source[index..].chars().next().unwrap().len_utf8();
            }
        } else if rest.starts_with('`') {
            let count = rest.bytes().take_while(|b| *b == b'`').count();
            if let Some(close) = find_code_close(rest, count) {
                index += close + count;
            } else {
                index += count;
            }
        } else if rest.starts_with(delimiter) {
            if index > open_len {
                return Some(index);
            }
            index += delimiter.len();
        } else {
            index += rest.chars().next().unwrap().len_utf8();
        }
    }
    None
}

fn find_code_close(source: &str, count: usize) -> Option<usize> {
    let mut index = count;
    while index < source.len() {
        if source.as_bytes()[index] == b'`' {
            let run = source[index..].bytes().take_while(|b| *b == b'`').count();
            if run == count {
                return Some(index);
            }
            index += run;
        } else {
            index += source[index..].chars().next().unwrap().len_utf8();
        }
    }
    None
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

fn escape(source: &str) -> String {
    let mut result = String::new();
    for character in source.chars() {
        if "\\`*_{}[]<>()#+-.!>|~".contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}
