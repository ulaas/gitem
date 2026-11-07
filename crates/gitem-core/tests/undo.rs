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
        self.run(entry.clone().as_it_stands(&self.git, &self.root).op)
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

/// main, then one and two stacked on it, with main ahead by a commit and HEAD on two.
fn stacked() -> Fx {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "one"]);
    f.write("one.txt", "1\n");
    f.commit("on one");
    f.git(&["switch", "-q", "-c", "two"]);
    f.write("two.txt", "2\n");
    f.commit("on two");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["switch", "-q", "two"]);
    f
}

fn rebase_stack() -> GitOp {
    ops::rebase(&ops::RebaseOpts { onto: "main".to_string(), update_refs: true, ..Default::default() })
}

#[test]
fn undoing_a_rebase_with_update_refs_puts_the_other_branches_back_and_redo_moves_them_again() {
    let f = stacked();
    let (one, two) = (f.git(&["rev-parse", "one"]), f.git(&["rev-parse", "two"]));
    let entry = f.run(rebase_stack()).expect("an undo");
    let (new_one, new_two) = (f.git(&["rev-parse", "one"]), f.git(&["rev-parse", "two"]));
    assert_ne!(new_one, one, "the rebase moved it");
    assert!(entry.detail.contains("one goes back too"), "{}", entry.detail);

    let redo = f.take_back(&entry).expect("a redo");
    assert_eq!(f.git(&["rev-parse", "two"]), two);
    assert_eq!(f.git(&["rev-parse", "one"]), one);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "two");
    assert_eq!(f.git(&["status", "--porcelain"]), "");

    assert!(redo.detail.contains("one goes back too"), "{}", redo.detail);
    f.take_back(&redo);
    assert_eq!(f.git(&["rev-parse", "two"]), new_two);
    assert_eq!(f.git(&["rev-parse", "one"]), new_one);
}

#[test]
fn rebase_update_refs_set_in_config_counts_too() {
    let f = stacked();
    f.git(&["config", "rebase.updateRefs", "true"]);
    let one = f.git(&["rev-parse", "one"]);
    let entry = f.run(ops::rebase(&ops::RebaseOpts { onto: "main".to_string(), ..Default::default() })).expect("an undo");
    assert_ne!(f.git(&["rev-parse", "one"]), one);
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "one"]), one);
}

#[test]
fn a_branch_moved_since_the_rebase_stays_and_the_undo_says_so() {
    let f = stacked();
    f.git(&["branch", "three", "one"]);
    let three = f.git(&["rev-parse", "three"]);
    let two = f.git(&["rev-parse", "two"]);
    let entry = f.run(rebase_stack()).expect("an undo");
    assert_ne!(f.git(&["rev-parse", "three"]), three, "both point at the same commit, so both moved");

    // Work done on one since, in a terminal. An undo mustn't throw it away.
    let later = f.git(&["commit-tree", "-m", "since", "-p", "one", "one^{tree}"]);
    f.git(&["update-ref", "refs/heads/one", &later]);

    let now = entry.clone().as_it_stands(&f.git, f.at());
    assert!(now.detail.contains("one has moved") && now.detail.contains("it stays"), "{}", now.detail);
    assert!(now.detail.contains("three goes back too"), "{}", now.detail);
    let outputs = now.op.run(&f.git, Some(f.at()), None, &mut |_| {}).unwrap();
    assert!(now.op.outcome_note(&outputs).is_some_and(|n| n.contains("one has moved")), "said when it's done, as well as when asking");
    assert_eq!(f.git(&["rev-parse", "two"]), two, "HEAD's branch went back");
    assert_eq!(f.git(&["rev-parse", "one"]), later, "kept");
    assert_eq!(f.git(&["rev-parse", "three"]), three, "still where the rebase left it, so it went back");
}

#[test]
fn a_branch_checked_out_elsewhere_since_the_rebase_stays() {
    let f = stacked();
    let one = f.git(&["rev-parse", "one"]);
    let entry = f.run(rebase_stack()).expect("an undo");
    let moved = f.git(&["rev-parse", "one"]);
    assert_ne!(moved, one);
    let wt = f._dir.path().join("wt").to_string_lossy().replace('\\', "/");
    f.git(&["worktree", "add", "-q", &wt, "one"]);
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "one"]), moved, "that checkout's files would no longer match it");
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "two");
}

#[test]
fn a_branch_outside_what_was_rebased_is_not_the_undos_to_move() {
    let f = stacked();
    f.git(&["branch", "aside", "main"]);
    let op = rebase_stack();
    let captured = op.undo.as_ref().unwrap().capture(&f.git, f.at()).unwrap();
    op.run(&f.git, Some(f.at()), None, &mut |_| {}).unwrap();
    // Moved meanwhile by something else, and no part of the branch being rebased.
    f.git(&["update-ref", "refs/heads/aside", "main~1"]);
    let entry = captured.into_entry(&f.git, f.at()).expect("an undo");
    assert!(!entry.detail.contains("aside"), "{}", entry.detail);
    let aside = f.git(&["rev-parse", "aside"]);
    f.take_back(&entry);
    assert_eq!(f.git(&["rev-parse", "aside"]), aside);
}

#[test]
fn a_rebase_undo_that_stops_on_uncommitted_changes_moves_no_other_branch() {
    let f = stacked();
    let entry = f.run(rebase_stack()).expect("an undo");
    let moved = f.git(&["rev-parse", "one"]);
    // m.txt came from main, so going back would have to delete this edit. `reset --keep` stops.
    f.write("m.txt", "edited\n");
    assert!(entry.op.run(&f.git, Some(f.at()), None, &mut |_| {}).is_err());
    assert_eq!(f.git(&["rev-parse", "one"]), moved);
    assert_eq!(f.read("m.txt"), "edited\n");
}
