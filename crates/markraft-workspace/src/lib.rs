//! Embeddable GPUI note workspace. Process services are supplied by the host.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub use markraft_notes::{daily, doc, fs, locale, persistence, storage, vault};
pub mod app;
mod export;
pub mod file_access;
pub mod host;
pub mod platform;
mod remote_images;
mod send;
pub use app::WorkspaceView;
pub use host::{SaveReceipt, WorkspaceError, WorkspaceEvent, WorkspaceOptions};
struct WorkspaceBindings;
impl gpui::Global for WorkspaceBindings {}
pub fn bind_workspace_keys(cx: &mut gpui::App) {
    if cx.try_global::<WorkspaceBindings>().is_some() {
        return;
    }
    cx.set_global(WorkspaceBindings);
    #[cfg(feature = "bundled-settings")]
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

pub mod instance;
pub mod updater;

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
