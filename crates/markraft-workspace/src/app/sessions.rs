//! Editor session lifetimes and workspace replacement.
use super::ui::CheckTrigger;
use super::*;
use crate::EditorLocaleExt;
use crate::locale::Message;
use markraft_commonmark::SourceTrack;
use std::sync::{Arc, Mutex};

pub(super) struct Session {
    editor: Entity<EditorView>,
    read_only: std::rc::Rc<std::cell::RefCell<Option<Message>>>,
    image_root: Result<Option<PathBuf>, Message>,
    edit_messages: EditMessages,
    _changes: Subscription,
    /// Everything else the editor's state does, a selection that moved without an edit
    /// included: the format toolbar reads it.
    _state_changes: Subscription,
    /// Unregisters the note's editor extensions when the session is evicted.
    _extensions: [ExtensionHandle; 4],
    /// Modal editing, while the preference is on. Dropping the handle turns it off.
    vim: Option<ExtensionHandle>,
    /// The mode this note's editor last reported. It belongs beside the handle:
    /// a session that has just been given modal editing, or had it taken away,
    /// is in the mode a fresh editor starts in, and the two must never be set
    /// apart — a badge reading `INSERT` over an editor with no vim at all is
    /// what setting one without the other looks like.
    vim_mode: markraft_vim::Mode,
}

/// The reusable editor carries strings; retain app-owned refusals alongside
/// the exact rejection that carried them, until the app drains its error slot.
#[derive(Clone, Default)]
struct EditMessages(Arc<Mutex<Option<(EditRejection, Message)>>>);

impl EditMessages {
    fn reject(
        &self,
        message: Message,
        i18n: &crate::locale::I18n,
        kind: fn(String) -> EditRejection,
    ) -> EditRejection {
        let rejection = kind(message.render(i18n));
        *self.0.lock().expect("the session edit-message lock") = Some((rejection.clone(), message));
        rejection
    }

    fn take(&self, rejection: Option<&EditRejection>) -> Option<Message> {
        self.0
            .lock()
            .expect("the session edit-message lock")
            .take()
            .filter(|(recorded, _)| Some(recorded) == rejection)
            .map(|(_, message)| message)
    }
}

impl Session {
    pub(super) fn take_edit_error(
        &self,
        cx: &mut Context<MarkraftApp>,
    ) -> Option<(EditRejection, Option<Message>)> {
        let rejection = self.editor.update(cx, |editor, _| editor.take_edit_error());
        // Drain even when the editor reports no error or a different one. A
        // callback's prior message must never attach to a later rejection.
        let message = self.edit_messages.take(rejection.as_ref());
        rejection.map(|rejection| (rejection, message))
    }

    pub(super) fn set_locale(&self, i18n: &crate::locale::I18n, cx: &mut Context<MarkraftApp>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_messages(i18n.editor_messages(), cx);
            editor.set_placeholder(i18n.text("input.start-writing"), cx);
            editor.set_image_root(
                self.image_root.clone().map_err(|error| error.render(i18n)),
                cx,
            );
        });
    }
    pub(super) fn set_read_only(&self, reason: Option<Message>) {
        *self.read_only.borrow_mut() = reason;
    }
    pub(super) fn editor(&self) -> &Entity<EditorView> {
        &self.editor
    }

    /// The mode this note's editor is in, which is `Normal` for one that has no
    /// modal editing.
    pub(super) fn vim_mode(&self) -> markraft_vim::Mode {
        self.vim_mode
    }

    /// Give this session modal editing, or take it away. Assigning drops the
    /// previous handle, which unregisters it, and the mode goes back to what a
    /// fresh editor reports.
    pub(super) fn set_vim(&mut self, handle: Option<ExtensionHandle>) {
        self.vim = handle;
        self.vim_mode = markraft_vim::Mode::default();
    }

    /// The mode the editor just reported.
    pub(super) fn report_mode(&mut self, mode: markraft_vim::Mode) {
        self.vim_mode = mode;
    }
}

