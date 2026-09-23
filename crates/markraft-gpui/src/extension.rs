//! Compile-time editor extensions.
//!
//! The editor owns a list of [`Extension`]s in registration order. They observe every
//! update, contribute key context, action listeners and at most one popup, and reach the
//! editor through [`EditorCx`] rather than an `Entity<EditorView>`: hosts routinely call
//! `editor.update` from inside their own update, so an extension holding a handle would
//! re-enter it and panic.
//!
//! An extension never touches the tree. It dispatches a
//! [`TransactionSpec`] or runs a [`Command`] from the catalogue, exactly as the
//! editor's own key bindings do.

use crate::{EditorEvent, EditorStyle, EditorView, clipboard};
use gpui::{prelude::*, *};
use markraft_core::commands::Command;
use markraft_core::projection::Projection;
use markraft_core::{
    ChangeDesc, EditorState, Selection, Slice, TrackMode, Transaction, TransactionSpec,
};
use std::sync::Arc;
use std::{any::Any, cell::Cell, collections::VecDeque, rc::Rc};

/// How many rounds of [`Extension::update`] one editor update may run. An edit an
/// extension makes from `update` is delivered as a further round carrying the
/// transaction it produced, so extensions must reach a fixed point; the budget only
/// stops a runaway.
const ROUNDS: usize = 8;

/// The [`origin`](markraft_core::origin) annotation every extension edit carries,
/// so an extension can tell its own edits from the user's.
pub const EXTENSION_ORIGIN_PREFIX: &str = "extension:";

/// What one editor update did. It is a hint: extensions derive their state from the
/// editor itself, so a detail missing here never leaves them stale.
#[derive(Debug, Default, Clone)]
pub struct Update {
    /// The transactions the update applied, in order.
    pub transactions: Vec<Transaction>,
    pub selection_moved: bool,
    /// The document changed outside a composition, or a composition was
    /// committed. Cancelling one does not set it.
    pub committed: bool,
    /// An input-method composition is live, so no extension may edit.
    pub composing: bool,
    /// The whole document was replaced; positions kept from before it are void.
    pub replaced: bool,
}

impl Update {
    /// Whether any of the update's transactions changed the document.
    pub fn changed(&self) -> bool {
        self.transactions.iter().any(Transaction::doc_changed)
    }

    /// The shapes of this update's changes, in order.
    pub fn changes(&self) -> Vec<ChangeDesc> {
        self.transactions
            .iter()
            .map(|tr| tr.changes().desc())
            .collect()
    }

    /// Follow a position an extension remembered through this update.
    ///
    /// `None` means the content it stood on is gone, which is how a dismissed
    /// trigger is forgotten.
    pub fn map_tracked(&self, pos: usize, assoc: i32, track: TrackMode) -> Option<usize> {
        self.transactions
            .iter()
            .try_fold(pos, |pos, tr| tr.changes().map_pos(pos, assoc, track))
    }

    /// Whether any of the update's transactions carries `prefix` as its user event.
    pub fn is_user_event(&self, prefix: &str) -> bool {
        self.transactions.iter().any(|tr| tr.is_user_event(prefix))
    }

    /// The `origin` annotation of the update's first transaction.
    pub fn origin(&self) -> Option<&str> {
        self.transactions
            .iter()
            .find_map(|tr| tr.annotation(markraft_core::origin()))
            .map(String::as_str)
    }
}

/// Whether the platform may put text into the editor. gpui asks one question of the
/// input handler and uses the answer twice — the editor gates inserted text with it
/// and gpui reuses it for `prefers_ime_for_printable_keys` — so
/// refusing text also keeps printable keys out of an active input method, which is what
/// lets an unmodified letter reach a key binding while a Chinese or Japanese input
/// source is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputPolicy {
    /// The editor's usual behaviour: text is inserted and an active input method sees
    /// printable keys first.
    #[default]
    Accept,
    /// The editor takes no inserted text, and printable keys reach key bindings even
    /// while an input method is active.
    Refuse,
}

