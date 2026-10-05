use super::EditorSurface;
use crate::{EditorView, Setup};
use gpui::{HitboxBehavior, TestAppContext, point, px, size};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::kind::DocTypes;

fn setup() -> Setup {
    let schema = commonmark_schema();
    Setup::new(schema.clone())
        .types(DocTypes::from_schema_names(
            &schema,
            &commonmark_doc_type_names(),
        ))
        .extensions(commonmark_extensions(&schema))
        .doc(
            from_markdown(
                &schema,
                "- [ ] Pending task\n- [x] Finished task\n\nPlain text",
            )
            .unwrap(),
        )
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
