//! One native popup belongs to one editor snapshot. Native tracking runs outside
//! GPUI updates; the returned command is validated before touching that editor.

use super::*;
use crate::platform::checking_panel::{CheckingPanel, SubstitutionKind};
use crate::platform::context_menu::{PreparedMenu, Row, TitleStyle};
use crate::platform::text_checking::{CheckKind, CheckOptions, SpellDocument};
use crate::platform::{
    text_requestor::{TextReplacement, TextServiceSession, TextServiceSnapshot},
    text_services::{self, OpenWithApplication, TextAnchor},
};
use crate::storage::TextCheckingSetting;
use editing::EditCommand;
use markraft_gpui::CopyFormat;
use markraft_gpui::{ContextRequest, ContextTarget, TextTransformation};

mod markdown;
use markdown::MarkdownAction;

#[derive(Default)]
pub(in crate::app) struct ContextMenus {
    next: u64,
    pending: Option<Session>,
    native: Option<NativeSession>,
    pub(super) checking: super::text_checking::TextChecking,
}

struct Session {
    id: u64,
    note: String,
    editor: WeakEntity<EditorView>,
    request: ContextRequest,
    actions: Vec<MenuAction>,
}

struct NativeSession {
    id: u64,
    note: String,
    editor: WeakEntity<EditorView>,
    request: ContextRequest,
    _requestor: Option<TextServiceSession>,
    _translation: Option<crate::platform::translation::Translation>,
}

#[derive(Clone)]
enum TextService {
    Lookup,
    Search,
    Share,
    Speak,
    StopSpeaking,
}

#[derive(Clone)]
enum MenuAction {
    Intent(Intent),
    Text(TextService, String),
    Transform(TextTransformation),
    PasteMatchStyle,
    Translate(String),
    Replace(ContextRequest, String),
    IgnoreSpelling(String),
    LearnSpelling(String),
    UnlearnSpelling(String),
    CheckSpelling,
    CheckingPanel(CheckingPanel),
    Substitute(SubstitutionKind),
    CheckSetting(TextCheckingSetting),
    OpenWith(String, OpenWithApplication),
    CopyUrl(String),
    OpenUrl(String),
    Wiki { target: String, embed: bool },
    CopyCode(usize),
    CodeLanguage(usize),
    CopyAs(CopyFormat),
    CopyTable(usize),
    Markdown(MarkdownAction),
    InsertParagraph { before: bool },
}

impl ContextMenus {
    fn dismiss_popup(&mut self) {
        self.pending = None;
        self.native = None;
    }

    pub(in crate::app) fn dismiss(&mut self) {
        self.dismiss_popup();
    }
}

struct MenuBuilder<'a> {
    app: &'a MarkraftApp,
    cx: &'a App,
    actions: Vec<MenuAction>,
}

impl MenuBuilder<'_> {
    fn spelling_word(&mut self, word: &str, learned: bool) -> Vec<Row> {
        if learned {
            vec![self.item(
                "command.unlearn-spelling",
                MenuAction::UnlearnSpelling(word.to_owned()),
                true,
                None,
            )]
        } else {
            vec![
                self.item(
                    "command.ignore-spelling",
                    MenuAction::IgnoreSpelling(word.to_owned()),
                    true,
                    None,
                ),
                self.item(
                    "command.learn-spelling",
                    MenuAction::LearnSpelling(word.to_owned()),
                    true,
                    None,
                ),
            ]
        }
    }

    fn item(
        &mut self,
        label: &str,
        action: MenuAction,
        enabled: bool,
        checked: Option<bool>,
    ) -> Row {
        self.literal(self.app.i18n.text(label), action, enabled, checked)
    }

    fn literal(
        &mut self,
        label: String,
        action: MenuAction,
        enabled: bool,
        checked: Option<bool>,
    ) -> Row {
        let id = self.actions.len();
        self.actions.push(action);
        Row::Item {
            id,
            label,
            enabled,
            checked,
            symbol: None,
            appearance: Default::default(),
        }
    }

    fn intent(&mut self, label: &str, intent: Intent) -> Row {
        let state = self.app.editing_state(&intent, self.cx);
        self.item(
            label,
            MenuAction::Intent(intent),
            state.is_none_or(|s| s.enabled),
            state.and_then(|s| s.checked),
        )
    }

    fn submenu(&self, label: &str, children: Vec<Row>) -> Row {
        Row::Submenu {
            label: self.app.i18n.text(label),
            enabled: true,
            children,
            symbol: None,
        }
    }
}

/// The rows that exist only for a selection with text in it.
#[derive(Default)]
struct SelectionRows {
    suggestions: Vec<Row>,
    lookup: Vec<Row>,
    share: Option<Row>,
    transformations: Option<Row>,
}

