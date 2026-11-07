//! The everyday options added to merge, cherry-pick, revert, tags, clones and external tools,
//! against real git in throwaway repositories.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::GitExe;
use gitem_core::ops::{self, GitOp};

struct Fx {
    dir: tempfile::TempDir,
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
        let f = Self { dir, root, git };
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
        std::fs::write(self.root.join(p), s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn commit(&self, m: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", m]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn run(&self, op: GitOp) {
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
    fn beside(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn conflicting_branches(&self) {
        self.git(&["switch", "-q", "-c", "side"]);
        self.write("a.txt", "side\n");
        self.commit("on side");
        self.git(&["switch", "-q", "main"]);
        self.write("a.txt", "main\n");
        self.commit("on main");
    }
    fn bare(&self) -> String {
        let bare = self.beside("r.git").to_string_lossy().replace('\\', "/");
        self.git(&["init", "-q", "--bare", "-b", "main", &bare]);
        self.git(&["remote", "add", "origin", &bare]);
        bare
    }
    fn at_bare(&self, bare: &str, args: &[&str]) -> String {
        let mut all = vec![format!("--git-dir={bare}")];
        all.extend(args.iter().map(|a| a.to_string()));
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        self.git.run(Some(self.root.as_path()), &refs).unwrap().text().trim().to_string()
    }
}

#[test]
fn merge_can_favour_a_side_and_take_a_message() {
    let f = Fx::new();
    f.conflicting_branches();
    f.run(ops::merge_with("side", ops::MergeMode::NoFf, Some("theirs"), Some("My words")));
    assert_eq!(f.read("a.txt"), "side\n");
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "My words");
    let g = Fx::new();
    g.conflicting_branches();
    g.run(ops::merge_with("side", ops::MergeMode::Default, Some("ours"), None));
    assert_eq!(g.read("a.txt"), "main\n");
}

#[test]
fn a_plain_merge_still_asks_for_nothing_new() {
    assert_eq!(ops::merge("x", ops::MergeMode::Default).steps[0].args, ["merge", "--no-edit", "x"]);
}

#[test]
fn a_merge_commit_can_be_cherry_picked_by_naming_a_parent() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("on topic");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["switch", "-q", "-c", "merged"]);
    f.git(&["merge", "-q", "--no-ff", "--no-edit", "topic"]);
    let merge = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "-q", "main"]);
    assert!(ops::cherry_pick(std::slice::from_ref(&merge), false, false).run(&f.git, Some(&f.root), None, &mut |_| {}).is_err(), "git needs to be told");
    f.run(ops::cherry_pick_parent(std::slice::from_ref(&merge), false, false, Some(1)));
    assert_eq!(f.read("t.txt"), "t\n");
}

#[test]
fn a_selection_of_commits_is_reverted_newest_first() {
    let f = Fx::new();
    f.write("b.txt", "b\n");
    let a = f.commit("adds b");
    f.write("b.txt", "b\nmore\n");
    let b = f.commit("adds more");
    f.run(ops::revert_many(&[b, a], false));
    assert!(!f.root.join("b.txt").exists());
    assert_eq!(f.git(&["log", "--format=%s", "-2"]).lines().count(), 2);
}

#[test]
fn a_range_of_commits_can_be_cherry_picked() {
    let f = Fx::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("x.txt", "x\n");
    f.commit("x");
    f.write("y.txt", "y\n");
    let tip = f.commit("y");
    f.git(&["switch", "-q", "main"]);
    f.run(ops::cherry_pick(&[format!("{base}..{tip}")], false, false));
    assert_eq!(f.git(&["log", "--format=%s", "-2"]), "y\nx");
}

