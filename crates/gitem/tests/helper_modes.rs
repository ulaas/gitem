//! Runs the real gitem binary in the modes git itself invokes, with real git as
//! the caller. Temp repositories only; no remote is contacted.
gitem_testenv::isolate!();

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

/// Opens a real window for a moment, so it's opt-in.
#[test]
#[ignore = "opens a window; run with --ignored --test-threads=1"]
fn a_screenshot_stays_inside_the_config_folder() {
    let d = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(d.path()).status().unwrap().success());
    run(&["init", "-q", "-b", "main"]);
    let config = tempfile::tempdir().unwrap();
    let out = config.path().join("shot.png");
    let s = Command::new(BIN).arg(d.path()).arg("--shot").arg(&out).arg("--size").arg("800x500").env("GITEM_CONFIG_DIR", config.path()).status().unwrap();
    assert!(s.success());
    assert!(out.exists(), "the screenshot was taken");
    assert!(config.path().join("app.ron").exists(), "eframe's state follows GITEM_CONFIG_DIR");
    assert!(!config.path().join("settings.json").exists(), "a screenshot doesn't save the session");
}

#[test]
fn a_panic_leaves_a_report_in_the_config_folder() {
    let d = tempfile::tempdir().unwrap();
    let o = Command::new(BIN).arg("--test-panic").env("GITEM_CONFIG_DIR", d.path()).output().unwrap();
    assert_eq!(o.status.code(), Some(101), "the crash itself isn't hidden");
    assert!(String::from_utf8_lossy(&o.stderr).contains("a panic asked for by a test"), "the message still reaches a console");
    let reports = gitem::crash::pending(d.path());
    assert_eq!(reports.len(), 1, "{reports:?}");
    let text = std::fs::read_to_string(&reports[0]).unwrap();
    for part in ["gitem crash report", "version: ", "a panic asked for by a test", "main.rs", "backtrace:"] {
        assert!(text.contains(part), "no {part:?} in:\n{text}");
    }
}

#[test]
fn the_passphrase_helper_prints_its_value_to_ssh_keygen() {
    // ssh-keygen passes the prompt as the only argument.
    let o = Command::new(BIN).arg("Enter passphrase (empty for no passphrase): ").env(gitem_core::cred::KEY_PASSPHRASE_VAR, "fake passphrase").output().unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), "fake passphrase");
}

fn helper_process(dir: &std::path::Path, op: &str, input: &str) -> std::process::Output {
    let mut c = Command::new(BIN).args(["--credential-helper", op]).env("GITEM_CONFIG_DIR", dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

/// SN-35. Git starts one helper per request, so these can happen at once. A file store is what
/// runs on Linux and macOS, and here with `GITEM_CONFIG_DIR`, which is how this test gets it.
#[test]
fn helpers_running_at_once_lose_no_credentials() {
    let d = tempfile::tempdir().unwrap();
    let hosts: Vec<String> = (0..16).map(|i| format!("gitem-many-{i}.invalid")).collect();
    let running: Vec<_> = hosts
        .iter()
        .map(|host| {
            let mut c = Command::new(BIN).args(["--credential-helper", "store"]).env("GITEM_CONFIG_DIR", d.path()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            c.stdin.take().unwrap().write_all(format!("protocol=https\nhost={host}\nusername=u\npassword=FAKE\n\n").as_bytes()).unwrap();
            c
        })
        .collect();
    for c in running {
        let o = c.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    for host in &hosts {
        let o = helper_process(d.path(), "get", &format!("protocol=https\nhost={host}\n\n"));
        assert!(String::from_utf8_lossy(&o.stdout).contains("password=FAKE"), "{host} was lost");
    }
    let leftovers: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name()).filter(|n| n != "credentials.json").collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// SN-35. A store that can't be read used to count as empty, and the next save replaced it.
#[test]
fn a_damaged_credential_file_is_left_alone() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("credentials.json");
    std::fs::write(&file, "{ not json, but maybe somebody's only copy").unwrap();
    let o = helper_process(d.path(), "store", "protocol=https\nhost=gitem-damaged.invalid\nusername=u\npassword=FAKE\n\n");
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("damaged"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json, but maybe somebody's only copy");
    let o = helper_process(d.path(), "get", "protocol=https\nhost=gitem-damaged.invalid\n\n");
    assert!(!o.status.success(), "and it says so, rather than that there's nothing there");
}

/// Real `ssh-keygen`, with the key in the scratch config folder. The full route: gitem
/// starts it, it asks the real helper binary, and the key opens only with that passphrase.
#[test]
fn a_generated_key_really_has_the_passphrase() {
    if Command::new("ssh-keygen").arg("-?").output().is_err() {
        eprintln!("ssh-keygen isn't installed: skipping");
        return;
    }
    let helper = std::path::Path::new(BIN);
    let path = gitem_core::cred::ssh_generate("with_passphrase", "test key", "FAKE_PASSPHRASE_42", Some(helper)).unwrap();
    let opens_with = |p: &str| Command::new("ssh-keygen").args(["-y", "-P", p, "-f", &path]).stdin(Stdio::null()).output().unwrap().status.success();
    assert!(opens_with("FAKE_PASSPHRASE_42"));
    assert!(!opens_with("wrong"));
    assert!(!opens_with(""));

    let open = gitem_core::cred::ssh_generate("no_passphrase", "test key", "", None).unwrap();
    assert!(Command::new("ssh-keygen").args(["-y", "-P", "", "-f", &open]).stdin(Stdio::null()).output().unwrap().status.success());

    assert!(gitem_core::cred::ssh_generate("../outside", "", "", None).is_err());
    assert!(!gitem_core::cred::ssh_dir().parent().unwrap().join("outside").exists());
}
