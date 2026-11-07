//! Tests that change the global git config. They have their own binary and take turns: on
//! Windows, a git reading that file while another git replaces it fails with "Permission
//! denied", which made unrelated tests beside them fail about one run in forty.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use gitem_core::ops::{self, GitOp};
use gitem_core::{GitExe, Repo};

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git");
    g.helper = None;
    g
}

fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: Repo,
}

impl Fx {
    /// A repository with one commit.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let g = git();
        g.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            g.run(Some(&root), &["config", k, v]).unwrap();
        }
        let repo = Repo::open(g, &root).unwrap();
        let f = Self { _dir: dir, root, repo };
        std::fs::write(f.root.join("a"), "first").unwrap();
        f.q(&["add", "-A"]);
        f.q(&["commit", "-qm", "first"]);
        f
    }
    fn q(&self, args: &[&str]) -> String {
        git().run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim_end().to_string()
    }
    fn run(&self, op: GitOp) {
        op.run(&git(), Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
}

#[test]
fn scheduled_maintenance_is_on_only_when_git_has_this_repository_listed() {
    let _turn = turn();
    let f = Fx::new();
    let item = |r: &Repo| r.health().unwrap().items.into_iter().find(|i| i.maintenance).unwrap();
    assert!(!item(&f.repo).enabled);
    // `start` is the real button, and adds a task to the system's scheduler, which a test
    // mustn't do. `register` is the part of it that gitem reads back.
    f.q(&["maintenance", "register"]);
    assert!(item(&f.repo).enabled, "{:?}", f.q(&["config", "--global", "--get-all", "maintenance.repo"]));
    f.run(ops::maintenance_unregister());
    assert!(!item(&f.repo).enabled);
    assert!(!git().run_raw(Some(&f.root), &["config", "--local", "--get-regexp", "^maintenance"]).unwrap().ok(), "nothing left behind");
    assert_eq!(ops::maintenance_start().steps[0].args, ["maintenance", "start"]);
}

#[test]
fn health_says_where_an_option_is_set() {
    let _turn = turn();
    let f = Fx::new();
    let item = |r: &Repo| r.health().unwrap().items.into_iter().find(|i| i.label == "Untracked cache").unwrap();
    f.q(&["config", "--local", "core.untrackedCache", "true"]);
    let local = item(&f.repo);
    assert!(local.enabled);
    assert_eq!(local.set_elsewhere, None, "undo here will work");

    f.q(&["config", "--local", "--unset", "core.untrackedCache"]);
    f.q(&["config", "--global", "core.untrackedCache", "true"]);
    let global = item(&f.repo);
    f.q(&["config", "--global", "--unset", "core.untrackedCache"]);
    assert!(global.enabled);
    assert_eq!(global.set_elsewhere.as_deref(), Some("global"), "undo here couldn't change it");
}
