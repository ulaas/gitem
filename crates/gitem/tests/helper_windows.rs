//! Drives the small windows git opens: password prompt, message editor, rebase
//! editor, and the standalone merge and diff tools.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use gitem::standalone::{HelperApp, Mode};
use gitem::views::mergeview::{MergeSource, MergeView};
use gitem::views::rebase::RebaseView;

/// Only for noticing that something hangs, so a slow machine must never reach it.
/// A loaded runner can make these tests ten times slower than usual.
const PATIENCE: Duration = Duration::from_secs(180);

fn harness<'a>(mode: Mode) -> Harness<'a, HelperApp> {
    let app = HelperApp::new(mode);
    Harness::builder().with_size(vec2(1000.0, 700.0)).with_max_steps(5).build_ui_state(|ui, app: &mut HelperApp| app.frame(ui), app)
}

fn find(h: &mut Harness<HelperApp>, label: &str) {
    let t = Instant::now();
    while h.query_by_label(label).is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..3 {
        h.step();
    }
}

fn click(h: &mut Harness<HelperApp>, label: &str) {
    find(h, label);
    h.get_by_label(label).click();
    h.step();
    h.step();
}

fn askpass(prompt: &str, secret: bool, confirm: bool) -> Mode {
    Mode::Askpass { prompt: prompt.to_string(), value: String::new(), secret, confirm }
}

#[test]
fn password_prompt_returns_what_was_typed() {
    let mut h = harness(askpass("Password for 'https://example.com': ", true, false));
    find(&mut h, "OK");
    h.event(egui::Event::Text("hunter2".into()));
    h.step();
    h.step();
    click(&mut h, "OK");
    assert_eq!(h.state().result, Some((0, Some("hunter2".to_string()))));
}

#[test]
fn password_prompt_cancels_with_an_error_code() {
    let mut h = harness(askpass("Username: ", false, false));
    click(&mut h, "Cancel");
    assert_eq!(h.state().result, Some((1, None)));
}

#[test]
fn ssh_host_question_answers_yes_or_no() {
    let mut h = harness(askpass("Are you sure you want to continue connecting (yes/no/[fingerprint])?", false, true));
    click(&mut h, "Yes");
    assert_eq!(h.state().result, Some((0, Some("yes".to_string()))));
    let mut h = harness(askpass("Continue (yes/no)?", false, true));
    click(&mut h, "No");
    assert_eq!(h.state().result, Some((0, Some("no".to_string()))));
}

fn temp_file(d: &tempfile::TempDir, name: &str, text: &[u8]) -> PathBuf {
    let p = d.path().join(name);
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn message_editor_saves_or_leaves_the_file_alone() {
    let d = tempfile::tempdir().unwrap();
    let p = temp_file(&d, "COMMIT_EDITMSG", b"original\n");
    let mut h = harness(Mode::Editor { path: p.clone(), text: "edited message\n".to_string() });
    click(&mut h, "Save and continue");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "edited message\n");

    let p2 = temp_file(&d, "other", b"keep me\n");
    let mut h = harness(Mode::Editor { path: p2.clone(), text: "changed\n".to_string() });
    click(&mut h, "Cancel");
    assert_eq!(h.state().result, Some((1, None)), "git aborts on a non-zero exit");
    assert_eq!(std::fs::read_to_string(&p2).unwrap(), "keep me\n");
}

#[test]
fn rebase_editor_writes_the_todo_or_empties_it_to_abort() {
    let d = tempfile::tempdir().unwrap();
    let todo = temp_file(&d, "git-rebase-todo", b"pick aaa one\npick bbb two\n# comment\n");
    let mut h = harness(Mode::Rebase(Box::new(RebaseView::for_todo_file(todo.clone()))));
    click(&mut h, "Squash all into first");
    click(&mut h, "Start rebase");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read_to_string(&todo).unwrap(), "pick aaa one\nsquash bbb two\n");

    let todo = temp_file(&d, "todo2", b"pick aaa one\n");
    let mut h = harness(Mode::Rebase(Box::new(RebaseView::for_todo_file(todo.clone()))));
    click(&mut h, "Abort rebase");
    assert_eq!(h.state().result, Some((1, None)));
    assert_eq!(std::fs::read_to_string(&todo).unwrap(), "", "an empty todo tells git to abort");
}

/// The merged file holds the conflict as git would have written it.
fn merge_mode(d: &tempfile::TempDir, base: &[u8], local: &[u8], remote: &[u8]) -> (Mode, PathBuf) {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let marked = gitem_core::merge::MergeDoc::new(&text(base), &text(local), &text(remote)).render("local", "remote");
    merge_mode_with(d, base, local, remote, marked.as_bytes())
}

