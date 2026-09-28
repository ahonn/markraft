//! Host-supplied editor presentation. Message IDs are stable; document content is not translated.
use std::rc::Rc;

macro_rules! messages {
    ($($variant:ident => ($key:literal, $english:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum EditorMessage { $($variant),+ }
        impl EditorMessage {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn key(self) -> &'static str {
                match self { $(Self::$variant => concat!("editor.", $key)),+ }
            }
            pub const fn english(self) -> &'static str {
                match self { $(Self::$variant => $english),+ }
            }
        }
    };
}
messages! {
    TextEditor => ("text-editor", "Text editor"),
    PlainText => ("plain-text", "Plain Text"),
    EmptyWikiLink => ("wiki-empty", "Empty wiki link"),
    WikiLink => ("wiki-link", "Wiki link: %{label}"),
    Callout => ("callout", "Callout: %{label}"),
    Task => ("task", "Task"),
    ImageRemote => ("image-remote", "Remote preview unavailable"),
    ImageLoading => ("image-loading", "Loading image"),
    ImageRemoteFailed => ("image-remote-failed", "Cannot load remote image"),
    ImageInvalidPath => ("image-invalid-path", "Invalid image path"),
    ImageMissing => ("image-missing", "Image file not found"),
    ImageTooLarge => ("image-too-large", "Image exceeds 16 MB"),
    ImageUnsupported => ("image-unsupported", "Unsupported image format"),
    ImageUnreadable => ("image-unreadable", "Cannot read image"),
    ImageUnsupportedRoot => ("image-unsupported-root", "Unsupported typora-root-url"),
    ImageStatus => ("image-status", "%{status}: %{name}"),
    InlineImage => ("image-inline", "Inline image: %{name}"),
    ImagePlaceholder => ("image-placeholder", "[image]"),
    EquationError => ("equation-error", "Equation number: %{error}"),
    FormulaError => ("formula-error", "Formula: %{error}"),
    FormulaInvalidScale => ("formula-invalid-scale", "Invalid formula display scale"),
    FormulaTooLarge => ("formula-too-large", "Formula exceeds the rendered image size limit"),
    FormulaParseSvg => ("formula-parse-svg", "Could not parse formula SVG: %{error}"),
    FormulaRenderSvg => ("formula-render-svg", "Could not render formula SVG: %{error}"),
    CalloutNote => ("callout-note", "Note"),
    CalloutInfo => ("callout-info", "Info"),
    CalloutTodo => ("callout-todo", "Todo"),
    CalloutAbstract => ("callout-abstract", "Abstract"),
    CalloutSummary => ("callout-summary", "Summary"),
    CalloutTldr => ("callout-tldr", "Tldr"),
    CalloutTip => ("callout-tip", "Tip"),
    CalloutHint => ("callout-hint", "Hint"),
    CalloutImportant => ("callout-important", "Important"),
    CalloutSuccess => ("callout-success", "Success"),
    CalloutCheck => ("callout-check", "Check"),
    CalloutDone => ("callout-done", "Done"),
    CalloutQuestion => ("callout-question", "Question"),
    CalloutHelp => ("callout-help", "Help"),
    CalloutFaq => ("callout-faq", "Faq"),
    CalloutWarning => ("callout-warning", "Warning"),
    CalloutCaution => ("callout-caution", "Caution"),
    CalloutAttention => ("callout-attention", "Attention"),
    CalloutFailure => ("callout-failure", "Failure"),
    CalloutFail => ("callout-fail", "Fail"),
    CalloutMissing => ("callout-missing", "Missing"),
    CalloutDanger => ("callout-danger", "Danger"),
    CalloutError => ("callout-error", "Error"),
    CalloutBug => ("callout-bug", "Bug"),
    CalloutExample => ("callout-example", "Example"),
    CalloutQuote => ("callout-quote", "Quote"),
    CalloutCite => ("callout-cite", "Cite"),
    EquationStandaloneTag => ("equation-standalone-tag", "Equation tags require a standalone display formula"),
    EquationMetadataRedefined => ("equation-metadata-redefined", "Redefining equation metadata commands is not supported"),
    EquationPerRowMetadata => ("equation-per-row-metadata", "Per-row equation metadata is not supported yet; place block metadata after the environment"),
    EquationIncompleteMetadata => ("equation-incomplete-metadata", "Incomplete equation metadata or reference"),
    EquationMultipleTags => ("equation-multiple-tags", "A formula can have only one equation tag"),
    EquationEmptyTag => ("equation-empty-tag", "Equation tags cannot be empty"),
    EquationTagReference => ("equation-tag-reference", "Equation tags cannot contain references"),
    EquationEmptyLabel => ("equation-empty-label", "Equation labels cannot be empty"),
    EquationDuplicateNumber => ("equation-duplicate-number", "Duplicate equation number: %{value}"),
    EquationDuplicateLabel => ("equation-duplicate-label", "Duplicate equation label: %{value}"),
    EquationAmbiguousReference => ("equation-ambiguous-reference", "Ambiguous equation reference: %{value}"),
    EquationUnknownReference => ("equation-unknown-reference", "Unknown equation reference: %{value}"),
    EquationUnnumberedReference => ("equation-unnumbered-reference", "Equation has no number: %{value}"),
    EquationUnsupportedEnvironment => ("equation-unsupported-environment", "The %{value} environment is not supported yet"),
    TypesetSourceTooLarge => ("typeset-source-too-large", "Formula exceeds the 16 KiB source limit"),
    TypesetEmpty => ("typeset-empty", "Formula is empty"),
    TypesetInvalidFontSize => ("typeset-invalid-font-size", "Invalid formula font size"),
    TypesetInvalidColor => ("typeset-invalid-color", "Invalid formula foreground color"),
    TypesetTooComplex => ("typeset-too-complex", "Formula exceeds the display complexity limit"),
    TypesetLayoutTooLarge => ("typeset-layout-too-large", "Formula exceeds the logical layout size limit"),
    TypesetInvalidLatex => ("typeset-invalid-latex", "Invalid LaTeX: %{error}"),
}

