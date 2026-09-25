use super::{EXTENSION_ORIGIN_PREFIX, EditorCx, Extension, ExtensionHandle, ROUNDS, Update};
use crate::{DocTypes, EditorView, Setup};
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown, to_markdown,
};
use markraft_core::commands::insert_text;
use markraft_core::{Change, Fragment, Selection, Slice, TransactionSpec};
use std::{cell::RefCell, rc::Rc};

fn setup(markdown: &str) -> Setup {
    let schema = commonmark_schema();
    let doc = from_markdown(&schema, markdown).expect("valid Markdown");
    Setup::new(schema.clone())
        .types(DocTypes::from_schema_names(
            &schema,
            &commonmark_doc_type_names(),
        ))
        .extensions(commonmark_extensions(&schema))
        .doc(doc)
}

fn markdown(view: &EditorView) -> String {
    to_markdown(view.state().schema(), view.state().doc())
}

/// What one extension saw of one round.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    origin: Option<String>,
    changed: bool,
    typed: bool,
}

/// Appends `suffix` to the document whenever an edit it did not make itself
/// arrives, and records every round it is given.
struct Echo {
    id: &'static str,
    suffix: &'static str,
    seen: Rc<RefCell<Vec<Seen>>>,
}

impl Echo {
    fn new(id: &'static str, suffix: &'static str) -> (Self, Rc<RefCell<Vec<Seen>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let echo = Echo {
            id,
            suffix,
            seen: seen.clone(),
        };
        (echo, seen)
    }
}

impl Extension for Echo {
    fn id(&self) -> &'static str {
        self.id
    }
    fn update(&mut self, update: &Update, cx: &mut EditorCx<'_>) {
        self.seen.borrow_mut().push(Seen {
            origin: update.origin().map(str::to_owned),
            changed: update.changed(),
            typed: update.is_user_event("input.type"),
        });
        let own = format!("{EXTENSION_ORIGIN_PREFIX}{}", self.id);
        if update.changed() && update.origin() != Some(own.as_str()) {
            let spec = append(cx.state(), self.suffix);
            cx.dispatch([spec]);
        }
    }
}

/// Insert `text` at the end of the last paragraph, with no user event.
fn append(state: &markraft_core::EditorState, text: &str) -> TransactionSpec {
    let end = state.doc().content_size() - 1;
    let text = state.schema().text(text);
    TransactionSpec::new().changes([Change::insert(
        end,
        Slice::from_fragment(Fragment::from_node(text)),
    )])
}

fn editor(cx: &mut TestAppContext, markdown: &str) -> Entity<EditorView> {
    cx.new(|cx| EditorView::new(setup(markdown), cx))
}

fn register(
    cx: &mut TestAppContext,
    view: &Entity<EditorView>,
    extension: impl Extension,
) -> ExtensionHandle {
    view.update(cx, |view, cx| view.add_extension(extension, cx))
}

fn type_at_end(cx: &mut TestAppContext, view: &Entity<EditorView>, text: &str) {
    view.update(cx, |view, cx| {
        let end = view.state().doc().content_size() - 1;
        // `dispatch` answers whether the document changed; a caret move does not.
        view.dispatch(
            [TransactionSpec::new().selection(Selection::cursor(end))],
            cx,
        );
        assert!(view.run_command(&insert_text(text), cx));
    });
}

#[gpui::test]
fn an_extension_follow_up_edit_is_applied_and_the_rounds_settle(cx: &mut TestAppContext) {
    let view = editor(cx, "one");
    let (echo, seen) = Echo::new("echo", "!");
    let _handle = register(cx, &view, echo);
    // Registering runs one empty round.
    assert_eq!(seen.borrow().len(), 1);
    seen.borrow_mut().clear();

    type_at_end(cx, &view, "x");

    view.read_with(cx, |view, _| assert_eq!(markdown(view), "onex!"));
    // The caret move, the typed text, then the echo's own edit, which it
    // declines to answer, so nothing further is queued.
    let seen = seen.borrow();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(!seen[0].changed);
    assert!(seen[1].changed && seen[1].typed);
    assert!(seen[2].changed);
}

#[gpui::test]
fn an_extension_sees_its_own_follow_up_tagged_with_its_origin_not_as_typing(
    cx: &mut TestAppContext,
) {
    let view = editor(cx, "one");
    let (echo, seen) = Echo::new("echo", "!");
    let _handle = register(cx, &view, echo);
    seen.borrow_mut().clear();

    type_at_end(cx, &view, "x");

    let seen = seen.borrow();
    let typed = &seen[1];
    assert!(
        typed
            .origin
            .as_deref()
            .is_none_or(|origin| !origin.starts_with(EXTENSION_ORIGIN_PREFIX)),
        "{typed:?}"
    );
    // The follow-up does come back as a further round, but it carries the
    // extension's origin and is not the user typing.
    assert_eq!(
        seen[2],
        Seen {
            origin: Some(format!("{EXTENSION_ORIGIN_PREFIX}echo")),
            changed: true,
            typed: false,
        }
    );
}

#[gpui::test]
fn extensions_that_keep_answering_each_other_stop_after_the_round_budget(cx: &mut TestAppContext) {
    let view = editor(cx, "one");
    let (ping, pings) = Echo::new("ping", "a");
    let (pong, pongs) = Echo::new("pong", "b");
    let _ping = register(cx, &view, ping);
    let _pong = register(cx, &view, pong);
    pings.borrow_mut().clear();
    pongs.borrow_mut().clear();

    // A bare edit with no caret move first, so the typed text is the only
    // update that starts the rounds.
    view.update(cx, |view, cx| {
        let spec = append(view.state(), "x");
        assert!(view.dispatch([spec], cx));
    });

    // Each extension is consulted once per round and the budget ends it.
    assert_eq!(pings.borrow().len(), ROUNDS);
    assert_eq!(pongs.borrow().len(), ROUNDS);
    // The first round answers the edit twice; every later round carries one
    // extension's edit, which the other answers once.
    let answers = ROUNDS + 1;
    view.read_with(cx, |view, _| {
        let text = markdown(view);
        let tail = text
            .trim_end()
            .strip_prefix("onex")
            .expect("the typed text");
        assert_eq!(tail.chars().count(), answers, "{text:?}");
    });
}

