//! What an editing surface needs of this document kind: the schema's role
//! names, every codec over one compiled schema, and the formatting commands
//! — which edit the source a style is spelled in, and say so with a
//! [`CommandRefusal`] where Markdown has no spelling for the result.
//!
//! Nothing here knows about a view or a platform — both items implement
//! contracts declared by [`markraft_core`], so the editor's GPUI layer consumes
//! them without depending on this crate.

use markraft_core::kind::SYNTAX_DISPLAY_ATTR;
use markraft_core::projection::{Ancestor, Line};
use markraft_core::{
    Attrs, Fragment, MarkSet, Node, NodeTypeId, Schema, Slice,
    kind::{Codecs, DocTypeNames, SourceSpelling},
};

pub use crate::commands::{
    CommandRefusal, FormatCommand, Formatted, Inexpressible, clear_formatting, keeping_styles,
    set_link, split_block_keeping_styles, toggle_style, toggle_style_mark, unlink,
};
use crate::derive::{BlockKind, DeriveContext, derive};
use crate::html::{HtmlParser, HtmlSerializer};
use crate::parse::MarkdownParser;
use crate::preset::commonmark_serializer;
use crate::schema;
use crate::text::slice_to_plain_text;
use crate::textblock::{Items, block_kind, build};

/// The names [`commonmark_schema`](crate::commonmark_schema) gives the roles an
/// editing surface knows about.
///
/// A consumer that extended the preset and renamed nothing passes this
/// unchanged; one that added a role of its own starts from it and overwrites
/// the field.
pub fn commonmark_doc_type_names() -> DocTypeNames {
    DocTypeNames {
        paragraph: Some(schema::PARAGRAPH),
        heading: Some(schema::HEADING),
        blockquote: Some(schema::BLOCKQUOTE),
        footnote_definition: Some(schema::FOOTNOTE_DEFINITION),
        code_block: Some(schema::CODE_BLOCK),
        bullet_list: Some(schema::BULLET_LIST),
        ordered_list: Some(schema::ORDERED_LIST),
        list_item: Some(schema::LIST_ITEM),
        task_item: Some(schema::TASK_ITEM),
        horizontal_rule: Some(schema::HORIZONTAL_RULE),
        raw_block: Some(schema::RAW_BLOCK),
        table: Some(schema::TABLE),
        table_row: Some(schema::TABLE_ROW),
        table_cell: Some(schema::TABLE_CELL),
        hard_break: Some(schema::LINE_BREAK),
        image: Some(schema::IMAGE),
        strong: Some(schema::STRONG),
        em: Some(schema::EM),
        code: Some(schema::CODE),
        strikethrough: Some(schema::STRIKETHROUGH),
        underline: Some(schema::UNDERLINE),
        highlight: Some(schema::HIGHLIGHT),
        superscript: Some(schema::SUPERSCRIPT),
        subscript: Some(schema::SUBSCRIPT),
        math: Some(schema::MATH),
        link: Some(schema::LINK),
        syntax: Some(schema::SYNTAX),
        footnote_reference: Some(schema::FOOTNOTE_REFERENCE),
    }
}

/// Every clipboard codec of this document kind, over one schema.
///
/// The markup flavour is Markdown, written and read as a fragment rather than
/// as a whole document; see [`MarkdownParser::parse_fragment`].
#[derive(Clone, Debug)]
pub struct CommonMarkCodecs {
    schema: Schema,
}

impl CommonMarkCodecs {
    /// Codecs over `schema`, which must be
    /// [`commonmark_schema`](crate::commonmark_schema) or an extension of it.
    pub fn new(schema: Schema) -> CommonMarkCodecs {
        CommonMarkCodecs { schema }
    }

    /// The schema the codecs read and write.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }
}

impl Codecs for CommonMarkCodecs {
    fn to_text(&self, slice: &Slice) -> String {
        slice_to_plain_text(&self.schema, slice)
    }

    fn to_markup(&self, slice: &Slice) -> Option<String> {
        Some(commonmark_serializer(&self.schema).serialize_fragment(slice))
    }

    fn to_html(&self, slice: &Slice) -> Option<String> {
        Some(HtmlSerializer::commonmark(&self.schema).serialize_fragment(slice))
    }

    fn from_html(&self, html: &str) -> Option<Slice> {
        HtmlParser::commonmark(self.schema.clone())
            .parse_fragment(html)
            .ok()
            .filter(|slice| !slice.is_empty())
    }

