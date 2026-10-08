//! Headless Markdown notes and persistence, independent of GPUI and application windows.
//! [`NotesLibrary`] is the integration API. The store, the storage worker and the
//! settings file behind it are the Markraft workspace's and application's to drive.
//! They are named only with the `unstable-internals` feature, and carry no
//! compatibility promise.

/// A module that only the Markraft workspace and application may name.
macro_rules! internal {
    ($($name:ident),*) => {$(
        #[cfg(feature = "unstable-internals")]
        #[doc(hidden)]
        pub mod $name;
        // Without the feature nothing outside this crate reaches the module, so
        // what only the workspace calls reads as unused.
        #[cfg(not(feature = "unstable-internals"))]
        #[allow(dead_code)]
        mod $name;
    )*};
}
pub mod backend;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod directory;
mod engine;
pub use backend::*;
pub use engine::{NewRecord, NoteSaveOutcome, NotesSession};
pub mod daily;
pub mod doc;
mod library;
pub mod locale;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod memory;
mod platform;
internal!(fs, persistence, storage, vault);
pub use fs::StoreError;
pub use library::*;
pub use storage::{
    AttachmentPolicy, BulletMarker, CodeFence, EditorFont, EmphasisMarker, HardBreakStyle,
    ImageNaming, LineHeight, LineWidth, NoteNaming, OrderedDelimiter, Preferences,
    SettingsWindowPlacement, Summon, TabKey, TextCheckingPreferences, TextCheckingSetting,
    WorkspaceSettings,
};
