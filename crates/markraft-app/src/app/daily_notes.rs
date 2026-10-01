//! Daily notes in the window: opening a day's note, making it from the template when it
//! does not exist yet, and stepping to the neighbouring days that have one.
//!
//! Which file holds which day is [`crate::daily`]'s to say; this is where that meets the
//! library, the persistence worker and the note on screen.

use super::*;
use crate::daily::{DailySettings, DateLocale};
use crate::persistence::NewNote;
use chrono::NaiveDate;
use std::collections::BTreeMap;

/// The day it is here, now.
pub(super) fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// The language month and weekday names are spelled in: the system's, as another tool
/// that shares the folder would spell them by default.
pub(super) fn date_locale() -> DateLocale {
    static LOCALE: std::sync::LazyLock<DateLocale> = std::sync::LazyLock::new(|| {
        DateLocale::from_language(&crate::platform::locale::preferred_language(
            &DateLocale::LANGUAGES,
        ))
    });
    *LOCALE
}

impl MarkraftApp {
    /// Every daily note the library holds, by day.
    fn daily_notes(&self) -> BTreeMap<NaiveDate, String> {
        let Some(root) = &self.path else {
            return BTreeMap::new();
        };
        let settings = &self.library.workspace.daily;
        let locale = date_locale();
        self.library
            .notes
            .iter()
            .filter_map(|note| {
                let relative = note.path.as_deref()?.strip_prefix(root).ok()?;
                Some((settings.day_of(relative, locale)?, note.id.clone()))
            })
            .collect()
    }

    /// The day the note on screen stands for, when it is a daily note.
    pub(super) fn active_daily_day(&self) -> Option<NaiveDate> {
        let root = self.path.as_ref()?;
        let relative = self
            .library
            .active_note()
            .path
            .as_deref()?
            .strip_prefix(root)
            .ok()?;
        self.library.workspace.daily.day_of(relative, date_locale())
    }

    /// The nearest daily note before or after the one on screen, skipping the days
    /// that have none. `None` when the note on screen is not a daily note.
    pub(super) fn adjacent_daily_note(&self, forward: bool) -> Option<String> {
        let day = self.active_daily_day()?;
        let notes = self.daily_notes();
        let found = if forward {
            notes.range(day.succ_opt()?..).next()
        } else {
            notes.range(..day).next_back()
        };
        found.map(|(_, id)| id.clone())
    }

