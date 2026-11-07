//! What the secondary views ask of git: search, contributors, health, bisect, worktrees and
//! starting work. Real git in throwaway repositories, with a local bare repository as a remote.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::model::*;
use gitem_core::ops::{self, GitOp};
use gitem_core::repo::parse_search;
use gitem_core::{GitExe, Repo};

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git");
    g.helper = None;
    g
}

struct Fx {
    dir: tempfile::TempDir,
    root: PathBuf,
    repo: Repo,
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

impl Fx {
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
        Self { dir, root, repo }
    }
    fn q(&self, args: &[&str]) -> String {
        git().run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim_end().to_string()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn commit(&self, file: &str, msg: &str) -> String {
        self.write(file, msg);
        self.q(&["add", "-A"]);
        self.q(&["commit", "-qm", msg]);
        self.q(&["rev-parse", "HEAD"])
    }
    /// A commit by someone else.
    fn commit_as(&self, file: &str, msg: &str, name: &str, email: &str) {
        self.write(file, msg);
        self.q(&["add", "-A"]);
        let (n, e) = (format!("user.name={name}"), format!("user.email={email}"));
        self.q(&["-c", &n, "-c", &e, "commit", "-qm", msg]);
    }
    fn run(&self, op: GitOp) {
        op.run(&git(), Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
    fn fails(&self, op: GitOp) -> String {
        op.run(&git(), Some(&self.root), None, &mut |_| {}).expect_err("should have failed").to_string()
    }
    fn found(&self, query: &str) -> Vec<String> {
        self.repo.log(&parse_search(query)).unwrap_or_else(|e| panic!("{query}: {e}")).into_iter().map(|c| c.subject).collect()
    }
    /// A bare repository beside this one, as `origin`, with `main` pushed and `origin/HEAD` set.
    fn with_origin(&self) -> PathBuf {
        let bare = self.dir.path().join("origin.git");
        git().run(None, &["init", "-q", "--bare", "-b", "main", &path_str(&bare)]).unwrap();
        self.q(&["remote", "add", "origin", &path_str(&bare)]);
        self.q(&["push", "-q", "origin", "main"]);
        self.q(&["fetch", "-q", "origin"]);
        self.q(&["remote", "set-head", "origin", "main"]);
        bare
    }
}

// ---- search

#[test]
fn words_are_found_as_written_and_all_of_them_count() {
    let f = Fx::new();
    f.commit("a", "fix(auth): tighten login");
    f.commit("b", "fixauth typo");
    f.commit("c", "C++ port");
    f.commit("d", "Cats and dogs");

    assert_eq!(f.found("fix(auth):"), ["fix(auth): tighten login"]);
    assert_eq!(f.found("\"fix(auth): tighten\""), ["fix(auth): tighten login"]);
    assert_eq!(f.found("fix(auth)"), ["fix(auth): tighten login"], "not the commit with no brackets");
    assert_eq!(f.found("fix("), ["fix(auth): tighten login"], "git used to call this a bad pattern");
    assert_eq!(f.found("C++"), ["C++ port"], "not everything with a C in it");
    assert_eq!(f.found("tighten login"), ["fix(auth): tighten login"]);
    assert!(f.found("tighten typo").is_empty(), "each word narrows the search");
    assert_eq!(f.found("TIGHTEN"), ["fix(auth): tighten login"], "case doesn't matter");

    let mut either = f.found("re:tighten|typo");
    either.sort();
    assert_eq!(either, ["fix(auth): tighten login", "fixauth typo"], "a pattern can say either");
    assert_eq!(f.found("fix( re:tigh.en"), ["fix(auth): tighten login"], "a word beside a pattern stays a word");
}

#[test]
fn a_commit_asked_for_is_that_commit_alone() {
    let f = Fx::new();
    f.commit("a", "first");
    let second = f.commit("b", "second");
    f.commit("c", "third");
    assert_eq!(f.found(&format!("#:{second}")), ["second"]);
    assert_eq!(f.found(&format!("commit:{}", &second[..8])), ["second"]);
    assert_eq!(f.found("ref:main").len(), 3, "a branch is its whole history");
}

#[test]
fn stash_commits_are_not_search_results() {
    let f = Fx::new();
    f.commit("a", "first");
    f.write("a", "changed");
    f.q(&["stash", "push", "-qm", "wip-secret"]);
    assert_eq!(f.found("first"), ["first"]);
    assert!(f.found("wip-secret").is_empty());
    assert!(f.found("index on main").is_empty());
}

#[test]
fn an_address_with_regex_characters_finds_its_owner() {
    let f = Fx::new();
    f.commit_as("a", "plus", "Ann", "ann+dev@example.com");
    f.commit_as("b", "other", "Bob", "annxdev@example.com");
    assert_eq!(f.found("@:ann+dev@example.com"), ["plus"]);
}

#[test]
fn dates_git_cannot_read_are_said_to_be_wrong() {
    let f = Fx::new();
    f.commit("a", "first");
    let check = |q: &str| f.repo.check_search_dates(&parse_search(q));
    for bad in ["after:notadate", "before:notadate"] {
        let e = check(bad).expect_err(bad).to_string();
        assert!(e.contains("notadate"), "{e}");
    }
    for good in ["after:2024-01-31", "before:\"2 weeks ago\"", "after:yesterday", "before:now", "first"] {
        check(good).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
}

// ---- contributors

#[test]
fn mailmap_joins_a_person_and_their_heatmap_is_theirs_alone() {
    let f = Fx::new();
    f.commit_as("a", "old address", "Ann Old", "ann@old.com");
    f.commit_as("b", "new address", "Ann New", "ANN@new.com");
    f.commit_as("c", "someone else", "Jo Ann", "joann@new.com");
    f.write(".mailmap", "Ann New <ann@new.com> Ann Old <ann@old.com>\n");

    let people = f.repo.contributors("HEAD", false).unwrap();
    let ann: Vec<&Contributor> = people.iter().filter(|c| c.name.starts_with("Ann")).collect();
    assert_eq!(ann.len(), 1, "one person: {people:?}");
    assert_eq!(ann[0].commits, 2);

    let times = f.repo.commit_times("HEAD", Some(&ann[0].email), "1 year ago").unwrap();
    assert_eq!(times.len(), 2, "both her commits and not joann's, whichever case each is in");
    assert_eq!(f.repo.commit_times("HEAD", None, "1 year ago").unwrap().len(), 3);

    let searched = f.found(&format!("@:<{}>", ann[0].email));
    assert_eq!(searched.len(), 2, "Search their commits agrees: {searched:?}");
}

// ---- bisect

#[test]
fn a_finished_bisect_names_the_commit() {
    let f = Fx::new();
    let good = f.commit("a", "good one");
    let culprit = f.commit("b", "the bug");
    f.commit("c", "after it");
    f.q(&["bisect", "start", "HEAD", &good]);
    assert_eq!(f.repo.bisect_state().culprit, None);
    // Git has one commit left to look at, then knows.
    loop {
        let here = f.q(&["rev-parse", "HEAD"]);
        let verdict = if here == culprit || !f.q(&["log", "--format=%s", "-1"]).contains("good") { "bad" } else { "good" };
        f.run(ops::bisect_mark_commit(verdict, &here));
        if f.repo.bisect_state().culprit.is_some() {
            break;
        }
    }
    assert_eq!(f.repo.bisect_state().culprit.as_deref(), Some(culprit.as_str()));
}

// ---- starting work

#[test]
fn work_started_from_a_remote_branch_can_be_published() {
    let f = Fx::new();
    f.commit("a", "first");
    let bare = f.with_origin();
    assert_eq!(f.repo.default_branch().as_deref(), Some("origin/main"));

    f.run(ops::branch_start_work("feature/x", Some("origin/main")));
    assert_eq!(f.q(&["branch", "--show-current"]), "feature/x");
    let st = f.repo.status().unwrap();
    assert_eq!(st.upstream, None, "so the toolbar offers Publish, and counts nothing against main");
    f.commit("b", "work");
    f.q(&["push", "-q", "-u", "origin", "feature/x"]);
    let dir = format!("--git-dir={}", path_str(&bare));
    let pushed = git().run(None, &[&dir, "rev-parse", "refs/heads/feature/x"]).unwrap().text();
    assert_eq!(pushed.trim(), f.q(&["rev-parse", "HEAD"]));

    let wt = f.dir.path().join("wt");
    f.run(ops::worktree_start_work(&path_str(&wt), "feature/y", Some("origin/main")));
    assert!(!git().run_raw(Some(&f.root), &["config", "--get", "branch.feature/y.remote"]).unwrap().ok(), "no upstream");
    assert_eq!(git().run(Some(&wt), &["branch", "--show-current"]).unwrap().text().trim(), "feature/y");
}

// ---- worktrees

#[test]
fn a_locked_worktree_comes_off_only_with_the_double_force() {
    let f = Fx::new();
    f.commit("a", "first");
    let wt = path_str(&f.dir.path().join("wt"));
    f.q(&["worktree", "add", "-q", "-b", "side", &wt]);
    f.q(&["worktree", "lock", &wt]);
    assert!(f.fails(ops::worktree_remove(&wt, true)).contains("locked"), "what the menu used to offer");
    f.run(ops::worktree_remove_locked(&wt));
    assert!(!Path::new(&wt).exists());
}

// ---- treemap and clean

#[test]
fn activity_leaves_out_files_that_are_gone() {
    let f = Fx::new();
    f.commit("keep.txt", "one");
    f.commit("gone.txt", "two");
    f.q(&["rm", "-q", "gone.txt"]);
    f.q(&["commit", "-qm", "remove"]);
    let names: Vec<String> = f.repo.file_activity("HEAD", 100).unwrap().into_iter().map(|x| x.0).collect();
    assert_eq!(names, ["keep.txt"]);
}

#[test]
fn clean_says_which_repositories_it_leaves_alone() {
    let f = Fx::new();
    f.commit("a", "first");
    f.write("junk/file.txt", "x");
    f.write("nested/inner.txt", "y");
    git().run(Some(&f.root.join("nested")), &["init", "-q"]).unwrap();
    assert_eq!(f.repo.clean_preview(true, false).unwrap(), ["junk/"]);
    assert_eq!(f.repo.clean_left_alone().unwrap(), ["nested/"]);
    f.run(ops::clean(true, false));
    assert!(f.root.join("nested/inner.txt").exists(), "as the page says");
    assert!(!f.root.join("junk").exists());
}

// ---- review

#[test]
fn the_versions_of_changed_files_can_be_told_apart() {
    let f = Fx::new();
    f.commit("a", "first");
    f.q(&["switch", "-qc", "side"]);
    f.commit("b", "one");
    let before = f.repo.compare_blobs("main", "HEAD").unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before["b"].len(), 40);
    f.commit("b", "a second, longer version");
    assert_ne!(f.repo.compare_blobs("main", "HEAD").unwrap()["b"], before["b"]);
}

// ---- stash

/// The way forward the refusal gives: stage them, then stash what's staged.
#[test]
fn a_list_too_long_for_stash_goes_by_staging_it() {
    let f = Fx::new();
    let long = "a-long-folder-name-to-use-up-the-command-line".repeat(2);
    let names: Vec<String> = (0..1200).map(|i| format!("{long}/file-{i}-with-a-long-file-name-too.txt")).collect();
    for n in &names {
        f.write(n, "one\n");
    }
    f.q(&["add", "-A"]);
    f.q(&["commit", "-qm", "all"]);
    for n in &names {
        f.write(n, "two\n");
    }
    f.write("other.txt", "stays out of it\n");

    let too_many = ops::stash_push(&ops::StashOpts { paths: names.clone(), ..Default::default() });
    let said = too_many.run(&git(), Some(&f.root), None, &mut |_| {}).unwrap_err().to_string();
    assert!(said.contains("Stage them and stash what's staged"), "{said}");

    f.run(ops::stage_paths(&names));
    f.run(ops::stash_push(&ops::StashOpts { staged_only: true, ..Default::default() }));
    assert_eq!(f.q(&["status", "--short"]), "?? other.txt", "they're in the stash, and not in the tree");
    f.q(&["stash", "pop", "-q"]);
    assert_eq!(std::fs::read_to_string(f.root.join(&names[7])).unwrap().trim(), "two");
}