    fn from_markup(&self, markup: &str) -> Option<Slice> {
        MarkdownParser::commonmark(self.schema.clone())
            .parse_fragment(markup)
            .ok()
            .filter(|slice| !slice.is_empty())
    }

    /// Every line ending becomes a block break: the text arrives as one
    /// paragraph per line, in an open slice, so its first and last paragraphs
    /// merge with the block it is inserted into. Each line is spelled as the
    /// source that reads as exactly its characters, so a pasted `*a*` or
    /// `# b` stays those characters rather than becoming emphasis or a
    /// heading.
    fn from_text(&self, text: &str) -> Slice {
        let Some(paragraph) = self.schema.node_id(schema::PARAGRAPH) else {
            return Slice::empty();
        };
        let serializer = commonmark_serializer(&self.schema);
        let attrs = self.schema.node_type(paragraph).default_attrs().clone();
        let nodes: Vec<_> = text
            .split('\n')
            .filter_map(|part| {
                let plain = self
                    .schema
                    .create(
                        paragraph,
                        attrs.clone(),
                        MarkSet::empty(),
                        if part.is_empty() {
                            Fragment::empty()
                        } else {
                            Fragment::from_node(self.schema.text(part))
                        },
                    )
                    .ok()?;
                let source = crate::serialize::spell(&serializer, &plain);
                let content = build(&self.schema, BlockKind::Paragraph, &source);
                Some(plain.copy(Fragment::from_nodes(content)))
            })
            .collect();
        Slice::new(Fragment::from_nodes(nodes), 1, 1)
    }

    /// A textblock whose marks its text does not spell — the inside of a
    /// styled span, copied without its delimiters — is spelled again from what
    /// it shows, so the paste reads with the styles the copy had. Every other
    /// block travels as it is written.
    fn copied(&self, slice: &Slice) -> Slice {
        let serializer = commonmark_serializer(&self.schema);
        let inline = slice
            .content()
            .iter()
            .next()
            .is_some_and(|node| self.schema.node_type(node.type_id()).is_inline());
        let content = match self.schema.node_id(schema::PARAGRAPH) {
            // Inline content copied from inside one block is that block's
            // text: respell it as a paragraph's.
            Some(paragraph) if inline => {
                let Ok(block) = self.schema.create(
                    paragraph,
                    Attrs::empty(),
                    MarkSet::empty(),
                    slice.content().clone(),
                ) else {
                    return slice.clone();
                };
                let respelled =
                    respell_fragment(&self.schema, &serializer, &Fragment::from_node(block));
                respelled
                    .iter()
                    .next()
                    .map_or_else(|| slice.content().clone(), |block| block.content().clone())
            }
            _ => respell_fragment(&self.schema, &serializer, slice.content()),
        };
        Slice::new(content, slice.open_start(), slice.open_end())
    }
}

/// A textblock's content as a reader sees it, for spelling again: concealed
/// runs replaced by what they display, and each line break a hard break only
/// where the text makes it one — a soft one travels as the line ending it is.
fn shown_content(schema: &Schema, block: &Node, hard_breaks: &[usize]) -> Fragment {
    let syntax = schema.mark_id(schema::SYNTAX);
    let line_break = schema.node_id(schema::LINE_BREAK);
    let mut out = Vec::new();
    let mut index = 0;
    for child in block.children() {
        let marks = match syntax {
            Some(syntax) => child.marks().remove_type(syntax),
            None => child.marks().clone(),
        };
        if let Some(text) = child.text() {
            index += text.chars().count();
            let concealed = syntax.and_then(|syntax| child.marks().get(syntax));
            match concealed {
                Some(mark) => {
                    let display = mark
                        .attrs
                        .get(SYNTAX_DISPLAY_ATTR)
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    if !display.is_empty() {
                        out.push(schema.text_marked(display, marks));
                    }
                }
                None => out.push(schema.text_marked(text, marks)),
            }
            continue;
        }
        let at = index;
        index += 1;
        if Some(child.type_id()) == line_break && !hard_breaks.contains(&at) {
            // Already spelling: the serialiser writes it as it stands.
            let raw = crate::textblock::syntax_mark(schema, 0, "")
                .map(|mark| marks.add(schema, mark))
                .unwrap_or(marks);
            out.push(schema.text_marked("\n", raw));
        } else {
            out.push(child.mark(marks));
        }
    }
    Fragment::from_nodes(out)
}

