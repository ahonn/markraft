//! A trigger-character typeahead: a menu that opens on a trigger such as `/` and
//! filters as the query is typed.
//!
//! Everything but the selected row and a dismissed trigger is derived from the
//! document on every update, so the menu closes by itself on blur, on a caret move, and
//! on any edit that breaks the match.

use crate::{
    ActionHandler, EditorCx, Extension, Overlay, Update, completion::CompletionList,
    extension::OVERLAY_MARGIN,
};
use gpui::*;
use markraft_core::TrackMode;
use markraft_core::commands::{Command, delete_range};
use markraft_core::projection::Projection;
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
        KeyBinding::new("ctrl-p", TypeaheadPrev, CONTEXT),
        KeyBinding::new("ctrl-n", TypeaheadNext, CONTEXT),
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
    /// Right-aligned keyboard shortcut, drawn one key per cap.
    pub hint: SharedString,
    /// Right-aligned muted prose, such as the folder a note sits in. Unlike
    /// [`Self::hint`] it is drawn as text, so a path does not come out as a row
    /// of key caps.
    pub detail: SharedString,
    /// A short leading glyph, such as an emoji.
    pub glyph: SharedString,
}

impl TypeaheadItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: SharedString::default(),
            detail: SharedString::default(),
            glyph: SharedString::default(),
        }
    }
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = detail.into();
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
    /// The command that applies `item`. It runs in the same transaction as the
    /// deletion of the trigger text, against the document that deletion leaves,
    /// so accepting is one undo step.
    fn accept(&self, item: &TypeaheadItem) -> Command;
    /// A value handed to the host as [`crate::EditorEvent::Extension`] once the
    /// edit has landed.
    fn payload(&self, _item: &TypeaheadItem) -> Option<Rc<dyn Any>> {
        None
    }
}

/// The trigger run the caret sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TriggerMatch {
    /// The trigger character itself, as a position range.
    pub trigger: Range<usize>,
    /// The text between the trigger and the caret.
    pub query: String,
}

/// The trigger run ending at `caret`: the word the caret is in must start with a
/// trigger. That is the same as scanning back to a trigger with no whitespace between
/// it and the caret, where the trigger itself opens the line or follows whitespace, so
/// a later trigger inside the run stays part of the query. A code block never matches.
/// Callers additionally require a collapsed selection and no live composition.
///
/// With `spaces`, the query may hold single spaces between its words — `/code bl` —
/// and the nearest trigger that opens the line or follows whitespace starts it. It
/// may not start with a space or hold two in a row, which is where a menu over prose
/// gives up; that the provider finds nothing for it closes the rest.
pub(crate) fn trigger_match(
    projection: &Projection,
    in_code: bool,
    caret: usize,
    triggers: &[char],
    spaces: bool,
) -> Option<TriggerMatch> {
    if in_code {
        return None;
    }
    let index = projection.line_at(caret)?;
    let before: Vec<(usize, &str)> = projection
        .graphemes(index)
        .filter(|(pos, _)| *pos < caret)
        .collect();
    let (start, trigger) = if spaces {
        let mut found = None;
        let mut spaced = false;
        for (at, &(pos, grapheme)) in before.iter().enumerate().rev() {
            if grapheme.chars().any(char::is_whitespace) {
                if spaced {
                    return None;
                }
                spaced = true;
                continue;
            }
            spaced = false;
            let opens = at == 0 || before[at - 1].1.chars().any(char::is_whitespace);
            if is_trigger(grapheme, triggers) && opens {
                found = Some((pos, grapheme));
                break;
            }
        }
        found?
    } else {
        let (start, trigger) = before
            .iter()
            .rev()
            .take_while(|(_, grapheme)| !grapheme.chars().any(char::is_whitespace))
            .last()?;
        (*start, *trigger)
    };
    if !is_trigger(trigger, triggers) {
        return None;
    }
    let end = start + trigger.chars().count();
    let query = projection.text_between(end, caret)?.to_owned();
    if query.starts_with(char::is_whitespace) {
        return None;
    }
    Some(TriggerMatch {
        trigger: start..end,
        query,
    })
}

