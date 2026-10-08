//! Exercise the native menu's request/completion boundary over a real note and
//! persistence session. Pointer geometry and AppKit tracking are tested elsewhere.

use super::{ContextTarget, EditCommand, Intent, MenuAction, Row, TableEdit, doc};
use crate::e2e::harness::{Harness, open_with};
use gpui::{ClipboardItem, TestAppContext, point, px};

fn open_menu(h: &mut Harness<'_>, target: ContextTarget) -> u64 {
    let entity = h.app.clone();
    h.cx.update(|window, cx| {
        entity.update(cx, |app, cx| {
            let editor = app.editor().clone();
            let request = editor
                .read(cx)
                .context_snapshot(point(px(20.), px(30.)), target);
            app.request_context_menu(&editor, request, window, cx);
            app.context_menus
                .pending
                .as_ref()
                .expect("a pending menu")
                .id
        })
    })
}

fn item(h: &mut Harness<'_>, matches: impl Fn(&MenuAction) -> bool) -> usize {
    h.app.update(h.cx, |app, _| {
        app.context_menus
            .pending
            .as_ref()
            .expect("a pending menu")
            .actions
            .iter()
            .position(matches)
            .expect("the requested menu action")
    })
}

fn command(h: &mut Harness<'_>, command: EditCommand) -> usize {
    item(
        h,
        |action| matches!(action, MenuAction::Intent(Intent::Edit(found)) if *found == command),
    )
}

fn finish(h: &mut Harness<'_>, id: u64, selected: Option<usize>) {
    let entity = h.app.clone();
    h.cx.update(|window, cx| {
        entity.update(cx, |app, cx| {
            app.finish_context_menu(id, selected, window, cx)
        })
    });
    h.cx.run_until_parked();
}

fn row_state(rows: &[Row], id: usize) -> Option<(bool, Option<bool>)> {
    rows.iter().find_map(|row| match row {
        Row::Item {
            id: found,
            enabled,
            checked,
            ..
        } if *found == id => Some((*enabled, *checked)),
        Row::Submenu { children, .. } => row_state(children, id),
        _ => None,
    })
}

fn state(h: &mut Harness<'_>, id: usize) -> (bool, Option<bool>) {
    h.app.update(h.cx, |app, cx| {
        let session = app.context_menus.pending.as_ref().expect("a pending menu");
        let (rows, _) = app.context_menu_rows(&session.request, cx);
        row_state(&rows, id).expect("a row for the action")
    })
}

