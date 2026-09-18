//! The inline half of the parser: marked leaves and semantic inline containers.
//!
//! Two things here need more context than the rule table can carry. Paired
//! inline HTML — `<u>`, `<em>`, `<strong>`, `<del>` — becomes a mark, which
//! means knowing which tags pair up across the whole block before any of them
//! is read. And adjacent text with equal marks has to be one leaf, which means
//! merging as the content is built.

use crate::inline::wrap_mark;
use comrak::nodes::{AstNode, NodeValue};
use markraft_core::{Fragment, Mark, MarkSet, MarkTypeId, Node, Schema};

use super::{ParseError, Walk};
use crate::rules::ParseRule;

impl<'a> Walk<'a> {
    pub(crate) fn inlines(&self, parent: &'a AstNode<'a>) -> Result<Vec<Node>, ParseError> {
        let tags = self.mark_tags(parent);
        let mut builder = Inlines {
            schema: self.schema,
            out: Vec::new(),
            html: Vec::new(),
            tags,
            seen: 0,
        };
        self.walk_inlines(parent, &[], &mut builder)?;
        Ok(builder.out)
    }

    fn walk_inlines(
        &self,
        parent: &'a AstNode<'a>,
        marks: &[Mark],
        builder: &mut Inlines<'_>,
    ) -> Result<(), ParseError> {
        for child in parent.children() {
            let target = self.target(child);
            if let NodeValue::HtmlInline(_) = &*self.value(child) {
                let tag = builder.next_tag();
                match tag {
                    Some((mark, true)) => {
                        let id = self.mark_id(&mark)?;
                        builder.open_html(Mark::new(id));
                        continue;
                    }
                    Some((mark, false)) => {
                        let id = self.mark_id(&mark)?;
                        builder.close_html(id)?;
                        continue;
                    }
                    None => {
                        if let NodeValue::HtmlInline(html) = &*self.value(child) {
                            let ty = self.node_id(crate::schema::RAW_INLINE)?;
                            let node = self.schema.create(
                                ty,
                                markraft_core::attrs! {"source" => html.clone()},
                                MarkSet::empty(),
                                Fragment::empty(),
                            )?;
                            builder.push_node(node, marks);
                            continue;
                        }
                    }
                }
            }
            let rule = self.rules.rule(&self.value(child)).clone();
            match rule {
                ParseRule::Ignore => {}
                ParseRule::Text { text, marks: extra } => {
                    let mut all = marks.to_vec();
                    for name in &extra {
                        all.push(Mark::new(self.mark_id(name)?));
                    }
                    builder.push_text(&text(target), &all);
                }
                ParseRule::Atom { node_type, attrs } => {
                    let ty = self.node_id(&node_type(target))?;
                    let node = self.schema.create(
                        ty,
                        attrs(target),
                        MarkSet::empty(),
                        Fragment::empty(),
                    )?;
                    builder.push_node(node, marks);
                }
                ParseRule::Mark { mark_type, attrs } => {
                    let id = self.mark_id(&mark_type(target))?;
                    let mark =
                        Mark::with_attrs(id, self.schema.build_mark_attrs(id, &attrs(target))?);
                    let children = self.inlines(child)?;
                    for node in wrap_mark(self.schema, mark, children)? {
                        builder.push_node(node, marks);
                    }
                }
                // A block rule or an unknown construct met inline: the source
                // text, which is exactly how a reader sees it.
                ParseRule::Block { .. } | ParseRule::TextBlock { .. } | ParseRule::Raw { .. } => {
                    builder.push_text(&target.source(), marks);
                }
            }
        }
        Ok(())
    }

    /// What each inline HTML tag of this block does, in the order
    /// [`Walk::walk_inlines`] meets them.
    ///
    /// Only tags paired within this Markdown scope become marks. Unpaired or
    /// overlapping tags stay raw primitives; guessing a DOM tree would change
    /// the source semantics.
    fn mark_tags(&self, parent: &'a AstNode<'a>) -> Vec<Tag> {
        let mut tags = Vec::new();
        self.collect_tags(parent, &mut tags);
        let mut open: Vec<usize> = Vec::new();
        for index in 0..tags.len() {
            match tags[index].clone() {
                Tag::Open(_) => open.push(index),
                Tag::Close(name) => {
                    let matching = open
                        .iter()
                        .rposition(|start| tags[*start] == Tag::Open(name.clone()));
                    match matching {
                        Some(position) => {
                            let nested: Vec<usize> = open.drain(position + 1..).collect();
                            open.pop();
                            for start in nested {
                                tags[start] = Tag::Literal;
                            }
                        }
                        None => tags[index] = Tag::Literal,
                    }
                }
                Tag::Literal => {}
            }
        }
        for index in open {
            tags[index] = Tag::Literal;
        }
        tags
    }

