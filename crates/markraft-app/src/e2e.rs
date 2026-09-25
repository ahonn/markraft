//! The app's own flows, run headless against GPUI's test platform: a real
//! [`MarkraftApp`] over a real notes folder in a temporary directory, driven by the
//! keystrokes and actions a person would use, and judged by what reaches the disk.
//!
//! What this layer does not have: the menu bar, the global shortcuts and the native
//! window (the app runs without its platform half), text shaped by a real font (the
//! test platform lays text out with a placeholder system, so nothing here clicks by
//! position), and an input method. Those stay with the checks on a real Mac.
//!
//! The vault writes from a thread of its own, outside GPUI's scheduler, so what is
//! on disk is waited for with [`Harness::wait_for_file`] rather than assumed.

use crate::app::{MarkraftApp, bind_app_keys};
use crate::instance::{Instance, Launch, Request};
use crate::storage::Preferences;
use crate::updater::Updater;
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
        std::fs::write(notes.join(name), text).expect("a seeded note");
    }
    let settings = root.path().join("settings.json");
    cx.update(|cx| {
        gpui_base::init(cx);
        markraft_gpui::bind_keys(cx);
        markraft_vim::bind_keys(cx);
        bind_app_keys(cx);
    });
    let (store, mut library) = Store::open(notes.clone(), settings.clone()).expect("the store");
    configure(&mut library.preferences);
    let Launch::Primary(instance) =
        Instance::acquire(&settings, Request::Show).expect("the instance lock")
    else {
        panic!("a temporary settings file has no other instance");
    };
    let directory = store.directory().to_owned();
    let (app, cx) = cx.add_window_view(|window, cx| {
        MarkraftApp::new(
            Some(directory),
            settings,
            Some(store),
            library,
            None,
            None,
            Updater::disabled(),
            instance,
            window,
            cx,
        )
    });
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

    /// ⌘S, which writes every note that has changed before it returns.
    pub(crate) fn save(&mut self) {
        self.keys("cmd-s");
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if ready(&text) || Instant::now() > deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
    pub(crate) fn wait_until(&mut self, ready: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            if ready(self) || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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

    /// The active note's selection.
    pub(crate) fn selection(&mut self) -> markraft_core::Selection {
        let editor = self.app.update(self.cx, |app, _| app.test_editor());
        editor.update(self.cx, |editor, _| editor.state().selection().clone())
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.cx.run_until_parked();
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if ready(&text) || Instant::now() > deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
    }

    /// The active note as the library holds it.
    pub(crate) fn active_note(&mut self) -> crate::storage::Note {
        self.app.update(self.cx, |app, _| app.test_active_note())
    }

    /// The sentences waiting to be shown.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `note` as the file reads when it says `markdown`.
    fn on_disk(note: &crate::storage::Note, markdown: &str) -> crate::storage::Note {
        crate::storage::Note {
            document: crate::doc::from_markdown(markdown),
            ..note.clone()
        }
    }

    // Another app rewrote the file while the note had edits of its own: the file
    // wins, and the edits are kept beside it as a conflicted copy, said once.
    #[gpui::test]
    fn edits_meeting_a_rewritten_file_are_kept_as_a_conflicted_copy(cx: &mut TestAppContext) {
        use crate::vault::External;
        let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
        let before = h.active_note();
        h.keys("cmd-down");
        h.type_text(" mine");
        h.external(vec![External::Updated {
            previous: Some(before.clone()),
            note: on_disk(&before, "theirs"),
        }]);
        assert_eq!(h.markdown(), "theirs");
        let copies = h.other_files("n.md");
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert_eq!(copies[0].1, "hello mine\n");
        assert_eq!(
            h.notices(),
            ["A note changed on disk; your edits were kept as a conflicted copy."]
        );
    }

    // The file was rewritten while the note held what the file used to say: the
    // new text is taken, with nothing to keep and nothing to say.
    #[gpui::test]
    fn an_untouched_note_takes_a_rewritten_file(cx: &mut TestAppContext) {
        use crate::vault::External;
        let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
        let before = h.active_note();
        h.external(vec![External::Updated {
            previous: Some(before.clone()),
            note: on_disk(&before, "theirs"),
        }]);
        assert_eq!(h.markdown(), "theirs");
        assert_eq!(h.other_files("n.md"), []);
        assert_eq!(h.notices(), Vec::<String>::new());
    }

    // Only the file's permissions changed: the edits on screen stay, not a copy,
    // and the note takes the new read-only state.
    #[gpui::test]
    fn a_permission_change_keeps_the_edits_on_screen(cx: &mut TestAppContext) {
        use crate::vault::External;
        let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
        let before = h.active_note();
        h.keys("cmd-down");
        h.type_text(" mine");
        let locked = crate::storage::Note {
            read_only: Some("locked".into()),
            ..before.clone()
        };
        h.external(vec![External::Updated {
            previous: Some(before),
            note: locked,
        }]);
        assert_eq!(h.markdown(), "hello mine");
        assert_eq!(h.active_note().read_only.as_deref(), Some("locked"));
        assert_eq!(h.other_files("n.md"), []);
        assert_eq!(h.notices(), Vec::<String>::new());
    }

    // The file was deleted: a note with nothing unsaved goes with it; one with
    // edits keeps them as a copy where the file was. Either way it is said.
    #[gpui::test]
    fn a_deleted_file_takes_its_note_and_keeps_its_edits(cx: &mut TestAppContext) {
        use crate::vault::External;
        let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
        let note = h.active_note();
        h.external(vec![External::Removed(note.clone())]);
        let gone = h.app.update(h.cx, |app, _| app.test_note(&note.id));
        assert!(gone.is_none(), "the note left with its file");
        assert_eq!(h.other_files("n.md"), []);
        assert_eq!(
            h.notices(),
            ["A note's file was deleted outside Markraft, so the note is gone too."]
        );

        let mut h = open_with(cx, &[("n.md", "hello\n")], |_| {});
        let note = h.active_note();
        h.keys("cmd-down");
        h.type_text(" mine");
        h.external(vec![External::Removed(note.clone())]);
        let gone = h.app.update(h.cx, |app, _| app.test_note(&note.id));
        assert!(gone.is_none(), "the note left with its file");
        let copies = h.other_files("n.md");
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert_eq!(copies[0].1, "hello mine\n");
    }

    #[gpui::test]
    fn a_note_typed_and_saved_reaches_its_file(cx: &mut TestAppContext) {
        let mut h = open(cx, |_| {});
        h.type_text("hello");
        h.save();
        assert_eq!(h.markdown(), "hello");
        let text = h.wait_for_file("hello.md", |text| text == "hello\n");
        assert_eq!(text, "hello\n", "files: {:?}", h.files());
    }

    // H1: a code block fenced in a new task item saves, and shows what is typed in
    // it. On a real Mac the autosave lands between keystrokes; each step here is
    // saved the same way. The edit was refused as one the source could not be
    // written back from, and what was typed after it went nowhere.
    #[gpui::test]
    fn a_code_block_fenced_in_a_task_item_saves(cx: &mut TestAppContext) {
        let mut h = open(cx, |_| {});
        let steps = [
            "t", "t", "enter", "-", " ", "[", " ", "]", " ", "t", "1", "enter", "`", "`", "`",
            "enter", "c", "o", "d", "e",
        ];
        for step in steps {
            if step == "enter" {
                h.keys("enter");
            } else {
                h.type_text(step);
            }
            h.save();
            assert_eq!(h.error(), None, "after {step:?}: {:?}", h.markdown());
        }
        assert_eq!(h.markdown(), "tt\n\n- [ ] t1\n- [ ] \n  ```\n  code\n  ```");
    }

    // M1: Return at the end of an item's first paragraph that more blocks of the
    // item follow splits the item, as Typora does: the new item takes those
    // blocks. Until something is typed there the file holds its marker alone on
    // its line, which reads back as the same item.
    #[gpui::test]
    fn return_before_more_blocks_of_an_item_splits_it(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("item.md", "- first\n\n  para2\n- next\n")], |_| {});
        h.keys("cmd-up cmd-right enter");
        h.save();
        assert_eq!(h.error(), None);
        let split = "- first\n\n- \n  para2\n- next\n";
        assert_eq!(h.wait_for_file("item.md", |text| text == split), split);
        h.type_text("new");
        assert_eq!(h.markdown(), "- first\n\n- new\n\n  para2\n\n- next");
        h.save();
        let text = h.wait_for_file("item.md", |text| text.contains("new"));
        assert_eq!(text, "- first\n\n- new\n\n  para2\n- next\n");
    }

    // M3: ⌥⌘C makes a code block with the fence the preferences ask for: on an
    // empty line in place of it, and after a line with text — as Typora does —
    // with the caret in it.
    #[gpui::test]
    fn the_code_block_shortcut_takes_the_preferred_fence(cx: &mut TestAppContext) {
        let mut h = open(cx, |p| p.code_fence = crate::storage::CodeFence::Tildes);
        h.keys("alt-cmd-c");
        h.type_text("x");
        assert_eq!(h.markdown(), "~~~\nx\n~~~");
        let mut h = open(cx, |p| p.code_fence = crate::storage::CodeFence::Tildes);
        h.type_text("x");
        h.keys("alt-cmd-c");
        h.type_text("y");
        assert_eq!(h.markdown(), "x\n\n~~~\ny\n~~~");
    }

    // L1: with Markdown shortcuts off, a fence typed at the start of a line stays text.
    #[gpui::test]
    fn a_fence_stays_text_with_markdown_shortcuts_off(cx: &mut TestAppContext) {
        let mut h = open(cx, |p| p.markdown_shortcuts = false);
        h.type_text("```");
        h.keys("enter");
        h.type_text("x");
        assert_eq!(h.markdown(), "\\```\n\nx");
        // The fence is punctuation, not a name: the note is filed after `x`.
        h.save();
        let text = h.wait_for_file("x.md", |text| text == "\\```\n\nx\n");
        assert_eq!(text, "\\```\n\nx\n", "files: {:?}", h.files());
    }

    // The list shortcuts turn the whole list into the other kind, as Typora does.
    #[gpui::test]
    fn a_list_shortcut_converts_the_list_it_is_in(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("list.md", "1. a\n2. b\n")], |_| {});
        h.keys("cmd-down");
        h.keys("cmd-*");
        assert_eq!(h.markdown(), "- a\n- b");
    }

    // A new line in a code block starts where the one above it did.
    #[gpui::test]
    fn return_in_a_code_block_keeps_the_indent(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("code.md", "```\nfn a() {\n    x\n```\n")], |_| {});
        h.keys("cmd-up down down end enter");
        h.type_text("y");
        assert_eq!(h.markdown(), "```\nfn a() {\n    x\n    y\n```");
    }

    // The `/` menu searches past a space: `/code bl` still finds Code Block.
    #[gpui::test]
    fn the_slash_menu_searches_past_a_space(cx: &mut TestAppContext) {
        let mut h = open(cx, |_| {});
        h.type_text("/code bl");
        h.keys("enter");
        // The query is taken away with the menu: nothing of `/code bl` is left.
        assert_eq!(h.markdown(), "```\n```");
    }

    // A pasted image is written beside the note and referenced with nothing after it;
    // named for a note named for today, its name holds the date once.
    #[gpui::test]
    fn a_pasted_image_lands_beside_the_note_under_its_name(cx: &mut TestAppContext) {
        use crate::storage::{ImageNaming, NoteNaming};
        let mut h = open(cx, |_| {});
        h.app.update(h.cx, |app, _| {
            app.set_workspace_naming(NoteNaming::DateTime, ImageNaming::NoteAndDate)
        });
        h.type_text("x");
        h.save();
        let png = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icon/markraft-menubar.png"
        ))
        .expect("an image to paste");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            png,
        )));
        h.keys("cmd-v");
        h.wait_until(|h| h.markdown().ends_with(".png)"));
        let markdown = h.markdown();
        // At the caret, inline: `x![image](…)`, with nothing typed after it.
        assert!(markdown.starts_with("x![image](assets/"), "{markdown:?}");
        assert!(
            markdown.ends_with(".png)"),
            "text after the image: {markdown:?}"
        );
        let image = std::fs::read_dir(h.notes.join("assets"))
            .expect("the assets folder")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .next()
            .expect("the pasted image");
        let date = &image[..10];
        assert_eq!(image.matches(date).count(), 1, "{image}");
    }

    // G1: a new note has no file for the guard to hold edits to until its first
    // save, and the first save writes the document whole. Whatever the editor takes
    // must still come back from that file.
    #[gpui::test]
    fn a_new_note_saves_what_it_holds_on_its_first_save(cx: &mut TestAppContext) {
        let mut h = open(cx, |_| {});
        h.type_text("a");
        h.keys("shift-enter");
        h.assert_round_trip("a line break at the end of a new note");
        h.type_text("b");
        h.assert_round_trip("text after it");
    }

    // G3: a line break at the end of a line in a note already on disk.
    #[gpui::test]
    fn a_line_break_at_the_end_of_a_saved_line(cx: &mut TestAppContext) {
        for spaces in [false, true] {
            let mut h = open_with(cx, &[("a.md", "a\n")], |p| {
                if spaces {
                    p.hard_break = crate::storage::HardBreakStyle::Spaces;
                }
            });
            h.keys("cmd-down cmd-right shift-enter");
            h.assert_round_trip("shift-return at the end");
            h.type_text("b");
            h.assert_round_trip("typing after the break");
        }
    }

    // G3: bold asked for on an empty line of a saved note, then typed into.
    #[gpui::test]
    fn bold_on_an_empty_line_of_a_saved_note(cx: &mut TestAppContext) {
        for underscore in [false, true] {
            let mut h = open_with(cx, &[("x.md", "x\n")], |p| {
                if underscore {
                    p.emphasis_marker = crate::storage::EmphasisMarker::Underscore;
                }
            });
            h.keys("cmd-down cmd-right enter cmd-b");
            h.assert_round_trip("an empty bold pair");
            h.type_text("y");
            h.assert_round_trip("typed into it");
            h.keys("cmd-b");
            h.type_text(" z");
            h.assert_round_trip("typed after leaving it");
        }
    }

    // G4: the table toolbar on a table written by hand, not as the writer would.
    // A row added — spaced as the header is — or deleted leaves the other rows
    // as they were spelled; a
    // column changed respells the table, since every row changes with it.
    #[gpui::test]
    fn table_edits_on_a_hand_written_table(cx: &mut TestAppContext) {
        use markraft_gpui::ColumnAlignment;
        type Edit = fn(
            &mut markraft_gpui::EditorView,
            &mut gpui::Context<markraft_gpui::EditorView>,
        ) -> bool;
        let edits: [(&str, Edit, &str); 7] = [
            (
                "row after",
                |e, cx| e.table_add_row_after(cx),
                "|a|b|\n|-|-|\n|1|2|\n| | |\n",
            ),
            (
                "row before",
                |e, cx| e.table_add_row_before(cx),
                "|a|b|\n|-|-|\n| | |\n|1|2|\n",
            ),
            (
                "column after",
                |e, cx| e.table_add_column_after(cx),
                "| a   | b   |     |\n| --- | --- | --- |\n| 1   | 2   |     |\n",
            ),
            (
                "column before",
                |e, cx| e.table_add_column_before(cx),
                "| a   |     | b   |\n| --- | --- | --- |\n| 1   |     | 2   |\n",
            ),
            (
                "align centre",
                |e, cx| e.table_set_alignment(ColumnAlignment::Center, cx),
                "| a   | b   |\n| --- | :-: |\n| 1   | 2   |\n",
            ),
            (
                "delete row",
                |e, cx| e.table_delete_row(cx),
                "|a|b|\n|-|-|\n",
            ),
            (
                "delete column",
                |e, cx| e.table_delete_column(cx),
                "| a   |\n| --- |\n| 1   |\n",
            ),
        ];
        let source = "|a|b|\n|-|-|\n|1|2|\n";
        for (name, edit, expected) in edits {
            let mut h = open_with(cx, &[("t.md", source)], |_| {});
            h.keys("cmd-down");
            let done = h.edit(edit);
            assert!(done, "{name} was refused on a hand-written table");
            h.assert_round_trip(name);
            let text = h.wait_for_file("t.md", |text| text == expected);
            assert_eq!(text, expected, "{name}");
        }
    }

    // Deleting across the edge of a list or a quote does what it does in Typora:
    // Delete at the end of a textblock and Backspace at the start of one after a
    // list or a quote join their text; Backspace at an item's start joins it to
    // the item before, or to the item its nested list is in.
    #[gpui::test]
    fn deleting_across_list_and_quote_edges_joins_as_typora_does(cx: &mut TestAppContext) {
        let cases: &[(&str, &str, &str)] = &[
            ("- 1\n- 2\n", "cmd-up cmd-right delete", "- 192"),
            ("- 1\n\n2\n", "cmd-down cmd-left backspace", "- 192"),
            ("- 1\n", "cmd-up cmd-right enter backspace", "- 1\n\n  9"),
            ("- 1\n- 2\n", "cmd-down cmd-left backspace", "- 1\n\n  92"),
            ("- 1\n- 2\n", "cmd-up backspace", "91\n\n- 2"),
            ("0\n\n- 1\n", "cmd-up cmd-right delete", "091"),
            ("- 1\n\n2\n", "cmd-up cmd-right delete", "- 192"),
            ("- 1\n  - 2\n", "cmd-down cmd-left backspace", "- 1\n\n  92"),
            ("- 1\n  - 2\n", "cmd-up cmd-right delete", "- 192"),
            (
                "1. 1\n2. 2\n",
                "cmd-down cmd-left backspace",
                "1. 1\n\n   92",
            ),
            (
                "- [ ] 1\n- [ ] 2\n",
                "cmd-down cmd-left backspace",
                "- [ ] 1\n\n  92",
            ),
            ("> 1\n\n2\n", "cmd-down cmd-left backspace", "> 192"),
            (
                "```\n1\n```\n\n2\n",
                "cmd-down cmd-left backspace",
                "```\n192\n```",
            ),
            (
                "1\n\n```\n2\n```\n",
                "cmd-up cmd-right delete",
                "19\n\n```\n2\n```",
            ),
        ];
        for (source, keys, expected) in cases {
            let mut h = open_with(cx, &[("x.md", source)], |_| {});
            h.keys(keys);
            h.type_text("9");
            assert_eq!(h.markdown(), *expected, "{source:?} {keys}");
            h.assert_round_trip(keys);
        }
    }

    // ⌘B at a caret does what it does in Typora: in a word it bolds the word,
    // in a bold span it takes the bold off the whole span, and elsewhere it
    // leaves a pair to type into.
    #[gpui::test]
    fn bold_at_a_caret_styles_the_word_as_typora_does(cx: &mut TestAppContext) {
        for (source, caret, expected) in [
            ("123 456\n", 6, "123 **4X56**"),
            ("0 **12** 3\n", 6, "0 1X2 3"),
            ("12  34\n", 4, "12 **X** 34"),
        ] {
            let mut h = open_with(cx, &[("b.md", source)], |_| {});
            h.edit(|editor, cx| {
                editor.dispatch(
                    [markraft_core::TransactionSpec::new()
                        .selection(markraft_core::Selection::cursor(caret))],
                    cx,
                )
            });
            h.keys("cmd-b");
            h.type_text("X");
            assert_eq!(h.markdown(), expected, "{source:?}");
            h.assert_round_trip("bold at a caret");
        }
    }

    // Around a divider, Backspace and Delete take it with one press, as Typora
    // does, and no empty line is left where it was.
    #[gpui::test]
    fn backspace_and_delete_take_a_divider_with_one_press(cx: &mut TestAppContext) {
        for (keys, expected) in [
            ("cmd-down cmd-left backspace", "a\n\nb\n"),
            ("cmd-up cmd-right delete", "ab\n"),
        ] {
            let mut h = open_with(cx, &[("d.md", "a\n\n---\n\nb\n")], |_| {});
            h.keys(keys);
            h.save();
            let text = h.wait_for_file("d.md", |text| text == expected);
            assert_eq!(text, expected, "{keys}");
        }
    }

    // ⌘Enter in a table opens a row below the caret's and moves into its first
    // cell, as Typora does. The rows written by hand keep their bytes, the
    // edit typed into the new row included.
    #[gpui::test]
    fn command_return_in_a_table_opens_a_row_to_type_in(cx: &mut TestAppContext) {
        let source = "|a|b|\n|-|-|\n|1|2|\n|3|4|\n";
        let mut h = open_with(cx, &[("t.md", source)], |_| {});
        h.keys("cmd-up down cmd-right cmd-enter");
        h.type_text("z");
        h.keys("down");
        h.type_text("y");
        h.save();
        let expected = "|a|b|\n|-|-|\n|1|2|\n|z| |\n|3y|4|\n";
        let text = h.wait_for_file("t.md", |text| text == expected);
        assert_eq!(text, expected);
    }

    // The Emacs keys every macOS text view takes: ⌃A and ⌃E go to the ends of
    // the paragraph, ⌃K deletes to its end, ⌃D and ⌃H delete a character.
    #[gpui::test]
    fn the_emacs_keys_move_and_delete(cx: &mut TestAppContext) {
        let cases: &[(&str, &str, &str)] = &[
            ("hello **world**\n", "cmd-up ctrl-e", "hello **world**X"),
            ("hello world\n", "cmd-up cmd-right ctrl-a", "Xhello world"),
            ("hello world\n", "cmd-up ctrl-f ctrl-f ctrl-k", "heX"),
            ("hello\n", "cmd-up ctrl-d", "Xello"),
            ("hello\n", "cmd-up ctrl-f ctrl-h", "Xello"),
        ];
        for (source, keys, expected) in cases {
            let mut h = open_with(cx, &[("e.md", source)], |_| {});
            h.keys(keys);
            h.type_text("X");
            assert_eq!(h.markdown(), *expected, "{source:?} {keys}");
        }
    }

    // Backspace right after a block shortcut takes the format off, as Typora
    // does, rather than giving back the characters that made it. A shortcut
    // inside the text is still undone to what was typed.
    #[gpui::test]
    fn backspace_after_a_block_shortcut_takes_the_format_off(cx: &mut TestAppContext) {
        for typed in ["- ", "1. ", "# ", "## ", "> ", "- [ ] "] {
            let mut h = open(cx, |_| {});
            h.type_text(typed);
            h.keys("backspace");
            h.type_text("x");
            assert_eq!(h.markdown(), "x", "{typed:?}");
        }
        // In a list already, the item leaves the list, as Backspace at any
        // first item's start does.
        let mut h = open_with(cx, &[("l.md", "a\n")], |_| {});
        h.keys("cmd-down enter");
        h.type_text("- ");
        h.keys("backspace");
        h.type_text("x");
        assert_eq!(h.markdown(), "a\n\nx");
    }

    // A divider typed with stars or underscores keeps them, as Typora does.
    #[gpui::test]
    fn a_typed_divider_keeps_its_characters(cx: &mut TestAppContext) {
        for divider in ["***", "___", "---"] {
            let mut h = open_with(cx, &[("d.md", "0\n")], |_| {});
            h.keys("cmd-down enter");
            h.type_text(divider);
            h.keys("enter");
            h.type_text("5");
            h.save();
            let expected = format!("0\n\n{divider}\n\n5\n");
            let text = h.wait_for_file("d.md", |text| text == expected);
            assert_eq!(text, expected, "{divider}");
        }
    }

    // ⌥⌘C does what it does in Typora: at a caret in text it opens a new code
    // block there — splitting the paragraph in its middle, and inside a task
    // item too — and on an empty line or over a selection it makes that a
    // code block, the caret at its start.
    #[gpui::test]
    fn the_code_block_shortcut_opens_a_block_at_the_caret(cx: &mut TestAppContext) {
        let cases: &[(&str, (usize, usize), &str)] = &[
            ("1234\n", (3, 3), "12\n\n```\n9\n```\n\n34"),
            ("1234\n", (5, 5), "1234\n\n```\n9\n```"),
            ("1234\n", (1, 1), "```\n9\n```\n\n1234"),
            ("1234\n", (1, 5), "```\n91234\n```"),
            (
                "- [ ] 12\n- [ ] 3\n",
                (5, 5),
                "- [ ] 12\n  ```\n  9\n  ```\n- [ ] 3",
            ),
        ];
        for (source, (anchor, head), expected) in cases {
            let mut h = open_with(cx, &[("c.md", source)], |_| {});
            h.edit(|editor, cx| {
                editor.dispatch(
                    [markraft_core::TransactionSpec::new()
                        .selection(markraft_core::Selection::text(*anchor, *head))],
                    cx,
                )
            });
            h.keys("alt-cmd-c");
            h.type_text("9");
            assert_eq!(h.markdown(), *expected, "{source:?} at {anchor}..{head}");
            h.assert_round_trip("a code block at the caret");
        }
    }

    // G4: a task box written `[X]` ticks off like any other.
    #[gpui::test]
    fn an_uppercase_task_box_toggles(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("t.md", "- [X] done\n")], |_| {});
        h.keys("cmd-down cmd-enter");
        assert_eq!(h.markdown(), "- [ ] done");
        h.assert_round_trip("unticking [X]");
    }

    // G5: every block format applied to a task item's text.
    #[gpui::test]
    fn block_formats_inside_a_task_item(cx: &mut TestAppContext) {
        // As in Typora, a task's line may be a heading or a quote after its box,
        // but not a fence, which goes under the box's line.
        let cases = [
            ("cmd-1", "- [ ] # t"),
            ("cmd-2", "- [ ] ## t"),
            ("cmd-6", "- [ ] ###### t"),
            ("cmd-0", "- [ ] t"),
            ("cmd-shift-b", "- [ ] > t"),
            ("alt-cmd-c", "- [ ] t\n  ```\n  ```"),
            ("cmd-&", "1. t"),
            ("cmd-*", "- t"),
            ("cmd-(", "t"),
        ];
        for (key, expected) in cases {
            let mut h = open_with(cx, &[("t.md", "- [ ] t\n")], |_| {});
            h.keys("cmd-down");
            h.keys(key);
            assert_eq!(h.markdown(), expected, "{key}");
            h.assert_round_trip(key);
        }
    }

    // G2: after several saves the guard and the save still agree on what can be
    // written back.
    #[gpui::test]
    fn the_guard_and_the_save_agree_after_several_saves(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("n.md", "a\n\n- b\n")], |_| {});
        for text in ["1", "2", "3"] {
            h.keys("cmd-down cmd-right");
            h.type_text(text);
            h.assert_round_trip("typing");
        }
        h.keys("cmd-up cmd-2");
        h.assert_round_trip("a heading after several saves");
        h.keys("cmd-down enter");
        h.type_text("c");
        h.assert_round_trip("a new item after several saves");
    }

    // G7: undoing an edit already saved writes the file back byte for byte.
    #[gpui::test]
    fn undo_after_a_save_restores_the_file(cx: &mut TestAppContext) {
        let original = "Title\n\n* one\n*  two\n\n|a|b|\n|-|-|\n";
        let mut h = open_with(cx, &[("u.md", original)], |_| {});
        h.keys("cmd-up cmd-1");
        h.assert_round_trip("a heading");
        h.keys("cmd-z");
        h.assert_round_trip("undone");
        let text = h.wait_for_file("u.md", |text| text == original);
        assert_eq!(text, original);
    }

    // G6: each preference reaches the note's editor.
    #[gpui::test]
    fn preferences_reach_the_editor(cx: &mut TestAppContext) {
        use crate::storage::*;
        // Pairing off: an opening bracket stays alone.
        let mut h = open(cx, |p| p.auto_pair = false);
        h.type_text("(a");
        assert_eq!(h.markdown(), "(a");
        // Pairing on, the default: the closing bracket comes with it.
        let mut h = open(cx, |_| {});
        h.type_text("(a");
        assert_eq!(h.markdown(), "(a)");
        // Tab in a code block writes the spaces asked for.
        let mut h = open_with(cx, &[("c.md", "```\nx\n```\n")], |p| {
            p.tab_key = TabKey::FourSpaces
        });
        h.keys("cmd-up tab");
        assert!(h.markdown().contains("\n    x"), "{:?}", h.markdown());
        // The list shortcuts take the preferred markers.
        let mut h = open(cx, |p| {
            p.bullet_marker = BulletMarker::Plus;
            p.ordered_delimiter = OrderedDelimiter::Parenthesis;
        });
        h.type_text("a");
        h.keys("cmd-*");
        assert_eq!(h.markdown(), "+ a");
        h.keys("cmd-&");
        assert_eq!(h.markdown(), "1) a");
        // Emphasis written with underscores.
        let mut h = open(cx, |p| p.emphasis_marker = EmphasisMarker::Underscore);
        h.type_text("a ");
        h.keys("cmd-i");
        h.type_text("b");
        assert_eq!(h.markdown(), "a _b_");
        // A shortcode stays a shortcode unless characters are asked for.
        let mut h = open(cx, |p| p.emoji_characters = false);
        h.type_text(":smile:");
        assert_eq!(h.markdown(), ":smile:");
        let mut h = open(cx, |p| p.emoji_characters = true);
        h.type_text(":smile:");
        assert_eq!(h.markdown(), "😄");
    }

    // G6: vim's normal mode edits the note, and what it does is saved.
    #[gpui::test]
    fn vim_edits_reach_the_file(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("v.md", "abc\n")], |p| p.vim_mode = true);
        // Vim starts in normal mode; Escape there would hide the window.
        h.keys("cmd-up");
        h.type_text("x");
        assert_eq!(h.markdown(), "bc");
        h.assert_round_trip("vim x");
    }

    // The input method sees the note in UTF-16 units, as macOS counts them: an
    // emoji is two. A range that starts or ends inside one is taken back to its
    // start; marked text and its caret are placed in those units; empty marked
    // text cancels; a marked range can replace text; a commit writes it.
    #[gpui::test]
    fn the_input_method_sees_the_note_in_utf16_units(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;
        let mut h = open_with(cx, &[("i.md", "a\u{1F600}b\n")], |_| {});
        h.keys("cmd-down");
        let editor = h.app.update(h.cx, |app, _| app.test_editor());
        let text = |h: &mut Harness, range: std::ops::Range<usize>| {
            editor.update_in(h.cx, |view, window, cx| {
                let mut actual = None;
                let text = view.text_for_range(range, &mut actual, window, cx);
                (text, actual)
            })
        };
        assert_eq!(text(&mut h, 0..4), (Some("a\u{1F600}b".into()), Some(0..4)));
        assert_eq!(text(&mut h, 1..3), (Some("\u{1F600}".into()), Some(1..3)));
        assert_eq!(text(&mut h, 1..2), (Some(String::new()), Some(1..1)));
        assert_eq!(text(&mut h, 2..4), (Some("\u{1F600}b".into()), Some(1..4)));
        let state = |h: &mut Harness| {
            editor.update_in(h.cx, |view, window, cx| {
                (
                    view.marked_text_range(window, cx),
                    view.selected_text_range(false, window, cx).map(|s| s.range),
                )
            })
        };
        assert_eq!(state(&mut h), (None, Some(4..4)));

        // A candidate whose caret is after its emoji: two units in.
        editor.update_in(h.cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, "\u{1F600}x", Some(2..2), window, cx)
        });
        assert_eq!(state(&mut h), (Some(4..7), Some(6..6)));
        // Empty marked text is how macOS says the candidate was cancelled.
        editor.update_in(h.cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, "", None, window, cx)
        });
        assert_eq!(state(&mut h), (None, Some(4..4)));
        assert_eq!(h.markdown(), "a\u{1F600}b");

        // A candidate over `b`, then committed in its place.
        editor.update_in(h.cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(Some(3..4), "x", None, window, cx)
        });
        assert_eq!(state(&mut h).0, Some(3..4));
        editor.update_in(h.cx, |view, window, cx| {
            view.replace_text_in_range(None, "\u{597d}", window, cx)
        });
        assert_eq!(state(&mut h), (None, Some(4..4)));
        assert_eq!(h.markdown(), "a\u{1F600}\u{597d}");

        // Where a candidate window goes: further along the line is further
        // right. The test platform lays text out with a placeholder font, so
        // only the order is judged.
        let bounds = |h: &mut Harness, range: std::ops::Range<usize>| {
            editor
                .update_in(h.cx, |view, window, cx| {
                    view.bounds_for_range(range, gpui::Bounds::default(), window, cx)
                })
                .expect("bounds for text on screen")
        };
        let first = bounds(&mut h, 0..1);
        let last = bounds(&mut h, 3..4);
        assert!(first.origin.x < last.origin.x, "{first:?} {last:?}");
        assert!(first.size.height > gpui::px(0.));
        h.assert_round_trip("a committed candidate");
    }

    // Vim's own tests drive a stand-in host with a keymap of their own; these keys
    // go through the bindings a person presses and the editor the app hosts: an
    // operator with a motion, undo and redo, a count, and a visual delete.
    #[gpui::test]
    fn vim_keys_reach_the_note_through_the_real_bindings(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("v.md", "one two three\n")], |p| p.vim_mode = true);
        h.keys("cmd-up");
        h.keys("d w");
        assert_eq!(h.markdown(), "two three");
        h.keys("u");
        assert_eq!(h.markdown(), "one two three");
        h.keys("ctrl-r");
        assert_eq!(h.markdown(), "two three");
        h.keys("2 x");
        assert_eq!(h.markdown(), "o three");
        h.keys("v l d");
        assert_eq!(h.markdown(), "three");
        h.assert_round_trip("vim operators");
    }

    // G6: the wiki-link and emoji menus open as the trigger is typed and write what
    // is chosen.
    #[gpui::test]
    fn the_link_and_emoji_menus_open_as_they_are_typed(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("Target note.md", "t\n"), ("here.md", "h\n")], |_| {});
        h.keys("cmd-n");
        h.type_text("see [[Targ");
        h.keys("enter");
        assert_eq!(h.markdown(), "see [[Target note]]");
        h.type_text(" :smil");
        h.keys("enter");
        assert_eq!(h.markdown(), "see [[Target note]] :smile:");
        h.assert_round_trip("a link and an emoji from the menus");
    }

    // G6: Markdown pasted into a list item and into a table cell. A GFM row is one
    // line, so what is pasted into a cell goes in as its inline content, a
    // `<br/>` for each line ending as Typora writes it, and the table keeps its
    // columns.
    #[gpui::test]
    fn markdown_pasted_into_a_list_item_and_a_table_cell(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("p.md", "- a\n")], |_| {});
        h.keys("cmd-down cmd-right");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string("**b** and *c*".into()));
        h.keys("cmd-v");
        assert_eq!(h.markdown(), "- a**b** and *c*");
        h.assert_round_trip("inline Markdown pasted into an item");
        let table = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        for (pasted, expected) in [
            ("x\ny", "| a | b |\n| --- | --- |\n| 1 | 2x<br/>y |\n"),
            (
                "x\n\ny",
                "| a | b |\n| --- | --- |\n| 1 | 2x<br/><br/>y |\n",
            ),
            ("x | y\n", "| a | b |\n| --- | --- |\n| 1 | 2x \\| y |\n"),
            (
                "**p** one\n- b",
                "| a | b |\n| --- | --- |\n| 1 | 2**p** one<br/>b |\n",
            ),
        ] {
            let mut h = open_with(cx, &[("t.md", table)], |_| {});
            h.keys("cmd-down");
            h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
            h.keys("cmd-v");
            h.assert_round_trip("blocks pasted into a cell");
            let text = h.wait_for_file("t.md", |text| text == expected);
            assert_eq!(text, expected, "{pasted:?}");
        }
    }

    // Paragraphs pasted in the middle of a paragraph carry on the text either
    // side of the caret, as Typora does; a heading at an end stays a block.
    #[gpui::test]
    fn paragraphs_pasted_mid_paragraph_join_the_text_around_them(cx: &mut TestAppContext) {
        for (pasted, expected) in [
            ("one\n\ntwo", "AAA one\n\ntwoBBB"),
            ("one\n\nmid\n\ntwo", "AAA one\n\nmid\n\ntwoBBB"),
            ("one\n\n# Head", "AAA one\n\n# Head\n\nBBB"),
        ] {
            let mut h = open_with(cx, &[("p.md", "AAA BBB\n")], |_| {});
            h.keys("cmd-up alt-right right");
            h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
            h.keys("cmd-v");
            assert_eq!(h.markdown(), expected, "{pasted:?}");
            h.assert_round_trip("paragraphs pasted mid-paragraph");
        }
    }

    // A `<br/>` in a table cell is the cell's line break, as in Typora: the caret
    // reaching it does not spell it out, and text typed on either side of it
    // stays on that side.
    #[gpui::test]
    fn a_break_in_a_table_cell_stays_a_break_under_the_caret(cx: &mut TestAppContext) {
        let table = "| a |\n| --- |\n| 1<br/>2 |\n";
        let doc = |h: &mut Harness<'_>| h.app.update(h.cx, |app, cx| app.active_document(cx));
        for (caret, expected) in [(9, "| 1x<br/>2 |"), (10, "| 1<br/>x2 |")] {
            let mut h = open_with(cx, &[("t.md", table)], |_| {});
            let before = doc(&mut h);
            h.edit(|editor, cx| {
                editor.dispatch(
                    [markraft_core::TransactionSpec::new()
                        .selection(markraft_core::Selection::cursor(caret))],
                    cx,
                )
            });
            assert_eq!(doc(&mut h), before, "the break is not spelled out");
            h.type_text("x");
            h.save();
            let text = h.wait_for_file("t.md", |text| text.contains('x'));
            assert!(text.contains(expected), "{text:?}");
        }
    }

    // Shift-Return in a table cell writes `<br />`, as Typora does.
    #[gpui::test]
    fn shift_return_in_a_table_cell_writes_a_break_tag(cx: &mut TestAppContext) {
        let table = "| 1 | 2 |\n| --- | --- |\n| 3 | 4 |\n";
        let mut h = open_with(cx, &[("t.md", table)], |_| {});
        h.keys("cmd-down shift-enter");
        h.type_text("9");
        h.save();
        let expected = "| 1 | 2 |\n| --- | --- |\n| 3 | 4<br />9 |\n";
        assert_eq!(h.wait_for_file("t.md", |text| text == expected), expected);
    }

    // Inline HTML is text like any other: the caret reaching a tag finds its
    // source, which is edited character by character and saved as typed. There
    // is no separate HTML editor.
    #[gpui::test]
    fn inline_html_is_edited_as_text(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("h.md", "press <kbd>K</kbd> now\n")], |_| {});
        h.edit(|editor, cx| {
            editor.dispatch(
                [markraft_core::TransactionSpec::new()
                    .selection(markraft_core::Selection::cursor(7))],
                cx,
            )
        });
        h.keys("right right");
        h.type_text("b");
        h.keys("cmd-down");
        h.save();
        let expected = "press <kbbd>K</kbd> now\n";
        assert_eq!(h.wait_for_file("h.md", |text| text == expected), expected);
        // ⌥⌘R opened the HTML editor; it is bound to nothing now.
        h.keys("alt-cmd-r");
        assert_eq!(h.markdown(), "press <kbbd>K</kbd> now");
    }

    // A list pasted into an empty item adds its items beside the ones around it,
    // whatever kind of list it was, rather than nesting a list in the item.
    #[gpui::test]
    fn a_list_pasted_into_an_empty_item_joins_the_list(cx: &mut TestAppContext) {
        for (source, pasted, expected) in [
            ("- a\n", "- b\n- c", "- a\n- b\n- c"),
            ("- a\n", "1. b\n2. c", "- a\n- b\n- c"),
            ("1. a\n", "- b\n- c", "1. a\n2. b\n3. c"),
            ("- [ ] a\n", "- b\n- c", "- [ ] a\n- b\n- c"),
        ] {
            let mut h = open_with(cx, &[("l.md", source)], |_| {});
            h.keys("cmd-down enter");
            h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(pasted.into()));
            h.keys("cmd-v");
            assert_eq!(h.markdown(), expected, "{source:?} + {pasted:?}");
            h.assert_round_trip("a list pasted into an empty item");
        }
    }

    // G6: with a panel open Tab walks its controls and leaves the note alone.
    #[gpui::test]
    fn tab_walks_an_open_panel_rather_than_indenting(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("l.md", "- a\n- b\n")], |_| {});
        h.keys("cmd-down cmd-k");
        let before = h.markdown();
        h.keys("tab tab");
        assert_eq!(h.markdown(), before);
        h.keys("escape tab");
        assert_eq!(h.markdown(), "- a\n  - b");
    }

    /// Documents the sweep edits: the writer's own spellings and ones only a person
    /// or another editor would write.
    const CORPUS: &[&str] = &[
        "para one\n\nsecond *em* and **strong**\n",
        "# Head\n\ntext\n",
        "Title\n===\n\ntext\n",
        "- a\n- b\n\n1. one\n2. two\n",
        "* a\n*  b\n",
        "- [ ] t\n- [X] u\n",
        "- first\n\n  para2\n- next\n",
        "> quote\n> more\n\n> [!note]\n> callout\n",
        "```rust\nfn a() {}\n```\n",
        "|a|b|\n|-|-|\n|1|2|\n",
        "| a | b |\n| --- | --- |\n| 1 | 2 |\n",
        "text[^1]\n\n[^1]: note\n",
        "$$\nx^2\n$$\n\ninline $y$ math\n",
        "<div>html</div>\n\nafter\n",
        "line one\\\nline two\n",
        "a [link](https://e.com) and [[wiki]]\n",
    ];

    /// `text` with the whitespace ending each line gone, which Markdown does not keep.
    fn trimmed(text: &str) -> String {
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The editing keys the sweep presses at every place it puts the caret.
    const EDITS: &[&str] = &[
        "cmd-b",
        "cmd-i",
        "cmd-e",
        "cmd-shift-s",
        "cmd-1",
        "cmd-2",
        "cmd-0",
        "cmd-shift-b",
        "alt-cmd-c",
        "cmd-&",
        "cmd-*",
        "cmd-(",
        "cmd-enter",
        "enter",
        "shift-enter",
        "backspace",
        "delete",
        "tab",
        "shift-tab",
        "alt-backspace",
        "alt-delete",
        "cmd-x",
        "cmd-v",
        "cmd-shift-v",
        "cmd-alt-shift-v",
        "type:x",
        "type:*",
        "type:`",
    ];

    /// What the sweep's pastes put in: blocks and styles, which a caret or a
    /// selection anywhere in the corpus must take or refuse whole.
    const CLIPBOARD: &str = "**p** one\n\n- item\n\n```\ncode\n```";

    /// Every edit key at the start and the end of every line of every document in
    /// [`CORPUS`], over each line's whole text and from the middle of each line into
    /// the next, in a note on disk and in one not yet saved. What the editor takes
    /// must be what a file can say: on disk, through the source it came from; unsaved,
    /// written whole and read back. Undo must put the note back exactly and redo
    /// must make the edit again. What the editor refuses is listed, for a person to
    /// judge, not failed.
    #[gpui::test]
    fn every_edit_in_the_corpus_leaves_a_note_a_file_can_hold(cx: &mut TestAppContext) {
        let mut failures = Vec::new();
        let mut refused = Vec::new();
        let mut applied = 0;
        for (index, document) in CORPUS.iter().enumerate() {
            for on_disk in [true, false] {
                let name = format!("c{index}.md");
                let mut h = if on_disk {
                    open_with(cx, &[(name.as_str(), document)], |_| {})
                } else {
                    let h = open(cx, |_| {});
                    let app = h.app.clone();
                    h.cx.update(|window, cx| {
                        app.update(cx, |app, cx| app.test_new_note(document, window, cx))
                    });
                    h.cx.run_until_parked();
                    h
                };
                // What the note would write: through the source it came from, or — not
                // yet saved — through an empty one, which holds a new note to what the
                // guard holds a saved one to.
                let source = if on_disk {
                    std::fs::read_to_string(h.notes.join(&name)).expect("the corpus file")
                } else {
                    String::new()
                };
                // Every textblock's start and end, its whole text, and the middle of it
                // to the middle of the next, placed by position: moving by lines needs
                // laid-out text, which the test platform does not shape.
                let mut blocks = Vec::new();
                let start = h.app.update(h.cx, |app, cx| app.active_document(cx));
                start.descendants(&mut |node, pos, _, _| {
                    if node.is_textblock(crate::doc::schema()) {
                        blocks.push((pos + 1, pos + 1 + node.content_size()));
                    }
                    true
                });
                let mut places = Vec::new();
                for &(from, to) in &blocks {
                    places.push((from, from));
                    places.push((to, to));
                    if from < to {
                        places.push((from, to));
                    }
                }
                for pair in blocks.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    places.push(((a.0 + a.1) / 2, (b.0 + b.1) / 2));
                }
                'places: for (anchor, head) in places {
                    for edit in EDITS {
                        h.focus_note();
                        h.edit(|editor, cx| {
                            editor.dispatch(
                                [markraft_core::TransactionSpec::new()
                                    .selection(markraft_core::Selection::text(anchor, head))],
                                cx,
                            )
                        });
                        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(CLIPBOARD.into()));
                        let before = h.app.update(h.cx, |app, cx| app.active_document(cx));
                        let selected = h.selection();
                        match edit.strip_prefix("type:") {
                            Some(text) => h.type_text(text),
                            None => h.keys(edit),
                        }
                        let after = h.app.update(h.cx, |app, cx| app.active_document(cx));
                        let at = format!("c{index} disk={on_disk} at {anchor}..{head} {edit}");
                        if after == before {
                            refused.push(at);
                            continue;
                        }
                        applied += 1;
                        // An empty paragraph writes as nothing, so the file is held
                        // to the note without the ones a reader would not give back.
                        let bare = |node: &markraft_core::Node| {
                            markraft_commonmark::source::without_empty_paragraphs(
                                crate::doc::schema(),
                                node,
                            )
                        };
                        let markdown = crate::doc::to_markdown(&bare(&after));
                        let rendered = markraft_commonmark::SourceDocument::parse(
                            crate::doc::schema(),
                            &source,
                        )
                        .expect("the corpus parses")
                        .render(crate::doc::schema(), &after);
                        match rendered {
                            Ok(text) => {
                                let read = bare(&crate::doc::from_markdown(&text));
                                let back = crate::doc::to_markdown(&read);
                                // A document that differs only by whitespace ending a
                                // line still says the same thing once written.
                                if read != bare(&after) && trimmed(&back) != trimmed(&markdown) {
                                    failures.push(format!(
                                        "{at}: file reads back as {back:?}, editor has {markdown:?}"
                                    ));
                                }
                            }
                            Err(error) => failures
                                .push(format!("{at}: taken but unsavable ({error}): {markdown:?}")),
                        }
                        h.focus_note();
                        h.keys("cmd-z");
                        let undone = h.app.update(h.cx, |app, cx| app.active_document(cx));
                        if undone != before {
                            failures.push(format!(
                                "{at}: undo left {:?}, was {:?}",
                                h.markdown(),
                                crate::doc::to_markdown(&before)
                            ));
                            // Sweeping on from a document undo got wrong would only
                            // report the same thing again.
                            break 'places;
                        }
                        if h.selection() != selected {
                            failures.push(format!(
                                "{at}: undo left the selection at {:?}, was {selected:?}",
                                h.selection()
                            ));
                        }
                        h.keys("cmd-shift-z");
                        let redone = h.app.update(h.cx, |app, cx| app.active_document(cx));
                        if redone != after {
                            failures.push(format!(
                                "{at}: redo left {:?}, the edit made {markdown:?}",
                                h.markdown()
                            ));
                        }
                        h.keys("cmd-z");
                        let undone = h.app.update(h.cx, |app, cx| app.active_document(cx));
                        if undone != before {
                            failures.push(format!(
                                "{at}: undo after redo left {:?}, was {:?}",
                                h.markdown(),
                                crate::doc::to_markdown(&before)
                            ));
                            break 'places;
                        }
                    }
                }
            }
        }
        eprintln!(
            "applied {applied}, refused ({}):\n{}",
            refused.len(),
            refused.join("\n")
        );
        // Refusals are listed, not failed, so a sweep whose keys stopped reaching
        // the note would pass on refusals alone; 5428 edits applied when this was set.
        assert!(applied >= 5000, "only {applied} edits applied");
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    // Typora 1.14.10 parity, found replaying the editing tests in both apps
    // (A1-A9 of the comparison). Each expectation is what Typora saved.

    // A1: ⌥← from the end of a line that ends in hidden markup reaches the
    // start of the word inside it, as it does before plain text.
    #[gpui::test]
    fn word_motion_crosses_hidden_markup_at_a_line_edge(cx: &mut TestAppContext) {
        for (source, keys, expected) in [
            ("**abc**\n", "cmd-up cmd-right alt-left", "**Xabc**"),
            ("`abc`\n", "cmd-up cmd-right alt-left", "`Xabc`"),
            ("*a b c*\n", "cmd-up cmd-right alt-left", "*a b Xc*"),
            ("**a** **b**\n", "cmd-up cmd-left alt-right", "**aX** **b**"),
        ] {
            let mut h = open_with(cx, &[("w.md", source)], |_| {});
            h.keys(keys);
            h.type_text("X");
            assert_eq!(h.markdown(), expected, "{source:?}");
        }
    }

    // A2: an opener typed into a note just emptied still writes its closer.
    #[gpui::test]
    fn an_emptied_note_pairs_the_first_opener(cx: &mut TestAppContext) {
        for (opener, expected) in [("(", "()"), ("[", "[]"), ("{", "{}"), ("\"", "\"\"")] {
            let mut h = open_with(cx, &[("p.md", "x\n")], |_| {});
            h.keys("cmd-a backspace");
            h.type_text(opener);
            assert_eq!(h.markdown(), expected, "{opener}");
        }
    }

    // A3: a word deletion over hidden markup keeps what is typed after it.
    #[gpui::test]
    fn typing_after_deleting_words_over_hidden_markup_reaches_the_note(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("w.md", "hello **world** end\n")], |_| {});
        h.keys("cmd-up cmd-right alt-backspace alt-backspace");
        h.type_text("X");
        assert_eq!(h.markdown(), "hello X");
    }

    // A4: the first item of a list, made by Return and lifted out again, is a
    // paragraph of its own that typing goes into, not the heading above.
    #[gpui::test]
    fn typing_into_a_new_first_item_lifted_out_stays_below_the_heading(cx: &mut TestAppContext) {
        for (list, rest) in [
            ("1. one\n2. two\n", "1. one\n2. two"),
            ("- one\n- two\n", "- one\n- two"),
        ] {
            let source = format!("# Lists\n\n{list}");
            let mut h = open_with(cx, &[("l.md", source.as_str())], |_| {});
            h.keys("cmd-up cmd-right down cmd-left enter up backspace");
            h.type_text("5");
            assert_eq!(h.markdown(), format!("# Lists\n\n5\n\n{rest}"));
        }
    }

    // A5: a paste keeps the space it starts with.
    #[gpui::test]
    fn a_paste_keeps_its_leading_space(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("p.md", "x\n")], |_| {});
        h.keys("cmd-down cmd-right");
        h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(" ![a](b) and".into()));
        h.keys("cmd-v");
        assert_eq!(h.markdown(), "x ![a](b) and");
    }

    // A6: what is copied across two list items pastes back as two items.
    #[gpui::test]
    fn a_copy_across_two_items_pastes_as_items(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("c.md", "- one\n- two\n")], |_| {});
        h.keys("cmd-up cmd-left right shift-down shift-right cmd-c cmd-down enter enter cmd-v");
        let markdown = h.markdown();
        let items: Vec<_> = markdown.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(items, ["- one", "- two", "- ne", "- tw"], "{markdown:?}");
    }

    // A7: moving into another cell with Tab, Shift-Tab or Return selects what it
    // holds, so typing replaces it.
    #[gpui::test]
    fn moving_into_a_cell_selects_its_content(cx: &mut TestAppContext) {
        let table = "| a | b |\n| - | - |\n| c | d |\n";
        for (keys, cells) in [
            ("cmd-up tab", ["a", "x", "c", "d"]),
            ("cmd-up tab shift-tab", ["x", "b", "c", "d"]),
            ("cmd-up enter", ["a", "b", "x", "d"]),
        ] {
            let mut h = open_with(cx, &[("t.md", table)], |_| {});
            h.keys(keys);
            h.type_text("x");
            let markdown = h.markdown();
            let found: Vec<_> = markdown
                .lines()
                .filter(|line| !line.contains('-'))
                .flat_map(|line| {
                    line.split('|')
                        .map(str::trim)
                        .filter(|cell| !cell.is_empty())
                })
                .collect();
            assert_eq!(found, cells, "{keys}: {markdown:?}");
        }
    }

    // A8: a reference definition typed on a line of its own is a definition.
    #[gpui::test]
    fn a_typed_reference_definition_defines(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("r.md", "[a][ref]\n")], |_| {});
        h.keys("cmd-down cmd-right enter");
        h.type_text("[ref]: /wx");
        h.keys("cmd-up");
        assert_eq!(h.markdown(), "[a][ref]\n\n[ref]: /wx");
    }

    // A9: Return inside a code line's indent drops the blanks after the caret.
    #[gpui::test]
    fn return_in_a_code_indent_drops_the_blanks_after_the_caret(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("c.md", "```\n    x\n```\n")], |_| {});
        h.keys("cmd-up right right enter");
        h.type_text("y");
        assert_eq!(h.markdown(), "```\n  \n  yx\n```");
    }

    // Decided to follow Typora 1.14.10 after the comparison (the E class).

    // A bracket pairs before whitespace or a closer, not before punctuation.
    #[gpui::test]
    fn a_bracket_before_punctuation_does_not_pair(cx: &mut TestAppContext) {
        let mut h = open_with(cx, &[("p.md", "see , then\n")], |_| {});
        h.keys("cmd-up cmd-left right right right right");
        h.type_text("(a");
        assert_eq!(h.markdown(), "see (a, then");
    }

    // ⌘B over part of a bold span takes the bold off the whole span; over
    // part of a code span it does nothing.
    #[gpui::test]
    fn strong_over_part_of_a_span_acts_on_the_whole_or_not_at_all(cx: &mut TestAppContext) {
        for (source, expected) in [("**abc**\n", "abc"), ("`abc`\n", "`abc`")] {
            let mut h = open_with(cx, &[("s.md", source)], |_| {});
            h.keys("cmd-up cmd-right alt-left right shift-right cmd-b");
            assert_eq!(h.markdown(), expected, "{source:?}");
        }
    }

    // A new item numbers the ones after it again in the file, unless the list
    // is written with one number throughout.
    #[gpui::test]
    fn an_item_added_to_an_ordered_list_renumbers_the_file(cx: &mut TestAppContext) {
        for (source, keys, expected) in [
            (
                "1. one\n2. two\n",
                "cmd-up cmd-left enter up",
                "1. 5\n2. one\n3. two\n",
            ),
            (
                "1. a\n1. b\n",
                "cmd-down cmd-right enter",
                "1. a\n1. b\n1. 5\n",
            ),
        ] {
            let mut h = open_with(cx, &[("o.md", source)], |_| {});
            h.keys(keys);
            h.type_text("5");
            h.save();
            let text = h.wait_for_file("o.md", |text| text != source);
            assert_eq!(text, expected, "{source:?}");
        }
    }
}
