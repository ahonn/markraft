//! Paper from a page of HTML: WebKit lays the page out and AppKit's printing
//! paginates it, for both the print panel and a PDF written without one.
//!
//! A web view is made for each job and released when it ends, so WebKit's
//! processes run only while something is printing. The job runs as a sheet on
//! the note's window: `NSPrintOperation::runOperation` never returns for a web
//! view's operation, and a modal run without a window prints blank pages.

use crate::locale::Message;
use futures_channel::oneshot;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSPrintInfo, NSPrintJobSavingURL, NSPrintOperation, NSPrintSaveJob, NSPrintingPaginationMode,
    NSView, NSWindow,
};
use objc2_foundation::{NSError, NSPoint, NSRect, NSString, NSURL};
use objc2_web_kit::{WKNavigation, WKNavigationDelegate, WKWebView, WKWebViewConfiguration};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;

/// Where a print job goes.
pub(crate) enum Destination {
    /// The system print panel, where the person picks a printer or saves a PDF.
    Panel,
    /// A PDF at this path, written without a panel.
    Pdf(PathBuf),
}

/// `Ok(true)` when the job printed or saved, `Ok(false)` when the person
/// cancelled the panel, and the web view's error when the page did not load.
pub(crate) type Outcome = Result<bool, String>;

/// Where a job is. Each step happens once: a late navigation callback after the
/// job ended, or a second one while printing, is ignored.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Loading,
    Printing,
    Done,
}

struct Job {
    window: Retained<NSWindow>,
    web: Option<Retained<WKWebView>>,
    destination: Destination,
    stage: Stage,
    done: Option<oneshot::Sender<Outcome>>,
}

/// How long the page may take to load before the job gives up. Pictures on
/// the web are part of the load; a stalled one must not hold a web view for
/// ever. Once printing starts the person is in charge, and nothing times out.
const LOAD_TIMEOUT_SECONDS: f64 = 60.;

thread_local! {
    // A web view holds its navigation delegate weakly; running jobs live here
    // until they finish.
    static RUNNING: RefCell<Vec<Retained<Printer>>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Job>]
    struct Printer;

    unsafe impl NSObjectProtocol for Printer {}

    unsafe impl WKNavigationDelegate for Printer {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web: &WKWebView, _navigation: Option<&WKNavigation>) {
            if self.advance(Stage::Loading, Stage::Printing) {
                self.print(web);
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _web: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.finish(Err(error.localizedDescription().to_string()));
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(
            &self,
            _web: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.finish(Err(error.localizedDescription().to_string()));
        }
    }

    impl Printer {
        #[unsafe(method(printOperationDidRun:success:contextInfo:))]
        fn did_run(&self, _operation: &NSPrintOperation, success: bool, _context: *mut c_void) {
            self.finish(Ok(success));
        }

        #[unsafe(method(loadTimedOut))]
        fn load_timed_out(&self) {
            let loading = self.ivars().borrow().stage == Stage::Loading;
            if loading {
                let web = self.ivars().borrow().web.clone();
                if let Some(web) = web {
                    unsafe { web.stopLoading() };
                }
                self.finish(Err("the page took too long to load".into()));
            }
        }
    }
);

impl Printer {
    /// Move from `from` to `to`, reporting whether the job was at `from`.
    fn advance(&self, from: Stage, to: Stage) -> bool {
        let mut job = self.ivars().borrow_mut();
        let current = job.stage == from;
        if current {
            job.stage = to;
        }
        current
    }

