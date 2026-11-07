//! Headless end-to-end tests: drive the real UI by clicking widgets, then verify
//! the outcome with git itself. Temp repositories only; remotes are local bare repos.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::action::Action;
use gitem::app::GitemApp;
use gitem::forms;
use gitem::views::Tab;
use gitem_core::repo::DiffSource;
use gitem_core::{GitExe, LogQuery, ops};

/// Only for noticing that something hangs, so a slow machine must never reach it.
/// A loaded runner can make these tests ten times slower than usual.
const PATIENCE: Duration = Duration::from_secs(180);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

fn real_git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git");
    // Use the actual built binary as git's helper, as in production.
    g.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
    g
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = real_git();
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }
    fn subjects(&self) -> Vec<String> {
        self.git(&["log", "--format=%s"]).lines().map(String::from).collect()
    }
}

fn harness<'a>(f: &Fx) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings { details_on_open: false, ..Default::default() });
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    builder.build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    )
}

fn pump(h: &mut Harness<GitemApp>, cond: impl Fn(&GitemApp) -> bool, what: &str) {
    let t = Instant::now();
    loop {
        h.step();
        if cond(h.state()) {
            h.step();
            return;
        }
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Nothing is working in the background: no task on a thread, no git write.
fn quiet(h: &Harness<GitemApp>) -> bool {
    gitem::task::running(&h.ctx) == 0 && h.state().ops_idle()
}

/// Step until all background work has finished and its results are on screen.
fn settle(h: &mut Harness<GitemApp>) {
    pump(h, |a| a.repos.first().is_some_and(|r| r.data.status.get().is_some() && r.data.refs.get().is_some()), "repo data");
    // The file watcher reports changes at most every 400 ms, and only then does a
    // refresh start. That's the one thing here with no signal to wait on.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(500) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_until_quiet(h);
}

/// A finished task lands on the next frame and may start another, and new windows
/// spend their first frames measuring themselves. So it takes several quiet frames
/// in a row, for as long as a slow machine needs to get there.
fn wait_until_quiet(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut quiet_frames = 0;
    while quiet_frames < 5 {
        h.step();
        quiet_frames = if quiet(h) { quiet_frames + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// For checking that something didn't happen. An op started by mistake is queued
/// or running until it ends, so a slow one can't slip past as nothing having happened.
fn assert_no_op_started(h: &mut Harness<GitemApp>, what: &str) {
    for _ in 0..5 {
        h.step();
        assert!(h.state().ops_idle(), "{what}");
    }
}

fn click(h: &mut Harness<GitemApp>, label: &str) {
    pump_for_label(h, label);
    // A dialog takes no clicks for a moment after it appears. A person never notices.
    h.step();
    h.get_by_label(label).click();
    h.step();
    h.step();
}

/// The commit box's AI actions.
fn open_ai_menu(h: &mut Harness<GitemApp>) {
    let label = format!("AI {}", gitem::chrome::icon::CARET_DOWN);
    pump_for_label(h, &label);
    for _ in 0..5 {
        h.step();
    }
    click(h, &label);
    pump_for_label(h, "Generate with AI");
}

fn pump_for_label(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_by_label(label).is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Views report a failed load inline, in an `error_box`, and not in the error dialog.
fn error_boxes(h: &Harness<GitemApp>) -> Vec<String> {
    use egui_kittest::kittest::NodeT;
    h.query_all_by_label_contains(gitem::widgets::ERROR_LABEL).map(|n| n.accesskit_node().value().unwrap_or_default()).collect()
}

fn no_errors(h: &Harness<GitemApp>) {
    assert!(h.state().errors_showing().is_empty(), "an error dialog is showing");
    let boxes = error_boxes(h);
    assert!(boxes.is_empty(), "a view is showing an error: {boxes:?}");
}

fn all_tabs() -> Vec<Tab> {
    vec![
        Tab::home(),
        Tab::graph(),
        Tab::changes(),
        Tab::Launchpad(Box::default()),
        Tab::Search(Box::default()),
        Tab::search("=:first"),
        Tab::compare("main~1", "main"),
        Tab::Worktrees(Box::default()),
        Tab::Contributors(Box::default()),
        Tab::Reflog(Box::default()),
        Tab::rebase(Some("HEAD~1".into())),
        Tab::Bisect(Box::default()),
        Tab::Patches(Box::default()),
        Tab::Composer(Box::default()),
        Tab::Tools(Box::default()),
        Tab::Config(Box::default()),
        Tab::Credentials(Box::default()),
        Tab::Console(Box::default()),
        Tab::Settings(Box::default()),
        Tab::file("a.txt", None, Some(2)),
        Tab::file("a.txt", Some("HEAD".into()), None),
        Tab::file_history("a.txt"),
        Tab::line_history("a.txt", 1, 1),
        Tab::visual("a.txt"),
        Tab::tree("HEAD"),
        Tab::Agents(Box::default()),
        Tab::Review(Box::default()),
        Tab::diff(DiffSource::Unstaged, None),
        Tab::diff(DiffSource::Staged, None),
        Tab::diff(DiffSource::Commit("HEAD".into()), None),
        Tab::diff(DiffSource::Untracked, Some("untracked.txt".into())),
    ]
}

/// Each view opens as the active tab, with its background load allowed to finish.
fn visit_all(h: &mut Harness<GitemApp>) {
    for tab in all_tabs() {
        let title = tab.title();
        h.state_mut().repos[0].open_tab(tab);
        wait_until_quiet(h);
        assert!(h.state().errors_showing().is_empty(), "error showing after opening {title}");
        let boxes = error_boxes(h);
        assert!(boxes.is_empty(), "{title} shows an error: {boxes:?}");
    }
}

fn sample_history(f: &Fx) {
    f.write("a.txt", "one\ntwo\nthree\n");
    f.commit("first");
    f.write("a.txt", "one\nTWO\nthree\n");
    f.write("b.txt", "fn main() {}\n");
    f.commit("second");
}

#[test]
fn every_view_renders_on_an_ordinary_repo() {
    let f = Fx::new();
    sample_history(&f);
    f.write("a.txt", "one\nTWO\nthree\nmore\n");
    f.write("untracked.txt", "new\n");
    f.git(&["tag", "v1"]);
    f.git(&["branch", "side"]);
    let mut h = harness(&f);
    settle(&mut h);
    visit_all(&mut h);
}

#[test]
fn every_view_renders_on_an_empty_repo() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    // What the rail offers. The rest of `all_tabs` names a commit or a file, which an
    // empty repository doesn't have, so failing to load those is correct.
    let tabs = [
        Tab::home(),
        Tab::graph(),
        Tab::changes(),
        Tab::Launchpad(Box::default()),
        Tab::Search(Box::default()),
        Tab::Worktrees(Box::default()),
        Tab::Contributors(Box::default()),
        Tab::Reflog(Box::default()),
        Tab::Bisect(Box::default()),
        Tab::Patches(Box::default()),
        Tab::Composer(Box::default()),
        Tab::Tools(Box::default()),
        Tab::Config(Box::default()),
        Tab::Credentials(Box::default()),
        Tab::Console(Box::default()),
        Tab::Settings(Box::default()),
        Tab::Agents(Box::default()),
        Tab::Review(Box::default()),
        Tab::diff(DiffSource::Unstaged, None),
        Tab::diff(DiffSource::Staged, None),
    ];
    for tab in tabs {
        let title = tab.title();
        h.state_mut().repos[0].open_tab(tab);
        wait_until_quiet(&mut h);
        let boxes = error_boxes(&h);
        assert!(boxes.is_empty(), "{title} shows an error: {boxes:?}");
        no_errors(&h);
    }
}

/// A smoke test is only as good as its check, so the check gets seen failing.
#[test]
fn a_view_that_fails_to_load_is_noticed() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Commit("no-such-revision".into()), None));
    let t = Instant::now();
    while error_boxes(&h).is_empty() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the failed load never showed");
        std::thread::sleep(Duration::from_millis(15));
    }
    let boxes = error_boxes(&h);
    assert!(boxes[0].contains("no-such-revision") || boxes[0].contains("bad"), "{boxes:?}");
}

#[test]
fn switches_theme_without_trouble() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    for t in gitem_core::settings::ThemeChoice::ALL {
        h.state_mut().settings.theme = t;
        for _ in 0..4 {
            h.step();
        }
    }
    no_errors(&h);
}

#[test]
fn commit_through_the_changes_view() {
    let f = Fx::new();
    sample_history(&f);
    f.write("c.txt", "new\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    let t = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    t.focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("Add c file");
    h.step();
    h.step();
    click(&mut h, "Stage all and commit 1 file");
    let t0 = Instant::now();
    while f.subjects()[0] != "Add c file" {
        h.step();
        assert!(t0.elapsed() < PATIENCE, "commit never landed: {:?}", f.subjects());
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(f.git(&["status", "--short"]), "");
    no_errors(&h);
}

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn() -> bool) {
    let t = Instant::now();
    while !cond() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
    h.step();
}

#[test]
fn stage_a_hunk_from_the_diff_view() {
    let f = Fx::new();
    let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("f.txt", &base);
    f.commit("base");
    f.write("f.txt", &base.replace("line 2\n", "line 2 x\n").replace("line 25\n", "line 25 y\n"));
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("f.txt".into())));
    pump_for_label(&mut h, "Stage file");
    // Two hunks, so two buttons; stage the first.
    let t = Instant::now();
    while h.query_all_by_label("Stage hunk").count() < 2 {
        h.step();
        assert!(t.elapsed() < PATIENCE);
    }
    h.query_all_by_label("Stage hunk").next().unwrap().click();
    h.step();
    wait_for(&mut h, "hunk staged", || !f.git(&["diff", "--cached"]).is_empty());
    let staged = f.git(&["diff", "--cached"]);
    assert!(staged.contains("line 2 x"), "{staged}");
    assert!(!staged.contains("line 25 y"), "only one hunk staged: {staged}");
    assert!(f.git(&["diff"]).contains("line 25 y"));
    no_errors(&h);
}

fn make_conflict(f: &Fx) {
    f.write("c.txt", "top\nmiddle\nbottom\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("c.txt", "TOP-SIDE\nmiddle\nbottom\n");
    f.commit("side change");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "TOP-MAIN\nmiddle\nbottom\n");
    f.commit("main change");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok(), "merge should conflict");
}

#[test]
fn resolve_a_conflict_with_the_merge_tool_and_finish_the_merge() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    // The operation banner offers Resolve and Abort.
    pump_for_label(&mut h, "Abort");
    click(&mut h, "Resolve");
    pump_for_label(&mut h, "Take theirs");
    click(&mut h, "Take theirs");
    pump_for_label(&mut h, "Save and mark resolved");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "conflict cleared", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(std::fs::read_to_string(f.root.join("c.txt")).unwrap(), "TOP-SIDE\nmiddle\nbottom\n");
    pump_for_label(&mut h, "Continue");
    click(&mut h, "Continue");
    wait_for(&mut h, "merge committed", || !f.root.join(".git/MERGE_HEAD").exists());
    assert_eq!(f.git(&["log", "-1", "--format=%p"]).split_whitespace().count(), 2, "merge commit has two parents");
    no_errors(&h);
}

#[test]
fn every_view_renders_during_a_conflict() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    make_conflict(&f);
    f.write("a.txt", "x\n");
    f.write("untracked.txt", "new\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::merge("c.txt"));
    pump_for_label(&mut h, "Take ours");
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    visit_all(&mut h);
    no_errors(&h);
}

#[test]
fn interactive_rebase_through_the_real_sequence_editor_helper() {
    let f = Fx::new();
    f.write("base", "b\n");
    f.commit("base");
    let base = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    for (n, m) in [("x", "one"), ("y", "two"), ("z", "three")] {
        f.write(n, "1\n");
        f.commit(m);
    }
    let mut h = harness(&f);
    settle(&mut h);
    let rebase = gitem::views::rebase::RebaseView::new(Some(base));
    h.state_mut().repos[0].open_tab(Tab::Rebase(Box::new(rebase)));
    pump_for_label(&mut h, "Rebase 3 commits");
    // Drop "two", the second row, through its action box. Rows list oldest first.
    let action_boxes = || egui_kittest::kittest::by().role(egui::accesskit::Role::ComboBox).value("pick");
    let t = Instant::now();
    while h.query_all(action_boxes()).count() < 3 {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the plan never showed");
    }
    let box_y = h.get_by_label("two").rect().center().y;
    h.query_all(action_boxes()).find(|n| (n.rect().center().y - box_y).abs() < 14.0).expect("the row of \"two\" has an action box").click();
    h.step();
    click_last(&mut h, "drop");
    click(&mut h, "Rebase 3 commits");
    wait_for(&mut h, "rebase done", || f.subjects().len() == 3);
    assert_eq!(f.subjects(), vec!["three", "one", "base"]);
    assert!(!f.root.join("y").exists());
    no_errors(&h);
}

#[test]
fn a_form_creates_a_branch() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    pump_for_label(&mut h, "Create");
    // Forms focus their first field.
    h.step();
    h.event(egui::Event::Text("feature/from-form".into()));
    h.step();
    h.step();
    click(&mut h, "Create");
    wait_for(&mut h, "branch created", || f.git(&["branch", "--show-current"]).trim() == "feature/from-form");
    no_errors(&h);
}

#[test]
fn a_form_rejects_missing_input() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_tag(None)));
    // New windows spend their first frame invisible, measuring themselves.
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Create");
    pump_for_label(&mut h, "A name is required.");
    assert!(f.git(&["tag"]).is_empty());
}

#[test]
fn destructive_ops_wait_for_confirmation() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "doomed"]);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let confirm = |h: &mut Harness<GitemApp>| {
        h.state_mut().apply_action(&ctx, Action::Confirm { title: "Delete branch".into(), body: "Delete doomed?".into(), label: "Yes, delete".into(), op: ops::branch_delete("doomed", false) });
        h.step();
    };
    confirm(&mut h);
    pump_for_label(&mut h, "Yes, delete");
    assert_no_op_started(&mut h, "nothing happens before confirming");
    assert!(f.git(&["branch"]).contains("doomed"), "nothing happens before confirming");
    click(&mut h, "Cancel");
    assert_no_op_started(&mut h, "cancel leaves it alone");
    assert!(f.git(&["branch"]).contains("doomed"), "cancel leaves it alone");
    confirm(&mut h);
    click(&mut h, "Yes, delete");
    wait_for(&mut h, "branch deleted", || !f.git(&["branch"]).contains("doomed"));
}

#[test]
fn failed_ops_show_the_error_and_drop_queued_followups() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::checkout("no-such-branch")));
    h.state_mut().apply_action(&ctx, Action::Run(ops::tag_create("must-not-exist", None, None, false)));
    pump_for_error(&mut h);
    assert_no_op_started(&mut h, "the queued step after a failure must not run");
    assert!(f.git(&["tag"]).is_empty(), "the queued step after a failure must not run");
    click(&mut h, "Dismiss");
    no_errors(&h);
}

#[test]
fn external_edits_refresh_the_status_on_their_own() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.state().repos[0].data.status.get().unwrap().is_clean());
    f.write("appeared.txt", "hi\n");
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| !s.is_clean()), "the watcher to notice the new file");
    no_errors(&h);
}

#[test]
fn the_palette_filters_and_runs_a_command() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::OpenPalette);
    h.step();
    h.step();
    h.event(egui::Event::Text("reflog".into()));
    h.step();
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    let opened = h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Reflog(_)));
    assert!(opened, "Enter runs the best match");
}

#[test]
fn opening_a_folder_that_is_not_a_repo_reports_it() {
    let f = Fx::new();
    let not_repo = tempfile::tempdir().unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let p: &Path = not_repo.path();
    h.state_mut().apply_action(&ctx, Action::OpenRepo(p.to_path_buf()));
    pump_for_error(&mut h);
    assert_eq!(h.state().repos.len(), 1);
}

#[test]
fn file_view_navigates_revisions() {
    let f = Fx::new();
    f.write("n.txt", "1\n");
    f.commit("rev one");
    f.write("n.txt", "2\n");
    f.commit("rev two");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("n.txt", None, None));
    pump_for_label(&mut h, "◀ Older");
    let rev_of = |a: &GitemApp| a.repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::File(v) = t { Some(v.rev.clone()) } else { None }).flatten();
    click(&mut h, "◀ Older");
    let two = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let one = f.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    pump(&mut h, |a| rev_of(a).as_deref() == Some(two.as_str()), "first step back");
    click(&mut h, "◀ Older");
    pump(&mut h, |a| rev_of(a).as_deref() == Some(one.as_str()), "second step back");
    click(&mut h, "Newer ▶");
    pump(&mut h, |a| rev_of(a).as_deref() == Some(two.as_str()), "step forward");
    no_errors(&h);
}

#[test]
fn log_query_helpers_used_by_views_work() {
    let f = Fx::new();
    sample_history(&f);
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let c = repo.log(&LogQuery { all: true, excludes: vec!["refs/tags/*".into()], ..Default::default() }).unwrap();
    assert_eq!(c.len(), 2);
}


/// The middle of `n` frames with the window idle, in milliseconds. The harness doesn't
/// paint, so this is the view's own work, and the middle ignores a busy moment.
fn median_frame_ms(h: &mut Harness<GitemApp>, n: usize) -> f64 {
    let mut times: Vec<f64> = (0..n)
        .map(|_| {
            let t = Instant::now();
            h.step();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[n / 2]
}

/// What a frame may cost with a very large repository open: most of one 60 Hz frame,
/// when these take under a millisecond on an idle machine. That leaves room for a
/// test run with every core busy, and is still far below anything a person would miss
/// only if the view's work stays proportional to what's on screen.
const FRAME_BOUND_MS: f64 = 12.0;

/// Git commands run so far, so a test can see the idle window starts none.
fn commands_so_far(f: &Fx) -> usize {
    f.git.log.lock().unwrap().len()
}

fn assert_quiet_and_quick(h: &mut Harness<GitemApp>, f: &Fx, what: &str) {
    wait_until_quiet(h);
    let before = commands_so_far(f);
    let frame = median_frame_ms(h, 120);
    eprintln!("{what}: {frame:.2} ms per frame");
    assert!(frame < FRAME_BOUND_MS, "{what}: {frame:.1} ms per frame");
    let ran = commands_so_far(f).saturating_sub(before);
    assert!(ran <= 2, "{what}: {ran} git commands in 120 idle frames, which should cost none");
}

#[test]
fn large_history_stays_responsive() {
    let f = Fx::new();
    let n = 30_000;
    let mut s = String::new();
    for i in 0..n {
        let mark = i + 1;
        let parent = if i > 0 { format!("from :{}\n", i) } else { String::new() };
        // Every 10th commit merges a side commit made just before it.
        let merge = if i > 20 && i % 10 == 0 { format!("merge :{}\n", i - 5) } else { String::new() };
        s.push_str(&format!("commit refs/heads/main\nmark :{mark}\ncommitter T <t@x> {} +0000\ndata 8\nc{:06}\n{parent}{merge}M 100644 inline f{}.txt\ndata 2\n{}\n\n", 1_500_000_000 + i * 600, i % 1_000_000, i % 50, i % 10));
    }
    let op = gitem_core::ops::GitOp::new("import").step(&["fast-import", "--quiet"]).stdin(s.into_bytes());
    op.run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    f.git(&["reset", "-q", "--hard", "main"]);

    let ctx = egui::Context::default();
    let mut settings = gitem_core::settings::Settings::default();
    settings.graph_page_size = 30_000;
    let mut app = GitemApp::with_settings(&ctx, settings);
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let mut h: Harness<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    );
    // Timed from the very first frame, which opens the repository. Settling first
    // would absorb the load and report nothing.
    let loaded = |a: &GitemApp| a.repos.first().and_then(|r| r.dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Graph(g) = t { Some(g.loaded_commits()) } else { None })).unwrap_or(0);
    let load_start = Instant::now();
    pump(&mut h, |a| loaded(a) == n as usize, "the commits to load");
    let load = load_start.elapsed();
    eprintln!("loaded {} commits in {load:?}", loaded(h.state()));
    assert!(load < Duration::from_secs(20), "{n} commits took {load:?} to load");
    settle(&mut h);
    assert_quiet_and_quick(&mut h, &f, "graph of 30,000 commits");
}

#[test]
fn a_confirmation_runs_in_the_repository_it_was_asked_about() {
    let a = Fx::new();
    sample_history(&a);
    a.git(&["branch", "doomed"]);
    let b = Fx::new();
    sample_history(&b);
    b.git(&["branch", "doomed"]);
    let mut h = harness(&a);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Confirm { title: "Delete".into(), body: "Delete?".into(), label: "Yes, delete".into(), op: ops::branch_delete("doomed", false) });
    h.step();
    // Switch to another repository while the dialog is open.
    h.state_mut().open_repo(&ctx, &b.root, false);
    assert_eq!(h.state().active, 1);
    pump_for_label(&mut h, "Yes, delete");
    h.step();
    h.step();
    click(&mut h, "Yes, delete");
    wait_for(&mut h, "branch deleted in the first repo", || !a.git(&["branch"]).contains("doomed"));
    assert!(b.git(&["branch"]).contains("doomed"), "the repo being shown must be untouched");
    assert_eq!(h.state().active, 1, "the view stays where it was");
}

#[test]
fn a_form_runs_in_the_repository_it_was_opened_for() {
    let a = Fx::new();
    sample_history(&a);
    let b = Fx::new();
    sample_history(&b);
    let mut h = harness(&a);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_tag(None)));
    h.step();
    h.state_mut().open_repo(&ctx, &b.root, false);
    for _ in 0..3 {
        h.step();
    }
    h.event(egui::Event::Text("v9".into()));
    h.step();
    h.step();
    click(&mut h, "Create");
    wait_for(&mut h, "tag created", || a.git(&["tag"]).contains("v9"));
    assert!(b.git(&["tag"]).is_empty());
}

#[test]
fn non_utf8_conflicts_are_never_rewritten_as_text() {
    let f = Fx::new();
    let bytes = |s: &[u8]| s.to_vec();
    std::fs::write(f.root.join("t.txt"), bytes(b"caf\xe9 one\nkeep\n")).unwrap();
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    std::fs::write(f.root.join("t.txt"), bytes(b"caf\xe9 side\nkeep\n")).unwrap();
    f.commit("side");
    f.git(&["switch", "-q", "main"]);
    std::fs::write(f.root.join("t.txt"), bytes(b"caf\xe9 main\nkeep\n")).unwrap();
    f.commit("main");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok());
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::merge("t.txt"));
    pump_for_label(&mut h, "Keep theirs…");
    assert!(h.query_by_label("Save and mark resolved").is_none(), "no text merge is offered");
    click(&mut h, "Keep theirs…");
    pump_for_label(&mut h, "Keep");
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Keep");
    wait_for(&mut h, "conflict cleared", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(std::fs::read(f.root.join("t.txt")).unwrap(), b"caf\xe9 side\nkeep\n", "bytes preserved exactly");
}

#[test]
fn merge_tab_reloads_when_the_conflict_changes_and_refuses_stale_saves() {
    let f = Fx::new();
    f.write("c.txt", "top\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "mine"]);
    f.write("c.txt", "mine one\n");
    f.commit("first");
    f.write("c.txt", "mine two\n");
    f.commit("second");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "main\n");
    f.commit("main change");
    f.git(&["switch", "-q", "mine"]);
    assert!(!f.git.run_raw(Some(&f.root), &["rebase", "main"]).unwrap().ok(), "first step conflicts");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::merge("c.txt"));
    // During a rebase the sides are named for what they really are.
    pump_for_label(&mut h, "Take your commit");
    assert!(h.query_by_label("Take theirs").is_none());
    // Keeping upstream here means the next commit conflicts with it as well.
    click(&mut h, "Take upstream");
    pump_for_label(&mut h, "Save and mark resolved");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "resolved", || !f.git(&["status", "--short"]).contains("UU"));
    pump_for_label(&mut h, "This file is no longer in conflict");
    assert_eq!(std::fs::read_to_string(f.root.join("c.txt")).unwrap(), "main\n");
    // The next step conflicts on the same file, with new versions.
    let cont = f.git.run_opts(Some(&f.root), &["rebase".to_string(), "--continue".to_string()], &gitem_core::runner::RunOpts { env: vec![("GIT_EDITOR".into(), "true".into())], ..Default::default() }, &mut |_| {}).unwrap();
    assert!(!cont.ok(), "second step conflicts too: {}", cont.text());
    let t = Instant::now();
    while h.query_by_label("Take your commit").is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "tab did not reload for the new conflict");
        std::thread::sleep(Duration::from_millis(15));
    }
    click(&mut h, "Take your commit");
    pump_for_label(&mut h, "Save and mark resolved");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "resolved again", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(std::fs::read_to_string(f.root.join("c.txt")).unwrap(), "mine two\n", "the second commit's content, not the first resolution");
}

/// Two separate conflicts in one file, in a merge. Takes the lines for each side.
fn make_two_conflicts(f: &Fx) {
    f.write("c.txt", "one\nkeep 1\nkeep 2\nkeep 3\nkeep 4\ntwo\nend\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("c.txt", "A-side\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-side\nend\n");
    f.commit("side change");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "A-main\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-main\nend\n");
    f.commit("main change");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok(), "merge should conflict");
}

fn c_txt(f: &Fx) -> String {
    std::fs::read_to_string(f.root.join("c.txt")).unwrap()
}

fn open_merge(h: &mut Harness<GitemApp>, label: &str) {
    settle(h);
    h.state_mut().repos[0].open_tab(Tab::merge("c.txt"));
    pump_for_label(h, label);
}

/// The merge tool used to rebuild from the index and save over this.
#[test]
fn a_hand_edit_in_the_file_is_seen_and_survives_the_merge_tool() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let on_disk = c_txt(&f);
    // Fix the first conflict by hand, leaving the second.
    let start = on_disk.find("<<<<<<<").unwrap();
    let end = on_disk.find(">>>>>>>").unwrap();
    let end = end + on_disk[end..].find('\n').unwrap() + 1;
    f.write("c.txt", &format!("{}HAND-WRITTEN\n{}", &on_disk[..start], &on_disk[end..]));

    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    pump_for_label(&mut h, "Start over from the three versions…");
    click(&mut h, "All ours");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "HAND-WRITTEN\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-main\nend\n");
}

#[test]
fn an_earlier_partial_save_is_seen_by_a_new_tab() {
    let f = Fx::new();
    make_two_conflicts(&f);
    {
        let mut h = harness(&f);
        open_merge(&mut h, "2 of 2 conflicts left in this file");
        click_nth(&mut h, "Take ours", 0);
        click(&mut h, "Save with markers");
        pump_for_label(&mut h, "Saved");
        assert_eq!(f.git(&["status", "--short"]).trim(), "UU c.txt", "still unresolved");
    }
    let saved = c_txt(&f);
    assert!(saved.starts_with("A-main\n") && saved.contains("<<<<<<<"), "{saved}");
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    click(&mut h, "All theirs");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "A-main\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-side\nend\n", "the first choice stands");
}

#[test]
fn an_untouched_conflict_opens_as_a_plain_merge() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "2 of 2 conflicts left in this file");
    assert!(h.query_by_label("Start over from the three versions…").is_none());
}

#[test]
fn starting_over_asks_and_only_then_uses_the_three_versions() {
    let f = Fx::new();
    make_two_conflicts(&f);
    f.write("c.txt", "HAND-WRITTEN\n");
    let mut h = harness(&f);
    open_merge(&mut h, "Start over from the three versions…");
    click(&mut h, "Start over from the three versions…");
    click(&mut h, "Keep the file's state");
    assert_eq!(c_txt(&f), "HAND-WRITTEN\n");
    click(&mut h, "Start over from the three versions…");
    click(&mut h, "Start over");
    pump_for_label(&mut h, "2 of 2 conflicts left in this file");
    assert_eq!(c_txt(&f), "HAND-WRITTEN\n", "nothing is written until a save");
    click(&mut h, "All theirs");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "A-side\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-side\nend\n");
}

#[test]
fn saving_over_a_file_changed_behind_the_tab_asks_first() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "2 of 2 conflicts left in this file");
    click(&mut h, "All ours");
    f.write("c.txt", "EDITED ELSEWHERE\n");
    click(&mut h, "Save and mark resolved");
    pump_for_label(&mut h, "Replace the file");
    assert_no_op_started(&mut h, "nothing is marked resolved yet");
    assert_eq!(c_txt(&f), "EDITED ELSEWHERE\n");

    click(&mut h, "Keep this tab as it is");
    assert_eq!(c_txt(&f), "EDITED ELSEWHERE\n");

    click(&mut h, "Save and mark resolved");
    click(&mut h, "Reload from disk");
    pump_for_label(&mut h, "Start over from the three versions…");
    assert_eq!(c_txt(&f), "EDITED ELSEWHERE\n");

    // A plain merge again, then changed once more, and this time replaced.
    click(&mut h, "Start over from the three versions…");
    click(&mut h, "Start over");
    click(&mut h, "All ours");
    f.write("c.txt", "EDITED AGAIN\n");
    click(&mut h, "Save and mark resolved");
    click(&mut h, "Replace the file");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "A-main\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nB-main\nend\n");
}

#[test]
fn a_tabs_own_save_is_not_taken_for_someone_elses_edit() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "2 of 2 conflicts left in this file");
    click_nth(&mut h, "Take ours", 0);
    click(&mut h, "Save with markers");
    pump_for_label(&mut h, "Saved");
    click(&mut h, "All theirs");
    click(&mut h, "Save and mark resolved");
    assert!(h.query_by_label("Replace the file").is_none());
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
}

#[test]
fn switching_off_plain_editing_keeps_what_was_typed() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    click(&mut h, "Edit text");
    let input = egui::accesskit::Role::MultilineTextInput;
    pump_for_role(&mut h, input);
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("TYPED\n");
    h.step();
    h.step();
    click(&mut h, "Edit text");
    // Still one conflict to decide, and the typed line is in the result.
    pump_for_label(&mut h, "1 of 1 conflict left in this file");
    click(&mut h, "All theirs");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    let saved = c_txt(&f);
    assert!(saved.contains("TYPED\n") && saved.contains("TOP-SIDE\n") && !saved.contains("<<<<<<<"), "{saved}");
}

#[test]
fn plain_editing_with_broken_markers_stays_put_rather_than_lose_the_text() {
    let f = Fx::new();
    make_conflict(&f);
    f.write("c.txt", "<<<<<<< HEAD\nhalf a conflict\n");
    let mut h = harness(&f);
    open_merge(&mut h, "Start over from the three versions…");
    let input = egui::accesskit::Role::MultilineTextInput;
    pump_for_role(&mut h, input);
    click(&mut h, "Edit text");
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_role(input).is_some(), "it stays on the text");
    assert_eq!(c_txt(&f), "<<<<<<< HEAD\nhalf a conflict\n");
}

#[test]
fn inline_edit_undo_and_base_decide_a_conflict() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    let input = egui::accesskit::Role::MultilineTextInput;
    // Edit starts from nothing while undecided. Apply makes the text the resolution.
    click(&mut h, "Edit");
    pump_for_role(&mut h, input);
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("INLINE");
    h.step();
    h.step();
    click(&mut h, "Apply");
    pump_for_label(&mut h, "All 1 conflict resolved");
    // Undo reopens it.
    click(&mut h, "Undo");
    pump_for_label(&mut h, "1 of 1 conflict left in this file");
    // The Base pane opens beside the others, and Base resolves to its text.
    let toolbar_base = h.query_all_by_label("Base").next().unwrap().rect();
    click_at(&mut h, toolbar_base.center());
    h.step();
    let conflict_base = h.query_all_by_label("Base").nth(1).unwrap();
    conflict_base.click();
    h.step();
    h.step();
    pump_for_label(&mut h, "All 1 conflict resolved");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "top\nmiddle\nbottom\n");
}

#[test]
fn an_inline_edit_saved_with_its_own_text() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    let input = egui::accesskit::Role::MultilineTextInput;
    click(&mut h, "Edit");
    pump_for_role(&mut h, input);
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("INLINE");
    h.step();
    h.step();
    click(&mut h, "Apply");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "INLINE\nmiddle\nbottom\n");
}

