mod assets;
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
use markraft_core::MarkSet;
use markraft_gpui::{
    ColumnAlignment, EditorEvent, EditorStyle, EditorView, ExtensionHandle, Setup, TableInfo,
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
        Trash,
        Export,
        Import
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Editor,
    Browse,
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
    _format_changes: Subscription,
    /// Unregisters the note's editor extensions when the session is evicted.
    _extensions: [ExtensionHandle; 3],
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
    error: Option<String>,
    platform_error: Option<String>,
    notice: Option<Notice>,
    /// Things the notes folder gave the user to read. They are sentences rather than
    /// acknowledgments, so they wait their turn instead of replacing one another.
    pending_notices: VecDeque<String>,
    conflict_prompted: HashSet<String>,
    conflict_dialog: bool,
    show_words: bool,
    format_toolbar: bool,
    format_menu: Option<FormatMenu>,
    link_popover: Option<LinkPopover>,
    /// The table the toolbar was last drawn for. It is paint geometry, so it is only
    /// ever as fresh as the last frame, which is also the frame the keyboard walks.
    table: Option<TableInfo>,
    format_selected: usize,
    format_snapshot: Option<(MarkSet, Option<doc::Block>)>,
    dark: bool,
    // Corner action buttons and traffic lights follow window hover alone.
    pointer_inside: bool,
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
            if this.window_active
                && let Some(persistence) = &this.persistence
            {
                persistence.refresh();
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
            error,
            platform_error,
            notice: None,
            pending_notices: VecDeque::new(),
            conflict_prompted: HashSet::new(),
            conflict_dialog: false,
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
                    return Err(format!("Read-only: {reason}"));
                }
                match &source {
                    Some(Ok(source)) => source
                        .render(doc::schema(), candidate)
                        .map(|_| ())
                        .map_err(|_| {
                            "This edit would rewrite protected Markdown and was not applied."
                                .to_owned()
                        }),
                    Some(Err(error)) => Err(format!("Cannot safely edit this file: {error}")),
                    None => Ok(()),
                }
            })
            .with_placeholder("Start writing…")
        });
        // Only note editors get the menus; the host's query field gets no extension.
        // The `/` menu is registered first: the two typeaheads derive from the same
        // caret and their triggers are disjoint, so only one is ever open, but were they
        // ever to overlap the first registered one would own the popup and the commands
        // matter more than the emoji. Auto-replace goes last so that it sees the menu's
        // view of a keystroke settled before it edits.
        let menu = self.slash_menu();
        let extensions = editor.update(cx, |editor, cx| {
            [
                editor.add_extension(menu, cx),
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
        let format_note_id = id.clone();
        let format_changes = cx.observe(&editor, move |this, editor, cx| {
            if this.format_toolbar && this.library.active_id == format_note_id {
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
                _format_changes: format_changes,
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
        for change in changes {
            let (External::Updated { note, .. } | External::Removed(note)) = &change;
            let id = note.id.clone();
            self.conflict_prompted.remove(&id);
            let local = self.library.note(&id).cloned();
            match change {
                External::Updated { previous, note } => {
                    if let Some(local) = local
                        && local.deleted_at.is_none()
                        && local.document != note.document
                        && previous.is_none_or(|previous| previous.document != local.document)
                    {
                        if let Some(persistence) = &self.persistence
                            && let Err(error) = persistence.recover(local.clone())
                        {
                            self.error = Some(error);
                        }
                        let mut local = local;
                        local.conflicted = true;
                        self.library.adopt(local);
                        kept += 1;
                        continue;
                    }
                    self.library.adopt(note);
                }
                External::Removed(note) => {
                    if local
                        .as_ref()
                        .is_none_or(|local| local.document == note.document)
                    {
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
        self.prompt_conflict(window, cx);
        cx.notify();
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.dirty = true;
        self.save_at = Some(Instant::now() + Duration::from_millis(350));
        cx.notify();
    }
    fn flush(&mut self, cx: &mut Context<Self>) -> bool {
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
        if let Some(error) = self
            .editor()
            .update(cx, |editor, _| editor.take_edit_error())
        {
            self.inform(error, cx);
        }
        if self.panel == Panel::Editor {
            self.editor()
                .update(cx, |editor, cx| editor.refresh_images(cx));
        }
        if let Some(platform) = &self.platform {
            let inside = platform.pointer_inside(window);
            if inside != self.pointer_inside {
                self.pointer_inside = inside;
                platform.set_traffic_lights_alpha(
                    window,
                    if inside { 1. } else { 0. },
                    !cx.reduce_motion(),
                );
                cx.notify();
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
                        Err(e) => {
                            if !self.library.active_note().conflicted {
                                self.error = Some(e);
                            }
                        }
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
        if self.save_at.is_some_and(|at| Instant::now() >= at) {
            self.save_at = None;
            if let Some(p) = &self.persistence
                && let Err(e) = p.save(self.revision, self.library.clone())
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
            let maximum = window
                .display(cx)
                .map(|d| d.visible_bounds().size.height * 0.8)
                .unwrap_or(px(720.));
            // The toolbar and footer float over the editor and are already part of its
            // content height; only an error banner adds to it.
            let chrome = if self.error.is_some() {
                px(64.)
            } else {
                px(0.)
            };
            let desired = px(f32::from((height + chrome).max(px(220.)).min(maximum)).round());
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
        }
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
            | self.code_language_block.take().is_some();
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
        self.query.update(cx, |e, cx| e.cancel_composition(cx));
        if self.persistence.is_none() {
            self.choose_folder(window, cx);
            return;
        }
        if self.path.is_none() {
            let directory = self
                .library
                .active_note()
                .path
                .as_ref()
                .and_then(|path| path.parent())
                .map(ToOwned::to_owned)
                .unwrap_or_default();
            let prompt = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(Some(path))) = prompt.await {
                    let _ = cx.update(|window, cx| {
                        this.update(cx, |this, cx| {
                            if path.exists() {
                                this.inform(
                                    "Choose a new filename; the existing file was not changed.",
                                    cx,
                                );
                                return;
                            }
                            this.sync_documents(cx);
                            let id = this.library.new_note(doc::empty());
                            if let Some(note) =
                                this.library.notes.iter_mut().find(|note| note.id == id)
                            {
                                note.path = Some(path);
                            }
                            this.ensure_session(window, cx);
                            this.panel = Panel::Editor;
                            this.focus_editor(window, cx);
                            this.changed(cx);
                        })
                    });
                }
            })
            .detach();
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
    fn matching_notes(&self, query: &str, deleted: bool) -> Vec<&crate::storage::Note> {
        let mut notes = self.library.search(query, deleted);
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
            .matching_notes(self.query.read(cx).text().trim(), false)
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
            self.selected = self.selected.min(
                self.library
                    .search(query.trim(), false)
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
                .search(self.query.read(cx).text().trim(), true)
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
            .search("", true)
            .iter()
            .map(|note| note.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!(
                "Permanently delete {} {}?",
                ids.len(),
                if ids.len() == 1 { "note" } else { "notes" }
            ),
            Some("Recently Deleted will be emptied. This cannot be undone."),
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
                    .search(self.query.read(cx).text(), true)
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
            "Reload notes from disk?",
            Some(
                "This discards unsaved changes in this app. \
                 Save a library copy first if you need to keep them.",
            ),
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
            Err(error) => {
                self.error = Some(error);
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
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        for path in paths {
            if path.is_dir() {
                self.open_folder(path, window, cx);
                continue;
            }
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
            if let Err(error) = result {
                self.error = Some(error);
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
        if self.library.active_note().conflicted {
            self.error = None;
        }
        cx.notify();
    }

    fn prompt_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.library.active_note();
        if !note.conflicted || self.conflict_dialog || self.conflict_prompted.contains(&note.id) {
            return;
        }
        let id = note.id.clone();
        self.conflict_prompted.insert(id.clone());
        self.conflict_dialog = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "File changed externally",
            Some("Keep your edits or load the latest version."),
            &["Not Now", "Load Changes"],
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
            Err(error) => {
                self.error = Some(error);
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
                            .map_err(|_| "Choose a folder inside the current workspace.".to_owned())
                    });
                let _ = this.update(cx, |this, cx| match relative {
                    Ok(relative) if this.path.as_ref() == Some(&root) => {
                        this.library.workspace.new_note_directory = relative;
                        this.changed(cx);
                    }
                    Ok(_) => this.inform("The workspace changed; choose the location again.", cx),
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
                            .map_err(|_| "Choose a folder inside the workspace.".to_owned())
                    });
                let _ = this.update(cx, |this, cx| match relative {
                    Ok(relative) if this.path.as_ref() == Some(&root) => {
                        this.library.workspace.attachments =
                            crate::storage::AttachmentPolicy::WorkspaceFolder(relative);
                        this.changed(cx);
                    }
                    Ok(_) => this.inform("The workspace changed; choose the location again.", cx),
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
            self.inform(
                "Resolve the file's read-only or conflict state before inserting images.",
                cx,
            );
            return;
        }
        // Capture document and selection before any prompt or asynchronous copy.
        let id = self.library.active_id.clone();
        let document = self.editor().read(cx).committed_document().clone();
        let selection = self.editor().read(cx).state().selection().clone();
        if !self.flush(cx) {
            return;
        }
        if let Some(persistence) = &self.persistence
            && let Ok(paths) = persistence.paths()
        {
            self.update_paths(paths, cx);
        }
        let Some(path) = self.library.active_note().path.clone() else {
            let directory = self
                .path
                .as_ref()
                .map(|root| root.join(&self.library.workspace.new_note_directory))
                .unwrap_or_default();
            let prompt = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(Some(path))) = prompt.await {
                    let _ = cx.update(|window, cx| this.update(cx, |this, cx| {
                        if this.library.active_id != id || this.editor().read(cx).committed_document() != &document { return; }
                        if path.exists() { this.inform("Choose a new Markdown filename; the existing file was not changed.", cx); return; }
                        let parent = path.parent().map(ToOwned::to_owned);
                        if let Some(note) = this.library.notes.iter_mut().find(|note| note.id == id) { note.path = Some(path); }
                        this.editor().update(cx, |editor, cx| editor.set_image_base(parent, cx));
                        this.insert_assets(assets, window, cx);
                    }));
                }
            }).detach();
            return;
        };
        let root = self
            .path
            .clone()
            .or_else(|| path.parent().map(ToOwned::to_owned))
            .unwrap_or_default();
        let policy = self.library.workspace.attachments.clone();
        let journal = self
            .settings_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("image-imports");
        cx.spawn_in(window, async move |this, cx| {
            let valid = this.update(cx, |this, cx| {
                this.library.active_id == id && this.editor().read(cx).committed_document() == &document
                    && this.editor().read(cx).state().selection() == &selection
            }).unwrap_or(false);
            if !valid { return; }
            let result = cx.background_executor().spawn(async move { assets::insert(assets, &path, &root, &policy, &journal) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.library.active_id != id || this.editor().read(cx).committed_document() != &document
                    || this.editor().read(cx).state().selection() != &selection {
                    this.inform("The note or selection changed. Copied images were kept; insert them again at the intended position.", cx); return;
                }
                match result {
                    Ok(markdown) => {
                        match markraft_commonmark::from_markdown_fragment(doc::schema(), &markdown) {
                            Ok(slice) => {
                                this.editor().update(cx, |editor, cx| { editor.run_command(&markraft_core::commands::replace_selection(slice), cx); });
                            }
                            Err(error) => this.inform(error.to_string(), cx),
                        }
                    }
                    Err(error) => this.inform(error, cx),
                }
            });
        }).detach();
    }
}
/// Heights of the toolbar and footer, which float over the top and bottom of the note.
const TOOLBAR_HEIGHT: Pixels = px(52.);
const FOOTER_HEIGHT: Pixels = px(44.);
/// How long a keystroke counts as someone being at the window.
const KEY_PRESENCE: Duration = Duration::from_millis(2500);
/// A queued notice is a sentence, not an acknowledgment, so it is given time to read.
const READING_NOTICE: Duration = Duration::from_secs(8);
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
        KeyBinding::new("cmd-o", Import, Some("MarkraftApp")),
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
            MenuItem::action("Open Markdown…", Import),
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
