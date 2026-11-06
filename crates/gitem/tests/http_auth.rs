//! Real git authenticating over HTTP through gitem's credential helper and askpass
//! window. The server is `git http-backend` behind a small Basic-auth bridge on
//! 127.0.0.1, serving a bare repository in a temp folder.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gitem_core::ops::{self, PushOpts};
use gitem_core::{GitExe, Repo};

const BIN: &str = env!("CARGO_BIN_EXE_gitem");

fn base64(data: &str) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.as_bytes().chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

struct Server {
    port: u16,
    _dir: tempfile::TempDir,
    rejected: Arc<AtomicUsize>,
    accepted_as: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Serves `repo.git`, created empty, to anyone sending `user:pass`.
    fn start(user: &str, pass: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        assert!(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&bare).status().unwrap().success());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = format!("Basic {}", base64(&format!("{user}:{pass}")));
        let root = dir.path().to_path_buf();
        let rejected = Arc::new(AtomicUsize::new(0));
        let accepted_as = Arc::new(Mutex::new(Vec::new()));
        let (r, a, u) = (rejected.clone(), accepted_as.clone(), user.to_string());
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (expected, root, r, a, u) = (expected.clone(), root.clone(), r.clone(), a.clone(), u.clone());
                std::thread::spawn(move || serve(conn, &expected, &root, &r, &a, &u));
            }
        });
        Self { port, _dir: dir, rejected, accepted_as }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/repo.git", self.port)
    }
}