#[test]
fn delete_the_file_resolves_a_conflict_with_a_deleted_side() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.git(&["rm", "-q", "c.txt"]);
    f.commit("delete it");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "changed\n");
    f.commit("change it");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok());
    let mut h = harness(&f);
    open_merge(&mut h, "Delete the file…");
    assert!(h.query_by_label("Keep theirs…").is_none(), "that side has no file to keep");
    click(&mut h, "Delete the file…");
    pump_for_label(&mut h, "Delete");
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Delete");
    wait_for(&mut h, "the file to be gone", || !f.root.join("c.txt").exists() && f.git(&["status", "--short"]).trim() == "D  c.txt");
    no_errors(&h);
}

/// An empty file is a version like any other, and used to read as deleted.
#[test]
fn an_empty_file_on_one_side_is_not_called_deleted() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("c.txt", "changed\n");
    f.commit("change it");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "");
    f.commit("empty it");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok());
    let mut h = harness(&f);
    open_merge(&mut h, "Take ours");
    assert!(h.query_by_label("Delete the file").is_none());
    click(&mut h, "Take ours");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "");
}

#[test]
fn a_real_autocrlf_file_keeps_its_line_ends_through_the_merge_tool() {
    let f = Fx::new();
    f.git(&["config", "core.autocrlf", "true"]);
    f.write("c.txt", "top\r\nmiddle\r\nbottom\r\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("c.txt", "TOP-SIDE\r\nmiddle\r\nbottom\r\n");
    f.commit("side change");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "TOP-MAIN\r\nmiddle\r\nbottom\r\n");
    f.commit("main change");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok());
    assert!(c_txt(&f).contains("\r\n"), "git writes the conflict with the file's own ending");
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    assert!(h.query_by_label("Start over from the three versions…").is_none(), "line ends alone don't count as an edit");
    click(&mut h, "All theirs");
    click(&mut h, "Save and mark resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert_eq!(c_txt(&f), "TOP-SIDE\r\nmiddle\r\nbottom\r\n");
}

#[test]
fn a_rebase_plan_refuses_to_run_if_head_moved() {
    let f = Fx::new();
    f.write("base", "b\n");
    f.commit("base");
    let base = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    f.write("x", "1\n");
    f.commit("one");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::rebase(Some(base.clone())));
    pump_for_label(&mut h, "Rebase 1 commit");
    // A new commit lands while the plan is open; the plan doesn't include it.
    f.write("y", "1\n");
    f.commit("two");
    for _ in 0..5 {
        h.step();
    }
    click(&mut h, "Rebase 1 commit");
    pump_for_label(&mut h, "HEAD moved since this plan was made, so it was not started.");
    assert_eq!(f.subjects(), vec!["two", "one", "base"], "no commit lost");
    click(&mut h, "Rebuild plan");
    pump_for_label(&mut h, "Rebase 2 commits");
}

#[test]
fn rebase_with_update_refs_keeps_dependent_branches_through_the_real_helper() {
    let f = Fx::new();
    f.write("base", "b\n");
    f.commit("base");
    let base = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    f.git(&["switch", "-q", "-c", "stack"]);
    f.write("a", "1\n");
    f.commit("stack commit");
    f.git(&["switch", "-q", "-c", "top"]);
    f.write("drop-me", "1\n");
    f.commit("droppable");
    f.write("c", "1\n");
    f.commit("top commit");
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let commits = forms::commits_since(&repo, Some(&base)).unwrap();
    let mut items: Vec<gitem_core::rebase::TodoItem> = commits.iter().map(gitem_core::rebase::TodoItem::from_commit).collect();
    items[1].action = gitem_core::rebase::RebaseAction::Drop;
    let o = ops::RebaseOpts { update_refs: true, ..Default::default() };
    let op = forms::start_interactive_rebase(&repo, Some(&base), &items, o).unwrap();
    op.run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    assert_eq!(f.subjects(), vec!["top commit", "stack commit", "base"]);
    let stack_parent = f.git(&["rev-parse", "stack"]);
    let expected = f.git(&["rev-parse", "HEAD~1"]);
    assert_eq!(stack_parent, expected, "the dependent branch still points into the stack");
}

#[test]
fn a_second_edit_to_a_modified_file_refreshes_an_open_diff() {
    let f = Fx::new();
    f.write("f.txt", "one\n");
    f.commit("base");
    f.write("f.txt", "two\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("f.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    let gen0 = h.state().repos[0].data.status_gen;
    // Status still reads " M f.txt", so its summary doesn't change.
    f.write("f.txt", "three\n");
    pump(&mut h, |a| a.repos[0].data.status_gen != gen0, "the change to be noticed");
    no_errors(&h);
}

#[test]
fn a_rejected_commit_keeps_the_message() {
    let f = Fx::new();
    sample_history(&f);
    f.write("c.txt", "new\n");
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho nope >&2\nexit 1\n").unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("Precious message");
    h.step();
    h.step();
    click(&mut h, "Stage all and commit 1 file");
    pump_for_error(&mut h);
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let kept = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Changes(c) = t { Some(c.message.clone()) } else { None }).unwrap();
    assert_eq!(kept, "Precious message");
    assert_eq!(f.subjects()[0], "second", "nothing was committed");
}

#[test]
fn context_menus_work_on_graph_rows_and_sidebar_branches() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "side"]);
    let mut h = harness(&f);
    settle(&mut h);
    // A graph row: subject text labels the row.
    pump_for_label(&mut h, "first");
    h.get_by_label("first").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Create ⏵");
    pump_for_label(&mut h, "Create tag here…");
    click(&mut h, "Create tag here…");
    for _ in 0..3 {
        h.step();
    }
    h.event(egui::Event::Text("from-menu".into()));
    h.step();
    h.step();
    click(&mut h, "Create");
    wait_for(&mut h, "tag from the context menu", || f.git(&["tag"]).contains("from-menu"));
    let first = f.git(&["rev-parse", "HEAD~1"]);
    assert_eq!(f.git(&["rev-list", "-n1", "from-menu"]), first, "tagged the commit that was right-clicked");

    // A sidebar branch.
    pump_for_label(&mut h, "side");
    h.get_by_label("side").click_secondary();
    h.step();
    h.step();
    pump_for_label(&mut h, "Check out");
    click(&mut h, "Check out");
    wait_for(&mut h, "switched", || f.git(&["branch", "--show-current"]).trim() == "side");
    no_errors(&h);
}

#[test]
fn undo_commit_from_the_context_menu() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "second");
    h.get_by_label("second").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Undo commit");
    pump_for_label(&mut h, "Take the last commit back? Its changes stay staged.");
    assert_eq!(f.subjects().len(), 2, "it asks first");
    wait_until_quiet(&mut h);
    click(&mut h, "Undo commit");
    wait_for(&mut h, "commit undone", || f.subjects() == vec!["first"]);
    assert!(f.git(&["status", "--short"]).contains("A  b.txt"), "changes stay staged");
}

#[test]
fn reword_from_the_menu_opens_a_plan_with_that_commit_marked() {
    let f = Fx::new();
    sample_history(&f);
    f.write("z", "1\n");
    f.commit("third");
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "second");
    h.get_by_label("second").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Rewrite history ⏵");
    click(&mut h, "Drop…");
    pump_for_label(&mut h, "Rebase 2 commits");
    let action = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Rebase(r) = t { r.items.iter().find(|i| i.text == "second").map(|i| i.action) } else { None });
    assert_eq!(action, Some(gitem_core::rebase::RebaseAction::Drop));
    assert_eq!(f.subjects().len(), 3, "nothing runs until the plan is confirmed");
}

#[test]
fn dragging_a_ref_onto_a_commit_offers_actions() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["tag", "v-drag"]);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "first");
    h.step();
    let source = h.get_by_label("second").rect();
    let target = h.get_by_label("first").rect();
    // The ref badges sit at the row's left edge.
    let from = source.left_center() + vec2(24.0, 0.0);
    let to = target.center();
    let press = |h: &Harness<GitemApp>, pressed: bool, p: egui::Pos2| {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    };
    h.event(egui::Event::PointerMoved(from));
    h.step();
    press(&h, true, from);
    h.step();
    h.event(egui::Event::PointerMoved(from + vec2(12.0, 6.0)));
    h.step();
    h.event(egui::Event::PointerMoved(to));
    h.step();
    h.step();
    press(&h, false, to);
    h.step();
    h.step();
    for _ in 0..3 {
        h.step();
    }
    pump_for_label(&mut h, "What should happen?");
    no_errors(&h);
}

#[test]
fn batch_selection_stages_only_what_was_picked() {
    let f = Fx::new();
    sample_history(&f);
    f.write("p1.txt", "1\n");
    f.write("p2.txt", "1\n");
    f.write("p3.txt", "1\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "p1.txt");
    h.get_by_label("p1.txt").click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    h.get_by_label("p3.txt").click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    pump_for_label(&mut h, "2 selected");
    click(&mut h, "Stage");
    wait_for(&mut h, "staged", || f.git(&["diff", "--cached", "--name-only"]).lines().count() == 2);
    let staged = f.git(&["diff", "--cached", "--name-only"]);
    assert!(staged.contains("p1.txt") && staged.contains("p3.txt") && !staged.contains("p2.txt"), "{staged}");
}

#[test]
fn changes_list_is_flat_until_the_tree_is_turned_on() {
    let f = Fx::new();
    sample_history(&f);
    f.write("src/views/one.rs", "1\n");
    f.write("src/views/two.rs", "2\n");
    f.write("src/main.rs", "3\n");
    f.write("top.txt", "4\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "src/main.rs");
    assert!(h.query_by_label("src").is_none(), "flat by default, with no folder rows");

    click(&mut h, "Show as tree");
    assert!(h.state().settings.changes_tree, "the choice is kept in settings");
    pump_for_label(&mut h, "src");
    assert!(h.query_by_label("views").is_some());
    assert!(h.query_by_label("src/views/one.rs").is_some(), "everything starts open");

    // Files in a tree do the same as in the list.
    h.get_by_label("top.txt").click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    pump_for_label(&mut h, "1 selected");
    click(&mut h, "Clear");

    click(&mut h, "views");
    assert!(h.query_by_label("src/views/one.rs").is_none(), "closing a folder hides its files");
    assert!(h.query_by_label("src/main.rs").is_some(), "and nothing else");
    click(&mut h, "views");
    assert!(h.query_by_label("src/views/one.rs").is_some());

    click(&mut h, "Collapse folders");
    assert!(h.query_by_label("src/main.rs").is_none());
    assert!(h.query_by_label("top.txt").is_some(), "a file outside folders stays");
    click(&mut h, "Expand folders");
    assert!(h.query_by_label("src/main.rs").is_some());

    click(&mut h, "Show as tree");
    pump_for_label(&mut h, "src/main.rs");
    assert!(h.query_by_label("views").is_none(), "and back to the flat list");
    no_errors(&h);
}

#[test]
fn a_folder_in_the_changes_tree_stages_and_unstages_as_one() {
    let f = Fx::new();
    sample_history(&f);
    f.write("src/views/one.rs", "1\n");
    f.write("src/main.rs", "2\n");
    f.write("top.txt", "3\n");
    let mut h = harness(&f);
    h.state_mut().settings.changes_tree = true;
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "views");

    h.get_by_label("src").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Stage 2 files");
    wait_for(&mut h, "folder staged", || f.git(&["diff", "--cached", "--name-only"]).lines().count() == 2);
    let staged = f.git(&["diff", "--cached", "--name-only"]);
    assert!(staged.contains("src/main.rs") && staged.contains("src/views/one.rs") && !staged.contains("top.txt"), "{staged}");

    // The same folder now shows under Staged, once the status has caught up.
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.entries.iter().filter(|e| e.is_staged()).count() == 2), "status to show the staged folder");
    pump_for_label(&mut h, "src");
    h.get_by_label("src").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Unstage 2 files");
    wait_for(&mut h, "folder unstaged", || f.git(&["diff", "--cached", "--name-only"]).is_empty());
    no_errors(&h);
}

#[test]
fn a_filtered_tree_shows_matches_and_stages_only_those() {
    let f = Fx::new();
    sample_history(&f);
    f.write("src/keep.rs", "1\n");
    f.write("src/other.rs", "2\n");
    let mut h = harness(&f);
    h.state_mut().settings.changes_tree = true;
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "src");

    click(&mut h, "src");
    assert!(h.query_by_label("src/keep.rs").is_none(), "folder closed");
    // Other panes have filter boxes too: this one sits straight above the folder.
    let src = h.get_by_label("src").rect();
    let above = |r: egui::Rect| r.left() < src.center().x && r.right() > src.center().x && r.bottom() <= src.top();
    let filter = h.query_all_by_role(egui::accesskit::Role::TextInput).map(|i| i.rect()).enumerate().filter(|(_, r)| above(*r)).max_by(|a, b| a.1.bottom().total_cmp(&b.1.bottom())).expect("the Changes filter").0;
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(filter).unwrap().focus();
    h.step();
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(filter).unwrap().type_text("keep");
    h.step();
    pump_for_label(&mut h, "src/keep.rs");
    assert!(h.query_by_label("src/other.rs").is_none(), "only what matches");

    h.get_by_label("src").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Stage 1 file");
    wait_for(&mut h, "match staged", || !f.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(f.git(&["diff", "--cached", "--name-only"]).trim(), "src/keep.rs");
    no_errors(&h);
}

#[test]
fn every_section_of_the_changes_tree_renders_during_a_conflict() {
    let f = Fx::new();
    f.write("dir/c.txt", "base\n");
    f.commit("base");
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("dir/c.txt", "side\n");
    f.commit("side");
    f.git(&["checkout", "-q", "main"]);
    f.write("dir/c.txt", "main\n");
    f.commit("main");
    assert!(f.git.run(Some(&f.root), &["merge", "side"]).is_err() || f.git(&["status", "--short"]).contains("UU"));
    f.write("dir/staged.txt", "s\n");
    f.git(&["add", "dir/staged.txt"]);
    f.write("dir/new.txt", "n\n");
    let mut h = harness(&f);
    h.state_mut().settings.changes_tree = true;
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "dir/c.txt");
    assert!(h.query_by_label("dir/staged.txt").is_some());
    assert!(h.query_by_label("dir/new.txt").is_some());
    no_errors(&h);
}

#[test]
fn health_view_enables_and_undoes_an_option() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Health(Box::default()));
    pump_for_label(&mut h, "Healthy");
    let t = Instant::now();
    while h.query_all_by_label("Enable").count() < 6 {
        h.step();
        assert!(t.elapsed() < PATIENCE);
    }
    // The second item is the untracked cache.
    h.query_all_by_label("Enable").nth(1).unwrap().click();
    h.step();
    wait_for(&mut h, "option on", || f.git.run_raw(Some(&f.root), &["config", "core.untrackedCache"]).unwrap().text().trim() == "true");
    pump_for_label(&mut h, "Undo");
    click(&mut h, "Undo");
    wait_for(&mut h, "option off", || !f.git.run_raw(Some(&f.root), &["config", "core.untrackedCache"]).unwrap().ok());
    no_errors(&h);
}

#[test]
fn new_views_render() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let head = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    for tab in [Tab::visual(""), Tab::visual("a.txt"), Tab::Treemap(Box::default()), Tab::tree(&head)] {
        h.state_mut().repos[0].open_tab(tab);
        wait_until_quiet(&mut h);
        no_errors(&h);
    }
    // Focus from the action reaches the graph.
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::GraphFocus("main".into()));
    h.state_mut().apply_action(&ctx, Action::GraphHide("main".into()));
    for _ in 0..5 {
        h.step();
    }
    no_errors(&h);
}

#[test]
fn folding_a_lane_and_moving_columns_keep_the_graph_working() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["switch", "-q", "-c", "side", "HEAD~1"]);
    f.write("s", "1\n");
    f.commit("on side");
    f.git(&["switch", "-q", "main"]);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "on side");
    h.get_by_label("on side").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Fold this lane");
    for _ in 0..4 {
        h.step();
    }
    pump_for_label(&mut h, "on side");
    h.get_by_label("on side").click_secondary();
    h.step();
    h.step();
    pump_for_label(&mut h, "Unfold this lane");
    click(&mut h, "Unfold this lane");
    no_errors(&h);
}

#[test]
fn session_tabs_round_trip() {
    for spec in ["home", "graph", "changes", "launchpad", "agents", "review", "health", "file:src/a b.rs"] {
        let t = Tab::from_spec(spec).unwrap();
        assert_eq!(t.spec().as_deref(), Some(spec));
    }
    assert!(Tab::from_spec("nonsense").is_none());
    assert_eq!(Tab::diff(DiffSource::Staged, None).spec(), None, "one-off tabs aren't restored");
}

#[test]
fn review_notes_persist_inside_the_git_folder() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "base", "HEAD~1"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Review(Box::default()));
    pump_for_label(&mut h, "New review");
    let store = f.root.join(".git/gitem-review.json");
    click(&mut h, "New review");
    wait_for(&mut h, "the review file", || store.exists());
    assert!(f.git(&["status", "--short"]).is_empty(), "nothing shows as a change in the repository");
    no_errors(&h);
}

#[test]
fn deep_links_open_the_commit_and_file() {
    let f = Fx::new();
    sample_history(&f);
    let sha = f.git(&["rev-parse", "HEAD~1"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let root = h.state().repos[0].repo.root.to_string_lossy().replace('\\', "/");
    let link = gitem_core::forge::DeepLink::parse(&gitem_core::forge::DeepLink { repo: root, commit: Some(sha), file: Some("a.txt".into()), line: Some(2) }.to_url()).unwrap();
    h.state_mut().apply_action(&ctx, Action::DeepLink(link));
    h.step();
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::File(_))));
    assert_eq!(h.state().repos.len(), 1, "an already open repository isn't opened twice");
}

#[test]
fn language_switch_changes_labels() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().settings.language = "es".into();
    h.step();
    h.step();
    pump_for_label(&mut h, "Actualizar");
    h.state_mut().settings.language = "en".into();
    h.step();
    h.step();
    pump_for_label(&mut h, "Refresh");
}

#[test]
fn saved_layouts_keep_splits_and_drop_one_off_tabs() {
    use egui_dock::DockState;
    let mut dock = DockState::new(vec![Tab::graph(), Tab::diff(DiffSource::Staged, None)]);
    dock.main_surface_mut().split_right(egui_dock::NodeIndex::root(), 0.6, vec![Tab::changes(), Tab::home()]);
    let json = gitem::app::layout_to_json(&dock).unwrap();
    let back = gitem::app::layout_from_json(&json).unwrap();
    let keys: Vec<String> = back.iter_all_tabs().map(|(_, t)| t.key()).collect();
    assert_eq!(keys.len(), 3, "the diff tab isn't kept: {keys:?}");
    assert_eq!(back.iter_leaves().count(), 2, "the split survives");
    assert!(gitem::app::layout_from_json("not json").is_none());
}

/// `f` tracks a bare repository, and a second clone can move it from elsewhere.
struct Remote {
    f: Fx,
    other: PathBuf,
    bare: PathBuf,
    _dir: tempfile::TempDir,
}

fn with_remote() -> Remote {
    let dir = tempfile::tempdir().unwrap();
    let (bare, other) = (dir.path().join("remote.git"), dir.path().join("other"));
    let f = Fx::new();
    sample_history(&f);
    let g = &f.git;
    g.run(None, &["init", "-q", "--bare", "-b", "main", &bare.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare.to_string_lossy()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    g.run(None, &["-c", "core.autocrlf=false", "clone", "-q", &bare.to_string_lossy(), &other.to_string_lossy()]).unwrap();
    for (k, v) in [("user.name", "Other"), ("user.email", "other@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        g.run(Some(&other), &["config", k, v]).unwrap();
    }
    Remote { f, other, bare, _dir: dir }
}

impl Remote {
    fn commit_elsewhere(&self, file: &str, msg: &str) {
        let g = &self.f.git;
        std::fs::write(self.other.join(file), "elsewhere\n").unwrap();
        g.run(Some(&self.other), &["add", "-A"]).unwrap();
        g.run(Some(&self.other), &["commit", "-q", "-m", msg]).unwrap();
        g.run(Some(&self.other), &["push", "-q"]).unwrap();
    }
    fn remote_tip(&self) -> String {
        self.f.git.run(Some(&self.bare), &["--git-dir=.", "rev-parse", "main"]).unwrap().text().trim().to_string()
    }
}

#[test]
fn toolbar_fetches_pulls_and_pushes() {
    let r = with_remote();
    let mut h = harness(&r.f);
    settle(&mut h);

    r.commit_elsewhere("new.txt", "from elsewhere");
    click(&mut h, "Fetch");
    click(&mut h, "Pull 1");
    wait_for(&mut h, "pull", || r.f.root.join("new.txt").exists());
    no_errors(&h);

    r.f.write("mine.txt", "mine\n");
    r.f.commit("from here");
    click(&mut h, "Push 1");
    wait_for(&mut h, "push", || r.remote_tip() == r.f.git(&["rev-parse", "HEAD"]).trim());
    no_errors(&h);
}

#[test]
fn a_rejected_push_shows_git_s_reason() {
    let r = with_remote();
    r.commit_elsewhere("new.txt", "from elsewhere");
    r.f.write("mine.txt", "mine\n");
    r.f.commit("from here");
    let tip = r.remote_tip();
    let mut h = harness(&r.f);
    settle(&mut h);
    click(&mut h, "Push 1");
    pump_for_error(&mut h);
    assert_eq!(r.remote_tip(), tip);
    assert!(r.f.git(&["log", "-1", "--format=%s"]).contains("from here"), "local work is untouched");
}

#[test]
fn a_pull_conflict_opens_into_the_merge_workflow() {
    let r = with_remote();
    r.commit_elsewhere("conflict.txt", "from elsewhere");
    r.f.write("conflict.txt", "from here\n");
    r.f.commit("from here");
    let mut h = harness(&r.f);
    settle(&mut h);
    click(&mut h, "Fetch");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::pull(ops::PullMode::Merge, false)));
    pump_for_label(&mut h, "Resolve");
    assert!(r.f.git(&["status", "--porcelain"]).contains("AA conflict.txt"));
    no_errors(&h);
    click(&mut h, "Resolve");
    pump_for_label(&mut h, "Take theirs");
}

/// The tab used to keep a copy of the settings from when it first rendered, and one edit
/// there overwrote everything changed elsewhere since.
#[test]
fn an_edit_in_a_settings_tab_keeps_what_changed_elsewhere() {
    use gitem_core::settings::{ThemeChoice, Workspace};
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::new(gitem::views::settings::SettingsView::on_page("Workflow"))));
    pump_for_label(&mut h, "Safety and speed");
    h.state_mut().settings.workspaces.push(Workspace { name: "kept".into(), repos: Vec::new() });
    h.state_mut().settings.theme = ThemeChoice::Light;
    h.state_mut().settings.favorite_branches.push("main".into());
    h.step();
    let before = h.state().settings.prune_on_fetch;
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Prune when fetching").click();
    h.step();
    h.step();
    let s = &h.state().settings;
    assert_eq!(s.prune_on_fetch, !before, "the edit took");
    assert_eq!(s.workspaces.len(), 1);
    assert_eq!(s.theme, ThemeChoice::Light);
    assert_eq!(s.favorite_branches, vec!["main"]);
}

fn saved_patch_for_test(id: &str, content: &[u8]) {
    let meta = gitem_core::patches::PatchMeta { id: id.to_string(), title: format!("Patch {id}"), description: String::new(), repo: "r".into(), created: 1, base: String::new(), gist_url: String::new() };
    gitem_core::patches::save(&meta, content).unwrap();
}

/// There's no getting a patch back, and one click used to delete it.
#[test]
fn deleting_a_patch_asks_first() {
    let f = Fx::new();
    sample_history(&f);
    saved_patch_for_test("delete-me", b"some patch\n");
    let path = gitem_core::patches::patch_path("delete-me");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Patches(Box::new(gitem::views::patches::PatchesView::showing("delete-me"))));
    click(&mut h, "Delete…");
    pump_for_label(&mut h, "Delete it");
    assert!(path.exists(), "still there before answering");
    click(&mut h, "Keep it");
    assert!(path.exists());
    click(&mut h, "Delete…");
    click(&mut h, "Delete it");
    assert!(!path.exists());
}

#[test]
fn a_patch_with_other_encodings_can_not_be_copied_as_text() {
    use egui_kittest::kittest::NodeT;
    let f = Fx::new();
    sample_history(&f);
    saved_patch_for_test("latin1", b"+caf\xe9\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Patches(Box::new(gitem::views::patches::PatchesView::showing("latin1"))));
    pump_for_label(&mut h, "Copy");
    assert!(h.get_by_label("Copy").accesskit_node().is_disabled());
    gitem_core::patches::delete("latin1").unwrap();
}

/// Trimming the pasted text took off a hunk's last line when that was a blank context line.
#[test]
fn a_pasted_patch_ending_in_a_blank_context_line_applies() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\n\n");
    f.commit("base");
    f.write("f.txt", "ONE\ntwo\n\n");
    let patch = f.git(&["diff"]);
    assert!(patch.ends_with("\n \n"), "the case being tested: {patch:?}");
    f.git(&["checkout", "--", "f.txt"]);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(gitem::palette::apply_copied_patch_form()));
    let input = egui::accesskit::Role::MultilineTextInput;
    pump_for_role(&mut h, input);
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text(&patch);
    h.step();
    h.step();
    click(&mut h, "Apply");
    wait_for(&mut h, "the patch to apply", || std::fs::read_to_string(f.root.join("f.txt")).unwrap() == "ONE\ntwo\n\n");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

const LOCK_TITLE: &str = "The repository is locked";

/// What a killed git leaves behind, and a write that runs into it.
fn stuck_on_a_lock(f: &Fx, h: &mut Harness<GitemApp>) -> PathBuf {
    let lock = f.root.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    f.write("new.txt", "x\n");
    settle(h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::stage_paths(&["new.txt".to_string()])));
    pump_for_label(h, LOCK_TITLE);
    for _ in 0..3 {
        h.step();
    }
    lock
}

#[test]
fn a_lock_in_the_way_is_never_removed_without_being_asked() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.state_mut().git_running = || false;
    let lock = stuck_on_a_lock(&f, &mut h);
    assert!(lock.exists());
    assert!(h.state().errors_showing().is_empty(), "this dialog replaces the raw error");
    click(&mut h, "Leave it");
    assert!(lock.exists());
    assert!(h.query_by_label(LOCK_TITLE).is_none());
}

#[test]
fn a_stale_lock_can_be_removed_on_request() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.state_mut().git_running = || false;
    let lock = stuck_on_a_lock(&f, &mut h);
    click(&mut h, "Remove the lock");
    assert!(!lock.exists());
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::stage_paths(&["new.txt".to_string()])));
    wait_for(&mut h, "the file to be staged", || f.git(&["diff", "--cached", "--name-only"]).trim() == "new.txt");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn a_lock_stays_while_any_git_process_is_running() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.state_mut().git_running = || true;
    let lock = stuck_on_a_lock(&f, &mut h);
    click(&mut h, "Remove the lock");
    pump_for_label(&mut h, "A git process is running on this computer, so the lock may be in use. Try again when it has finished.");
    assert!(lock.exists());
    // Once it's finished, the same button works.
    h.state_mut().git_running = || false;
    click(&mut h, "Remove the lock");
    assert!(!lock.exists());
}

/// Cancelling kills git with no cleanup, so its lock can stay.
#[test]
fn cancelling_with_a_lock_left_behind_offers_the_way_out() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.state_mut().git_running = || false;
    settle(&mut h);
    let slow = ops::GitOp::new("Slow pull").step(&["-c", "alias.slowly=!sleep 60", "slowly"]).net();
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(slow));
    pump_for_label(&mut h, "Cancel");
    std::fs::write(f.root.join(".git/index.lock"), "").unwrap();
    click(&mut h, "Cancel");
    pump_for_label(&mut h, LOCK_TITLE);
    assert!(h.query_by_label_contains("cancelled").is_some());
    assert!(f.root.join(".git/index.lock").exists());
}

/// A lock that clears by itself needs no decision.
#[test]
fn the_lock_dialog_goes_away_when_the_lock_does() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.state_mut().git_running = || false;
    let lock = stuck_on_a_lock(&f, &mut h);
    std::fs::remove_file(&lock).unwrap();
    h.step();
    h.step();
    assert!(h.query_by_label(LOCK_TITLE).is_none());
}

#[test]
fn a_multi_commit_pick_finished_by_hand_offers_to_carry_on_from_the_banner() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-qc", "side"]);
    f.write("c.txt", "side\n");
    f.commit("pick-A");
    let a = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    f.write("b.txt", "b\n");
    f.commit("pick-B");
    let b = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "main\n");
    f.commit("main change");
    assert!(!f.git.run_raw(Some(&f.root), &["cherry-pick", &a, &b]).unwrap().ok());
    f.write("c.txt", "resolved\n");
    f.git(&["add", "c.txt"]);
    f.git.run_opts(Some(&f.root), &["commit".to_string(), "-m".to_string(), "pick-A".to_string()], &gitem_core::runner::RunOpts { env: vec![("GIT_EDITOR".into(), "true".into())], ..Default::default() }, &mut |_| {}).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Cherry-picking 1 commit left");
    click(&mut h, "Continue");
    wait_for(&mut h, "the second pick", || f.subjects()[0] == "pick-B");
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert!(h.query_by_label("Cherry-picking 1 commit left").is_none());
}

/// Marking a file resolved with its markers still in is how they get committed.
#[test]
fn marking_a_file_resolved_with_markers_left_asks_first() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "c.txt");
    h.get_by_label("c.txt").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Mark as resolved");
    pump_for_label(&mut h, "Mark resolved anyway");
    assert_no_op_started(&mut h, "nothing is marked before answering");
    assert!(f.git(&["status", "--short"]).contains("UU c.txt"));
    click(&mut h, "Cancel");
    assert!(f.git(&["status", "--short"]).contains("UU c.txt"));

    // With the markers gone, it just marks it.
    f.write("c.txt", "fixed by hand\nmiddle\nbottom\n");
    h.get_by_label("c.txt").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Mark as resolved");
    wait_for(&mut h, "the file to be resolved", || !f.git(&["status", "--short"]).contains("UU"));
    assert!(h.query_by_label("Mark resolved anyway").is_none());
    assert_eq!(std::fs::read_to_string(f.root.join("c.txt")).unwrap(), "fixed by hand\nmiddle\nbottom\n");
}

fn start_bisect(f: &Fx) -> Vec<String> {
    let mut shas = Vec::new();
    for n in 0..16 {
        f.write("n.txt", &format!("{n}\n"));
        f.commit(&format!("c{n}"));
        shas.push(f.git(&["rev-parse", "HEAD"]).trim().to_string());
    }
    f.git(&["bisect", "start", &shas[15], &shas[0]]);
    shas
}

/// Double-clicking marked two commits, the second one never tested.
#[test]
fn a_bisect_mark_applies_to_the_commit_shown_however_often_it_is_clicked() {
    let f = Fx::new();
    let _ = start_bisect(&f);
    let tested = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Bisect(Box::default()));
    pump_for_label(&mut h, "Bisect in progress");
    wait_until_quiet(&mut h);
    // Two clicks before the first has run: the same as the worst double-click.
    h.get_by_label("\u{E182} Good").click();
    h.get_by_label("\u{E182} Good").click();
    h.step();
    h.step();
    let t = Instant::now();
    while !quiet(&h) {
        h.step();
        assert!(t.elapsed() < PATIENCE);
        std::thread::sleep(Duration::from_millis(15));
    }
    let log = f.git(&["bisect", "log"]);
    let goods: Vec<&str> = log.lines().filter(|l| l.starts_with("git bisect good")).collect();
    assert!(goods.iter().all(|l| l.contains(&tested)), "only the commit shown was marked: {log}");
}

