//! Daily notes: today's made where it belongs and found again rather than made twice,
//! the days either side of one, what bringing the window back opens, and what a folder
//! that is also an Obsidian vault says about them.

use super::harness::{Harness, open, open_with};
use crate::daily::DailySettings;
use crate::storage::{Pref, Summon};
use chrono::NaiveDate;
use gpui::TestAppContext;
use std::path::PathBuf;

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn stamp(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

/// Run the ⌘K command that `query` finds first.
fn run_command(h: &mut Harness, query: &str) {
    h.keys("cmd-k");
    h.type_text(query);
    h.keys("enter");
    h.wait_for_io();
}

/// What the ⌘K panel offers while nothing is typed.
fn commands(h: &mut Harness) -> Vec<String> {
    h.keys("cmd-k");
    let labels = h
        .app
        .update(h.cx, |app, cx| app.test_action_labels(cx))
        .expect("the actions panel");
    h.keys("escape");
    labels
}

fn set_daily(h: &mut Harness, daily: DailySettings) {
    h.app.update(h.cx, |app, _| app.test_set_daily(daily));
}

/// The active note's file, relative to the notes folder.
fn active_file(h: &mut Harness) -> String {
    let path = h.active_note().path.expect("a note with a file");
    path.strip_prefix(&h.notes)
        .expect("a note in the folder")
        .display()
        .to_string()
}

fn note_count(h: &mut Harness) -> usize {
    h.app.update(h.cx, |app, _| app.test_note_count())
}

// Today's note is a file as soon as it is asked for, named as the default format
// names it, and asking again opens that one rather than a second.
#[gpui::test]
fn todays_note_is_made_once_and_found_again(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("alpha.md", "alpha\n")], |_| {});
    run_command(&mut h, "today");
    let name = format!("{}.md", stamp(today()));
    assert_eq!(active_file(&mut h), name);
    assert_eq!(h.files(), [name.clone(), "alpha.md".to_owned()]);
    assert_eq!(std::fs::read_to_string(h.notes.join(&name)).unwrap(), "");

    h.browse_to("alpha");
    let notes = note_count(&mut h);
    run_command(&mut h, "today");
    assert_eq!(active_file(&mut h), name);
    assert_eq!(note_count(&mut h), notes);
    // The file was empty, so it keeps having no line break at its end.
    h.type_text("1");
    assert_eq!(h.wait_for_file(&name, |text| text == "1"), "1");
}

// A new day starts from the template with its placeholders filled in for that day,
// in the folder the format names, with the caret after what the template wrote.
#[gpui::test]
fn a_new_day_starts_from_the_template_in_its_folder(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[(
            "Templates/Daily.md",
            "# {{date:YYYY}}\n\nAfter [[{{yesterday}}]]\n",
        )],
        |_| {},
    );
    set_daily(
        &mut h,
        DailySettings {
            folder: PathBuf::from("Journal"),
            format: "YYYY/YYYY-MM-DD".to_owned(),
            template: Some(PathBuf::from("Templates/Daily.md")),
        },
    );
    run_command(&mut h, "today");
    let day = today();
    let path = format!("Journal/{}/{}.md", day.format("%Y"), stamp(day));
    let expected = format!(
        "# {}\n\nAfter [[{}]]\n",
        day.format("%Y"),
        stamp(day.pred_opt().unwrap())
    );
    assert_eq!(h.wait_for_file(&path, |text| text == expected), expected);
    assert_eq!(active_file(&mut h), path);
    h.type_text(" 2");
    assert!(h.markdown().ends_with("]] 2"), "{}", h.markdown());

    // A template that is gone leaves the day empty, and says so.
    set_daily(
        &mut h,
        DailySettings {
            template: Some(PathBuf::from("Templates/Gone.md")),
            ..DailySettings::default()
        },
    );
    run_command(&mut h, "today");
    let name = format!("{}.md", stamp(day));
    assert_eq!(active_file(&mut h), name);
    assert_eq!(std::fs::read_to_string(h.notes.join(&name)).unwrap(), "");
    assert!(
        h.notices().iter().any(|notice| notice.contains("Gone")),
        "{:?}",
        h.notices()
    );
}

