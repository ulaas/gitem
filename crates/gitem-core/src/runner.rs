//! Spawning `git`. Every call in the app funnels through [`GitExe::run_opts`],
//! which records it in the shared command log shown in the Console view.
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub command: String,
    pub cwd: String,
    pub code: i32,
    pub millis: u128,
    pub output: String,
    pub is_write: bool,
}

pub type CommandLog = Arc<Mutex<VecDeque<LogEntry>>>;
const LOG_CAP: usize = 2000;
const LOG_OUTPUT_CAP: usize = 8192;
/// A process git left running, such as an ssh control master or a hook's background
/// job, can hold the pipes open after git exits, so reading can't wait for them to
/// close forever. These limits count from the last data received, never from the exit:
/// on a busy machine the reader may simply not have run yet (about 8 seconds was
/// measured under heavy load), and cutting it off reports an empty answer as if git had
/// said nothing.
///
/// Reads have hooks turned off (see [`READ_FLAGS`]) and their output is the answer, so
/// they wait a long time. Writes are what run hooks, and their output is only
/// informational, so they don't. A successful command's stderr is only progress and
/// warnings.
const STDERR_QUIET_OK: Duration = Duration::from_secs(2);
const STDERR_QUIET_FAILED: Duration = Duration::from_secs(10);
const STDOUT_QUIET_READ: Duration = Duration::from_secs(30);
const STDOUT_QUIET_WRITE: Duration = Duration::from_secs(3);
const STDOUT_QUIET_CANCELLED: Duration = Duration::from_millis(200);

/// Reads start without anyone asking, just by opening or looking at a repository, so they
/// never run its file system monitor, and with [`NO_HOOKS`] its hooks: `git diff` would
/// otherwise fire `post-index-change`, since it refreshes the index even with optional
/// locks off. A folder shaped like a git directory, such as one tracked inside a clone, is
/// refused as a repository unless named outright. And a read can start a lazy fetch in a
/// partial clone, which mustn't be able to run a command through an `ext::` remote.
///
/// These can't stop a filter, a textconv or a gpg program, because turning those off for
/// everyone would break Git LFS: see [`crate::trust`], and [`Neutral`] below.
const READ_FLAGS: [&str; 6] = ["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit", "-c", "protocol.ext.allow=never"];

/// A path no hook can be found under. Not a real folder on any system.
const NO_HOOKS: &str = if cfg!(windows) { "core.hooksPath=NUL" } else { "core.hooksPath=/dev/null" };

/// Variables that point git at some other repository, or change what it reads. A hook or
/// a terminal can leave them set, and gitem would then show and change that repository
/// instead of the one on screen. This is `git rev-parse --local-env-vars`, plus the ones
/// that change how paths or diffs are understood.
const FOREIGN_ENV: [&str; 21] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_EXTERNAL_DIFF",
    "GIT_GLOB_PATHSPECS",
    "GIT_GRAFT_FILE",
    "GIT_ICASE_PATHSPECS",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_LITERAL_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

#[derive(Debug, Clone)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: i32,
}

impl GitOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn text(&self) -> String {
        crate::util::text(&self.stdout)
    }
    pub fn into_result(self) -> Result<Self> {
        if self.ok() {
            Ok(self)
        } else {
            Err(Error::Git(failure_text(&self.stderr, &self.text())))
        }
    }
}

/// What to show for a failed command: stderr without progress noise, then stdout,
/// because git prints things like `CONFLICT (content): ...` there.
fn failure_text(stderr: &str, stdout: &str) -> String {
    const STDOUT_LINES: usize = 40;
    let mut out: Vec<&str> = Vec::new();
    for line in stderr.lines() {
        let last = line.rsplit('\r').map(str::trim_end).find(|l| !l.is_empty()).unwrap_or("");
        if !last.is_empty() && !is_progress(last) {
            out.push(last);
        }
    }
    let lines: Vec<&str> = stdout.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    out.extend_from_slice(&lines[lines.len().saturating_sub(STDOUT_LINES)..]);
    out.join("\n")
}