#[test]
fn deleting_a_hook_asks_first() {
    let f = Fx::new();
    sample_history(&f);
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Tools(Box::default()));
    click(&mut h, "Hooks");
    click(&mut h, "Delete…");
    pump_for_label(&mut h, "Delete it");
    assert!(hook.exists());
    click(&mut h, "Keep it");
    assert!(hook.exists());
    click(&mut h, "Delete…");
    click(&mut h, "Delete it");
    assert!(!hook.exists());
    no_errors(&h);
}

#[cfg(unix)]
#[test]
fn a_new_hook_is_executable() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Tools(Box::default()));
    click(&mut h, "Hooks");
    click_nth(&mut h, "Create", 0);
    let made = std::fs::read_dir(f.root.join(".git/hooks")).unwrap().flatten().find(|e| !e.file_name().to_string_lossy().ends_with(".sample")).expect("a hook");
    assert_ne!(made.metadata().unwrap().permissions().mode() & 0o111, 0);
}

fn review_file(f: &Fx) -> PathBuf {
    f.root.join(".git/gitem-review.json")
}

/// A damaged review file used to read as empty, and the next save destroyed it.
#[test]
fn a_damaged_review_file_is_kept_and_reported() {
    let f = Fx::new();
    sample_history(&f);
    std::fs::write(review_file(&f), "{ not json").unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Review(Box::default()));
    pump_for_error(&mut h);
    let kept = std::fs::read_dir(f.root.join(".git")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("gitem-review.json.bad-")).count();
    assert_eq!(kept, 1);
}

#[test]
fn starting_a_new_review_asks_when_there_are_notes_to_lose() {
    let f = Fx::new();
    sample_history(&f);
    let saved = r#"{"base":"HEAD~1","reviewed":["a.txt"],"threads":[{"path":"a.txt","text":"a note","author":"me","dismissed":false}]}"#;
    std::fs::write(review_file(&f), saved).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Review(Box::default()));
    click(&mut h, "New review");
    pump_for_label(&mut h, "Clear them");
    assert!(std::fs::read_to_string(review_file(&f)).unwrap().contains("a note"));
    click(&mut h, "Keep them");
    assert!(std::fs::read_to_string(review_file(&f)).unwrap().contains("a note"));
    click(&mut h, "New review");
    click(&mut h, "Clear them");
    assert!(!std::fs::read_to_string(review_file(&f)).unwrap().contains("a note"));
    // Nothing left to lose, so nothing to ask.
    click(&mut h, "New review");
    assert!(h.query_by_label("Clear them").is_none());
}

const CLOSE_TITLE: &str = "Close with work not saved?";

fn close_commands(h: &Harness<GitemApp>) -> Vec<egui::ViewportCommand> {
    h.output().viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

/// What the window's close button and the system both end up as.
fn ask_window_to_close(h: &mut Harness<GitemApp>) {
    let root = h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
    root.events.push(egui::ViewportEvent::Close);
    h.step();
    let root = h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
    root.events.clear();
}

fn type_commit_message(h: &mut Harness<GitemApp>, text: &str) {
    h.state_mut().repos[0].open_tab(Tab::changes());
    let input = egui::accesskit::Role::MultilineTextInput;
    pump_for_role(h, input);
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text(text);
    h.step();
    h.step();
}

fn middle_click(h: &mut Harness<GitemApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed: true, modifiers: Default::default() });
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed: false, modifiers: Default::default() });
    h.step();
    h.step();
}

fn dock_tab_rect(h: &Harness<GitemApp>, is: impl Fn(&Tab) -> bool) -> egui::Rect {
    let r = &h.state().repos[0];
    let path = r.dock.find_tab_from(is).expect("the tab");
    h.ctx.read_response(gitem::app::dock_tab_id(path.node_path(), path.tab.0)).expect("the tab's title").rect
}

/// A new window spends its first frames measuring itself, and a click then can land anywhere.
fn answer_close(h: &mut Harness<GitemApp>, answer: &str) {
    pump_for_label(h, CLOSE_TITLE);
    for _ in 0..3 {
        h.step();
    }
    click(h, answer);
}

fn merge_tab_open(h: &Harness<GitemApp>) -> bool {
    h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Merge(_)))
}

#[test]
fn closing_the_window_with_nothing_at_stake_just_closes() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    ask_window_to_close(&mut h);
    assert!(!close_commands(&h).contains(&egui::ViewportCommand::CancelClose));
    assert!(h.query_by_label(CLOSE_TITLE).is_none());
}

#[test]
fn closing_the_window_asks_about_a_written_commit_message() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    type_commit_message(&mut h, "half a thought");
    ask_window_to_close(&mut h);
    assert!(close_commands(&h).contains(&egui::ViewportCommand::CancelClose), "the close was held back");
    pump_for_label(&mut h, CLOSE_TITLE);
    assert!(h.query_by_label_contains("A commit message").is_some(), "it names what would go");
    answer_close(&mut h, "Keep it open");
    assert!(h.query_by_label(CLOSE_TITLE).is_none());

    ask_window_to_close(&mut h);
    pump_for_label(&mut h, CLOSE_TITLE);
    answer_close(&mut h, "Close anyway");
    // One frame on, the close is sent, and a further request isn't held back.
    assert!(h.query_by_label(CLOSE_TITLE).is_none());
    ask_window_to_close(&mut h);
    assert!(!close_commands(&h).contains(&egui::ViewportCommand::CancelClose));
}

/// A message put there by git, with nothing written, is nothing to ask about.
#[test]
fn a_prepared_merge_message_is_not_a_draft() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    let t = Instant::now();
    while h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Changes(c) = t { Some(c.message.is_empty()) } else { None }).unwrap() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "git's message never came");
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(h.state().window_at_stake().is_empty(), "{:?}", h.state().window_at_stake());
}

#[test]
fn closing_a_merge_tab_with_resolutions_asks_first() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "2 of 2 conflicts left in this file");

    click_nth(&mut h, "Take ours", 0);
    wait_until_quiet(&mut h);
    let tab = dock_tab_rect(&h, |t| matches!(t, Tab::Merge(_)));
    middle_click(&mut h, tab.center());
    pump_for_label(&mut h, CLOSE_TITLE);
    assert!(h.query_by_label_contains("conflict resolutions not saved").is_some());
    assert!(merge_tab_open(&h), "held back");
    answer_close(&mut h, "Keep it open");
    assert!(merge_tab_open(&h));

    // Saved work isn't at stake any more.
    click(&mut h, "Save with markers");
    pump_for_label(&mut h, "Saved");
    let tab = dock_tab_rect(&h, |t| matches!(t, Tab::Merge(_)));
    middle_click(&mut h, tab.center());
    assert!(h.query_by_label(CLOSE_TITLE).is_none());
    assert!(!merge_tab_open(&h), "closed with no question");
}

#[test]
fn closing_a_merge_tab_anyway_closes_only_that_tab() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    click(&mut h, "Take ours");
    wait_until_quiet(&mut h);
    let tab = dock_tab_rect(&h, |t| matches!(t, Tab::Merge(_)));
    middle_click(&mut h, tab.center());
    pump_for_label(&mut h, CLOSE_TITLE);
    answer_close(&mut h, "Close anyway");
    assert!(!merge_tab_open(&h));
    assert_eq!(h.state().repos.len(), 1);
}

#[test]
fn an_untouched_merge_tab_closes_without_a_question() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "1 of 1 conflict left in this file");
    wait_until_quiet(&mut h);
    let tab = dock_tab_rect(&h, |t| matches!(t, Tab::Merge(_)));
    middle_click(&mut h, tab.center());
    assert!(h.query_by_label(CLOSE_TITLE).is_none());
    assert!(!merge_tab_open(&h));
}

/// A repository tab with work in it used to vanish with one click.
#[test]
fn closing_a_repository_asks_about_a_composer_plan() {
    let f = Fx::new();
    sample_history(&f);
    f.write("one.txt", "1\n");
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "planned");
    let name = f.root.file_name().unwrap().to_string_lossy().to_string();
    click(&mut h, &format!("Close {name}"));
    pump_for_label(&mut h, CLOSE_TITLE);
    assert!(h.query_by_label_contains("a plan of 1 commit").is_some());
    answer_close(&mut h, "Keep it open");
    assert_eq!(h.state().repos.len(), 1);
    click(&mut h, &format!("Close {name}"));
    pump_for_label(&mut h, CLOSE_TITLE);
    answer_close(&mut h, "Close anyway");
    assert!(h.state().repos.is_empty());
}

#[test]
fn closing_a_repository_with_nothing_at_stake_just_closes() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let name = f.root.file_name().unwrap().to_string_lossy().to_string();
    click(&mut h, &format!("Close {name}"));
    assert!(h.query_by_label(CLOSE_TITLE).is_none());
    assert!(h.state().repos.is_empty());
}

/// A slow op that ends in failure, so a test can close the tab while it runs.
fn slow_failing_op(f: &Fx) -> gitem_core::ops::GitOp {
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nsleep 2\necho refused by the slow hook >&2\nexit 1\n").unwrap();
    ops::commit(&ops::CommitOpts { message: "slow".into(), allow_empty: true, ..Default::default() })
}

/// The queue used to vanish and the running op's failure with it, with no word to anyone.
#[test]
fn closing_a_repository_with_ops_running_says_so_and_still_reports_the_failure() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let op = slow_failing_op(&f);
    h.state_mut().apply_action(&ctx, Action::Run(op));
    h.state_mut().apply_action(&ctx, Action::Run(ops::tag_create("queued-tag", None, None, false)));
    h.step();
    let name = f.root.file_name().unwrap().to_string_lossy().to_string();
    click(&mut h, &format!("Close {name}"));
    pump_for_label(&mut h, CLOSE_TITLE);
    assert!(h.query_by_label_contains("is running").is_some());
    assert!(h.query_by_label_contains("queued git command").is_some());
    answer_close(&mut h, "Close anyway");
    assert!(h.state().repos.is_empty());
    pump_for_error(&mut h);
    assert!(h.query_by_label_contains("refused by the slow hook").is_some());
    assert_eq!(f.git(&["tag", "--list", "queued-tag"]).trim(), "", "the queued op did not run");
}

#[test]
fn a_fetch_in_progress_is_no_reason_to_ask() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let hold = ops::fetch(&ops::FetchOpts::default());
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(hold));
    assert!(h.state().window_at_stake().is_empty());
}

/// Dialogs don't block the window, so a second could replace the first and its typing.
#[test]
fn a_second_form_does_not_replace_one_with_typing_in_it() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    pump_for_label(&mut h, "Branch name");
    for _ in 0..3 {
        h.step();
    }
    // Opening another over an untouched one is fine, and the usual way to change your mind.
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_tag(None)));
    pump_for_label(&mut h, "Tag name");
    for _ in 0..3 {
        h.step();
    }
    h.event(egui::Event::Text("v-typed".into()));
    h.step();
    h.step();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label("Tag name").is_some(), "the form with typing stays");
    assert!(h.query_by_label("Branch name").is_none());
}

#[test]
fn a_second_question_does_not_replace_the_first() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let ask = |title: &str| Action::Confirm { title: title.into(), body: String::new(), label: "Go".into(), op: ops::tag_create("v1", None, None, false) };
    h.state_mut().apply_action(&ctx, ask("First question"));
    pump_for_label(&mut h, "First question");
    h.state_mut().apply_action(&ctx, ask("Second question"));
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label("First question").is_some());
    assert!(h.query_by_label("Second question").is_none());
}

#[test]
fn clone_opens_the_new_repository() {
    let r = with_remote();
    let d = tempfile::tempdir().unwrap();
    let dest = d.path().join("cloned");
    let mut h = harness(&r.f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let op = ops::clone_repo(&r.bare.to_string_lossy(), &dest.to_string_lossy(), None, false, None);
    h.state_mut().apply_action(&ctx, Action::RunGlobal { op, open_after: Some(dest.clone()) });
    pump(&mut h, |a| a.repos.len() == 2, "the clone to open");
    no_errors(&h);
    assert!(dest.join("a.txt").exists() || std::fs::read_dir(&dest).unwrap().count() > 1);
}

#[test]
fn a_failed_clone_opens_nothing_and_says_so() {
    let r = with_remote();
    let d = tempfile::tempdir().unwrap();
    let dest = d.path().join("cloned");
    let mut h = harness(&r.f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let op = ops::clone_repo(&d.path().join("missing.git").to_string_lossy(), &dest.to_string_lossy(), None, false, None);
    h.state_mut().apply_action(&ctx, Action::RunGlobal { op, open_after: Some(dest) });
    pump_for_error(&mut h);
    assert_eq!(h.state().repos.len(), 1);
}

#[test]
fn the_rail_opens_every_view_and_marks_the_one_in_front() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let mut items: Vec<&gitem::chrome::RailItem> = gitem::chrome::rail_top().into_iter().flatten().collect();
    items.extend(gitem::chrome::rail_bottom());
    let ctx = h.ctx.clone();
    for item in items {
        // Some of these names are also tab titles, so the rail's is found by position.
        let before = h.state().repos[0].dock.iter_all_tabs().filter(|(_, t)| (item.is)(t)).count();
        h.state_mut().apply_action(&ctx, Action::OpenTab((item.make)()));
        h.step();
        h.step();
        assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| (item.is)(t)), "{} didn't open (had {before})", item.name);
    }
    no_errors(&h);
}

#[test]
fn settings_and_about_are_reachable_from_the_titlebar() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    click(&mut h, "About gitem");
    // Not the commit rows: a build from a source tarball has no git history.
    pump_for_label(&mut h, "Version");
    pump_for_label(&mut h, env!("CARGO_PKG_VERSION"));
    // New windows spend their first frames measuring themselves.
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Close");
    assert!(h.query_by_label("Version").is_none(), "the About box stays open");
    assert!(!h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Settings(_))));
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::OpenSettings);
    h.step();
    h.step();
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Settings(_))));
}

#[test]
fn settings_work_with_no_repository_open() {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings { details_on_open: false, ..Default::default() });
    app.git = Ok(real_git());
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    let mut h = builder.build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app);
    h.step();
    assert!(h.query_by_label("Open folder…").is_some());
    click(&mut h, "Settings");
    h.step();
    assert!(h.query_by_label("Open folder…").is_none(), "settings replace the welcome page");
    click(&mut h, "Settings");
    h.step();
    assert!(h.query_by_label("Open folder…").is_some(), "the same button goes back");
}

#[test]
fn toolbar_icons_have_names() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    for name in ["Refresh", "Stash", "Details", "Sidebar", "Minimize window", "Maximize window", "Close window", "Command palette"] {
        pump_for_label(&mut h, name);
    }
}

fn graph_columns<'a>(h: &'a Harness<GitemApp>) -> &'a gitem::views::graphcols::Columns {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Graph(g) = t { Some(&g.cols) } else { None }).expect("a graph tab")
}

fn pointer_drag(h: &mut Harness<GitemApp>, from: egui::Pos2, to: egui::Pos2) {
    h.event(egui::Event::PointerMoved(from));
    h.step();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.step();
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.step();
    h.step();
}

#[test]
fn graph_columns_resize_and_move_by_dragging_and_are_remembered() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "Resize Branch / tag column");

    // The divider next to the message resizes what's past it.
    let c = h.get_by_label("Resize Branch / tag column").rect().center();
    pointer_drag(&mut h, c, c + vec2(60.0, 0.0));
    assert_eq!(graph_columns(&h).widths["refs"].round(), 250.0);

    // Dragging a header moves the column.
    let sha = h.get_by_label("SHA column").rect().center();
    let refs = h.get_by_label("Branch / tag column").rect();
    pointer_drag(&mut h, sha, refs.left_center() + vec2(4.0, 0.0));
    assert_eq!(graph_columns(&h).order[0], "sha");

    // Both are saved, for the next graph opened.
    let saved = h.state().settings.graph_layout.clone().expect("a saved layout");
    assert_eq!(saved.columns[0], "sha");
    assert_eq!(saved.widths["refs"].round(), 250.0);

    // A header's context menu hides columns.
    h.get_by_label("Date column").click_secondary();
    h.step();
    h.step();
    pump_for_label(&mut h, "Reset columns");
    click(&mut h, "Date");
    assert!(!graph_columns(&h).shown("date"));
    assert!(h.state().settings.graph_layout.as_ref().unwrap().hidden.iter().any(|x| x == "date"));
    no_errors(&h);
}

#[test]
fn the_sidebar_shows_incoming_and_outgoing_commits_on_a_branch() {
    let r = with_remote();
    r.commit_elsewhere("theirs.txt", "from elsewhere");
    r.f.write("mine.txt", "mine\n");
    r.f.commit("from here");
    let mut h = harness(&r.f);
    settle(&mut h);
    // Nothing is known about the remote until a fetch.
    assert!(h.query_by_label("Incoming from origin/main (1)").is_none());
    click(&mut h, "Fetch");
    pump_for_label(&mut h, "Incoming from origin/main (1)");
    pump_for_label(&mut h, "Outgoing to origin/main (1)");
    settle(&mut h);

    // The commit is listed under the branch, on top of wherever else it appears.
    let listed = h.query_all_by_label("from elsewhere").count();
    assert!(listed >= 1);
    click(&mut h, "Collapse main");
    assert!(h.query_by_label("Incoming from origin/main (1)").is_none());
    assert_eq!(h.query_all_by_label("from elsewhere").count(), listed - 1);
    click(&mut h, "Expand main");
    pump_for_label(&mut h, "Incoming from origin/main (1)");

    // Pulling from there clears it.
    click(&mut h, "Pull");
    wait_for(&mut h, "pull", || r.f.root.join("theirs.txt").exists());
    let t = Instant::now();
    while h.query_by_label("Incoming from origin/main (1)").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "still shown as incoming");
        std::thread::sleep(Duration::from_millis(15));
    }
    no_errors(&h);
}

#[test]
fn active_tab_markers_can_find_their_tabs() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let dock = &h.state().repos[0].dock;
    let (path, leaf) = dock.iter_leaves().next().unwrap();
    let response = h.ctx.read_response(gitem::app::dock_tab_id(path, leaf.active.0));
    assert!(response.is_some(), "egui_dock's tab ids have changed, so the markers aren't drawn");
    assert!(response.unwrap().rect.width() > 10.0);
}

#[test]
fn sidebar_sections_are_tall_toggle_and_keep_their_actions_on_the_header() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "listed-branch"]);
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.get_by_label("Branches section").rect().height() >= 27.0);
    assert!(h.query_by_label("+ New branch").is_none());
    pump_for_label(&mut h, "listed-branch");

    // An action leaves the section as it is.
    click(&mut h, "New branch");
    pump_for_label(&mut h, "Create");
    assert!(h.query_by_label("listed-branch").is_some());
    click(&mut h, "Cancel");
    for _ in 0..8 { h.step(); }

    // The header folds it, and sections stay usable closed.
    click(&mut h, "Branches section");
    let t = Instant::now();
    while h.query_by_label("listed-branch").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the section didn't close");
        std::thread::sleep(Duration::from_millis(15));
    }
    click(&mut h, "New branch");
    pump_for_label(&mut h, "Create");
    click(&mut h, "Cancel");
    click(&mut h, "Branches section");
    pump_for_label(&mut h, "listed-branch");

    // Clearing stashes appears only when there's something to clear.
    assert!(h.query_by_label("New stash").is_some());
    assert!(h.query_by_label("Clear all stashes").is_none());
    no_errors(&h);
}

#[test]
fn text_boxes_have_room_and_stay_inside_the_window() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    for tab in all_tabs() {
        h.state_mut().repos[0].open_tab(tab);
        for _ in 0..3 {
            h.step();
        }
        let window = ctx.content_rect();
        for input in h.query_all_by_role(egui::accesskit::Role::TextInput).chain(h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)) {
            let r = input.rect();
            assert!(r.height() >= 24.0, "too short: {r:?}");
            assert!(r.left() >= window.left() && r.right() <= window.right() + 1.0, "past the window's edge: {r:?}");
        }
    }
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_all_by_role(egui::accesskit::Role::TextInput).any(|i| i.rect().height() >= 24.0), "form fields too");
}

#[test]
fn settings_pages_all_render_and_their_controls_change_settings() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    for page in ["Git", "Workflow", "Blame", "Hosting", "Issue trackers", "AI", "Agents and links", "Appearance"] {
        pump_for_label(&mut h, page);
        click(&mut h, page);
        h.step();
        no_errors(&h);
    }

    // Theme previews.
    assert_eq!(h.state().settings.theme, gitem_core::settings::ThemeChoice::Dark);
    click(&mut h, "Light theme");
    h.step();
    assert_eq!(h.state().settings.theme, gitem_core::settings::ThemeChoice::Light);

    // A switch toggles from anywhere on its row, and back.
    click(&mut h, "Workflow");
    let before = h.state().settings.prune_on_fetch;
    for expected in [!before, before] {
        pump_for_label(&mut h, "Safety and speed");
        // The row's caption has the same words, so ask for the switch itself.
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Prune when fetching").click();
        h.step();
        h.step();
        assert_eq!(h.state().settings.prune_on_fetch, expected);
    }

    // Segmented choices.
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Rebase").click();
    h.step();
    h.step();
    assert_eq!(h.state().settings.pull_mode, "rebase");

    // Lists grow and shrink.
    click(&mut h, "Hosting");
    click(&mut h, "Add server");
    h.step();
    assert_eq!(h.state().settings.custom_hosts.len(), 1);
    pump_for_label(&mut h, "Remove server");
    click(&mut h, "Remove server");
    h.step();
    assert!(h.state().settings.custom_hosts.is_empty());
    no_errors(&h);
}

#[test]
fn settings_warn_about_an_editor_that_cannot_be_found() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Settings(Box::new(|s| s.default_external_editor = "C:/no/such/editor.exe".into())));
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    pump_for_label(&mut h, "Git");
    click(&mut h, "Git");
    pump_for_label(&mut h, &format!("{} This program can't be found, so opening files will fail.", gitem::chrome::icon::WARNING));
    no_errors(&h);
}

/// Notepad is on every Windows machine, so the list always has something to show.
/// Picking from the list was checked on the real window: this harness's clicks don't
/// reach combo box popups.
#[cfg(windows)]
#[test]
fn the_editor_list_offers_what_was_detected() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    pump_for_label(&mut h, "Git");
    click(&mut h, "Git");
    pump_for_label(&mut h, "Editor");
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    h.step();
    h.step();
    pump_for_label(&mut h, "Notepad");
    no_errors(&h);
}

/// A launcher script is how many editors arrive on Windows, and paths have spaces.
#[cfg(windows)]
#[test]
fn opening_a_file_runs_a_launcher_script_with_a_path_containing_spaces() {
    let d = tempfile::tempdir().unwrap();
    let marker = d.path().join("opened.txt");
    let script = d.path().join("my editor.cmd");
    std::fs::write(&script, format!("@echo %~1> \"{}\"\r\n", marker.display())).unwrap();
    let target = d.path().join("a file with spaces.txt");
    std::fs::write(&target, "x").unwrap();
    let mut errors = Vec::new();
    gitem::app::open_file(&target, &script.to_string_lossy(), &mut errors);
    assert!(errors.is_empty(), "{errors:?}");
    let t = Instant::now();
    while !marker.exists() {
        assert!(t.elapsed() < PATIENCE, "the script didn't run");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(std::fs::read_to_string(&marker).unwrap().contains("a file with spaces.txt"));
}

#[test]
fn opening_with_a_missing_editor_reports_it() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("f.txt");
    std::fs::write(&target, "x").unwrap();
    let mut errors = Vec::new();
    gitem::app::open_file(&target, &d.path().join("no-such-editor.exe").to_string_lossy(), &mut errors);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("f.txt"));
}

fn open_palette(h: &mut Harness<GitemApp>, typed: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::OpenPalette);
    h.step();
    h.step();
    if !typed.is_empty() {
        h.event(egui::Event::Text(typed.into()));
        h.step();
        h.step();
    }
}

#[test]
fn palette_prefixes_narrow_it_to_branches_and_files() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "feature-x"]);
    let mut h = harness(&f);
    settle(&mut h);

    open_palette(&mut h, "@feat");
    assert!(h.query_all_by_label("feature-x").count() >= 2, "listed beside the sidebar's");
    assert!(h.query_by_label("Stage All").is_none(), "commands aren't listed");
    h.key_press(egui::Key::Enter);
    h.step();
    assert!(!h.state().palette_is_open());
    wait_for(&mut h, "the switch", || f.git(&["branch", "--show-current"]).trim() == "feature-x");

    open_palette(&mut h, ":second");
    assert!(h.query_by_label("Stage All").is_none());
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(!h.state().palette_is_open(), "Esc closes it");

    open_palette(&mut h, "/b.txt");
    pump_for_label(&mut h, "b.txt");
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| t.key().contains("b.txt")), "the file opened");
    no_errors(&h);
}

#[test]
fn the_palette_remembers_recent_commands_and_moves_by_keyboard() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);

    open_palette(&mut h, "reflog");
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    assert_eq!(h.state().settings.palette_recent, ["Reflog"]);

    // It now leads the list, so moving down and back returns to it.
    open_palette(&mut h, "");
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::ArrowUp);
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Reflog(_))));
    assert_eq!(h.state().settings.palette_recent, ["Reflog"], "listed once");
    no_errors(&h);
}

#[test]
fn the_palette_footer_switches_modes() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_palette(&mut h, "");
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_by_label("Stage All").is_some());
    click(&mut h, "@ branches");
    h.step();
    assert!(h.query_by_label("Stage All").is_none(), "commands are gone");
    click(&mut h, "@ branches");
    h.step();
    pump_for_label(&mut h, "Stage All");
    no_errors(&h);
}

#[test]
fn accents_and_new_themes_can_be_chosen_and_saved() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    pump_for_label(&mut h, "Teal accent");
    click(&mut h, "Teal accent");
    h.step();
    assert_eq!(h.state().settings.accent, "Teal");
    click(&mut h, "Paper theme");
    h.step();
    assert_eq!(h.state().settings.theme, gitem_core::settings::ThemeChoice::Paper);
    assert_eq!(h.state().settings.accent, "Teal", "the accent stays through a theme change");
    click(&mut h, "Theme default accent");
    h.step();
    assert!(h.state().settings.accent.is_empty());
    no_errors(&h);
}

#[test]
fn the_sidebar_has_a_files_view() {
    let f = Fx::new();
    f.write("src/nested/deep.rs", "fn deep() {}\n");
    f.write("top.txt", "one\n");
    f.commit("files");
    f.write("top.txt", "two\n");
    f.write("fresh.txt", "new\n");
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.query_by_label("Files section").is_none(), "no longer a section of the branch tree");
    click(&mut h, "Files");
    assert!(h.state().settings.sidebar_files, "remembered");

    // Folders first, joined when they hold one folder, and untracked files included.
    pump_for_label(&mut h, "src/nested");
    pump_for_label(&mut h, "top.txt");
    pump_for_label(&mut h, "fresh.txt");
    assert!(h.query_by_label("deep.rs").is_none());
    assert!(h.query_by_label("Branches section").is_none());

    click(&mut h, "src/nested");
    pump_for_label(&mut h, "deep.rs");
    click(&mut h, "deep.rs");
    h.step();
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| t.key().contains("deep.rs")), "the file opened");

    click(&mut h, "Collapse folders");
    h.step();
    assert!(h.query_by_label("deep.rs").is_none());

    // Only what changed: the folder without changes goes.
    click(&mut h, "Changed files only");
    h.step();
    assert!(h.query_by_label("src/nested").is_none());
    assert!(h.query_by_label("top.txt").is_some());
    click(&mut h, "Changed files only");
    h.step();

    // Filtering finds files inside closed folders.
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.step();
    h.event(egui::Event::Text("deep".into()));
    h.step();
    h.step();
    pump_for_label(&mut h, "deep.rs");
    assert!(h.query_by_label("top.txt").is_none());

    click(&mut h, "Repository");
    pump_for_label(&mut h, "Branches section");
    no_errors(&h);
}

#[test]
fn hovering_a_tab_keeps_the_normal_pointer() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    h.step();
    h.step();
    let (path, leaf) = h.state().repos[0].dock.iter_leaves().next().unwrap();
    let at = h.ctx.read_response(gitem::app::dock_tab_id(path, leaf.active.0)).expect("the active tab").rect.center();
    h.event(egui::Event::PointerMoved(at));
    h.step();
    h.step();
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::Default);
}

/// `n` commits, quickly, through fast-import.
fn many_commits(f: &Fx, n: usize) {
    let mut script = String::new();
    for i in 0..n {
        let parent = if i > 0 { format!("from :{i}\n") } else { String::new() };
        let msg = format!("commit number {i}");
        script.push_str(&format!("commit refs/heads/main\nmark :{}\ncommitter T <t@x> {} +0000\ndata {}\n{msg}\n{parent}M 100644 inline f.txt\ndata {}\n{i}\n\n", i + 1, 1_600_000_000 + i, msg.len(), i.to_string().len() + 1));
    }
    let opts = gitem_core::runner::RunOpts { stdin: Some(script.into_bytes()), is_write: true, ..Default::default() };
    let o = f.git.run_opts(Some(&f.root), &["fast-import".to_string(), "--quiet".to_string()], &opts, &mut |_| {}).unwrap();
    assert!(o.ok(), "{}", o.stderr);
    f.git(&["reset", "-q", "--hard", "main"]);
}

fn graph_loaded(h: &Harness<GitemApp>) -> usize {
    h.query_all_by_label("Load all").count()
}

#[test]
fn the_commit_graph_loads_more_from_a_bar_and_then_says_nothing() {
    let f = Fx::new();
    many_commits(&f, 130);
    let ctx = egui::Context::default();
    let mut settings = gitem_core::settings::Settings::default();
    settings.graph_page_size = 100;
    let mut app = GitemApp::with_settings(&ctx, settings);
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    let mut h = builder.build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    );
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "Showing the latest 100 commits");
    assert_eq!(graph_loaded(&h), 1);
    click(&mut h, "Load 100 more");
    // All 130 fit in the next page, so the bar goes away.
    let t = Instant::now();
    while graph_loaded(&h) > 0 {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the bar stayed");
        std::thread::sleep(Duration::from_millis(15));
    }
    no_errors(&h);
}

#[test]
fn the_sidebar_commits_section_loads_more() {
    let f = Fx::new();
    many_commits(&f, 130);
    let mut h = harness(&f);
    // Tall enough to reach the end of a hundred rows without scrolling.
    h.set_size(vec2(1500.0, 3200.0));
    settle(&mut h);
    click(&mut h, "Commits section");
    pump_for_label(&mut h, "Load 100 more");
    // The section animates open, so wait for the button to stop moving.
    settle(&mut h);
    // 130 fit in the next page, so the button goes away.
    click(&mut h, "Load 100 more");
    let t = Instant::now();
    while h.query_by_label("Load 100 more").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the button stayed");
        std::thread::sleep(Duration::from_millis(15));
    }
    no_errors(&h);
}

#[test]
fn search_offers_more_when_it_stops_at_a_limit() {
    let f = Fx::new();
    many_commits(&f, 520);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::search("commit number"));
    pump_for_label(&mut h, "Showing the latest 500 matches");
    click(&mut h, "Load all");
    pump_for_label(&mut h, "520 commits");
    assert!(h.query_by_label("Showing the latest 500 matches").is_none());
    no_errors(&h);
}

#[test]
fn load_more_counts() {
    use gitem::widgets::{LOAD_ALL, LoadMore, grouped};
    assert_eq!(LoadMore::More.apply(100, 50), 150);
    assert_eq!(LoadMore::All.apply(100, 50), LOAD_ALL);
    assert_eq!((grouped(7), grouped(2000), grouped(1234567)), ("7".to_string(), "2,000".to_string(), "1,234,567".to_string()));
}

