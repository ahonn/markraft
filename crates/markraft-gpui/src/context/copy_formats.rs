//! Explicit clipboard representations without changing the editing selection.

use crate::{EditorView, clipboard};
use gpui::Context;
use markraft_core::{Fragment, Slice};

impl EditorView {
    /// Whether the selected fragment can be exported in this representation.
    /// Copy is available in read-only documents too.
    pub fn can_copy_as(&self, format: clipboard::CopyFormat) -> bool {
        self.prepared_copy(format).is_some()
    }

    /// Write the requested representation without an editing transaction.
    pub fn copy_as(&self, format: clipboard::CopyFormat, cx: &mut Context<Self>) -> bool {
        let Some(copy) = self.prepared_copy(format) else {
            return false;
        };
        clipboard::write_prepared(copy, cx);
        true
    }

    fn prepared_copy(&self, format: clipboard::CopyFormat) -> Option<clipboard::PreparedCopy> {
        if self.is_composing() || self.state.selection().is_empty(self.state.doc()) {
            return None;
        }
        clipboard::prepare_copy(
            self.state.schema(),
            self.codecs.as_deref().filter(|_| !self.single_line),
            &self.types,
            &self.selection_slice(),
            format,
        )
    }

    /// Whether `pos` identifies a complete table in the current document.
    pub fn can_copy_table_at(&self, pos: usize) -> bool {
        !self.is_composing() && self.table_copy_slice(pos).is_some()
    }

    /// Copy the complete table under the pointer, independently of the text
    /// selection. The host must validate its context snapshot before calling.
    pub fn copy_table_at(&self, pos: usize, cx: &mut Context<Self>) -> bool {
        if self.is_composing() {
            return false;
        }
        let Some(slice) = self.table_copy_slice(pos) else {
            return false;
        };
        if let Some(codecs) = self.codecs.as_deref() {
            clipboard::write(self.state.schema(), codecs, &slice, false, cx);
        } else {
            let Some(copy) = clipboard::prepare_copy(
                self.state.schema(),
                None,
                &self.types,
                &slice,
                clipboard::CopyFormat::PlainText,
            ) else {
                return false;
            };
            clipboard::write_prepared(copy, cx);
        }
        true
    }

    fn table_copy_slice(&self, pos: usize) -> Option<Slice> {
        if self.single_line {
            return None;
        }
        let table_type = self.types.table_types()?.table;
        let node = self.state.doc().node_at(pos)?;
        (node.type_id() == table_type).then(|| Slice::from_fragment(Fragment::from_node(node)))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::{CopyFormat, EditRejection, Setup};
    use gpui::{AppContext, ClipboardItem, TestAppContext};
    use markraft_commonmark::{
        CommonMarkCodecs, commonmark_doc_type_names, commonmark_extensions, commonmark_schema,
        from_markdown,
    };
    use markraft_core::{history, kind::DocTypes};

    fn editor(source: &str, cx: &mut Context<EditorView>) -> EditorView {
        let schema = commonmark_schema();
        let setup = Setup::new(schema.clone())
            .types(DocTypes::from_schema_names(
                &schema,
                &commonmark_doc_type_names(),
            ))
            .extensions(commonmark_extensions(&schema))
            .doc(from_markdown(&schema, source).unwrap());
        let mut view = EditorView::new(setup, cx);
        view.codecs = Some(std::sync::Arc::new(CommonMarkCodecs::new(
            schema,
            Default::default(),
        )));
        view
    }

    #[gpui::test]
    fn explicit_formats_preserve_structure_and_unicode_without_editing(cx: &mut TestAppContext) {
        let view = cx.new(|cx| {
            editor(
                "# 标题\n\n**café 👩🏽‍💻** &amp; [网站](https://example.com)",
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.select_all(cx);
            let selection = view.state.selection().clone();
            let document = view.state.doc().clone();
            let depth = history::undo_depth(&view.state);
            let expected = [
                (CopyFormat::PlainText, "标题\ncafé 👩🏽‍💻 & 网站"),
                (CopyFormat::Markdown, "# 标题\n\n**café 👩🏽‍💻** &amp; [网站](https://example.com)"),
                (CopyFormat::HtmlCode, "<h1>标题</h1>\n<p><strong>café 👩🏽‍💻</strong> &amp; <a href=\"https://example.com\">网站</a></p>"),
                (CopyFormat::RichText, "标题\ncafé 👩🏽‍💻 & 网站"),
            ];
            for (format, text) in expected {
                assert!(view.can_copy_as(format));
                assert!(view.copy_as(format, cx));
                let item = cx.read_from_clipboard().unwrap();
                assert_eq!(item.text().as_deref(), Some(text), "{format:?}");
                if format == CopyFormat::RichText {
                    let copied = clipboard::read_fragment(
                        view.state.schema(), view.codecs.as_deref().unwrap(), &item,
                        clipboard::PasteMode::Formatted, cx,
                    ).unwrap();
                    assert_eq!(clipboard::markup(view.codecs.as_deref().unwrap(), &copied), expected[1].1);
                } else {
                    assert!(item.metadata().is_none(), "explicit text formats carry only text");
                }
                assert_eq!(view.state.doc(), &document);
                assert_eq!(view.state.selection(), &selection);
                assert_eq!(history::undo_depth(&view.state), depth);
            }
        });
    }

    #[gpui::test]
    fn partial_styled_selection_copies_the_requested_format_in_read_only_document(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| {
            editor("before **café** after", cx)
                .with_document_guard(|_| Err(EditRejection::ReadOnly("Read only".into())))
        });
        view.update(cx, |view, cx| {
            view.select_range(10, 14, cx);
            for (format, expected) in [
                (CopyFormat::PlainText, "café"),
                (CopyFormat::Markdown, "**café**"),
                (CopyFormat::HtmlCode, "<p><strong>café</strong></p>"),
                (CopyFormat::RichText, "café"),
            ] {
                assert!(view.copy_as(format, cx));
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some(expected),
                    "{format:?}"
                );
            }
            assert_eq!(history::undo_depth(&view.state), 0);
        });
    }

