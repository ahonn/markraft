//! Native editing adapter. The core owns document semantics; the host owns persistence.
//!
//! The view holds an [`EditorState`] built from the host's schema and
//! extensions. Every edit is a [`TransactionSpec`] or a
//! [`markraft_core::commands::Command`] from the catalogue; nothing here
//! touches the tree. Everything drawn comes from the state's
//! [`markraft_core::projection::Projection`].

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod accessibility;
mod callout;
mod caret;
mod clipboard;
mod completion;
mod emoji;
mod extension;
mod find;
mod footnotes;
mod format_state;
mod images;
pub mod ime;
mod layout;
mod links;
mod shaping;
mod shown;
mod single_line;
mod style;
mod surface;
mod syntax;
mod typeahead;
mod wiki;
pub use clipboard::use_system_pasteboard;
pub use emoji::{EmojiInsertion, EmojiShortcodes, emoji_menu};
pub use extension::{
    ActionHandler, CaretShape, EXTENSION_ORIGIN_PREFIX, EditorCx, Extension, ExtensionHandle,
    ExtensionPayload, InputPolicy, Overlay, Update,
};
pub use find::FindStatus;
pub use markraft_core::commands::ColumnAlignment;
pub use markraft_core::kind::{CalloutAttrs, DocTypes, DocumentKind, Formatting, PlainKind};
use markraft_core::kind::{chains, conceal};
pub use style::EditorStyle;
pub use syntax::{canonical_language, code_languages};
pub use typeahead::{Typeahead, TypeaheadItem, TypeaheadProvider};

use extension::AnchoredOverlay;
use gpui::{prelude::*, *};
use markraft_core::commands::{Command, Direction};
use markraft_core::projection::{Projection, projection_of};
use markraft_core::protocol::event;
use markraft_core::{
    Attrs, EditorState, EditorStateConfig, MarkSet, MarkTypeId, Node, NodeTypeId, Schema,
    Selection, Transaction, TransactionSpec, history::HistoryConfig,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use surface::{EditorSurface, FrameLayout, LayoutLine, ShapeInput};

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
        CopyCodeBlock,
        CharacterPalette,
        LineBreak,
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
        DeleteToLineStart,
        DeleteToLineEnd,
        ParagraphStart,
        ParagraphEnd,
        DeleteToParagraphEnd,
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

/// What a paste puts in a table cell. A GFM row is one line, so the cell takes
/// the inline content of each block pasted, one after another with `between`
/// — a space where it is `None` — rather than the blocks themselves, which
/// would open cells and rows of their own.
fn cell_content(
    schema: &Schema,
    slice: &markraft_core::Slice,
    between: Option<&Node>,
) -> markraft_core::Slice {
    fn runs(schema: &Schema, node: &Node, out: &mut Vec<Vec<Node>>, inline: &mut bool) {
        if node.is_inline(schema) {
            if !*inline {
                out.push(Vec::new());
                *inline = true;
            }
            out.last_mut().expect("pushed").push(node.clone());
        } else if node.is_textblock(schema) {
            out.push(node.children().cloned().collect());
            *inline = false;
        } else {
            *inline = false;
            for child in node.children() {
                runs(schema, child, out, inline);
            }
            *inline = false;
        }
    }
    let mut found = Vec::new();
    let mut inline = false;
    for node in slice.content().iter() {
        runs(schema, node, &mut found, &mut inline);
    }
    let mut nodes = Vec::new();
    for run in found.into_iter().filter(|run| !run.is_empty()) {
        if !nodes.is_empty() {
            nodes.push(between.cloned().unwrap_or_else(|| schema.text(" ")));
        }
        nodes.extend(run);
    }
    markraft_core::Slice::from_fragment(markraft_core::Fragment::from_nodes(nodes))
}

pub fn bind_keys(cx: &mut App) {
    macro_rules! bind { ($($key:literal => $action:ident),* $(,)?) => {
        cx.bind_keys([$(KeyBinding::new($key, $action, Some("Markraft"))),*]);
    }; }
    bind! {
        "backspace" => Backspace, "delete" => Delete, "enter" => Enter,
        "shift-enter" => LineBreak,
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
        "cmd-shift-s" => Strikethrough, "cmd-0" => Paragraph,
        "cmd-1" => Heading, "cmd-2" => Heading2,
        "cmd-3" => Heading3, "cmd-4" => Heading4,
        "cmd-5" => Heading5, "cmd-6" => Heading6,
        "cmd-shift-b" => Quote, "cmd-alt-c" => CodeBlock,
        "cmd-enter" => ToggleTask,
        "cmd-alt-l" => ChooseCodeLanguage, "cmd-alt-shift-c" => CopyCodeBlock,
        "ctrl-cmd-space" => CharacterPalette,
        "alt-left" => WordLeft, "alt-right" => WordRight,
        "alt-shift-left" => SelectWordLeft, "alt-shift-right" => SelectWordRight,
        "alt-backspace" => DeleteWordBackward, "alt-delete" => DeleteWordForward,
        "cmd-up" => DocumentStart, "cmd-down" => DocumentEnd,
        "cmd-shift-up" => SelectDocumentStart, "cmd-shift-down" => SelectDocumentEnd,
        "escape" => CancelComposition,
        "cmd-backspace" => DeleteToLineStart, "cmd-delete" => DeleteToLineEnd,
    }
    cx.bind_keys(emacs_key_bindings());
    cx.bind_keys(list_key_bindings());
    // Last, so that at the same context depth an extension's bindings win while its
    // identifier is in the editor's key context.
    typeahead::bind_keys(cx);
}

/// The Emacs keys every macOS text view takes. Not while an extension reads
/// keys as commands — a vim Normal mode — where they would edit under its
/// caret; see [`Extension::key_context`].
fn emacs_key_bindings() -> Vec<KeyBinding> {
    const CONTEXT: Option<&str> = Some("Markraft && !modal");
    vec![
        KeyBinding::new("ctrl-a", ParagraphStart, CONTEXT),
        KeyBinding::new("ctrl-e", ParagraphEnd, CONTEXT),
        KeyBinding::new("ctrl-k", DeleteToParagraphEnd, CONTEXT),
        KeyBinding::new("ctrl-d", Delete, CONTEXT),
        KeyBinding::new("ctrl-h", Backspace, CONTEXT),
        KeyBinding::new("ctrl-f", Right, CONTEXT),
        KeyBinding::new("ctrl-b", Left, CONTEXT),
        KeyBinding::new("ctrl-n", Down, CONTEXT),
        KeyBinding::new("ctrl-p", Up, CONTEXT),
    ]
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
/// [`EditorView::code_language_bounds`] is.
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

/// An edit of the table the caret is in — or, with [`TableOp::Insert`], a new
/// table — as [`EditorView::table`] runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableOp {
    /// A new `rows` × `columns` table after the caret's block, or in place of
    /// an empty one.
    Insert {
        /// How many rows, the header among them.
        rows: usize,
        /// How many columns.
        columns: usize,
    },
    /// A row above the caret's.
    AddRowBefore,
    /// A row below the caret's.
    AddRowAfter,
    /// A column before the caret's.
    AddColumnBefore,
    /// A column after the caret's.
    AddColumnAfter,
    /// The caret's row; the last row takes the table with it.
    DeleteRow,
    /// The caret's column; the last column takes the table with it.
    DeleteColumn,
    /// The whole table.
    DeleteTable,
    /// The caret's column aligned this way.
    SetAlignment(ColumnAlignment),
}

/// How to build an [`EditorView`]: the host's document kind and its extensions.
///
/// The view is schema-agnostic. Everything that names a concrete document kind
/// comes in here: the compiled [`Schema`], the [`DocTypes`] that say which of
/// its types play the roles the view draws and binds keys to, and the
/// [`DocumentKind`] that supplies the behaviour behind those roles — the
/// clipboard codecs, the spelling, and how the kind toggles, links, splits and
/// breaks where it does so differently from the view.
pub struct Setup {
    /// The document kind's compiled schema.
    pub schema: Schema,
    /// Which of the schema's types play the roles the view knows about. The
    /// default leaves every role unset, which is what a plain-text editor wants.
    pub types: DocTypes,
    /// The host's state extensions — input rules, corrections and fields. The
    /// view adds history, composition and the projection itself.
    pub extensions: markraft_core::Extension,
    /// The document kind's own behaviour. [`PlainKind`] — the default — leaves
    /// everything to the view.
    pub kind: Arc<dyn DocumentKind>,
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
            kind: Arc::new(PlainKind),
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
    pub fn kind(mut self, kind: Arc<dyn DocumentKind>) -> Setup {
        self.kind = kind;
        self
    }
    pub fn doc(mut self, doc: Node) -> Setup {
        self.doc = Some(doc);
        self
    }
}

/// Why an edit did not reach the document. The editor only keeps the cases apart;
/// the host words each one, because only it knows what the document is stored in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRejection {
    /// Nothing can be edited here for as long as this holds, so repeating the
    /// message per keystroke says nothing new.
    ReadOnly(String),
    /// This change cannot be kept where it lands, though others still can.
    Protected(String),
    /// The transaction itself could not be built.
    Invalid(String),
    /// The document kind has no way to write this edit here — a style its
    /// syntax cannot spell at that spot. Nothing changed.
    Refused(String),
}

