//! The log a report can be read against: one file beside the crash reports,
//! `Markraft.log`, moved aside to `Markraft.log.old` once it passes a size so
//! the two together stay small.
//!
//! Markraft's own records are kept from `info` up, and everything its
//! libraries say from `warn` up. `MARKRAFT_LOG=debug` (or any level) lowers
//! Markraft's own threshold. Records are also written to stderr when it is a
//! terminal, which is how a development build is run.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

/// Past this size the log is moved aside and a fresh one started.
const ROTATE_AT: u64 = 1024 * 1024;
const FILE_NAME: &str = "Markraft.log";
const OLD_FILE_NAME: &str = "Markraft.log.old";

/// Start logging into `directory`. A log that cannot be opened leaves only
/// stderr, which is still better than not starting.
pub fn init(directory: &Path) {
    let own = std::env::var("MARKRAFT_LOG")
        .ok()
        .and_then(|level| level.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::Info);
    let file = fs::create_dir_all(directory)
        .and_then(|()| open(&directory.join(FILE_NAME)))
        .map_err(|error| eprintln!("Markraft: the log could not be opened: {error}"))
        .ok();
    let logger = Logger {
        own,
        directory: directory.to_owned(),
        file: Mutex::new(file),
        terminal: io::stderr().is_terminal(),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(own.max(LevelFilter::Warn));
    }
}

/// The log file in `directory`.
pub fn file(directory: &Path) -> PathBuf {
    directory.join(FILE_NAME)
}

/// The last `bytes` of the log, the older file first when the current one is
/// shorter than that. Read without the logger's lock, so a panic inside the
/// logger can still attach it to its report.
pub fn tail(directory: &Path, bytes: u64) -> String {
    let (current, mut cut) = read_tail(&directory.join(FILE_NAME), bytes);
    let mut older = Vec::new();
    let wanted = bytes.saturating_sub(current.len() as u64);
    if !cut && wanted > 0 {
        (older, cut) = read_tail(&directory.join(OLD_FILE_NAME), wanted);
    }
    let mut text = String::from_utf8_lossy(&older).into_owned();
    text.push_str(&String::from_utf8_lossy(&current));
    // A cut through the middle of a record leaves half a line at the top.
    if cut && let Some(newline) = text.find('\n') {
        text.drain(..=newline);
    }
    text
}

struct Logger {
    own: LevelFilter,
    directory: PathBuf,
    file: Mutex<Option<File>>,
    terminal: bool,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let threshold = if is_own(metadata.target()) {
            self.own
        } else {
            LevelFilter::Warn
        };
        metadata.level() <= threshold
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            timestamp(SystemTime::now()),
            record.level(),
            record.target(),
            record.args()
        );
        if self.terminal {
            let _ = io::stderr().write_all(line.as_bytes());
        }
        // `try_lock`: a panic raised while the lock is held must not wait on
        // itself when its hook, or the unwinding, logs again.
        let Ok(mut file) = self.file.try_lock() else {
            return;
        };
        if let Some(handle) = file.as_mut() {
            let _ = handle.write_all(line.as_bytes());
            if handle.metadata().is_ok_and(|meta| meta.len() > ROTATE_AT) {
                *file = rotate(&self.directory);
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.try_lock()
            && let Some(handle) = file.as_mut()
        {
            let _ = handle.flush();
        }
    }
}

fn is_own(target: &str) -> bool {
    target.starts_with("markraft")
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotate(directory: &Path) -> Option<File> {
    let current = directory.join(FILE_NAME);
    let _ = fs::rename(&current, directory.join(OLD_FILE_NAME));
    open(&current).ok()
}

/// The last `bytes` of the file at `path`, and whether that left some out.
fn read_tail(path: &Path, bytes: u64) -> (Vec<u8>, bool) {
    let Ok(mut file) = File::open(path) else {
        return (Vec::new(), false);
    };
    let length = file.metadata().map_or(0, |meta| meta.len());
    let start = length.saturating_sub(bytes);
    let _ = file.seek(SeekFrom::Start(start));
    let mut out = Vec::new();
    let _ = file.take(bytes).read_to_end(&mut out);
    (out, start > 0)
}

/// `time` as `2026-09-25T08:30:00.123Z`.
fn timestamp(time: SystemTime) -> String {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = elapsed.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let of_day = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60,
        elapsed.subsec_millis()
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01, after Howard
/// Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;
    use std::time::Duration;

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap_day = UNIX_EPOCH + Duration::from_millis(951_782_400_123);
        assert_eq!(timestamp(leap_day), "2000-02-29T00:00:00.123Z");
        let later = UNIX_EPOCH + Duration::from_secs(1_790_316_054);
        assert_eq!(timestamp(later), "2026-09-25T06:00:54.000Z");
    }

    #[test]
    fn a_full_log_moves_aside_and_its_tail_spans_both_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(FILE_NAME), "old one\nold two\n").unwrap();
        let mut fresh = rotate(directory.path()).unwrap();
        fresh.write_all(b"new one\n").unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join(OLD_FILE_NAME)).unwrap(),
            "old one\nold two\n"
        );
        assert_eq!(tail(directory.path(), 1024), "old one\nold two\nnew one\n");
        // Twelve bytes reach into the middle of "old two", which is dropped.
        assert_eq!(tail(directory.path(), 12), "new one\n");
    }

    #[test]
    fn only_markraft_s_own_records_go_below_warn() {
        let logger = Logger {
            own: LevelFilter::Info,
            directory: PathBuf::new(),
            file: Mutex::new(None),
            terminal: false,
        };
        let at = |level, target| {
            logger.enabled(&Metadata::builder().level(level).target(target).build())
        };
        assert!(at(Level::Info, "markraft_app::vault"));
        assert!(!at(Level::Debug, "markraft_app::vault"));
        assert!(!at(Level::Info, "gpui::window"));
        assert!(at(Level::Warn, "gpui::window"));
    }
}
