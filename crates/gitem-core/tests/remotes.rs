//! Fetch, pull, push and clone actually run, against a bare repository in a temp
//! folder. Nothing here leaves the machine.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gitem_core::model::*;
use gitem_core::ops::{self, FetchOpts, GitOp, PullMode, PushOpts};
use gitem_core::{GitExe, Repo};

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    g.helper = None;
    g
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A bare remote and two clones of it, so one can move the remote under the other.
struct Net {
    dir: tempfile::TempDir,
    bare: PathBuf,
    a: Clone_,
    b: Clone_,
}

struct Clone_ {
    repo: Repo,
}

fn configure(g: &GitExe, p: &Path) {
    for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        g.run(Some(p), &["config", k, v]).unwrap();
    }
}

impl Clone_ {
    fn at(url: &str, dest: &Path) -> Self {
        let g = git();
        run_in(&g, None, ops::clone_repo(url, &path_str(dest), None, false, None));
        configure(&g, dest);
        // The user's autocrlf applied during the clone.
        g.run(Some(dest), &["reset", "--hard", "-q"]).ok();
        Self { repo: Repo::open(g, dest).unwrap() }
    }
    fn run(&self, op: GitOp) {
        run_in(&self.repo.git, Some(&self.repo.root), op);
    }
    fn fails(&self, op: GitOp) -> String {
        match op.run(&self.repo.git, Some(&self.repo.root), None, &mut |_| {}) {
            Ok(_) => panic!("{} should have failed", op.title),
            Err(e) => e.to_string(),
        }
    }
    fn write(&self, p: &str, s: &str) {
        std::fs::write(self.repo.root.join(p), s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.repo.root.join(p)).unwrap()
    }
    fn commit(&self, file: &str, text: &str, msg: &str) -> String {
        self.write(file, text);
        self.run(ops::stage_all());
        self.run(ops::commit(&ops::CommitOpts { message: msg.into(), ..Default::default() }));
        self.head()
    }
    fn head(&self) -> String {
        self.repo.resolve("HEAD").unwrap()
    }
    fn status(&self) -> RepoStatus {
        self.repo.status().unwrap()
    }
    fn subjects(&self) -> Vec<String> {
        self.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap().into_iter().map(|c| c.subject).collect()
    }
    fn branch(&self, name: &str) -> Option<Branch> {
        self.repo.branches().unwrap().into_iter().find(|b| b.name == name)
    }
    fn fetch(&self) {
        self.run(ops::fetch(&FetchOpts::default()));
    }
    fn push(&self) {
        self.run(ops::push(&PushOpts::default()));
    }
}

fn init_bare(p: &Path) {
    git().run(None, &["init", "-q", "--bare", "-b", "main", &path_str(p)]).unwrap();
}

fn run_in(g: &GitExe, cwd: Option<&Path>, op: GitOp) {
    op.run(g, cwd, None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
}

impl Net {
    /// `a` has pushed one commit to `main`; `b` has cloned it.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("remote.git");
        init_bare(&bare);
        let url = path_str(&bare);
        let a = Clone_::at(&url, &dir.path().join("a"));
        a.commit("f.txt", "one\ntwo\nthree\n", "first");
        a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some("main".into()), set_upstream: true, ..Default::default() }));
        let b = Clone_::at(&url, &dir.path().join("b"));
        Self { dir, bare, a, b }
    }
    fn url(&self) -> String {
        path_str(&self.bare)
    }
    fn remote_sha(&self, r: &str) -> Option<String> {
        let o = git().run_raw(Some(&self.bare), &["--git-dir=.", "rev-parse", "--verify", "-q", r]).unwrap();
        o.ok().then(|| o.text().trim().to_string())
    }
}

#[test]
fn cloning_an_empty_remote_then_pushing_sets_up_tracking() {
    let n = Net::new();
    let st = n.a.status();
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.ahead, st.behind), (0, 0));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.a.head());
    assert_eq!(n.b.head(), n.a.head());
    assert_eq!(n.b.status().upstream.as_deref(), Some("origin/main"));
    assert_eq!(n.a.repo.remotes().unwrap()[0].name, "origin");
}

