//! Ops that destroy or move state, run for real against throwaway repositories. Each
//! test checks what happened with git itself, and what must not have happened.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::ops::{self, GitOp};
use gitem_core::{GitExe, Repo};

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    g.helper = None;
    g
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

struct Fx {
    /// Also holds what a test puts beside the repository, such as a remote or a worktree.
    dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
    repo: Repo,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let g = git();
        g.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
        configure(&g, &root);
        let repo = Repo::open(g.clone(), &root).unwrap();
        Self { dir, root, git: g, repo }
    }
    fn beside(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn exists(&self, p: &str) -> bool {
        self.root.join(p).exists()
    }
    fn run(&self, op: GitOp) {
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
    fn fails(&self, op: GitOp) -> bool {
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).is_err()
    }
    /// Git's own output, trimmed.
    fn q(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim().to_string()
    }
    fn has_ref(&self, r: &str) -> bool {
        self.git.run_raw(Some(&self.root), &["rev-parse", "--verify", "-q", r]).unwrap().ok()
    }
    fn commit(&self, file: &str, text: &str, msg: &str) -> String {
        self.write(file, text);
        self.q(&["add", "-A"]);
        self.q(&["commit", "-q", "-m", msg]);
        self.q(&["rev-parse", "HEAD"])
    }
    fn stash_of(&self, file: &str, text: &str, msg: &str) {
        self.write(file, text);
        self.q(&["stash", "push", "-q", "-m", msg]);
    }
    fn stash_messages(&self) -> Vec<String> {
        self.q(&["stash", "list", "--format=%s"]).lines().map(String::from).collect()
    }
    /// A conflict on `c.txt`, with the branch `side` being merged in.
    fn conflicted_merge(&self) {
        self.commit("c.txt", "base\n", "base");
        self.q(&["switch", "-q", "-c", "side"]);
        self.commit("c.txt", "side\n", "on side");
        self.q(&["switch", "-q", "main"]);
        self.commit("c.txt", "main\n", "on main");
        assert!(!self.git.run_raw(Some(&self.root), &["merge", "side"]).unwrap().ok());
    }
}

fn configure(g: &GitExe, root: &Path) {
    for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        g.run(Some(root), &["config", k, v]).unwrap();
    }
}

/// Git refuses a local path as a submodule source unless told otherwise.
fn allowing_local_clones(mut op: GitOp) -> GitOp {
    for s in &mut op.steps {
        s.env.push(("GIT_ALLOW_PROTOCOL".to_string(), "file".to_string()));
    }
    op
}

// ---- restoring and unstaging ------------------------------------------------------

#[test]
fn restore_file_from_replaces_only_that_file_in_both_places() {
    let f = Fx::new();
    f.write("other.txt", "first\n");
    let old = f.commit("a.txt", "old\n", "first");
    f.write("other.txt", "second\n");
    f.commit("a.txt", "new\n", "second");
    f.write("a.txt", "uncommitted\n");
    f.write("other.txt", "keep me\n");
    f.run(ops::restore_file_from(&old, "a.txt"));
    assert_eq!(f.read("a.txt"), "old\n");
    assert_eq!(f.q(&["show", ":a.txt"]), "old");
    assert_eq!(f.read("other.txt"), "keep me\n", "the other file isn't touched");
    assert_eq!(f.q(&["show", ":other.txt"]), "second");
}

#[test]
fn restore_file_from_takes_a_name_with_glob_characters_literally() {
    let f = Fx::new();
    f.write("pages/[id].tsx", "old\n");
    let old = f.commit("pages/i.tsx", "old\n", "first");
    f.write("pages/[id].tsx", "new\n");
    f.commit("pages/i.tsx", "new\n", "second");
    f.run(ops::restore_file_from(&old, "pages/[id].tsx"));
    assert_eq!(f.read("pages/[id].tsx"), "old\n");
    assert_eq!(f.read("pages/i.tsx"), "new\n", "as a glob, [id] would have matched this");
}

