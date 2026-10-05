use crate::e2e::harness::open_with;
use gpui::{MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext};

#[gpui::test]
fn repeated_count_clicks_preserve_the_note_selection(cx: &mut TestAppContext) {
    let mut h = open_with(cx, &[("count.md", "First word\n\nLast word")], |_| {});

    for (anchor, head) in [(1, 1), (6, 1)] {
        h.select(anchor, head);
        let selection = h.selection();
        for click_count in 1..=3 {
            let position =
                h.cx.debug_bounds("word-count")
                    .expect("the rendered count button")
                    .center();
            let counted_words = h.app.update(h.cx, |app, _| app.toolbar.counts_words());

            h.cx.simulate_event(MouseDownEvent {
                position,
                button: MouseButton::Left,
                modifiers: Default::default(),
                click_count,
                first_mouse: false,
            });
            h.cx.run_until_parked();
            assert_eq!(
                h.selection(),
                selection,
                "count press {click_count} must preserve the note selection"
            );

            h.cx.simulate_event(MouseUpEvent {
                position,
                button: MouseButton::Left,
                modifiers: Default::default(),
                click_count,
            });
            h.cx.run_until_parked();
            assert_eq!(
                h.selection(),
                selection,
                "count click {click_count} must preserve the note selection"
            );
            assert_eq!(
                h.app.update(h.cx, |app, _| app.toolbar.counts_words()),
                !counted_words,
                "count click {click_count} must still switch the count mode"
            );
        }
    }
}
