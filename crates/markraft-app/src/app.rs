mod assets;
mod rename;
mod ui;

use crate::doc;
use crate::{
    instance::Instance,
    persistence::{Event, Persistence},
    platform::{Platform, PlatformEvent},
    storage::Library,
    updater::Updater,
    vault::{External, Store},
};
use gpui::{prelude::*, *};
use markraft_core::{MarkSet, Node, Selection};
use markraft_gpui::{
    ColumnAlignment, EditRejection, EditorEvent, EditorStyle, EditorView, ExtensionHandle, Setup,
    TableInfo,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

actions!(
    markraft_app,
    [
        Save,
        CopyMarkdown,
        Quit,
        CheckForUpdates,
        Hide,
        Show,
        NewNote,
        Browse,
        Actions,
        Settings,
        Link,
        Export,
        OpenMarkdown
    ]
);

/// Which of the library's notes a panel lists.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Notes,
    /// Work that is not in a file the way it was left; see [`is_draft`].
    Drafts,
    Deleted,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Editor,
    Browse,
    /// Work that is not in a file the way the user left it: a note with no file yet,
    /// and one whose file says something else. The same list Browse draws, filtered.
    Drafts,
    Trash,
    Actions,
    Settings,
}
/// The pill above a link: its actions, or the field that edits its address.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkPopover {
    View,
    Edit,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormatMenu {
    Block,
    Inline,
    List,
}
/// A transient message over the note. One that carries an action shows it as a button
/// and stays longer, because reaching that button takes a moment.
#[derive(Clone)]
struct Notice {
    text: SharedString,
    until: Instant,
    /// The note an "Undo" button puts back; a plain notice carries none.
    undo: Option<String>,
}
struct Session {
    editor: Entity<EditorView>,
    _changes: Subscription,
    /// Everything else the editor's state does, a selection that moved without an edit
    /// included: the format toolbar reads it, and so does the line a new note's file
    /// would be named after.
    _state_changes: Subscription,
    /// Unregisters the note's editor extensions when the session is evicted.
    _extensions: [ExtensionHandle; 4],
    /// Modal editing, while the preference is on. Dropping the handle turns it off.
    vim: Option<ExtensionHandle>,
    /// The mode this note's editor last reported.
    vim_mode: markraft_vim::Mode,
}
pub struct NotesApp {
    library: Library,
    persistence: Option<Persistence>,
    /// The notes folder, once one has been chosen.
    path: Option<PathBuf>,
    settings_path: PathBuf,
    platform: Option<Platform>,
    updater: Updater,
    instance: Instance,
    sessions: HashMap<String, Session>,
    session_order: VecDeque<String>,
    query: Entity<EditorView>,
    _query_changes: Subscription,
    panel: Panel,
    panel_focus: FocusHandle,
    /// The stop id of the chrome control Tab has moved to. `None` while the caret owns
    /// the keyboard and nothing wears the focus ring.
    chrome_focus: Option<SharedString>,
    selected: usize,
    /// The deleted note whose "Delete Permanently" button has been asked once and is
    /// waiting for the confirming second click.
    confirm_purge: Option<String>,
    picker_scroll: ScrollHandle,
    actions_scroll: ScrollHandle,
    settings_scroll: ScrollHandle,
    format_scroll: ScrollHandle,
    code_language_scroll: ScrollHandle,
    code_language_block: Option<usize>,
    code_language_focus_pending: bool,
    code_language_selected: usize,
    html_editor: Option<ui::html::HtmlEditor>,
    dirty: bool,
    revision: u64,
    save_at: Option<Instant>,
    /// The never-filed note whose first line the caret is still in. Autosave holds it
    /// in recovery rather than naming its file after half a title; see
    /// [`NotesApp::follow_title`].
    held_draft: Option<String>,
    /// Drafts whose name has settled. They are filed by the next save and never held
    /// again, so a caret wandering back into the first line — or a filing that failed
    /// and must be reported — cannot put one back into recovery.
    released_drafts: HashSet<String>,
    /// When the held draft's name stops being worked on. Typing pushes it back; the
    /// poll files the note once it passes.
    name_at: Option<Instant>,
    error: Option<String>,
    platform_error: Option<String>,
    notice: Option<Notice>,
    /// Things the notes folder gave the user to read. They are sentences rather than
    /// acknowledgments, so they wait their turn instead of replacing one another.
    pending_notices: VecDeque<String>,
    conflict_prompted: HashSet<String>,
    conflict_dialog: bool,
    /// The card over the lower-left indicator: why this file cannot be written, and
    /// the ways out of that. Only a read-only note has one; a conflict opens its
    /// dialog instead of a card.
    file_status_popover: bool,
    /// Until when that indicator stays lit, after a keystroke the file refused. It
    /// is attention rather than a message, so it expires on its own.
    file_status_flash: Option<Instant>,
    show_words: bool,
    format_toolbar: bool,
    format_menu: Option<FormatMenu>,
    link_popover: Option<LinkPopover>,
    /// The pill under the title that gives the note's file another name.
    rename: Option<rename::Rename>,
    /// The table the toolbar was last drawn for. It is paint geometry, so it is only
    /// ever as fresh as the last frame, which is also the frame the keyboard walks.
    table: Option<TableInfo>,
    format_selected: usize,
    format_snapshot: Option<(MarkSet, Option<doc::Block>)>,
    dark: bool,
    // Corner action buttons and traffic lights follow window hover alone.
    pointer_inside: bool,
    /// Whether the notes folder had to be made on open because the one the settings
    /// named was gone.
    folder_was_created: bool,
    /// Whether the platform's close button is currently shown. It follows window hover
    /// like the rest of the chrome, but also stands down for a popup it would cover.
    close_button_shown: bool,
    /// The notes the `[[` menu offers, shared with the editor's provider so that a note
    /// written after this editor opened can still be linked to.
    link_targets: ui::wiki::LinkTargets,
    /// Every spelling that reaches a note, for the editor's question about each wiki
    /// link it draws.
    link_index: ui::wiki::LinkIndex,
    /// The revision the shared list was built from, so it is rebuilt when the library
    /// moves on rather than on every tick.
    link_targets_revision: Option<u64>,
    /// When the keyboard was last used here. Someone typing is present even with the
    /// pointer parked outside the window, so the chrome stays up for a moment after.
    last_key_at: Option<Instant>,
    chrome_shown: bool,
    window_active: bool,
    expected_size: Option<Size<Pixels>>,
    last_size: Size<Pixels>,
    _poll: Task<()>,
    _bounds: Subscription,
    _appearance: Subscription,
    _activation: Subscription,
    _quit: Subscription,
}
impl NotesApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: Option<PathBuf>,
        settings_path: PathBuf,
        store: Option<Store>,
        library: Library,
        error: Option<String>,
        platform: Result<Platform, String>,
        instance: Instance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = path.map(|path| path.canonicalize().unwrap_or(path));
        let folder_was_created = store.as_ref().is_some_and(Store::created_folder);
        let dark = library.preferences.dark_mode.unwrap_or(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        let query = cx.new(|cx| {
            EditorView::single_line(cx)
                .with_style(query_style(dark))
                .with_placeholder("Search notes…")
                .with_aria_label("Search notes")
        });
        let query_changes = cx.subscribe(&query, |this, _, event: &EditorEvent, cx| {
            if !matches!(event, EditorEvent::Changed { .. }) {
                return;
            }
            this.selected = 0;
            this.actions_scroll.scroll_to_item(0);
            this.picker_scroll.scroll_to_item(0);
            // Opening clears the search but preserves the current language selection.
            if !this.code_language_focus_pending {
                this.code_language_selected = 0;
                this.code_language_scroll.scroll_to_item(0);
            }
            cx.notify();
        });
        let mut platform_error = None;
        let platform = match platform {
            Ok(mut p) => {
                if let Err(e) = p
                    .configure_window(window)
                    .and_then(|_| p.set_shortcut(&library.preferences.hotkey))
                {
                    platform_error = Some(e);
                }
                Some(p)
            }
            Err(e) => {
                platform_error = Some(e);
                None
            }
        };
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            let bounds = window.bounds();
            if bounds.size != this.last_size {
                if this.expected_size.is_some_and(|expected| {
                    (expected.width - bounds.size.width).abs() <= px(2.)
                        && (expected.height - bounds.size.height).abs() <= px(2.)
                }) {
                    this.expected_size = None;
                } else {
                    this.library.preferences.auto_height = false;
                    this.expected_size = None;
                }
                this.last_size = bounds.size;
            }
            let next = Some([
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ]);
            if this.library.preferences.window_bounds != next {
                this.library.preferences.window_bounds = next;
                this.changed(cx);
            }
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.apply_theme(window, cx);
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if this.window_active {
                if let Some(persistence) = &this.persistence {
                    persistence.refresh();
                }
            } else {
                // Nobody is typing here now, so a held draft's name is as settled as it
                // is going to get; it is filed rather than left waiting in recovery.
                this.release_title(cx);
            }
            cx.notify();
        });
        let quit = cx.on_app_quit(|this, cx| {
            if !this.prepare_to_quit(cx) {
                eprintln!(
                    "Markraft: {}",
                    this.error.as_deref().unwrap_or("Could not save")
                );
            }
            async {}
        });
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if cx
                    .update(|window, cx| this.update(cx, |this, cx| this.poll(window, cx)))
                    .is_err()
                {
                    break;
                }
            }
        });
        let pointer_inside = platform.as_ref().is_none_or(|p| p.pointer_inside(window));
        if let Some(platform) = &platform {
            platform.set_traffic_lights_alpha(window, if pointer_inside { 1. } else { 0. }, false);
        }
        let mut app = Self {
            library,
            persistence: store.map(Persistence::new),
            path,
            settings_path,
            platform,
            updater: Updater::new(),
            instance,
            sessions: HashMap::new(),
            session_order: VecDeque::new(),
            query,
            _query_changes: query_changes,
            panel: Panel::Editor,
            link_popover: None,
            rename: None,
            table: None,
            panel_focus: cx.focus_handle(),
            selected: 0,
            confirm_purge: None,
            picker_scroll: ScrollHandle::new(),
            actions_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            format_scroll: ScrollHandle::new(),
            code_language_scroll: ScrollHandle::new(),
            code_language_block: None,
            code_language_focus_pending: false,
            code_language_selected: 0,
            html_editor: None,
            dirty: false,
            revision: 0,
            save_at: None,
            held_draft: None,
            released_drafts: HashSet::new(),
            name_at: None,
            error,
            platform_error,
            notice: None,
            pending_notices: VecDeque::new(),
            conflict_prompted: HashSet::new(),
            conflict_dialog: false,
            file_status_popover: false,
            file_status_flash: None,
            folder_was_created,
            close_button_shown: false,
            link_targets: Default::default(),
            link_index: Default::default(),
            link_targets_revision: None,
            chrome_focus: None,
            show_words: false,
            format_toolbar: false,
            format_menu: None,
            format_selected: 0,
            format_snapshot: None,
            dark,
            pointer_inside,
            last_key_at: None,
            chrome_shown: true,
            window_active: window.is_window_active(),
            expected_size: None,
            last_size: window.bounds().size,
            _poll: poll,
            _bounds: bounds,
            _appearance: appearance,
            _activation: activation,
            _quit: quit,
        };
        app.ensure_session(window, cx);
        if app.persistence.is_some() {
            app.focus_editor(window, cx);
        } else {
            window.focus(&app.panel_focus, cx);
        }
        // Application preferences and drafts live outside the document folder.
        app.changed(cx);
        if let Some(error) = app.updater.take_startup_error() {
            app.queue_notice(error);
        }
        app
    }
    fn editor(&self) -> Entity<EditorView> {
        self.sessions[&self.library.active_id].editor.clone()
    }
    fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_language_block = None;
        // Nobody is typing a held draft's name any more once its editor is not the one
        // in front: another note became active, or its session was evicted below.
        if self
            .held_draft
            .as_ref()
            .is_some_and(|held| held != &self.library.active_id)
        {
            self.release_title(cx);
        }
        let id = self.library.active_id.clone();
        self.session_order.retain(|entry| entry != &id);
        self.session_order.push_back(id.clone());
        while self.session_order.len() > 8 {
            if let Some(expired) = self.session_order.pop_front() {
                self.sessions.remove(&expired);
            }
        }
        if self.sessions.contains_key(&id) {
            return;
        }
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
                    if this.library.active_id == note_id && this.panel == Panel::Editor {
                        this.open_code_language(*pos, cx);
                    }
                    return;
                }
                if let EditorEvent::RawHtmlRequested { pos } = event {
                    if this.library.active_id == note_id && this.panel == Panel::Editor {
                        this.open_html_source(*pos, window, cx);
                    }
                    return;
                }
                if let EditorEvent::WikiLinkClicked { target, embed } = event {
                    if this.library.active_id == note_id && this.panel == Panel::Editor {
                        this.follow_wiki_link(target, *embed, window, cx);
                    }
                    return;
                }
                if matches!(event, EditorEvent::LinkClicked) {
                    if this.library.active_id == note_id && this.panel == Panel::Editor {
                        this.code_language_block = None;
                        this.link_popover = Some(LinkPopover::View);
                        cx.notify();
                    }
                    return;
                }
                if this.library.active_id == note_id {
                    this.link_popover = None;
                    this.code_language_block = None;
                }
                let document = editor.read(cx).committed_document().clone();
                if this.library.set_document(&note_id, document) {
                    this.changed(cx);
                }
                cx.notify();
            },
        );
        let state_note_id = id.clone();
        let state_changes = cx.observe(&editor, move |this, editor, cx| {
            if this.library.active_id != state_note_id {
                return;
            }
            this.follow_title(cx);
            if this.format_toolbar {
                let editor = editor.read(cx);
                let snapshot = (
                    editor.active_marks(),
                    doc::Block::active(editor.state(), &editor.projection()),
                );
                if this.format_snapshot.as_ref() != Some(&snapshot) {
                    this.format_snapshot = Some(snapshot);
                    cx.notify();
                }
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
    }
    fn sync_documents(&mut self, cx: &App) {
        for (id, session) in &self.sessions {
            self.library
                .set_document(id, session.editor.read(cx).committed_document().clone());
        }
    }
    /// Reconcile external changes without creating files or reviving deleted paths.
    fn apply_external(
        &mut self,
        changes: Vec<External>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_was_focused = self.editor().focus_handle(cx).is_focused(window);
        self.sync_documents(cx);
        let mut ids = Vec::new();
        let mut kept = 0;
        let mut vanished = 0;
        for change in changes {
            let (External::Updated { note, .. } | External::Removed(note)) = &change;
            let id = note.id.clone();
            self.conflict_prompted.remove(&id);
            let local = self.library.note(&id).cloned();
            match change {
                External::Updated { previous, note } => {
                    // The bytes on disk still say what they said: only the file's
                    // permissions moved. There is nothing to choose between, so keep
                    // the document the user is looking at and take the new state,
                    // rather than calling it a change by another app.
                    let permissions_only = previous
                        .as_ref()
                        .is_some_and(|previous| previous.document == note.document);
                    if let Some(mut local) = local
                        && local.deleted_at.is_none()
                        && local.document != note.document
                    {
                        if permissions_only {
                            local.read_only = note.read_only.clone();
                            self.library.adopt(local);
                            ids.push(id);
                            continue;
                        }
                        if previous.is_none_or(|previous| previous.document != local.document) {
                            if let Some(persistence) = &self.persistence
                                && let Err(error) = persistence.recover(local.clone())
                            {
                                self.error = Some(error);
                            }
                            local.conflicted = true;
                            self.library.adopt(local);
                            kept += 1;
                            continue;
                        }
                    }
                    self.library.adopt(note);
                }
                External::Removed(note) => {
                    if local
                        .as_ref()
                        .is_none_or(|local| local.document == note.document)
                    {
                        // A note the user still had is going without their asking, so
                        // say so. One already in Recently Deleted is being tidied up.
                        if local
                            .as_ref()
                            .is_some_and(|local| local.deleted_at.is_none())
                        {
                            vanished += 1;
                        }
                        self.library.remove(&id);
                    } else if let Some(mut local) = local {
                        if let Some(persistence) = &self.persistence
                            && let Err(error) = persistence.recover(local.clone())
                        {
                            self.error = Some(error);
                        }
                        local.conflicted = true;
                        self.library.adopt(local);
                        kept += 1;
                        continue;
                    }
                }
            }
            self.sessions.remove(&id);
            self.session_order.retain(|entry| entry != &id);
            ids.push(id);
        }
        self.ensure_session(window, cx);
        // A file appearing or leaving changes what `[[` can link to, and arrives
        // without touching the revision counter the poll watches.
        self.refresh_link_targets();
        // Replacing an editor session drops its focus handle. Restore editing
        // focus without taking it away from a picker or settings input.
        if editor_was_focused {
            self.focus_editor(window, cx);
        }
        if let Some(persistence) = &self.persistence {
            persistence.acknowledge(ids);
        }
        if kept > 0 {
            self.changed(cx);
        }
        if vanished > 0 {
            self.queue_notice(if vanished == 1 {
                "A note's file was deleted outside Markraft, so the note is gone too."
                    .to_owned()
            } else {
                format!(
                    "{vanished} notes' files were deleted outside Markraft, so those notes are gone too."
                )
            });
        }
        self.prompt_conflict(window, cx);
        cx.notify();
    }
    /// How many notes are not in a file the way the user left them: one with no file
    /// yet, and one whose file says something else. Both come back from recovery as
    /// ordinary notes, so this is a filter rather than a second list.
    pub(crate) fn draft_count(&self) -> usize {
        self.library.notes.iter().filter(|n| is_draft(n)).count()
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.dirty = true;
        self.save_at = Some(Instant::now() + Duration::from_millis(350));
        // A held draft is waiting for its first line to stop changing, so every edit
        // puts that moment off again.
        if self.held_draft.is_some() {
            self.name_at = Some(Instant::now() + NAME_SETTLES);
        }
        cx.notify();
    }
    /// Keep autosave from naming a new note's file after a half-typed first line.
    ///
    /// Autosave names a note's file once and never again on its own, so a note filed
    /// on its first keystroke would stay `M.md` while its first line went on to read
    /// "Meeting notes for Q3"; only [`Self::open_rename`] moves it afterwards. While
    /// the caret is still in the line the name would come from, the store holds the
    /// note in recovery instead. The name has settled once the caret leaves that line
    /// or the typing stops for [`NAME_SETTLES`], and the next save files the note
    /// under what the line says then — a note that is only a first line is filed like
    /// any other.
    ///
    /// Called wherever the editor's state moved, a selection with no edit included.
    fn follow_title(&mut self, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        let naming = !self.released_drafts.contains(&id) && self.naming_title(cx);
        if self
            .held_draft
            .as_ref()
            .is_some_and(|held| *held != id || !naming)
        {
            self.release_title(cx);
        }
        if naming && self.held_draft.is_none() {
            self.held_draft = Some(id);
            self.name_at = Some(Instant::now() + NAME_SETTLES);
        }
    }
    /// Whether the active note is one the store would file under its first line, with
    /// the selection still working on that line.
    ///
    /// The editor's live document answers where the selection is; an input method's
    /// uncommitted candidate sits at the caret, so composing a title counts as still
    /// typing it, which is what holds the note back.
    fn naming_title(&self, cx: &App) -> bool {
        if self.persistence.is_none() || self.path.is_none() {
            return false;
        }
        let note = self.library.active_note();
        if note.path.is_some()
            || note.conflicted
            || note.deleted_at.is_some()
            || note.document_is_empty()
        {
            return false;
        }
        self.sessions
            .get(&self.library.active_id)
            .map(|session| session.editor.read(cx))
            .is_some_and(|editor| naming_title(editor.doc(), editor.state().selection()))
    }
    /// Let go of the held draft: its name has settled, the next save files it, and it is
    /// never held again. A caret leaving the first line and a window losing focus are
    /// not edits, so the save this needs is scheduled here.
    ///
    /// A note emptied while it was held is the exception. It has no name for anything
    /// to have settled on, and the page was cleared to begin again, so it is not shut
    /// out of being held: what is typed next deserves the wait a new note gets.
    fn release_title(&mut self, cx: &mut Context<Self>) {
        self.name_at = None;
        if let Some(id) = self.held_draft.take() {
            if !self
                .library
                .note(&id)
                .is_none_or(crate::storage::Note::document_is_empty)
            {
                self.released_drafts.insert(id);
            }
            self.changed(cx);
        }
    }
    /// Whether the active note is a draft the store has nowhere to file: without a
    /// folder a new note is held privately until the user names a file for it.
    fn unfiled_draft(&self) -> bool {
        self.persistence.is_some()
            && self.path.is_none()
            && self.library.active_note().path.is_none()
    }
    /// ⌘S. A draft with no folder behind it asks where to go before it is written;
    /// everywhere else the note already knows its file.
    fn save_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.unfiled_draft() {
            self.save_as(window, cx);
        } else {
            self.flush(cx);
        }
    }
    /// Ask where the active note should live, then carry on with `next`. Everything
    /// that needs a draft to have a file comes through here, so the rules are the
    /// same each time: only a name nothing else holds, and the editor's image base
    /// follows the note into its folder.
    fn prompt_for_note_path(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        next: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let id = self.library.active_id.clone();
        let directory = self
            .path
            .as_ref()
            .map(|root| root.join(&self.library.workspace.new_note_directory))
            .or_else(|| {
                self.library
                    .notes
                    .iter()
                    .find_map(|note| note.path.as_ref()?.parent().map(ToOwned::to_owned))
            })
            .unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        if this.library.active_id != id {
                            return;
                        }
                        if path.exists() {
                            this.queue_notice(
                                "Choose a new filename; the existing file was not changed."
                                    .to_owned(),
                            );
                            return;
                        }
                        let parent = path.parent().map(ToOwned::to_owned);
                        if let Some(note) = this.library.notes.iter_mut().find(|n| n.id == id) {
                            note.path = Some(path);
                        }
                        this.editor()
                            .update(cx, |editor, cx| editor.set_image_base(parent, cx));
                        next(this, window, cx);
                    })
                });
            }
        })
        .detach();
    }
    /// Give the active draft a file, then write it there.
    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_note_path(window, cx, |this, window, cx| {
            this.flush(cx);
            this.focus_editor(window, cx);
        });
    }
    fn flush(&mut self, cx: &mut Context<Self>) -> bool {
        // A flush is asked for — ⌘S, quitting, an update relaunch — so it files a held
        // draft under whatever its first line says now. The barrier below clears the
        // save this schedules.
        self.release_title(cx);
        self.sync_documents(cx);
        self.save_at = None;
        self.revision += 1; // Discard acknowledgments for snapshots preceding this barrier.
        let result = self
            .persistence
            .as_ref()
            .ok_or_else(|| "Open or recover the library before saving.".to_string())
            .and_then(|p| p.flush(self.library.clone()));
        match result {
            Ok(()) => {
                self.dirty = false;
                self.error = None;
            }
            Err(e) => {
                self.dirty = true;
                self.error = Some(e);
            }
        }
        if let Some(persistence) = &self.persistence
            && let Ok(paths) = persistence.paths()
        {
            self.update_paths(paths, cx);
        }
        if let Some(persistence) = &self.persistence
            && let Ok(ids) = persistence.conflicts()
        {
            self.update_conflicts(ids, cx);
        }
        cx.notify();
        !self.dirty
    }
    /// The title recedes while the window is idle. Corner action buttons and native
    /// traffic lights follow `pointer_inside` alone and fade out completely;
    /// typing, focus and open panels must not keep those buttons visible.
    fn chrome_visible(&self) -> bool {
        self.pointer_inside
            || self.window_active
            || self.panel != Panel::Editor
            || self.format_menu.is_some()
            || self
                .last_key_at
                .is_some_and(|at| at.elapsed() < KEY_PRESENCE)
    }
    /// Critically damped: chrome fades without overshoot, and a reversal while the
    /// pointer crosses the window edge continues from the current opacity. Reduced
    /// motion keeps both end states and drops the travel between them.
    pub(super) fn chrome_spring(visible: bool, reduce_motion: bool) -> SpringAnimation<bool> {
        SpringAnimation::new(SpringConfig::new(600., 49., 1.))
            .to(visible)
            .playback(ui::playback(reduce_motion))
    }
    /// A keystroke counts as presence for a moment, so the chrome does not vanish from
    /// under someone who is writing with the pointer parked outside the window.
    pub(super) fn note_key_press(&mut self, cx: &mut Context<Self>) {
        let was_visible = self.chrome_visible();
        self.last_key_at = Some(Instant::now());
        if !was_visible {
            cx.notify();
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_conflict(window, cx);
        if let Some(rejection) = self
            .editor()
            .update(cx, |editor, _| editor.take_edit_error())
        {
            match rejection {
                // The corner already says "Read-only" in words, beside a capsule that
                // opens the ways out of it. A sentence per keystroke would only say
                // it again, so the capsule lights instead.
                EditRejection::ReadOnly(_) => {
                    self.file_status_flash = Some(Instant::now() + FILE_STATUS_FLASH);
                    cx.notify();
                }
                EditRejection::Protected(message) | EditRejection::Invalid(message) => {
                    self.queue_notice(message);
                }
                // The shading said it where the edit landed.
                EditRejection::Marked(_) => {}
            }
        }
        if self.panel == Panel::Editor {
            self.editor()
                .update(cx, |editor, cx| editor.refresh_images(cx));
        }
        // The `[[` menu reads a shared list rather than a snapshot, so it follows notes
        // being written, renamed and deleted. Rebuilding costs a string a note, so it
        // is tied to the counter every change already bumps; a change arriving from
        // outside refills the list where it lands.
        if self.link_targets_revision != Some(self.revision) {
            self.link_targets_revision = Some(self.revision);
            self.refresh_link_targets();
        }
        // The platform draws the close button above everything the view renders, so a
        // popup that reaches the top-left corner would be covered by it. It stands down
        // while one is open, the same way it does when the pointer leaves the window.
        let covered = self.panel == Panel::Editor && self.editor().read(cx).overlay_open();
        if let Some(platform) = &self.platform {
            let inside = platform.pointer_inside(window);
            if inside != self.pointer_inside {
                self.pointer_inside = inside;
                cx.notify();
            }
            let shown = inside && !covered;
            if shown != self.close_button_shown {
                self.close_button_shown = shown;
                platform.set_traffic_lights_alpha(
                    window,
                    if shown { 1. } else { 0. },
                    !cx.reduce_motion(),
                );
            }
        }
        // The keystroke timer expires on its own, so the chrome is compared here rather
        // than only where its inputs change.
        let chrome = self.chrome_visible();
        if chrome != self.chrome_shown {
            self.chrome_shown = chrome;
            cx.notify();
        }
        for request in self.instance.requests() {
            match request {
                crate::instance::Request::Show => self.show(window, cx),
                crate::instance::Request::OpenPaths(paths) => self.open_paths(paths, window, cx),
            }
        }
        let events = self
            .platform
            .as_ref()
            .map(|p| p.poll_events())
            .unwrap_or_default();
        for event in events {
            match event {
                PlatformEvent::Toggle => self.toggle(window, cx),
                PlatformEvent::NewNote => {
                    self.show(window, cx);
                    self.new_note(window, cx);
                }
                PlatformEvent::Settings => {
                    self.show(window, cx);
                    self.open_panel(Panel::Settings, window, cx);
                }
                PlatformEvent::CheckForUpdates => self.check_for_updates(window, cx),
                PlatformEvent::Quit => self.quit(cx),
            }
        }
        for notice in self
            .persistence
            .as_ref()
            .map(Persistence::notices)
            .unwrap_or_default()
        {
            self.queue_notice(notice);
        }
        let events = self
            .persistence
            .as_ref()
            .map(Persistence::poll)
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Saved(saved) if saved.revision == self.revision => {
                    self.update_paths(saved.paths, cx);
                    self.update_conflicts(saved.conflicts, cx);
                    match saved.result {
                        Ok(()) => {
                            self.dirty = false;
                            self.error = None;
                        }
                        // A save failure is a state of the file now, so the whole
                        // report stands: the Not saved capsule carries it, and the
                        // conflict it may mention has a capsule of its own beside it.
                        Err(e) => self.error = Some(e),
                    }
                    cx.notify();
                }
                Event::Saved(saved) => {
                    self.update_paths(saved.paths, cx);
                    self.update_conflicts(saved.conflicts, cx);
                }
                Event::External(changes) => self.apply_external(changes, window, cx),
            }
        }
        // Process external file changes first, then make the same save barrier as
        // a normal quit. A failed save must never approve Sparkle's relaunch.
        if let Some(continuation) = self.updater.take_relaunch() {
            if self.prepare_to_quit(cx) {
                cx.defer(move |_| {
                    if let Some(main_thread) = sparkle_updater::MainThreadMarker::new() {
                        continuation.resume(main_thread);
                    }
                });
            } else {
                self.updater.postpone(continuation);
                self.show(window, cx);
                self.inform("Update paused. Resolve the save error, then choose Check for Updates to retry.", cx);
            }
        }
        // Nobody has touched the name for a while, so it is as settled as it is going
        // to get; the note is filed rather than left waiting for the caret to move.
        if self.name_at.is_some_and(|at| Instant::now() >= at) {
            self.release_title(cx);
        }
        if self.save_at.is_some_and(|at| Instant::now() >= at) {
            self.save_at = None;
            if let Some(p) = &self.persistence
                && let Err(e) = p.save(
                    self.revision,
                    self.library.clone(),
                    self.held_draft.clone().into_iter().collect(),
                )
            {
                self.error = Some(e);
            }
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| Instant::now() > notice.until)
        {
            self.notice = None;
            cx.notify();
        }
        if self
            .file_status_flash
            .is_some_and(|until| Instant::now() >= until)
        {
            self.file_status_flash = None;
            cx.notify();
        }
        // One queued sentence at a time, once whatever was on screen has had its turn.
        if self.notice.is_none()
            && let Some(text) = self.pending_notices.pop_front()
        {
            self.notice = Some(Notice {
                text: text.into(),
                until: Instant::now() + READING_NOTICE,
                undo: None,
            });
            cx.notify();
        }
        if self.panel == Panel::Editor
            && self.library.preferences.auto_height
            && self.persistence.is_some()
        {
            let Some(height) = self.editor().read(cx).content_height() else {
                return;
            };
            // Growing is all this does: `resize` keeps the origin, so the window
            // only ever extends downwards. Its room is therefore what is left
            // below its own top edge, not a share of the whole display — a window
            // sitting low would otherwise grow straight past the bottom of the
            // screen, and since it is sized to its content there is no overflow
            // left to scroll the hidden part back into view.
            let maximum = window
                .display(cx)
                .map(|d| {
                    let visible = d.visible_bounds();
                    (visible.bottom() - window.bounds().origin.y).min(visible.size.height * 0.8)
                })
                .unwrap_or(px(720.))
                .max(MINIMUM_HEIGHT);
            // The toolbar and footer float over the editor and are already part of its
            // content height; only an error banner adds to it.
            let chrome = if self.error.is_some() {
                px(64.)
            } else {
                px(0.)
            };
            let desired = px(f32::from((height + chrome).max(MINIMUM_HEIGHT).min(maximum)).round());
            let size = size(window.bounds().size.width, desired);
            if (size.height - window.bounds().size.height).abs() > px(2.)
                && self.expected_size.is_none()
            {
                self.expected_size = Some(size);
                window.resize(size);
            }
        }
    }
    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome_focus = None;
        if !self.focus_html_source(window, cx) {
            window.focus(&self.editor().focus_handle(cx), cx);
        }
    }
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.platform {
            if let Err(e) = p.show(window) {
                self.error = Some(e);
            }
        } else {
            window.activate_window();
        }
        self.chrome_focus = None;
        if self.persistence.is_none() {
            window.focus(&self.panel_focus, cx);
        } else if self.focus_html_source(window, cx) {
            // Keep the source draft as the keyboard owner after hiding the app.
        } else if self.panel != Panel::Editor {
            window.focus(&self.query.focus_handle(cx), cx);
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }
    pub fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_language_block = None;
        self.query.update(cx, |e, cx| e.cancel_composition(cx));
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        if !self.flush(cx) {
            // The window staying put is the only sign the key landed at all, and a
            // 24px capsule is a thin place to keep the reason.
            self.file_status_popover = true;
            cx.notify();
            return;
        }
        if let Some(p) = &mut self.platform {
            if let Err(e) = p.hide(window) {
                self.error = Some(e);
                cx.notify();
            }
        } else {
            cx.hide();
        }
    }
    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_window_active() {
            self.hide(window, cx);
        } else {
            self.show(window, cx);
        }
    }
    fn prepare_to_quit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.persistence.is_none() {
            return true;
        }
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.flush(cx)
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        if self.prepare_to_quit(cx) {
            cx.quit();
            return;
        }
        self.file_status_popover = true;
        cx.notify();
    }
    pub fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.updater.check() {
            self.show(window, cx);
            self.inform(&error, cx);
        }
    }
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_html_source(window, cx) {
            return;
        }
        if self.query.read(cx).is_composing() {
            self.query.update(cx, |e, cx| e.cancel_composition(cx));
            return;
        }
        if self.editor().read(cx).is_composing() {
            self.editor().update(cx, |e, cx| e.cancel_composition(cx));
            return;
        }
        // A notice offering an action takes the first Escape; the cascade below resumes
        // on the next one.
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.undo.is_some())
        {
            self.notice = None;
            self.chrome_focus = None;
            // Hand the keyboard back to the panel the notice was drawn over.
            if self.panel != Panel::Editor {
                window.focus(&self.query.focus_handle(cx), cx);
            }
            cx.notify();
            return;
        }
        // A question standing on a row takes the first Escape, so nothing behind it
        // closes while the answer is still pending.
        if self.confirm_purge.take().is_some() {
            cx.notify();
            return;
        }
        self.chrome_focus = None;
        let had_popover = self.format_menu.take().is_some()
            | self.link_popover.take().is_some()
            | self.rename.take().is_some()
            | self.code_language_block.take().is_some()
            | std::mem::take(&mut self.file_status_popover);
        if had_popover {
            self.focus_editor(window, cx);
            cx.notify();
        } else if self.panel != Panel::Editor {
            self.panel = Panel::Editor;
            self.focus_editor(window, cx);
            cx.notify();
        } else if self.format_toolbar {
            self.format_toolbar = false;
            cx.notify();
        } else {
            self.hide(window, cx);
        }
    }
    fn new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.format_menu = None;
        self.code_language_block = None;
        self.link_popover = None;
        self.rename = None;
        self.file_status_popover = false;
        self.query.update(cx, |e, cx| e.cancel_composition(cx));
        if self.persistence.is_none() {
            self.choose_folder(window, cx);
            return;
        }
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        self.library.new_note(doc::empty());
        self.ensure_session(window, cx);
        self.panel = Panel::Editor;
        self.focus_editor(window, cx);
        self.changed(cx);
    }
    fn select_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.format_menu = None;
        self.code_language_block = None;
        self.link_popover = None;
        self.rename = None;
        self.file_status_popover = false;
        self.query.update(cx, |e, cx| e.cancel_composition(cx));
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        if self.library.select(id) {
            self.ensure_session(window, cx);
            self.panel = Panel::Editor;
            self.focus_editor(window, cx);
            self.changed(cx);
        }
    }
    /// Open what a clicked wiki link names, exactly as selecting it in Browse
    /// would — [`NotesApp::select_note`] is what `Intent::Select` runs, so the
    /// session, the focus and the panel all end up where Browse leaves them.
    ///
    /// A target that names nothing is said out loud rather than created: a file
    /// this window makes is a file the folder did not have, and a mistyped link
    /// is the likelier reason for a miss.
    fn follow_wiki_link(
        &mut self,
        target: &str,
        embed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = wiki_link_page(target);
        if page.is_empty() {
            // `[[#Heading]]` names a place in this very note, and going to one
            // is not something this does.
            return;
        }
        let from = self.library.active_note().path.clone();
        let found = resolve_wiki_link(
            target,
            from.as_deref(),
            self.path.as_deref(),
            self.library
                .notes
                .iter()
                .filter(|note| note.deleted_at.is_none())
                .filter_map(|note| Some((note.id.as_str(), note.path.as_deref()?))),
        );
        match found {
            Some(id) => self.select_note(&id, window, cx),
            // Not a page the folder holds, so it may still be a file it holds — an
            // embedded image is the usual one, and reporting that as a missing note
            // would be telling the user something they can see is untrue.
            None => match linked_file(page, from.as_deref(), self.path.as_deref()) {
                Some(path) => cx.open_with_system(&path),
                None => {
                    let what = if embed { "file" } else { "note" };
                    self.queue_notice(format!("No {what} named “{page}” in this folder."))
                }
            },
        }
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.format_menu = None;
        self.code_language_block = None;
        self.link_popover = None;
        if self.persistence.is_none() {
            return;
        }
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.query.update(cx, |e, cx| e.cancel_composition(cx));
        self.panel = if self.panel == panel {
            Panel::Editor
        } else {
            panel
        };
        self.selected = 0;
        self.chrome_focus = None;
        self.confirm_purge = None;
        let (query, placeholder, label) = match self.panel {
            Panel::Settings => (
                self.library.preferences.hotkey.clone(),
                "Type a shortcut, e.g. Alt+N",
                "Global shortcut",
            ),
            Panel::Actions => (String::new(), "Search for actions…", "Search actions"),
            Panel::Trash => (
                String::new(),
                "Search deleted notes…",
                "Search deleted notes",
            ),
            _ => (String::new(), "Search for notes…", "Search notes"),
        };
        self.link_popover = None;
        self.set_query(query, placeholder, label, cx);
        if self.panel != Panel::Editor {
            window.focus(&self.query.focus_handle(cx), cx);
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }
    /// Which of the library's notes the open panel is looking at. Drafts and the
    /// trash are the same list the Browse panel draws, filtered two ways.
    pub(crate) fn scope(&self) -> Scope {
        match self.panel {
            Panel::Drafts => Scope::Drafts,
            Panel::Trash => Scope::Deleted,
            _ => Scope::Notes,
        }
    }
    fn matching_notes(&self, query: &str, scope: Scope) -> Vec<&crate::storage::Note> {
        let deleted = scope == Scope::Deleted;
        let mut notes = self.library.search(query, deleted, self.path.as_deref());
        if scope == Scope::Drafts {
            notes.retain(|note| is_draft(note));
        }
        if !deleted {
            notes.sort_by_key(|note| note.id != self.library.active_id);
        }
        notes
    }
    fn toggle_pin(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(note) = self
            .library
            .notes
            .iter_mut()
            .find(|note| note.id == id && note.deleted_at.is_none())
        {
            note.pinned = !note.pinned;
            self.changed(cx);
        }
        // Pinning reorders results; keep the same note selected.
        self.selected = self
            .matching_notes(self.query.read(cx).text().trim(), self.scope())
            .iter()
            .position(|note| note.id == id)
            .unwrap_or(0);
        self.picker_scroll.scroll_to_item(self.selected);
    }
    fn trash_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        if self.library.delete(id) {
            self.sessions.remove(id);
            self.ensure_session(window, cx);
            let query = self.query.read(cx).text().to_owned();
            let root = self.path.clone();
            self.selected = self.selected.min(
                self.library
                    .search(query.trim(), false, root.as_deref())
                    .len()
                    .saturating_sub(1),
            );
            self.picker_scroll.scroll_to_item(self.selected);
            self.chrome_focus = None;
            window.focus(&self.query.focus_handle(cx), cx);
            self.changed(cx);
            self.inform_undo("Moved to Recently Deleted", id.to_owned(), cx);
        }
    }
    fn delete_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        self.sync_documents(cx);
        if self.library.delete(&id) {
            self.sessions.remove(&id);
            self.ensure_session(window, cx);
            self.panel = Panel::Editor;
            self.focus_editor(window, cx);
            self.changed(cx);
            self.inform_undo("Moved to Recently Deleted", id, cx);
        }
    }
    /// Take back the deletion the notice still offers, and open that note again.
    fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.notice.take().and_then(|notice| notice.undo) else {
            return;
        };
        if self.library.restore(&id) {
            self.ensure_session(window, cx);
            self.panel = Panel::Editor;
            self.focus_editor(window, cx);
            self.changed(cx);
            self.inform("Restored note", cx);
        } else {
            cx.notify();
        }
    }
    /// Delete deleted notes for good. The trash is the undo, so this one is asked twice
    /// before it runs; what the folder actually gave up is what leaves the library.
    fn purge_notes(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        let count = ids.len();
        let (purged, result) = persistence.purge(ids);
        for id in &purged {
            self.library.remove(id);
            self.sessions.remove(id);
            self.session_order.retain(|entry| entry != id);
        }
        self.confirm_purge = None;
        self.chrome_focus = None;
        self.ensure_session(window, cx);
        self.selected = self.selected.min(
            self.library
                .search(
                    self.query.read(cx).text().trim(),
                    true,
                    self.path.as_deref(),
                )
                .len()
                .saturating_sub(1),
        );
        self.picker_scroll.scroll_to_item(self.selected);
        self.changed(cx);
        match result {
            Ok(()) if count == 1 => self.inform("Deleted permanently", cx),
            Ok(()) => self.inform("Emptied Recently Deleted", cx),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
    /// ⌘K's bulk purge. It reaches past what the list shows, so it is confirmed in a
    /// dialog rather than by a second click.
    fn empty_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .library
            .search("", true, None)
            .iter()
            .map(|note| note.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!(
                "Delete {} {} for good?",
                ids.len(),
                if ids.len() == 1 { "note" } else { "notes" }
            ),
            Some("This cannot be undone."),
            &["Cancel", "Delete"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(1) {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| this.purge_notes(ids, window, cx))
                });
            }
        })
        .detach();
    }
    fn restore_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.restore(id) {
            // The row and its buttons leave the list with the note.
            self.chrome_focus = None;
            self.confirm_purge = None;
            self.ensure_session(window, cx);
            self.changed(cx);
            self.selected = self.selected.min(
                self.library
                    .search(self.query.read(cx).text(), true, self.path.as_deref())
                    .len()
                    .saturating_sub(1),
            );
            self.inform("Restored note", cx);
        }
    }
    fn inform(&mut self, text: impl AsRef<str>, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: text.as_ref().to_owned().into(),
            until: Instant::now() + Duration::from_secs(3),
            undo: None,
        });
        cx.notify();
    }
    /// A sentence the user has to read, rather than an acknowledgment of what they just
    /// did. It waits for the notice on screen instead of replacing it, and stays longer.
    fn queue_notice(&mut self, text: String) {
        if !self.pending_notices.contains(&text) {
            self.pending_notices.push_back(text);
        }
    }
    /// A notice whose deletion can still be taken back.
    fn inform_undo(&mut self, text: &str, note: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: text.into(),
            until: Instant::now() + Duration::from_secs(8),
            undo: Some(note),
        });
        cx.notify();
    }
    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = self.library.preferences.dark_mode.unwrap_or(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        for session in self.sessions.values() {
            session
                .editor
                .update(cx, |e, cx| e.set_style(notes_style(self.dark), cx));
        }
        self.query
            .update(cx, |e, cx| e.set_style(query_style(self.dark), cx));
        cx.notify();
    }
    /// Lend the one query field to a surface. It holds literal text, so it is never read
    /// as Markdown, and it takes the name of whatever it is serving: the field reports
    /// that name itself rather than borrowing one from a group drawn around it.
    fn set_query(
        &mut self,
        text: String,
        placeholder: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.query.update(cx, |e, cx| {
            e.set_value(&text, cx);
            e.set_placeholder(placeholder, cx);
            e.set_aria_label(label, cx);
        });
    }
    /// ⌘L: a link under the caret shows its actions, anything else asks for an address.
    fn open_link_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() || self.panel != Panel::Editor {
            return;
        }
        self.format_menu = None;
        self.code_language_block = None;
        self.chrome_focus = None;
        if self.editor().read(cx).active_link().is_some() {
            self.link_popover = Some(LinkPopover::View);
            cx.notify();
        } else {
            self.edit_link(window, cx);
        }
    }
    fn edit_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_language_block = None;
        let url = self
            .editor()
            .read(cx)
            .active_link()
            .unwrap_or_default()
            .to_owned();
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.chrome_focus = None;
        self.set_query(url, "Enter a link…", "Link URL", cx);
        self.link_popover = Some(LinkPopover::Edit);
        window.focus(&self.query.focus_handle(cx), cx);
        cx.notify();
    }
    fn unlink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor()
            .update(cx, |editor, cx| editor.set_link(None, cx));
        self.link_popover = None;
        self.focus_editor(window, cx);
        cx.notify();
    }
    fn apply_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query.read(cx);
        let url =
            markraft_core::projection::Projection::of(query.committed_document(), query.schema())
                .plain_text()
                .trim()
                .to_string();
        self.editor().update(cx, |editor, cx| {
            editor.set_link((!url.is_empty()).then_some(url.as_str()), cx)
        });
        self.link_popover = None;
        self.focus_editor(window, cx);
        cx.notify();
    }
    fn apply_shortcut(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx);
        let text =
            markraft_core::projection::Projection::of(query.committed_document(), query.schema())
                .plain_text()
                .trim()
                .to_string();
        if let Some(platform) = &mut self.platform {
            match platform.set_shortcut(&text) {
                Ok(()) => {
                    self.library.preferences.hotkey = text;
                    self.platform_error = None;
                    self.changed(cx);
                    self.inform("Updated shortcut", cx);
                }
                Err(e) => {
                    self.platform_error = Some(e);
                    cx.notify();
                }
            }
        }
    }
    fn copy_markdown(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(doc::to_markdown(
            self.editor().read(cx).committed_document(),
        )));
        self.inform("Copied as Markdown", cx);
    }
    fn recover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(directory) = self.path.clone() {
            self.open_folder(directory, window, cx);
        }
    }
    pub(super) fn default_folder() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Documents/Markraft"))
    }
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = prompt.await
                && let Some(directory) = paths.pop()
            {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| this.open_folder(directory, window, cx))
                });
            }
        })
        .detach();
    }
    /// Open `directory` as the notes folder and remember the choice. The folder in use
    /// stays open when the new one cannot be.
    fn open_folder(&mut self, directory: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let reopening = self.persistence.is_none();
        if !reopening
            && (self.path.as_ref().is_some_and(|current| {
                current.canonicalize().ok() == directory.canonicalize().ok()
            }) || !self.flush(cx))
        {
            return;
        }
        let opened = Store::open(directory.clone(), self.settings_path.clone()).and_then(
            |(mut store, library)| {
                store
                    .update_settings(|settings| settings.notes_folder = Some(directory.clone()))?;
                Ok((store, library))
            },
        );
        match opened {
            Ok((store, mut library)) => {
                // Preferences belong to this Mac, not to the folder.
                if !reopening {
                    library.preferences = self.library.preferences.clone();
                }
                self.path = Some(store.directory().to_owned());
                self.library = library;
                self.persistence = Some(Persistence::new(store));
                self.sessions.clear();
                self.session_order.clear();
                self.ensure_session(window, cx);
                self.error = None;
                self.panel = Panel::Editor;
                self.focus_editor(window, cx);
                self.apply_theme(window, cx);
                self.platform_error = self
                    .platform
                    .as_mut()
                    .and_then(|p| p.set_shortcut(&self.library.preferences.hotkey).err());
                self.changed(cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }
    fn save_copy(&mut self, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        let bytes = match crate::vault::backup(&self.library) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let Some(folder) = self.path.clone() else {
            return;
        };
        let prompt = cx.prompt_for_new_path(&folder, Some("Markraft Backup.json"));
        let original = folder;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if path == original
                            || path
                                .canonicalize()
                                .ok()
                                .zip(original.canonicalize().ok())
                                .is_some_and(|(a, b)| a == b)
                        {
                            return Err(
                                "Choose a different path to preserve the existing library.".into(),
                            );
                        }
                        std::fs::write(path, bytes).map_err(|e| e.to_string())
                    })
                    .await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(()) => this.inform("Saved library copy", cx),
                    Err(e) => {
                        this.error = Some(e);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Reload from disk?",
            Some("Unsaved changes in Markraft will be lost."),
            &["Cancel", "Reload"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(1) {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        this.revision += 1;
                        this.save_at = None;
                        let result = this
                            .persistence
                            .as_ref()
                            .ok_or_else(|| "Choose a notes folder first.".to_string())
                            .and_then(|p| p.reload());
                        match result {
                            Ok(library) => {
                                this.query.update(cx, |e, cx| e.cancel_composition(cx));
                                this.library = library;
                                this.sessions.clear();
                                this.session_order.clear();
                                this.ensure_session(window, cx);
                                this.panel = Panel::Editor;
                                this.dirty = false;
                                this.error = None;
                                this.focus_editor(window, cx);
                                this.apply_theme(window, cx);
                                this.platform_error = this.platform.as_mut().and_then(|p| {
                                    p.set_shortcut(&this.library.preferences.hotkey).err()
                                });
                            }
                            Err(e) => this.error = Some(e),
                        }
                        cx.notify();
                    })
                });
            }
        })
        .detach();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        if self.persistence.is_none() {
            return;
        }
        let note = self.library.active_note();
        let title = note.title();
        let filename = format!("{}.md", title.replace(['/', ':'], "-"));
        let mut snapshot = note.clone();
        snapshot.document = self.editor().read(cx).committed_document().clone();
        let document = match self.persistence.as_ref().unwrap().markdown(snapshot) {
            Ok(document) => document,
            // Nothing was written, so this is not the save banner's business.
            Err(error) => {
                self.queue_notice(format!("Could not prepare the export: {error}"));
                cx.notify();
                return;
            }
        };
        let directory = self.path.clone().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&directory, Some(&filename));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        use std::io::Write;
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                            .map_err(|e| e.to_string())?;
                        file.write_all(document.as_bytes())
                            .and_then(|_| file.sync_all())
                            .map_err(|e| e.to_string())
                    })
                    .await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(()) => this.inform("Exported Markdown", cx),
                    Err(e) => {
                        this.error = Some(e);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
    fn open_markdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open Markdown".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| this.open_paths(paths, window, cx))
                });
            }
        })
        .detach();
    }

    /// Take a drop apart and give each kind of path the handler it belongs to. A
    /// folder replaces everything that is open, so it is asked about first, and is
    /// ignored when the drop does not clearly mean one folder.
    fn drop_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let dropped = classify_drop(&paths, |path| path.is_dir());
        // A folder replaces everything that is open, so it is only taken when the
        // drop says nothing else.
        let folder = (dropped.folders.len() == 1
            && dropped.images.is_empty()
            && dropped.markdown.is_empty())
        .then(|| dropped.folders[0].clone());
        if folder.is_none() && !dropped.folders.is_empty() {
            self.queue_notice(
                "Drop one folder on its own to open it; folders were left alone.".to_owned(),
            );
        }
        if dropped.skipped > 0 {
            self.queue_notice(format!(
                "Skipped {} {} Markraft cannot open.",
                dropped.skipped,
                if dropped.skipped == 1 {
                    "file"
                } else {
                    "files"
                }
            ));
        }
        if !dropped.images.is_empty() {
            if self.panel == Panel::Editor && self.persistence.is_some() {
                self.insert_assets(
                    dropped
                        .images
                        .into_iter()
                        .map(assets::Asset::File)
                        .collect(),
                    window,
                    cx,
                );
            } else {
                self.queue_notice(
                    "Open a note before dropping images; they are inserted where the caret is."
                        .to_owned(),
                );
            }
        }
        if !dropped.markdown.is_empty() {
            self.open_paths(dropped.markdown, window, cx);
        }
        if let Some(folder) = folder {
            self.confirm_folder(folder, window, cx);
        }
    }

    /// Opening a folder puts every other note away, so it is a question rather than
    /// something a stray drop can do on its own.
    fn confirm_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder.display().to_string());
        let answer = window.prompt(
            PromptLevel::Info,
            &format!("Open “{name}” instead?"),
            Some("Markraft shows one folder at a time. Nothing is moved or renamed."),
            &["Cancel", "Open"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(1) {
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| this.open_folder(folder, window, cx))
                });
            }
        })
        .detach();
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        for path in paths {
            if path.is_dir() {
                self.open_folder(path, window, cx);
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let result = if let Some(persistence) = &self.persistence {
                persistence.open_file(path).map(|note| {
                    let id = note.id.clone();
                    // An already-open dirty session must not be replaced by disk.
                    if self.library.note(&id).is_none() {
                        self.library.adopt(note);
                    }
                    self.library.select(&id);
                })
            } else {
                Store::open_file(path, self.settings_path.clone()).map(|(store, mut library)| {
                    library.preferences = self.library.preferences.clone();
                    self.library = library;
                    self.persistence = Some(Persistence::new(store));
                    self.path = None;
                    self.sessions.clear();
                    self.session_order.clear();
                })
            };
            // Failing to open a file says nothing about saving, so it is a sentence
            // rather than the save banner — and each file gets its own.
            if let Err(error) = result {
                self.queue_notice(format!("Could not open “{name}”: {error}"));
            }
        }
        self.ensure_session(window, cx);
        self.panel = Panel::Editor;
        self.show(window, cx);
        self.focus_editor(window, cx);
        self.changed(cx);
    }

    fn update_paths(&mut self, paths: Vec<(String, PathBuf)>, cx: &mut Context<Self>) {
        for (id, path) in paths {
            if let Some(note) = self.library.notes.iter_mut().find(|note| note.id == id)
                && note.path.as_ref() != Some(&path)
            {
                note.path = Some(path.clone());
                if let Some(session) = self.sessions.get(&id) {
                    session.editor.update(cx, |editor, cx| {
                        editor.set_image_base(path.parent().map(ToOwned::to_owned), cx)
                    });
                }
            }
        }
    }

    fn update_conflicts(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        for id in ids {
            if let Some(note) = self.library.notes.iter_mut().find(|note| note.id == id) {
                note.conflicted = true;
            }
        }
        cx.notify();
    }

    /// Bring the conflict dialog back for the active note. ⌘S, the footer indicator
    /// and the command all arrive here, so a note that was answered "Keep Mine" can be
    /// asked again from wherever the user looks for it.
    fn reopen_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        self.conflict_prompted.remove(&id);
        self.prompt_conflict(window, cx);
    }

    fn prompt_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.library.active_note();
        if !note.conflicted || self.conflict_dialog || self.conflict_prompted.contains(&note.id) {
            return;
        }
        let id = note.id.clone();
        let subject = conflict_subject(note);
        self.conflict_prompted.insert(id.clone());
        self.conflict_dialog = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("“{subject}” changed on disk"),
            Some("Your edits are kept either way."),
            &["Keep Mine", "Use Disk Version"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let load = answer.await == Ok(1);
            let _ = cx.update(|window, cx| {
                this.update(cx, |this, cx| {
                    this.conflict_dialog = false;
                    if load && this.library.active_id == id {
                        this.resolve_conflict(window, cx);
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn resolve_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        let note = self.library.active_note().clone();
        let Some(persistence) = &self.persistence else {
            return;
        };
        let result = persistence
            .review_conflict(note.clone())
            .and_then(|_| persistence.resolve_conflict(note.clone()));
        match result {
            Ok(result) => {
                match result {
                    Some(note) => self.library.adopt(note),
                    None => self.library.remove(&note.id),
                }
                self.sessions.remove(&note.id);
                self.ensure_session(window, cx);
                self.error = None;
                self.changed(cx);
                self.focus_editor(window, cx);
            }
            // The note stays conflicted and its indicator keeps saying so, so this is
            // one sentence about a failed load rather than a standing save banner.
            Err(error) => {
                self.queue_notice(format!("Could not load the version on disk: {error}"));
                cx.notify();
            }
        }
    }

    fn configure_new_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.path.clone() else {
            self.inform("Open a folder to set a default location for new notes.", cx);
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("New Notes Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let relative = root
                    .canonicalize()
                    .and_then(|root| path.canonicalize().map(|path| (root, path)))
                    .map_err(|error| error.to_string())
                    .and_then(|(root, path)| {
                        path.strip_prefix(root)
                            .map(ToOwned::to_owned)
                            .map_err(|_| "Choose a folder inside the notes folder.".to_owned())
                    });
                let _ = this.update(cx, |this, cx| match relative {
                    Ok(relative) if this.path.as_ref() == Some(&root) => {
                        this.library.workspace.new_note_directory = relative;
                        this.changed(cx);
                    }
                    Ok(_) => {
                        this.inform("The notes folder changed; choose the location again.", cx)
                    }
                    Err(error) => this.inform(error, cx),
                });
            }
        })
        .detach();
    }

    fn configure_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.path.clone() else {
            self.inform("Open a folder to set an image location.", cx);
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Image Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let relative = root
                    .canonicalize()
                    .and_then(|root| path.canonicalize().map(|path| (root, path)))
                    .map_err(|error| error.to_string())
                    .and_then(|(root, path)| {
                        path.strip_prefix(root)
                            .map(ToOwned::to_owned)
                            .map_err(|_| "Choose a folder inside the notes folder.".to_owned())
                    });
                let _ = this.update(cx, |this, cx| match relative {
                    Ok(relative) if this.path.as_ref() == Some(&root) => {
                        this.library.workspace.attachments =
                            crate::storage::AttachmentPolicy::WorkspaceFolder(relative);
                        this.changed(cx);
                    }
                    Ok(_) => {
                        this.inform("The notes folder changed; choose the location again.", cx)
                    }
                    Err(error) => this.inform(error, cx),
                });
            }
        })
        .detach();
    }

    fn insert_assets(
        &mut self,
        assets: Vec<assets::Asset>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if assets.is_empty() {
            return;
        }
        if self.library.active_note().read_only.is_some() || self.library.active_note().conflicted {
            self.queue_notice(
                "Resolve the file's read-only or conflict state before inserting images."
                    .to_owned(),
            );
            return;
        }
        // The note must be on disk before an image can be placed beside it.
        let id = self.library.active_id.clone();
        if !self.flush(cx) {
            return;
        }
        // In a folder, that flush is what files a new note, so the path it was given
        // arrives with these. Only a note the store had nothing to write — one that
        // is still empty — comes back without one.
        if let Some(persistence) = &self.persistence
            && let Ok(paths) = persistence.paths()
        {
            self.update_paths(paths, cx);
        }
        let Some(path) = self.library.active_note().path.clone() else {
            // The save panel has no room to say why it opened, so the reason goes
            // before it rather than into it.
            self.queue_notice(
                "Save this note first — images are stored next to its file.".to_owned(),
            );
            self.prompt_for_note_path(window, cx, move |this, window, cx| {
                this.insert_assets(assets, window, cx);
            });
            return;
        };
        let root = self
            .path
            .clone()
            .or_else(|| path.parent().map(ToOwned::to_owned))
            .unwrap_or_default();
        // Named in the one notice that cannot point at the open note any more.
        let beside = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let policy = self.library.workspace.attachments.clone();
        let journal = self
            .settings_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("image-imports");
        cx.spawn_in(window, async move |this, cx| {
            // The copy belongs to the note it was started in; where the caret is by
            // the time it finishes is the user's business, not a reason to drop it.
            if !this
                .update(cx, |this, _| this.library.active_id == id)
                .unwrap_or(false)
            {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { assets::insert(assets, &path, &root, &policy, &journal) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let inserted = match result {
                    Ok(inserted) => inserted,
                    Err(error) => {
                        this.queue_notice(error);
                        return;
                    }
                };
                // Anything that could not be inserted is already on disk, so the
                // sentence has to end with where it is or the file is lost to them.
                let kept =
                    |what: &str| format!("{what} The images are in {}.", inserted.urls.join(", "));
                let note = this.library.active_note();
                if this.library.active_id != id || note.read_only.is_some() || note.conflicted {
                    this.queue_notice(format!(
                        "The note changed before the images could be added. They are in {}, beside “{beside}”.",
                        inserted.urls.join(", ")
                    ));
                    return;
                }
                let slice = match markraft_commonmark::from_markdown_fragment(
                    doc::schema(),
                    &inserted.markdown,
                ) {
                    Ok(slice) => slice,
                    Err(error) => {
                        this.queue_notice(kept(&error.to_string()));
                        return;
                    }
                };
                // Typing during the copy only moves the caret; the images go where
                // it is now.
                let applied = this.editor().update(cx, |editor, cx| {
                    editor.run_command(&markraft_core::commands::replace_selection(slice), cx)
                });
                if !applied {
                    this.queue_notice(kept("This note would not take the images."));
                }
            });
        })
        .detach();
    }
}
/// What to tell someone whose keystroke the source-preserving codec refused. Each
/// case names the syntax that stood in the way and something they can do about it,
/// because "not saved" on its own leaves nowhere to go.
fn rejection_message(error: &markraft_commonmark::SourceError) -> String {
    use markraft_commonmark::SourceError;
    match error {
        SourceError::ProtectedSpan => {
            "Markraft leaves this Markdown exactly as written — math, a block anchor or a \
             callout's own first line. Edit that part in another editor."
        }
        SourceError::ProtectedBlock => {
            "This change would drop source Markraft cannot represent, such as a link reference \
             definition. Edit this section in another editor."
        }
        SourceError::UnsupportedEdit => {
            "Markraft could not write this change back without rewriting source it does not \
             represent. Your text is still here; use Export Markdown… for a copy."
        }
    }
    .to_owned()
}

