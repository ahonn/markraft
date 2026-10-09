//! Local note-library persistence owned by the application, never by the editor.
use crate::doc;
use crate::fs::StoreError;
use crate::locale::LanguagePreference;
use crate::locale::Message;
use markraft_core::Node;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

/// Native prose services are shared across notes; code remains literal.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TextCheckingPreferences {
    pub spelling: bool,
    pub grammar: bool,
    pub correction: bool,
    pub quotes: bool,
    pub dashes: bool,
    pub replacements: bool,
    pub links: bool,
    pub data_detectors: bool,
    pub smart_insert_delete: bool,
}

impl Default for TextCheckingPreferences {
    fn default() -> Self {
        Self {
            spelling: true,
            grammar: false,
            correction: false,
            quotes: false,
            dashes: false,
            replacements: true,
            links: true,
            data_detectors: true,
            smart_insert_delete: true,
        }
    }
}

/// One switch of [`TextCheckingPreferences`], as a menu item or a native
/// panel names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextCheckingSetting {
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

impl TextCheckingPreferences {
    pub fn get(&self, setting: TextCheckingSetting) -> bool {
        let mut settings = *self;
        *settings.slot(setting)
    }

    pub fn set(&mut self, setting: TextCheckingSetting, value: bool) {
        *self.slot(setting) = value;
    }

    fn slot(&mut self, setting: TextCheckingSetting) -> &mut bool {
        match setting {
            TextCheckingSetting::SmartInsertDelete => &mut self.smart_insert_delete,
            TextCheckingSetting::Spelling => &mut self.spelling,
            TextCheckingSetting::Grammar => &mut self.grammar,
            TextCheckingSetting::Correction => &mut self.correction,
            TextCheckingSetting::Quotes => &mut self.quotes,
            TextCheckingSetting::Dashes => &mut self.dashes,
            TextCheckingSetting::Replacements => &mut self.replacements,
            TextCheckingSetting::Links => &mut self.links,
            TextCheckingSetting::DataDetectors => &mut self.data_detectors,
        }
    }
}

/// Folder name under `Documents` used when Settings has no notes folder yet.
pub const DEFAULT_NOTES_FOLDER_NAME: &str = "Markraft";

/// `~/Documents/Markraft` for the current user, when `HOME` is available.
pub fn default_notes_folder() -> Option<PathBuf> {
    Some(default_notes_folder_in(Path::new(&env::var_os("HOME")?)))
}

/// Notes folder used for a fresh install under `home`.
pub fn default_notes_folder_in(home: &Path) -> PathBuf {
    home.join("Documents").join(DEFAULT_NOTES_FOLDER_NAME)
}

/// Create `directory` if needed and return its canonical path.
pub fn ensure_notes_folder(directory: &Path) -> Result<PathBuf, StoreError> {
    fs::create_dir_all(directory).map_err(|error| crate::fs::describe(directory, &error))?;
    fs::canonicalize(directory).map_err(|error| crate::fs::describe(directory, &error))
}