/// How the caret is drawn at a collapsed selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaretShape {
    /// A thin bar between two graphemes.
    #[default]
    Bar,
    /// A translucent quad over the grapheme the caret rests on, so the character under
    /// it stays readable. Past the last grapheme of a line it takes a nominal width.
    Block,
    /// A thin bar along the bottom of the grapheme the caret rests on.
    Underline,
}

/// An editor extension. Every hook is optional; a view with no extension behaves
/// exactly as one that never learned about them.
pub trait Extension: 'static {
    /// Stable identity, used for the edits' `origin` and [`EditorEvent::Extension`].
    fn id(&self) -> &'static str;
    /// Contribute identifiers to the editor's key context, merged every render.
    fn key_context(&self, _context: &mut KeyContext) {}
    /// Whether the platform may insert text. The first extension returning `Some` wins;
    /// with none the editor accepts text, as it always has.
    ///
    /// An extension must not switch to [`InputPolicy::Refuse`] while a composition is
    /// live: the input method owns the marked text. The editor ignores a refusal until
    /// the composition ends rather than stranding it.
    fn input_policy(&self) -> Option<InputPolicy> {
        None
    }
    /// The caret's shape. The first extension returning `Some` wins.
    fn caret(&self) -> Option<CaretShape> {
        None
    }
    /// Called after every edit, every selection move and every focus change, once per
    /// editor update.
    fn update(&mut self, _update: &Update, _cx: &mut EditorCx<'_>) {}
    /// Action listeners installed on the editor root. They run from window dispatch,
    /// outside the editor's update.
    fn actions(&self) -> Vec<ActionHandler> {
        Vec::new()
    }
    /// A popup anchored at a document position. The first extension returning `Some`
    /// wins; the editor draws it over itself.
    fn overlay(
        &mut self,
        _cx: &EditorCx<'_>,
        _window: &mut Window,
        _app: &mut App,
    ) -> Option<Overlay> {
        None
    }
}

/// A type-erased action listener. The editor registers `action` on its root element and
/// runs `run` when it is dispatched.
pub struct ActionHandler {
    pub(crate) action: Box<dyn Action>,
    pub(crate) run: Rc<dyn Fn(&mut EditorCx<'_>)>,
}

impl ActionHandler {
    pub fn new(action: impl Action, run: impl Fn(&mut EditorCx<'_>) + 'static) -> Self {
        Self {
            action: Box::new(action),
            run: Rc::new(run),
        }
    }
}

/// A popup the editor draws over itself, hanging from the caret at `anchor`.
pub struct Overlay {
    pub anchor: usize,
    pub element: AnyElement,
    /// Space between the anchored line and the popup.
    pub gap: Pixels,
}

/// Keeps an extension registered. Dropping it unregisters the extension before the
/// editor's next update or render.
pub struct ExtensionHandle {
    alive: Rc<Cell<bool>>,
}

impl Drop for ExtensionHandle {
    fn drop(&mut self) {
        self.alive.set(false);
    }
}

/// An opaque value an extension hands to the host through [`EditorEvent::Extension`].
#[derive(Clone)]
pub struct ExtensionPayload(Rc<dyn Any>);

impl ExtensionPayload {
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl std::fmt::Debug for ExtensionPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExtensionPayload")
    }
}

pub(crate) struct Registration {
    alive: Rc<Cell<bool>>,
    extension: Box<dyn Extension>,
}

/// What an extension asked the editor to do, applied once its hook has returned.
#[derive(Default)]
struct Effects {
    events: Vec<EditorEvent>,
    transactions: Vec<Transaction>,
    notify: bool,
    /// An extension moved the selection: the caret must be revealed, the blink restarted
    /// and the extensions told, the way the editor's own selection funnel does it.
    selected: bool,
}

/// The editor as an extension sees it: state reads, one editing funnel, layout queries
/// and queued host effects.
pub struct EditorCx<'a> {
    view: &'a mut EditorView,
    /// The application, when the hook runs where it can be reached. Only overlay
    /// rendering cannot: it is handed its own `&mut App` instead.
    app: Option<&'a mut App>,
    id: &'static str,
    effects: Effects,
}

