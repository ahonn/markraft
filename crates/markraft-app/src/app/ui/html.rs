use super::*;
use markraft_core::{Node, NodeTypeSpec, Schema, SchemaSpec};

pub(in crate::app) struct HtmlEditor {
    note: String,
    pos: usize,
    original: Node,
    source: Entity<EditorView>,
    save_focus: FocusHandle,
    cancel_focus: FocusHandle,
}

fn source_document(source: &str) -> (Schema, Node) {
    let schema = Schema::new(
        SchemaSpec::new()
            .node(NodeTypeSpec::new("doc", "paragraph+"))
            .node(NodeTypeSpec::new("paragraph", "text*").group("block"))
            .node(NodeTypeSpec::text("text").group("inline")),
    )
    .expect("the source editor schema is valid");
    let paragraphs = source.split('\n').map(|line| {
        let text = if line.is_empty() {
            vec![]
        } else {
            vec![schema.text(line)]
        };
        schema.node("paragraph", text).expect("plain text is valid")
    });
    let doc = schema
        .doc(paragraphs)
        .expect("source has at least one line");
    (schema, doc)
}

fn source_setup(source: &str) -> Setup {
    let (schema, doc) = source_document(source);
    let types = markraft_gpui::DocTypes {
        paragraph: schema.node_id("paragraph"),
        ..markraft_gpui::DocTypes::none()
    };
    Setup::new(schema).types(types).doc(doc)
}

fn source_to_save(state: &markraft_core::EditorState) -> Option<String> {
    if markraft_core::is_composing(state) {
        return None;
    }
    Some(
        markraft_core::projection::Projection::of(state.doc(), state.schema())
            .plain_text()
            .to_owned(),
    )
}