    fn print(&self, web: &WKWebView) {
        let job = self.ivars().borrow();
        let info: Retained<NSPrintInfo> =
            unsafe { msg_send![&*NSPrintInfo::sharedPrintInfo(), copy] };
        info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
        info.setVerticalPagination(NSPrintingPaginationMode::Automatic);
        if let Destination::Pdf(path) = &job.destination {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            unsafe {
                info.setJobDisposition(NSPrintSaveJob);
                info.dictionary()
                    .setObject_forKey(&url, ProtocolObject::from_ref(NSPrintJobSavingURL));
            }
        }
        let operation = unsafe { web.printOperationWithPrintInfo(&info) };
        let panel = matches!(job.destination, Destination::Panel);
        operation.setShowsPrintPanel(panel);
        operation.setShowsProgressPanel(panel);
        // The operation's view has no size of its own and crashes without one.
        if let Some(view) = operation.view() {
            view.setFrame(NSRect::new(NSPoint::ZERO, info.paperSize()));
        }
        let window = job.window.clone();
        drop(job);
        unsafe {
            operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                &window,
                Some(self),
                Some(sel!(printOperationDidRun:success:contextInfo:)),
                std::ptr::null_mut(),
            );
        }
    }

    fn finish(&self, outcome: Outcome) {
        let (web, done) = {
            let mut job = self.ivars().borrow_mut();
            if job.stage == Stage::Done {
                return;
            }
            job.stage = Stage::Done;
            (job.web.take(), job.done.take())
        };
        // The load timeout has nothing left to time.
        unsafe {
            let _: () = msg_send![
                objc2::class!(NSObject),
                cancelPreviousPerformRequestsWithTarget: self,
                selector: sel!(loadTimedOut),
                object: Option::<&NSObject>::None
            ];
        }
        // The registry may hold the last reference to this job; keep it until
        // this method is done with `self`.
        let keep = RUNNING.with(|running| {
            let mut running = running.borrow_mut();
            let index = running
                .iter()
                .position(|printer| std::ptr::eq::<Printer>(&**printer, self));
            index.map(|index| running.remove(index))
        });
        if let Some(web) = web {
            unsafe { web.setNavigationDelegate(None) };
        }
        if let Some(done) = done {
            let _ = done.send(outcome);
        }
        drop(keep);
    }
}

/// Lay `html` out and send it to `destination`, as a sheet on `window`. The
/// receiver answers once the job ends.
pub(crate) fn print_html(
    window: &gpui::Window,
    html: &str,
    destination: Destination,
) -> Result<oneshot::Receiver<Outcome>, Message> {
    let mtm = MainThreadMarker::new().ok_or_else(|| Message::new("error.native-window-control"))?;
    let view = super::native_view(window)?;
    // SAFETY: GPUI's view is an NSView on this thread for as long as the window lives.
    let view: &NSView = unsafe { &*view.cast::<NSView>() };
    let native = view
        .window()
        .ok_or_else(|| Message::new("error.native-window-control"))?;
    let (sender, receiver) = oneshot::channel();
    let paper = NSPrintInfo::sharedPrintInfo().paperSize();
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    let preferences = unsafe { config.preferences() };
    // Backgrounds are left out of print unless asked for (macOS 13.3 and later);
    // the print stylesheet asks too, for earlier systems.
    let prints_backgrounds: bool =
        unsafe { msg_send![&*preferences, respondsToSelector: sel!(setShouldPrintBackgrounds:)] };
    if prints_backgrounds {
        unsafe { preferences.setShouldPrintBackgrounds(true) };
    }
    let web = unsafe {
        WKWebView::initWithFrame_configuration(
            WKWebView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, paper),
            &config,
        )
    };
    let printer = Printer::alloc(mtm).set_ivars(RefCell::new(Job {
        window: native,
        web: Some(web.clone()),
        destination,
        stage: Stage::Loading,
        done: Some(sender),
    }));
    let printer: Retained<Printer> = unsafe { msg_send![super(printer), init] };
    RUNNING.with(|running| running.borrow_mut().push(printer.clone()));
    unsafe {
        web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*printer)));
        web.loadHTMLString_baseURL(&NSString::from_str(html), None);
        let _: () = msg_send![
            &*printer,
            performSelector: sel!(loadTimedOut),
            withObject: Option::<&NSObject>::None,
            afterDelay: LOAD_TIMEOUT_SECONDS
        ];
    }
    Ok(receiver)
}