#[derive(Default)]
pub(super) struct Sessions {
    entries: VecDeque<(String, Session)>,
}
impl Sessions {
    pub fn get(&self, id: &str) -> Option<&Session> {
        self.entries
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, session)| session)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.entries
            .iter_mut()
            .find(|(key, _)| key == id)
            .map(|(_, session)| session)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Session)> {
        self.entries.iter().map(|(id, session)| (id, session))
    }
    /// The session activated or inserted last, which is the note on screen.
    pub fn current(&self) -> Option<(&String, &Session)> {
        self.entries.back().map(|(id, session)| (id, session))
    }
    pub fn values(&self) -> impl Iterator<Item = &Session> {
        self.entries.iter().map(|(_, session)| session)
    }
    pub fn activate(&mut self, id: &str) -> bool {
        let Some(index) = self.entries.iter().position(|(key, _)| key == id) else {
            return false;
        };
        let entry = self
            .entries
            .remove(index)
            .expect("the cached session exists");
        self.entries.push_back(entry);
        true
    }
    pub fn insert(&mut self, id: String, session: Session) {
        self.remove(&id);
        self.entries.push_back((id, session));
        while self.entries.len() > 8 {
            self.entries.pop_front();
        }
    }
    pub fn remove(&mut self, id: &str) {
        self.entries.retain(|(key, _)| key != id);
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl MarkraftApp {
    pub(super) fn editor(&self) -> Entity<EditorView> {
        self.sessions
            .get(&self.library.active_id)
            .expect("the active note has an editor")
            .editor()
            .clone()
    }
    pub(super) fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        if self.find_vim.as_ref().is_some_and(|find| find.note != id) {
            self.cancel_vim_find(cx);
        }
        let restore_focus = self.interaction.note_changed(&id);
        if restore_focus {
            self.leave_input(cx);
        }
        // The note going off screen keeps its editor for a quick return, but
        // not the layout of every line it holds: that is most of what an open
        // note costs, and the next frame that shows it lays it out again.
        if let Some((_, previous)) = self.sessions.current().filter(|(key, _)| **key != id) {
            previous
                .editor()
                .update(cx, |editor, _| editor.release_layout());
        }
        if self.sessions.activate(&id) {
            self.reconcile_vim_find(window, cx);
            if restore_focus && !self.find_open {
                self.focus_editor(window, cx);
            }
            self.sync_find(cx);
            self.schedule_text_checking(CheckTrigger::Changed, cx);
            return;
        }
        let restore_focus = self.close_popover(cx) || restore_focus;
        let document = self.library.active_note().document.clone();
        let note = self.library.active_note();
        let image_base = note
            .path
            .as_ref()
            .and_then(|path| path.parent().map(ToOwned::to_owned));
        let read_only = std::rc::Rc::new(std::cell::RefCell::new(note.read_only.clone()));
        let protected = read_only.clone();
        let reloading = self.reloading.clone();
        // The editor checks each keystroke against the track the store saves
        // through, so what it takes is what a save writes.
        let source = note
            .path
            .as_ref()
            .and(self.persistence.as_ref())
            .map(|persistence| {
                match persistence.source(note.clone()) {
                    Ok(Some(track)) => Ok(track),
                    // A file the store holds no copy of is written whole.
                    Ok(None) => persistence.markdown(note.clone()).and_then(|text| {
                        markraft_commonmark::SourceDocument::parse(doc::schema(), &text)
                            .map(|source| Arc::new(SourceTrack::new(source)))
                            .map_err(|error| error.to_string().into())
                    }),
                    Err(error) => Err(error),
                }
                .map_err(|error| error.message())
            });
        let image_root = source
            .as_ref()
            .and_then(|source| source.as_ref().ok())
            .zip(note.path.as_ref())
            .map(|(source, path)| assets::image_root(source.origin().source(), path))
            .unwrap_or(Ok(None));
        // A note not yet saved has no file for its edits to be written back through,
        // and its first save writes it whole. It is held to an empty source, which
        // refuses what a whole write could not say — the same edits a saved note's
        // guard refuses — rather than taking them and losing them on that first save.
        let source = source.or_else(|| {
            (note.path.is_none() && self.persistence.is_some()).then(|| {
                markraft_commonmark::SourceDocument::parse(doc::schema(), "")
                    .map(|source| Arc::new(SourceTrack::new(source)))
                    .map_err(|error| Message::from(error.to_string()))
            })
        });
        let style = self.editor_style();
        let edit_messages = EditMessages::default();
        let formatting_messages = edit_messages.clone();
        let guard_messages = edit_messages.clone();
        let format_locale = self.locale_state.clone();
        let kind =
            doc::MarkdownKind::new(self.house.clone(), self.shortcuts.clone(), move |refusal| {
                formatting_messages
                    .reject(
                        refusal_message(refusal),
                        &format_locale.read().expect("the app locale lock"),
                        EditRejection::Refused,
                    )
                    .message()
                    .to_owned()
            });
        let guard_locale = self.locale_state.clone();
        let editor = cx.new(|cx| {
            EditorView::new(
                Setup::new(doc::schema().clone())
                    .types(doc::types().clone())
                    .kind(std::sync::Arc::new(kind))
                    .extensions(doc::extensions_with_quotes(
                        self.shortcuts.clone(),
                        self.pairs.clone(),
                        self.quote_pairs.clone(),
                    ))
                    .doc(document),
                cx,
            )
            .with_style(style)
            .with_messages(self.i18n.editor_messages())
            .with_image_base(image_base)
            .with_image_root(image_root.clone().map_err(|error| error.render(&self.i18n)))
            .with_file_paste(true)
            .with_smart_insert_delete(self.preferences.text_checking.smart_insert_delete)
            .with_transaction_guard(move |transactions| {
                let locale = guard_locale.read().expect("the app locale lock");
                if reloading.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(guard_messages.reject(
                        Message::new("notice.reloading"),
                        &locale,
                        EditRejection::ReadOnly,
                    ));
                }
                if let Some(reason) = protected.borrow().as_ref() {
                    return Err(guard_messages.reject(
                        reason.clone(),
                        &locale,
                        EditRejection::ReadOnly,
                    ));
                }
                match &source {
                    Some(Ok(source)) => source
                        .apply_transactions(doc::schema(), transactions)
                        .map_err(|_| {
                            guard_messages.reject(
                                Message::new(UNSAVABLE_EDIT),
                                &locale,
                                EditRejection::Protected,
                            )
                        }),
                    // The file was read but its Markdown could not be lined up with
                    // its source, so no keystroke could ever be written back.
                    Some(Err(error)) => Err(guard_messages.reject(
                        Message::new("refusal.invalid-file").arg("error", error),
                        &locale,
                        EditRejection::Invalid,
                    )),
                    None => Ok(()),
                }
            })
            .with_placeholder(self.i18n.text("input.start-writing"))
        });
        // Only note editors get the menus; the host's query field gets no extension.
        // The `/` menu is registered first: the three typeaheads derive from the same
        // caret and their triggers are disjoint, so only one is ever open, but were they
        // ever to overlap the first registered one would own the popup and the commands
        // matter more than a link, and a link more than an emoji. Auto-replace goes last
        // so that it sees the menu's view of a keystroke settled before it edits.
        self.refresh_link_targets();
        let menu = self.slash_menu();
        let links = self.wiki_menu();
        let resolver = self.wiki_resolver();
        let extensions = editor.update(cx, |editor, cx| {
            editor.set_wiki_resolver(resolver, cx);
            editor.set_remote_images(self.remote_image_fetcher(), cx);
            editor.set_animate_images(self.preferences.animate_images, cx);
            editor.set_auto_number_equations(self.preferences.auto_number_equations, cx);
            editor.set_indent_text(self.preferences.tab_key.text(), cx);
            [
                editor.add_extension(menu, cx),
                editor.add_extension(links, cx),
                editor.add_extension(markraft_gpui::emoji_menu(self.emoji.clone()), cx),
                editor.add_extension(markraft_gpui::EmojiShortcodes::new(self.emoji.clone()), cx),
            ]
        });
        let vim = self
            .preferences
            .vim_mode
            .then(|| Self::attach_vim(&editor, cx));
        let note_id = id.clone();
        let changes = cx.subscribe_in(
            &editor,
            window,
            move |this, editor, event: &EditorEvent, window, cx| {
                if let EditorEvent::ContextMenuRequested(request) = event {
                    if this.library.active_id == note_id {
                        this.request_context_menu(editor, request.clone(), window, cx);
                    }
                    return;
                }
                if let EditorEvent::FilesPasted(item) = event {
                    if this.library.active_id == note_id {
                        this.insert_assets(assets::from_clipboard(item.clone()), window, cx);
                    }
                    return;
                }
                if let EditorEvent::Extension { id, payload } = event {
                    if *id == markraft_vim::VIM {
                        this.vim_effect(&note_id, payload, cx);
                    } else if *id == ui::slash::SLASH_MENU && this.library.active_id == note_id {
                        this.slash_effect(payload, window, cx);
                    }
                    return;
                }
                if matches!(event, EditorEvent::CodeCopied) {
                    if this.library.active_id == note_id {
                        this.inform(Message::new("notice.copied-code"), cx);
                    }
                    return;
                }
                if let EditorEvent::CodeLanguageRequested { pos } = event {
                    if this.library.active_id == note_id
                        && this.interaction.panel() == Panel::Editor
                    {
                        this.open_code_language(*pos, cx);
                    }
                    return;
                }
                if let EditorEvent::WikiLinkClicked { target, embed } = event {
                    if this.library.active_id == note_id
                        && this.interaction.panel() == Panel::Editor
                    {
                        this.follow_wiki_link(target, *embed, window, cx);
                    }
                    return;
                }
                if matches!(event, EditorEvent::LinkClicked) {
                    if this.library.active_id == note_id
                        && this.interaction.panel() == Panel::Editor
                    {
                        this.close_popover(cx);
                        this.show_popover(Popover::Link(LinkPopover::View), cx);
                        cx.notify();
                    }
                    return;
                }
                if this.library.active_id == note_id
                    && matches!(
                        this.interaction.popover(),
                        Some(Popover::Link(_) | Popover::CodeLanguage(_))
                    )
                {
                    this.close_popover(cx);
                }
                if this.library.active_id == note_id {
                    let trigger = if matches!(
                        event,
                        EditorEvent::Changed {
                            text_input: true,
                            ..
                        }
                    ) {
                        CheckTrigger::Typed
                    } else {
                        CheckTrigger::Changed
                    };
                    this.schedule_text_checking(trigger, cx);
                }
                let document = editor.read(cx).committed_document().clone();
                this.reconcile_vim_find(window, cx);
                let title = this.library.note(&note_id).map(|note| note.title());
                if this.library.set_document(&note_id, document) {
                    this.links.invalidate_if(
                        title != this.library.note(&note_id).map(|note| note.title()),
                    );
                    this.schedule_save(cx);
                }
                cx.notify();
            },
        );
        let state_note_id = id.clone();
        let state_changes = cx.observe(&editor, move |this, editor, cx| {
            if this.library.active_id != state_note_id {
                return;
            }
            this.invalidate_text_service(cx);
            this.sync_checking_panel(cx);
            if this.toolbar.formats_changed(|| {
                let editor = editor.read(cx);
                (
                    editor.active_marks(),
                    doc::Block::active(editor.state(), &editor.projection()),
                )
            }) {
                cx.notify();
            }
        });
        self.sessions.insert(
            id,
            Session {
                editor,
                read_only,
                image_root,
                edit_messages,
                _changes: changes,
                _state_changes: state_changes,
                _extensions: extensions,
                vim,
                vim_mode: markraft_vim::Mode::default(),
            },
        );
        self.reconcile_vim_find(window, cx);
        if restore_focus && !self.find_open {
            self.focus_editor(window, cx);
        }
        self.sync_find(cx);
        self.schedule_text_checking(CheckTrigger::Changed, cx);
    }
    /// Apply an application edit through the editor's source guard and history.
    /// Composition is left untouched; callers can report that note as skipped.
    pub(super) fn edit_session_document(
        &mut self,
        id: &str,
        document: Node,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = self.sessions.get(id) else {
            return false;
        };
        let editor = session.editor().clone();
        let accepted = editor.update(cx, |editor, cx| {
            if editor.is_composing() {
                return false;
            }
            let Some(spec) = doc::document_edit(editor.committed_document(), &document) else {
                return false;
            };
            editor.dispatch_isolated([spec], cx)
        });
        if accepted {
            let title = self.library.note(id).map(|note| note.title());
            if self
                .library
                .set_document(id, editor.read(cx).committed_document().clone())
            {
                self.links
                    .invalidate_if(title != self.library.note(id).map(|note| note.title()));
                self.schedule_save(cx);
            }
        }
        accepted
    }

    pub(super) fn sync_documents(&mut self, cx: &App) {
        for (id, session) in self.sessions.iter() {
            let document = session.editor().read(cx).committed_document().clone();
            let title = self.library.note(id).map(|note| note.title());
            if self.library.set_document(id, document) {
                self.links
                    .invalidate_if(title != self.library.note(id).map(|note| note.title()));
                self.save.schedule(Instant::now());
            }
        }
    }

    pub(super) fn replace_library(
        &mut self,
        library: Library,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_input(cx);
        self.io.reset();
        self.save_waiters.clear();
        self.library = library;
        self.sessions.clear();
        self.save.reset();
        self.links.invalidate();
        self.ensure_session(window, cx);
    }
}