fn merge_mode_with(d: &tempfile::TempDir, base: &[u8], local: &[u8], remote: &[u8], merged_now: &[u8]) -> (Mode, PathBuf) {
    let merged = temp_file(d, "merged", merged_now);
    let m = MergeSource::Files { base: temp_file(d, "base", base), local: temp_file(d, "local", local), remote: temp_file(d, "remote", remote), merged: merged.clone() };
    (Mode::Merge(Box::new(MergeView::with_source(m))), merged)
}

#[test]
fn standalone_merge_tool_resolves_and_saves() {
    let d = tempfile::tempdir().unwrap();
    let (mode, merged) = merge_mode(&d, b"a\nb\nc\n", b"a\nOURS\nc\n", b"a\nTHEIRS\nc\n");
    let mut h = harness(mode);
    find(&mut h, "Take theirs");
    assert_eq!(h.state().result, None);
    click(&mut h, "Take theirs");
    click(&mut h, "Save and finish");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read_to_string(&merged).unwrap(), "a\nTHEIRS\nc\n");
}

#[test]
fn standalone_merge_tool_with_nothing_in_the_merged_file_yet_merges_as_usual() {
    let d = tempfile::tempdir().unwrap();
    let (mode, merged) = merge_mode_with(&d, b"a\nb\nc\n", b"a\nOURS\nc\n", b"a\nTHEIRS\nc\n", b"");
    let mut h = harness(mode);
    find(&mut h, "Take theirs");
    click(&mut h, "Take theirs");
    click(&mut h, "Save and finish");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read_to_string(&merged).unwrap(), "a\nTHEIRS\nc\n");
}

/// As in the repository: git's mergetool can be started on a file already edited by hand.
#[test]
fn standalone_merge_tool_keeps_a_hand_edit_in_the_merged_file() {
    let d = tempfile::tempdir().unwrap();
    let edited = b"HAND\n<<<<<<< local\nOURS\n=======\nTHEIRS\n>>>>>>> remote\nc\n";
    let (mode, merged) = merge_mode_with(&d, b"a\nb\nc\n", b"a\nOURS\nc\n", b"a\nTHEIRS\nc\n", edited);
    let mut h = harness(mode);
    find(&mut h, "Take theirs");
    click(&mut h, "Take theirs");
    click(&mut h, "Save and finish");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read_to_string(&merged).unwrap(), "HAND\nTHEIRS\nc\n");
}

#[test]
fn standalone_merge_tool_cancel_leaves_the_file() {
    let d = tempfile::tempdir().unwrap();
    let (mode, merged) = merge_mode(&d, b"a\n", b"x\n", b"y\n");
    let before = std::fs::read(&merged).unwrap();
    let mut h = harness(mode);
    click(&mut h, "Cancel merge");
    assert_eq!(h.state().result, Some((1, None)));
    assert_eq!(std::fs::read(&merged).unwrap(), before);
}

#[test]
fn standalone_merge_tool_never_truncates_binary_files() {
    let d = tempfile::tempdir().unwrap();
    let (mode, merged) = merge_mode(&d, b"\0base", b"\0local", b"\0remote");
    let mut h = harness(mode);
    find(&mut h, "Keep remote");
    assert!(h.query_by_label("Save and finish").is_none());
    click(&mut h, "Keep remote");
    assert_eq!(h.state().result, Some((0, None)));
    assert_eq!(std::fs::read(&merged).unwrap(), b"\0remote");
}

#[test]
fn standalone_merge_tool_keeps_non_utf8_bytes() {
    let d = tempfile::tempdir().unwrap();
    let (mode, merged) = merge_mode(&d, b"caf\xe9\n", b"caf\xe9 l\n", b"caf\xe9 r\n");
    let mut h = harness(mode);
    click(&mut h, "Keep local");
    assert_eq!(std::fs::read(&merged).unwrap(), b"caf\xe9 l\n");
}

#[test]
fn helper_windows_have_their_own_titlebar() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("msg");
    std::fs::write(&file, "text\n").unwrap();
    let app = HelperApp::new(Mode::Editor { path: file, text: "text\n".into() });
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(680.0, 474.0)).with_max_steps(5).build_ui_state(|ui, a: &mut HelperApp| a.frame(ui), app);
    h.step();
    h.step();
    for name in ["Minimize window", "Maximize window", "Close window"] {
        assert!(h.query_by_label(name).is_some(), "{name}");
    }
    let prompt = HelperApp::new(Mode::Askpass { prompt: "Password".into(), value: String::new(), secret: true, confirm: false });
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(460.0, 204.0)).with_max_steps(5).build_ui_state(|ui, a: &mut HelperApp| a.frame(ui), prompt);
    h.step();
    h.step();
    assert!(h.query_by_label("Close window").is_some());
    assert!(h.query_by_label("Maximize window").is_none(), "a prompt only closes");
}
