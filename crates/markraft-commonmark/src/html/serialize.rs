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
//! attributes, which every other reader ignores and [`HtmlParser`](crate::html::HtmlParser) reads back,
//! so a copy from Markraft into Markraft loses nothing:
//!
//! | attribute | on | what it carries |
//! |---|---|---|
//! | `data-type="taskItem"`, `data-checked` | `<li>` | a GFM task item |
//! | `data-tight` | `<ul>`, `<ol>` | whether the list renders without `<p>` |
//! | `data-bullet` | `<ul>` | the bullet character |
//! | `data-delimiter` | `<ol>` | `.` or `)`, where it is not the house style's |
//! | `data-same-ordinal` | `<ol>` | every item written with the first one's number |
//! | `data-fence`, `data-fence-length` | `<pre>` | the code fence's spelling |
//! | `data-type="rawBlock"` | `<pre>` | source the model does not interpret |
//! | `data-callout`, `data-callout-fold`, `data-callout-title` | `<blockquote>` | a callout's marker |
//! | `data-type="wikiLink"`, `data-target`, `data-alias`, `data-embed` | `<a>` | a wiki link's parts |
//! | `data-type="emoji"`, `data-code` | `<span>` | an emoji's shortcode |
//! | `data-type="rawInline"`, `data-source` | `<span>` | an inline HTML tag as written |
//! | `data-type="softBreak"` | `<span>` | a line ending of the source |
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
//! HTML collapses whitespace, so a run of spaces *inside* inline content comes
//! back as one space. Text inside `<pre>` is exempt and
//! survives byte for byte. Everything else — structure, attributes and marks —
//! round trips exactly.
//!
//! A paragraph holding nothing but a hard break has no spelling either: a lone
//! `<br>` is how an editor writes an *empty* paragraph, which is what it reads
//! back as. CommonMark cannot write that shape down either.

use std::collections::HashMap;
use std::sync::Arc;

use markraft_core::kind::{SYNTAX_DISPLAY_ATTR, SYNTAX_SPAN_ATTR};
use markraft_core::{Mark, MarkTypeId, Node, NodeTypeId, Schema, Slice};

use crate::house::HouseStyleHandle;
use crate::schema as md;
use crate::table::{Alignment, alignments_of};

