//! Taking back the last operation, against real git in throwaway repositories. Each test runs the
//! op as the app would: capture, run, make the entry, then run what the entry says.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::GitExe;
use gitem_core::ops::{self, GitOp};
use gitem_core::undo::UndoEntry;

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
        let mut git = GitExe::discover(None).unwrap();
        git.helper = None;
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write("a.txt", "one\n");
        f.commit("first");
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim().to_string()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn commit(&self, m: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", m]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn run(&self, op: GitOp) -> Option<UndoEntry> {
        let captured = op.undo.as_ref().map(|p| p.capture(&self.git, &self.root).unwrap());
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
        captured.and_then(|c| c.into_entry(&self.git, &self.root))
    }
    fn take_back(&self, entry: &UndoEntry) -> Option<UndoEntry> {
        assert_eq!(entry.refusal(&self.git, &self.root), None);
        self.run(entry.op.clone())
    }
    fn at(&self) -> &Path {
        &self.root
    }
}

#[test]
fn a_hard_reset_comes_back_with_the_uncommitted_changes_it_threw_away() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    let tip = f.commit("second");
    f.write("a.txt", "uncommitted\n");
    f.write("staged.txt", "staged\n");
    f.git(&["add", "staged.txt"]);
    let entry = f.run(ops::reset("HEAD~1", ops::ResetMode::Hard)).expect("an undo");
    assert_eq!(f.read("a.txt"), "one\n");
    assert!(!f.at().join("staged.txt").exists());
    assert_eq!(entry.label, "Undo reset");
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
    assert_eq!(f.read("a.txt"), "uncommitted\n");
    assert_eq!(f.read("staged.txt"), "staged\n");
    assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "staged.txt", "what was staged is staged again");
}

#[test]
fn soft_and_mixed_resets_come_back_with_nothing_left_over() {
    for mode in [ops::ResetMode::Soft, ops::ResetMode::Mixed] {
        let f = Fx::new();
        f.write("a.txt", "two\n");
        let tip = f.commit("second");
        let entry = f.run(ops::reset("HEAD~1", mode)).expect("an undo");
        f.take_back(&entry);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
        assert_eq!(f.git(&["status", "--porcelain"]), "", "{mode:?}");
    }
}

#[test]
fn a_merge_is_undone_and_redone() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("on topic");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    let before = f.commit("on main");
    let entry = f.run(ops::merge("topic", ops::MergeMode::NoFf)).expect("an undo");
    let merged = f.git(&["rev-parse", "HEAD"]);
    assert_eq!(entry.label, "Undo merge");
    let redo = f.take_back(&entry).expect("a redo");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert_eq!(redo.label, "Redo merge");
    f.take_back(&redo);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), merged);
}

#[test]
fn a_rebase_comes_back_to_its_old_commits() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    let tip = f.commit("on topic");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["switch", "-q", "topic"]);
    let entry = f.run(ops::rebase(&ops::RebaseOpts { onto: "main".to_string(), ..Default::default() })).expect("an undo");
    assert_ne!(f.git(&["rev-parse", "HEAD"]), tip);
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
}

#[test]
fn an_op_that_changed_nothing_has_nothing_to_undo() {
    let f = Fx::new();
    assert!(f.run(ops::merge("HEAD", ops::MergeMode::Default)).is_none());
    assert!(f.run(ops::reset("HEAD", ops::ResetMode::Keep)).is_none());
    assert!(f.run(ops::discard_paths(&["a.txt".to_string()])).is_none());
}

#[test]
fn undoing_refuses_once_head_has_moved_or_gone_to_another_branch() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let entry = f.run(ops::reset("HEAD~1", ops::ResetMode::Keep)).expect("an undo");
    f.write("b.txt", "b\n");
    let later = f.commit("made since");
    assert!(entry.refusal(&f.git, f.at()).is_some_and(|w| w.contains("moved")));
    assert!(entry.op.run(&f.git, Some(f.at()), None, &mut |_| {}).is_err(), "and the op's own guard stops it");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), later);

    let g = Fx::new();
    g.write("a.txt", "two\n");
    g.commit("second");
    let entry = g.run(ops::reset("HEAD~1", ops::ResetMode::Keep)).expect("an undo");
    g.git(&["switch", "-q", "-c", "elsewhere"]);
    assert!(entry.refusal(&g.git, g.at()).is_some_and(|w| w.contains("main")), "at the same commit, but on another branch");
}