/// A wiki link target without the `#heading`, `^block` or `#^block` it may end
/// with, and without the spaces around it.
///
/// Going to a place *inside* a note is not something this does, so the suffix
/// only ever names one; what is left is the note. An empty result is a link
/// into the note it was written in, which is nowhere to go.
fn wiki_link_page(target: &str) -> &str {
    let end = target.find(['#', '^']).unwrap_or(target.len());
    target[..end].trim()
}

/// `name` without a trailing `.md`, whatever case it is written in.
fn without_markdown(name: &str) -> &str {
    let start = name.len().saturating_sub(3);
    if name.is_char_boundary(start) && name[start..].eq_ignore_ascii_case(".md") {
        &name[..start]
    } else {
        name
    }
}

/// Which note a wiki link target names, resolved the way Obsidian does.
///
/// A target holding a `/` is a path relative to the notes folder, with or
/// without its `.md` extension; one without is a file stem, matched against
/// every note the folder holds. Both are matched without regard to case,
/// because the file systems these files live on do not keep it either.
///
/// Where more than one note answers, the one nearest `from` wins: a note in the
/// same directory first, then the shortest path relative to `root`, then that
/// path itself — so the answer never depends on the order the notes arrived in.
///
/// With no folder — the standalone window, where the open files have no root in
/// common — every target is matched by stem, because a path relative to nothing
/// names nothing.
/// The file a wiki link names, where the folder really holds one. A target is written
/// relative to the note or to the folder, the two places an image source is looked up,
/// and it may not climb out of either.
fn linked_file(
    page: &str,
    from: Option<&std::path::Path>,
    root: Option<&std::path::Path>,
) -> Option<PathBuf> {
    let relative = std::path::Path::new(page.trim_start_matches("./"));
    if !crate::vault::safe_relative(relative) {
        return None;
    }
    from.and_then(std::path::Path::parent)
        .into_iter()
        .chain(root)
        .map(|base| base.join(relative))
        .find(|path| path.is_file())
}