/// The trigger run at `caret` this instance opens on: a [`trigger_match`] whose query is
/// at least `min_query` grapheme clusters long. `None` keeps the instance closed, which
/// is what also makes it deaf to the typeahead actions.
pub(crate) fn open_match(
    projection: &Projection,
    in_code: bool,
    caret: usize,
    triggers: &[char],
    min_query: usize,
    spaces: bool,
) -> Option<TriggerMatch> {
    let found = trigger_match(projection, in_code, caret, triggers, spaces)?;
    (found.query.graphemes(true).count() >= min_query).then_some(found)
}

/// A trigger is one whole grapheme, so a character carrying a combining mark is text.
pub(crate) fn is_trigger(grapheme: &str, triggers: &[char]) -> bool {
    let mut chars = grapheme.chars();
    chars
        .next()
        .is_some_and(|first| triggers.contains(&first) && chars.next().is_none())
}

/// Follow a dismissed trigger through an update. Both ends take
/// [`TrackMode::Before`], so text typed after the trigger leaves it alone. It is
/// forgotten once the text it stood on is gone: the map drops an end, or the two
/// ends meet.
fn track_dismissed(dismissed: &Range<usize>, update: &Update) -> Option<Range<usize>> {
    let start = update.map_tracked(dismissed.start, 1, TrackMode::Before)?;
    let end = update.map_tracked(dismissed.end, 1, TrackMode::Before)?;
    (start != end).then_some(start..end)
}

#[derive(Default)]
struct State {
    /// The open menu, or `None` while it must stay closed.
    open: Option<Open>,
    selected: usize,
    /// The trigger character a user dismissed with escape, tracked through later
    /// changes so that deleting it, or typing another one, opens the menu again.
    dismissed: Option<Range<usize>>,
}

/// What each of the four actions does, so that "an action a closed instance receives is
/// a no-op" is one decision taken in one place rather than four early returns.
impl State {
    /// The row an arrow key moves to. Clamped rather than wrapping, like the app's
    /// other pickers.
    fn stepped(&self, delta: isize) -> Option<usize> {
        let last = self.open.as_ref()?.items.len() - 1;
        Some(self.selected.saturating_add_signed(delta).min(last))
    }
    /// Where the accepting transaction starts, and the item it applies there.
    fn accepting(&self) -> Option<(usize, TypeaheadItem)> {
        let open = self.open.as_ref()?;
        Some((open.trigger.start, open.items.get(self.selected)?.clone()))
    }
    /// Close the menu and remember its trigger, so escape leaves it shut.
    fn dismiss(&mut self) -> bool {
        let Some(open) = self.open.take() else {
            return false;
        };
        self.dismissed = Some(open.trigger);
        self.selected = 0;
        true
    }
    /// Close the menu for good: the item is applied, so its trigger text is gone.
    fn accepted(&mut self) {
        self.open = None;
        self.dismissed = None;
        self.selected = 0;
    }
}

struct Open {
    trigger: Range<usize>,
    query: String,
    items: Vec<TypeaheadItem>,
}