fn clipboard(h: &mut Harness<'_>) -> String {
    h.cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

fn rows(h: &mut Harness<'_>) -> Vec<Row> {
    h.app.update(h.cx, |app, cx| {
        let session = app.context_menus.pending.as_ref().expect("a pending menu");
        app.context_menu_rows(&session.request, cx).0
    })
}

fn assert_item(row: &Row, expected: usize) {
    assert!(matches!(row, Row::Item { id, .. } if *id == expected));
}

#[gpui::test]
fn link_commands_lead_without_changing_generic_edit_order(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("note.md", "[sample link](https://example.com) tail\n")],
        |_| {},
    );
    h.select(2, 13);
    let id = open_menu(
        &mut h,
        ContextTarget::Link {
            url: "https://example.com".into(),
            range: 2..13,
        },
    );
    let menu = rows(&mut h);
    let open = item(&mut h, |a| matches!(a, MenuAction::OpenUrl(_)));
    let copy = item(&mut h, |a| matches!(a, MenuAction::CopyUrl(_)));
    let edit = item(&mut h, |a| {
        matches!(a, MenuAction::Intent(Intent::EditLink))
    });
    let unlink = item(&mut h, |a| matches!(a, MenuAction::Intent(Intent::Unlink)));
    for (row, action) in menu.iter().zip([open, copy, edit, unlink]) {
        assert_item(row, action);
    }
    assert!(matches!(menu[4], Row::Separator));
    let edits = menu
        .iter()
        .filter_map(|row| match row {
            Row::Item { id, .. } => Some(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let cut = command(&mut h, EditCommand::Cut);
    let copy = command(&mut h, EditCommand::Copy);
    let paste = command(&mut h, EditCommand::Paste);
    let position = |id| edits.iter().position(|found| *found == id).unwrap();
    assert_eq!(position(copy), position(cut) + 1);
    assert_eq!(position(paste), position(copy) + 1);
    let paste_match = item(&mut h, |a| matches!(a, MenuAction::PasteMatchStyle));
    assert_eq!(position(paste_match), position(paste) + 1);
    finish(&mut h, id, Some(unlink));
    assert_eq!(h.markdown(), "sample link tail");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "[sample link](https://example.com) tail");

    let id = open_menu(&mut h, ContextTarget::Text);
    let menu = rows(&mut h);
    assert!(matches!(
        &menu[0],
        Row::Item {
            symbol: Some("info.circle"),
            ..
        }
    ));
    assert!(menu.len() > 7);
    finish(&mut h, id, None);
}

fn lock_note(h: &mut Harness<'_>) {
    let before = h.active_note();
    let locked = crate::storage::Note {
        read_only: Some("Locked".into()),
        ..before.clone()
    };
    h.external(vec![crate::vault::External::Updated {
        previous: Some(before),
        note: locked,
    }]);
}

#[gpui::test]
fn copy_uses_the_selection_and_cancel_keeps_the_document_and_clipboard(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two three\n")], |_| {});
    h.select(5, 8);
    let selection = h.selection();
    let id = open_menu(&mut h, ContextTarget::Text);
    let copy = command(&mut h, EditCommand::Copy);
    assert_eq!(state(&mut h, copy), (true, None));
    finish(&mut h, id, Some(copy));
    assert_eq!(clipboard(&mut h), "two");
    assert_eq!(h.markdown(), "one two three");
    assert_eq!(h.selection(), selection);

    let id = open_menu(&mut h, ContextTarget::Text);
    finish(&mut h, id, None);
    assert_eq!(h.markdown(), "one two three");
    assert_eq!(h.selection(), selection);
    assert_eq!(clipboard(&mut h), "two");
    h.app
        .update(h.cx, |app, _| assert!(app.context_menus.pending.is_none()));
}

#[gpui::test]
fn cancelling_checking_retires_services_retained_after_menu_escape(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let id = open_menu(&mut h, ContextTarget::Text);
    // Supply the application session without instantiating AppKit in a headless
    // test. The native responder restoration is covered by device validation.
    h.app.update(h.cx, |app, _| {
        let pending = app.context_menus.pending.as_ref().unwrap();
        app.context_menus.native = Some(super::NativeSession {
            id,
            note: pending.note.clone(),
            editor: pending.editor.clone(),
            request: pending.request.clone(),
            _requestor: None,
            _translation: None,
        });
    });
    finish(&mut h, id, None);
    h.app.update(h.cx, |app, _| {
        // AppKit reports no menu action when a Service handles the selection.
        // Escape alone must preserve that asynchronous session.
        assert_eq!(app.context_menus.native.as_ref().unwrap().id, id);
        app.cancel_checking_panel();
        // Returned native results require this session to still be active.
        assert!(app.context_menus.native.is_none());
        assert!(app.context_menus.pending.is_none());
    });
    assert_eq!(h.markdown(), "hello world");
}

#[gpui::test]
fn cancelling_checking_rejects_a_queued_menu_action(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let id = open_menu(&mut h, ContextTarget::Text);
    let cut = command(&mut h, EditCommand::Cut);
    h.cx.write_to_clipboard(ClipboardItem::new_string("keep".into()));
    h.app.update(h.cx, |app, _| app.cancel_checking_panel());
    finish(&mut h, id, Some(cut));
    assert_eq!(h.markdown(), "hello world");
    assert_eq!(clipboard(&mut h), "keep");
}

#[gpui::test]
fn paste_is_undoable_and_persists_through_the_existing_edit_pipeline(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two three\n")], |_| {});
    h.select(5, 8);
    h.cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
    let id = open_menu(&mut h, ContextTarget::Text);
    let paste = command(&mut h, EditCommand::Paste);
    finish(&mut h, id, Some(paste));
    assert_eq!(h.markdown(), "one new three");
    h.assert_round_trip("a context-menu paste");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "one two three");
    h.assert_round_trip("undoing a context-menu paste");
}

#[gpui::test]
fn asynchronous_image_paste_is_separate_from_vim_insert_typing(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "start\n")], |p| p.vim_mode = true);
    h.keys("cmd-up A");
    h.type_text("before");
    assert_eq!(h.markdown(), "startbefore");
    let png = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/icon/markraft-menubar.png"
    ))
    .expect("an image to paste");
    h.cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png,
    )));
    let id = open_menu(&mut h, ContextTarget::Text);
    let paste = command(&mut h, EditCommand::Paste);
    assert_eq!(state(&mut h, paste), (true, None));
    finish(&mut h, id, Some(paste));
    h.wait_until(|h| h.markdown().ends_with(".png)"));
    let with_image = h.markdown();
    assert!(with_image.starts_with("startbefore![image](assets/"));
    assert!(with_image.ends_with(".png)"));
    h.type_text("after");
    assert_eq!(h.markdown(), format!("{with_image}after"));
    h.assert_round_trip("an asynchronous context-menu image paste in Vim Insert mode");
    h.keys("escape u");
    assert_eq!(h.markdown(), with_image);
    h.keys("u");
    assert_eq!(h.markdown(), "startbefore");
    h.keys("u");
    assert_eq!(h.markdown(), "start");
    h.assert_round_trip("undoing a context-menu image paste and its surrounding typing");
}