/// The groups of one context menu, each built in the order its actions are
/// numbered.
impl MenuBuilder<'_> {
    fn clipboard_rows(&mut self, editor: &EditorView) -> Vec<Row> {
        let mut rows: Vec<Row> = [EditCommand::Cut, EditCommand::Copy, EditCommand::Paste]
            .into_iter()
            .map(|command| self.intent(command.label(), Intent::Edit(command)))
            .collect();
        let paste_enabled = self
            .app
            .editing_state(&Intent::Edit(EditCommand::PastePlain), self.cx)
            .is_some_and(|state| state.enabled);
        rows.insert(
            3,
            self.item(
                "command.paste-match-style",
                MenuAction::PasteMatchStyle,
                paste_enabled,
                None,
            ),
        );
        let mut copies = [
            ("command.copy-as-plain-text", CopyFormat::PlainText),
            ("command.copy-as-markdown", CopyFormat::Markdown),
            ("command.copy-as-html", CopyFormat::HtmlCode),
            ("command.copy-without-theme", CopyFormat::RichText),
        ]
        .into_iter()
        .map(|(label, format)| {
            self.item(
                label,
                MenuAction::CopyAs(format),
                editor.can_copy_as(format),
                None,
            )
        })
        .collect::<Vec<_>>();
        copies.push(Row::Separator);
        copies.extend(
            [EditCommand::PastePlain, EditCommand::PasteMarkdown]
                .into_iter()
                .map(|command| self.intent(command.label(), Intent::Edit(command))),
        );
        rows.push(self.submenu("menu.copy-paste-as", copies));
        rows
    }

    /// Only styles Markdown can spell: the document format is not widened to
    /// carry fonts, colours or paragraph geometry.
    fn format_menu(&mut self) -> Row {
        let mut formats: Vec<Row> = [
            ("command.bold", doc::Inline::Bold, TitleStyle::Bold),
            ("command.italic", doc::Inline::Italic, TitleStyle::Italic),
            (
                "command.underline",
                doc::Inline::Underline,
                TitleStyle::Underline,
            ),
        ]
        .into_iter()
        .map(|(label, mark, style)| {
            self.intent(label, Intent::Mark(mark))
                .with_title_style(style)
        })
        .collect();
        formats.push(Row::Separator);
        formats.extend(
            [
                ("command.strikethrough", doc::Inline::Strikethrough),
                ("command.inline-code", doc::Inline::Code),
                ("command.highlight", doc::Inline::Highlight),
            ]
            .into_iter()
            .map(|(label, mark)| self.intent(label, Intent::Mark(mark))),
        );
        formats.push(self.intent("command.link", Intent::Link));
        formats.push(Row::Separator);
        formats.push(self.markdown("command.clear-format", MarkdownAction::ClearFormatting));
        self.submenu("menu.format", formats)
            .with_symbol("textformat")
    }

    fn markdown(&mut self, label: &str, action: MarkdownAction) -> Row {
        let enabled = self.app.markdown_action_enabled(action, self.cx);
        self.item(label, MenuAction::Markdown(action), enabled, None)
    }

    fn paragraph_menu(&mut self) -> Row {
        let paragraph = [
            ("command.heading-1", doc::Block::Heading(1)),
            ("command.heading-2", doc::Block::Heading(2)),
            ("command.heading-3", doc::Block::Heading(3)),
            ("command.heading-4", doc::Block::Heading(4)),
            ("command.heading-5", doc::Block::Heading(5)),
            ("command.heading-6", doc::Block::Heading(6)),
            ("command.paragraph", doc::Block::Paragraph),
            ("command.quote", doc::Block::Quote),
            ("command.ordered-list", doc::Block::Ordered),
            ("command.bullet-list", doc::Block::Bullet),
            ("command.task-list", doc::Block::Task),
        ]
        .into_iter()
        .map(|(label, block)| self.intent(label, Intent::Block(block)))
        .collect();
        self.submenu("menu.paragraph", paragraph)
    }

    fn insert_menu(
        &mut self,
        editor: &EditorView,
        request: &ContextRequest,
        writable: bool,
    ) -> Row {
        let mut inserts = vec![
            self.markdown("command.insert-image", MarkdownAction::InsertImage),
            self.intent("command.code-block", Intent::Block(doc::Block::Code)),
            self.markdown("command.math-block", MarkdownAction::InsertMath),
            self.intent("command.table", Intent::InsertTable),
            self.intent("command.divider", Intent::Block(doc::Block::Divider)),
            Row::Separator,
        ];
        for (label, before) in [
            ("command.insert-paragraph-before", true),
            ("command.insert-paragraph-after", false),
        ] {
            inserts.push(self.item(
                label,
                MenuAction::InsertParagraph { before },
                writable && editor.can_insert_context_paragraph(request, before),
                None,
            ));
        }
        self.submenu("menu.insert", inserts)
    }

    /// Commands for the object under the pointer.
    fn target_rows(
        &mut self,
        editor: &EditorView,
        request: &ContextRequest,
        writable: bool,
    ) -> Vec<Row> {
        let mut contextual = Vec::new();
        match &request.target {
            ContextTarget::Link { url, range } => {
                contextual.push(self.item(
                    "command.open-link",
                    MenuAction::OpenUrl(url.clone()),
                    true,
                    None,
                ));
                contextual.push(self.item(
                    "command.copy-link",
                    MenuAction::CopyUrl(url.clone()),
                    true,
                    None,
                ));
                // A selection crossing other text must not be moved just to make
                // an object command work. Only offer edits for its active link.
                let selection = request.selection_range();
                if selection.start >= range.start
                    && selection.end <= range.end
                    && editor.active_link().as_ref() == Some(url)
                {
                    contextual.push(self.intent("surfaces.link.edit", Intent::EditLink));
                    contextual.push(self.intent("command.remove-link", Intent::Unlink));
                }
                if self.app.platform.is_some() {
                    let children = text_services::applications_for_url(url)
                        .into_iter()
                        .map(|application| {
                            let label = application.name.clone();
                            self.literal(
                                label,
                                MenuAction::OpenWith(url.clone(), application),
                                true,
                                None,
                            )
                        })
                        .collect::<Vec<_>>();
                    if !children.is_empty() {
                        contextual.push(self.submenu("menu.open-with", children));
                    }
                }
            }
            ContextTarget::WikiLink { target, embed, .. } => {
                contextual.push(self.item(
                    "command.open-link",
                    MenuAction::Wiki {
                        target: target.clone(),
                        embed: *embed,
                    },
                    true,
                    None,
                ));
                contextual.push(self.item(
                    "command.copy-link",
                    MenuAction::CopyUrl(target.clone()),
                    true,
                    None,
                ));
            }
            ContextTarget::CodeBlock { pos } => {
                contextual.push(self.item(
                    "command.copy-code-block",
                    MenuAction::CopyCode(*pos),
                    editor.code_text_at(*pos).is_some(),
                    None,
                ));
                contextual.push(self.item(
                    "command.choose-code-language",
                    MenuAction::CodeLanguage(*pos),
                    writable,
                    None,
                ));
            }
            ContextTarget::Table { pos, cell } => {
                contextual.push(self.table_menu(editor, *pos, *cell));
            }
            ContextTarget::Text | ContextTarget::Image { .. } | ContextTarget::Math { .. } => {}
        }
        contextual
    }

    fn table_menu(&mut self, editor: &EditorView, pos: usize, cell: usize) -> Row {
        let state = editor.state();
        let selection = state.selection();
        let in_cell = |position| {
            state.doc().resolve(position).ok().is_some_and(|resolved| {
                (1..=resolved.depth()).any(|depth| resolved.before(depth) == cell)
            })
        };
        let mut table = Vec::new();
        if in_cell(selection.from(state.doc())) && in_cell(selection.to(state.doc())) {
            table = [
                ("command.add-row-above", TableEdit::RowBefore),
                ("command.add-row-below", TableEdit::RowAfter),
                ("command.add-column-left", TableEdit::ColumnBefore),
                ("command.add-column-right", TableEdit::ColumnAfter),
                (
                    "command.align-column-left",
                    TableEdit::Align(ColumnAlignment::Left),
                ),
                (
                    "command.align-column-center",
                    TableEdit::Align(ColumnAlignment::Center),
                ),
                (
                    "command.align-column-right",
                    TableEdit::Align(ColumnAlignment::Right),
                ),
                ("command.delete-row", TableEdit::DeleteRow),
                ("command.delete-column", TableEdit::DeleteColumn),
                ("command.delete-table", TableEdit::DeleteTable),
            ]
            .into_iter()
            .map(|(label, edit)| self.intent(label, Intent::Table(edit)))
            .collect();
            table.push(Row::Separator);
        }
        table.push(self.item(
            "command.copy-table",
            MenuAction::CopyTable(pos),
            editor.can_copy_table_at(pos),
            None,
        ));
        self.submenu("command.table", table)
    }

    fn setting(&mut self, label: &str, setting: TextCheckingSetting) -> Row {
        let value = self.app.preferences.text_checking.get(setting);
        self.item(label, MenuAction::CheckSetting(setting), true, Some(value))
    }

    fn panel(&mut self, panel: CheckingPanel) -> Row {
        self.item(
            self.app.checking_panel_label(panel),
            MenuAction::CheckingPanel(panel),
            true,
            None,
        )
    }

    fn spelling_menu(&mut self) -> Row {
        let mut checking = vec![
            self.panel(CheckingPanel::Spelling),
            self.item(
                "command.check-spelling",
                MenuAction::CheckSpelling,
                true,
                None,
            ),
            Row::Separator,
        ];
        for (label, setting) in [
            ("command.check-continuously", TextCheckingSetting::Spelling),
            ("command.check-grammar", TextCheckingSetting::Grammar),
            ("command.correct-spelling", TextCheckingSetting::Correction),
        ] {
            checking.push(self.setting(label, setting));
        }
        self.submenu("menu.spelling", checking)
    }

    /// Substitutions validate each detected reading range separately. A
    /// concealed delimiter between ranges only prevents whole-text Services
    /// replacement; it must not disable edits inside those ranges.
    fn substitutions_menu(&mut self, has_text: bool, writable: bool) -> Row {
        let mut rows = Vec::new();
        if has_text {
            rows.extend(
                [
                    ("command.replace-quotes", SubstitutionKind::Quotes),
                    ("command.replace-dashes", SubstitutionKind::Dashes),
                    ("command.add-links", SubstitutionKind::Links),
                    ("command.replace-text", SubstitutionKind::Text),
                ]
                .into_iter()
                .map(|(label, kind)| {
                    self.item(label, MenuAction::Substitute(kind), writable, None)
                }),
            );
            rows.push(Row::Separator);
        }
        rows.extend([self.panel(CheckingPanel::Substitutions), Row::Separator]);
        for (label, setting) in [
            (
                "command.smart-copy-paste",
                TextCheckingSetting::SmartInsertDelete,
            ),
            ("command.smart-quotes", TextCheckingSetting::Quotes),
            ("command.smart-dashes", TextCheckingSetting::Dashes),
            ("command.smart-links", TextCheckingSetting::Links),
            ("command.data-detection", TextCheckingSetting::DataDetectors),
            (
                "command.text-replacement",
                TextCheckingSetting::Replacements,
            ),
        ] {
            rows.push(self.setting(label, setting));
        }
        self.submenu("menu.substitutions", rows)
    }

    fn selection_rows(
        &mut self,
        editor: &EditorView,
        request: &ContextRequest,
        text: &markraft_gpui::ContextText,
        writable: bool,
    ) -> SelectionRows {
        let mut suggestions = Vec::new();
        if let Some(checker) = self.app.spell_document()
            && text.text.chars().count() <= 2048
            && editor.context_is_prose(request)
        {
            suggestions = self.checked_rows(editor, request, &text.text, &checker, writable);
            if self.app.preferences.text_checking.data_detectors {
                suggestions.extend(self.detected_data_row(editor, request, &checker));
            }
        }
        let detected = suggestions
            .iter()
            .any(|row| matches!(row, Row::DetectedData { .. }));
        SelectionRows {
            suggestions,
            lookup: self.lookup_rows(&text.text, detected),
            share: Some(
                self.item(
                    "command.share-text",
                    MenuAction::Text(TextService::Share, text.text.clone()),
                    true,
                    None,
                )
                .with_symbol("square.and.arrow.up"),
            ),
            transformations: self.transformations_menu(text, writable),
        }
    }

    /// Spelling and grammar results for the selected text, with the guesses
    /// and dictionary commands for each.
    fn checked_rows(
        &mut self,
        editor: &EditorView,
        request: &ContextRequest,
        text: &str,
        checker: &SpellDocument,
        writable: bool,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        // Learned words produce no spelling result. Query the native
        // dictionary separately so users can reverse Learn Spelling.
        if checker.has_learned(text) {
            rows.extend(self.spelling_word(text, true));
        }
        let checks = checker.check(
            text,
            CheckOptions {
                spelling: true,
                grammar: self.app.preferences.text_checking.grammar,
                links: true,
                data_detectors: false,
                ..Default::default()
            },
        );
        let urls = checks
            .iter()
            .filter(|check| check.kind == CheckKind::Link)
            .map(|check| check.range.clone())
            .collect::<Vec<_>>();
        for check in checks {
            let Some(narrowed) = editor.context_text_range(request, check.range.clone()) else {
                continue;
            };
            if matches!(check.kind, CheckKind::Spelling | CheckKind::Grammar)
                && !urls
                    .iter()
                    .any(|range| range.start < check.range.end && check.range.start < range.end)
            {
                if let Some(detail) = check.detail {
                    rows.push(self.literal(detail, MenuAction::CheckSpelling, false, None));
                }
                let guesses = if check.kind == CheckKind::Spelling {
                    checker.guesses(text, check.range.clone())
                } else {
                    check.suggestions
                };
                for suggestion in guesses.into_iter().take(5) {
                    rows.push(self.literal(
                        suggestion.clone(),
                        MenuAction::Replace(narrowed.clone(), suggestion),
                        writable,
                        None,
                    ));
                }
                if check.kind == CheckKind::Spelling {
                    let word = text
                        .chars()
                        .skip(check.range.start)
                        .take(check.range.end - check.range.start)
                        .collect::<String>();
                    rows.extend(self.spelling_word(&word, false));
                }
            }
            if self.app.preferences.text_checking.data_detectors
                && !matches!(request.target, ContextTarget::Link { .. })
                && let Some(url) = check.url
            {
                rows.push(self.item("command.open-link", MenuAction::OpenUrl(url), true, None));
            }
        }
        rows
    }

    /// The first date, address or similar in the selection's paragraph that
    /// the selection overlaps.
    fn detected_data_row(
        &self,
        editor: &EditorView,
        request: &ContextRequest,
        checker: &SpellDocument,
    ) -> Option<Row> {
        let selection = request.selection_range();
        let projection = editor.projection();
        let (index, _) = projection.pos_to_line_offset(selection.start)?;
        let line = projection.line(index).filter(|line| line.len() <= 2048)?;
        let paragraph = editor.context_source_range_snapshot(line.from()..line.to())?;
        let reading = editor.context_text(&paragraph)?;
        let options = CheckOptions {
            data_detectors: true,
            ..Default::default()
        };
        checker
            .check(&reading.text, options)
            .into_iter()
            .find_map(|check| {
                let result = check.data?;
                let detected = editor
                    .context_text_range(&paragraph, check.range)
                    .filter(|detected| editor.context_is_prose(detected))?;
                let range = detected.text_range();
                (range.start < selection.end && range.end > selection.start)
                    .then_some(Row::DetectedData { result })
            })
    }

    fn lookup_rows(&mut self, text: &str, detected: bool) -> Vec<Row> {
        let mut lookup = Vec::new();
        if !detected {
            let snippet = crate::platform::context_menu::selection_label(text);
            let app = self.app;
            lookup.push(
                self.literal(
                    app.i18n
                        .text_with("command.look-up-selection", &[("text", &snippet)]),
                    MenuAction::Text(TextService::Lookup, text.to_owned()),
                    true,
                    None,
                )
                .with_symbol("info.circle"),
            );
            if app.platform.is_some() && crate::platform::translation::available() {
                lookup.push(
                    self.literal(
                        app.i18n
                            .text_with("command.translate-selection", &[("text", &snippet)]),
                        MenuAction::Translate(text.to_owned()),
                        true,
                        None,
                    )
                    .with_symbol("translate"),
                );
            }
            lookup.push(Row::Separator);
        }
        lookup.push(
            self.item(
                "command.search-web",
                MenuAction::Text(TextService::Search, text.to_owned()),
                true,
                None,
            )
            .with_symbol("magnifyingglass"),
        );
        lookup
    }

    fn transformations_menu(
        &mut self,
        text: &markraft_gpui::ContextText,
        writable: bool,
    ) -> Option<Row> {
        let transforms: Vec<_> = TextTransformation::available_for(&text.text)
            .into_iter()
            .map(|transform| {
                let label = match transform {
                    TextTransformation::Uppercase => "command.uppercase",
                    TextTransformation::Lowercase => "command.lowercase",
                    TextTransformation::Capitalize => "command.capitalize",
                    TextTransformation::TraditionalChinese => "command.traditional-chinese",
                    TextTransformation::SimplifiedChinese => "command.simplified-chinese",
                };
                self.item(
                    label,
                    MenuAction::Transform(transform),
                    writable && text.transformable,
                    None,
                )
            })
            .collect();
        (!transforms.is_empty()).then(|| self.submenu("menu.transformations", transforms))
    }

    fn speech_menu(&mut self, text: String) -> Row {
        let speech = vec![
            self.item(
                "command.start-speaking",
                MenuAction::Text(TextService::Speak, text.clone()),
                !text.trim().is_empty(),
                None,
            ),
            self.item(
                "command.stop-speaking",
                MenuAction::Text(TextService::StopSpeaking, String::new()),
                self.app.platform.is_some() && text_services::is_speaking(),
                None,
            ),
        ];
        self.submenu("menu.speech", speech)
    }
}