/// `content` with every textblock whose marks disagree with its text spelled
/// again from what it shows.
fn respell_fragment(
    schema: &Schema,
    serializer: &crate::serialize::MarkdownSerializer,
    content: &Fragment,
) -> Fragment {
    let nodes: Vec<Node> = content
        .iter()
        .map(|node| {
            if let Some(kind) = block_kind(schema, node.type_id()) {
                let items = Items::from_nodes(schema, node.children());
                let derived = derive(kind, &items.text(), &DeriveContext::new());
                let spelled = items.nodes(schema, &derived);
                if node.children().eq(spelled.iter()) {
                    return node.clone();
                }
                let shown = node.copy(shown_content(schema, node, &derived.hard_breaks));
                let source = crate::serialize::spell(serializer, &shown);
                node.copy(Fragment::from_nodes(build(schema, kind, &source)))
            } else if node.is_container() && !node.is_textblock(schema) {
                node.copy(respell_fragment(schema, serializer, node.content()))
            } else {
                node.clone()
            }
        })
        .collect();
    Fragment::from_nodes(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::commonmark_schema;

    fn codecs() -> CommonMarkCodecs {
        CommonMarkCodecs::new(commonmark_schema())
    }

    #[test]
    fn every_flavour_round_trips_a_fragment() {
        let codecs = codecs();
        let slice = codecs
            .from_markup("**bold** and `code`")
            .expect("a fragment");
        assert_eq!(codecs.to_text(&slice), "bold and code");
        assert_eq!(
            codecs.to_markup(&slice).as_deref(),
            Some("**bold** and `code`")
        );
        assert_eq!(
            codecs.to_html(&slice).as_deref(),
            Some("<p><strong>bold</strong> and <code>code</code></p>")
        );
        let html = codecs.to_html(&slice).expect("HTML");
        assert_eq!(codecs.from_html(&html).as_ref(), Some(&slice));
    }

    #[test]
    fn a_copied_table_survives_every_flavour() {
        let codecs = codecs();
        let markup = "| a | b |\n| :- | --: |\n| 1 |  |";
        let slice = codecs.from_markup(markup).expect("a fragment");
        assert_eq!(
            codecs.to_markup(&slice).as_deref(),
            Some("| a   | b   |\n| :-- | --: |\n| 1   |     |")
        );
        // The plain flavour is what a spreadsheet reads.
        assert_eq!(codecs.to_text(&slice), "a\tb\n1\t");
        // And both rich flavours read their own output back as the same slice.
        let written = codecs.to_markup(&slice).expect("Markdown");
        assert_eq!(codecs.from_markup(&written).as_ref(), Some(&slice));
        let html = codecs.to_html(&slice).expect("HTML");
        assert_eq!(codecs.from_html(&html).as_ref(), Some(&slice));
    }

    #[test]
    fn a_reader_answers_none_rather_than_an_empty_slice() {
        let codecs = codecs();
        assert_eq!(codecs.from_markup(""), None);
        assert_eq!(codecs.from_html(""), None);
    }

    #[test]
    fn plain_text_arrives_as_one_open_paragraph_per_line() {
        let codecs = codecs();
        let slice = codecs.from_text("one\ntwo");
        assert_eq!(slice.open_start(), 1);
        assert_eq!(slice.open_end(), 1);
        assert_eq!(slice.content().child_count(), 2);
        assert_eq!(codecs.to_markup(&slice).as_deref(), Some("one\n\ntwo"));
        // Blank lines in plain text become empty paragraphs; CommonMark has no
        // spelling for those, so they collapse back to a single separator.
        assert_eq!(
            codecs.to_markup(&codecs.from_text("one\n\ntwo")).as_deref(),
            Some("one\n\ntwo")
        );
    }

    #[test]
    fn the_role_names_all_resolve_against_the_preset() {
        let schema = commonmark_schema();
        let names = commonmark_doc_type_names();
        for name in [
            names.paragraph,
            names.heading,
            names.blockquote,
            names.code_block,
            names.bullet_list,
            names.ordered_list,
            names.list_item,
            names.task_item,
            names.horizontal_rule,
            names.raw_block,
            names.table,
            names.table_row,
            names.table_cell,
            names.hard_break,
            names.image,
        ] {
            let name = name.expect("every node role is named");
            assert!(schema.node_id(name).is_some(), "missing node {name}");
        }
        for name in [
            names.strong,
            names.em,
            names.code,
            names.strikethrough,
            names.underline,
            names.link,
        ] {
            let name = name.expect("every mark role is named");
            assert!(schema.mark_id(name).is_some(), "missing mark {name}");
        }
    }
}

/// How CommonMark spells the parts of a document a view may reveal as source.
///
/// The counterpart of [`CommonMarkCodecs`] for an editing surface: the same
/// escaping the serialiser uses, so what a reader is shown is what a save
/// writes.
#[derive(Clone, Debug)]
pub struct CommonMarkSpelling {
    schema: Schema,
}

impl CommonMarkSpelling {
    /// A spelling over `schema`.
    pub fn new(schema: Schema) -> CommonMarkSpelling {
        CommonMarkSpelling { schema }
    }

    fn name(&self, ty: NodeTypeId) -> &str {
        self.schema.node_type(ty).name()
    }

    fn is_item(&self, ty: NodeTypeId) -> bool {
        matches!(self.name(ty), schema::LIST_ITEM | schema::TASK_ITEM)
    }

    /// The item and list a line sits in, when the line is the item's first —
    /// which is where a marker belongs. Every ancestor between the item and the
    /// line has to be a first child, or a table row inside an item would get a
    /// bullet of its own.
    fn item_of<'a>(&self, line: &'a Line) -> Option<(&'a Ancestor, &'a Ancestor)> {
        let index = line
            .ancestors()
            .iter()
            .rposition(|ancestor| self.is_item(ancestor.node_type))?;
        let starts = index + 1 < line.ancestors().len()
            && line.ancestors()[index + 1..]
                .iter()
                .all(|ancestor| ancestor.index == 0);
        if !starts {
            return None;
        }
        let list = line.ancestors().get(index.checked_sub(1)?)?;
        matches!(
            self.name(list.node_type),
            schema::BULLET_LIST | schema::ORDERED_LIST
        )
        .then(|| (&line.ancestors()[index], list))
    }
}