#[test]
fn unstage_paths_leaves_other_staged_files_and_the_working_tree() {
    let f = Fx::new();
    f.write("a.txt", "1\n");
    f.commit("b.txt", "1\n", "first");
    f.write("a.txt", "2\n");
    f.write("b.txt", "2\n");
    f.q(&["add", "-A"]);
    f.run(ops::unstage_paths(&["a.txt".into()], true));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), "b.txt");
    assert_eq!(f.read("a.txt"), "2\n");
}

#[test]
fn unstage_paths_works_before_the_first_commit() {
    let f = Fx::new();
    f.write("a.txt", "1\n");
    f.write("b.txt", "1\n");
    f.q(&["add", "-A"]);
    f.run(ops::unstage_paths(&["a.txt".into()], false));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), "b.txt");
    assert!(f.exists("a.txt"), "only the index changes");
}

#[test]
fn unstage_all_keeps_the_changes_on_disk() {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    f.write("a.txt", "2\n");
    f.write("new.txt", "n\n");
    f.q(&["add", "-A"]);
    f.run(ops::unstage_all(true));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), "");
    assert_eq!(f.read("a.txt"), "2\n");
    assert_eq!(f.read("new.txt"), "n\n");
}

#[test]
fn unstage_all_works_before_the_first_commit() {
    let f = Fx::new();
    f.write("a.txt", "1\n");
    f.q(&["add", "-A"]);
    f.run(ops::unstage_all(false));
    assert_eq!(f.q(&["status", "--short"]), "?? a.txt");
    assert!(f.exists("a.txt"));
}

// ---- stashes -----------------------------------------------------------------------

#[test]
fn stash_drop_removes_only_the_one_named() {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    f.stash_of("a.txt", "older\n", "older");
    f.stash_of("a.txt", "newer\n", "newer");
    f.run(ops::stash_drop("stash@{1}"));
    let left = f.stash_messages();
    assert_eq!(left.len(), 1);
    assert!(left[0].ends_with("newer"), "{left:?}");
    assert_eq!(f.read("a.txt"), "1\n", "the working tree isn't involved");
}

#[test]
fn stash_clear_removes_every_stash_and_nothing_else() {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    f.stash_of("a.txt", "x\n", "one");
    f.stash_of("a.txt", "y\n", "two");
    f.write("a.txt", "in progress\n");
    f.run(ops::stash_clear());
    assert!(f.stash_messages().is_empty());
    assert_eq!(f.read("a.txt"), "in progress\n");
}

#[test]
fn stash_branch_restores_the_work_on_a_new_branch_and_uses_the_stash_up() {
    let f = Fx::new();
    let start = f.commit("a.txt", "1\n", "first");
    f.stash_of("a.txt", "stashed\n", "work");
    f.commit("a.txt", "moved on\n", "second");
    f.run(ops::stash_branch("rescued", "stash@{0}"));
    assert_eq!(f.q(&["branch", "--show-current"]), "rescued");
    assert_eq!(f.q(&["rev-parse", "HEAD"]), start, "it starts where the stash was made");
    assert_eq!(f.read("a.txt"), "stashed\n");
    assert!(f.stash_messages().is_empty());
}

#[test]
fn stash_branch_keeps_the_stash_when_the_name_is_taken() {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    f.q(&["branch", "taken"]);
    f.stash_of("a.txt", "stashed\n", "work");
    assert!(f.fails(ops::stash_branch("taken", "stash@{0}")));
    assert_eq!(f.stash_messages().len(), 1, "the work is still there");
    assert_eq!(f.q(&["branch", "--show-current"]), "main");
}

// ---- conflicts and stopped operations ----------------------------------------------

