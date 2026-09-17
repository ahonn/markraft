//! A trigger-character typeahead: a menu that opens on a trigger such as `/` and
//! filters as the query is typed.
//!
//! Everything but the selected row and a dismissed trigger is derived from the
//! committed document on every update, so the menu closes by itself on blur, on a
//! caret move, and on any edit that breaks the match.

use crate::{
    ActionHandler, EditorCx, Extension, Overlay, Update, completion::CompletionList,
    extension::OVERLAY_MARGIN,
};
use gpui::*;
use markraft_core::{Affinity, BlockKind, Change, Document, Position, Transaction};
use std::{any::Any, cell::RefCell, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    markraft_typeahead,
    [
        TypeaheadPrev,
        TypeaheadNext,
        TypeaheadAccept,
        TypeaheadDismiss
    ]
);

/// Registered after the editor's own bindings so that, at the same context depth, these
/// win — but only while the `typeahead` identifier is in the editor's key context, which
/// happens only while a menu is open. When it is closed every key behaves as before.
pub(crate) fn bind_keys(cx: &mut App) {
    const CONTEXT: Option<&str> = Some("Markraft && typeahead");
    cx.bind_keys([
        KeyBinding::new("up", TypeaheadPrev, CONTEXT),
        KeyBinding::new("down", TypeaheadNext, CONTEXT),
        KeyBinding::new("enter", TypeaheadAccept, CONTEXT),
        KeyBinding::new("tab", TypeaheadAccept, CONTEXT),
        KeyBinding::new("escape", TypeaheadDismiss, CONTEXT),
    ]);
}

const WIDTH: Pixels = px(248.);
const MAX_HEIGHT: Pixels = px(248.);
const MIN_HEIGHT: Pixels = px(72.);
const GAP: Pixels = px(4.);
/// The popup's own border, which the list's height does not include.
const CHROME: Pixels = px(2.);

/// How tall the list may be so that the popup fits on the roomier side of the trigger's
/// line instead of covering it. The window is small and may follow the note's height.
/// `caret` comes from the last painted frame, which is close enough for sizing; the
/// editor places the popup from the current one.
fn list_height(caret: Option<Bounds<Pixels>>, viewport_height: Pixels) -> Pixels {
    let room = caret.map_or(viewport_height - px(96.), |caret| {
        let below = viewport_height - caret.bottom();
        let above = caret.top();
        below.max(above) - GAP - OVERLAY_MARGIN - CHROME
    });
    room.clamp(MIN_HEIGHT, MAX_HEIGHT)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeaheadItem {
    /// Identifies the item to the provider that produced it.
    pub id: SharedString,
    pub label: SharedString,
    /// Right-aligned muted text, such as a keyboard shortcut.
    pub hint: SharedString,
    /// A short leading glyph, such as an emoji.
    pub glyph: SharedString,
}

impl TypeaheadItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: SharedString::default(),
            glyph: SharedString::default(),
        }
    }
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn glyph(mut self, glyph: impl Into<SharedString>) -> Self {
        self.glyph = glyph.into();
        self
    }
}

pub trait TypeaheadProvider: 'static {
    /// Items for `query`, the text between the trigger and the caret. An empty list
    /// keeps the menu closed, so the keys it would take keep their usual meaning.
    fn items(&self, query: &str) -> Vec<TypeaheadItem>;
    /// An element drawn before the label in place of [`TypeaheadItem::glyph`], such as
    /// an icon only the host can draw. `color` is the row's text color.
    fn leading(&self, _item: &TypeaheadItem, _color: Hsla) -> Option<AnyElement> {
        None
    }
    /// Apply `item` inside the accepting transaction, after the trigger text has been
    /// deleted and the caret left where the trigger stood. The returned value, if any,
    /// reaches the host as [`crate::EditorEvent::Extension`] once the edit has landed.
    fn accept(&self, item: &TypeaheadItem, tx: &mut Transaction<'_>) -> Option<Rc<dyn Any>>;
}

/// The trigger run the caret sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TriggerMatch {
    /// The trigger character itself.
    pub trigger: Range<Position>,
    /// The text between the trigger and the caret.
    pub query: String,
}