fn is_progress(line: &str) -> bool {
    let l = line.strip_prefix("remote: ").unwrap_or(line);
    ["Enumerating objects", "Counting objects", "Compressing objects", "Writing objects", "Receiving objects", "Resolving deltas", "Unpacking objects", "Delta compression", "Total "].iter().any(|p| l.starts_with(p))
}

/// Whether git failed because something holds a lock file, by its C-locale message.
pub fn is_lock_failure(stderr: &str) -> bool {
    stderr.contains(".lock': File exists") || stderr.contains("Another git process seems to be running")
}

/// Whether any git process is running on this computer. Used before offering to remove a
/// lock, where a wrong "no" could damage a repository, so not knowing counts as yes.
pub fn git_processes_running() -> bool {
    #[cfg(windows)]
    let found = {
        use std::os::windows::process::CommandExt;
        Command::new("tasklist").args(["/FI", "IMAGENAME eq git.exe", "/NH", "/FO", "CSV"]).stdin(Stdio::null()).stderr(Stdio::null()).creation_flags(0x0800_0000).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("git.exe"))
    };
    #[cfg(not(windows))]
    let found = Command::new("pgrep").args(["-x", "git"]).stdin(Stdio::null()).stderr(Stdio::null()).output().map(|o| o.status.success());
    found.unwrap_or(true)
}

/// Ends a process and whatever it started, such as ssh or a hook.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &child.id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(0x0800_0000).status();
    }
    let _ = child.kill();
}

/// Where the git binary came from, for display in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitSource {
    Configured,
    Embedded,
    System,
}

#[derive(Clone)]
pub struct GitExe {
    pub path: PathBuf,
    pub source: GitSource,
    pub log: CommandLog,
    /// This executable; used as askpass / editor / sequence-editor helper.
    pub helper: Option<PathBuf>,
    /// What to override for the repository this belongs to. Empty until [`crate::Repo::open`]
    /// has looked at its config.
    pub neutral: Arc<Neutral>,
    /// Set this and whatever this copy is running stops, and it runs nothing more. For
    /// reads whose answer nobody wants any more.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Overrides that keep a repository's own programs from running until it's trusted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Neutral {
    /// Passed as `GIT_CONFIG_KEY_n`, not `-c`, which can't name a driver with `=` in it.
    /// Submodules inherit them, and they have to.
    pub config: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    /// An empty `textconv` is a fatal error, so that one takes a flag.
    pub no_textconv: bool,
}

/// The oldest git gitem works with. 2.38 brought `merge-tree --write-tree` and
/// `rebase --update-refs`, the newest features used. The README states this too.
pub const MIN_GIT: (u32, u32) = (2, 38);

/// Major and minor from `git version 2.56.0.windows.4`.
pub fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().strip_prefix("git version ")?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Extra per-invocation options.
#[derive(Default, Clone)]
pub struct RunOpts {
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    pub is_write: bool,
    pub cancel: Option<Arc<AtomicBool>>,
    /// A write nobody asked for just now, such as a scheduled fetch, so no hooks either.
    pub no_hooks: bool,
}

