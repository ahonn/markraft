//! Sparkle stays on AppKit's main thread; callbacks hand work to the app's poll loop.
use objc2_foundation::{NSBundle, NSString};
use sparkle_updater::{MainThreadMarker, RelaunchContinuation, SparkleUpdater, UpdaterConfig};
use std::{cell::RefCell, path::Path, rc::Rc};

const UNBUNDLED: &str = "Updates work only in the installed app.";
const UNCONFIGURED: &str = "This copy can't update. Install a release from GitHub.";

pub struct Updater {
    // Retain the controller and delegates for the whole application lifetime.
    native: Option<SparkleUpdater>,
    unavailable: String,
    pending: Rc<RefCell<PendingRelaunch<RelaunchContinuation>>>,
    startup_error: Option<String>,
}

impl Updater {
    pub fn new() -> Self {
        let pending = Rc::new(RefCell::new(PendingRelaunch::default()));
        let mut this = Self {
            native: None,
            unavailable: UNBUNDLED.into(),
            pending,
            startup_error: None,
        };
        let Some(main_thread) = MainThreadMarker::new() else {
            this.fail("The updater must start on the macOS main thread.".into());
            return this;
        };
        let bundle = NSBundle::mainBundle();
        if cfg!(feature = "updater-mock")
            && bundle
                .bundleIdentifier()
                .as_ref()
                .map(|id| id.to_string())
                .as_deref()
                != Some("app.markraft.mac.update-test")
        {
            this.unavailable =
                "The mock updater requires the isolated update-test app bundle.".into();
            return this;
        }
        if Path::new(&bundle.bundlePath().to_string())
            .extension()
            .is_none_or(|ext| ext != "app")
        {
            return this;
        }
        let value = |key: &str| {
            bundle
                .objectForInfoDictionaryKey(&NSString::from_str(key))
                .and_then(|value| value.downcast::<NSString>().ok())
                .map(|value| value.to_string())
        };
        if !configured(
            value("SUFeedURL").as_deref(),
            value("SUPublicEDKey").as_deref(),
        ) {
            this.unavailable = UNCONFIGURED.into();
            return this;
        }
        let pending = this.pending.clone();
        match SparkleUpdater::new(
            main_thread,
            UpdaterConfig {
                event_callback: if cfg!(feature = "updater-mock") {
                    Some(Rc::new(|event| log::info!("mock update: {event:?}")))
                } else {
                    None
                },
                relaunch_handler: Some(Rc::new(move |_, continuation| {
                    // Never borrow GPUI from a native callback: Sparkle can call back
                    // synchronously while MarkraftApp is already being updated.
                    pending.borrow_mut().request(continuation);
                })),
                ..Default::default()
            },
        ) {
            Ok(Some(native)) => this.native = Some(native),
            Ok(None) => {}
            Err(error) => {
                log::warn!("the updater could not start: {error}");
                this.fail("Automatic updates couldn't start.".into());
            }
        }
        this
    }

    /// An updater that never checks: for the headless tests, which run off the main
    /// thread and outside any bundle.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn disabled() -> Self {
        Self {
            native: None,
            unavailable: UNBUNDLED.into(),
            pending: Rc::new(RefCell::new(PendingRelaunch::default())),
            startup_error: None,
        }
    }

    fn fail(&mut self, message: String) {
        log::warn!("{message}");
        self.unavailable = message.clone();
        self.startup_error = Some(message);
    }

    pub fn take_startup_error(&mut self) -> Option<String> {
        self.startup_error.take()
    }

    pub fn check(&self) -> Result<(), String> {
        if self.pending.borrow_mut().retry() {
            return Ok(());
        }
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| self.unavailable.clone())?;
        // Sparkle also brings an existing update dialog forward here.
        native
            .check_for_updates()
            .map_err(|error| error.to_string())
    }

    /// Whether Sparkle checks for updates on its own schedule. None where there is no
    /// updater to ask: an unbundled or unconfigured copy.
    pub fn automatically_checks(&self) -> Option<bool> {
        self.native
            .as_ref()?
            .automatically_checks_for_updates()
            .ok()
    }

    /// Sparkle keeps the answer in its own user defaults, so it outlives the app.
    pub fn set_automatically_checks(&self, enabled: bool) -> Result<(), String> {
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| self.unavailable.clone())?;
        native
            .set_automatically_checks_for_updates(enabled)
            .map_err(|error| error.to_string())
    }

    pub fn take_relaunch(&self) -> Option<RelaunchContinuation> {
        self.pending.borrow_mut().take_ready()
    }

    pub fn postpone(&self, continuation: RelaunchContinuation) {
        self.pending.borrow_mut().postpone(continuation);
    }
}

fn configured(feed: Option<&str>, public_key: Option<&str>) -> bool {
    feed.and_then(|feed| url::Url::parse(feed).ok())
        .is_some_and(|feed| {
            let secure = feed.scheme() == "https" && feed.host_str().is_some();
            let local_test = cfg!(feature = "updater-mock")
                && feed.scheme() == "http"
                && feed.host_str() == Some("127.0.0.1");
            (secure || local_test) && feed.username().is_empty() && feed.password().is_none()
        })
        && public_key.is_some_and(|key| !key.trim().is_empty())
}

/// A failed save keeps its one-shot continuation without retrying every poll.
struct PendingRelaunch<T> {
    continuation: Option<T>,
    ready: bool,
}

impl<T> Default for PendingRelaunch<T> {
    fn default() -> Self {
        Self {
            continuation: None,
            ready: false,
        }
    }
}

impl<T> PendingRelaunch<T> {
    fn request(&mut self, continuation: T) {
        self.continuation = Some(continuation);
        self.ready = true;
    }

    fn take_ready(&mut self) -> Option<T> {
        if self.ready {
            self.ready = false;
            self.continuation.take()
        } else {
            None
        }
    }

    fn postpone(&mut self, continuation: T) {
        self.continuation = Some(continuation);
        self.ready = false;
    }

    fn retry(&mut self) -> bool {
        self.ready = self.continuation.is_some();
        self.ready
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn updates_require_https_feed_and_signing_key() {
        assert!(configured(
            Some("https://updates.markraft.app/appcast.xml"),
            Some("public-key")
        ));
        assert!(!configured(None, Some("public-key")));
        assert!(!configured(Some("https://example.com/appcast.xml"), None));
        assert!(!configured(
            Some("https://example.com/appcast.xml"),
            Some("  ")
        ));
        assert!(!configured(
            Some("http://example.com/appcast.xml"),
            Some("public-key")
        ));
        assert!(!configured(Some("https://"), Some("public-key")));
    }

    #[test]
    fn failed_save_retains_relaunch_until_user_retries() {
        let mut pending = PendingRelaunch::default();
        pending.request(42);
        let continuation = pending.take_ready().unwrap();
        pending.postpone(continuation);
        assert_eq!(pending.take_ready(), None);
        assert!(pending.retry());
        assert_eq!(pending.take_ready(), Some(42));
        assert_eq!(pending.take_ready(), None);
        assert!(!pending.retry());
    }

    #[test]
    fn plain_http_is_only_allowed_on_loopback_in_mock_builds() {
        assert_eq!(
            configured(Some("http://127.0.0.1:8765/appcast.xml"), Some("key")),
            cfg!(feature = "updater-mock")
        );
        for feed in [
            "http://192.168.1.1/feed",
            "http://127.0.0.1.example.com/feed",
            "http://127.0.0.1@evil.example/feed",
            "http://user@127.0.0.1/feed",
        ] {
            assert!(!configured(Some(feed), Some("key")));
        }
    }
}
