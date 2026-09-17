//! Convert clipboard HTML to the supported document vocabulary. No resources are loaded.
use crate::{Block, BlockKind, Document, Marks, Span, push_linked_span};
use scraper::{ElementRef, Html, Node};

#[derive(Clone, Default)]
struct Context {
    marks: Marks,
    link: Option<String>,
    kind: BlockKind,
    depth: u8,
    list_depth: u8,
    list_kind: BlockKind,
    list_item_start: Option<usize>,
    quotes: u8,
}

#[derive(Default)]
struct Importer {
    blocks: Vec<Block>,
    current: Option<Block>,
}

impl Importer {
    fn begin(&mut self, context: &Context) {
        self.current.get_or_insert_with(|| Block {
            kind: context.kind.clone(),
            depth: context.depth,
            spans: vec![],
        });
    }

    fn flush(&mut self) {
        if let Some(mut block) = self.current.take() {
            while let Some(span) = block.spans.last_mut() {
                span.text
                    .truncate(span.text.trim_end_matches([' ', '\n', '\r', '\t']).len());
                if !span.text.is_empty() {
                    break;
                }
                block.spans.pop();
            }
            self.blocks.push(block);
        }
    }

    fn text(&mut self, text: &str, context: &Context) {
        // HTML collapses source indentation, but spaces between styled runs matter.
        let mut normalized = String::new();
        let mut space = self.current.as_ref().is_none_or(|block| {
            block.is_empty()
                || block
                    .spans
                    .last()
                    .is_some_and(|span| span.text.ends_with(' '))
        });
        for character in text.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                if !space {
                    normalized.push(' ');
                }
                space = true;
            } else {
                normalized.push(character);
                space = false;
            }
        }
        if !normalized.is_empty() {
            self.begin(context);
            push_linked_span(
                &mut self.current.as_mut().unwrap().spans,
                &normalized,
                context.marks,
                context.link.as_deref(),
            );
        }
    }

    fn children(&mut self, element: ElementRef<'_>, context: &Context) {
        for child in element.children() {
            match child.value() {
                Node::Text(text) => self.text(text, context),
                Node::Element(_) => self.element(ElementRef::wrap(child).unwrap(), context.clone()),
                _ => {}
            }
        }
    }

    fn element(&mut self, element: ElementRef<'_>, mut context: Context) {
        let name = element.value().name();
        match name {
            "script" | "style" | "head" | "template" | "noscript" => return,
            "pre" => {
                self.flush();
                let code = element
                    .child_elements()
                    .find(|child| child.value().name() == "code");
                let language = code
                    .and_then(|code| code.value().attr("class"))
                    .and_then(|classes| {
                        classes
                            .split_whitespace()
                            .find_map(|class| class.strip_prefix("language-"))
                    })
                    .unwrap_or("")
                    .to_owned();
                let text = element
                    .text()
                    .collect::<String>()
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                for line in text.split('\n') {
                    self.blocks.push(Block {
                        kind: BlockKind::Code {
                            language: language.clone(),
                        },
                        depth: 0,
                        spans: vec![Span {
                            text: line.to_owned(),
                            marks: Marks::default(),
                            link: None,
                        }],
                    });
                }
                return;
            }
            "br" => {
                // A sole break is an editor's empty-paragraph placeholder. Other
                // breaks create a following line, including when it stays empty.
                let placeholder = self.current.is_none()
                    && element.parent().is_some_and(|parent| {
                        parent.children().all(|sibling| {
                            sibling.id() == element.id()
                                || match sibling.value() {
                                    Node::Text(text) => text.trim().is_empty(),
                                    Node::Comment(_) => true,
                                    _ => false,
                                }
                        })
                    });
                self.begin(&context);
                if !placeholder {
                    self.flush();
                    self.begin(&context);
                }
                return;
            }
            "hr" => {
                self.flush();
                self.blocks.push(Block {
                    kind: BlockKind::Divider,
                    ..Block::default()
                });
                return;
            }
            "img" => {
                // Keep a useful representation without fetching or discarding the image.
                if let Some(src) = element.value().attr("src") {
                    self.text(
                        &format!("![{}]({src})", element.value().attr("alt").unwrap_or("")),
                        &context,
                    );
                }
                return;
            }
            "input" if element.value().attr("type") == Some("checkbox") => {
                // List-item context already owns the checkbox state, so its UI
                // wrapper must not emit a separate empty task before a <p>.
                if matches!(context.kind, BlockKind::Task { .. }) {
                    return;
                }
                self.begin(&context);
                self.current.as_mut().unwrap().kind = BlockKind::Task {
                    checked: element.value().attr("checked").is_some(),
                };
                return;
            }
            "strong" | "b" => context.marks.bold = true,
            "em" | "i" => context.marks.italic = true,
            "del" | "s" | "strike" => context.marks.strikethrough = true,
            "u" => context.marks.underline = true,
            "code" => context.marks.code = true,
            "a" => context.link = element.value().attr("href").map(str::to_owned),
            _ => {}
        }
        if let Some(style) = element.value().attr("style") {
            for declaration in style.split(';') {
                if let Some((key, value)) = declaration.split_once(':') {
                    let value = value.trim().to_ascii_lowercase();
                    match key.trim().to_ascii_lowercase().as_str() {
                        "font-weight" => {
                            context.marks.bold = value == "bold"
                                || value.parse::<u16>().is_ok_and(|weight| weight >= 600)
                        }
                        "font-style" => context.marks.italic = value == "italic",
                        "text-decoration" | "text-decoration-line" => {
                            context.marks.underline |= value.contains("underline");
                            context.marks.strikethrough |= value.contains("line-through");
                        }
                        _ => {}
                    }
                }
            }
        }
        let boundary = matches!(
            name,
            "p" | "div"
                | "section"
                | "article"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "li"
                | "ul"
                | "ol"
                | "blockquote"
                | "tr"
        );
        if boundary {
            if matches!(name, "ul" | "ol")
                && context.kind.is_list()
                && context.list_item_start == Some(self.blocks.len())
                && self.current.is_none()
            {
                self.begin(&context);
            }
            self.flush();
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                context.kind = BlockKind::Heading(name.as_bytes()[1] - b'0');
                context.depth = 0;
            }
            "ul" | "ol" => {
                context.list_depth = context.list_depth.saturating_add(1);
                context.list_kind = if name == "ol" {
                    BlockKind::Ordered
                } else {
                    BlockKind::Bullet
                };
            }
            "li" => {
                context.list_item_start = Some(self.blocks.len());
                context.kind = if element.value().attr("data-type") == Some("taskItem") {
                    BlockKind::Task {
                        checked: element.value().attr("data-checked") == Some("true"),
                    }
                } else if let Some(checked) = task_checkbox(element) {
                    BlockKind::Task { checked }
                } else {
                    context.list_kind.clone()
                };
                context.depth = context.list_depth.saturating_sub(1);
            }
            "blockquote" => {
                context.kind = BlockKind::Quote;
                context.depth = context.quotes;
                context.quotes = context.quotes.saturating_add(1);
            }
            _ => {}
        }
        let child_start = self.blocks.len();
        self.children(element, &context);
        if boundary {
            if matches!(name, "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
                && self.current.is_none()
                && self.blocks.len() == child_start
            {
                self.begin(&context);
            }
            self.flush();
        }
        if matches!(name, "td" | "th") {
            self.text(" ", &context);
        }
    }
}