fn resolve_wiki_link<'a>(
    target: &str,
    from: Option<&std::path::Path>,
    root: Option<&std::path::Path>,
    notes: impl Iterator<Item = (&'a str, &'a std::path::Path)>,
) -> Option<String> {
    let page = wiki_link_page(target);
    if page.is_empty() {
        return None;
    }
    let wanted = without_markdown(page.trim_start_matches("./")).to_lowercase();
    let by_path = root.is_some() && wanted.contains('/');
    let here = from.and_then(std::path::Path::parent);
    let mut best: Option<(bool, usize, String, String)> = None;
    for (id, path) in notes {
        let relative = root
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path);
        let text = relative.to_string_lossy();
        let found = if by_path {
            without_markdown(&text).to_lowercase() == wanted
        } else {
            path.file_stem()
                .is_some_and(|stem| stem.to_string_lossy().to_lowercase() == wanted)
        };
        if !found {
            continue;
        }
        let key = (
            here != path.parent(),
            relative.components().count(),
            text.into_owned(),
            id.to_owned(),
        );
        if best.as_ref().is_none_or(|best| key < *best) {
            best = Some(key);
        }
    }
    best.map(|(_, _, _, id)| id)
}

/// What a drop is made of. Each kind has somewhere different to go, so a mixed drop
/// is split rather than sent whole to whichever handler the first path suggested.
#[derive(Debug, Default, PartialEq, Eq)]
struct Dropped {
    images: Vec<PathBuf>,
    markdown: Vec<PathBuf>,
    folders: Vec<PathBuf>,
    /// How many paths Markraft has nothing to do with, so one sentence can name them.
    skipped: usize,
}

