//! Native editing adapter. The core owns document semantics; the host owns persistence.
//!
//! The view holds an [`EditorState`] built from the host's schema and
//! extensions. Every edit is a [`TransactionSpec`] or a
//! [`markraft_core::commands::Command`] from the catalogue; nothing here
//! touches the tree. Everything drawn comes from the state's
//! [`markraft_core::projection::Projection`].
mod accessibility;
mod callout;
mod caret;
mod clipboard;
pub mod commands;
mod completion;
mod emoji;
mod extension;
mod format_state;
mod html;
mod images;
pub mod ime;
mod keymap;
mod links;
mod shaping;
mod single_line;
mod style;
mod surface;
mod syntax;
mod typeahead;
mod types;
mod wiki;
pub use emoji::{EmojiShortcodes, emoji_menu};
pub use extension::{
    ActionHandler, CaretShape, EXTENSION_ORIGIN_PREFIX, EditorCx, Extension, ExtensionHandle,
    ExtensionPayload, InputPolicy, Overlay, Update,
};
pub use markraft_core::commands::ColumnAlignment;
pub use style::EditorStyle;
pub use syntax::{canonical_language, code_languages};
pub use typeahead::{Typeahead, TypeaheadItem, TypeaheadProvider};
pub use types::{CalloutAttrs, DocTypes};

use extension::AnchoredOverlay;
use gpui::{prelude::*, *};
use markraft_core::commands::{Command, Direction};
use markraft_core::projection::{Projection, projection_of};
use markraft_core::{
    Attrs, EditorState, EditorStateConfig, HistoryConfig, MarkSet, MarkTypeId, Node, NodeTypeId,
    Schema, Selection, Transaction, TransactionSpec,
};
use std::collections::HashMap;
use std::{cell::RefCell, rc::Rc, sync::Arc};
use surface::{EditorSurface, LayoutLine, ShapeInput, TableScroll};

/// How long a pause splits one typing session from the next, in milliseconds.
const TYPING_GROUP_DELAY: u64 = 750;

/// What an editor calls itself until its host gives it a name of its own.
const DEFAULT_ARIA_LABEL: &str = "Text editor";

// All bindings are scoped so embedding hosts retain their own shortcuts.
actions!(
    markraft,
    [
        Backspace,
        Delete,
        Enter,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        Home,
        End,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        PastePlain,
        PasteMarkdown,
        Indent,
        Outdent,
        Undo,
        Redo,
        Bold,
        Italic,
        Code,
        Strikethrough,
        Paragraph,
        Heading,
        Heading2,
        Heading3,
        Heading4,
        Heading5,
        Heading6,
        Quote,
        CodeBlock,
        Ordered,
        Bullet,
        Task,
        ToggleTask,
        ChooseCodeLanguage,
        EditRawHtml,
        CopyCodeBlock,
        CharacterPalette,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordBackward,
        DeleteWordForward,
        DocumentStart,
        DocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        CancelComposition
    ]
);

/// Links come from imported Markdown too, so only web and mail URLs are opened.
/// A bare host such as "example.com" is treated as HTTPS.
fn openable_url(url: &str) -> Option<String> {
    let scheme = url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    match scheme.as_deref() {
        Some("http" | "https" | "mailto") => Some(url.to_owned()),
        Some(scheme)
            if scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            // "host:port/path" has digits after the colon; anything else is a foreign scheme.
            let rest = &url[scheme.len() + 1..];
            rest.starts_with(|c: char| c.is_ascii_digit())
                .then(|| format!("https://{url}"))
        }
        _ => Some(format!("https://{url}")),
    }
}

fn is_web_url(text: &str) -> bool {
    ["http://", "https://"]
        .iter()
        .any(|scheme| text.len() > scheme.len() && text.starts_with(scheme))
        && !text.contains(char::is_whitespace)
}

pub fn bind_keys(cx: &mut App) {
    macro_rules! bind { ($($key:literal => $action:ident),* $(,)?) => {
        cx.bind_keys([$(KeyBinding::new($key, $action, Some("Markraft"))),*]);
    }; }
    bind! {
        "backspace" => Backspace, "delete" => Delete, "enter" => Enter,
        "left" => Left, "right" => Right, "up" => Up, "down" => Down,
        "shift-left" => SelectLeft, "shift-right" => SelectRight,
        "shift-up" => SelectUp, "shift-down" => SelectDown,
        "cmd-left" => Home, "cmd-right" => End,
        "cmd-shift-left" => SelectHome, "cmd-shift-right" => SelectEnd,
        "home" => Home, "end" => End, "cmd-a" => SelectAll,
        "cmd-c" => Copy, "cmd-x" => Cut, "cmd-v" => Paste,
        "cmd-shift-v" => PastePlain, "cmd-alt-shift-v" => PasteMarkdown,
        "tab" => Indent, "shift-tab" => Outdent,
        "cmd-z" => Undo, "cmd-shift-z" => Redo, "cmd-b" => Bold,
        "cmd-i" => Italic, "cmd-e" => Code,
        "cmd-shift-s" => Strikethrough, "cmd-alt-0" => Paragraph,
        "cmd-alt-1" => Heading, "cmd-alt-2" => Heading2,
        "cmd-alt-3" => Heading3, "cmd-alt-4" => Heading4,
        "cmd-alt-5" => Heading5, "cmd-alt-6" => Heading6,
        "cmd-shift-b" => Quote, "cmd-alt-c" => CodeBlock,
        "cmd-enter" => ToggleTask,
        "cmd-alt-l" => ChooseCodeLanguage, "cmd-alt-shift-c" => CopyCodeBlock,
        "cmd-alt-r" => EditRawHtml,
        "ctrl-cmd-space" => CharacterPalette,
        "alt-left" => WordLeft, "alt-right" => WordRight,
        "alt-shift-left" => SelectWordLeft, "alt-shift-right" => SelectWordRight,
        "alt-backspace" => DeleteWordBackward, "alt-delete" => DeleteWordForward,
        "cmd-up" => DocumentStart, "cmd-down" => DocumentEnd,
        "cmd-shift-up" => SelectDocumentStart, "cmd-shift-down" => SelectDocumentEnd,
        "escape" => CancelComposition,
    }
    cx.bind_keys(list_key_bindings());
    // Last, so that at the same context depth an extension's bindings win while its
    // identifier is in the editor's key context.
    typeahead::bind_keys(cx);
}

// GPUI folds Shift into non-letter keys on macOS: Cmd+Shift+7/8/9
// arrive as Cmd+&/*/(. Bind the normalized symbols rather than raw digits.
fn list_key_bindings() -> [KeyBinding; 3] {
    [
        KeyBinding::new("cmd-&", Ordered, Some("Markraft")),
        KeyBinding::new("cmd-*", Bullet, Some("Markraft")),
        KeyBinding::new("cmd-(", Task, Some("Markraft")),
    ]
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    /// The host owns writing pasted files and images to its document location.
    FilesPasted(ClipboardItem),
    Changed {
        revision: u64,
    },
    /// A link was clicked without ⌘; the caret is now inside it.
    LinkClicked,
    /// The code header was clicked; the host can show a language picker without
    /// moving the editor's selection. `pos` is directly before the code block.
    CodeLanguageRequested {
        pos: usize,
    },
    CodeCopied,
    /// An opaque inline HTML primitive was clicked; the host can edit its source.
    RawHtmlRequested {
        pos: usize,
    },
    /// A wiki link was clicked. `target` is what the source spelled before any
    /// `|`, which only the host can turn into a document to open. `embed` says the
    /// link was written `![[…]]`, so it names a file to open rather than a page.
    WikiLinkClicked {
        target: String,
        embed: bool,
    },
    /// An extension asked the host to do something only the host can do. Hosts that
    /// register no extension never see it.
    Extension {
        id: &'static str,
        payload: ExtensionPayload,
    },
}

/// The table the caret sits in, as a host's table controls need it.
///
/// Every field is geometry or position from the frame the editor last painted,
/// so this is only meaningful after a paint — as
/// [`EditorView::code_header_bounds`] is.
#[derive(Clone, Copy, Debug)]
pub struct TableInfo {
    /// Window bounds of the whole grid, for anchoring a toolbar to the table.
    pub bounds: Bounds<Pixels>,
    /// Window bounds of the cell the caret is in, for anchoring to the column.
    pub cell_bounds: Bounds<Pixels>,
    /// The caret's row. Row 0 is the header row.
    pub row: usize,
    /// The caret's column.
    pub column: usize,
    pub rows: usize,
    pub columns: usize,
    /// The alignment of the caret's own column.
    pub alignment: ColumnAlignment,
}