/// The trigger run ending at `caret`: the word the caret is in must start with a
/// trigger. That is the same as scanning back to a trigger with no whitespace between
/// it and the caret, where the trigger itself opens the block or follows whitespace, so
/// a later trigger inside the run stays part of the query. A code block never matches.
/// Callers additionally require a collapsed selection and no live composition.
pub(crate) fn trigger_match(
    document: &Document,
    caret: Position,
    triggers: &[char],
) -> Option<TriggerMatch> {
    let caret = document.clamp_position(caret);
    let block = document.blocks.get(caret.block)?;
    if matches!(block.kind, BlockKind::Code { .. }) {
        return None;
    }
    let text = block.text();
    let before = text.get(..caret.byte)?;
    let (start, trigger) = before
        .grapheme_indices(true)
        .rev()
        .take_while(|(_, grapheme)| !grapheme.chars().any(char::is_whitespace))
        .last()?;
    if !is_trigger(trigger, triggers) {
        return None;
    }
    let at = |byte| Position {
        block: caret.block,
        byte,
    };
    Some(TriggerMatch {
        trigger: at(start)..at(start + trigger.len()),
        query: before[start + trigger.len()..].to_owned(),
    })
}

/// A trigger is one whole grapheme, so a character carrying a combining mark is text.
fn is_trigger(grapheme: &str, triggers: &[char]) -> bool {
    let mut chars = grapheme.chars();
    chars
        .next()
        .is_some_and(|first| triggers.contains(&first) && chars.next().is_none())
}

/// Follow a dismissed trigger through `change`. Both ends take [`Affinity::Before`], so
/// text typed after the trigger leaves it alone. It is forgotten once the text it stood
/// on is gone: the map drops an end, or the two ends meet.
fn track_dismissed(
    dismissed: &Range<Position>,
    change: &Change,
    after: &Document,
) -> Option<Range<Position>> {
    let start = change
        .mapping
        .map_tracked(dismissed.start, Affinity::Before)?;
    let end = change
        .mapping
        .map_tracked(dismissed.end, Affinity::Before)?;
    let (start, end) = (after.clamp_position(start), after.clamp_position(end));
    (start != end).then_some(start..end)
}

#[derive(Default)]
struct State {
    /// The open menu, or `None` while it must stay closed.
    open: Option<Open>,
    selected: usize,
    /// The trigger character a user dismissed with escape, tracked through later
    /// changes so that deleting it, or typing another one, opens the menu again.
    dismissed: Option<Range<Position>>,
}

struct Open {
    trigger: Range<Position>,
    query: String,
    items: Vec<TypeaheadItem>,
}

/// A typeahead over `triggers`. Several instances may share one editor; each owns its
/// trigger set, its provider and its own `Origin::Extension(id)`.
pub struct Typeahead {
    id: &'static str,
    triggers: Vec<char>,
    provider: Rc<dyn TypeaheadProvider>,
    state: Rc<RefCell<State>>,
    scroll: ScrollHandle,
}

impl Typeahead {
    /// With a Chinese input method `/` types `、` and `:` types `：`, so a menu that
    /// should open under one usually configures both.
    pub fn new(id: &'static str, triggers: Vec<char>, provider: impl TypeaheadProvider) -> Self {
        Self {
            id,
            triggers,
            provider: Rc::new(provider),
            state: Rc::default(),
            scroll: ScrollHandle::new(),
        }
    }
}

