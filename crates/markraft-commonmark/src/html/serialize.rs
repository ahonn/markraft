//! Writing a document tree as HTML — a clipboard's rich flavour.
//!
//! The shape mirrors [`crate::serialize`]: a rule per node type and a rule per
//! mark type, keyed by schema type name, so a consumer that added a node type
//! adds a rule for it and nothing else changes.
//!
//! # What the output is for
//!
//! Another application pastes this. It is therefore ordinary HTML — `<p>`,
//! `<h1>`, `<ul>`, `<strong>` — and not a private encoding. The few things
//! CommonMark records that HTML has no element for travel as `data-`
//! attributes, which every other reader ignores and [`HtmlParser`] reads back,
//! so a copy from Markraft into Markraft loses nothing:
//!
//! | attribute | on | what it carries |
//! |---|---|---|
//! | `data-type="taskItem"`, `data-checked` | `<li>` | a GFM task item |
//! | `data-tight` | `<ul>`, `<ol>` | whether the list renders without `<p>` |
//! | `data-bullet` | `<ul>` | the bullet character |
//! | `data-delimiter` | `<ol>` | `.` or `)` |
//! | `data-fence`, `data-fence-length` | `<pre>` | the code fence's spelling |
//! | `data-type="rawBlock"` | `<pre>` | source the model does not interpret |
//!
//! Each is written only when it differs from what the reader would assume, so
//! the common shapes stay plain.
//!
//! A table needs no private attribute at all: its first row is its header row,
//! which is what `<thead>` says, and its alignments are the `align` attribute
//! every reader already understands — written only where a column is aligned.
//!
//! # Known losses
//!
//! HTML collapses whitespace, so a run of spaces or a line ending *inside*
//! inline content comes back as one space. Text inside `<pre>` is exempt and
//! survives byte for byte. Everything else — structure, attributes and marks —
//! round trips exactly.
//!
//! A paragraph holding nothing but a hard break has no spelling either: a lone
//! `<br>` is how an editor writes an *empty* paragraph, which is what it reads
//! back as. CommonMark cannot write that shape down either.

use std::collections::HashMap;
use std::sync::Arc;

use markraft_core::{Mark, MarkTypeId, Node, NodeTypeId, Schema, Slice};

use crate::schema as md;
use crate::table::{Alignment, alignments_of};

/// Writes one node, and whatever of its content the rule decides to visit.
///
/// The second argument is the node's parent, which an empty block needs to know
/// whether it is all that parent holds.
pub type HtmlNodeRule = Arc<dyn Fn(&mut HtmlState<'_>, &Node, Option<&Node>) + Send + Sync>;
/// The opening and closing tags one mark wraps its content in.
pub type HtmlMarkRule = Arc<dyn Fn(&Mark) -> (String, String) + Send + Sync>;
/// Node rules keyed by schema type name.
pub type HtmlNodeRules = HashMap<String, HtmlNodeRule>;
/// Mark rules keyed by schema type name.
pub type HtmlMarkRules = HashMap<String, HtmlMarkRule>;

/// `text` with the three characters that would otherwise start markup escaped.
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

/// `value` as an attribute value, quoted with `"`.
pub fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
    out
}

/// Writes documents of one schema as HTML.
#[derive(Clone)]
pub struct HtmlSerializer {
    schema: Schema,
    nodes: Vec<Option<HtmlNodeRule>>,
    marks: Vec<Option<HtmlMarkRule>>,
}

impl HtmlSerializer {
    /// A serialiser binding name-keyed rules to `schema`.
    ///
    /// Rules naming a type the schema does not declare are ignored, and a type
    /// with no rule falls back to writing its content, which keeps text rather
    /// than losing it.
    pub fn new(schema: Schema, nodes: HtmlNodeRules, marks: HtmlMarkRules) -> HtmlSerializer {
        let mut node_rules = vec![None; schema.node_types().len()];
        for (name, rule) in nodes {
            if let Some(id) = schema.node_id(&name) {
                node_rules[id.index()] = Some(rule);
            }
        }
        let mut mark_rules = vec![None; schema.mark_types().len()];
        for (name, rule) in marks {
            if let Some(id) = schema.mark_id(&name) {
                mark_rules[id.index()] = Some(rule);
            }
        }
        HtmlSerializer {
            schema,
            nodes: node_rules,
            marks: mark_rules,
        }
    }