/// How to build an [`EditorView`]: the host's document kind and its extensions.
///
/// The view is schema-agnostic. Everything that names a concrete document kind
/// comes in here: the compiled [`Schema`], the [`DocTypes`] that say which of
/// its types play the roles the view draws and binds keys to, and the
/// [`Codecs`](markraft_core::Codecs) the clipboard reads and writes with.
pub struct Setup {
    /// The document kind's compiled schema.
    pub schema: Schema,
    /// Which of the schema's types play the roles the view knows about. The
    /// default leaves every role unset, which is what a plain-text editor wants.
    pub types: DocTypes,
    /// The host's state extensions — input rules, corrections and fields. The
    /// view adds history, composition and the projection itself.
    pub extensions: markraft_core::Extension,
    /// How the clipboard reads and writes this document kind. Without it a copy
    /// writes plain text and a paste is inserted literally.
    pub codecs: Option<Arc<dyn markraft_core::Codecs>>,
    /// How this document kind toggles an inline mark. A kind that keeps the
    /// characters spelling a mark in the document edits *those*, which the
    /// model's own [`toggle_mark`](markraft_core::commands::toggle_mark) knows
    /// nothing about; one that does not leaves this unset and gets it.
    pub mark_toggle: Option<MarkToggle>,
    /// The document to open with. The schema's smallest valid document
    /// otherwise.
    pub doc: Option<Node>,
}

impl Setup {
    pub fn new(schema: Schema) -> Setup {
        Setup {
            schema,
            types: DocTypes::none(),
            extensions: markraft_core::Extension::none(),
            codecs: None,
            mark_toggle: None,
            doc: None,
        }
    }
    pub fn types(mut self, types: DocTypes) -> Setup {
        self.types = types;
        self
    }
    pub fn extensions(mut self, extensions: markraft_core::Extension) -> Setup {
        self.extensions = extensions;
        self
    }
    pub fn codecs(mut self, codecs: Arc<dyn markraft_core::Codecs>) -> Setup {
        self.codecs = Some(codecs);
        self
    }
    pub fn mark_toggle(mut self, toggle: MarkToggle) -> Setup {
        self.mark_toggle = Some(toggle);
        self
    }
    pub fn doc(mut self, doc: Node) -> Setup {
        self.doc = Some(doc);
        self
    }
}

/// How a document kind toggles one inline mark over the selection.
pub type MarkToggle =
    Arc<dyn Fn(markraft_core::MarkTypeId, Attrs) -> markraft_core::commands::Command + Send + Sync>;

/// Why an edit did not reach the document. The editor only keeps the cases apart;
/// the host words each one, because only it knows what the document is stored in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRejection {
    /// Nothing can be edited here for as long as this holds, so repeating the
    /// message per keystroke says nothing new.
    ReadOnly(String),
    /// This change cannot be kept where it lands, though others still can.
    Protected(String),
    /// The same, where the view already draws the boundary that refused it. The
    /// host says nothing further: the reason is on screen where the edit landed,
    /// and a sentence in the corner would only say it a second time.
    Marked(String),
    /// The transaction itself could not be built.
    Invalid(String),
}

impl EditRejection {
    /// The sentence the host attached, whichever case it belongs to.
    pub fn message(&self) -> &str {
        let (Self::ReadOnly(message)
        | Self::Protected(message)
        | Self::Marked(message)
        | Self::Invalid(message)) = self;
        message
    }
}

impl std::fmt::Display for EditRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

type DocumentGuard = Box<dyn Fn(&Node) -> Result<(), EditRejection>>;

/// Whether a wiki link target names something the host can open. Only the host can
/// say, and it is asked once per link per layout, so it answers from what it already
/// knows rather than by looking at the disk.
pub type WikiResolver = Box<dyn Fn(&str) -> bool>;

/// The parts of a line of text the host keeps exactly as written, as byte ranges
/// within it. The view shades them so that the boundary of an edit it will refuse
/// is visible before the edit is attempted. Which syntax those are is the host's
/// question, not the view's — this crate knows no Markdown.
pub type ProtectedSpans = Box<dyn Fn(&str) -> Vec<std::ops::Range<usize>>>;

/// Build all transactions before publishing any state. Unlike a transaction
/// filter, this boundary also covers no-filter edits, undo and appender output.
fn apply_guarded(
    state: &mut EditorState,
    specs: impl IntoIterator<Item = TransactionSpec>,
    guard: Option<&DocumentGuard>,
) -> Result<Vec<Transaction>, EditRejection> {
    let transactions = state
        .update_with_appended(specs)
        .map_err(|error| EditRejection::Invalid(error.to_string()))?;
    let last = transactions
        .last()
        .ok_or_else(|| EditRejection::Invalid("No transaction was produced.".to_owned()))?;
    if last.new_doc() != state.doc()
        && let Some(guard) = guard
    {
        guard(last.new_doc())?;
    }
    *state = last.state().clone();
    Ok(transactions)
}

pub struct EditorView {
    document_guard: Option<DocumentGuard>,
    edit_error: Option<EditRejection>,
    file_paste: bool,
    state: EditorState,
    projection: Arc<Projection>,
    pub(crate) types: DocTypes,
    /// The host's clipboard codecs, absent for an editor that only holds text.
    pub(crate) codecs: Option<Arc<dyn markraft_core::Codecs>>,
    /// How the host's document kind toggles a mark; see [`Setup::mark_toggle`].
    mark_toggle: Option<MarkToggle>,
    /// The host's extensions, kept so the state can be rebuilt on a replacement.
    host_extensions: markraft_core::Extension,
    pub(crate) extensions: Vec<extension::Registration>,
    /// The selection the extensions were last told about.
    pub(crate) extension_selection: Selection,
    /// Whether the last frame drew an extension's popup, for a host whose window
    /// chrome is drawn by the platform above everything this view renders.
    overlay_open: bool,
    /// The style, the images and the two host callbacks shaping reads, and the
    /// rows it last produced from them.
    shaping: shaping::Shaping,
    pub(crate) placeholder: SharedString,
    /// What the editor calls itself to assistive technology. A host that lends one
    /// editor to several surfaces renames it as it hands it over.
    pub(crate) aria_label: SharedString,
    pub(crate) single_line: bool,
    pub(crate) single_line_scroll_x: Pixels,
    pub(crate) focus: FocusHandle,
    pub(crate) layout: Vec<LayoutLine>,
    /// How far each grid that does not fit the note is scrolled sideways, keyed
    /// by the position before its table node. Kept across frames — it is the
    /// reader's place in the grid — and dropped when the grid is gone.
    pub(crate) tables: HashMap<usize, TableScroll>,
    /// The box the last frame drew the note's content in, which is what a grid
    /// is clipped to and what the table toolbar anchors inside.
    pub(crate) content_bounds: Bounds<Pixels>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) reveal: bool,
    pub(crate) upstream: bool,
    selecting: bool,
    pub(crate) preferred_x: Option<Pixels>,
    published_revision: u64,
    undo_group_depth: usize,
    pub(crate) accessible_text: Rc<RefCell<accessibility::AccessibleText>>,
    focus_subscriptions: Option<[Subscription; 3]>,
    pub(crate) caret_blink: caret::CaretBlink,
    /// Whether the editor holds the window's focus, regardless of window activation.
    pub(crate) focused: bool,
    caret_focused: bool,
    caret_blink_task: Option<gpui::Task<()>>,
    // The overlay scrollbar shows while scrolling and fades once scrolling stops.
    scrollbar_active: bool,
    scrollbar_task: Option<gpui::Task<()>>,
}

impl EventEmitter<EditorEvent> for EditorView {}
impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The extensions every view configures, whatever the host adds.
fn base_extensions() -> markraft_core::Extension {
    markraft_core::Extension::all([
        markraft_core::history(HistoryConfig {
            new_group_delay: TYPING_GROUP_DELAY,
            ..HistoryConfig::default()
        }),
        markraft_core::composition(),
        markraft_core::projection::projection(),
    ])
}

fn build_state(schema: &Schema, host: &markraft_core::Extension, doc: Option<Node>) -> EditorState {
    let extensions = markraft_core::Extension::all([base_extensions(), host.clone()]);
    let config = EditorStateConfig::new(schema.clone()).extensions(extensions.clone());
    let config = match doc {
        Some(doc) => config.doc(doc),
        None => config,
    };
    EditorState::create(config).unwrap_or_else(|_| {
        // A document the schema rejects is a host bug; an empty one keeps the
        // view usable rather than taking the process down.
        EditorState::create(EditorStateConfig::new(schema.clone()).extensions(extensions))
            .expect("the schema describes a valid empty document")
    })
}

