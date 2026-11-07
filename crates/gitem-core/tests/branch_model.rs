//! The branch model against real git in throwaway repositories, with names from its settings.
gitem_testenv::isolate!();

use std::path::PathBuf;

use gitem_core::GitExe;
use gitem_core::branchmodel::{BranchModel, Kind};
use gitem_core::ops::GitOp;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let git = GitExe::discover(None).unwrap();
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.commit("a.txt", "one\n", "first");
        f.git(&["branch", "develop"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim_end().to_string()
    }
    fn commit(&self, p: &str, s: &str, m: &str) {
        std::fs::write(self.root.join(p), s).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", m]);
    }
    fn run(&self, op: GitOp) -> Result<(), String> {
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).map(drop).map_err(|e| e.to_string())
    }
    fn branch_exists(&self, b: &str) -> bool {
        !self.git(&["branch", "--list", b]).is_empty()
    }
    fn has(&self, rev: &str, file: &str) -> bool {
        self.git.run_raw(Some(&self.root), &["cat-file", "-e", &format!("{rev}:{file}")]).unwrap().ok()
    }
    fn current(&self) -> String {
        self.git(&["symbolic-ref", "--short", "HEAD"])
    }
}

#[test]
fn a_feature_starts_from_develop_and_finishes_into_it_alone() {
    let f = Fx::new();
    let m = BranchModel::default();
    f.git(&["switch", "-q", "main"]);
    f.commit("on-main.txt", "m\n", "only on main");
    f.run(m.start(Kind::Feature, "login", false)).unwrap();
    assert_eq!(f.current(), "feature/login");
    assert!(!f.has("HEAD", "on-main.txt"), "from develop, not from where it was");
    f.commit("login.txt", "l\n", "login");
    f.run(m.finish(Kind::Feature, "login", false)).unwrap();
    assert_eq!(f.current(), "develop");
    assert!(f.has("develop", "login.txt"));
    assert!(!f.has("main", "login.txt"));
    assert!(!f.branch_exists("feature/login"));
    assert_eq!(f.git(&["log", "-1", "--format=%p", "develop"]).split(' ').count(), 2, "a merge commit, even where it could fast-forward");
    assert_eq!(f.git(&["tag", "--list"]), "", "a feature has no tag");
}

#[test]
fn a_release_goes_to_main_with_a_tag_and_back_to_develop() {
    let f = Fx::new();
    let m = BranchModel::default();
    f.run(m.start(Kind::Release, "1.2", false)).unwrap();
    assert_eq!(f.current(), "release/1.2");
    f.commit("version.txt", "1.2\n", "bump");
    f.run(m.finish(Kind::Release, "1.2", false)).unwrap();
    assert_eq!(f.current(), "develop");
    assert!(f.has("main", "version.txt") && f.has("develop", "version.txt"));
    assert!(!f.branch_exists("release/1.2"));
    assert_eq!(f.git(&["rev-parse", "v1.2^{commit}"]), f.git(&["rev-parse", "main"]), "the tag is on main's merge");
    assert_eq!(f.git(&["cat-file", "-t", "v1.2"]), "tag", "annotated");
}

#[test]
fn a_hotfix_starts_from_main_and_uses_names_from_settings() {
    let f = Fx::new();
    f.git(&["branch", "-m", "main", "trunk"]);
    f.git(&["branch", "-m", "develop", "next"]);
    f.git(&["switch", "-q", "next"]);
    f.commit("wip.txt", "w\n", "not released");
    let m = BranchModel { main: "trunk".into(), develop: "next".into(), hotfix_prefix: "fix-".into(), tag_prefix: "rel-".into(), enabled: true, ..Default::default() };
    f.run(m.start(Kind::Hotfix, "9", false)).unwrap();
    assert_eq!(f.current(), "fix-9");
    assert!(!f.has("HEAD", "wip.txt"), "from what's released");
    f.commit("fix.txt", "f\n", "the fix");
    f.run(m.finish(Kind::Hotfix, "9", false)).unwrap();
    assert!(f.has("trunk", "fix.txt") && f.has("next", "fix.txt"));
    assert!(!f.has("trunk", "wip.txt"));
    assert_eq!(f.git(&["rev-parse", "rel-9^{commit}"]), f.git(&["rev-parse", "trunk"]));
    assert!(!f.branch_exists("fix-9"));
}

