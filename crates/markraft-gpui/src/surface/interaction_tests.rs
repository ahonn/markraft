use super::{EditorSurface, visible_strips};
use crate::{EditorView, Setup};
use gpui::{
    Bounds, Entity, HitboxBehavior, MouseButton, Pixels, TestAppContext, VisualTestContext, point,
    px, size,
};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::kind::DocTypes;

fn setup() -> Setup {
    setup_with("- [ ] Pending task\n- [x] Finished task\n\nPlain text")
}

fn setup_with(source: &str) -> Setup {
    let schema = commonmark_schema();
    Setup::new(schema.clone())
        .types(DocTypes::from_schema_names(
            &schema,
            &commonmark_doc_type_names(),
        ))
        .extensions(commonmark_extensions(&schema))
        .doc(from_markdown(&schema, source).unwrap())
}

/// Twenty columns of one unbreakable word each: wider than the window a test opens.
fn wide_table() -> String {
    let row = |cell: &str| format!("|{}\n", format!(" {cell} |").repeat(20));
    row(&"w".repeat(40)) + &row("---") + &row("x")
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

/// The one scrolling grid of the note: its key, and the part of it on screen.
fn scrolling_grid(
    editor: &Entity<EditorView>,
    cx: &mut VisualTestContext,
) -> (usize, Bounds<Pixels>) {
    editor.read_with(cx, |editor, _| {
        let frame = &editor.frame;
        let strips = visible_strips(frame.rows(), frame.tables(), frame.content_bounds());
        assert_eq!(strips.len(), 1);
        strips[0]
    })
}

#[gpui::test]
fn task_pointer_regions_cover_checkboxes_without_blocking_clicks(cx: &mut TestAppContext) {
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(), cx));
    cx.run_until_parked();
    let positions = editor.read_with(cx, |editor, _| {
        editor
            .frame
            .rows()
            .iter()
            .filter_map(|row| row.task_marker().map(|(_, bounds)| bounds.center()))
            .collect::<Vec<_>>()
    });
    assert_eq!(positions.len(), 2);
    for (index, position) in positions.into_iter().enumerate() {
        cx.simulate_mouse_move(position, None, Default::default());
        cx.simulate_click(position, Default::default());
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            assert_eq!(
                editor.frame.rows()[index].task_marker().unwrap().0,
                index == 0
            );
            assert!(!editor.selecting);
        });
    }

    // Inspect the actual regions registered by the surface, including their
    // content mask. Moving the surface above the viewport must clip them.
    for top in [px(0.), px(-200.)] {
        let (_, painted) = cx.draw(point(px(0.), top), size(px(400.), px(300.)), |_, _| {
            EditorSurface {
                editor: editor.clone(),
            }
        });
        assert_eq!(painted.task_hitboxes.len(), 2);
        for (index, hitbox) in painted.task_hitboxes.iter().enumerate() {
            let row = &painted.rows[index];
            assert_eq!(hitbox.bounds, row.task_marker().unwrap().1);
            assert_eq!(hitbox.behavior, HitboxBehavior::Normal);
            assert!(!hitbox.bounds.contains(&row.origin));
            assert_eq!(
                hitbox.content_mask.bounds.contains(&hitbox.bounds.center()),
                top == px(0.),
            );
        }
    }
}

#[gpui::test]
fn dragging_the_thumb_under_a_wide_table_scrolls_it(cx: &mut TestAppContext) {
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(setup_with(&wide_table()), cx));
    cx.run_until_parked();
    let (table, strip) = scrolling_grid(&editor, cx);
    let grid = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |editor, _| editor.frame.tables()[&table])
    };
    let overflow = grid(cx).overflow;
    assert!(overflow > px(0.));
    let selection = editor.read_with(cx, |editor, _| editor.state().selection().clone());

    // The thumb stands under the grid, at the start of its track.
    let on_thumb = point(strip.left() + px(20.), strip.bottom() + px(4.));
    cx.simulate_mouse_move(on_thumb, None, Default::default());
    draw(cx);
    cx.simulate_mouse_down(on_thumb, MouseButton::Left, Default::default());
    draw(cx);
    assert_eq!(grid(cx).offset, px(0.), "a press alone scrolls nothing");

    let end = point(strip.right() + px(500.), on_thumb.y);
    cx.simulate_mouse_move(end, MouseButton::Left, Default::default());
    draw(cx);
    assert_eq!(grid(cx).offset, overflow);
    cx.simulate_mouse_move(
        point(strip.left() - px(500.), on_thumb.y),
        MouseButton::Left,
        Default::default(),
    );
    draw(cx);
    assert_eq!(grid(cx).offset, px(0.));

    cx.simulate_mouse_up(on_thumb, MouseButton::Left, Default::default());
    draw(cx);
    editor.read_with(cx, |editor, _| {
        assert!(!editor.selecting, "the press belongs to the thumb");
        assert_eq!(*editor.state().selection(), selection);
    });
}