impl Extension for Typeahead {
    fn id(&self) -> &'static str {
        self.id
    }

    fn key_context(&self, context: &mut KeyContext) {
        if self.state.borrow().open.is_some() {
            context.add("typeahead");
        }
    }

    fn update(&mut self, update: &Update, cx: &mut EditorCx<'_>) {
        let mut state = self.state.borrow_mut();
        if update.replaced {
            state.dismissed = None;
        } else if let (Some(change), Some(dismissed)) = (&update.change, state.dismissed.clone()) {
            state.dismissed = track_dismissed(&dismissed, change, cx.committed_document());
        }
        let before = state
            .open
            .as_ref()
            .map(|open| (open.trigger.clone(), open.query.clone()));
        state.open = None;
        let found = (cx.is_focused() && !cx.is_composing() && cx.selection().is_empty())
            .then(|| trigger_match(cx.committed_document(), cx.selection().head, &self.triggers))
            .flatten();
        if let Some(found) = found {
            if state.dismissed.as_ref() == Some(&found.trigger) {
                // Dismissed: stays closed until this trigger goes or another is typed.
            } else {
                state.dismissed = None;
                let items = self.provider.items(&found.query);
                if !items.is_empty() {
                    state.open = Some(Open {
                        trigger: found.trigger,
                        query: found.query,
                        items,
                    });
                }
            }
        }
        if state
            .open
            .as_ref()
            .map(|open| (open.trigger.clone(), open.query.clone()))
            != before
        {
            state.selected = 0;
            self.scroll.scroll_to_item(0);
            cx.notify();
        }
    }

    fn actions(&self) -> Vec<ActionHandler> {
        let step = |delta: isize| {
            let state = self.state.clone();
            let scroll = self.scroll.clone();
            move |cx: &mut EditorCx<'_>| {
                let mut state = state.borrow_mut();
                let Some(last) = state.open.as_ref().map(|open| open.items.len() - 1) else {
                    return;
                };
                // Clamped rather than wrapping, like the app's other pickers.
                state.selected = state.selected.saturating_add_signed(delta).min(last);
                scroll.scroll_to_item(state.selected);
                drop(state);
                cx.notify();
            }
        };
        let accepting = self.state.clone();
        let provider = self.provider.clone();
        let dismissing = self.state.clone();
        vec![
            ActionHandler::new(TypeaheadPrev, step(-1)),
            ActionHandler::new(TypeaheadNext, step(1)),
            ActionHandler::new(TypeaheadAccept, move |cx: &mut EditorCx<'_>| {
                accept(&accepting, &provider, cx);
            }),
            ActionHandler::new(TypeaheadDismiss, move |cx: &mut EditorCx<'_>| {
                let mut state = dismissing.borrow_mut();
                if let Some(open) = state.open.take() {
                    state.dismissed = Some(open.trigger);
                    state.selected = 0;
                    cx.notify();
                }
            }),
        ]
    }

    fn overlay(&mut self, cx: &EditorCx<'_>, window: &mut Window, _: &mut App) -> Option<Overlay> {
        let state = self.state.borrow();
        let open = state.open.as_ref()?;
        let viewport = window.viewport_size();
        let hovering = self.state.clone();
        let activating = self.state.clone();
        let element = CompletionList {
            style: cx.style(),
            items: &open.items,
            leading: &|item, color| self.provider.leading(item, color),
            selected: state.selected,
            width: WIDTH.min((viewport.width - px(24.)).max(px(140.))),
            max_height: list_height(cx.caret_bounds(open.trigger.start), viewport.height),
            scroll: self.scroll.clone(),
            hover: Rc::new(move |index, window, _| {
                let mut state = hovering.borrow_mut();
                if state.selected == index {
                    return;
                }
                state.selected = index;
                drop(state);
                window.refresh();
            }),
            activate: Rc::new(move |index, window, cx| {
                activating.borrow_mut().selected = index;
                window.dispatch_action(Box::new(TypeaheadAccept), cx);
            }),
        }
        .render();
        Some(Overlay {
            anchor: open.trigger.start,
            element,
            gap: GAP,
        })
    }
}

/// One transaction: the literal trigger text goes, then the provider applies the item,
/// so a single undo brings the typed `/query` back.
fn accept(state: &Rc<RefCell<State>>, provider: &Rc<dyn TypeaheadProvider>, cx: &mut EditorCx<'_>) {
    let selected = {
        let state = state.borrow();
        state.open.as_ref().and_then(|open| {
            open.items
                .get(state.selected)
                .map(|item| (open.trigger.start, item.clone()))
        })
    };
    let Some((start, item)) = selected else {
        return;
    };
    let caret = cx.selection().head;
    let mut payload = None;
    if cx
        .transact(|tx| {
            tx.delete_range(start..caret);
            payload = provider.accept(&item, tx);
        })
        .is_none()
    {
        return;
    }
    if let Some(payload) = payload {
        cx.emit(payload);
    }
    let mut state = state.borrow_mut();
    state.open = None;
    state.dismissed = None;
    state.selected = 0;
}

