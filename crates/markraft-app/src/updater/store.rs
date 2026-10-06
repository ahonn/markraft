use crate::locale::Message;

/// The App Store manages updates, so no relaunch can originate here.
pub enum RelaunchContinuation {}

pub fn resume(continuation: RelaunchContinuation) {
    match continuation {}
}

pub struct Updater;

impl Updater {
    pub fn new() -> Self {
        Self
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self
    }

    pub fn take_startup_error(&mut self) -> Option<Message> {
        None
    }
    pub fn take_relaunch(&self) -> Option<RelaunchContinuation> {
        None
    }
    pub fn postpone(&self, continuation: RelaunchContinuation) {
        match continuation {}
    }
    pub fn check(&self) -> Result<(), Message> {
        Err(Message::new("error.updates-unconfigured"))
    }
}