    fn collect_tags(&self, parent: &'a AstNode<'a>, tags: &mut Vec<Tag>) {
        for child in parent.children() {
            if let NodeValue::HtmlInline(html) = &*self.value(child) {
                tags.push(match mark_tag(html) {
                    Some((name, true)) => Tag::Open(name),
                    Some((name, false)) => Tag::Close(name),
                    None => Tag::Literal,
                });
            }
        }
    }
}

/// One inline HTML tag's role in a block.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Tag {
    Open(String),
    Close(String),
    Literal,
}

/// The mark an inline HTML tag stands for, and whether it opens it.
fn mark_tag(html: &str) -> Option<(String, bool)> {
    let (body, open) = match html.strip_prefix("</") {
        Some(body) => (body, false),
        None => (html.strip_prefix('<')?, true),
    };
    let name = body.strip_suffix('>')?.trim().to_ascii_lowercase();
    let mark = match name.as_str() {
        "u" => crate::schema::UNDERLINE,
        "em" => crate::schema::EM,
        "strong" => crate::schema::STRONG,
        "del" => crate::schema::STRIKETHROUGH,
        _ => return None,
    };
    Some((mark.to_string(), open))
}

/// Whether an HTML block's literal is nothing but a line break tag.
pub(crate) fn is_break_tag(literal: &str) -> bool {
    let trimmed = literal.trim().to_ascii_lowercase();
    matches!(trimmed.as_str(), "<br>" | "<br/>" | "<br />")
}

/// Accumulates a textblock's inline content, merging adjacent text with equal
/// marks and tracking the marks inline HTML tags have opened.
struct Inlines<'s> {
    schema: &'s Schema,
    out: Vec<Node>,
    html: Vec<(Mark, Vec<Node>)>,
    tags: Vec<Tag>,
    seen: usize,
}

impl Inlines<'_> {
    fn next_tag(&mut self) -> Option<(String, bool)> {
        let tag = self.tags.get(self.seen).cloned();
        self.seen += 1;
        match tag {
            Some(Tag::Open(name)) => Some((name, true)),
            Some(Tag::Close(name)) => Some((name, false)),
            _ => None,
        }
    }

    fn open_html(&mut self, mark: Mark) {
        let before = std::mem::take(&mut self.out);
        self.html.push((mark, before));
    }

    fn close_html(&mut self, ty: MarkTypeId) -> Result<(), ParseError> {
        if self.html.last().is_some_and(|(mark, _)| mark.ty == ty) {
            let children = std::mem::take(&mut self.out);
            let (mark, before) = self.html.pop().expect("matched open tag");
            self.out = before;
            for node in wrap_mark(self.schema, mark, children)? {
                self.push_node(node, &[]);
            }
        }
        Ok(())
    }

    fn mark_set(&self, marks: &[Mark]) -> MarkSet {
        MarkSet::from_marks(self.schema, marks.iter().cloned())
    }

    fn push_text(&mut self, text: &str, marks: &[Mark]) {
        if text.is_empty() {
            return;
        }
        let set = self.mark_set(marks);
        if let Some(last) = self.out.last_mut()
            && last.is_text()
            && *last.marks() == set
        {
            let joined = format!("{}{text}", last.text().unwrap_or_default());
            *last = last.with_text(&joined);
            return;
        }
        self.out.push(self.schema.text_marked(text, set));
    }

    fn push_node(&mut self, node: Node, marks: &[Mark]) {
        let set = marks.iter().fold(node.marks().clone(), |set, mark| {
            set.add(self.schema, mark.clone())
        });
        let node = node.mark(set);
        if let Some(text) = node.text() {
            self.push_text(text, &node.marks().iter().cloned().collect::<Vec<_>>());
        } else {
            self.out.push(node);
        }
    }
}