    /// A serialiser with the CommonMark/GFM rule tables.
    pub fn commonmark(schema: &Schema) -> HtmlSerializer {
        HtmlSerializer::new(
            schema.clone(),
            commonmark_html_node_rules(),
            commonmark_html_mark_rules(),
        )
    }

    /// The schema this serialiser writes.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The rule for a node type, if any.
    pub fn node_rule(&self, ty: NodeTypeId) -> Option<&HtmlNodeRule> {
        self.nodes.get(ty.index()).and_then(Option::as_ref)
    }

    /// The rule for a mark type, if any.
    pub fn mark_rule(&self, ty: MarkTypeId) -> Option<&HtmlMarkRule> {
        self.marks.get(ty.index()).and_then(Option::as_ref)
    }

    /// Write `doc` as HTML, with no wrapper element around it.
    pub fn serialize(&self, doc: &Node) -> String {
        let mut state = HtmlState {
            serializer: self,
            out: String::new(),
        };
        state.render_content(doc);
        state.out
    }

    /// Write a copied [`Slice`] as HTML.
    ///
    /// The slice is closed first, exactly as
    /// [`MarkdownSerializer::serialize_fragment`](crate::MarkdownSerializer::serialize_fragment)
    /// closes it, so a cut taken inside one paragraph writes as that text alone.
    pub fn serialize_fragment(&self, slice: &Slice) -> String {
        match crate::fragment::close(&self.schema, slice) {
            Some(doc) => self.serialize(&doc),
            None => String::new(),
        }
    }
}

/// The output being built, and everything a rule needs to add to it.
pub struct HtmlState<'a> {
    serializer: &'a HtmlSerializer,
    out: String,
}

impl HtmlState<'_> {
    /// The schema being written.
    pub fn schema(&self) -> &Schema {
        &self.serializer.schema
    }

    /// The output so far.
    pub fn out(&self) -> &str {
        &self.out
    }

    /// Append markup, taken exactly as written.
    pub fn write(&mut self, raw: &str) {
        self.out.push_str(raw);
    }

    /// Append text, escaped.
    pub fn text(&mut self, text: &str) {
        let escaped = escape_text(text);
        self.out.push_str(&escaped);
    }

    /// Append ` name="value"`, escaped, unless `value` is empty.
    pub fn attr(&mut self, name: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        let written = format!(" {name}=\"{}\"", escape_attr(value));
        self.out.push_str(&written);
    }

    /// Write one node through its rule, or its content when it has none.
    pub fn render(&mut self, node: &Node, parent: Option<&Node>) {
        match self.serializer.node_rule(node.type_id()).cloned() {
            Some(rule) => rule(self, node, parent),
            None if node.is_container() => self.render_content(node),
            None => {}
        }
    }

    /// Write `parent`'s children as blocks, one per line.
    pub fn render_content(&mut self, parent: &Node) {
        let mut first = true;
        for child in parent.children() {
            let before = self.out.len();
            if !first {
                self.write("\n");
            }
            self.render(child, Some(parent));
            // A block that wrote nothing takes its separator back with it.
            if self.out.len() == before + usize::from(!first) {
                self.out.truncate(before);
            } else {
                first = false;
            }
        }
    }

    /// Write `parent`'s children as inline content, opening each mark once for
    /// the whole run that carries it.
    pub fn render_inline(&mut self, parent: &Node) {
        let mut open: Vec<Mark> = Vec::new();
        for child in parent.children() {
            let marks = child.marks();
            // Both lists are sorted by rank, so the marks that stay open are the
            // longest prefix the two share.
            let keep = open
                .iter()
                .zip(marks.iter())
                .take_while(|(a, b)| a == b)
                .count();
            for mark in open.split_off(keep).into_iter().rev() {
                let close = self.mark_tags(&mark).1;
                self.write(&close);
            }
            for mark in marks.iter().skip(keep) {
                let open_tag = self.mark_tags(mark).0;
                self.write(&open_tag);
                open.push(mark.clone());
            }
            self.render(child, Some(parent));
        }
        for mark in open.into_iter().rev() {
            let close = self.mark_tags(&mark).1;
            self.write(&close);
        }
    }

    fn mark_tags(&self, mark: &Mark) -> (String, String) {
        match self.serializer.mark_rule(mark.ty) {
            Some(rule) => rule(mark),
            None => (String::new(), String::new()),
        }
    }
}