// `EditorCx::move_visual_rows`, which modal extensions use for j and k, and
// the ↓ key (`EditorView::vertical`) decide a move in one place
// (`EditorView::vertical_target`), so at a block's lower edge vim's j does
// what the arrow does: leave a final code block or table for a new block, and
// go to the document's end from its last row.
//
// The test platform lays text out with a placeholder text system, so
// positions and rows exist but no pixel is asserted on here.

fn laid_out<'a>(
    cx: &'a mut TestAppContext,
    markdown: &str,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(setup(markdown), cx));
    cx.run_until_parked();
    (view, cx)
}

/// The start of the projected line holding exactly `text`.
fn line_start(view: &EditorView, text: &str) -> usize {
    let projection = view.projection();
    (0..projection.line_count())
        .find(|&line| projection.line_text(line) == Some(text))
        .and_then(|line| projection.line(line))
        .unwrap_or_else(|| panic!("no line reads {text:?}"))
        .from()
}

fn place_caret(view: &Entity<EditorView>, cx: &mut VisualTestContext, pos: usize) {
    view.update(cx, |view, cx| {
        view.dispatch(
            [TransactionSpec::new().selection(Selection::cursor(pos))],
            cx,
        );
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(!view.frame.rows().is_empty(), "no layout was painted")
    });
}

fn vim_row_down(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> bool {
    let moved = view.update(cx, |view, cx| {
        EditorCx::new(view, "vim", Some(cx)).move_visual_rows(1, false)
    });
    cx.run_until_parked();
    moved
}

fn arrow_down(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |view, cx| view.vertical(1, false, cx));
    cx.run_until_parked();
}

fn caret(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |view, _| view.head())
}

fn text(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |view, _| markdown(view))
}

/// How many top-level blocks the document holds. An empty trailing paragraph
/// writes no Markdown, so the source alone cannot show one was added.
fn blocks(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |view, _| view.state().doc().child_count())
}

/// The caret and block count after `down` from `pos` in a fresh view of `source`.
fn after_down(
    cx: &mut TestAppContext,
    source: &str,
    pos: impl Fn(&EditorView) -> usize,
    down: fn(&Entity<EditorView>, &mut VisualTestContext),
) -> (usize, usize, String) {
    let (view, cx) = laid_out(cx, source);
    let at = view.read_with(cx, |view, _| pos(view));
    place_caret(&view, cx, at);
    down(&view, cx);
    (caret(&view, cx), blocks(&view, cx), text(&view, cx))
}

fn vim_down(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    assert!(vim_row_down(view, cx));
}

#[gpui::test]
fn vim_rows_leave_a_final_code_block_as_the_arrow_does(cx: &mut TestAppContext) {
    let source = "```\ncode\n```";
    let end = |view: &EditorView| line_start(view, "code") + "code".len();
    let vim = after_down(cx, source, end, vim_down);
    let arrow = after_down(cx, source, end, arrow_down);
    assert_eq!(vim, arrow);
    assert_eq!(vim.1, 2, "both add a block after the code block");
}

#[gpui::test]
fn vim_rows_leave_a_final_table_as_the_arrow_does(cx: &mut TestAppContext) {
    let source = "| a | b |\n| - | - |\n| c | d |";
    let cell = |view: &EditorView| line_start(view, "c");
    let vim = after_down(cx, source, cell, vim_down);
    let arrow = after_down(cx, source, cell, arrow_down);
    assert_eq!(vim, arrow);
    assert_eq!(vim.1, 2, "both add a block after the table");
}

#[gpui::test]
fn vim_rows_go_to_the_document_end_from_the_last_row_as_the_arrow_does(cx: &mut TestAppContext) {
    let inside = |view: &EditorView| line_start(view, "two") + 1;
    let vim = after_down(cx, "one\n\ntwo", inside, vim_down);
    let arrow = after_down(cx, "one\n\ntwo", inside, arrow_down);
    assert_eq!(vim, arrow);
    let (view, cx) = laid_out(cx, "one\n\ntwo");
    let end = view.read_with(cx, |view, _| view.state().doc().content_size() - 1);
    assert_eq!(vim.0, end);
}

#[gpui::test]
fn vim_rows_refuse_to_move_before_the_first_paint(cx: &mut TestAppContext) {
    let view = editor(cx, "one\n\ntwo");
    let moved = view.update(cx, |view, cx| {
        EditorCx::new(view, "vim", Some(cx)).move_visual_rows(1, false)
    });
    assert!(!moved);
}

#[gpui::test]
fn vim_rows_refuse_to_move_while_composing(cx: &mut TestAppContext) {
    let (view, cx) = laid_out(cx, "one\n\ntwo");
    let start = view.read_with(cx, |view, _| line_start(view, "one"));
    place_caret(&view, cx, start);
    view.update(cx, |view, cx| {
        let spec = markraft_core::composition::update_composition(view.state(), "中", 1)
            .expect("a composition");
        assert!(view.dispatch([spec], cx));
        assert!(view.is_composing());
    });
    let head = caret(&view, cx);
    assert!(!vim_row_down(&view, cx));
    assert_eq!(caret(&view, cx), head);
}