// Another program that makes today's note first — after the folder was read, before
// the watcher reports it — keeps its file: it is opened as it is, and the watcher
// finding it afterwards adds no second note.
#[gpui::test]
fn a_note_another_program_made_for_today_is_opened_as_it_is(cx: &mut TestAppContext) {
    let mut h = open(cx, |_| {});
    let name = format!("{}.md", stamp(today()));
    std::fs::write(h.notes.join(&name), "written elsewhere\n").unwrap();
    run_command(&mut h, "today");
    assert_eq!(active_file(&mut h), name);
    assert_eq!(h.markdown(), "written elsewhere");
    let notes = note_count(&mut h);

    // ⌘S goes through the same queue after the refresh, so once it is back the
    // refresh has been reported; the poll after it takes the report in.
    h.refresh_files();
    h.save();
    h.pass_time(std::time::Duration::from_millis(200));
    assert_eq!(note_count(&mut h), notes);
    assert_eq!(
        std::fs::read_to_string(h.notes.join(&name)).unwrap(),
        "written elsewhere\n"
    );
    assert_eq!(h.files(), [name]);
}

// From a daily note, the neighbouring days are the nearest ones that have a note; a
// note without a title of its own goes by its file's name.
#[gpui::test]
fn previous_and_next_step_over_days_without_a_note(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[
            ("2026-09-23.md", "a\n"),
            ("2026-09-25.md", ""),
            ("2026-09-28.md", "c\n"),
            ("other.md", "other\n"),
        ],
        |_| {},
    );
    let latest = h.notes.join("2026-09-28.md");
    h.open_path(&latest);
    let offered = commands(&mut h);
    assert!(offered.iter().any(|label| label == "Previous Daily Note"));
    assert!(!offered.iter().any(|label| label == "Next Daily Note"));

    run_command(&mut h, "previous daily");
    assert_eq!(active_file(&mut h), "2026-09-25.md");
    assert_eq!(
        h.active_note()
            .display_title(&crate::locale::I18n::english()),
        "2026-09-25"
    );
    let offered = commands(&mut h);
    assert!(offered.iter().any(|label| label == "Previous Daily Note"));
    assert!(offered.iter().any(|label| label == "Next Daily Note"));

    run_command(&mut h, "next daily");
    assert_eq!(active_file(&mut h), "2026-09-28.md");

    h.browse_to("other");
    let offered = commands(&mut h);
    assert!(
        !offered
            .iter()
            .any(|label| label.ends_with("Daily Note") && label != "Open Today’s Daily Note")
    );
}

// Brought back from hiding with "New Note", a blank daily note that is already a file
// is left alone and a new note opens; with "Today's Daily Note", today's opens.
#[gpui::test]
fn show_on_open_chooses_between_a_new_note_and_todays(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("alpha.md", "alpha\n")], |_| {});
    run_command(&mut h, "today");
    let today_file = format!("{}.md", stamp(today()));
    assert_eq!(active_file(&mut h), today_file);

    h.set_preference(Pref::Summon(Summon::NewNote));
    let summon = |h: &mut Harness| {
        let app = h.app.clone();
        h.cx.update(|window, cx| app.update(cx, |app, cx| app.test_summon(window, cx)));
        h.wait_for_io();
    };
    let notes = note_count(&mut h);
    summon(&mut h);
    assert_eq!(
        h.active_note().path,
        None,
        "the blank daily note was reused"
    );
    assert_eq!(note_count(&mut h), notes + 1);
    // A new note still blank and not yet a file is the fresh page already.
    summon(&mut h);
    assert_eq!(note_count(&mut h), notes + 1);

    h.set_preference(Pref::Summon(Summon::DailyNote));
    h.browse_to("alpha");
    summon(&mut h);
    assert_eq!(active_file(&mut h), today_file);
}

// A folder that is also an Obsidian vault offers the daily note settings the vault
// keeps, its template found among the notes; one whose format cannot name days here
// offers nothing.
#[gpui::test]
fn an_obsidian_vault_offers_its_daily_settings(cx: &mut TestAppContext) {
    let h = open_with(
        cx,
        &[
            (
                ".obsidian/daily-notes.json",
                r#"{"folder": "Journal", "format": "YYYY/YYYY-MM-DD", "template": "Daily"}"#,
            ),
            ("Templates/Daily.md", "# {{date}}\n"),
        ],
        |_| {},
    );
    assert_eq!(
        h.app.update(h.cx, |app, _| app.test_obsidian_daily()),
        Some(DailySettings {
            folder: PathBuf::from("Journal"),
            format: "YYYY/YYYY-MM-DD".to_owned(),
            template: Some(PathBuf::from("Templates/Daily.md")),
        })
    );
    std::fs::write(
        h.notes.join(".obsidian/daily-notes.json"),
        r#"{"format": "YYYY-MM"}"#,
    )
    .unwrap();
    assert_eq!(h.app.update(h.cx, |app, _| app.test_obsidian_daily()), None);
}
