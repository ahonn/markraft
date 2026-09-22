mod assets;
mod feedback;
mod interaction;
mod lists;
mod presence;
mod ring;
mod sessions;
mod toolbar;
use sessions::Sessions;
mod workspace;

use feedback::Feedback;
use interaction::{InputSession, Interaction, Popover};
use lists::{Cursor, Picker};
use presence::{Presence, WindowSize};
use ring::FocusRing;
use toolbar::Toolbar;
use workspace::{Conflicts, DraftNaming, QuitState, SaveCompletion, SaveState};
mod rename;
mod ui;

use crate::doc;
use crate::{
    instance::Instance,
    persistence::{Event, Persistence, Saved},
    platform::{Platform, PlatformEvent},
    storage::Library,
    updater::Updater,
    vault::{External, Store},
};
use gpui::{prelude::*, *};
use markraft_core::{Node, Selection};
use markraft_gpui::{
    ColumnAlignment, EditRejection, EditorEvent, EditorStyle, EditorView, ExtensionHandle, Setup,
    TableInfo,
};
use std::{
    collections::VecDeque,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Notes,
    /// Work that is not in a file the way it was left; see [`is_draft`].
    Drafts,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkPopover {
    View,
    Edit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatMenu {
    Block,
    Inline,
    List,
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
    sessions: Sessions,
    interaction: Interaction,
    input: Option<InputSession>,
    /// Where Tab has walked the chrome, and the handle its controls share.
    ring: FocusRing,
    /// Browse and the command list: one selected row between them, a scroll
    /// each, and the delete question that stands on a row.
    picker: Picker,
    /// The code block's language list.
    code_language: Cursor,
    save: SaveState,
    naming: DraftNaming,
    /// Everything the window has to tell the user: the errors that stand, the
    /// notices that pass, and whose turn it is.
    feedback: Feedback,
    /// Which notes have been asked about their conflict, and whether that
    /// question is on screen.
    conflicts: Conflicts,
    /// Whether the quit question is already on screen, so a second ⌘Q cannot stack
    /// another one behind it.
    quitting: QuitState,
    /// The formatting toolbar over the note, and what the footer counts.
    toolbar: Toolbar,
    /// The format menu's list.
    format: Cursor,
    dark: bool,

    /// Whether the notes folder had to be made on open because the one the settings
    /// named was gone.
    folder_was_created: bool,
    /// What the `[[` menu offers and what the editor asks about each link it
    /// draws, and whether either is out of date.
    links: ui::wiki::Links,
    /// Whether someone is at the window, which is what the chrome follows.
    presence: Presence,
    /// The window's own size, and the one resize the app asked for.
    window_size: WindowSize,
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
        let mut feedback = Feedback::default();
        if let Some(error) = error {
            feedback.set_error(error);
        }
        feedback.set_platform_error(platform_error);
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            let bounds = window.bounds();
            // A resize the app did not ask for is the user's, and dragging the
            // window's edge is how they say they want the height left alone.
            if this.window_size.settled(bounds.size) == Some(false) {
                this.library.preferences.auto_height = false;
            }
            let next = Some([
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ]);
            if this.library.preferences.window_bounds != next {
                this.library.preferences.window_bounds = next;
                this.schedule_save(cx);
            }
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.apply_theme(window, cx);
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.presence.set_window_active(window.is_window_active());
            if this.presence.window_active() {
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
                    this.feedback
                        .error()
                        .map(String::as_str)
                        .unwrap_or("Could not save")
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
            sessions: Sessions::default(),
            interaction: Interaction::default(),
            input: None,
            ring: FocusRing::new(cx.focus_handle()),
            picker: Picker::default(),
            code_language: Cursor::default(),
            save: SaveState::default(),
            naming: DraftNaming::default(),
            feedback,
            conflicts: Conflicts::default(),
            quitting: QuitState::default(),
            folder_was_created,
            links: Default::default(),
            toolbar: Toolbar::default(),
            format: Cursor::default(),
            dark,
            presence: Presence::new(pointer_inside, window.is_window_active()),
            window_size: WindowSize::new(window.bounds().size),
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
            window.focus(app.ring.panel(), cx);
        }
        // Application preferences and drafts live outside the document folder.
        app.schedule_save(cx);
        if let Some(error) = app.updater.take_startup_error() {
            app.feedback.queue(error);
        }
        app
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
            self.conflicts.ask_again(&id);
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
                                self.feedback.set_error(error);
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
                            self.feedback.set_error(error);
                        }
                        local.conflicted = true;
                        self.library.adopt(local);
                        kept += 1;
                        continue;
                    }
                }
            }
            self.sessions.remove(&id);

            ids.push(id);
        }
        self.ensure_session(window, cx);
        // A file appearing or leaving changes what `[[` can link to, and arrives
        // independently of autosave scheduling.
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
            self.schedule_save(cx);
        }
        if vanished > 0 {
            self.feedback.queue(if vanished == 1 {
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
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save.schedule(Instant::now());
        cx.notify();
    }
    fn notes_changed(&mut self, cx: &mut Context<Self>) {
        self.links.invalidate();
        let library = &self.library;
        self.naming.retain(|id| {
            library
                .note(id)
                .is_some_and(|note| note.path.is_none() && note.deleted_at.is_none())
        });
        self.schedule_save(cx);
    }

    /// Keep autosave from naming a new note's file after a half-typed first line.
    ///
    /// Autosave names a note's file once and never again on its own, so a note filed
    /// on its first keystroke would stay `M.md` while its first line went on to read
    /// "Meeting notes for Q3"; only [`Self::open_rename`] moves it afterwards. While
    /// the caret is still in the line the name would come from, the store holds the
    /// note in recovery instead. The name has settled once the caret leaves that line
    /// or the typing stops for [`workspace::NAME_SETTLES`], and the next save files the note
    /// under what the line says then — a note that is only a first line is filed like
    /// any other.
    ///
    /// Called wherever the editor's state moved, a selection with no edit included.
    fn follow_title(&mut self, cx: &mut Context<Self>) {
        let naming = self.naming_title(cx);
        let composing = self.composing(cx);
        let library = &self.library;
        if self.naming.observe(
            &library.active_id,
            naming,
            composing,
            Instant::now(),
            |id| {
                library
                    .note(id)
                    .is_some_and(|note| !note.document_is_empty())
            },
        ) {
            self.schedule_save(cx);
        }
    }
    /// Whether the active note's editor holds an input method's uncommitted candidate.
    fn composing(&self, cx: &App) -> bool {
        self.sessions
            .get(&self.library.active_id)
            .is_some_and(|session| session.editor().read(cx).is_composing())
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
            .map(|session| session.editor().read(cx))
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
        let library = &self.library;
        if self.naming.release(|id| {
            library
                .note(id)
                .is_some_and(|note| !note.document_is_empty())
        }) {
            self.schedule_save(cx);
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
        next: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>, bool) + 'static,
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
            let result = prompt.await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |this, cx| {
                    let selected = match result {
                        Ok(Ok(Some(path))) if this.library.active_id == id => {
                            if path.exists() {
                                this.feedback.queue(
                                    "Choose a new filename; the existing file was not changed."
                                        .to_owned(),
                                );
                                false
                            } else {
                                let parent = path.parent().map(ToOwned::to_owned);
                                if let Some(note) =
                                    this.library.notes.iter_mut().find(|n| n.id == id)
                                {
                                    note.path = Some(path);
                                }
                                this.editor()
                                    .update(cx, |editor, cx| editor.set_image_base(parent, cx));
                                this.notes_changed(cx);
                                true
                            }
                        }
                        _ => false,
                    };
                    next(this, window, cx, selected);
                })
            });
        })
        .detach();
    }
    /// Give the active draft a file, then write it there.
    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_note_path(window, cx, |this, window, cx, selected| {
            if selected {
                this.flush(cx);
            }
            this.focus_editor(window, cx);
        });
    }
    /// Hand the latest snapshot to the notes folder without waiting for it.
    ///
    /// Hiding the window is the one save the user must never wait on: it happens
    /// many times an hour, the note stays open behind the window, and a folder on
    /// a network drive would otherwise freeze ⌥N for as long as the folder takes
    /// to answer — with the window still up, which is the opposite of what the
    /// key asked for. The receipt comes back through `poll` like any other, so a
    /// failure still reaches `Not saved` and is there when the window returns.
    fn flush_in_background(&mut self, cx: &mut Context<Self>) {
        self.release_title(cx);
        self.sync_documents(cx);
        let revision = self.save.barrier();
        let Some(persistence) = &self.persistence else {
            return;
        };
        // No held drafts, exactly as the waiting flush passes none: this is the
        // user asking for everything to be written, so a draft whose title is
        // still being typed is filed under what it has rather than kept back.
        if let Err(error) = persistence.save(revision, self.library.clone(), Vec::new()) {
            self.save.apply_completion(revision, false);
            self.feedback.set_error(error);
            cx.notify();
        }
    }
    fn flush(&mut self, cx: &mut Context<Self>) -> bool {
        self.release_title(cx);
        self.sync_documents(cx);
        let revision = self.save.barrier();
        let result = self
            .persistence
            .as_ref()
            .ok_or_else(|| "Open or recover the library before saving.".to_owned())
            .and_then(|p| p.flush(revision, self.library.clone()));
        match result {
            Ok(saved) => self.apply_saved(saved, cx),
            Err(error) => {
                self.save.apply_completion(revision, false);
                self.feedback.set_error(error);
                cx.notify();
            }
        }
        !self.save.is_dirty()
    }

    fn apply_saved(&mut self, saved: Saved, cx: &mut Context<Self>) {
        let completion = self
            .save
            .apply_completion(saved.revision, saved.result.is_ok());
        if completion == SaveCompletion::Ignored {
            return;
        }
        self.update_paths(saved.paths, cx);
        self.update_conflicts(saved.conflicts, cx);
        if completion == SaveCompletion::Current {
            match saved.result {
                Ok(()) => self.feedback.clear_error(),
                Err(error) => self.feedback.set_error(error),
            }
        }
        cx.notify();
    }
    /// The title recedes while the window is idle. Corner action buttons and native
    /// traffic lights follow `pointer_inside` alone and fade out completely;
    /// typing, focus and open panels must not keep those buttons visible.
    fn chrome_visible(&self) -> bool {
        let busy =
            self.interaction.panel() != Panel::Editor || self.interaction.format_menu().is_some();
        self.presence.at_window(busy)
    }
    /// Critically damped: chrome fades without overshoot, and a reversal while the
    /// pointer crosses the window edge continues from the current opacity. Reduced
    /// motion keeps both end states and drops the travel between them.
    pub(super) fn chrome_spring(visible: bool, reduce_motion: bool) -> SpringAnimation<bool> {
        SpringAnimation::new(SpringConfig::new(600., 49., 1.))
            .to(visible)
            .playback(ui::playback(reduce_motion))
    }
    /// Someone is at the window. Only a keystroke that brings the chrome *back*
    /// is worth a redraw: while it is already up, every key would ask for one.
    pub(super) fn note_key_press(&mut self, cx: &mut Context<Self>) {
        let was_visible = self.chrome_visible();
        self.presence.note_key_press();
        if !was_visible {
            cx.notify();
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reconcile_interaction(window, cx);
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
                    self.feedback.flash_file_status();
                    cx.notify();
                }
                EditRejection::Protected(message) | EditRejection::Invalid(message) => {
                    self.feedback.queue(message);
                }
                // The shading said it where the edit landed.
                EditRejection::Marked(_) => {}
            }
        }
        if self.interaction.panel() == Panel::Editor {
            self.editor()
                .update(cx, |editor, cx| editor.refresh_images(cx));
        }
        // The `[[` menu reads a shared list rather than a snapshot, so it follows notes
        // being written, renamed and deleted. Rebuilding costs a string a note, so it
        // is invalidated only by changes to its note catalogue dependencies.
        if self.links.take_stale() {
            self.refresh_link_targets();
        }
        // The platform draws the close button above everything the view renders, so a
        // popup that reaches the top-left corner would be covered by it. It stands down
        // while one is open, the same way it does when the pointer leaves the window.
        let covered =
            self.interaction.panel() == Panel::Editor && self.editor().read(cx).overlay_open();
        if let Some(platform) = &self.platform {
            let inside = platform.pointer_inside(window);
            if self.presence.set_pointer_inside(inside) {
                cx.notify();
            }
            if self.presence.close_button_changed(inside && !covered) {
                platform.set_traffic_lights_alpha(
                    window,
                    if inside && !covered { 1. } else { 0. },
                    !cx.reduce_motion(),
                );
            }
        }
        // The keystroke timer expires on its own, so the chrome is compared here rather
        // than only where its inputs change.
        if self.presence.chrome_changed(self.chrome_visible()) {
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
                PlatformEvent::Quit => self.quit(window, cx),
            }
        }
        for notice in self
            .persistence
            .as_ref()
            .map(Persistence::notices)
            .unwrap_or_default()
        {
            self.feedback.queue(notice);
        }
        let events = self
            .persistence
            .as_ref()
            .map(Persistence::poll)
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Saved(saved) => self.apply_saved(saved, cx),
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
        if self.naming.due(Instant::now()) {
            self.release_title(cx);
        }
        if let Some(revision) = self.save.take_due(Instant::now())
            && let Some(p) = &self.persistence
            && let Err(e) = p.save(
                revision,
                self.library.clone(),
                self.naming
                    .held_id()
                    .map(str::to_owned)
                    .into_iter()
                    .collect(),
            )
        {
            self.feedback.set_error(e);
        }
        if self.feedback.tick() {
            cx.notify();
        }
        if self.interaction.panel() == Panel::Editor
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
            let chrome = if self.feedback.error().is_some() {
                px(64.)
            } else {
                px(0.)
            };
            let desired = px(f32::from((height + chrome).max(MINIMUM_HEIGHT).min(maximum)).round());
            let size = size(window.bounds().size.width, desired);
            if (size.height - window.bounds().size.height).abs() > px(2.)
                && !self.window_size.waiting()
            {
                self.window_size.expect(size);
                window.resize(size);
            }
        }
    }
    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ring.release();
        if !self.focus_html_source(window, cx) {
            window.focus(&self.editor().focus_handle(cx), cx);
        }
    }
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.platform {
            if let Err(e) = p.show(window) {
                self.feedback.set_error(e);
            }
        } else {
            window.activate_window();
        }
        self.ring.release();
        if self.persistence.is_none() {
            window.focus(self.ring.panel(), cx);
        } else if self.focus_html_source(window, cx) {
            // Keep the source draft as the keyboard owner after hiding the app.
        } else if self.interaction.panel() != Panel::Editor {
            window.focus(&self.query().focus_handle(cx), cx);
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }
    pub fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(cx);
        self.cancel_input(cx);
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.flush_in_background(cx);
        if let Some(p) = &mut self.platform {
            if let Err(e) = p.hide(window) {
                self.feedback.set_error(e);
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
    /// ⌘Q. A note with no file of its own is never written to one by autosave, so
    /// this is the last moment to offer it a file. Hiding the window asks nothing:
    /// the note is still open behind it, and that happens many times an hour.
    fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.quitting.begin() {
            return;
        }
        if self.unfiled_draft() && is_draft(self.library.active_note()) {
            let answer = window.prompt(
                PromptLevel::Warning,
                "This note has not been saved to a file",
                // The words are not at stake — recovery has them and the next launch
                // opens them again — so the question is only about the file.
                Some("It is kept inside Markraft and will be here when you come back."),
                &["Save As…", "Cancel", "Quit"],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                let choice = answer.await;
                let _ = cx.update(|window, cx| {
                    this.update(cx, |this, cx| {
                        match choice {
                            // Giving it a file is what the question was about, so
                            // leaving is what follows once it has one.
                            Ok(0) => {
                                this.prompt_for_note_path(window, cx, |this, _, cx, selected| {
                                    if selected {
                                        this.quit_now(cx);
                                    } else {
                                        this.quitting.cancel();
                                        cx.notify();
                                    }
                                })
                            }
                            Ok(2) => this.quit_now(cx),
                            _ => {
                                this.quitting.cancel();
                                cx.notify();
                            }
                        }
                    })
                });
            })
            .detach();
            return;
        }
        self.quit_now(cx);
    }
    /// Write everything that has a file and go; stay and say why if that fails.
    fn quit_now(&mut self, cx: &mut Context<Self>) {
        if self.prepare_to_quit(cx) {
            cx.quit();
            return;
        }
        self.quitting.cancel();
        self.show_popover(Popover::FileStatus, cx);
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
        if self.input_composing(cx) {
            self.cancel_input(cx);
            return;
        }
        if self.editor().read(cx).is_composing() {
            self.editor().update(cx, |e, cx| e.cancel_composition(cx));
            return;
        }
        // A notice offering an action takes the first Escape; the cascade below resumes
        // on the next one.
        if self.feedback.dismiss_action() {
            self.ring.release();
            // Hand the keyboard back to the panel the notice was drawn over.
            if self.interaction.panel() != Panel::Editor {
                window.focus(&self.query().focus_handle(cx), cx);
            }
            cx.notify();
            return;
        }
        // A question standing on a row takes the first Escape, so nothing behind it
        // closes while the answer is still pending.
        if self.picker.forget_question() {
            cx.notify();
            return;
        }
        self.ring.release();
        let had_popover = self.close_popover(cx);
        if had_popover {
            self.focus_editor(window, cx);
            cx.notify();
        } else if self.interaction.panel() != Panel::Editor {
            self.set_panel(Panel::Editor, cx);
            self.focus_editor(window, cx);
            cx.notify();
        } else if self.toolbar.dismiss() {
            cx.notify();
        } else {
            self.hide(window, cx);
        }
    }
    fn new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(cx);
        self.cancel_input(cx);
        if self.persistence.is_none() {
            self.choose_folder(window, cx);
            return;
        }
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        self.library.new_note(doc::empty());
        self.ensure_session(window, cx);
        self.set_panel(Panel::Editor, cx);
        self.focus_editor(window, cx);
        self.notes_changed(cx);
    }
    fn select_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(cx);
        self.cancel_input(cx);
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        if self.library.select(id) {
            self.ensure_session(window, cx);
            self.set_panel(Panel::Editor, cx);
            self.focus_editor(window, cx);
            self.notes_changed(cx);
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
                    self.feedback
                        .queue(format!("No {what} named “{page}” in this folder."))
                }
            },
        }
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(cx);
        if self.persistence.is_none() {
            return;
        }
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.cancel_input(cx);
        let next = if self.interaction.panel() == panel {
            Panel::Editor
        } else {
            panel
        };
        self.set_panel(next, cx);
        self.picker.reopen_browse();
        self.ring.release();
        let (query, placeholder, label) = match self.interaction.panel() {
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
        self.close_popover(cx);
        self.set_query(query, placeholder, label, cx);
        if self.interaction.panel() != Panel::Editor {
            window.focus(&self.query().focus_handle(cx), cx);
        } else {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }
    /// Which of the library's notes the open panel is looking at. Drafts and the
    /// trash are the same list the Browse panel draws, filtered two ways.
    pub(crate) fn scope(&self) -> Scope {
        match self.interaction.panel() {
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
            self.notes_changed(cx);
        }
        // Pinning reorders results; keep the same note selected.
        let row = self
            .matching_notes(self.search_text(cx).trim(), self.scope())
            .iter()
            .position(|note| note.id == id)
            .unwrap_or(0);
        self.picker.select_in_browse(row);
    }
    fn trash_note(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        if self.library.delete(id) {
            self.sessions.remove(id);
            self.ensure_session(window, cx);
            let query = self.search_text(cx);
            let root = self.path.clone();
            self.picker.clamp_to(
                self.library
                    .search(query.trim(), false, root.as_deref())
                    .len(),
            );
            self.ring.release();
            window.focus(&self.query().focus_handle(cx), cx);
            self.notes_changed(cx);
            self.inform_undo("Moved to Recently Deleted", id.to_owned(), cx);
        }
    }
    fn delete_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.library.active_id.clone();
        self.sync_documents(cx);
        if self.library.delete(&id) {
            self.sessions.remove(&id);
            self.ensure_session(window, cx);
            self.set_panel(Panel::Editor, cx);
            self.focus_editor(window, cx);
            self.notes_changed(cx);
            self.inform_undo("Moved to Recently Deleted", id, cx);
        }
    }
    /// Take back the deletion the notice still offers, and open that note again.
    fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.feedback.take_undo() else {
            return;
        };
        if self.library.restore(&id) {
            self.ensure_session(window, cx);
            self.set_panel(Panel::Editor, cx);
            self.focus_editor(window, cx);
            self.notes_changed(cx);
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
        }
        self.picker.forget_question();
        self.ring.release();
        self.ensure_session(window, cx);
        self.picker.clamp_to(
            self.library
                .search(self.search_text(cx).trim(), true, self.path.as_deref())
                .len(),
        );
        self.notes_changed(cx);
        match result {
            Ok(()) if count == 1 => self.inform("Deleted permanently", cx),
            Ok(()) => self.inform("Emptied Recently Deleted", cx),
            Err(error) => {
                self.feedback.set_error(error);
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
            self.ring.release();
            self.picker.forget_question();
            self.ensure_session(window, cx);
            self.notes_changed(cx);
            self.picker.clamp_to(
                self.library
                    .search(&self.search_text(cx), true, self.path.as_deref())
                    .len(),
            );
            self.inform("Restored note", cx);
        }
    }
    fn inform(&mut self, text: impl AsRef<str>, cx: &mut Context<Self>) {
        self.feedback.inform(text);
        cx.notify();
    }
    /// A notice whose deletion can still be taken back.
    fn inform_undo(&mut self, text: &str, note: String, cx: &mut Context<Self>) {
        self.feedback.inform_undo(text, note);
        cx.notify();
    }
    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = self.library.preferences.dark_mode.unwrap_or(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        for session in self.sessions.values() {
            session
                .editor()
                .update(cx, |e, cx| e.set_style(notes_style(self.dark), cx));
        }
        self.style_input(cx);
        cx.notify();
    }
    /// ⌘L: a link under the caret shows its actions, anything else asks for an address.
    fn open_link_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persistence.is_none() || self.interaction.panel() != Panel::Editor {
            return;
        }
        self.close_popover(cx);
        self.ring.release();
        if self.editor().read(cx).active_link().is_some() {
            self.show_popover(Popover::Link(LinkPopover::View), cx);
            cx.notify();
        } else {
            self.edit_link(window, cx);
        }
    }
    fn edit_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popover(cx);
        let url = self
            .editor()
            .read(cx)
            .active_link()
            .unwrap_or_default()
            .to_owned();
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.ring.release();
        self.show_popover(Popover::Link(LinkPopover::Edit), cx);
        self.set_query(url, "Enter a link…", "Link URL", cx);
        window.focus(&self.query().focus_handle(cx), cx);
        cx.notify();
    }
    fn unlink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor()
            .update(cx, |editor, cx| editor.set_link(None, cx));
        self.close_popover(cx);
        self.focus_editor(window, cx);
        cx.notify();
    }
    fn apply_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query().read(cx);
        let url =
            markraft_core::projection::Projection::of(query.committed_document(), query.schema())
                .plain_text()
                .trim()
                .to_string();
        self.editor().update(cx, |editor, cx| {
            editor.set_link((!url.is_empty()).then_some(url.as_str()), cx)
        });
        self.close_popover(cx);
        self.focus_editor(window, cx);
        cx.notify();
    }
    fn apply_shortcut(&mut self, cx: &mut Context<Self>) {
        let query = self.query().read(cx);
        let text =
            markraft_core::projection::Projection::of(query.committed_document(), query.schema())
                .plain_text()
                .trim()
                .to_string();
        if let Some(platform) = &mut self.platform {
            match platform.set_shortcut(&text) {
                Ok(()) => {
                    self.library.preferences.hotkey = text;
                    self.feedback.set_platform_error(None);
                    self.schedule_save(cx);
                    self.inform("Updated shortcut", cx);
                }
                Err(e) => {
                    self.feedback.set_platform_error(Some(e));
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
                self.persistence = Some(Persistence::new(store));
                self.replace_library(library, window, cx);
                self.feedback.clear_error();
                self.set_panel(Panel::Editor, cx);
                self.focus_editor(window, cx);
                self.apply_theme(window, cx);
                let refused = self
                    .platform
                    .as_mut()
                    .and_then(|p| p.set_shortcut(&self.library.preferences.hotkey).err());
                self.feedback.set_platform_error(refused);
                self.notes_changed(cx);
            }
            Err(e) => {
                self.feedback.set_error(e);
                cx.notify();
            }
        }
    }
    fn save_copy(&mut self, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        let bytes = match crate::vault::backup(&self.library) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.feedback.set_error(e);
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
                        this.feedback.set_error(e);
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
                        this.save.barrier();
                        let result = this
                            .persistence
                            .as_ref()
                            .ok_or_else(|| "Choose a notes folder first.".to_string())
                            .and_then(|p| p.reload());
                        match result {
                            Ok(library) => {
                                this.replace_library(library, window, cx);
                                this.set_panel(Panel::Editor, cx);
                                this.feedback.clear_error();
                                this.focus_editor(window, cx);
                                this.apply_theme(window, cx);
                                let refused = this.platform.as_mut().and_then(|p| {
                                    p.set_shortcut(&this.library.preferences.hotkey).err()
                                });
                                this.feedback.set_platform_error(refused);
                            }
                            Err(e) => this.feedback.set_error(e),
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
                self.feedback
                    .queue(format!("Could not prepare the export: {error}"));
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
                        this.feedback.set_error(e);
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
            self.feedback.queue(
                "Drop one folder on its own to open it; folders were left alone.".to_owned(),
            );
        }
        if dropped.skipped > 0 {
            self.feedback.queue(format!(
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
            if self.interaction.panel() == Panel::Editor && self.persistence.is_some() {
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
                self.feedback.queue(
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
                    self.persistence = Some(Persistence::new(store));
                    self.path = None;
                    self.replace_library(library, window, cx);
                })
            };
            // Failing to open a file says nothing about saving, so it is a sentence
            // rather than the save banner — and each file gets its own.
            if let Err(error) = result {
                self.feedback
                    .queue(format!("Could not open “{name}”: {error}"));
            }
        }
        self.ensure_session(window, cx);
        self.set_panel(Panel::Editor, cx);
        self.show(window, cx);
        self.focus_editor(window, cx);
        self.notes_changed(cx);
    }

    fn update_paths(&mut self, paths: Vec<(String, PathBuf)>, cx: &mut Context<Self>) {
        for (id, path) in paths {
            if let Some(note) = self.library.notes.iter_mut().find(|note| note.id == id)
                && note.path.as_ref() != Some(&path)
            {
                note.path = Some(path.clone());
                self.links.invalidate();
                if let Some(session) = self.sessions.get(&id) {
                    session.editor().update(cx, |editor, cx| {
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
        self.conflicts.ask_again(&id);
        self.prompt_conflict(window, cx);
    }

    fn prompt_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.library.active_note();
        if !note.conflicted || !self.conflicts.would_ask(&note.id) {
            return;
        }
        let id = note.id.clone();
        let subject = conflict_subject(note);
        if !self.conflicts.ask(&id) {
            return;
        }
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
                    this.conflicts.answered();
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
                self.feedback.clear_error();
                self.notes_changed(cx);
                self.focus_editor(window, cx);
            }
            // The note stays conflicted and its indicator keeps saying so, so this is
            // one sentence about a failed load rather than a standing save banner.
            Err(error) => {
                self.feedback
                    .queue(format!("Could not load the version on disk: {error}"));
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
                        this.schedule_save(cx);
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
                        this.schedule_save(cx);
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
            self.feedback.queue(
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
        let Some(path) = self.library.active_note().path.clone() else {
            // The save panel has no room to say why it opened, so the reason goes
            // before it rather than into it.
            self.feedback
                .queue("Save this note first — images are stored next to its file.".to_owned());
            self.prompt_for_note_path(window, cx, move |this, window, cx, selected| {
                if selected {
                    this.insert_assets(assets, window, cx);
                }
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
                        this.feedback.queue(error);
                        return;
                    }
                };
                // Anything that could not be inserted is already on disk, so the
                // sentence has to end with where it is or the file is lost to them.
                let kept =
                    |what: &str| format!("{what} The images are in {}.", inserted.urls.join(", "));
                let note = this.library.active_note();
                if this.library.active_id != id || note.read_only.is_some() || note.conflicted {
                    this.feedback.queue(format!(
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
                        this.feedback.queue(kept(&error.to_string()));
                        return;
                    }
                };
                // Typing during the copy only moves the caret; the images go where
                // it is now.
                let applied = this.editor().update(cx, |editor, cx| {
                    editor.run_command(&markraft_core::commands::replace_selection(slice), cx)
                });
                if !applied {
                    this.feedback.queue(kept("This note would not take the images."));
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
/// nothing, because the name settles on its own after [`workspace::NAME_SETTLES`].
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
/// Characters a live Browse row's second line holds at 12px. The card is a fixed
/// width and every live row keeps 60px clear for its buttons, which leaves about
/// 226px; measured on a real run the line averages 6px a character.
const LIVE_META_CHARS: usize = 38;
/// The same line in Recently Deleted, whose buttons sit beside it only while the row
/// is selected and then take 202px of it.
const DELETED_META_CHARS: usize = 46;
const DELETED_SELECTED_META_CHARS: usize = 12;
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