impl EditRejection {
    /// The sentence the host attached, whichever case it belongs to.
    pub fn message(&self) -> &str {
        let (Self::ReadOnly(message)
        | Self::Protected(message)
        | Self::Invalid(message)
        | Self::Refused(message)) = self;
        message
    }
}

impl std::fmt::Display for EditRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

type DocumentGuard = Box<dyn Fn(&Node) -> Result<(), EditRejection>>;
type TransactionGuard = Box<dyn Fn(&[Transaction]) -> Result<(), EditRejection>>;

/// Whether a wiki link target names something the host can open. Only the host can
/// say, and it is asked once per link per layout, so it answers from what it already
/// knows rather than by looking at the disk.
pub type WikiResolver = Box<dyn Fn(&str) -> bool>;

/// Fetches the bytes behind a remote image URL. It is called on a background
/// thread, once per image source while that source stays in the document, and may
/// block. An error is shown as an image that could not be loaded; what went wrong
/// is the host's to log.
pub type RemoteImageFetcher = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Build all transactions before publishing any state. Unlike a transaction
/// filter, this boundary also covers no-filter edits, undo and appender output.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
fn apply_guarded(
    state: &mut EditorState,
    specs: impl IntoIterator<Item = TransactionSpec>,
    guard: Option<&DocumentGuard>,
) -> Result<Vec<Transaction>, EditRejection> {
    apply_guarded_transactions(state, specs, guard, None)
}

fn apply_guarded_transactions(
    state: &mut EditorState,
    specs: impl IntoIterator<Item = TransactionSpec>,
    guard: Option<&DocumentGuard>,
    transaction_guard: Option<&TransactionGuard>,
) -> Result<Vec<Transaction>, EditRejection> {
    let started = std::time::Instant::now();
    let transactions = state
        .update_with_appended(specs)
        .map_err(|error| EditRejection::Invalid(error.to_string()))?;
    let last = transactions
        .last()
        .ok_or_else(|| EditRejection::Invalid("No transaction was produced.".to_owned()))?;
    // Resolve every state field before a source guard commits its baseline.
    // After successful guards, publishing the prepared state cannot fail.
    let next = last.state().clone();
    let built = started.elapsed();
    let checking = std::time::Instant::now();
    let result = if last.new_doc() != state.doc() {
        guard
            .map_or(Ok(()), |guard| guard(last.new_doc()))
            .and_then(|()| transaction_guard.map_or(Ok(()), |guard| guard(&transactions)))
    } else {
        Ok(())
    };
    log::debug!(
        "editor transaction: build_us={} guard_us={} count={} accepted={}",
        built.as_micros(),
        checking.elapsed().as_micros(),
        transactions.len(),
        result.is_ok(),
    );
    result?;
    *state = next;
    Ok(transactions)
}

/// Where a vertical move goes; see [`EditorView::vertical_target`].
pub(crate) enum VerticalMove {
    /// A block-edge rule takes over: the edit that leaves a code block, a
    /// final table or a selected divider for a new block, or that carries the
    /// caret to the document's edge.
    Run(TransactionSpec),
    /// The neighbouring visual row, at `x` — the column the next vertical
    /// move keeps.
    To {
        /// The document position under the column on the target row.
        position: usize,
        /// The column the caret came from.
        x: Pixels,
        /// Whether the position sits at the end of a wrapped row rather than
        /// the start of the next.
        upstream: bool,
    },
    /// Nowhere to go: no layout yet, or an edge with no edit to make there.
    Stay,
}

