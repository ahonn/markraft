//! How a changed preference reaches what reads it.
//!
//! The settings window speaks in [`Pref`]s; [`MarkraftApp::set_preference`]
//! writes one into the application's [`Preferences`] and
//! [`MarkraftApp::apply_preferences`] carries whatever differs from before to
//! the editors, the platform and the Markdown writer — the one place that knows
//! which field is read where.

use super::{MarkraftApp, apply_markdown_style};
use crate::platform::Shortcut;
use crate::storage::{Pref, Preferences};
use gpui::{Context, Window};

impl MarkraftApp {
    /// Set one preference and carry it to whatever reads it.
    pub(in crate::app) fn set_preference(
        &mut self,
        pref: Pref,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.preferences.clone();
        pref.apply(&mut self.preferences);
        self.apply_preferences(&before, window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Carry every preference that differs from `before` to what reads it. A
    /// global shortcut the platform refuses goes back to what it was, and the
    /// refusal is shown beside its field.
    pub(in crate::app) fn apply_preferences(
        &mut self,
        before: &Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.preferences.clone();
        if before.dark_mode != now.dark_mode {
            self.apply_theme(window, cx);
        }
        if before.vim_mode != now.vim_mode {
            self.apply_vim(now.vim_mode, window, cx);
        }
        if before.emoji_characters != now.emoji_characters {
            self.emoji.set_characters(now.emoji_characters);
        }
        if before.remote_images != now.remote_images {
            let fetcher = self.remote_image_fetcher();
            for editor in self.editors() {
                editor.update(cx, |editor, cx| {
                    editor.set_remote_images(fetcher.clone(), cx)
                });
            }
        }
        if before.animate_images != now.animate_images {
            for editor in self.editors() {
                editor.update(cx, |editor, cx| {
                    editor.set_animate_images(now.animate_images, cx)
                });
            }
        }
        if before.auto_number_equations != now.auto_number_equations {
            for editor in self.editors() {
                editor.update(cx, |editor, cx| {
                    editor.set_auto_number_equations(now.auto_number_equations, cx)
                });
            }
        }
        if before.hotkey != now.hotkey {
            self.apply_shortcut(Shortcut::Toggle, before);
        }
        if before.new_note_hotkey != now.new_note_hotkey {
            self.apply_shortcut(Shortcut::NewNote, before);
        }
        if before.text_size != now.text_size
            || before.font != now.font
            || before.line_height != now.line_height
            || before.line_width != now.line_width
        {
            self.restyle_editors(cx);
        }
        if before.always_on_top != now.always_on_top
            && let Some(platform) = &self.platform
            && let Err(error) = platform.set_always_on_top(window, now.always_on_top)
        {
            self.feedback.set_platform_error(Some(error));
        }
        if before.all_spaces != now.all_spaces
            && let Some(platform) = &self.platform
            && let Err(error) = platform.set_all_spaces(window, now.all_spaces)
        {
            self.feedback.set_platform_error(Some(error));
        }
        if before.tab_key != now.tab_key {
            for editor in self.editors() {
                editor.update(cx, |editor, cx| {
                    editor.set_indent_text(now.tab_key.text(), cx)
                });
            }
        }
        if before.markdown_shortcuts != now.markdown_shortcuts {
            self.shortcuts
                .store(now.markdown_shortcuts, std::sync::atomic::Ordering::Relaxed);
        }
        if before.auto_pair != now.auto_pair {
            self.pairs
                .store(now.auto_pair, std::sync::atomic::Ordering::Relaxed);
        }
        if before.bullet_marker != now.bullet_marker
            || before.code_fence != now.code_fence
            || before.emphasis_marker != now.emphasis_marker
            || before.ordered_delimiter != now.ordered_delimiter
            || before.hard_break != now.hard_break
        {
            apply_markdown_style(&self.house, &now);
        }
    }

    /// Register the global shortcut the preferences now name for `which`. One the
    /// platform refuses is put back to `before`'s, with the refusal beside the field.
    fn apply_shortcut(&mut self, which: Shortcut, before: &Preferences) {
        let Some(platform) = &mut self.platform else {
            return;
        };
        let index = which as usize;
        let value = match which {
            Shortcut::Toggle => self.preferences.hotkey.clone(),
            Shortcut::NewNote => self.preferences.new_note_hotkey.clone(),
        };
        match platform.set_shortcut(which, &value) {
            Ok(()) => {
                self.settings_errors.shortcuts[index] = None;
                self.feedback.set_platform_error(None);
            }
            Err(error) => {
                match which {
                    Shortcut::Toggle => self.preferences.hotkey.clone_from(&before.hotkey),
                    Shortcut::NewNote => self
                        .preferences
                        .new_note_hotkey
                        .clone_from(&before.new_note_hotkey),
                }
                self.settings_errors.shortcuts[index] = Some(error);
            }
        }
    }

    /// Every open note's editor.
    fn editors(&self) -> Vec<gpui::Entity<markraft_gpui::EditorView>> {
        self.sessions
            .values()
            .map(|session| session.editor().clone())
            .collect()
    }
}