#[cfg(test)]
mod tests {
    use super::{TriggerMatch, track_dismissed, trigger_match};
    use markraft_core::{
        BlockKind, Document, Editor, Origin, Position, Selection, TransactionOptions,
    };
    use std::ops::Range;

    const SLASH: [char; 2] = ['/', '、'];

    fn doc(blocks: &[(BlockKind, &str)]) -> Document {
        let mut document = Document {
            blocks: blocks
                .iter()
                .map(|(kind, text)| markraft_core::Block {
                    kind: kind.clone(),
                    depth: 0,
                    spans: vec![markraft_core::Span {
                        text: (*text).to_owned(),
                        marks: Default::default(),
                        link: None,
                    }],
                })
                .collect(),
        };
        document.normalize();
        document
    }

    fn at(document: &Document, block: usize, byte: usize) -> Option<TriggerMatch> {
        trigger_match(document, Position { block, byte }, &SLASH)
    }

    fn query(document: &Document, byte: usize) -> Option<String> {
        at(document, 0, byte).map(|found| found.query)
    }

    fn position(byte: usize) -> Position {
        Position { block: 0, byte }
    }

    /// The trigger of the only match in `editor`, the way the extension derives it.
    fn dismissed(editor: &Editor) -> Range<Position> {
        trigger_match(editor.document(), editor.selection().head, &SLASH)
            .expect("a match to dismiss")
            .trigger
    }

    fn caret(editor: &mut Editor, byte: usize) {
        editor.set_selection(Selection::caret(position(byte)));
    }

    #[test]
    fn a_trigger_at_the_block_start_matches_everything_typed_after_it() {
        let document = doc(&[(BlockKind::Paragraph, "/head")]);
        assert_eq!(query(&document, 5).as_deref(), Some("head"));
        assert_eq!(query(&document, 1).as_deref(), Some(""));
        assert_eq!(
            at(&document, 0, 5).map(|found| found.trigger),
            Some(position(0)..position(1))
        );
    }

    #[test]
    fn a_trigger_needs_whitespace_or_the_block_start_before_it() {
        let document = doc(&[(BlockKind::Paragraph, "and/or")]);
        assert_eq!(query(&document, 6), None);
        let document = doc(&[(BlockKind::Paragraph, "and /or")]);
        assert_eq!(query(&document, 7).as_deref(), Some("or"));
    }

    #[test]
    fn whitespace_after_the_trigger_ends_the_match() {
        let document = doc(&[(BlockKind::Paragraph, "/two words")]);
        assert_eq!(query(&document, 10), None);
        assert_eq!(query(&document, 4).as_deref(), Some("two"));
    }

    #[test]
    fn a_second_trigger_inside_the_run_stays_part_of_the_query() {
        let document = doc(&[(BlockKind::Paragraph, "/a/b")]);
        assert_eq!(
            at(&document, 0, 4),
            Some(TriggerMatch {
                trigger: position(0)..position(1),
                query: "a/b".to_owned(),
            })
        );
    }

    #[test]
    fn a_caret_on_plain_text_or_right_after_whitespace_never_matches() {
        let document = doc(&[(BlockKind::Paragraph, "plain")]);
        assert_eq!(query(&document, 5), None);
        let document = doc(&[(BlockKind::Paragraph, "a ")]);
        assert_eq!(query(&document, 2), None);
        let document = doc(&[(BlockKind::Paragraph, "")]);
        assert_eq!(query(&document, 0), None);
    }

    #[test]
    fn a_code_block_never_matches() {
        let document = doc(&[(
            BlockKind::Code {
                language: String::new(),
            },
            "/head",
        )]);
        assert_eq!(query(&document, 5), None);
    }

    #[test]
    fn the_chinese_slash_triggers_too_and_carries_a_multibyte_query() {
        let document = doc(&[(BlockKind::Paragraph, "、标题")]);
        let bytes = "、标题".len();
        assert_eq!(
            at(&document, 0, bytes),
            Some(TriggerMatch {
                trigger: position(0)..position("、".len()),
                query: "标题".to_owned(),
            })
        );
    }

