//! The suites that say nothing about files, run again over notes that a host keeps.
//! One store serves both kinds of storage, and these runs hold the workspace to
//! that: what a suite sees must not depend on where a note is kept.
//!
//! Each suite's file is a module here and under `e2e`, on purpose: the same tests,
//! over the other harness.
#![allow(clippy::duplicate_mod)]

pub(crate) mod harness {
    pub(crate) use crate::e2e::harness::Harness;
    use crate::storage::Preferences;
    use gpui::TestAppContext;

    pub(crate) fn open(
        cx: &mut TestAppContext,
        configure: impl FnOnce(&mut Preferences),
    ) -> Harness<'_> {
        open_with(cx, &[], configure)
    }
    pub(crate) fn open_with<'a>(
        cx: &'a mut TestAppContext,
        files: &[(&str, &str)],
        configure: impl FnOnce(&mut Preferences),
    ) -> Harness<'a> {
        crate::e2e::harness::open_records(cx, files, configure)
    }
}

#[path = "blocks.rs"]
mod blocks;
#[path = "find.rs"]
mod find;
#[path = "ime.rs"]
mod ime;
#[path = "inline.rs"]
mod inline;
#[path = "input.rs"]
mod input;
#[path = "math.rs"]
mod math;
#[path = "paste.rs"]
mod paste;
#[path = "sweep.rs"]
mod sweep;
#[path = "switching.rs"]
mod switching;
#[path = "tables.rs"]
mod tables;
#[path = "vim.rs"]
mod vim;