#[gpui::test]
fn readonly_menus_allow_copy_and_refuse_mutations(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two\n")], |_| {});
    h.select(1, 4);
    h.cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
    lock_note(&mut h);
    let id = open_menu(&mut h, ContextTarget::Text);
    for edit in [EditCommand::Cut, EditCommand::Paste] {
        let selected = command(&mut h, edit);
        assert_eq!(state(&mut h, selected), (false, None));
    }
    let paste_match = item(&mut h, |a| matches!(a, MenuAction::PasteMatchStyle));
    assert_eq!(state(&mut h, paste_match), (false, None));
    let copy = command(&mut h, EditCommand::Copy);
    assert_eq!(state(&mut h, copy), (true, None));
    finish(&mut h, id, Some(copy));
    assert_eq!(clipboard(&mut h), "one");
    assert_eq!(h.markdown(), "one two");
}

#[gpui::test]
fn completion_rechecks_permissions_after_the_menu_was_built(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two\n")], |_| {});
    h.select(1, 4);
    h.cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
    let id = open_menu(&mut h, ContextTarget::Text);
    let cut = command(&mut h, EditCommand::Cut);
    assert_eq!(state(&mut h, cut), (true, None));
    lock_note(&mut h);
    h.app
        .update(h.cx, |app, cx| assert!(app.context_menu_current(id, cx)));
    finish(&mut h, id, Some(cut));
    assert_eq!(h.markdown(), "one two");
    assert_eq!(clipboard(&mut h), "unchanged");
}

#[gpui::test]
fn selection_or_document_changes_expire_a_pending_command(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two\n")], |_| {});
    for change_document in [false, true] {
        h.select(1, 4);
        h.cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        let id = open_menu(&mut h, ContextTarget::Text);
        let cut = command(&mut h, EditCommand::Cut);
        if change_document {
            h.type_text("new");
        } else {
            h.select(5, 8);
        }
        let expected = h.markdown();
        finish(&mut h, id, Some(cut));
        assert_eq!(h.markdown(), expected);
        assert_eq!(clipboard(&mut h), "unchanged");
        h.app
            .update(h.cx, |app, _| assert!(app.context_menus.pending.is_none()));
    }
}

#[gpui::test]
fn switching_notes_or_replacing_a_popup_cannot_deliver_an_old_command(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("first.md", "first\n"), ("second.md", "second\n")],
        |_| {},
    );
    h.browse_to("first");
    h.select(1, 6);
    h.cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
    let old = open_menu(&mut h, ContextTarget::Text);
    let cut = command(&mut h, EditCommand::Cut);
    let current = open_menu(&mut h, ContextTarget::Text);
    finish(&mut h, old, Some(cut));
    h.app.update(h.cx, |app, cx| {
        assert!(app.context_menu_current(current, cx))
    });
    h.browse_to("second");
    finish(&mut h, current, Some(cut));
    assert_eq!(h.markdown(), "second");
    assert_eq!(clipboard(&mut h), "unchanged");
    h.browse_to("first");
    assert_eq!(h.markdown(), "first");
}

#[gpui::test]
fn code_copy_targets_the_clicked_block_without_moving_the_selection(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[(
            "note.md",
            "paragraph\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n",
        )],
        |_| {},
    );
    h.select(1, 10);
    let selection = h.selection();
    let pos = h.app.update(h.cx, |app, cx| {
        let editor = app.editor().read(cx);
        (0..editor.state().doc().content().size())
            .find(|pos| editor.code_text_at(*pos).is_some())
            .expect("a code block")
    });
    let id = open_menu(&mut h, ContextTarget::CodeBlock { pos });
    let copy = item(
        &mut h,
        |action| matches!(action, MenuAction::CopyCode(found) if *found == pos),
    );
    let menu = rows(&mut h);
    assert_item(&menu[0], copy);
    assert!(matches!(menu[2], Row::Separator));
    assert!(row_state(&menu, command(&mut h, EditCommand::Cut)).is_some());
    finish(&mut h, id, Some(copy));
    assert_eq!(clipboard(&mut h), "let x = 1;\nlet y = 2;");
    assert_eq!(h.selection(), selection);
}