#[test]
fn remove_conflicted_resolves_by_deleting_and_keeps_the_merge_going() {
    let f = Fx::new();
    f.conflicted_merge();
    f.run(ops::remove_conflicted("c.txt"));
    assert!(!f.exists("c.txt"));
    assert_eq!(f.q(&["ls-files", "-u"]), "", "no longer unmerged");
    assert!(f.root.join(".git/MERGE_HEAD").exists(), "the merge isn't over");
    assert_eq!(f.q(&["status", "--short"]), "D  c.txt");
}

#[test]
fn op_skip_drops_only_the_commit_a_rebase_stopped_on() {
    let f = Fx::new();
    f.commit("c.txt", "base\n", "base");
    f.q(&["switch", "-q", "-c", "side"]);
    f.commit("c.txt", "side\n", "conflicts");
    f.commit("d.txt", "kept\n", "clean");
    f.q(&["switch", "-q", "main"]);
    f.commit("c.txt", "main\n", "on main");
    f.q(&["switch", "-q", "side"]);
    assert!(!f.git.run_raw(Some(&f.root), &["rebase", "main"]).unwrap().ok());
    f.run(ops::op_skip("rebase"));
    assert_eq!(f.q(&["log", "--format=%s"]), "clean\non main\nbase");
    assert_eq!(f.read("c.txt"), "main\n");
    assert_eq!(f.read("d.txt"), "kept\n");
    assert!(!f.root.join(".git/rebase-merge").exists(), "the rebase is over");
}

#[test]
fn op_skip_works_for_a_cherry_pick() {
    let f = Fx::new();
    f.commit("c.txt", "base\n", "base");
    f.q(&["switch", "-q", "-c", "side"]);
    let first = f.commit("c.txt", "side\n", "conflicts");
    let second = f.commit("d.txt", "kept\n", "clean");
    f.q(&["switch", "-q", "main"]);
    f.commit("c.txt", "main\n", "on main");
    assert!(!f.git.run_raw(Some(&f.root), &["cherry-pick", &first, &second]).unwrap().ok());
    f.run(ops::op_skip("cherry-pick"));
    assert_eq!(f.q(&["log", "--format=%s"]), "clean\non main\nbase");
    assert_eq!(f.read("c.txt"), "main\n");
}

// ---- branches ----------------------------------------------------------------------

#[test]
fn branch_force_move_moves_the_branch_and_leaves_the_checkout() {
    let f = Fx::new();
    let first = f.commit("a.txt", "1\n", "first");
    f.q(&["branch", "other"]);
    let second = f.commit("a.txt", "2\n", "second");
    f.run(ops::branch_force_move("other", &second));
    assert_eq!(f.q(&["rev-parse", "other"]), second);
    assert_eq!(f.q(&["rev-parse", "main"]), second);
    f.run(ops::branch_force_move("other", &first));
    assert_eq!(f.q(&["rev-parse", "other"]), first, "backwards too");
    assert_eq!(f.read("a.txt"), "2\n");
}

#[test]
fn branch_force_move_refuses_the_branch_that_is_checked_out() {
    let f = Fx::new();
    let first = f.commit("a.txt", "1\n", "first");
    let second = f.commit("a.txt", "2\n", "second");
    assert!(f.fails(ops::branch_force_move("main", &first)));
    assert_eq!(f.q(&["rev-parse", "main"]), second);
}

#[test]
fn checkout_detached_leaves_every_branch_where_it_was() {
    let f = Fx::new();
    let first = f.commit("a.txt", "1\n", "first");
    let second = f.commit("a.txt", "2\n", "second");
    f.run(ops::checkout_detached(&first));
    assert_eq!(f.q(&["rev-parse", "HEAD"]), first);
    assert_eq!(f.git.run_raw(Some(&f.root), &["symbolic-ref", "-q", "HEAD"]).unwrap().ok(), false, "HEAD is detached");
    assert_eq!(f.q(&["rev-parse", "main"]), second);
    assert_eq!(f.read("a.txt"), "1\n");
}

