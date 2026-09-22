//! The inline half of the parser: marked leaves and semantic inline containers.
//!
//! Two things here need more context than the rule table can carry. Paired
//! inline HTML — `<u>`, `<em>`, `<strong>`, `<del>`, `<a>` — becomes a mark,
//! which means knowing which tags pair up across the whole block before any of
//! them is read. And adjacent text with equal marks has to be one leaf, which
//! means merging as the content is built.
//!
//! A tag is read as something other than source text only when the tree holds
//! everything it says and the serialiser can write that back. An `<a>` with a
//! `target`, an `<img>` with a `width` and a `<br>` in a table cell all stay
//! raw primitives, because the alternative loses what they carry.

use crate::escape::code_span_delimiters;
use crate::inline::{style_delimiters, wrap_mark, wrap_mark_method_b};
use comrak::nodes::{AstNode, NodeValue};
use markraft_core::{Attrs, Fragment, Mark, MarkSet, MarkTypeId, Node, Schema, attrs};

use super::{ParseError, Walk};
use crate::rules::ParseRule;
use crate::schema as md;

impl<'a> Walk<'a> {
    pub(crate) fn inlines(&self, parent: &'a AstNode<'a>) -> Result<Vec<Node>, ParseError> {
        let tags = self.tags(parent);
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
            if self.wiki_link_stays_text(child) {
                let source = self.cx.wrapped_source(target.sourcepos());
                self.push_source_text(&source, marks, builder)?;
                continue;
            }
            if self.embeds(child, marks, builder)? {
                continue;
            }
            let inline_html = match &*self.value(child) {
                NodeValue::HtmlInline(html) => Some(html.clone()),
                _ => None,
            };
            if let Some(html) = inline_html {
                match builder.next_tag() {
                    Tag::Open { mark, attrs } => {
                        let id = self.mark_id(&mark)?;
                        let attrs = self.schema.build_mark_attrs(id, &attrs)?;
                        builder.open_html(Mark::with_attrs(id, attrs));
                    }
                    Tag::Close(mark) => {
                        let id = self.mark_id(&mark)?;
                        builder.close_html(id)?;
                    }
                    Tag::Atom { node_type, attrs } => {
                        let ty = self.node_id(&node_type)?;
                        let node =
                            self.schema
                                .create(ty, attrs, MarkSet::empty(), Fragment::empty())?;
                        builder.push_node(node, marks);
                    }
                    Tag::Literal => {
                        let ty = self.node_id(md::RAW_INLINE)?;
                        let node = self.schema.create(
                            ty,
                            attrs! {"source" => html},
                            MarkSet::empty(),
                            Fragment::empty(),
                        )?;
                        builder.push_node(node, marks);
                    }
                }
                continue;
            }
            let rule = self.rules.rule(&self.value(child)).clone();
            match rule {
                ParseRule::Ignore => {}
                ParseRule::Text { text, marks: extra } => {
                    let content = text(target);
                    // Code spans are Method-B: backticks stay in the tree.
                    if extra.len() == 1 && extra[0] == md::CODE {
                        let (open, close) = code_span_delimiters(&content);
                        let code = Mark::new(self.mark_id(md::CODE)?);
                        let open_leaf = crate::inline::syntax_text(self.schema, &open)?;
                        let close_leaf = crate::inline::syntax_text(self.schema, &close)?;
                        let open_leaf =
                            open_leaf.mark(open_leaf.marks().add(self.schema, code.clone()));
                        let close_leaf =
                            close_leaf.mark(close_leaf.marks().add(self.schema, code.clone()));
                        builder.push_node(open_leaf, marks);
                        let mut inner = marks.to_vec();
                        inner.push(code);
                        builder.push_text(&content, &inner);
                        builder.push_node(close_leaf, marks);
                        continue;
                    }
                    let mut all = marks.to_vec();
                    for name in &extra {
                        all.push(Mark::new(self.mark_id(name)?));
                    }
                    builder.push_text(&content, &all);
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
                    let name = mark_type(target);
                    let id = self.mark_id(&name)?;
                    let mark =
                        Mark::with_attrs(id, self.schema.build_mark_attrs(id, &attrs(target))?);
                    let children = self.inlines(child)?;
                    let wrapped = if let Some((open, close)) = style_delimiters(&name) {
                        wrap_mark_method_b(self.schema, mark, open, close, children)?
                    } else {
                        wrap_mark(self.schema, mark, children)?
                    };
                    for node in wrapped {
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

    /// Whether comrak read a `[[…]]` that this codec does not: one with an
    /// empty alias, or one spanning a line ending. See [`crate::wiki`] for why
    /// neither can be an atom. Such a source stays the text a reader sees,
    /// exactly as it was before the extension was enabled.
    fn wiki_link_stays_text(&self, node: &'a AstNode<'a>) -> bool {
        matches!(&*self.value(node), NodeValue::WikiLink(_))
            && crate::wiki::whole_wiki_link(&self.cx.wrapped_source(self.target(node).sourcepos()))
                .is_none()
    }

    /// Push source no node stands for: one text leaf per line, with the soft
    /// breaks between them the parser would have produced for it anyway.
    fn push_source_text(
        &self,
        source: &str,
        marks: &[Mark],
        builder: &mut Inlines<'_>,
    ) -> Result<(), ParseError> {
        let soft_break = self.node_id(md::SOFT_BREAK)?;
        for (index, line) in source.split('\n').enumerate() {
            if index > 0 {
                let node = self.schema.create(
                    soft_break,
                    Attrs::empty(),
                    MarkSet::empty(),
                    Fragment::empty(),
                )?;
                builder.push_node(node, marks);
            }
            builder.push_text(line, marks);
        }
        Ok(())
    }

    /// Split a text run around the `![[…]]` embeds in it, answering whether it
    /// held any.
    ///
    /// comrak has no embed syntax: the `!` opens an image label, which stops
    /// the wiki link inside from being seen at all, so the whole run arrives
    /// here as the text a reader sees.
    ///
    /// The atom keeps the bytes its source spelled, so the embeds are found in
    /// that source; the text around them is the literal comrak read, which is
    /// the text everywhere else in this parser too. [`literal_offsets`] is what
    /// carries a position from one to the other.
    fn embeds(
        &self,
        node: &'a AstNode<'a>,
        marks: &[Mark],
        builder: &mut Inlines<'_>,
    ) -> Result<bool, ParseError> {
        let literal = match &*self.value(node) {
            NodeValue::Text(text) if text.contains("[[") => text.to_string(),
            _ => return Ok(false),
        };
        let source = self.target(node).source();
        let Some(offsets) = literal_offsets(&source, &literal) else {
            return Ok(false);
        };
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(offset) = source[at..].find("![[").map(|index| at + index) {
            // An escaped `\!` is not an embed's, and its `!` is not a position
            // the literal has one of either.
            match crate::wiki::read_wiki_link(&source[offset..])
                .filter(|_| offsets[offset] != NOT_IN_LITERAL)
            {
                Some((link, len)) => {
                    found.push((offsets[offset]..offsets[offset + len], link));
                    at = offset + len;
                }
                None => at = offset + 1,
            }
        }
        if found.is_empty() {
            return Ok(false);
        }
        let ty = self.node_id(md::WIKI_LINK)?;
        let mut at = 0;
        for (span, link) in &found {
            builder.push_text(&literal[at..span.start], marks);
            let attrs = attrs! {
                "target" => link.target.clone(),
                "alias" => link.alias.clone(),
                "embed" => true,
            };
            let atom = self
                .schema
                .create(ty, attrs, MarkSet::empty(), Fragment::empty())?;
            builder.push_node(atom, marks);
            at = span.end;
        }
        builder.push_text(&literal[at..], marks);
        Ok(true)
    }

    /// What each inline HTML tag of this block does, in the order
    /// [`Walk::walk_inlines`] meets them.
    ///
    /// Only tags paired within this Markdown scope become marks. Unpaired or
    /// overlapping tags stay raw primitives; guessing a DOM tree would change
    /// the source semantics.
    fn tags(&self, parent: &'a AstNode<'a>) -> Vec<Tag> {
        let mut tags = self.collect_tags(parent);
        let mut open: Vec<usize> = Vec::new();
        for index in 0..tags.len() {
            match tags[index].clone() {
                Tag::Open { mark, .. } => {
                    // CommonMark has no link inside a link, so an `<a>` that
                    // would nest in one could not be written back at all.
                    if mark == md::LINK && open.iter().any(|start| tags[*start].opens(md::LINK)) {
                        tags[index] = Tag::Literal;
                    } else {
                        open.push(index);
                    }
                }
                Tag::Close(name) => {
                    let matching = open.iter().rposition(|start| tags[*start].opens(&name));
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
                Tag::Atom { .. } | Tag::Literal => {}
            }
        }
        for index in open {
            tags[index] = Tag::Literal;
        }
        tags
    }

    fn collect_tags(&self, parent: &'a AstNode<'a>) -> Vec<Tag> {
        let breaks = self.hard_break_fits(parent);
        let links = self.link_mark_fits(parent);
        parent
            .children()
            .filter_map(|child| match &*self.value(child) {
                // A break with nothing after it in its block is dropped on the
                // way out, so one read here would be lost; it stays raw.
                NodeValue::HtmlInline(html) => Some(tag_role(
                    html,
                    breaks && child.next_sibling().is_some(),
                    links,
                )),
                _ => None,
            })
            .collect()
    }

    /// Whether a `hard_break` read where this content sits comes back as one.
    ///
    /// A heading and a table row are each a single source line, and the
    /// serialiser spells a break in them as a space and as `<br>`. Reading the
    /// tag as a break there would lose it on the way out; the raw primitive
    /// writes itself again unchanged.
    fn hard_break_fits(&self, parent: &'a AstNode<'a>) -> bool {
        !parent.ancestors().any(|node| {
            matches!(
                &*self.value(node),
                NodeValue::Heading(_) | NodeValue::TableCell
            )
        })
    }

    /// Whether a link mark can be added where this content sits. CommonMark
    /// has no link inside a link, so an `<a>` in a link's label stays raw.
    fn link_mark_fits(&self, parent: &'a AstNode<'a>) -> bool {
        !parent
            .ancestors()
            .any(|node| matches!(&*self.value(node), NodeValue::Link(_)))
    }
}

/// The mark [`literal_offsets`] puts on a source offset the literal has no
/// position of its own for: the byte after a backslash that escapes it.
const NOT_IN_LITERAL: usize = usize::MAX;

/// Where each byte offset of `source` lands in the `literal` comrak read out of
/// it, or `None` where the two cannot be walked together.
///
/// comrak resolves a backslash escape and a character reference while it reads
/// text. A backslash escape is a local two-bytes-for-one substitution this
/// follows, and so is a *numeric* reference, which is the only kind this
/// codec's own serialiser writes. A named one — `&amp;` in someone else's file
/// — is not, so a run holding one cannot be lined up at all and is left as the
/// single text leaf it arrived as.
fn literal_offsets(source: &str, literal: &str) -> Option<Vec<usize>> {
    let (source, literal) = (source.as_bytes(), literal.as_bytes());
    let mut offsets = vec![NOT_IN_LITERAL; source.len() + 1];
    let (mut read, mut written) = (0, 0);
    while read < source.len() {
        offsets[read] = written;
        if let Some((character, len)) = numeric_reference(&source[read..]) {
            let mut buffer = [0_u8; 4];
            let encoded = character.encode_utf8(&mut buffer).as_bytes();
            if literal.get(written..written + encoded.len()) != Some(encoded) {
                return None;
            }
            read += len;
            written += encoded.len();
            continue;
        }
        let escape = source[read] == b'\\'
            && source
                .get(read + 1)
                .is_some_and(|byte| byte.is_ascii_punctuation());
        read += usize::from(escape);
        if literal.get(written) != source.get(read) {
            return None;
        }
        read += 1;
        written += 1;
    }
    offsets[read] = written;
    (written == literal.len()).then_some(offsets)
}

/// The character a numeric reference at the start of `source` stands for, with
/// the number of bytes it spells it in.
fn numeric_reference(source: &[u8]) -> Option<(char, usize)> {
    let rest = source.strip_prefix(b"&#")?;
    let (digits, radix) = match rest.first() {
        Some(b'x' | b'X') => (&rest[1..], 16),
        _ => (rest, 10),
    };
    let end = digits.iter().take(9).position(|byte| *byte == b';')?;
    let code = u32::from_str_radix(std::str::from_utf8(&digits[..end]).ok()?, radix).ok()?;
    // CommonMark gives a reference to nothing the replacement character.
    let character = char::from_u32(code)
        .filter(|c| *c != '\0')
        .unwrap_or('\u{fffd}');
    Some((character, source.len() - digits.len() + end + 1))
}

/// One inline HTML tag's role in a block.
#[derive(Clone, Debug)]
enum Tag {
    /// Opens a mark, closed by a later tag standing for the same one.
    Open { mark: String, attrs: Attrs },
    /// Closes the mark of that name.
    Close(String),
    /// A node of its own: an image, a hard break.
    Atom { node_type: String, attrs: Attrs },
    /// Source text, kept in a raw inline primitive.
    Literal,
}

impl Tag {
    fn opens(&self, mark: &str) -> bool {
        matches!(self, Tag::Open { mark: name, .. } if name == mark)
    }
}

/// What one inline HTML tag stands for.
///
/// `hard_break` says whether a break read here would survive being written
/// out, and `link` whether a link mark may be added at all.
fn tag_role(html: &str, hard_break: bool, link: bool) -> Tag {
    let Some((name, opening, rest)) = split_tag(html) else {
        return Tag::Literal;
    };
    if !opening {
        // A closing tag carries nothing but its name.
        if !rest.is_empty() {
            return Tag::Literal;
        }
        if name == "a" {
            return Tag::Close(md::LINK.to_string());
        }
        return style_mark(&name).map_or(Tag::Literal, |mark| Tag::Close(mark.to_string()));
    }
    match name.as_str() {
        "a" if link => link_tag(html),
        "img" => image_tag(html),
        "br" if hard_break => break_tag(html),
        _ if rest.is_empty() => style_mark(&name).map_or(Tag::Literal, |mark| Tag::Open {
            mark: mark.to_string(),
            attrs: Attrs::empty(),
        }),
        _ => Tag::Literal,
    }
}

/// The mark a bare styling tag stands for.
fn style_mark(name: &str) -> Option<&'static str> {
    match name {
        "u" => Some(md::UNDERLINE),
        "em" => Some(md::EM),
        "strong" => Some(md::STRONG),
        "del" => Some(md::STRIKETHROUGH),
        _ => None,
    }
}

/// `<a href="…" title="…">` as the link mark.
///
/// The mark holds a destination and a title and nothing else, so an anchor
/// carrying any other attribute — `target`, `class`, `id` — stays raw rather
/// than dropping it, and one with no `href` is not a link to begin with.
fn link_tag(html: &str) -> Tag {
    let Some(attrs) = tag_attrs(html, "a") else {
        return Tag::Literal;
    };
    let mut href = None;
    let mut title = String::new();
    for (name, value) in attrs {
        match name.as_str() {
            "href" => href = Some(value),
            "title" => title = value,
            _ => return Tag::Literal,
        }
    }
    let Some(href) = href else {
        return Tag::Literal;
    };
    Tag::Open {
        mark: md::LINK.to_string(),
        attrs: attrs! {"href" => href, "title" => title},
    }
}

/// `<img src="…" alt="…" title="…">`, self-closing or not, as the image atom.
///
/// The atom holds those three attributes; a `width`, a `class` or a `style`
/// would be lost, so such a tag stays raw.
fn image_tag(html: &str) -> Tag {
    let Some(attrs) = tag_attrs(html, "img") else {
        return Tag::Literal;
    };
    let mut src = None;
    let mut alt = String::new();
    let mut title = String::new();
    for (name, value) in attrs {
        match name.as_str() {
            "src" => src = Some(value),
            "alt" => alt = value,
            "title" => title = value,
            _ => return Tag::Literal,
        }
    }
    let Some(src) = src else {
        return Tag::Literal;
    };
    Tag::Atom {
        node_type: md::IMAGE.to_string(),
        attrs: attrs! {"src" => src, "alt" => alt, "title" => title},
    }
}

/// `<br>`, `<br/>` or `<br />` as the hard break atom. The node carries no
/// attributes, so a tag with one stays raw.
fn break_tag(html: &str) -> Tag {
    match tag_attrs(html, "br") {
        Some(attrs) if attrs.is_empty() => Tag::Atom {
            node_type: md::HARD_BREAK.to_string(),
            attrs: Attrs::empty(),
        },
        _ => Tag::Literal,
    }
}

/// The attributes of a tag of `name`, read by the same HTML parser the
/// clipboard importer uses so both resolve an entity in a value the same way.
fn tag_attrs(html: &str, name: &str) -> Option<Vec<(String, String)>> {
    let (parsed, attrs) = crate::html::read_tag(html)?;
    (parsed == name).then_some(attrs)
}

/// A tag's lowercased name, whether it opens, and whatever stands between the
/// name and the `>`.
fn split_tag(html: &str) -> Option<(String, bool, &str)> {
    let body = html.strip_prefix('<')?.strip_suffix('>')?;
    let (body, opening) = match body.strip_prefix('/') {
        Some(body) => (body, false),
        None => (body, true),
    };
    let end = body
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .unwrap_or(body.len());
    let (name, rest) = body.split_at(end);
    (!name.is_empty()).then(|| (name.to_ascii_lowercase(), opening, rest.trim()))
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
    fn next_tag(&mut self) -> Tag {
        let tag = self.tags.get(self.seen).cloned();
        self.seen += 1;
        tag.unwrap_or(Tag::Literal)
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
            let name = self.schema.mark_type(mark.ty).name().to_string();
            let wrapped = if let Some((open, close)) = style_delimiters(&name) {
                wrap_mark_method_b(self.schema, mark, open, close, children)?
            } else {
                wrap_mark(self.schema, mark, children)?
            };
            for node in wrapped {
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
        // Method-B delimiter leaves must not merge with each other.
        let syntax = self.schema.mark_id(md::SYNTAX);
        let mergeable = syntax.is_none_or(|ty| set.get(ty).is_none());
        if mergeable
            && let Some(last) = self.out.last_mut()
            && last.is_text()
            && *last.marks() == set
            && syntax.is_none_or(|ty| last.marks().get(ty).is_none())
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