#[test]
fn clone_options_take_effect() {
    let n = Net::new();
    n.a.commit("f.txt", "changed\n", "second");
    n.a.run(ops::branch_create("feature", None, true));
    n.a.commit("g.txt", "g\n", "on feature");
    n.a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some("feature".into()), set_upstream: true, ..Default::default() }));
    n.a.run(ops::checkout("main"));
    n.a.push();

    // Shallow clones are ignored for plain paths, so this goes through file://.
    let url = format!("file:///{}", n.url().trim_start_matches('/'));
    let dest = n.dir.path().join("shallow");
    run_in(&git(), None, ops::clone_repo(&url, &path_str(&dest), Some(1), false, Some("feature")));
    let c = Repo::open(git(), &dest).unwrap();
    assert_eq!(c.status().unwrap().branch.as_deref(), Some("feature"));
    let log = c.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(log.len(), 1, "depth 1");
    assert_eq!(log[0].subject, "on feature");
}

#[test]
fn a_clone_url_cannot_be_read_as_an_option() {
    let d = tempfile::tempdir().unwrap();
    let marker = d.path().join("ran");
    let url = format!("--upload-pack=touch {}", path_str(&marker));
    let dest = d.path().join("dest");
    let r = ops::clone_repo(&url, &path_str(&dest), None, false, None).run(&git(), Some(d.path()), None, &mut |_| {});
    assert!(r.is_err());
    assert!(!marker.exists(), "the URL ran as a command");
}

#[test]
fn fetch_reports_behind_without_moving_the_branch() {
    let n = Net::new();
    let before = n.b.head();
    n.a.commit("f.txt", "changed\n", "second");
    n.a.push();
    assert_eq!(n.b.status().behind, 0, "nothing fetched yet");
    n.b.fetch();
    let st = n.b.status();
    assert_eq!((st.ahead, st.behind), (0, 1));
    assert_eq!(n.b.head(), before);
    assert_eq!(n.b.branch("main").unwrap().behind, 1);
}

#[test]
fn fetch_all_covers_every_remote() {
    let n = Net::new();
    let second = n.dir.path().join("second.git");
    init_bare(&second);
    n.a.run(ops::remote_add("second", &path_str(&second)));
    n.a.run(ops::push(&PushOpts { remote: Some("second".into()), refspec: Some("main".into()), ..Default::default() }));
    n.b.run(ops::remote_add("second", &path_str(&second)));
    n.b.run(ops::fetch(&FetchOpts { all: true, ..Default::default() }));
    assert!(n.b.repo.resolve("refs/remotes/second/main").is_ok());
    assert_eq!(n.b.repo.remotes().unwrap().len(), 2);
}

#[test]
fn prune_removes_branches_deleted_on_the_remote_and_flags_their_locals() {
    let n = Net::new();
    n.a.run(ops::branch_create("gone", None, true));
    n.a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some("gone".into()), set_upstream: true, ..Default::default() }));
    n.a.run(ops::checkout("main"));
    n.b.fetch();
    assert!(n.b.branch("origin/gone").is_some());
    n.a.run(ops::branch_delete_remote("origin/gone", &[]));
    assert!(n.remote_sha("refs/heads/gone").is_none());
    assert!(n.a.branch("gone").unwrap().upstream_gone, "the local branch's upstream is gone");
    n.b.fetch();
    assert!(n.b.branch("origin/gone").is_some(), "plain fetch keeps it");
    n.b.run(ops::fetch(&FetchOpts { prune: true, ..Default::default() }));
    assert!(n.b.branch("origin/gone").is_none());
}

#[test]
fn pull_fast_forwards() {
    let n = Net::new();
    let tip = n.a.commit("f.txt", "changed\n", "second");
    n.a.push();
    n.b.run(ops::pull(PullMode::FfOnly, false));
    assert_eq!(n.b.head(), tip);
    assert_eq!(n.b.read("f.txt"), "changed\n");
}

fn diverged() -> Net {
    let n = Net::new();
    n.a.commit("from_a.txt", "a\n", "from a");
    n.a.push();
    n.b.commit("from_b.txt", "b\n", "from b");
    n
}

#[test]
fn ff_only_refuses_diverged_history() {
    let n = diverged();
    let before = n.b.head();
    let e = n.b.fails(ops::pull(PullMode::FfOnly, false));
    assert!(e.to_lowercase().contains("fast-forward") || e.contains("Not possible"), "{e}");
    assert_eq!(n.b.head(), before);
}

#[test]
fn pull_rebase_replays_local_work() {
    let n = diverged();
    n.b.run(ops::pull(PullMode::Rebase, false));
    assert_eq!(n.b.subjects(), ["from b", "from a", "first"]);
    assert_eq!((n.b.status().ahead, n.b.status().behind), (1, 0));
}