/// Sort dropped paths by what Markraft can do with each. `is_folder` is passed in so
/// this stays a pure function; the caller asks the file system.
fn classify_drop(paths: &[PathBuf], is_folder: impl Fn(&std::path::Path) -> bool) -> Dropped {
    let mut dropped = Dropped::default();
    for path in paths {
        let markdown = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
            });
        if is_folder(path) {
            dropped.folders.push(path.clone());
        } else if assets::is_image(path) {
            dropped.images.push(path.clone());
        } else if markdown {
            dropped.markdown.push(path.clone());
        } else {
            dropped.skipped += 1;
        }
    }
    dropped
}

/// Where a note's file is, as a Browse row says it: relative to the notes folder
/// when there is one, and otherwise the absolute path with the home folder written
/// the way the user would write it.
fn note_location(
    path: &std::path::Path,
    root: Option<&std::path::Path>,
    home: Option<&str>,
) -> String {
    if let Some(relative) = root.and_then(|root| path.strip_prefix(root).ok()) {
        return relative.display().to_string();
    }
    let path = path.display().to_string();
    match home.filter(|home| !home.is_empty()) {
        Some(home) if path == home => "~".to_owned(),
        Some(home) => match path.strip_prefix(&format!("{}/", home.trim_end_matches('/'))) {
            Some(rest) => format!("~/{rest}"),
            None => path,
        },
        None => path,
    }
}

