//! Public AppKit spelling/substitution panels bridged to guarded editor commands.
use super::{native_view, text_checking::SpellDocument};
use crate::{
    locale::Message,
    storage::{TextCheckingPreferences, TextCheckingSetting},
};
use futures_channel::mpsc;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObjectProtocol},
};
use objc2_app_kit::{
    NSApplication, NSResponder, NSWindow, NSWindowDidBecomeKeyNotification,
    NSWindowDidResignKeyNotification, NSWindowWillCloseNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSRange, NSString};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CheckingPanel {
    Spelling,
    Substitutions,
}

impl CheckingPanel {
    pub(crate) fn menu_label(self, visible: bool) -> &'static str {
        match (self, visible) {
            (Self::Spelling, false) => "command.show-spelling",
            (Self::Spelling, true) => "command.hide-spelling",
            (Self::Substitutions, false) => "command.show-substitutions",
            (Self::Substitutions, true) => "command.hide-substitutions",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum SubstitutionKind {
    Quotes,
    Dashes,
    Text,
    Links,
    All,
}

#[derive(Debug)]
pub(crate) enum PanelCommand {
    Focus,
    Next,
    Replace(String),
    Ignore,
    Settings(Vec<(TextCheckingSetting, bool)>),
    Substitute { document: bool },
}

pub(crate) struct PanelEvent {
    pub generation: u64,
    pub command: PanelCommand,
}

struct State {
    generation: u64,
    events: Option<mpsc::UnboundedSender<PanelEvent>>,
    settings: TextCheckingPreferences,
    editable: bool,
    range: NSRange,
    tag: isize,
}

impl State {
    fn update_settings(&mut self, changes: impl IntoIterator<Item = (TextCheckingSetting, bool)>) {
        if self.events.is_none() {
            return;
        }
        let changes = changes
            .into_iter()
            .filter(|(setting, value)| {
                let changed = self.settings.get(*setting) != *value;
                self.settings.set(*setting, *value);
                changed
            })
            .collect::<Vec<_>>();
        if !changes.is_empty() {
            self.send(PanelCommand::Settings(changes));
        }
    }

    fn set_checking_types(&mut self, types: u64) {
        use objc2_foundation::NSTextCheckingType as Type;
        self.update_settings(
            [
                (TextCheckingSetting::Spelling, Type::Spelling.0),
                (TextCheckingSetting::Grammar, Type::Grammar.0),
                (TextCheckingSetting::Correction, Type::Correction.0),
                (TextCheckingSetting::Quotes, Type::Quote.0),
                (TextCheckingSetting::Dashes, Type::Dash.0),
                (TextCheckingSetting::Replacements, Type::Replacement.0),
                (TextCheckingSetting::Links, Type::Link.0),
                (
                    TextCheckingSetting::DataDetectors,
                    Type::Date.0
                        | Type::Address.0
                        | Type::PhoneNumber.0
                        | Type::TransitInformation.0,
                ),
            ]
            .into_iter()
            .map(|(setting, mask)| (setting, types & mask != 0)),
        );
    }

    fn send(&self, command: PanelCommand) {
        if let Some(events) = &self.events {
            let _ = events.unbounded_send(PanelEvent {
                generation: self.generation,
                command,
            });
        }
    }
}

define_class!(
    #[unsafe(super(NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<State>]
    struct CheckingResponder;
    unsafe impl NSObjectProtocol for CheckingResponder {}
    impl CheckingResponder {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }
        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool { true }
        #[unsafe(method(checkingPanelFocusChanged:))]
        fn focus_changed(&self, _: &NSNotification) { self.ivars().borrow().send(PanelCommand::Focus); }
        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate(&self, item: &AnyObject) -> bool { self.validate_action(item) }
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu(&self, item: &AnyObject) -> bool { self.validate_action(item) }
        #[unsafe(method(checkSpelling:))]
        fn check(&self, _: Option<&AnyObject>) { self.ivars().borrow().send(PanelCommand::Next); }
        #[unsafe(method(changeSpelling:))]
        fn change(&self, sender: &AnyObject) {
            let text: Option<Retained<NSString>> = unsafe { msg_send![sender, stringValue] };
            if let Some(text) = text { self.ivars().borrow().send(PanelCommand::Replace(text.to_string())); }
        }
        #[unsafe(method(ignoreSpelling:))]
        fn ignore(&self, _: Option<&AnyObject>) { self.ivars().borrow().send(PanelCommand::Ignore); }
        #[unsafe(method(checkTextInSelection:))]
        fn selection(&self, _: Option<&AnyObject>) { self.ivars().borrow().send(PanelCommand::Substitute { document: false }); }
        #[unsafe(method(checkTextInDocument:))]
        fn document(&self, _: Option<&AnyObject>) { self.ivars().borrow().send(PanelCommand::Substitute { document: true }); }
        #[unsafe(method(toggleContinuousSpellChecking:))]
        fn spelling(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Spelling); }
        #[unsafe(method(toggleGrammarChecking:))]
        fn grammar(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Grammar); }
        #[unsafe(method(toggleAutomaticSpellingCorrection:))]
        fn correction(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Correction); }
        #[unsafe(method(toggleAutomaticQuoteSubstitution:))]
        fn quotes(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Quotes); }
        #[unsafe(method(toggleAutomaticDashSubstitution:))]
        fn dashes(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Dashes); }
        #[unsafe(method(toggleAutomaticTextReplacement:))]
        fn text(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Replacements); }
        #[unsafe(method(toggleAutomaticLinkDetection:))]
        fn links(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::Links); }
        #[unsafe(method(toggleAutomaticDataDetection:))]
        fn data(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::DataDetectors); }
        #[unsafe(method(toggleSmartInsertDelete:))]
        fn smart_insert(&self, _: Option<&AnyObject>) { self.toggle(TextCheckingSetting::SmartInsertDelete); }
        #[unsafe(method(smartInsertDeleteEnabled))]
        fn has_smart_insert(&self) -> bool { self.ivars().borrow().settings.smart_insert_delete }
        #[unsafe(method(isContinuousSpellCheckingEnabled))]
        fn has_spelling(&self) -> bool { self.ivars().borrow().settings.spelling }
        #[unsafe(method(isGrammarCheckingEnabled))]
        fn has_grammar(&self) -> bool { self.ivars().borrow().settings.grammar }
        #[unsafe(method(isAutomaticSpellingCorrectionEnabled))]
        fn has_correction(&self) -> bool { self.ivars().borrow().settings.correction }
        #[unsafe(method(isAutomaticQuoteSubstitutionEnabled))]
        fn has_quotes(&self) -> bool { self.ivars().borrow().settings.quotes }
        #[unsafe(method(isAutomaticDashSubstitutionEnabled))]
        fn has_dashes(&self) -> bool { self.ivars().borrow().settings.dashes }
        #[unsafe(method(isAutomaticTextReplacementEnabled))]
        fn has_text(&self) -> bool { self.ivars().borrow().settings.replacements }
        #[unsafe(method(isAutomaticLinkDetectionEnabled))]
        fn has_links(&self) -> bool { self.ivars().borrow().settings.links }
        #[unsafe(method(isAutomaticDataDetectionEnabled))]
        fn has_data(&self) -> bool { self.ivars().borrow().settings.data_detectors }
        #[unsafe(method(setSmartInsertDeleteEnabled:))]
        fn set_smart_insert(&self, value: bool) { self.set(TextCheckingSetting::SmartInsertDelete, value); }
        #[unsafe(method(setContinuousSpellCheckingEnabled:))]
        fn set_spelling(&self, value: bool) { self.set(TextCheckingSetting::Spelling, value); }
        #[unsafe(method(setGrammarCheckingEnabled:))]
        fn set_grammar(&self, value: bool) { self.set(TextCheckingSetting::Grammar, value); }
        #[unsafe(method(setAutomaticSpellingCorrectionEnabled:))]
        fn set_correction(&self, value: bool) { self.set(TextCheckingSetting::Correction, value); }
        #[unsafe(method(setAutomaticQuoteSubstitutionEnabled:))]
        fn set_quotes(&self, value: bool) { self.set(TextCheckingSetting::Quotes, value); }
        #[unsafe(method(setAutomaticDashSubstitutionEnabled:))]
        fn set_dashes(&self, value: bool) { self.set(TextCheckingSetting::Dashes, value); }
        #[unsafe(method(setAutomaticTextReplacementEnabled:))]
        fn set_text(&self, value: bool) { self.set(TextCheckingSetting::Replacements, value); }
        #[unsafe(method(setAutomaticLinkDetectionEnabled:))]
        fn set_links(&self, value: bool) { self.set(TextCheckingSetting::Links, value); }
        #[unsafe(method(setAutomaticDataDetectionEnabled:))]
        fn set_data(&self, value: bool) { self.set(TextCheckingSetting::DataDetectors, value); }
        #[unsafe(method(setEnabledTextCheckingTypes:))]
        fn set_checking_types(&self, value: u64) { self.ivars().borrow_mut().set_checking_types(value); }
        #[unsafe(method(enabledTextCheckingTypes))]
        fn enabled_checking_types(&self) -> u64 {
            let settings = self.ivars().borrow().settings;
            super::text_checking::CheckOptions {
                spelling: settings.spelling, grammar: settings.grammar, quotes: settings.quotes,
                dashes: settings.dashes, replacements: settings.replacements,
                correction: settings.correction, links: settings.links, data_detectors: settings.data_detectors,
            }.mask()
        }
        #[unsafe(method(isEditable))]
        fn editable(&self) -> bool { self.ivars().borrow().editable }
        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange { self.ivars().borrow().range }
        #[unsafe(method(spellCheckerDocumentTag))]
        fn document_tag(&self) -> isize { self.ivars().borrow().tag }
    }
);