#[test]
fn checkout_detached_refuses_to_overwrite_local_changes() {
    let f = Fx::new();
    let first = f.commit("a.txt", "1\n", "first");
    f.commit("a.txt", "2\n", "second");
    f.write("a.txt", "precious\n");
    assert!(f.fails(ops::checkout_detached(&first)));
    assert_eq!(f.read("a.txt"), "precious\n");
}

/// A clone of a bare remote that also has the branch `feature/x`, not yet checked out.
fn clone_with_remote_branch() -> (Fx, PathBuf) {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    let bare = f.beside("remote.git");
    f.q(&["init", "-q", "--bare", "-b", "main", &path_str(&bare)]);
    f.q(&["remote", "add", "origin", &path_str(&bare)]);
    f.q(&["push", "-q", "origin", "main"]);
    f.q(&["switch", "-q", "-c", "feature/x"]);
    f.commit("b.txt", "b\n", "on feature");
    f.q(&["push", "-q", "origin", "feature/x"]);
    f.q(&["switch", "-q", "main"]);
    f.q(&["branch", "-D", "feature/x"]);
    f.q(&["fetch", "-q", "origin"]);
    (f, bare)
}

#[test]
fn checkout_remote_makes_a_tracking_branch_of_the_short_name() {
    let (f, _bare) = clone_with_remote_branch();
    f.run(ops::checkout_remote("origin/feature/x"));
    assert_eq!(f.q(&["branch", "--show-current"]), "feature/x");
    assert_eq!(f.q(&["rev-parse", "--abbrev-ref", "@{u}"]), "origin/feature/x");
    assert_eq!(f.read("b.txt"), "b\n");
}

/// FG-9 asks for this to switch to the branch instead. Until then, it mustn't move it.
#[test]
fn checkout_remote_with_the_local_name_taken_fails_and_moves_nothing() {
    let (f, _bare) = clone_with_remote_branch();
    let main = f.q(&["rev-parse", "main"]);
    f.q(&["branch", "feature/x"]);
    assert!(f.fails(ops::checkout_remote("origin/feature/x")));
    assert_eq!(f.q(&["rev-parse", "feature/x"]), main);
    assert_eq!(f.q(&["branch", "--show-current"]), "main");
}

#[test]
fn upstream_can_be_set_and_unset() {
    let (f, _bare) = clone_with_remote_branch();
    f.q(&["branch", "mine"]);
    f.run(ops::branch_set_upstream("mine", "origin/main"));
    assert_eq!(f.q(&["rev-parse", "--abbrev-ref", "mine@{u}"]), "origin/main");
    f.run(ops::branch_unset_upstream("mine"));
    assert!(!f.has_ref("mine@{u}"));
    assert!(f.has_ref("mine"), "the branch stays");
}

// ---- worktrees ---------------------------------------------------------------------

fn with_worktree(f: &Fx) -> PathBuf {
    f.commit("a.txt", "1\n", "first");
    let wt = f.beside("wt");
    f.q(&["worktree", "add", "-q", "-b", "wt-branch", &path_str(&wt)]);
    wt
}

fn worktree_paths(f: &Fx) -> Vec<PathBuf> {
    f.repo.worktrees().unwrap().into_iter().map(|w| PathBuf::from(w.path)).collect()
}

#[test]
fn worktree_move_keeps_its_uncommitted_work() {
    let f = Fx::new();
    let wt = with_worktree(&f);
    std::fs::write(wt.join("wip.txt"), "wip\n").unwrap();
    let to = f.beside("moved");
    f.run(ops::worktree_move(&path_str(&wt), &path_str(&to)));
    assert!(!wt.exists());
    assert_eq!(std::fs::read_to_string(to.join("wip.txt")).unwrap(), "wip\n");
    assert!(worktree_paths(&f).contains(&PathBuf::from(path_str(&to))), "{:?}", worktree_paths(&f));
}

