use crate::e2e::harness::open_with;
use gpui::{MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext};

#[gpui::test]
fn hidden_count_keeps_its_mode_and_toolbar_controls_in_a_narrow_window(cx: &mut TestAppContext) {
    use crate::storage::Pref;
    use gpui::{px, size};

    let mut h = open_with(cx, &[("count.md", "First word\n\nLast word")], |_| {});
    let window = h.cx.update(|window, _| window.window_handle());
    h.cx.simulate_window_resize(window, size(px(360.), px(300.)));
    h.cx.run_until_parked();
    let count = h.cx.debug_bounds("word-count").expect("visible by default");
    h.cx.simulate_click(count.center(), Default::default());
    h.cx.run_until_parked();
    assert!(h.app.update(h.cx, |app, _| app.toolbar.counts_words()));
    let toggle = h.cx.debug_bounds("format-toolbar-toggle").unwrap();
    assert!(toggle.left() >= px(0.) && toggle.right() <= px(360.));

    h.set_preference(Pref::ShowWordCount(false));
    assert!(h.cx.debug_bounds("word-count").is_none());
    assert_eq!(h.cx.debug_bounds("format-toolbar-toggle"), Some(toggle));
    h.cx.simulate_click(toggle.center(), Default::default());
    h.cx.run_until_parked();
    assert!(h.app.update(h.cx, |app, _| app.toolbar.shown()));
    assert!(h.cx.debug_bounds("word-count").is_none());

    h.set_preference(Pref::ShowWordCount(true));
    assert!(
        h.cx.debug_bounds("word-count").is_none(),
        "the toolbar owns the center while open"
    );
    h.cx.simulate_click(toggle.center(), Default::default());
    h.cx.run_until_parked();
    assert!(!h.app.update(h.cx, |app, _| app.toolbar.shown()));
    assert!(h.app.update(h.cx, |app, _| app.toolbar.counts_words()));
    let restored = h.cx.debug_bounds("word-count").unwrap();
    assert!(restored.left() >= px(0.) && restored.right() < toggle.left());
}

#[gpui::test]
fn hidden_count_preference_survives_saving_and_reopening(cx: &mut TestAppContext) {
    use crate::storage::Pref;

    let saved = {
        let mut h = open_with(cx, &[("count.md", "A note")], |_| {});
        h.set_preference(Pref::ShowWordCount(false));
        let saved = h
            .saved_preferences(|p| !p.show_word_count)
            .expect("saved preferences");
        assert!(!saved.show_word_count);
        saved
    };
    let h = open_with(cx, &[("count.md", "A note")], |p| *p = saved);
    assert!(h.cx.debug_bounds("word-count").is_none());
    assert!(h.cx.debug_bounds("format-toolbar-toggle").is_some());
}

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
