//! What an editing surface needs of this document kind: the schema's role
//! names, and every codec over one compiled schema.
//!
//! Nothing here knows about a view or a platform — both items implement
//! contracts declared by [`markraft_core`], so the editor's GPUI layer consumes
//! them without depending on this crate.

use markraft_core::{Codecs, DocTypeNames, Fragment, MarkSet, Schema, Slice};

use crate::html::{HtmlParser, HtmlSerializer};
use crate::parse::MarkdownParser;
use crate::preset::commonmark_serializer;
use crate::schema;
use crate::text::slice_to_plain_text;

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
        hard_break: Some(schema::HARD_BREAK),
        image: Some(schema::IMAGE),
        strong: Some(schema::STRONG),
        em: Some(schema::EM),
        code: Some(schema::CODE),
        strikethrough: Some(schema::STRIKETHROUGH),
        underline: Some(schema::UNDERLINE),
        link: Some(schema::LINK),
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
    /// merge with the block it is inserted into.
    fn from_text(&self, text: &str) -> Slice {
        let Some(paragraph) = self.schema.node_id(schema::PARAGRAPH) else {
            return Slice::empty();
        };
        let attrs = self.schema.node_type(paragraph).default_attrs().clone();
        let nodes: Vec<_> = text
            .split('\n')
            .filter_map(|part| {
                let content = if part.is_empty() {
                    Fragment::empty()
                } else {
                    Fragment::from_node(self.schema.text(part))
                };
                self.schema
                    .create(paragraph, attrs.clone(), MarkSet::empty(), content)
                    .ok()
            })
            .collect();
        Slice::new(Fragment::from_nodes(nodes), 1, 1)
    }
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
        // A blank line is an empty paragraph, which the codec writes as `<br>`.
        assert_eq!(
            codecs.to_markup(&codecs.from_text("one\n\ntwo")).as_deref(),
            Some("one\n\n<br>\n\ntwo")
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
