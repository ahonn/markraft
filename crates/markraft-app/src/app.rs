mod ui;

use crate::doc;
use crate::{
    instance::Instance,
    persistence::{Event, Persistence},
    platform::{Platform, PlatformEvent},
    storage::Library,
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
    dirty: bool,
    revision: u64,
    save_at: Option<Instant>,
    error: Option<String>,
    platform_error: Option<String>,
    notice: Option<Notice>,
    /// Things the notes folder gave the user to read. They are sentences rather than
    /// acknowledgments, so they wait their turn instead of replacing one another.
    pending_notices: VecDeque<String>,
    /// Notes already warned about being read from a file that is not valid text.
    lossy_warned: HashSet<String>,
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
    // The pointer is over the window; toolbar chrome recedes while it is away.
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
            cx.notify();
        });
        let quit = cx.on_app_quit(|this, cx| {
            if !this.flush(cx) {
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
        let mut app = Self {
            library,
            persistence: store.map(Persistence::new),
            path,
            settings_path,
            platform,
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
            dirty: false,
            revision: 0,
            save_at: None,
            error,
            platform_error,
            notice: None,
            pending_notices: VecDeque::new(),
            lossy_warned: HashSet::new(),
            chrome_focus: None,
            show_words: false,
            format_toolbar: false,
            format_menu: None,
            format_selected: 0,
            format_snapshot: None,
            dark,
            pointer_inside: true,
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
        // Persist a newly created library and any one-time legacy import.
        app.changed(cx);
        app
    }
    fn editor(&self) -> Entity<EditorView> {
        self.sessions[&self.library.active_id].editor.clone()
    }
    fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_language_block = None;
        let id = self.library.active_id.clone();
        // Once per note: what the file cost to read is worth knowing before the first
        // save writes the replacements back over it.
        if self
            .library
            .note(&id)
            .is_some_and(|note| note.lossy && !self.lossy_warned.contains(&id))
        {
            self.lossy_warned.insert(id.clone());
            self.queue_notice(
                "This file was not valid text; saving replaces the unreadable parts.".to_owned(),
            );
        }
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
    /// Take over what other programs changed in the notes folder. Edits made here that
    /// had not reached the disk are never dropped: they continue as a separate note.
    fn apply_external(
        &mut self,
        changes: Vec<External>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_documents(cx);
        let mut ids = Vec::new();
        let mut kept = 0;
        for change in changes {
            let (External::Updated { note, .. } | External::Removed(note)) = &change;
            let id = note.id.clone();
            let local = self.library.note(&id).cloned();
            match change {
                External::Updated { previous, note } => {
                    if let Some(local) = local
                        && local.deleted_at.is_none()
                        && local.document != note.document
                        && previous.is_none_or(|previous| previous.document != local.document)
                    {
                        self.library.keep_copy(local.document);
                        kept += 1;
                    }
                    self.library.adopt(note);
                }
                // With unsaved edits the note stays, and is written to a new file.
                External::Removed(note) => {
                    if local.is_none_or(|local| local.document == note.document) {
                        self.library.remove(&id);
                    } else {
                        kept += 1;
                    }
                }
            }
            self.sessions.remove(&id);
            self.session_order.retain(|entry| entry != &id);
            ids.push(id);
        }
        self.ensure_session(window, cx);
        if let Some(persistence) = &self.persistence {
            persistence.acknowledge(ids);
        }
        if kept > 0 {
            self.inform("Kept your unsaved edits as a separate note", cx);
            self.changed(cx);
        }
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
        cx.notify();
        !self.dirty
    }
    /// Window controls, the action capsule and the formatting toggle are up while
    /// someone is present: the pointer is over the window, the window is the one being
    /// typed into, a key was pressed a moment ago, or a keyboard-opened panel needs
    /// them. Away from all of that they recede to [`CHROME_REST`] rather than
    /// disappearing, so what the window can do stays discoverable.
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
        if let Some(platform) = &self.platform {
            let inside = platform.pointer_inside(window);
            if inside != self.pointer_inside {
                self.pointer_inside = inside;
                cx.notify();
            }
        }
        // The keystroke timer expires on its own, so the chrome is compared here rather
        // than only where its inputs change.
        let chrome = self.chrome_visible();
        if chrome != self.chrome_shown {
            self.chrome_shown = chrome;
            if let Some(platform) = &self.platform {
                platform.set_traffic_lights_alpha(
                    window,
                    if chrome { 1. } else { CHROME_REST },
                    !cx.reduce_motion(),
                );
            }
            cx.notify();
        }
        if self.instance.requested_show() {
            self.show(window, cx);
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
                    match saved.result {
                        Ok(()) => {
                            self.dirty = false;
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                    cx.notify();
                }
                Event::Saved(_) => {}
                Event::External(changes) => self.apply_external(changes, window, cx),
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
        window.focus(&self.editor().focus_handle(cx), cx);
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
    fn quit(&mut self, cx: &mut Context<Self>) {
        if self.persistence.is_none() {
            cx.quit();
            return;
        }
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        if self.flush(cx) {
            cx.quit();
        }
    }
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            self.lossy_warned.remove(id);
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
    fn inform(&mut self, text: &str, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: text.into(),
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
                let library = store.import_legacy(library, &legacy_library(&self.settings_path));
                Ok((store, library))
            },
        );
        match opened {
            Ok((store, mut library)) => {
                // Preferences belong to this Mac, not to the folder.
                if !reopening {
                    library.preferences = self.library.preferences.clone();
                }
                self.path = Some(directory);
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
        let prompt = cx.prompt_for_new_path(&folder, Some("Markraft Notes Backup.json"));
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
        let document = doc::to_markdown(self.editor().read(cx).committed_document());
        let directory = self.path.clone().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&directory, Some(&filename));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = prompt.await {
                let result = cx
                    .background_executor()
                    .spawn(async move { std::fs::write(path, document).map_err(|e| e.to_string()) })
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
        if self.persistence.is_none() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        paths
                            .into_iter()
                            .map(|path| {
                                let text = std::fs::read_to_string(&path)
                                    .map_err(|e| crate::vault::describe(&path, &e))?;
                                // A `.json` file is a backup an earlier version
                                // wrote; everything else is Markdown.
                                if path.extension().is_some_and(|e| e == "json") {
                                    crate::legacy::read_document(&text)
                                } else {
                                    Ok(doc::from_markdown(&text))
                                }
                            })
                            .collect::<Result<Vec<_>, String>>()
                    })
                    .await;
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| match result {
                        Ok(documents) => {
                            this.query.update(cx, |e, cx| e.cancel_composition(cx));
                            this.editor().update(cx, |e, cx| e.cancel_composition(cx));
                            this.sync_documents(cx);
                            for doc in documents {
                                this.library.new_note(doc);
                            }
                            this.ensure_session(window, cx);
                            this.panel = Panel::Editor;
                            this.focus_editor(window, cx);
                            this.changed(cx);
                        }
                        Err(e) => {
                            this.error = Some(e);
                            cx.notify();
                        }
                    })
                });
            }
        })
        .detach();
    }
}
/// Heights of the toolbar and footer, which float over the top and bottom of the note.
const TOOLBAR_HEIGHT: Pixels = px(52.);
const FOOTER_HEIGHT: Pixels = px(44.);
/// What the chrome fades to while nobody is there. Far enough back to leave the note
/// alone, near enough that the controls can still be found.
const CHROME_REST: f32 = 0.35;
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
/// The single-file library of earlier versions, next to the settings file.
pub fn legacy_library(settings_path: &std::path::Path) -> PathBuf {
    settings_path.with_file_name("notes.json")
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
        Menu::new("Markraft Notes").items([
            MenuItem::action("Show Notes", Show),
            MenuItem::action("Settings…", Settings),
            MenuItem::separator(),
            MenuItem::action("Quit Markraft Notes", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Browse Notes", Browse),
            MenuItem::action("Save Now", Save),
            MenuItem::action("Import…", Import),
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