#[test]
fn worktree_prune_forgets_only_folders_that_are_gone() {
    let f = Fx::new();
    let gone = with_worktree(&f);
    let kept = f.beside("kept");
    f.q(&["worktree", "add", "-q", "-b", "kept-branch", &path_str(&kept)]);
    std::fs::remove_dir_all(&gone).unwrap();
    f.run(ops::worktree_prune());
    let left = worktree_paths(&f);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&PathBuf::from(path_str(&kept))), "{left:?}");
    assert!(f.has_ref("wt-branch"), "its branch stays");
}

#[test]
fn worktree_repair_reconnects_worktrees_after_the_main_folder_moved() {
    let f = Fx::new();
    let wt = with_worktree(&f);
    std::fs::write(wt.join("wip.txt"), "wip\n").unwrap();
    assert!(f.git.run_raw(Some(&wt), &["status"]).unwrap().ok());
    let moved = f.beside("repo-moved");
    std::fs::rename(&f.root, &moved).unwrap();
    assert!(!f.git.run_raw(Some(&wt), &["status"]).unwrap().ok(), "the worktree has lost its repository");
    ops::worktree_repair().run(&f.git, Some(&moved), None, &mut |_| {}).unwrap();
    assert!(f.git.run_raw(Some(&wt), &["status"]).unwrap().ok());
    assert_eq!(std::fs::read_to_string(wt.join("wip.txt")).unwrap(), "wip\n");
}

// ---- remotes -----------------------------------------------------------------------

#[test]
fn publish_repository_adds_the_remote_pushes_and_tracks() {
    let f = Fx::new();
    let head = f.commit("a.txt", "1\n", "first");
    let bare = f.beside("pub.git");
    f.q(&["init", "-q", "--bare", "-b", "main", &path_str(&bare)]);
    f.run(ops::publish_repository("origin", &path_str(&bare), "main"));
    assert_eq!(f.q(&["remote", "get-url", "origin"]), path_str(&bare));
    assert_eq!(f.q(&["rev-parse", "--abbrev-ref", "main@{u}"]), "origin/main");
    assert_eq!(f.git.run(Some(&bare), &["rev-parse", "main"]).unwrap().text().trim(), head);
}

#[test]
fn remote_add_checked_adds_a_remote_that_answers() {
    let f = Fx::new();
    let bare = f.beside("ok.git");
    f.q(&["init", "-q", "--bare", &path_str(&bare)]);
    f.run(ops::remote_add_checked("good", &path_str(&bare)));
    assert_eq!(f.q(&["remote"]), "good");
}

#[test]
fn remote_add_checked_adds_nothing_when_the_url_does_not_answer() {
    let f = Fx::new();
    let nowhere = path_str(&f.beside("no-such.git"));
    assert!(f.fails(ops::remote_add_checked("bad", &nowhere)));
    assert_eq!(f.q(&["remote"]), "");
}

// ---- submodules --------------------------------------------------------------------

/// A repository to use as a submodule, in its own folder.
fn library(f: &Fx) -> String {
    let lib = f.beside("lib");
    std::fs::create_dir(&lib).unwrap();
    f.git.run(Some(&lib), &["init", "-q", "-b", "main"]).unwrap();
    configure(&f.git, &lib);
    std::fs::write(lib.join("s.txt"), "s\n").unwrap();
    f.git.run(Some(&lib), &["add", "."]).unwrap();
    f.git.run(Some(&lib), &["commit", "-q", "-m", "s"]).unwrap();
    path_str(&lib)
}

fn with_submodule(f: &Fx) -> String {
    f.commit("a.txt", "1\n", "first");
    let url = library(f);
    f.run(allowing_local_clones(ops::submodule_add(&url, "libs/sub")));
    f.q(&["commit", "-q", "-m", "add sub"]);
    url
}