impl<'a> EditorCx<'a> {
    fn new(view: &'a mut EditorView, id: &'static str, app: Option<&'a mut App>) -> Self {
        Self {
            view,
            app,
            id,
            effects: Effects::default(),
        }
    }
    /// The editor's state: document, selection and every extension field.
    pub fn state(&self) -> &EditorState {
        self.view.state()
    }
    /// The document's flattened, line-oriented view.
    pub fn projection(&self) -> Arc<Projection> {
        self.view.projection()
    }
    /// Which of the schema's types play the roles the editor knows about, as
    /// the host configured them. The [`commands`](crate::commands) take it.
    pub fn types(&self) -> &crate::DocTypes {
        &self.view.types
    }
    pub fn selection(&self) -> &Selection {
        self.view.state().selection()
    }
    /// The caret, or the moving end of a range.
    pub fn head(&self) -> usize {
        self.view.head()
    }
    pub fn is_composing(&self) -> bool {
        self.view.is_composing()
    }
    /// Whether the editor holds the window's focus, as of the last frame or focus
    /// change. A popup must close when it does not.
    pub fn is_focused(&self) -> bool {
        self.view.focused
    }
    pub fn style(&self) -> &EditorStyle {
        self.view.style()
    }
    /// Window bounds of the caret at `pos`; see [`EditorView::caret_bounds`].
    pub fn caret_bounds(&self, pos: usize) -> Option<Bounds<Pixels>> {
        self.view.caret_bounds(pos)
    }
    /// Redraw the editor. Extensions never call `cx.notify` themselves.
    pub fn notify(&mut self) {
        self.effects.notify = true;
    }
    /// Hand `payload` to the host as [`EditorEvent::Extension`], after this hook ends.
    pub fn emit(&mut self, payload: Rc<dyn Any>) {
        self.effects.events.push(EditorEvent::Extension {
            id: self.id,
            payload: ExtensionPayload(payload),
        });
    }
    /// Apply `specs` as one transaction tagged with this extension's origin.
    ///
    /// Returns `None` and makes no edit while an input-method composition is live:
    /// the input method owns the marked text, so an extension must never edit under it.
    pub fn dispatch(
        &mut self,
        specs: impl IntoIterator<Item = TransactionSpec>,
    ) -> Option<Transaction> {
        if self.view.is_composing() {
            return None;
        }
        let origin = format!("{EXTENSION_ORIGIN_PREFIX}{}", self.id);
        let specs: Vec<TransactionSpec> = specs
            .into_iter()
            .map(|spec| spec.annotate(markraft_core::origin().of(origin.clone())))
            .collect();
        let applied = self.view.apply(specs)?;
        self.view.upstream = false;
        self.view.reveal = true;
        self.effects.transactions.extend(applied.iter().cloned());
        applied.into_iter().next_back()
    }
    /// Run a command from the catalogue. `false` when it does not apply.
    pub fn run(&mut self, command: &Command) -> bool {
        match command(self.view.state()) {
            Some(spec) => self.dispatch([spec]).is_some(),
            None => false,
        }
    }
    /// Undo one entry, as ⌘Z does. `None` while a composition is live or when there is
    /// nothing left to undo.
    pub fn undo(&mut self) -> Option<Transaction> {
        let spec = markraft_core::undo(self.view.state())?;
        self.dispatch([spec])
    }
    pub fn redo(&mut self) -> Option<Transaction> {
        let spec = markraft_core::redo(self.view.state())?;
        self.dispatch([spec])
    }
    /// Set the selection, collapsed or ranged, without changing the document. It goes
    /// through the editor's own funnel: the caret is revealed and every extension is
    /// told once this hook has returned. A live composition refuses the move.
    ///
    /// `keep_column` retains the column a vertical move remembered, for a caller only
    /// correcting the caret within the row it just reached; any other move forgets it,
    /// the way a horizontal arrow key does.
    pub fn select(&mut self, selection: Selection, keep_column: bool) -> bool {
        if self.view.is_composing() {
            return false;
        }
        let column = self.view.preferred_x;
        self.apply_selection(selection, false);
        if keep_column {
            self.view.preferred_x = column;
        }
        true
    }
    /// Move the caret `rows` visual rows, negative for up, keeping the column it started
    /// from the way ↑ and ↓ do. `false` before the first paint, when no layout exists,
    /// and while composing.
    pub fn move_visual_rows(&mut self, rows: isize, extend: bool) -> bool {
        if self.view.is_composing() {
            return false;
        }
        let Some((head, x, upstream)) = self.view.visual_row_target(rows) else {
            return false;
        };
        let anchor = if extend {
            self.view
                .state()
                .selection()
                .anchor(self.view.state().doc())
        } else {
            head
        };
        self.apply_selection(Selection::text(anchor, head), upstream);
        self.view.preferred_x = Some(x);
        true
    }
    /// Move the caret to the start or end of its visual row, the way Home and End do.
    /// A wrapped block has several of them.
    pub fn move_visual_line_edge(&mut self, end: bool, extend: bool) -> bool {
        if self.view.is_composing() {
            return false;
        }
        let Some((head, upstream)) = self.view.line_edge_target(end) else {
            return false;
        };
        let anchor = if extend {
            self.view
                .state()
                .selection()
                .anchor(self.view.state().doc())
        } else {
            head
        };
        self.apply_selection(Selection::text(anchor, head), upstream);
        true
    }
    fn apply_selection(&mut self, selection: Selection, upstream: bool) {
        if let Some(applied) = self.view.apply([TransactionSpec::new()
            .selection(selection)
            .scroll_into_view()])
        {
            self.effects.transactions.extend(applied);
        }
        self.view.upstream = upstream;
        self.view.preferred_x = None;
        self.view.reveal = true;
        self.effects.selected = true;
    }
    /// Fold every undo entry made from now until [`Self::end_undo_group`] into one, so
    /// that a modal editor's insert session undoes as a whole. Undo, redo and the
    /// extension's removal end it.
    pub fn begin_undo_group(&mut self) {
        self.view.begin_undo_group();
    }
    pub fn end_undo_group(&mut self) {
        self.view.end_undo_group();
    }
    /// `slice` as the prose a plain-text surface shows: whatever the host's
    /// codecs call text, which for Markdown leaves the delimiter characters out.
    ///
    /// An editor with no codecs falls back to the model's own flattening, with
    /// each run the kind conceals read as what it displays.
    pub fn plain_text(&self, slice: &Slice) -> String {
        match &self.view.codecs {
            Some(codecs) => codecs.to_text(slice),
            None => crate::conceal::slice_text(
                self.view.state().schema(),
                self.view.types.syntax,
                slice,
            ),
        }
    }
    /// Put a slice and its markup on the system clipboard, exactly as ⌘C does, so
    /// another application pastes the markup and this one pastes the slice.
    ///
    /// `false` when the hook cannot reach the application, or when the host
    /// configured no codecs.
    pub fn write_clipboard(&mut self, slice: &Slice) -> bool {
        let schema = self.view.state().schema().clone();
        let Some(codecs) = self.view.codecs.clone() else {
            return false;
        };
        let Some(app) = self.app.as_deref_mut() else {
            return false;
        };
        clipboard::write(&schema, codecs.as_ref(), slice, app);
        true
    }
    /// The slice on the system clipboard, read as ⌘V reads it: this editor's own
    /// slice when it wrote one, otherwise HTML or markup from another application.
    pub fn read_clipboard(&mut self) -> Option<Slice> {
        let schema = self.view.state().schema().clone();
        let codecs = self.view.codecs.clone()?;
        let app = self.app.as_deref_mut()?;
        let item = app.read_from_clipboard()?;
        clipboard::read_fragment(
            &schema,
            codecs.as_ref(),
            &item,
            clipboard::PasteMode::Formatted,
        )
    }
}

