use crate::locale::Message;
pub type RelaunchContinuation = Box<dyn FnOnce()>;
pub fn resume(continuation: RelaunchContinuation) {
    continuation();
}
pub trait UpdateServices {
    fn take_startup_error(&mut self) -> Option<Message> {
        None
    }
    fn check(&self) -> Result<(), Message> {
        Err(Message::new("error.updates-unconfigured"))
    }
    fn automatically_checks(&self) -> Option<bool> {
        None
    }
    fn set_automatically_checks(&self, _: bool) -> Result<(), Message> {
        Ok(())
    }
    fn take_relaunch(&self) -> Option<RelaunchContinuation> {
        None
    }
    fn postpone(&self, _: RelaunchContinuation) {}
}
pub struct Disabled;
impl UpdateServices for Disabled {}
pub type Updater = Box<dyn UpdateServices>;