type Translator = dyn Fn(EditorMessage, &[(&str, &str)]) -> String;

/// An immutable translation callback shared by an editor's rendering surfaces.
/// Replace it with `EditorView::set_messages` when the host changes language.
#[derive(Clone, Default)]
pub struct EditorMessages(Option<Rc<Translator>>);

impl EditorMessages {
    pub const ENGLISH: Self = Self(None);

    pub fn new(translate: impl Fn(EditorMessage, &[(&str, &str)]) -> String + 'static) -> Self {
        Self(Some(Rc::new(translate)))
    }

    pub fn text(&self, message: EditorMessage) -> String {
        self.format(message, &[])
    }

    pub fn format(&self, message: EditorMessage, args: &[(&str, &str)]) -> String {
        if let Some(translate) = &self.0 {
            return translate(message, args);
        }
        // Read only the template: inserted user text is never interpreted again.
        let mut result = String::new();
        let mut rest = message.english();
        while let Some((before, after)) = rest.split_once("%{") {
            result.push_str(before);
            let Some((name, remaining)) = after.split_once('}') else {
                result.push_str("%{");
                rest = after;
                break;
            };
            if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                result.push_str(value);
            } else {
                result.push_str("%{");
                result.push_str(name);
                result.push('}');
            }
            rest = remaining;
        }
        result.push_str(rest);
        result
    }

    pub(crate) fn typeset_error(&self, error: &markraft_math::TypesetError) -> String {
        use markraft_math::TypesetError as E;
        match error {
            E::SourceTooLarge => self.text(EditorMessage::TypesetSourceTooLarge),
            E::Empty => self.text(EditorMessage::TypesetEmpty),
            E::InvalidFontSize => self.text(EditorMessage::TypesetInvalidFontSize),
            E::InvalidColor => self.text(EditorMessage::TypesetInvalidColor),
            E::TooComplex => self.text(EditorMessage::TypesetTooComplex),
            E::LayoutTooLarge => self.text(EditorMessage::TypesetLayoutTooLarge),
            E::InvalidLatex(error) => {
                self.format(EditorMessage::TypesetInvalidLatex, &[("error", error)])
            }
        }
    }

    pub(crate) fn equation_diagnostic(
        &self,
        diagnostic: &markraft_core::kind::equations::EquationDiagnostic,
    ) -> String {
        use markraft_core::kind::equations::EquationDiagnostic as D;
        match diagnostic {
            D::StandaloneTag => self.text(EditorMessage::EquationStandaloneTag),
            D::MetadataRedefined => self.text(EditorMessage::EquationMetadataRedefined),
            D::PerRowMetadata => self.text(EditorMessage::EquationPerRowMetadata),
            D::IncompleteMetadata => self.text(EditorMessage::EquationIncompleteMetadata),
            D::MultipleTags => self.text(EditorMessage::EquationMultipleTags),
            D::EmptyTag => self.text(EditorMessage::EquationEmptyTag),
            D::TagReference => self.text(EditorMessage::EquationTagReference),
            D::EmptyLabel => self.text(EditorMessage::EquationEmptyLabel),
            D::DuplicateNumber(value) => {
                self.format(EditorMessage::EquationDuplicateNumber, &[("value", value)])
            }
            D::DuplicateLabel(value) => {
                self.format(EditorMessage::EquationDuplicateLabel, &[("value", value)])
            }
            D::AmbiguousReference(value) => self.format(
                EditorMessage::EquationAmbiguousReference,
                &[("value", value)],
            ),
            D::UnknownReference(value) => {
                self.format(EditorMessage::EquationUnknownReference, &[("value", value)])
            }
            D::UnnumberedReference(value) => self.format(
                EditorMessage::EquationUnnumberedReference,
                &[("value", value)],
            ),
            D::UnsupportedEnvironment(value) => self.format(
                EditorMessage::EquationUnsupportedEnvironment,
                &[("value", value)],
            ),
        }
    }

    pub(crate) fn callout_title(&self, kind: &str) -> String {
        let key = format!("editor.callout-{}", kind.to_lowercase());
        let message = EditorMessage::ALL
            .iter()
            .find(|message| message.key() == key);
        if let Some(message) = message {
            let translated = self.text(*message);
            if translated != message.english() {
                return translated;
            }
        }
        // Preserve the author's capitalization and unknown custom callout types.
        let mut chars = kind.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_messages_keep_interpolated_content_literal() {
        assert_eq!(
            EditorMessages::default().format(EditorMessage::WikiLink, &[("label", "%{label}")]),
            "Wiki link: %{label}"
        );
    }
    #[test]
    fn translated_callout_defaults_preserve_unknown_types() {
        let messages = EditorMessages::new(|message, _| match message {
            EditorMessage::CalloutNote => "注意".into(),
            _ => message.english().into(),
        });
        assert_eq!(messages.callout_title("NOTE"), "注意");
        assert_eq!(EditorMessages::default().callout_title("NOTE"), "NOTE");
        assert_eq!(messages.callout_title("Custom-kind"), "Custom-kind");
    }

    #[test]
    fn diagnostics_translate_semantic_context_and_keep_source_details() {
        use markraft_core::kind::equations::EquationDiagnostic;
        let messages = EditorMessages::new(|message, args| match message {
            EditorMessage::EquationUnknownReference => format!("未知公式參照：{}", args[0].1),
            EditorMessage::TypesetInvalidLatex => format!("無效的 LaTeX：{}", args[0].1),
            _ => EditorMessages::ENGLISH.format(message, args),
        });
        assert_eq!(
            messages
                .equation_diagnostic(&EquationDiagnostic::UnknownReference("eq:%{value}".into())),
            "未知公式參照：eq:%{value}"
        );
        let error = markraft_math::TypesetError::InvalidLatex("unexpected token".into());
        assert_eq!(
            messages.typeset_error(&error),
            "無效的 LaTeX：unexpected token"
        );
        assert_eq!(
            EditorMessages::ENGLISH.typeset_error(&error),
            error.to_string()
        );
    }

    #[gpui::test]
    fn changing_messages_preserves_editing_state_and_refreshes_default_accessibility(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext;
        let view = cx.new(crate::EditorView::single_line);
        view.update(cx, |view, cx| {
            assert!(view.run_command(&markraft_core::commands::insert_text("My note"), cx));
            let document = view.state.doc().clone();
            let selection = view.state.selection().clone();
            let revision = view.shaping.revision();
            view.set_messages(
                EditorMessages::new(|message, args| match message {
                    EditorMessage::TextEditor => "文字編輯器".into(),
                    _ => EditorMessages::ENGLISH.format(message, args),
                }),
                cx,
            );
            assert_eq!(view.aria_label.as_ref(), "文字編輯器");
            assert_eq!(view.state.doc(), &document);
            assert_eq!(view.state.selection(), &selection);
            assert!(view.shaping.revision() > revision);
            let undo =
                markraft_core::history::undo(&view.state).expect("language changes keep undo");
            assert!(view.dispatch([undo], cx));
            assert_eq!(view.text(), "");
            view.set_aria_label("My custom field", cx);
            view.set_messages(EditorMessages::ENGLISH, cx);
            assert_eq!(view.aria_label.as_ref(), "My custom field");
        });
    }
}