#[test]
fn local_and_remote_branches_open_to_their_latest_commits() {
    let f = Fx::new();
    many_commits(&f, 30);
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["branch", "side", "HEAD~25"]);

    let mut h = harness(&f);
    settle(&mut h);
    let count = |h: &Harness<GitemApp>, n: usize| h.query_all_by_label(&format!("commit number {n}")).count();

    // A local branch with nothing tracking it: the latest twenty of its 5 are all there.
    let before = count(&h, 4);
    click(&mut h, "Expand side");
    settle(&mut h);
    assert_eq!(count(&h, 4), before + 1, "its tip is listed");
    assert!(h.query_by_label("Load 20 more").is_none(), "it has fewer than a page");
    click(&mut h, "Collapse side");
    settle(&mut h);

    // A longer one stops at twenty, and loading more reaches the oldest.
    let oldest = count(&h, 0);
    click(&mut h, "Expand main");
    settle(&mut h);
    assert_eq!(count(&h, 0), oldest, "commit 0 is past the first twenty");
    click(&mut h, "Load 20 more");
    settle(&mut h);
    assert_eq!(count(&h, 0), oldest + 1, "now it's listed");
    assert!(h.query_by_label("Load 20 more").is_none(), "that was all of them");
    click(&mut h, "Collapse main");
    settle(&mut h);

    // Remote branches open the same way.
    click(&mut h, "Remotes section");
    settle(&mut h);
    click(&mut h, "origin");
    settle(&mut h);
    let tip = count(&h, 29);
    click(&mut h, "Expand origin/main");
    settle(&mut h);
    assert_eq!(count(&h, 29), tip + 1);
    pump_for_label(&mut h, "Load 20 more");
    no_errors(&h);
}

fn selected_in_graph(h: &Harness<GitemApp>) -> Vec<String> {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Graph(g) = t { Some(g.selected().to_vec()) } else { None }).unwrap()
}

fn sha_of(f: &Fx, subject: &str) -> String {
    f.git(&["log", "--all", "--format=%H", "--fixed-strings", "--grep", subject]).lines().next().unwrap().to_string()
}

#[test]
fn shift_click_after_the_graph_got_shorter_does_not_panic() {
    let f = Fx::new();
    many_commits(&f, 30);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "commit number 3");
    h.get_by_label("commit number 3").click();
    h.step();
    h.step();
    // Row 26, then history shrinks to 3 rows, as a reset or a filter would do it.
    f.git(&["reset", "-q", "--hard", "HEAD~27"]);
    let t = Instant::now();
    while h.query_by_label("commit number 29").is_some() || h.query_by_label("commit number 0").is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the graph never reloaded");
        std::thread::sleep(Duration::from_millis(15));
    }
    h.step();
    h.get_by_label("commit number 1").click_modifiers(egui::Modifiers::SHIFT);
    h.step();
    h.step();
    // The anchor isn't on screen any more, so only the clicked row can be meant.
    assert_eq!(selected_in_graph(&h), [sha_of(&f, "commit number 1")]);
}

#[test]
fn shift_click_ranges_from_the_commit_clicked_even_after_rows_moved() {
    let f = Fx::new();
    many_commits(&f, 12);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "commit number 8");
    h.get_by_label("commit number 8").click();
    h.step();
    h.step();
    // New commits push every row down, so a remembered row index now means another commit.
    for i in 0..3 {
        f.write("f.txt", &format!("new {i}\n"));
        f.commit(&format!("newer {i}"));
    }
    pump_for_label(&mut h, "newer 2");
    h.step();
    h.get_by_label("commit number 6").click_modifiers(egui::Modifiers::SHIFT);
    h.step();
    h.step();
    let want: Vec<String> = [8, 7, 6].iter().map(|n| sha_of(&f, &format!("commit number {n}"))).collect();
    assert_eq!(selected_in_graph(&h), want);
}

/// Rows of the graph list have no inspector or menu around them.
fn row_on_screen(h: &Harness<GitemApp>, label: &str) -> bool {
    h.query_all_by_label(label).any(|n| {
        let r = n.rect();
        r.left() < 700.0 && r.top() >= 0.0 && r.bottom() <= 940.0
    })
}

#[test]
fn arrow_keys_keep_the_selected_graph_row_on_screen() {
    let f = Fx::new();
    many_commits(&f, 300);
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "commit number 299");
    for row in 0..200 {
        h.key_press(egui::Key::ArrowDown);
        h.step();
        h.step();
        let label = format!("commit number {}", 299 - row);
        assert!(row_on_screen(&h, &label), "row {row} is off screen");
    }
}

#[test]
fn arrow_keys_work_in_the_graph_with_uncommitted_changes() {
    let f = Fx::new();
    sample_history(&f);
    f.write("a.txt", "changed\n");
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Work in progress");
    assert!(selected_in_graph(&h).is_empty());
    h.key_press(egui::Key::J);
    h.step();
    h.step();
    assert_eq!(selected_in_graph(&h), [sha_of(&f, "second")], "the first key picks the newest commit");
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.step();
    assert_eq!(selected_in_graph(&h), [sha_of(&f, "first")]);
    h.key_press(egui::Key::K);
    h.step();
    h.step();
    h.key_press(egui::Key::K);
    h.step();
    h.step();
    assert_eq!(selected_in_graph(&h), [sha_of(&f, "second")], "going up stops at the newest commit");
}

/// Text the last frame painted: where it starts, where it's clipped, what it says.
fn painted_text(h: &Harness<GitemApp>) -> Vec<(egui::Pos2, egui::Rect, String)> {
    h.output().shapes.iter().filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some((t.pos, c.clip_rect, t.galley.job.text.clone())) } else { None }).collect()
}

/// What the last frame sends to the GPU. egui-wgpu asks for double the buffer as soon
/// as a frame grows, and the limit is 256 MiB, so a frame over 128 MiB can crash the app.
fn vertex_bytes(h: &Harness<GitemApp>) -> usize {
    let shapes = h.output().shapes.clone();
    let meshes = h.ctx.tessellate(shapes, 1.0);
    meshes.iter().map(|m| if let egui::epaint::Primitive::Mesh(m) = &m.primitive { m.vertices.len() * std::mem::size_of::<egui::epaint::Vertex>() } else { 0 }).sum()
}

const GPU_SAFE_BYTES: usize = 128 * 1024 * 1024;

fn assert_long_line_is_cut(h: &Harness<GitemApp>, what: &str) {
    let bytes = vertex_bytes(h);
    assert!(bytes < GPU_SAFE_BYTES, "{what}: {} MiB of vertices", bytes / 1024 / 1024);
    assert!(painted_text(h).iter().any(|(_, _, t)| t.contains("line truncated")), "{what}: no marker says the line is cut");
}

#[test]
fn a_very_long_line_is_cut_in_the_file_and_diff_views_but_stays_whole_in_git() {
    let f = Fx::new();
    let old = "a".repeat(4_000_000);
    let new = "b".repeat(4_000_000);
    f.write("long.txt", &format!("{old}\n"));
    f.commit("long line");
    f.write("long.txt", &format!("{new}\n"));
    let mut h = harness(&f);
    settle(&mut h);

    h.state_mut().repos[0].open_tab(Tab::file("long.txt", None, None));
    wait_until_quiet(&mut h);
    assert_long_line_is_cut(&h, "file view");

    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("long.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    wait_until_quiet(&mut h);
    assert_long_line_is_cut(&h, "side by side");
    click(&mut h, "Unified");
    assert_long_line_is_cut(&h, "unified");

    // Only the display is cut: staging takes the real line.
    click(&mut h, "Stage hunk");
    wait_for(&mut h, "the hunk to be staged", || f.git(&["diff", "--cached", "--name-only"]).contains("long.txt"));
    assert_eq!(f.git(&["show", ":long.txt"]).trim_end(), new);
    assert_eq!(std::fs::read_to_string(f.root.join("long.txt")).unwrap().trim_end(), new);
    no_errors(&h);
}

#[test]
fn a_screen_full_of_long_lines_stays_within_the_gpu_buffer() {
    let f = Fx::new();
    let body = |c: &str| (0..200).map(|i| format!("{i}{}\n", c.repeat(200_000))).collect::<String>();
    f.write("many.txt", &body("a"));
    f.commit("many long lines");
    f.write("many.txt", &body("b"));
    let mut h = harness(&f);
    settle(&mut h);
    h.set_size(vec2(3840.0, 2160.0));
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("many.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    wait_until_quiet(&mut h);
    let bytes = vertex_bytes(&h);
    assert!(bytes < GPU_SAFE_BYTES, "{} MiB of vertices", bytes / 1024 / 1024);
}

#[test]
fn one_long_line_leaves_the_new_side_of_a_side_by_side_diff_on_screen() {
    let f = Fx::new();
    f.write("f.txt", &format!("{}\nold short\n", "x".repeat(3000)));
    f.commit("base");
    f.write("f.txt", &format!("{}\nnew short\n", "y".repeat(3000)));
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("f.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    wait_until_quiet(&mut h);
    let texts = painted_text(&h);
    let at = |needle: &str| texts.iter().find(|(_, _, t)| t.contains(needle)).unwrap_or_else(|| panic!("{needle} isn't painted")).clone();
    let (old, new) = (at("old short"), at("new short"));
    assert_eq!(old.0.y, new.0.y, "the two sides of a change share a row");
    assert!(old.0.x < new.0.x);
    assert!(new.0.x < 1150.0, "the new side starts off screen, at x={}", new.0.x);
    assert!(new.1.contains(new.0), "the new side is clipped away");

    // Scrolling sideways moves both sides by the same amount, and stays inside each half.
    let long = at("yyyy");
    let row = long.1.center();
    h.event(egui::Event::PointerMoved(row));
    h.step();
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(-400.0, 0.0), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE });
    for _ in 0..30 {
        h.step();
    }
    let texts = painted_text(&h);
    let moved = |needle: &str| texts.iter().find(|(_, _, t)| t.contains(needle)).unwrap().clone();
    let (long2, x2) = (moved("yyyy"), moved("xxxx"));
    assert!(long2.0.x < long.0.x, "the long line didn't scroll");
    assert_eq!(long.0.x - long2.0.x, at("xxxx").0.x - x2.0.x);
    assert_eq!(long2.1, long.1, "each side stays in its own half");
}

/// The Changes tab with a message typed in the box, which keeps the focus.
fn changes_with_message(h: &mut Harness<GitemApp>, msg: &str) {
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(h, "Commit to main");
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text(msg);
    h.step();
    h.step();
}

fn staged_and_unstaged(f: &Fx) {
    sample_history(f);
    f.write("staged.txt", "s\n");
    f.git(&["add", "staged.txt"]);
    f.write("a.txt", "unstaged\n");
}

#[test]
fn ctrl_enter_in_a_form_submits_only_the_form() {
    let f = Fx::new();
    staged_and_unstaged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "half written");
    // The real route: a pointer click moves focus off the commit box, and the form's
    // first field takes it.
    let more = gitem::chrome::icon::DOTS_THREE;
    click(&mut h, more);
    click(&mut h, "Stash…");
    h.step();
    assert!(!h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "this test is about focus being in the form");
    h.event(egui::Event::Text("hello".into()));
    h.step();
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Enter);
    wait_for(&mut h, "the stash", || f.git(&["stash", "list"]).contains("hello"));
    wait_until_quiet(&mut h);
    assert_eq!(f.subjects(), ["second", "first"], "the commit box must not fire too");
    no_errors(&h);
}

#[test]
fn ctrl_enter_goes_to_an_open_form_even_with_a_message_written() {
    let f = Fx::new();
    staged_and_unstaged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "from the box");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::stash_form(Vec::new())));
    wait_until_quiet(&mut h);
    // A dialog sits on top of the app, and the keyboard goes with it.
    assert!(!h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "the box lost the keyboard");
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Enter);
    wait_for(&mut h, "the stash", || f.git(&["stash", "list"]).contains("WIP") || f.git(&["stash", "list"]).contains("On main"));
    wait_until_quiet(&mut h);
    assert_eq!(f.subjects(), ["second", "first"], "the commit box must not fire too");
}

#[test]
fn ctrl_enter_still_commits_with_no_dialog_open() {
    let f = Fx::new();
    staged_and_unstaged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "plain commit");
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Enter);
    wait_for(&mut h, "the commit", || f.subjects()[0] == "plain commit");
    wait_until_quiet(&mut h);
    assert_eq!(f.subjects().len(), 3, "exactly one commit");
}

fn merge_with_staged_files(f: &Fx) {
    f.write("auto.txt", "base\n");
    f.write("c.txt", "top\nmiddle\nbottom\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("auto.txt", "from side\n");
    f.write("c.txt", "TOP-SIDE\nmiddle\nbottom\n");
    f.commit("side change");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "TOP-MAIN\nmiddle\nbottom\n");
    f.commit("main change");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "side"]).unwrap().ok(), "merge should conflict");
}

#[test]
fn unstage_all_during_a_merge_keeps_the_merge() {
    let f = Fx::new();
    merge_with_staged_files(&f);
    assert!(f.git(&["status", "--short"]).contains("M  auto.txt"));
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    click(&mut h, "Unstage all");
    wait_for(&mut h, "the file to be unstaged", || !f.git(&["status", "--short"]).contains("M  auto.txt"));
    wait_until_quiet(&mut h);
    assert!(f.root.join(".git/MERGE_HEAD").exists(), "the merge was cancelled");
    assert!(f.git(&["status", "--short"]).contains("UU c.txt"), "the conflict was lost: {}", f.git(&["status", "--short"]));
    no_errors(&h);
}

fn open_composer(h: &mut Harness<GitemApp>) {
    settle(h);
    h.state_mut().repos[0].open_tab(Tab::Composer(Box::default()));
    pump_for_label(h, "New commit");
}

/// Plans `path` as a new commit with this message, by the same clicks as a user.
fn plan_commit(h: &mut Harness<GitemApp>, path_index: usize, message: &str) {
    pump_for_any(h, "Add \u{E136}");
    click_nth(h, "Add \u{E136}", path_index);
    pump_for_any(h, "New commit");
    // The toolbar's button comes first and the one in the menu after it.
    let n = h.query_all_by_label("New commit").count();
    click_nth(h, "New commit", n - 1);
    let input = egui::accesskit::Role::MultilineTextInput;
    pump_for_role(h, input);
    let last = h.query_all_by_role(input).count() - 1;
    h.query_all_by_role(input).nth(last).unwrap().focus();
    h.step();
    h.query_all_by_role(input).nth(last).unwrap().type_text(message);
    h.step();
    h.step();
}

fn composer_plan_len(h: &Harness<GitemApp>) -> Option<String> {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Composer(c) = t { c.unsaved() } else { None })
}

#[test]
fn the_composer_makes_one_commit_per_group_with_only_its_files() {
    let f = Fx::new();
    sample_history(&f);
    f.write("one.txt", "1\n");
    f.write("two.txt", "2\n");
    f.write("left.txt", "not in the plan\n");
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "Add left aside"); // left.txt sorts first
    plan_commit(&mut h, 0, "Add one");
    click(&mut h, "Create 2 commits");
    wait_for(&mut h, "both commits", || f.subjects().first().is_some_and(|s| s == "Add one"));
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["show", "--format=", "--name-only", "HEAD"]).trim(), "one.txt");
    assert_eq!(f.git(&["show", "--format=", "--name-only", "HEAD~1"]).trim(), "left.txt");
    assert!(f.git(&["status", "--short"]).contains("?? two.txt"), "not in the plan, not touched");
    assert_eq!(composer_plan_len(&h), None, "the plan is done");
}

/// The plan used to be cleared on the click, so a refusal lost every message.
#[test]
fn a_rejected_composer_commit_keeps_what_has_not_landed() {
    let f = Fx::new();
    sample_history(&f);
    f.write("one.txt", "1\n");
    f.write("two.txt", "2\n");
    let hook = f.root.join(".git/hooks/commit-msg");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nif grep -q refused \"$1\"; then echo no >&2; exit 1; fi\n").unwrap();
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "fine one");
    plan_commit(&mut h, 0, "refused two");
    click(&mut h, "Create 2 commits");
    pump_for_error(&mut h);
    wait_until_quiet(&mut h);
    assert_eq!(f.subjects()[0], "fine one", "the first landed");
    assert_eq!(composer_plan_len(&h).as_deref(), Some("Composer: a plan of 1 commit"), "the second is still planned");

    // Fix the hook's objection and go again, with the message intact.
    std::fs::remove_file(&hook).unwrap();
    click(&mut h, "Dismiss");
    click(&mut h, "Create 1 commit");
    wait_for(&mut h, "the second commit", || f.subjects()[0] == "refused two");
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["show", "--format=", "--name-only", "HEAD"]).trim(), "two.txt");
}

/// Line-level staging used to be flattened with no word about it.
#[test]
fn the_composer_asks_before_committing_a_partly_staged_file_whole() {
    let f = Fx::new();
    f.write("f.txt", "one\n");
    f.commit("base");
    f.write("f.txt", "one\ntwo\n");
    f.git(&["add", "f.txt"]);
    f.write("f.txt", "one\ntwo\nleft unstaged\n");
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "the plan");
    click(&mut h, "Create 1 commit");
    pump_for_label(&mut h, "Commit the whole files");
    assert_no_op_started(&mut h, "nothing is committed before answering");
    click(&mut h, "Go back");
    assert!(h.query_by_label("Commit the whole files").is_none());
    assert_eq!(f.subjects()[0], "base");
    assert_eq!(f.git(&["show", ":f.txt"]), "one\ntwo\n", "the staging is as it was");
    click(&mut h, "Create 1 commit");
    click(&mut h, "Commit the whole files");
    wait_for(&mut h, "the commit", || f.subjects()[0] == "the plan");
    assert_eq!(f.git(&["show", "HEAD:f.txt"]), "one\ntwo\nleft unstaged\n");
}

/// With no partly staged file in the plan, staging elsewhere is left alone and nothing asks.
#[test]
fn the_composer_leaves_other_staged_files_staged() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base");
    f.write("a.txt", "a2\n");
    f.git(&["add", "a.txt"]);
    f.write("a.txt", "a2\nunstaged\n");
    f.write("b.txt", "b2\n");
    f.git(&["add", "b.txt"]);
    let mut h = harness(&f);
    open_composer(&mut h);
    // Files sort by path, so b.txt is the second. Remove a.txt's confirm from the picture.
    plan_commit(&mut h, 1, "just b");
    click(&mut h, "Create 1 commit");
    wait_for(&mut h, "the commit", || f.subjects()[0] == "just b");
    wait_until_quiet(&mut h);
    assert_eq!(f.git(&["show", "--format=", "--name-only", "HEAD"]).trim(), "b.txt");
    assert_eq!(f.git(&["show", ":a.txt"]), "a2\n", "a.txt's staged part is still staged");
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "a2\nunstaged\n");
}

/// A file briefly leaving the status used to leave its group for good.
#[test]
fn a_planned_file_that_goes_clean_for_a_moment_stays_in_the_plan() {
    let f = Fx::new();
    f.write("f.txt", "one\n");
    f.commit("base");
    f.write("f.txt", "edited\n");
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "keep me");
    f.write("f.txt", "one\n");
    pump_for_label(&mut h, "no changes now");
    f.write("f.txt", "edited\n");
    let t = Instant::now();
    while h.query_by_label("no changes now").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the file never came back");
        std::thread::sleep(Duration::from_millis(15));
    }
    click(&mut h, "Create 1 commit");
    wait_for(&mut h, "the commit", || f.subjects()[0] == "keep me");
}

/// Splitting a commit at an edit stop is the reason to compose during a rebase.
#[test]
fn the_composer_can_split_a_commit_at_a_rebase_edit_stop() {
    let f = Fx::new();
    f.write("base.txt", "b\n");
    f.commit("base");
    f.write("one.txt", "1\n");
    f.write("two.txt", "2\n");
    f.commit("too big");
    f.write("after.txt", "a\n");
    f.commit("after");
    let edit = gitem_core::runner::RunOpts { env: vec![("GIT_SEQUENCE_EDITOR".into(), "sed -i 1s/^pick/edit/".into())], ..Default::default() };
    let o = f.git.run_opts(Some(&f.root), &["rebase".to_string(), "-i".to_string(), "HEAD~2".to_string()], &edit, &mut |_| {}).unwrap();
    assert!(o.ok(), "{}", o.stderr);
    f.git(&["reset", "-q", "HEAD~1"]);
    let mut h = harness(&f);
    open_composer(&mut h);
    plan_commit(&mut h, 0, "just one");
    plan_commit(&mut h, 0, "just two");
    assert!(h.query_by_label_contains("is in progress").is_none(), "this is the allowed case");
    click(&mut h, "Create 2 commits");
    wait_for(&mut h, "both commits", || f.subjects()[0] == "just two");
    wait_until_quiet(&mut h);
    click(&mut h, "Continue");
    wait_for(&mut h, "the rebase to finish", || !f.root.join(".git/rebase-merge").exists());
    assert_eq!(f.subjects(), vec!["after", "just two", "just one", "base"]);
}

#[test]
fn the_composer_refuses_to_commit_during_a_merge() {
    let f = Fx::new();
    merge_with_staged_files(&f);
    let before = f.git(&["rev-parse", "HEAD"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Composer(Box::default()));
    click(&mut h, "New commit");
    let msg = h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).last().unwrap().rect();
    h.event(egui::Event::PointerMoved(msg.center()));
    h.event(egui::Event::PointerButton { pos: msg.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: msg.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
    h.event(egui::Event::Text("one commit".into()));
    h.step();
    h.query_all_by_label("Add \u{E136}").next().unwrap().click();
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Commit 1").click();
    h.step();
    h.step();
    h.step();
    pump_for_label(&mut h, "Create 1 commit");
    h.get_by_label("Create 1 commit").click();
    for _ in 0..3 {
        h.step();
    }
    assert_no_op_started(&mut h, "the composer started git during a merge");
    assert!(f.root.join(".git/MERGE_HEAD").exists(), "the merge was cancelled");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert!(h.query_by_label_contains("Finish or abort it first").is_some(), "the reason isn't on screen");
}

fn tab_titles(h: &Harness<GitemApp>) -> Vec<String> {
    h.state().repos[0].dock.iter_all_tabs().map(|(_, t)| t.title()).collect()
}

fn stage_and_unstage_with_row_buttons(tree: bool) {
    let f = Fx::new();
    sample_history(&f);
    f.write("b.txt", "changed\n");
    f.write("dir/deep.txt", "new\n");
    let mut h = harness(&f);
    h.state_mut().settings.changes_tree = tree;
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    let open_tabs = tab_titles(&h).len();

    pump_for_label(&mut h, "b.txt");
    assert!(h.query_by_label("Stage b.txt").is_none(), "only on the row under the pointer");
    h.get_by_label("b.txt").hover();
    h.step();
    h.step();
    click(&mut h, "Stage b.txt");
    wait_for(&mut h, "b.txt to be staged", || f.git(&["status", "--short"]).contains("M  b.txt"));
    wait_until_quiet(&mut h);

    pump_for_label(&mut h, "dir/deep.txt");
    h.get_by_label("dir/deep.txt").hover();
    h.step();
    h.step();
    click(&mut h, "Stage dir/deep.txt");
    wait_for(&mut h, "dir/deep.txt to be staged", || f.git(&["status", "--short"]).contains("A  dir/deep.txt"));
    wait_until_quiet(&mut h);

    h.get_by_label("b.txt").hover();
    h.step();
    h.step();
    click(&mut h, "Unstage b.txt");
    wait_for(&mut h, "b.txt to be unstaged", || f.git(&["status", "--short"]).contains(" M b.txt"));
    wait_until_quiet(&mut h);
    assert_eq!(tab_titles(&h).len(), open_tabs, "the button must not also open the diff: {:?}", tab_titles(&h));
    no_errors(&h);
}

#[test]
fn rows_have_a_stage_button_in_the_flat_list() {
    stage_and_unstage_with_row_buttons(false);
}

#[test]
fn rows_have_a_stage_button_in_the_tree() {
    stage_and_unstage_with_row_buttons(true);
}

fn push_form_open(h: &mut Harness<GitemApp>) {
    let ctx = h.ctx.clone();
    let confirm = h.state().settings.confirm_destructive;
    h.state_mut().apply_action(&ctx, Action::Form(forms::push_form(Some("main".into()), vec!["origin".into()], true, confirm)));
    pump_for_label(h, "Force (can overwrite others' work)");
    wait_until_quiet(h);
}

fn rewrite_local_history(r: &Remote) {
    r.f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    r.f.write("rewritten.txt", "r\n");
    r.f.commit("rewritten");
}

#[test]
fn force_push_from_the_form_asks_first_and_shows_the_command() {
    let r = with_remote();
    rewrite_local_history(&r);
    let before = r.remote_tip();
    let mut h = harness(&r.f);
    settle(&mut h);
    push_form_open(&mut h);
    click(&mut h, "Force (can overwrite others' work)");
    click_last(&mut h, "Push");
    pump_for_any(&mut h, "Force push");
    assert!(h.query_by_label_contains("git push --progress --force -- origin main").is_some(), "the exact command is shown");
    assert_no_op_started(&mut h, "pushed without waiting for an answer");
    assert_eq!(r.remote_tip(), before);
    click(&mut h, "Cancel");
    assert_no_op_started(&mut h, "pushed after Cancel");
    assert_eq!(r.remote_tip(), before);

    push_form_open(&mut h);
    click(&mut h, "Force (can overwrite others' work)");
    click_last(&mut h, "Push");
    pump_for_any(&mut h, "Force push");
    h.query_all_by_label("Force push").last().unwrap().click();
    wait_for(&mut h, "the push", || r.remote_tip() != before);
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn force_push_runs_directly_with_confirmations_off() {
    let r = with_remote();
    rewrite_local_history(&r);
    let before = r.remote_tip();
    let mut h = harness(&r.f);
    h.state_mut().settings.confirm_destructive = false;
    settle(&mut h);
    push_form_open(&mut h);
    click(&mut h, "Force (can overwrite others' work)");
    click_last(&mut h, "Push");
    wait_for(&mut h, "the push", || r.remote_tip() != before);
    no_errors(&h);
}

#[test]
fn force_with_lease_alone_does_not_ask() {
    let r = with_remote();
    rewrite_local_history(&r);
    r.f.git(&["fetch", "-q", "origin"]);
    let before = r.remote_tip();
    let mut h = harness(&r.f);
    settle(&mut h);
    push_form_open(&mut h);
    click(&mut h, "Force with lease");
    click_last(&mut h, "Push");
    wait_for(&mut h, "the push", || r.remote_tip() != before);
    no_errors(&h);
}

#[test]
fn ticking_both_force_options_is_refused_rather_than_dropping_the_lease() {
    let r = with_remote();
    rewrite_local_history(&r);
    let before = r.remote_tip();
    let mut h = harness(&r.f);
    h.state_mut().settings.confirm_destructive = false;
    settle(&mut h);
    push_form_open(&mut h);
    click(&mut h, "Force with lease");
    click(&mut h, "Force (can overwrite others' work)");
    click_last(&mut h, "Push");
    pump_for_label(&mut h, "Choose Force with lease or Force, not both.");
    assert_no_op_started(&mut h, "pushed with both ticked");
    assert_eq!(r.remote_tip(), before);
}

/// Until an error dialog is up. Its title names what failed, so tests look at the app.
fn pump_for_error(h: &mut Harness<GitemApp>) {
    pump(h, |a| !a.errors_showing().is_empty(), "an error");
    h.step();
    h.step();
}

fn pump_for_any(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_all_by_label(label).next().is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// For a label that's also on something behind a dialog, or beside the list in view.
fn click_last(h: &mut Harness<GitemApp>, label: &str) {
    pump_for_any(h, label);
    h.query_all_by_label(label).last().unwrap().click();
    h.step();
    h.step();
}

fn delete_button_of(h: &Harness<GitemApp>, name: &str) -> egui::Pos2 {
    let y = h.get_by_label(name).rect().center().y;
    h.query_all_by_label("Delete").find(|n| (n.rect().center().y - y).abs() < 12.0).unwrap_or_else(|| panic!("no Delete on the row of {name}")).rect().center()
}

fn click_at(h: &mut Harness<GitemApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
}

fn fake_token(host: &str) -> String {
    gitem_core::cred::set_forge_token(host, "", "FAKE_SECRET_VALUE").unwrap();
    format!("{}{host}", gitem_core::cred::PREFIX_FORGE)
}

fn open_credentials(h: &mut Harness<GitemApp>) {
    h.state_mut().repos[0].open_tab(Tab::Credentials(Box::default()));
    wait_until_quiet(h);
    click_last(h, "Refresh");
}

#[test]
fn deleting_a_stored_secret_asks_first() {
    let f = Fx::new();
    sample_history(&f);
    let host = "ask-first.invalid";
    let target = fake_token(host);
    let mut h = harness(&f);
    settle(&mut h);
    open_credentials(&mut h);
    pump_for_label(&mut h, host);

    let at = delete_button_of(&h, host);
    click_at(&mut h, at);
    assert!(gitem_core::cred::get(&target).unwrap().is_some(), "deleted without asking");
    pump_for_label(&mut h, "Delete this secret?");
    click(&mut h, "Keep it");
    assert!(gitem_core::cred::get(&target).unwrap().is_some(), "Keep it deleted it");
    assert!(h.query_by_label("Delete this secret?").is_none());

    let at = delete_button_of(&h, host);
    click_at(&mut h, at);
    pump_for_label(&mut h, "Delete this secret?");
    click(&mut h, "Delete secret");
    assert!(gitem_core::cred::get(&target).unwrap().is_none(), "the confirmed delete didn't happen");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label(host).is_none(), "the row is still listed");
}

#[test]
fn deleting_a_stored_secret_does_not_ask_with_confirmations_off() {
    let f = Fx::new();
    sample_history(&f);
    let host = "no-confirm.invalid";
    let target = fake_token(host);
    let mut h = harness(&f);
    h.state_mut().settings.confirm_destructive = false;
    settle(&mut h);
    open_credentials(&mut h);
    pump_for_label(&mut h, host);
    let at = delete_button_of(&h, host);
    click_at(&mut h, at);
    assert!(gitem_core::cred::get(&target).unwrap().is_none());
}

#[test]
fn a_revealed_secret_hides_when_you_leave_the_page_or_the_tab() {
    let f = Fx::new();
    sample_history(&f);
    let host = "reveal.invalid";
    let target = fake_token(host);
    let mut h = harness(&f);
    settle(&mut h);
    open_credentials(&mut h);
    pump_for_label(&mut h, host);
    let reveal = |h: &mut Harness<GitemApp>| {
        let y = h.get_by_label(host).rect().center().y;
        let at = h.query_all_by_label("Reveal").find(|n| (n.rect().center().y - y).abs() < 12.0).unwrap().rect().center();
        click_at(h, at);
        pump_for_label(h, "FAKE_SECRET_VALUE");
    };
    reveal(&mut h);
    click(&mut h, "SSH keys");
    wait_until_quiet(&mut h);
    click(&mut h, "Stored secrets");
    assert!(h.query_by_label("FAKE_SECRET_VALUE").is_none(), "still shown after changing page");

    reveal(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Console(Box::default()));
    wait_until_quiet(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Credentials(Box::default()));
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("FAKE_SECRET_VALUE").is_none(), "still shown after changing tab");
    gitem_core::cred::delete(&target).unwrap();
}

fn git_commands_matching(f: &Fx, needle: &str) -> usize {
    f.git.log.lock().unwrap().iter().filter(|e| e.command.contains(needle)).count()
}

#[test]
fn the_co_authors_menu_reads_history_once_while_open() {
    let f = Fx::new();
    sample_history(&f);
    f.write("x.txt", "x
");
    f.git(&["add", "-A"]);
    f.git(&["-c", "user.name=Other Person", "-c", "user.email=other@example.com", "commit", "-q", "-m", "by someone else"]);
    f.write("y.txt", "y
");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    wait_until_quiet(&mut h);
    let before = git_commands_matching(&f, "user.email");
    click(&mut h, "Co-authors \u{E136}");
    pump_for_label(&mut h, "Other Person <other@example.com>");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(git_commands_matching(&f, "user.email"), before + 1, "git should run once, however many frames the menu is open for");
}

#[test]
fn the_about_box_and_signing_page_run_git_once() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::About);
    pump_for_label(&mut h, "About gitem");
    for _ in 0..5 {
        h.step();
    }
    let versions = git_commands_matching(&f, "--version");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(git_commands_matching(&f, "--version"), versions, "git --version ran on every frame");
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();

    h.state_mut().repos[0].open_tab(Tab::Credentials(Box::default()));
    click(&mut h, "Commit signing");
    wait_until_quiet(&mut h);
    let reads = git_commands_matching(&f, "allowedSignersFile");
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(git_commands_matching(&f, "allowedSignersFile"), reads, "git config ran on every frame");
}

#[test]
fn line_actions_stay_clickable_at_the_default_window_size() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\nthree\n");
    f.commit("base");
    f.write("f.txt", "one\nTWO\nthree\n");
    for (width, mode) in [(1500.0, "Unified"), (1400.0, "Unified"), (1100.0, "Unified"), (1500.0, "Side by side")] {
        let mut h = harness(&f);
        h.set_size(vec2(width, 940.0));
        settle(&mut h);
        h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("f.txt".into())));
        pump_for_label(&mut h, "Stage hunk");
        click(&mut h, mode);
        // The line's gutter is left of its text, on the first row whose text changed.
        let head = h.get_by_label("Stage hunk").rect();
        let row_h = 19.0;
        let panel_left = h.get_by_label("Side by side").rect().left();
        let added_row = if mode == "Unified" { 3.0 } else { 2.0 };
        click_at(&mut h, egui::pos2(panel_left + 20.0, head.center().y + row_h * added_row));
        pump_for_label(&mut h, "Stage 1 selected line");
        let stage = h.get_by_label("Stage 1 selected line").rect();
        let discard = h.get_by_label("Discard selected…").rect();
        let details_left = h.query_all_by_label("Details").map(|n| n.rect().left()).fold(width, f32::min);
        assert!(stage.right() <= details_left && discard.right() <= details_left, "{mode} at {width}: buttons run under the Details panel at {details_left}: stage {stage:?}, discard {discard:?}");
        click(&mut h, "Stage 1 selected line");
        wait_for(&mut h, "the line to be staged", || !f.git(&["diff", "--cached", "--name-only"]).trim().is_empty());
        wait_until_quiet(&mut h);
        no_errors(&h);
        f.git(&["reset", "-q"]);
    }
}