/// Shorten a location so a narrow row still ends in the file name, which is the part
/// `truncate` would otherwise cut. The name and the folder holding it identify the
/// note, so the folders above them give way first.
///
/// `truncate` still handles whatever does not fit; see [`location_budget`] for how
/// much a row has.
fn shorten_location(location: &str, budget: usize) -> String {
    if location.chars().count() <= budget {
        return location.to_owned();
    }
    let parts: Vec<_> = location.split('/').collect();
    for first in 1..parts.len() {
        let candidate = format!("…/{}", parts[first..].join("/"));
        // The last pair is kept whatever it measures: there is nothing else to drop.
        if candidate.chars().count() <= budget || first + 1 >= parts.len() {
            return candidate;
        }
    }
    location.to_owned()
}

/// How many characters of a row's second line are left for the location once the
/// status in front of it has been written. The status varies from "Current" to
/// "Edited yesterday", so a fixed share would cut the file name on the long ones and
/// waste room on the short ones.
fn location_budget(status: &str, current: bool, deleted: bool, selected: bool) -> usize {
    let line = match (deleted, selected) {
        (false, _) => LIVE_META_CHARS,
        (true, false) => DELETED_META_CHARS,
        (true, true) => DELETED_SELECTED_META_CHARS,
    };
    // " ·" and the gap after it, and the dot that marks the current note.
    let taken = status.chars().count() + 3 + usize::from(current);
    line.saturating_sub(taken)
}