impl NotesApp {
    pub(super) fn html_source_entry(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.interaction.panel() != Panel::Editor
            || self.interaction.html().is_some()
            || self.interaction.popover().is_some()
        {
            return None;
        }
        let editor = self.editor().read(cx);
        let pos = editor.raw_html_at_caret()?;
        let anchor = editor.anchor_bounds()?;
        let width = px(184.).min(window.bounds().size.width - px(16.));
        let left = (anchor.left())
            .max(px(8.))
            .min(window.bounds().size.width - width - px(8.));
        let top = if anchor.top() < px(76.) {
            anchor.bottom() + px(6.)
        } else {
            anchor.top() - px(36.)
        };
        Some(
            div()
                .id("html-source-entry")
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .rounded(ROW_RADIUS)
                .bg(self.surface_color())
                .border_1()
                .border_color(self.border_color())
                .shadow(popover_shadow())
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    self.button("html-edit", "Edit HTML · ⌥⌘R", Intent::EditHtml(pos), cx)
                        .w_full(),
                ),
        )
    }

    pub(in crate::app) fn open_html_source(
        &mut self,
        pos: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focus_html_source(window, cx) {
            return;
        }
        let Some(original) = self.editor().read(cx).raw_html_at(pos) else {
            return;
        };
        let text = original
            .attrs()
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let setup = source_setup(text);
        let mut style = query_style(self.dark);
        style.padding = px(12.);
        let source = cx.new(|cx| {
            EditorView::new(setup, cx)
                .with_style(style)
                .with_aria_label("HTML source")
        });
        self.leave_input(cx);
        window.focus(&source.focus_handle(cx), cx);
        self.interaction.begin_html(HtmlEditor {
            note: self.library.active_id.clone(),
            pos,
            original,
            source,
            save_focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle(),
        });
        cx.notify();
    }

    pub(in crate::app) fn cancel_html_source(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.interaction.html() else {
            return false;
        };
        if edit.source.read(cx).is_composing() {
            edit.source
                .update(cx, |editor, cx| editor.cancel_composition(cx));
            return true;
        }
        self.interaction.end_html();
        self.focus_editor(window, cx);
        cx.notify();
        true
    }

    pub(super) fn save_html_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.interaction.html() else {
            return;
        };
        if edit.note != self.library.active_id {
            self.inform(
                "The original note changed. Copy this HTML draft or cancel.",
                cx,
            );
            self.focus_html_source(window, cx);
            return;
        }
        let Some(source) = source_to_save(edit.source.read(cx).state()) else {
            self.inform("Finish the current input before saving HTML.", cx);
            self.focus_html_source(window, cx);
            return;
        };
        let (pos, original) = (edit.pos, edit.original.clone());
        let saved = self.editor().update(cx, |editor, cx| {
            editor.set_raw_html_at(pos, &original, &source, cx)
        });
        if !saved {
            self.inform("The original HTML changed. Copy this draft or cancel.", cx);
            self.focus_html_source(window, cx);
            return;
        }
        self.interaction.end_html();
        self.focus_editor(window, cx);
        cx.notify();
    }

    pub(in crate::app) fn focus_html_source(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.interaction.html() else {
            return false;
        };
        window.focus(&edit.source.focus_handle(cx), cx);
        true
    }

    pub(super) fn html_focus_step(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.interaction.html() else {
            return false;
        };
        let handles = [
            edit.source.focus_handle(cx),
            edit.cancel_focus.clone(),
            edit.save_focus.clone(),
        ];
        let current = handles
            .iter()
            .position(|handle| handle.is_focused(window))
            .unwrap_or(0);
        window.focus(&handles[(current + if forward { 1 } else { 2 }) % 3], cx);
        true
    }

    pub(super) fn html_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.interaction.html() else {
            return false;
        };
        if matches!(key, "enter" | "space") && edit.save_focus.is_focused(window) {
            self.save_html_source(window, cx);
            return true;
        }
        if matches!(key, "enter" | "space") && edit.cancel_focus.is_focused(window) {
            self.cancel_html_source(window, cx);
            return true;
        }
        false
    }

    pub(super) fn html_source_popover(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let edit = self.interaction.html()?;
        let viewport = window.bounds().size;
        let width = px(560.).min(viewport.width - px(24.));
        let height = px(340.).min(viewport.height - px(72.));
        let source = edit.source.clone();
        let cancel_focus = edit.cancel_focus.clone();
        let save_focus = edit.save_focus.clone();
        let cancel = self
            .button("html-cancel", "Cancel", Intent::CancelHtml, cx)
            .track_focus(&cancel_focus)
            .focus(|s| s.border_1().border_color(rgb(0x4488cc)));
        let save = self
            .button("html-save", "Save · ⌘↩", Intent::SaveHtml, cx)
            .track_focus(&save_focus)
            .focus(|s| s.border_1().border_color(rgb(0x4488cc)));
        let dialog = div()
            .id("html-source-popover")
            .role(Role::Dialog)
            .aria_label("Edit HTML source")
            .absolute()
            .top((viewport.height - height) / 2.)
            .left((viewport.width - width) / 2.)
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .rounded(POPOVER_RADIUS)
            .bg(self.surface_color())
            .border_1()
            .border_color(self.border_color())
            .shadow(popover_shadow())
            .occlude()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(40.))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .child("Edit HTML source"),
            )
            .child(div().flex_1().min_h_0().child(source))
            .child(
                div()
                    .h(px(44.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(save),
            );
        Some(
            div()
                .id("html-source-modal")
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(dialog),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{source_document, source_setup, source_to_save};
    use markraft_core::{EditorState, EditorStateConfig, Extension, Selection, TransactionSpec};

    fn state_of(source: &str) -> (EditorState, markraft_gpui::DocTypes) {
        let setup = source_setup(source);
        let state = EditorState::create(
            EditorStateConfig::new(setup.schema)
                .doc(setup.doc.unwrap())
                .extensions(Extension::all([
                    markraft_core::composition(),
                    markraft_core::projection::projection(),
                ])),
        )
        .unwrap();
        (state, setup.types)
    }

    #[test]
    fn source_editor_accepts_multiline_paste_as_literal_text() {
        let (state, types) = state_of("<old>");
        let state = state
            .update([TransactionSpec::new().selection(Selection::All)])
            .unwrap();
        let source = "<span\n title=\"你好 **literal**\">\n\n";
        let paste = markraft_gpui::commands::insert_plain(&types, source)(state.state()).unwrap();
        let pasted = state.state().update([paste]).unwrap();
        assert_eq!(source_to_save(pasted.state()).as_deref(), Some(source));
    }

    #[test]
    fn saving_waits_for_input_method_completion() {
        let (state, _) = state_of("<span>");
        let started = state
            .update([markraft_core::start_composition(
                markraft_core::CompositionRange::new(2, 2),
            )])
            .unwrap();
        let candidate = markraft_core::update_composition(started.state(), "你", 1).unwrap();
        let composing = started.state().update([candidate]).unwrap();
        assert_eq!(source_to_save(composing.state()), None);
        assert!(markraft_core::is_composing(composing.state()));
        let committed = composing
            .state()
            .update([markraft_core::finish_composition()])
            .unwrap();
        assert_eq!(
            source_to_save(committed.state()).as_deref(),
            Some("<你span>")
        );
    }

    #[test]
    fn source_editor_preserves_multiline_html_and_literal_markdown() {
        for source in [
            "",
            "<span title=\"a  b\">",
            "<!--\n**literal** &amp;\n\n-->",
            "<tag>\n",
        ] {
            let (schema, doc) = source_document(source);
            assert_eq!(
                markraft_core::projection::Projection::of(&doc, &schema).plain_text(),
                source
            );
        }
    }
}
