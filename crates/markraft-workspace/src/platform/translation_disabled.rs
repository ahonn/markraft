use crate::locale::Message;
pub(crate) struct Translation;
pub(crate) fn available() -> bool {
    false
}
impl Translation {
    pub(crate) fn show(
        _: &gpui::Window,
        _: gpui::Bounds<gpui::Pixels>,
        _: &str,
        _: bool,
    ) -> Result<(Self, futures_channel::oneshot::Receiver<Option<String>>), Message> {
        Err(Message::new("error.native-window-control"))
    }
}