/// Only a checkbox owned by this list item defines its task state; a
/// descendant list's checkbox belongs to that child item instead.
fn task_checkbox(item: ElementRef<'_>) -> Option<bool> {
    item.descendants()
        .filter_map(ElementRef::wrap)
        .find(|element| {
            element.value().name() == "input"
                && element.value().attr("type") == Some("checkbox")
                && element
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|ancestor| ancestor.value().name() == "li")
                    .is_some_and(|ancestor| ancestor.id() == item.id())
        })
        .map(|checkbox| checkbox.value().attr("checked").is_some())
}

impl Document {
    /// Import supported HTML formatting, ignoring scripts and keeping unsupported text.
    pub fn from_html(source: &str) -> Self {
        let html = Html::parse_fragment(source);
        let mut importer = Importer::default();
        importer.children(html.root_element(), &Context::default());
        importer.flush();
        let mut document = Self {
            blocks: importer.blocks,
        };
        document.normalize();
        document
    }

    /// Semantic HTML for other applications; internal clipboard metadata preserves exact
    /// document fragments when copying between Markraft editors.
    pub fn to_html(&self) -> String {
        let mut html = String::from("<meta charset=\"utf-8\">");
        let mut index = 0;
        while index < self.blocks.len() {
            let block = &self.blocks[index];
            if is_list(&block.kind) {
                render_list(&self.blocks, &mut index, block.depth, &mut html);
                continue;
            }
            match &block.kind {
                BlockKind::Code { language } => {
                    html.push_str(&format!(
                        "<pre><code class=\"language-{}\">",
                        escape(language)
                    ));
                    let kind = block.kind.clone();
                    let mut lines = Vec::new();
                    while index < self.blocks.len() && self.blocks[index].kind == kind {
                        lines.push(escape(&self.blocks[index].text()));
                        index += 1;
                    }
                    html.push_str(&lines.join("\n"));
                    html.push_str("</code></pre>");
                    continue;
                }
                BlockKind::Divider => html.push_str("<hr>"),
                BlockKind::Heading(level) => {
                    html.push_str(&format!("<h{level}>{}</h{level}>", spans_html(block)))
                }
                BlockKind::Quote => {
                    html.push_str(&"<blockquote>".repeat(usize::from(block.depth) + 1));
                    html.push_str(&format!("<p>{}</p>", spans_html(block)));
                    html.push_str(&"</blockquote>".repeat(usize::from(block.depth) + 1));
                }
                _ => html.push_str(&format!("<p>{}</p>", spans_html(block))),
            }
            index += 1;
        }
        html
    }
}

