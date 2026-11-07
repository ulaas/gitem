//! A pull on a branch that has diverged from its remote, with nothing configured about how to
//! join them. The other test binaries set `pull.rebase` for everyone, and without it git
//! refuses, so this one takes it out again: on its own, as the setting is process-wide. Real
//! git and the real UI, with a local bare repository for the remote.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem_core::{GitExe, Repo, ops};

const PATIENCE: Duration = Duration::from_secs(180);

struct Fx {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn commit(&self, name: &str, msg: &str) {
        std::fs::write(self.root.join(name), msg).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }
}

fn config_for_real_users() {
    // What a new user has: no `pull.rebase` anywhere.
    let path = std::env::var_os("GIT_CONFIG_GLOBAL").expect("isolated");
    std::fs::write(path, "").unwrap();
}

/// A clone whose remote has a commit it hasn't got, and which has one of its own.
fn diverged() -> Fx {
    config_for_real_users();
    let (bare, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut git = GitExe::discover(None).unwrap();
    git.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
    let url = bare.path().to_string_lossy().replace('\\', "/");
    git.run(Some(bare.path()), &["init", "-q", "--bare", "-b", "main"]).unwrap();
    let root = work.path().canonicalize().unwrap();
    git.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
    let f = Fx { _dirs: (bare, work), root, git };
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        f.git(&["config", k, v]);
    }
    f.git(&["remote", "add", "origin", &url]);
    f.commit("base.txt", "base");
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.commit("theirs.txt", "theirs");
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.commit("mine.txt", "mine");
    f.git(&["fetch", "-q", "origin"]);
    assert!(f.git(&["status", "-sb"]).contains("ahead 1, behind 1"), "{}", f.git(&["status", "-sb"]));
    f
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

fn step_until(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// A new window spends its first frames measuring itself, when a click can't land.
fn click_when_settled(h: &mut Harness<GitemApp>, label: &str) {
    step_until(h, label, |h| h.query_by_label(label).is_some());
    for _ in 0..5 {
        h.step();
    }
    h.get_by_label(label).click();
    h.step();
    h.step();
}

/// Plain git refuses this. The toolbar's Pull is what a real user presses, and it has no
/// mode of its own unless Settings says so.
fn pull_fails_and_offers_the_choice(f: &Fx, h: &mut Harness<GitemApp>) {
    assert!(!f.git.run_raw(Some(&f.root), &["pull", "--dry-run"]).is_err());
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::Run(ops::pull(ops::PullMode::Default, false)));
    step_until(h, "the failure to be explained", |h| h.query_by_label_contains("both have new commits").is_some());
    assert!(h.state().errors_showing()[0].starts_with("Pull"));
    assert!(h.query_by_label("Pull options…").is_none(), "that form's first choice is the very thing that just failed");
    click_when_settled(h, "Merge or rebase…");
    step_until(h, "the form", |h| h.query_by_label_contains("How to bring them together").is_some());
}

#[test]
fn a_diverged_pull_with_nothing_configured_offers_merge_or_rebase() {
    let f = diverged();
    let mut h = harness(&f);
    step_until(&mut h, "the repository to open", |h| h.state().repos.first().is_some_and(|r| r.data.status.get().is_some()));
    pull_fails_and_offers_the_choice(&f, &mut h);

    // Merge is what comes selected, as it rewrites nothing. The form's button is the last of
    // the two called Pull, after the toolbar's.
    for _ in 0..5 {
        h.step();
    }
    h.query_all_by_label("Pull").last().expect("the form's button").click();
    h.step();
    h.step();
    let t = Instant::now();
    while !f.root.join("theirs.txt").exists() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the merge never happened");
        std::thread::sleep(Duration::from_millis(15));
    }
    step_until(&mut h, "the op to finish", |h| h.state().ops_idle());
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    let parents = f.git(&["log", "-1", "--format=%P"]);
    assert_eq!(parents.split_whitespace().count(), 2, "a merge commit");
    assert!(f.git.run_raw(Some(&f.root), &["config", "--local", "pull.rebase"]).unwrap().stdout.is_empty(), "remembering wasn't asked for");

    // The rest of the form, against real git: rebase and remembering it.
    let g = diverged();
    let repo = Repo::open(g.git.clone(), &g.root).unwrap();
    let mut form = gitem::forms::pull_reconcile_form();
    form.set_for_test("mode", "Rebase: replay your commits on top of the remote's");
    form.set_for_test("remember", "1");
    for action in form.submit_for_test().unwrap() {
        let gitem::action::Action::Run(op) = action else { panic!("only ops expected") };
        op.run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    }
    assert_eq!(g.git(&["log", "--format=%s", "-2"]).lines().collect::<Vec<_>>(), vec!["mine", "theirs"], "replayed on top");
    assert_eq!(g.git(&["log", "-1", "--format=%P"]).split_whitespace().count(), 1);
    assert_eq!(g.git(&["config", "--local", "pull.rebase"]).trim(), "true");
}
