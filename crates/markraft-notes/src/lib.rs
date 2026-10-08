//! Headless Markdown notes and persistence, independent of GPUI and application windows.
//! [`NotesLibrary`] is the integration API. The module-level APIs remain available
//! for the Markraft workspace adapter while its existing file formats are preserved.
pub mod backend;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod directory;
mod engine;
pub use backend::*;
pub use engine::{NewRecord, NoteSaveOutcome, NotesSession};
pub mod daily;
pub mod doc;
#[doc(hidden)]
pub mod fs;
mod library;
pub mod locale;
#[doc(hidden)]
pub mod persistence;
mod platform;
#[doc(hidden)]
pub mod storage;
#[doc(hidden)]
pub mod vault;
pub use library::*;
