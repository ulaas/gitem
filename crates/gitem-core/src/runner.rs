//! Spawning `git`. Every call in the app funnels through [`GitExe::run_opts`],
//! which records it in the shared command log shown in the Console view.
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
            let msg = if self.stderr.trim().is_empty() { self.text() } else { self.stderr.clone() };
            Err(Error::Git(msg.trim().to_string()))
        }
    }
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
}

/// Extra per-invocation options.
#[derive(Default, Clone)]
pub struct RunOpts {
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    pub is_write: bool,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl GitExe {
    /// Resolve git: explicit setting, then a `git/` folder beside our binary
    /// (the portable "embedded" layout), then PATH, then the usual install dirs.
    pub fn discover(configured: Option<&Path>) -> Result<Self> {
        let helper = std::env::current_exe().ok();
        let log: CommandLog = Arc::new(Mutex::new(VecDeque::new()));
        let make = |path: PathBuf, source| GitExe { path, source, log: log.clone(), helper: helper.clone() };
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
        let start = Instant::now();
        let mut cmd = Command::new(&self.path);
        // Stable, parseable output regardless of the user's configuration.
        cmd.args(["-c", "core.quotepath=off", "-c", "color.ui=false", "-c", "core.pager=cat"]);
        cmd.args(["-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false", "-c", "log.showSignature=false", "-c", "i18n.logOutputEncoding=UTF-8"]);
        if !opts.is_write {
            // Background reads run just by opening a repository. Its config could
            // otherwise name a program to execute here, so that hook is disabled.
            cmd.args(["-c", "core.fsmonitor=false"]);
        }
        cmd.args(args);
        if let Some(c) = cwd {
            cmd.current_dir(c);
        }
        cmd.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env("LC_ALL", "C");
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
        let out_t = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = so.read_to_end(&mut v);
            v
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
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(chunk) => {
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
            if !cancelled && opts.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                cancelled = true;
                let _ = child.kill();
            }
        }
        if !line.is_empty() {
            on_line(&String::from_utf8_lossy(&line));
        }
        let _ = err_t.join();
        let stdout = out_t.join().unwrap_or_default();
        let status = child.wait()?;
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
    if a.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}