#[test]
fn pull_merge_creates_a_merge_commit() {
    let n = diverged();
    n.b.run(ops::pull(PullMode::Merge, false));
    assert!(n.b.subjects()[0].starts_with("Merge branch"), "{:?}", n.b.subjects());
    assert_eq!(n.b.status().ahead, 2);
}

#[test]
fn pull_autostash_keeps_uncommitted_changes() {
    let n = diverged();
    n.b.write("f.txt", "uncommitted\n");
    n.b.run(ops::pull(PullMode::Rebase, true));
    assert_eq!(n.b.read("f.txt"), "uncommitted\n");
    assert_eq!(n.b.subjects()[0], "from b");
    assert!(n.b.repo.stashes().unwrap().is_empty(), "the autostash was reapplied");
}

#[test]
fn pull_conflicts_surface_as_unmerged_files() {
    let n = Net::new();
    n.a.commit("f.txt", "theirs\n", "from a");
    n.a.push();
    n.b.commit("f.txt", "ours\n", "from b");
    let e = n.b.fails(ops::pull(PullMode::Merge, false));
    assert!(e.contains("CONFLICT (content): Merge conflict in f.txt"), "{e}");
    assert!(!e.contains("Counting objects"), "progress isn't part of the message: {e}");
    let st = n.b.status();
    assert!(st.entries.iter().any(|x| x.path == "f.txt" && x.conflict.is_some()), "{:?}", st.entries);
    assert!(n.b.read("f.txt").contains("<<<<<<<"));
}

#[test]
fn rejected_push_leaves_the_remote_alone_and_says_why() {
    let n = diverged();
    let remote = n.remote_sha("refs/heads/main").unwrap();
    let e = n.b.fails(ops::push(&PushOpts::default()));
    assert!(e.contains("rejected") || e.contains("non-fast-forward"), "{e}");
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), remote);
}

#[test]
fn force_with_lease_checks_what_we_last_saw() {
    let n = diverged();
    // A fetch moves what a bare lease compares with, without anyone having looked at the
    // commits it brought. Pushing now would overwrite them, so it's refused.
    n.b.fetch();
    let theirs = n.remote_sha("refs/heads/main").unwrap();
    let e = n.b.fails(ops::push(&PushOpts { force_with_lease: true, ..Default::default() }));
    assert!(e.contains("rejected") || e.contains("stale"), "{e}");
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), theirs, "their commits are still there");

    // Having taken them in, the lease holds.
    n.b.run(ops::rebase(&ops::RebaseOpts { onto: "origin/main".into(), ..Default::default() }));
    n.b.run(ops::push(&PushOpts { force_with_lease: true, ..Default::default() }));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.b.head());

    // Rewriting your own pushed commit is what the option is for, and it still works.
    n.b.run(ops::GitOp::new("amend").step(&["commit", "--amend", "-q", "-m", "reworded"]));
    n.b.run(ops::push(&PushOpts { force_with_lease: true, ..Default::default() }));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.b.head());

    // `a` hasn't seen that, so its lease fails even though it fetched earlier.
    n.a.commit("again.txt", "x\n", "unseen");
    let tip = n.remote_sha("refs/heads/main").unwrap();
    let e = n.a.fails(ops::push(&PushOpts { force_with_lease: true, ..Default::default() }));
    assert!(e.contains("stale info") || e.contains("rejected"), "{e}");
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), tip);

    // A plain force doesn't check.
    n.a.run(ops::push(&PushOpts { force: true, ..Default::default() }));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.a.head());
}

#[test]
fn tags_reach_the_remote_and_can_be_removed() {
    let n = Net::new();
    n.a.run(ops::tag_create("v1", None, Some("one"), false));
    n.a.run(ops::tag_push("origin", "v1"));
    assert!(n.remote_sha("refs/tags/v1").is_some());
    n.b.run(ops::fetch(&FetchOpts { tags: true, ..Default::default() }));
    assert!(n.b.repo.tags().unwrap().iter().any(|t| t.name == "v1"));
    n.a.run(ops::tag_delete_remote("origin", "v1"));
    assert!(n.remote_sha("refs/tags/v1").is_none());

    // --follow-tags sends annotated tags on commits being pushed.
    n.a.commit("f.txt", "x\n", "tagged");
    n.a.run(ops::tag_create("v2", None, Some("two"), false));
    n.a.run(ops::push(&PushOpts { tags: true, remote: Some("origin".into()), refspec: Some("main".into()), ..Default::default() }));
    assert!(n.remote_sha("refs/tags/v2").is_some());
}

