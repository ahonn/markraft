//! Native prose checking and substitutions, guarded by the editor snapshot.
use super::*;
use crate::platform::checking_panel::{
    CheckingPanel, CheckingPanelSession, PanelCommand, PanelEvent, PanelSetting, SubstitutionKind,
};
use crate::platform::text_checking::{CheckKind, CheckOptions, SpellDocument, TextCheck};
use crate::storage::TextCheckingPreferences;
use markraft_gpui::ContextRequest;
use std::{collections::HashMap, ops::Range, rc::Rc, time::Duration};
use unicode_segmentation::UnicodeSegmentation;

// Native checking is synchronous and main-thread only. Bound each call and
// yield between chunks so typing and note switches can cancel a large scan.
const CHECK_CHUNK_CHARS: usize = 2048;
const SMART_CONTEXT_CHARS: usize = 1024;

#[derive(Default)]
pub(super) struct TextChecking {
    documents: HashMap<String, Rc<SpellDocument>>,
    generation: u64,
    panel: Option<PanelTarget>,
    panel_serial: u64,
}

struct PanelTarget {
    id: u64,
    generation: u64,
    checker: Rc<SpellDocument>,
    session: CheckingPanelSession,
    note: String,
    editor: WeakEntity<EditorView>,
    request: ContextRequest,
}

#[derive(Clone, Copy)]
pub(super) enum CheckingSetting {
    SmartInsertDelete,
    Spelling,
    Grammar,
    Correction,
    Quotes,
    Dashes,
    Replacements,
    Links,
    DataDetectors,
}

impl CheckingSetting {
    pub(super) fn value(self, settings: &mut TextCheckingPreferences) -> &mut bool {
        match self {
            Self::SmartInsertDelete => &mut settings.smart_insert_delete,
            Self::Spelling => &mut settings.spelling,
            Self::Grammar => &mut settings.grammar,
            Self::Correction => &mut settings.correction,
            Self::Quotes => &mut settings.quotes,
            Self::Dashes => &mut settings.dashes,
            Self::Replacements => &mut settings.replacements,
            Self::Links => &mut settings.links,
            Self::DataDetectors => &mut settings.data_detectors,
        }
    }
}

#[derive(Clone, Copy)]
struct TypingBoundary {
    caret: usize,
    completed_end: Option<usize>,
}

impl TypingBoundary {
    fn accepts(self, kind: CheckKind, end: usize) -> bool {
        match kind {
            CheckKind::Quote | CheckKind::Dash => {
                end == self.caret || Some(end) == self.completed_end
            }
            CheckKind::Correction | CheckKind::Replacement | CheckKind::Link => {
                Some(end) == self.completed_end
            }
            _ => false,
        }
    }
}