#[test]
fn a_crash_report_is_offered_once_and_then_kept_quietly() {
    let f = Fx::new();
    sample_history(&f);
    let d = tempfile::tempdir().unwrap();
    let report = d.path().join("crash-1.log");
    std::fs::write(&report, "gitem crash report").unwrap();
    let mut h = harness(&f);
    h.state_mut().crash_reports = gitem::crash::pending(d.path());
    settle(&mut h);
    pump_for_label(&mut h, "gitem crashed last time");
    click(&mut h, "Dismiss");
    assert!(h.query_by_label("gitem crashed last time").is_none());
    assert!(gitem::crash::pending(d.path()).is_empty(), "it won't be offered again");
    assert!(d.path().join("crash-1.seen").exists(), "the file stays");
}

#[test]
fn a_40_byte_branch_name_with_a_multibyte_character_does_not_panic() {
    let f = Fx::new();
    sample_history(&f);
    let name = format!("fix-ab\u{f6}{}", "x".repeat(32));
    assert_eq!(name.len(), 40);
    f.git(&["branch", &name]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::compare(&name, "HEAD"));
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Range(name.clone(), "HEAD".into()), None));
    wait_until_quiet(&mut h);
    no_errors(&h);
}

/// A 30-line file with one line changed near the top and one near the bottom, so a
/// diff of it has two hunks.
fn two_hunks(f: &Fx) -> (String, String) {
    let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("f.txt", &base);
    f.commit("base");
    let edited = base.replace("line 2\n", "line 2 x\n").replace("line 25\n", "line 25 y\n");
    f.write("f.txt", &edited);
    (base, edited)
}

/// Empty while git is replacing the file, which a test polling it can catch on Windows.
fn on_disk(f: &Fx) -> String {
    std::fs::read_to_string(f.root.join("f.txt")).unwrap_or_default()
}

fn in_index(f: &Fx) -> String {
    f.git(&["show", ":f.txt"])
}

fn open_diff(h: &mut Harness<GitemApp>, src: DiffSource, first_button: &str) {
    h.state_mut().repos[0].open_tab(Tab::diff(src, Some("f.txt".into())));
    pump_for_any(h, first_button);
    click(h, "Unified");
}

/// The gutter of a row in a unified diff, counted from its hunk's header. Each hunk
/// has its own button on the header row, so the row's position comes from that.
fn click_line(h: &mut Harness<GitemApp>, button: &str, hunk: usize, row: f32) {
    let head = h.query_all_by_label(button).nth(hunk).unwrap_or_else(|| panic!("no hunk {hunk}")).rect();
    let left = h.get_by_label("Side by side").rect().left();
    click_at(h, egui::pos2(left + 20.0, head.center().y + 19.0 * row));
}

fn pump_for_role(h: &mut Harness<GitemApp>, role: egui::accesskit::Role) {
    let t = Instant::now();
    while h.query_all_by_role(role).next().is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget with the role {role:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn click_nth(h: &mut Harness<GitemApp>, label: &str, n: usize) {
    pump_for_any(h, label);
    h.query_all_by_label(label).nth(n).unwrap_or_else(|| panic!("no {label} number {n}")).click();
    h.step();
    h.step();
}

/// A new window spends its first frames measuring itself, when a click can't land.
fn confirm_discard(h: &mut Harness<GitemApp>) {
    pump_for_label(h, "Discard");
    for _ in 0..3 {
        h.step();
    }
    click(h, "Discard");
}

#[test]
fn discard_hunk_asks_first_and_reverts_only_that_hunk() {
    let f = Fx::new();
    let (base, edited) = two_hunks(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Discard hunk…");
    click_nth(&mut h, "Discard hunk…", 0);
    pump_for_label(&mut h, "Discard");
    assert_no_op_started(&mut h, "nothing is discarded before confirming");
    assert_eq!(on_disk(&f), edited);
    click(&mut h, "Cancel");
    assert_no_op_started(&mut h, "cancel leaves the file alone");
    assert_eq!(on_disk(&f), edited);

    click_nth(&mut h, "Discard hunk…", 0);
    click(&mut h, "Discard");
    let want = base.replace("line 25\n", "line 25 y\n");
    wait_for(&mut h, "the hunk to be discarded", || on_disk(&f) == want);
    assert_eq!(in_index(&f), base, "the index isn't involved");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn discard_hunk_picks_the_second_hunk_when_that_is_the_one_clicked() {
    let f = Fx::new();
    let (base, _) = two_hunks(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Discard hunk…");
    click_nth(&mut h, "Discard hunk…", 1);
    confirm_discard(&mut h);
    let want = base.replace("line 2\n", "line 2 x\n");
    wait_for(&mut h, "the hunk to be discarded", || on_disk(&f) == want);
    no_errors(&h);
}

#[test]
fn discard_selected_asks_first_and_reverts_only_those_lines() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\nthree\n");
    f.commit("base");
    f.write("f.txt", "one\nnew a\nnew b\ntwo\nthree\n");
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Stage hunk");
    // Rows: one, new a, new b. Pick "new b".
    click_line(&mut h, "Stage hunk", 0, 3.0);
    pump_for_label(&mut h, "Discard selected…");
    click(&mut h, "Discard selected…");
    pump_for_label(&mut h, "Discard");
    assert_no_op_started(&mut h, "nothing is discarded before confirming");
    assert_eq!(on_disk(&f), "one\nnew a\nnew b\ntwo\nthree\n");
    click(&mut h, "Discard");
    wait_for(&mut h, "the line to be discarded", || on_disk(&f) == "one\nnew a\ntwo\nthree\n");
    assert_eq!(in_index(&f), "one\ntwo\nthree\n", "the index isn't involved");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn discard_selected_across_two_hunks_keeps_the_rest() {
    let f = Fx::new();
    let (base, _) = two_hunks(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Stage hunk");
    // In the first hunk: one line of context, the removed line, then the added one.
    // In the second: three of context, then the same.
    click_line(&mut h, "Stage hunk", 0, 3.0);
    click_line(&mut h, "Stage hunk", 1, 5.0);
    pump_for_label(&mut h, "Stage 2 selected lines");
    click(&mut h, "Discard selected…");
    confirm_discard(&mut h);
    // A discard takes the selected added line out. The removed line wasn't selected, so stays gone.
    let want = base.replace("line 2\n", "").replace("line 25\n", "");
    wait_for(&mut h, "the lines to be discarded", || on_disk(&f) == want);
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn stage_selected_line_puts_only_that_line_in_the_index() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\nthree\n");
    f.commit("base");
    let edited = "one\nnew a\nnew b\ntwo\nthree\n";
    f.write("f.txt", edited);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Stage hunk");
    click_line(&mut h, "Stage hunk", 0, 3.0);
    click(&mut h, "Stage 1 selected line");
    wait_for(&mut h, "the line to be staged", || in_index(&f) == "one\nnew b\ntwo\nthree\n");
    assert_eq!(on_disk(&f), edited, "staging leaves the file alone");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn clicking_a_selected_line_again_clears_it() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\n");
    f.commit("base");
    f.write("f.txt", "one\nnew\ntwo\n");
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Stage hunk");
    click_line(&mut h, "Stage hunk", 0, 2.0);
    pump_for_label(&mut h, "Stage 1 selected line");
    click_line(&mut h, "Stage hunk", 0, 2.0);
    assert!(h.query_by_label("Stage 1 selected line").is_none());
    assert!(h.query_by_label("Discard selected…").is_none());
}

/// The diff's text is decoded lossily, so a patch from it would put U+FFFD in the file.
#[test]
fn a_selected_line_that_is_not_utf8_offers_no_way_to_rewrite_it() {
    let f = Fx::new();
    f.write("f.txt", "keep\n");
    f.commit("base");
    std::fs::write(f.root.join("f.txt"), b"keep\ncaf\xe9 added\n").unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Unstaged, "Not UTF-8: stage the whole file");
    click_line(&mut h, "Not UTF-8: stage the whole file", 0, 2.0);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label("Stage 1 selected line").is_none());
    assert!(h.query_by_label("Discard selected…").is_none());
    assert_eq!(h.query_all_by_label("Not UTF-8: stage the whole file").count(), 2, "the hunk's notice, and the toolbar's in place of the buttons");
    click(&mut h, "Stage file");
    wait_for(&mut h, "the file to be staged", || f.git.run(Some(&f.root), &["show", ":f.txt"]).unwrap().stdout == b"keep\ncaf\xe9 added\n");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

/// The whole route: the result tab, its Undo button and the confirm, with work in the tree.
#[test]
fn undo_of_an_auto_rebase_from_its_tab_keeps_uncommitted_work() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.write("dirty.txt", "committed\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "work"]);
    f.write("c.txt", "mine\n");
    f.commit("my change");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "upstream\n");
    f.commit("upstream change");
    f.git(&["switch", "-q", "work"]);
    f.write("dirty.txt", "UNCOMMITTED WORK\n");
    let start = f.git(&["rev-parse", "HEAD"]).trim().to_string();

    let mut h = harness(&f);
    settle(&mut h);
    let undo = gitem::views::textview::UndoSlot::default();
    let slot = undo.clone();
    let repo = h.state().repos[0].repo.clone();
    let job: Box<dyn FnOnce() -> Result<String, String> + Send> = Box::new(move || {
        let done = gitem_core::autorebase::run(&repo, "main", "", false, &|_, _| Ok("merged".to_string())).map_err(|e| e.to_string())?;
        *slot.lock().unwrap() = Some(gitem_core::autorebase::undo_op(&done));
        Ok(done.describe("main"))
    });
    let tab = Tab::Text(Box::new(gitem::views::textview::TextView::with_undo("Auto-rebase", job, undo)));
    h.state_mut().repos[0].open_tab(tab);
    pump_for_label(&mut h, "Undo…");
    assert_ne!(f.git(&["rev-parse", "HEAD"]).trim(), start, "it rebased");
    wait_until_quiet(&mut h);
    click(&mut h, "Undo…");
    pump_for_label(&mut h, "Cancel");
    assert_no_op_started(&mut h, "nothing happens before confirming");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Undo").click();
    wait_for(&mut h, "the branch to go back", || f.git(&["rev-parse", "HEAD"]).trim() == start);
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert_eq!(std::fs::read_to_string(f.root.join("dirty.txt")).unwrap(), "UNCOMMITTED WORK\n");
}

#[test]
fn unstage_selected_line_takes_only_that_line_out_of_the_index() {
    let f = Fx::new();
    f.write("f.txt", "one\ntwo\nthree\n");
    f.commit("base");
    let edited = "one\nnew a\nnew b\ntwo\nthree\n";
    f.write("f.txt", edited);
    f.git(&["add", "f.txt"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Staged, "Unstage hunk");
    // Rows: one, new a. Pick "new a".
    click_line(&mut h, "Unstage hunk", 0, 2.0);
    pump_for_label(&mut h, "Unstage 1 selected line");
    assert!(h.query_by_label("Discard selected…").is_none(), "a staged diff has nothing in the working tree to discard");
    click(&mut h, "Unstage 1 selected line");
    wait_for(&mut h, "the line to be unstaged", || in_index(&f) == "one\nnew b\ntwo\nthree\n");
    assert_eq!(on_disk(&f), edited, "unstaging leaves the file alone");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn unstage_hunk_takes_only_that_hunk_out_of_the_index() {
    let f = Fx::new();
    let (base, edited) = two_hunks(&f);
    f.git(&["add", "f.txt"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_diff(&mut h, DiffSource::Staged, "Unstage hunk");
    click_nth(&mut h, "Unstage hunk", 1);
    let want = base.replace("line 2\n", "line 2 x\n");
    wait_for(&mut h, "the hunk to be unstaged", || in_index(&f) == want);
    assert_eq!(on_disk(&f), edited);
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn a_huge_diff_costs_a_frame_no_more_than_a_small_one() {
    let f = Fx::new();
    let base: String = (0..60_000).map(|i| format!("line {i}\n")).collect();
    f.write("big.txt", &base);
    f.commit("base");
    f.write("big.txt", &base.replace('l', "L"));
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("big.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    assert_quiet_and_quick(&mut h, &f, "diff of 120,000 lines");
}

#[test]
fn a_big_blame_costs_a_frame_no_more_than_a_small_one() {
    let f = Fx::new();
    let text: String = (0..30_000).map(|i| format!("line {i}\n")).collect();
    f.write("big.txt", &text);
    f.commit("base");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("big.txt", None, None));
    pump_for_label(&mut h, "Blame");
    assert_quiet_and_quick(&mut h, &f, "file of 30,000 lines");
}

/// Only the sidebar rows on screen are drawn. It used to take 40 ms per frame.
#[test]
fn many_refs_cost_a_frame_no_more_than_a_few() {
    let f = Fx::new();
    many_commits(&f, 200);
    let shas = f.git(&["rev-list", "HEAD"]);
    let shas: Vec<&str> = shas.lines().collect();
    let refs: String = (0..5_000).map(|i| { let sha = shas[i % shas.len()]; format!("create refs/heads/topic/branch-{i} {sha}\ncreate refs/tags/t{i} {sha}\n") }).collect();
    let op = ops::GitOp::new("refs").step(&["update-ref", "--stdin"]).stdin(refs.into_bytes());
    op.run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    assert_quiet_and_quick(&mut h, &f, "graph and sidebar with 10,000 refs");
}

/// Only the rows on screen are drawn. It used to take 18 ms per frame.
#[test]
fn a_huge_status_costs_a_frame_no_more_than_a_small_one() {
    let f = Fx::new();
    sample_history(&f);
    // Untracked files, one level down so `git status` lists each.
    std::fs::create_dir(f.root.join("many")).unwrap();
    for i in 0..5_000 {
        std::fs::write(f.root.join(format!("many/file{i}.txt")), "x").unwrap();
    }
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_any(&mut h, "many/file0.txt");
    assert_quiet_and_quick(&mut h, &f, "Changes with 5,000 files");
}

fn fake_git(dir: &Path, version_line: &str) -> PathBuf {
    #[cfg(windows)]
    let (name, text) = ("git.cmd", format!("@echo {version_line}\r\n"));
    #[cfg(not(windows))]
    let (name, text) = ("git", format!("#!/bin/sh\necho '{version_line}'\n"));
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn app_with_git(path: &Path) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let settings = gitem_core::settings::Settings { git_path: path.to_string_lossy().into_owned(), ..Default::default() };
    let app = GitemApp::with_settings(&ctx, settings);
    Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app)
}

#[test]
fn an_old_git_is_warned_about_once_at_startup() {
    let d = tempfile::tempdir().unwrap();
    let mut h = app_with_git(&fake_git(d.path(), "git version 2.30.1.windows.1"));
    pump_for_label(&mut h, "Your git is older than gitem needs");
    pump_for_any(&mut h, "This is git 2.30. gitem needs 2.38 or newer. Merge checks and updating dependent branches during a rebase will fail, and other commands may too.");
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Dismiss");
    assert!(h.query_by_label("Your git is older than gitem needs").is_none());
}

#[test]
fn a_current_git_gets_no_warning() {
    let d = tempfile::tempdir().unwrap();
    let mut h = app_with_git(&fake_git(d.path(), "git version 2.38.0"));
    for _ in 0..5 {
        h.step();
    }
    assert!(h.query_by_label("Your git is older than gitem needs").is_none());
}

#[test]
fn the_git_this_machine_has_gets_no_warning() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.query_by_label("Your git is older than gitem needs").is_none());
}

#[test]
fn a_rebase_row_is_reordered_by_its_handle_and_selected_by_a_click() {
    let f = Fx::new();
    f.write("base", "b\n");
    f.commit("base");
    let base = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    for (n, m) in [("x", "one"), ("y", "two"), ("z", "three")] {
        f.write(n, "1\n");
        f.commit(m);
    }
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Rebase(Box::new(gitem::views::rebase::RebaseView::new(Some(base)))));
    pump_for_label(&mut h, "Rebase 3 commits");
    pump_for_label(&mut h, "three");
    // Oldest first. Drag the handle of "three" above "one".
    let order = |h: &Harness<GitemApp>| h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Rebase(r) = t { Some(r.items.iter().map(|i| i.text.clone()).collect::<Vec<_>>()) } else { None }).unwrap();
    assert_eq!(order(&h), ["one", "two", "three"]);
    let handle = h.query_all_by_value("⠿").last().expect("each row has a handle").rect().center();
    let target = h.get_by_label("one").rect().center() - vec2(0.0, 12.0);
    pointer_drag(&mut h, handle, egui::pos2(handle.x, target.y));
    assert_eq!(order(&h), ["three", "one", "two"]);
    // The keyboard acts on the row last clicked.
    click(&mut h, "two");
    h.key_press(egui::Key::D);
    h.step();
    let dropped = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Rebase(r) = t { r.items.iter().find(|i| i.text == "two").map(|i| i.action) } else { None });
    assert_eq!(dropped, Some(gitem_core::rebase::RebaseAction::Drop));
}

fn many_untracked(f: &Fx, n: usize) {
    std::fs::create_dir(f.root.join("many")).unwrap();
    for i in 0..n {
        std::fs::write(f.root.join(format!("many/file{i}.txt")), "x").unwrap();
    }
}

/// Virtualising the list keeps everything that was on it in reach.
#[test]
fn a_long_changes_list_keeps_its_filter_and_section_buttons() {
    let f = Fx::new();
    sample_history(&f);
    many_untracked(&f, 300);
    f.write("staged.txt", "s\n");
    f.git(&["add", "staged.txt"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_any(&mut h, "many/file0.txt");
    pump_for_any(&mut h, "STAGED (1)");
    pump_for_any(&mut h, "UNSTAGED (300)");
    assert!(h.query_by_label("many/file299.txt").is_none(), "a row off screen isn't drawn");
    // The filter box is the first text field in the Changes tab, and stays on screen.
    let filter = h.query_all_by_role(egui::accesskit::Role::TextInput).position(|n| n.rect().left() > 200.0).expect("the filter box");
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(filter).unwrap().focus();
    h.step();
    h.query_all_by_role(egui::accesskit::Role::TextInput).nth(filter).unwrap().type_text("file299");
    h.step();
    h.step();
    pump_for_any(&mut h, "many/file299.txt");
    pump_for_any(&mut h, "UNSTAGED (1)");
    assert!(h.query_by_label("many/file0.txt").is_none());
    click(&mut h, "Stage all");
    wait_for(&mut h, "everything to be staged", || !f.git(&["status", "--porcelain"]).contains("??"));
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn folders_in_a_long_changes_tree_still_open_and_close() {
    let f = Fx::new();
    sample_history(&f);
    many_untracked(&f, 300);
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings { changes_tree: true, ..Default::default() });
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    let mut h = builder.build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    );
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_any(&mut h, "many/file0.txt");
    click(&mut h, "many");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.query_by_label("many/file0.txt").is_none(), "closed");
    click(&mut h, "many");
    pump_for_any(&mut h, "many/file0.txt");
    no_errors(&h);
}

/// AS-7: this used to say "Reading status…" forever, with no error anywhere.
#[test]
fn a_corrupt_index_says_so_and_can_be_retried() {
    let f = Fx::new();
    sample_history(&f);
    let index = f.root.join(".git/index");
    let good = std::fs::read(&index).unwrap();
    std::fs::write(&index, b"not an index").unwrap();
    let mut h = harness(&f);
    pump(&mut h, |a| a.repos.first().is_some_and(|r| r.data.status.error.is_some()), "status to fail");
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_any(&mut h, "Git can't read this repository. What's showing may be out of date.");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.query_by_label_contains("Reading status").is_none());
    assert!(h.query_by_label_contains("Error: Git couldn't read the status").is_some(), "Changes says what's wrong");
    std::fs::write(&index, good).unwrap();
    // On the banner, and in the Changes tab.
    h.query_all_by_label("Try again").next().unwrap().click();
    h.step();
    pump(&mut h, |a| a.repos[0].data.status.error.is_none() && a.repos[0].data.status.get().is_some(), "status to work again");
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label_contains("Git can't read this repository").is_none());
}

#[test]
fn a_repository_folder_that_goes_away_says_so() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    // Windows can refuse a folder rename while something watches inside it.
    let moved = f.root.with_extension("moved");
    if std::fs::rename(&f.root, &moved).is_err() {
        eprintln!("skipped: this system won't rename a watched folder");
        return;
    }
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Refresh);
    pump_for_any(&mut h, "This repository's folder is gone. It may have been moved, renamed or deleted.");
    assert!(h.state().repos[0].data.status.get().is_some(), "what was there stays");
    std::fs::rename(&moved, &f.root).unwrap();
}

/// UX-10: an empty list before git has answered isn't a repository with nothing in it.
#[test]
fn the_sidebar_shows_no_counts_until_it_knows_them() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    h.step();
    assert!(h.state().repos[0].data.refs.get().is_none(), "this frame is before git has answered");
    assert!(h.query_by_label_contains("Reading branches").is_some());
    assert!(h.query_by_label("Branches section").is_none());
    settle(&mut h);
    assert!(h.query_by_label("Branches section").is_some());
}

/// AS-25: a failure mustn't cost actions that have nothing to do with it.
#[test]
fn a_failure_keeps_what_was_asked_for_separately() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::checkout("no-such-branch")));
    h.step();
    h.state_mut().apply_action(&ctx, Action::Run(ops::tag_create("still-runs", None, None, false)));
    pump_for_error(&mut h);
    wait_until_quiet(&mut h);
    assert!(f.git(&["tag"]).contains("still-runs"));
}

/// Nothing queued was asked for with a rebase or merge going on.
#[test]
fn a_failure_that_leaves_an_operation_going_drops_the_queue() {
    let r = with_remote();
    r.commit_elsewhere("conflict.txt", "from elsewhere");
    r.f.write("conflict.txt", "from here\n");
    r.f.commit("from here");
    let mut h = harness(&r.f);
    settle(&mut h);
    click(&mut h, "Fetch");
    wait_until_quiet(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::pull(ops::PullMode::Merge, false)));
    h.step();
    h.state_mut().apply_action(&ctx, Action::Run(ops::tag_create("must-not-exist", None, None, false)));
    pump_for_error(&mut h);
    wait_until_quiet(&mut h);
    assert!(r.f.git(&["tag"]).is_empty());
    assert!(h.state().errors_showing()[0].contains("1 queued command after it was not run"), "{:?}", h.state().errors_showing());
}

/// AS-19: applies to every button that runs something, not only those checked by hand.
#[test]
fn a_double_click_on_a_button_runs_its_command_once() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let op = || ops::tag_create("once", None, None, false);
    h.state_mut().apply_action(&ctx, Action::Run(op()));
    h.step();
    h.state_mut().apply_action(&ctx, Action::Run(op()));
    wait_until_quiet(&mut h);
    no_errors(&h);
    // Asking again later is asking again.
    std::thread::sleep(Duration::from_millis(700));
    h.state_mut().apply_action(&ctx, Action::Run(op()));
    pump_for_error(&mut h);
}

/// Two parts of one action, such as two hunks staged by one click, can be identical commands.
#[test]
fn two_of_the_same_command_in_one_frame_both_run() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    let before = f.git(&["rev-list", "--count", "HEAD"]);
    let empty = || ops::commit(&ops::CommitOpts { message: "again".into(), allow_empty: true, ..Default::default() });
    h.state_mut().apply_action(&ctx, Action::Run(empty()));
    h.state_mut().apply_action(&ctx, Action::Run(empty()));
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap(), before.trim().parse::<u32>().unwrap() + 2);
}

/// CG-27: a hook that never returns used to hold everything behind it, with no way out.
#[test]
fn a_write_that_hangs_can_be_stopped() {
    let f = Fx::new();
    sample_history(&f);
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nsleep 300\n").unwrap();
    f.write("n.txt", "n\n");
    f.git(&["add", "n.txt"]);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::commit(&ops::CommitOpts { message: "hangs".into(), ..Default::default() })));
    let t = Instant::now();
    pump_for_label(&mut h, "Cancel…");
    assert!(t.elapsed() >= Duration::from_secs(2), "not offered at once");
    h.get_by_label("Cancel…").click();
    h.step();
    h.step();
    assert!(h.query_by_label_contains("half updated").is_some(), "it warns first");
    click(&mut h, "Stop it");
    pump(&mut h, |a| a.ops_idle(), "the op to stop");
    no_errors_except_cancel(&h);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]).trim(), "second", "nothing was committed");
}

fn no_errors_except_cancel(h: &Harness<GitemApp>) {
    for e in h.state().errors_showing() {
        assert!(e.contains("cancelled"), "{e}");
    }
}

/// UX-19: says what went wrong and offers the way out.
#[test]
fn a_failed_pull_with_no_upstream_names_itself_and_offers_the_fix() {
    let r = with_remote();
    let f = &r.f;
    f.git(&["branch", "--unset-upstream"]);
    let mut h = harness(f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::pull(ops::PullMode::Merge, false)));
    pump_for_error(&mut h);
    pump_for_label(&mut h, "Set upstream…");
    assert!(h.state().errors_showing()[0].starts_with("Pull"), "the title says what failed");
    assert!(h.query_by_label("Something went wrong").is_none());
    assert!(h.query_by_label_contains("isn't linked to a branch").is_some(), "in plain words");
    pump_for_any(&mut h, "What git said");
    h.get_by_label("Set upstream…").click();
    h.step();
    h.step();
    assert!(h.state().errors_showing().is_empty());
    pump_for_any(&mut h, "Set upstream");
}

#[test]
fn escape_dismisses_an_error_and_the_next_one_shows_with_a_count() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Error("first problem".into()));
    h.state_mut().apply_action(&ctx, Action::Error("second problem".into()));
    pump_for_any(&mut h, "1 of 2");
    assert!(h.query_by_label("first problem").is_some());
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();
    assert!(h.query_by_label("second problem").is_some());
    assert!(h.query_by_label("1 of 2").is_none());
}

#[test]
fn errors_people_often_meet_are_explained() {
    use gitem::app::{ErrorFix, explain_error, error_parts};
    let cases = [
        ("There is no tracking information for the current branch.", ErrorFix::SetUpstream),
        ("fatal: The current branch x has no upstream branch.", ErrorFix::SetUpstream),
        ("error: Your local changes to the following files would be overwritten by merge:", ErrorFix::Stash),
        ("fatal: No configured push destination.", ErrorFix::AddRemote),
        ("fatal: 'origin' does not appear to be a git repository", ErrorFix::AddRemote),
        ("hint: You have divergent branches and need to specify how to reconcile divergent branches.", ErrorFix::Reconcile),
        (" ! [rejected] main -> main (non-fast-forward)", ErrorFix::PullOptions),
        ("fatal: Authentication failed for 'https://example.invalid/'", ErrorFix::Credentials),
        ("git@example.invalid: Permission denied (publickey).", ErrorFix::Credentials),
    ];
    for (text, fix) in cases {
        assert_eq!(explain_error(text).map(|x| x.1), Some(fix), "{text}");
    }
    assert!(explain_error("fatal: something nobody has seen").is_none());
    assert_eq!(error_parts("Pull failed:\nfatal: no"), ("Pull failed", "fatal: no"));
    assert_eq!(error_parts("fatal: no"), ("Something went wrong", "fatal: no"));
}

fn auto_fetch_on(h: &mut Harness<GitemApp>) {
    h.state_mut().settings.auto_fetch_minutes = 5;
    h.state_mut().repos[0].make_fetch_due();
}

/// FG-4: it used to go through the op queue, in everybody's way.
#[test]
fn a_scheduled_fetch_stays_out_of_the_way() {
    let r = with_remote();
    r.commit_elsewhere("new.txt", "from elsewhere");
    let mut h = harness(&r.f);
    settle(&mut h);
    let tip = r.remote_tip();
    auto_fetch_on(&mut h);
    let mut seen_busy = false;
    let t = Instant::now();
    while r.f.git(&["rev-parse", "origin/main"]).trim() != tip {
        h.step();
        seen_busy |= h.state().repos[0].data.ops_busy;
        assert!(t.elapsed() < PATIENCE);
    }
    wait_until_quiet(&mut h);
    assert!(!seen_busy, "Commit stayed available");
    assert!(h.query_by_label_contains("done").is_none(), "no toast");
    no_errors(&h);
}

#[test]
fn a_scheduled_fetch_that_fails_says_little_and_waits_longer() {
    let r = with_remote();
    let mut h = harness(&r.f);
    settle(&mut h);
    r.f.git(&["remote", "set-url", "origin", &r.f.root.join("no-such-remote").to_string_lossy()]);
    auto_fetch_on(&mut h);
    h.step();
    let ctx = h.ctx.clone();
    // Asked for while it's running, and has nothing to do with it.
    h.state_mut().apply_action(&ctx, Action::Run(ops::tag_create("unrelated", None, None, false)));
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert!(r.f.git(&["tag"]).contains("unrelated"), "its failure touched nothing else");
    pump_for_any(&mut h, &format!("Background fetch failed, trying again in about {} min", 10));
    assert_eq!(h.state().repos[0].fetch_wait_minutes(), Some(10), "twice the usual wait");
}

#[test]
fn a_scheduled_fetch_waits_while_an_operation_is_going_on() {
    let r = with_remote();
    r.commit_elsewhere("conflict.txt", "from elsewhere");
    r.f.write("conflict.txt", "from here\n");
    r.f.commit("from here");
    r.f.git(&["fetch", "-q"]);
    let _ = r.f.git.run_raw(Some(&r.f.root), &["merge", "origin/main"]);
    let mut h = harness(&r.f);
    settle(&mut h);
    let before = commands_so_far(&r.f);
    auto_fetch_on(&mut h);
    for _ in 0..30 {
        h.step();
    }
    wait_until_quiet(&mut h);
    let log = r.f.git.log.lock().unwrap();
    assert!(!log.iter().skip(before).any(|e| e.command.starts_with("git fetch")), "it waits");
}