impl MarkraftApp {
    /// What a note editor fetches remote images with: nothing while the preference
    /// is off, so each one reads as an image this editor does not load.
    pub(in crate::app) fn remote_image_fetcher(&self) -> Option<markraft_gpui::RemoteImageFetcher> {
        self.preferences
            .remote_images
            .then(|| self.remote_fetcher.clone())
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::EditMessages;
    use crate::locale::{I18n, LanguagePreference, Message};
    use markraft_gpui::EditRejection;

    #[test]
    fn a_pending_rejection_keeps_its_message_when_the_language_changes() {
        let messages = EditMessages::default();
        let semantic =
            Message::new("refusal.invalid-file").arg("error", Message::new("error.file-read-only"));
        let rejection = messages.reject(semantic.clone(), &I18n::english(), EditRejection::Invalid);
        let traditional = I18n::for_preference(&LanguagePreference::Locale("zh-Hant".into()));
        let delivered = messages.take(Some(&rejection)).unwrap();
        assert_eq!(delivered, semantic);
        assert_ne!(delivered.render(&traditional), rejection.message());
        assert!(messages.take(Some(&rejection)).is_none());
    }

    #[test]
    fn unmatched_rejections_and_empty_polls_discard_old_message_records() {
        let messages = EditMessages::default();
        let rejection = messages.reject(
            Message::new("refusal.unsavable"),
            &I18n::english(),
            EditRejection::Protected,
        );
        // Even identical wording from a different error kind is unrelated.
        let unrelated = EditRejection::Invalid(rejection.message().to_owned());
        assert!(messages.take(Some(&unrelated)).is_none());
        assert!(messages.take(Some(&rejection)).is_none());
        let rejection = messages.reject(
            Message::new("refusal.unsavable"),
            &I18n::english(),
            EditRejection::Protected,
        );
        assert!(messages.take(None).is_none());
        assert!(messages.take(Some(&rejection)).is_none());
    }

    #[gpui::test]
    fn an_accepted_host_edit_schedules_its_own_save(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("Welcome.md", "Original\n")], |_| {});
        // The window settles its first size, which is saved as a preference,
        // on a later frame; let it before the workspace has to be clean.
        h.pass_time(std::time::Duration::from_secs(1));
        h.save();
        let id = h.active_note().id;
        let app = h.app.clone();
        h.cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                assert!(!app.save.is_dirty());
                assert!(app.edit_session_document(&id, crate::doc::from_markdown("Changed"), cx));
                assert!(app.save.is_dirty());
                assert!(app.library.changes.contains_key(&id));
            });
        });
        h.keys("cmd-z");
        assert_eq!(h.markdown(), "Original");
        h.keys("cmd-shift-z");
        assert_eq!(h.markdown(), "Changed");
        h.save();
        assert_eq!(
            std::fs::read_to_string(h.notes.join("Welcome.md")).unwrap(),
            "Changed\n"
        );
    }

    #[gpui::test]
    fn host_dispatch_splits_an_open_typing_group(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("Welcome.md", "Original\n")], |_| {});
        let id = h.active_note().id;
        let app = h.app.clone();
        h.cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                app.editor()
                    .update(cx, |editor, _| editor.begin_undo_group())
            });
        });
        h.keys("cmd-down cmd-right");
        h.type_text(" first");
        h.cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                assert!(app.edit_session_document(&id, crate::doc::from_markdown("Changed"), cx));
            });
        });
        h.keys("cmd-down cmd-right");
        h.type_text(" last");
        h.cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                app.editor().update(cx, |editor, _| editor.end_undo_group())
            });
        });
        for expected in ["Changed", "Original first", "Original"] {
            h.keys("cmd-z");
            assert_eq!(h.markdown(), expected);
        }
    }

    #[gpui::test]
    fn a_rejected_host_edit_keeps_document_source_and_dirty_generation(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut h = crate::e2e::harness::open_with(cx, &[("Welcome.md", "Original\n")], |_| {});
        let id = h.active_note().id;
        let app = h.app.clone();
        h.cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                let before = app.library.active_note().document.clone();
                let generations = app.library.changes.clone();
                let track = app
                    .persistence
                    .as_ref()
                    .unwrap()
                    .source(app.library.active_note().clone())
                    .unwrap()
                    .unwrap();
                let source = track
                    .snapshot()
                    .render(crate::doc::schema(), &before)
                    .unwrap();
                app.sessions
                    .get(&id)
                    .unwrap()
                    .set_read_only(Some("Protected".into()));
                assert!(!app.edit_session_document(&id, crate::doc::from_markdown("Changed"), cx));
                assert_eq!(app.library.active_note().document, before);
                assert_eq!(app.editor().read(cx).committed_document(), &before);
                assert_eq!(app.library.changes, generations);
                assert_eq!(
                    track
                        .snapshot()
                        .render(crate::doc::schema(), &before)
                        .unwrap(),
                    source
                );
            });
        });
    }
}
