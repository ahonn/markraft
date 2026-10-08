//! The app opened over a notes folder of its own, and the ways a test drives it.

use crate::app::{MarkraftApp, bind_app_keys};

use crate::storage::Preferences;

use crate::vault::Store;
use gpui::{Entity, TestAppContext, VisualTestContext};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct Harness<'a> {
    pub(crate) app: Entity<MarkraftApp>,
    pub(crate) cx: &'a mut VisualTestContext,
    pub(crate) notes: PathBuf,
    _root: tempfile::TempDir,
}

/// Open the app over an empty notes folder, with `configure` applied to the
/// preferences first, focused on its first note as a fresh launch is.
pub(crate) fn open(
    cx: &mut TestAppContext,
    configure: impl FnOnce(&mut Preferences),
) -> Harness<'_> {
    open_with(cx, &[], configure)
}

/// [`open`], over a folder that already holds `files`, each a name and its text.
pub(crate) fn open_with<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&str, &str)],
    configure: impl FnOnce(&mut Preferences),
) -> Harness<'a> {
    let root = tempfile::tempdir().expect("a temporary directory");
    let notes = root.path().join("notes");
    std::fs::create_dir_all(&notes).expect("the notes folder");
    for (name, text) in files {
        let path = notes.join(name);
        std::fs::create_dir_all(path.parent().expect("a seeded note's folder"))
            .expect("a seeded note's folder");
        std::fs::write(path, text).expect("a seeded note");
    }
    let settings = root.path().join("settings.json");
    cx.update(|cx| {
        #[cfg(feature = "bundled-settings")]
        gpui_base::init(cx);
        markraft_gpui::bind_keys(cx);
        markraft_vim::bind_keys(cx);
        bind_app_keys(cx);
    });
    let (store, library) = Store::open(
        notes.clone(),
        settings.clone(),
        crate::storage::Settings::default(),
    )
    .expect("the store");
    let mut preferences = Preferences::default();
    configure(&mut preferences);
    let directory = store.directory().to_owned();
    let (app, cx) = cx.add_window_view(|window, cx| {
        MarkraftApp::new(
            Some(directory),
            settings,
            Some(store),
            library,
            preferences,
            None,
            None,
            Box::new(crate::updater::Disabled),
            Box::new(crate::instance::NoRequests),
            window,
            cx,
        )
    });
    cx.update(|window, cx| app.update(cx, |app, cx| app.test_focus_editor(window, cx)));
    cx.run_until_parked();
    Harness {
        app,
        cx,
        notes: notes.canonicalize().unwrap_or(notes),
        _root: root,
    }
}

