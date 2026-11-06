//! Runs the real gitem binary in the modes git itself invokes, with real git as
//! the caller. Temp repositories only; no remote is contacted.
use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_gitem");

fn helper_config() -> String {
    format!("credential.helper=!\"{}\" --credential-helper", BIN.replace('\\', "/"))
}

fn git_stdin(args: &[&str], input: &str) -> (bool, String) {
    let mut c = Command::new("git").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("GIT_TERMINAL_PROMPT", "0").env("GCM_INTERACTIVE", "never").spawn().unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr))
}

#[test]
fn acts_as_git_credential_helper_through_real_git() {
    // `-c credential.helper=` first clears other helpers, so only gitem is asked.
    let cfg = helper_config();
    let host = format!("gitem-helper-test-{}.invalid", std::process::id());
    let base = ["-c", "credential.helper=", "-c", &cfg];
    let mut approve = base.to_vec();
    approve.extend(["credential", "approve"]);
    let mut fill = base.to_vec();
    fill.extend(["credential", "fill"]);
    let mut reject = base.to_vec();
    reject.extend(["credential", "reject"]);

    let result = std::panic::catch_unwind(|| {
        let (ok, out) = git_stdin(&approve, &format!("protocol=https\nhost={host}\nusername=alice\npassword=hunter2\n\n"));
        assert!(ok, "approve: {out}");
        let (ok, out) = git_stdin(&fill, &format!("protocol=https\nhost={host}\n\n"));
        assert!(ok, "fill: {out}");
        assert!(out.contains("username=alice"), "{out}");
        assert!(out.contains("password=hunter2"), "{out}");
        let (ok, out) = git_stdin(&reject, &format!("protocol=https\nhost={host}\nusername=alice\n\n"));
        assert!(ok, "reject: {out}");
        // After rejecting, git has no answer and prompting is disabled, so fill fails.
        let (ok, _) = git_stdin(&fill, &format!("protocol=https\nhost={host}\n\n"));
        assert!(!ok, "credential should be gone");
    });
    // Always remove the entry from the OS vault, whatever happened above.
    let _ = gitem_core::cred::delete(&format!("{}https://{host}", gitem_core::cred::PREFIX_GIT));
    result.unwrap();
}

#[test]
fn apply_todo_mode_replaces_the_todo_and_cleans_up() {
    let d = tempfile::tempdir().unwrap();
    let (src, dest) = (d.path().join("plan"), d.path().join("git-rebase-todo"));
    std::fs::write(&src, "pick abc one\n").unwrap();
    std::fs::write(&dest, "old\n").unwrap();
    let s = Command::new(BIN).arg("--apply-todo").arg(&src).arg(&dest).status().unwrap();
    assert!(s.success());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "pick abc one\n");
    assert!(!src.exists(), "the temp plan is removed");
}

#[test]
fn apply_todo_mode_reports_failure() {
    let d = tempfile::tempdir().unwrap();
    let s = Command::new(BIN).arg("--apply-todo").arg(d.path().join("missing")).arg(d.path().join("t")).status().unwrap();
    assert!(!s.success(), "a failed copy must abort the rebase");
}

#[test]
fn unknown_options_are_rejected() {
    let o = Command::new(BIN).arg("--no-such-option").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn help_prints_usage() {
    let o = Command::new(BIN).arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("--credential-helper"));
}

#[test]
fn mcp_mode_speaks_over_stdio_with_the_real_binary() {
    let d = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(d.path()).status().unwrap().success());
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("a.txt"), "x\n").unwrap();
    run(&["add", "."]);
    run(&["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "first"]);
    let mut c = Command::new(BIN).arg("--mcp").arg(d.path()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"git_log\",\"arguments\":{}}}\n"
    );
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("serverInfo"));
    assert!(lines[1].contains("first"));
}

#[test]
fn bad_deep_links_are_rejected() {
    assert_eq!(Command::new(BIN).arg("gitem://nonsense").output().unwrap().status.code(), Some(2));
}