fn rule(
    f: impl Fn(&mut HtmlState<'_>, &Node, Option<&Node>) + Send + Sync + 'static,
) -> HtmlNodeRule {
    Arc::new(f)
}

fn attr_str<'a>(node: &'a Node, name: &str, default: &'a str) -> &'a str {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
}

fn attr_int(node: &Node, name: &str, default: i64) -> i64 {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_int())
        .unwrap_or(default)
}

fn attr_bool(node: &Node, name: &str, default: bool) -> bool {
    node.attrs()
        .get(name)
        .and_then(|value| value.as_bool())
        .unwrap_or(default)
}

/// The concatenated text of a node's children, which is what a code block holds.
fn text_content(node: &Node) -> String {
    node.children().filter_map(|child| child.text()).collect()
}

/// Write `text` inside a `<pre>`, with the closing line ending the reader
/// strips again.
///
/// That ending is the block's, not the content's, which is what keeps `"a"` and
/// `"a\n"` two different blocks.
fn preformatted(state: &mut HtmlState<'_>, text: &str) {
    state.text(text);
    state.write("\n");
}

/// The CommonMark/GFM HTML node rules, keyed by schema type name.
pub fn commonmark_html_node_rules() -> HtmlNodeRules {
    let mut rules = HtmlNodeRules::new();
    rules.insert(
        md::DOC.to_string(),
        rule(|state, node, _| state.render_content(node)),
    );
    rules.insert(
        md::PARAGRAPH.to_string(),
        rule(|state, node, parent| {
            if node.content_size() == 0 {
                // A lone `<br>` is how an editor writes an empty paragraph, and
                // what this codec's reader gives back for one — unless the
                // paragraph is all its parent holds, in which case it *is* the
                // empty container, which writes as nothing and is filled back
                // in on the way home.
                if parent.is_none_or(|parent| parent.child_count() > 1) {
                    state.write("<p><br></p>");
                }
                return;
            }
            state.write("<p>");
            state.render_inline(node);
            state.write("</p>");
        }),
    );
    rules.insert(
        md::HEADING.to_string(),
        rule(|state, node, _| {
            let level = attr_int(node, "level", 1).clamp(1, 6);
            state.write(&format!("<h{level}>"));
            state.render_inline(node);
            state.write(&format!("</h{level}>"));
        }),
    );
    rules.insert(
        md::BLOCKQUOTE.to_string(),
        rule(|state, node, _| {
            state.write("<blockquote>\n");
            state.render_content(node);
            state.write("\n</blockquote>");
        }),
    );
    rules.insert(md::CODE_BLOCK.to_string(), rule(code_block));
    rules.insert(md::BULLET_LIST.to_string(), rule(bullet_list));
    rules.insert(md::ORDERED_LIST.to_string(), rule(ordered_list));
    rules.insert(
        md::LIST_ITEM.to_string(),
        rule(|state, node, _| {
            state.write("<li>");
            state.render_content(node);
            state.write("</li>");
        }),
    );
    rules.insert(md::TASK_ITEM.to_string(), rule(task_item));
    rules.insert(md::TABLE.to_string(), rule(table));
    // A row or a cell only reaches a rule of its own when something writes one
    // outside its table; inside one the table's rule places every tag, because
    // a cell's tag and its `align` depend on where it sits.
    rules.insert(
        md::TABLE_ROW.to_string(),
        rule(|state, node, _| {
            state.write("<tr>");
            for cell in node.children() {
                state.render(cell, Some(node));
            }
            state.write("</tr>");
        }),
    );
    rules.insert(
        md::TABLE_CELL.to_string(),
        rule(|state, node, _| {
            state.write("<td>");
            state.render_inline(node);
            state.write("</td>");
        }),
    );
    rules.insert(
        md::HORIZONTAL_RULE.to_string(),
        rule(|state, _, _| state.write("<hr>")),
    );
    rules.insert(
        md::RAW_BLOCK.to_string(),
        rule(|state, node, _| {
            // A line ending straight after `<pre>` is one an HTML parser drops,
            // so text that starts with one is written with an extra.
            state.write("<pre data-type=\"rawBlock\">");
            let source = text_content(node);
            if source.starts_with('\n') {
                state.write("\n");
            }
            preformatted(state, &source);
            state.write("</pre>");
        }),
    );
    rules.insert(
        md::TEXT.to_string(),
        rule(|state, node, _| state.text(node.text().unwrap_or_default())),
    );
    rules.insert(
        md::IMAGE.to_string(),
        rule(|state, node, _| {
            state.write("<img");
            state.attr("src", attr_str(node, "src", ""));
            state.attr("alt", attr_str(node, "alt", ""));
            state.attr("title", attr_str(node, "title", ""));
            state.write(">");
        }),
    );
    rules.insert(
        md::WIKI_LINK.to_string(),
        rule(|state, node, _| {
            // A reader outside this editor sees an ordinary link to the target;
            // a paste back into it takes the atom's parts from the data
            // attributes rather than reading a source spelling out of the
            // rendered label, which is only the alias when there is one.
            let target = attr_str(node, "target", "");
            let alias = attr_str(node, "alias", "");
            state.write("<a");
            state.attr("href", target.trim());
            state.attr("data-type", "wikiLink");
            state.attr("data-target", target);
            state.attr("data-alias", alias);
            if attr_bool(node, "embed", false) {
                state.attr("data-embed", "true");
            }
            state.write(">");
            state.text(if alias.is_empty() { target } else { alias });
            state.write("</a>");
        }),
    );
    rules.insert(
        md::SOFT_BREAK.to_string(),
        rule(|state, _, _| state.write("<span data-type=\"softBreak\"> </span>")),
    );
    rules.insert(
        md::INLINE_SPAN.to_string(),
        rule(|state, node, _| state.render_inline(node)),
    );
    rules.insert(
        md::RAW_INLINE.to_string(),
        rule(|state, node, _| {
            // Keep the source opaque in clipboard HTML. Pasting must not execute
            // or reinterpret a stored HTML primitive as the clipboard's own DOM.
            state.write("<span data-type=\"rawInline\" data-source=\"");
            state.write(&escape_attr(attr_str(node, "source", "")));
            state.write("\"></span>");
        }),
    );
    rules.insert(
        md::HARD_BREAK.to_string(),
        rule(|state, _, _| state.write("<br>")),
    );
    rules
}

