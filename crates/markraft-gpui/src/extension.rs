//! Compile-time editor extensions.
//!
//! The editor owns a list of [`Extension`]s in registration order. They observe every
//! update, contribute key context, action listeners and at most one popup, and reach the
//! editor through [`EditorCx`] rather than an `Entity<EditorView>`: hosts routinely call
//! `editor.update` from inside their own update, so an extension holding a handle would
//! re-enter it and panic.

use crate::{EditorEvent, EditorStyle, EditorView};
use gpui::{prelude::*, *};
use markraft_core::{
    Change, Document, Origin, Position, Selection, Transaction, TransactionOptions,
};
use std::{any::Any, cell::Cell, collections::VecDeque, rc::Rc};

/// How many rounds of [`Extension::update`] one editor update may run. An edit an
/// extension makes from `update` is delivered as a further round with
/// `Origin::Extension(id)`, so extensions must reach a fixed point; the budget only
/// stops a runaway.
const ROUNDS: usize = 8;

/// What one editor update did. It is a hint: extensions derive their state from the
/// editor itself, so a detail missing here never leaves them stale.
#[derive(Debug, Default)]
pub struct Update {
    /// The change the update published, carrying its origin and position mapping.
    pub change: Option<Change>,
    pub selection_moved: bool,
    /// [`EditorView::committed_document`] changed: an edit landed outside a
    /// composition, or a composition was committed. Cancelling one does not set it.
    pub committed: bool,
    /// An input-method composition is live, so no extension may edit.
    pub composing: bool,
    /// The whole document was replaced; positions kept from before it are void.
    pub replaced: bool,
}

/// An editor extension. Every hook is optional; a view with no extension behaves
/// exactly as one that never learned about them.
pub trait Extension: 'static {
    /// Stable identity, used for `Origin::Extension` and [`EditorEvent::Extension`].
    fn id(&self) -> &'static str;
    /// Contribute identifiers to the editor's key context, merged every render.
    fn key_context(&self, _context: &mut KeyContext) {}
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
    pub anchor: Position,
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
    changes: Vec<Change>,
    notify: bool,
}

/// The editor as an extension sees it: core reads, one editing funnel, layout queries
/// and queued host effects.
pub struct EditorCx<'a> {
    view: &'a mut EditorView,
    id: &'static str,
    effects: Effects,
}

impl<'a> EditorCx<'a> {
    fn new(view: &'a mut EditorView, id: &'static str) -> Self {
        Self {
            view,
            id,
            effects: Effects::default(),
        }
    }
    pub fn document(&self) -> &Document {
        self.view.document()
    }
    pub fn committed_document(&self) -> &Document {
        self.view.committed_document()
    }
    pub fn selection(&self) -> Selection {
        self.view.core.selection()
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
        &self.view.style
    }
    /// Window bounds of the caret at `position`; see [`EditorView::caret_bounds`].
    pub fn caret_bounds(&self, position: Position) -> Option<Bounds<Pixels>> {
        self.view.caret_bounds(position)
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
    /// Run `action` as one undo entry with `Origin::Extension(id)`. Returns `None` and
    /// makes no edit while an input-method composition is live: the input method owns
    /// the marked text, so an extension must never edit under it.
    pub fn transact(&mut self, action: impl FnOnce(&mut Transaction<'_>)) -> Option<Change> {
        if self.view.core.is_composing() {
            return None;
        }
        let change = self.view.core.transact(
            TransactionOptions {
                group: None,
                origin: Origin::Extension(self.id),
            },
            action,
        )?;
        self.view.upstream = false;
        self.view.last_typed_at = None;
        self.view.reveal = true;
        self.effects.changes.push(change.clone());
        Some(change)
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

    /// Window bounds of the caret at `position`, from the layout of the frame that last
    /// painted. `None` before the first paint.
    pub fn caret_bounds(&self, position: Position) -> Option<Bounds<Pixels>> {
        let row = self.layout.get(position.block)?;
        Some(Bounds::new(
            row.caret(position.byte, false),
            size(px(0.), row.line_height),
        ))
    }

    /// Forget the extensions whose handle has been dropped.
    pub(crate) fn prune_extensions(&mut self) {
        self.extensions
            .retain(|registration| registration.alive.get());
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
            let mut ecx = EditorCx::new(self, id);
            run(&mut ecx);
            ecx.effects
        };
        for change in self.flush_extension_effects(effects, cx) {
            self.run_extensions(
                Update {
                    change: Some(change),
                    committed: true,
                    ..Update::default()
                },
                cx,
            );
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
            let mut ecx = EditorCx::new(self, "");
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
    /// called again with the change it made; a fixed point must be reached within
    /// [`ROUNDS`] rounds.
    pub(crate) fn run_extensions(&mut self, first: Update, cx: &mut Context<Self>) {
        self.prune_extensions();
        if self.extensions.is_empty() {
            self.extension_selection = self.core.selection();
            return;
        }
        let mut pending = VecDeque::from([first]);
        for _ in 0..ROUNDS {
            let Some(mut update) = pending.pop_front() else {
                return;
            };
            update.selection_moved = self.core.selection() != self.extension_selection;
            update.composing = self.core.is_composing();
            self.extension_selection = self.core.selection();
            let mut registrations = std::mem::take(&mut self.extensions);
            let effects = {
                let mut ecx = EditorCx::new(self, "");
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
            for change in self.flush_extension_effects(effects, cx) {
                pending.push_back(Update {
                    change: Some(change),
                    committed: true,
                    ..Update::default()
                });
            }
        }
    }

    /// Apply what the extensions asked for and report the edits they made. Host events
    /// go out through `cx.emit`, which queues them until the current update finishes.
    fn flush_extension_effects(&mut self, effects: Effects, cx: &mut Context<Self>) -> Vec<Change> {
        let Effects {
            events,
            changes,
            notify,
        } = effects;
        // The edit is published before the extension's own events, so a host that
        // closes its popovers on a document change cannot undo what the event asks for.
        if changes.is_empty() {
            if notify {
                cx.notify();
            }
        } else {
            self.preferred_x = None;
            self.reset_caret_blink(cx);
            self.publish(cx);
        }
        for event in events {
            cx.emit(event);
        }
        changes
    }
}

/// Draws an extension's popup at the caret of `anchor`. The position is resolved in
/// `prepaint`, after the editor surface has published this frame's rows, so the popup
/// never trails the text it points at by a frame.
pub(crate) struct AnchoredOverlay {
    pub(crate) editor: Entity<EditorView>,
    pub(crate) anchor: Position,
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