fn attr_str<'a>(attrs: &'a Attrs, name: &str, default: &'a str) -> &'a str {
    attrs
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
}

fn attr_int(attrs: &Attrs, name: &str, default: i64) -> i64 {
    attrs
        .get(name)
        .and_then(|value| value.as_int())
        .unwrap_or(default)
}

impl SourceSpelling for CommonMarkSpelling {
    fn line_prefix(&self, line: &Line) -> Option<String> {
        let block = line.ancestors().last()?;
        if self.name(block.node_type) == schema::HEADING {
            let level = attr_int(&block.attrs, "level", 1).clamp(1, 6) as usize;
            return Some(format!("{} ", "#".repeat(level)));
        }
        let (item, list) = self.item_of(line)?;
        let bullet = attr_str(&list.attrs, "bullet_char", "-");
        if self.name(item.node_type) == schema::TASK_ITEM {
            let checked = item
                .attrs
                .get("checked")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            return Some(format!("{bullet} [{}] ", if checked { "x" } else { " " }));
        }
        if self.name(list.node_type) == schema::ORDERED_LIST {
            let start = attr_int(&list.attrs, "start", 1);
            let delimiter = attr_str(&list.attrs, "delimiter", ".");
            return Some(format!("{}{delimiter} ", start + item.index as i64));
        }
        Some(format!("{bullet} "))
    }

    fn verbatim_fence(&self, line: &Line) -> Option<(String, String)> {
        let block = line.ancestors().last()?;
        if self.name(block.node_type) != schema::CODE_BLOCK {
            return None;
        }
        let character = attr_str(&block.attrs, "fence_char", "`")
            .chars()
            .next()
            .unwrap_or('`');
        let length = attr_int(&block.attrs, "fence_length", 3).clamp(3, 12) as usize;
        let fence: String = std::iter::repeat_n(character, length).collect();
        let language = attr_str(&block.attrs, "language", "");
        Some((format!("{fence}{language}"), fence))
    }

    fn container_marker(&self, node_type: NodeTypeId) -> Option<String> {
        // The space is part of the spelling: `>foo` and `> foo` are the same
        // quote, and what a writer types — and what a save writes — is the
        // second.
        (self.name(node_type) == schema::BLOCKQUOTE).then(|| "> ".to_string())
    }

    fn atom_source(&self, node: &Node) -> Option<String> {
        match self.name(node.type_id()) {
            schema::IMAGE => Some(crate::textblock::image_spelling(node.attrs())),
            schema::WIKI_LINK => Some(
                crate::wiki::WikiLink {
                    target: attr_str(node.attrs(), "target", "").to_string(),
                    alias: attr_str(node.attrs(), "alias", "").to_string(),
                    embed: node
                        .attrs()
                        .get("embed")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                }
                .source(),
            ),
            _ => None,
        }
    }
}
