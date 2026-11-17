//! The relay's log: what it did and what went wrong, on stderr (the journal,
//! under systemd) and in a file per command and day in the state folder.
//!
//! Bank transactions and secrets are never in it. Messages, digests and what a
//! failed command printed can hold them, so the log says that they happened,
//! not what they said. Other crates' events are kept to warnings: at lower
//! levels the Matrix SDK describes the events it handles.

use std::fs::File;
use std::io::{self, IsTerminal as _, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use jiff::civil::Date;
use tracing::{Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::layer::SubscriberExt as _;

/// How many days of each command's log files are kept.
const KEEP_DAYS: usize = 30;

/// Starts the log for this run of `command` ("daily" or "run"), with its
/// files in `state`'s `log` folder.
pub fn start(state: &Path, command: &str) -> Result<()> {
    let files = DailyFiles::new(state.join("log"), command)?;
    let log = subscriber(files, io::stderr, io::stderr().is_terminal());
    tracing::subscriber::set_global_default(log).context("starting the log")
}

/// The log, written to `files` and to `stderr` (in colour, if it's a terminal).
fn subscriber<W>(files: DailyFiles, stderr: W, colours: bool) -> impl Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let levels = Targets::new().with_default(Level::WARN).with_target(env!("CARGO_CRATE_NAME"), Level::INFO);
    // A line that can't be written is on the other side already, or nowhere:
    // there's nobody left to tell.
    let stderr = fmt::layer().with_writer(stderr).with_ansi(colours).log_internal_errors(false);
    let file = fmt::layer().with_writer(files).with_ansi(false).log_internal_errors(false);
    tracing_subscriber::registry().with(levels).with(stderr).with(file)
}

/// The log's files, `<command>.<date>.log`: a new one each day, by the local
/// date, so the service's log doesn't grow without end, and each readable
/// only by its owner.
struct DailyFiles {
    dir: PathBuf,
    command: String,
    /// The file that's open, and the day it's for.
    open: Mutex<Option<(Date, Arc<File>)>>,
}

impl DailyFiles {
    /// Opens today's file, so that a folder that can't be written to stops the
    /// relay when it starts instead of leaving it without a log.
    fn new(dir: PathBuf, command: &str) -> Result<DailyFiles> {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let files = DailyFiles { dir, command: command.to_string(), open: Mutex::new(None) };
        files.today().with_context(|| format!("opening a log file in {}", files.dir.display()))?;
        Ok(files)
    }

    /// Today's file, opened when the day is new.
    fn today(&self) -> io::Result<Arc<File>> {
        let today = jiff::Zoned::now().date();
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((day, file)) = &*open
            && *day == today
        {
            return Ok(file.clone());
        }
        let mut options = std::fs::OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = Arc::new(options.open(self.dir.join(format!("{}.{today}.log", self.command)))?);
        *open = Some((today, file.clone()));
        // Nothing to report a failure to from in here: the log is what's
        // being written. The next day tries again.
        let _ = self.remove_old();
        Ok(file)
    }

    /// Removes this command's oldest files, so that `KEEP_DAYS` are left.
    fn remove_old(&self) -> io::Result<()> {
        let prefix = format!("{}.", self.command);
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let day = name.strip_prefix(&prefix).and_then(|rest| rest.strip_suffix(".log"));
            if day.is_some_and(|day| day.parse::<Date>().is_ok()) {
                files.push(name);
            }
        }
        // The date is written so that names sort by it.
        files.sort();
        for name in files.iter().rev().skip(KEEP_DAYS) {
            std::fs::remove_file(self.dir.join(name))?;
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for DailyFiles {
    type Writer = Line;

    fn make_writer(&'a self) -> Line {
        Line(self.today().ok())
    }
}

/// Where one line of the log goes: today's file, or nowhere if it couldn't
/// be opened (stderr still gets the line).
struct Line(Option<Arc<File>>);

impl Write for Line {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &self.0 {
            Some(file) => (&**file).write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &self.0 {
            Some(file) => (&**file).flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("konter-relay-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn todays_file(dir: &Path, command: &str) -> PathBuf {
        dir.join(format!("{command}.{}.log", jiff::Zoned::now().date()))
    }

    #[test]
    fn the_relays_events_are_written_to_todays_file_and_other_crates_only_warn() {
        let dir = scratch("levels");
        let files = DailyFiles::new(dir.clone(), "run").expect("the log folder is made");
        tracing::subscriber::with_default(subscriber(files, io::sink, false), || {
            tracing::info!(room = "!room:example.org", "Listening.");
            tracing::debug!("Too detailed.");
            tracing::info!(target: "matrix_sdk::sync", "An event the SDK describes.");
            tracing::warn!(target: "matrix_sdk::sync", "The SDK's warning.");
        });
        let path = todays_file(&dir, "run");
        let log = std::fs::read_to_string(&path).expect("today's file is read");
        assert!(log.contains("Listening.") && log.contains("!room:example.org"), "{log}");
        assert!(log.contains("The SDK's warning."), "{log}");
        assert!(!log.contains("Too detailed.") && !log.contains("the SDK describes"), "{log}");
        assert_eq!(log.lines().count(), 2, "{log}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).expect("the file has metadata").permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).expect("the scratch folder is removed");
    }

    #[test]
    fn a_new_run_adds_to_the_days_file() {
        let dir = scratch("append");
        for line in ["The first run.", "The second run."] {
            let files = DailyFiles::new(dir.clone(), "daily").expect("the log folder is made");
            tracing::subscriber::with_default(subscriber(files, io::sink, false), || tracing::info!("{line}"));
        }
        let log = std::fs::read_to_string(todays_file(&dir, "daily")).expect("today's file is read");
        assert!(log.contains("The first run.") && log.contains("The second run."), "{log}");
        std::fs::remove_dir_all(&dir).expect("the scratch folder is removed");
    }

    #[test]
    fn old_files_are_removed_and_other_files_left_alone() {
        let dir = scratch("old");
        std::fs::create_dir_all(&dir).expect("the scratch folder is made");
        let day = |n: i8| format!("2001-{:02}-{:02}", 1 + n / 28, 1 + n % 28);
        for n in 0..40 {
            std::fs::write(dir.join(format!("run.{}.log", day(n))), "").expect("an old file is written");
        }
        let others = ["daily.2001-01-01.log", "run.notes.log", "run.2001-01-01.txt"];
        for other in others {
            std::fs::write(dir.join(other), "").expect("another file is written");
        }
        // Opening today's file removes the old ones.
        DailyFiles::new(dir.clone(), "run").expect("the log folder is made");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("the folder is read")
            .map(|entry| entry.expect("the entry is read").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        // Today's and the newest before it: the 40 written are all older.
        assert_eq!(left.len() - others.len(), KEEP_DAYS, "{left:?}");
        assert!(!left.contains(&format!("run.{}.log", day(0))), "{left:?}");
        assert!(left.contains(&format!("run.{}.log", day(39))), "{left:?}");
        for other in others {
            assert!(left.contains(&other.to_string()), "{left:?}");
        }
        std::fs::remove_dir_all(&dir).expect("the scratch folder is removed");
    }
}
