//! Headless end-to-end tests: drive the real UI by clicking widgets, then verify
//! the outcome with git itself. Temp repositories only; remotes are local bare repos.
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
        assert!(t.elapsed() < Duration::from_secs(25), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Step until all background reads have had a chance to land.
fn settle(h: &mut Harness<GitemApp>) {
    pump(h, |a| a.repos.first().is_some_and(|r| r.data.status.get().is_some() && r.data.refs.get().is_some()), "repo data");
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(500) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
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
        assert!(t.elapsed() < Duration::from_secs(25), "no widget labelled {label:?}");
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
        for _ in 0..10 {
            h.step();
            std::thread::sleep(Duration::from_millis(12));
        }
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
        for _ in 0..8 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
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
        assert!(t0.elapsed() < Duration::from_secs(20), "commit never landed: {:?}", f.subjects());
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(f.git(&["status", "--short"]), "");
    no_errors(&h);
}

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn() -> bool) {
    let t = Instant::now();
    while !cond() {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(20), "timed out: {what}");
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
        assert!(t.elapsed() < Duration::from_secs(15));
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
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
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
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(f.git(&["branch"]).contains("doomed"), "nothing happens before confirming");
    click(&mut h, "Cancel");
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
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
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
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
        assert!(t.elapsed() < Duration::from_secs(20), "tab did not reload for the new conflict");
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
        assert!(t.elapsed() < Duration::from_secs(15));
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
        for _ in 0..12 {
            h.step();
            std::thread::sleep(Duration::from_millis(12));
        }
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
    pump_for_label(&mut h, "Commit date");
    pump_for_label(&mut h, env!("CARGO_PKG_VERSION"));
    // New windows spend their first frames measuring themselves.
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "Close");
    assert!(h.query_by_label("Commit date").is_none(), "the About box stays open");
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
        assert!(t.elapsed() < Duration::from_secs(15), "still shown as incoming");
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
        assert!(t.elapsed() < Duration::from_secs(5), "the section didn't close");
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
        assert!(t.elapsed() < Duration::from_secs(10), "the script didn't run");
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
