//! Every edit key at every place in a corpus of documents.

use super::harness::{open, open_with};
use gpui::TestAppContext;

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
                h.stored(&name)
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
                    h.select(anchor, head);
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
                    let rendered =
                        markraft_commonmark::SourceDocument::parse(crate::doc::schema(), &source)
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
