//! `duckcall run`: follows Handy's log and ducks the output volume while Handy records.

use crate::{Result, platform};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Output volume while Handy records, 0-100.
pub fn level() -> Result<u8> {
    parse_level(std::env::var("DUCK_VOLUME").ok().as_deref())
}

fn parse_level(level: Option<&str>) -> Result<u8> {
    let Some(level) = level else { return Ok(5) };
    level.parse().ok().filter(|level| *level <= 100).ok_or_else(|| "DUCK_VOLUME must be a number from 0 to 100".into())
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_signal: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn run() -> Result<()> {
    let level = level()?;
    // The service may be stopped in the middle of a dictation; nothing may stay ducked or muted.
    unsafe {
        let handler = stop as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let mut duck = Duck { before: None };
    let mut tail = Tail::new(platform::handy_log());
    while !STOP.load(Ordering::Relaxed) {
        match tail.next() {
            Some(line) => duck.line(&line, || platform::duck(level), platform::restore),
            // ponytail: polls; kqueue/inotify if 20 wakeups a second ever matter.
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    if let Some(before) = duck.before {
        platform::restore(before);
    }
    Ok(())
}

/// Holds what ducking changed (`platform::duck`), so repeated starts and stops change nothing.
struct Duck<T> {
    before: Option<T>,
}

impl<T> Duck<T> {
    // Handy's own log is the trigger, so every Handy shortcut and activation mode is covered.
    // These are DEBUG lines: Handy's log_level must stay "debug" (its default; on macOS install sets it).
    fn line(&mut self, line: &str, duck: impl FnOnce() -> T, restore: impl FnOnce(T)) {
        if line.contains("TranscribeAction::start called") {
            if self.before.is_none() {
                self.before = Some(duck());
            }
        } else if (line.contains("TranscribeAction::stop called") || line.contains("Operation cancellation completed"))
            && let Some(before) = self.before.take()
        {
            restore(before);
        }
    }
}

/// `tail -F`: yields the lines appended to a file, across truncation and rotation.
struct Tail {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    line: Vec<u8>,
}

impl Tail {
    /// Starts at the current end of the file; a file that appears later is read from its start.
    fn new(path: PathBuf) -> Self {
        let reader = File::open(&path).ok().and_then(|mut file| {
            file.seek(SeekFrom::End(0)).ok()?;
            Some(BufReader::new(file))
        });
        Tail { path, reader, line: Vec::new() }
    }

    /// The next complete line, or `None` if there is none yet.
    fn next(&mut self) -> Option<String> {
        if self.reader.is_none() {
            self.reader = File::open(&self.path).ok().map(BufReader::new);
        }
        let reader = self.reader.as_mut()?;
        if reader.read_until(b'\n', &mut self.line).unwrap_or(0) > 0 && self.line.ends_with(b"\n") {
            let line = String::from_utf8_lossy(&self.line).into_owned();
            self.line.clear();
            return Some(line);
        }
        // At the end: an unfinished line stays buffered. Reopen if the file was replaced or truncated.
        let replaced = match (fs::metadata(&self.path), reader.get_ref().metadata()) {
            (Ok(now), Ok(open)) => now.ino() != open.ino() || now.len() < reader.stream_position().unwrap_or(0),
            _ => true,
        };
        if replaced {
            self.reader = None;
            self.line.clear();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;

    const STARTED: &str = "[handy_app_lib::actions][DEBUG] TranscribeAction::start called for binding: transcribe";
    const STOPPED: &str = "[handy_app_lib::actions][DEBUG] TranscribeAction::stop called for binding: transcribe";
    const CANCELLED: &str = "[handy_app_lib::utils][INFO] Operation cancellation completed - returned to idle state";

    #[test]
    fn level_defaults_to_5_and_accepts_0_to_100_only() {
        assert_eq!(parse_level(None).unwrap(), 5);
        assert_eq!(parse_level(Some("0")).unwrap(), 0);
        assert_eq!(parse_level(Some("100")).unwrap(), 100);
        for bad in ["101", "-1", "5%", "", "loud"] {
            assert!(parse_level(Some(bad)).is_err(), "{bad:?} must be rejected");
        }
    }

    /// Feeds log lines to a `Duck` whose output reports the given volume; returns the volume changes it made.
    /// What restoring means for an output without a volume control is up to `platform::restore`.
    fn changes(lines: &[(&str, u8)]) -> Vec<String> {
        let calls = RefCell::new(Vec::new());
        let mut duck = Duck { before: None };
        for &(line, volume) in lines {
            duck.line(
                line,
                || {
                    calls.borrow_mut().push("duck".to_string());
                    volume
                },
                |volume| calls.borrow_mut().push(format!("restore {volume}")),
            );
        }
        calls.into_inner()
    }

    #[test]
    fn ducks_once_and_restores_the_volume_from_before() {
        let calls = changes(&[
            (STARTED, 50),
            (STARTED, 5), // must not take the ducked volume as "before"
            (STOPPED, 0),
            (STOPPED, 0),
        ]);
        assert_eq!(calls, ["duck", "restore 50"]);
    }

    #[test]
    fn a_cancelled_recording_restores_the_volume_too() {
        assert_eq!(changes(&[(STARTED, 30), (CANCELLED, 0)]), ["duck", "restore 30"]);
    }

    #[test]
    fn a_stop_without_a_start_and_other_lines_change_nothing() {
        let calls = changes(&[
            (STOPPED, 50),
            (CANCELLED, 50),
            ("[handy_app_lib::tray][DEBUG] tray apply: icon=resources/tray_recording_dark.png", 50),
        ]);
        assert!(calls.is_empty(), "{calls:?}");
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("duckcall-tail-{name}"));
        fs::remove_file(&path).ok();
        path
    }

    #[test]
    fn tail_follows_appends_unfinished_lines_and_rotation() {
        let path = scratch("rotation");
        fs::write(&path, "old\n").unwrap();
        let mut tail = Tail::new(path.clone());
        assert_eq!(tail.next(), None); // starts at the end: "old" is not replayed
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "one\ntw").unwrap();
        assert_eq!(tail.next().as_deref(), Some("one\n"));
        assert_eq!(tail.next(), None); // "tw" is not a line yet
        writeln!(file, "o").unwrap();
        assert_eq!(tail.next().as_deref(), Some("two\n"));
        fs::remove_file(&path).unwrap(); // rotation: a new file under the same name
        fs::write(&path, "three\n").unwrap();
        assert_eq!(tail.next(), None); // notices the replacement
        assert_eq!(tail.next().as_deref(), Some("three\n"));
    }

    #[test]
    fn tail_reads_a_file_that_appears_later_from_its_start() {
        let path = scratch("later");
        let mut tail = Tail::new(path.clone());
        assert_eq!(tail.next(), None);
        fs::write(&path, "first\n").unwrap();
        assert_eq!(tail.next().as_deref(), Some("first\n"));
    }

    #[test]
    fn tail_starts_over_when_the_file_is_truncated() {
        let path = scratch("truncated");
        fs::write(&path, "a long first line\n").unwrap();
        let mut tail = Tail::new(path.clone());
        fs::write(&path, "new\n").unwrap(); // the same file, now shorter than where the tail stands
        assert_eq!(tail.next(), None); // notices the truncation
        assert_eq!(tail.next().as_deref(), Some("new\n"));
    }
}
