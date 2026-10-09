//! Embeddable GPUI note workspace. Process services are supplied by the host.
//!
//! A host mounts [`WorkspaceView`] and answers its [`WorkspaceEvent`]s. What the
//! standalone Markraft application adds, such as its menu bar window, its updater
//! and its settings window, is named only with the `unstable-standalone` feature.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub use markraft_notes::{EditorPreferences, daily, doc, locale};
pub(crate) use markraft_notes::{fs, persistence, storage, vault};
// These modules are private in every build. The standalone application reaches
// what it needs of them through `standalone`, which names each item. With the
// feature on, the compiler therefore reports what neither the workspace nor the
// application uses. Without it, what only the application calls reads as unused.
#[cfg_attr(not(feature = "unstable-standalone"), allow(dead_code))]
mod app;
mod export;
#[cfg_attr(not(feature = "unstable-standalone"), allow(dead_code))]
mod file_access;
mod host;
#[cfg_attr(not(feature = "unstable-standalone"), allow(dead_code))]
mod instance;
#[cfg_attr(not(feature = "unstable-standalone"), allow(dead_code))]
mod platform;
mod remote_images;
mod send;
#[cfg(feature = "unstable-standalone")]
#[doc(hidden)]
pub mod standalone;
#[cfg_attr(not(feature = "unstable-standalone"), allow(dead_code))]
mod updater;
pub use app::WorkspaceView;
pub use host::{
    PendingSave, WorkspaceError, WorkspaceEvent, WorkspaceOptions, WorkspaceSaveReceipt,
};
struct WorkspaceBindings;
impl gpui::Global for WorkspaceBindings {}
pub fn bind_workspace_keys(cx: &mut gpui::App) {
    if cx.try_global::<WorkspaceBindings>().is_some() {
        return;
    }
    cx.set_global(WorkspaceBindings);
    #[cfg(feature = "unstable-standalone")]
    gpui_base::init(cx);
    markraft_gpui::bind_keys(cx);
    markraft_gpui::use_system_pasteboard(cx);
    markraft_vim::bind_keys(cx);
    app::bind_app_keys(cx);
}
pub trait EditorLocaleExt {
    fn editor_messages(&self) -> markraft_gpui::EditorMessages;
}
impl EditorLocaleExt for locale::I18n {
    fn editor_messages(&self) -> markraft_gpui::EditorMessages {
        let i18n = self.clone();
        markraft_gpui::EditorMessages::new(move |message, args| i18n.text_with(message.key(), args))
    }
}

#[cfg(test)]
mod e2e;

#[cfg(test)]
mod locale_tests {
    #[test]
    fn editor_defaults_match_application_resources() {
        for message in markraft_gpui::EditorMessage::ALL {
            assert_eq!(
                super::locale::I18n::english().text(message.key()),
                message.english()
            );
        }
    }
}
