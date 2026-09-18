//! The inline half of the parser: comrak's inline nodes into marked content.
//!
//! Two things here need more context than the rule table can carry. Paired
//! inline HTML — `<u>`, `<em>`, `<strong>`, `<del>` — becomes a mark, which
//! means knowing which tags pair up across the whole block before any of them
//! is read. And adjacent text with equal marks has to be one leaf, which means
//! merging as the content is built.

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
                        builder.close_html(id);
                        continue;
                    }
                    None => {}
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
                    // A construct with no content — `[](url)` — has no inline to
                    // carry the mark, so it travels as the text it reads as.
                    if child.first_child().is_none() {
                        builder.push_text(&target.source(), marks);
                        continue;
                    }
                    let id = self.mark_id(&mark_type(target))?;
                    let mark =
                        Mark::with_attrs(id, self.schema.build_mark_attrs(id, &attrs(target))?);
                    let mut inner = marks.to_vec();
                    inner.push(mark);
                    self.walk_inlines(child, &inner, builder)?;
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
    /// Only tags that pair up carry a mark; a stray one stays literal text, as
    /// it reads. A pair that straddles another pair's opening tag makes the
    /// inner one literal, because marks are a set and cannot interleave.
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
            // Descend exactly where `walk_inlines` does, so an image's opaque
            // label cannot shift the marks of the text after it.
            if matches!(self.rules.rule(&self.value(child)), ParseRule::Mark { .. })
                && child.first_child().is_some()
            {
                self.collect_tags(child, tags);
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
    html: Vec<Mark>,
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
        self.html.push(mark);
    }

    fn close_html(&mut self, ty: MarkTypeId) {
        self.html.retain(|mark| mark.ty != ty);
    }

    fn mark_set(&self, marks: &[Mark]) -> MarkSet {
        MarkSet::from_marks(self.schema, marks.iter().chain(self.html.iter()).cloned())
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
        let set = self.mark_set(marks);
        self.out.push(node.mark(set));
    }
}
