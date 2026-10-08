use crate::locale::Message;

use markraft_workspace::updater::RelaunchContinuation;

pub struct Updater;

impl Updater {
    pub fn new() -> Self {
        Self
    }

    pub fn take_startup_error(&mut self) -> Option<Message> {
        None
    }
    pub fn take_relaunch(&self) -> Option<RelaunchContinuation> {
        None
    }
    pub fn postpone(&self, continuation: RelaunchContinuation) {
        drop(continuation);
    }
    pub fn check(&self) -> Result<(), Message> {
        Err(Message::new("error.updates-unconfigured"))
    }
}