/// Pick the notes folder for this launch: `--dir`, then Settings, then the default.
///
/// `--dir` and the default are created when missing. A path already stored in
/// Settings is left alone so a missing vault surfaces as an error instead of
/// silently falling back to Documents/Markraft.
pub fn resolve_notes_folder(
    override_dir: Option<PathBuf>,
    settings_folder: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<PathBuf, StoreError> {
    if let Some(path) = override_dir {
        return ensure_notes_folder(&path);
    }
    if let Some(path) = settings_folder {
        return Ok(path);
    }
    let home = home.ok_or_else(|| Message::new("error.home-missing"))?;
    ensure_notes_folder(&default_notes_folder_in(home))
}

/// Whether `settings` already records `folder` (same path, allowing non-canonical forms).
pub fn notes_folder_matches(settings: Option<&Path>, folder: &Path) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    if settings == folder {
        return true;
    }
    match (fs::canonicalize(settings), fs::canonicalize(folder)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

const LIBRARY_VERSION: u32 = 2;

/// Settings coordinates are local to the display identified by its stable UUID.
/// The last height avoids moving a short page upward before it is measured again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsWindowPlacement {
    pub origin: [f32; 2],
    pub height: f32,
    pub display_uuid: Option<String>,
}

impl SettingsWindowPlacement {
    pub fn is_valid(&self) -> bool {
        self.origin.iter().all(|value| value.is_finite())
            && self.height.is_finite()
            && self.height > 0.
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Preferences {
    pub text_checking: TextCheckingPreferences,
    /// Requested display language; old settings continue to follow the system.
    pub language: LanguagePreference,
    pub dark_mode: Option<bool>,
    pub auto_height: bool,
    pub show_word_count: bool,
    pub hotkey: String,
    pub window_bounds: Option<[f32; 4]>,
    pub settings_window: Option<SettingsWindowPlacement>,
    /// Modal editing in the note editors. Settings files written before it existed
    /// deserialize to `false`.
    pub vim_mode: bool,
    /// Whether notes fetch the remote images they show. Settings files written
    /// before it existed deserialize to the default, on.
    pub remote_images: bool,
    /// Whether an animated image plays while the pointer rests on it. Settings
    /// files written before it existed deserialize to the default, on.
    pub animate_images: bool,
    /// The note's body text size in points; headings and spacing scale with it.
    pub text_size: f32,
    /// Hide the note when another app becomes active, for quick capture.
    pub hide_on_deactivate: bool,
    /// Keep the note above other apps' windows.
    pub always_on_top: bool,
    /// The global shortcut that opens a new note. Empty turns it off.
    pub new_note_hotkey: String,
    /// The global shortcut that opens today's daily note. Empty turns it off.
    pub daily_note_hotkey: String,
    /// Whether the emoji menu and `:name:` write the emoji character rather than its
    /// shortcode. Off by default: shortcodes are written.
    pub emoji_characters: bool,
    /// What Tab inserts in a code block.
    pub tab_key: TabKey,
    /// Whether typing `# `, `- `, `> ` and the like at the start of a line turns it
    /// into that block.
    pub markdown_shortcuts: bool,
    /// Number display formulas in document order; manual tags work in either mode.
    pub auto_number_equations: bool,
    /// The typeface of the note's prose; code keeps its monospaced one.
    pub font: EditorFont,
    pub line_height: LineHeight,
    /// The marker a list made from the toolbar or the `/` menu is written with. One
    /// typed at the start of a line keeps the marker typed.
    pub bullet_marker: BulletMarker,
    /// The fence a code block made from the toolbar or the `/` menu is written with.
    pub code_fence: CodeFence,
    /// The delimiter ⌘I and ⌘B write: `*` or `_`, doubled for strong.
    pub emphasis_marker: EmphasisMarker,
    /// The Settings page last shown, which the window opens on next time.
    pub settings_page: String,
    /// What the note shows when the shortcut or the menu bar brings it back.
    pub summon: Summon,
    /// How wide a line of the note may run before it wraps.
    pub line_width: LineWidth,
    /// Typing an opening bracket or quote writes its closing one too.
    pub auto_pair: bool,
    /// Moving a note to the Trash asks first.
    pub confirm_delete: bool,
    /// The note window is on every Space rather than the one it was opened on.
    pub all_spaces: bool,
    /// The note comes up on the display the pointer is on.
    pub follow_pointer: bool,
    /// The delimiter a numbered list made from the toolbar or the `/` menu takes.
    pub ordered_delimiter: OrderedDelimiter,
    /// How a line break inside a paragraph is written.
    pub hard_break: HardBreakStyle,
}

// The variant names are what the settings file stores, so they keep their shape.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Summon {
    /// The note that was showing.
    #[default]
    LastNote,
    /// A new note, unless the one showing is still empty.
    NewNote,
    /// Today's daily note, made if it does not exist yet.
    DailyNote,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineWidth {
    Narrow,
    #[default]
    Normal,
    /// As wide as the window.
    Full,
}

impl LineWidth {
    /// The longest a line may run, in multiples of the text size: 36 and 50 em are
    /// about 504 and 700 points at 14 pt, the latter a common readable length.
    pub fn ems(self) -> Option<f32> {
        match self {
            LineWidth::Narrow => Some(36.),
            LineWidth::Normal => Some(50.),
            LineWidth::Full => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderedDelimiter {
    #[default]
    Period,
    Parenthesis,
}

impl OrderedDelimiter {
    pub fn char(self) -> char {
        match self {
            OrderedDelimiter::Period => '.',
            OrderedDelimiter::Parenthesis => ')',
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardBreakStyle {
    /// `\` at the end of the line, which shows.
    #[default]
    Backslash,
    /// Two trailing spaces, which do not.
    Spaces,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabKey {
    #[default]
    Tab,
    TwoSpaces,
    FourSpaces,
}

impl TabKey {
    pub fn text(self) -> &'static str {
        match self {
            TabKey::Tab => "\t",
            TabKey::TwoSpaces => "  ",
            TabKey::FourSpaces => "    ",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorFont {
    #[default]
    System,
    Serif,
    Rounded,
    Mono,
}

impl EditorFont {
    /// The family GPUI shapes it with: each is one of the system's own designs, so
    /// every Mac has it.
    pub fn family(self) -> &'static str {
        match self {
            EditorFont::System => ".SystemUIFont",
            EditorFont::Serif => ".AppleSystemUIFontSerif",
            EditorFont::Rounded => ".AppleSystemUIFontRounded",
            EditorFont::Mono => ".AppleSystemUIFontMonospaced",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineHeight {
    Tight,
    #[default]
    Normal,
    Relaxed,
}

impl LineHeight {
    /// Line height as a multiple of the text size.
    pub fn ratio(self) -> f32 {
        match self {
            LineHeight::Tight => 1.35,
            LineHeight::Normal => 1.5,
            LineHeight::Relaxed => 1.7,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletMarker {
    #[default]
    Dash,
    Star,
    Plus,
}

impl BulletMarker {
    pub fn char(self) -> char {
        match self {
            BulletMarker::Dash => '-',
            BulletMarker::Star => '*',
            BulletMarker::Plus => '+',
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeFence {
    #[default]
    Backticks,
    Tildes,
}

impl CodeFence {
    pub fn char(self) -> char {
        match self {
            CodeFence::Backticks => '`',
            CodeFence::Tildes => '~',
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmphasisMarker {
    #[default]
    Star,
    Underscore,
}

impl EmphasisMarker {
    pub fn char(self) -> char {
        match self {
            EmphasisMarker::Star => '*',
            EmphasisMarker::Underscore => '_',
        }
    }
}

impl Preferences {
    /// Formatting command preferences scoped to this workspace.
    pub fn markers(&self) -> crate::doc::Markers {
        crate::doc::Markers {
            bullet: self.bullet_marker.char(),
            ordered: self.ordered_delimiter.char(),
            fence: self.code_fence.char(),
        }
    }

    pub const DEFAULT_TEXT_SIZE: f32 = 14.;
    pub const TEXT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=24.0;

    /// Whether these preferences can be written as they stand: a remembered
    /// window size that is not a size refuses the save, since it would refuse
    /// the next launch.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.window_bounds.is_some_and(|bounds| {
            bounds.iter().any(|value| !value.is_finite()) || bounds[2] <= 0.0 || bounds[3] <= 0.0
        }) {
            return Err(Message::new("error.invalid-bounds").into());
        }
        if self
            .settings_window
            .as_ref()
            .is_some_and(|saved| !saved.is_valid())
        {
            return Err(Message::new("error.invalid-bounds").into());
        }
        Ok(())
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: LanguagePreference::default(),
            text_checking: TextCheckingPreferences::default(),
            dark_mode: None,
            auto_height: true,
            show_word_count: true,
            hotkey: "Alt+N".into(),
            window_bounds: None,
            settings_window: None,
            vim_mode: false,
            remote_images: true,
            animate_images: true,
            text_size: Self::DEFAULT_TEXT_SIZE,
            hide_on_deactivate: false,
            always_on_top: true,
            new_note_hotkey: String::new(),
            daily_note_hotkey: String::new(),
            emoji_characters: false,
            tab_key: TabKey::default(),
            markdown_shortcuts: true,
            auto_number_equations: false,
            font: EditorFont::default(),
            line_height: LineHeight::default(),
            bullet_marker: BulletMarker::default(),
            code_fence: CodeFence::default(),
            emphasis_marker: EmphasisMarker::default(),
            settings_page: String::new(),
            summon: Summon::default(),
            line_width: LineWidth::default(),
            auto_pair: true,
            confirm_delete: true,
            all_spaces: false,
            follow_pointer: false,
            ordered_delimiter: OrderedDelimiter::default(),
            hard_break: HardBreakStyle::default(),
        }
    }
}

/// One preference as the settings window sets it: the vocabulary the window
/// speaks, with [`Pref::apply`] the one place each word meets its field. The
/// window's bounds are the window's to remember, not the settings window's to
/// set, so they have no word here.
#[derive(Clone, Debug, PartialEq)]
pub enum Pref {
    TextChecking(TextCheckingPreferences),
    Language(LanguagePreference),
    Theme(Option<bool>),
    AutoHeight(bool),
    ShowWordCount(bool),
    VimMode(bool),
    EmojiCharacters(bool),
    RemoteImages(bool),
    AnimateImages(bool),
    /// The global shortcut that shows the note. Empty turns it off.
    Hotkey(String),
    /// The global shortcut that opens a new note. Empty turns it off.
    NewNoteHotkey(String),
    /// The global shortcut that opens today's daily note. Empty turns it off.
    DailyNoteHotkey(String),
    /// Rounded and kept within [`Preferences::TEXT_SIZES`].
    TextSize(f32),
    HideOnDeactivate(bool),
    AlwaysOnTop(bool),
    TabKey(TabKey),
    MarkdownShortcuts(bool),
    AutoNumberEquations(bool),
    Font(EditorFont),
    LineHeight(LineHeight),
    Bullet(BulletMarker),
    Fence(CodeFence),
    Emphasis(EmphasisMarker),
    Summon(Summon),
    LineWidth(LineWidth),
    AutoPair(bool),
    ConfirmDelete(bool),
    AllSpaces(bool),
    FollowPointer(bool),
    OrderedDelimiter(OrderedDelimiter),
    HardBreak(HardBreakStyle),
    /// The settings page on screen, remembered for the next time the window opens.
    SettingsPage(String),
}

impl Pref {
    /// Write this setting into `preferences`.
    pub fn apply(self, preferences: &mut Preferences) {
        match self {
            Pref::TextChecking(settings) => preferences.text_checking = settings,
            Pref::Language(language) => preferences.language = language,
            Pref::Theme(mode) => preferences.dark_mode = mode,
            Pref::AutoHeight(on) => preferences.auto_height = on,
            Pref::ShowWordCount(on) => preferences.show_word_count = on,
            Pref::VimMode(on) => preferences.vim_mode = on,
            Pref::EmojiCharacters(on) => preferences.emoji_characters = on,
            Pref::RemoteImages(on) => preferences.remote_images = on,
            Pref::AnimateImages(on) => preferences.animate_images = on,
            Pref::Hotkey(shortcut) => preferences.hotkey = shortcut,
            Pref::NewNoteHotkey(shortcut) => preferences.new_note_hotkey = shortcut,
            Pref::DailyNoteHotkey(shortcut) => preferences.daily_note_hotkey = shortcut,
            Pref::TextSize(size) => {
                let range = Preferences::TEXT_SIZES;
                preferences.text_size = size.round().clamp(*range.start(), *range.end());
            }
            Pref::HideOnDeactivate(on) => preferences.hide_on_deactivate = on,
            Pref::AlwaysOnTop(on) => preferences.always_on_top = on,
            Pref::TabKey(key) => preferences.tab_key = key,
            Pref::MarkdownShortcuts(on) => preferences.markdown_shortcuts = on,
            Pref::AutoNumberEquations(on) => preferences.auto_number_equations = on,
            Pref::Font(font) => preferences.font = font,
            Pref::LineHeight(height) => preferences.line_height = height,
            Pref::Bullet(marker) => preferences.bullet_marker = marker,
            Pref::Fence(fence) => preferences.code_fence = fence,
            Pref::Emphasis(marker) => preferences.emphasis_marker = marker,
            Pref::Summon(summon) => preferences.summon = summon,
            Pref::LineWidth(width) => preferences.line_width = width,
            Pref::AutoPair(on) => preferences.auto_pair = on,
            Pref::ConfirmDelete(on) => preferences.confirm_delete = on,
            Pref::AllSpaces(on) => preferences.all_spaces = on,
            Pref::FollowPointer(on) => preferences.follow_pointer = on,
            Pref::OrderedDelimiter(delimiter) => preferences.ordered_delimiter = delimiter,
            Pref::HardBreak(style) => preferences.hard_break = style,
            Pref::SettingsPage(page) => preferences.settings_page = page,
        }
    }
}

/// A document and its local UI state. Paths never depend on the displayed title.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: String,
    pub document: Node,
    /// Host-defined display name, independent of Markdown headings.
    pub title_override: Option<String>,
    /// Backend-defined unique identity, for example `daily:2026-10-08`.
    pub logical_key: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub deleted_at: Option<u64>,
    pub pinned: bool,
    /// Absolute location, independent of the displayed title.
    pub path: Option<PathBuf>,
    pub read_only: Option<Message>,
    pub conflicted: bool,
}

impl Note {
    /// Whether the note holds nothing at all. A file Markraft could not read comes
    /// back like this: it has a name and a reason, but never became a document.
    pub fn document_is_empty(&self) -> bool {
        crate::doc::is_blank(&self.document)
    }
    pub fn title(&self) -> String {
        title_of(self.title_override.as_deref(), &self.document)
    }

    /// The note's file name without its extension, which copies of the note
    /// are named after. A note without a file goes by its title.
    pub fn file_stem(&self) -> String {
        match self.path.as_deref().and_then(Path::file_stem) {
            Some(stem) => stem.to_string_lossy().into_owned(),
            None => self.title().replace(['/', ':'], "-"),
        }
    }

    /// The note's file name with `extension` in place of its own, for a copy of
    /// it in another format.
    pub fn file_name(&self, extension: &str) -> String {
        format!("{}.{extension}", self.file_stem())
    }

    /// User-visible fallback titles are localized without changing file names. A
    /// note with nothing to title it but a file, such as a daily note made from no
    /// template, goes by the file's name, as the title bar names it.
    pub fn title_message(&self) -> Message {
        if self.title_override.is_some() || doc::title_line(&self.document).is_some() {
            return self.title().into();
        }
        match self.path.as_deref().and_then(Path::file_stem) {
            Some(stem) => stem.to_string_lossy().into_owned().into(),
            None => Message::new("error.untitled-note"),
        }
    }

    pub fn display_title(&self, i18n: &crate::locale::I18n) -> String {
        self.title_message().render(i18n)
    }

    /// Where the file is, as a search matches it: the path under the notes folder
    /// when it is inside one, and otherwise the file's own name.
    pub fn location(&self, root: Option<&Path>) -> Option<String> {
        let path = self.path.as_ref()?;
        match root.and_then(|root| path.strip_prefix(root).ok()) {
            Some(relative) => Some(relative.display().to_string()),
            None => Some(path.file_name()?.to_string_lossy().into_owned()),
        }
    }
}

/// What a note is called: the name its host gave it, or the first line it says.
pub(crate) fn title_of(title_override: Option<&str>, document: &Node) -> String {
    if let Some(title) = title_override {
        return title.to_owned();
    }
    doc::title_line(document)
        .as_deref()
        .unwrap_or("Untitled")
        .graphemes(true)
        .take(64)
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    pub version: u32,
    pub active_id: String,
    pub notes: Vec<Note>,
    pub workspace: WorkspaceSettings,
    /// Explicit user deletion intent. Absence from a snapshot never means deletion.
    pub deletions: HashMap<String, Note>,
    /// Per-note generations allow receipts to clear only the snapshot they saved.
    pub changes: HashMap<String, u64>,
    pub(crate) generation: u64,
    pub(crate) search_index: SearchIndex,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SearchIndex(Arc<Mutex<HashMap<String, SearchEntry>>>);
impl PartialEq for SearchIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
#[derive(Clone, Debug)]
struct SearchEntry {
    document: Node,
    text: String,
    title: String,
    /// The title a host gave the note, which neither its text nor its path says.
    given_title: Option<String>,
}
impl SearchEntry {
    fn of(note: &Note) -> Self {
        Self {
            document: note.document.clone(),
            text: doc::plain_text(&note.document).to_lowercase(),
            title: note.title().to_lowercase(),
            given_title: note.title_override.clone(),
        }
    }
}

impl Default for Library {
    fn default() -> Self {
        let mut library = Self {
            version: LIBRARY_VERSION,
            active_id: String::new(),
            notes: Vec::new(),
            workspace: WorkspaceSettings::default(),
            deletions: HashMap::new(),
            changes: HashMap::new(),
            generation: 0,
            search_index: SearchIndex::default(),
        };
        library.new_note(doc::empty());
        library
    }
}

impl Library {
    /// Current committed library generation, for immutable export snapshots.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn active_note(&self) -> &Note {
        self.note(&self.active_id)
            .expect("the library always has an active note")
    }

    pub fn note(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|note| note.id == id)
    }

    pub fn new_note(&mut self, document: Node) -> String {
        let id = Uuid::new_v4().to_string();
        let now = timestamp();
        self.notes.push(Note {
            id: id.clone(),
            document,
            title_override: None,
            logical_key: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            pinned: false,
            path: None,
            read_only: None,
            conflicted: false,
        });
        self.active_id.clone_from(&id);
        self.mark_changed(&id);
        id
    }

    /// Take a note as another program left it on disk, replacing any note with its id.
    pub fn adopt(&mut self, note: Note) {
        self.changes.remove(&note.id);
        self.deletions.remove(&note.id);
        match self
            .notes
            .iter_mut()
            .find(|existing| existing.id == note.id)
        {
            Some(existing) => *existing = note,
            None => self.notes.push(note),
        }
        self.ensure_active();
    }

    pub fn remove(&mut self, id: &str) {
        self.changes.remove(id);
        self.deletions.remove(id);
        self.notes.retain(|note| note.id != id);
        self.ensure_active();
    }

    /// The active note must exist; otherwise open the most recent one, or a new one
    /// when none is left.
    fn ensure_active(&mut self) {
        if self.note(&self.active_id).is_some() {
            return;
        }
        match self.search("", None).first().map(|note| note.id.clone()) {
            Some(id) => self.active_id = id,
            None => {
                self.new_note(doc::empty());
            }
        }
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.note(id).is_some() {
            self.active_id = id.to_owned();
            true
        } else {
            false
        }
    }

    /// Remove the note from the library. The file is trashed by the store on save.
    pub fn delete(&mut self, id: &str) -> bool {
        let Some(index) = self.notes.iter().position(|note| note.id == id) else {
            return false;
        };
        let note = self.notes.remove(index);
        self.deletions.insert(id.to_owned(), note);
        self.mark_changed(id);
        self.ensure_active();
        true
    }

    /// Put back a note whose deletion the store refused. Its file was never
    /// touched, so there is nothing left to save for it.
    pub fn restore_deleted(&mut self, id: &str) -> bool {
        let Some(note) = self.deletions.remove(id) else {
            return false;
        };
        self.changes.remove(id);
        self.notes.push(note);
        self.ensure_active();
        true
    }

    pub fn mark_changed(&mut self, id: &str) {
        self.generation += 1;
        self.changes.insert(id.to_owned(), self.generation);
    }

    pub fn acknowledge_saved(&mut self, changes: &[(String, u64)]) {
        for (id, generation) in changes {
            if self.changes.get(id) == Some(generation) {
                self.changes.remove(id);
                self.deletions.remove(id);
            }
        }
    }

    /// A permissions notification changes editability, not the local document or
    /// the generation of an outstanding save.
    pub fn update_read_only(&mut self, id: &str, value: Option<Message>) {
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == id) {
            note.read_only = value;
        }
    }

    pub fn set_document(&mut self, id: &str, document: Node) -> bool {
        let Some(note) = self.notes.iter_mut().find(|note| note.id == id) else {
            return false;
        };
        if note.read_only.is_some() || note.document == document {
            return false;
        }
        note.document = document;
        note.updated_at = timestamp();
        self.mark_changed(id);
        true
    }

    /// Notes matching `query`, by what they say, by a title their host gave them or by
    /// where their file is. A file's name can differ from its note's title, so a search has
    /// to reach both; `root` is the notes folder, which is what makes a match read like the
    /// path the Browse row shows.
    pub fn search(&self, query: &str, root: Option<&Path>) -> Vec<&Note> {
        let query = query.trim().to_lowercase();
        let mut index = self
            .search_index
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Keep only live entries; snapshots share the cache, but each entry verifies
        // its document before use, so an older snapshot cannot produce stale results.
        let live: HashSet<_> = self.notes.iter().map(|note| note.id.as_str()).collect();
        index.retain(|id, _| live.contains(id.as_str()));
        let mut matches = Vec::new();
        for note in &self.notes {
            let rank = if query.is_empty() {
                0
            } else {
                let entry = index
                    .entry(note.id.clone())
                    .or_insert_with(|| SearchEntry::of(note));
                if entry.document != note.document || entry.given_title != note.title_override {
                    *entry = SearchEntry::of(note);
                }
                // A title taken from the text is matched there, and a placeholder
                // title is not something the note says.
                let titled = entry.given_title.is_some() && entry.title.contains(&query);
                let found = titled
                    || entry.text.contains(&query)
                    || note
                        .location(root)
                        .is_some_and(|path| path.to_lowercase().contains(&query));
                if !found {
                    continue;
                }
                if entry.title == query {
                    0
                } else if entry.title.starts_with(&query) {
                    1
                } else if entry.title.contains(&query) {
                    2
                } else {
                    3
                }
            };
            matches.push((rank, note));
        }
        drop(index);
        matches.sort_by(|(a_rank, a), (b_rank, b)| {
            a_rank
                .cmp(b_rank)
                .then(b.pinned.cmp(&a.pinned))
                .then(b.updated_at.cmp(&a.updated_at))
                .then(a.id.cmp(&b.id))
        });
        matches.into_iter().map(|(_, note)| note).collect()
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != LIBRARY_VERSION {
            return Err(Message::new("error.library-version").into());
        }
        let mut ids = HashSet::new();
        if self
            .notes
            .iter()
            .any(|note| note.id.is_empty() || !ids.insert(&note.id))
        {
            return Err(Message::new("error.duplicate-notes").into());
        }
        if self.note(&self.active_id).is_none() {
            return Err(Message::new("error.active-note-missing").into());
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// The default is assets beside the note, created only on insertion.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AttachmentPolicy {
    #[default]
    Default,
    WorkspaceFolder(PathBuf),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct WorkspaceSettings {
    pub new_note_directory: PathBuf,
    pub attachments: AttachmentPolicy,
    /// What a new note's file is called when it is first written.
    pub new_note_name: NoteNaming,
    /// What a pasted or dropped image's copy is called.
    pub image_name: ImageNaming,
    /// Where daily notes go, what they are called and what they start as.
    pub daily: crate::daily::DailySettings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageNaming {
    /// `image-<uuid>`: never the same twice, and saying nothing.
    #[default]
    RandomId,
    /// The note's name and the local time, `Meeting notes 2026-09-24 10.21.05`.
    NoteAndDate,
}

/// How a new note's file is named. Either way the name is given once, when the note
/// is first written, and never follows later edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteNaming {
    /// Its first line, as a title.
    #[default]
    FirstLine,
    /// The local date and time it was created, `2026-09-23 14.05`.
    DateTime,
}

/// Per-machine state kept outside the notes folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder holding the notes. Set on a normal launch to the chosen path
    /// or to `~/Documents/Markraft` when none was stored yet.
    pub notes_folder: Option<PathBuf>,
    pub open_files: Vec<PathBuf>,
    pub preferences: Preferences,
    /// Where the unreadable settings file was kept when these settings had to
    /// fall back to the defaults. It belongs to this launch, not to the file, so
    /// it is never written back.
    #[serde(skip)]
    pub recovered_from: Option<PathBuf>,
}

impl Settings {
    /// A missing file is a first launch. A damaged one is set aside, not overwritten.
    pub fn read(path: &Path) -> Result<Self, StoreError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(crate::fs::describe(path, &error)),
        };
        serde_json::from_slice(&bytes).or_else(|error| {
            log::warn!("{} is not readable settings: {error}", path.display());
            let mut damaged = path.as_os_str().to_os_string();
            damaged.push(format!(".damaged-{}", Uuid::new_v4()));
            let damaged = PathBuf::from(damaged);
            fs::rename(path, &damaged).map_err(|error| crate::fs::describe(path, &error))?;
            Ok(Self {
                recovered_from: Some(damaged),
                ..Self::default()
            })
        })
    }

    /// What to tell the user when their folder choice and hotkey were lost with
    /// the settings file, so the reset does not go unexplained.
    /// What to say about settings that could not be read, and the unreadable
    /// file, kept aside, for the notice to show.
    pub fn recovery_notice(&self) -> Option<(Message, PathBuf)> {
        self.recovered_from
            .clone()
            .map(|damaged| (Message::new("error.settings-reset"), damaged))
    }

    pub fn write(&self, path: &Path) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            log::warn!("settings could not be encoded: {error}");
            Message::new("error.settings-encode")
        })?;
        crate::fs::atomic_write(path, &bytes)
    }
}

/// Said when disk won over edits that were then kept as a conflicted copy.
pub fn conflict_kept() -> Message {
    Message::new("error.conflict-kept")
}

/// Conflict notices distinguish one note from several without embedding a count.
pub fn conflicts_kept(count: usize) -> Message {
    if count == 1 {
        conflict_kept()
    } else {
        Message::new("error.conflicts-kept").arg("count", count.to_string())
    }
}

/// Things the user should be told once, noticed where there is no interface to
/// show them in: on the way to the first window, or on the save worker's thread.
/// Whoever can show them drains them.
#[derive(Clone, Debug, Default)]
pub struct Notices(Arc<Mutex<Vec<Message>>>);

impl Notices {
    /// Repeats are dropped: the same file is read again on every refresh.
    pub fn raise(&self, text: Message) {
        if let Ok(mut pending) = self.0.lock()
            && !pending.contains(&text)
        {
            pending.push(text);
        }
    }

    /// Everything raised since the last call.
    pub fn take(&self) -> Vec<Message> {
        self.0
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn permissions_updates_preserve_unsaved_document_generation() {
        let mut library = Library::default();
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("Unsaved local text"));
        let generation = library.changes[&id];
        library.update_read_only(&id, Some("Read only".into()));
        assert_eq!(library.changes[&id], generation);
        assert_eq!(
            doc::plain_text(&library.note(&id).unwrap().document),
            "Unsaved local text"
        );
        library.update_read_only(&id, None);
        assert_eq!(library.changes[&id], generation);
    }

    #[test]
    fn receipts_clear_only_the_generation_they_saved() {
        let mut library = Library::default();
        let id = library.active_id.clone();
        library.set_document(&id, doc::from_markdown("First"));
        let receipt = vec![(id.clone(), library.changes[&id])];
        library.set_document(&id, doc::from_markdown("Second"));
        library.acknowledge_saved(&receipt);
        assert!(library.changes.contains_key(&id));
        let receipt = vec![(id.clone(), library.changes[&id])];
        library.acknowledge_saved(&receipt);
        assert!(!library.changes.contains_key(&id));
        library.delete(&id);
        assert!(library.deletions.contains_key(&id));
        library.acknowledge_saved(&receipt);
        assert!(library.deletions.contains_key(&id));
    }

    #[test]
    fn cached_search_recomputes_changed_documents_and_keeps_ranking() {
        let mut library = Library::default();
        let first = library.active_id.clone();
        library.set_document(&first, doc::from_markdown("# Alpha\n\nBody"));
        let second = library.new_note(doc::from_markdown("# Alphabet"));
        assert_eq!(library.search("alpha", None)[0].id, first);
        library.set_document(&first, doc::from_markdown("# Renamed\n\nOmega"));
        assert_eq!(library.search("alpha", None)[0].id, second);
        assert_eq!(library.search("omega", None)[0].id, first);
        let older = library.clone();
        library.set_document(&first, doc::from_markdown("# Later"));
        assert!(library.search("omega", None).is_empty());
        assert_eq!(older.search("omega", None)[0].id, first);
        assert!(library.search("omega", None).is_empty());
    }

    #[test]
    fn notes_can_be_created_searched_and_deleted_without_losing_unicode() {
        let mut library = Library::default();
        let initial = library.active_id.clone();
        let document = doc::from_markdown("# 中文 👩🏽‍💻\n\n- [x] **Idea** é");
        let id = library.new_note(document.clone());
        assert_eq!(library.active_note().title(), "中文 👩🏽‍💻");
        assert_eq!(library.search("IDEA", None)[0].document, document);
        assert!(library.delete(&id));
        assert!(!library.select(&id));
        assert_eq!(library.active_id, initial);
        assert!(library.search("👩🏽‍💻", None).is_empty());
        assert!(library.delete(&initial));
        assert_eq!(library.search("", None).len(), 1);
        assert_eq!(library.active_note().document, doc::empty());
    }

    // A note with nothing to title it goes by a placeholder, which is not something
    // it says. A title that a host gave is in neither its text nor its path.
    #[test]
    fn a_search_reaches_a_given_title_but_not_the_placeholder() {
        let mut library = Library::default();
        let blank = library.active_id.clone();
        assert_eq!(library.active_note().title(), "Untitled");
        assert!(library.search("untitled", None).is_empty());
        library.notes[0].title_override = Some("Quarterly plan".into());
        assert_eq!(library.search("quarterly", None)[0].id, blank);
        library.notes[0].title_override = Some("Renamed".into());
        assert!(library.search("quarterly", None).is_empty());
        library.notes[0].title_override = None;
        assert!(library.search("renamed", None).is_empty());
    }

    #[test]
    fn a_search_puts_the_note_titled_by_it_first() {
        let mut library = Library::default();
        let titled = library.new_note(doc::from_markdown("hello world"));
        let starts = library.new_note(doc::from_markdown("hello world tour"));
        // Written last, so recency alone would put it first.
        let mentions = library.new_note(doc::from_markdown("links\n\nsee [[hello world]]"));
        for note in &mut library.notes {
            note.updated_at = if note.id == mentions {
                3
            } else if note.id == starts {
                2
            } else {
                1
            };
        }
        let order: Vec<_> = library
            .search("Hello World", None)
            .iter()
            .map(|note| note.id.clone())
            .collect();
        assert_eq!(order, [titled, starts, mentions]);
    }

    #[test]
    fn notes_are_found_by_where_their_file_is_as_well_as_by_what_they_say() {
        let root = PathBuf::from("/Users/someone/Notes");
        let mut library = Library::default();
        let filed = library.new_note(doc::from_markdown("# Standup\n\nagenda"));
        library
            .notes
            .iter_mut()
            .find(|n| n.id == filed)
            .unwrap()
            .path = Some(root.join("Work/Clients/quarterly-review.md"));
        let loose = library.new_note(doc::from_markdown("# Elsewhere"));
        library
            .notes
            .iter_mut()
            .find(|n| n.id == loose)
            .unwrap()
            .path = Some(PathBuf::from("/tmp/scratch-pad.md"));

        fn live(library: &Library, root: &Path, query: &str) -> Vec<String> {
            library
                .search(query, Some(root))
                .iter()
                .map(|note| note.id.clone())
                .collect()
        }
        // By file name, by a directory on the way to it, and case-insensitively.
        assert_eq!(
            live(&library, &root, "quarterly"),
            std::slice::from_ref(&filed)
        );
        assert_eq!(
            live(&library, &root, "CLIENTS"),
            std::slice::from_ref(&filed)
        );
        assert_eq!(
            live(&library, &root, "work/clients"),
            std::slice::from_ref(&filed)
        );
        // Outside the folder only the file's own name is matched, not its folders.
        assert_eq!(
            live(&library, &root, "scratch-pad"),
            std::slice::from_ref(&loose)
        );
        assert!(live(&library, &root, "tmp").is_empty());
        assert_eq!(
            live(&library, &root, "agenda"),
            std::slice::from_ref(&filed)
        );
        assert!(library.delete(&filed));
        assert!(live(&library, &root, "quarterly").is_empty());
    }

    #[test]
    fn fallback_titles_translate_without_changing_persistent_names_or_user_titles() {
        let i18n =
            crate::locale::I18n::for_preference(&LanguagePreference::Locale("zh-Hant".into()));
        let mut library = Library::default();
        let id = library.active_id.clone();
        let note = library.note(&id).unwrap();
        assert_eq!(note.title(), "Untitled");
        assert_eq!(note.display_title(&i18n), "未命名");
        library.set_document(&id, doc::from_markdown("Untitled"));
        assert_eq!(library.note(&id).unwrap().display_title(&i18n), "Untitled");
    }

    #[test]
    fn a_title_skips_leading_markup_and_falls_back_to_untitled() {
        let mut library = Library::default();
        let html = library.new_note(doc::from_markdown("<div class=\"card\">\n\nReal title\n"));
        assert_eq!(library.note(&html).unwrap().title(), "Real title");
        let markup = library.new_note(doc::from_markdown("<hr/>"));
        assert_eq!(library.note(&markup).unwrap().title(), "Untitled");
    }

    #[test]
    fn a_copy_is_named_after_the_file_and_otherwise_the_title() {
        let mut library = Library::default();
        let id = library.new_note(doc::from_markdown("# Trip: day 1/2\n"));
        let note = library.note(&id).unwrap();
        assert_eq!(note.file_name("pdf"), "Trip- day 1-2.pdf");
        let note = library.notes.iter_mut().find(|note| note.id == id).unwrap();
        note.path = Some(PathBuf::from("/notes/2026 plans.v2.md"));
        assert_eq!(note.file_name("html"), "2026 plans.v2.html");
    }

    #[test]
    fn default_notes_folder_lives_under_documents() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            default_notes_folder_in(home),
            PathBuf::from("/Users/someone/Documents/Markraft")
        );
    }

    #[test]
    fn resolve_prefers_override_then_settings_then_default() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let override_dir = root.path().join("override");
        let settings_dir = root.path().join("settings-vault");
        fs::create_dir_all(&settings_dir).unwrap();

        let resolved = resolve_notes_folder(
            Some(override_dir.clone()),
            Some(settings_dir.clone()),
            Some(&home),
        )
        .unwrap();
        assert_eq!(resolved, fs::canonicalize(&override_dir).unwrap());
        assert!(override_dir.is_dir());

        let resolved = resolve_notes_folder(None, Some(settings_dir.clone()), Some(&home)).unwrap();
        assert_eq!(resolved, settings_dir);

        let resolved = resolve_notes_folder(None, None, Some(&home)).unwrap();
        let expected = default_notes_folder_in(&home);
        assert_eq!(resolved, fs::canonicalize(&expected).unwrap());
        assert!(expected.is_dir());
    }

    #[test]
    fn resolve_does_not_create_a_missing_settings_folder() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("gone");
        let resolved = resolve_notes_folder(None, Some(missing.clone()), None).unwrap();
        assert_eq!(resolved, missing);
        assert!(!missing.exists());
    }

    #[test]
    fn resolve_requires_home_when_falling_back_to_the_default() {
        let error = resolve_notes_folder(None, None, None).unwrap_err();
        assert!(error.to_string().contains("HOME"), "{error}");
    }

    #[test]
    fn notes_folder_matches_canonical_and_literal_paths() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("notes");
        fs::create_dir_all(&folder).unwrap();
        let canonical = fs::canonicalize(&folder).unwrap();
        assert!(notes_folder_matches(Some(&folder), &canonical));
        assert!(notes_folder_matches(Some(&canonical), &folder));
        assert!(!notes_folder_matches(None, &folder));
        assert!(!notes_folder_matches(
            Some(&root.path().join("other")),
            &folder
        ));
    }

    #[test]
    fn damaged_settings_are_set_aside_with_something_to_tell_the_user() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();

        let settings = Settings::read(&path).unwrap();
        assert_eq!(settings.notes_folder, None);
        assert_eq!(settings.preferences, Preferences::default());
        let kept = settings.recovered_from.clone().expect("the file was kept");
        assert!(kept.exists());
        let (notice, shown) = settings.recovery_notice().expect("a notice");
        assert!(notice.to_string().contains("reset"), "{notice}");
        assert_eq!(shown, kept);

        // The recovery belongs to this launch: it is not written back, and the
        // settings that replace the damaged file load without a notice.
        settings.write(&path).unwrap();
        let reread = Settings::read(&path).unwrap();
        assert_eq!(reread.recovered_from, None);
        assert_eq!(reread.recovery_notice(), None);
    }

    /// Every field set to something other than its default, so a field that
    /// serialization drops or renames — or a setting aimed at the wrong field —
    /// shows up as a difference.
    fn every_preference_changed() -> Preferences {
        let preferences = Preferences {
            text_checking: TextCheckingPreferences {
                spelling: false,
                grammar: true,
                correction: true,
                quotes: true,
                dashes: true,
                replacements: false,
                links: false,
                data_detectors: false,
                smart_insert_delete: false,
            },
            language: LanguagePreference::Locale("future-Language".into()),
            dark_mode: Some(true),
            auto_height: false,
            show_word_count: false,
            hotkey: "Ctrl+Shift+M".into(),
            window_bounds: Some([1., 2., 3., 4.]),
            settings_window: Some(SettingsWindowPlacement {
                origin: [120., 80.],
                height: 420.,
                display_uuid: Some("settings-display".into()),
            }),
            vim_mode: true,
            remote_images: false,
            animate_images: false,
            text_size: Preferences::DEFAULT_TEXT_SIZE + 3.,
            hide_on_deactivate: true,
            always_on_top: false,
            new_note_hotkey: "Alt+Shift+N".into(),
            daily_note_hotkey: "Alt+Shift+D".into(),
            emoji_characters: true,
            tab_key: TabKey::FourSpaces,
            markdown_shortcuts: false,
            auto_number_equations: true,
            font: EditorFont::Serif,
            line_height: LineHeight::Relaxed,
            bullet_marker: BulletMarker::Plus,
            code_fence: CodeFence::Tildes,
            emphasis_marker: EmphasisMarker::Underscore,
            settings_page: "editor".into(),
            summon: Summon::NewNote,
            line_width: LineWidth::Full,
            auto_pair: false,
            confirm_delete: false,
            all_spaces: true,
            follow_pointer: true,
            ordered_delimiter: OrderedDelimiter::Parenthesis,
            hard_break: HardBreakStyle::Spaces,
        };
        assert_ne!(preferences, Preferences::default());
        preferences
    }

    #[test]
    fn older_settings_keep_new_preferences_at_defaults() {
        let preferences: Preferences =
            serde_json::from_str(r#"{"markdown_shortcuts":true}"#).unwrap();
        assert!(!preferences.auto_number_equations);
        assert!(preferences.settings_window.is_none());
        assert!(preferences.show_word_count);
        assert_eq!(preferences.language, LanguagePreference::System);
    }

    #[test]
    fn invalid_settings_window_geometry_refuses_a_save() {
        for (origin, height) in [
            ([f32::NAN, 20.], 300.),
            ([20., f32::INFINITY], 300.),
            ([20., 30.], f32::INFINITY),
            ([20., 30.], 0.),
            ([20., 30.], -1.),
        ] {
            let preferences = Preferences {
                settings_window: Some(SettingsWindowPlacement {
                    origin,
                    height,
                    display_uuid: None,
                }),
                ..Preferences::default()
            };
            assert!(preferences.validate().is_err());
        }
    }

    #[test]
    fn every_preference_survives_the_settings_file() {
        let preferences = every_preference_changed();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        Settings {
            preferences: preferences.clone(),
            ..Settings::default()
        }
        .write(&path)
        .unwrap();
        assert_eq!(Settings::read(&path).unwrap().preferences, preferences);
    }

    /// Every word the settings window speaks lands in its own field: a field
    /// without a word fails to build the literal, a word aimed at the wrong
    /// field fails the comparison.
    #[test]
    fn every_pref_sets_its_own_field() {
        let changed = every_preference_changed();
        let words = vec![
            Pref::TextChecking(changed.text_checking),
            Pref::Language(changed.language.clone()),
            Pref::Theme(changed.dark_mode),
            Pref::AutoHeight(changed.auto_height),
            Pref::ShowWordCount(changed.show_word_count),
            Pref::VimMode(changed.vim_mode),
            Pref::EmojiCharacters(changed.emoji_characters),
            Pref::RemoteImages(changed.remote_images),
            Pref::AnimateImages(changed.animate_images),
            Pref::Hotkey(changed.hotkey.clone()),
            Pref::NewNoteHotkey(changed.new_note_hotkey.clone()),
            Pref::DailyNoteHotkey(changed.daily_note_hotkey.clone()),
            Pref::TextSize(changed.text_size),
            Pref::HideOnDeactivate(changed.hide_on_deactivate),
            Pref::AlwaysOnTop(changed.always_on_top),
            Pref::TabKey(changed.tab_key),
            Pref::MarkdownShortcuts(changed.markdown_shortcuts),
            Pref::AutoNumberEquations(changed.auto_number_equations),
            Pref::Font(changed.font),
            Pref::LineHeight(changed.line_height),
            Pref::Bullet(changed.bullet_marker),
            Pref::Fence(changed.code_fence),
            Pref::Emphasis(changed.emphasis_marker),
            Pref::Summon(changed.summon),
            Pref::LineWidth(changed.line_width),
            Pref::AutoPair(changed.auto_pair),
            Pref::ConfirmDelete(changed.confirm_delete),
            Pref::AllSpaces(changed.all_spaces),
            Pref::FollowPointer(changed.follow_pointer),
            Pref::OrderedDelimiter(changed.ordered_delimiter),
            Pref::HardBreak(changed.hard_break),
            Pref::SettingsPage(changed.settings_page.clone()),
        ];
        let mut applied = Preferences::default();
        for word in words {
            word.apply(&mut applied);
        }
        // The window remembers its own bounds; no setting speaks for them.
        applied.window_bounds = changed.window_bounds;
        applied.settings_window = changed.settings_window.clone();
        assert_eq!(applied, changed);
    }

    #[test]
    fn a_text_size_is_rounded_and_kept_within_range() {
        let mut preferences = Preferences::default();
        Pref::TextSize(99.).apply(&mut preferences);
        assert_eq!(preferences.text_size, *Preferences::TEXT_SIZES.end());
        Pref::TextSize(12.4).apply(&mut preferences);
        assert_eq!(preferences.text_size, 12.);
    }

    #[test]
    fn notices_are_taken_once_and_never_repeat_themselves() {
        let notices = Notices::default();
        notices.raise("A note could not be read.".into());
        notices.raise("A note could not be read.".into());
        notices.raise("The settings were reset.".into());
        assert_eq!(
            notices
                .take()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["A note could not be read.", "The settings were reset."]
        );
        assert!(notices.take().is_empty());
    }
}