/// Where inside the notes folder a setting points, written the way the user reads
/// the folder itself: its own name, then the path under it.
fn folder_label(root: &std::path::Path, relative: &std::path::Path) -> String {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    if relative.as_os_str().is_empty() {
        name
    } else {
        format!("{name}/{}", relative.display())
    }
}

/// Whether a note is work that is not in a file the way it was left: it has no file
/// yet, or the file says something else. A blank page with no file is not work: the
/// store never writes one, and the library always keeps one open to type into.
pub(crate) fn is_draft(note: &crate::storage::Note) -> bool {
    note.deleted_at.is_none()
        && (note.conflicted || (note.path.is_none() && !note.document_is_empty()))
}

/// Whether `selection` still reaches the block the note's title — and so the name of
/// the file it would be filed under — is read from.
///
/// A selection covers the blocks between its ends, and touching that one anywhere is
/// enough. Select All reaches from the start of the document to past its last block,
/// and a drag out of the first line leaves the other end behind; neither is someone
/// moving on from the title, and filing the note on one of them would name its file
/// after however much of the title had been typed. Letting go a moment late costs
/// nothing, because the name settles on its own after [`NAME_SETTLES`].
///
/// A document with nothing to read has no such block for a selection to reach.
fn naming_title(doc: &Node, selection: &Selection) -> bool {
    let block_at = |pos: usize| Some(doc.resolve(pos).ok()?.index(0));
    doc::title_block(doc)
        .zip(block_at(selection.from(doc)).zip(block_at(selection.to(doc))))
        .is_some_and(|(title, (first, last))| (first..=last).contains(&title))
}

