//! Editor session lifetimes and workspace replacement.
use super::*;

pub(super) struct Session {
    editor: Entity<EditorView>,
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

impl Session {
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

impl NotesApp {
    pub(super) fn editor(&self) -> Entity<EditorView> {
        self.sessions
            .get(&self.library.active_id)
            .expect("the active note has an editor")
            .editor()
            .clone()
    }
    pub(super) fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        let restore_focus = self.interaction.note_changed(&id);
        if restore_focus {
            self.leave_input(cx);
        }
        if self.sessions.activate(&id) {
            if restore_focus {
                self.focus_editor(window, cx);
            }
            return;
        }
        let restore_focus = self.close_popover(cx) || restore_focus;
        let document = self.library.active_note().document.clone();
        let note = self.library.active_note();
        let image_base = note
            .path
            .as_ref()
            .and_then(|path| path.parent().map(ToOwned::to_owned));
        let protected = note.read_only.clone();
        let source = note
            .path
            .as_ref()
            .and(self.persistence.as_ref())
            .map(|persistence| {
                persistence.markdown(note.clone()).and_then(|text| {
                    markraft_commonmark::SourceDocument::parse(doc::schema(), &text)
                        .map_err(|error| error.to_string())
                })
            });
        let image_root = source
            .as_ref()
            .and_then(|source| source.as_ref().ok())
            .zip(note.path.as_ref())
            .map(|(source, path)| assets::image_root(source.source(), path))
            .unwrap_or(Ok(None));
        let style = notes_style(self.dark);
        let editor = cx.new(|cx| {
            EditorView::new(
                Setup::new(doc::schema().clone())
                    .types(doc::types().clone())
                    .codecs(doc::codecs())
                    .spelling(doc::spelling())
                    .mark_toggle(doc::mark_toggle(refusal_message))
                    .link_setter(doc::link_setter(refusal_message))
                    .split_wrap(doc::split_wrap())
                    .extensions(doc::extensions())
                    .doc(document),
                cx,
            )
            .with_style(style)
            .with_image_base(image_base)
            .with_image_root(image_root)
            .with_file_paste(true)
            .with_document_guard(move |candidate| {
                if let Some(reason) = &protected {
                    return Err(EditRejection::ReadOnly(reason.clone()));
                }
                match &source {
                    Some(Ok(source)) => source
                        .render(doc::schema(), candidate)
                        .map(|_| ())
                        .map_err(|error| {
                            let message = rejection_message(&error);
                            // The editor shades a protected span, so the boundary the
                            // keystroke landed in is already on screen; the other two
                            // have nowhere else to appear.
                            match error {
                                markraft_commonmark::SourceError::ProtectedSpan => {
                                    EditRejection::Marked(message)
                                }
                                _ => EditRejection::Protected(message),
                            }
                        }),
                    // The file was read but its Markdown could not be lined up with
                    // its source, so no keystroke could ever be written back.
                    Some(Err(error)) => Err(EditRejection::Invalid(format!(
                        "This file cannot be edited in Markraft: {error}"
                    ))),
                    None => Ok(()),
                }
            })
            .with_placeholder("Start writing…")
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
            // What the source codec will refuse, drawn before it is attempted.
            editor.set_protected_spans(markraft_commonmark::protected_spans, cx);
            [
                editor.add_extension(menu, cx),
                editor.add_extension(links, cx),
                editor.add_extension(markraft_gpui::emoji_menu(), cx),
                editor.add_extension(markraft_gpui::EmojiShortcodes, cx),
            ]
        });
        let vim = self
            .library
            .preferences
            .vim_mode
            .then(|| Self::attach_vim(&editor, cx));
        let note_id = id.clone();
        let changes = cx.subscribe_in(
            &editor,
            window,
            move |this, editor, event: &EditorEvent, window, cx| {
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
                        this.inform("Copied code", cx);
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
                if let EditorEvent::RawHtmlRequested { pos } = event {
                    if this.library.active_id == note_id
                        && this.interaction.panel() == Panel::Editor
                    {
                        this.open_html_source(*pos, window, cx);
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
                let document = editor.read(cx).committed_document().clone();
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
                _changes: changes,
                _state_changes: state_changes,
                _extensions: extensions,
                vim,
                vim_mode: markraft_vim::Mode::default(),
            },
        );
        if restore_focus {
            self.focus_editor(window, cx);
        }
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
        self.library = library;
        self.sessions.clear();
        self.save.reset();
        self.links.invalidate();
        self.ensure_session(window, cx);
    }
}