    #[test]
    fn a_trigger_inside_a_grapheme_cluster_is_text() {
        // "/" followed by a combining acute accent is one cluster, not a trigger.
        let document = doc(&[(BlockKind::Paragraph, "a /\u{0301}x")]);
        assert_eq!(query(&document, "a /\u{0301}x".len()), None);
    }

    #[test]
    fn an_emoji_before_the_trigger_is_not_whitespace() {
        let document = doc(&[(BlockKind::Paragraph, "👩‍👩‍👧/x")]);
        assert_eq!(query(&document, "👩‍👩‍👧/x".len()), None);
        let document = doc(&[(BlockKind::Paragraph, "👩‍👩‍👧 /x")]);
        assert_eq!(query(&document, "👩‍👩‍👧 /x".len()).as_deref(), Some("x"));
    }

    #[test]
    fn an_emoji_query_is_returned_whole() {
        let document = doc(&[(BlockKind::Paragraph, "/👩‍👩‍👧")]);
        assert_eq!(query(&document, "/👩‍👩‍👧".len()).as_deref(), Some("👩‍👩‍👧"));
    }

    #[test]
    fn an_insertion_before_the_dismissed_trigger_carries_it_along() {
        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain("x /a");
        let trigger = dismissed(&editor);
        assert_eq!(trigger, position(2)..position(3));
        caret(&mut editor, 0);
        let change = editor.insert_text_plain("hello ").expect("an edit");
        assert_eq!(
            track_dismissed(&trigger, &change, editor.document()),
            Some(position(8)..position(9))
        );
    }

    #[test]
    fn editing_after_the_dismissed_trigger_leaves_it_where_it_is() {
        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain("/a");
        let trigger = dismissed(&editor);
        let change = editor.insert_text_plain("bc").expect("an edit");
        assert_eq!(
            track_dismissed(&trigger, &change, editor.document()),
            Some(position(0)..position(1))
        );
    }

    #[test]
    fn deleting_the_dismissed_trigger_forgets_it() {
        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain("/ab");
        let trigger = dismissed(&editor);
        // Backspacing over the trigger alone, and deleting a range that swallows it.
        let change = editor
            .delete_range(trigger.clone())
            .expect("the trigger to go");
        assert_eq!(track_dismissed(&trigger, &change, editor.document()), None);

        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain("x /ab");
        let trigger = dismissed(&editor);
        let change = editor
            .delete_range(position(0)..position(5))
            .expect("the line to go");
        assert_eq!(track_dismissed(&trigger, &change, editor.document()), None);
    }

    /// The accepting transaction the extension runs, driven directly on the core.
    #[test]
    fn accepting_is_one_undo_step_that_restores_the_typed_text() {
        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain("/head");
        let found = trigger_match(editor.document(), editor.selection().head, &SLASH)
            .expect("a match at the caret");
        let caret = editor.selection().head;
        let change = editor
            .transact(
                TransactionOptions {
                    group: None,
                    origin: Origin::Extension("typeahead"),
                },
                |tx| {
                    tx.delete_range(found.trigger.start..caret);
                    tx.set_block_kind(BlockKind::Heading(1));
                },
            )
            .expect("an edit");
        assert_eq!(change.origin, Origin::Extension("typeahead"));
        assert_eq!(editor.document().plain_text(), "");
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Heading(1));

        editor.undo();
        assert_eq!(editor.document().plain_text(), "/head");
        assert_eq!(editor.document().blocks[0].kind, BlockKind::Paragraph);
        // The entry before it is the typing itself, so exactly one step was added.
        editor.undo();
        assert_eq!(editor.document().plain_text(), "");
    }

    #[test]
    fn the_match_is_scoped_to_the_caret_block() {
        let document = doc(&[
            (BlockKind::Paragraph, "/one"),
            (BlockKind::Paragraph, "two"),
        ]);
        assert_eq!(at(&document, 1, 3), None);
        assert_eq!(
            at(&document, 0, 4).map(|found| found.query).as_deref(),
            Some("one")
        );
    }
}