fn serve(conn: TcpStream, expected: &str, root: &Path, rejected: &AtomicUsize, accepted: &Mutex<Vec<String>>, user: &str) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).is_err() || l.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = l.trim().split_once(':') {
            headers.insert(k.to_lowercase(), v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if headers.get("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; n + 2];
            reader.read_exact(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    } else if let Some(n) = headers.get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.resize(n, 0);
        reader.read_exact(&mut body).unwrap();
    }
    let mut w = conn;
    if headers.get("authorization").map(String::as_str) != Some(expected) {
        if headers.contains_key("authorization") {
            rejected.fetch_add(1, Ordering::Relaxed);
        }
        let _ = w.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    accepted.lock().unwrap().push(target.clone());
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut c = Command::new("git");
    c.arg("http-backend")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REQUEST_METHOD", &method)
        .env("REMOTE_USER", user)
        .env("CONTENT_TYPE", headers.get("content-type").map_or("", String::as_str))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("HTTP_CONTENT_ENCODING", headers.get("content-encoding").map_or("", String::as_str))
        .env("GIT_PROTOCOL", headers.get("git-protocol").map_or("", String::as_str))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    let mut si = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let _ = si.write_all(&body);
    });
    let out = child.wait_with_output().unwrap().stdout;
    let split = out.windows(4).position(|x| x == b"\r\n\r\n").map(|i| (i, i + 4)).or_else(|| out.windows(2).position(|x| x == b"\n\n").map(|i| (i, i + 2))).expect("CGI headers");
    let head = String::from_utf8_lossy(&out[..split.0]).into_owned();
    let content = &out[split.1..];
    let status = head.lines().find_map(|l| l.strip_prefix("Status: ")).unwrap_or("200 OK").to_string();
    let extra: String = head.lines().filter(|l| !l.starts_with("Status:")).map(|l| format!("{l}\r\n")).collect();
    let _ = w.write_all(format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", content.len()).as_bytes());
    let _ = w.write_all(content);
}

fn helper_config() -> String {
    format!("!\"{}\" --credential-helper", BIN.replace('\\', "/"))
}

/// Talks to gitem's helper as git does: for HTTP it sends no path by default, and
/// only the fields it knows.
fn run_helper(op: &str, url: &str, user: &str, pass: &str) -> String {
    let (host, _) = url.trim_start_matches("http://").split_once('/').unwrap();
    let mut input = format!("protocol=http\nhost={host}\n");
    if !user.is_empty() {
        input.push_str(&format!("username={user}\npassword={pass}\n"));
    }
    input.push('\n');
    let mut c = Command::new(BIN).args(["--credential-helper", op]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    String::from_utf8_lossy(&c.wait_with_output().unwrap().stdout).into_owned()
}

/// Removes the test's vault entry even if the test fails.
struct Vault(String);
impl Drop for Vault {
    fn drop(&mut self) {
        run_helper("erase", &self.0, "", "");
    }
}

fn stored(url: &str) -> bool {
    run_helper("get", url, "", "").contains("password=")
}

/// Git set up the way the app runs it, with only gitem as credential helper.
fn gitem_git(ask: bool) -> GitExe {
    let mut g = GitExe::discover(None).unwrap();
    g.helper = ask.then(|| PathBuf::from(BIN));
    g
}

fn creds(args: Vec<String>) -> Vec<String> {
    let mut a = vec!["-c".to_string(), "credential.helper=".to_string(), "-c".to_string(), format!("credential.helper={}", helper_config())];
    a.extend(args);
    a
}

fn run_with_helper(g: &GitExe, cwd: Option<&Path>, mut op: ops::GitOp) -> Result<(), String> {
    for s in &mut op.steps {
        s.args = creds(std::mem::take(&mut s.args));
    }
    op.run(g, cwd, None, &mut |_| {}).map(|_| ()).map_err(|e| e.to_string())
}

fn seed(dir: &Path) {
    for a in [&["config", "user.name", "T"][..], &["config", "user.email", "t@x"], &["config", "commit.gpgsign", "false"]] {
        assert!(Command::new("git").args(a).current_dir(dir).status().unwrap().success());
    }
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    assert!(Command::new("git").args(["add", "."]).current_dir(dir).status().unwrap().success());
    assert!(Command::new("git").args(["commit", "-qm", "first"]).current_dir(dir).status().unwrap().success());
}

#[test]
fn clone_and_push_use_the_stored_login_and_a_rejected_one_is_erased() {
    let srv = Server::start("alice", "s3cret");
    let url = srv.url();
    let _cleanup = Vault(url.clone());
    let g = gitem_git(false);
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let work_s = work.to_string_lossy().replace('\\', "/");

    // Nothing stored and prompts off: a clear failure, not a hang.
    let e = run_with_helper(&g, None, ops::clone_repo(&url, &work_s, None, false, None)).unwrap_err();
    assert!(e.contains("Authentication failed") || e.contains("could not read Username"), "{e}");

    // A wrong password is rejected, and git has the helper forget it.
    run_helper("store", &url, "alice", "wrong");
    assert!(stored(&url));
    let e = run_with_helper(&g, None, ops::clone_repo(&url, &work_s, None, false, None)).unwrap_err();
    assert!(e.contains("Authentication failed") || e.contains("401") || e.contains("could not read"), "{e}");
    assert!(srv.rejected.load(Ordering::Relaxed) >= 1);
    assert!(!stored(&url), "the rejected login stays in the vault");

    // The right one clones and pushes.
    run_helper("store", &url, "alice", "s3cret");
    run_with_helper(&g, None, ops::clone_repo(&url, &work_s, None, false, None)).unwrap();
    seed(&work);
    run_with_helper(&g, Some(&work), ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some("main".into()), set_upstream: true, ..Default::default() })).unwrap();
    assert!(srv.accepted_as.lock().unwrap().iter().any(|t| t.contains("receive-pack")));

    let repo = Repo::open(g.clone(), &work).unwrap();
    assert_eq!(repo.status().unwrap().upstream.as_deref(), Some("origin/main"));
    run_with_helper(&g, Some(&work), ops::fetch(&ops::FetchOpts::default())).unwrap();
    run_with_helper(&g, Some(&work), ops::pull(ops::PullMode::FfOnly, false)).unwrap();

    // The password never lands in the command log.
    let log = g.log.lock().unwrap();
    assert!(!log.iter().any(|l| l.command.contains("s3cret") || l.output.contains("s3cret")));
}

/// Opens a real prompt window, which answers itself.
#[test]
#[ignore = "opens windows; run with --ignored --test-threads=1"]
fn the_askpass_window_answers_git_and_the_helper_saves_it() {
    let srv = Server::start("typed", "typed");
    let url = srv.url();
    let _cleanup = Vault(url.clone());
    let g = gitem_git(true);
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let mut op = ops::clone_repo(&url, &work.to_string_lossy().replace('\\', "/"), None, false, None);
    for s in &mut op.steps {
        s.env.push(("GITEM_TEST_ACCEPT".to_string(), "typed".to_string()));
    }
    run_with_helper(&g, None, op).unwrap();
    assert!(work.join(".git").exists());
    assert!(stored(&url), "git asks the helper to save a login that worked");
}

#[test]
fn base64_matches_known_values() {
    assert_eq!(base64("alice:s3cret"), "YWxpY2U6czNjcmV0");
    assert_eq!(base64("ab"), "YWI=");
}