impl CheckingResponder {
    fn validate_action(&self, item: &AnyObject) -> bool {
        let action: objc2::runtime::Sel = unsafe { msg_send![item, action] };
        let state = self.ivars().borrow();
        if state.events.is_none() {
            return false;
        }
        if action == objc2::sel!(changeSpelling:) || action == objc2::sel!(checkTextInSelection:) {
            return state.editable && state.range.length != 0;
        }
        if action == objc2::sel!(checkTextInDocument:) {
            return state.editable;
        }
        true
    }
    fn set(&self, setting: TextCheckingSetting, value: bool) {
        self.ivars()
            .borrow_mut()
            .update_settings([(setting, value)]);
    }

    fn toggle(&self, setting: TextCheckingSetting) {
        let mut state = self.ivars().borrow_mut();
        let value = !state.settings.get(setting);
        state.update_settings([(setting, value)]);
    }
}

/// Own the panel's action target until the editor context changes or another menu opens.
pub(crate) struct CheckingPanelSession {
    view: Retained<NSResponder>,
    previous: Option<Retained<NSResponder>>,
    responder: Retained<CheckingResponder>,
    owner: Retained<NSWindow>,
    panels: [Retained<NSWindow>; 2],
    center: Retained<NSNotificationCenter>,
    proxy_active: Cell<bool>,
}

