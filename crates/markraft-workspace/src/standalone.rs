//! What only the standalone Markraft application asks of the workspace view: to
//! be built over the application's own window services, and to be shown, hidden
//! and spoken through as its menu bar and shortcuts ask.
use crate::{
    WorkspaceView,
    file_access::FileAccess,
    instance::Instance,
    locale::Message,
    platform::Platform,
    storage::{Library, Preferences},
    updater::Updater,
    vault::Store,
};
use gpui::{Context, Window};
use std::path::PathBuf;

pub trait Standalone: Sized {
    /// The view of the application's one window. `platform` is `None` where
    /// there is no menu bar, shortcut or native window to drive.
    #[allow(clippy::too_many_arguments)]
    fn standalone(
        path: Option<PathBuf>,
        settings_path: PathBuf,
        store: Option<Store>,
        library: Library,
        preferences: Preferences,
        error: Option<Message>,
        platform: Option<Result<Platform, Message>>,
        updater: Updater,
        instance: Instance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self;
    /// Keep the folder grants that the application restored at launch, for every
    /// worker and reader that the view starts.
    fn with_file_access(self, file_access: FileAccess) -> Self;
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>);
    /// Say something found on the way to the first window.
    fn announce(&mut self, text: Message, cx: &mut Context<Self>);
    /// [`Standalone::announce`], with a button that shows the file at `path`.
    fn announce_with_reveal(&mut self, text: Message, path: PathBuf, cx: &mut Context<Self>);
    fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>);
    /// Open the Settings window, or bring the open one forward.
    fn open_settings_window(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

impl Standalone for WorkspaceView {
    fn standalone(
        path: Option<PathBuf>,
        settings_path: PathBuf,
        store: Option<Store>,
        library: Library,
        preferences: Preferences,
        error: Option<Message>,
        platform: Option<Result<Platform, Message>>,
        updater: Updater,
        instance: Instance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        WorkspaceView::new(
            path,
            settings_path,
            store,
            library,
            preferences,
            error,
            platform,
            updater,
            instance,
            window,
            cx,
        )
    }
    fn with_file_access(self, file_access: FileAccess) -> Self {
        WorkspaceView::with_file_access(self, file_access)
    }
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        WorkspaceView::show(self, window, cx)
    }
    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        WorkspaceView::hide(self, window, cx)
    }
    fn announce(&mut self, text: Message, cx: &mut Context<Self>) {
        WorkspaceView::announce(self, text, cx)
    }
    fn announce_with_reveal(&mut self, text: Message, path: PathBuf, cx: &mut Context<Self>) {
        WorkspaceView::announce_with_reveal(self, text, path, cx)
    }
    fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        WorkspaceView::check_for_updates(self, window, cx)
    }
    fn open_settings_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        WorkspaceView::open_bundled_settings(self, window, cx)
    }
}