impl EditorView {
    pub fn new(setup: Setup, cx: &mut Context<Self>) -> Self {
        let Setup {
            schema,
            types,
            extensions,
            codecs,
            mark_toggle,
            doc,
        } = setup;
        let state = build_state(&schema, &extensions, doc);
        let projection = projection_of(&state);
        Self {
            document_guard: None,
            edit_error: None,
            file_paste: false,
            types,
            codecs,
            mark_toggle,
            extension_selection: state.selection().clone(),
            overlay_open: false,
            state,
            projection,
            host_extensions: extensions,
            extensions: Vec::new(),
            placeholder: SharedString::default(),
            aria_label: DEFAULT_ARIA_LABEL.into(),
            single_line: false,
            single_line_scroll_x: px(0.),
            focus: cx.focus_handle(),
            layout: vec![],
            tables: HashMap::new(),
            content_bounds: Bounds::default(),
            scroll: ScrollHandle::new(),
            reveal: false,
            upstream: false,
            selecting: false,
            preferred_x: None,
            published_revision: 0,
            undo_group_depth: 0,
            accessible_text: Rc::default(),
            focus_subscriptions: None,
            caret_blink: caret::CaretBlink::default(),
            focused: false,
            caret_focused: false,
            caret_blink_task: None,
            scrollbar_active: false,
            scrollbar_task: None,
            shaping: shaping::Shaping::default(),
        }
    }

    /// A literal, single-line input for host-owned query and settings fields.
    /// Enter is left to the host; native composition and selection stay enabled.
    ///
    /// It runs on its own `doc > paragraph > text` schema, so nothing it is told
    /// to do can produce a second block or a mark.
    pub fn single_line(cx: &mut Context<Self>) -> Self {
        let mut view = EditorView::new(Setup::new(single_line::schema().clone()), cx);
        view.single_line = true;
        view
    }

    pub fn with_style(mut self, style: EditorStyle) -> Self {
        self.shaping.set_style(style);
        self
    }
    /// Resolve relative image URLs against the directory containing the document.
    pub fn with_image_base(mut self, directory: Option<std::path::PathBuf>) -> Self {
        self.shaping.set_image_base(directory);
        self
    }

    /// Configure the preview root for slash-prefixed image URLs. Relative paths
    /// still use the document directory, and explicit `file://` URLs stay absolute.
    /// An unsupported root disables these previews instead of guessing a location.
    pub fn with_image_root(mut self, root: Result<Option<std::path::PathBuf>, String>) -> Self {
        self.shaping.set_image_root(root);
        self
    }

    /// Update an image preview root without changing document content or history.
    pub fn set_image_root(
        &mut self,
        root: Result<Option<std::path::PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        self.shaping.set_image_root(root);
        cx.notify();
    }

    /// Say which wiki link targets can be opened, so that a link leading nowhere is
    /// not drawn as one that leads somewhere. Without this every link is drawn as
    /// followable, which is what an editor with no notion of pages should do.
    pub fn set_wiki_resolver(
        &mut self,
        resolves: impl Fn(&str) -> bool + 'static,
        cx: &mut Context<Self>,
    ) {
        self.shaping.set_wiki(Box::new(resolves));
        cx.notify();
    }

    /// Say which parts of a line are kept exactly as written, so the view can shade
    /// them. An editor whose host says nothing shades nothing, which is what an
    /// editor with no protected syntax should do.
    pub fn set_protected_spans(
        &mut self,
        spans: impl Fn(&str) -> Vec<std::ops::Range<usize>> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.shaping.set_protected(Box::new(spans));
        cx.notify();
    }

    /// Validate a complete candidate before changing state, including undo and IME.
    pub fn with_document_guard(
        mut self,
        guard: impl Fn(&Node) -> Result<(), EditRejection> + 'static,
    ) -> Self {
        self.document_guard = Some(Box::new(guard));
        self
    }

    pub fn take_edit_error(&mut self) -> Option<EditRejection> {
        self.edit_error.take()
    }

    pub fn with_file_paste(mut self, enabled: bool) -> Self {
        self.file_paste = enabled;
        self
    }

    pub fn set_image_base(
        &mut self,
        directory: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.shaping.set_image_base(directory);
        cx.notify();
    }