/// PF-11: a build's output, ignored by git, used to mean a `git status` twice a second.
#[test]
fn writes_to_an_ignored_file_run_no_git() {
    let f = Fx::new();
    f.write(".gitignore", "target/\n");
    sample_history(&f);
    std::fs::create_dir_all(f.root.join("target")).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    let before = commands_so_far(&f);
    for i in 0..8 {
        f.write("target/out.txt", &i.to_string());
        for _ in 0..10 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Only the one question, once, of where the user's own excludes file is.
    let ran: Vec<String> = f.git.log.lock().unwrap().iter().skip(before).map(|e| e.command.clone()).filter(|c| !c.contains("core.excludesFile")).collect();
    assert!(ran.is_empty(), "{ran:?}");
    // A real change still shows.
    f.write("visible.txt", "seen\n");
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| !s.is_clean()), "the visible file");
}

/// PF-12: staging isn't a change to any ref.
#[test]
fn staging_refreshes_the_status_and_not_the_refs() {
    let f = Fx::new();
    sample_history(&f);
    f.write("n.txt", "n\n");
    let mut h = harness(&f);
    settle(&mut h);
    let before = commands_so_far(&f);
    f.git(&["add", "n.txt"]);
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.entries.iter().any(|e| e.is_staged())), "the status to show it");
    wait_until_quiet(&mut h);
    let log = f.git.log.lock().unwrap();
    let ran: Vec<&str> = log.iter().skip(before).map(|e| e.command.as_str()).collect();
    assert!(!ran.iter().any(|c| c.contains("for-each-ref")), "{ran:?}");
}

/// PF-14: only the branch you're on opens by itself. Each open one runs git.
#[test]
fn many_branches_behind_their_upstream_run_no_git_at_open() {
    let f = Fx::new();
    many_commits(&f, 5);
    for i in 0..10 {
        let name = format!("b{i}");
        f.git(&["branch", &name, "HEAD~1"]);
        f.git(&["update-ref", &format!("refs/remotes/origin/{name}"), "HEAD"]);
        f.git(&["config", &format!("branch.{name}.remote"), "origin"]);
        f.git(&["config", &format!("branch.{name}.merge"), &format!("refs/heads/{name}")]);
    }
    f.git(&["remote", "add", "origin", "https://example.invalid/r.git"]);
    let before = commands_so_far(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let logs = f.git.log.lock().unwrap().iter().skip(before).filter(|e| e.command.starts_with("git log")).count();
    assert!(logs <= 2, "{logs} log commands");
    drop(h);
}

/// PF-4: what it takes every ref to answer waits until someone asks.
#[test]
fn the_refs_holding_a_commit_load_when_asked_for() {
    let f = Fx::new();
    sample_history(&f);
    let sha = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    let before = commands_so_far(&f);
    h.state_mut().repos[0].inspect.set(&sha);
    pump_for_any(&mut h, "Branches and tags with this commit");
    wait_until_quiet(&mut h);
    assert!(!f.git.log.lock().unwrap().iter().skip(before).any(|e| e.command.contains("--contains")));
    click(&mut h, "Branches and tags with this commit");
    pump_for_any(&mut h, "On 1 branch, 0 tags");
}

/// PF-15: blame takes minutes on a big file, and nobody asked for it.
#[test]
fn a_huge_file_opens_without_blame_until_asked() {
    let f = Fx::new();
    let body: String = (0..1_000_000).map(|i| format!("{i:012}\n")).collect();
    f.write("big.txt", &body);
    f.commit("base");
    let mut h = harness(&f);
    settle(&mut h);
    let before = commands_so_far(&f);
    h.state_mut().repos[0].open_tab(Tab::file("big.txt", None, None));
    pump_for_label(&mut h, "Blame it anyway");
    wait_until_quiet(&mut h);
    assert!(!f.git.log.lock().unwrap().iter().skip(before).any(|e| e.command.starts_with("git blame")));
    click(&mut h, "Blame it anyway");
    wait_until_quiet(&mut h);
    assert!(f.git.log.lock().unwrap().iter().skip(before).any(|e| e.command.starts_with("git blame")));
    assert!(h.query_by_label("Blame it anyway").is_none());
}

/// A local server answering like an AI provider, after the test says it may.
struct SlowAi {
    url: String,
    release: std::sync::mpsc::Sender<&'static str>,
}

fn slow_ai() -> SlowAi {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = std::sync::mpsc::channel::<&'static str>();
    std::thread::spawn(move || {
        let Ok((mut s, _)) = listener.accept() else { return };
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        // Headers, then the body they announce.
        loop {
            let Ok(n) = s.read(&mut buf) else { return };
            seen.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&seen);
            if let Some(at) = text.find("\r\n\r\n") {
                let len = text.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
                if seen.len() >= at + 4 + len {
                    break;
                }
            }
        }
        let Ok(reply) = wait.recv() else { return };
        let body = format!("{{\"choices\":[{{\"message\":{{\"content\":\"{reply}\"}}}}]}}");
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    });
    SlowAi { url, release }
}

/// AS-23: a reply that took a minute used to replace whatever had been typed meanwhile.
#[test]
fn a_late_ai_message_does_not_replace_what_was_typed() {
    let f = Fx::new();
    sample_history(&f);
    f.write("n.txt", "n\n");
    f.git(&["add", "n.txt"]);
    let ai = slow_ai();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().settings.ai = gitem_core::ai::AiConfig { provider: gitem_core::ai::AiProvider::OpenAiCompatible, base_url: ai.url.clone(), model: "m".into(), ..Default::default() };
    h.state_mut().repos[0].open_tab(Tab::changes());
    open_ai_menu(&mut h);
    click(&mut h, "Generate with AI");
    let typed = "typed while waiting";
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text(typed);
    h.step();
    h.step();
    ai.release.send("from the model").unwrap();
    pump_for_any(&mut h, "Use it");
    let box_text = |h: &Harness<GitemApp>| h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().unwrap_or_default().to_string();
    assert_eq!(box_text(&h), typed);
    for _ in 0..5 {
        h.step();
    }
    click(&mut h, "Use it");
    assert_eq!(box_text(&h), "from the model");
}

#[test]
fn an_ai_message_fills_a_box_nobody_has_touched() {
    let f = Fx::new();
    sample_history(&f);
    f.write("n.txt", "n\n");
    f.git(&["add", "n.txt"]);
    let ai = slow_ai();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().settings.ai = gitem_core::ai::AiConfig { provider: gitem_core::ai::AiProvider::OpenAiCompatible, base_url: ai.url.clone(), model: "m".into(), ..Default::default() };
    h.state_mut().repos[0].open_tab(Tab::changes());
    open_ai_menu(&mut h);
    click(&mut h, "Generate with AI");
    ai.release.send("from the model").unwrap();
    let t = Instant::now();
    loop {
        h.step();
        if h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().is_some_and(|v| v == "from the model") {
            break;
        }
        assert!(t.elapsed() < PATIENCE, "the message never came");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- daily views: names, renames, filters ----------------------------------------------

fn rename_staged(f: &Fx) {
    f.write("old.txt", "one\ntwo\nthree\nfour\nfive\n");
    f.commit("base");
    f.git(&["mv", "old.txt", "new.txt"]);
}

/// The whole rename goes back, not half of it left staged as a deletion.
#[test]
fn unstaging_a_renamed_row_takes_back_both_names() {
    let f = Fx::new();
    rename_staged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "new.txt");
    h.get_by_label("new.txt").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Unstage");
    wait_for(&mut h, "both names to be unstaged", || f.git(&["diff", "--cached", "--name-only"]).trim().is_empty());
    let st = f.git(&["status", "--short"]);
    assert!(st.contains(" D old.txt") && st.contains("?? new.txt"), "{st}");
    no_errors(&h);
}

#[test]
fn unstaging_from_a_rename_diff_also_takes_both() {
    let f = Fx::new();
    rename_staged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Staged, Some("new.txt".into())));
    pump_for_label(&mut h, "Unstage file");
    assert!(h.query_by_label_contains("old.txt → new.txt").is_some(), "shown as the rename it is");
    click(&mut h, "Unstage file");
    wait_for(&mut h, "both names to be unstaged", || f.git(&["diff", "--cached", "--name-only"]).trim().is_empty());
    no_errors(&h);
}

/// A binary file has no hunks, and the button used to go with them.
#[test]
fn stage_file_is_there_for_a_change_with_no_text() {
    let f = Fx::new();
    f.write("base.txt", "x\n");
    f.commit("base");
    std::fs::write(f.root.join("b.bin"), [0u8, 1, 2, 3, 0, 5]).unwrap();
    f.git(&["add", "b.bin"]);
    f.commit("add binary");
    std::fs::write(f.root.join("b.bin"), [9u8, 0, 2, 3, 0, 5]).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("b.bin".into())));
    pump_for_label(&mut h, "Stage file");
    click(&mut h, "Stage file");
    wait_for(&mut h, "it to be staged", || f.git(&["diff", "--cached", "--name-only"]).trim() == "b.bin");
}

#[test]
fn batch_unstage_leaves_out_what_isnt_staged() {
    let f = Fx::new();
    f.write("t.txt", "1\n");
    f.commit("base");
    f.write("t.txt", "2\n");
    f.git(&["add", "t.txt"]);
    f.write("u.txt", "new\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "t.txt");
    h.get_by_label("t.txt").click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.get_by_label("u.txt").click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    pump_for_label(&mut h, "2 selected");
    click(&mut h, "Unstage");
    wait_for(&mut h, "t.txt to be unstaged", || f.git(&["diff", "--cached", "--name-only"]).trim().is_empty());
    no_errors(&h);
}

#[test]
fn a_nested_repository_isnt_deleted_with_a_false_ok() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    std::fs::create_dir(f.root.join("nested")).unwrap();
    f.git.run(Some(&f.root.join("nested")), &["init", "-q"]).unwrap();
    std::fs::write(f.root.join("nested/x"), "x").unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "nested/");
    h.get_by_label("nested/").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Delete file…");
    pump_for_error(&mut h);
    assert!(h.state().errors_showing()[0].contains("repository of its own"));
    assert!(f.root.join("nested/x").exists());
}

// ---- prepared messages --------------------------------------------------------------------

fn changes_message(h: &Harness<GitemApp>) -> String {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Changes(c) = t { Some(c.message.clone()) } else { None }).unwrap()
}

#[test]
fn a_squash_merge_fills_in_its_message_and_says_what_it_is() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "s\n");
    f.commit("side work");
    f.git(&["switch", "-q", "main"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::merge("side", ops::MergeMode::Squash)));
    let t = Instant::now();
    while !changes_message(&h).contains("side work") {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the message never came");
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(h.query_by_label_contains("A squash merge is staged").is_some());
    no_errors(&h);
}

#[test]
fn an_aborted_merges_message_leaves_with_it() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    let t = Instant::now();
    while changes_message(&h).is_empty() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the message never came");
        std::thread::sleep(Duration::from_millis(15));
    }
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::op_abort("merge")));
    let t = Instant::now();
    while !changes_message(&h).is_empty() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the merge's message stayed after the abort");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Only an unchanged message goes. One the user touched is theirs.
#[test]
fn a_message_edited_during_a_merge_stays_after_the_abort() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    let t = Instant::now();
    while changes_message(&h).is_empty() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the message never came");
        std::thread::sleep(Duration::from_millis(15));
    }
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text(" and more");
    h.step();
    h.step();
    let typed = changes_message(&h);
    assert!(typed.contains("and more"), "{typed}");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::op_abort("merge")));
    settle(&mut h);
    assert_eq!(changes_message(&h), typed);
}

#[test]
fn a_message_with_no_blank_second_line_gets_a_note() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("subject\nnot blank");
    h.step();
    h.step();
    assert!(h.query_by_label_contains("Leave the second line blank").is_some());
}

// ---- the graph ----------------------------------------------------------------------------

fn painted_y(h: &Harness<GitemApp>, text: &str) -> f32 {
    painted_text(h).iter().find(|(_, _, t)| t == text).map(|x| x.0.y).unwrap_or_else(|| panic!("{text} isn't on screen"))
}

/// One row down shouldn't move the list when that row is already on screen, and the row
/// the keyboard reaches is always brought into view.
#[test]
fn the_keyboard_only_scrolls_the_graph_when_it_has_to() {
    let f = Fx::new();
    for i in 0..120 {
        f.git(&["commit", "-q", "--allow-empty", "-m", &format!("c{i:03}")]);
    }
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "c119");
    click(&mut h, "c119");
    h.step();
    let before = painted_y(&h, "c117");
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.step();
    assert_eq!(painted_y(&h, "c117"), before, "the row was on screen, so nothing had to move");

    // On past the bottom of the screen.
    for _ in 0..90 {
        h.key_press(egui::Key::ArrowDown);
        h.step();
    }
    for _ in 0..4 {
        h.step();
    }
    let selected = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Graph(g) = t { g.selected().last().cloned() } else { None }).unwrap();
    let subject = f.git(&["log", "-1", "--format=%s", &selected]);
    let y = painted_y(&h, subject.trim());
    assert!(y > 0.0 && y < 940.0, "{} is off screen at {y}", subject.trim());
    no_errors(&h);
}

#[test]
fn a_search_that_fails_says_so_and_drops_old_highlights() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "second");
    click(&mut h, ".*");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.step();
    h.step();
    h.event(egui::Event::Text("foo(".into()));
    h.step();
    h.key_press(egui::Key::Enter);
    let t = Instant::now();
    while h.query_by_label_contains("Search failed").is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "nothing said the search failed");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// `--all` always brought HEAD back, so this used to do nothing at all.
#[test]
fn hiding_the_current_branch_hides_it() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("shared");
    f.git(&["switch", "-q", "-c", "other"]);
    f.write("o.txt", "o\n");
    f.commit("on other");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "on main");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::GraphHide("main".into()));
    settle(&mut h);
    assert!(h.query_by_label("on main").is_none(), "main's own commit is still shown");
    assert!(h.query_by_label("on other").is_some() && h.query_by_label("shared").is_some());
    no_errors(&h);
}

#[test]
fn hiding_with_a_dirty_tree_keeps_the_working_row_from_dangling() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("shared");
    f.git(&["switch", "-q", "-c", "other"]);
    f.write("o.txt", "o\n");
    f.commit("on other");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.write("a.txt", "dirty\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "on main");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::GraphHide("main".into()));
    settle(&mut h);
    assert!(h.query_by_label("Work in progress").is_some());
    assert!(h.query_by_label("on main").is_none());
    no_errors(&h);
}

#[test]
fn menu_presets_refuse_a_merge_and_a_commit_from_elsewhere() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "s\n");
    f.commit("on side only");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["merge", "-q", "--no-ff", "-m", "the merge", "side"]);
    f.git(&["switch", "-q", "-c", "elsewhere", "HEAD~1"]);
    f.write("e.txt", "e\n");
    f.commit("on elsewhere");
    f.git(&["switch", "-q", "main"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "the merge");
    h.get_by_label("the merge").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Rewrite history ⏵");
    click(&mut h, "Drop…");
    pump_for_error(&mut h);
    assert!(h.state().errors_showing()[0].contains("merge commit"), "{:?}", h.state().errors_showing());
    assert!(!h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Rebase(_))));
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();
    h.get_by_label("on elsewhere").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Rewrite history ⏵");
    click(&mut h, "Reword…");
    pump_for_error(&mut h);
    assert!(h.state().errors_showing()[0].contains("isn't on the current branch"), "{:?}", h.state().errors_showing());
    assert!(!h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Rebase(_))));
}

/// Saying no leaves everything as it was, and that includes the tabs.
#[test]
fn recompose_opens_the_composer_only_on_the_go_ahead() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    pump_for_label(&mut h, "first");
    h.get_by_label("first").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Rewrite history ⏵");
    click(&mut h, "Recompose from here…");
    pump_for_label(&mut h, "Cancel");
    for _ in 0..3 {
        h.step();
    }
    let composer = |a: &GitemApp| a.repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Composer(_)));
    assert!(!composer(h.state()), "before answering");
    click(&mut h, "Cancel");
    h.step();
    assert!(!composer(h.state()), "after saying no");
    assert_eq!(f.subjects()[0], "second");
}

// ---- file and line history -----------------------------------------------------------------

fn renamed_file_history(f: &Fx) {
    let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    f.write("old.txt", &body);
    f.commit("create");
    f.write("old.txt", &body.replace("line 3\n", "line three\n"));
    f.commit("edit before rename");
    f.git(&["mv", "old.txt", "new.txt"]);
    f.commit("rename");
    f.write("new.txt", &body.replace("line 3\n", "line three\n").replace("line 20\n", "line twenty\n"));
    f.commit("edit after rename");
}

/// A row from before a rename used to show nothing, and an error for the file's contents.
#[test]
fn file_history_opens_a_commit_from_before_a_rename_by_its_name_then() {
    let f = Fx::new();
    renamed_file_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file_history("new.txt"));
    pump_for_label(&mut h, "edit before rename");
    wait_until_quiet(&mut h);
    h.get_by_label("edit before rename").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Open this revision with blame");
    pump_for_label(&mut h, "old.txt");
    wait_until_quiet(&mut h);
    no_errors(&h);
    let file = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::File(v) = t { Some(v.path.clone()) } else { None });
    assert_eq!(file.as_deref(), Some("old.txt"));

    h.state_mut().repos[0].open_tab(Tab::file_history("new.txt"));
    h.step();
    pump_for_label(&mut h, "edit before rename");
    h.get_by_label("edit before rename").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Open changes to this file");
    let t = Instant::now();
    while !painted_text(&h).iter().any(|(_, _, x)| x.contains("line three")) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the commit's diff never showed");
        std::thread::sleep(Duration::from_millis(15));
    }
    wait_until_quiet(&mut h);
    no_errors(&h);
}

/// Starting at a revision, each Newer goes one newer. It used to swing between two.
#[test]
fn newer_walks_forward_from_a_file_opened_at_a_revision() {
    let f = Fx::new();
    for i in 0..5 {
        f.write("n.txt", &format!("{i}\n"));
        f.commit(&format!("rev {i}"));
    }
    let shas: Vec<String> = f.git(&["log", "--reverse", "--format=%H"]).lines().map(String::from).collect();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("n.txt", Some(shas[1].clone()), None));
    pump_for_label(&mut h, "Newer ▶");
    let rev_of = |a: &GitemApp| a.repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::File(v) = t { Some(v.rev.clone()) } else { None }).flatten();
    for want in &shas[2..] {
        click(&mut h, "Newer ▶");
        pump(&mut h, |a| rev_of(a).as_deref() == Some(want.as_str()), "the next revision");
    }
    no_errors(&h);
}

/// The numbers belong to the revision on screen, and `-L` counts in the revision it starts at.
#[test]
fn line_history_follows_the_revision_its_numbers_came_from() {
    let f = Fx::new();
    f.write("n.txt", "keep\nold line\n");
    f.commit("old version");
    let old = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut long = String::new();
    for i in 0..10 {
        long.push_str(&format!("pad {i}\n"));
    }
    f.write("n.txt", &format!("{long}keep\nold line\n"));
    f.commit("padded out");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::line_history_at("n.txt", 2, 2, Some(old), None));
    pump_for_label(&mut h, "old version");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("padded out").is_none(), "a later commit can't be in the past of this revision");
    no_errors(&h);
}

#[test]
fn line_history_of_an_edited_file_says_its_numbers_may_be_off() {
    let f = Fx::new();
    f.write("n.txt", "a\nb\nc\n");
    f.commit("base");
    f.write("n.txt", "new\na\nb\nc\n");
    let mut h = harness(&f);
    settle(&mut h);
    let note = Some("This file has uncommitted changes".to_string());
    h.state_mut().repos[0].open_tab(Tab::line_history_at("n.txt", 1, 1, None, note));
    pump_for_any(&mut h, "This file has uncommitted changes");
}

// ---- diff view ------------------------------------------------------------------------------

/// Any file saved anywhere reloads a diff, which used to clear the lines picked for staging.
#[test]
fn a_reload_keeps_the_line_selection_unless_those_lines_changed() {
    let f = Fx::new();
    f.write("a.txt", "one\ntwo\nthree\n");
    f.write("other.txt", "x\n");
    f.commit("base");
    f.write("a.txt", "one\nTWO\nthree\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("a.txt".into())));
    pump_for_label(&mut h, "Stage file");
    click(&mut h, "Unified");
    wait_until_quiet(&mut h);
    click_line(&mut h, "Stage hunk", 0, 2.0);
    h.step();
    h.step();
    pump_for_label(&mut h, "Stage 1 selected line");

    f.write("other.txt", "changed elsewhere\n");
    settle(&mut h);
    assert!(h.query_by_label("Stage 1 selected line").is_some(), "an unrelated file changed, and the selection was lost");

    f.write("a.txt", "one\nDIFFERENT\nthree\n");
    settle(&mut h);
    assert!(h.query_by_label("Stage 1 selected line").is_none(), "those lines changed, so the selection can't stand");
}

#[test]
fn hiding_whitespace_takes_away_the_buttons_that_couldnt_apply() {
    let f = Fx::new();
    f.write("a.txt", "one\ntwo\n");
    f.commit("base");
    f.write("a.txt", "one\n  two\nthree\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("a.txt".into())));
    pump_for_label(&mut h, "Stage hunk");
    h.get_by_label("Whitespace \u{E136}").click();
    h.step();
    h.step();
    click(&mut h, "Ignore all");
    pump_for_any(&mut h, "Showing whitespace is needed to stage parts of a file");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("Stage hunk").is_none());
    assert!(h.query_by_label("Stage file").is_some(), "the whole file doesn't need a patch");
}

#[test]
fn a_conflicted_file_says_so_in_the_diff_and_leads_to_the_merge_tool() {
    let f = Fx::new();
    make_conflict(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let path = f.git(&["diff", "--name-only", "--diff-filter=U"]).trim().to_string();
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some(path)));
    pump_for_any(&mut h, "This file is in conflict");
    assert!(h.query_by_label("Stage file").is_none());
    click(&mut h, "Resolve in merge tool");
    h.step();
    assert!(merge_tab_open(&h));
}

// ---- merge view -----------------------------------------------------------------------------

fn merge_view_of<'a>(h: &'a Harness<'_, GitemApp>) -> Option<&'a gitem::views::mergeview::MergeView> {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Merge(m) = t { Some(&**m) } else { None })
}

/// All the sides stay on screen, and a long line scrolls inside its pane.
#[test]
fn long_conflict_lines_can_be_scrolled_to_their_end() {
    let f = Fx::new();
    let long = |c: char| format!("{}END\n", c.to_string().repeat(800));
    f.write("c.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "other"]);
    f.write("c.txt", &long('t'));
    f.commit("theirs");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", &long('o'));
    f.commit("ours");
    assert!(!f.git.run_raw(Some(&f.root), &["merge", "other"]).unwrap().ok());
    let mut h = harness(&f);
    open_merge(&mut h, "Take ours");
    let row = h.get_by_label("Take ours").rect().center() + vec2(0.0, 40.0);
    assert_eq!(merge_view_of(&h).unwrap().text_scroll(), 0.0);
    let start = painted_text(&h).into_iter().find(|(_, _, t)| t.starts_with("ooo")).expect("ours is on screen");
    h.event(egui::Event::PointerMoved(row));
    h.step();
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(-400.0, 0.0), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE });
    for _ in 0..30 {
        h.step();
    }
    assert!(merge_view_of(&h).unwrap().text_scroll() > 0.0, "the wheel didn't reach the panes");
    let moved = painted_text(&h).into_iter().find(|(_, _, t)| t.starts_with("ooo")).expect("ours is still on screen");
    assert!(moved.0.x < start.0.x, "the text didn't move");
    assert_eq!(moved.1, start.1, "it stays inside its own pane");
    assert!(h.query_by_label("Take ours").is_some(), "the conflict's buttons stay where they are");
}

#[test]
fn the_first_jump_goes_to_the_first_conflict() {
    let f = Fx::new();
    make_two_conflicts(&f);
    let mut h = harness(&f);
    open_merge(&mut h, "All ours");
    assert_eq!(merge_view_of(&h).unwrap().current_conflict(), None);
    h.get_by_label("Next conflict").click();
    h.step();
    h.step();
    assert_eq!(merge_view_of(&h).unwrap().current_conflict(), Some(0));
    h.get_by_label("Next conflict").click();
    h.step();
    h.step();
    assert_eq!(merge_view_of(&h).unwrap().current_conflict(), Some(1));
}

/// A deleted side has nothing to take, and asking for it was only an error from git.
#[test]
fn a_conflict_row_offers_only_the_sides_that_exist() {
    use gitem::views::changes::conflict_sides;
    assert_eq!(conflict_sides("UU"), (true, true));
    assert_eq!(conflict_sides("AA"), (true, true));
    assert_eq!(conflict_sides("DU"), (false, true));
    assert_eq!(conflict_sides("UD"), (true, false));
    assert_eq!(conflict_sides("AU"), (true, false));
    assert_eq!(conflict_sides("UA"), (false, true));
    assert_eq!(conflict_sides("DD"), (false, false));
}

#[test]
fn a_gitignore_entry_matches_that_file_alone() {
    let f = Fx::new();
    f.write("a.txt", "x");
    f.commit("base");
    // Asked by name, as Windows wouldn't allow some of these as files.
    for (name, near_miss) in [("a[1].txt", "a1.txt"), ("what?.txt", "whatX.txt"), ("sta*r.txt", "staxxr.txt"), ("#hash.txt", "hash.txt"), ("!bang.txt", "bang.txt"), ("sub dir/back up.txt", "sub dir/other.txt"), ("ends in space ", "ends in space")] {
        let entry = gitem::views::changes::gitignore_entry(name);
        f.write(".gitignore", &format!("{entry}\n"));
        let ignored = |p: &str| f.git.run_raw(Some(&f.root), &["check-ignore", "-q", "--no-index", "--", p]).unwrap().ok();
        assert!(ignored(name), "{entry} doesn't ignore {name}");
        assert!(!ignored(near_miss), "{entry} also ignores {near_miss}");
    }
}

// ---- file view -------------------------------------------------------------------------------

#[test]
fn a_utf16_file_reads_as_text_and_says_which_it_is() {
    let f = Fx::new();
    let bytes: Vec<u8> = [0xff, 0xfe].into_iter().chain("first line\nsecond line\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    std::fs::write(f.root.join("u.txt"), bytes).unwrap();
    f.commit("utf-16");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("u.txt", None, None));
    pump_for_any(&mut h, "UTF-16");
    wait_until_quiet(&mut h);
    assert!(painted_text(&h).iter().any(|(_, _, t)| t.contains("second line")));
    let sha = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    h.state_mut().repos[0].open_tab(Tab::file("u.txt", Some(sha), None));
    h.step();
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn a_file_that_isnt_utf8_reads_as_a_guess_and_says_so() {
    let f = Fx::new();
    std::fs::write(f.root.join("l.txt"), b"caf\xe9 au lait\n").unwrap();
    f.commit("latin-1");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("l.txt", None, None));
    pump_for_any(&mut h, "Latin-1 (a guess: not UTF-8)");
    wait_until_quiet(&mut h);
    assert!(painted_text(&h).iter().any(|(_, _, t)| t.contains("caf\u{e9} au lait")));
}

#[test]
fn a_picture_shows_as_a_picture() {
    let f = Fx::new();
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    std::fs::write(f.root.join("p.png"), &png).unwrap();
    f.commit("picture");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("p.png", None, None));
    pump_for_any(&mut h, "3 × 2 px");
    assert!(h.query_by_label("Binary file").is_none());
}

// ---- a right click means the line under it ----------------------------------------------------

#[test]
fn a_right_click_outside_the_selection_is_about_that_line() {
    let f = Fx::new();
    f.write("n.txt", "one\ntwo\nthree\nfour\nfive\n");
    f.commit("base");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("n.txt", None, None));
    pump_for_label(&mut h, "◀ Older");
    wait_until_quiet(&mut h);
    let spot = |h: &Harness<GitemApp>, t: &str| painted_text(h).into_iter().find(|(_, _, x)| x == t).map(|x| x.0).unwrap_or_else(|| panic!("{t} isn't on screen"));
    let two = spot(&h, "two") + vec2(4.0, 6.0);
    click_at(&mut h, two);
    h.step();
    pump_for_any(&mut h, "Line history 2-2");
    let p = spot(&h, "five") + vec2(4.0, 6.0);
    h.event(egui::Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    h.step();
    pump_for_any(&mut h, "Line history (5-5)");
}

use egui_kittest::kittest::NodeT;

fn pump_for_any_contains(h: &mut Harness<GitemApp>, text: &str) {
    let t = Instant::now();
    while h.query_all_by_label_contains(text).next().is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "nothing says {text:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

// ---- secondary views: a second request to an open tab (AS-18, VS-5, VS-14, VS-15)

fn tab_keys(h: &Harness<GitemApp>) -> Vec<String> {
    h.state().repos[0].dock.iter_all_tabs().map(|(_, t)| t.key()).collect()
}

fn open_tab_now(h: &mut Harness<GitemApp>, tab: Tab) {
    h.state_mut().repos[0].open_tab(tab);
    h.step();
    h.step();
}

#[test]
fn a_new_search_replaces_what_an_open_search_tab_shows() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::search("=:first"));
    pump_for_label(&mut h, "first");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("second").is_none());

    open_tab_now(&mut h, Tab::search("=:second"));
    pump_for_label(&mut h, "second");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("first").is_none(), "the old results are gone");
    assert_eq!(tab_keys(&h).iter().filter(|k| *k == "search").count(), 1, "in the same tab");

    // Plain Search commits leaves it alone.
    open_tab_now(&mut h, Tab::Search(Box::default()));
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("second").is_some(), "just opening the tab asks nothing new");
}

#[test]
fn a_bad_date_in_a_search_says_so() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::search("before:notadate"));
    wait_until_quiet(&mut h);
    let boxes = error_boxes(&h);
    assert!(boxes.iter().any(|b| b.contains("notadate")), "{boxes:?}");
}

#[test]
fn save_as_patch_with_the_patches_tab_open_starts_a_new_patch() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Patches(Box::default()));
    pump_for_label(&mut h, "New from staged");
    assert!(h.query_by_label("New patch").is_none());
    open_tab_now(&mut h, Tab::Patches(Box::new(gitem::views::patches::PatchesView::create_from(DiffSource::Commit("HEAD".into()), "from a commit"))));
    pump_for_label(&mut h, "New patch");
}

#[test]
fn compare_refs_opens_without_an_error_and_waits_for_two_refs() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::compare("", ""));
    pump_for_label(&mut h, "Pick two refs");
    wait_until_quiet(&mut h);
    no_errors(&h);
}

#[test]
fn a_second_job_with_the_same_title_runs() {
    use gitem::views::textview::{TextView, UndoSlot};
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let job = |text: &'static str| -> Box<dyn FnOnce() -> Result<String, String> + Send> { Box::new(move || Ok(text.to_string())) };
    open_tab_now(&mut h, Tab::Text(Box::new(TextView::new("Explain", job("first answer")))));
    pump_for_label(&mut h, "first answer");
    open_tab_now(&mut h, Tab::Text(Box::new(TextView::new("Explain", job("second answer")))));
    pump_for_label(&mut h, "second answer");
    assert!(h.query_by_label("first answer").is_none());
    assert_eq!(tab_keys(&h).iter().filter(|k| k.starts_with("text:")).count(), 1);

    // An answer that can still be undone isn't thrown away for a new one.
    let slot = UndoSlot::default();
    *slot.lock().unwrap() = Some(ops::undo_commit());
    open_tab_now(&mut h, Tab::Text(Box::new(TextView::with_undo("Rebase", job("rebased"), slot))));
    pump_for_label(&mut h, "rebased");
    open_tab_now(&mut h, Tab::Text(Box::new(TextView::with_undo("Rebase", job("rebased again"), UndoSlot::default()))));
    pump_for_label(&mut h, "rebased again");
    assert_eq!(tab_keys(&h).iter().filter(|k| k.starts_with("text:Rebase")).count(), 2, "{:?}", tab_keys(&h));
}