impl EditorView {
    /// Register `extension`. Extensions are consulted in registration order; dropping
    /// the returned handle unregisters it.
    pub fn add_extension(
        &mut self,
        extension: impl Extension,
        cx: &mut Context<Self>,
    ) -> ExtensionHandle {
        let alive = Rc::new(Cell::new(true));
        self.extensions.push(Registration {
            alive: alive.clone(),
            extension: Box::new(extension),
        });
        self.run_extensions(Update::default(), cx);
        ExtensionHandle { alive }
    }

    /// Window bounds of the caret at `pos`, from the layout of the frame that last
    /// painted. `None` before the first paint.
    pub fn caret_bounds(&self, pos: usize) -> Option<Bounds<Pixels>> {
        let (row, offset) = self.row_at(pos)?;
        Some(Bounds::new(
            row.caret(offset, false),
            size(px(0.), row.line_height),
        ))
    }

    /// Forget the extensions whose handle has been dropped. One of them may have left
    /// an undo group open, which nothing else would ever close.
    pub(crate) fn prune_extensions(&mut self) {
        let before = self.extensions.len();
        self.extensions
            .retain(|registration| registration.alive.get());
        if self.extensions.len() != before {
            self.end_undo_group();
        }
    }

    /// The first live extension's answer, or the default when none has one. Called from
    /// paint and from the input handler, where the list cannot be pruned.
    fn first_extension_answer<T>(&self, answer: impl Fn(&dyn Extension) -> Option<T>) -> Option<T> {
        self.extensions
            .iter()
            .filter(|registration| registration.alive.get())
            .find_map(|registration| answer(registration.extension.as_ref()))
    }