#[gpui::test]
fn link_address_copy_uses_the_clicked_object_and_preserves_unrelated_selection(
    cx: &mut TestAppContext,
) {
    let mut h = open_with(
        cx,
        &[("note.md", "[link](https://example.com) tail\n")],
        |_| {},
    );
    let tail = "[link](https://example.com) ".chars().count() + 1;
    h.select(tail, tail + 4);
    let selection = h.selection();
    let id = open_menu(
        &mut h,
        ContextTarget::Link {
            url: "https://example.com".into(),
            range: 2..6,
        },
    );
    h.app.update(h.cx, |app, _| {
        assert!(
            !app.context_menus
                .pending
                .as_ref()
                .unwrap()
                .actions
                .iter()
                .any(|action| matches!(
                    action,
                    MenuAction::Intent(Intent::EditLink | Intent::Unlink)
                ))
        );
    });
    let copy = item(&mut h, |action| matches!(action, MenuAction::CopyUrl(_)));
    finish(&mut h, id, Some(copy));
    assert_eq!(clipboard(&mut h), "https://example.com");
    assert_eq!(h.selection(), selection);
    assert_eq!(h.markdown(), "[link](https://example.com) tail");
}

#[gpui::test]
fn empty_selection_and_clipboard_disable_the_corresponding_actions(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "one two\n")], |_| {});
    h.select(1, 1);
    h.cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    let id = open_menu(&mut h, ContextTarget::Text);
    for edit in [EditCommand::Copy, EditCommand::Cut, EditCommand::Paste] {
        let selected = command(&mut h, edit);
        assert_eq!(state(&mut h, selected), (false, None));
    }
    let paste_match = item(&mut h, |a| matches!(a, MenuAction::PasteMatchStyle));
    assert_eq!(state(&mut h, paste_match), (false, None));
    finish(&mut h, id, None);
    assert_eq!(h.markdown(), "one two");
}

#[gpui::test]
fn identical_link_destinations_do_not_make_a_cross_link_selection_editable(
    cx: &mut TestAppContext,
) {
    let mut h = open_with(
        cx,
        &[(
            "note.md",
            "[one](https://example.com)[two](https://example.com)\n",
        )],
        |_| {},
    );
    let second = "[one](https://example.com)[".chars().count() + 1;
    h.select(2, second + 3);
    h.app.update(h.cx, |app, cx| {
        assert_eq!(
            app.editor().read(cx).active_link().as_deref(),
            Some("https://example.com")
        );
    });
    let id = open_menu(
        &mut h,
        ContextTarget::Link {
            url: "https://example.com".into(),
            range: 2..5,
        },
    );
    h.app.update(h.cx, |app, _| {
        assert!(
            !app.context_menus
                .pending
                .as_ref()
                .unwrap()
                .actions
                .iter()
                .any(|action| matches!(
                    action,
                    MenuAction::Intent(Intent::EditLink | Intent::Unlink)
                ))
        );
    });
    finish(&mut h, id, None);

    h.select(2, 5);
    let id = open_menu(
        &mut h,
        ContextTarget::Link {
            url: "https://example.com".into(),
            range: 2..5,
        },
    );
    let unlink = item(&mut h, |action| {
        matches!(action, MenuAction::Intent(Intent::Unlink))
    });
    assert_eq!(state(&mut h, unlink), (true, None));
    finish(&mut h, id, Some(unlink));
    assert_eq!(h.markdown(), "one[two](https://example.com)");
    h.assert_round_trip("unlinking only the clicked link");
}

#[gpui::test]
fn format_checks_and_table_commands_follow_the_current_selection(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "**bold** plain\n")], |_| {});
    h.select(1, 5);
    let id = open_menu(&mut h, ContextTarget::Text);
    let bold = item(&mut h, |action| {
        matches!(action, MenuAction::Intent(Intent::Mark(doc::Inline::Bold)))
    });
    let italic = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Intent(Intent::Mark(doc::Inline::Italic))
        )
    });
    assert_eq!(state(&mut h, bold), (true, Some(true)));
    assert_eq!(state(&mut h, italic), (true, Some(false)));
    finish(&mut h, id, Some(bold));
    assert_eq!(h.markdown(), "bold plain");
    h.assert_round_trip("a context-menu format toggle");

    let mut h = open_with(
        cx,
        &[("table.md", "| a | b |\n| --- | --- |\n| 1 | 2 |\n")],
        |_| {},
    );
    h.keys("cmd-down");
    let target = h.app.update(h.cx, |app, cx| {
        let editor = app.editor().read(cx);
        let resolved = editor.state().doc().resolve(editor.head()).unwrap();
        // Paragraph, cell, row and table are the table's schema structure.
        ContextTarget::Table {
            pos: resolved.before(1),
            cell: resolved.before(3),
        }
    });
    let id = open_menu(&mut h, target);
    let menu = rows(&mut h);
    assert!(matches!(menu[0], Row::Submenu { .. }));
    assert!(matches!(menu[1], Row::Separator));
    assert!(row_state(&menu, command(&mut h, EditCommand::Cut)).is_some());
    let add_row = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Intent(Intent::Table(TableEdit::RowAfter))
        )
    });
    assert_eq!(state(&mut h, add_row), (true, None));
    finish(&mut h, id, Some(add_row));
    assert_eq!(h.markdown().lines().count(), 4);
    h.assert_round_trip("a context-menu table row");
}

