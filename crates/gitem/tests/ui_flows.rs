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
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings::default());
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
    h.get_by_label(label).click();
    h.step();
    h.step();
}

fn pump_for_label(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_by_label(label).is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn no_errors(h: &Harness<GitemApp>) {
    assert!(h.query_by_label("Something went wrong").is_none(), "an error dialog is showing");
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
        assert!(h.query_by_label("Something went wrong").is_none(), "error showing after opening {title}");
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
    for tab in [Tab::home(), Tab::graph(), Tab::changes(), Tab::Worktrees(Box::default()), Tab::Contributors(Box::default()), Tab::Reflog(Box::default()), Tab::Composer(Box::default()), Tab::Console(Box::default())] {
        h.state_mut().repos[0].open_tab(tab);
        wait_until_quiet(&mut h);
        no_errors(&h);
    }
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
    click(&mut h, "Stage all and commit 1 file(s)");
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
    make_conflict(&f);
    f.write("a.txt", "x\n");
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::merge("c.txt"));
    pump_for_label(&mut h, "Take ours");
    h.state_mut().repos[0].open_tab(Tab::changes());
    pump_for_label(&mut h, "Commit to main");
    h.state_mut().repos[0].open_tab(Tab::graph());
    wait_until_quiet(&mut h);
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
    pump_for_label(&mut h, "Rebase 3 commit(s)");
    // Edit the plan directly: drop "two", which the view would do via its combo box.
    for (_, t) in h.state_mut().repos[0].dock.iter_all_tabs_mut() {
        if let Tab::Rebase(r) = t {
            r.items[1].action = gitem_core::rebase::RebaseAction::Drop;
        }
    }
    h.step();
    click(&mut h, "Rebase 3 commit(s)");
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
    pump_for_label(&mut h, "Something went wrong");
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
    pump_for_label(&mut h, "Something went wrong");
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


/// Not part of the normal run: `cargo test --release -- --ignored large_history`.
#[test]
#[ignore]
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
    settle(&mut h);
    let loaded = |a: &GitemApp| a.repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Graph(g) = t { Some(g.loaded_commits()) } else { None }).unwrap_or(0);
    let load_start = Instant::now();
    pump(&mut h, |a| loaded(a) == n as usize, "the commits to load");
    eprintln!("loaded {} commits in {:.2} s", loaded(h.state()), load_start.elapsed().as_secs_f32());
    let t = Instant::now();
    for _ in 0..120 {
        h.step();
    }
    let per_frame = t.elapsed().as_secs_f32() * 1000.0 / 120.0;
    eprintln!("graph with {n} commits: {per_frame:.1} ms per frame");
    assert!(per_frame < 40.0, "{per_frame} ms per frame");
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
    pump_for_label(&mut h, "Keep theirs");
    assert!(h.query_by_label("Save and mark resolved").is_none(), "no text merge is offered");
    click(&mut h, "Keep theirs");
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
    pump_for_label(&mut h, "✔ This file is no longer in conflict");
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
    pump_for_label(&mut h, "Rebase 1 commit(s)");
    // A new commit lands while the plan is open; the plan doesn't include it.
    f.write("y", "1\n");
    f.commit("two");
    for _ in 0..5 {
        h.step();
    }
    click(&mut h, "Rebase 1 commit(s)");
    pump_for_label(&mut h, "HEAD moved since this plan was made, so it was not started.");
    assert_eq!(f.subjects(), vec!["two", "one", "base"], "no commit lost");
    click(&mut h, "Rebuild plan");
    pump_for_label(&mut h, "Rebase 2 commit(s)");
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
    click(&mut h, "Stage all and commit 1 file(s)");
    pump_for_label(&mut h, "Something went wrong");
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
    pump_for_label(&mut h, "Switch to");
    click(&mut h, "Switch to");
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
    click(&mut h, "Drop…");
    pump_for_label(&mut h, "Rebase 2 commit(s)");
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
    click(&mut h, "Stage 2 file(s)");
    wait_for(&mut h, "folder staged", || f.git(&["diff", "--cached", "--name-only"]).lines().count() == 2);
    let staged = f.git(&["diff", "--cached", "--name-only"]);
    assert!(staged.contains("src/main.rs") && staged.contains("src/views/one.rs") && !staged.contains("top.txt"), "{staged}");

    // The same folder now shows under Staged, once the status has caught up.
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.entries.iter().filter(|e| e.is_staged()).count() == 2), "status to show the staged folder");
    pump_for_label(&mut h, "src");
    h.get_by_label("src").click_secondary();
    h.step();
    h.step();
    click(&mut h, "Unstage 2 file(s)");
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
    click(&mut h, "Stage 1 file(s)");
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
    let root = f.root.to_string_lossy().replace('\\', "/");
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
        self.f.git.run(Some(&self.bare), &["rev-parse", "main"]).unwrap().text().trim().to_string()
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
    pump_for_label(&mut h, "Something went wrong");
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
    pump_for_label(&mut h, "Something went wrong");
    assert!(r.f.git(&["status", "--porcelain"]).contains("AA conflict.txt"));
    click(&mut h, "Dismiss");
    click(&mut h, "Resolve");
    pump_for_label(&mut h, "Take theirs");
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
    pump_for_label(&mut h, "Something went wrong");
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
    click(&mut h, "About");
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
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings::default());
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
    pump_for_label(&mut h, "520 commit(s)");
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
fn ctrl_enter_in_the_commit_box_commits_once_and_leaves_an_open_form_alone() {
    let f = Fx::new();
    staged_and_unstaged(&f);
    let mut h = harness(&f);
    settle(&mut h);
    changes_with_message(&mut h, "from the box");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Form(forms::stash_form(Vec::new())));
    h.step();
    h.step();
    assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "this test is about focus staying in the box");
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Enter);
    wait_for(&mut h, "the commit", || f.subjects()[0] == "from the box");
    wait_until_quiet(&mut h);
    assert_eq!(f.git(&["stash", "list"]), "", "the form must not fire too");
    assert!(h.query_by_label("Stash all changes").is_some(), "the form stays open");
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
    h.query_all_by_label("Add ▾").next().unwrap().click();
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Commit 1").click();
    h.step();
    h.step();
    h.step();
    pump_for_label(&mut h, "Create 1 commit(s)");
    h.get_by_label("Create 1 commit(s)").click();
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
    click(&mut h, "Co-authors ▾");
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
        pump_for_label(&mut h, "Stage 1 selected line(s)");
        let stage = h.get_by_label("Stage 1 selected line(s)").rect();
        let discard = h.get_by_label("Discard selected…").rect();
        let details_left = h.query_all_by_label("Details").map(|n| n.rect().left()).fold(width, f32::min);
        assert!(stage.right() <= details_left && discard.right() <= details_left, "{mode} at {width}: buttons run under the Details panel at {details_left}: stage {stage:?}, discard {discard:?}");
        click(&mut h, "Stage 1 selected line(s)");
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