#[test]
fn a_link_to_a_line_of_an_open_file_goes_to_it() {
    let f = Fx::new();
    let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
    f.write("long.txt", &text);
    f.commit("long");
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::file("long.txt", None, None));
    wait_until_quiet(&mut h);
    open_tab_now(&mut h, Tab::file("long.txt", None, Some(400)));
    wait_until_quiet(&mut h);
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(tab_keys(&h).iter().filter(|k| k.starts_with("file:long.txt")).count(), 1);
    let wanted = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::File(v) = t { v.current_line() } else { None });
    assert_eq!(wanted, Some(399), "the line asked for, counting from 0");
}

// ---- Visual History (VS-9, VS-10, VS-35)

/// Moves the pointer over the chart until a bubble's tooltip shows.
fn find_bubble(h: &mut Harness<GitemApp>, subject: &str) -> egui::Pos2 {
    let size = h.ctx.content_rect();
    for y in (120..size.height() as i32 - 150).step_by(14) {
        for x in (400..size.width() as i32 - 340).step_by(14) {
            let at = egui::pos2(x as f32, y as f32);
            h.event(egui::Event::PointerMoved(at));
            h.step();
            if h.query_by_label(subject).is_some() {
                return at;
            }
        }
    }
    panic!("no bubble for {subject}");
}

fn right_click_at(h: &mut Harness<GitemApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
}

#[test]
fn visual_history_menu_survives_the_pointer_leaving_and_means_the_whole_commit() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.write("z.txt", "two\n");
    f.commit("touches two files");
    let sha = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::visual(""));
    wait_until_quiet(&mut h);
    let at = find_bubble(&mut h, "touches two files");
    assert!(h.query_by_label_contains("as a.txt").is_none(), "one file picked out of the commit for no reason");
    right_click_at(&mut h, at);
    pump_for_label(&mut h, "Open changes");
    assert!(h.query_by_label("Open this revision").is_none(), "there's no one file to open here");

    // On the way to the menu.
    h.event(egui::Event::PointerMoved(at + egui::vec2(60.0, 40.0)));
    for _ in 0..5 {
        h.step();
    }
    click(&mut h, "Open changes");
    wait_until_quiet(&mut h);
    let want = format!("diff:{:?}:None", DiffSource::Commit(sha));
    assert!(tab_keys(&h).contains(&want), "the whole commit: {:?}", tab_keys(&h));
    no_errors(&h);
}

#[test]
fn visual_history_of_one_file_offers_that_file() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("about a");
    let sha = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::visual("a.txt"));
    wait_until_quiet(&mut h);
    let at = find_bubble(&mut h, "about a");
    right_click_at(&mut h, at);
    pump_for_label(&mut h, "Open this revision");
    click(&mut h, "Open changes");
    wait_until_quiet(&mut h);
    let want = format!("diff:{:?}:{:?}", DiffSource::Commit(sha), Some("a.txt"));
    assert!(tab_keys(&h).contains(&want), "{:?}", tab_keys(&h));
}

#[test]
fn visual_history_of_a_bad_branch_shows_the_error_and_stops_there() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Visual(Box::new(gitem::views::visual::VisualView::new("").on_rev("no-such-branch"))));
    wait_until_quiet(&mut h);
    assert!(!error_boxes(&h).is_empty(), "the error");
    assert!(h.query_by_label("Reading file history…").is_none(), "nothing's still being read");
}

// ---- Start work (VS-2)

#[test]
fn work_started_from_home_on_the_default_base_offers_publish() {
    let f = Fx::new();
    sample_history(&f);
    let bare = tempfile::tempdir().unwrap();
    let dir = bare.path().to_string_lossy().replace('\\', "/");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &dir]).unwrap();
    f.git(&["remote", "add", "origin", &dir]);
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["remote", "set-head", "origin", "main"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::home());
    click(&mut h, "Start work…");
    pump_for_label(&mut h, "Based on");
    wait_until_quiet(&mut h);
    h.event(egui::Event::Text("feature/x".into()));
    h.step();
    click(&mut h, "Start");
    wait_for(&mut h, "on the new branch", || f.git(&["branch", "--show-current"]).trim() == "feature/x");
    assert!(!f.git.run_raw(Some(&f.root), &["config", "--get", "branch.feature/x.remote"]).unwrap().ok(), "it doesn't track main");
    wait_until_quiet(&mut h);
    pump_for_any(&mut h, "Publish");
    no_errors(&h);
}

// ---- Changes picks up settings (VS-16)

#[test]
fn turning_on_signoff_reaches_an_open_changes_tab() {
    let f = Fx::new();
    sample_history(&f);
    f.write("c.txt", "new\n");
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::changes());
    pump_for_label(&mut h, "Sign-off");
    assert_eq!(h.get_by_label("Sign-off").accesskit_node().toggled(), Some(egui::accesskit::Toggled::False));
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Settings(Box::new(|s| s.commit_signoff = true)));
    h.step();
    h.step();
    assert_eq!(h.get_by_label("Sign-off").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));

    // Unticking it for one commit isn't undone by nothing having changed.
    click(&mut h, "Sign-off");
    h.step();
    assert_eq!(h.get_by_label("Sign-off").accesskit_node().toggled(), Some(egui::accesskit::Toggled::False));
}

// ---- Bisect (VS-19)

#[test]
fn bisect_says_when_it_has_found_the_commit() {
    let f = Fx::new();
    f.write("a.txt", "good\n");
    f.commit("the good one");
    let good = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    f.write("a.txt", "bug\n");
    f.commit("the bug");
    f.git(&["bisect", "start", "HEAD", &good]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Bisect(Box::default()));
    pump_for_label(&mut h, "Bisect found the commit");
    pump_for_label(&mut h, "the bug");
    assert!(h.query_by_label("\u{E182} Good").is_none(), "nothing's left to mark");
    assert!(h.query_by_label("Now testing").is_none());
    click(&mut h, "End bisect");
    wait_for(&mut h, "bisect ended", || !f.root.join(".git/BISECT_LOG").exists());
    no_errors(&h);
}

// ---- Worktrees (VS-20, VS-44, VS-45)

#[test]
fn the_worktree_a_tab_is_open_on_cant_be_removed_from_it() {
    let f = Fx::new();
    sample_history(&f);
    let wt = f.root.parent().unwrap().join(format!("{}-wt", f.root.file_name().unwrap().to_string_lossy()));
    let wt_str = wt.to_string_lossy().replace('\\', "/");
    let wt_str = wt_str.strip_prefix("//?/").unwrap_or(&wt_str).to_string();
    f.git(&["worktree", "add", "-q", "-b", "side", &wt_str]);
    let linked = Fx { _dir: tempfile::tempdir().unwrap(), root: wt.canonicalize().unwrap(), git: f.git.clone() };
    let mut h = harness(&linked);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Worktrees(Box::default()));
    pump_for_label(&mut h, "open here");
    let ups = h.query_all_by_label("More \u{E136}").count();
    assert_eq!(ups, 2);
    let current_is_second = h.get_by_label("open here").rect().top() > h.get_by_label("main worktree").rect().top();
    h.query_all_by_label("More \u{E136}").nth(usize::from(current_is_second)).unwrap().click();
    h.step();
    h.step();
    pump_for_label(&mut h, "Copy path");
    assert!(h.query_by_label("Remove…").is_none(), "git would empty the folder and then fail");
    assert!(h.query_by_label("Force remove…").is_none());
    assert!(wt.exists());
    std::fs::remove_dir_all(&wt).ok();
}

// ---- Workspaces (VS-42)

#[test]
fn a_repository_can_be_taken_out_of_a_workspace_and_names_stay_unique() {
    let f = Fx::new();
    sample_history(&f);
    let path = f.root.to_string_lossy().replace('\\', "/");
    let mut h = harness(&f);
    h.state_mut().settings.workspaces.push(gitem_core::settings::Workspace { name: "work".into(), repos: vec![path.clone()] });
    settle(&mut h);
    open_tab_now(&mut h, Tab::home());
    pump_for_label(&mut h, "New workspace…");
    click(&mut h, "New workspace…");
    pump_for_label(&mut h, "New workspace");
    h.event(egui::Event::Text("work".into()));
    h.step();
    click(&mut h, "Create");
    pump_for_label(&mut h, "There's already a workspace called work.");
    assert_eq!(h.state().settings.workspaces.len(), 1);
    click(&mut h, "Cancel");

    let name = f.root.file_name().unwrap().to_string_lossy().to_string();
    wait_until_quiet(&mut h);
    let row = h.query_all_by_label(&name).last().unwrap().rect().center();
    right_click_at(&mut h, row);
    click(&mut h, "Remove from this workspace");
    wait_for(&mut h, "removed", || true);
    h.step();
    assert!(h.state().settings.workspaces[0].repos.is_empty(), "{:?}", h.state().settings.workspaces);
}

// ---- Two repositories (AS-15, AS-16)

#[test]
fn undo_in_one_repositorys_commit_box_stays_in_that_repository() {
    let (a, b) = (Fx::new(), Fx::new());
    sample_history(&a);
    sample_history(&b);
    let mut h = harness(&a);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_any(&mut h, "Commit to main");
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("MESSAGE-FROM-REPO-A");
    for _ in 0..3 {
        h.step();
    }

    h.state_mut().open_repo(&ctx, &b.root, false);
    wait_until_quiet(&mut h);
    h.state_mut().repos[1].open_tab(Tab::changes());
    pump_for_any(&mut h, "Commit to main");
    h.get_by_role(input).focus();
    h.step();
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.step();
    }
    let message = |h: &Harness<GitemApp>, i: usize| h.state().repos[i].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Changes(c) = t { Some(c.message.clone()) } else { None }).unwrap();
    assert_eq!(message(&h, 1), "", "the other repository's message came back");
    assert_eq!(message(&h, 0), "MESSAGE-FROM-REPO-A");
}

#[test]
fn closing_a_repository_before_the_one_on_show_keeps_showing_it() {
    let (a, b, c) = (Fx::new(), Fx::new(), Fx::new());
    for f in [&a, &b, &c] {
        sample_history(f);
    }
    let mut h = harness(&a);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().open_repo(&ctx, &b.root, false);
    h.state_mut().open_repo(&ctx, &c.root, false);
    h.state_mut().active = 1;
    h.step();
    let showing = h.state().repos[1].repo.root.clone();
    h.state_mut().request_close_repo(0);
    h.step();
    assert_eq!(h.state().repos.len(), 2);
    assert_eq!(h.state().repos[h.state().active].repo.root, showing);
}

// ---- Inspect after a write (AS-20)

#[test]
fn the_inspect_panel_shows_a_note_as_soon_as_it_is_written() {
    let f = Fx::new();
    sample_history(&f);
    let sha = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].inspect.set(&sha);
    wait_until_quiet(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::note_set(&sha, "NOTE-XYZ")));
    pump_for_any_contains(&mut h, "NOTE-XYZ");
}

// ---- Keys (AS-31)

#[test]
fn redo_in_a_text_field_leaves_the_panels_alone() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    let shift_command = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
    h.key_press_modifiers(shift_command, egui::Key::Z);
    h.step();
    h.step();
    assert!(h.state().repos[0].panels_showing(), "Redo, in a text field");

    // Elsewhere it's still Zen mode.
    h.get_by_label("Commit to main");
    h.state_mut().repos[0].open_tab(Tab::graph());
    h.ctx.memory_mut(|m| m.stop_text_input());
    h.step();
    h.key_press_modifiers(shift_command, egui::Key::Z);
    h.step();
    h.step();
    assert!(!h.state().repos[0].panels_showing());
}

// ---- Status bar (AS-28, AS-35)

#[test]
fn the_status_bar_only_speaks_of_the_file_and_branch_in_front() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "other"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].data.branch_pr = Some(("main".to_string(), 7, "https://example.invalid/pr/7".to_string()));
    h.step();
    h.step();
    pump_for_label(&mut h, "PR #7");
    f.git(&["switch", "-q", "other"]);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Refresh);
    let t = Instant::now();
    while h.query_by_label("PR #7").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "still links to main's pull request on another branch");
    }

    h.state_mut().repos[0].data.last_blame = "BLAME-TEXT".to_string();
    open_tab_now(&mut h, Tab::file("a.txt", None, None));
    wait_until_quiet(&mut h);
    open_tab_now(&mut h, Tab::graph());
    assert!(h.query_by_label("BLAME-TEXT").is_none(), "that file isn't in front");
}

// ---- Launchpad (VS-22, VS-23, VS-24, VS-50). A `.invalid` host: nothing ever answers.

fn launchpad_repo(host: &str) -> (Fx, gitem_core::settings::CustomHost) {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["remote", "add", "origin", &format!("https://{host}/group/project.git")]);
    let custom = gitem_core::settings::CustomHost { host: format!("https://{host}/"), kind: gitem_core::forge::ForgeKind::GitLab, api_base: String::new(), templates: Default::default() };
    (f, custom)
}

#[test]
fn launchpad_reloads_when_a_token_is_saved() {
    let host = "reload-on-save.invalid";
    let (f, custom) = launchpad_repo(host);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Settings(Box::new(move |s| s.custom_hosts.push(custom))));
    open_tab_now(&mut h, Tab::Launchpad(Box::default()));
    wait_until_quiet(&mut h);
    let no_token = |h: &Harness<GitemApp>| error_boxes(h).iter().any(|b| b.contains("No token"));
    assert!(no_token(&h), "{:?}", error_boxes(&h));

    gitem_core::cred::set_forge_token(host, "", "FAKE_SECRET_VALUE").unwrap();
    wait_until_quiet(&mut h);
    assert!(no_token(&h), "nothing has told it yet");
    // What saving the token form does.
    h.state_mut().apply_action(&ctx, Action::Refresh);
    h.step();
    wait_until_quiet(&mut h);
    assert!(!no_token(&h), "still says there's no token: {:?}", error_boxes(&h));
    gitem_core::cred::delete(&format!("{}{host}", gitem_core::cred::PREFIX_FORGE)).unwrap();
}

#[test]
fn create_pull_request_with_launchpad_open_goes_to_the_form_for_that_branch() {
    let host = "retarget.invalid";
    let (f, custom) = launchpad_repo(host);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Settings(Box::new(move |s| s.custom_hosts.push(custom))));
    open_tab_now(&mut h, Tab::Launchpad(Box::default()));
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("Open in browser instead").is_none());
    open_tab_now(&mut h, Tab::Launchpad(Box::new(gitem::views::launchpad::LaunchpadView::create_pr_for("topic", "main"))));
    pump_for_label(&mut h, "Open in browser instead");
    assert_eq!(tab_keys(&h).iter().filter(|k| *k == "launchpad").count(), 1);
}

// ---- Repository Tools writes (TQ-3 item 6, CG-33)

#[test]
fn hooks_are_made_turned_off_and_on_and_deleted() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Tools(Box::default()));
    click(&mut h, "Hooks");
    wait_until_quiet(&mut h);
    let hook = f.root.join(".git/hooks/pre-commit");
    let off = f.root.join(".git/hooks/pre-commit.disabled");
    std::fs::remove_file(&hook).ok();
    let y = h.get_by_label("pre-commit").rect().center().y;
    let create = h.query_all_by_label("Create").find(|n| (n.rect().center().y - y).abs() < 12.0).expect("a Create on its row").rect().center();
    click_at(&mut h, create);
    wait_until_quiet(&mut h);
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), "#!/bin/sh\n\nexit 0\n");

    click(&mut h, "Enabled");
    wait_until_quiet(&mut h);
    assert!(off.exists() && !hook.exists(), "turned off by renaming it");
    click(&mut h, "Enabled");
    wait_until_quiet(&mut h);
    assert!(hook.exists() && !off.exists(), "and back");

    click(&mut h, "Delete…");
    click(&mut h, "Delete it");
    wait_until_quiet(&mut h);
    assert!(!hook.exists());
    no_errors(&h);
}

#[test]
fn the_ignore_page_saves_what_is_typed_and_reloads_what_is_there() {
    let f = Fx::new();
    sample_history(&f);
    f.write(".gitignore", "*.log\n");
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Tools(Box::default()));
    click(&mut h, ".gitignore");
    pump_for_label(&mut h, "Save");
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("build/\n");
    h.step();
    h.step();
    click(&mut h, "Save");
    wait_until_quiet(&mut h);
    let saved = std::fs::read_to_string(f.root.join(".gitignore")).unwrap();
    assert!(saved.contains("build/") && saved.contains("*.log"), "{saved:?}");
    assert!(f.git.run_raw(Some(&f.root), &["check-ignore", "-q", "build/x"]).unwrap().ok(), "git goes by it");

    std::fs::write(f.root.join(".gitignore"), "changed-outside\n").unwrap();
    click(&mut h, "Reload");
    h.step();
    h.step();
    assert_eq!(h.get_by_role(input).accesskit_node().value().unwrap_or_default().trim(), "changed-outside");
    no_errors(&h);
}

#[test]
fn clean_deletes_what_it_lists_and_says_what_it_leaves() {
    let f = Fx::new();
    sample_history(&f);
    f.write("junk/file.txt", "x");
    f.write("nested/inner.txt", "y");
    f.git.run(Some(&f.root.join("nested")), &["init", "-q"]).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Tools(Box::default()));
    click(&mut h, "Clean");
    click(&mut h, "Include folders");
    wait_until_quiet(&mut h);
    pump_for_label(&mut h, "1 item would be deleted");
    pump_for_any_contains(&mut h, "Left alone");
    assert!(h.query_by_label_contains("nested/").is_some());
    click(&mut h, "Delete these…");
    pump_for_label(&mut h, "Delete");
    wait_until_quiet(&mut h);
    click(&mut h, "Delete");
    wait_for(&mut h, "deleted", || !f.root.join("junk").exists());
    assert!(f.root.join("nested/inner.txt").exists());
    no_errors(&h);
}

#[test]
fn sparse_patterns_are_read_and_applied() {
    let f = Fx::new();
    f.write("keep/a.txt", "a\n");
    f.write("drop/b.txt", "b\n");
    f.commit("two folders");
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Tools(Box::default()));
    click(&mut h, "Sparse checkout");
    pump_for_label(&mut h, "Disable sparse checkout");
    let input = egui::accesskit::Role::MultilineTextInput;
    h.get_by_role(input).focus();
    h.step();
    h.get_by_role(input).type_text("keep");
    h.step();
    h.step();
    click(&mut h, "Apply");
    wait_for(&mut h, "sparse applied", || !f.root.join("drop/b.txt").exists());
    assert!(f.root.join("keep/a.txt").exists());
    click(&mut h, "Disable sparse checkout");
    wait_for(&mut h, "sparse off", || f.root.join("drop/b.txt").exists());
    no_errors(&h);
}

#[test]
fn object_counts_change_after_garbage_collection() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Tools(Box::default()));
    click(&mut h, "Maintenance");
    wait_until_quiet(&mut h);
    pump_for_any_contains(&mut h, "in-pack: 0");
    click(&mut h, "Garbage collect");
    wait_for(&mut h, "packed", || !f.git(&["count-objects", "-v"]).contains("in-pack: 0"));
    let t = Instant::now();
    while h.query_by_label_contains("in-pack: 0").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "still the counts from before");
    }
}

// ---- Review (VS-36, VS-39)

#[test]
fn review_with_a_base_that_doesnt_exist_asks_for_another() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("first");
    f.git(&["branch", "-m", "work"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Review(Box::default()));
    wait_until_quiet(&mut h);
    assert!(h.query_by_label_contains("Can't compare with").is_some(), "{:?}", error_boxes(&h));
    no_errors(&h);
}

#[test]
fn a_review_note_can_be_dismissed_and_brought_back() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "base", "HEAD~1"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Review(Box::default()));
    wait_until_quiet(&mut h);
    h.query_all_by_role(egui::accesskit::Role::TextInput).last().unwrap().focus();
    h.step();
    h.event(egui::Event::Text("look at this".into()));
    h.step();
    click(&mut h, "Add");
    pump_for_label(&mut h, "look at this");
    click(&mut h, "Dismiss");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("look at this").is_none());
    click(&mut h, "Show dismissed (1)");
    pump_for_label(&mut h, "look at this");
    click(&mut h, "Restore");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("look at this").is_some());
    assert!(h.query_by_label_contains("Show dismissed").is_some());
}

#[test]
fn a_reviewed_mark_goes_when_the_file_changes() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "main-base", "HEAD~1"]);
    f.git(&["switch", "-qc", "topic"]);
    f.write("t.txt", "one\n");
    f.commit("topic work");
    let mut h = harness(&f);
    settle(&mut h);
    let review = f.root.join(".git/gitem-review.json");
    std::fs::write(&review, r#"{"base":"main","reviewed":[],"threads":[]}"#).unwrap();
    open_tab_now(&mut h, Tab::Review(Box::default()));
    pump_for_label(&mut h, "0 of 1 files reviewed");
    wait_until_quiet(&mut h);
    // The file's row is the only tick box here.
    h.get_by_role(egui::accesskit::Role::CheckBox).click();
    h.step();
    h.step();
    pump_for_label(&mut h, "1 of 1 files reviewed");

    f.write("t.txt", "two\n");
    f.commit("more");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Refresh);
    pump_for_label(&mut h, "0 of 1 files reviewed");
    no_errors(&h);
}

// ---- Recents (AS-44)

#[test]
fn a_recent_repository_can_be_forgotten() {
    let settings = gitem_core::settings::Settings { recent_repos: vec!["Z:/gone/one".to_string(), "Z:/gone/two".to_string()], ..Default::default() };
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, settings);
    app.git = Ok(real_git());
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1200.0, 800.0)).with_max_steps(5);
    let mut h = builder.build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app);
    h.step();
    pump_for_label(&mut h, "Z:/gone/one");
    let at = h.get_by_label("Z:/gone/one").rect().center();
    right_click_at(&mut h, at);
    click(&mut h, "Remove from this list");
    h.step();
    assert_eq!(h.state().settings.recent_repos, ["Z:/gone/two"]);
    assert!(h.state().errors_showing().is_empty(), "nothing tried to open it");
}

// ---- Graph (Current filter, Phase 6 remainder)

#[test]
fn the_current_filter_works_on_a_branch_with_no_upstream() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["switch", "-qc", "lonely"]);
    f.write("c.txt", "c\n");
    f.commit("on lonely");
    f.git(&["switch", "-qc", "elsewhere", "main"]);
    f.write("d.txt", "d\n");
    f.commit("on elsewhere");
    f.git(&["switch", "-q", "lonely"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::graph());
    pump_for_label(&mut h, "on elsewhere");
    click(&mut h, "Filters \u{e136}");
    click(&mut h, "Current");
    let t = Instant::now();
    while h.query_by_label("on elsewhere").is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the filter did nothing");
    }
    pump_for_label(&mut h, "on lonely");
    assert!(h.query_by_label("second").is_some(), "the branch's history, not an empty graph");
    no_errors(&h);
}

// ---- Pick lists (CG-11)

#[test]
fn a_tag_and_branch_of_one_name_are_told_apart_in_pick_lists() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "same"]);
    f.git(&["tag", "same", "HEAD~1"]);
    f.git(&["tag", "only-tag"]);
    let mut h = harness(&f);
    settle(&mut h);
    let names = h.state().repos[0].data.ref_names();
    assert!(names.contains(&"refs/heads/same".to_string()), "{names:?}");
    assert!(names.contains(&"refs/tags/same".to_string()), "{names:?}");
    assert!(!names.contains(&"same".to_string()), "{names:?}");
    assert!(names.contains(&"only-tag".to_string()), "{names:?}");
    for n in &names {
        assert_eq!(names.iter().filter(|x| *x == n).count(), 1, "{n}");
        f.git(&["rev-parse", "--verify", "-q", n]);
    }
    let branch = f.git(&["rev-parse", "refs/heads/same"]);
    let tag = f.git(&["rev-parse", "refs/tags/same"]);
    assert_ne!(branch, tag, "each means what it says");
}

// ---- Console (VS-50)

#[test]
fn a_console_with_nothing_to_show_says_why() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Console(Box::default()));
    pump_for_label(&mut h, "Nothing to show");
}

// ---- Palette (VS-43)

#[test]
fn the_palette_finds_commands_people_look_for() {
    let f = Fx::new();
    sample_history(&f);
    let names: Vec<String> = gitem::palette::commands().into_iter().map(|c| c.label).collect();
    let _ = f;
    for (typed, expect) in [("blame", "Blame a File…"), ("amend", "Amend Last Commit's Message…"), ("delete", "Delete a Branch…"), ("commit", "Commit Changes"), ("theme", "Change Theme…")] {
        let best = names.iter().filter_map(|n| gitem_core::util::fuzzy_match(typed, n).map(|m| (m.0, n))).max_by_key(|x| x.0).map(|x| x.1.as_str());
        assert!(names.iter().any(|n| n == expect), "{expect}");
        assert!(gitem_core::util::fuzzy_match(typed, expect).is_some(), "{typed} doesn't reach {expect}: best was {best:?}");
    }
}

/// Through the palette, with what it would do on the first result for the typed text.
fn run_from_palette(h: &mut Harness<GitemApp>, typed: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::OpenPalette);
    h.step();
    h.step();
    h.event(egui::Event::Text(typed.into()));
    h.step();
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
}

#[test]
fn undo_last_commit_from_the_palette_on_a_first_commit_says_so() {
    let f = Fx::new();
    f.write("a.txt", "a
");
    f.commit("only");
    let mut h = harness(&f);
    settle(&mut h);
    run_from_palette(&mut h, "Undo Last Commit");
    pump_for_any_contains(&mut h, "first commit on this branch");
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    assert_eq!(f.subjects(), ["only"]);
}

#[test]
fn checking_out_a_remote_branch_with_a_local_one_of_that_name_switches_to_it() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.git(&["switch", "-qc", "other"]);
    let mut h = harness(&f);
    settle(&mut h);
    run_from_palette(&mut h, "origin/main");
    wait_for(&mut h, "switched", || f.git(&["branch", "--show-current"]).trim() == "main");
    no_errors(&h);
}

// ---- The interface (Phase 8)

fn two_changed_files(f: &Fx) {
    sample_history(f);
    f.write("a.txt", "one\nTWO\nthree\nmore\n");
    f.write("b.txt", "fn other() {}\n");
}

/// The Changes tab in front, with its list on screen.
fn changes_up(h: &mut Harness<GitemApp>, file: &str) {
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(h, file);
    wait_until_quiet(h);
}

/// A window of this size, with default settings apart from `settings`.
fn sized_harness<'a>(f: &Fx, size: egui::Vec2, settings: gitem_core::settings::Settings) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, settings);
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    Harness::builder().with_size(size).with_max_steps(5).build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    )
}

/// With frames as far apart as a real display's, as a double click needs.
fn fast_clock_harness<'a>(f: &Fx) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, no_details());
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).with_step_dt(1.0 / 60.0).build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    )
}

fn no_details() -> gitem_core::settings::Settings {
    gitem_core::settings::Settings { details_on_open: false, ..Default::default() }
}

fn checks_changes_screen(tree: bool) {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = harness(&f);
    h.state_mut().settings.changes_tree = tree;
    settle(&mut h);
    changes_up(&mut h, "a.txt");
    let tabs = tab_titles(&h).len();
    click(&mut h, "a.txt");
    pump_for_label(&mut h, "Stage file");
    wait_until_quiet(&mut h);
    // The diff is beside the list, and both the list and the message box stayed.
    assert_eq!(tab_titles(&h).len(), tabs, "no new tab");
    assert!(h.query_by_label("b.txt").is_some());
    assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect().left() < h.get_by_label("Stage file").rect().left());
    // Its own tab is still there for anyone who wants one, as are the menu's routes.
    click(&mut h, "Open in tab");
    wait_until_quiet(&mut h);
    assert_eq!(tab_titles(&h).len(), tabs + 1);
    assert!(tab_titles(&h).iter().any(|t| t == "a.txt (Unstaged)"));
}

#[test]
fn a_click_on_a_changed_file_shows_its_diff_beside_the_list() {
    checks_changes_screen(false);
}

#[test]
fn the_tree_list_shows_the_diff_beside_it_too() {
    checks_changes_screen(true);
}

#[test]
fn a_narrow_changes_pane_opens_the_diff_in_a_tab_as_before() {
    let f = Fx::new();
    two_changed_files(&f);
    // Narrower than the app's own minimum, as a pane of a split can be.
    let mut h = sized_harness(&f, vec2(700.0, 600.0), no_details());
    settle(&mut h);
    changes_up(&mut h, "a.txt");
    click(&mut h, "a.txt");
    wait_until_quiet(&mut h);
    assert!(tab_titles(&h).iter().any(|t| t == "a.txt (Unstaged)"), "{:?}", tab_titles(&h));
}

#[test]
fn a_double_click_stages_the_file_and_its_diff_goes_with_it() {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = fast_clock_harness(&f);
    settle(&mut h);
    changes_up(&mut h, "a.txt");
    // Inside the double-click time, on a clock that isn't a test's quarter of a second a frame.
    h.get_by_label("a.txt").click();
    h.step();
    h.get_by_label("a.txt").click();
    h.step();
    wait_for(&mut h, "a.txt to be staged", || f.git(&["status", "--short"]).contains("M  a.txt"));
    wait_until_quiet(&mut h);
    // Space takes it back, since the keyboard follows the same file.
    assert!(h.query_by_label("Unstage file").is_some(), "the diff went to the staged list with it");
    h.key_press(egui::Key::Space);
    wait_for(&mut h, "a.txt to be unstaged", || f.git(&["status", "--short"]).contains(" M a.txt"));
    no_errors(&h);
}

#[test]
fn the_keyboard_walks_the_changed_files_and_stages_them() {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_up(&mut h, "a.txt");
    h.key_press(egui::Key::ArrowDown);
    h.step();
    pump_for_label(&mut h, "Stage file");
    h.key_press(egui::Key::ArrowDown);
    wait_until_quiet(&mut h);
    h.key_press(egui::Key::Space);
    wait_for(&mut h, "b.txt to be staged", || f.git(&["status", "--short"]).contains("M  b.txt"));
    wait_until_quiet(&mut h);
    assert!(f.git(&["status", "--short"]).contains(" M a.txt"), "only the current file");
    // Its menu, which has been a mouse's alone.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F10);
    pump_for_label(&mut h, "Unstage");
    no_errors(&h);
}

#[test]
fn keys_stay_out_of_a_text_box() {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "typing");
    h.key_press(egui::Key::Space);
    wait_until_quiet(&mut h);
    assert!(!f.git(&["status", "--short"]).contains("M  "), "a space in the message stages nothing");
}

#[test]
fn a_dialog_keeps_clicks_off_the_app_behind_it_and_lets_the_window_close() {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_up(&mut h, "a.txt");
    let row = h.get_by_label("a.txt").rect().center();
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    wait_until_quiet(&mut h);
    // What's behind it takes no click.
    h.event(egui::Event::PointerMoved(row));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: row, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("Stage file").is_none(), "the click reached the list behind the dialog");
    // The window's own controls aren't behind anything.
    h.get_by_label("Close window").click();
    h.step();
    let closing = h.output().viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    h.step();
    let closing = closing || h.output().viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closing, "the close button took no click");
}

#[test]
fn a_click_as_a_dialog_appears_does_nothing() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Confirm { title: "Delete it".into(), body: "Sure?".into(), label: "Do it".into(), op: ops::tag_create("too-soon", None, None, false) });
    h.step();
    // In the first moments, as the second half of a double click would be.
    h.get_by_label("Do it").click();
    h.step();
    assert_no_op_started(&mut h, "that click wasn't aimed at this");
    assert!(h.query_by_label("Do it").is_some(), "the question is still there");
    click(&mut h, "Do it");
    wait_for(&mut h, "the tag", || f.git(&["tag"]).contains("too-soon"));
}