/// What the conflict dialog calls the note: the file another app changed, or the
/// note's own title while it has no file yet.
fn conflict_subject(note: &crate::storage::Note) -> String {
    note.path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| note.title())
}

/// Heights of the toolbar and footer, which float over the top and bottom of the note.
const TOOLBAR_HEIGHT: Pixels = px(52.);
const FOOTER_HEIGHT: Pixels = px(48.);
/// How long a new note's first line has to stand still before it names the file. Long
/// enough that a pause for thought mid-title does not name the file after half of it,
/// and short enough that a note which is only that line still reaches the folder.
const NAME_SETTLES: Duration = Duration::from_millis(2000);
/// How long a keystroke counts as someone being at the window.
const KEY_PRESENCE: Duration = Duration::from_millis(2500);
/// A queued notice is a sentence, not an acknowledgment, so it is given time to read.
const READING_NOTICE: Duration = Duration::from_secs(8);
/// Characters a live Browse row's second line holds at 12px. The card is a fixed
/// width and every live row keeps 60px clear for its buttons, which leaves about
/// 226px; measured on a real run the line averages 6px a character.
const LIVE_META_CHARS: usize = 38;
/// The same line in Recently Deleted, whose buttons sit beside it only while the row
/// is selected and then take 202px of it.
const DELETED_META_CHARS: usize = 46;
const DELETED_SELECTED_META_CHARS: usize = 12;
/// How long the file status indicator stays lit after a keystroke the file refused.
/// Long enough to be seen without following the typing that provoked it.
const FILE_STATUS_FLASH: Duration = Duration::from_millis(900);
/// The shortest the window is allowed to become while it follows its content. Below
/// this the chrome has nowhere to sit, so a window with less room than this keeps the
/// height and lets the editor scroll instead.
const MINIMUM_HEIGHT: Pixels = px(220.);
fn notes_style(dark: bool) -> EditorStyle {
    let mut style = if dark {
        EditorStyle::notes_dark()
    } else {
        EditorStyle::notes()
    };
    // The whole toolbar height, so a block that runs under it — a code block's header
    // bar, a heading — is scrolled clear of the capsule rather than behind it.
    style.top_overlay = TOOLBAR_HEIGHT;
    style.bottom_overlay = FOOTER_HEIGHT;
    style
}
fn query_style(dark: bool) -> EditorStyle {
    let mut style = notes_style(dark);
    style.background = if dark { rgb(0x2e2f33) } else { rgb(0xf8f8f8) }.into();
    style.body_size = px(13.);
    style.padding = px(0.);
    style.paragraph_gap = px(0.);
    style.top_overlay = px(0.);
    style.bottom_overlay = px(0.);
    style
}
pub fn bind_app_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", markraft_gpui::Up, Some("MarkraftApp")),
        KeyBinding::new("down", markraft_gpui::Down, Some("MarkraftApp")),
        KeyBinding::new("enter", markraft_gpui::Enter, Some("MarkraftApp")),
        // Tab walks the open panel's controls. The editor binds the same keys in its own,
        // deeper context, so a note still indents; the app intercepts them in the capture
        // phase and only keeps them while a panel or popover is open.
        KeyBinding::new("tab", markraft_gpui::Indent, Some("MarkraftApp")),
        KeyBinding::new("shift-tab", markraft_gpui::Outdent, Some("MarkraftApp")),
        KeyBinding::new("cmd-s", Save, Some("MarkraftApp")),
        KeyBinding::new("cmd-shift-c", CopyMarkdown, Some("MarkraftApp")),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("escape", Hide, Some("MarkraftApp")),
        KeyBinding::new("cmd-n", NewNote, Some("MarkraftApp")),
        KeyBinding::new("cmd-p", Browse, Some("MarkraftApp")),
        KeyBinding::new("cmd-k", Actions, Some("MarkraftApp")),
        KeyBinding::new("cmd-,", Settings, Some("MarkraftApp")),
        KeyBinding::new("cmd-l", Link, Some("MarkraftApp")),
        KeyBinding::new("cmd-shift-e", Export, Some("MarkraftApp")),
        KeyBinding::new("cmd-o", OpenMarkdown, Some("MarkraftApp")),
    ]);
    cx.set_menus([
        Menu::new("Markraft").items([
            MenuItem::action("Show Notes", Show),
            MenuItem::action("Settings…", Settings),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Quit Markraft", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Browse Notes", Browse),
            MenuItem::action("Save Now", Save),
            MenuItem::action("Open Markdown…", OpenMarkdown),
            MenuItem::action("Export Markdown…", Export),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Undo", markraft_gpui::Undo),
            MenuItem::action("Redo", markraft_gpui::Redo),
            MenuItem::separator(),
            MenuItem::action("Cut", markraft_gpui::Cut),
            MenuItem::action("Copy", markraft_gpui::Copy),
            MenuItem::action("Paste", markraft_gpui::Paste),
            MenuItem::action("Paste as Plain Text", markraft_gpui::PastePlain),
            MenuItem::action("Paste as Markdown", markraft_gpui::PasteMarkdown),
            MenuItem::action("Select All", markraft_gpui::SelectAll),
        ]),
    ]);
}