#[test]
fn submodule_add_clones_it_and_records_it() {
    let f = Fx::new();
    let url = with_submodule(&f);
    assert_eq!(f.read("libs/sub/s.txt"), "s\n");
    assert_eq!(f.q(&["config", "-f", ".gitmodules", "submodule.libs/sub.url"]), url);
    assert_eq!(f.repo.submodules().unwrap()[0].state, ' ');
}

#[test]
fn submodule_deinit_empties_the_folder_but_keeps_the_record() {
    let f = Fx::new();
    with_submodule(&f);
    f.run(ops::submodule_deinit("libs/sub", false));
    assert!(!f.exists("libs/sub/s.txt"));
    assert!(f.exists("libs/sub"), "the empty folder stays");
    assert!(f.exists(".gitmodules"));
    assert_eq!(f.repo.submodules().unwrap()[0].state, '-');
}

#[test]
fn submodule_deinit_refuses_local_changes_unless_forced() {
    let f = Fx::new();
    with_submodule(&f);
    f.write("libs/sub/s.txt", "precious\n");
    assert!(f.fails(ops::submodule_deinit("libs/sub", false)));
    assert_eq!(f.read("libs/sub/s.txt"), "precious\n");
    f.run(ops::submodule_deinit("libs/sub", true));
    assert!(!f.exists("libs/sub/s.txt"));
}

#[test]
fn submodule_update_brings_back_a_deinitialised_one() {
    let f = Fx::new();
    with_submodule(&f);
    f.run(ops::submodule_deinit("libs/sub", false));
    f.run(allowing_local_clones(ops::submodule_update(Some("libs/sub"), true, false, false)));
    assert_eq!(f.read("libs/sub/s.txt"), "s\n");
    assert_eq!(f.repo.submodules().unwrap()[0].state, ' ');
}

#[test]
fn submodule_sync_copies_a_changed_url_into_the_config() {
    let f = Fx::new();
    with_submodule(&f);
    f.q(&["config", "-f", ".gitmodules", "submodule.libs/sub.url", "https://x.invalid/moved.git"]);
    f.run(ops::submodule_sync());
    assert_eq!(f.q(&["config", "submodule.libs/sub.url"]), "https://x.invalid/moved.git");
}

#[test]
fn submodule_remove_takes_out_the_folder_and_the_record() {
    let f = Fx::new();
    with_submodule(&f);
    f.run(ops::submodule_remove("libs/sub"));
    assert!(!f.exists("libs/sub"));
    assert_eq!(f.q(&["config", "-f", ".gitmodules", "--list"]), "", "its section is gone");
    f.q(&["commit", "-q", "-m", "remove sub"]);
    assert!(f.repo.submodules().unwrap().is_empty());
    assert!(f.exists("a.txt"));
}

// ---- other ---------------------------------------------------------------------------

#[test]
fn init_repo_makes_a_repository_with_the_branch_named() {
    let f = Fx::new();
    let p = f.beside("fresh folder");
    f.run(ops::init_repo(&path_str(&p), Some("trunk")));
    let branch = f.git.run(Some(&p), &["symbolic-ref", "--short", "HEAD"]).unwrap().text();
    assert_eq!(branch.trim(), "trunk");
}

#[test]
fn am_patch_file_recreates_the_commit_with_its_author_and_message() {
    let f = Fx::new();
    f.commit("a.txt", "1\n", "first");
    f.q(&["switch", "-q", "-c", "side"]);
    let c = f.commit("a.txt", "2\n", "the change\n\nwith a body");
    let out = f.beside("patches");
    let name = f.q(&["format-patch", "-1", &c, "-o", &path_str(&out)]);
    f.q(&["switch", "-q", "main"]);
    f.run(ops::am_patch_file(&name));
    assert_eq!(f.read("a.txt"), "2\n");
    assert_eq!(f.q(&["log", "-1", "--format=%s|%b|%an"]), "the change|with a body\n|Test User");
    assert!(!f.root.join(".git/rebase-apply").exists(), "am is finished");
}

