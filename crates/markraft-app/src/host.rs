//! Standalone process services adapted to the reusable workspace.
use crate::locale::Message;
use markraft_workspace::updater::{RelaunchContinuation, UpdateServices};
pub struct Updates {
    updater: crate::updater::Updater,
}
impl Updates {
    pub fn new() -> Self {
        Self {
            updater: crate::updater::Updater::new(),
        }
    }
}
impl UpdateServices for Updates {
    fn updates_itself(&self) -> bool {
        cfg!(feature = "direct-distribution")
    }
    fn take_startup_error(&mut self) -> Option<Message> {
        self.updater.take_startup_error()
    }
    fn check(&self) -> Result<(), Message> {
        self.updater.check()
    }
    #[cfg(feature = "direct-distribution")]
    fn automatically_checks(&self) -> Option<bool> {
        self.updater.automatically_checks()
    }
    #[cfg(feature = "direct-distribution")]
    fn set_automatically_checks(&self, enabled: bool) -> Result<(), Message> {
        self.updater.set_automatically_checks(enabled)
    }
    fn take_relaunch(&self) -> Option<RelaunchContinuation> {
        self.updater.take_relaunch()
    }
    fn postpone(&self, continuation: RelaunchContinuation) {
        self.updater.postpone(continuation);
    }
}
impl markraft_workspace::instance::RequestSource for crate::instance::Instance {
    fn requests(&self) -> Vec<markraft_workspace::instance::Request> {
        self.requests()
            .into_iter()
            .map(|request| match request {
                crate::instance::Request::Show => markraft_workspace::instance::Request::Show,
                crate::instance::Request::OpenPaths(paths) => {
                    markraft_workspace::instance::Request::OpenPaths(paths)
                }
            })
            .collect()
    }
}

pub fn set_app_menus(i18n: &crate::locale::I18n, cx: &gpui::App) {
    use gpui::{Menu, MenuItem};
    use markraft_workspace::app::*;
    cx.set_menus([
        Menu::new("Markraft").items([
            MenuItem::action(i18n.text("menu.show-notes"), Show),
            MenuItem::action(i18n.text("menu.settings"), Settings),
            #[cfg(feature = "direct-distribution")]
            MenuItem::action(i18n.text("menu.updates"), CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action(i18n.text("menu.quit"), Quit),
        ]),
        Menu::new(i18n.text("menu.file")).items([
            MenuItem::action(i18n.text("command.new-note"), NewNote),
            MenuItem::action(i18n.text("command.browse-notes"), Browse),
            MenuItem::action(i18n.text("command.save-now"), Save),
            MenuItem::action(i18n.text("command.open-markdown"), OpenMarkdown),
            MenuItem::action(i18n.text("command.export-markdown"), Export),
            MenuItem::action(i18n.text("command.export-html"), ExportHtml),
            MenuItem::action(i18n.text("command.export-pdf"), ExportPdf),
            MenuItem::separator(),
            MenuItem::action(i18n.text("command.print"), Print),
        ]),
        Menu::new(i18n.text("menu.edit")).items([
            MenuItem::action(i18n.text("command.undo"), markraft_gpui::Undo),
            MenuItem::action(i18n.text("command.redo"), markraft_gpui::Redo),
            MenuItem::separator(),
            MenuItem::action(i18n.text("menu.cut"), markraft_gpui::Cut),
            MenuItem::action(i18n.text("menu.copy"), markraft_gpui::Copy),
            MenuItem::action(i18n.text("menu.paste"), markraft_gpui::Paste),
            MenuItem::action(
                i18n.text("command.paste-as-plain-text"),
                markraft_gpui::PastePlain,
            ),
            MenuItem::action(
                i18n.text("command.paste-as-markdown"),
                markraft_gpui::PasteMarkdown,
            ),
            MenuItem::action(i18n.text("menu.select-all"), markraft_gpui::SelectAll),
            MenuItem::separator(),
            MenuItem::action(i18n.text("menu.find"), Find),
            MenuItem::action(i18n.text("menu.find-next"), FindNext),
            MenuItem::action(i18n.text("menu.find-previous"), FindPrevious),
        ]),
    ]);
}

/// Keep an accepted quit request while a save or directory operation completes.
/// Storage failures are returned to the host instead of being retried forever.
pub fn close_when_ready(
    view: &mut markraft_workspace::WorkspaceView,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<markraft_workspace::WorkspaceView>,
    done: CloseCompletion,
) {
    view.prepare_close(window, cx, move |result, window, cx| {
        if matches!(result, Err(markraft_workspace::WorkspaceError::Busy)) {
            cx.spawn_in(window, async move |view, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let _ = cx.update(|window, cx| {
                    view.update(cx, |view, cx| close_when_ready(view, window, cx, done))
                });
            })
            .detach();
        } else {
            done(result, window, cx);
        }
    });
}

type CloseCompletion = Box<
    dyn FnOnce(
        Result<(), markraft_workspace::WorkspaceError>,
        &mut gpui::Window,
        &mut gpui::Context<markraft_workspace::WorkspaceView>,
    ),
>;