#[gpui::test]
fn transformations_preserve_marks_and_have_independent_undo(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "**hello** world\n")], |_| {});
    h.select(1, 16);
    let id = open_menu(&mut h, ContextTarget::Text);
    let uppercase = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Transform(markraft_gpui::TextTransformation::Uppercase)
        )
    });
    assert_eq!(state(&mut h, uppercase), (true, None));
    finish(&mut h, id, Some(uppercase));
    assert_eq!(h.markdown(), "**HELLO** WORLD");
    h.assert_round_trip("transforming reading text with formatting");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "**hello** world");
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn chinese_transformations_preserve_context_across_marks_and_source(cx: &mut TestAppContext) {
    use markraft_gpui::TextTransformation;
    let source = "**头**发 [里**面**](https://example.com/里面) 发型\n";
    let mut h = open_with(cx, &[("note.md", source)], |_| {});
    h.select(1, source.trim_end().chars().count() + 1);
    let id = open_menu(&mut h, ContextTarget::Text);
    let traditional = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Transform(TextTransformation::TraditionalChinese)
        )
    });
    assert_eq!(state(&mut h, traditional), (true, None));
    finish(&mut h, id, Some(traditional));
    assert_eq!(
        h.markdown(),
        "**頭**髮 [裡**面**](https://example.com/里面) 髮型"
    );
    h.assert_round_trip("contextual Chinese transformation across formatting");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), source.trim_end());

    // Read-only validation gates Chinese conversion through the same menu session.
    h.select(1, source.trim_end().chars().count() + 1);
    lock_note(&mut h);
    let id = open_menu(&mut h, ContextTarget::Text);
    let traditional = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Transform(TextTransformation::TraditionalChinese)
        )
    });
    assert_eq!(state(&mut h, traditional), (false, None));
    finish(&mut h, id, Some(traditional));
    assert_eq!(h.markdown(), source.trim_end());
}

#[gpui::test]
fn transformations_hide_empty_submenu_for_uncased_text(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "123 😀 中文\n")], |_| {});
    h.select(1, 9);
    let id = open_menu(&mut h, ContextTarget::Text);
    assert!(
        !rows(&mut h)
            .iter()
            .any(|row| matches!(row, Row::Submenu { label, .. } if label == "Transformations"))
    );
    finish(&mut h, id, None);
}

#[gpui::test]
fn asynchronous_service_writeback_rechecks_snapshot_and_readonly(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    let (note, editor, request) = h.app.update(h.cx, |app, cx| {
        let editor = app.editor();
        (
            app.notes.library.active_id.clone(),
            editor.downgrade(),
            editor
                .read(cx)
                .context_snapshot(point(px(0.), px(0.)), ContextTarget::Text),
        )
    });
    let applied = h.app.update(h.cx, |app, cx| {
        app.apply_service_text(&note, &editor, &request, "goodbye", cx)
    });
    assert!(applied);
    assert_eq!(h.markdown(), "goodbye world");
    h.assert_round_trip("a native service replacement");
    let applied = h.app.update(h.cx, |app, cx| {
        app.apply_service_text(&note, &editor, &request, "stale", cx)
    });
    assert!(!applied);
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "hello world");
    h.select(1, 6);
    let request = h.app.update(h.cx, |app, cx| {
        app.editor()
            .read(cx)
            .context_snapshot(point(px(0.), px(0.)), ContextTarget::Text)
    });
    lock_note(&mut h);
    let applied = h.app.update(h.cx, |app, cx| {
        app.apply_service_text(&note, &editor, &request, "locked", cx)
    });
    assert!(!applied);
    assert_eq!(h.markdown(), "hello world");
}