#[test]
fn bisect_run_finds_the_commit_that_broke_it() {
    let f = Fx::new();
    let good = f.commit("a.txt", "1\n", "good");
    f.commit("a.txt", "2\n", "still good");
    let bad = f.commit("broken", "x\n", "breaks it");
    f.commit("a.txt", "3\n", "later");
    f.run(ops::bisect_start("HEAD", &good));
    f.run(ops::bisect_run("test ! -f broken"));
    assert_eq!(f.q(&["rev-parse", "refs/bisect/bad"]), bad);
    f.run(ops::bisect_reset());
    assert_eq!(f.q(&["branch", "--show-current"]), "main");
}

#[test]
fn maintenance_run_keeps_every_commit() {
    let f = Fx::new();
    let first = f.commit("a.txt", "1\n", "first");
    let second = f.commit("a.txt", "2\n", "second");
    f.run(ops::maintenance_run());
    assert_eq!(f.q(&["rev-parse", "HEAD"]), second);
    assert_eq!(f.q(&["rev-parse", "HEAD~1"]), first);
    assert_eq!(f.q(&["status", "--short"]), "");
}

/// Git for Windows turns symlinks on, where making one can then fail. So this asks for
/// real copies, and the tool here reports what it was given.
#[test]
fn difftool_dir_shows_both_sides_even_where_symlinks_are_on() {
    let f = Fx::new();
    let a = f.commit("a.txt", "old\n", "first");
    let b = f.commit("a.txt", "new\n", "second");
    let report = f.beside("report.txt");
    f.q(&["config", "core.symlinks", "true"]);
    f.q(&["config", "diff.tool", "probe"]);
    let cmd = format!("cat \"$LOCAL/a.txt\" \"$REMOTE/a.txt\" > \"{}\"", path_str(&report));
    f.q(&["config", "difftool.probe.cmd", &cmd]);
    f.run(ops::difftool_dir(&a, &b));
    assert_eq!(std::fs::read_to_string(report).unwrap(), "old\nnew\n");
    assert_eq!(f.read("a.txt"), "new\n");
}

// ---- LFS, where it's installed ---------------------------------------------------------

fn has_lfs(f: &Fx) -> bool {
    let found = f.git.run_raw(None, &["lfs", "version"]).is_ok_and(|o| o.ok());
    if !found {
        eprintln!("git-lfs isn't installed, so this test checked nothing");
    }
    found
}

#[test]
fn lfs_install_is_local_and_track_stages_the_attributes() {
    let f = Fx::new();
    if !has_lfs(&f) {
        return;
    }
    f.commit("a.txt", "1\n", "first");
    f.run(ops::lfs_install());
    assert!(f.q(&["config", "--local", "filter.lfs.clean"]).contains("git-lfs"));
    let global = f.git.run_raw(Some(&f.root), &["config", "--global", "filter.lfs.clean"]).unwrap();
    assert!(!global.ok(), "--local leaves the user's global config alone");
    f.run(ops::lfs_track("*.psd"));
    assert!(f.read(".gitattributes").contains("*.psd filter=lfs"));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), ".gitattributes");
    f.write("art.psd", "pretend image\n");
    f.q(&["add", "art.psd"]);
    assert!(f.q(&["show", ":art.psd"]).starts_with("version https://git-lfs"), "stored as a pointer");
    assert_eq!(f.read("art.psd"), "pretend image\n");
}

#[test]
fn lfs_prune_keeps_what_the_checkout_needs() {
    let f = Fx::new();
    if !has_lfs(&f) {
        return;
    }
    f.commit("a.txt", "1\n", "first");
    f.run(ops::lfs_install());
    f.run(ops::lfs_track("*.psd"));
    f.commit("art.psd", "pretend image\n", "art");
    f.run(ops::lfs_prune());
    assert_eq!(f.read("art.psd"), "pretend image\n");
}
