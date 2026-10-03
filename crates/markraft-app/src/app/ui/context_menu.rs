//! One native popup belongs to one editor snapshot. Native tracking runs outside
//! GPUI updates; the returned command is validated before touching that editor.

use super::text_checking::CheckingSetting;
use super::*;
use crate::platform::checking_panel::{CheckingPanel, SubstitutionKind};
use crate::platform::context_menu::{PreparedMenu, Row, TitleStyle};
use crate::platform::text_checking::{CheckKind, CheckOptions};
use crate::platform::{
    text_requestor::{TextReplacement, TextServiceSession, TextServiceSnapshot},
    text_services::{self, OpenWithApplication, TextAnchor},
};
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
    CheckSetting(CheckingSetting),
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

impl MarkraftApp {
    fn context_menu_rows(&self, request: &ContextRequest, cx: &App) -> (Vec<Row>, Vec<MenuAction>) {
        let mut menu = MenuBuilder {
            app: self,
            cx,
            actions: Vec::new(),
        };
        let mut rows: Vec<Row> = [EditCommand::Cut, EditCommand::Copy, EditCommand::Paste]
            .into_iter()
            .map(|command| menu.intent(command.label(), Intent::Edit(command)))
            .collect();
        let paste_enabled = self
            .editing_state(&Intent::Edit(EditCommand::PastePlain), cx)
            .is_some_and(|state| state.enabled);
        rows.insert(
            3,
            menu.item(
                "command.paste-match-style",
                MenuAction::PasteMatchStyle,
                paste_enabled,
                None,
            ),
        );
        let editor = self.editor().read(cx);
        let mut copies = [
            ("command.copy-as-plain-text", CopyFormat::PlainText),
            ("command.copy-as-markdown", CopyFormat::Markdown),
            ("command.copy-as-html", CopyFormat::HtmlCode),
            ("command.copy-without-theme", CopyFormat::RichText),
        ]
        .into_iter()
        .map(|(label, format)| {
            menu.item(
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
                .map(|command| menu.intent(command.label(), Intent::Edit(command))),
        );
        rows.push(menu.submenu("menu.copy-paste-as", copies));
        // Only styles Markdown can spell: the document format is not widened
        // to carry fonts, colours or paragraph geometry.
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
            menu.intent(label, Intent::Mark(mark))
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
            .map(|(label, mark)| menu.intent(label, Intent::Mark(mark))),
        );
        formats.push(menu.intent("command.link", Intent::Link));
        formats.push(Row::Separator);
        formats.push(menu.item(
            "command.clear-format",
            MenuAction::Markdown(MarkdownAction::ClearFormatting),
            self.markdown_action_enabled(MarkdownAction::ClearFormatting, cx),
            None,
        ));
        let format = menu
            .submenu("menu.format", formats)
            .with_symbol("textformat");
        let writable = !self.is_reloading()
            && self.library.active_note().read_only.is_none()
            && !editor.is_composing();
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
        .map(|(label, block)| menu.intent(label, Intent::Block(block)))
        .collect();
        let paragraph = menu.submenu("menu.paragraph", paragraph);
        let mut inserts = vec![
            menu.item(
                "command.insert-image",
                MenuAction::Markdown(MarkdownAction::InsertImage),
                self.markdown_action_enabled(MarkdownAction::InsertImage, cx),
                None,
            ),
            menu.intent("command.code-block", Intent::Block(doc::Block::Code)),
            menu.item(
                "command.math-block",
                MenuAction::Markdown(MarkdownAction::InsertMath),
                self.markdown_action_enabled(MarkdownAction::InsertMath, cx),
                None,
            ),
            menu.intent("command.table", Intent::InsertTable),
            menu.intent("command.divider", Intent::Block(doc::Block::Divider)),
            Row::Separator,
        ];
        for (label, before) in [
            ("command.insert-paragraph-before", true),
            ("command.insert-paragraph-after", false),
        ] {
            inserts.push(menu.item(
                label,
                MenuAction::InsertParagraph { before },
                writable && editor.can_insert_context_paragraph(request, before),
                None,
            ));
        }
        let insert = menu.submenu("menu.insert", inserts);
        let mut contextual = Vec::new();
        match &request.target {
            ContextTarget::Link { url, range } => {
                contextual.push(menu.item(
                    "command.open-link",
                    MenuAction::OpenUrl(url.clone()),
                    true,
                    None,
                ));
                contextual.push(menu.item(
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
                    contextual.push(menu.intent("surfaces.link.edit", Intent::EditLink));
                    contextual.push(menu.intent("command.remove-link", Intent::Unlink));
                }
                if self.platform.is_some() {
                    let apps = text_services::applications_for_url(url);
                    let children = apps
                        .into_iter()
                        .map(|application| {
                            let label = application.name.clone();
                            let id = menu.actions.len();
                            menu.actions
                                .push(MenuAction::OpenWith(url.clone(), application));
                            Row::Item {
                                symbol: None,
                                appearance: Default::default(),
                                id,
                                label,
                                enabled: true,
                                checked: None,
                            }
                        })
                        .collect::<Vec<_>>();
                    if !children.is_empty() {
                        contextual.push(menu.submenu("menu.open-with", children));
                    }
                }
            }
            ContextTarget::WikiLink { target, embed, .. } => {
                contextual.push(menu.item(
                    "command.open-link",
                    MenuAction::Wiki {
                        target: target.clone(),
                        embed: *embed,
                    },
                    true,
                    None,
                ));
                contextual.push(menu.item(
                    "command.copy-link",
                    MenuAction::CopyUrl(target.clone()),
                    true,
                    None,
                ));
            }
            ContextTarget::CodeBlock { pos } => {
                contextual.push(menu.item(
                    "command.copy-code-block",
                    MenuAction::CopyCode(*pos),
                    editor.code_text_at(*pos).is_some(),
                    None,
                ));
                contextual.push(menu.item(
                    "command.choose-code-language",
                    MenuAction::CodeLanguage(*pos),
                    writable,
                    None,
                ));
            }
            ContextTarget::Table { pos, cell } => {
                let state = editor.state();
                let selection = state.selection();
                let in_cell = |position| {
                    state.doc().resolve(position).ok().is_some_and(|resolved| {
                        (1..=resolved.depth()).any(|depth| resolved.before(depth) == *cell)
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
                    .map(|(label, edit)| menu.intent(label, Intent::Table(edit)))
                    .collect();
                    table.push(Row::Separator);
                }
                table.push(menu.item(
                    "command.copy-table",
                    MenuAction::CopyTable(*pos),
                    editor.can_copy_table_at(*pos),
                    None,
                ));
                contextual.push(menu.submenu("command.table", table));
            }
            ContextTarget::Text | ContextTarget::Image { .. } | ContextTarget::Math { .. } => {}
        }
        let mut checking = vec![
            menu.item(
                self.checking_panel_label(CheckingPanel::Spelling),
                MenuAction::CheckingPanel(CheckingPanel::Spelling),
                true,
                None,
            ),
            menu.item(
                "command.check-spelling",
                MenuAction::CheckSpelling,
                true,
                None,
            ),
            Row::Separator,
        ];
        for (label, setting) in [
            ("command.check-continuously", CheckingSetting::Spelling),
            ("command.check-grammar", CheckingSetting::Grammar),
            ("command.correct-spelling", CheckingSetting::Correction),
        ] {
            let value = *setting.value(&mut self.preferences.text_checking.clone());
            checking.push(menu.item(label, MenuAction::CheckSetting(setting), true, Some(value)));
        }
        let spelling = menu.submenu("menu.spelling", checking);
        // Substitutions validate each detected reading range separately. A
        // concealed delimiter between ranges only prevents whole-text Services
        // replacement; it must not disable edits inside those ranges.
        let substitutable = writable
            && editor
                .context_text(request)
                .is_some_and(|text| !text.text.is_empty());
        let mut substitution_rows = [
            ("command.replace-quotes", SubstitutionKind::Quotes),
            ("command.replace-dashes", SubstitutionKind::Dashes),
            ("command.add-links", SubstitutionKind::Links),
            ("command.replace-text", SubstitutionKind::Text),
        ]
        .into_iter()
        .map(|(label, kind)| menu.item(label, MenuAction::Substitute(kind), substitutable, None))
        .collect::<Vec<_>>();
        if editor
            .context_text(request)
            .is_none_or(|text| text.text.is_empty())
        {
            substitution_rows.clear();
        } else {
            substitution_rows.push(Row::Separator);
        }
        substitution_rows.extend([
            menu.item(
                self.checking_panel_label(CheckingPanel::Substitutions),
                MenuAction::CheckingPanel(CheckingPanel::Substitutions),
                true,
                None,
            ),
            Row::Separator,
        ]);
        let substitutions = [
            (
                "command.smart-copy-paste",
                CheckingSetting::SmartInsertDelete,
            ),
            ("command.smart-quotes", CheckingSetting::Quotes),
            ("command.smart-dashes", CheckingSetting::Dashes),
            ("command.smart-links", CheckingSetting::Links),
            ("command.data-detection", CheckingSetting::DataDetectors),
            ("command.text-replacement", CheckingSetting::Replacements),
        ]
        .into_iter()
        .map(|(label, setting)| {
            let value = *setting.value(&mut self.preferences.text_checking.clone());
            menu.item(label, MenuAction::CheckSetting(setting), true, Some(value))
        })
        .collect::<Vec<_>>();
        substitution_rows.extend(substitutions);
        let substitutions = menu.submenu("menu.substitutions", substitution_rows);
        let mut suggestions = Vec::new();
        let mut lookup = Vec::new();
        let mut share = None;
        let mut transformations = None;
        if let Some(text) = editor
            .context_text(request)
            .filter(|text| !text.text.trim().is_empty())
        {
            if let Some(checker) = self.spell_document()
                && text.text.chars().count() <= 2048
                && editor.context_is_prose(request)
            {
                // Learned words produce no spelling result. Query the native
                // dictionary separately so users can reverse Learn Spelling.
                if checker.has_learned(&text.text) {
                    suggestions.extend(menu.spelling_word(&text.text, true));
                }
                let checks = checker.check(
                    &text.text,
                    CheckOptions {
                        spelling: true,
                        grammar: self.preferences.text_checking.grammar,
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
                    let Some(narrowed) = editor.context_text_range(request, check.range.clone())
                    else {
                        continue;
                    };
                    if matches!(check.kind, CheckKind::Spelling | CheckKind::Grammar)
                        && !urls.iter().any(|range| {
                            range.start < check.range.end && check.range.start < range.end
                        })
                    {
                        if let Some(detail) = check.detail {
                            suggestions.push(menu.literal(
                                detail,
                                MenuAction::CheckSpelling,
                                false,
                                None,
                            ));
                        }
                        for suggestion in check.suggestions.into_iter().take(5) {
                            suggestions.push(menu.literal(
                                suggestion.clone(),
                                MenuAction::Replace(narrowed.clone(), suggestion),
                                writable,
                                None,
                            ));
                        }
                        if check.kind == CheckKind::Spelling {
                            let word = text
                                .text
                                .chars()
                                .skip(check.range.start)
                                .take(check.range.end - check.range.start)
                                .collect::<String>();
                            suggestions.extend(menu.spelling_word(&word, false));
                        }
                    }
                    if self.preferences.text_checking.data_detectors
                        && !matches!(request.target, ContextTarget::Link { .. })
                        && let Some(url) = check.url
                    {
                        suggestions.push(menu.item(
                            "command.open-link",
                            MenuAction::OpenUrl(url),
                            true,
                            None,
                        ));
                    }
                }
                if self.preferences.text_checking.data_detectors {
                    let selection = request.selection_range();
                    let projection = editor.projection();
                    if let Some((index, _)) = projection.pos_to_line_offset(selection.start)
                        && let Some(line) = projection.line(index)
                        && line.len() <= 2048
                        && let Some(paragraph) =
                            editor.context_source_range_snapshot(line.from()..line.to())
                        && let Some(reading) = editor.context_text(&paragraph)
                    {
                        for check in checker.check(
                            &reading.text,
                            CheckOptions {
                                data_detectors: true,
                                ..Default::default()
                            },
                        ) {
                            let Some(date) = check.data else {
                                continue;
                            };
                            let Some(detected) = editor.context_text_range(&paragraph, check.range)
                            else {
                                continue;
                            };
                            if !editor.context_is_prose(&detected) {
                                continue;
                            }
                            let range = detected.text_range();
                            if range.start >= selection.end || range.end <= selection.start {
                                continue;
                            }
                            suggestions.push(Row::DetectedData {
                                result: date,
                                position: request.position,
                            });
                            break;
                        }
                    }
                }
            }
            if !suggestions
                .iter()
                .any(|row| matches!(row, Row::DetectedData { .. }))
            {
                let snippet = crate::platform::context_menu::selection_label(&text.text);
                lookup.push(
                    menu.literal(
                        self.i18n
                            .text_with("command.look-up-selection", &[("text", &snippet)]),
                        MenuAction::Text(TextService::Lookup, text.text.clone()),
                        true,
                        None,
                    )
                    .with_symbol("info.circle"),
                );
                if self.platform.is_some() && crate::platform::translation::available() {
                    lookup.push(
                        menu.literal(
                            self.i18n
                                .text_with("command.translate-selection", &[("text", &snippet)]),
                            MenuAction::Translate(text.text.clone()),
                            true,
                            None,
                        )
                        .with_symbol("translate"),
                    );
                }
                lookup.push(Row::Separator);
            }
            lookup.push(
                menu.item(
                    "command.search-web",
                    MenuAction::Text(TextService::Search, text.text.clone()),
                    true,
                    None,
                )
                .with_symbol("magnifyingglass"),
            );
            share = Some(
                menu.item(
                    "command.share-text",
                    MenuAction::Text(TextService::Share, text.text.clone()),
                    true,
                    None,
                )
                .with_symbol("square.and.arrow.up"),
            );
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
                    menu.item(
                        label,
                        MenuAction::Transform(transform),
                        writable && text.transformable,
                        None,
                    )
                })
                .collect();
            if !transforms.is_empty() {
                transformations = Some(menu.submenu("menu.transformations", transforms));
            }
        }
        let speech_text = editor
            .context_text(request)
            .filter(|text| !text.text.trim().is_empty())
            .or_else(|| editor.context_text(&editor.context_document_snapshot()))
            .map(|text| text.text)
            .unwrap_or_default();
        let speech = vec![
            menu.item(
                "command.start-speaking",
                MenuAction::Text(TextService::Speak, speech_text.clone()),
                !speech_text.trim().is_empty(),
                None,
            ),
            menu.item(
                "command.stop-speaking",
                MenuAction::Text(TextService::StopSpeaking, String::new()),
                self.platform.is_some() && text_services::is_speaking(),
                None,
            ),
        ];
        let mut result = Vec::new();
        for group in [suggestions, contextual, lookup, rows] {
            if !group.is_empty() {
                if !result.is_empty() {
                    result.push(Row::Separator);
                }
                result.extend(group);
            }
        }
        if let Some(share) = share {
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
        if let Some(transformations) = transformations {
            result.push(transformations);
        }
        result.push(menu.submenu("menu.speech", speech));
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
        self.attach_text_services(id, window, cx);
        let writing_tools = self
            .context_menus
            .native
            .as_ref()
            .and_then(|session| session._requestor.as_ref())
            .and_then(TextServiceSession::writing_tools_items);
        let prepared = match PreparedMenu::new(&rows, window, writing_tools) {
            Ok(menu) => menu,
            Err(error) => {
                self.context_menus.dismiss_popup();
                self.feedback.set_error(error);
                cx.notify();
                return;
            }
        };
        // AppKit tracking can suspend GPUI presentation. Let the pointer's new
        // selection paint before handing event delivery to the native popup.
        let (painted, ready) = futures_channel::oneshot::channel();
        window.on_next_frame(move |_, _| {
            let _ = painted.send(());
        });
        cx.spawn_in(window, async move |this, cx| {
            if ready.await.is_err() {
                return;
            }
            let valid = this
                .update(cx, |app, cx| app.context_menu_current(id, cx))
                .unwrap_or(false);
            if !valid {
                return;
            }
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

    fn attach_text_services(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let session = self.context_menus.pending.as_ref().expect("a pending menu");
        let Some(text) = self.editor().read(cx).context_text(&session.request) else {
            return;
        };
        let editable = text.replaceable && self.library.active_note().read_only.is_none();
        let prose = self.editor().read(cx).context_is_prose(&session.request);
        let editor_style = self.editor().read(cx).style();
        let line_height = editor_style.body_size * editor_style.line_height_ratio;
        let Ok((requestor, returned)) = TextServiceSession::attach(
            window,
            TextServiceSnapshot {
                text: text.text,
                editable,
                prose,
            },
            session.request.position,
            line_height,
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
        position: Point<Pixels>,
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
        let anchor = match TextAnchor::new(window, position) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.feedback.set_error(error);
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = match service {
                TextService::Lookup => anchor.show_definition(&text),
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
            MenuAction::Translate(text) => {
                let editable = self.library.active_note().read_only.is_none()
                    && self
                        .editor()
                        .read(cx)
                        .context_text(&session.request)
                        .is_some_and(|text| text.replaceable);
                match crate::platform::translation::Translation::show(
                    window,
                    session.request.position,
                    &text,
                    editable,
                ) {
                    Ok((translation, returned)) => {
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
                    Err(error) => {
                        self.feedback.set_error(error);
                        cx.notify();
                    }
                }
            }
            MenuAction::Replace(request, text) => {
                self.apply_service_text(&session.note, &session.editor, &request, &text, cx);
                self.focus_editor(window, cx);
            }
            MenuAction::IgnoreSpelling(word) => {
                if let Some(checker) = self.spell_document() {
                    checker.ignore(&word);
                }
                self.schedule_text_checking(false, false, cx);
            }
            MenuAction::LearnSpelling(word) => {
                if let Some(checker) = self.spell_document() {
                    checker.learn(&word);
                }
                self.schedule_text_checking(false, false, cx);
            }
            MenuAction::UnlearnSpelling(word) => {
                if let Some(checker) = self.spell_document() {
                    checker.unlearn(&word);
                }
                self.schedule_text_checking(false, false, cx);
            }
            MenuAction::CheckSpelling => self.schedule_text_checking(false, true, cx),
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
                self.present_text_service(service, text, session.request.position, window, cx)
            }
            MenuAction::OpenWith(url, application) => {
                if let Err(error) = text_services::open_with(&url, &application) {
                    self.feedback.set_error(error);
                    cx.notify();
                }
            }
            MenuAction::Intent(intent) => {
                if !self.editing_enabled(&intent, cx) {
                    return;
                }
                match intent {
                    Intent::Edit(command) => {
                        self.editor().update(cx, |editor, cx| {
                            editor.execute_context_action(command.into(), cx)
                        });
                        self.focus_editor(window, cx);
                    }
                    Intent::Mark(mark) => {
                        self.editor().update(cx, |editor, cx| {
                            editor.with_context_edit(cx, |editor, cx| {
                                editor.toggle_mark(mark.mark(), Default::default(), cx)
                            })
                        });
                        self.focus_editor(window, cx);
                    }
                    Intent::Block(block) => {
                        self.editor().update(cx, |editor, cx| {
                            editor.with_context_edit(cx, |editor, cx| {
                                editor.run_command(&block.command(), cx);
                            });
                        });
                        self.focus_editor(window, cx);
                    }
                    Intent::InsertTable => {
                        self.editor().update(cx, |editor, cx| {
                            editor.with_context_edit(cx, |editor, cx| {
                                editor.table(
                                    markraft_gpui::TableOp::Insert {
                                        rows: 2,
                                        columns: 3,
                                    },
                                    cx,
                                );
                            });
                        });
                        self.focus_editor(window, cx);
                    }
                    Intent::Table(edit) => {
                        self.editor().update(cx, |editor, cx| {
                            editor.with_context_edit(cx, |editor, cx| {
                                edit.run(editor, cx);
                            })
                        });
                        self.focus_editor(window, cx);
                    }
                    Intent::Unlink => {
                        self.editor().update(cx, |editor, cx| {
                            editor.with_context_edit(cx, |editor, cx| editor.set_link(None, cx))
                        });
                        self.focus_editor(window, cx);
                    }
                    other => self.intent(other, window, cx),
                }
            }
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