#[test]
fn enter_submits_a_form_from_its_field_and_a_pick_field_takes_the_typing() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "side"]);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::new_branch(None)));
    wait_until_quiet(&mut h);
    h.event(egui::Event::Text("via-enter".into()));
    h.step();
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, "the branch", || f.git(&["branch"]).contains("via-enter"));
    wait_until_quiet(&mut h);
    // A form whose first field is a picker.
    h.state_mut().apply_action(&ctx, Action::Form(forms::set_upstream("main".into(), vec!["origin/main".into()])));
    wait_until_quiet(&mut h);
    pump_for_label(&mut h, "origin/main");
    h.event(egui::Event::Text("zzz".into()));
    h.step();
    h.step();
    assert!(h.query_by_label("origin/main").is_none(), "the typing reached the picker and narrowed it");
}

#[test]
fn enter_answers_a_question_that_loses_nothing_and_not_one_that_does() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Confirm { title: "Loses".into(), body: "Sure?".into(), label: "Do it".into(), op: ops::tag_create("risky", None, None, false) });
    wait_until_quiet(&mut h);
    h.key_press(egui::Key::Enter);
    assert_no_op_started(&mut h, "Enter doesn't answer a question about losing something");
    click(&mut h, "Cancel");
    h.state_mut().apply_action(&ctx, Action::Confirm { title: "Safe".into(), body: "Fine?".into(), label: "Do it".into(), op: ops::tag_create("safe", None, None, false).lossless() });
    wait_until_quiet(&mut h);
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, "the tag", || f.git(&["tag"]).contains("safe"));
}

#[test]
fn the_close_guard_covers_a_changed_rebase_plan() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::rebase(Some("HEAD~1".into())));
    pump_for_label(&mut h, "Rebase 1 commit");
    wait_until_quiet(&mut h);
    assert!(h.state().window_at_stake().is_empty(), "an untouched plan is no loss");
    for (_, t) in h.state_mut().repos[0].dock.iter_all_tabs_mut() {
        if let Tab::Rebase(r) = t {
            r.items[0].action = gitem_core::rebase::RebaseAction::Drop;
        }
    }
    assert_eq!(h.state().window_at_stake().len(), 1, "{:?}", h.state().window_at_stake());
}

#[test]
fn a_note_being_typed_counts_and_a_pull_request_being_written_counts() {
    let mut v = gitem::views::launchpad::LaunchpadView::default();
    assert!(v.unsaved().is_none());
    v.write_for_test("A title", "A description");
    assert!(v.unsaved().is_some());
    let mut r = gitem::views::review::ReviewView::default();
    assert!(r.unsaved().is_none());
    r.write_for_test("a thought");
    assert!(r.unsaved().is_some());
}

#[test]
fn many_repositories_leave_the_window_controls_on_screen() {
    let parent = tempfile::tempdir().unwrap();
    let mut roots = Vec::new();
    for i in 0..9 {
        let root = parent.path().canonicalize().unwrap().join(format!("a-repository-with-a-really-rather-long-name-{i}"));
        std::fs::create_dir(&root).unwrap();
        real_git().run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
        roots.push(root);
    }
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    for r in &roots {
        h.state_mut().open_repo(&ctx, r, false);
    }
    wait_until_quiet(&mut h);
    let edge = h.ctx.content_rect().right();
    for name in ["Close window", "Maximize window", "Minimize window", "Command palette"] {
        assert!(h.get_by_label(name).rect().right() <= edge + 0.5, "{name} is off screen");
    }
    // The ones that don't fit are in a list, and the one in use is on show.
    assert!(h.query_by_label("a-repository-with-a-really-rather-long-name-8").is_some());
    assert!(h.query_by_label_contains(gitem::chrome::icon::DOTS_THREE).is_some(), "the rest are a click away");
}

#[test]
fn f1_lists_the_keys_and_can_be_narrowed() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    h.key_press(egui::Key::F1);
    pump_for_label(&mut h, "Keyboard shortcuts");
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("Ctrl+Shift+K").is_some());
    h.event(egui::Event::Text("fetch".into()));
    h.step();
    h.step();
    assert!(h.query_by_label("Ctrl+Shift+F").is_some());
    assert!(h.query_by_label("Ctrl+Shift+K").is_none());
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();
    assert!(h.query_by_label("Keyboard shortcuts").is_none());
}

#[test]
fn keys_close_a_tab_and_go_to_a_repository() {
    let f = Fx::new();
    sample_history(&f);
    let other = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    let before = tab_titles(&h).len();
    let ctx = h.ctx.clone();
    h.state_mut().repos[0].open_tab(Tab::Tools(Box::default()));
    wait_until_quiet(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    wait_until_quiet(&mut h);
    assert_eq!(tab_titles(&h).len(), before, "{:?}", tab_titles(&h));
    h.state_mut().open_repo(&ctx, &other.root, false);
    wait_until_quiet(&mut h);
    assert_eq!(h.state().active, 1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num1);
    h.step();
    h.step();
    assert_eq!(h.state().active, 0);
}

#[test]
fn the_merge_banner_names_both_sides_and_the_tool_goes_on_to_the_next_file() {
    let f = Fx::new();
    f.write("a.txt", "base\n");
    f.write("b.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("a.txt", "side\n");
    f.write("b.txt", "side\n");
    f.commit("on side");
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.write("b.txt", "main\n");
    f.commit("on main");
    assert!(f.git.run(Some(&f.root), &["merge", "side"]).is_err() || f.git(&["status", "--short"]).contains("UU"));
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Merging side into main");
    click(&mut h, "Resolve");
    pump_for_label(&mut h, "Next file");
    assert!(h.query_by_label("File 1 of 2").is_some());
    click(&mut h, "Next file");
    pump_for_label(&mut h, "File 2 of 2");
    // Once the last is done, the merge is one click away.
    click(&mut h, "All ours");
    click(&mut h, "Save and mark resolved");
    pump_for_label(&mut h, "Next file: a.txt");
    click(&mut h, "Next file: a.txt");
    pump_for_label(&mut h, "All ours");
    click(&mut h, "All ours");
    click(&mut h, "Save and mark resolved");
    pump_for_label(&mut h, "Commit merge…");
}

#[test]
fn a_rebase_conflict_offers_continue_and_names_the_branch() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit("base");
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("c.txt", "topic\n");
    f.commit("on topic");
    f.git(&["switch", "-q", "main"]);
    f.write("c.txt", "main\n");
    f.commit("on main");
    f.git(&["switch", "-q", "topic"]);
    assert!(f.git.run(Some(&f.root), &["rebase", "main"]).is_err() || f.git(&["status", "--short"]).contains("UU"));
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Rebasing topic onto main (step 1 of 1)");
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Continue rebase");
    assert!(h.get_by_label("Continue rebase").accesskit_node().is_disabled(), "not with a conflict left");
    assert!(h.query_by_label("Commit to topic (rebasing)").is_some(), "HEAD is detached, but that's not the story");
    // The tool's sides read for a rebase, once it has read the status (AS-43).
    h.state_mut().repos[0].open_tab(Tab::merge("c.txt"));
    pump_for_label(&mut h, "All upstream");
    assert!(h.query_by_label("All your commit").is_some());
}

#[test]
fn an_empty_repository_points_at_a_first_commit_and_offers_no_dead_ends() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "No commits yet");
    assert!(h.query_by_label("Add remote…").is_some());
    assert!(h.get_by_label("Pull").accesskit_node().is_disabled(), "there's nothing to pull from");
    assert!(h.get_by_label("Fetch").accesskit_node().is_disabled());
    click(&mut h, "Publish");
    pump_for_label(&mut h, "Name");
    pump_for_label(&mut h, "URL");
}

#[test]
fn details_only_show_beside_views_that_have_commits() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.query_by_label("No commit selected").is_none(), "nothing picked, so no panel taking room");
    let head = f.git(&["rev-parse", "HEAD"]);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Inspect(head.trim().to_string()));
    pump_for_label(&mut h, "Open all");
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("Open all").is_none(), "Settings has nothing to do with a commit");
}

#[test]
fn opening_the_graph_shows_the_head_commit() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = sized_harness(&f, vec2(1500.0, 940.0), Default::default());
    settle(&mut h);
    pump_for_label(&mut h, "Open all");
    assert_eq!(h.state().repos[0].inspect.sha.as_deref(), Some(f.git(&["rev-parse", "HEAD"]).trim()));
}

#[test]
fn the_rail_keeps_a_few_views_and_the_more_list_doesnt_pile_up_tabs() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.query_by_label("Reflog").is_none());
    let before = tab_titles(&h).len();
    click(&mut h, "More");
    click(&mut h, "Reflog");
    wait_until_quiet(&mut h);
    assert!(tab_titles(&h).iter().any(|t| t == "Reflog"), "{:?}", tab_titles(&h));
    click(&mut h, "More");
    click(&mut h, "Health");
    wait_until_quiet(&mut h);
    let now = tab_titles(&h);
    assert!(now.iter().any(|t| t == "Health") && !now.iter().any(|t| t == "Reflog"), "{now:?}");
    assert_eq!(now.len(), before + 1);
    // All of them on the rail is a setting away.
    h.state_mut().settings.rail_full = true;
    h.step();
    h.step();
    assert!(h.query_by_label("Reflog").is_some());
}

#[test]
fn the_commit_menu_fits_a_short_window() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["tag", "v1"]);
    f.git(&["branch", "side"]);
    let mut h = sized_harness(&f, vec2(1000.0, 500.0), no_details());
    settle(&mut h);
    pump_for_label(&mut h, "second");
    h.get_by_label("second").click_secondary();
    wait_until_quiet(&mut h);
    for item in ["Show details", "Copy SHA", "Export ⏵"] {
        let r = h.get_by_label(item).rect();
        assert!(r.top() >= 0.0 && r.bottom() <= 500.0, "{item} at {r:?}");
    }
}

#[test]
fn wrapped_lines_take_the_rows_they_need() {
    use gitem::views::diffview::wrapped_lines;
    assert_eq!(wrapped_lines(0, 10), 1);
    assert_eq!(wrapped_lines(10, 10), 1);
    assert_eq!(wrapped_lines(11, 10), 2);
}

/// Each is counted against what the layout really does, with the app's real fonts.
#[test]
fn a_wrapped_row_is_as_tall_as_its_text_with_wide_characters() {
    use gitem::views::diffview::wrapped_line_count;
    let ctx = egui::Context::default();
    gitem::theme::install_fonts(&ctx);
    ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    let texts = ["plain ascii ".repeat(40), "中文宽字符测试的一行文字".repeat(12), "日本語の全角文字と한국어 mixed with ascii ".repeat(10), "é".repeat(300), "tab\tseparated\tcolumns\t".repeat(20), "🦀 emoji ".repeat(30)];
    let font = 13.0;
    let row_h = (font * 1.45_f32).ceil();
    for width in [180.0, 333.0, 700.0] {
        for text in &texts {
            let mut job = gitem::widgets::code_job(text, font, egui::Color32::WHITE, None, None, egui::Color32::TRANSPARENT);
            job.wrap.max_width = width;
            job.wrap.break_anywhere = true;
            let galley = ctx.fonts_mut(|fx| fx.layout_job(job));
            let n = wrapped_line_count(&ctx, text, width, font);
            assert!(n >= galley.rows.len(), "{text:?} at {width}: {n} rows for {}", galley.rows.len());
            // Too many is only a blank line. Tabs come out slightly high.
            assert!(n <= galley.rows.len() + galley.rows.len() / 8 + 1, "{text:?} at {width}: {n} rows for {}", galley.rows.len());
            assert!(galley.size().y <= n as f32 * row_h + 1.0, "{text:?} at {width}: {} tall in {n} rows of {row_h}", galley.size().y);
        }
    }
}

#[test]
fn wrap_works_in_both_diff_layouts_and_keeps_the_hunk_actions() {
    let f = Fx::new();
    f.write("long.txt", "short\n");
    f.commit("base");
    let long = "word ".repeat(400);
    f.write("long.txt", &format!("short\n{long}\nafter\n"));
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::diff(DiffSource::Unstaged, Some("long.txt".into())));
    pump_for_label(&mut h, "Wrap");
    click(&mut h, "Wrap");
    wait_until_quiet(&mut h);
    no_errors(&h);
    click(&mut h, "Unified");
    wait_until_quiet(&mut h);
    click(&mut h, "Stage hunk");
    wait_for(&mut h, "the hunk to be staged", || f.git(&["status", "--short"]).contains("M  long.txt"));
}

/// HEAD that git can't make sense of: everything that reads history fails.
fn break_head(f: &Fx) {
    std::fs::write(f.root.join(".git/HEAD"), "this is not a ref\n").unwrap();
}

#[test]
fn every_view_says_so_when_git_cant_read_the_repository() {
    let f = Fx::new();
    sample_history(&f);
    f.write("a.txt", "one\nTWO\nthree\nmore\n");
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    break_head(&f);
    h.state_mut().apply_action(&ctx, Action::Refresh);
    wait_until_quiet(&mut h);
    let mut silent = Vec::new();
    for tab in all_tabs() {
        let title = tab.title();
        if matches!(tab, Tab::Settings(_) | Tab::Credentials(_) | Tab::Console(_) | Tab::Agents(_)) {
            continue;
        }
        h.state_mut().repos[0].open_tab(tab);
        wait_until_quiet(&mut h);
        // The banner is enough for most, as it's there whichever view is in front. A view that
        // read something and found it wanting should say so too, and these two have had to.
        let boxes = error_boxes(&h);
        if matches!(title.as_str(), "Contributors" | "Reflog") && boxes.is_empty() {
            silent.push(title);
        }
    }
    assert!(silent.is_empty(), "showing no error: {silent:?}");
    assert!(h.query_by_label("Try again").is_some() || h.query_by_label_contains("can't read").is_some(), "the banner");
}

#[test]
fn a_folder_that_has_gone_can_be_closed_from_the_banner() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().canonicalize().unwrap().join("moved-away");
    std::fs::create_dir(&root).unwrap();
    let git = real_git();
    git.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    assert!(h.state_mut().open_repo(&ctx, &root, false));
    wait_until_quiet(&mut h);
    // Windows can refuse a folder rename while something watches inside it.
    if std::fs::rename(&root, parent.path().join("elsewhere")).is_err() {
        eprintln!("banner not shown: this system won't rename a watched folder");
        let open = h.state().repos[1].repo.root.clone();
        h.state_mut().apply_action(&ctx, Action::CloseRepo(open));
    } else {
        h.state_mut().apply_action(&ctx, Action::Refresh);
        pump_for_label(&mut h, "Locate…");
        assert_eq!(h.state().repos.len(), 2);
        click(&mut h, "Close");
    }
    wait_until_quiet(&mut h);
    assert_eq!(h.state().repos.len(), 1, "the tab went");
}

#[test]
fn opening_a_missing_folder_offers_to_make_a_repository_there() {
    let parent = tempfile::tempdir().unwrap();
    let wanted = parent.path().join("new-project");
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::OpenRepo(wanted.clone()));
    pump_for_label(&mut h, "Create repository here…");
    click(&mut h, "Create repository here…");
    pump_for_label(&mut h, "Initialize repository");
    wait_until_quiet(&mut h);
    click(&mut h, "Initialize");
    let t = Instant::now();
    while !wanted.join(".git").exists() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no repository was made");
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn a_folder_with_files_in_it_isnt_offered_that() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("work.txt"), "x").unwrap();
    let full = format!("{} isn't a git repository:\nnope", d.path().display());
    assert_eq!(gitem::app::folder_to_create(&full), None);
    std::fs::remove_file(d.path().join("work.txt")).unwrap();
    assert!(gitem::app::folder_to_create(&full).is_some());
    assert_eq!(gitem::app::folder_to_create("Pull failed:\nwhatever"), None);
}

#[test]
fn an_error_about_setup_leads_to_where_it_can_be_fixed() {
    use gitem::app::{ErrorFix, explain_error};
    assert_eq!(explain_error("No API key for OpenAI. Add one in Settings > AI.").map(|x| x.1), Some(ErrorFix::AiSettings));
    assert_eq!(explain_error("Author identity unknown\n\n*** Please tell me who you are.").map(|x| x.1), Some(ErrorFix::Identity));
}

#[test]
fn the_credentials_tab_doesnt_come_back_by_itself() {
    assert!(Tab::Credentials(Box::default()).spec().is_none());
    assert!(Tab::from_spec("credentials").is_none());
    assert!(Tab::from_spec("reflog").is_some(), "others do");
}

#[test]
fn an_amend_step_in_a_plan_can_be_changed_like_any_other() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::rebase(Some("HEAD~1".into())));
    pump_for_label(&mut h, "Rebase 1 commit");
    wait_until_quiet(&mut h);
    for (_, t) in h.state_mut().repos[0].dock.iter_all_tabs_mut() {
        if let Tab::Rebase(r) = t {
            r.items[0] = gitem_core::rebase::TodoItem { action: gitem_core::rebase::RebaseAction::Other, sha: String::new(), text: "fixup -C abc1234 amend! one".to_string() };
        }
    }
    wait_until_quiet(&mut h);
    h.query(egui_kittest::kittest::by().role(egui::accesskit::Role::ComboBox).value("amend")).expect("an action box").click();
    h.step();
    click(&mut h, "drop");
    wait_until_quiet(&mut h);
    let item = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Rebase(r) = t { Some(r.items[0].clone()) } else { None }).unwrap();
    assert_eq!(item, gitem_core::rebase::TodoItem { action: gitem_core::rebase::RebaseAction::Drop, sha: "abc1234".to_string(), text: "amend! one".to_string() });
}

#[test]
fn config_marks_a_value_something_else_overrides() {
    let f = Fx::new();
    sample_history(&f);
    // The test config has this at global level, and a local value goes on top.
    f.git(&["config", "pull.rebase", "true"]);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Config(Box::default()));
    pump_for_label(&mut h, "overridden");
    // Editing is on the row under the pointer.
    assert!(h.query_by_label("Edit").is_none());
    let row = h.get_by_label("user.email").rect().center();
    h.event(egui::Event::PointerMoved(row));
    h.step();
    h.step();
    pump_for_label(&mut h, "Edit");
}

#[test]
fn a_branch_folder_folds_away() {
    let f = Fx::new();
    sample_history(&f);
    for b in ["team/one", "team/two", "team/three"] {
        f.git(&["branch", b]);
    }
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "team/one");
    let chip = format!("{} team/ 3", gitem::chrome::icon::CARET_DOWN);
    click(&mut h, &chip);
    wait_until_quiet(&mut h);
    assert!(h.query_by_label("team/one").is_none());
    assert!(h.query_by_label("main").is_some(), "only that folder");
}

#[test]
fn the_toolbars_branch_menu_filters_and_lists_where_youve_been() {
    let f = Fx::new();
    sample_history(&f);
    f.git(&["branch", "alpha"]);
    f.git(&["branch", "beta"]);
    let mut h = harness(&f);
    settle(&mut h);
    let menu = format!("{} main  {}", gitem::chrome::icon::GIT_BRANCH, gitem::chrome::icon::CARET_DOWN);
    wait_until_quiet(&mut h);
    h.query_all_by_label(&menu).next().unwrap().click();
    h.step();
    wait_until_quiet(&mut h);
    // Each is also in the sidebar, behind this.
    assert_eq!(h.query_all_by_label("beta").count(), 2);
    h.event(egui::Event::Text("alp".into()));
    h.step();
    h.step();
    assert_eq!(h.query_all_by_label("alpha").count(), 2);
    assert_eq!(h.query_all_by_label("beta").count(), 1);
}

#[test]
fn a_commit_says_what_it_made_and_can_be_taken_back_from_the_toolbar() {
    let f = Fx::new();
    two_changed_files(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "from the box");
    click(&mut h, "Stage all and commit 2 files");
    wait_for(&mut h, "the commit", || f.subjects()[0] == "from the box");
    wait_until_quiet(&mut h);
    let short = f.git(&["rev-parse", "--short", "HEAD"]);
    let name = format!("Undo commit {}", short.trim());
    pump_for_label(&mut h, &name);
    click(&mut h, &name);
    pump_for_label(&mut h, "Undo commit");
    click(&mut h, "Undo commit");
    wait_for(&mut h, "the undo", || f.subjects()[0] != "from the box");
}

#[test]
fn a_reflog_entry_has_its_restore_on_the_row() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    open_tab_now(&mut h, Tab::Reflog(Box::default()));
    wait_until_quiet(&mut h);
    h.query_all_by_label("HEAD@{0}").next().expect("the latest entry").hover();
    h.step();
    h.step();
    click(&mut h, "Restore: reset the current branch to here");
    wait_until_quiet(&mut h);
    assert!(h.query_all_by_label_contains("Reset").next().is_some(), "the form");
    assert!(h.state().ops_idle(), "it asks how, first");
}

#[test]
fn a_view_with_nothing_to_act_on_keeps_its_buttons_quiet() {
    let f = Fx::new();
    sample_history(&f);
    let mut h = harness(&f);
    settle(&mut h);
    // On the base branch, there's nothing to review.
    open_tab_now(&mut h, Tab::Review(Box::default()));
    pump_for_label(&mut h, "No changes to review");
    assert!(h.get_by_label("Review with AI").accesskit_node().is_disabled());
    // With no rebase base picked, nothing to rebase.
    open_tab_now(&mut h, Tab::rebase(None));
    pump_for_label(&mut h, "Squash all into first");
    assert!(h.get_by_label("Squash all into first").accesskit_node().is_disabled());
    // With no host, no pull request can be made.
    open_tab_now(&mut h, Tab::Launchpad(Box::default()));
    pump_for_label(&mut h, "Create pull request");
    assert!(h.query_all_by_label("Refresh").any(|n| n.accesskit_node().is_disabled()));
}

// ---- Scan for repositories, dropping a folder (UX-38), switching with changes in the way and the
// branch tree (UX-27), arrow keys (UX-8). Dropped files and keys are kittest's synthetic input.

fn welcome_harness<'a>(f: &Fx) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings { details_on_open: false, ..Default::default() });
    app.git = Ok(f.git.clone());
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    builder.build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app)
}

/// A scratch folder with repositories in it, and a repository's folder inside a repository,
/// which a scan shouldn't go looking in.
fn scan_tree(f: &Fx) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().canonicalize().unwrap();
    for name in ["one", "deep/er/two", "skip/node_modules/three"] {
        let d = top.join(name);
        std::fs::create_dir_all(&d).unwrap();
        f.git.run(Some(&d), &["init", "-q"]).unwrap();
    }
    std::fs::create_dir_all(top.join("one/inner")).unwrap();
    f.git.run(Some(&top.join("one/inner")), &["init", "-q"]).unwrap();
    std::fs::create_dir_all(top.join("not-a-repo")).unwrap();
    dir
}

#[test]
fn scanning_a_folder_offers_its_repositories_and_opens_the_ones_ticked() {
    let f = Fx::new();
    let dir = scan_tree(&f);
    let top = dir.path().canonicalize().unwrap();
    let mut h = welcome_harness(&f);
    click(&mut h, "Scan for repositories…");
    pump_for_label(&mut h, "Scan");
    let ctx = h.ctx.clone();
    // The form's Browse is a real picker, so this goes in by the action its Scan makes.
    click(&mut h, "Cancel");
    h.state_mut().apply_action(&ctx, Action::ScanRepos(top.clone()));
    pump_for_label(&mut h, "Open 2 repositories");
    assert!(h.query_by_label("one").is_some() && h.query_by_label("deep/er/two").is_some());
    assert!(h.query_by_label_contains("three").is_none(), "not in node_modules");
    assert!(h.query_by_label_contains("inner").is_none(), "not inside a repository");
    click(&mut h, "one");
    pump_for_label(&mut h, "Open 1 repository");
    click(&mut h, "Open 1 repository");
    pump(&mut h, |a| a.repos.len() == 1, "the repository to open");
    assert!(h.state().repos[0].repo.root.ends_with("two"), "{:?}", h.state().repos[0].repo.root);
}

#[test]
fn a_folder_with_no_repositories_says_so_and_closes() {
    let f = Fx::new();
    let dir = scan_tree(&f);
    let top = dir.path().canonicalize().unwrap().join("not-a-repo");
    let mut h = welcome_harness(&f);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::ScanRepos(top.clone()));
    pump_for_label(&mut h, "No repositories in this folder.");
    click(&mut h, "Close");
    assert!(h.query_by_label("No repositories in this folder.").is_none());
    assert!(h.state().repos.is_empty());
}

fn drop_on_window(h: &mut Harness<GitemApp>, path: &Path) {
    #[derive(Debug)]
    struct Dropped(PathBuf);
    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("not read".into())
        }
    }
    h.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(path.to_path_buf())));
    h.step();
    h.step();
}

#[test]
fn dropping_a_folder_or_a_file_on_the_window_opens_its_repository() {
    let f = Fx::new();
    f.write("sub/file.txt", "x\n");
    f.commit("first");
    let mut h = welcome_harness(&f);
    drop_on_window(&mut h, &f.root);
    pump(&mut h, |a| a.repos.len() == 1, "a dropped folder to open");
    assert_eq!(h.state().repos[0].repo.root.canonicalize().unwrap(), f.root);
    let second = Fx::new();
    second.write("sub/file.txt", "x\n");
    second.commit("first");
    drop_on_window(&mut h, &second.root.join("sub/file.txt"));
    pump(&mut h, |a| a.repos.len() == 2, "a dropped file's repository to open");
    assert_eq!(h.state().repos[1].repo.root.canonicalize().unwrap(), second.root);
}

/// Two branches whose versions of `a.txt` differ, and an edit to it.
fn changes_in_the_way(f: &Fx) {
    f.write("a.txt", "one\n");
    f.commit("first");
    f.git(&["branch", "other"]);
    f.write("a.txt", "two\n");
    f.commit("second");
    f.write("a.txt", "my edit\n");
}

fn switch_to_other(h: &mut Harness<GitemApp>) {
    settle(h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::checkout("other")));
}

#[test]
fn a_switch_with_changes_in_the_way_offers_a_stash() {
    let f = Fx::new();
    changes_in_the_way(&f);
    let mut h = harness(&f);
    switch_to_other(&mut h);
    pump_for_label(&mut h, "Changes are in the way of other");
    assert_eq!(f.git(&["branch", "--show-current"]).trim(), "main", "nothing's happened yet");
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    click(&mut h, "Switch");
    wait_for(&mut h, "the switch", || f.git(&["branch", "--show-current"]).trim() == "other");
    wait_until_quiet(&mut h);
    no_errors(&h);
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "one\n");
    assert!(f.git(&["stash", "list"]).contains("Before switching to other"));
}

#[test]
fn a_switch_with_changes_in_the_way_can_bring_them_along_or_be_cancelled() {
    let f = Fx::new();
    changes_in_the_way(&f);
    let mut h = harness(&f);
    switch_to_other(&mut h);
    pump_for_label(&mut h, "Changes are in the way of other");
    click(&mut h, "Cancel");
    wait_until_quiet(&mut h);
    assert_eq!(f.git(&["branch", "--show-current"]).trim(), "main");
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "my edit\n");
    // The same again straight away would count as a double click.
    std::thread::sleep(Duration::from_millis(700));
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Run(ops::checkout("other")));
    pump_for_label(&mut h, "Changes are in the way of other");
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    h.step();
    h.step();
    click(&mut h, "Bring the changes along, merging them. This can leave conflicts");
    click(&mut h, "Switch");
    wait_for(&mut h, "the switch", || f.git(&["branch", "--show-current"]).trim() == "other");
    wait_until_quiet(&mut h);
    assert!(std::fs::read_to_string(f.root.join("a.txt")).unwrap().contains("my edit"), "not lost");
    // Those two versions conflict, and then git also keeps a copy in the stash.
    assert!(f.git(&["status"]).contains("both modified"));
}

#[test]
fn branch_folders_open_and_close_in_the_sidebar() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("first");
    for b in ["team/alpha/one", "team/alpha/two", "team/beta", "solo/lone"] {
        f.git(&["branch", b]);
    }
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Close folder team");
    assert!(h.query_by_label("one").is_some() && h.query_by_label("two").is_some() && h.query_by_label("beta").is_some());
    assert!(h.query_by_label("solo/lone").is_some(), "one branch makes no folder");
    click(&mut h, "Close folder team/alpha");
    pump_for_label(&mut h, "Open folder team/alpha");
    assert!(h.query_by_label("one").is_none());
    assert!(h.query_by_label("beta").is_some(), "only that folder");
    click(&mut h, "Close folder team");
    pump_for_label(&mut h, "Open folder team");
    assert!(h.query_by_label("beta").is_none());
    click(&mut h, "Open folder team");
    pump_for_label(&mut h, "beta");
    assert!(h.query_by_label("one").is_none(), "a folder inside stays as it was");
}

/// The rows further down aren't drawn until they're near the view, so focus has to bring them.
#[test]
fn arrow_keys_go_down_a_sidebar_list_longer_than_the_window() {
    use egui_kittest::kittest::NodeT;
    let f = Fx::new();
    f.write("a.txt", "one
");
    f.commit("first");
    for i in 0..60 {
        f.git(&["branch", &format!("b{i:02}")]);
    }
    let mut h = sized_harness(&f, vec2(1100.0, 500.0), no_details());
    settle(&mut h);
    pump_for_label(&mut h, "b00");
    let focused = |h: &Harness<GitemApp>| h.query_all_by_label_contains("").find(|n| n.accesskit_node().is_focused()).map(|n| n.accesskit_node().label().unwrap_or_default());
    h.get_by_label("b00").focus();
    h.step();
    h.step();
    assert_eq!(focused(&h).as_deref(), Some("b00"));
    for i in 0..40 {
        h.key_press(egui::Key::ArrowDown);
        h.step();
        h.step();
        assert_eq!(focused(&h).unwrap_or_default(), format!("b{:02}", i + 1));
    }
    let now = focused(&h).unwrap_or_default();
    assert_eq!(now, "b40");
    let r = h.get_by_label("b40").rect();
    assert!(r.bottom() <= 500.0, "off the window at {r:?}");
}

#[test]
fn right_and_left_open_and_close_a_focused_row() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("first");
    for b in ["aaa", "team/one", "team/two"] {
        f.git(&["branch", b]);
    }
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "Close folder team");
    h.get_by_label("Close folder team").focus();
    h.step();
    h.step();
    h.key_press(egui::Key::ArrowLeft);
    pump_for_label(&mut h, "Open folder team");
    assert!(h.query_by_label("one").is_none());
    h.key_press(egui::Key::ArrowRight);
    pump_for_label(&mut h, "one");
    h.get_by_label("aaa").focus();
    h.step();
    h.step();
    h.key_press(egui::Key::ArrowRight);
    pump_for_label(&mut h, "Collapse aaa");
    h.key_press(egui::Key::ArrowLeft);
    pump_for_label(&mut h, "Expand aaa");
}

#[test]
fn arrow_keys_move_through_the_rows_of_the_sidebar() {
    use egui_kittest::kittest::NodeT;
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("first");
    for b in ["aaa", "bbb", "ccc"] {
        f.git(&["branch", b]);
    }
    let mut h = harness(&f);
    settle(&mut h);
    pump_for_label(&mut h, "aaa");
    let mut order: Vec<(f32, String)> = ["aaa", "bbb", "ccc", "main"].iter().map(|n| (h.get_by_label(n).rect().top(), n.to_string())).collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    let order: Vec<String> = order.into_iter().map(|(_, n)| n).collect();
    let focused = |h: &Harness<GitemApp>| h.query_all_by_label_contains("").find(|n| n.accesskit_node().is_focused()).map(|n| n.accesskit_node().label().unwrap_or_default());
    h.get_by_label(&order[0]).focus();
    h.step();
    h.step();
    assert_eq!(focused(&h).as_deref(), Some(order[0].as_str()));
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.step();
    assert_eq!(focused(&h).as_deref(), Some(order[1].as_str()), "down");
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.step();
    assert_eq!(focused(&h).as_deref(), Some(order[2].as_str()), "down again");
    h.key_press(egui::Key::ArrowUp);
    h.step();
    h.step();
    assert_eq!(focused(&h).as_deref(), Some(order[1].as_str()), "up");
}
