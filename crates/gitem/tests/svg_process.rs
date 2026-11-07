//! The SVG renderer as a real child process of the real binary. What makes one misbehave is a
//! debug build's test mode, so none of this applies to a release build's tests.
#![cfg(debug_assertions)]
gitem_testenv::isolate!();

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gitem::svg::{CHILD_ARG, GAVE_UP, NO_TURN, Renderer, Started, children_running};

const BIN: &str = env!("CARGO_BIN_EXE_gitem");
const RED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;
/// Long, for what must end some other way than by reaching it.
const LONG: Duration = Duration::from_secs(50);
const SOON: Duration = Duration::from_secs(20);

/// One test at a time: they share the app's limit on children, and watch how many there are.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn renderer(mode: Option<&'static str>, limit: Duration) -> Renderer {
    Renderer { exe: BIN.into(), limit, patience: LONG, test_mode: mode, started: Some(Started::default()) }
}

fn eventually(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < SOON, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// By the operating system, not the app's list. Reads only: nothing here touches another process.
#[cfg(windows)]
fn alive(id: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: the handle is checked and closed, and only waited on for no time.
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, id);
        if h.is_null() {
            return false;
        }
        let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        CloseHandle(h);
        running
    }
}

#[cfg(not(windows))]
fn alive(id: u32) -> bool {
    Command::new("kill").args(["-0", &id.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Its answer, with the child it ran seen gone by the operating system too.
fn run_to_the_end(r: &Renderer, bytes: &[u8]) -> Result<gitem::svg::Drawn, String> {
    let got = r.render(bytes, None);
    for id in r.started.as_ref().unwrap().lock().unwrap().iter() {
        assert!(!children_running().contains(id), "{id} still listed");
        assert!(!alive(*id), "{id} still running");
    }
    got
}

#[test]
fn an_ordinary_one_comes_back_from_a_real_child_and_it_ends() {
    let _t = turn();
    let d = run_to_the_end(&renderer(None, LONG), RED).unwrap();
    assert_eq!(d.image.size, [40, 20]);
    assert_eq!(d.image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
    assert!(!d.font_missing);
}

#[test]
fn the_childs_limits_and_their_reasons_reach_the_view() {
    let _t = turn();
    let deep = format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{}{}</svg>"#, "<g>".repeat(5000), "</g>".repeat(5000));
    assert!(run_to_the_end(&renderer(None, LONG), deep.as_bytes()).err().unwrap().contains("nests too deeply"));
    assert!(run_to_the_end(&renderer(None, LONG), b"\xff\xfe\x00").err().unwrap().contains("UTF-8"));
    // Too big doesn't need a child to say so.
    let r = renderer(None, LONG);
    assert!(run_to_the_end(&r, &vec![b' '; 4 * 1024 * 1024 + 1]).err().unwrap().contains("MB"));
    assert!(r.started.as_ref().unwrap().lock().unwrap().is_empty());
}

/// The environment is what's left to the fonts, so this is what shows it wasn't cut too far.
#[test]
fn text_is_drawn_with_only_the_environment_a_child_is_given() {
    let _t = turn();
    let doc = |font: &str| format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30"><text x="2" y="20" font-size="16" font-family="{font}">Words</text></svg>"#);
    let d = run_to_the_end(&renderer(None, LONG), doc("Ubuntu").as_bytes()).unwrap();
    assert!(d.image.pixels.iter().any(|p| p.a() > 0));
    assert!(!d.font_missing, "gitem's own");
    #[cfg(windows)]
    assert!(!run_to_the_end(&renderer(None, LONG), doc("Segoe UI").as_bytes()).unwrap().font_missing, "this computer's");
    assert!(run_to_the_end(&renderer(None, LONG), doc("No Such Font").as_bytes()).unwrap().font_missing);
}

#[test]
fn the_child_isnt_given_the_parents_environment() {
    let _t = turn();
    assert!(std::env::var_os("GIT_CONFIG_GLOBAL").is_some() && std::env::var_os("GITEM_CONFIG_DIR").is_some(), "there to pass on");
    let names = run_to_the_end(&renderer(Some("environment"), LONG), RED).err().unwrap();
    assert!(names.contains("GITEM_SVG_TEST"), "{names}");
    assert!(!names.contains("GIT_CONFIG_GLOBAL") && !names.contains("GITEM_CONFIG_DIR") && !names.contains("PATH"), "{names}");
}

/// Only the kill can end this one in time: it's deaf to its stdin and has no limit of its own.
#[test]
fn a_render_that_never_ends_is_killed_at_the_limit() {
    let _t = turn();
    let t = Instant::now();
    let r = renderer(Some("hang-deaf"), Duration::from_millis(200));
    assert_eq!(run_to_the_end(&r, RED).err().as_deref(), Some(GAVE_UP));
    assert!(t.elapsed() < SOON, "{:?}", t.elapsed());
    assert_eq!(r.started.as_ref().unwrap().lock().unwrap().len(), 1);
}

/// The real renderer and not a test mode: fonts alone take longer than this.
#[test]
fn a_real_render_is_killed_at_the_limit_too() {
    let _t = turn();
    let doc = r#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><text>Words</text></svg>"#;
    assert_eq!(run_to_the_end(&renderer(None, Duration::from_millis(1)), doc.as_bytes()).err().as_deref(), Some(GAVE_UP));
}

/// A crash says what a timeout says, and the next one works.
#[test]
fn a_crash_is_told_as_a_timeout_is_and_costs_nothing_else() {
    let _t = turn();
    for mode in ["panic", "abort", "exit-quietly"] {
        assert_eq!(run_to_the_end(&renderer(Some(mode), LONG), RED).err().as_deref(), Some(GAVE_UP), "{mode}");
    }
    assert!(run_to_the_end(&renderer(None, LONG), RED).is_ok());
}

#[test]
fn output_that_isnt_right_is_refused() {
    let _t = turn();
    for mode in ["garbage", "wrong-size", "too-many-pixels", "too-wide", "overflowing-sides", "long-why"] {
        assert_eq!(run_to_the_end(&renderer(Some(mode), LONG), RED).err().as_deref(), Some(GAVE_UP), "{mode}");
    }
    assert_eq!(run_to_the_end(&renderer(Some("control-characters"), LONG), RED).err().as_deref(), Some("line [31m one"));
}

/// It would fill the memory if it were read to its end. Reading stops, and it's killed.
#[test]
fn output_that_never_ends_is_cut_off_and_refused() {
    let _t = turn();
    let t = Instant::now();
    assert_eq!(run_to_the_end(&renderer(Some("flood"), LONG), RED).err().as_deref(), Some(GAVE_UP));
    assert!(t.elapsed() < SOON, "{:?}", t.elapsed());
}

/// An answer doesn't count until the child's stdout closes, so this waits for the limit, and goes.
#[test]
fn a_child_that_answers_and_doesnt_end_is_killed() {
    let _t = turn();
    assert_eq!(run_to_the_end(&renderer(Some("answer-then-hang"), Duration::from_millis(300)), RED).err().as_deref(), Some(GAVE_UP));
}

#[test]
fn being_called_off_kills_the_child_without_waiting_for_the_limit() {
    let _t = turn();
    let r = renderer(Some("hang-deaf"), LONG);
    let started = r.started.clone().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let waiting = std::thread::spawn(move || r.render(RED, Some(&flag)).err());
    eventually("it to start", || !started.lock().unwrap().is_empty());
    let id = started.lock().unwrap()[0];
    assert!(alive(id));
    let t = Instant::now();
    cancel.store(true, Ordering::Relaxed);
    assert!(waiting.join().unwrap().is_some());
    assert!(t.elapsed() < SOON, "{:?}", t.elapsed());
    assert!(!alive(id));
    // Called off from the start, nothing runs.
    let r = renderer(None, LONG);
    assert!(r.render(RED, Some(&AtomicBool::new(true))).is_err());
    assert!(r.started.as_ref().unwrap().lock().unwrap().is_empty());
}

/// Stuck ones used to stop later ones until gitem restarted. Now they're out of the way at the
/// limit, and there are never more than two.
#[test]
fn many_at_once_keep_to_the_limit_and_stuck_ones_dont_stop_later_ones() {
    let _t = turn();
    let most = Arc::new(Mutex::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (most, done) = (most.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let n = children_running().len();
                let mut m = most.lock().unwrap();
                *m = n.max(*m);
                drop(m);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let waiting: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let r = Renderer { patience: Duration::from_millis(600), ..renderer(Some("hang-deaf"), Duration::from_millis(300)) };
                let got = r.render(RED, None).err().unwrap();
                (got, r.started.unwrap().lock().unwrap().clone())
            })
        })
        .collect();
    let mut ran = Vec::new();
    for w in waiting {
        let (got, ids) = w.join().unwrap();
        assert!(got == GAVE_UP || got == NO_TURN, "{got}");
        ran.extend(ids);
    }
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    let most = *most.lock().unwrap();
    assert!((1..=2).contains(&most), "{most} at once");
    assert!(ran.iter().all(|id| !alive(*id)), "{ran:?}");
    assert!(children_running().is_empty());
    assert!(run_to_the_end(&renderer(None, LONG), RED).is_ok(), "the next one works");
}

// ---- no process left, with the parent closed abruptly

const PARENT_MODE: &str = "SVG_PROCESS_TEST_PARENT_MODE";

/// Not a test of its own: the parent that the next test starts and kills. Unless it's that, nothing.
#[test]
#[ignore = "only ever run by the test below, as a process to kill"]
fn parent_to_be_killed() {
    let Some(mode) = std::env::var(PARENT_MODE).ok() else { return };
    let mode: &'static str = mode.leak();
    let r = renderer(Some(mode), LONG);
    let started = r.started.clone().unwrap();
    std::thread::spawn(move || r.render(RED, None));
    eventually("it to start", || !started.lock().unwrap().is_empty());
    // Past when the child has its file, so it's as far along as it'll get.
    std::thread::sleep(Duration::from_millis(100));
    let mut out = std::io::stdout();
    writeln!(out, "child {}", started.lock().unwrap()[0]).unwrap();
    out.flush().unwrap();
    std::thread::sleep(LONG);
}

fn a_child_doesnt_outlive_its_parent_being_killed(mode: &str) {
    let mut parent = Command::new(std::env::current_exe().unwrap()).args(["--exact", "parent_to_be_killed", "--ignored", "--nocapture"]).env(PARENT_MODE, mode).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut lines = BufReader::new(parent.stdout.take().unwrap()).lines();
    let id: u32 = lines.by_ref().map_while(Result::ok).find_map(|l| l.split("child ").nth(1).and_then(|n| n.trim().parse().ok())).expect("the child's id");
    assert!(alive(id), "{mode}");
    parent.kill().unwrap();
    parent.wait().unwrap();
    eventually(&format!("{mode}: child {id} to go with its parent"), || !alive(id));
}

/// What every platform has: the child sees its stdin close.
#[test]
fn a_child_ends_when_its_parent_is_killed() {
    a_child_doesnt_outlive_its_parent_being_killed("hang");
}

/// One that's stuck where it can't notice: only the job object can end this in time.
#[cfg(windows)]
#[test]
fn on_windows_even_a_stuck_child_ends_with_its_parent() {
    a_child_doesnt_outlive_its_parent_being_killed("hang-deaf");
}

// ---- the hidden mode itself

fn hidden_mode(mode: Option<&str>, config: &std::path::Path) -> std::process::Child {
    let mut c = Command::new(BIN);
    c.arg(CHILD_ARG).env("GITEM_CONFIG_DIR", config).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(m) = mode {
        c.env("GITEM_SVG_TEST", m);
    }
    c.spawn().unwrap()
}

fn send(child: &mut std::process::Child, bytes: &[u8]) {
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(&(bytes.len() as u64).to_le_bytes()).unwrap();
    stdin.write_all(bytes).unwrap();
    stdin.flush().unwrap();
}

/// Settings and crash reports live in this folder, so it's all that needs looking at.
#[test]
fn the_hidden_mode_writes_nothing_where_settings_live_even_on_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir(&config).unwrap();
    for mode in [None, Some("panic"), Some("abort")] {
        let mut c = hidden_mode(mode, &config);
        send(&mut c, RED);
        // Open until it's over, as closing it says the parent has gone.
        let stdin = c.stdin.take();
        let out = c.wait_with_output().unwrap();
        drop(stdin);
        assert!(out.stderr.is_empty(), "{mode:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.status.success(), mode.is_none(), "{mode:?}");
        assert_eq!(std::fs::read_dir(&config).unwrap().count(), 0, "{mode:?}");
    }
}

#[test]
fn the_hidden_mode_ends_with_nothing_to_read_or_too_much_said() {
    let dir = tempfile::tempdir().unwrap();
    for sent in [&[][..], &[1, 2, 3], &u64::MAX.to_le_bytes(), &(5u64 * 1024 * 1024).to_le_bytes()] {
        let mut c = hidden_mode(None, dir.path());
        c.stdin.as_mut().unwrap().write_all(sent).unwrap();
        drop(c.stdin.take());
        let out = c.wait_with_output().unwrap();
        assert!(!out.status.success() && out.stdout.is_empty(), "{sent:?}");
    }
}

#[test]
fn the_hidden_mode_stops_when_its_stdin_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = hidden_mode(Some("hang"), dir.path());
    send(&mut c, RED);
    std::thread::sleep(Duration::from_millis(100));
    drop(c.stdin.take());
    let t = Instant::now();
    while c.try_wait().unwrap().is_none() {
        if t.elapsed() > SOON {
            let _ = c.kill();
            panic!("still running");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(windows)]
#[test]
fn the_hidden_mode_opens_no_window() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
    unsafe extern "system" fn note(window: HWND, found: LPARAM) -> windows_sys::core::BOOL {
        let mut owner = 0;
        // SAFETY: the pointer is the pair below, which outlives `EnumWindows`.
        unsafe {
            GetWindowThreadProcessId(window, &mut owner);
            let (id, count) = &mut *(found as *mut (u32, usize));
            if owner == *id {
                *count += 1;
            }
        }
        1
    }
    let dir = tempfile::tempdir().unwrap();
    let mut c = hidden_mode(Some("hang"), dir.path());
    send(&mut c, RED);
    std::thread::sleep(Duration::from_millis(500));
    let mut found = (c.id(), 0usize);
    // SAFETY: the callback only uses what's passed, for as long as this call lasts.
    unsafe {
        EnumWindows(Some(note), (&raw mut found) as LPARAM);
    }
    c.kill().unwrap();
    c.wait().unwrap();
    assert_eq!(found.1, 0, "windows it opened");
}