impl CheckingPanelSession {
    pub(crate) fn attach(
        window: &gpui::Window,
        checker: &SpellDocument,
        settings: TextCheckingPreferences,
        editable: bool,
    ) -> Result<(Self, mpsc::UnboundedReceiver<PanelEvent>), Message> {
        let failure = || Message::new("error.native-window-control");
        let mtm = MainThreadMarker::new().ok_or_else(failure)?;
        let view = unsafe { Retained::retain(native_view(window)?.cast::<NSResponder>()) }
            .ok_or_else(failure)?;
        let owner: Option<Retained<NSWindow>> = unsafe { msg_send![&*view, window] };
        let owner = owner.ok_or_else(failure)?;
        let native_checker: Retained<AnyObject> =
            unsafe { msg_send![objc2::class!(NSSpellChecker), sharedSpellChecker] };
        let panels = unsafe {
            [
                msg_send![&*native_checker, spellingPanel],
                msg_send![&*native_checker, substitutionsPanel],
            ]
        };
        let previous = unsafe { view.nextResponder() };
        let (events, receiver) = mpsc::unbounded();
        let responder = CheckingResponder::alloc(mtm).set_ivars(RefCell::new(State {
            generation: 0,
            events: Some(events),
            settings,
            editable,
            range: NSRange::new(0, 0),
            tag: checker.tag(),
        }));
        let responder: Retained<CheckingResponder> = unsafe { msg_send![super(responder), init] };
        unsafe {
            responder.setNextResponder(previous.as_deref());
            view.setNextResponder(Some(&responder));
        }
        let center = NSNotificationCenter::defaultCenter();
        for observed in [&owner, &panels[0], &panels[1]] {
            for name in unsafe {
                [
                    NSWindowDidBecomeKeyNotification,
                    NSWindowDidResignKeyNotification,
                    NSWindowWillCloseNotification,
                ]
            } {
                unsafe {
                    center.addObserver_selector_name_object(
                        &responder,
                        objc2::sel!(checkingPanelFocusChanged:),
                        Some(name),
                        Some(observed),
                    );
                }
            }
        }
        Ok((
            Self {
                view,
                previous,
                responder,
                owner,
                panels,
                center,
                proxy_active: Cell::new(false),
            },
            receiver,
        ))
    }