impl GitExe {
    /// Resolve git: explicit setting, then a `git/` folder beside our binary
    /// (the portable "embedded" layout), then PATH, then the usual install dirs.
    pub fn discover(configured: Option<&Path>) -> Result<Self> {
        let helper = std::env::current_exe().ok();
        let log: CommandLog = Arc::new(Mutex::new(VecDeque::new()));
        let make = |path: PathBuf, source| GitExe { path, source, log: log.clone(), helper: helper.clone(), neutral: Arc::default(), cancel: None };
        if let Some(p) = configured.filter(|p| !p.as_os_str().is_empty())
            && works(p) {
                return Ok(make(p.to_path_buf(), GitSource::Configured));
            }
        let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
        if let Some(dir) = helper.as_ref().and_then(|h| h.parent()) {
            for sub in ["git/cmd", "git/bin", "mingit/cmd", "PortableGit/cmd"] {
                let p = dir.join(sub).join(exe_name);
                if p.exists() && works(&p) {
                    return Ok(make(p, GitSource::Embedded));
                }
            }
        }
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let p = dir.join(exe_name);
                if p.exists() && works(&p) {
                    return Ok(make(p, GitSource::System));
                }
            }
        }
        #[cfg(windows)]
        for root in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(r) = std::env::var_os(root) {
                let mut p = PathBuf::from(r);
                if root == "LOCALAPPDATA" {
                    p.push("Programs");
                }
                let p = p.join("Git").join("cmd").join(exe_name);
                if p.exists() && works(&p) {
                    return Ok(make(p, GitSource::System));
                }
            }
        }
        Err(Error::Other("git executable not found. Install Git, or set its path in Settings.".into()))
    }

    pub fn version(&self) -> String {
        self.run(None, &["--version"]).map(|o| o.text().trim().to_string()).unwrap_or_default()
    }

    /// What to tell the user if this git is too old for gitem. `None` when it's fine,
    /// or when its version can't be read, which isn't a reason to worry anyone.
    pub fn version_warning(&self) -> Option<String> {
        let (major, minor) = parse_git_version(&self.version())?;
        ((major, minor) < MIN_GIT).then(|| format!("This is git {major}.{minor}. gitem needs {}.{} or newer. Merge checks and updating dependent branches during a rebase will fail, and other commands may too.", MIN_GIT.0, MIN_GIT.1))
    }

    /// Read-only helper. Returns the raw output with non-zero exit codes intact.
    pub fn run_raw(&self, cwd: Option<&Path>, args: &[&str]) -> Result<GitOutput> {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.run_opts(cwd, &v, &RunOpts::default(), &mut |_| {})
    }

    /// Read-only helper. A non-zero exit becomes `Err`.
    pub fn run(&self, cwd: Option<&Path>, args: &[&str]) -> Result<GitOutput> {
        self.run_raw(cwd, args)?.into_result()
    }

    /// Full control: stdin, env, cancellation, and a callback receiving stderr
    /// lines as they arrive (`\r` counts as a line end so progress works).
    pub fn run_opts(&self, cwd: Option<&Path>, args: &[String], opts: &RunOpts, on_line: &mut dyn FnMut(&str)) -> Result<GitOutput> {
        let stop = [opts.cancel.as_ref(), self.cancel.as_ref()];
        let stopped = || stop.iter().flatten().any(|c| c.load(Ordering::Relaxed));
        if stopped() {
            return Err(Error::Other("cancelled".into()));
        }
        let start = Instant::now();
        let mut cmd = Command::new(&self.path);
        // Stable, parseable output regardless of the user's configuration.
        cmd.args(["-c", "core.quotepath=off", "-c", "color.ui=false", "-c", "core.pager=cat"]);
        cmd.args(["-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false", "-c", "log.showSignature=false", "-c", "i18n.logOutputEncoding=UTF-8"]);
        // `git config` runs nothing, and has to report what the repository really says.
        let is_config = args.first().is_some_and(|a| a == "config");
        if !opts.is_write && !is_config {
            cmd.args(READ_FLAGS);
            cmd.args(["-c", NO_HOOKS]);
        } else if opts.no_hooks {
            cmd.args(["-c", NO_HOOKS]);
        }
        cmd.args(guarded_args(args, !opts.is_write, !is_config && self.neutral.no_textconv));
        if let Some(c) = cwd {
            cmd.current_dir(c);
        } else if !opts.is_write {
            // Whatever folder gitem was started in isn't a repository anyone asked it to read.
            cmd.current_dir(std::env::temp_dir());
        }
        for name in FOREIGN_ENV {
            cmd.env_remove(name);
        }
        cmd.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // gitem reads git's messages, so they have to be in English. Reads run nothing of
        // the user's, so they can simply use the C locale. Writes run hooks, which should
        // see the user's own locale: a C locale breaks scripts handling non-ASCII text.
        if opts.is_write {
            cmd.env("LANGUAGE", "C");
        } else {
            cmd.env("LC_ALL", "C");
        }
        cmd.env("GIT_PAGER", "cat");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        if !opts.is_write {
            // Do not take the index lock for background reads (e.g. status).
            cmd.env("GIT_OPTIONAL_LOCKS", "0");
        }
        if let Some(h) = &self.helper {
            // Prompts and editors from git become small gitem windows.
            let h_str = h.to_string_lossy().replace('\\', "/");
            cmd.env("GITEM_ASKPASS", "1");
            cmd.env("GIT_ASKPASS", h);
            cmd.env("SSH_ASKPASS", h);
            cmd.env("SSH_ASKPASS_REQUIRE", "prefer");
            cmd.env("GIT_EDITOR", format!("\"{h_str}\" --edit-file"));
            cmd.env("GIT_SEQUENCE_EDITOR", format!("\"{h_str}\" --rebase-editor"));
        }
        if !is_config && !self.neutral.config.is_empty() {
            cmd.env("GIT_CONFIG_COUNT", self.neutral.config.len().to_string());
            for (i, (k, v)) in self.neutral.config.iter().enumerate() {
                cmd.env(format!("GIT_CONFIG_KEY_{i}"), k);
                cmd.env(format!("GIT_CONFIG_VALUE_{i}"), v);
            }
        }
        if !is_config {
            for (k, v) in &self.neutral.env {
                cmd.env(k, v);
            }
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let display = format!("git {}", args.iter().map(|a| display_arg(a)).collect::<Vec<_>>().join(" "));
        let mut child = cmd.spawn().map_err(|e| Error::Other(format!("failed to start {}: {e}", self.path.display())))?;

        if let Some(data) = &opts.stdin
            && let Some(mut si) = child.stdin.take() {
                let data = data.clone();
                std::thread::spawn(move || {
                    let _ = si.write_all(&data);
                });
            }
        let mut so = child.stdout.take().expect("stdout is piped");
        let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::default();
        let out_shared = stdout_buf.clone();
        let out_t = std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match so.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut v) = out_shared.lock() {
                            v.extend_from_slice(&buf[..n]);
                        }
                    }
                }
            }
        });
        let mut se = child.stderr.take().expect("stderr is piped");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let err_t = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match se.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let mut stderr_all = Vec::new();
        let mut line = Vec::new();
        let mut cancelled = false;
        let mut stop_reading_at: Option<Instant> = None;
        let mut stderr_quiet = STDERR_QUIET_OK;
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(chunk) => {
                    if stop_reading_at.is_some() {
                        stop_reading_at = Some(Instant::now() + stderr_quiet);
                    }
                    stderr_all.extend_from_slice(&chunk);
                    for b in chunk {
                        if b == b'\n' || b == b'\r' {
                            if !line.is_empty() {
                                on_line(&String::from_utf8_lossy(&line));
                                line.clear();
                            }
                        } else {
                            line.push(b);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !cancelled && stopped() {
                cancelled = true;
                kill_tree(&mut child);
            }
            if stop_reading_at.is_none()
                && let Ok(Some(status)) = child.try_wait()
            {
                // The message is the point of a failure, unless it was cancelled.
                stderr_quiet = if status.success() || cancelled { STDERR_QUIET_OK } else { STDERR_QUIET_FAILED };
                stop_reading_at = Some(Instant::now() + stderr_quiet);
            }
            if stop_reading_at.is_some_and(|t| Instant::now() >= t) {
                break;
            }
        }
        if !line.is_empty() {
            on_line(&String::from_utf8_lossy(&line));
        }
        drop(err_t);
        let status = child.wait()?;
        // Stdout may be held open the same way. Normally the reader finishes at once.
        let quiet = if cancelled {
            STDOUT_QUIET_CANCELLED
        } else if opts.is_write {
            STDOUT_QUIET_WRITE
        } else {
            STDOUT_QUIET_READ
        };
        let mut seen = 0;
        let mut last_data = Instant::now();
        while !out_t.is_finished() && last_data.elapsed() < quiet {
            std::thread::sleep(Duration::from_millis(1));
            let len = stdout_buf.lock().map_or(seen, |v| v.len());
            if len != seen {
                seen = len;
                last_data = Instant::now();
            }
        }
        // Taken, not copied: a big blame is hundreds of megabytes.
        let stdout = stdout_buf.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        let code = status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&stderr_all).into_owned();

        if let Ok(mut log) = self.log.lock() {
            let mut output = if opts.is_write {
                let mut s = String::from_utf8_lossy(&stdout).into_owned();
                s.push_str(&stderr);
                s
            } else if code != 0 {
                stderr.clone()
            } else {
                String::new()
            };
            if output.len() > LOG_OUTPUT_CAP {
                let mut end = LOG_OUTPUT_CAP;
                while !output.is_char_boundary(end) {
                    end -= 1;
                }
                output.truncate(end);
            }
            log.push_back(LogEntry {
                command: display,
                cwd: cwd.map(|c| c.display().to_string()).unwrap_or_default(),
                code,
                millis: start.elapsed().as_millis(),
                output,
                is_write: opts.is_write,
            });
            while log.len() > LOG_CAP {
                log.pop_front();
            }
        }
        if cancelled {
            return Err(Error::Other("cancelled".into()));
        }
        Ok(GitOutput { stdout, stderr, code })
    }
}