/// A typeahead over `triggers`. Several instances may share one editor; each owns its
/// trigger set, its provider and its own extension origin.
///
/// One caret, one menu: an instance opens only while the trigger run at the caret begins
/// with one of *its* triggers, so instances whose trigger sets are disjoint are never
/// open together. The key context carries the one `typeahead` identifier while any of
/// them is open, which means all of them receive the four actions; each is a strict
/// no-op for every instance but the open one.
pub struct Typeahead {
    id: &'static str,
    triggers: Vec<char>,
    min_query: usize,
    /// Whether the query may run on past a space; see [`trigger_match`].
    spaces: bool,
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
            min_query: 0,
            spaces: false,
            provider: Rc::new(provider),
            state: Rc::default(),
            scroll: ScrollHandle::new(),
        }
    }
    /// Keep the menu shut until the query is `graphemes` long, so that the keys it would
    /// take — Return above all — keep their usual meaning for a run too short to mean a
    /// menu. The default, 0, opens on the bare trigger.
    pub fn min_query(mut self, graphemes: usize) -> Self {
        self.min_query = graphemes;
        self
    }
    /// Let the query run on past single spaces, for a menu searched by phrases: the
    /// `/` menu's "Code Block".
    pub fn spaces_in_query(mut self) -> Self {
        self.spaces = true;
        self
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
        } else if let Some(dismissed) = state.dismissed.clone() {
            state.dismissed = track_dismissed(&dismissed, update);
        }
        let before = state
            .open
            .as_ref()
            .map(|open| (open.trigger.clone(), open.query.clone()));
        state.open = None;
        let found = (cx.is_focused() && !cx.is_composing() && cx.selection().is_cursor())
            .then(|| {
                open_match(
                    &cx.projection(),
                    cx.types().in_verbatim_block_at(cx.state()),
                    cx.head(),
                    &self.triggers,
                    self.min_query,
                    self.spaces,
                )
            })
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
                let Some(selected) = state.stepped(delta) else {
                    return;
                };
                state.selected = selected;
                scroll.scroll_to_item(selected);
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
                if dismissing.borrow_mut().dismiss() {
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

/// One transaction: the literal trigger text goes, then the provider's command
/// runs against what that leaves, so a single undo brings the typed `/query` back.
fn accept(state: &Rc<RefCell<State>>, provider: &Rc<dyn TypeaheadProvider>, cx: &mut EditorCx<'_>) {
    let accepting = state.borrow().accepting();
    let Some((start, item)) = accepting else {
        return;
    };
    let caret = cx.head();
    let specs = {
        let Some(delete) = delete_range(start, caret)(cx.state()) else {
            return;
        };
        let applied = cx
            .state()
            .update([delete.clone()])
            .ok()
            .and_then(|tr| provider.accept(&item)(tr.state()));
        let mut specs = vec![delete];
        if let Some(applied) = applied {
            specs.push(applied.sequential());
        }
        specs
    };
    if cx.dispatch(specs).is_none() {
        return;
    }
    if let Some(payload) = provider.payload(&item) {
        cx.emit(payload);
    }
    state.borrow_mut().accepted();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Open, State, TriggerMatch, Update, open_match, track_dismissed, trigger_match};
    use crate::typeahead::TypeaheadItem;
    use markraft_commonmark::{
        commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
        schema as md,
    };
    use markraft_core::EditorState;
    use markraft_core::commands::{
        Command, delete_range, insert_text, run_command, set_block_type,
    };
    use markraft_core::kind::DocTypes;
    use markraft_core::projection::projection_of;
    use markraft_core::{
        Attrs, EditorStateConfig, Extension as DocExtension, Schema, Selection, TransactionSpec,
    };

    const SLASH: [char; 2] = ['/', '、'];

    /// A state on the CommonMark schema with the editor's own extensions.
    pub(crate) fn state_of(source: &str) -> EditorState {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, source).expect("valid Markdown");
        state_with(schema, doc)
    }

    fn state_with(schema: Schema, doc: markraft_core::Node) -> EditorState {
        EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
            DocExtension::all([
                markraft_core::projection::projection(),
                markraft_core::composition::composition(),
                markraft_core::history::history(Default::default()),
                commonmark_extensions(&schema),
            ]),
        ))
        .expect("a valid state")
    }

    /// A state whose only paragraph holds exactly `text`, for the cases where
    /// the Markdown reader would normalise the literal away.
    pub(crate) fn text_state(text: &str) -> EditorState {
        let schema = commonmark_schema();
        let content = if text.is_empty() {
            Vec::new()
        } else {
            vec![schema.text(text)]
        };
        let paragraph = schema
            .node(md::PARAGRAPH, content)
            .expect("text is valid paragraph content");
        let doc = schema.doc([paragraph]).expect("a valid document");
        state_with(schema, doc)
    }

    /// The CommonMark roles, as a host wires them.
    pub(crate) fn types_of(state: &EditorState) -> DocTypes {
        DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names())
    }

    /// Move the caret to `pos`.
    pub(crate) fn at(state: &EditorState, pos: usize) -> EditorState {
        state
            .update([TransactionSpec::new().selection(Selection::cursor(pos))])
            .expect("a selection")
            .state()
            .clone()
    }

    pub(crate) fn run(state: &EditorState, command: &Command) -> EditorState {
        run_command(state, command)
            .expect("the command applies")
            .expect("a transaction")
            .state()
            .clone()
    }

    fn found(state: &EditorState, pos: usize) -> Option<TriggerMatch> {
        let types = types_of(state);
        let state = at(state, pos);
        trigger_match(
            &projection_of(&state),
            types.in_verbatim_block_at(&state),
            pos,
            &SLASH,
            false,
        )
    }

    fn query(state: &EditorState, pos: usize) -> Option<String> {
        found(state, pos).map(|found| found.query)
    }

    #[test]
    fn a_phrase_query_runs_past_single_spaces_but_not_a_leading_or_double_one() {
        let spaced = |text: &str| {
            let state = state_of(text);
            let caret = state.doc().content_size() - 1;
            let types = types_of(&state);
            let state = at(&state, caret);
            trigger_match(
                &projection_of(&state),
                types.in_verbatim_block_at(&state),
                caret,
                &SLASH,
                true,
            )
            .map(|found| found.query)
        };
        assert_eq!(spaced("/code bl").as_deref(), Some("code bl"));
        assert_eq!(spaced("say /code bl").as_deref(), Some("code bl"));
        assert_eq!(spaced("/ code"), None);
        assert_eq!(spaced("/code  bl"), None);
        assert_eq!(spaced("a/b c"), None);
        // Without spaces the word the caret is in has to start with the trigger.
        assert_eq!(query(&state_of("/code bl"), 9), None);
    }

    #[test]
    fn a_trigger_at_the_line_start_matches_everything_typed_after_it() {
        let state = state_of("/head");
        assert_eq!(query(&state, 6).as_deref(), Some("head"));
        assert_eq!(query(&state, 2).as_deref(), Some(""));
        assert_eq!(found(&state, 6).map(|f| f.trigger), Some(1..2));
    }

    #[test]
    fn a_trigger_needs_whitespace_or_the_line_start_before_it() {
        assert_eq!(query(&state_of("and/or"), 7), None);
        assert_eq!(query(&state_of("and /or"), 8).as_deref(), Some("or"));
    }

    #[test]
    fn whitespace_after_the_trigger_ends_the_match() {
        let state = state_of("/two words");
        assert_eq!(query(&state, 11), None);
        assert_eq!(query(&state, 5).as_deref(), Some("two"));
    }

    #[test]
    fn a_second_trigger_inside_the_run_stays_part_of_the_query() {
        let state = state_of("/a/b");
        assert_eq!(
            found(&state, 5),
            Some(TriggerMatch {
                trigger: 1..2,
                query: "a/b".to_owned(),
            })
        );
    }

    #[test]
    fn a_caret_on_plain_text_or_right_after_whitespace_never_matches() {
        assert_eq!(query(&state_of("plain"), 6), None);
        assert_eq!(query(&state_of("a "), 2), None);
        assert_eq!(query(&state_of(""), 1), None);
    }

    #[test]
    fn a_verbatim_block_never_matches() {
        let state = state_of("```\n/head\n```");
        assert_eq!(query(&state, 6), None);
        // A raw block keeps its source verbatim, so a slash in one is a slash.
        let state = state_of("<div>\n/head\n</div>");
        let caret = projection_of(&state).lines()[0]
            .offset_to_pos("<div>\n/head".chars().count())
            .expect("an offset inside the block");
        assert_eq!(query(&state, caret), None);
    }

    #[test]
    fn the_chinese_slash_triggers_too_and_carries_a_multibyte_query() {
        let state = state_of("、标题");
        assert_eq!(
            found(&state, 4),
            Some(TriggerMatch {
                trigger: 1..2,
                query: "标题".to_owned(),
            })
        );
    }

    #[test]
    fn a_trigger_inside_a_grapheme_cluster_is_text() {
        // "/" followed by a combining acute accent is one cluster, not a trigger.
        let state = state_of("a /\u{0301}x");
        assert_eq!(query(&state, 6), None);
    }

    #[test]
    fn an_emoji_before_the_trigger_is_not_whitespace() {
        let family = "👩‍👩‍👧";
        let chars = family.chars().count();
        let state = state_of(&format!("{family}/x"));
        assert_eq!(query(&state, 1 + chars + 2), None);
        let state = state_of(&format!("{family} /x"));
        assert_eq!(query(&state, 1 + chars + 3).as_deref(), Some("x"));
    }

    #[test]
    fn an_emoji_query_is_returned_whole() {
        let family = "👩‍👩‍👧";
        let state = state_of(&format!("/{family}"));
        assert_eq!(
            query(&state, 2 + family.chars().count()).as_deref(),
            Some(family)
        );
    }

    #[test]
    fn a_minimum_query_keeps_the_menu_shut_for_a_short_run() {
        let state = state_of("/ab");
        let projection = projection_of(&state);
        let at =
            |pos, min| open_match(&projection, false, pos, &SLASH, min, false).map(|f| f.query);
        // The default opens on the bare trigger.
        assert_eq!(at(2, 0).as_deref(), Some(""));
        assert_eq!(at(2, 2), None);
        assert_eq!(at(3, 2), None);
        assert_eq!(at(4, 2).as_deref(), Some("ab"));
        // Grapheme clusters, not bytes or code points: one family is one.
        let family = "👩‍👩‍👧";
        let state = state_of(&format!("/{family}x"));
        let projection = projection_of(&state);
        let chars = family.chars().count();
        let at =
            |pos, min| open_match(&projection, false, pos, &SLASH, min, false).map(|f| f.query);
        assert_eq!(at(2 + chars, 2), None);
        assert_eq!(
            at(3 + chars, 2).as_deref(),
            Some(format!("{family}x").as_str())
        );
    }

    #[test]
    fn the_match_is_scoped_to_the_caret_line() {
        let state = state_of("/one\n\ntwo");
        // The second paragraph's text holds no trigger of its own.
        assert_eq!(query(&state, 10), None);
        assert_eq!(query(&state, 5).as_deref(), Some("one"));
    }

    /// The accepting transaction the extension runs, driven directly on the state.
    #[test]
    fn accepting_is_one_undo_step_that_restores_the_typed_text() {
        let schema = commonmark_schema();
        let heading = schema.node_id(md::HEADING).unwrap();
        let state = state_of("");
        let state = run(&state, &insert_text("/head"));
        let projection = projection_of(&state);
        let found =
            trigger_match(&projection, false, 6, &SLASH, false).expect("a match at the caret");
        let delete = delete_range(found.trigger.start, 6)(&state).expect("a deletion");
        let after = state
            .update([delete.clone()])
            .expect("the deletion applies");
        let apply = set_block_type(heading, Attrs::from_pairs([("level", 1i64)]))(after.state())
            .expect("the heading applies");
        let state = state
            .update([delete, apply.sequential()])
            .expect("one transaction")
            .state()
            .clone();
        assert_eq!(projection_of(&state).plain_text(), "");
        assert_eq!(state.doc().child(0).type_id(), heading);

        let state = run(
            &state,
            &markraft_core::commands::command(markraft_core::history::undo),
        );
        assert_eq!(projection_of(&state).plain_text(), "/head");
        assert_eq!(
            state.doc().child(0).type_id(),
            schema.node_id(md::PARAGRAPH).unwrap()
        );
        // The entry before it is the typing itself, so exactly one step was added.
        let state = run(
            &state,
            &markraft_core::commands::command(markraft_core::history::undo),
        );
        assert_eq!(projection_of(&state).plain_text(), "");
    }

    fn open(rows: usize) -> State {
        State {
            open: Some(Open {
                trigger: 1..2,
                query: String::new(),
                items: (0..rows)
                    .map(|row| TypeaheadItem::new(row.to_string(), row.to_string()))
                    .collect(),
            }),
            selected: 0,
            dismissed: None,
        }
    }

    /// Every instance receives the four actions while any of them is open, so a closed
    /// one must do nothing at all.
    #[test]
    fn a_closed_typeahead_takes_no_action() {
        let mut state = State::default();
        assert_eq!(state.stepped(1), None);
        assert_eq!(state.stepped(-1), None);
        assert_eq!(state.accepting(), None);
        assert!(!state.dismiss());
        assert_eq!(state.dismissed, None);
    }

    #[test]
    fn an_open_typeahead_steps_clamped_accepts_its_row_and_remembers_a_dismissal() {
        let mut state = open(3);
        assert_eq!(state.stepped(-1), Some(0));
        assert_eq!(state.stepped(1), Some(1));
        state.selected = 2;
        assert_eq!(state.stepped(1), Some(2));
        let accepting = state.accepting().expect("the selected row");
        assert_eq!((accepting.0, accepting.1.id.as_ref()), (1, "2"));
        assert!(state.dismiss());
        assert_eq!(state.dismissed, Some(1..2));
        // Dismissing closes it, so the next action finds nothing to do.
        assert_eq!(state.stepped(1), None);
        assert_eq!(state.accepting(), None);
        assert!(!state.dismiss());
    }

    /// The tracking a dismissed trigger goes through, expressed over transactions.
    fn update_of(before: &EditorState, after: &markraft_core::Transaction) -> Update {
        let _ = before;
        Update {
            transactions: vec![after.clone()],
            ..Update::default()
        }
    }

    #[test]
    fn an_insertion_before_the_dismissed_trigger_carries_it_along() {
        let state = state_of("x /a");
        let trigger = 3..4;
        let state = at(&state, 1);
        let tr = state
            .update([insert_text("hello ")(&state).expect("a spec")])
            .expect("an edit");
        assert_eq!(
            track_dismissed(&trigger, &update_of(&state, &tr)),
            Some(9..10)
        );
    }

    #[test]
    fn editing_after_the_dismissed_trigger_leaves_it_where_it_is() {
        let state = state_of("/a");
        let trigger = 1..2;
        let state = at(&state, 3);
        let tr = state
            .update([insert_text("bc")(&state).expect("a spec")])
            .expect("an edit");
        assert_eq!(
            track_dismissed(&trigger, &update_of(&state, &tr)),
            Some(1..2)
        );
    }

    #[test]
    fn deleting_the_dismissed_trigger_forgets_it() {
        // Deleting the trigger alone, and deleting a range that swallows it.
        let state = state_of("/ab");
        let trigger = 1..2;
        let tr = state
            .update([delete_range(1, 2)(&state).expect("a spec")])
            .expect("an edit");
        assert_eq!(track_dismissed(&trigger, &update_of(&state, &tr)), None);

        let state = state_of("x /ab");
        let trigger = 3..4;
        let tr = state
            .update([delete_range(1, 6)(&state).expect("a spec")])
            .expect("an edit");
        assert_eq!(track_dismissed(&trigger, &update_of(&state, &tr)), None);
    }
}
