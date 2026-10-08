//! Math editing must reach the source-preserving save path through real bindings.

use super::harness::open_with;
use gpui::TestAppContext;

#[gpui::test]
fn math_fences_enter_finish_and_save_through_the_app(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("math.md", "Start\n\n$$\n")], |_| {});
    h.keys("cmd-down");
    h.keys("enter");
    assert_eq!(h.markdown(), "Start\n\n$$\n\n$$");
    h.save();
    assert!(
        h.wait_for_file("math.md", |text| text.contains("$$\n\n$$"))
            .contains("$$\n\n$$")
    );
    h.type_text("x^2");
    h.keys("enter");
    h.type_text("+ y^2");
    h.keys("cmd-enter");
    h.type_text("After");
    h.save();
    let expected = "Start\n\n$$\nx^2\n+ y^2\n$$\n\nAfter";
    assert_eq!(h.markdown(), expected);
    h.assert_round_trip("multiline math and finish editing");
    assert_eq!(
        h.wait_for_file("math.md", |text| text.trim_end() == expected)
            .trim_end(),
        expected
    );
}

#[gpui::test]
fn math_source_edits_undo_and_save_without_touching_other_bytes(cx: &mut TestAppContext) {
    let source =
        "---\ntitle: Math\n---\n\nInline $x^2$ and `literal $x$`.\n\n$$\n\\frac{1}{2}\n$$\n\nEnd\n";
    let mut h = open_with(cx, &[("existing.md", source)], |_| {});
    h.keys("cmd-up");
    h.keys("right right right right right right right right");
    h.type_text("y");
    h.keys("cmd-z");
    h.save();
    assert_eq!(
        h.wait_for_file("existing.md", |text| text == source),
        source
    );
}

#[gpui::test]
fn pasted_tex_stays_literal_inside_empty_and_nonempty_formulas(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("paste.md", "$$\n")], |_| {});
    h.keys("cmd-down enter");
    h.cx.write_to_clipboard(gpui::ClipboardItem::new_string(
        "\\frac{1}{2}\n+ \\text{中文}".into(),
    ));
    h.keys("cmd-v cmd-enter");
    h.type_text("After");
    let expected = "$$\n\\frac{1}{2}\n+ \\text{中文}\n$$\n\nAfter";
    assert_eq!(h.markdown(), expected);
    h.assert_round_trip("TeX paste remains inside formula");
    h.save();
    assert_eq!(
        h.wait_for_file("paste.md", |text| text.trim_end() == expected)
            .trim_end(),
        expected
    );
}