    /// The daily note shortcut: bring today's note forward, or put the window away when
    /// it is already what the window shows, so one key goes both ways.
    pub(super) fn toggle_daily_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let today = today();
        let showing = self
            .platform
            .as_ref()
            .is_none_or(|platform| platform.is_visible(window));
        if showing
            && window.is_window_active()
            && self.interaction.panel() == Panel::Editor
            && self.active_daily_day() == Some(today)
        {
            self.hide(window, cx);
            return;
        }
        self.show(window, cx);
        self.open_daily_note(today, window, cx);
    }

    /// Open `day`'s note, making it first when there is none.
    ///
    /// A new one is written at once, through the persistence worker and without
    /// replacing anything: when another program makes the same day's note first, that
    /// note is the one opened.
    pub(super) fn open_daily_note(
        &mut self,
        day: NaiveDate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_reloading() {
            return;
        }
        let Some(root) = self.path.clone().filter(|_| self.persistence.is_some()) else {
            self.inform(Message::new("notice.folder-daily"), cx);
            return;
        };
        let settings = self.library.workspace.daily.clone();
        let locale = date_locale();
        let relative = settings.path_for(day, locale);
        let path = root.join(&relative);
        if let Some(id) = self
            .library
            .notes
            .iter()
            .find(|note| note.path.as_deref() == Some(path.as_path()))
            .map(|note| note.id.clone())
        {
            self.select_note(&id, window, cx);
            return;
        }
        self.close_popover(cx);
        self.cancel_input(cx);
        self.editor().update(cx, |e, cx| e.cancel_composition(cx));
        self.sync_documents(cx);
        self.io.opening += 1;
        let opening = self.io.opening;
        let now = chrono::Local::now().naive_local();
        let template = settings.template.clone();
        let template_name = template
            .as_deref()
            .and_then(std::path::Path::file_stem)
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(persistence) = &self.persistence else {
            return;
        };
        let future = persistence.create_note_async(NewNote {
            relative,
            template,
            fill: Box::new(move |template: Option<&str>| {
                template
                    .map(|text| settings.expand(text, day, now, locale))
                    .unwrap_or_default()
            }),
        });
        self.run_io(future, window, cx, move |this, result, window, cx| {
            let created = match result {
                Ok(created) => created,
                Err(error) => {
                    this.feedback
                        .queue(Message::new("notice.daily-failed").arg("error", error));
                    return;
                }
            };
            if created.template_missing {
                this.feedback.queue(
                    Message::new("notice.daily-template-missing").arg("name", template_name),
                );
            }
            let id = created.note.id.clone();
            if this.library.note(&id).is_none() {
                this.library.adopt(created.note);
            }
            if this.io.opening == opening && this.library.select(&id) {
                this.ensure_session(window, cx);
                this.set_panel(Panel::Editor, cx);
                // The template has been written; what the day adds goes after it.
                this.editor().update(cx, |editor, cx| {
                    let doc = editor.state().doc().clone();
                    let end = markraft_core::Selection::at_end(doc::schema(), &doc);
                    editor.dispatch([markraft_core::TransactionSpec::new().selection(end)], cx);
                });
                if !this.find_open {
                    this.focus_editor(window, cx);
                }
            }
            this.notes_changed(cx);
        });
    }

    /// Settings › Save daily notes in › Choose Folder….
    pub(super) fn configure_daily_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_inside_folder(
            false,
            "dialog.daily-folder",
            window,
            cx,
            |this, relative| {
                this.library.workspace.daily.folder = relative;
                Ok(())
            },
        );
    }

    /// Settings › Daily note template › Choose File…: a Markdown file in the notes
    /// folder.
    pub(super) fn choose_daily_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_inside_folder(
            true,
            "dialog.daily-template",
            window,
            cx,
            |this, relative| {
                let markdown = relative
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| {
                        e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown")
                    });
                if !markdown {
                    return Err(Message::new("notice.choose-markdown-inside"));
                }
                this.library.workspace.daily.template = Some(relative);
                Ok(())
            },
        );
    }

    /// Ask for a folder, or a file, inside the notes folder and hand `apply` where it
    /// is relative to it. What `apply` or the choice itself refuses is said beside the
    /// daily note settings.
    fn choose_inside_folder(
        &mut self,
        files: bool,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Self, std::path::PathBuf) -> Result<(), Message> + 'static,
    ) {
        let Some(root) = self.path.clone() else {
            self.inform(Message::new("notice.folder-daily"), cx);
            return;
        };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files,
            directories: !files,
            multiple: false,
            prompt: Some(self.i18n.text(prompt).into()),
        });
        let paths = self.file_panel(paths, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.first() else {
                return;
            };
            let refusal = if files {
                "notice.choose-markdown-inside"
            } else {
                "notice.choose-inside-folder"
            };
            let relative = root
                .canonicalize()
                .and_then(|root| path.canonicalize().map(|path| (root, path)))
                .map_err(|error| Message::from(error.to_string()))
                .and_then(|(root, path)| {
                    path.strip_prefix(root)
                        .map(ToOwned::to_owned)
                        .map_err(|_| Message::new(refusal))
                });
            let _ = this.update(cx, |this, cx| {
                let result = match relative {
                    Ok(_) if this.path.as_ref() != Some(&root) => {
                        Err(Message::new("notice.folder-changed"))
                    }
                    Ok(relative) => apply(this, relative),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => this.schedule_save(cx),
                    Err(error) => this.set_settings_error_daily(Some(error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The daily note settings of the Obsidian vault this folder is, with its template
    /// found among the notes, when they can be read and used here. Read when the
    /// Settings window comes forward, never while it draws.
    pub(super) fn read_obsidian_daily(&self) -> Option<DailySettings> {
        let root = self.path.as_ref()?;
        let obsidian = crate::daily::read_obsidian(root)?;
        crate::daily::validate_format(&obsidian.format, date_locale()).ok()?;
        let template = obsidian.template.as_deref().and_then(|target| {
            let id = super::resolve_wiki_link(
                target,
                None,
                Some(root),
                self.library
                    .notes
                    .iter()
                    .filter_map(|note| Some((note.id.as_str(), note.path.as_deref()?))),
            )?;
            let path = self.library.note(&id)?.path.as_deref()?;
            Some(path.strip_prefix(root).ok()?.to_owned())
        });
        Some(DailySettings {
            folder: obsidian.folder,
            format: obsidian.format,
            template,
        })
    }

    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_set_daily(&mut self, daily: DailySettings) {
        self.library.workspace.daily = daily;
    }

    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_obsidian_daily(&self) -> Option<DailySettings> {
        self.read_obsidian_daily()
    }

    /// The window brought back from hiding, as the show-and-hide shortcut does.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_summon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.summon(window, cx);
    }

    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_note_count(&self) -> usize {
        self.library.notes.len()
    }
}