#[test]
fn push_up_to_a_commit_leaves_later_ones_local() {
    let n = Net::new();
    let middle = n.a.commit("f.txt", "1\n", "middle");
    n.a.commit("f.txt", "2\n", "latest");
    n.a.run(ops::push_to_commit("origin", &middle, "main"));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), middle);
    n.a.fetch();
    assert_eq!(n.a.status().ahead, 1);
}

#[test]
fn pull_request_refs_fetch_into_a_local_branch() {
    let n = Net::new();
    let sha = n.a.commit("pr.txt", "pr\n", "pull request work");
    n.a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some(format!("{sha}:refs/pull/7/head")), ..Default::default() }));
    n.b.run(ops::fetch_pr("origin", "refs/pull/7/head", "pr-7"));
    assert_eq!(n.b.branch("pr-7").unwrap().sha, sha);
}

#[test]
fn a_pull_requests_branch_says_which_commits_a_fetch_would_replace() {
    let n = Net::new();
    let first = n.a.commit("pr.txt", "pr\n", "pull request work");
    n.a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some(format!("{first}:refs/pull/7/head")), ..Default::default() }));
    assert_eq!(n.b.repo.commits_a_fetch_would_replace("pr-7").unwrap(), None, "no such branch");
    n.b.run(ops::fetch_pr("origin", "refs/pull/7/head", "pr-7"));
    let replaced = |n: &Net| n.b.repo.commits_a_fetch_would_replace("pr-7").unwrap().unwrap().1;
    assert!(replaced(&n).is_empty(), "only what was fetched: {:?}", replaced(&n));

    n.b.run(ops::checkout("pr-7"));
    n.b.commit("mine.txt", "mine\n", "mine");
    assert_eq!(replaced(&n), ["mine"]);

    // What's on a remote can come back, so it doesn't count.
    n.b.run(ops::push_to_upstream(Some("pr-7"), Some("origin/elsewhere"), &["origin"]));
    assert_eq!(n.remote_sha("refs/heads/elsewhere").unwrap(), n.b.branch("pr-7").unwrap().sha);
    n.b.fetch();
    assert!(replaced(&n).is_empty(), "{:?}", replaced(&n));

    // Update and Reset to its tip count as coming from the host, and what's new after them doesn't.
    let tip = n.a.commit("pr.txt", "pr 2\n", "more pull request work");
    n.a.run(ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some(format!("{tip}:refs/pull/7/head")), force: true, ..Default::default() }));
    n.b.run(ops::reset_pr_branch("origin", "refs/pull/7/head"));
    assert!(replaced(&n).is_empty(), "{:?}", replaced(&n));
    n.b.commit("mine2.txt", "mine\n", "mine again");
    assert_eq!(replaced(&n), ["mine again"]);
}

/// What real git prints, as the toast's end says it.
#[test]
fn finished_network_ops_say_what_happened() {
    use gitem_core::GitExe;
    let n = Net::new();
    let git = GitExe::discover(None).unwrap();
    let say = |repo: &gitem_core::Repo, op: ops::GitOp| {
        let out = op.run(&git, Some(&repo.root), None, &mut |_| {}).unwrap();
        op.outcome_note(&out).unwrap_or_default()
    };
    let push = || ops::push(&PushOpts { remote: Some("origin".into()), refspec: Some("main".into()), ..Default::default() });
    let fetch = || ops::fetch(&FetchOpts::default());
    n.a.commit("f.txt", "1\n", "one");
    assert_eq!(say(&n.a.repo, push()), "main to main");
    assert_eq!(say(&n.a.repo, push()), "already up to date");
    assert_eq!(say(&n.b.repo, fetch()), "1 ref updated");
    assert_eq!(say(&n.b.repo, fetch()), "nothing new");
    assert!(say(&n.b.repo, ops::pull(ops::PullMode::FfOnly, false)).contains("1 file changed"));
    assert_eq!(say(&n.b.repo, ops::pull(ops::PullMode::FfOnly, false)), "already up to date");
    assert_eq!(say(&n.b.repo, ops::merge("HEAD", ops::MergeMode::Default)), "already up to date");
    assert_eq!(n.a.commit("g.txt", "1\n", "two").len(), 40);
    n.a.write("g.txt", "2\n");
    let branch = n.a.repo.status().unwrap().branch.unwrap();
    assert_eq!(say(&n.a.repo, ops::stash_push(&Default::default())), format!("from {branch}"));
    assert_eq!(say(&n.a.repo, ops::stash_push(&Default::default())), "nothing to stash");
}