#[test]
fn tags_go_to_the_chosen_remote_all_together_and_can_replace() {
    let f = Fx::new();
    let bare = f.bare();
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["tag", "light"]);
    f.git(&["tag", "-a", "marked", "-m", "m"]);
    f.run(ops::tags_push_all("origin", false));
    assert_eq!(f.at_bare(&bare, &["tag", "--list"]), "light\nmarked", "lightweight ones too");

    let old = f.git(&["rev-parse", "light"]);
    f.write("a.txt", "two\n");
    let newer = f.commit("second");
    f.git(&["tag", "-f", "light"]);
    assert!(ops::tag_push("origin", "light").run(&f.git, Some(&f.root), None, &mut |_| {}).is_err(), "refused without asking to replace");
    assert_eq!(f.at_bare(&bare, &["rev-parse", "refs/tags/light"]), old);
    f.run(ops::tag_push_replacing("origin", "light"));
    assert_eq!(f.at_bare(&bare, &["rev-parse", "refs/tags/light"]), newer);
}

#[test]
fn a_partial_clone_and_a_shallow_one_made_whole() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    f.git(&["config", "uploadpack.allowFilter", "true"]);
    let source = format!("file:///{}", f.root.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
    let dest = f.beside("partial").to_string_lossy().replace('\\', "/");
    let op = ops::clone_partial(&source, &dest, None, false, None);
    assert_eq!(op.steps[0].args[1], "--filter=blob:none");
    op.run(&f.git, None, None, &mut |_| {}).unwrap();
    assert_eq!(f.git.run(Some(Path::new(&dest)), &["config", "remote.origin.promisor"]).unwrap().text().trim(), "true");

    let dest = f.beside("shallow");
    let dest_s = dest.to_string_lossy().replace('\\', "/");
    ops::clone_repo(&source, &dest_s, Some(1), false, None).run(&f.git, None, None, &mut |_| {}).unwrap();
    assert_eq!(f.git.run(Some(&dest), &["rev-list", "--count", "HEAD"]).unwrap().text().trim(), "1");
    ops::unshallow().run(&f.git, Some(&dest), None, &mut |_| {}).unwrap();
    assert_eq!(f.git.run(Some(&dest), &["rev-list", "--count", "HEAD"]).unwrap().text().trim(), "2");
}

/// A tool that says what it was given, as git would run it. Set up with git's own config, which is
/// what the setting names.
#[test]
fn a_file_opens_in_the_diff_tool_named_or_the_one_git_is_set_to() {
    let f = Fx::new();
    f.write("a b.txt", "x\n");
    f.commit("spaces");
    f.write("a b.txt", "y\n");
    let out = f.beside("seen.txt").to_string_lossy().replace('\\', "/");
    f.git(&["config", "difftool.fake.cmd", &format!("cat \"$REMOTE\" > \"{out}\"")]);
    f.run(ops::difftool_file("fake", None, "a b.txt"));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "y\n");

    std::fs::remove_file(&out).unwrap();
    f.git(&["config", "diff.tool", "fake"]);
    f.run(ops::difftool_file("", None, "a b.txt"));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "y\n");
}

#[test]
fn a_conflict_opens_in_the_merge_tool_and_is_marked_resolved_if_it_says_so() {
    let f = Fx::new();
    f.conflicting_branches();
    assert!(ops::merge("side", ops::MergeMode::Default).run(&f.git, Some(&f.root), None, &mut |_| {}).is_err());
    f.git(&["config", "mergetool.fake.cmd", "cat \"$REMOTE\" > \"$MERGED\""]);
    f.git(&["config", "mergetool.fake.trustExitCode", "true"]);
    f.git(&["config", "mergetool.keepBackup", "false"]);
    f.run(ops::mergetool_file("fake", "a.txt"));
    assert_eq!(f.read("a.txt"), "side\n");
    assert_eq!(f.git(&["diff", "--name-only", "--diff-filter=U"]), "", "nothing left in conflict");
}

#[test]
fn lfs_stops_tracking_a_pattern() {
    let f = Fx::new();
    if f.git.run_raw(Some(&f.root), &["lfs", "version"]).map(|o| !o.ok()).unwrap_or(true) {
        eprintln!("git-lfs isn't installed here, so this didn't run");
        return;
    }
    f.git(&["lfs", "track", "*.bin"]);
    assert!(f.read(".gitattributes").contains("*.bin"));
    f.run(ops::lfs_untrack("*.bin"));
    assert!(!f.read(".gitattributes").contains("*.bin"));
}