/// The command line after git's own options. Reads never use an external diff: gitem has to
/// parse the result, and `git diff`, unlike `show` and `log`, would run one by default.
fn guarded_args(args: &[String], is_read: bool, no_textconv: bool) -> Vec<String> {
    let mut out = args.to_vec();
    let sub = args.first().map(String::as_str);
    if no_textconv && matches!(sub, Some("diff" | "show" | "log" | "blame")) {
        out.insert(1, "--no-textconv".to_string());
    }
    if is_read && sub == Some("diff") {
        out.insert(1, "--no-ext-diff".to_string());
    }
    out
}

fn works(p: &Path) -> bool {
    let mut c = Command::new(p);
    c.arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.status().map(|s| s.success()).unwrap_or(false)
}

fn display_arg(a: &str) -> String {
    let a = crate::util::mask_url_credentials(a);
    if a.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_line_of_each_platform() {
        assert_eq!(parse_git_version("git version 2.56.0.windows.4\n"), Some((2, 56)));
        assert_eq!(parse_git_version("git version 2.39.5 (Apple Git-154)"), Some((2, 39)));
        assert_eq!(parse_git_version("git version 2.38.0"), Some((2, 38)));
        assert_eq!(parse_git_version("something else"), None);
        assert_eq!(parse_git_version(""), None);
    }

    #[test]
    fn a_lock_failure_is_told_from_other_failures() {
        assert!(is_lock_failure("fatal: Unable to create 'C:/r/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository"));
        assert!(!is_lock_failure("fatal: not a git repository"));
        assert!(!is_lock_failure("error: Your local changes would be overwritten by merge"));
    }

    #[test]
    fn reads_never_use_an_external_diff_and_only_guarded_repos_lose_textconv() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(guarded_args(&v(&["diff", "HEAD"]), true, false), v(&["diff", "--no-ext-diff", "HEAD"]));
        assert_eq!(guarded_args(&v(&["diff", "HEAD"]), false, false), v(&["diff", "HEAD"]));
        assert_eq!(guarded_args(&v(&["show", "HEAD"]), true, true), v(&["show", "--no-textconv", "HEAD"]));
        assert_eq!(guarded_args(&v(&["blame", "--", "a"]), true, true), v(&["blame", "--no-textconv", "--", "a"]));
        assert_eq!(guarded_args(&v(&["status"]), true, true), v(&["status"]));
    }

    #[test]
    fn the_boundary_is_at_the_minimum() {
        assert!((2, 37) < MIN_GIT);
        assert!((2, 38) >= MIN_GIT);
        assert!((3, 0) > MIN_GIT);
        assert!((1, 99) < MIN_GIT);
    }
}