pub struct EditorView {
    document_guard: Option<DocumentGuard>,
    transaction_guard: Option<TransactionGuard>,
    edit_error: Option<EditRejection>,
    file_paste: bool,
    state: EditorState,
    projection: Arc<Projection>,
    /// The find query and its hits. See [`find`].
    find: markraft_core::StateField<find::Find>,
    pub(crate) types: DocTypes,
    /// The host's document kind; see [`Setup::kind`].
    kind: Arc<dyn DocumentKind>,
    /// [`DocumentKind::codecs`], asked once: absent for an editor that only
    /// holds text.
    pub(crate) codecs: Option<Arc<dyn markraft_core::kind::Codecs>>,
    /// [`DocumentKind::spelling`], asked once.
    pub(crate) spelling: Option<Arc<dyn markraft_core::kind::SourceSpelling>>,
    /// The host's extensions, kept so the state can be rebuilt on a replacement.
    host_extensions: markraft_core::Extension,
    pub(crate) extensions: Vec<extension::Registration>,
    /// The selection the extensions were last told about.
    pub(crate) extension_selection: Selection,
    /// The style, the images and the two host callbacks shaping reads, and the
    /// rows it last produced from them.
    shaping: shaping::Shaping,
    pub(crate) placeholder: SharedString,
    /// What Tab inserts in a verbatim block; see [`EditorView::set_indent_text`].
    indent_text: SharedString,
    /// What the editor calls itself to assistive technology. A host that lends one
    /// editor to several surfaces renames it as it hands it over.
    pub(crate) aria_label: SharedString,
    pub(crate) single_line: bool,
    pub(crate) focus: FocusHandle,
    /// What the last paint produced; see [`FrameLayout`].
    pub(crate) frame: FrameLayout,
    pub(crate) scroll: ScrollHandle,
    /// The text system the last frame laid out with, which a key that moves
    /// over lines no frame laid out lays them out with; see [`layout`].
    text_system: RefCell<Option<Arc<gpui::WindowTextSystem>>>,
    /// How far down the note the host needs its height exact; see
    /// [`EditorView::set_exact_height`].
    exact_height: Option<Pixels>,
    /// Whether the last measuring pass left lines whose height is only an
    /// estimate, so the frame asks for another to measure more of them.
    pub(crate) unmeasured: std::cell::Cell<bool>,
    /// The caret's own view state: break side, kept column, pending reveal.
    pub(crate) caret: caret::CaretView,
    selecting: bool,
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
        markraft_core::history::history(HistoryConfig {
            new_group_delay: TYPING_GROUP_DELAY,
            ..HistoryConfig::default()
        }),
        markraft_core::composition::composition(),
        markraft_core::projection::projection(),
    ])
}

fn build_state(
    schema: &Schema,
    host: &markraft_core::Extension,
    doc: Option<Node>,
    types: &DocTypes,
) -> (EditorState, markraft_core::StateField<find::Find>) {
    let (search, field) = find::extension(types.clone());
    let extensions = markraft_core::Extension::all([base_extensions(), search, host.clone()]);
    let config = EditorStateConfig::new(schema.clone()).extensions(extensions.clone());
    let config = match doc {
        Some(doc) => config.doc(doc),
        None => config,
    };
    let state = EditorState::create(config).unwrap_or_else(|_| {
        // A document the schema rejects is a host bug; an empty one keeps the
        // view usable rather than taking the process down.
        EditorState::create(EditorStateConfig::new(schema.clone()).extensions(extensions))
            .expect("the schema describes a valid empty document")
    });
    (state, field)
}

