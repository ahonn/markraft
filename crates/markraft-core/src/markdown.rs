use crate::{Block, BlockKind, Document, Mark, Marks, Span, push_span};

impl Document {
    /// Import the supported Markdown subset. Each physical line is one block and blank
    /// lines are retained as empty paragraphs. Unsupported block syntax stays literal;
    /// fenced blocks are retained verbatim, including their opening/closing fences.
    pub fn from_markdown(source: &str) -> Self {
        let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
        let mut fence: Option<char> = None;
        let mut blocks = Vec::new();
        for line in normalized.split('\n') {
            let trimmed = line.trim_start();
            let fence_char = if trimmed.starts_with("```")
                && find_code_close(trimmed, trimmed.bytes().take_while(|b| *b == b'`').count())
                    .is_none()
            {
                Some('`')
            } else if trimmed.starts_with("~~~") {
                Some('~')
            } else {
                None
            };
            if fence.is_some() || fence_char.is_some() {
                blocks.push(Block {
                    kind: BlockKind::Paragraph,
                    spans: if line.is_empty() {
                        vec![]
                    } else {
                        vec![Span {
                            text: line.to_owned(),
                            marks: Marks::default(),
                        }]
                    },
                });
                if fence.is_none() {
                    fence = fence_char;
                } else if fence == fence_char {
                    fence = None;
                }
                continue;
            }
            let (kind, text) = parse_block(line);
            blocks.push(Block {
                kind,
                spans: parse_inline(text, Marks::default()),
            });
        }
        let mut document = Self { blocks };
        document.normalize();
        document
    }

    pub fn to_markdown(&self) -> String {
        self.blocks
            .iter()
            .map(|block| {
                let prefix = match block.kind {
                    BlockKind::Paragraph => String::new(),
                    BlockKind::Heading(level) => {
                        format!("{} ", "#".repeat(usize::from(level.clamp(1, 6))))
                    }
                    BlockKind::Bullet => "- ".to_owned(),
                    BlockKind::Task { checked: false } => "- [ ] ".to_owned(),
                    BlockKind::Task { checked: true } => "- [x] ".to_owned(),
                };
                let mut line = prefix;
                for span in &block.spans {
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
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
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
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return (BlockKind::Bullet, rest);
        }
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
                    push_span(&mut spans, &span.text, span.marks);
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