fn code_block(state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let language = attr_str(node, "language", "");
    state.write("<pre");
    let fence = attr_str(node, "fence_char", "`");
    if fence != "`" {
        state.attr("data-fence", fence);
    }
    let length = attr_int(node, "fence_length", 3);
    if length != 3 {
        state.attr("data-fence-length", &length.to_string());
    }
    state.write("><code");
    if !language.is_empty() {
        state.attr("class", &format!("language-{language}"));
    }
    state.write(">");
    preformatted(state, &text_content(node));
    state.write("</code></pre>");
}

fn bullet_list(state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    state.write("<ul");
    let bullet = attr_str(node, "bullet_char", "-");
    if bullet != "-" {
        state.attr("data-bullet", bullet);
    }
    if !attr_bool(node, "tight", true) {
        state.attr("data-tight", "false");
    }
    state.write(">\n");
    state.render_content(node);
    state.write("\n</ul>");
}

fn ordered_list(state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    state.write("<ol");
    let start = attr_int(node, "start", 1);
    if start != 1 {
        state.attr("start", &start.to_string());
    }
    let delimiter = attr_str(node, "delimiter", ".");
    if delimiter != "." {
        state.attr("data-delimiter", delimiter);
    }
    if !attr_bool(node, "tight", true) {
        state.attr("data-tight", "false");
    }
    state.write(">\n");
    state.render_content(node);
    state.write("\n</ol>");
}

