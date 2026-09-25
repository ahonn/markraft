//! What a panic leaves behind: a report on disk for the person to send, and a
//! mark the next launch finds and mentions once.
//!
//! The report is written from inside the panic hook, so everything here is
//! best effort — a hook that fails to write says nothing and hands on to the
//! default hook, which still prints to stderr.

use std::backtrace::Backtrace;
use std::fs;
use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The file naming the newest report nobody has been told about.
const UNREPORTED: &str = "unreported";
/// How many reports are kept; older ones are removed as new ones arrive.
const KEPT: usize = 10;

/// Where reports go: `~/Library/Logs/Markraft`, or `None` without a home.
pub fn directory() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(if cfg!(feature = "updater-mock") {
        "Library/Logs/Markraft Update Test"
    } else {
        "Library/Logs/Markraft"
    }))
}

/// How much of the log a report carries: what happened just before a panic
/// usually says more than the backtrace does.
const LOG_TAIL: u64 = 32 * 1024;

/// Write a report into `directory` for every panic, on any thread, before the
/// default hook runs. The panic is logged first, so the log the report ends
/// with ends with it too.
pub fn install(directory: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let summary = summarize(info);
        log::error!("{summary}");
        log::logger().flush();
        let report = format!(
            "Markraft {}\n{summary}\n\n{}\n--- Recent log ---\n{}",
            env!("CARGO_PKG_VERSION"),
            Backtrace::force_capture(),
            crate::logging::tail(&directory, LOG_TAIL)
        );
        let _ = write_report(&directory, &report);
        default(info);
    }));
}

/// The sentence for the next launch to show, once, if a report was written
/// since the last time one was shown, and the report to reveal beside it.
pub fn take_notice(directory: &Path) -> Option<(String, PathBuf)> {
    let mark = directory.join(UNREPORTED);
    let named = fs::read(&mark).ok()?;
    let _ = fs::remove_file(&mark);
    let report = PathBuf::from(String::from_utf8_lossy(&named).into_owned());
    let report = if report.exists() {
        report
    } else {
        directory.to_owned()
    };
    Some((
        "Markraft quit unexpectedly. A report was saved.".to_owned(),
        report,
    ))
}

/// A new issue on the project's tracker, its environment field filled with
/// `debug_info`. The form asks for the steps; a report without them is a
/// backtrace nobody can act on.
pub fn new_issue_url(debug_info: &str) -> String {
    let environment: String = url::form_urlencoded::byte_serialize(debug_info.as_bytes()).collect();
    format!(
        "{}/issues/new?template=bug_report.yml&environment={environment}",
        env!("CARGO_PKG_REPOSITORY")
    )
}

/// The panic in one line and its message: which thread, where, and what.
fn summarize(info: &PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(no message)");
    let location = info
        .location()
        .map(|location| location.to_string())
        .unwrap_or_default();
    format!(
        "thread '{}' panicked at {location}:\n{message}",
        thread.name().unwrap_or("<unnamed>")
    )
}

fn write_report(directory: &Path, report: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let path = directory.join(format!("crash-{stamp}.log"));
    fs::write(&path, report)?;
    fs::write(
        directory.join(UNREPORTED),
        path.as_os_str().as_encoded_bytes(),
    )?;
    prune(directory);
    Ok(path)
}

/// Remove all but the newest [`KEPT`] reports.
fn prune(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-") && name.ends_with(".log"))
        })
        .collect();
    // The names carry the time in milliseconds, all of one width for the
    // lifetime of anyone reading them, so they sort by name as they do by age.
    reports.sort();
    let excess = reports.len().saturating_sub(KEPT);
    for path in &reports[..excess] {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_mentioned_on_the_next_launch_once() {
        let directory = tempfile::tempdir().unwrap();
        let logs = directory.path().join("Logs");
        assert_eq!(take_notice(&logs), None);
        let report = write_report(&logs, "thread 'main' panicked").unwrap();
        assert_eq!(
            fs::read_to_string(&report).unwrap(),
            "thread 'main' panicked"
        );
        let (notice, revealed) = take_notice(&logs).expect("a notice");
        assert!(notice.contains("quit unexpectedly"), "{notice}");
        assert_eq!(revealed, report);
        assert_eq!(take_notice(&logs), None);
        assert!(report.exists(), "the report outlives its notice");
    }

    #[test]
    fn an_issue_url_carries_the_environment_to_the_form() {
        let url = new_issue_url("Markraft 0.1.0\nmacOS 26.0, arm64");
        assert_eq!(
            url,
            "https://github.com/ahonn/markraft/issues/new?template=bug_report.yml\
             &environment=Markraft+0.1.0%0AmacOS+26.0%2C+arm64"
        );
    }

    #[test]
    fn only_the_newest_reports_are_kept() {
        let directory = tempfile::tempdir().unwrap();
        for stamp in 0..KEPT + 3 {
            fs::write(
                directory
                    .path()
                    .join(format!("crash-{:013}.log", 1_700_000_000_000 + stamp)),
                "",
            )
            .unwrap();
        }
        write_report(directory.path(), "newest").unwrap();
        let mut kept: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("crash-"))
            .collect();
        kept.sort();
        assert_eq!(kept.len(), KEPT);
        assert_eq!(kept[0], format!("crash-{:013}.log", 1_700_000_000_004u64));
    }
}