    /// Refresh changed image files without modifying document state or history.
    pub fn refresh_images(&mut self, cx: &mut Context<Self>) {
        if self.shaping.refresh_images() {
            cx.notify();
        }
    }
    pub fn with_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
    /// Name this editor for assistive technology, in place of the generic default.
    pub fn with_aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = label.into();
        self
    }
    pub fn set_aria_label(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.aria_label = label.into();
        cx.notify();
    }
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }
    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.shaping.set_style(style);
        self.reveal = true;
        cx.notify();
    }

    /// The editor's state: document, selection and every extension field.
    pub fn state(&self) -> &EditorState {
        &self.state
    }
    pub fn doc(&self) -> &Node {
        self.state.doc()
    }
    /// The persistent document, excluding the input method’s uncommitted candidate.
    pub fn committed_document(&self) -> &Node {
        markraft_core::committed_document(&self.state)
    }
    pub fn schema(&self) -> &Schema {
        self.state.schema()
    }
    /// The document's flattened, line-oriented view.
    pub fn projection(&self) -> Arc<Projection> {
        self.projection.clone()
    }
    /// The same projection, as the handle the laid-out rows are keyed by: a
    /// document that has not changed hands back the very same `Arc`.
    pub(crate) fn projection_arc(&self) -> &Arc<Projection> {
        &self.projection
    }
    /// The whole document as text, lines joined by `'\n'`.
    pub fn text(&self) -> &str {
        self.projection.plain_text()
    }
    /// The style rows are shaped and drawn with.
    pub(crate) fn style(&self) -> &crate::style::EditorStyle {
        self.shaping.style()
    }
    /// Everything shaping reads besides the document, the projection and the
    /// width, and the rows it last produced from them.
    pub(crate) fn shaping(&self) -> &shaping::Shaping {
        &self.shaping
    }
    pub(crate) fn shape_input(&self) -> ShapeInput<'_> {
        let doc = self.state.doc();
        let selection = self.state.selection();
        ShapeInput {
            doc,
            types: &self.types,
            projection: &self.projection,
            style: self.shaping.style(),
            single_line: self.single_line,
            images: self.shaping.images(),
            wiki: self.shaping.wiki(),
            protected: self.shaping.protected(),
            selection: selection.from(doc)..selection.to(doc),
            composition: markraft_core::composition_range(&self.state)
                .map(|range| range.from..range.to),
        }
    }
    /// The laid-out row holding `pos`, and the `char` offset into it.
    ///
    /// Found by asking the rows, not by taking a line index from the projection
    /// and indexing `layout` with it: the layout is one frame's work and the
    /// projection is the live document, and a lookup that assumes they agree
    /// silently answers for the wrong row when they do not.
    pub(crate) fn row_at(&self, pos: usize) -> Option<(&LayoutLine, usize)> {
        let row = self.layout.iter().find(|row| row.contains(pos))?;
        Some((row, row.pos_to_offset(pos)))
    }

    /// The caret, or the moving end of a range.
    pub fn head(&self) -> usize {
        self.state.selection().head(self.state.doc())
    }
    pub fn is_composing(&self) -> bool {
        markraft_core::is_composing(&self.state)
    }

    /// Whether an extension's popup — the `/` menu, the emoji list — is on screen.
    /// A host whose window chrome the platform draws above the whole view reads this
    /// to keep that chrome off the popup.
    pub fn overlay_open(&self) -> bool {
        self.overlay_open
    }

    /// Height at the most recently laid-out width, including editor padding.
    pub fn content_height(&self) -> Option<Pixels> {
        (!self.layout.is_empty()).then(|| {
            self.layout.iter().fold(
                (if self.single_line { px(0.) } else { px(40.) })
                    + self.style().padding * 2.
                    + self.style().top_overlay
                    + self.style().bottom_overlay,
                |height, row| height + row.top_gap + row.height,
            )
        })
    }

    /// Marks shared by all selected text, or the marks new text would get.
    pub fn active_marks(&self) -> MarkSet {
        format_state::active_marks(&self.state)
    }
    /// The type and attributes every selected block shares; mixed formats give `None`.
    pub fn active_block_type(&self) -> Option<(NodeTypeId, Attrs)> {
        format_state::active_block_type(&self.state, &self.projection)
    }

    /// Replace the document, discarding the undo history with it.
    pub fn replace_doc(&mut self, doc: Node, cx: &mut Context<Self>) {
        let doc = if self.single_line {
            single_line::document(&doc, &self.state.schema().clone(), self.codecs.as_deref())
        } else {
            doc
        };
        self.single_line_scroll_x = px(0.);
        self.state = build_state(
            &self.state.schema().clone(),
            &self.host_extensions,
            Some(doc),
        );
        self.projection = projection_of(&self.state);
        self.extension_selection = self.state.selection().clone();
        self.undo_group_depth = 0;
        self.layout.clear();
        self.tables.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.reset_caret_blink(cx);
        self.publish(cx);
        self.run_extensions(
            extension::Update {
                committed: true,
                replaced: true,
                ..extension::Update::default()
            },
            cx,
        );
    }

    /// Replace a single-line editor's value.
    pub fn set_value(&mut self, value: &str, cx: &mut Context<Self>) {
        let doc = single_line::document_from_text(&single_line::text(value, self.single_line));
        self.replace_doc(doc, cx);
    }

    /// Apply `specs` as one transaction, together with whatever the configured
    /// appenders add. `None` when the edit could not be built.
    pub(crate) fn apply(
        &mut self,
        specs: impl IntoIterator<Item = TransactionSpec>,
    ) -> Option<Vec<Transaction>> {
        let transactions = match apply_guarded(&mut self.state, specs, self.document_guard.as_ref())
        {
            Ok(transactions) => transactions,
            Err(error) => {
                self.edit_error = Some(error);
                return None;
            }
        };
        self.projection = projection_of(&self.state);
        Some(transactions)
    }

    /// The editing funnel: apply, publish and tell the extensions.
    ///
    /// `discarding` marks an edit that takes back uncommitted text — cancelling a
    /// composition — so extensions are not told the document changed for good.
    fn edit(
        &mut self,
        cx: &mut Context<Self>,
        discarding: bool,
        specs: Vec<TransactionSpec>,
    ) -> Option<bool> {
        let composing = self.is_composing();
        let transactions = self.apply(specs)?;
        let changed = transactions.iter().any(Transaction::doc_changed);
        self.upstream = false;
        if changed || composing != self.is_composing() {
            self.publish(cx);
        }
        self.preferred_x = None;
        self.reveal = true;
        self.reset_caret_blink(cx);
        cx.notify();
        let composing_now = self.is_composing();
        self.run_extensions(
            extension::Update {
                committed: !discarding && !composing_now && (changed || composing),
                transactions,
                ..extension::Update::default()
            },
            cx,
        );
        Some(changed)
    }

    /// Run a catalogue command. `false` when it does not apply.
    pub fn run_command(&mut self, command: &Command, cx: &mut Context<Self>) -> bool {
        match command(&self.state) {
            Some(spec) => self.edit(cx, false, vec![spec]).is_some(),
            None => false,
        }
    }

    /// Apply specs the host built itself.
    pub fn dispatch(
        &mut self,
        specs: impl IntoIterator<Item = TransactionSpec>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.edit(cx, false, specs.into_iter().collect())
            .unwrap_or(false)
    }

    /// Fold every undo entry made until [`EditorView::end_undo_group`] into one.
    pub fn begin_undo_group(&mut self) {
        let _ = self.apply([TransactionSpec::new()
            .effect(markraft_core::begin_undo_group().of(()))
            .add_to_history(false)]);
        self.undo_group_depth += 1;
    }

    pub fn end_undo_group(&mut self) {
        while self.undo_group_depth > 0 {
            self.undo_group_depth -= 1;
            let _ = self.apply([TransactionSpec::new()
                .effect(markraft_core::end_undo_group().of(()))
                .add_to_history(false)]);
        }
    }

    /// Restore the content and selection from before the input method started.
    pub fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Some(spec) = markraft_core::cancel_composition(&self.state) {
            self.edit(cx, true, vec![spec]);
        }
    }

    /// Select everything, as ⌘A does. A host that fills a field with a value meant to
    /// be typed over calls this, so the first keystroke replaces it.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let command = commands::select_all(&self.types);
        self.run_command(&command, cx);
    }

    pub fn toggle_mark(&mut self, ty: MarkTypeId, attrs: Attrs, cx: &mut Context<Self>) {
        let command = self.mark_command(ty, attrs);
        self.run_command(&command, cx);
    }

    /// The host's way of toggling `ty`, or the model's where it has none.
    fn mark_command(&self, ty: MarkTypeId, attrs: Attrs) -> markraft_core::commands::Command {
        match &self.mark_toggle {
            Some(toggle) => toggle(ty, attrs),
            None => markraft_core::commands::toggle_mark(ty, attrs),
        }
    }

    pub fn set_block_type(&mut self, ty: NodeTypeId, attrs: Attrs, cx: &mut Context<Self>) {
        let command = markraft_core::commands::set_block_type(ty, attrs);
        self.run_command(&command, cx);
    }

    /// The link containing the caret, or the one shared by the whole selection.
    pub fn active_link(&self) -> Option<String> {
        links::active_link(&self.state, self.types.link?)
    }

    /// Link the selection to `url`, or unlink it with `None`.
    pub fn set_link(&mut self, url: Option<&str>, cx: &mut Context<Self>) {
        if self.single_line {
            return;
        }
        let Some(ty) = self.types.link else { return };
        if let Some(spec) = links::set_link(&self.state, ty, url) {
            self.edit(cx, false, vec![spec]);
        }
    }

    /// The URL of the link drawn under `point`.
    fn link_under(&self, point: Point<Pixels>) -> Option<String> {
        let ty = self.types.link?;
        let pos = self.hit(point);
        let (range, mark) = links::link_at(self.state.doc(), ty, pos)?;
        let (row, offset) = self.row_at(range.start)?;
        row.rectangles(offset..row.pos_to_offset(range.end), false)
            .iter()
            .any(|bounds| bounds.contains(&point))
            .then(|| {
                mark.attrs
                    .get("href")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned()
            })
    }

    /// Open `url` in the browser if it is a web or mail address.
    pub fn open_link(url: &str, cx: &mut App) {
        if let Some(url) = openable_url(url) {
            cx.open_url(&url);
        }
    }

    /// The language of the code block starting at `pos`.
    pub fn code_language(&self, pos: usize) -> Option<String> {
        let node = self.state.doc().node_at(pos)?;
        (Some(node.type_id()) == self.types.code_block).then(|| {
            node.attrs()
                .get("language")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned()
        })
    }

    pub fn code_header_bounds(&self, pos: usize) -> Option<Bounds<Pixels>> {
        self.layout
            .iter()
            .find(|row| row.code_pos == Some(pos))?
            .code_language_bounds()
    }

    /// Set the language of the code block starting at `pos`.
    pub fn set_code_language_at(&mut self, pos: usize, language: &str, cx: &mut Context<Self>) {
        if self.single_line || self.code_language(pos).is_none() {
            return;
        }
        let Some(node) = self.state.doc().node_at(pos) else {
            return;
        };
        let updated = node.with_attrs(node.attrs().with("language", language));
        let slice =
            markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(updated));
        let change = markraft_core::Change::replace(pos, pos + node.node_size(), slice);
        // The node keeps its size, so the caret keeps its position.
        if let Some(spec) =
            markraft_core::commands::changes_spec(&self.state, vec![change], "format.block")
        {
            let spec = spec.selection(self.state.selection().clone());
            self.edit(cx, false, vec![spec]);
        }
    }

    /// Where the caret's table is, or `None` outside one. Valid after a paint.
    pub fn table_at_caret(&self) -> Option<TableInfo> {
        let head = self.head();
        let line = self.layout.iter().find(|line| line.contains(head))?;
        let cell = line.table?;
        let bounds = self
            .layout
            .iter()
            .filter(|line| line.table.is_some_and(|other| other.table == cell.table))
            .filter_map(LayoutLine::cell_bounds)
            .reduce(|all, bounds| all.union(&bounds))?;
        // A grid wider than the note is scrolled inside it, so the toolbar
        // anchors to the part of it the reader can actually see.
        let bounds = bounds.intersect(&self.content_bounds);
        Some(TableInfo {
            bounds,
            cell_bounds: line.cell_bounds()?,
            row: cell.row,
            column: cell.column,
            rows: cell.rows,
            columns: cell.columns,
            alignment: cell.alignment,
        })
    }

    /// Run one of the catalogue's table commands.
    ///
    /// `false` where the schema declares no table types, and where the command
    /// does not apply — which, for all but [`EditorView::insert_table`], means
    /// the caret is not in a table.
    fn table_command(
        &mut self,
        build: impl FnOnce(markraft_core::commands::TableTypes) -> Command,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.single_line {
            return false;
        }
        let Some(types) = self.types.table_types() else {
            return false;
        };
        self.run_command(&build(types), cx)
    }

    /// Insert a `rows` by `columns` table of empty cells, caret in the first.
    pub fn insert_table(&mut self, rows: usize, columns: usize, cx: &mut Context<Self>) -> bool {
        self.table_command(
            |types| markraft_core::commands::insert_table(types, rows, columns),
            cx,
        )
    }

    /// Insert an empty row above the caret's row. A row inserted above the
    /// header becomes the new header.
    pub fn table_add_row_before(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::add_row_before, cx)
    }

    /// Insert an empty row below the caret's row.
    pub fn table_add_row_after(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::add_row_after, cx)
    }

    /// Insert an empty column to the left of the caret's column.
    pub fn table_add_column_before(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::add_column_before, cx)
    }

    /// Insert an empty column to the right of the caret's column.
    pub fn table_add_column_after(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::add_column_after, cx)
    }

    /// Delete the caret's row, or the table when it is the only one.
    pub fn table_delete_row(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::delete_row, cx)
    }

    /// Delete the caret's column, or the table when it is the only one.
    pub fn table_delete_column(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::delete_column, cx)
    }

    /// Delete the whole table the caret is in.
    pub fn table_delete_table(&mut self, cx: &mut Context<Self>) -> bool {
        self.table_command(markraft_core::commands::delete_table, cx)
    }

    /// Set the alignment of the caret's column.
    pub fn table_set_alignment(
        &mut self,
        alignment: ColumnAlignment,
        cx: &mut Context<Self>,
    ) -> bool {
        self.table_command(
            move |types| markraft_core::commands::set_column_alignment(types, alignment),
            cx,
        )
    }

    /// Window bounds of the first line of the selection, or of the link touching the
    /// caret, for anchoring host popovers. Valid after the editor has painted.
    pub fn anchor_bounds(&self) -> Option<Bounds<Pixels>> {
        let doc = self.state.doc();
        let (start, end) = (
            self.state.selection().from(doc),
            self.state.selection().to(doc),
        );
        let row = surface::selection_anchor_row(&self.layout, start, end)?;
        let offset = row.pos_to_offset(start);
        let range = if start != end {
            offset..row.pos_to_offset(end)
        } else if let Some((span, _)) = self
            .types
            .link
            .and_then(|ty| links::link_at(doc, ty, start))
        {
            row.pos_to_offset(span.start)..row.pos_to_offset(span.end)
        } else {
            offset..offset
        };
        if range.is_empty() {
            let caret = row.caret(range.start, self.upstream);
            return Some(Bounds::new(caret, size(px(0.), row.line_height)));
        }
        let rectangles = row.rectangles(range, false);
        let first = *rectangles.first()?;
        // Only the first visual row: a popover belongs above where the text begins.
        Some(
            rectangles
                .iter()
                .filter(|rect| rect.origin.y == first.origin.y)
                .fold(first, |all, rect| all.union(rect)),
        )
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.published_revision += 1;
        cx.emit(EditorEvent::Changed {
            revision: self.published_revision,
        });
        cx.notify();
    }

    fn sync_caret_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.focused = self.focus.is_focused(window);
        let focused = window.is_window_active() && self.focused;
        if self.caret_focused != focused {
            self.caret_focused = focused;
            self.reset_caret_blink(cx);
            cx.notify();
        }
    }

    fn flash_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_active = true;
        // Replacing the task cancels the previous hide timer.
        self.scrollbar_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.scrollbar_active = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Thumb offset and height within the editor, from the previous frame's scroll
    /// geometry. None while the content fits.
    fn scrollbar_thumb(&self) -> Option<(Pixels, Pixels)> {
        let inset = px(4.);
        let viewport = self.scroll.bounds().size.height;
        let max = self.scroll.max_offset().y;
        let track = viewport - inset * 2. - self.style().top_overlay - self.style().bottom_overlay;
        if self.single_line || max <= px(1.) || track <= px(48.) {
            return None;
        }
        let height = (track * (viewport / (viewport + max))).max(px(28.));
        let progress = (-self.scroll.offset().y / max).clamp(0., 1.);
        Some((
            self.style().top_overlay + inset + (track - height) * progress,
            height,
        ))
    }

    pub(crate) fn reset_caret_blink(&mut self, cx: &mut Context<Self>) {
        // Dropping the previous task cancels its pending timer. Only the focused
        // editor with a collapsed, committed selection owns a ticking task.
        self.caret_blink_task = None;
        let doc = self.state.doc();
        if !self.caret_blink.reset(
            self.caret_focused,
            self.is_composing(),
            self.state.selection().is_empty(doc),
            self.extension_caret() != CaretShape::Bar || cx.reduce_motion(),
        ) {
            return;
        }
        self.caret_blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(caret::BLINK_INTERVAL).await;
                let keep_blinking = this.update(cx, |this, cx| {
                    if this.caret_blink.tick() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });
                if !matches!(keep_blinking, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Move the selection, as a pointer or an arrow key does.
    fn select(&mut self, head: usize, extend: bool, cx: &mut Context<Self>) {
        let doc = self.state.doc();
        let anchor = if extend {
            self.state.selection().anchor(doc)
        } else {
            head
        };
        let selection = if extend {
            Selection::text(anchor, head)
        } else {
            Selection::near(self.state.schema(), doc, head, 1)
        };
        let mut specs = vec![
            TransactionSpec::new()
                .selection(selection)
                .user_event("select.pointer")
                .scroll_into_view(),
        ];
        if self.is_composing() {
            specs.push(markraft_core::finish_composition().sequential());
        }
        self.edit(cx, false, specs);
    }

    /// The document position under `point`.
    pub(crate) fn hit(&self, point: Point<Pixels>) -> usize {
        let Some(last) = self.layout.last() else {
            return 0;
        };
        if point.y >= last.origin.y + last.height {
            return last.offset_to_pos(last.char_len);
        }
        let row = self
            .layout
            .iter()
            .find(|row| point.y < row.origin.y + row.height)
            .unwrap_or(&self.layout[0]);
        // A grid's cells share one band of y, and only the last of a row
        // carries the row's height, so the search above lands on that one
        // whatever column the point was in. The grid picks the column.
        let row = match row.table.map(|cell| cell.table) {
            Some(table) => surface::cell_under(&self.layout, table, point).unwrap_or(row),
            None => row,
        };
        if row.in_callout_header(point.y) {
            return row.offset_to_pos(0);
        }
        let local = gpui::point(point.x - row.origin.x, point.y - row.origin.y);
        row.hit_position(row.char_at(local), &self.projection)
    }

    fn select_point(&mut self, point: Point<Pixels>, extend: bool, cx: &mut Context<Self>) {
        let (position, upstream) = self.hit_upstream(point);
        self.upstream = upstream;
        self.select(position, extend, cx);
    }

    /// The position under `target`, and whether the caret there belongs to the visual row
    /// above the one the point fell in.
    fn hit_upstream(&self, target: Point<Pixels>) -> (usize, bool) {
        let pos = self.hit(target);
        let upstream = self
            .row_at(pos)
            .map(|(row, offset)| row.caret(offset, false).y > target.y)
            .unwrap_or(false);
        (pos, upstream)
    }

    /// Every visual row's vertical centre, across the whole laid-out document,
    /// each one listed once however many lines sit on it.
    fn visual_row_centers(&self) -> Vec<Pixels> {
        surface::merge_row_centers(
            self.layout
                .iter()
                .flat_map(|row| {
                    (0..row.visual_rows())
                        .map(move |i| row.origin.y + row.line_height * (i as f32 + 0.5))
                })
                .collect(),
        )
    }

    /// The caret `delta` visual rows away and the column to keep there. `None` before the
    /// first paint, when there is no layout to walk.
    pub(crate) fn visual_row_target(&self, delta: isize) -> Option<(usize, Pixels, bool)> {
        let head = self.head();
        let (row, offset) = self.row_at(head)?;
        let caret = row.caret(offset, self.upstream);
        let x = self.preferred_x.unwrap_or(caret.x);
        let centers = self.visual_row_centers();
        if centers.is_empty() {
            return None;
        }
        let current = centers
            .iter()
            .position(|&y| y > caret.y)
            .unwrap_or(centers.len() - 1);
        let next = (current as isize + delta).clamp(0, centers.len() as isize - 1) as usize;
        let target = point(x, centers[next]);
        let (position, upstream) = self.hit_upstream(target);
        Some((position, x, upstream))
    }

    /// The start or end of the caret's visual row. A wrapped block has several.
    pub(crate) fn line_edge_target(&self, end: bool) -> Option<(usize, bool)> {
        let head = self.head();
        let (row, offset) = self.row_at(head)?;
        let caret = row.caret(offset, self.upstream);
        Some(self.hit_upstream(point(
            if end {
                row.origin.x + row.width
            } else {
                row.origin.x
            },
            caret.y + row.line_height * 0.5,
        )))
    }

    fn vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let head = self.head();
        let last_line = self.projection.line_count().saturating_sub(1);
        if delta > 0
            && !extend
            && self.state.selection().is_cursor()
            && self.projection.line_at(head) == Some(last_line)
            && self
                .projection
                .line(last_line)
                .is_some_and(|line| line.to == head && self.types.is_verbatim_block(line))
        {
            let command = markraft_core::commands::exit_code();
            if self.run_command(&command, cx) {
                return;
            }
        }
        if let Some((position, x, upstream)) = self.visual_row_target(delta) {
            self.upstream = upstream;
            self.select(position, extend, cx);
            self.preferred_x = Some(x);
        }
    }

    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        if let Some((position, upstream)) = self.line_edge_target(end) {
            self.preferred_x = None;
            self.upstream = upstream;
            self.select(position, extend, cx);
        }
    }

    /// The selected content, as a slice.
    pub fn selection_slice(&self) -> markraft_core::Slice {
        self.state
            .selection()
            .content_with_schema(self.state.doc(), self.state.schema())
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let slice = self.selection_slice();
        if slice.is_empty() {
            return;
        }
        let schema = self.state.schema().clone();
        match self.codecs.clone().filter(|_| !self.single_line) {
            Some(codecs) => clipboard::write(&schema, codecs.as_ref(), &slice, cx),
            // Without codecs there is only one flavour to write, and a
            // single-line editor holds nothing but text anyway.
            None => {
                let text = markraft_core::projection::slice_to_plain_text(&schema, &slice);
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn paste(&mut self, mode: clipboard::PasteMode, cx: &mut Context<Self>) {
        let item = cx
            .read_from_clipboard()
            .unwrap_or_else(|| ClipboardItem::new_string(String::new()));
        if self.file_paste
            && !matches!(mode, clipboard::PasteMode::Plain)
            && item.entries().iter().any(|entry| {
                matches!(
                    entry,
                    ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
                )
            })
        {
            cx.emit(EditorEvent::FilesPasted(item));
            return;
        }
        let clipboard_text = item.text();
        let text = clipboard_text.as_deref().unwrap_or_default();
        let literal = self.single_line
            || self.codecs.is_none()
            || matches!(mode, clipboard::PasteMode::Plain)
            || self.types.in_verbatim_block_at(&self.state);
        if literal && clipboard_text.is_none() {
            return;
        }
        let schema = self.state.schema().clone();
        let spec = if literal {
            let text = single_line::text(text, self.single_line);
            keymap::insert_plain(&self.types, &text)(&self.state)
        } else if is_web_url(text.trim())
            && self.types.link.is_some()
            && (!self.state.selection().is_empty(self.state.doc()) || self.active_link().is_none())
        {
            links::set_link(
                &self.state,
                self.types.link.expect("checked"),
                Some(text.trim()),
            )
        } else if let Some(slice) = self
            .codecs
            .clone()
            .and_then(|codecs| clipboard::read_fragment(&schema, codecs.as_ref(), &item, mode))
        {
            markraft_core::commands::replace_selection(slice)(&self.state)
        } else {
            None
        };
        if let Some(spec) = spec {
            self.edit(cx, false, vec![spec.user_event("input.paste")]);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.preferred_x = None;
        if event.modifiers.platform
            && let Some(url) = self.link_under(event.position)
        {
            self.selecting = false;
            Self::open_link(&url, cx);
            return;
        }
        let position = self.hit(event.position);
        if !event.modifiers.shift
            && let Some(pos) = self.raw_html_under(event.position)
        {
            self.selecting = false;
            cx.emit(EditorEvent::RawHtmlRequested { pos });
            return;
        }
        // Task markers are presentation outside the text coordinate space.
        if let Some((row, _)) = self.row_at(position)
            && row
                .task_marker()
                .is_some_and(|(_, bounds)| bounds.contains(&event.position))
        {
            let target = row.from;
            self.selecting = false;
            self.run_control(
                accessibility::ControlAction::ToggleTask(target),
                true,
                window,
                cx,
            );
            return;
        }
        self.select_point(event.position, event.modifiers.shift, cx);
        if event.click_count == 1
            && !event.modifiers.shift
            && !self.single_line
            && self.link_under(event.position).is_some()
        {
            cx.emit(EditorEvent::LinkClicked);
        }
        // A wiki link is an atom rather than a mark, so it carries no link mark
        // for `link_under` to find; the same click follows it.
        if event.click_count == 1
            && !event.modifiers.shift
            && !self.single_line
            && let Some(pos) = self.wiki_link_under(event.position)
            && let Some(node) = self.wiki_link_at(pos)
        {
            cx.emit(EditorEvent::WikiLinkClicked {
                target: wiki::wiki_link_target(&node),
                embed: wiki::wiki_link_embed(&node),
            });
        }
        let head = self.head();
        if event.click_count >= 3 {
            if let Some((index, _)) = self.projection.pos_to_line_offset(head)
                && let Some(line) = self.projection.line(index)
            {
                self.select_range(line.from, line.to, cx);
            }
        } else if event.click_count == 2 {
            let from = self.projection.prev_word_boundary(head).unwrap_or(head);
            let to = self.projection.next_word_boundary(from).unwrap_or(head);
            self.select_range(from, to, cx);
        }
        self.reset_caret_blink(cx);
        cx.notify();
    }

    fn select_range(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.edit(
            cx,
            false,
            vec![
                TransactionSpec::new()
                    .selection(Selection::text(from, to))
                    .user_event("select.pointer"),
            ],
        );
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_point(event.position, true, cx);
        }
    }
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    /// The one answer gpui asks the input handler for, and uses twice: it gates inserted
    /// text, and `ElementInputHandler` also returns it for
    /// `prefers_ime_for_printable_keys`, which is what decides whether a printable key
    /// goes to an active input method before key-binding matching.
    ///
    /// A live composition always accepts: the input method owns the marked text, so an
    /// extension that refuses mid-composition is ignored until it ends rather than
    /// stranding the candidate.
    pub(crate) fn accepts_text_input(&self) -> bool {
        self.is_composing() || self.extension_input_policy() == InputPolicy::Accept
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_subscriptions.is_none() {
            self.focus_subscriptions = Some([
                cx.on_focus(&self.focus, window, |this, window, cx| {
                    this.sync_caret_focus(window, cx);
                    this.run_extensions(extension::Update::default(), cx);
                }),
                cx.on_blur(&self.focus, window, |this, window, cx| {
                    this.selecting = false;
                    this.sync_caret_focus(window, cx);
                    // The input method owns commit/unmark ordering. A host that
                    // dismisses an editor can explicitly cancel composition.
                    this.run_extensions(extension::Update::default(), cx);
                }),
                cx.observe_window_activation(window, |this, window, cx| {
                    this.sync_caret_focus(window, cx);
                }),
            ]);
        }
        self.sync_caret_focus(window, cx);
        self.prune_extensions();
        let key_context = self.extension_key_context();
        let overlay = self.extension_overlay(window, cx);
        self.overlay_open = overlay.is_some();
        let accessible_text = self.accessible_text.clone();
        let mut root = div()
            .id("markraft-editor")
            .role(if self.single_line {
                Role::TextInput
            } else {
                Role::MultilineTextInput
            })
            .aria_label(self.aria_label.clone())
            .aria_placeholder(self.placeholder.clone())
            .a11y_synthetic_children(move |builder| accessible_text.borrow_mut().write(builder))
            .size_full()
            .key_context(key_context)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up));
        root = self.bind_accessibility_actions(root, cx);
        // Extension listeners run from window dispatch, so they may update the editor.
        // One listener per action: a listener consumes its action, so with one each the
        // first extension's would starve the others that listen for the same action.
        let mut listeners: Vec<(Box<dyn Action>, Vec<_>)> = Vec::new();
        for (id, handler) in self.extension_actions() {
            let entry = (id, handler.run);
            match listeners
                .iter_mut()
                .find(|(action, _)| action.partial_eq(&*handler.action))
            {
                Some((_, runs)) => runs.push(entry),
                None => listeners.push((handler.action, vec![entry])),
            }
        }
        for (action, runs) in listeners {
            root = root.on_boxed_action(
                &*action,
                cx.listener(move |this, _: &dyn Action, _, cx| {
                    for (id, run) in &runs {
                        this.run_extension_action(id, run, cx);
                    }
                }),
            );
        }
        root = self.bind_editing_actions(root, cx);
        let root = root
            .when(self.single_line, |this| {
                this.overflow_x_hidden().overflow_y_hidden()
            })
            .when(!self.single_line, |this| this.overflow_y_scroll())
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.flash_scrollbar(cx)))
            .p(self.style().padding)
            .pt(self.style().padding + self.style().top_overlay)
            .pb(self.style().padding + self.style().bottom_overlay)
            .bg(self.style().background)
            .text_color(self.style().text)
            .child(EditorSurface {
                editor: cx.entity(),
            });
        // The thumb sits beside the scroller rather than inside it, so it does not
        // scroll with the content. It is an indicator only and takes no pointer input.
        let active = self.scrollbar_active;
        let color = self.style().scrollbar;
        // Reduced motion keeps the fade's two end states and drops the travel
        // between them.
        let reduce_motion = cx.reduce_motion();
        let editor = cx.entity();
        div()
            .size_full()
            .relative()
            .child(root)
            .when_some(self.scrollbar_thumb(), |this, (top, height)| {
                let thumb = div()
                    .absolute()
                    .top(top)
                    .right(px(3.))
                    .w(px(6.))
                    .h(height)
                    .rounded_full()
                    .bg(color);
                this.child(if reduce_motion {
                    thumb
                        .opacity(if active { 1. } else { 0. })
                        .into_any_element()
                } else {
                    thumb
                        .with_spring(
                            "scrollbar-fade",
                            SpringAnimation::new(SpringConfig::new(500., 45., 1.)).to(active),
                            |s, phase| s.opacity(phase.interpolate_clamped(0., 1.)),
                        )
                        .into_any_element()
                })
            })
            // Deferred so it draws over the note and is positioned after the surface has
            // published this frame's rows.
            .when_some(overlay, |this, overlay: Overlay| {
                this.child(deferred(AnchoredOverlay {
                    editor,
                    anchor: overlay.anchor,
                    gap: overlay.gap,
                    child: overlay.element,
                }))
            })
    }
}