fn task_item(state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let checked = attr_bool(node, "checked", false);
    state.write("<li data-type=\"taskItem\"");
    state.attr("data-checked", if checked { "true" } else { "false" });
    state.write("><input type=\"checkbox\"");
    if checked {
        state.write(" checked");
    }
    state.write(" disabled>");
    state.render_content(node);
    state.write("</li>");
}

/// `<table><thead><tr><th>…` — the shape every reader knows, with the header
/// row in its `<thead>` and the alignments on the cells that have one.
fn table(state: &mut HtmlState<'_>, node: &Node, _: Option<&Node>) {
    let alignments = alignments_of(node);
    let mut rows = node.children();
    state.write("<table>\n<thead>\n");
    if let Some(header) = rows.next() {
        table_row(state, header, &alignments, "th");
        state.write("\n");
    }
    state.write("</thead>");
    let body: Vec<&Node> = rows.collect();
    if !body.is_empty() {
        state.write("\n<tbody>\n");
        for (index, row) in body.iter().enumerate() {
            if index > 0 {
                state.write("\n");
            }
            table_row(state, row, &alignments, "td");
        }
        state.write("\n</tbody>");
    }
    state.write("\n</table>");
}

fn table_row(state: &mut HtmlState<'_>, row: &Node, alignments: &[Alignment], tag: &str) {
    state.write("<tr>");
    for (index, cell) in row.children().enumerate() {
        let alignment = alignments.get(index).copied().unwrap_or_default();
        state.write(&format!("<{tag}"));
        if alignment != Alignment::None {
            state.attr("align", alignment.name());
        }
        state.write(">");
        state.render_inline(cell);
        state.write(&format!("</{tag}>"));
    }
    state.write("</tr>");
}

fn tags(open: &'static str, close: &'static str) -> HtmlMarkRule {
    Arc::new(move |_| (open.to_string(), close.to_string()))
}

/// The CommonMark/GFM HTML mark rules, keyed by schema type name.
pub fn commonmark_html_mark_rules() -> HtmlMarkRules {
    let mut rules = HtmlMarkRules::new();
    rules.insert(
        md::LINK.to_string(),
        Arc::new(|mark: &Mark| {
            let value = |name: &str| {
                mark.attrs
                    .get(name)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let mut open = format!("<a href=\"{}\"", escape_attr(&value("href")));
            let title = value("title");
            if !title.is_empty() {
                open.push_str(&format!(" title=\"{}\"", escape_attr(&title)));
            }
            open.push('>');
            (open, "</a>".to_string())
        }),
    );
    rules.insert(md::UNDERLINE.to_string(), tags("<u>", "</u>"));
    rules.insert(md::STRIKETHROUGH.to_string(), tags("<del>", "</del>"));
    rules.insert(md::STRONG.to_string(), tags("<strong>", "</strong>"));
    rules.insert(md::EM.to_string(), tags("<em>", "</em>"));
    rules.insert(md::CODE.to_string(), tags("<code>", "</code>"));
    rules
}

/// An HTML serialiser for `schema` with the CommonMark/GFM rules.
pub fn commonmark_html_serializer(schema: &Schema) -> HtmlSerializer {
    HtmlSerializer::commonmark(schema)
}
