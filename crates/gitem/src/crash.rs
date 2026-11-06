//! Crash reports. A release build has no console, so a panic would otherwise just make
//! the window vanish. The report stays on this computer and the next launch offers it.
use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gitem_core::runner::{CommandLog, LogEntry};
use gitem_core::{settings, util};

/// Enough to see what was going on, without filling the report.
const LAST_COMMANDS: usize = 15;
/// A crash on every launch shouldn't fill the disk.
const KEEP_REPORTS: usize = 20;

static GIT_LOG: OnceLock<CommandLog> = OnceLock::new();

/// Puts the app's git commands into later reports.
pub fn watch_git_log(log: CommandLog) {
    let _ = GIT_LOG.set(log);
}

/// Writes a report on any panic, then lets the default hook print its message too.
pub fn install_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        // Not `lock`: the panicking thread may be the one holding it.
        let commands: Vec<LogEntry> = GIT_LOG.get().and_then(|l| l.try_lock().ok()).map(|l| l.iter().rev().take(LAST_COMMANDS).rev().cloned().collect()).unwrap_or_default();
        let text = report(&thread, &info.to_string(), &Backtrace::force_capture().to_string(), &commands);
        // A failure here has nowhere to go, and must not cause a second panic.
        let _ = write_report(&settings::config_dir(), util::now_ts(), &text);
        default_hook(info);
    }));
}

fn report(thread: &str, panic: &str, backtrace: &str, commands: &[LogEntry]) -> String {
    let mut out = format!("gitem crash report\nversion: {}\ntime: {}\nos: {}\nthread: {thread}\n\n{panic}\n", crate::app::version_line(), util::format_date(util::now_ts()), std::env::consts::OS);
    out.push_str("\nlast git commands, oldest first:\n");
    if commands.is_empty() {
        out.push_str("  (none recorded)\n");
    }
    for c in commands {
        out.push_str(&format!("  [{}] {} (in {})\n", c.code, c.command, c.cwd));
    }
    out.push_str(&format!("\nbacktrace:\n{backtrace}\n"));
    out
}

fn write_report(dir: &Path, when: i64, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    // Two threads can panic within a second.
    let mut path = dir.join(format!("crash-{when}.log"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("crash-{when}-{n}.log"));
        n += 1;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

fn reports_in(dir: &Path, wanted: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("crash-") && wanted(n)))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found.into_iter().map(|x| x.1).collect()
}

/// Reports nobody has seen yet, newest first. Also drops all but the latest few.
pub fn pending(dir: &Path) -> Vec<PathBuf> {
    let all = reports_in(dir, |n| n.ends_with(".log") || n.ends_with(".seen"));
    for old in all.iter().skip(KEEP_REPORTS) {
        let _ = std::fs::remove_file(old);
    }
    reports_in(dir, |n| n.ends_with(".log"))
}

/// Keeps the file, in case it's wanted later, but stops offering it.
pub fn mark_seen(report: &Path) {
    let _ = std::fs::rename(report, report.with_extension("seen"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str) -> LogEntry {
        LogEntry { command: command.into(), cwd: "C:/repo".into(), code: 1, millis: 3, output: "SHOULD NOT APPEAR".into(), is_write: true }
    }

    #[test]
    fn a_report_says_what_happened() {
        let text = report("main", "panicked at src/x.rs:1:2:\nboom", "0: frame", &[entry("git status"), entry("git push")]);
        for part in ["version: ", "thread: main", "boom", "src/x.rs:1:2", "[1] git status (in C:/repo)", "0: frame"] {
            assert!(text.contains(part), "no {part:?} in:\n{text}");
        }
        assert!(text.find("git status") < text.find("git push"), "oldest first");
        assert!(!text.contains("SHOULD NOT APPEAR"), "a command's output can hold file contents");
    }

    #[test]
    fn reports_are_offered_until_seen_and_never_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let first = write_report(d.path(), 100, "a").unwrap();
        let second = write_report(d.path(), 100, "b").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "a");
        assert_eq!(pending(d.path()).len(), 2);
        mark_seen(&first);
        assert_eq!(pending(d.path()), std::slice::from_ref(&second));
        assert!(first.with_extension("seen").exists(), "the file is kept");
        mark_seen(&second);
        assert!(pending(d.path()).is_empty());
    }

    #[test]
    fn old_reports_are_dropped() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..KEEP_REPORTS + 5 {
            write_report(d.path(), i as i64, "x").unwrap();
        }
        pending(d.path());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), KEEP_REPORTS);
    }

    #[test]
    fn a_missing_folder_means_no_reports() {
        assert!(pending(Path::new("does-not-exist-anywhere")).is_empty());
    }
}