#[cfg(test)]
mod tests {
    // Not a glob: `gpui::prelude` carries a `test` attribute of its own, and these
    // are ordinary unit tests.
    use super::{
        classify_drop, conflict_subject, folder_label, linked_file, location_budget, note_location,
        rejection_message, resolve_wiki_link, shorten_location, wiki_link_page,
    };
    use crate::{doc, storage::Library};
    use std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
    };

    fn note(markdown: &str) -> crate::storage::Note {
        let mut library = Library::default();
        let id = library.new_note(doc::from_markdown(markdown));
        library.note(&id).unwrap().clone()
    }

    /// The folder a wiki link is followed in: ids paired with the paths of the
    /// notes it holds.
    fn folder() -> Vec<(&'static str, PathBuf)> {
        [
            ("root", "/notes/Note.md"),
            ("inbox", "/notes/inbox/Note.md"),
            ("deep", "/notes/a/b/c/Note.md"),
            ("other", "/notes/Other.md"),
            ("nested", "/notes/projects/Plan.md"),
            ("spaced", "/notes/inbox/Weekly Review.md"),
        ]
        .into_iter()
        .map(|(id, path)| (id, PathBuf::from(path)))
        .collect()
    }

    fn resolve(target: &str, from: &str) -> Option<String> {
        let notes = folder();
        resolve_wiki_link(
            target,
            Some(Path::new(from)),
            Some(Path::new("/notes")),
            notes.iter().map(|(id, path)| (*id, path.as_path())),
        )
    }

    #[test]
    fn a_wiki_link_target_loses_the_place_it_names_inside_a_note() {
        assert_eq!(wiki_link_page("Note"), "Note");
        assert_eq!(wiki_link_page("Note#Heading"), "Note");
        assert_eq!(wiki_link_page("Note^block-id"), "Note");
        assert_eq!(wiki_link_page("Note#^block-id"), "Note");
        assert_eq!(wiki_link_page(" Note "), "Note");
        assert_eq!(wiki_link_page("#Heading"), "");
    }

    #[test]
    fn an_embed_finds_the_file_beside_the_note_or_in_the_folder_and_climbs_out_of_neither() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path();
        fs::create_dir_all(folder.join("sub/assets")).unwrap();
        fs::write(folder.join("sub/assets/near.png"), b"near").unwrap();
        fs::write(folder.join("top.png"), b"top").unwrap();
        fs::write(root.path().join("outside.png"), b"outside").unwrap();
        let note = folder.join("sub/Note.md");
        let find = |page: &str| linked_file(page, Some(&note), Some(folder));
        // Beside the note, and the same file written the other way.
        assert_eq!(
            find("assets/near.png"),
            Some(folder.join("sub/assets/near.png"))
        );
        assert_eq!(
            find("./assets/near.png"),
            Some(folder.join("sub/assets/near.png"))
        );
        // Not beside the note, so the folder root answers for it.
        assert_eq!(find("top.png"), Some(folder.join("top.png")));
        // A name the folder does not hold stays unresolved, and `..` never escapes.
        assert_eq!(find("missing.png"), None);
        assert_eq!(find("../outside.png"), None);
        assert_eq!(find("/etc/hosts"), None);
    }

    #[test]
    fn a_wiki_link_resolves_by_stem_by_path_and_without_regard_to_case() {
        // A bare stem, from the note beside it.
        assert_eq!(resolve("Other", "/notes/Note.md").as_deref(), Some("other"));
        assert_eq!(resolve("other", "/notes/Note.md").as_deref(), Some("other"));
        assert_eq!(
            resolve("Other.md", "/notes/Note.md").as_deref(),
            Some("other")
        );
        assert_eq!(
            resolve("Weekly Review", "/notes/Note.md").as_deref(),
            Some("spaced")
        );
        // A place inside the note does not change which note it is.
        assert_eq!(
            resolve("Other#Heading", "/notes/Note.md").as_deref(),
            Some("other")
        );
        assert_eq!(
            resolve("Other#^b-1", "/notes/Note.md").as_deref(),
            Some("other")
        );
        // A `/` makes it a path relative to the folder, with or without `.md`.
        for target in [
            "projects/Plan",
            "projects/Plan.md",
            "PROJECTS/plan",
            "./projects/Plan",
        ] {
            assert_eq!(
                resolve(target, "/notes/Note.md").as_deref(),
                Some("nested"),
                "{target:?}"
            );
        }
        assert_eq!(resolve("projects/Missing", "/notes/Note.md"), None);
        assert_eq!(resolve("Missing", "/notes/Note.md"), None);
        assert_eq!(resolve("#Heading", "/notes/Note.md"), None);
    }

    #[test]
    fn an_ambiguous_stem_picks_the_nearest_note_the_same_way_every_time() {
        // Three notes are called `Note`; the one in the linking note's own
        // directory wins wherever the link is followed from.
        assert_eq!(
            resolve("Note", "/notes/inbox/x.md").as_deref(),
            Some("inbox")
        );
        assert_eq!(
            resolve("Note", "/notes/a/b/c/x.md").as_deref(),
            Some("deep")
        );
        // From anywhere else the shortest path relative to the folder wins.
        assert_eq!(
            resolve("Note", "/notes/projects/x.md").as_deref(),
            Some("root")
        );
        // And the order the notes arrive in does not change the answer.
        let mut notes = folder();
        notes.reverse();
        assert_eq!(
            resolve_wiki_link(
                "Note",
                Some(Path::new("/notes/projects/x.md")),
                Some(Path::new("/notes")),
                notes.iter().map(|(id, path)| (*id, path.as_path())),
            )
            .as_deref(),
            Some("root")
        );
    }

    #[test]
    fn a_wiki_link_never_resolves_to_a_note_that_is_not_offered() {
        // Deleted notes are kept out by the caller, so a folder without them
        // finds nothing rather than something unopenable.
        let notes = folder();
        let kept: Vec<_> = notes
            .iter()
            .filter(|(id, _)| *id != "other")
            .map(|(id, path)| (*id, path.as_path()))
            .collect();
        assert_eq!(
            resolve_wiki_link(
                "Other",
                Some(Path::new("/notes/Note.md")),
                Some(Path::new("/notes")),
                kept.into_iter(),
            ),
            None
        );
    }

    #[test]
    fn standalone_files_resolve_by_stem_alone() {
        let notes = [
            ("a", PathBuf::from("/tmp/one/Note.md")),
            ("b", PathBuf::from("/var/two/Plan.md")),
        ];
        let resolve = |target: &str| {
            resolve_wiki_link(
                target,
                Some(Path::new("/tmp/one/Note.md")),
                None,
                notes.iter().map(|(id, path)| (*id, path.as_path())),
            )
        };
        assert_eq!(resolve("Plan").as_deref(), Some("b"));
        // With no folder there is nothing for a path to be relative to, so the
        // last component is all that is matched.
        assert_eq!(resolve("two/Plan").as_deref(), None);
        assert_eq!(resolve("Plan.md").as_deref(), Some("b"));
    }

    #[test]
    fn a_conflict_names_the_file_and_falls_back_to_the_title() {
        let mut note = note("# Meeting\n\nnotes");
        assert_eq!(conflict_subject(&note), "Meeting");
        note.path = Some(PathBuf::from("/tmp/notes/2024-05 Meeting.md"));
        assert_eq!(conflict_subject(&note), "2024-05 Meeting.md");
    }

    #[test]
    fn every_refusal_says_which_syntax_stood_in_the_way() {
        use markraft_commonmark::SourceError;
        let messages: Vec<_> = [
            SourceError::ProtectedSpan,
            SourceError::ProtectedBlock,
            SourceError::UnsupportedEdit,
        ]
        .iter()
        .map(rejection_message)
        .collect();
        assert_eq!(
            messages.iter().collect::<HashSet<_>>().len(),
            messages.len(),
            "each case needs its own sentence: {messages:?}"
        );
        // A wiki link is a node of its own now, so it is not on the list.
        assert!(messages[0].contains("callout"), "{}", messages[0]);
        assert!(!messages[0].contains("wiki link"), "{}", messages[0]);
        assert!(
            messages[1].contains("link reference definition"),
            "{}",
            messages[1]
        );
    }

    #[test]
    fn a_drop_is_split_by_what_each_path_is_for() {
        let paths = [
            "/notes/photo.PNG",
            "/notes/Readme.md",
            "/notes/deep",
            "/notes/archive.zip",
            "/notes/other.markdown",
            "/notes/notes.txt",
        ]
        .map(PathBuf::from);
        let dropped = classify_drop(&paths, |path| path.ends_with("deep"));
        assert_eq!(dropped.images, [PathBuf::from("/notes/photo.PNG")]);
        assert_eq!(
            dropped.markdown,
            [
                PathBuf::from("/notes/Readme.md"),
                PathBuf::from("/notes/other.markdown")
            ]
        );
        assert_eq!(dropped.folders, [PathBuf::from("/notes/deep")]);
        assert_eq!(dropped.skipped, 2);
        // A folder wins over its name looking like anything else.
        let folder = [PathBuf::from("/notes/pictures.md")];
        assert_eq!(classify_drop(&folder, |_| true).folders, folder);
        assert!(classify_drop(&[], |_| false) == super::Dropped::default());
    }

    #[test]
    fn a_longer_status_leaves_the_location_less_room() {
        let current = location_budget("Current", true, false, false);
        let today = location_budget("Edited today", false, false, true);
        let yesterday = location_budget("Edited yesterday", false, false, false);
        assert!(
            current > today && today > yesterday,
            "{current} {today} {yesterday}"
        );
        // Selection does not move a live row's text: its buttons' room is always kept.
        assert_eq!(today, location_budget("Edited today", false, false, false));
        // A selected deleted row gives the line to its buttons, and never underflows.
        assert_eq!(location_budget("Deleted yesterday", false, true, true), 0);
        assert!(location_budget("Deleted today", false, true, false) > today);
    }

    #[test]
    fn a_narrow_row_keeps_the_file_name_and_the_folder_around_it() {
        // Short enough already: nothing is elided.
        assert_eq!(shorten_location("Inbox/Meeting.md", 24), "Inbox/Meeting.md");
        // The folders above the last one give way first, one at a time.
        assert_eq!(
            shorten_location("Work/Clients/Acme/Q3/Meeting notes.md", 24),
            "…/Q3/Meeting notes.md"
        );
        assert_eq!(
            shorten_location("Work/Clients/Acme/A very long meeting name.md", 24),
            "…/A very long meeting name.md"
        );
        // A bare name has nothing to drop, so it is left to `truncate`.
        let long = "an extremely long file name that will not fit.md";
        assert_eq!(shorten_location(long, 24), long);
    }

    #[test]
    fn a_location_is_relative_to_the_folder_or_written_from_home() {
        let root = PathBuf::from("/Users/someone/Notes");
        assert_eq!(
            note_location(
                &root.join("Inbox/A.md"),
                Some(&root),
                Some("/Users/someone")
            ),
            "Inbox/A.md"
        );
        // Outside any folder the home directory is written the way it is typed.
        assert_eq!(
            note_location(
                Path::new("/Users/someone/Desktop/A.md"),
                None,
                Some("/Users/someone")
            ),
            "~/Desktop/A.md"
        );
        assert_eq!(
            note_location(Path::new("/tmp/A.md"), None, Some("/Users/someone")),
            "/tmp/A.md"
        );
        // A prefix that is not a whole path component is not the home directory.
        assert_eq!(
            note_location(
                Path::new("/Users/someone2/A.md"),
                None,
                Some("/Users/someone")
            ),
            "/Users/someone2/A.md"
        );
        assert_eq!(
            note_location(Path::new("/tmp/A.md"), None, None),
            "/tmp/A.md"
        );
    }

    #[test]
    fn a_placement_reads_as_the_folder_the_user_opened() {
        let root = Path::new("/Users/someone/Documents/Notes");
        assert_eq!(folder_label(root, Path::new("")), "Notes");
        assert_eq!(
            folder_label(root, Path::new("Inbox/Daily")),
            "Notes/Inbox/Daily"
        );
    }

    #[test]
    fn a_draft_is_a_note_with_no_file_or_one_its_file_disagrees_with() {
        let mut library = Library::default();
        let id = library.new_note(doc::from_markdown("Unfiled"));
        let note = |library: &Library, id: &str| {
            library.notes.iter().find(|n| n.id == id).unwrap().clone()
        };
        // Nothing has been written for it yet, so it lives only in the app.
        assert!(super::is_draft(&note(&library, &id)));
        // The blank page an empty library opens on has nothing in it to file.
        let blank = library.new_note(doc::empty());
        assert!(!super::is_draft(&note(&library, &blank)));
        // Given a file it agrees with, it is an ordinary note.
        let filed = library.notes.iter_mut().find(|n| n.id == id).unwrap();
        filed.path = Some(PathBuf::from("/notes/Unfiled.md"));
        assert!(!super::is_draft(&note(&library, &id)));
        // Until the file says something else.
        library
            .notes
            .iter_mut()
            .find(|n| n.id == id)
            .unwrap()
            .conflicted = true;
        assert!(super::is_draft(&note(&library, &id)));
        // Something thrown away is not waiting to be filed.
        let gone = library.notes.iter_mut().find(|n| n.id == id).unwrap();
        gone.deleted_at = Some(1);
        assert!(!super::is_draft(&note(&library, &id)));
    }

    #[test]
    fn the_caret_is_naming_the_file_until_it_leaves_the_title_line() {
        let inside = |source: &str| {
            let document = doc::from_markdown(source);
            (0..=document.content_size())
                .filter(|pos| {
                    super::naming_title(&document, &markraft_core::Selection::cursor(*pos))
                })
                .collect::<Vec<_>>()
        };
        // "Meet" is the line the file would be named after, so the caret is still
        // naming it anywhere in that paragraph; the body below it is not.
        assert_eq!(inside("Meet\n\nbody"), (0..=5).collect::<Vec<_>>());
        // A block with nothing to read is not the title line: the name comes from the
        // first line that says something.
        assert_eq!(inside("***\n\nMeet\n\nbody"), (1..=6).collect::<Vec<_>>());
        // A note with nothing to read has no title line for the caret to be in.
        assert!(inside("***").is_empty());
    }

    #[test]
    fn a_selection_reaching_out_of_the_title_line_is_still_naming_it() {
        use markraft_core::Selection;
        let document = doc::from_markdown("Meet\n\nbody");
        let naming = |selection: Selection| super::naming_title(&document, &selection);
        // Select All runs past the last block, to a position in no block at all.
        // Copying a half-typed note must not be what files it.
        assert!(naming(Selection::All));
        // Nor does dragging out of the title into the body, either way round.
        assert!(naming(Selection::text(2, 8)));
        assert!(naming(Selection::text(8, 2)));
        // Having moved on to the body is.
        assert!(!naming(Selection::cursor(8)));
        assert!(!naming(Selection::text(7, 10)));
        // A note whose title is not its first block is reached all the same: the
        // anchor Select All leaves at the start of the document is in the rule above
        // it, not in the line being named.
        let ruled = doc::from_markdown("***\n\nMeet\n\nbody");
        assert!(super::naming_title(&ruled, &Selection::All));
        assert!(!super::naming_title(&ruled, &Selection::cursor(9)));
    }
}

#[cfg(test)]
mod scratch_probe {
    use crate::{doc, storage::Library};
    #[test]
    fn probe_blank_and_names() {
        for source in [
            " ", "   ", "\t", "\u{00a0}", ".", "...", "[[Link]]", "#", "- ",
        ] {
            let d = doc::from_markdown(source);
            let mut lib = Library::default();
            let id = lib.new_note(d.clone());
            println!(
                "{source:?} blank={} title={:?} draft={}",
                doc::is_blank(&d),
                lib.note(&id).unwrap().title(),
                crate::app::is_draft(lib.note(&id).unwrap()),
            );
        }
    }
}