    #[gpui::test]
    fn empty_selection_and_unavailable_formats_leave_the_clipboard_unchanged(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|cx| editor("some **text**", cx));
        view.update(cx, |view, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("keep".into()));
            for format in [
                CopyFormat::PlainText,
                CopyFormat::Markdown,
                CopyFormat::HtmlCode,
                CopyFormat::RichText,
            ] {
                assert!(!view.can_copy_as(format));
                assert!(!view.copy_as(format, cx));
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some("keep")
                );
            }
            view.select_all(cx);
            view.codecs = None;
            for format in [
                CopyFormat::Markdown,
                CopyFormat::HtmlCode,
                CopyFormat::RichText,
            ] {
                assert!(!view.can_copy_as(format));
                assert!(!view.copy_as(format, cx));
            }
            assert!(view.copy_as(CopyFormat::PlainText, cx));
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("some text")
            );
        });
    }

    #[gpui::test]
    fn copy_table_exports_the_whole_table_without_moving_selection(cx: &mut TestAppContext) {
        let view = cx.new(|cx| {
            editor(
                "intro\n\n| 名称 | 值 |\n| --- | ---: |\n| **甲** | 42 |\n\nafter",
                cx,
            )
        });
        view.update(cx, |view, cx| {
            let table = view.types.table_types().unwrap().table;
            let pos = view.state.doc().first_child().unwrap().node_size();
            let selection = view.state.selection().clone();
            let document = view.state.doc().clone();
            assert!(view.can_copy_table_at(pos));
            assert!(view.copy_table_at(pos, cx));
            let item = cx.read_from_clipboard().unwrap();
            let codecs = view.codecs.as_deref().unwrap();
            let copied = clipboard::read_fragment(
                view.state.schema(),
                codecs,
                &item,
                clipboard::PasteMode::Formatted,
                cx,
            )
            .unwrap();
            assert_eq!(copied.content().child_count(), 1);
            assert_eq!(copied.content().child(0).type_id(), table);
            assert_eq!(codecs.to_text(&copied), "名称\t值\n甲\t42");
            assert!(item.text().unwrap().contains("**甲**"));
            assert!(!view.copy_table_at(0, cx));
            assert!(!view.can_copy_table_at(view.state.doc().content_size() + 1));
            assert_eq!(cx.read_from_clipboard().unwrap().text(), item.text());
            assert_eq!(view.state.doc(), &document);
            assert_eq!(view.state.selection(), &selection);
            assert_eq!(history::undo_depth(&view.state), 0);
        });
    }
}