#[gpui::test]
fn proofreading_preserves_local_styles_and_undoes_as_one_edit(cx: &mut TestAppContext) {
    use super::TextReplacement;

    let source = "**This** are a smple sentnce.";
    let corrected = "**This** is a simple sentence.";
    let mut h = open_with(cx, &[("note.md", source)], |_| {});
    h.keys("cmd-a");
    let (note, editor, request) = h.app.update(h.cx, |app, cx| {
        let editor = app.editor();
        (
            app.notes.library.active_id.clone(),
            editor.downgrade(),
            editor
                .read(cx)
                .context_snapshot(Default::default(), ContextTarget::Text),
        )
    });
    assert!(h.app.update(h.cx, |app, cx| app.apply_service_replacement(
        &note,
        &editor,
        &request,
        TextReplacement::PreserveStyles("This is a simple sentence.".into()),
        cx,
    )));
    assert_eq!(h.markdown(), corrected);
    h.assert_round_trip("proofreading preserves the unchanged bold word");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), source);
    h.keys("cmd-shift-z");
    assert_eq!(h.markdown(), corrected);
}

#[gpui::test]
fn service_origins_apply_distinct_styles_even_when_returned_text_is_unchanged(
    cx: &mut TestAppContext,
) {
    use super::TextReplacement;

    let source = "**red** blue";
    let mut h = open_with(cx, &[("note.md", source)], |_| {});
    h.keys("cmd-a");
    let (note, editor, request) = h.app.update(h.cx, |app, cx| {
        let editor = app.editor();
        (
            app.notes.library.active_id.clone(),
            editor.downgrade(),
            editor
                .read(cx)
                .context_snapshot(Default::default(), ContextTarget::Text),
        )
    });
    assert!(h.app.update(h.cx, |app, cx| app.apply_service_replacement(
        &note,
        &editor,
        &request,
        TextReplacement::PreserveStyles("red blue".into()),
        cx,
    )));
    assert_eq!(h.markdown(), source);
    // Plain Services change every character's style even for identical text;
    // the asynchronous application boundary must not short-circuit this case.
    assert!(h.app.update(h.cx, |app, cx| app.apply_service_replacement(
        &note,
        &editor,
        &request,
        TextReplacement::PlainText("red blue".into()),
        cx,
    )));
    assert_eq!(h.markdown(), "**red blue**");
    h.app.read_with(h.cx, |app, cx| {
        let state = app.editor().read(cx).state();
        assert!(state.selection().is_empty(state.doc()));
    });
    h.assert_round_trip("ordinary Services inherit the first character style");
    assert!(!h.app.update(h.cx, |app, cx| app.apply_service_replacement(
        &note,
        &editor,
        &request,
        TextReplacement::PlainText("stale".into()),
        cx,
    )));
    h.keys("cmd-z");
    assert_eq!(h.markdown(), source);

    h.keys("cmd-a");
    let request = h.app.update(h.cx, |app, cx| {
        app.editor()
            .read(cx)
            .context_snapshot(Default::default(), ContextTarget::Text)
    });
    lock_note(&mut h);
    assert!(!h.app.update(h.cx, |app, cx| app.apply_service_replacement(
        &note,
        &editor,
        &request,
        TextReplacement::PlainText("red blue".into()),
        cx,
    )));
    assert_eq!(h.markdown(), source);
}

#[gpui::test]
fn readonly_transformations_are_disabled(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "hello world\n")], |_| {});
    h.select(1, 6);
    lock_note(&mut h);
    let id = open_menu(&mut h, ContextTarget::Text);
    let uppercase = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::Transform(markraft_gpui::TextTransformation::Uppercase)
        )
    });
    assert_eq!(state(&mut h, uppercase), (false, None));
    finish(&mut h, id, Some(uppercase));
    assert_eq!(h.markdown(), "hello world");
}

#[gpui::test]
fn paste_matching_style_uses_target_marks_and_persists(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "**word** tail\n")], |_| {});
    h.select(1, 9);
    h.cx.write_to_clipboard(ClipboardItem::new_string("new".into()));
    let id = open_menu(&mut h, ContextTarget::Text);
    let paste = item(&mut h, |action| {
        matches!(action, MenuAction::PasteMatchStyle)
    });
    assert_eq!(state(&mut h, paste), (true, None));
    finish(&mut h, id, Some(paste));
    assert_eq!(h.markdown(), "**new** tail");
    h.assert_round_trip("paste matching target style");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "**word** tail");
}

#[gpui::test]
fn smart_quote_preference_updates_pairing_and_roundtrips(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "word\n")], |_| {});
    let id = open_menu(&mut h, ContextTarget::Text);
    let quotes = item(&mut h, |action| {
        matches!(
            action,
            MenuAction::CheckSetting(crate::storage::TextCheckingSetting::Quotes)
        )
    });
    assert_eq!(state(&mut h, quotes), (true, Some(false)));
    finish(&mut h, id, Some(quotes));
    h.app.update(h.cx, |app, _| {
        assert!(app.preferences.text_checking.quotes);
        assert!(!app.quote_pairs.load(std::sync::atomic::Ordering::Relaxed));
    });
    let id = open_menu(&mut h, ContextTarget::Text);
    assert_eq!(state(&mut h, quotes), (true, Some(true)));
    finish(&mut h, id, Some(quotes));
    h.app.update(h.cx, |app, _| {
        assert!(!app.preferences.text_checking.quotes);
        assert!(app.quote_pairs.load(std::sync::atomic::Ordering::Relaxed));
    });
}