impl Harness<'_> {
    /// Keystrokes as GPUI spells them, `"cmd-s"`, `"a b enter"`. Keys that type
    /// nothing of their own — Return, Tab, Backspace, anything held with a modifier —
    /// go out as a bare key-down: `dispatch_keystroke` gives them the text an input
    /// method would type ("\n", "\t") and types it when nothing handled the key, so a
    /// Return the editor refused would come back as a line break, which a real Mac
    /// does not do. Text is what [`Harness::type_text`] is for.
    pub(crate) fn keys(&mut self, keys: &str) {
        for key in keys.split_whitespace() {
            let keystroke = gpui::Keystroke::parse(key).expect("a keystroke");
            let m = &keystroke.modifiers;
            let texts = keystroke.key.chars().count() == 1
                && !(m.platform || m.control || m.alt || m.function);
            if texts {
                self.cx.simulate_keystrokes(key);
            } else {
                self.cx.update(|window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                            keystroke,
                            is_held: false,
                            prefer_character_input: false,
                        }),
                        cx,
                    )
                });
            }
            self.cx.run_until_parked();
        }
    }

    /// Text typed into whatever has the keyboard, one character at a time as a
    /// person types it: input rules and pairing see each keystroke, not the whole.
    pub(crate) fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.cx.simulate_input(&character.to_string());
            self.cx.run_until_parked();
        }
    }

    /// ⌘S, waiting for its asynchronous result before making disk assertions.
    pub(crate) fn save(&mut self) {
        self.keys("cmd-s");
        self.wait_for_io();
    }

    /// Wait for explicit file operations without requiring unsaved edits to
    /// disappear: a refused save or recovery must leave those edits pending.
    pub(crate) fn wait_for_io(&mut self) {
        self.wait_until(|h| !h.app.update(h.cx, |app, _| app.test_io_pending()));
        assert!(
            !self.app.update(self.cx, |app, _| app.test_io_pending()),
            "a file operation did not complete within five seconds"
        );
    }

    /// The folder holding the notes folder, the settings file and the app's state.
    pub(crate) fn root(&self) -> &std::path::Path {
        self._root.path()
    }

    /// Change one preference as the Settings window does, while notes are open.
    pub(crate) fn set_preference(&mut self, pref: crate::storage::Pref) {
        let app = self.app.clone();
        self.cx.update(|window, cx| {
            app.update(cx, |app, cx| app.test_set_preference(pref, window, cx))
        });
        self.cx.run_until_parked();
    }

    /// The preferences the next launch reads, once the settings file satisfies
    /// `ready`, or what it last held when the wait ran out.
    pub(crate) fn saved_preferences(
        &mut self,
        ready: impl Fn(&Preferences) -> bool,
    ) -> Option<Preferences> {
        let path = self._root.path().join("settings.json");
        let mut read = None;
        self.wait_until(|_| {
            read = crate::storage::Settings::read(&path)
                .ok()
                .map(|settings| settings.preferences);
            read.as_ref().is_some_and(&ready)
        });
        read
    }

    /// Open the note titled `title` from Browse, as a person does.
    pub(crate) fn browse_to(&mut self, title: &str) {
        self.keys("cmd-p");
        self.type_text(title);
        self.keys("enter");
        self.wait_for_io();
    }

    /// Deliver a folder change deterministically. Headless tests do not start
    /// an OS watcher; native watcher delivery is covered by its integration test.
    pub(crate) fn refresh_files(&mut self) {
        self.app.update(self.cx, |app, _| app.test_refresh_files());
    }

    /// Open a file through the same single-instance request as a second launch.
    pub(crate) fn open_path(&mut self, path: &std::path::Path) {
        self.cx.update(|window, cx| {
            self.app.update(cx, |app, cx| {
                app.open_files(vec![path.to_owned()], window, cx)
            })
        });
        let path = path.canonicalize().expect("the file being opened");
        self.wait_until(|h| h.active_note().path.as_ref() == Some(&path));
        self.wait_for_io();
        assert_eq!(self.active_note().path.as_ref(), Some(&path));
    }

    /// The Markdown the active note's editor holds.
    pub(crate) fn markdown(&mut self) -> String {
        self.app.update(self.cx, |app, cx| {
            crate::doc::to_markdown(&app.active_document(cx))
        })
    }

    /// The file named `name` in the notes folder, once it holds what `ready` wants,
    /// or the last thing it held when the wait ran out.
    pub(crate) fn wait_for_file(&mut self, name: &str, ready: impl Fn(&str) -> bool) -> String {
        let path = self.notes.join(name);
        self.wait_for_path(&path, ready)
    }

    /// Every Markdown file in the notes folder, by name.
    pub(crate) fn files(&self) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(&self.notes)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".md"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Run the app until `ready` holds or five seconds pass: work the vault's
    /// thread does lands outside GPUI's scheduler, so it is waited for rather
    /// than given a fixed pause that a loaded machine could outlast.
    ///
    /// While it waits, the app's clock moves on too. Its poll — which starts an
    /// autosave that is due and takes in what the vault's thread reports — runs
    /// on a timer, and the test platform's timers fire only when told the time
    /// has passed.
    pub(crate) fn wait_until(&mut self, mut ready: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            if ready(self) || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
            self.cx.executor().advance_clock(Duration::from_millis(50));
        }
    }

    /// Let `time` pass on the app's clock and run what its timers start: the
    /// window's first activation, which checks the folder again, is one of them.
    pub(crate) fn pass_time(&mut self, time: Duration) {
        self.cx.executor().advance_clock(time);
        self.cx.run_until_parked();
    }

    /// Give the keyboard back to the note, as clicking into it does: a key the
    /// note refuses can hand it to the app, as Shift-Tab in a table's first cell
    /// hands it to the table's toolbar.
    pub(crate) fn focus_note(&mut self) {
        let app = self.app.clone();
        self.cx
            .update(|window, cx| app.update(cx, |app, cx| app.test_focus_editor(window, cx)));
        self.cx.run_until_parked();
    }

    pub(crate) fn selection(&mut self) -> markraft_core::Selection {
        let editor = self.app.update(self.cx, |app, _| app.test_editor());
        editor.update(self.cx, |editor, _| editor.state().selection().clone())
    }

    /// Put the active note's selection from `anchor` to `head`, positions in its
    /// document: moving by lines needs laid-out text, which the test platform
    /// does not shape.
    pub(crate) fn select(&mut self, anchor: usize, head: usize) {
        self.edit(|editor, cx| {
            editor.dispatch(
                [markraft_core::TransactionSpec::new()
                    .selection(markraft_core::Selection::text(anchor, head))],
                cx,
            )
        });
    }

    /// Run `edit` on the active note's editor, as a toolbar button does.
    pub(crate) fn edit(
        &mut self,
        edit: impl FnOnce(
            &mut markraft_gpui::EditorView,
            &mut gpui::Context<markraft_gpui::EditorView>,
        ) -> bool,
    ) -> bool {
        let editor = self.app.update(self.cx, |app, _| app.test_editor());
        let done = editor.update(self.cx, edit);
        self.cx.run_until_parked();
        done
    }

    /// Save, then hold the note to what the file now says: no failure shown, and the
    /// file read back is the document the editor holds. `step` names the edit in
    /// the message.
    pub(crate) fn assert_round_trip(&mut self, step: &str) {
        self.save();
        assert_eq!(self.error(), None, "{step}: {:?}", self.markdown());
        let path = self
            .app
            .update(self.cx, |app, _| app.active_path())
            .unwrap_or_else(|| panic!("{step}: the note has no file"));
        let expected = self.markdown();
        let on_disk = self.wait_for_path(&path, |text| {
            crate::doc::to_markdown(&crate::doc::from_markdown(text)) == expected
        });
        let read_back = crate::doc::to_markdown(&crate::doc::from_markdown(&on_disk));
        assert_eq!(
            read_back, expected,
            "{step}: the file reads back as another note\nfile: {on_disk:?}"
        );
    }

    fn wait_for_path(&mut self, path: &std::path::Path, ready: impl Fn(&str) -> bool) -> String {
        let mut text = String::new();
        self.wait_until(|_| {
            text = std::fs::read_to_string(path).unwrap_or_default();
            ready(&text)
        });
        text
    }

    /// The message the app is showing about a failure, if any.
    pub(crate) fn error(&mut self) -> Option<String> {
        self.app.update(self.cx, |app, _| app.shown_error())
    }

    /// Reconcile `changes` as the watcher would report them.
    pub(crate) fn external(&mut self, changes: Vec<crate::vault::External>) {
        let app = self.app.clone();
        self.cx.update(|window, cx| {
            app.update(cx, |app, cx| app.test_apply_external(changes, window, cx))
        });
        self.cx.run_until_parked();
        self.wait_for_io();
    }

    pub(crate) fn active_note(&mut self) -> crate::storage::Note {
        self.app.update(self.cx, |app, _| app.test_active_note())
    }

    pub(crate) fn notices(&mut self) -> Vec<String> {
        self.app.update(self.cx, |app, _| app.test_queued_notices())
    }

    /// The Markdown files in the notes folder other than `name`, with their text.
    pub(crate) fn other_files(&self, name: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(&self.notes)
            .expect("the notes folder")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .filter(|path| path.file_name().is_some_and(|file| file != name))
            .map(|path| {
                let text = std::fs::read_to_string(&path).expect("a note file");
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    text,
                )
            })
            .collect();
        out.sort();
        out
    }
}
