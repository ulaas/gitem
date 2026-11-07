//! Everyday features driven through the real UI against real git in throwaway repositories: undo,
//! branch cleanup, and what's added beside them. Clicks and keys are kittest's synthetic input.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::settings::Settings;

const PATIENCE: Duration = Duration::from_secs(180);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir(&root).unwrap();
        let mut git = GitExe::discover(None).expect("git");
        git.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write("a.txt", "one\n");
        f.commit("first");
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
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }
    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }
    fn harness(&self, tweak: impl FnOnce(&mut Settings)) -> Harness<'static, GitemApp> {
        let ctx = egui::Context::default();
        let mut settings = Settings { details_on_open: false, ..Default::default() };
        tweak(&mut settings);
        let mut app = GitemApp::with_settings(&ctx, settings);
        app.git = Ok(self.git.clone());
        let root = self.root.clone();
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
        h
    }
}

fn settle(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut frames = 0;
    while frames < 5 {
        h.step();
        let loaded = h.state().repos.first().is_none_or(|r| r.data.status.get().is_some() && r.data.refs.get().is_some());
        frames = if loaded && gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() { frames + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_label(h: &mut Harness<GitemApp>, label: &str) {
    wait_for(h, label, |h| h.query_by_label(label).is_some());
}

fn wait_for_text(h: &mut Harness<GitemApp>, text: &str) {
    wait_for(h, text, |h| h.query_by_label_contains(text).is_some());
}

fn click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label(h, label);
    // A dialog takes no clicks for a moment after it appears. A person never notices.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.get_by_label(label).click();
    h.step();
    h.step();
}

fn open(h: &mut Harness<GitemApp>, tab: Tab) {
    h.state_mut().repos[0].open_tab(tab);
    h.step();
    settle(h);
}

#[test]
fn a_hard_reset_is_taken_back_from_the_toolbar() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let tip = f.head();
    f.write("a.txt", "not committed\n");
    let mut h = f.harness(|_| {});
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::reset("HEAD~1", gitem_core::ops::ResetMode::Hard));
    settle(&mut h);
    assert_eq!(f.read("a.txt"), "one\n");
    click(&mut h, "Undo reset");
    wait_for_text(&mut h, "uncommitted changes come back");
    let t = Instant::now();
    while h.query_all_by_label("Undo reset").count() < 2 {
        h.step();
        assert!(t.elapsed() < PATIENCE);
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.query_all_by_label("Undo reset").last().unwrap().click();
    h.step();
    settle(&mut h);
    assert_eq!(f.head(), tip);
    assert_eq!(f.read("a.txt"), "not committed\n");
    assert!(h.query_by_label("Redo reset").is_some(), "and it can be redone");
}

#[test]
fn a_new_operation_replaces_what_can_be_undone() {
    let f = Fx::new();
    f.git(&["branch", "gone"]);
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("gone", false));
    settle(&mut h);
    wait_for_label(&mut h, "Undo delete of branch gone");
    f.write("s.txt", "s\n");
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::stage_all());
    settle(&mut h);
    assert!(h.query_by_label("Undo delete of branch gone").is_none());
}

#[test]
fn a_deleted_branch_comes_back_with_confirmations_off() {
    let f = Fx::new();
    f.git(&["branch", "gone"]);
    let tip = f.head();
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("gone", false));
    settle(&mut h);
    click(&mut h, "Undo delete of branch gone");
    settle(&mut h);
    assert_eq!(f.git(&["rev-parse", "refs/heads/gone"]).trim(), tip);
}

#[test]
fn undo_says_why_when_it_can_no_longer_be_done() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::reset("HEAD~1", gitem_core::ops::ResetMode::Keep));
    settle(&mut h);
    f.write("b.txt", "b\n");
    f.commit("made outside gitem");
    let there = f.head();
    settle(&mut h);
    click(&mut h, "Undo reset");
    settle(&mut h);
    wait_for(&mut h, "the refusal", |h| h.state().errors_showing().iter().any(|e| e.contains("moved")));
    assert_eq!(f.head(), there);
}

#[test]
fn a_remote_branch_whose_name_is_taken_goes_to_the_local_one() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["switch", "-q", "-c", "other"]);
    let mut h = f.harness(|_| {});
    let op = h.state().repos[0].data.check_out_remote("origin/main");
    assert_eq!(op.steps[0].args, ["switch", "main"], "not a second branch called main");
    h.state_mut().repos[0].queue_for_test(op);
    settle(&mut h);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]).trim(), "main");
}

#[test]
fn fast_forward_moves_a_branch_that_is_not_checked_out() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.git(&["switch", "-q", "-c", "side"]);
    f.git(&["push", "-q", "origin", "side:main"]);
    f.write("b.txt", "b\n");
    f.commit("ahead on side");
    f.git(&["push", "-q", "origin", "side:main"]);
    let tip = f.head();
    f.git(&["fetch", "-q", "origin"]);
    let mut h = f.harness(|_| {});
    let remotes = h.state().repos[0].data.remote_names();
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::fast_forward_branch("main", "origin/main", &remotes));
    settle(&mut h);
    assert_eq!(f.git(&["rev-parse", "refs/heads/main"]).trim(), tip);
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]).trim(), "side", "and nothing else moved");
}

#[test]
fn cleaning_up_deletes_whats_ticked_and_can_be_undone() {
    let f = Fx::new();
    f.git(&["branch", "merged-one"]);
    f.git(&["branch", "merged-two"]);
    f.git(&["switch", "-q", "-c", "keep-me"]);
    f.write("k.txt", "k\n");
    f.commit("not merged");
    f.git(&["switch", "-q", "main"]);
    let mut h = f.harness(|s| s.confirm_destructive = false);
    open(&mut h, Tab::Prune(Box::default()));
    wait_for(&mut h, "the list", |h| h.query_by_role_and_label(Role::CheckBox, "merged-one").is_some());
    assert!(h.query_by_role_and_label(Role::CheckBox, "keep-me").is_none(), "work in progress isn't offered");
    h.get_by_role_and_label(Role::CheckBox, "merged-two").click();
    h.step();
    h.step();
    click(&mut h, "Delete 1 branch");
    settle(&mut h);
    assert_eq!(f.git(&["branch", "--list", "merged-one"]), "");
    assert_ne!(f.git(&["branch", "--list", "merged-two"]), "", "it was unticked");
    click(&mut h, "Undo delete of branch merged-one");
    settle(&mut h);
    assert_ne!(f.git(&["branch", "--list", "merged-one"]), "");
}

#[test]
fn the_toolbars_pull_stashes_when_the_setting_says_to() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.write("a.txt", "two\n");
    f.commit("second");
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.write("a.txt", "mine\n");
    f.git(&["fetch", "-q", "origin"]);
    // In the way of what's coming, so without the stash the pull is refused.
    let mut h = f.harness(|s| {
        s.pull_autostash = true;
        s.pull_mode = "merge".to_string();
    });
    click(&mut h, "Pull 1");
    settle(&mut h);
    let log = f.git(&["log", "--format=%s", "-1"]);
    assert_eq!(log.trim(), "second", "{:?}", h.state().errors_showing());
    assert_ne!(f.git(&["status", "--porcelain"]), "", "its changes came back, in conflict or merged");
}

#[test]
fn undo_commit_can_be_redone() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let tip = f.head();
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::undo_commit());
    settle(&mut h);
    assert_ne!(f.head(), tip);
    click(&mut h, "Redo commit");
    settle(&mut h);
    assert_eq!(f.head(), tip);
}