impl MarkraftApp {
    fn context_menu_rows(&self, request: &ContextRequest, cx: &App) -> (Vec<Row>, Vec<MenuAction>) {
        let mut menu = MenuBuilder {
            app: self,
            cx,
            actions: Vec::new(),
        };
        let editor = self.editor().read(cx);
        let writable = !self.is_reloading()
            && self.library.active_note().read_only.is_none()
            && !editor.is_composing();
        // One reading of the request serves every group that asks about it.
        let reading = editor.context_text(request);
        let has_text = reading.as_ref().is_some_and(|text| !text.text.is_empty());
        let selected = reading.filter(|text| !text.text.trim().is_empty());

        let clipboard = menu.clipboard_rows(editor);
        let format = menu.format_menu();
        let paragraph = menu.paragraph_menu();
        let insert = menu.insert_menu(editor, request, writable);
        let contextual = menu.target_rows(editor, request, writable);
        let spelling = menu.spelling_menu();
        let substitutions = menu.substitutions_menu(has_text, writable);
        let selection = selected
            .as_ref()
            .map(|text| menu.selection_rows(editor, request, text, writable))
            .unwrap_or_default();
        let speech_text = selected
            .or_else(|| editor.context_text(&editor.context_document_snapshot()))
            .map(|text| text.text)
            .unwrap_or_default();
        let speech = menu.speech_menu(speech_text);

        let mut result = Vec::new();
        for group in [
            selection.suggestions,
            contextual,
            selection.lookup,
            clipboard,
        ] {
            if !group.is_empty() {
                if !result.is_empty() {
                    result.push(Row::Separator);
                }
                result.extend(group);
            }
        }
        if let Some(share) = selection.share {
            result.push(Row::Separator);
            result.push(share);
        }
        result.extend([
            Row::Separator,
            paragraph,
            format,
            insert,
            Row::WritingTools,
            Row::Separator,
            spelling,
            substitutions,
        ]);
        result.extend(selection.transformations);
        result.push(speech);
        (result, menu.actions)
    }