    pub(crate) fn extension_input_policy(&self) -> InputPolicy {
        self.first_extension_answer(Extension::input_policy)
            .unwrap_or_default()
    }

    pub(crate) fn extension_caret(&self) -> CaretShape {
        self.first_extension_answer(Extension::caret)
            .unwrap_or_default()
    }

    pub(crate) fn extension_key_context(&self) -> KeyContext {
        let mut context = KeyContext::default();
        context.add("Markraft");
        for registration in &self.extensions {
            registration.extension.key_context(&mut context);
        }
        context
    }

    /// Every action every extension listens for, each tagged with its extension's id so
    /// that the edits and events it makes carry the right origin.
    pub(crate) fn extension_actions(&self) -> Vec<(&'static str, ActionHandler)> {
        self.extensions
            .iter()
            .flat_map(|registration| {
                let id = registration.extension.id();
                registration
                    .extension
                    .actions()
                    .into_iter()
                    .map(move |handler| (id, handler))
            })
            .collect()
    }

    /// Run an extension's action listener. It arrives from window dispatch, outside the
    /// editor's own update, so its edits are ordinary edits.
    pub(crate) fn run_extension_action(
        &mut self,
        id: &'static str,
        run: &Rc<dyn Fn(&mut EditorCx<'_>)>,
        cx: &mut Context<Self>,
    ) {
        let effects = {
            let mut ecx = EditorCx::new(self, id, Some(cx));
            run(&mut ecx);
            ecx.effects
        };
        for update in self.flush_extension_effects(effects, cx) {
            self.run_extensions(update, cx);
        }
    }

    /// The popup of the first extension that offers one.
    pub(crate) fn extension_overlay(
        &mut self,
        window: &mut Window,
        app: &mut App,
    ) -> Option<Overlay> {
        if self.extensions.is_empty() {
            return None;
        }
        let mut registrations = std::mem::take(&mut self.extensions);
        let mut overlay = None;
        {
            let mut ecx = EditorCx::new(self, "", None);
            for registration in &mut registrations {
                ecx.id = registration.extension.id();
                overlay = registration.extension.overlay(&ecx, window, app);
                if overlay.is_some() {
                    break;
                }
            }
        }
        registrations.extend(std::mem::take(&mut self.extensions));
        self.extensions = registrations;
        overlay
    }

    /// Tell the extensions what just happened. An extension that edits from `update` is
    /// called again with the transaction it made; a fixed point must be reached within
    /// [`ROUNDS`] rounds.
    pub(crate) fn run_extensions(&mut self, first: Update, cx: &mut Context<Self>) {
        self.prune_extensions();
        if self.extensions.is_empty() {
            self.extension_selection = self.state().selection().clone();
            return;
        }
        let mut pending = VecDeque::from([first]);
        for _ in 0..ROUNDS {
            let Some(mut update) = pending.pop_front() else {
                return;
            };
            update.selection_moved = *self.state().selection() != self.extension_selection;
            update.composing = self.is_composing();
            self.extension_selection = self.state().selection().clone();
            let mut registrations = std::mem::take(&mut self.extensions);
            let effects = {
                let mut ecx = EditorCx::new(self, "", Some(cx));
                for registration in &mut registrations {
                    ecx.id = registration.extension.id();
                    registration.extension.update(&update, &mut ecx);
                }
                ecx.effects
            };
            // An extension registered from within a hook queues up behind the others.
            registrations.extend(std::mem::take(&mut self.extensions));
            self.extensions = registrations;
            self.prune_extensions();
            pending.extend(self.flush_extension_effects(effects, cx));
        }
    }

    /// Apply what the extensions asked for and report the rounds it owes them. Host
    /// events go out through `cx.emit`, which queues them until the current update
    /// finishes.
    fn flush_extension_effects(&mut self, effects: Effects, cx: &mut Context<Self>) -> Vec<Update> {
        let Effects {
            events,
            transactions,
            notify,
            selected,
        } = effects;
        let changed = transactions.iter().any(Transaction::doc_changed);
        // The edit is published before the extension's own events, so a host that
        // closes its popovers on a document change cannot undo what the event asks for.
        if !changed {
            if selected {
                self.reset_caret_blink(cx);
            }
            if notify || selected {
                cx.notify();
            }
        } else {
            // An edit resets the column a vertical move would keep, even when the
            // extension also moved the selection.
            self.preferred_x = None;
            self.reset_caret_blink(cx);
            self.publish(cx);
        }
        for event in events {
            cx.emit(event);
        }
        // Every applied transaction owes the extensions a round, whether or not it
        // touched the document: one that only moved the selection — an undo
        // restoring where the caret was, another extension's edit — is exactly
        // what a modal extension has to settle.
        let mut updates: Vec<Update> = transactions
            .into_iter()
            .map(|transaction| Update {
                committed: transaction.doc_changed(),
                transactions: vec![transaction],
                ..Update::default()
            })
            .collect();
        // A move with no transaction of its own still owes them one;
        // `selection_moved` is derived there, so the round carries no other news.
        if updates.is_empty() && selected {
            updates.push(Update::default());
        }
        updates
    }
}

/// Draws an extension's popup at the caret of `anchor`. The position is resolved in
/// `prepaint`, after the editor surface has published this frame's rows, so the popup
/// never trails the text it points at by a frame.
pub(crate) struct AnchoredOverlay {
    pub(crate) editor: Entity<EditorView>,
    pub(crate) anchor: usize,
    pub(crate) gap: Pixels,
    pub(crate) child: AnyElement,
}

/// Space kept between a popup and the window's edges.
pub(crate) const OVERLAY_MARGIN: Pixels = px(8.);

impl IntoElement for AnchoredOverlay {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for AnchoredOverlay {
    type RequestLayoutState = LayoutId;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, LayoutId) {
        let child = self.child.request_layout(window, cx);
        let style = Style {
            position: gpui::Position::Absolute,
            display: Display::Flex,
            ..Style::default()
        };
        (window.request_layout(style, [child], cx), child)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) {
        let popup = window.layout_bounds(*child).size;
        let viewport = window.viewport_size();
        let origin = match self.editor.read(cx).caret_bounds(self.anchor) {
            Some(caret) => {
                let below = caret.bottom() + self.gap;
                let above = caret.top() - self.gap - popup.height;
                let y = if below + popup.height <= viewport.height - OVERLAY_MARGIN {
                    below
                } else if above >= OVERLAY_MARGIN {
                    above
                } else {
                    // Neither side fits: keep the popup on screen and let it scroll.
                    (viewport.height - OVERLAY_MARGIN - popup.height).max(OVERLAY_MARGIN)
                };
                point(
                    caret
                        .left()
                        .min(viewport.width - OVERLAY_MARGIN - popup.width)
                        .max(OVERLAY_MARGIN),
                    y,
                )
            }
            None => bounds.origin,
        };
        let offset = origin - bounds.origin;
        window.with_element_offset(point(offset.x.round(), offset.y.round()), |window| {
            self.child.prepaint(window, cx);
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