/// Writes one node, and whatever of its content the rule decides to visit.
///
/// The second argument is the node's parent, which an empty block needs to know
/// whether it is all that parent holds.
pub type HtmlNodeRule = Arc<dyn Fn(&mut HtmlState<'_>, &Node, Option<&Node>) + Send + Sync>;
/// The opening and closing tags one mark wraps its content in.
pub type HtmlMarkRule = Arc<dyn Fn(&Mark) -> (String, String) + Send + Sync>;
/// Writes a whole formula-like run of inline content in one go.
///
/// The run is the stretch of inline children carrying one mark, delimiters
/// included. The rule gets the mark, the run's text with its concealed spelling
/// left out and line breaks as `\n`, and the document position where that text
/// begins (`None` inside a tree written with [`HtmlState::render_detached`]);
/// what it writes stands in for the entire run.
pub type HtmlAtomRule = Arc<dyn Fn(&mut HtmlState<'_>, &Mark, &str, Option<usize>) + Send + Sync>;
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
    atoms: Vec<Option<HtmlAtomRule>>,
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
        let atoms = vec![None; schema.mark_types().len()];
        HtmlSerializer {
            schema,
            nodes: node_rules,
            marks: mark_rules,
            atoms,
        }
    }

    /// Replace the rule for one node type, keyed by schema type name. A name
    /// the schema does not declare is ignored.
    pub fn with_node_rule(mut self, name: &str, rule: HtmlNodeRule) -> HtmlSerializer {
        if let Some(id) = self.schema.node_id(name) {
            self.nodes[id.index()] = Some(rule);
        }
        self
    }

    /// Write every run carrying the mark named `name` through `rule` instead of
    /// wrapping it in tags (see [`HtmlAtomRule`]). A name the schema does not
    /// declare is ignored.
    pub fn with_atom_rule(mut self, name: &str, rule: HtmlAtomRule) -> HtmlSerializer {
        if let Some(id) = self.schema.mark_id(name) {
            self.marks[id.index()] = None;
            self.atoms[id.index()] = Some(rule);
        }
        self
    }

    /// A serialiser with the CommonMark/GFM rule tables, writing in `house`'s
    /// style where the HTML has to say which it is.
    pub fn commonmark(schema: &Schema, house: &HouseStyleHandle) -> HtmlSerializer {
        HtmlSerializer::new(
            schema.clone(),
            commonmark_html_node_rules(house),
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
            at: None,
            detached: false,
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
    // Where the node being rendered starts; `None` while writing the root's
    // content, which starts at 0.
    at: Option<usize>,
    // Whether the nodes being written belong to some other tree than the one
    // handed to `serialize`, so positions say nothing about that document.
    detached: bool,
}

impl HtmlState<'_> {
    /// The document position where the node being rendered starts, as the
    /// document handed to [`HtmlSerializer::serialize`] counts positions, or
    /// `None` inside a tree written with [`Self::render_detached`].
    pub fn position(&self) -> Option<usize> {
        (!self.detached).then(|| self.at.unwrap_or(0))
    }

    /// Run `f` with the `index`th child of `parent` — the node being rendered —
    /// as the node being rendered, so positions inside it count from where it
    /// starts. A rule that walks its node's children itself, rather than
    /// through [`Self::render_content`] or [`Self::render_inline`], goes through
    /// this for each child it writes.
    pub fn within_child<R>(
        &mut self,
        parent: &Node,
        index: usize,
        f: impl FnOnce(&mut Self, &Node) -> R,
    ) -> R {
        let child = parent.child(index);
        let start = self.content_start()
            + parent
                .children()
                .take(index)
                .map(Node::node_size)
                .sum::<usize>();
        let at = self.at.replace(start);
        let result = f(self, child);
        self.at = at;
        result
    }

    /// Write the `index`th child of `parent` through its rule, with its position.
    pub fn render_child(&mut self, parent: &Node, index: usize) {
        self.within_child(parent, index, |state, child| {
            state.render(child, Some(parent))
        });
    }

    /// Write `node`, from a tree that is not the document's — such as what an
    /// HTML block renders to — through the same rules, without positions.
    pub fn render_detached(&mut self, node: &Node, parent: Option<&Node>) {
        let detached = std::mem::replace(&mut self.detached, true);
        self.render(node, parent);
        self.detached = detached;
    }

    fn content_start(&self) -> usize {
        self.at.map_or(0, |at| at + 1)
    }

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
        let parent_at = self.at;
        let mut child_at = self.content_start();
        let mut first = true;
        for child in parent.children() {
            let before = self.out.len();
            if !first {
                self.write("\n");
            }
            self.at = Some(child_at);
            self.render(child, Some(parent));
            self.at = parent_at;
            child_at += child.node_size();
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
    ///
    /// The text is Markdown source, so what it spells rather than says — the
    /// runs carrying [`crate::schema::SYNTAX`] — is left out, and an entity is
    /// written as the character it displays. A line break is `<br>` where the
    /// text makes it a hard break, and otherwise a soft break the importer
    /// reads back as one. Style marks that
    /// are already open are kept as a prefix even when rank order would
    /// otherwise close them — nested `*a **b** c*` must write
    /// `<em>a <strong>b</strong> c</em>`, not reopen `<em>` around `b`. A run
    /// carrying a mark with an [`HtmlAtomRule`] is written by that rule alone.
    pub fn render_inline(&mut self, parent: &Node) {
        let mut open: Vec<Mark> = Vec::new();
        let schema = self.schema();
        let syntax = schema.mark_id(crate::schema::SYNTAX);
        let line_break = schema.node_id(crate::schema::LINE_BREAK);
        let hard = hard_break_indexes(schema, parent);
        let children: Vec<&Node> = parent.children().collect();
        let parent_at = self.at;
        let mut child_at = self.content_start();
        let mut index = 0;
        while index < children.len() {
            let child = children[index];
            let marks = self.ordered_marks(&open, child.marks());
            let keep = open
                .iter()
                .zip(marks.iter())
                .take_while(|(a, b)| a == b)
                .count();
            for mark in open.split_off(keep).into_iter().rev() {
                let close = self.mark_tags(&mark).1;
                self.write(&close);
            }
            for mark in marks.into_iter().skip(keep) {
                let open_tag = self.mark_tags(&mark).0;
                self.write(&open_tag);
                open.push(mark);
            }
            if let Some((mark, rule)) = self.atom_of(child) {
                let end = atom_end(&children, index, &mark, syntax);
                let (text, start) = atom_text(&children[index..end], child_at, syntax, line_break);
                let start = (!self.detached).then_some(start);
                rule(self, &mark, &text, start);
                child_at += children[index..end]
                    .iter()
                    .map(|child| child.node_size())
                    .sum::<usize>();
                index = end;
                continue;
            }
            let display = syntax.and_then(|ty| child.marks().get(ty)).map(|mark| {
                mark.attrs
                    .get(SYNTAX_DISPLAY_ATTR)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string()
            });
            // A concealed run still opens and closes the marks around it, so
            // a link that is all spelling is still a link.
            if let Some(display) = display {
                self.text(&display);
            } else if Some(child.type_id()) == line_break {
                self.write(if hard.contains(&index) {
                    "<br>"
                } else {
                    "<span data-type=\"softBreak\"> </span>"
                });
            } else {
                self.at = Some(child_at);
                self.render(child, Some(parent));
                self.at = parent_at;
            }
            child_at += child.node_size();
            index += 1;
        }
        for mark in open.into_iter().rev() {
            let close = self.mark_tags(&mark).1;
            self.write(&close);
        }
    }

    /// The first of `child`'s marks written by an atom rule, with that rule.
    fn atom_of(&self, child: &Node) -> Option<(Mark, HtmlAtomRule)> {
        child.marks().iter().find_map(|mark| {
            self.serializer
                .atoms
                .get(mark.ty.index())
                .and_then(Option::as_ref)
                .map(|rule| (mark.clone(), rule.clone()))
        })
    }

    /// Reorder `marks` so every mark still in `open` stays as a prefix.
    fn ordered_marks(&self, open: &[Mark], marks: &markraft_core::MarkSet) -> Vec<Mark> {
        let mut pending: Vec<Mark> = marks
            .iter()
            .filter(|mark| self.serializer.mark_rule(mark.ty).is_some())
            .cloned()
            .collect();
        let mut ordered = Vec::with_capacity(pending.len());
        for kept in open {
            let Some(index) = pending.iter().position(|mark| mark == kept) else {
                break;
            };
            ordered.push(pending.remove(index));
        }
        ordered.append(&mut pending);
        ordered
    }

    fn mark_tags(&self, mark: &Mark) -> (String, String) {
        match self.serializer.mark_rule(mark.ty) {
            Some(rule) => rule(mark),
            None => (String::new(), String::new()),
        }
    }
}

/// Where the atom run starting at `children[start]` ends: the children that
/// carry `mark`, cut where a second pair of delimiters opens. Two formulas can
/// touch (`$a$$b$`) with equal marks; their delimiters' span numbers differ.
fn atom_end(children: &[&Node], start: usize, mark: &Mark, syntax: Option<MarkTypeId>) -> usize {
    let span_of = |child: &Node| {
        syntax
            .and_then(|ty| child.marks().get(ty))
            .and_then(|syntax| syntax.attrs.get(SYNTAX_SPAN_ATTR))
            .and_then(|value| value.as_int())
    };
    let opening = span_of(children[start]);
    let mut end = start + 1;
    while let Some(child) = children.get(end) {
        if child.marks().get(mark.ty) != Some(mark) {
            break;
        }
        if let Some(span) = span_of(child)
            && opening.is_some_and(|opening| opening != span)
        {
            break;
        }
        end += 1;
    }
    end
}

/// The text an atom run says, with its concealed spelling left out and line
/// breaks as `\n`, and the position where that text begins.
fn atom_text(
    run: &[&Node],
    run_at: usize,
    syntax: Option<MarkTypeId>,
    line_break: Option<NodeTypeId>,
) -> (String, usize) {
    let mut text = String::new();
    let mut start = None;
    let mut at = run_at;
    for child in run {
        let concealed = syntax.is_some_and(|ty| child.marks().get(ty).is_some());
        if !concealed {
            start.get_or_insert(at);
            if let Some(value) = child.text() {
                text.push_str(value);
            } else if Some(child.type_id()) == line_break {
                text.push('\n');
            }
        }
        at += child.node_size();
    }
    (text, start.unwrap_or(run_at))
}

fn hard_break_indexes(schema: &Schema, parent: &Node) -> Vec<usize> {
    let Some(kind) = crate::textblock::block_kind(schema, parent.type_id()) else {
        return Vec::new();
    };
    let items = crate::textblock::Items::from_nodes(schema, parent.children());
    let derived = crate::derive::derive(kind, &items.text(), &crate::derive::DeriveContext::new());
    if derived.hard_breaks.is_empty() {
        return Vec::new();
    }
    // Each child is one item except text, which is one per character.
    let mut out = Vec::new();
    let mut offset = 0;
    for (index, child) in parent.children().enumerate() {
        if derived.hard_breaks.contains(&offset) && child.text().is_none() {
            out.push(index);
        }
        offset += child.text().map_or(1, |text| text.chars().count());
    }
    out
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

/// The CommonMark/GFM HTML node rules, keyed by schema type name. An ordered
/// list in `house`'s delimiter is written without naming it, as the reader
/// takes one without a name for.
pub fn commonmark_html_node_rules(house: &HouseStyleHandle) -> HtmlNodeRules {
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
        md::FOOTNOTE_DEFINITION.to_string(),
        rule(|state, node, _| {
            state.write("<div class=\"footnote\"");
            state.attr(
                "id",
                &format!("fn-{}", attr_str(node, md::FOOTNOTE_LABEL_ATTR, "")),
            );
            state.write(">\n");
            state.render_content(node);
            state.write("\n</div>");
        }),
    );
    rules.insert(
        md::BLOCKQUOTE.to_string(),
        rule(|state, node, _| {
            // A callout's marker is not content, so it travels in data
            // attributes rather than as a line of text another reader would
            // then show twice.
            state.write("<blockquote");
            state.attr("data-callout", attr_str(node, "callout", ""));
            state.attr("data-callout-fold", attr_str(node, "fold", ""));
            state.attr("data-callout-title", attr_str(node, "title", ""));
            state.write(">\n");
            state.render_content(node);
            state.write("\n</blockquote>");
        }),
    );
    rules.insert(md::CODE_BLOCK.to_string(), rule(code_block));
    rules.insert(md::BULLET_LIST.to_string(), rule(bullet_list));
    let ordered_house = house.clone();
    rules.insert(
        md::ORDERED_LIST.to_string(),
        rule(move |state, node, _| {
            ordered_list(state, node, ordered_house.get().ordered_delimiter)
        }),
    );
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
            for index in 0..node.child_count() {
                state.render_child(node, index);
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
        md::EMOJI.to_string(),
        rule(|state, node, _| {
            // A reader outside this editor sees the emoji; a paste back into
            // it takes the shortcode from the data attribute.
            let code = attr_str(node, "code", "");
            state.write("<span");
            state.attr("data-type", "emoji");
            state.attr("data-code", code);
            state.write(">");
            match crate::shortcode::emoji(code) {
                Some(emoji) => state.text(emoji),
                None => state.text(&crate::shortcode::spelling(code)),
            }
            state.write("</span>");
        }),
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
        md::LINE_BREAK.to_string(),
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

fn ordered_list(state: &mut HtmlState<'_>, node: &Node, house_delimiter: char) {
    state.write("<ol");
    let start = attr_int(node, "start", 1);
    if start != 1 {
        state.attr("start", &start.to_string());
    }
    // The reader takes an `<ol>` without one for a list in the house style's
    // delimiter, since a list pasted from anywhere else is one the editor
    // makes; see `commonmark_html_rules`.
    let delimiter = attr_str(node, "delimiter", ".");
    if delimiter != house_delimiter.to_string() {
        state.attr("data-delimiter", delimiter);
    }
    if !attr_bool(node, "tight", true) {
        state.attr("data-tight", "false");
    }
    if attr_bool(node, "same_ordinal", false) {
        state.attr("data-same-ordinal", "true");
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
    state.write("<table>\n<thead>\n");
    if node.child_count() > 0 {
        state.within_child(node, 0, |state, header| {
            table_row(state, header, &alignments, "th")
        });
        state.write("\n");
    }
    state.write("</thead>");
    if node.child_count() > 1 {
        state.write("\n<tbody>\n");
        for index in 1..node.child_count() {
            if index > 1 {
                state.write("\n");
            }
            state.within_child(node, index, |state, row| {
                table_row(state, row, &alignments, "td")
            });
        }
        state.write("\n</tbody>");
    }
    state.write("\n</table>");
}

/// One row, written while it is the node being rendered.
fn table_row(state: &mut HtmlState<'_>, row: &Node, alignments: &[Alignment], tag: &str) {
    state.write("<tr>");
    for index in 0..row.child_count() {
        let alignment = alignments.get(index).copied().unwrap_or_default();
        state.write(&format!("<{tag}"));
        if alignment != Alignment::None {
            state.attr("align", alignment.name());
        }
        state.write(">");
        state.within_child(row, index, |state, cell| state.render_inline(cell));
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
    // The label is the text inside, which is what a reader of the HTML sees;
    // the link goes to the definition's `id`.
    rules.insert(
        md::FOOTNOTE_REFERENCE.to_string(),
        Arc::new(|mark: &Mark| {
            let label = mark
                .attrs
                .get(md::FOOTNOTE_LABEL_ATTR)
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            (
                format!("<sup><a href=\"#fn-{}\">", escape_attr(label)),
                "</a></sup>".to_string(),
            )
        }),
    );
    for spec in crate::styles::STYLES {
        rules.insert(spec.mark.to_string(), tags(spec.tags.0, spec.tags.1));
    }
    rules.insert(md::CODE.to_string(), tags("<code>", "</code>"));
    // TeX is shown as the source it is, in the attribute comrak renders math
    // with, so a reader that knows it can typeset it.
    rules.insert(
        md::MATH.to_string(),
        Arc::new(|mark: &Mark| {
            let display = mark
                .attrs
                .get(md::MATH_DISPLAY_ATTR)
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let style = if display { "display" } else { "inline" };
            (
                format!("<code data-math-style=\"{style}\">"),
                "</code>".to_string(),
            )
        }),
    );
    rules
}

/// An HTML serialiser for `schema` with the CommonMark/GFM rules.
pub fn commonmark_html_serializer(schema: &Schema, house: &HouseStyleHandle) -> HtmlSerializer {
    HtmlSerializer::commonmark(schema, house)
}
