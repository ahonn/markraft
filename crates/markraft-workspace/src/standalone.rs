//! What only the standalone Markraft application asks of the workspace view: to
//! be built over the application's own window services, and to be shown, hidden
//! and spoken through as its menu bar and shortcuts ask.
use crate::{
    WorkspaceView,
    locale::Message,
    storage::{Library, Preferences},
    vault::Store,
};
use file_access::FileAccess;
use instance::Instance;
use platform::Platform;
use updater::Updater;

/// The actions that the application's menus and shortcuts dispatch to the view.
pub mod app {
    pub use crate::app::{
        Browse, CheckForUpdates, Export, ExportHtml, ExportPdf, Find, FindNext, FindPrevious,
        NewNote, OpenMarkdown, Print, Quit, Save, Settings, Show,
    };
}
/// The folder grants that a sandboxed application restores at launch.
pub mod file_access {
    pub use crate::file_access::FileAccess;
}
/// The requests that a second launch hands to the running application.
pub mod instance {
    pub use crate::instance::{Instance, Request, RequestSource};
}
/// The window services that the application provides, and what they report.
pub mod platform {
    pub use crate::platform::{Platform, PlatformEvent, PlatformServices, Shortcut, debug_info};
}
/// The update service that the application provides.
pub mod updater {
    pub use crate::updater::{RelaunchContinuation, UpdateServices, Updater};
}
use gpui::{Context, Window};
use std::path::PathBuf;

/// What the application hands over to build its view from.
pub struct StandaloneParts {
    /// The notes folder, when one could be opened.
    pub path: Option<PathBuf>,
    pub settings_path: PathBuf,
    /// `None` when the folder could not be opened. `error` then says why.
    pub store: Option<Store>,
    pub library: Library,
    pub preferences: Preferences,
    pub error: Option<Message>,
    /// The menu bar, the shortcuts and the native window, or why they could not start.
    pub platform: Result<Platform, Message>,
    pub updater: Updater,
    pub instance: Instance,
}

pub trait Standalone: Sized {
    /// The view of the application's one window.
    fn standalone(parts: StandaloneParts, window: &mut Window, cx: &mut Context<Self>) -> Self;
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
    fn standalone(parts: StandaloneParts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = crate::app::Services::Standalone {
            platform: Some(parts.platform),
            updater: parts.updater,
            instance: parts.instance,
        };
        WorkspaceView::new(
            crate::app::Parts::over_store(
                parts.path,
                parts.settings_path,
                parts.store,
                parts.library,
                parts.preferences,
                services,
            )
            .with_error(parts.error),
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
