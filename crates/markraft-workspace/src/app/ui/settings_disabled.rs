use super::*;
#[derive(Default, Clone)]
pub(in crate::app) struct SettingsErrors {
    pub(in crate::app) shortcuts: [Option<Message>; 3],
}
pub(in crate::app) fn bind_keys(_: &mut App) {}
impl WorkspaceView {
    pub(in crate::app) fn open_settings(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(crate::host::WorkspaceEvent::OpenSettings);
    }
}
