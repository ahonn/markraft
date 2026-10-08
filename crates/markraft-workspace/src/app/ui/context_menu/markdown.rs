//! Markdown commands that have no native text-system equivalent.

use super::*;
use markraft_core::{EditorState, Fragment, Selection, Slice, TransactionSpec};

#[derive(Clone, Copy)]
pub(super) enum MarkdownAction {
    ClearFormatting,
    InsertMath,
    InsertImage,
}

fn math_insertion(state: &EditorState) -> Option<TransactionSpec> {
    let selection = state.selection();
    let doc = state.doc();
    let from = doc.resolve(selection.from(doc)).ok()?;
    let to = doc.resolve(selection.to(doc)).ok()?;
    if from.depth() == 0
        || !from.parent().is_textblock(state.schema())
        || !from.same_parent(&to)
        || doc::types().in_verbatim_block_at(state)
        || doc::types()
            .table_types()
            .and_then(|types| markraft_core::commands::cell_at(types, state))
            .is_some()
    {
        return None;
    }
    let schema = state.schema();
    let line_break = schema
        .node(markraft_commonmark::schema::LINE_BREAK, [])
        .ok()?;
    let paragraph = schema
        .node(
            markraft_commonmark::schema::PARAGRAPH,
            [
                schema.text("$$"),
                line_break.clone(),
                line_break,
                schema.text("$$"),
            ],
        )
        .ok()?;
    // A block insert goes after this paragraph and keeps selected prose intact.
    let position = from.after(from.depth());
    markraft_core::commands::changes_spec(
        state,
        vec![markraft_core::Change::insert(
            position,
            Slice::from_fragment(Fragment::from_node(paragraph)),
        )],
        "input.block",
    )
    .map(|spec| spec.selection(Selection::cursor(position + 4)))
}

impl MarkraftApp {
    pub(super) fn markdown_action_enabled(&self, action: MarkdownAction, cx: &App) -> bool {
        let editor = self.editor().read(cx);
        if self.is_reloading()
            || self.notes.library.active_note().read_only.is_some()
            || editor.is_composing()
        {
            return false;
        }
        match action {
            MarkdownAction::ClearFormatting => markraft_commonmark::Formatter::new(
                self.house.clone(),
            )
            .clear_formatting()(editor.state())
            .is_ok_and(|spec| spec.is_some()),
            MarkdownAction::InsertMath => math_insertion(editor.state()).is_some(),
            MarkdownAction::InsertImage => {
                !doc::types().in_verbatim_block_at(editor.state())
                    && self.notes.persistence.as_ref().is_some_and(|persistence| {
                        let capabilities = persistence.capabilities();
                        capabilities.file_operations || capabilities.assets
                    })
            }
        }
    }

    pub(super) fn run_markdown_action(
        &mut self,
        action: MarkdownAction,
        request: &ContextRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MarkdownAction::ClearFormatting => {
                let command =
                    markraft_commonmark::Formatter::new(self.house.clone()).clear_formatting();
                let result = command(self.editor().read(cx).state());
                match result {
                    Ok(Some(spec)) => {
                        self.editor()
                            .update(cx, |editor, cx| editor.dispatch_isolated([spec], cx));
                    }
                    Err(error) => {
                        self.feedback.set_error(refusal_message(&error));
                    }
                    Ok(None) => {}
                }
                self.focus_editor(window, cx);
            }
            MarkdownAction::InsertMath => {
                self.editor().update(cx, |editor, cx| {
                    if let Some(spec) = math_insertion(editor.state()) {
                        editor.dispatch_isolated([spec], cx);
                    }
                });
                self.focus_editor(window, cx);
            }
            MarkdownAction::InsertImage => {
                let note = self.notes.library.active_id.clone();
                let editor = self.editor().downgrade();
                let request = request.clone();
                let prompt = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: None,
                });
                let prompt = self.file_panel(prompt, window, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(paths))) = prompt.await else {
                        return;
                    };
                    let _ = cx.update(|window, cx| {
                        this.update(cx, |app, cx| {
                            if app.notes.library.active_id != note
                                || app.is_reloading()
                                || !editor.upgrade().is_some_and(|editor| {
                                    editor == app.editor()
                                        && editor.read(cx).context_is_current(&request)
                                })
                                || !app.markdown_action_enabled(MarkdownAction::InsertImage, cx)
                            {
                                return;
                            }
                            app.insert_context_assets(
                                paths.into_iter().map(assets::Asset::File).collect(),
                                request.clone(),
                                window,
                                cx,
                            );
                            app.focus_editor(window, cx);
                        })
                    });
                })
                .detach();
            }
        }
    }
}

#[cfg(test)]
mod tests;
