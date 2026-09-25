//! The app's own flows, run headless against GPUI's test platform: a real
//! [`MarkraftApp`] over a real notes folder in a temporary directory, driven by the
//! keystrokes and actions a person would use, and judged by what reaches the disk.
//!
//! What this layer does not have: the menu bar, the global shortcuts and the native
//! window (the app runs without its platform half), text shaped by a real font (the
//! test platform lays text out with a placeholder system, so nothing here clicks by
//! position), and an input method. Those stay with the checks on a real Mac.
//!
//! The vault writes from a thread of its own, outside GPUI's scheduler, so what is
//! on disk is waited for with [`Harness::wait_for_file`] rather than assumed.
//!
//! Each file is one area of the app.

mod harness;

mod blocks;
mod disk;
mod inline;
mod input;
mod paste;
mod sweep;
mod switching;
mod tables;
mod vim;