impl EditorView {
    /// The editor's own key bindings, each a chain of catalogue commands.
    fn bind_editing_actions(
        &self,
        mut root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        macro_rules! run {
            ($action:ty, $build:expr) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    let command: Command = $build(&this.types);
                    if !this.run_command(&command, cx) {
                        cx.propagate();
                    }
                }));
            };
        }
        macro_rules! rich {
            ($action:ty, $build:expr) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    if this.single_line {
                        cx.propagate();
                        return;
                    }
                    let command: Command = $build(&this.types);
                    if !this.run_command(&command, cx) {
                        cx.propagate();
                    }
                }));
            };
        }
        run!(Backspace, keymap::backspace);
        run!(Delete, keymap::delete_forward);
        root = root.on_action(cx.listener(|this, _: &Enter, _, cx| {
            if this.single_line {
                cx.propagate();
                return;
            }
            let command = keymap::enter(&this.types);
            if !this.run_command(&command, cx) {
                cx.propagate();
            }
        }));
        rich!(Indent, keymap::indent);
        rich!(Outdent, keymap::outdent);
        run!(Left, |_: &DocTypes| keymap::move_grapheme(
            Direction::Backward,
            false
        ));
        run!(Right, |_: &DocTypes| keymap::move_grapheme(
            Direction::Forward,
            false
        ));
        run!(SelectLeft, |_: &DocTypes| keymap::move_grapheme(
            Direction::Backward,
            true
        ));
        run!(SelectRight, |_: &DocTypes| keymap::move_grapheme(
            Direction::Forward,
            true
        ));
        run!(WordLeft, |_: &DocTypes| keymap::move_word(
            Direction::Backward,
            false
        ));
        run!(WordRight, |_: &DocTypes| keymap::move_word(
            Direction::Forward,
            false
        ));
        run!(SelectWordLeft, |_: &DocTypes| keymap::move_word(
            Direction::Backward,
            true
        ));
        run!(SelectWordRight, |_: &DocTypes| keymap::move_word(
            Direction::Forward,
            true
        ));
        run!(DeleteWordBackward, |types: &DocTypes| keymap::delete_word(
            types,
            Direction::Backward
        ));
        run!(DeleteWordForward, |types: &DocTypes| keymap::delete_word(
            types,
            Direction::Forward
        ));
        run!(DocumentStart, |_: &DocTypes| keymap::move_document_edge(
            false, false
        ));
        run!(DocumentEnd, |_: &DocTypes| keymap::move_document_edge(
            true, false
        ));
        run!(SelectDocumentStart, |_: &DocTypes| {
            keymap::move_document_edge(false, true)
        });
        run!(SelectDocumentEnd, |_: &DocTypes| {
            keymap::move_document_edge(true, true)
        });
        run!(Undo, |_: &DocTypes| keymap::history(true));
        run!(Redo, |_: &DocTypes| keymap::history(false));
        run!(SelectAll, keymap::select_all);
        macro_rules! mark {
            ($action:ty, $role:ident) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    let command = this
                        .types
                        .$role
                        .filter(|_| !this.single_line)
                        .map(|ty| this.mark_command(ty, Attrs::empty()));
                    match command {
                        Some(command) if this.run_command(&command, cx) => {}
                        _ => cx.propagate(),
                    }
                }));
            };
        }
        mark!(Bold, strong);
        mark!(Italic, em);
        mark!(Code, code);
        mark!(Strikethrough, strikethrough);
        rich!(Paragraph, |types: &DocTypes| block(
            types,
            types.paragraph,
            Attrs::empty()
        ));
        rich!(Heading, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 1i64)])
        ));
        rich!(Heading2, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 2i64)])
        ));
        rich!(Heading3, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 3i64)])
        ));
        rich!(Heading4, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 4i64)])
        ));
        rich!(Heading5, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 5i64)])
        ));
        rich!(Heading6, |types: &DocTypes| block(
            types,
            types.heading,
            Attrs::from_pairs([("level", 6i64)])
        ));
        rich!(CodeBlock, |types: &DocTypes| block(
            types,
            types.code_block,
            Attrs::empty()
        ));
        rich!(Quote, keymap::toggle_quote);
        rich!(Ordered, |types: &DocTypes| list(
            types,
            types.ordered_list,
            types.list_item
        ));
        rich!(Bullet, |types: &DocTypes| list(
            types,
            types.bullet_list,
            types.list_item
        ));
        rich!(Task, |types: &DocTypes| list(
            types,
            types.bullet_list,
            types.task_item
        ));
        rich!(ToggleTask, keymap::toggle_task);
        root = root
            .on_action(cx.listener(|this, _: &EditRawHtml, _, cx| {
                if let Some(pos) = this.raw_html_at_caret() {
                    cx.emit(EditorEvent::RawHtmlRequested { pos });
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ChooseCodeLanguage, window, cx| {
                if let Some(pos) = this.active_code_pos() {
                    this.run_control(
                        accessibility::ControlAction::CodeLanguage(pos),
                        true,
                        window,
                        cx,
                    );
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CopyCodeBlock, window, cx| {
                if let Some(pos) = this.active_code_pos() {
                    this.run_control(
                        accessibility::ControlAction::CopyCode(pos),
                        true,
                        window,
                        cx,
                    );
                } else {
                    cx.propagate();
                }
            }));
        root = root
            .on_action(cx.listener(|this, _: &Up, _, cx| this.vertical(-1, false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.vertical(1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(-1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(1, true, cx)))
            .on_action(cx.listener(|this, _: &Home, _, cx| this.line_edge(false, false, cx)))
            .on_action(cx.listener(|this, _: &End, _, cx| this.line_edge(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.line_edge(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| this.line_edge(true, true, cx)))
            .on_action(cx.listener(|this, _: &CancelComposition, _, cx| {
                if this.is_composing() {
                    this.cancel_composition(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                this.copy(cx);
                let command = markraft_core::commands::delete_selection();
                this.run_command(&command, cx);
            }))
            .on_action(
                cx.listener(|this, _: &Paste, _, cx| {
                    this.paste(clipboard::PasteMode::Formatted, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &PastePlain, _, cx| {
                this.paste(clipboard::PasteMode::Plain, cx)
            }))
            .on_action(cx.listener(|this, _: &PasteMarkdown, _, cx| {
                this.paste(clipboard::PasteMode::Markdown, cx)
            }))
            .on_action(
                cx.listener(|_, _: &CharacterPalette, window, _| window.show_character_palette()),
            );
        root
    }
}

/// Toggle a block type that the schema may not declare.
fn block(types: &DocTypes, ty: Option<NodeTypeId>, attrs: Attrs) -> Command {
    match ty {
        Some(ty) => keymap::toggle_block(types, ty, attrs),
        None => markraft_core::commands::command(|_| None),
    }
}

/// Toggle a list whose type or item type the schema may not declare.
fn list(types: &DocTypes, ty: Option<NodeTypeId>, item: Option<NodeTypeId>) -> Command {
    match (ty, item) {
        (Some(ty), Some(item)) => keymap::toggle_list(types, ty, item),
        _ => markraft_core::commands::command(|_| None),
    }
}

#[cfg(test)]
mod link_tests {
    use super::openable_url;

    #[test]
    fn only_web_and_mail_links_open() {
        assert_eq!(
            openable_url("https://a.example/x").as_deref(),
            Some("https://a.example/x")
        );
        assert_eq!(
            openable_url("mailto:a@b.example").as_deref(),
            Some("mailto:a@b.example")
        );
        assert_eq!(
            openable_url("a.example/x").as_deref(),
            Some("https://a.example/x")
        );
        assert_eq!(
            openable_url("localhost:8080/x").as_deref(),
            Some("https://localhost:8080/x")
        );
        assert_eq!(openable_url("file:///etc/passwd"), None);
        assert_eq!(openable_url("javascript:alert(1)"), None);
    }
}

#[cfg(test)]
mod key_binding_tests {
    use super::list_key_bindings;
    use gpui::{Keystroke, Modifiers};

    #[test]
    fn list_shortcuts_match_macos_shifted_digit_events() {
        let bindings = list_key_bindings();
        for (index, key) in ["&", "*", "("].into_iter().enumerate() {
            // macOS GPUI clears Shift after translating the key to its symbol.
            let typed = Keystroke {
                modifiers: Modifiers::command(),
                key: key.into(),
                key_char: None,
            };
            for (binding_index, binding) in bindings.iter().enumerate() {
                assert_eq!(
                    binding.match_keystrokes(std::slice::from_ref(&typed)),
                    (index == binding_index).then_some(false)
                );
            }
        }
    }
}

#[cfg(test)]
mod document_guard_tests {
    use super::{DocumentGuard, EditRejection, apply_guarded, build_state, clipboard, ime};
    use crate::typeahead::tests::{at, state_of, types_of};
    use markraft_commonmark::{CommonMarkCodecs, commonmark_schema, from_markdown};
    use markraft_core::{
        Attrs, EditorState, Selection, TransactionAppenderFn, TransactionSpec, appended,
        cancel_composition, commands, committed_document, composition_range, is_composing, origin,
        redo, redo_depth, transaction_appender, undo, undo_depth, update_composition,
    };
    use std::sync::Arc;

    fn read_only() -> DocumentGuard {
        Box::new(|_| {
            Err(EditRejection::ReadOnly(
                "This document is read-only.".into(),
            ))
        })
    }

    fn assert_unchanged(before: &EditorState, after: &EditorState) {
        assert_eq!(before.doc(), after.doc());
        assert_eq!(before.selection(), after.selection());
        assert_eq!(undo_depth(before), undo_depth(after));
        assert_eq!(redo_depth(before), redo_depth(after));
        assert_eq!(composition_range(before), composition_range(after));
        assert_eq!(committed_document(before), committed_document(after));
    }

    #[test]
    fn read_only_rejects_commands_no_filter_and_extension_edits_atomically() {
        let initial = at(&state_of("original"), 4);
        let spec = commands::insert_text("changed")(&initial).unwrap();
        let attempts = [
            spec.clone(),
            spec.clone().no_filter(),
            spec.annotate(origin().of("extension:test".into()))
                .no_filter(),
        ];
        for spec in attempts {
            let mut state = initial.clone();
            assert!(apply_guarded(&mut state, [spec], Some(&read_only())).is_err());
            assert_unchanged(&initial, &state);
            assert_eq!(undo_depth(&state), 0);
        }
    }

    #[test]
    fn mark_only_commands_cannot_bypass_read_only() {
        let mut state = state_of("original");
        apply_guarded(
            &mut state,
            [TransactionSpec::new().selection(Selection::text(1, 5))],
            None,
        )
        .unwrap();
        let before = state.clone();
        let strong = state.schema().mark_id("strong").unwrap();
        let spec = commands::toggle_mark(strong, Attrs::empty())(&state).unwrap();
        assert!(apply_guarded(&mut state, [spec.no_filter()], Some(&read_only())).is_err());
        assert_unchanged(&before, &state);
    }

    #[test]
    fn rejecting_undo_or_redo_does_not_consume_history() {
        let original = state_of("base");
        let mut state = original.clone();
        let typed = commands::insert_text("new")(&state).unwrap();
        apply_guarded(&mut state, [typed], None).unwrap();
        let edited = state.clone();
        let undo_spec = undo(&state).unwrap();
        assert!(apply_guarded(&mut state, [undo_spec.clone()], Some(&read_only())).is_err());
        assert_unchanged(&edited, &state);
        apply_guarded(&mut state, [undo_spec], None).unwrap();
        assert_eq!(state.doc(), original.doc());
        let undone = state.clone();
        let redo_spec = redo(&state).unwrap();
        assert!(apply_guarded(&mut state, [redo_spec.clone()], Some(&read_only())).is_err());
        assert_unchanged(&undone, &state);
        apply_guarded(&mut state, [redo_spec], None).unwrap();
        assert_eq!(state.doc(), edited.doc());
    }

    #[test]
    fn rejected_first_ime_candidate_leaves_no_composition_or_history() {
        let mut state = at(&state_of("before 😀"), 4);
        let before = state.clone();
        let candidate = update_composition(&state, "中", 1).unwrap();
        assert!(apply_guarded(&mut state, [candidate], Some(&read_only())).is_err());
        assert_unchanged(&before, &state);
        assert!(!is_composing(&state));
        assert!(cancel_composition(&state).is_none());
    }

    #[test]
    fn rejected_ime_refinement_and_commit_preserve_the_live_candidate() {
        let mut state = at(&state_of("before"), 4);
        let original = state.clone();
        let candidate = update_composition(&state, "中", 1).unwrap();
        apply_guarded(&mut state, [candidate], None).unwrap();
        let preview = state.clone();
        let refinement = update_composition(&state, "中文", 2).unwrap();
        assert!(apply_guarded(&mut state, [refinement], Some(&read_only())).is_err());
        assert_unchanged(&preview, &state);
        let commit = ime::commit_specs(&state, &types_of(&state), None, "中文");
        assert!(apply_guarded(&mut state, commit, Some(&read_only())).is_err());
        assert_unchanged(&preview, &state);
        assert!(is_composing(&state));
        let cancel = cancel_composition(&state).unwrap();
        apply_guarded(&mut state, [cancel], None).unwrap();
        assert_unchanged(&original, &state);
    }

    #[test]
    fn appender_output_is_validated_before_any_transaction_is_published() {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, "base").unwrap();
        let appender: TransactionAppenderFn = Arc::new(|transaction| {
            if !transaction.doc_changed() || transaction.annotation(appended()).is_some() {
                return None;
            }
            commands::insert_text("!")(transaction.state())
        });
        let mut state = build_state(&schema, &transaction_appender().of(appender), Some(doc));
        let before = state.clone();
        let guard_schema = schema.clone();
        let forbid_exclamation: DocumentGuard = Box::new(move |doc| {
            if guard_schema.describe(doc).contains('!') {
                Err(EditRejection::Protected(
                    "Unsupported appender output.".into(),
                ))
            } else {
                Ok(())
            }
        });
        let edit = commands::insert_text("safe")(&state).unwrap();
        assert!(apply_guarded(&mut state, [edit.clone()], Some(&forbid_exclamation)).is_err());
        assert_unchanged(&before, &state);
        let transactions = apply_guarded(&mut state, [edit], None).unwrap();
        assert_eq!(transactions.len(), 2);
        assert!(schema.describe(state.doc()).contains('!'));
        assert_eq!(undo_depth(&state), 1);
        let undo_spec = undo(&state).unwrap();
        // This appender reacts to every document change, so inspect the inverse
        // directly rather than asking it to append to an undo once again.
        let undone = state.update([undo_spec]).unwrap();
        assert_eq!(undone.new_doc(), before.doc());
    }

    #[test]
    fn a_rejection_reaches_the_host_as_the_case_it_was_refused_for() {
        let initial = at(&state_of("original"), 4);
        let mut state = initial.clone();
        // A guard's own case is carried through untouched, so the host can tell a
        // file that takes no edit at all from one change it cannot keep.
        let spec = commands::insert_text("changed")(&state).unwrap();
        assert_eq!(
            apply_guarded(&mut state, [spec.clone()], Some(&read_only())).unwrap_err(),
            EditRejection::ReadOnly("This document is read-only.".into())
        );
        assert_unchanged(&initial, &state);
        let protected: DocumentGuard =
            Box::new(|_| Err(EditRejection::Protected("Protected source.".into())));
        assert_eq!(
            apply_guarded(&mut state, [spec], Some(&protected)).unwrap_err(),
            EditRejection::Protected("Protected source.".into())
        );
        assert_unchanged(&initial, &state);
    }

    #[test]
    fn read_only_preserves_selection_and_copy_content() {
        let mut state = state_of("copy **this**");
        let before = state.doc().clone();
        let selection = Selection::text(1, state.doc().content_size() - 1);
        apply_guarded(
            &mut state,
            [TransactionSpec::new().selection(selection)],
            Some(&read_only()),
        )
        .unwrap();
        let slice = state
            .selection()
            .content_with_schema(state.doc(), state.schema());
        let codecs = CommonMarkCodecs::new(state.schema().clone());
        assert_eq!(clipboard::markup(&codecs, &slice), "copy **this**");
        assert_eq!(state.doc(), &before);
        assert_eq!(undo_depth(&state), 0);
    }
}