#[gpui::test]
fn prose_menu_follows_native_group_order_and_exposes_checking_panels(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "A simple sentence.\n")], |_| {});
    h.app.update(h.cx, |app, _| assert!(app.platform.is_none()));
    h.select(3, 9);
    let id = open_menu(&mut h, ContextTarget::Text);
    let menu = rows(&mut h);
    let labels = menu
        .iter()
        .filter_map(|row| match row {
            Row::Item { label, .. } | Row::Submenu { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            "Look Up “simple”",
            "Search the Web",
            "Cut",
            "Copy",
            "Paste",
            "Paste and Match Style",
            "Copy / Paste As",
            "Share…",
            "Paragraph",
            "Format",
            "Insert",
            "Spelling and Grammar",
            "Substitutions",
            "Transformations",
            "Speech"
        ]
    );
    assert!(
        row_state(
            &menu,
            item(&mut h, |action| matches!(
                action,
                MenuAction::CheckingPanel(super::CheckingPanel::Spelling)
            ))
        )
        .is_some()
    );
    assert!(
        row_state(
            &menu,
            item(&mut h, |action| matches!(
                action,
                MenuAction::CheckingPanel(super::CheckingPanel::Substitutions)
            ))
        )
        .is_some()
    );
    fn has_label(rows: &[Row], expected: &str) -> bool {
        rows.iter().any(|row| match row {
            Row::Item { label, .. } => label == expected,
            Row::Submenu { children, .. } => has_label(children, expected),
            _ => false,
        })
    }
    assert!(has_label(&menu, "Show Spelling and Grammar"));
    assert!(has_label(&menu, "Show Substitutions"));
    assert!(!has_label(&menu, "Hide Spelling and Grammar"));
    assert!(!has_label(&menu, "Hide Substitutions"));
    finish(&mut h, id, None);
}

#[gpui::test]
fn learned_spelling_can_be_reversed_without_offering_learn_again(cx: &mut TestAppContext) {
    let h = open_with(cx, &[("note.md", "Markrafttestword\n")], |_| {});
    h.app.update(h.cx, |app, cx| {
        let mut menu = super::MenuBuilder {
            app,
            cx,
            actions: Vec::new(),
        };
        // Headless tests do not use the user's system dictionary. Supply the
        // native learned state to the same builder that handles spelling results.
        let rows = menu.spelling_word("Markrafttestword", true);
        assert!(matches!(
            rows.as_slice(),
            [Row::Item { label, enabled: true, .. }] if label == "Unlearn Spelling"
        ));
        assert!(matches!(
            menu.actions.as_slice(),
            [MenuAction::UnlearnSpelling(word)] if word == "Markrafttestword"
        ));

        menu.actions.clear();
        let rows = menu.spelling_word("Markrafttestword", false);
        assert_eq!(rows.len(), 2);
        assert!(matches!(
            menu.actions.as_slice(),
            [MenuAction::IgnoreSpelling(ignored), MenuAction::LearnSpelling(learned)]
                if ignored == "Markrafttestword" && learned == ignored
        ));
    });
}

#[gpui::test]
fn markdown_context_formats_preserve_content_and_undo(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("note.md", "**Hello** *世界* [link](https://example.com)\n")],
        |_| {},
    );
    h.keys("cmd-a");
    let id = open_menu(&mut h, ContextTarget::Text);
    let clear = item(&mut h, |a| {
        matches!(
            a,
            MenuAction::Markdown(super::MarkdownAction::ClearFormatting)
        )
    });
    assert_eq!(state(&mut h, clear), (true, None));
    finish(&mut h, id, Some(clear));
    assert_eq!(h.markdown(), "Hello 世界 link");
    h.assert_round_trip("clearing all inline styles");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "**Hello** *世界* [link](https://example.com)");

    h.select(3, 8);
    let id = open_menu(&mut h, ContextTarget::Text);
    let highlight = item(&mut h, |a| {
        matches!(a, MenuAction::Intent(Intent::Mark(doc::Inline::Highlight)))
    });
    assert_eq!(state(&mut h, highlight), (true, Some(false)));
    finish(&mut h, id, Some(highlight));
    assert!(h.markdown().contains("=="));
    h.assert_round_trip("highlighting existing bold text");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "**Hello** *世界* [link](https://example.com)");
}