fn is_list(kind: &BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Bullet | BlockKind::Ordered | BlockKind::Task { .. }
    )
}

fn render_list(blocks: &[Block], index: &mut usize, depth: u8, html: &mut String) {
    let ordered = blocks[*index].kind == BlockKind::Ordered;
    let tag = if ordered { "ol" } else { "ul" };
    html.push_str(&format!("<{tag}>"));
    while let Some(block) = blocks.get(*index) {
        if !is_list(&block.kind)
            || block.depth < depth
            || (block.depth == depth && (block.kind == BlockKind::Ordered) != ordered)
        {
            break;
        }
        if let BlockKind::Task { checked } = block.kind {
            html.push_str(&format!(
                "<li data-type=\"taskItem\" data-checked=\"{checked}\"><input type=\"checkbox\"{}>",
                if checked { " checked" } else { "" }
            ));
        } else {
            html.push_str("<li>");
        }
        html.push_str(&spans_html(block));
        *index += 1;
        while let Some(child) = blocks.get(*index) {
            if !is_list(&child.kind) || child.depth <= depth {
                break;
            }
            render_list(blocks, index, child.depth, html);
        }
        html.push_str("</li>");
    }
    html.push_str(&format!("</{tag}>"));
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn spans_html(block: &Block) -> String {
    block
        .spans
        .iter()
        .map(|span| {
            let mut text = escape(&span.text);
            for (active, tag) in [
                (span.marks.code, "code"),
                (span.marks.bold, "strong"),
                (span.marks.italic, "em"),
                (span.marks.strikethrough, "s"),
                (span.marks.underline, "u"),
            ] {
                if active {
                    text = format!("<{tag}>{text}</{tag}>");
                }
            }
            if let Some(link) = &span.link {
                text = format!("<a href=\"{}\">{text}</a>", escape(link));
            }
            text
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_semantic_styles_nested_lists_and_code_without_source_whitespace() {
        let doc = Document::from_html(
            "<h2>Title</h2>\n<p>Hello <strong>bold</strong> <a href='https://example.com?a=1&amp;b=2'><em>link</em></a></p><ul><li>Parent<ul><li>Child</li></ul></li><li><input type='checkbox' checked>Done</li></ul><pre><code class='language-rust'>let x = 1;\n  x</code></pre>",
        );
        assert_eq!(doc.blocks[0].kind, BlockKind::Heading(2));
        assert_eq!(doc.blocks[1].text(), "Hello bold link");
        assert!(doc.blocks[1].spans[1].marks.bold);
        assert_eq!(doc.blocks[3].depth, 1);
        assert_eq!(doc.blocks[4].kind, BlockKind::Task { checked: true });
        assert_eq!(doc.blocks.last().unwrap().text(), "  x");
        assert_eq!(Document::from_html(&doc.to_html()), doc);
    }

    #[test]
    fn nested_quotes_empty_paragraphs_and_unsupported_content_are_not_lost() {
        let doc = Document::from_html(
            "<blockquote><p>Outer</p><blockquote>Inner</blockquote></blockquote><p></p><p>A<br>B</p><script>secret()</script><img alt='diagram' src='image.png'><p><span style='font-weight:700;text-decoration:underline'>style</span></p>",
        );
        assert_eq!(doc.blocks[1].depth, 1);
        assert!(doc.blocks[2].is_empty());
        assert!(!doc.plain_text().contains("secret"));
        assert!(doc.plain_text().contains("![diagram](image.png)"));
        assert!(doc.blocks.last().unwrap().spans[0].marks.underline);
        assert_eq!(Document::from_html(&doc.to_html()), doc);
    }

    #[test]
    fn wrapped_task_content_imports_once_with_checked_state_and_nested_children() {
        let doc = Document::from_html(
            "<ul><li data-type='taskItem' data-checked='true'><label><input type='checkbox' checked></label><div><p>Done</p><ul><li data-type='taskItem' data-checked='false'><label><input type='checkbox'></label><div><p>Child</p></div></li></ul></div></li></ul>",
        );
        assert_eq!(doc.plain_text(), "Done\nChild");
        assert_eq!(doc.blocks.len(), 2);
        assert_eq!(doc.blocks[0].kind, BlockKind::Task { checked: true });
        assert_eq!(doc.blocks[1].kind, BlockKind::Task { checked: false });
        assert_eq!(doc.blocks[1].depth, 1);
        let ordinary = Document::from_html(
            "<ul><li><label><input type='checkbox' checked></label><div><p>Done</p></div></li></ul>",
        );
        assert_eq!(ordinary.blocks.len(), 1);
        assert_eq!(ordinary.blocks[0].kind, BlockKind::Task { checked: true });
    }

    #[test]
    fn html_preserves_explicit_line_breaks_and_trailing_code_newlines() {
        for (html, text) in [
            ("<p>A<br>B<br></p>", "A\nB\n"),
            ("<p><br>A</p>", "\nA"),
            ("<p><br></p>", ""),
            ("<p><br><br></p>", "\n\n"),
        ] {
            assert_eq!(Document::from_html(html).plain_text(), text, "{html}");
        }
        let document = Document::from_markdown("```rust\nline\n\n```");
        assert_eq!(Document::from_html(&document.to_html()), document);
    }

    #[test]
    fn empty_list_parents_keep_their_children_nested() {
        let document = Document::from_markdown("- \n    - child\n- [x] \n    - [ ] child");
        assert_eq!(Document::from_html(&document.to_html()), document);
    }

    #[test]
    fn exported_html_escapes_user_text_and_links() {
        let doc = Document::from_markdown("**<&>** [link](https://example.com?q=1&b=2)");
        assert!(doc.to_html().contains("&lt;&amp;&gt;"));
        assert!(doc.to_html().contains("q=1&amp;b=2"));
        assert_eq!(Document::from_html(&doc.to_html()), doc);
    }
}