    /// AppKit reads checking properties directly from the document's first
    /// responder. Borrow that position only while one of our panels is key.
    /// GPUI keeps its original input view and input context when editing resumes.
    pub(crate) fn sync_focus(&self) {
        let app = NSApplication::sharedApplication(self.responder.mtm());
        let panel_is_key = app.keyWindow().as_ref().is_some_and(|key| {
            self.panels
                .iter()
                .any(|panel| std::ptr::eq(&**panel, &**key))
        });
        let first = self.owner.firstResponder();
        let next = unsafe { self.view.nextResponder() };
        let owns_editor = same_responder(first.as_deref(), Some(&self.view))
            && same_responder(next.as_deref(), Some(&self.responder));
        match focus_action(panel_is_key, self.proxy_active.get(), owns_editor) {
            FocusAction::Restore => {
                self.restore_focus();
                return;
            }
            FocusAction::Keep => return,
            FocusAction::Borrow => {}
        }
        unsafe {
            // Remove view -> proxy before installing proxy -> view. There must
            // never be a cycle, including during AppKit focus callbacks.
            self.view.setNextResponder(self.previous.as_deref());
            self.responder.setNextResponder(Some(&self.view));
        }
        if self.owner.makeFirstResponder(Some(&self.responder)) {
            self.proxy_active.set(true);
        } else {
            unsafe {
                self.responder.setNextResponder(self.previous.as_deref());
                self.view.setNextResponder(Some(&self.responder));
            }
        }
        self.update_native_panels();
    }

    fn restore_focus(&self) {
        if !self.proxy_active.get() {
            return;
        }
        if !release_owned_focus(
            || {
                same_responder(
                    self.owner.firstResponder().as_deref(),
                    Some(&self.responder),
                )
            },
            || {
                self.owner.makeFirstResponder(Some(&self.view));
            },
            || {
                self.owner.makeFirstResponder(None);
            },
        ) {
            return;
        }
        self.proxy_active.set(false);
        let next = unsafe { self.view.nextResponder() };
        unsafe {
            // Break proxy -> view before restoring view -> proxy.
            self.responder.setNextResponder(self.previous.as_deref());
            if same_responder(next.as_deref(), self.previous.as_deref()) {
                self.view.setNextResponder(Some(&self.responder));
            }
        }
        self.update_native_panels();
    }

