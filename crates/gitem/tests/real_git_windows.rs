//! Real git launching the real gitem windows, which answer by themselves through
//! GITEM_TEST_ACCEPT. Windows flash on screen, so these are opt-in:
//! `cargo test -p gitem --test real_git_windows -- --ignored --test-threads=1`
//! Temp repositories only.
gitem_testenv::isolate!();

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_gitem");

fn bin() -> String {
    BIN.replace('\\', "/")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "T"), ("user.email", "t@x"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        git(d.path(), &["config", k, v]);
    }
    d
}

fn commit(d: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(d.join(file), text).unwrap();
    git(d, &["add", "."]);
    git(d, &["commit", "-q", "-m", msg]);
}

#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn git_commit_uses_the_message_window() {
    let d = repo();
    std::fs::write(d.path().join("a"), "1\n").unwrap();
    git(d.path(), &["add", "."]);
    let s = Command::new("git").args(["commit"]).current_dir(d.path()).env("GIT_EDITOR", format!("\"{}\" --edit-file", bin())).env("GITEM_TEST_ACCEPT", "From the window").status().unwrap();
    assert!(s.success());
    assert_eq!(git(d.path(), &["log", "-1", "--format=%s"]).trim(), "From the window");
}

#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn git_rebase_uses_the_rebase_window() {
    let d = repo();
    commit(d.path(), "a", "1\n", "base");
    commit(d.path(), "b", "1\n", "one");
    commit(d.path(), "c", "1\n", "two");
    let s = Command::new("git").args(["rebase", "-i", "HEAD~2"]).current_dir(d.path()).env("GIT_SEQUENCE_EDITOR", format!("\"{}\" --rebase-editor", bin())).env("GIT_EDITOR", "true").env("GITEM_TEST_ACCEPT", "squash").status().unwrap();
    assert!(s.success());
    assert_eq!(git(d.path(), &["rev-list", "--count", "HEAD"]).trim(), "2", "the two commits were squashed");
}

#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn git_asks_the_password_window() {
    // No helper configured, so git asks through GIT_ASKPASS. Nothing is contacted.
    let d = repo();
    let mut c = Command::new("git")
        .args(["-c", "credential.helper=", "credential", "fill"])
        .current_dir(d.path())
        .env("GIT_ASKPASS", BIN)
        .env("GITEM_ASKPASS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GITEM_TEST_ACCEPT", "typed-value")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(b"protocol=https\nhost=example.invalid\n\n").unwrap();
    let o = c.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("username=typed-value") && out.contains("password=typed-value"), "{out}");
}

#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn git_mergetool_uses_the_merge_window() {
    let d = repo();
    commit(d.path(), "c", "top\n", "base");
    git(d.path(), &["switch", "-q", "-c", "side"]);
    commit(d.path(), "c", "side\n", "side");
    git(d.path(), &["switch", "-q", "main"]);
    commit(d.path(), "c", "main\n", "main");
    assert!(!Command::new("git").args(["merge", "side"]).current_dir(d.path()).output().unwrap().status.success());
    let cmd = format!("\"{}\" --mergetool \"$BASE\" \"$LOCAL\" \"$REMOTE\" \"$MERGED\"", bin());
    let s = Command::new("git")
        .args(["-c", &format!("mergetool.gitem.cmd={cmd}"), "-c", "mergetool.gitem.trustExitCode=true", "-c", "mergetool.keepBackup=false", "mergetool", "--tool=gitem", "--no-prompt"])
        .current_dir(d.path())
        .env("GITEM_TEST_ACCEPT", "theirs")
        .status()
        .unwrap();
    assert!(s.success());
    assert_eq!(std::fs::read_to_string(d.path().join("c")).unwrap(), "side\n");
    assert!(!git(d.path(), &["status", "--short"]).contains("UU"));
}

/// The window exits with an error unless the diff it loaded holds the text asked for.
#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn git_difftool_uses_the_diff_window() {
    let d = repo();
    commit(d.path(), "a", "old line\n", "base");
    std::fs::write(d.path().join("a"), "new line\n").unwrap();
    let cmd = format!("\"{}\" --difftool \"$LOCAL\" \"$REMOTE\"", bin());
    let run = |asked: &str| Command::new("git").args(["-c", &format!("difftool.gitem.cmd={cmd}"), "-c", "difftool.trustExitCode=true", "difftool", "--tool=gitem", "--no-prompt"]).current_dir(d.path()).env("GITEM_TEST_ACCEPT", asked).status().unwrap();
    assert!(run("old line\nnew line").success(), "the window shows both sides of the change");
    assert!(!run("not in this diff").success(), "the check can fail");
}
