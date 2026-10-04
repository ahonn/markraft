//! Context targets survive selection corrections without trusting stale geometry.
use super::*;
use crate::Setup;
use gpui::{Entity, MouseButton, TestAppContext, VisualTestContext};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
};
use markraft_core::kind::DocTypes;
use std::{cell::RefCell, rc::Rc};

fn setup(source: &str) -> Setup {
    let schema = commonmark_schema();
    Setup::new(schema.clone())
        .types(DocTypes::from_schema_names(
            &schema,
            &commonmark_doc_type_names(),
        ))
        .extensions(commonmark_extensions(&schema))
        .doc(from_markdown(&schema, source).unwrap())
}

fn right_click(
    view: &Entity<EditorView>,
    cx: &mut VisualTestContext,
    position: Point<Pixels>,
) -> ContextRequest {
    let received = Rc::new(RefCell::new(None));
    let seen = received.clone();
    let subscription = cx.update(|_, cx| {
        cx.subscribe(view, move |_, event, _| {
            if let EditorEvent::ContextMenuRequested(request) = event {
                *seen.borrow_mut() = Some(request.clone());
            }
        })
    });
    cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
    cx.run_until_parked();
    drop(subscription);
    let request = received.borrow().clone().expect("a context menu request");
    view.read_with(cx, |view, _| {
        assert!(view.context_is_current(&request));
        assert!(view.context_target_bookmark.is_none());
    });
    request
}

fn atom_point(view: &EditorView, label: &str) -> Point<Pixels> {
    let mut at = None;
    view.state.doc().descendants(&mut |node, pos, _, _| {
        if Some(node.type_id()) == view.types.wiki_link && wiki::wiki_link_target(node) == label {
            at = Some(pos);
        }
        true
    });
    let pos = at.expect("a folded wiki link");
    let (row, _) = view.row_at(pos).unwrap();
    row.rectangles(row.pos_to_offset(pos)..row.pos_to_offset(pos + 1), false)[0].center()
}

#[gpui::test]
fn wiki_context_selects_the_atom_and_keeps_its_actions_on_repeated_clicks(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|_, cx| EditorView::new(setup("before [[Menu Audit]] after"), cx));
    cx.run_until_parked();
    for _ in 0..2 {
        let point = view.read_with(cx, |view, _| atom_point(view, "Menu Audit"));
        let request = right_click(&view, cx, point);
        let ContextTarget::WikiLink { pos, target, embed } = request.target else {
            panic!("right click must retain the wiki target");
        };
        assert_eq!(target, "Menu Audit");
        assert!(!embed);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.state.selection(), &Selection::node(pos));
            assert!(view.wiki_link_at(pos).is_some());
            assert!(view.edit_capabilities(cx).copy);
        });
    }
}

#[gpui::test]
fn link_context_maps_past_a_wiki_source_that_folds_on_the_first_click(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| {
        EditorView::new(setup("[[Earlier]] [Example](https://example.com) tail"), cx)
    });
    view.update(cx, |view, cx| view.select_range(1, 1, cx));
    cx.run_until_parked();
    let (point, old_start) = view.read_with(cx, |view, _| {
        let projection = view.projection();
        let text = projection.line_text(0).unwrap();
        assert!(text.starts_with("[[Earlier]]"));
        let offset = text.find("Example").unwrap();
        let row = &view.frame.rows()[0];
        (
            row.rectangles(offset..offset + 1, false)[0].center(),
            row.offset_to_pos(offset),
        )
    });
    let request = right_click(&view, cx, point);
    let selected = request.selection_range();
    let ContextTarget::Link { url, range } = request.target else {
        panic!("the first click must retain the external link");
    };
    assert_eq!(url, "https://example.com");
    assert!(range.start < old_start - 1, "folding must move the target");
    view.read_with(cx, |view, _| {
        assert!(view.wiki_link_at(1).is_some());
        assert_eq!(view.active_link().as_deref(), Some("https://example.com"));
        assert_eq!(view.context_link_label(&range), Some(selected));
    });
}

#[gpui::test]
fn wiki_context_maps_past_another_atom_that_folds(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|_, cx| EditorView::new(setup("[[Earlier]] then [[Menu Audit]]"), cx));
    view.update(cx, |view, cx| view.select_range(1, 1, cx));
    cx.run_until_parked();
    let point = view.read_with(cx, |view, _| atom_point(view, "Menu Audit"));
    let request = right_click(&view, cx, point);
    let ContextTarget::WikiLink { pos, target, .. } = request.target else {
        panic!("the clicked wiki link must survive an earlier atom folding");
    };
    assert_eq!(target, "Menu Audit");
    view.read_with(cx, |view, _| {
        assert!(view.wiki_link_at(1).is_some());
        assert_eq!(view.state.selection(), &Selection::node(pos));
        assert_eq!(
            wiki::wiki_link_target(&view.wiki_link_at(pos).unwrap()),
            target
        );
    });
}

#[gpui::test]
fn context_discards_a_link_rewritten_by_a_pointer_extension(cx: &mut TestAppContext) {
    struct RewriteTarget(bool);
    impl crate::Extension for RewriteTarget {
        fn id(&self) -> &'static str {
            "rewrite-context-target"
        }
        fn update(&mut self, update: &crate::Update, cx: &mut crate::EditorCx<'_>) {
            if !self.0 && update.is_user_event(event::SELECT_POINTER) {
                self.0 = true;
                let state = cx.state();
                let replacement =
                    from_markdown(state.schema(), "[Other](https://other.example)").unwrap();
                let change = markraft_core::Change::replace(
                    0,
                    state.doc().content_size(),
                    markraft_core::Slice::from_fragment(replacement.content().clone()),
                );
                cx.dispatch([TransactionSpec::new().changes([change])]);
            }
        }
    }
    let (view, cx) =
        cx.add_window_view(|_, cx| EditorView::new(setup("[Example](https://example.com)"), cx));
    let _extension = view.update(cx, |view, cx| view.add_extension(RewriteTarget(false), cx));
    cx.run_until_parked();
    let point = view.read_with(cx, |view, _| {
        view.frame.rows()[0].rectangles(2..3, false)[0].center()
    });
    let request = right_click(&view, cx, point);
    assert_eq!(request.target, ContextTarget::Text);
    view.read_with(cx, |view, _| assert!(view.text().contains("Other")));
}