#[test]
fn a_deleted_branch_comes_back_with_its_upstream() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    let tip = f.commit("on topic");
    f.git(&["branch", "--set-upstream-to=main", "topic"]);
    f.git(&["switch", "-q", "main"]);
    let entry = f.run(ops::branch_delete("topic", true)).expect("an undo");
    assert_eq!(f.git(&["branch", "--list", "topic"]), "");
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "refs/heads/topic"]), tip);
    assert_eq!(f.git(&["rev-parse", "--abbrev-ref", "topic@{u}"]), "main");
}

#[test]
fn a_dropped_stash_comes_back_and_so_does_a_whole_list() {
    let f = Fx::new();
    f.write("a.txt", "first stash\n");
    f.git(&["stash", "push", "-q", "-m", "older"]);
    f.write("a.txt", "second stash\n");
    f.git(&["stash", "push", "-q", "-m", "newer"]);
    let entry = f.run(ops::stash_drop("stash@{0}")).expect("an undo");
    assert_eq!(f.git(&["stash", "list", "--format=%gs"]).lines().count(), 1);
    f.take_back(&entry);
    let list = f.git(&["stash", "list", "--format=%gs"]);
    assert!(list.lines().next().unwrap().contains("newer"), "{list}");

    let entry = f.run(ops::stash_clear()).expect("an undo");
    assert_eq!(f.git(&["stash", "list"]), "");
    f.take_back(&entry);
    let list = f.git(&["stash", "list", "--format=%gs"]);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("newer") && lines[1].contains("older"), "in their old order: {list}");
}

#[test]
fn discarded_edits_come_back_and_what_was_staged_stays_staged() {
    let f = Fx::new();
    f.write("b [1].txt", "base\n");
    f.commit("second");
    f.write("a.txt", "staged edit\n");
    f.git(&["add", "a.txt"]);
    f.write("a.txt", "later edit\n");
    f.write("b [1].txt", "other edit\n");
    let entry = f.run(ops::discard_paths(&["a.txt".to_string(), "b [1].txt".to_string()])).expect("an undo");
    assert_eq!(f.read("a.txt"), "staged edit\n");
    assert_eq!(f.read("b [1].txt"), "base\n");
    f.take_back(&entry);
    assert_eq!(f.read("a.txt"), "later edit\n");
    assert_eq!(f.read("b [1].txt"), "other edit\n");
    assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "a.txt");
}

#[test]
fn deleted_untracked_files_and_folders_come_back() {
    let f = Fx::new();
    f.write("new/inner.txt", "inner\n");
    f.write("loose.txt", "loose\n");
    let entry = f.run(ops::delete_untracked(&["new/".to_string(), "loose.txt".to_string()])).expect("an undo");
    assert!(!f.at().join("new/inner.txt").exists());
    assert!(!f.at().join("loose.txt").exists());
    f.take_back(&entry);
    assert_eq!(f.read("new/inner.txt"), "inner\n");
    assert_eq!(f.read("loose.txt"), "loose\n");
    assert_eq!(f.git(&["status", "--porcelain"]), "?? loose.txt\n?? new/", "still untracked");
}

#[test]
fn undoing_a_discard_refuses_if_the_file_was_changed_again() {
    let f = Fx::new();
    f.write("a.txt", "edit\n");
    let entry = f.run(ops::discard_paths(&["a.txt".to_string()])).expect("an undo");
    f.write("a.txt", "something new\n");
    assert!(entry.refusal(&f.git, f.at()).is_some_and(|w| w.contains("changed")));
    assert_eq!(f.read("a.txt"), "something new\n");
}

#[test]
fn a_discarded_hunk_comes_back_while_the_rest_of_the_file_stays() {
    let f = Fx::new();
    f.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n");
    f.commit("long");
    f.write("a.txt", "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\ntwelve\n");
    let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,4 +1,4 @@\n-1\n+one\n 2\n 3\n 4\n";
    let op = ops::apply_patch("Discard hunk", patch.to_string(), false, true).undoable(gitem_core::undo::UndoPlan::Discard { label: "Undo discard".to_string(), paths: vec!["a.txt".to_string()] });
    let entry = f.run(op).expect("an undo");
    assert!(f.read("a.txt").starts_with("1\n") && f.read("a.txt").ends_with("twelve\n"));
    f.take_back(&entry);
    assert!(f.read("a.txt").starts_with("one\n") && f.read("a.txt").ends_with("twelve\n"));
}