impl MarkraftApp {
    pub(super) fn checking_panel_label(&self, panel: CheckingPanel) -> &'static str {
        // Headless menus must never instantiate AppKit's shared panels.
        let visible = self.platform.is_some()
            && self
                .spell_document()
                .is_some_and(|checker| checker.panel_visible(panel));
        panel.menu_label(visible)
    }

    pub(in crate::app) fn cancel_checking_panel(&mut self) {
        if let Some(target) = self.context_menus.checking.panel.take() {
            target.checker.hide_panel(CheckingPanel::Spelling);
            target.checker.hide_panel(CheckingPanel::Substitutions);
        }
    }

    /// Follow deliberate selection changes, while invalidating already queued actions.
    pub(in crate::app) fn sync_checking_panel(&mut self, cx: &App) {
        let Some(target) = self.context_menus.checking.panel.as_ref() else {
            return;
        };
        let editor = self.editor();
        if (!target.checker.panel_visible(CheckingPanel::Spelling)
            && !target.checker.panel_visible(CheckingPanel::Substitutions))
            || target.note != self.library.active_id
            || target.editor.upgrade().as_ref() != Some(&editor)
            || self.interaction.panel() != Panel::Editor
        {
            self.cancel_checking_panel();
        } else if !editor.read(cx).context_is_current(&target.request) {
            self.refresh_checking_panel_target(cx);
        }
    }

    pub(super) fn show_checking_panel(
        &mut self,
        panel: CheckingPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use futures_util::StreamExt;
        self.sync_checking_panel(cx);
        self.ensure_spell_document();
        let Some(checker) = self.spell_document() else {
            return;
        };
        if checker.panel_visible(panel) {
            checker.hide_panel(panel);
            if !checker.panel_visible(CheckingPanel::Spelling)
                && !checker.panel_visible(CheckingPanel::Substitutions)
            {
                self.cancel_checking_panel();
            }
            return;
        }
        if self.context_menus.checking.panel.is_some() {
            checker.show_panel(panel);
            if matches!(panel, CheckingPanel::Spelling) {
                self.schedule_text_checking(false, true, cx);
            }
            return;
        }
        let editor = self.editor();
        let request = editor
            .read(cx)
            .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text);
        let editable = self.library.active_note().read_only.is_none() && !self.is_reloading();
        let Ok((session, mut commands)) = CheckingPanelSession::attach(
            window,
            &checker,
            self.preferences.text_checking,
            editable,
        ) else {
            return;
        };
        session.update(
            0,
            self.preferences.text_checking,
            editable,
            editor
                .read(cx)
                .context_text(&request)
                .map_or(0, |text| text.text.encode_utf16().count()),
        );
        self.context_menus.checking.panel_serial =
            self.context_menus.checking.panel_serial.wrapping_add(1);
        let id = self.context_menus.checking.panel_serial;
        self.context_menus.checking.panel = Some(PanelTarget {
            id,
            generation: 0,
            checker: checker.clone(),
            session,
            note: self.library.active_id.clone(),
            editor: editor.downgrade(),
            request,
        });
        checker.show_panel(panel);
        if matches!(panel, CheckingPanel::Spelling) {
            self.schedule_text_checking(false, true, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Some(command) = commands.next().await {
                if cx
                    .update(|window, cx| {
                        this.update(cx, |app, cx| {
                            app.handle_checking_panel(id, command, window, cx)
                        })
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_checking_panel(
        &mut self,
        id: u64,
        event: PanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event.command, PanelCommand::Focus) {
            if let Some(target) = self
                .context_menus
                .checking
                .panel
                .as_ref()
                .filter(|target| target.id == id)
            {
                target.session.sync_focus();
            }
            self.sync_checking_panel(cx);
            return;
        }
        let Some(target) = self
            .context_menus
            .checking
            .panel
            .as_ref()
            .filter(|target| target.id == id && target.generation == event.generation)
        else {
            return;
        };
        let editor = self.editor();
        if (!target.checker.panel_visible(CheckingPanel::Spelling)
            && !target.checker.panel_visible(CheckingPanel::Substitutions))
            || target.note != self.library.active_id
            || target.editor.upgrade().as_ref() != Some(&editor)
            || !editor.read(cx).context_is_current(&target.request)
        {
            self.cancel_checking_panel();
            return;
        }
        let request = target.request.clone();
        match event.command {
            PanelCommand::Focus => {
                unreachable!("focus notifications are handled before editor commands")
            }
            PanelCommand::Next => self.schedule_text_checking(false, true, cx),
            PanelCommand::Replace(text) => {
                if self.library.active_note().read_only.is_none() && !self.is_reloading() {
                    editor.update(cx, |editor, cx| {
                        editor.replace_context_text(&request, &text, cx);
                    });
                    self.refresh_checking_panel_target(cx);
                    self.schedule_text_checking(false, true, cx);
                }
            }
            PanelCommand::Ignore => {
                if let Some(text) = editor.read(cx).context_text(&request)
                    && let Some(checker) = self.spell_document()
                {
                    checker.ignore(&text.text);
                    self.schedule_text_checking(false, true, cx);
                }
            }
            PanelCommand::Settings(changes) => {
                self.apply_panel_settings(changes, window, cx);
                self.refresh_checking_panel_target(cx);
            }
            PanelCommand::Substitute { document } => {
                let request = if document {
                    editor.read(cx).context_document_snapshot()
                } else {
                    request
                };
                self.apply_selected_substitutions(&request, SubstitutionKind::All, cx);
                self.refresh_checking_panel_target(cx);
            }
        }
    }

    fn apply_panel_settings(
        &mut self,
        changes: Vec<(PanelSetting, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.preferences.text_checking;
        for (setting, value) in changes {
            let setting = match setting {
                PanelSetting::SmartInsertDelete => CheckingSetting::SmartInsertDelete,
                PanelSetting::Spelling => CheckingSetting::Spelling,
                PanelSetting::Grammar => CheckingSetting::Grammar,
                PanelSetting::Correction => CheckingSetting::Correction,
                PanelSetting::Quotes => CheckingSetting::Quotes,
                PanelSetting::Dashes => CheckingSetting::Dashes,
                PanelSetting::Text => CheckingSetting::Replacements,
                PanelSetting::Links => CheckingSetting::Links,
                PanelSetting::Data => CheckingSetting::DataDetectors,
            };
            *setting.value(&mut settings) = value;
        }
        if settings != self.preferences.text_checking {
            self.set_preference(crate::storage::Pref::TextChecking(settings), window, cx);
        }
    }

    fn refresh_checking_panel_target(&mut self, cx: &App) {
        let editor = self.editor();
        let request = editor
            .read(cx)
            .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text);
        let text = editor
            .read(cx)
            .context_text(&request)
            .map(|text| text.text)
            .unwrap_or_default();
        let editable = self.library.active_note().read_only.is_none() && !self.is_reloading();
        if let Some(target) = self.context_menus.checking.panel.as_mut() {
            // A preference refresh changes no editor target. Keep queued native
            // setters valid until the actual document or selection changes.
            if !editor.read(cx).context_is_current(&target.request) {
                target.generation = target.generation.wrapping_add(1);
            }
            target.request = request;
            target.session.update(
                target.generation,
                self.preferences.text_checking,
                editable,
                text.encode_utf16().count(),
            );
            if let Some(checker) = self.spell_document() {
                // A selection is not evidence of a misspelling. Only a completed
                // scan may put a word or grammar issue into the native panel.
                checker.update_panel("");
            }
        }
    }

    pub(super) fn apply_selected_substitutions(
        &mut self,
        request: &ContextRequest,
        kind: SubstitutionKind,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.library.active_note().read_only.is_some() || self.is_reloading() {
            return false;
        }
        self.ensure_spell_document();
        let Some(checker) = self.spell_document() else {
            return false;
        };
        let editor = self.editor();
        let Some(text) = editor.read(cx).context_text(request) else {
            return false;
        };
        let settings = self.preferences.text_checking;
        let options = match kind {
            SubstitutionKind::Quotes => CheckOptions {
                quotes: true,
                ..Default::default()
            },
            SubstitutionKind::Dashes => CheckOptions {
                dashes: true,
                ..Default::default()
            },
            SubstitutionKind::Text => CheckOptions {
                replacements: true,
                ..Default::default()
            },
            SubstitutionKind::Links => CheckOptions {
                links: true,
                ..Default::default()
            },
            SubstitutionKind::All => CheckOptions {
                quotes: settings.quotes,
                dashes: settings.dashes,
                replacements: settings.replacements,
                links: settings.links,
                ..Default::default()
            },
        };
        let mut checks = Vec::new();
        let characters = text.text.chars().collect::<Vec<_>>();
        for chunk in checking_chunks(&text.text, CHECK_CHUNK_CHARS) {
            let part: String = characters[chunk.clone()].iter().collect();
            let mut detected = checker.check(
                &part,
                CheckOptions {
                    links: true,
                    ..options
                },
            );
            exclude_url_edits(&mut detected, options.links);
            for mut check in detected {
                check.range = (check.range.start + chunk.start)..(check.range.end + chunk.start);
                checks.push(check);
            }
        }
        self.apply_checked_substitutions(request, checks, cx)
    }

    fn apply_checked_substitutions(
        &mut self,
        request: &ContextRequest,
        checks: Vec<TextCheck>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.library.active_note().read_only.is_some() || self.is_reloading() {
            return false;
        }
        let editor = self.editor();
        // Native checks address reading text, which may cross concealed source
        // or include code. Only independently writable prose results proceed;
        // the editor then validates and commits them as one transaction.
        let checks = checks
            .into_iter()
            .filter(|check| {
                editor
                    .read(cx)
                    .context_text_range(request, check.range.clone())
                    .is_some_and(|range| editor.read(cx).context_is_prose(&range))
            })
            .collect::<Vec<_>>();
        let edits = checks
            .iter()
            .filter_map(|check| {
                check
                    .replacement
                    .clone()
                    .map(|text| (check.range.clone(), text))
            })
            .collect();
        let links = checks
            .into_iter()
            .filter_map(|check| check.url.map(|url| (check.range, url)))
            .collect::<Vec<_>>();
        editor.update(cx, |editor, cx| {
            editor.apply_context_text_checks(request, edits, links, cx)
        })
    }

    pub(super) fn spell_document(&self) -> Option<Rc<SpellDocument>> {
        self.context_menus
            .checking
            .documents
            .get(&self.library.active_id)
            .cloned()
    }

    pub(super) fn ensure_spell_document(&mut self) {
        if self.platform.is_some()
            && !self
                .context_menus
                .checking
                .documents
                .contains_key(&self.library.active_id)
            && let Ok(document) = SpellDocument::new()
        {
            self.context_menus
                .checking
                .documents
                .insert(self.library.active_id.clone(), Rc::new(document));
        }
    }

    pub(super) fn toggle_text_checking(
        &mut self,
        setting: CheckingSetting,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.preferences.text_checking;
        let value = setting.value(&mut settings);
        *value = !*value;
        self.set_preference(crate::storage::Pref::TextChecking(settings), window, cx);
    }

    pub(in crate::app) fn schedule_text_checking(
        &mut self,
        typed: bool,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        self.context_menus.checking.generation =
            self.context_menus.checking.generation.wrapping_add(1);
        let generation = self.context_menus.checking.generation;
        if self.platform.is_none() || self.interaction.panel() != Panel::Editor {
            return;
        }
        self.ensure_spell_document();
        let Some(checker) = self.spell_document() else {
            return;
        };
        let editor = self.editor();
        let snapshot = editor.read(cx).context_document_snapshot();
        let settings = self.preferences.text_checking;
        let smart_enabled = settings.quotes
            || settings.dashes
            || settings.replacements
            || settings.correction
            || settings.links;
        let smart = (typed && smart_enabled)
            .then(|| typing_context(editor.read(cx)))
            .flatten();
        let note = self.library.active_id.clone();
        let weak = editor.downgrade();
        cx.spawn(async move |this, cx| {
            // Smart punctuation must be handled at the typing boundary, before
            // the longer spelling debounce lets the caret advance into a word.
            if let Some((request, boundary)) = smart {
                cx.background_executor()
                    .timer(Duration::from_millis(1))
                    .await;
                let text = this
                    .update(cx, |app, cx| {
                        if app.context_menus.checking.generation != generation
                            || app.library.active_id != note
                        {
                            return None;
                        }
                        weak.upgrade()?.read(cx).context_text(&request)
                    })
                    .ok()
                    .flatten();
                if let Some(text) =
                    text.filter(|text| text.text.chars().count() <= CHECK_CHUNK_CHARS)
                {
                    let mut checks = checker.check(
                        &text.text,
                        CheckOptions {
                            quotes: settings.quotes,
                            dashes: settings.dashes,
                            replacements: settings.replacements,
                            correction: settings.correction,
                            links: true,
                            ..Default::default()
                        },
                    );
                    // URL contents are never spelling-corrected or expanded as
                    // text snippets; link detection itself remains opt-in.
                    exclude_url_edits(&mut checks, settings.links);
                    let applied = this
                        .update(cx, |app, cx| {
                            if app.context_menus.checking.generation != generation
                                || app.library.active_id != note
                            {
                                return false;
                            }
                            let Some(editor) =
                                weak.upgrade().filter(|editor| *editor == app.editor())
                            else {
                                return false;
                            };
                            app.apply_substitution(&editor, &request, boundary, checks, cx)
                        })
                        .unwrap_or(false);
                    if applied {
                        return;
                    }
                }
            }
            if !explicit {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
            }
            let projection = this
                .update(cx, |app, cx| {
                    if app.context_menus.checking.generation != generation
                        || app.library.active_id != note
                    {
                        return None;
                    }
                    let editor = weak.upgrade().filter(|editor| *editor == app.editor())?;
                    editor
                        .read(cx)
                        .context_document_is_current(&snapshot)
                        .then(|| editor.read(cx).projection())
                })
                .ok()
                .flatten();
            let Some(projection) = projection else {
                return;
            };
            let mut diagnostics = Vec::new();
            if settings.spelling || settings.grammar || explicit {
                for (index, line) in projection.lines().iter().enumerate() {
                    let Some(text) = projection.line_text(index) else {
                        continue;
                    };
                    for chunk in checking_chunks(text, CHECK_CHUNK_CHARS) {
                        let (Some(from), Some(to)) = (
                            line.offset_to_pos(chunk.start),
                            line.offset_to_pos(chunk.end),
                        ) else {
                            continue;
                        };
                        let source = from..to;
                        let reading = this
                            .update(cx, |app, cx| {
                                if app.context_menus.checking.generation != generation
                                    || app.library.active_id != note
                                {
                                    return None;
                                }
                                let editor =
                                    weak.upgrade().filter(|editor| *editor == app.editor())?;
                                let editor = editor.read(cx);
                                if !editor.context_document_is_current(&snapshot) {
                                    return None;
                                }
                                Some(
                                    editor
                                        .context_source_range_snapshot(source.clone())
                                        .and_then(|request| editor.context_text(&request)),
                                )
                            })
                            .ok()
                            .flatten();
                        let Some(reading) = reading else {
                            return;
                        };
                        let Some(reading) = reading else {
                            continue;
                        };
                        if reading.text.chars().count() > CHECK_CHUNK_CHARS {
                            continue;
                        }
                        let mut checks = checker.check(
                            &reading.text,
                            CheckOptions {
                                spelling: settings.spelling || explicit,
                                grammar: settings.grammar,
                                links: true,
                                ..Default::default()
                            },
                        );
                        exclude_url_edits(&mut checks, false);
                        let ranges = this
                            .update(cx, |app, cx| {
                                if app.context_menus.checking.generation != generation
                                    || app.library.active_id != note
                                {
                                    return None;
                                }
                                let editor =
                                    weak.upgrade().filter(|editor| *editor == app.editor())?;
                                let editor = editor.read(cx);
                                if !editor.context_document_is_current(&snapshot) {
                                    return None;
                                }
                                // Rebuild with the current selection: moving the caret
                                // does not invalidate unchanged document diagnostics.
                                let request = editor.context_source_range_snapshot(source)?;
                                Some(
                                    checks
                                        .into_iter()
                                        .filter_map(|check| {
                                            if !matches!(
                                                check.kind,
                                                CheckKind::Spelling | CheckKind::Grammar
                                            ) {
                                                return None;
                                            }
                                            let range = editor.context_text_range(
                                                &request,
                                                check.range.clone(),
                                            )?;
                                            editor.context_is_prose(&range).then(|| {
                                                let grammar = (check.kind == CheckKind::Grammar)
                                                    .then(|| {
                                                        (
                                                            editor
                                                                .context_text(&range)
                                                                .map(|text| text.text)
                                                                .unwrap_or_default(),
                                                            check.detail.unwrap_or_default(),
                                                            check.suggestions,
                                                        )
                                                    });
                                                (range.text_range(), grammar)
                                            })
                                        })
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .ok()
                            .flatten();
                        let Some(ranges) = ranges else {
                            return;
                        };
                        diagnostics.extend(ranges);
                        cx.background_executor()
                            .timer(Duration::from_millis(1))
                            .await;
                    }
                }
            }
            let _ = this.update(cx, |app, cx| {
                if app.library.active_id != note
                    || app.context_menus.checking.generation != generation
                {
                    return;
                }
                let Some(editor) = weak.upgrade().filter(|editor| *editor == app.editor()) else {
                    return;
                };
                if !editor.read(cx).context_document_is_current(&snapshot) {
                    return;
                }
                let mut selected_grammar = None;
                let mut selected_spelling = None;
                if explicit {
                    let head = editor
                        .read(cx)
                        .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text)
                        .text_range()
                        .end;
                    if let Some((range, grammar)) = diagnostics
                        .iter()
                        .find(|(range, _)| range.start >= head)
                        .or_else(|| diagnostics.first())
                    {
                        selected_grammar = grammar.clone();
                        if grammar.is_none() {
                            let editor = editor.read(cx);
                            selected_spelling = editor
                                .context_source_range_snapshot(range.clone())
                                .and_then(|request| editor.context_text(&request))
                                .map(|text| text.text);
                        }
                        editor.update(cx, |editor, cx| {
                            editor.dispatch(
                                [markraft_core::TransactionSpec::new().selection(
                                    markraft_core::Selection::text(range.start, range.end),
                                )],
                                cx,
                            )
                        });
                    } else {
                        app.inform(Message::new("notice.spelling-complete"), cx);
                    }
                }
                editor.update(cx, |editor, cx| {
                    editor.set_text_diagnostics(
                        diagnostics.into_iter().map(|(range, _)| range).collect(),
                        cx,
                    )
                });
                if explicit {
                    app.refresh_checking_panel_target(cx);
                    if app.context_menus.checking.panel.is_some() {
                        if let Some((phrase, detail, suggestions)) = selected_grammar {
                            checker.update_grammar_panel(&phrase, &detail, &suggestions);
                        } else if let Some(word) = selected_spelling {
                            checker.update_panel(&word);
                        }
                    }
                }
            });
        })
        .detach();
    }

    fn apply_substitution(
        &mut self,
        editor: &Entity<EditorView>,
        paragraph: &ContextRequest,
        boundary: TypingBoundary,
        checks: Vec<TextCheck>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.library.active_note().read_only.is_some() || self.is_reloading() {
            return false;
        }
        for check in checks.into_iter().rev() {
            let Some(request) = editor
                .read(cx)
                .context_text_range(paragraph, check.range.clone())
            else {
                continue;
            };
            if !boundary.accepts(check.kind, request.text_range().end)
                || !editor.read(cx).context_is_prose(&request)
            {
                continue;
            }
            if let Some(replacement) = check.replacement {
                if editor.update(cx, |editor, cx| {
                    // Automated checks keep the typing caret, including a
                    // delimiter or a new paragraph after the corrected word.
                    editor.apply_context_text_checks(
                        paragraph,
                        vec![(check.range, replacement)],
                        Vec::new(),
                        cx,
                    )
                }) {
                    return true;
                }
            } else if let Some(url) = check.url
                && editor.update(cx, |editor, cx| editor.set_context_link(&request, &url, cx))
            {
                return true;
            }
        }
        false
    }
}

fn typing_context(editor: &EditorView) -> Option<(ContextRequest, TypingBoundary)> {
    editor.context_paragraph_snapshot_at_caret()?;
    let projection = editor.projection();
    let caret = editor.head();
    let (index, offset) = projection.pos_to_line_offset(caret)?;
    let (line_index, completed_end) = if offset == 0 && index > 0 {
        (index - 1, Some(projection.line(index - 1)?.to()))
    } else {
        let previous = projection
            .line_text(index)?
            .chars()
            .nth(offset.checked_sub(1)?);
        let completed = previous
            .filter(|character| is_word_boundary(*character))
            .and_then(|_| projection.line(index)?.offset_to_pos(offset - 1));
        (index, completed)
    };
    let line = projection.line(line_index)?;
    let end_offset = if line_index == index {
        offset
    } else {
        line.len()
    };
    let from = line.offset_to_pos(end_offset.saturating_sub(SMART_CONTEXT_CHARS))?;
    let to = line.offset_to_pos((end_offset + 2).min(line.len()))?;
    let request = editor.context_source_range_snapshot(from..to)?;
    Some((
        request,
        TypingBoundary {
            caret,
            completed_end,
        },
    ))
}

fn is_word_boundary(character: char) -> bool {
    !character.is_alphanumeric() && !matches!(character, '_' | '\'' | '’')
}

fn exclude_url_edits(checks: &mut Vec<TextCheck>, include_links: bool) {
    let links = checks
        .iter()
        .filter(|check| check.kind == CheckKind::Link)
        .map(|check| check.range.clone())
        .collect::<Vec<_>>();
    checks.retain(|check| {
        if check.kind == CheckKind::Link {
            return include_links;
        }
        !links
            .iter()
            .any(|link| check.range.start < link.end && link.start < check.range.end)
    });
}

/// Preserve complete Unicode words while bounding each native call. An isolated
/// token exceeding the limit is skipped instead of being diagnosed in fragments.
fn checking_chunks(text: &str, limit: usize) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for word in text.split_word_bounds() {
        let length = word.chars().count();
        if end - start + length > limit {
            if start < end {
                chunks.push(start..end);
            }
            start = end;
        }
        end += length;
        if length > limit {
            start = end;
        }
    }
    if start < end {
        chunks.push(start..end);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[::core::prelude::v1::test]
    fn substitutions_only_accept_the_current_typing_boundary() {
        let boundary = TypingBoundary {
            caret: 12,
            completed_end: Some(11),
        };
        assert!(boundary.accepts(CheckKind::Quote, 12));
        assert!(boundary.accepts(CheckKind::Replacement, 11));
        assert!(!boundary.accepts(CheckKind::Replacement, 12));
        assert!(!boundary.accepts(CheckKind::Correction, 10));
        assert!(
            !TypingBoundary {
                caret: 12,
                completed_end: None
            }
            .accepts(CheckKind::Correction, 11)
        );
        assert!(is_word_boundary(' '));
        assert!(is_word_boundary('.'));
        assert!(!is_word_boundary('a'));
        assert!(!is_word_boundary('中'));
        assert!(!is_word_boundary('’'));
    }

    #[::core::prelude::v1::test]
    fn checking_chunks_preserve_words_and_unicode_offsets() {
        assert_eq!(checking_chunks("hello world", 6), vec![0..6, 6..11]);
        let text = "中文 😀 cafe\u{301}";
        let ranges = checking_chunks(text, 6);
        assert!(ranges.iter().all(|range| range.len() <= 6));
        let rebuilt = ranges
            .into_iter()
            .map(|range| {
                text.chars()
                    .skip(range.start)
                    .take(range.len())
                    .collect::<String>()
            })
            .collect::<String>();
        assert_eq!(rebuilt, text);
        assert_eq!(
            checking_chunks("ok exceptionallylong end", 6),
            vec![0..3, 20..24]
        );
    }
    #[::core::prelude::v1::test]
    fn url_spans_do_not_receive_spelling_or_snippet_edits() {
        let make = |kind, range| TextCheck {
            kind,
            range,
            replacement: None,
            suggestions: Vec::new(),
            detail: None,
            url: None,
            data: None,
        };
        let mut checks = vec![
            make(CheckKind::Spelling, 0..4),
            make(CheckKind::Link, 5..25),
            make(CheckKind::Spelling, 8..14),
            make(CheckKind::Replacement, 18..22),
        ];
        exclude_url_edits(&mut checks, true);
        assert_eq!(
            checks.iter().map(|check| check.kind).collect::<Vec<_>>(),
            vec![CheckKind::Spelling, CheckKind::Link]
        );
        exclude_url_edits(&mut checks, false);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].range, 0..4);
    }
    fn replacement(range: Range<usize>, text: &str) -> TextCheck {
        TextCheck {
            kind: CheckKind::Replacement,
            range,
            replacement: Some(text.into()),
            suggestions: Vec::new(),
            detail: None,
            url: None,
            data: None,
        }
    }

    #[gpui::test]
    fn native_panel_settings_apply_explicit_values_without_losing_other_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        let h = crate::e2e::harness::open_with(cx, &[("note.md", "hello")], |preferences| {
            preferences.text_checking.grammar = false;
            preferences.text_checking.quotes = false;
        });
        let app = h.app.clone();
        h.cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                let before = app.preferences.text_checking;
                app.apply_panel_settings(vec![(PanelSetting::Grammar, true)], window, cx);
                app.apply_panel_settings(vec![(PanelSetting::Grammar, true)], window, cx);
                app.apply_panel_settings(vec![(PanelSetting::Quotes, true)], window, cx);
                let mut expected = before;
                expected.grammar = true;
                expected.quotes = true;
                assert_eq!(app.preferences.text_checking, expected);
                app.apply_panel_settings(
                    vec![(PanelSetting::Grammar, false), (PanelSetting::Text, false)],
                    window,
                    cx,
                );
                expected.grammar = false;
                expected.replacements = false;
                assert_eq!(app.preferences.text_checking, expected);
                assert_eq!(app.editor().read(cx).text(), "hello");
            });
        });
    }

    #[gpui::test]
    fn explicit_replacements_are_one_undo_and_preserve_unicode(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", "😀 hello -- world")], |_| {});
        h.select(1, 17);
        let applied = h.app.update(h.cx, |app, cx| {
            let request = app
                .editor()
                .read(cx)
                .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text);
            app.apply_checked_substitutions(
                &request,
                vec![replacement(2..7, "hi"), replacement(8..10, "—")],
                cx,
            )
        });
        assert!(applied);
        h.cx.run_until_parked();
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "😀 hi — world"
        );
        h.keys("cmd-z");
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "😀 hello -- world"
        );
    }

    #[gpui::test]
    fn explicit_substitutions_filter_code_and_nonliteral_ranges_in_mixed_selections(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "plain -- text `code -- text` &amp;\n\n```\nblock -- text\n```";
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", source)], |_| {});
        h.keys("cmd-a");
        let applied = h.app.update(h.cx, |app, cx| {
            let request = app
                .editor()
                .read(cx)
                .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text);
            let text = app.editor().read(cx).context_text(&request).unwrap();
            let characters = text.text.chars().collect::<Vec<_>>();
            let mut checks = characters
                .windows(2)
                .enumerate()
                .filter(|(_, pair)| *pair == ['-', '-'])
                .map(|(index, _)| replacement(index..index + 2, "—"))
                .collect::<Vec<_>>();
            assert_eq!(checks.len(), 3);
            let entity = characters.iter().position(|ch| *ch == '&').unwrap();
            checks.push(replacement(entity..entity + 1, "and"));
            app.apply_checked_substitutions(&request, checks, cx)
        });
        assert!(applied);
        assert_eq!(h.markdown(), source.replacen("--", "—", 1));
        h.keys("cmd-z");
        assert_eq!(h.markdown(), source);
    }

    #[gpui::test]
    fn explicit_replacements_reject_a_changed_selection(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", "hello -- world")], |_| {});
        h.select(1, 15);
        let request = h.app.update(h.cx, |app, cx| {
            app.editor()
                .read(cx)
                .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text)
        });
        h.select(1, 6);
        assert!(
            !h.app
                .update(h.cx, |app, cx| app.apply_checked_substitutions(
                    &request,
                    vec![replacement(6..8, "—")],
                    cx
                ))
        );
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "hello -- world"
        );
    }

    #[gpui::test]
    fn explicit_replacements_recheck_read_only_at_execution(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", "hello -- world")], |_| {});
        h.select(1, 15);
        let applied = h.app.update(h.cx, |app, cx| {
            let request = app
                .editor()
                .read(cx)
                .context_snapshot(Default::default(), markraft_gpui::ContextTarget::Text);
            let id = app.library.active_id.clone();
            app.library
                .update_read_only(&id, Some(Message::new("notice.text-service-rejected")));
            app.apply_checked_substitutions(&request, vec![replacement(6..8, "—")], cx)
        });
        assert!(!applied);
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "hello -- world"
        );
    }

    #[gpui::test]
    fn a_completed_snippet_preserves_the_delimiter_and_has_its_own_undo(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", "omw")], |_| {});
        h.select(4, 4);
        h.type_text(" ");
        let applied = h.app.update(h.cx, |app, cx| {
            let editor = app.editor();
            let (request, boundary) = typing_context(editor.read(cx)).expect("typed prose");
            app.apply_substitution(
                &editor,
                &request,
                boundary,
                vec![TextCheck {
                    kind: CheckKind::Replacement,
                    range: 0..3,
                    replacement: Some("on my way".into()),
                    suggestions: Vec::new(),
                    detail: None,
                    url: None,
                    data: None,
                }],
                cx,
            )
        });
        assert!(applied);
        h.cx.run_until_parked();
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "on my way "
        );
        h.keys("cmd-z");
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "omw "
        );
    }

    #[gpui::test]
    fn automatic_substitutions_keep_the_caret_for_continued_typing(cx: &mut gpui::TestAppContext) {
        for (source, caret, range, kind, result, continuation, expected) in [
            ("\"", 2, 0..1, CheckKind::Quote, "“", "hello", "“hello"),
            (
                "hello --",
                9,
                6..8,
                CheckKind::Dash,
                "—",
                " world",
                "hello — world",
            ),
            (
                "omw tail",
                5,
                0..3,
                CheckKind::Replacement,
                "on my way",
                "next",
                "on my way nexttail",
            ),
            ("**--**", 5, 0..2, CheckKind::Dash, "—", "x", "**—x**"),
        ] {
            let mut h = crate::e2e::harness::open_with(cx, &[("note.md", source)], |_| {});
            h.select(caret, caret);
            assert!(
                h.app.update(h.cx, |app, cx| {
                    let editor = app.editor();
                    let (request, boundary) =
                        typing_context(editor.read(cx)).expect("typing boundary");
                    let mut check = replacement(range, result);
                    check.kind = kind;
                    app.apply_substitution(&editor, &request, boundary, vec![check], cx)
                }),
                "{source}"
            );
            h.type_text(continuation);
            assert_eq!(h.markdown(), expected, "{source}");
        }
    }

    #[gpui::test]
    fn existing_links_do_not_cancel_other_substitutions_or_new_links(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "\"hello\" -- world\n\nhttps://example.com\n\n[example.org](https://custom.test)\n\nnew.example.org";
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", source)], |_| {});
        assert!(h.app.update(h.cx, |app, cx| {
            let editor = app.editor();
            let request = editor.read(cx).context_document_snapshot();
            let text = editor.read(cx).context_text(&request).unwrap().text;
            let mut checks = vec![
                replacement(0..1, "“"),
                replacement(6..7, "”"),
                replacement(8..10, "—"),
            ];
            for (label, url) in [
                ("https://example.com", "https://example.com"),
                ("example.org", "http://example.org"),
                ("new.example.org", "http://new.example.org"),
            ] {
                let start = text.find(label).unwrap();
                checks.push(TextCheck {
                    kind: CheckKind::Link,
                    range: start..start + label.len(),
                    replacement: None,
                    suggestions: Vec::new(),
                    detail: None,
                    url: Some(url.into()),
                    data: None,
                });
            }
            app.apply_checked_substitutions(&request, checks, cx)
        }));
        assert_eq!(
            h.markdown(),
            "“hello” — world\n\nhttps://example.com\n\n[example.org](https://custom.test)\n\n[new.example.org](http://new.example.org)"
        );
        h.keys("cmd-z");
        assert_eq!(h.markdown(), source);
    }

    #[gpui::test]
    fn return_completes_a_snippet_in_the_previous_paragraph(cx: &mut gpui::TestAppContext) {
        let mut h = crate::e2e::harness::open_with(cx, &[("note.md", "omw")], |_| {});
        h.select(4, 4);
        h.keys("enter");
        let applied = h.app.update(h.cx, |app, cx| {
            let editor = app.editor();
            let (request, boundary) = typing_context(editor.read(cx)).expect("paragraph boundary");
            app.apply_substitution(
                &editor,
                &request,
                boundary,
                vec![TextCheck {
                    kind: CheckKind::Replacement,
                    range: 0..3,
                    replacement: Some("on my way".into()),
                    suggestions: Vec::new(),
                    detail: None,
                    url: None,
                    data: None,
                }],
                cx,
            )
        });
        assert!(applied);
        h.cx.run_until_parked();
        assert_eq!(
            h.app
                .update(h.cx, |app, cx| app.editor().read(cx).text().to_owned()),
            "on my way\n"
        );
        h.type_text("next");
        assert_eq!(h.markdown(), "on my way\n\nnext");
    }
}
