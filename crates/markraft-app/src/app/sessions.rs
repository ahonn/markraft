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
        // A note not yet saved has no file for its edits to be written back through,
        // and its first save writes it whole. It is held to an empty source, which
        // refuses what a whole write could not say — the same edits a saved note's
        // guard refuses — rather than taking them and losing them on that first save.
        let source = source.or_else(|| {
            (note.path.is_none() && self.persistence.is_some()).then(|| {
                markraft_commonmark::SourceDocument::parse(doc::schema(), "")
                    .map_err(|error| error.to_string())
            })
        });
        let style = self.editor_style();
        let house = self.house.clone();
        let editor = cx.new(|cx| {
            EditorView::new(
                Setup::new(doc::schema().clone())
                    .types(doc::types().clone())
                    .codecs(doc::codecs(&self.house))
                    .spelling(doc::spelling())
                    .mark_toggle(doc::mark_toggle(refusal_message, &self.house))
                    .link_setter(doc::link_setter(refusal_message, &self.house))
                    .split_wrap(doc::split_wrap(&self.house))
                    .enter_rule(doc::enter_rule(self.shortcuts.clone()))
                    // Shift-Return writes the break the preferences ask for.
                    .break_spelling(std::sync::Arc::new(move || house.get().hard_break.marker()))
                    .extensions(doc::extensions(self.shortcuts.clone(), self.pairs.clone()))
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
                        .map_err(|_| EditRejection::Protected(UNSAVABLE_EDIT.to_owned())),
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
            editor.set_remote_images(self.remote_image_fetcher(), cx);
            editor.set_indent_text(self.library.preferences.tab_key.text(), cx);
            [
                editor.add_extension(menu, cx),
                editor.add_extension(links, cx),
                editor.add_extension(markraft_gpui::emoji_menu(self.emoji.clone()), cx),
                editor.add_extension(markraft_gpui::EmojiShortcodes::new(self.emoji.clone()), cx),
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

impl MarkraftApp {
    /// What a note editor fetches remote images with: nothing while the preference
    /// is off, so each one reads as an image this editor does not load.
    fn remote_image_fetcher(&self) -> Option<markraft_gpui::RemoteImageFetcher> {
        self.library
            .preferences
            .remote_images
            .then(crate::remote_images::shared)
    }

    /// Write emoji characters rather than shortcodes, or back, in every open note at
    /// once: their menus and auto-replace share [`MarkraftApp::emoji`].
    pub(super) fn set_emoji_characters(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.library.preferences.emoji_characters == enabled {
            return;
        }
        self.library.preferences.emoji_characters = enabled;
        self.emoji.set_characters(enabled);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Turn the Markdown input rules on or off in every open note at once: their rules
    /// all read the one flag.
    pub(super) fn set_markdown_shortcuts(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.library.preferences.markdown_shortcuts = enabled;
        self.shortcuts
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Turn bracket and quote pairing on or off in every open note at once.
    pub(super) fn set_auto_pair(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.library.preferences.auto_pair = enabled;
        self.pairs
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_tab_key(&mut self, key: crate::storage::TabKey, cx: &mut Context<Self>) {
        self.library.preferences.tab_key = key;
        let editors: Vec<_> = self
            .sessions
            .values()
            .map(|session| session.editor().clone())
            .collect();
        for editor in editors {
            editor.update(cx, |editor, cx| editor.set_indent_text(key.text(), cx));
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// The typeface or line height changed: every open note is set again.
    pub(super) fn set_typography(
        &mut self,
        font: crate::storage::EditorFont,
        line_height: crate::storage::LineHeight,
        cx: &mut Context<Self>,
    ) {
        self.library.preferences.font = font;
        self.library.preferences.line_height = line_height;
        self.restyle_editors(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// The markers new syntax is written with. What is already written stays as it is.
    pub(super) fn set_markdown_markers(
        &mut self,
        bullet: crate::storage::BulletMarker,
        fence: crate::storage::CodeFence,
        emphasis: crate::storage::EmphasisMarker,
        cx: &mut Context<Self>,
    ) {
        let preferences = &mut self.library.preferences;
        preferences.bullet_marker = bullet;
        preferences.code_fence = fence;
        preferences.emphasis_marker = emphasis;
        super::apply_markdown_style(&self.house, preferences);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Turn fetching remote images on or off in every open note at once.
    pub(super) fn set_remote_images(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.library.preferences.remote_images == enabled {
            return;
        }
        self.library.preferences.remote_images = enabled;
        let fetcher = self.remote_image_fetcher();
        let editors: Vec<_> = self
            .sessions
            .values()
            .map(|session| session.editor().clone())
            .collect();
        for editor in editors {
            editor.update(cx, |editor, cx| {
                editor.set_remote_images(fetcher.clone(), cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }
}