    fn update_native_panels(&self) {
        unsafe {
            let checker: Retained<AnyObject> =
                msg_send![objc2::class!(NSSpellChecker), sharedSpellChecker];
            let _: () = msg_send![&*checker, updatePanels];
        }
    }

    pub(crate) fn update(
        &self,
        generation: u64,
        settings: TextCheckingPreferences,
        editable: bool,
        selected_length: usize,
    ) {
        let mut state = self.responder.ivars().borrow_mut();
        state.generation = generation;
        state.settings = settings;
        state.editable = editable;
        state.range = NSRange::new(0, selected_length);
    }
}

impl Drop for CheckingPanelSession {
    fn drop(&mut self) {
        unsafe {
            self.center.removeObserver(&self.responder);
        }
        self.responder.ivars().borrow_mut().events.take();
        self.restore_focus();
        let current = unsafe { self.view.nextResponder() };
        if current
            .as_deref()
            .is_some_and(|value| std::ptr::eq(value, &*self.responder as &NSResponder))
        {
            unsafe {
                self.view.setNextResponder(self.previous.as_deref());
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FocusAction {
    Keep,
    Borrow,
    Restore,
}

fn focus_action(panel_is_key: bool, borrowed: bool, owns_editor: bool) -> FocusAction {
    match (panel_is_key, borrowed, owns_editor) {
        (false, true, _) => FocusAction::Restore,
        (true, false, true) => FocusAction::Borrow,
        _ => FocusAction::Keep,
    }
}

/// A focus change may be refused or another control may take ownership during
/// it. Recheck actual ownership before falling back to clearing firstResponder.
fn release_owned_focus(
    is_owned: impl Fn() -> bool,
    restore: impl FnOnce(),
    clear: impl FnOnce(),
) -> bool {
    if is_owned() {
        restore();
        if is_owned() {
            clear();
        }
    }
    !is_owned()
}

fn same_responder(left: Option<&NSResponder>, right: Option<&NSResponder>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => std::ptr::eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_focus_restoration_clears_a_refused_target_without_overwriting_new_focus() {
        let owned = Cell::new(true);
        let cleared = Cell::new(false);
        assert!(release_owned_focus(
            || owned.get(),
            || {}, // The original editor refused first responder.
            || {
                cleared.set(true);
                owned.set(false);
            },
        ));
        assert!(cleared.get());
        assert!(!owned.get());

        let restored = Cell::new(false);
        assert!(release_owned_focus(
            || false, // Another control already owns focus.
            || restored.set(true),
            || panic!("must not clear another control's focus"),
        ));
        assert!(!restored.get());

        owned.set(true);
        assert!(release_owned_focus(
            || owned.get(),
            || owned.set(false),
            || panic!("successful restoration must not clear focus"),
        ));
        assert!(!release_owned_focus(|| true, || {}, || {}));
    }

    #[test]
    fn panel_focus_borrowing_requires_an_active_panel_and_owned_editor() {
        assert_eq!(focus_action(true, false, true), FocusAction::Borrow);
        assert_eq!(focus_action(true, false, false), FocusAction::Keep);
        assert_eq!(focus_action(false, false, true), FocusAction::Keep);
        assert_eq!(focus_action(false, false, false), FocusAction::Keep);
        assert_eq!(focus_action(true, true, false), FocusAction::Keep);
        assert_eq!(focus_action(false, true, true), FocusAction::Restore);
        // Restore wiring even if another control has already taken focus;
        // restore_focus separately preserves that control's firstResponder.
        assert_eq!(focus_action(false, true, false), FocusAction::Restore);
    }

    #[test]
    fn checking_responder_exposes_writable_public_text_checking_properties() {
        use objc2::{ClassType, sel};
        for selector in [
            sel!(acceptsFirstResponder),
            sel!(checkingPanelFocusChanged:),
            sel!(setSmartInsertDeleteEnabled:),
            sel!(setContinuousSpellCheckingEnabled:),
            sel!(setGrammarCheckingEnabled:),
            sel!(setAutomaticSpellingCorrectionEnabled:),
            sel!(setAutomaticQuoteSubstitutionEnabled:),
            sel!(setAutomaticDashSubstitutionEnabled:),
            sel!(setAutomaticTextReplacementEnabled:),
            sel!(setAutomaticLinkDetectionEnabled:),
            sel!(setAutomaticDataDetectionEnabled:),
            sel!(setEnabledTextCheckingTypes:),
        ] {
            assert!(
                CheckingResponder::class()
                    .instance_method(selector)
                    .is_some(),
                "{selector}"
            );
        }
    }

    fn state_with_events() -> (State, mpsc::UnboundedReceiver<PanelEvent>) {
        let (events, receiver) = mpsc::unbounded();
        (
            State {
                generation: 7,
                events: Some(events),
                settings: TextCheckingPreferences::default(),
                editable: true,
                range: NSRange::new(0, 5),
                tag: 0,
            },
            receiver,
        )
    }

    #[test]
    fn panel_setters_emit_explicit_values_and_are_idempotent() {
        let (mut state, mut receiver) = state_with_events();
        state.settings.grammar = false;
        state.settings.quotes = false;
        state.update_settings([(TextCheckingSetting::Grammar, true)]);
        state.update_settings([(TextCheckingSetting::Grammar, true)]);
        state.update_settings([(TextCheckingSetting::Quotes, true)]);
        assert!(state.settings.grammar);
        assert!(state.settings.quotes);
        for setting in [TextCheckingSetting::Grammar, TextCheckingSetting::Quotes] {
            let event = receiver.try_recv().unwrap();
            assert_eq!(event.generation, 7);
            let PanelCommand::Settings(changes) = event.command else {
                panic!("expected settings");
            };
            assert_eq!(changes, vec![(setting, true)]);
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        state.events.take();
        state.update_settings([(TextCheckingSetting::Grammar, false)]);
        assert!(state.settings.grammar);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Closed)
        ));
    }

    #[test]
    fn checking_type_mask_updates_one_batch_and_preserves_smart_copy() {
        use objc2_foundation::NSTextCheckingType as Type;
        let (mut state, mut receiver) = state_with_events();
        let smart_copy = state.settings.smart_insert_delete;
        state.set_checking_types(Type::Grammar.0 | Type::Link.0 | Type::PhoneNumber.0);
        assert!(!state.settings.spelling);
        assert!(state.settings.grammar);
        assert!(!state.settings.correction);
        assert!(!state.settings.quotes);
        assert!(!state.settings.dashes);
        assert!(!state.settings.replacements);
        assert!(state.settings.links);
        assert!(state.settings.data_detectors);
        assert_eq!(state.settings.smart_insert_delete, smart_copy);
        assert!(matches!(
            receiver.try_recv().unwrap().command,
            PanelCommand::Settings(_)
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn panel_menu_labels_follow_native_visibility() {
        for (panel, show, hide) in [
            (
                CheckingPanel::Spelling,
                "command.show-spelling",
                "command.hide-spelling",
            ),
            (
                CheckingPanel::Substitutions,
                "command.show-substitutions",
                "command.hide-substitutions",
            ),
        ] {
            assert_eq!(panel.menu_label(false), show);
            assert_eq!(panel.menu_label(true), hide);
            assert_eq!(panel.menu_label(false), show);
        }
    }

    #[test]
    fn queued_panel_actions_keep_the_selection_generation_that_issued_them() {
        let (events, mut receiver) = mpsc::unbounded();
        let mut state = State {
            generation: 4,
            events: Some(events),
            settings: TextCheckingPreferences::default(),
            editable: true,
            range: NSRange::new(0, 5),
            tag: 0,
        };
        state.send(PanelCommand::Replace("old target".into()));
        state.generation = 5;
        state.send(PanelCommand::Replace("new target".into()));
        assert_eq!(receiver.try_recv().unwrap().generation, 4);
        assert_eq!(receiver.try_recv().unwrap().generation, 5);
        state.events.take();
        state.send(PanelCommand::Replace("cancelled".into()));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Closed)
        ));
    }
}