#[gpui::test]
fn paragraph_context_commands_and_hyperlink_are_available(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "alpha\n")], |_| {});
    h.select(1, 6);
    let id = open_menu(&mut h, ContextTarget::Text);
    let link = item(&mut h, |a| matches!(a, MenuAction::Intent(Intent::Link)));
    assert_eq!(state(&mut h, link), (true, None));
    let heading = item(&mut h, |a| {
        matches!(a, MenuAction::Intent(Intent::Block(doc::Block::Heading(2))))
    });
    finish(&mut h, id, Some(heading));
    assert_eq!(h.markdown(), "## alpha");
    h.assert_round_trip("a context heading change");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "alpha");
}

#[gpui::test]
fn copy_formats_in_menu_use_selection_and_remain_available_readonly(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "**Hello** 世界\n")], |_| {});
    lock_note(&mut h);
    h.keys("cmd-a");
    let id = open_menu(&mut h, ContextTarget::Text);
    let copy = item(&mut h, |a| {
        matches!(a, MenuAction::CopyAs(markraft_gpui::CopyFormat::PlainText))
    });
    assert_eq!(state(&mut h, copy), (true, None));
    let clear = item(&mut h, |a| {
        matches!(
            a,
            MenuAction::Markdown(super::MarkdownAction::ClearFormatting)
        )
    });
    let math = item(&mut h, |a| {
        matches!(a, MenuAction::Markdown(super::MarkdownAction::InsertMath))
    });
    let image = item(&mut h, |a| {
        matches!(a, MenuAction::Markdown(super::MarkdownAction::InsertImage))
    });
    let paragraph = item(&mut h, |a| {
        matches!(a, MenuAction::InsertParagraph { before: false })
    });
    let table = item(&mut h, |a| {
        matches!(a, MenuAction::Intent(Intent::InsertTable))
    });
    for action in [clear, math, image, paragraph, table] {
        assert!(!state(&mut h, action).0);
    }
    finish(&mut h, id, Some(copy));
    assert_eq!(clipboard(&mut h), "Hello 世界");
    assert_eq!(h.markdown(), "**Hello** 世界");
}

#[gpui::test]
fn insert_math_keeps_prose_and_places_typing_inside_delimiters(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "original\n")], |_| {});
    h.keys("cmd-up");
    let id = open_menu(&mut h, ContextTarget::Text);
    let math = item(&mut h, |a| {
        matches!(a, MenuAction::Markdown(super::MarkdownAction::InsertMath))
    });
    assert_eq!(state(&mut h, math), (true, None));
    finish(&mut h, id, Some(math));
    h.type_text("x^2");
    assert_eq!(h.markdown(), "original\n\n$$\nx^2\n$$");
    h.assert_round_trip("inserting a display formula");
    h.keys("cmd-z cmd-z");
    assert_eq!(h.markdown(), "original");
}

#[gpui::test]
fn insert_paragraph_after_code_preserves_code_and_uses_new_caret(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("note.md", "```rust\nlet x = 1;\n```\n")], |_| {});
    let id = open_menu(&mut h, ContextTarget::CodeBlock { pos: 0 });
    let after = item(&mut h, |a| {
        matches!(a, MenuAction::InsertParagraph { before: false })
    });
    assert_eq!(state(&mut h, after), (true, None));
    finish(&mut h, id, Some(after));
    h.type_text("after");
    assert_eq!(h.markdown(), "```rust\nlet x = 1;\n```\n\nafter");
    h.assert_round_trip("a paragraph after code");
    h.keys("cmd-z cmd-z");
    assert_eq!(h.markdown(), "```rust\nlet x = 1;\n```");
}

#[gpui::test]
fn full_document_selection_disables_math_and_can_copy_clicked_table(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("note.md", "| a | b |\n| - | - |\n| 1 | 2 |\n\ntail\n")],
        |_| {},
    );
    h.keys("cmd-a");
    let original = h.markdown();
    let id = open_menu(&mut h, ContextTarget::Table { pos: 0, cell: 3 });
    let math = item(&mut h, |a| {
        matches!(a, MenuAction::Markdown(super::MarkdownAction::InsertMath))
    });
    assert!(!state(&mut h, math).0);
    let copy = item(&mut h, |a| matches!(a, MenuAction::CopyTable(0)));
    assert!(state(&mut h, copy).0);
    finish(&mut h, id, Some(copy));
    let copied = clipboard(&mut h);
    let cells = copied
        .lines()
        .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(cells[0], ["", "a", "b", ""]);
    assert_eq!(cells[2], ["", "1", "2", ""]);
    assert!(!copied.contains("tail"));
    assert_eq!(h.markdown(), original);
}