    pub(in crate::app) fn request_context_menu(
        &mut self,
        editor: &Entity<EditorView>,
        request: ContextRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor() != *editor
            || self.interaction.panel() != Panel::Editor
            || !editor.read(cx).context_is_current(&request)
        {
            return;
        }
        // A secondary click on our nonactivating NSPanel does not make the app
        // key like a primary click does. Activate before native menu tracking so
        // keyboard navigation and Escape belong to the popup immediately.
        if self.platform.is_some() {
            cx.activate(true);
            window.activate_window();
        }
        self.close_popover(cx);
        self.context_menus.dismiss_popup();
        self.ensure_spell_document();
        let (rows, actions) = self.context_menu_rows(&request, cx);
        self.context_menus.next = self.context_menus.next.wrapping_add(1);
        let id = self.context_menus.next;
        let position = request.position;
        self.context_menus.pending = Some(Session {
            id,
            note: self.library.active_id.clone(),
            editor: editor.downgrade(),
            request,
            actions,
        });
        // Headless tests exercise the same pending session and completion path.
        if self.platform.is_none() {
            return;
        }
        // AppKit tracking can suspend GPUI presentation. Let the pointer's new
        // selection paint before reading its anchor and tracking the popup.
        let (painted, ready) = futures_channel::oneshot::channel();
        window.on_next_frame(move |_, _| {
            let _ = painted.send(());
        });
        cx.spawn_in(window, async move |this, cx| {
            if ready.await.is_err() {
                return;
            }
            let prepared = cx
                .update(|window, cx| {
                    this.update(cx, |app, cx| {
                        if !app.context_menu_current(id, cx) {
                            return None;
                        }
                        app.attach_text_services(id, window, cx);
                        let writing_tools = app
                            .context_menus
                            .native
                            .as_ref()
                            .and_then(|session| session._requestor.as_ref())
                            .and_then(TextServiceSession::writing_tools_items);
                        match PreparedMenu::new(
                            &rows,
                            window,
                            app.text_service_bounds(position, cx),
                            writing_tools,
                        ) {
                            Ok(menu) => Some(menu),
                            Err(error) => {
                                app.context_menus.dismiss_popup();
                                app.feedback.set_error(error);
                                cx.notify();
                                None
                            }
                        }
                    })
                })
                .ok()
                .and_then(Result::ok)
                .flatten();
            let Some(prepared) = prepared else {
                return;
            };
            // No App, Window or entity borrow crosses AppKit's nested event loop.
            let selected = prepared.show(position);
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |app, cx| {
                    app.finish_context_menu(id, selected, window, cx)
                });
            });
        })
        .detach();
    }

    fn text_service_bounds(&self, position: Point<Pixels>, cx: &App) -> Bounds<Pixels> {
        let editor = self.editor().read(cx);
        editor.anchor_bounds().unwrap_or_else(|| {
            let style = editor.style();
            Bounds::new(
                position,
                size(px(1.), style.body_size * style.line_height_ratio),
            )
        })
    }

    fn attach_text_services(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let session = self.context_menus.pending.as_ref().expect("a pending menu");
        let Some(text) = self.editor().read(cx).context_text(&session.request) else {
            return;
        };
        let editable = text.replaceable && self.library.active_note().read_only.is_none();
        let prose = self.editor().read(cx).context_is_prose(&session.request);
        let anchor = self.text_service_bounds(session.request.position, cx);
        let Ok((requestor, returned)) = TextServiceSession::attach(
            window,
            TextServiceSnapshot {
                text: text.text,
                editable,
                prose,
            },
            anchor,
        ) else {
            return;
        };
        self.context_menus.native = Some(NativeSession {
            id,
            note: session.note.clone(),
            editor: session.editor.clone(),
            request: session.request.clone(),
            _requestor: Some(requestor),
            _translation: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(replacement) = returned.await else {
                return;
            };
            let _ = this.update(cx, |app, cx| {
                if app
                    .context_menus
                    .native
                    .as_ref()
                    .is_none_or(|session| session.id != id)
                {
                    return;
                }
                let session = app.context_menus.native.take().expect("the active service");
                if !app.apply_service_replacement(
                    &session.note,
                    &session.editor,
                    &session.request,
                    replacement,
                    cx,
                ) {
                    app.inform(Message::new("notice.text-service-rejected"), cx);
                }
            });
        })
        .detach();
    }

    fn apply_service_text(
        &mut self,
        note: &str,
        editor: &WeakEntity<EditorView>,
        request: &ContextRequest,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_service_replacement(
            note,
            editor,
            request,
            TextReplacement::PreserveStyles(text.to_owned()),
            cx,
        )
    }

    fn apply_service_replacement(
        &mut self,
        note: &str,
        editor: &WeakEntity<EditorView>,
        request: &ContextRequest,
        replacement: TextReplacement,
        cx: &mut Context<Self>,
    ) -> bool {
        use markraft_core::kind::ReadingReplacementPolicy;

        if note != self.library.active_id
            || self.interaction.panel() != Panel::Editor
            || self.is_reloading()
            || self.library.active_note().read_only.is_some()
        {
            return false;
        }
        let Some(editor) = editor.upgrade().filter(|editor| *editor == self.editor()) else {
            return false;
        };
        let (text, policy) = match replacement {
            TextReplacement::PlainText(text) => {
                (text, ReadingReplacementPolicy::InheritSelectionStart)
            }
            TextReplacement::PreserveStyles(text) => {
                (text, ReadingReplacementPolicy::PreserveUnchanged)
            }
        };
        editor.update(cx, |editor, cx| {
            // A plain service can return identical text but replace its styles.
            // Only source-preserving replacements may skip that transaction.
            (policy == ReadingReplacementPolicy::PreserveUnchanged
                && editor
                    .context_text(request)
                    .is_some_and(|selected| selected.text == text))
                || editor.replace_context_text_with_policy(request, &text, policy, cx)
        })
    }

    pub(in crate::app) fn invalidate_text_service(&mut self, cx: &App) {
        if self.context_menus.native.as_ref().is_some_and(|session| {
            session.note != self.library.active_id
                || !session.editor.upgrade().is_some_and(|editor| {
                    editor == self.editor() && editor.read(cx).context_is_current(&session.request)
                })
        }) {
            self.context_menus.native = None;
        }
    }

    fn present_text_service(
        &mut self,
        service: TextService,
        text: String,
        request: &ContextRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(service, TextService::Search) {
            let mut url =
                url::Url::parse("https://www.google.com/search").expect("static search URL");
            url.query_pairs_mut().append_pair("q", &text);
            cx.open_url(url.as_str());
            return;
        }
        if matches!(service, TextService::StopSpeaking) {
            text_services::stop_speaking();
            return;
        }
        let presentation = matches!(service, TextService::Lookup)
            .then(|| {
                self.editor()
                    .read(cx)
                    .context_text_presentation(request, cx)
            })
            .flatten();
        let anchor = match TextAnchor::new(window, self.text_service_bounds(request.position, cx)) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.feedback.set_error(error);
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = match service {
                TextService::Lookup => presentation.as_ref().map_or_else(
                    || Err(Message::new("error.native-window-control")),
                    |presentation| anchor.show_definition(presentation),
                ),
                TextService::Share => anchor.share(&text),
                TextService::Speak => text_services::start_speaking(&text),
                _ => Ok(()),
            };
            if let Err(error) = result {
                let _ = this.update(cx, |app, cx| {
                    app.feedback.set_error(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn context_menu_current(&self, id: u64, cx: &App) -> bool {
        self.context_menus.pending.as_ref().is_some_and(|session| {
            session.id == id
                && session.note == self.library.active_id
                && self.interaction.panel() == Panel::Editor
                && !self.is_reloading()
                && session.editor.upgrade().is_some_and(|editor| {
                    editor == self.editor() && editor.read(cx).context_is_current(&session.request)
                })
        })
    }

    /// Run a menu's intent through the app's own dispatch. An edit is the
    /// menu's own undo step, apart from the typing around it.
    fn run_menu_intent(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            // A native popup holds keyboard focus, so the clipboard command
            // goes to this editor rather than through the focused responder.
            Intent::Edit(command) => {
                if self.editing_enabled(&Intent::Edit(command), cx) {
                    self.editor().update(cx, |editor, cx| {
                        editor.execute_context_action(command.into(), cx)
                    });
                    self.focus_editor(window, cx);
                }
            }
            Intent::Mark(_)
            | Intent::Block(_)
            | Intent::InsertTable
            | Intent::Table(_)
            | Intent::Unlink => {
                let editor = self.editor();
                let previous = editor.update(cx, |editor, _| editor.set_context_edit(true));
                self.intent(intent, window, cx);
                editor.update(cx, |editor, _| editor.set_context_edit(previous));
            }
            other => self.intent(other, window, cx),
        }
    }

    fn update_dictionary(&mut self, update: impl FnOnce(&SpellDocument), cx: &mut Context<Self>) {
        if let Some(checker) = self.spell_document() {
            update(&checker);
        }
        self.schedule_text_checking(CheckTrigger::Changed, cx);
    }

    /// Present the system translation of `text`. The session stays alive
    /// until the popover returns, and a returned replacement is written only
    /// if that session is still the active one.
    fn show_translation(
        &mut self,
        id: u64,
        session: Session,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editable = self.library.active_note().read_only.is_none()
            && self
                .editor()
                .read(cx)
                .context_text(&session.request)
                .is_some_and(|text| text.replaceable);
        let shown = crate::platform::translation::Translation::show(
            window,
            self.text_service_bounds(session.request.position, cx),
            text,
            editable,
        );
        let (translation, returned) = match shown {
            Ok(shown) => shown,
            Err(error) => {
                self.feedback.set_error(error);
                cx.notify();
                return;
            }
        };
        self.context_menus.native = Some(NativeSession {
            id,
            note: session.note,
            editor: session.editor,
            request: session.request,
            _requestor: None,
            _translation: Some(translation),
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = returned.await else {
                return;
            };
            let _ = this.update(cx, |app, cx| {
                if app
                    .context_menus
                    .native
                    .as_ref()
                    .is_none_or(|session| session.id != id)
                {
                    return;
                }
                let session = app
                    .context_menus
                    .native
                    .take()
                    .expect("the active translation");
                if let Some(text) = result
                    && !app.apply_service_text(
                        &session.note,
                        &session.editor,
                        &session.request,
                        &text,
                        cx,
                    )
                {
                    app.inform(Message::new("notice.text-service-rejected"), cx);
                }
            });
        })
        .detach();
    }

    fn finish_context_menu(
        &mut self,
        id: u64,
        selected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.context_menu_current(id, cx) {
            if self
                .context_menus
                .pending
                .as_ref()
                .is_some_and(|session| session.id == id)
            {
                self.context_menus.dismiss_popup();
            }
            return;
        }
        let session = self.context_menus.pending.take().expect("validated popup");
        let Some(action) = selected
            .and_then(|index| session.actions.get(index))
            .cloned()
        else {
            self.focus_editor(window, cx);
            return;
        };
        self.context_menus.native = None;
        match action {
            MenuAction::CopyAs(format) => {
                self.editor().update(cx, |editor, cx| {
                    editor.copy_as(format, cx);
                });
                self.focus_editor(window, cx);
            }
            MenuAction::CopyTable(pos) => {
                self.editor().update(cx, |editor, cx| {
                    editor.copy_table_at(pos, cx);
                });
                self.focus_editor(window, cx);
            }
            MenuAction::Markdown(action) => {
                if self.markdown_action_enabled(action, cx) {
                    self.run_markdown_action(action, &session.request, window, cx);
                }
            }
            MenuAction::InsertParagraph { before } => {
                if self.library.active_note().read_only.is_none() {
                    self.editor().update(cx, |editor, cx| {
                        editor.insert_context_paragraph(&session.request, before, cx);
                    });
                }
                self.focus_editor(window, cx);
            }
            MenuAction::CheckingPanel(panel) => self.show_checking_panel(panel, window, cx),
            MenuAction::Substitute(kind) => {
                self.apply_selected_substitutions(&session.request, kind, cx);
                self.focus_editor(window, cx);
            }
            MenuAction::PasteMatchStyle => {
                if self.editing_enabled(&Intent::Edit(EditCommand::PastePlain), cx) {
                    self.editor().update(cx, |editor, cx| {
                        editor.execute_context_action(
                            markraft_gpui::ContextAction::PasteMatchStyle,
                            cx,
                        )
                    });
                }
                self.focus_editor(window, cx);
            }
            MenuAction::Translate(text) => self.show_translation(id, session, &text, window, cx),
            MenuAction::Replace(request, text) => {
                self.apply_service_text(&session.note, &session.editor, &request, &text, cx);
                self.focus_editor(window, cx);
            }
            MenuAction::IgnoreSpelling(word) => {
                self.update_dictionary(|checker| checker.ignore(&word), cx)
            }
            MenuAction::LearnSpelling(word) => {
                self.update_dictionary(|checker| checker.learn(&word), cx)
            }
            MenuAction::UnlearnSpelling(word) => {
                self.update_dictionary(|checker| checker.unlearn(&word), cx)
            }
            MenuAction::CheckSpelling => self.schedule_text_checking(CheckTrigger::Requested, cx),
            MenuAction::CheckSetting(setting) => self.toggle_text_checking(setting, window, cx),
            MenuAction::Transform(transform) => {
                if self.library.active_note().read_only.is_none() {
                    self.editor().update(cx, |editor, cx| {
                        editor.transform_context_text(&session.request, transform, cx);
                    });
                }
                self.focus_editor(window, cx);
            }
            MenuAction::Text(service, text) => {
                self.present_text_service(service, text, &session.request, window, cx)
            }
            MenuAction::OpenWith(url, application) => {
                if let Err(error) = text_services::open_with(&url, &application) {
                    self.feedback.set_error(error);
                    cx.notify();
                }
            }
            MenuAction::Intent(intent) => self.run_menu_intent(intent, window, cx),
            MenuAction::CopyUrl(url) => {
                cx.write_to_clipboard(ClipboardItem::new_string(url));
                self.focus_editor(window, cx);
            }
            MenuAction::OpenUrl(url) => EditorView::open_link(&url, cx),
            MenuAction::Wiki { target, embed } => self.follow_wiki_link(&target, embed, window, cx),
            MenuAction::CopyCode(pos) => {
                if let Some(text) = self.editor().read(cx).code_text_at(pos) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                }
                self.focus_editor(window, cx);
            }
            MenuAction::CodeLanguage(pos) => {
                if self.library.active_note().read_only.is_none() {
                    self.open_code_language(pos, cx);
                }
            }
        }
    }
}

impl From<EditCommand> for markraft_gpui::ContextAction {
    fn from(command: EditCommand) -> Self {
        match command {
            EditCommand::Copy => Self::Copy,
            EditCommand::Cut => Self::Cut,
            EditCommand::Paste => Self::Paste,
            EditCommand::PastePlain => Self::PastePlain,
            EditCommand::PasteMarkdown => Self::PasteMarkdown,
        }
    }
}

#[cfg(test)]
mod tests;