#[test]
fn with_one_long_lived_branch_it_only_merges_once() {
    let f = Fx::new();
    let m = BranchModel { develop: "main".into(), ..Default::default() };
    f.run(m.start(Kind::Release, "2", false)).unwrap();
    f.commit("two.txt", "2\n", "two");
    f.run(m.finish(Kind::Release, "2", false)).unwrap();
    assert_eq!(f.current(), "main");
    assert_eq!(f.git(&["rev-parse", "v2^{commit}"]), f.git(&["rev-parse", "main"]));
    assert!(!f.branch_exists("release/2"));
}

#[test]
fn starting_can_make_develop_from_main_when_asked() {
    let f = Fx::new();
    f.git(&["branch", "-D", "develop"]);
    let m = BranchModel::default();
    assert!(f.run(m.start(Kind::Feature, "x", false)).is_err(), "not without asking");
    assert_eq!(f.current(), "main");
    assert!(!f.branch_exists("develop"));
    f.run(m.start(Kind::Feature, "x", true)).unwrap();
    assert_eq!(f.current(), "feature/x");
    assert_eq!(f.git(&["rev-parse", "develop"]), f.git(&["rev-parse", "main"]));
    assert!(!f.git.run_raw(Some(&f.root), &["config", "branch.feature/x.remote"]).unwrap().ok(), "no upstream");
}

/// A conflict stops it with the merge in progress. Once that's finished, asking again carries on
/// with what was left, and doesn't fail on the tag it already made.
#[test]
fn a_conflict_stops_it_and_finishing_again_carries_on() {
    let f = Fx::new();
    let m = BranchModel::default();
    f.run(m.start(Kind::Release, "3", false)).unwrap();
    f.commit("a.txt", "from the release\n", "release side");
    f.git(&["switch", "-q", "develop"]);
    f.commit("a.txt", "from develop\n", "develop side");
    f.git(&["switch", "-q", "release/3"]);

    let e = f.run(m.finish(Kind::Release, "3", false)).unwrap_err();
    assert!(e.contains("CONFLICT"), "{e}");
    assert_eq!(f.current(), "develop", "stopped at the second merge");
    assert!(f.branch_exists("release/3"), "nothing's deleted");
    assert_eq!(f.git(&["rev-parse", "v3^{commit}"]), f.git(&["rev-parse", "main"]));
    assert!(f.has("main", "a.txt"));

    std::fs::write(f.root.join("a.txt"), "both\n").unwrap();
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-q", "--no-edit"]);
    let tag = f.git(&["rev-parse", "v3"]);
    f.run(m.finish(Kind::Release, "3", true)).unwrap();
    assert_eq!(f.current(), "develop");
    assert!(!f.branch_exists("release/3"));
    assert_eq!(f.git(&["rev-parse", "v3"]), tag, "the same tag");
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "both\n");
}

#[test]
fn a_tag_already_there_is_said_and_left_alone() {
    let m = BranchModel::default();
    assert!(m.finish_says(Kind::Release, "4", true).contains("leaves the tag v4"));
    assert!(m.finish_says(Kind::Release, "4", false).contains("tags that v4"));
    assert!(!m.finish(Kind::Release, "4", true).preview().contains("tag -a"));
    assert!(m.finish(Kind::Release, "4", false).preview().contains("tag -a v4"));
}

#[test]
fn unmerged_work_on_the_branch_isnt_lost_by_a_finish_that_stops_early() {
    let f = Fx::new();
    let m = BranchModel::default();
    f.run(m.start(Kind::Feature, "keep", false)).unwrap();
    f.commit("keep.txt", "k\n", "kept");
    let at = f.git(&["rev-parse", "HEAD"]);
    // A folder in the way of what the merge writes.
    f.git(&["switch", "-q", "develop"]);
    std::fs::create_dir(f.root.join("keep.txt")).unwrap();
    std::fs::write(f.root.join("keep.txt/inside"), "x").unwrap();
    assert!(f.run(m.finish(Kind::Feature, "keep", false)).is_err());
    assert!(f.branch_exists("feature/keep"));
    assert_eq!(f.git(&["rev-parse", "feature/keep"]), at);
}