impl EditorView {
    pub fn new(setup: Setup, cx: &mut Context<Self>) -> Self {
        let Setup {
            schema,
            types,
            extensions,
            kind,
            doc,
        } = setup;
        let (state, find) = build_state(&schema, &extensions, doc, &types);
        let projection = projection_of(&state);
        Self {
            document_guard: None,
            transaction_guard: None,
            edit_error: None,
            file_paste: false,
            find,
            types,
            codecs: kind.codecs(),
            spelling: kind.spelling(),
            kind,
            extension_selection: state.selection().clone(),
            state,
            projection,
            host_extensions: extensions,
            extensions: Vec::new(),
            placeholder: SharedString::default(),
            indent_text: "\t".into(),
            aria_label: DEFAULT_ARIA_LABEL.into(),
            single_line: false,
            focus: cx.focus_handle(),
            frame: FrameLayout::default(),
            scroll: ScrollHandle::new(),
            text_system: RefCell::default(),
            exact_height: None,
            unmeasured: std::cell::Cell::new(false),
            caret: caret::CaretView::default(),
            selecting: false,
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

    /// Validate a complete candidate before changing state, including undo and IME.
    pub fn with_document_guard(
        mut self,
        guard: impl Fn(&Node) -> Result<(), EditRejection> + 'static,
    ) -> Self {
        self.document_guard = Some(Box::new(guard));
        self
    }

    /// Validate the completed transaction chain before publishing its state.
    /// The guard may advance a source baseline on success: all document guards
    /// and state construction finish first, and no fallible step follows it.
    pub fn with_transaction_guard(
        mut self,
        guard: impl Fn(&[Transaction]) -> Result<(), EditRejection> + 'static,
    ) -> Self {
        self.transaction_guard = Some(Box::new(guard));
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

    /// Fetch remote images with `fetcher`, or, with `None`, show every one as a
    /// remote image this editor does not load. A standalone image being fetched is
    /// drawn as a placeholder the size of a picture, and replaced when it arrives.
    pub fn set_remote_images(
        &mut self,
        fetcher: Option<RemoteImageFetcher>,
        cx: &mut Context<Self>,
    ) {
        self.shaping.set_remote_images(fetcher);
        cx.notify();
    }

    /// Start a background fetch for each remote image the last layout asked for.
    pub(crate) fn fetch_remote_images(&mut self, cx: &mut Context<Self>) {
        let Some(fetcher) = self.shaping.remote_images().cloned() else {
            return;
        };
        for source in self.shaping.images().take_requests() {
            let fetcher = fetcher.clone();
            cx.spawn(async move |this, cx| {
                let fetching = source.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { images::fetch(&fetcher, &fetching) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.shaping.finish_remote_image(&source, result) {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
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
    /// What Tab inserts at a caret in a verbatim block — a tab, the default,
    /// or the spaces a host's indentation preference asks for.
    pub fn set_indent_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.indent_text = text.into();
        cx.notify();
    }
    /// Take what prepaint produced, in one assignment.
    pub(crate) fn set_frame(&mut self, frame: FrameLayout) {
        self.frame = frame;
    }

    /// Give back the layout of an editor the host is not showing: the rows
    /// kept for the next frame, the last frame's rows and the text built for
    /// assistive apps. Nothing a reader sees changes; the next frame that
    /// draws this editor lays the document out again.
    pub fn release_layout(&mut self) {
        self.shaping.release();
        self.frame.release_rows();
        self.accessible_text.borrow_mut().release();
    }

    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.shaping.set_style(style);
        self.caret.ask_reveal();
        cx.notify();
    }

    /// The editor's state: document, selection and every extension field.
    pub fn state(&self) -> &EditorState {
        &self.state
    }

    /// Where finding stands: the query, which hit is current, and how many
    /// there are. The current hit is the selection; the others are decorations.
    pub fn find_status(&self) -> FindStatus {
        match self.state.field(&self.find) {
            Some(found) => FindStatus {
                query: found.query().to_owned(),
                current: found.current(),
                total: found.total(),
            },
            None => FindStatus {
                query: String::new(),
                current: None,
                total: 0,
            },
        }
    }

    /// The selection as the reader sees it, when it stays on one line. What
    /// ⌘F puts in the field.
    pub fn find_selection_query(&self) -> Option<String> {
        let doc = self.state.doc();
        let selection = self.state.selection();
        shown::ShownText::build(&self.projection, &self.types, &conceal::Reveal::nothing())
            .text_inside(selection.from(doc)..selection.to(doc))
    }

    /// Find `query` and select the first hit at or after the caret. An empty
    /// query clears the hits and leaves the caret where it is.
    pub fn set_find_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.run_find(find::Command::Query(query), cx);
    }

    /// Select the next hit, wrapping to the first.
    pub fn find_next(&mut self, cx: &mut Context<Self>) {
        self.run_find(find::Command::Next, cx);
    }

    /// Select the previous hit, wrapping to the last.
    pub fn find_previous(&mut self, cx: &mut Context<Self>) {
        self.run_find(find::Command::Previous, cx);
    }

    fn run_find(&mut self, command: find::Command, cx: &mut Context<Self>) {
        let previous = self.state.field(&self.find).cloned().unwrap_or_default();
        let next = find::apply(&previous, &self.state, &self.types, &command);
        let mut spec = TransactionSpec::new()
            .effect(find::effect(command.clone()))
            .add_to_history(false);
        if let Some(range) = find::selection_for(&next, &command) {
            spec = spec.selection(Selection::text(range.start, range.end));
        }
        let _ = self.edit(cx, false, vec![spec]);
    }
    pub fn doc(&self) -> &Node {
        self.state.doc()
    }
    /// The persistent document, excluding the input method’s uncommitted candidate.
    pub fn committed_document(&self) -> &Node {
        markraft_core::composition::committed_document(&self.state)
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
    pub fn style(&self) -> &crate::style::EditorStyle {
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
            spelling: self.spelling.as_deref(),
            selection: selection.from(doc)..selection.to(doc),
            composition: markraft_core::composition::composition_range(&self.state)
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
        let row = self.frame.rows().iter().find(|row| row.contains(pos))?;
        Some((row, row.pos_to_offset(pos)))
    }

    /// The caret, or the moving end of a range.
    pub fn head(&self) -> usize {
        self.state.selection().head(self.state.doc())
    }

    /// Where arrows and line edges measure from: the head, except for a
    /// selected node. Its head lies just past it, which no row holds when the
    /// node is a leaf block — a divider's row is the single position before it
    /// — so the node's start stands in and the motion leaves from its row.
    fn motion_head(&self) -> usize {
        let doc = self.state.doc();
        match self.state.selection() {
            Selection::Node { .. } => self.state.selection().from(doc),
            selection => selection.head(doc),
        }
    }
    pub fn is_composing(&self) -> bool {
        markraft_core::composition::is_composing(&self.state)
    }

    /// Height at the most recently laid-out width, including editor padding.
    /// `None` before the first paint.
    ///
    /// Lines no frame has shaped yet count at an estimate; see
    /// [`EditorView::set_exact_height`] for a host that sizes itself by this.
    pub fn content_height(&self) -> Option<Pixels> {
        self.frame.placed()?;
        Some(
            (if self.single_line { px(0.) } else { px(40.) })
                + self.style().padding * 2.
                + self.style().top_overlay
                + self.style().bottom_overlay
                + self.shaping.lines().total(),
        )
    }

    /// Measure the note exactly from its top down to `limit`, for a host that
    /// sizes its window by [`EditorView::content_height`] up to that height.
    /// Past what is on screen a line's height is otherwise an estimate until
    /// it is shaped, and a window sized by one would change size as the note
    /// is read.
    pub fn set_exact_height(&mut self, limit: Option<Pixels>, cx: &mut Context<Self>) {
        if self.exact_height != limit {
            self.exact_height = limit;
            cx.notify();
        }
    }

    /// Marks shared by all selected text, or the marks new text would get.
    pub fn active_marks(&self) -> MarkSet {
        format_state::active_marks(&self.state, self.types.syntax)
    }
    /// The type and attributes every selected block shares; mixed formats give `None`.
    pub fn active_block_type(&self) -> Option<(NodeTypeId, Attrs)> {
        format_state::active_block_type(&self.state, &self.projection)
    }

    /// Replace the document, discarding the undo history with it.
    pub fn replace_doc(&mut self, doc: Node, cx: &mut Context<Self>) {
        let doc = if self.single_line {
            single_line::document(
                &doc,
                &self.state.schema().clone(),
                self.types.syntax,
                self.codecs.as_deref(),
            )
        } else {
            doc
        };
        self.frame.clear();
        let (state, find) = build_state(
            &self.state.schema().clone(),
            &self.host_extensions,
            Some(doc),
            &self.types,
        );
        self.find = find;
        self.state = state;
        self.projection = projection_of(&self.state);
        self.extension_selection = self.state.selection().clone();
        self.undo_group_depth = 0;
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
        let transactions = match apply_guarded_transactions(
            &mut self.state,
            specs,
            self.document_guard.as_ref(),
            self.transaction_guard.as_ref(),
        ) {
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
        self.caret.moved_by_edit();
        if changed || composing != self.is_composing() {
            self.publish(cx);
        }
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

    /// The view's side of [`EditorCx::begin_undo_group`].
    pub fn begin_undo_group(&mut self) {
        let _ = self.apply([TransactionSpec::new()
            .effect(markraft_core::history::begin_undo_group().of(()))
            .add_to_history(false)]);
        self.undo_group_depth += 1;
    }

    pub fn end_undo_group(&mut self) {
        while self.undo_group_depth > 0 {
            self.undo_group_depth -= 1;
            let _ = self.apply([TransactionSpec::new()
                .effect(markraft_core::history::end_undo_group().of(()))
                .add_to_history(false)]);
        }
    }

    /// Restore the content and selection from before the input method started.
    pub fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Some(spec) = markraft_core::composition::cancel_composition(&self.state) {
            self.edit(cx, true, vec![spec]);
        }
    }

    /// Select everything, as ⌘A does. A host that fills a field with a value meant to
    /// be typed over calls this, so the first keystroke replaces it.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let command = chains::select_all(&self.types);
        self.run_command(&command, cx);
    }

    pub fn toggle_mark(&mut self, ty: MarkTypeId, attrs: Attrs, cx: &mut Context<Self>) {
        let command = self.mark_command(ty, attrs);
        self.run_formatting(&command, cx);
    }

    /// The host's way of toggling `ty`, or the model's where it has none.
    fn mark_command(&self, ty: MarkTypeId, attrs: Attrs) -> Formatting {
        self.kind.toggle_mark(ty, attrs.clone()).unwrap_or_else(|| {
            let command = markraft_core::commands::toggle_mark(ty, attrs);
            Arc::new(move |state| Ok(command(state)))
        })
    }

    /// The host's way of linking the selection to `url`, or unlinking it, or
    /// the view's own where it has none.
    fn link_command(&self, ty: MarkTypeId, url: Option<&str>) -> Formatting {
        self.kind.set_link(ty, url).unwrap_or_else(|| {
            let url = url.map(str::to_owned);
            Arc::new(move |state| Ok(links::set_link(state, ty, url.as_deref())))
        })
    }

    /// Run a document kind's formatting edit. A refusal changes nothing and is
    /// kept for the host, as [`EditorView::take_edit_error`] reports it; `false`
    /// only when the edit does not apply here at all.
    pub fn run_formatting(&mut self, command: &Formatting, cx: &mut Context<Self>) -> bool {
        match command(&self.state) {
            Ok(Some(spec)) => self.edit(cx, false, vec![spec]).is_some(),
            Ok(None) => false,
            Err(message) => {
                self.edit_error = Some(EditRejection::Refused(message));
                cx.notify();
                true
            }
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
        let command = self.link_command(ty, url);
        self.run_formatting(&command, cx);
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

    /// Where the definition of the footnote reference drawn under `point`
    /// starts.
    fn footnote_definition_under(&self, point: Point<Pixels>) -> Option<usize> {
        let ty = self.types.footnote_reference?;
        let pos = self.hit(point);
        let doc = self.state.doc();
        let (range, _) = links::link_at(doc, ty, pos)?;
        let (row, offset) = self.row_at(range.start)?;
        let drawn = row
            .rectangles(offset..row.pos_to_offset(range.end), false)
            .iter()
            .any(|bounds| bounds.contains(&point));
        let label = footnotes::reference_at(doc, &self.types, pos).filter(|_| drawn)?;
        footnotes::definition(self.state.schema(), doc, &self.types, &label)
    }

    /// Put the caret at `pos` and bring it into view.
    fn go_to(&mut self, pos: usize, cx: &mut Context<Self>) {
        self.edit(
            cx,
            false,
            vec![
                TransactionSpec::new()
                    .selection(Selection::cursor(pos))
                    .user_event(event::SELECT_POINTER)
                    .scroll_into_view(),
            ],
        );
        self.reset_caret_blink(cx);
        cx.notify();
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

    /// Window bounds of the language tag of the code block starting at `pos`,
    /// for anchoring the host's language picker to it.
    pub fn code_language_bounds(&self, pos: usize) -> Option<Bounds<Pixels>> {
        self.frame
            .rows()
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
        let line = self.frame.rows().iter().find(|line| line.contains(head))?;
        let cell = line.table?;
        let bounds = self
            .frame
            .rows()
            .iter()
            .filter(|line| line.table.is_some_and(|other| other.table == cell.table))
            .filter_map(LayoutLine::cell_bounds)
            .reduce(|all, bounds| all.union(&bounds))?;
        // A grid wider than the note is scrolled inside it, so the toolbar
        // anchors to the part of it the reader can actually see.
        let bounds = bounds.intersect(&self.frame.content_bounds());
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
    /// does not apply — which, for all but [`TableOp::Insert`], means
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

    /// One edit of the table the caret is in, or a new table where there is
    /// none: what every table control resolves to. `false` where it does not
    /// apply — outside a table for every shape but [`TableOp::Insert`], and
    /// in a single-line editor always.
    pub fn table(&mut self, op: TableOp, cx: &mut Context<Self>) -> bool {
        use markraft_core::commands as c;
        self.table_command(
            move |types| match op {
                TableOp::Insert { rows, columns } => c::insert_table(types, rows, columns),
                TableOp::AddRowBefore => c::add_row_before(types),
                TableOp::AddRowAfter => c::add_row_after(types),
                TableOp::AddColumnBefore => c::add_column_before(types),
                TableOp::AddColumnAfter => c::add_column_after(types),
                TableOp::DeleteRow => c::delete_row(types),
                TableOp::DeleteColumn => c::delete_column(types),
                TableOp::DeleteTable => c::delete_table(types),
                TableOp::SetAlignment(alignment) => c::set_column_alignment(types, alignment),
            },
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
        let row = surface::selection_anchor_row(self.frame.rows(), start, end)?;
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
            let caret = row.caret(range.start, self.caret.upstream());
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
                .user_event(event::SELECT_POINTER)
                .scroll_into_view(),
        ];
        if self.is_composing() {
            specs.push(markraft_core::composition::finish_composition().sequential());
        }
        self.edit(cx, false, specs);
    }

    /// The document position under `point`.
    pub(crate) fn hit(&self, point: Point<Pixels>) -> usize {
        let Some(last) = self.frame.rows().last() else {
            return 0;
        };
        // Below the last row laid out is the document's end only where that
        // row is the document's last line.
        let at_end = last.index + 1 >= self.projection.line_count();
        if at_end && point.y >= last.origin.y + last.height {
            return last.offset_to_pos(last.char_len);
        }
        let (row, local) = self.row_under(point);
        if row.in_callout_header(point.y) {
            return row.offset_to_pos(0);
        }
        row.hit_position(row.char_at(local), &self.projection)
    }

    /// The laid-out line `point` falls on, and the point relative to its origin.
    fn row_under(&self, point: Point<Pixels>) -> (&LayoutLine, Point<Pixels>) {
        let row = self
            .frame
            .rows()
            .iter()
            .find(|row| point.y < row.origin.y + row.height)
            .unwrap_or(&self.frame.rows()[0]);
        // A grid's cells share one band of y, and only the last of a row
        // carries the row's height, so the search above lands on that one
        // whatever column the point was in. The grid picks the column.
        let row = match row.table.map(|cell| cell.table) {
            Some(table) => surface::cell_under(self.frame.rows(), table, point).unwrap_or(row),
            None => row,
        };
        let local = gpui::point(point.x - row.origin.x, point.y - row.origin.y);
        (row, local)
    }

    /// A click inside source shown as text — an inline HTML tag — puts the caret
    /// where it was clicked, as it would in any other text. The click lands on
    /// the atom's edge, which spells the source out, and the caret then moves
    /// into the spelling. Where the spelling is not what was shown, the caret
    /// stays at the edge.
    fn caret_into_source(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        self.lay_out_at(point);
        let Some(last) = self.frame.rows().last() else {
            return;
        };
        if point.y >= last.origin.y + last.height {
            return;
        }
        let (row, local) = self.row_under(point);
        let Some((offset, source, before)) = row.source_text_at(local) else {
            return;
        };
        let pos = row.offset_to_pos(offset);
        let doc = self.state.doc();
        let Ok(resolved) = doc.resolve(pos) else {
            return;
        };
        let start = resolved.parent_offset();
        let spelled = resolved.parent().text_between(
            self.state.schema(),
            start,
            (start + source.chars().count()).min(resolved.parent().content_size()),
            None,
            None,
        );
        if spelled == source {
            self.select(pos + before, false, cx);
        }
    }

    fn select_point(&mut self, point: Point<Pixels>, extend: bool, cx: &mut Context<Self>) {
        self.lay_out_at(point);
        let (position, upstream) = self.hit_upstream(point);
        self.caret.landed(upstream);
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
            self.frame
                .rows()
                .iter()
                .flat_map(|row| {
                    (0..row.navigable_rows())
                        .map(move |i| row.origin.y + row.line_height * (i as f32 + 0.5))
                })
                .collect(),
        )
    }

    /// The caret `delta` visual rows away and the column to keep there. `None` before the
    /// first paint, when there is no layout to walk.
    pub(crate) fn visual_row_target(&self, delta: isize) -> Option<(usize, Pixels, bool)> {
        let head = self.motion_head();
        let (row, offset) = self.row_at(head)?;
        let caret = row.caret(offset, self.caret.upstream());
        let x = self.caret.preferred_x().unwrap_or(caret.x);
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

    /// Whether `position` shows on the same visual row as the caret.
    fn same_visual_row(&self, position: usize, upstream: bool) -> bool {
        let caret_y = |pos: usize, upstream: bool| {
            self.row_at(pos)
                .map(|(row, offset)| row.caret(offset, upstream).y)
        };
        caret_y(position, upstream) == caret_y(self.motion_head(), self.caret.upstream())
    }

    /// The start or end of the caret's visual row. A wrapped block has several.
    pub(crate) fn line_edge_target(&self, end: bool) -> Option<(usize, bool)> {
        let head = self.motion_head();
        let (row, offset) = self.row_at(head)?;
        let caret = row.caret(offset, self.caret.upstream());
        Some(self.hit_upstream(point(
            if end {
                row.origin.x + row.width
            } else {
                row.origin.x
            },
            caret.y + row.line_height * 0.5,
        )))
    }

    /// Where a vertical move of `delta` rows from the caret goes: the one
    /// decision ↑ and ↓ and a modal extension's j and k share, so the block
    /// edges behave the same under either.
    ///
    /// The rules, in order, for a move down with nothing selected: at the end
    /// of a verbatim block that ends the document it leaves the block for a
    /// new one ([`exit_code`](markraft_core::commands::exit_code)); on a
    /// table's last visual row with nothing after the table it leaves the
    /// table ([`exit_table_below`](markraft_core::commands::exit_table_below));
    /// on a selected divider with nothing after it, likewise
    /// ([`chains::exit_leaf_below`]). With no row above
    /// the first or below the last, the caret goes to the start or the end of
    /// the document, as in every macOS text view, and a shifted arrow takes
    /// the selection there ([`chains::move_document_edge`]). Otherwise the
    /// move lands on the neighbouring row at the column the caret keeps.
    /// Before the first paint nothing moves.
    pub(crate) fn vertical_target(&self, delta: isize, extend: bool) -> VerticalMove {
        // Before the first paint there are no rows to move between, and no
        // edge to have reached.
        if self.frame.rows().is_empty() {
            return VerticalMove::Stay;
        }
        let head = self.head();
        let cursor = self.state.selection().is_cursor();
        let last_line = self.projection.line_count().saturating_sub(1);
        if delta > 0
            && !extend
            && cursor
            && self.projection.line_at(head) == Some(last_line)
            && self
                .projection
                .line(last_line)
                .is_some_and(|line| line.to() == head && self.types.is_verbatim_block(line))
            && let Some(spec) = markraft_core::commands::exit_code()(&self.state)
        {
            return VerticalMove::Run(spec);
        }
        let target = self.visual_row_target(delta);
        let stuck =
            target.is_none_or(|(position, _, upstream)| self.same_visual_row(position, upstream));
        if delta > 0
            && !extend
            && cursor
            && stuck
            && let Some(types) = self.types.table_types()
            && let Some(spec) = markraft_core::commands::exit_table_below(types)(&self.state)
        {
            return VerticalMove::Run(spec);
        }
        if delta > 0
            && !extend
            && stuck
            && matches!(self.state.selection(), Selection::Node { .. })
            && let Some(spec) = chains::exit_leaf_below()(&self.state)
        {
            return VerticalMove::Run(spec);
        }
        if stuck {
            return match chains::move_document_edge(delta > 0, extend)(&self.state) {
                Some(spec) => VerticalMove::Run(spec),
                None => VerticalMove::Stay,
            };
        }
        match target {
            Some((position, x, upstream)) => VerticalMove::To {
                position,
                x,
                upstream,
            },
            None => VerticalMove::Stay,
        }
    }

    fn vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        self.lay_out_near(self.motion_head(), delta.unsigned_abs() + 1);
        match self.vertical_target(delta, extend) {
            VerticalMove::Run(spec) => {
                self.edit(cx, false, vec![spec]);
            }
            VerticalMove::To {
                position,
                x,
                upstream,
            } => {
                self.caret.landed(upstream);
                self.select(position, extend, cx);
                self.caret.moved_vertically(x);
            }
            VerticalMove::Stay => {}
        }
    }

    /// ⌘⌫ and ⌘⌦: delete to the start or the end of the caret's visual row, as
    /// every macOS text view does. On a textblock's first row — or its last,
    /// going forward — the deletion reaches the textblock's own edge, so a
    /// style's hidden opening or closing markup goes with the text it styled.
    /// At the edge already, or over a selection, the key does what Backspace or
    /// Delete does.
    fn delete_to_line_edge(&mut self, end: bool, cx: &mut Context<Self>) {
        self.lay_out_near(self.head(), 0);
        let fallback = || {
            if end {
                chains::delete_forward(&self.types)
            } else {
                chains::backspace(&self.types)
            }
        };
        let head = self.head();
        let range = self
            .state
            .selection()
            .is_cursor()
            .then(|| self.line_edge_range(head, end))
            .flatten();
        let command = match range {
            Some((from, to)) if from < to => chains::delete_within_textblock(from, to),
            _ => fallback(),
        };
        if !self.run_command(&command, cx) {
            cx.propagate();
        }
    }

    /// From `head` to the edge of its visual row that `end` names, ordered.
    fn line_edge_range(&self, head: usize, end: bool) -> Option<(usize, usize)> {
        let (row, offset) = self.row_at(head)?;
        let caret = row.caret(offset, self.caret.upstream());
        let first = caret.y < row.origin.y + row.line_height;
        let last = caret.y >= row.origin.y + row.line_height * (row.visual_rows() as f32 - 1.);
        let edge = match end {
            false if first => row.from,
            true if last => row.to(),
            _ => self.line_edge_target(end)?.0,
        };
        Some((head.min(edge), head.max(edge)))
    }

    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        self.lay_out_near(self.motion_head(), 0);
        if let Some((position, upstream)) = self.line_edge_target(end) {
            self.caret.forget_column();
            self.caret.landed(upstream);
            self.select(position, extend, cx);
        }
    }

    /// The selected content, as a slice.
    pub fn selection_slice(&self) -> markraft_core::Slice {
        clipboard::whole_items(&self.state, &self.types).unwrap_or_else(|| {
            self.state
                .selection()
                .content_with_schema(self.state.doc(), self.state.schema())
        })
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
                let text = conceal::slice_text(&schema, self.types.syntax, &slice);
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
        let verbatim = self.types.in_verbatim_block_at(&self.state);
        let in_cell = self
            .types
            .table_types()
            .and_then(|types| markraft_core::commands::cell_at(types, &self.state))
            .is_some();
        let cell_break = self.types.raw_inline.and_then(|raw| {
            schema
                .create(
                    raw,
                    markraft_core::attrs! { "source" => "<br/>" },
                    MarkSet::empty(),
                    markraft_core::Fragment::empty(),
                )
                .ok()
        });
        let spec = if let Some(codecs) = self
            .codecs
            .clone()
            .filter(|_| in_cell && !self.single_line && clipboard_text.is_some())
            .filter(|_| !is_web_url(text.trim()) || self.types.link.is_none())
            && let Some(cell_break) = cell_break
        {
            // A GFM row is one line: each line pasted goes into the cell as its
            // inline content, with a `<br/>` between them — one for each line
            // ending, a blank line's included.
            let text = text.strip_suffix('\n').unwrap_or(text);
            let lines = text.split('\n').map(|line| {
                let line = line.trim_end_matches('\r');
                let slice = match mode {
                    clipboard::PasteMode::Plain => Some(codecs.from_text(line)),
                    _ => codecs.from_markup(line),
                };
                slice.map(|slice| cell_content(&schema, &slice, None))
            });
            let mut nodes = Vec::new();
            for (index, line) in lines.enumerate() {
                if index > 0 {
                    nodes.push(cell_break.clone());
                }
                if let Some(line) = line {
                    nodes.extend(line.content().iter().cloned());
                }
            }
            let slice =
                markraft_core::Slice::from_fragment(markraft_core::Fragment::from_nodes(nodes));
            markraft_core::commands::replace_selection(slice)(&self.state)
        } else if let Some(codecs) = self
            .codecs
            .clone()
            .filter(|_| matches!(mode, clipboard::PasteMode::Plain))
            .filter(|_| !self.single_line && !verbatim)
        {
            // The kind reads plain text as the characters it is.
            markraft_core::commands::replace_selection(codecs.from_text(text))(&self.state)
        } else if literal {
            let text = single_line::text(text, self.single_line);
            chains::insert_plain(&self.types, &text)(&self.state)
        } else if is_web_url(text.trim())
            && self.types.link.is_some()
            && (!self.state.selection().is_empty(self.state.doc()) || self.active_link().is_none())
        {
            let command = self.link_command(self.types.link.expect("checked"), Some(text.trim()));
            match command(&self.state) {
                Ok(spec) => spec,
                Err(message) => {
                    self.edit_error = Some(EditRejection::Refused(message));
                    cx.notify();
                    return;
                }
            }
        } else if let Some(slice) = self
            .codecs
            .clone()
            .and_then(|codecs| clipboard::read_fragment(&schema, codecs.as_ref(), &item, mode, cx))
        {
            // Only an item with no text reaches here in a cell: its blocks go in
            // one after another, a `<br/>` between them.
            let slice = if in_cell {
                cell_content(&schema, &slice, cell_break.as_ref())
            } else {
                slice
            };
            markraft_core::commands::replace_selection(slice)(&self.state)
        } else {
            None
        };
        if let Some(spec) = spec {
            self.edit(cx, false, vec![spec.user_event(event::INPUT_PASTE)]);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.caret.forget_column();
        if event.modifiers.platform
            && let Some(url) = self.link_under(event.position)
        {
            self.selecting = false;
            Self::open_link(&url, cx);
            return;
        }
        // ⌘-click on a footnote reference goes to its definition, as it follows
        // a link; the definition's label goes back to the first reference.
        if event.modifiers.platform
            && let Some(target) = self.footnote_definition_under(event.position)
        {
            self.selecting = false;
            self.go_to(target, cx);
            return;
        }
        let position = self.hit(event.position);
        if let Some((row, _)) = self.row_at(position)
            && row
                .footnote_marker()
                .is_some_and(|bounds| bounds.contains(&event.position))
            && let Some(label) =
                footnotes::definition_label_at(self.state.doc(), &self.types, row.from)
            && let Some(target) = footnotes::first_reference(self.state.doc(), &self.types, &label)
        {
            self.selecting = false;
            self.go_to(target, cx);
            return;
        }
        // A focused code block's language tag is chrome over the block's text:
        // a click on it opens the picker rather than placing the caret.
        if let Some(pos) = self.frame.rows().iter().find_map(|row| {
            row.code_language_bounds()
                .filter(|bounds| bounds.contains(&event.position))
                .and(row.code_pos)
        }) {
            self.selecting = false;
            self.run_control(
                accessibility::ControlAction::CodeLanguage(pos),
                true,
                window,
                cx,
            );
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
        if event.click_count == 1 && !event.modifiers.shift {
            self.caret_into_source(event.position, cx);
        }
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
                self.select_range(line.from(), line.to(), cx);
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
                    .user_event(event::SELECT_POINTER),
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
        run!(Backspace, chains::backspace);
        run!(Delete, chains::delete_forward);
        run!(ParagraphStart, |_: &DocTypes| chains::textblock_edge(false));
        run!(ParagraphEnd, |_: &DocTypes| chains::textblock_edge(true));
        run!(DeleteToParagraphEnd, chains::delete_to_textblock_end);
        root =
            root.on_action(cx.listener(|this, _: &DeleteToLineStart, _, cx| {
                this.delete_to_line_edge(false, cx)
            }))
            .on_action(
                cx.listener(|this, _: &DeleteToLineEnd, _, cx| this.delete_to_line_edge(true, cx)),
            );
        root = root.on_action(cx.listener(|this, _: &Enter, _, cx| {
            if this.single_line {
                cx.propagate();
                return;
            }
            let command = chains::enter_with(&this.types, this.kind.as_ref());
            if !this.run_command(&command, cx) {
                cx.propagate();
            }
        }));
        root = root.on_action(cx.listener(|this, _: &LineBreak, _, cx| {
            if this.single_line {
                cx.propagate();
                return;
            }
            let command = chains::line_break(&this.types, this.kind.as_ref());
            if !this.run_command(&command, cx) {
                cx.propagate();
            }
        }));
        root = root.on_action(cx.listener(|this, _: &Indent, _, cx| {
            if this.single_line {
                cx.propagate();
                return;
            }
            let command = chains::indent(&this.types, &this.indent_text);
            if !this.run_command(&command, cx) {
                cx.propagate();
            }
        }));
        root = root.on_action(cx.listener(|this, _: &Outdent, _, cx| {
            if this.single_line {
                cx.propagate();
                return;
            }
            let command = chains::outdent(&this.types, &this.indent_text);
            if !this.run_command(&command, cx) {
                cx.propagate();
            }
        }));
        run!(Left, |_: &DocTypes| chains::move_grapheme(
            Direction::Backward,
            false
        ));
        run!(Right, |_: &DocTypes| chains::move_grapheme(
            Direction::Forward,
            false
        ));
        run!(SelectLeft, |_: &DocTypes| chains::move_grapheme(
            Direction::Backward,
            true
        ));
        run!(SelectRight, |_: &DocTypes| chains::move_grapheme(
            Direction::Forward,
            true
        ));
        run!(WordLeft, |types: &DocTypes| chains::move_word(
            types,
            Direction::Backward,
            false
        ));
        run!(WordRight, |types: &DocTypes| chains::move_word(
            types,
            Direction::Forward,
            false
        ));
        run!(SelectWordLeft, |types: &DocTypes| chains::move_word(
            types,
            Direction::Backward,
            true
        ));
        run!(SelectWordRight, |types: &DocTypes| chains::move_word(
            types,
            Direction::Forward,
            true
        ));
        run!(DeleteWordBackward, |types: &DocTypes| chains::delete_word(
            types,
            Direction::Backward
        ));
        run!(DeleteWordForward, |types: &DocTypes| chains::delete_word(
            types,
            Direction::Forward
        ));
        run!(DocumentStart, |_: &DocTypes| chains::move_document_edge(
            false, false
        ));
        run!(DocumentEnd, |_: &DocTypes| chains::move_document_edge(
            true, false
        ));
        run!(SelectDocumentStart, |_: &DocTypes| {
            chains::move_document_edge(false, true)
        });
        run!(SelectDocumentEnd, |_: &DocTypes| {
            chains::move_document_edge(true, true)
        });
        run!(Undo, |_: &DocTypes| chains::history(true));
        run!(Redo, |_: &DocTypes| chains::history(false));
        run!(SelectAll, chains::select_all);
        macro_rules! mark {
            ($action:ty, $role:ident) => {
                root = root.on_action(cx.listener(|this, _: &$action, _, cx| {
                    let command = this
                        .types
                        .$role
                        .filter(|_| !this.single_line)
                        .map(|ty| this.mark_command(ty, Attrs::empty()));
                    match command {
                        Some(command) if this.run_formatting(&command, cx) => {}
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
        rich!(CodeBlock, |types: &DocTypes| match types.code_block {
            Some(ty) => chains::code_block(types, ty, Attrs::empty()),
            None => markraft_core::commands::command(|_| None),
        });
        rich!(Quote, chains::toggle_quote);
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
        rich!(ToggleTask, chains::toggle_task);
        root = root
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
        Some(ty) => chains::toggle_block(types, ty, attrs),
        None => markraft_core::commands::command(|_| None),
    }
}

/// Toggle a list whose type or item type the schema may not declare.
///
/// A new list takes the schema's default attributes: the view has no list
/// preference of its own, so a host that wants another marker binds the
/// action itself and calls [`chains::toggle_list`] with its attributes.
fn list(types: &DocTypes, ty: Option<NodeTypeId>, item: Option<NodeTypeId>) -> Command {
    match (ty, item) {
        (Some(ty), Some(item)) => chains::toggle_list(types, ty, Attrs::empty(), item),
        _ => markraft_core::commands::command(|_| None),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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
#[cfg_attr(coverage_nightly, coverage(off))]
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod document_guard_tests {
    use super::{DocumentGuard, EditRejection, apply_guarded, build_state, clipboard, ime};
    use crate::typeahead::tests::{at, state_of, types_of};
    use markraft_commonmark::{CommonMarkCodecs, commonmark_schema, from_markdown};
    use markraft_core::kind::DocTypes;
    use markraft_core::{
        Attrs, EditorState, Selection, TransactionAppenderFn, TransactionSpec, commands,
        composition::{
            cancel_composition, committed_document, composition_range, is_composing,
            update_composition,
        },
        history::{redo, redo_depth, undo, undo_depth},
        protocol::{appended, origin},
        transaction_appender,
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
        let (mut state, _) = build_state(
            &schema,
            &transaction_appender().of(appender),
            Some(doc),
            &DocTypes::none(),
        );
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
    fn transaction_guard_sees_the_whole_chain_and_commits_source_with_state() {
        use markraft_commonmark::{SourceDocument, SourceTrack};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let schema = commonmark_schema();
        let source = SourceDocument::parse(&schema, "base").unwrap();
        let track = Arc::new(SourceTrack::new(source.clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let appender: TransactionAppenderFn = Arc::new(|transaction| {
            if !transaction.doc_changed() || transaction.annotation(appended()).is_some() {
                return None;
            }
            commands::insert_text("!")(transaction.state())
        });
        let (mut state, _) = build_state(
            &schema,
            &transaction_appender().of(appender),
            Some(source.document().clone()),
            &DocTypes::none(),
        );
        let before = state.clone();
        let guard_track = track.clone();
        let guard_calls = calls.clone();
        let guard_schema = schema.clone();
        let guard: super::TransactionGuard = Box::new(move |transactions| {
            assert_eq!(transactions.len(), 2);
            guard_calls.fetch_add(1, Ordering::Relaxed);
            guard_track
                .apply_transactions(&guard_schema, transactions)
                .map_err(|error| EditRejection::Protected(error.to_string()))
        });
        let edit = commands::insert_text("new")(&state).unwrap();
        // A snapshot rejection must run before a source guard with side effects.
        assert!(
            super::apply_guarded_transactions(
                &mut state,
                [edit.clone()],
                Some(&read_only()),
                Some(&guard)
            )
            .is_err()
        );
        assert_unchanged(&before, &state);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let transactions =
            super::apply_guarded_transactions(&mut state, [edit], None, Some(&guard)).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.doc(), transactions.last().unwrap().new_doc());
        assert_eq!(track.save(&schema, state.doc()).unwrap(), "new!base");
    }

    #[test]
    fn transaction_guard_rejection_keeps_state_and_history() {
        let mut state = at(&state_of("original"), 4);
        let before = state.clone();
        let guard: super::TransactionGuard =
            Box::new(|_| Err(EditRejection::Protected("Cannot save.".into())));
        let edit = commands::insert_text("new")(&state).unwrap();
        assert!(super::apply_guarded_transactions(&mut state, [edit], None, Some(&guard)).is_err());
        assert_unchanged(&before, &state);
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
        let codecs = CommonMarkCodecs::new(state.schema().clone(), Default::default());
        assert_eq!(clipboard::markup(&codecs, &slice), "copy **this**");
        assert_eq!(state.doc(), &before);
        assert_eq!(undo_depth(&state), 0);
    }
}
