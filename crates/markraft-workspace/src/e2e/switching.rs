//! Moving between notes: each keeps its own edits and its own undo.

use super::harness::open_with;
use gpui::TestAppContext;

// The note left is written, the one moved to shows what its file says, and
// ⌘Z in either reaches only that note's own edits.
#[gpui::test]
fn switching_notes_keeps_each_notes_edits_and_undo(cx: &mut TestAppContext) {
    let mut h = open_with(
        cx,
        &[("alpha.md", "alpha\n"), ("beta.md", "beta\n")],
        |_| {},
    );
    h.browse_to("alpha");
    assert_eq!(h.active_name(), "alpha.md");
    h.keys("cmd-down cmd-right");
    h.type_text("1");

    h.browse_to("beta");
    assert_eq!(h.active_name(), "beta.md");
    assert_eq!(h.markdown(), "beta");
    assert_eq!(
        h.wait_for_file("alpha.md", |text| text == "alpha1\n"),
        "alpha1\n"
    );
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "beta", "undo reached into the note left");
    h.keys("cmd-down cmd-right");
    h.type_text("2");

    h.browse_to("alpha");
    assert_eq!(h.markdown(), "alpha1");
    h.keys("cmd-z");
    assert_eq!(h.markdown(), "alpha");
    h.save();
    assert_eq!(
        h.wait_for_file("alpha.md", |text| text == "alpha\n"),
        "alpha\n"
    );
    assert_eq!(
        h.wait_for_file("beta.md", |text| text == "beta2\n"),
        "beta2\n"
    );
}