#[test]
fn lfs_locks_and_a_dash_named_path_are_built_safely() {
    assert_eq!(ops::lfs_lock("a.bin").steps[0].args, ["lfs", "lock", "a.bin"]);
    assert_eq!(ops::lfs_unlock("a.bin", true).steps[0].args, ["lfs", "unlock", "--force", "a.bin"]);
    assert!(ops::lfs_lock("--oops").bad_arg.is_some());
}

#[test]
fn a_stack_goes_to_the_remote_together_and_stops_if_one_moved() {
    let f = Fx::new();
    let bare = f.bare();
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["switch", "-q", "-c", "one"]);
    f.write("o.txt", "o\n");
    f.commit("one");
    f.git(&["switch", "-q", "-c", "two"]);
    f.write("t.txt", "t\n");
    let old_two = f.commit("two");
    f.git(&["push", "-q", "origin", "one", "two"]);
    f.git(&["fetch", "-q", "origin"]);
    // A rebase moves both, as --update-refs would.
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["switch", "-q", "two"]);
    f.git(&["rebase", "-q", "--update-refs", "main"]);
    let (new_one, new_two) = (f.git(&["rev-parse", "one"]), f.git(&["rev-parse", "two"]));
    assert_ne!(new_two, old_two);
    let stack = ["one".to_string(), "two".to_string()];
    f.run(ops::push_stack("origin", &stack));
    assert_eq!(f.at_bare(&bare, &["rev-parse", "refs/heads/one"]), new_one);
    assert_eq!(f.at_bare(&bare, &["rev-parse", "refs/heads/two"]), new_two);

    // Someone else pushes to one of them: nothing goes, not even the other.
    let other = f.beside("other");
    f.git(&["clone", "-q", &bare, &other.to_string_lossy().replace('\\', "/")]);
    let run_other = |a: &[&str]| f.git.run(Some(&other), a).unwrap().text();
    for (k, v) in [("user.name", "O"), ("user.email", "o@example.com")] {
        run_other(&["config", k, v]);
    }
    run_other(&["switch", "-q", "two"]);
    std::fs::write(other.join("x.txt"), "x").unwrap();
    run_other(&["add", "-A"]);
    run_other(&["commit", "-q", "-m", "theirs"]);
    run_other(&["push", "-q", "origin", "two"]);
    f.git(&["commit", "-q", "--amend", "-m", "two again"]);
    f.git(&["branch", "-f", "one", "HEAD~1"]);
    f.git(&["switch", "-q", "one"]);
    f.git(&["commit", "-q", "--amend", "-m", "one again"]);
    assert!(ops::push_stack("origin", &stack).run(&f.git, Some(&f.root), None, &mut |_| {}).is_err());
    assert_eq!(f.at_bare(&bare, &["rev-parse", "refs/heads/one"]), new_one, "all or none");
}

#[test]
fn blame_can_look_through_what_the_project_lists() {
    use gitem_core::repo::{BlameOpts, IGNORE_REVS_FILE};
    let f = Fx::new();
    f.write("a.txt", "ONE\n");
    let reformat = f.commit("reformat");
    std::fs::write(f.root.join(IGNORE_REVS_FILE), format!("{reformat}\n")).unwrap();
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let first = f.git(&["rev-parse", "HEAD~1"]);
    assert_eq!(repo.blame("a.txt", None, BlameOpts::default()).unwrap()[0].sha, reformat);
    assert_eq!(repo.blame("a.txt", None, BlameOpts { ignore_revs: true, ..Default::default() }).unwrap()[0].sha, first);
}

#[test]
fn rerere_is_offered_in_health_and_shows_when_on() {
    let f = Fx::new();
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let item = |r: &gitem_core::Repo| r.health().unwrap().items.into_iter().find(|i| i.label == "Remember conflict resolutions").unwrap();
    assert!(!item(&repo).enabled);
    f.git(&["config", "rerere.enabled", "true"]);
    assert!(item(&repo).enabled);
}