#[test]
fn remotes_can_be_managed() {
    let n = Net::new();
    n.b.run(ops::remote_add("extra", "https://example.invalid/x.git"));
    n.b.run(ops::remote_rename("extra", "renamed"));
    n.b.run(ops::remote_set_url("renamed", &n.url()));
    n.b.run(ops::fetch(&FetchOpts { remote: Some("renamed".into()), ..Default::default() }));
    assert!(n.b.branch("renamed/main").is_some());
    n.b.run(ops::remote_prune("renamed"));
    n.b.run(ops::remote_remove("renamed"));
    assert_eq!(n.b.repo.remotes().unwrap().len(), 1);
    assert!(n.b.branch("renamed/main").is_none());
}

#[test]
fn hooks_block_pushes_and_no_verify_skips_only_the_local_one() {
    let n = Net::new();
    let local = n.a.repo.root.join(".git/hooks/pre-push");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    write_script(&local, "#!/bin/sh\necho local hook says no >&2\nexit 1\n");
    n.a.commit("f.txt", "1\n", "one");
    let e = n.a.fails(ops::push(&PushOpts::default()));
    assert!(e.contains("local hook says no"), "{e}");
    n.a.run(ops::push(&PushOpts { no_verify: true, ..Default::default() }));
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.a.head());

    let server = n.bare.join("hooks/pre-receive");
    std::fs::create_dir_all(server.parent().unwrap()).unwrap();
    write_script(&server, "#!/bin/sh\necho server policy refuses >&2\nexit 1\n");
    n.a.commit("f.txt", "2\n", "two");
    let e = n.a.fails(ops::push(&PushOpts { no_verify: true, ..Default::default() }));
    assert!(e.contains("server policy refuses"), "{e}");
}

#[test]
fn a_missing_remote_is_a_readable_failure() {
    let n = Net::new();
    n.b.run(ops::remote_set_url("origin", &path_str(&n.dir.path().join("nothing-here.git"))));
    let e = n.b.fails(ops::fetch(&FetchOpts::default()));
    assert!(e.contains("does not appear to be a git repository") || e.contains("not found"), "{e}");
    assert!(n.b.fails(ops::pull(PullMode::Default, false)).len() > 5);
    assert!(n.b.fails(ops::push(&PushOpts::default())).len() > 5);
}

#[test]
fn network_ops_stream_output_and_can_be_cancelled() {
    let n = Net::new();
    // Enough objects that git has something to count.
    for i in 0..40 {
        n.a.write(&format!("file{i}.txt"), &format!("{i}\n"));
    }
    n.a.commit("x.txt", "x\n", "many files");
    let mut lines = Vec::new();
    ops::push(&PushOpts::default()).run(&n.a.repo.git, Some(&n.a.repo.root), None, &mut |l| lines.push(l.to_string())).unwrap();
    assert!(lines.iter().any(|l| l.contains("Writing objects") || l.contains("main -> main")), "{lines:?}");

}

#[test]
fn cancelling_returns_promptly_even_when_a_child_process_holds_the_pipes() {
    let n = Net::new();
    let hook = n.a.repo.root.join(".git/hooks/pre-push");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, "#!/bin/sh
sleep 25
");
    n.a.commit("f.txt", "1
", "one");

    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(700));
        c.store(true, Ordering::Relaxed);
    });
    let start = std::time::Instant::now();
    let r = ops::push(&PushOpts::default()).run(&n.a.repo.git, Some(&n.a.repo.root), Some(cancel), &mut |_| {});
    assert!(r.is_err(), "a cancelled push is a failure");
    assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
}

#[test]
fn a_background_process_left_by_a_hook_does_not_hold_up_the_op() {
    let n = Net::new();
    let hook = n.a.repo.root.join(".git/hooks/pre-push");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, "#!/bin/sh\nsleep 25 &\nexit 0\n");
    n.a.commit("f.txt", "1\n", "one");
    let start = std::time::Instant::now();
    n.a.push();
    assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
    assert_eq!(n.remote_sha("refs/heads/main").unwrap(), n.a.head());
}

/// A script git or the shell will run: written, and on Unix marked executable, which they need.
fn write_script(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
