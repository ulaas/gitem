//! End-to-end tests against the real git binary, in throwaway temp repositories
//! only. Fetch, pull, push and clone are in `remotes.rs`.
gitem_testenv::isolate!();

use std::path::Path;

use gitem_core::diff::{LineKind, PatchMode, build_patch};
use gitem_core::merge::{MergeDoc, Resolution};
use gitem_core::model::*;
use gitem_core::ops::{self, GitOp};
use gitem_core::rebase::{RebaseAction, TodoItem, render_todo};
use gitem_core::repo::{BlameOpts, DiffOpts, DiffSource, parse_search};
use gitem_core::{GitExe, Repo, graph};

struct Fx {
    _dir: tempfile::TempDir,
    repo: Repo,
}

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    // The test binary must not be used as editor or askpass helper.
    g.helper = None;
    g
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let g = git();
        g.run(Some(dir.path()), &["init", "-q", "-b", "main"]).unwrap();
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            g.run(Some(dir.path()), &["config", k, v]).unwrap();
        }
        let repo = Repo::open(g, dir.path()).unwrap();
        Self { _dir: dir, repo }
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.repo.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn run(&self, op: GitOp) {
        op.run(&self.repo.git, Some(&self.repo.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
    fn try_run(&self, op: GitOp) -> bool {
        op.run(&self.repo.git, Some(&self.repo.root), None, &mut |_| {}).is_ok()
    }
    fn commit_all(&self, msg: &str) -> String {
        self.run(ops::stage_all());
        self.run(ops::commit(&ops::CommitOpts { message: msg.into(), ..Default::default() }));
        self.repo.resolve("HEAD").unwrap()
    }
    fn subjects(&self) -> Vec<String> {
        self.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap().into_iter().map(|c| c.subject).collect()
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.repo.root.join(p)).unwrap()
    }
}

#[test]
fn empty_repo_is_handled() {
    let f = Fx::new();
    let st = f.repo.status().unwrap();
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert!(st.head_sha.is_empty());
    assert!(f.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap().is_empty());
    assert!(f.repo.log(&LogQuery { all: true, ..Default::default() }).unwrap().is_empty());
    assert!(f.repo.branches().unwrap().is_empty());
}

#[test]
fn commit_log_and_details() {
    let f = Fx::new();
    f.write("a.txt", "one\ntwo\n");
    let s1 = f.commit_all("first\n\nthe body");
    f.write("a.txt", "one\nTWO\n");
    f.write("sp ace/b c.txt", "x\n");
    let s2 = f.commit_all("second");

    let log = f.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].sha, s2);
    assert_eq!(log[0].parents, vec![s1.clone()]);
    assert_eq!(log[1].body, "the body");
    assert_eq!(log[0].author_email, "test@example.com");
    assert!(log[0].refs.iter().any(|r| r.name == "main" && r.current));

    let files = f.repo.commit_files(&s2).unwrap();
    assert_eq!(files.len(), 2);
    let a = files.iter().find(|x| x.path == "a.txt").unwrap();
    assert_eq!((a.additions, a.deletions), (Some(1), Some(1)));
    assert!(files.iter().any(|x| x.path == "sp ace/b c.txt" && x.status == FileStatus::Added));
    // Root commit diffs work too.
    assert_eq!(f.repo.commit_files(&s1).unwrap().len(), 1);
    let d = f.repo.diff(&DiffSource::Commit(s1.clone()), None, &DiffOpts::default()).unwrap();
    assert!(d[0].is_new);
}

#[test]
fn status_all_states_and_unicode_paths() {
    let f = Fx::new();
    f.write("mod.txt", "1\n");
    f.write("del.txt", "1\n");
    f.write("ren.txt", "some content that is long enough to be detected as a rename\n");
    f.commit_all("base");
    f.write("mod.txt", "2\n");
    std::fs::remove_file(f.repo.root.join("del.txt")).unwrap();
    f.write("ünï côdé.txt", "u\n");
    f.write("staged.txt", "s\n");
    f.run(ops::stage_paths(&["staged.txt".into()]));
    f.repo.git.run(Some(&f.repo.root), &["mv", "ren.txt", "renamed.txt"]).unwrap();
    let st = f.repo.status().unwrap();
    let e = |p: &str| st.entries.iter().find(|x| x.path == p).unwrap_or_else(|| panic!("{p}: {:?}", st.entries));
    assert_eq!(e("mod.txt").worktree, FileStatus::Modified);
    assert_eq!(e("del.txt").worktree, FileStatus::Deleted);
    assert_eq!(e("ünï côdé.txt").worktree, FileStatus::Untracked);
    assert_eq!(e("staged.txt").index, FileStatus::Added);
    assert!(e("staged.txt").is_staged());
    assert_eq!(e("renamed.txt").index, FileStatus::Renamed);
    assert_eq!(e("renamed.txt").orig_path.as_deref(), Some("ren.txt"));
}

#[test]
fn untracked_diff_uses_no_index() {
    let f = Fx::new();
    f.write("base.txt", "b\n");
    f.commit_all("base");
    f.write("new file.txt", "hello\nworld\n");
    let d = f.repo.diff(&DiffSource::Untracked, Some("new file.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].additions(), 2);
}

#[test]
fn compare_arbitrary_files() {
    let f = Fx::new();
    f.write("base.txt", "b\n");
    f.commit_all("base");
    f.write("x.txt", "a\nb\n");
    f.write("y.txt", "a\nc\n");
    let d = f.repo.diff(&DiffSource::Files("x.txt".into(), "y.txt".into()), None, &DiffOpts::default()).unwrap();
    assert_eq!(d[0].additions(), 1);
    assert_eq!(d[0].deletions(), 1);
}

#[test]
fn hunk_and_line_staging_roundtrip() {
    let f = Fx::new();
    let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("f.txt", &base);
    f.commit_all("base");
    let changed = base.replace("line 2\n", "line 2 changed\n").replace("line 25\n", "line 25 changed\nextra\n");
    f.write("f.txt", &changed);

    let d = f.repo.diff(&DiffSource::Unstaged, Some("f.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(d[0].hunks.len(), 2);

    // Stage only the first hunk.
    let p = build_patch(&d[0], 0, None, PatchMode::Forward).unwrap();
    f.run(ops::apply_patch("stage", p, true, false));
    let staged = f.repo.diff(&DiffSource::Staged, Some("f.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(staged[0].hunks.len(), 1);
    assert_eq!(staged[0].additions(), 1);
    assert_eq!(f.read("f.txt"), changed, "worktree untouched");

    // Stage a single added line of the second hunk.
    let d = f.repo.diff(&DiffSource::Unstaged, Some("f.txt"), &DiffOpts::default()).unwrap();
    let h = &d[0].hunks[0];
    let extra = h.lines.iter().position(|l| l.text == "extra").unwrap();
    let p = build_patch(&d[0], 0, Some(&[extra]), PatchMode::Forward).unwrap();
    f.run(ops::apply_patch("stage line", p, true, false));
    let staged = f.repo.diff(&DiffSource::Staged, Some("f.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(staged[0].additions(), 2);
    assert!(staged[0].hunks[1].lines.iter().any(|l| l.text == "extra" && l.kind == gitem_core::diff::LineKind::Add));
    assert!(!staged[0].hunks[1].lines.iter().any(|l| l.text == "line 25 changed"));

    // Unstage that line again.
    let h = &staged[0].hunks[1];
    let extra = h.lines.iter().position(|l| l.text == "extra").unwrap();
    let p = build_patch(&staged[0], 1, Some(&[extra]), PatchMode::Reverse).unwrap();
    f.run(ops::apply_patch("unstage line", p, true, true));
    let staged = f.repo.diff(&DiffSource::Staged, Some("f.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(staged[0].hunks.len(), 1);

    // Discard one worktree hunk.
    let d = f.repo.diff(&DiffSource::Unstaged, Some("f.txt"), &DiffOpts::default()).unwrap();
    let last = d[0].hunks.len() - 1;
    let p = build_patch(&d[0], last, None, PatchMode::Reverse).unwrap();
    f.run(ops::apply_patch("discard", p, false, true));
    assert!(!f.read("f.txt").contains("extra"));
    assert!(f.read("f.txt").contains("line 2 changed"));
}

#[test]
fn blame_and_history() {
    let f = Fx::new();
    f.write("f.txt", "a\nb\nc\n");
    let s1 = f.commit_all("one");
    f.write("f.txt", "a\nB\nc\n");
    let s2 = f.commit_all("two");
    f.write("f.txt", "a\nB\nc\nuncommitted\n");
    let b = f.repo.blame("f.txt", None, BlameOpts::default()).unwrap();
    assert_eq!(b.len(), 4);
    assert_eq!(b[0].sha, s1);
    assert_eq!(b[1].sha, s2);
    assert_eq!(b[1].summary, "two");
    assert_eq!(b[1].text, "B");
    assert!(b[3].is_uncommitted());
    let at1 = f.repo.blame("f.txt", Some(&s1), BlameOpts::default()).unwrap();
    assert_eq!(at1.len(), 3);

    assert_eq!(f.repo.file_at(&s1, "f.txt").unwrap(), b"a\nb\nc\n");

    // Line history
    let l = f.repo.log(&LogQuery { line_range: Some((2, 2, "f.txt".into())), revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(l.iter().map(|c| c.subject.clone()).collect::<Vec<_>>(), vec!["two", "one"]);

    // Revision walking
    assert_eq!(f.repo.prev_revision("f.txt", &s2), Some((s1.clone(), "f.txt".to_string())));
    assert_eq!(f.repo.next_revision("f.txt", &s1).as_deref(), Some(s2.as_str()));
    assert_eq!(f.repo.prev_revision("f.txt", &s1), None);
}

#[test]
fn file_history_follows_renames_with_stats() {
    let f = Fx::new();
    let content: String = (0..20).map(|i| format!("row {i}\n")).collect();
    f.write("old.txt", &content);
    f.commit_all("add");
    f.repo.git.run(Some(&f.repo.root), &["mv", "old.txt", "new.txt"]).unwrap();
    f.commit_all("move");
    f.write("new.txt", &format!("{content}more\n"));
    f.commit_all("edit");
    let pts = f.repo.file_history_points("new.txt", 100).unwrap();
    assert_eq!(pts.len(), 3);
    assert_eq!(pts[0].subject, "edit");
    assert_eq!(pts[0].additions, 1);
    let log = f.repo.log(&LogQuery { paths: vec!["new.txt".into()], follow: true, revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(log.len(), 3);
}

#[test]
fn branches_tags_tracking_and_graph() {
    let f = Fx::new();
    f.write("a", "1\n");
    let s1 = f.commit_all("c1");
    f.run(ops::branch_create("feature/x", None, true));
    f.write("b", "1\n");
    f.commit_all("c2");
    f.run(ops::checkout("main"));
    f.write("c", "1\n");
    f.commit_all("c3");
    f.run(ops::merge("feature/x", ops::MergeMode::NoFf));
    f.run(ops::tag_create("v1", None, Some("rel"), false));
    f.run(ops::tag_create("light", Some(&s1), None, false));

    let bs = f.repo.branches().unwrap();
    assert_eq!(bs.len(), 2);
    assert!(bs.iter().any(|b| b.name == "main" && b.is_current));
    assert!(bs.iter().any(|b| b.name == "feature/x" && !b.is_remote));

    let ts = f.repo.tags().unwrap();
    assert_eq!(ts.len(), 2);
    let v1 = ts.iter().find(|t| t.name == "v1").unwrap();
    assert!(v1.annotated);
    assert_eq!(v1.sha, f.repo.resolve("HEAD").unwrap(), "annotated tag peeled to its commit");
    assert_eq!(ts.iter().find(|t| t.name == "light").unwrap().sha, s1);

    let commits = f.repo.log(&LogQuery { all: true, ..Default::default() }).unwrap();
    assert_eq!(commits.len(), 4);
    // A slash-named local branch must not be labelled as remote.
    let labels: Vec<_> = commits.iter().flat_map(|c| c.refs.clone()).collect();
    assert_eq!(labels.iter().find(|r| r.name == "feature/x").unwrap().kind, RefKind::Branch);

    let rows = graph::layout(&commits);
    assert_eq!(rows.len(), 4);
    assert!(rows[0].bottom.len() >= 2, "merge has two outgoing edges");
    assert!(rows.iter().any(|r| r.column > 0), "side branch uses a second lane");

    let (ahead, behind) = f.repo.ahead_behind("main", "feature/x").unwrap();
    assert_eq!((ahead, behind), (2, 0));
    assert_eq!(f.repo.merge_base("main", "feature/x").unwrap(), f.repo.resolve("feature/x").unwrap());
    assert!(f.repo.is_ancestor("feature/x", "main"));
    let (b, t) = f.repo.containing_refs(&s1).unwrap();
    assert!(b.contains(&"main".to_string()));
    assert!(t.contains(&"light".to_string()));
    assert_eq!(f.repo.default_branch().as_deref(), Some("main"));

    f.run(ops::branch_rename("feature/x", "feature/y"));
    f.run(ops::branch_delete("feature/y", false));
    f.run(ops::tag_delete("light"));
    assert_eq!(f.repo.branches().unwrap().len(), 1);
    assert_eq!(f.repo.tags().unwrap().len(), 1);
}

#[test]
fn compare_and_search() {
    let f = Fx::new();
    f.write("a.txt", "needle one\n");
    f.commit_all("feat: alpha");
    f.run(ops::branch_create("dev", None, true));
    f.write("b.txt", "hay\n");
    f.commit_all("fix: beta");
    let files = f.repo.compare_files("main", "dev", false).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "b.txt");
    assert_eq!(files[0].additions, Some(1));

    let r = f.repo.log(&parse_search("=:beta")).unwrap();
    assert_eq!(r.len(), 1);
    let r = f.repo.log(&parse_search("change:needle")).unwrap();
    assert_eq!(r[0].subject, "feat: alpha");
    let r = f.repo.log(&parse_search("file:b.txt")).unwrap();
    assert_eq!(r[0].subject, "fix: beta");
    let r = f.repo.log(&parse_search("author:nosuchperson")).unwrap();
    assert!(r.is_empty());
}

#[test]
fn stash_lifecycle() {
    let f = Fx::new();
    f.write("a.txt", "1\n");
    f.commit_all("base");
    f.write("a.txt", "2\n");
    f.write("new.txt", "n\n");
    f.run(ops::stash_push(&ops::StashOpts { message: "wip thing".into(), include_untracked: true, ..Default::default() }));
    assert!(f.repo.status().unwrap().is_clean());
    let s = f.repo.stashes().unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].selector, "stash@{0}");
    assert_eq!(s[0].message, "wip thing");
    assert_eq!(s[0].branch, "main");

    // Rename keeps content.
    f.run(ops::stash_rename(&s[0].selector, &s[0].sha, "renamed"));
    let s = f.repo.stashes().unwrap();
    assert_eq!(s[0].message, "renamed");

    let files = f.repo.commit_files(&s[0].sha).unwrap();
    assert!(files.iter().any(|x| x.path == "a.txt"));

    // Graph hides the stash's helper commits.
    let mut q = LogQuery { all: true, ..Default::default() };
    q.exclude_stash = true;
    q.revs.push(s[0].sha.clone());
    let commits = f.repo.log(&q).unwrap();
    let filtered = graph::hide_stash_internals(commits, &[s[0].sha.clone()]);
    assert!(!filtered.iter().any(|c| c.subject.starts_with("index on")), "{:?}", filtered.iter().map(|c| &c.subject).collect::<Vec<_>>());

    f.run(ops::stash_apply("stash@{0}", true, false));
    assert_eq!(f.read("a.txt"), "2\n");
    assert!(f.repo.stashes().unwrap().is_empty());
}

#[test]
fn worktree_lifecycle() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("base");
    let wt_dir = tempfile::tempdir().unwrap();
    let wt = wt_dir.path().join("wt");
    let wts = wt.to_string_lossy().replace('\\', "/");
    f.run(ops::worktree_add(&wts, None, Some("wt-branch"), None, false));
    let list = f.repo.worktrees().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].is_main && list[0].is_current);
    let w = list.iter().find(|w| !w.is_main).unwrap();
    assert_eq!(w.branch.as_deref(), Some("wt-branch"));

    // Queries from the linked worktree resolve per-worktree git dirs.
    let linked = Repo::open(f.repo.git.clone(), &wt).unwrap();
    assert_ne!(linked.git_dir, f.repo.git_dir);
    assert_eq!(linked.status().unwrap().branch.as_deref(), Some("wt-branch"));
    assert!(linked.worktrees().unwrap().iter().any(|w| w.is_current && !w.is_main));

    let bs = f.repo.branches().unwrap();
    assert!(bs.iter().find(|b| b.name == "wt-branch").unwrap().worktree.is_some());

    f.run(ops::worktree_lock(&wts, Some("busy")));
    assert_eq!(f.repo.worktrees().unwrap().iter().find(|w| !w.is_main).unwrap().locked.as_deref(), Some("busy"));
    assert!(!f.try_run(ops::worktree_remove(&wts, false)), "locked worktree refuses removal");
    f.run(ops::worktree_unlock(&wts));
    f.run(ops::worktree_remove(&wts, false));
    assert_eq!(f.repo.worktrees().unwrap().len(), 1);
}

fn make_conflict(f: &Fx) {
    f.write("c.txt", "top\nmiddle\nbottom\nend1\nend2\nend3\n");
    f.commit_all("base");
    f.run(ops::branch_create("side", None, true));
    f.write("c.txt", "TOP-SIDE\nmiddle\nbottom\nend1\nend2\nside-end\n");
    f.commit_all("side change");
    f.run(ops::checkout("main"));
    f.write("c.txt", "TOP-MAIN\nmiddle\nbottom\nmain-end1\nend2\nend3\n");
    f.commit_all("main change");
}

#[test]
fn merge_conflict_resolved_by_our_merge_engine() {
    let f = Fx::new();
    make_conflict(&f);
    assert!(!f.try_run(ops::merge("side", ops::MergeMode::Default)), "conflict expected");

    let st = f.repo.status().unwrap();
    assert_eq!(st.op, RepoOp::Merge);
    let c: Vec<_> = st.conflicts().collect();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].conflict.as_deref(), Some("UU"));

    let base = String::from_utf8(f.repo.stage_content(1, "c.txt")).unwrap();
    let ours = String::from_utf8(f.repo.stage_content(2, "c.txt")).unwrap();
    let theirs = String::from_utf8(f.repo.stage_content(3, "c.txt")).unwrap();
    assert!(ours.contains("TOP-MAIN") && theirs.contains("TOP-SIDE") && base.contains("top"));

    let mut doc = MergeDoc::new(&base, &ours, &theirs);
    assert_eq!(doc.conflict_count(), 1, "only the first line truly conflicts");
    doc.resolve_all(Resolution::OursThenTheirs);
    let merged = doc.render("HEAD", "side");
    assert_eq!(merged, "TOP-MAIN\nTOP-SIDE\nmiddle\nbottom\nmain-end1\nend2\nside-end\n");

    f.write("c.txt", &merged);
    f.run(ops::mark_resolved("c.txt"));
    assert_eq!(f.repo.status().unwrap().conflicts().count(), 0);
    assert!(f.repo.prepared_message().unwrap().contains("side"));
    f.run(ops::op_continue("merge"));
    let st = f.repo.status().unwrap();
    assert_eq!(st.op, RepoOp::None);
    assert!(st.is_clean());
    assert_eq!(f.repo.commit("HEAD").unwrap().parents.len(), 2);
}

#[test]
fn merge_abort_and_conflict_side_helpers() {
    let f = Fx::new();
    make_conflict(&f);
    assert!(!f.try_run(ops::merge("side", ops::MergeMode::Default)));
    f.run(ops::op_abort("merge"));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
    assert!(f.repo.status().unwrap().is_clean());

    assert!(!f.try_run(ops::merge("side", ops::MergeMode::Default)));
    f.run(ops::checkout_conflict_side("c.txt", true));
    assert!(f.read("c.txt").contains("TOP-SIDE"));
    assert_eq!(f.repo.status().unwrap().conflicts().count(), 0);
}

#[test]
fn cherry_pick_revert_reset() {
    let f = Fx::new();
    f.write("a", "1\n");
    let base = f.commit_all("base");
    f.run(ops::branch_create("dev", None, true));
    f.write("b", "b\n");
    let pick = f.commit_all("add b");
    f.run(ops::checkout("main"));
    f.run(ops::cherry_pick(&[pick], false, true));
    assert!(f.repo.commit("HEAD").unwrap().body.contains("cherry picked"));
    let head = f.repo.resolve("HEAD").unwrap();
    f.run(ops::revert(&head, false, None));
    assert!(f.subjects()[0].starts_with("Revert"));
    assert!(!f.repo.root.join("b").exists());
    f.run(ops::reset(&base, ops::ResetMode::Hard));
    assert_eq!(f.subjects(), vec!["base"]);
    let rl = f.repo.reflog("HEAD", 50).unwrap();
    assert!(rl.iter().any(|e| e.action.starts_with("reset")));
    assert_eq!(rl[0].selector, "HEAD@{0}");
}

#[test]
fn amend_signoff_notes_author() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("orig");
    f.run(ops::commit(&ops::CommitOpts { message: "changed\n\nbody".into(), amend: true, signoff: true, author: Some("Other <o@x>".into()), ..Default::default() }));
    let c = f.repo.commit("HEAD").unwrap();
    assert_eq!(c.subject, "changed");
    assert_eq!(c.author_name, "Other");
    assert!(c.body.contains("Signed-off-by: Test User"));
    assert_eq!(f.subjects().len(), 1);

    f.run(ops::note_set(&c.sha, "a note"));
    assert_eq!(f.repo.commit_note(&c.sha).unwrap().trim(), "a note");
    f.run(ops::note_remove(&c.sha));
    assert!(f.repo.commit_note(&c.sha).is_none());
}

/// Sequence editor substitute for tests: a tiny shell command copying our todo.
fn sequence_editor(todo: &Path) -> String {
    format!("cp \"{}\"", todo.to_string_lossy().replace('\\', "/"))
}

#[test]
fn interactive_rebase_with_prebuilt_todo() {
    let f = Fx::new();
    f.write("a", "0\n");
    f.commit_all("base");
    let mut shas = Vec::new();
    for (name, msg) in [("b", "one"), ("c", "two"), ("d", "three"), ("e", "four")] {
        f.write(name, "x\n");
        shas.push((f.commit_all(msg), msg));
    }
    // reorder, squash two into one, drop four
    let items = vec![
        TodoItem { action: RebaseAction::Pick, sha: shas[2].0.clone(), text: "three".into() },
        TodoItem { action: RebaseAction::Pick, sha: shas[0].0.clone(), text: "one".into() },
        TodoItem { action: RebaseAction::Fixup, sha: shas[1].0.clone(), text: "two".into() },
        TodoItem { action: RebaseAction::Drop, sha: shas[3].0.clone(), text: "four".into() },
    ];
    let dir = tempfile::tempdir().unwrap();
    let todo = dir.path().join("todo");
    std::fs::write(&todo, render_todo(&items)).unwrap();
    let op = GitOp::new("rebase").step(&["rebase", "-i", "HEAD~4"]).env("GIT_SEQUENCE_EDITOR", sequence_editor(&todo));
    f.run(op);
    assert_eq!(f.subjects(), vec!["one", "three", "base"]);
    assert!(f.repo.root.join("c").exists(), "fixup content kept");
    assert!(!f.repo.root.join("e").exists(), "dropped commit gone");
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
}

#[test]
fn rebase_conflict_state_detected() {
    let f = Fx::new();
    make_conflict(&f);
    f.run(ops::checkout("side"));
    assert!(!f.try_run(ops::rebase(&ops::RebaseOpts { onto: "main".into(), ..Default::default() })));
    let st = f.repo.status().unwrap();
    assert!(matches!(st.op, RepoOp::Rebase | RepoOp::RebaseInteractive), "{:?}", st.op);
    assert_eq!(st.op_progress, Some((1, 1)));
    assert_eq!(st.conflicts().count(), 1);
    f.run(ops::op_abort("rebase"));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
}

#[test]
fn bisect_flow() {
    let f = Fx::new();
    let mut shas = Vec::new();
    for i in 0..8 {
        f.write("v", &format!("{i}\n"));
        shas.push(f.commit_all(&format!("c{i}")));
    }
    f.run(ops::bisect_start(&shas[7], &shas[0]));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::Bisect);
    let s = f.repo.bisect_state();
    assert!(s.active);
    assert_eq!(s.bad.as_deref(), Some(shas[7].as_str()));
    assert_eq!(s.good, vec![shas[0].clone()]);
    f.run(ops::bisect_mark("good"));
    assert_eq!(f.repo.bisect_state().good.len(), 2);
    f.run(ops::bisect_reset());
    assert!(!f.repo.bisect_state().active);
}

#[test]
fn clean_config_hooks_patch_archive_gc() {
    let f = Fx::new();
    f.write("a", "1\n");
    let s1 = f.commit_all("base");
    f.write(".gitignore", "*.log\n");
    f.commit_all("ignore");
    f.write("junk.txt", "j\n");
    f.write("dir/junk2.txt", "j\n");
    f.write("x.log", "l\n");
    assert!(f.repo.check_ignore("x.log").is_some());
    assert!(f.repo.check_ignore("a").is_none());

    let p = f.repo.clean_preview(true, false).unwrap();
    assert_eq!(p.len(), 2, "{p:?}");
    assert_eq!(f.repo.clean_preview(true, true).unwrap().len(), 3);
    f.run(ops::delete_untracked(&["junk.txt".into()]));
    assert!(!f.repo.root.join("junk.txt").exists());
    f.run(ops::clean(true, false));
    assert!(!f.repo.root.join("dir").exists());
    assert!(f.repo.root.join("x.log").exists());

    f.run(ops::config_set("gitem.test", "yes", "local"));
    assert_eq!(f.repo.config_get("gitem.test").as_deref(), Some("yes"));
    assert!(f.repo.config_list().unwrap().iter().any(|c| c.key == "gitem.test" && c.scope == "local"));
    f.run(ops::config_unset("gitem.test", "local"));
    assert!(f.repo.config_get("gitem.test").is_none());

    assert!(f.repo.hooks().iter().any(|h| h.name == "pre-commit" && !h.enabled));

    // Patch export and re-apply.
    let text = f.repo.diff_text(&DiffSource::Commit(s1.clone())).unwrap();
    assert!(text.contains("+1"));
    let pdir = tempfile::tempdir().unwrap();
    let pf = pdir.path().join("p.patch");
    f.write("b", "1\n");
    let c = f.commit_all("addb");
    std::fs::write(&pf, f.repo.diff_text(&DiffSource::Commit(c)).unwrap()).unwrap();
    f.run(ops::reset("HEAD~1", ops::ResetMode::Hard));
    assert!(!f.repo.root.join("b").exists());
    f.run(ops::apply_patch_file(&pf.to_string_lossy().replace('\\', "/"), false, false));
    assert!(f.repo.root.join("b").exists());

    let z = pdir.path().join("out.zip");
    f.run(ops::archive("HEAD", &z.to_string_lossy().replace('\\', "/"), "zip"));
    assert!(z.exists());
    let fp = pdir.path().join("fp");
    f.run(ops::format_patch("HEAD~1..HEAD", &fp.to_string_lossy().replace('\\', "/")));
    assert_eq!(std::fs::read_dir(&fp).unwrap().count(), 1);

    assert!(f.repo.count_objects().unwrap().contains("count"));
    f.run(ops::gc(false, false));
    f.run(ops::fsck());
}

#[test]
fn contributors_and_activity() {
    let f = Fx::new();
    f.write("a", "1\n2\n");
    f.commit_all("c1");
    f.write("a", "1\n2\n3\n");
    f.run(ops::stage_all());
    f.run(ops::commit(&ops::CommitOpts { message: "c2".into(), author: Some("Bob <bob@x>".into()), ..Default::default() }));
    let c = f.repo.contributors("HEAD", true).unwrap();
    assert_eq!(c.len(), 2);
    let me = c.iter().find(|x| x.is_you).unwrap();
    assert_eq!(me.email, "test@example.com");
    assert_eq!(me.additions, 2);
    assert_eq!(f.repo.commit_times("HEAD", None, "1970-01-01").unwrap().len(), 2);
}

#[test]
fn submodule_status_and_sparse() {
    let f = Fx::new();
    f.write("top/a.txt", "1\n");
    f.write("other/b.txt", "1\n");
    f.commit_all("base");
    // Submodule add from a local path needs protocol.file.allow.
    let sub_dir = tempfile::tempdir().unwrap();
    let g = f.repo.git.clone();
    g.run(Some(sub_dir.path()), &["init", "-q", "-b", "main"]).unwrap();
    std::fs::write(sub_dir.path().join("s.txt"), "s\n").unwrap();
    g.run(Some(sub_dir.path()), &["add", "."]).unwrap();
    g.run(Some(sub_dir.path()), &["-c", "user.name=t", "-c", "user.email=t@x", "commit", "-q", "-m", "s"]).unwrap();
    let url = sub_dir.path().to_string_lossy().replace('\\', "/");
    g.run(Some(&f.repo.root), &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &url, "libs/sub"]).unwrap();
    let subs = f.repo.submodules().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].path, "libs/sub");
    assert_eq!(subs[0].url, url);

    f.commit_all("add sub");
    f.run(ops::sparse_set(&["top".into()], true));
    assert!(f.repo.root.join("top/a.txt").exists());
    assert!(!f.repo.root.join("other/b.txt").exists());
    assert!(f.repo.sparse_patterns().iter().any(|p| p == "top"));
    f.run(ops::sparse_disable());
    assert!(f.repo.root.join("other/b.txt").exists());
}

#[test]
fn signature_and_describe_and_tree() {
    let f = Fx::new();
    f.write("a/b.txt", "1\n");
    let s = f.commit_all("base");
    assert_eq!(f.repo.signature(&s).unwrap().status, "N");
    f.run(ops::tag_create("v0", None, None, false));
    assert_eq!(f.repo.describe(&s).as_deref(), Some("v0"));
    assert_eq!(f.repo.tracked_files(Some(&s)).unwrap(), vec!["a/b.txt"]);
    f.write("u.txt", "1\n");
    assert_eq!(f.repo.tracked_files(None).unwrap(), vec!["a/b.txt"], "untracked files come from status");
}

#[test]
fn command_log_records_everything() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("base");
    let log = f.repo.git.log.lock().unwrap();
    assert!(log.iter().any(|e| e.command.contains("commit") && e.is_write && e.code == 0));
}

#[test]
fn large_file_output_does_not_deadlock() {
    let f = Fx::new();
    let big: String = (0..200_000).map(|i| format!("line {i}\n")).collect();
    f.write("big.txt", &big);
    f.commit_all("big");
    let b = f.repo.file_at("HEAD", "big.txt").unwrap();
    assert_eq!(b.len(), big.len());
    // Large stdin too.
    let op = GitOp::new("hash").step(&["hash-object", "--stdin"]).stdin(big.into_bytes());
    f.run(op);
}

#[test]
fn walking_back_across_a_rename_reports_the_old_name() {
    let f = Fx::new();
    let content: String = (0..20).map(|i| format!("row {i}\n")).collect();
    f.write("old.txt", &content);
    let s1 = f.commit_all("add");
    f.repo.git.run(Some(&f.repo.root), &["mv", "old.txt", "new.txt"]).unwrap();
    let s2 = f.commit_all("move");
    assert_eq!(f.repo.prev_revision("new.txt", &s2), Some((s1.clone(), "old.txt".to_string())));
    assert_eq!(f.repo.file_at(&s1, "old.txt").unwrap(), content.as_bytes());
}

#[test]
fn parsers_survive_config_that_changes_output_formats() {
    let f = Fx::new();
    for (k, v) in [("diff.noprefix", "true"), ("diff.mnemonicPrefix", "true"), ("log.showSignature", "true"), ("color.ui", "always"), ("format.pretty", "oneline"), ("diff.renames", "false"), ("status.branch", "true"), ("core.quotepath", "true")] {
        f.repo.git.run(Some(&f.repo.root), &["config", k, v]).unwrap();
    }
    f.write("ünï.txt", "a\nb\n");
    let s = f.commit_all("first");
    f.write("ünï.txt", "a\nB\n");
    let log = f.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(log[0].sha, s);
    assert_eq!(log[0].subject, "first");
    assert_eq!(f.repo.status().unwrap().entries[0].path, "ünï.txt");
    let d = f.repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    assert_eq!(d[0].new_path, "ünï.txt");
    assert_eq!(d[0].additions(), 1);
    // Partial staging must still apply despite the odd config.
    let p = build_patch(&d[0], 0, None, PatchMode::Forward).unwrap();
    f.run(ops::apply_patch("stage", p, true, false));
    assert!(f.repo.status().unwrap().entries[0].is_staged());
}

#[test]
fn a_repo_cannot_run_a_program_through_fsmonitor_on_a_read() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("base");
    let marker = f.repo.root.join("pwned");
    let script = f.repo.root.join("evil.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", marker.to_string_lossy().replace('\\', "/"))).unwrap();
    let path = script.to_string_lossy().replace('\\', "/");
    f.repo.git.run(Some(&f.repo.root), &["config", "core.fsmonitor", &path]).unwrap();
    f.repo.status().unwrap();
    assert!(!marker.exists(), "status executed the repository's fsmonitor program");
}

#[test]
fn staging_works_on_crlf_files() {
    let f = Fx::new();
    let base: String = (1..=20).map(|i| format!("line {i}\r\n")).collect();
    f.write("w.txt", &base);
    f.commit_all("base");
    f.write("w.txt", &base.replace("line 3\r\n", "line 3 x\r\n").replace("line 15\r\n", "line 15 y\r\n"));
    let d = f.repo.diff(&DiffSource::Unstaged, Some("w.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(d[0].hunks.len(), 2);
    assert!(d[0].hunks[0].lines.iter().all(|l| !l.text.ends_with('\r')), "display text has no CR");
    f.run(ops::apply_patch("stage", build_patch(&d[0], 0, None, PatchMode::Forward).unwrap(), true, false));
    let staged = String::from_utf8(f.repo.stage_content(0, "w.txt")).unwrap();
    assert!(staged.contains("line 3 x\r\n"), "{staged}");
    assert!(staged.contains("line 15\r\n"), "CRLF and the other hunk untouched");
    // Line selection and discard on CRLF too.
    let d = f.repo.diff(&DiffSource::Unstaged, Some("w.txt"), &DiffOpts::default()).unwrap();
    f.run(ops::apply_patch("discard", build_patch(&d[0], 0, None, PatchMode::Reverse).unwrap(), false, true));
    assert!(f.read("w.txt").contains("line 15\r\n"));
    assert!(f.read("w.txt").contains("line 3 x\r\n"));
}

#[test]
fn staged_hunk_lands_on_the_right_copy_of_repeated_text() {
    let f = Fx::new();
    // Identical blocks, so context alone can't identify the location.
    let block = "start\nsame\nsame\nsame\nend\n";
    let base: String = std::iter::repeat_n(block, 40).collect();
    f.write("r.txt", &base);
    f.commit_all("base");
    // Far up: a big insertion. Further down: a change inside block 30.
    let mut lines: Vec<String> = base.lines().map(String::from).collect();
    let target = 30 * 5 + 2;
    lines[target] = "CHANGED".to_string();
    for i in 0..100 {
        lines.insert(0, format!("added {i}"));
    }
    f.write("r.txt", &(lines.join("\n") + "\n"));
    let d = f.repo.diff(&DiffSource::Unstaged, Some("r.txt"), &DiffOpts::default()).unwrap();
    assert_eq!(d[0].hunks.len(), 2);
    f.run(ops::apply_patch("stage second", build_patch(&d[0], 1, None, PatchMode::Forward).unwrap(), true, false));
    let staged = String::from_utf8(f.repo.stage_content(0, "r.txt")).unwrap();
    let got: Vec<&str> = staged.lines().collect();
    assert_eq!(got.len(), 200);
    assert_eq!(got[target], "CHANGED", "landed on line {:?}", got.iter().position(|l| *l == "CHANGED"));

    // And back again, while the worktree still has the shifted version.
    let s = f.repo.diff(&DiffSource::Staged, Some("r.txt"), &DiffOpts::default()).unwrap();
    f.run(ops::apply_patch("unstage", build_patch(&s[0], 0, None, PatchMode::Reverse).unwrap(), true, true));
    assert_eq!(String::from_utf8(f.repo.stage_content(0, "r.txt")).unwrap(), base);
}

#[test]
fn paths_with_glob_characters_are_exact() {
    let f = Fx::new();
    f.write("pages/[id].tsx", "1\n");
    f.write("pages/d.tsx", "1\n");
    f.write("pages/i.tsx", "1\n");
    f.commit_all("base");
    for p in ["pages/[id].tsx", "pages/d.tsx", "pages/i.tsx"] {
        f.write(p, "2\n");
    }
    f.run(ops::discard_paths(&["pages/[id].tsx".to_string()]));
    assert_eq!(f.read("pages/[id].tsx"), "1\n");
    assert_eq!(f.read("pages/d.tsx"), "2\n", "a glob match must not be discarded");
    assert_eq!(f.read("pages/i.tsx"), "2\n");
    f.write("pages/[id].tsx", "3\n");
    f.run(ops::stage_paths(&["pages/[id].tsx".to_string()]));
    let st = f.repo.status().unwrap();
    assert!(st.entries.iter().find(|e| e.path == "pages/d.tsx").unwrap().index == FileStatus::Unmodified);
    let d = f.repo.diff(&DiffSource::Staged, Some("pages/[id].tsx"), &DiffOpts::default()).unwrap();
    assert_eq!(d.len(), 1);
    f.run(ops::commit(&ops::CommitOpts { message: "edit".into(), ..Default::default() }));
    assert_eq!(f.repo.file_history_points("pages/[id].tsx", 10).unwrap().len(), 2);
}

#[test]
fn commit_keeps_lines_starting_with_a_hash() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.run(ops::stage_all());
    f.run(ops::commit(&ops::CommitOpts { message: "Fix it\n\n#123 is the issue\n".into(), ..Default::default() }));
    assert_eq!(f.repo.commit("HEAD").unwrap().body, "#123 is the issue");
}

#[test]
fn conflict_signature_tracks_resolution() {
    let f = Fx::new();
    make_conflict(&f);
    assert!(!f.try_run(ops::merge("side", ops::MergeMode::Default)));
    assert!(!f.repo.unmerged_signature("c.txt").is_empty());
    f.run(ops::mark_resolved("c.txt"));
    assert!(f.repo.unmerged_signature("c.txt").is_empty());
}

#[test]
fn stash_of_one_file_with_glob_characters() {
    let f = Fx::new();
    f.write("[a].txt", "1
");
    f.write("a.txt", "1
");
    f.commit_all("base");
    f.write("[a].txt", "2
");
    f.write("a.txt", "2
");
    f.run(ops::stash_push(&ops::StashOpts { message: "one".into(), paths: vec!["[a].txt".into()], ..Default::default() }));
    assert_eq!(f.read("[a].txt"), "1
", "stashed");
    assert_eq!(f.read("a.txt"), "2
", "left alone");
}

#[test]
fn parity_commands_undo_reword_fixups_and_copies() {
    let f = Fx::new();
    f.write("a.txt", "1\n");
    f.commit_all("base");
    f.write("b.txt", "1\n");
    let target = f.commit_all("feature");
    f.write("c.txt", "1\n");
    f.commit_all("later");

    // Undo keeps the changes staged.
    f.run(ops::undo_commit());
    assert_eq!(f.subjects()[0], "feature");
    assert!(f.repo.status().unwrap().entries.iter().any(|e| e.path == "c.txt" && e.is_staged()));
    f.run(ops::commit(&ops::CommitOpts { message: "later".into(), ..Default::default() }));

    f.run(ops::reword_head("later, reworded\n\n#7 stays"));
    let c = f.repo.commit("HEAD").unwrap();
    assert_eq!(c.subject, "later, reworded");
    assert_eq!(c.body, "#7 stays");

    // Squash fixups folds a fixup commit into its target, with no editor.
    f.write("b.txt", "2\n");
    f.run(ops::stage_all());
    f.run(ops::fixup_commit(&target));
    assert_eq!(f.subjects().len(), 4);
    f.run(ops::squash_fixups("HEAD~3"));
    assert_eq!(f.subjects(), vec!["later, reworded", "feature", "base"]);
    assert_eq!(f.read("b.txt"), "2\n");

    // Apply a copied patch.
    f.write("a.txt", "changed\n");
    let patch = f.repo.diff_text(&DiffSource::WorkingVsHead).unwrap();
    f.run(ops::discard_paths(&["a.txt".to_string()]));
    assert_eq!(f.read("a.txt"), "1\n");
    f.run(ops::apply_patch_text(&patch, false));
    assert_eq!(f.read("a.txt"), "changed\n");

    // Copy changes and stashes into another worktree.
    let d = tempfile::tempdir().unwrap();
    let wt = d.path().join("w").to_string_lossy().replace('\\', "/");
    f.run(ops::worktree_add(&wt, None, Some("other"), None, false));
    f.run(ops::apply_patch_in(&wt, patch.as_bytes()));
    assert_eq!(std::fs::read_to_string(format!("{wt}/a.txt")).unwrap(), "changed\n");
    f.repo.git.run(Some(Path::new(&wt)), &["reset", "-q", "--hard"]).unwrap();
    f.run(ops::stash_push(&ops::StashOpts { message: "s".into(), ..Default::default() }));
    f.run(ops::stash_apply_in(&wt, "stash@{0}"));
    assert_eq!(std::fs::read_to_string(format!("{wt}/a.txt")).unwrap(), "changed\n");
}

#[test]
fn recompose_keeps_all_work_in_the_tree() {
    let f = Fx::new();
    f.write("a", "1\n");
    let base = f.commit_all("base");
    f.write("b", "1\n");
    f.commit_all("one");
    f.write("c", "1\n");
    f.commit_all("two");
    f.run(ops::recompose_reset(&base));
    assert_eq!(f.subjects(), vec!["base"]);
    assert!(f.repo.root.join("b").exists() && f.repo.root.join("c").exists());
    assert!(f.repo.status().unwrap().entries.iter().all(|e| !e.is_staged()));
}

#[test]
fn conflicts_are_predicted_without_touching_anything() {
    let f = Fx::new();
    make_conflict(&f);
    f.run(ops::branch_create("clean", Some("main~1"), false));
    let conflicts = f.repo.predict_conflicts("main", "side").unwrap();
    assert_eq!(conflicts, vec!["c.txt"]);
    assert!(f.repo.predict_conflicts("main", "clean").unwrap().is_empty());
    assert!(f.repo.status().unwrap().is_clean());
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
}

#[test]
fn richer_search_filters() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("feat: alpha");
    f.run(ops::stage_all());
    f.write("b", "1\n");
    f.run(ops::stage_all());
    f.run(ops::commit(&ops::CommitOpts { message: "fix: beta".into(), author: Some("Other <o@x>".into()), ..Default::default() }));
    f.run(ops::branch_create("s", Some("HEAD~1"), true));
    f.write("c", "1\n");
    f.commit_all("side");
    f.run(ops::checkout("main"));
    f.run(ops::merge("s", ops::MergeMode::NoFf));

    let subj = |q: &str| -> Vec<String> { f.repo.log(&parse_search(q)).unwrap().into_iter().map(|c| c.subject).collect() };
    assert_eq!(subj("-message:Merge -is:merge -message:alpha").len(), 2);
    assert_eq!(subj("is:merge").len(), 1);
    assert_eq!(subj("type:merge").len(), 1);
    let mine = subj("@me");
    assert!(mine.contains(&"feat: alpha".to_string()));
    assert!(!mine.contains(&"fix: beta".to_string()));
    assert!(subj("committer:Test").contains(&"fix: beta".to_string()));
    assert!(subj("committer:nobody").is_empty());
    // Options set by the graph's search toggles.
    let mut q = parse_search("=:feat. =:alpha");
    q.all_match = true;
    q.fixed_strings = false;
    assert_eq!(f.repo.log(&q).unwrap().len(), 1);
    q.fixed_strings = true;
    q.grep = Some("feat.".into());
    assert!(f.repo.log(&q).unwrap().is_empty(), "a dot is literal in plain-text mode");
}

#[test]
fn health_visual_scopes_sizes_and_activity() {
    let f = Fx::new();
    f.write("dir/a.txt", "hello\n");
    f.commit_all("one");
    f.write("dir/a.txt", "hello again\n");
    f.write("b.txt", "x\n");
    f.commit_all("two");
    assert_eq!(f.repo.history_points(Some("dir"), "HEAD", 100).unwrap().len(), 2);
    assert_eq!(f.repo.history_points(Some("b.txt"), "HEAD", 100).unwrap().len(), 1);
    assert_eq!(f.repo.history_points(None, "HEAD", 100).unwrap().len(), 2);
    let stats = f.repo.commit_stats(&["HEAD".to_string()], false, 10).unwrap();
    assert_eq!(stats[&f.repo.resolve("HEAD").unwrap()], (2, 1));
    let sizes = f.repo.file_sizes("HEAD").unwrap();
    assert!(sizes.contains(&("b.txt".to_string(), 2)));
    let activity = f.repo.file_activity("HEAD", 100).unwrap();
    assert!(activity.contains(&("dir/a.txt".to_string(), 2)));

    let h = f.repo.health().unwrap();
    assert_eq!(h.tracked_files, 2);
    assert_eq!(h.commits, 2);
    let item = h.items.iter().find(|i| i.label == "Untracked cache").unwrap();
    assert!(!item.enabled);
    for (k, v) in &item.config {
        f.run(ops::config_set(k, v, "local"));
    }
    assert!(f.repo.health().unwrap().items.iter().find(|i| i.label == "Untracked cache").unwrap().enabled);
    f.repo.status().unwrap();
}

#[test]
fn ai_diffs_respect_ignore_files() {
    let f = Fx::new();
    f.write("keep.txt", "1\n");
    f.write("secret.env", "1\n");
    f.write(".aiignore", "# rules\n*.env\n");
    f.commit_all("base");
    f.write("keep.txt", "shown\n");
    f.write("secret.env", "TOKEN\n");
    let d = f.repo.diff_for_ai(&DiffSource::WorkingVsHead).unwrap();
    assert!(d.contains("shown"));
    assert!(!d.contains("TOKEN"));
}

fn stacked_conflict(f: &Fx) {
    f.write("c.txt", "top\nmiddle\n");
    f.commit_all("base");
    f.run(ops::branch_create("work", None, true));
    f.write("c.txt", "mine\nmiddle\n");
    f.commit_all("my change");
    f.write("d.txt", "d\n");
    f.commit_all("another");
    f.run(ops::checkout("main"));
    f.write("c.txt", "upstream\nmiddle\n");
    f.commit_all("upstream change");
    f.run(ops::checkout("work"));
}

#[test]
fn auto_rebase_resolves_with_the_model_then_can_be_undone() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", false, &|_s, u| {
        assert!(u.contains("Ours:") && u.contains("Theirs:") && u.contains("mine"), "{u}");
        Ok("merged line".to_string())
    })
    .unwrap();
    assert_eq!(sum.paused, None);
    assert_eq!(sum.resolved, vec!["c.txt"]);
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
    assert_eq!(f.read("c.txt"), "merged line\nmiddle\n");
    assert_eq!(f.subjects(), vec!["another", "my change", "upstream change", "base"]);
    assert!(sum.describe("main").contains("c.txt"));

    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.repo.resolve("HEAD").unwrap(), sum.start);
    assert_eq!(f.read("c.txt"), "mine\nmiddle\n");
}

/// The model's answer came partly from text on someone's branch. It's staged for reading,
/// and nothing has been committed from it until the user continues.
#[test]
fn auto_rebase_stops_for_review_after_the_model_resolves_a_commit() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", true, &|_, _| Ok("merged line".to_string())).unwrap();
    let why = sum.paused.as_deref().expect("it stopped");
    assert!(why.contains("c.txt") && why.contains("nothing has been committed"), "{why}");
    assert_eq!(sum.resolved, vec!["c.txt"]);
    let st = f.repo.status().unwrap();
    assert!(matches!(st.op, RepoOp::Rebase | RepoOp::RebaseInteractive), "still in the rebase, for the user to finish");
    assert_eq!(f.read("c.txt"), "merged line
middle
", "the answer is there to read");
    assert_eq!(f.subjects(), vec!["upstream change", "base"], "and not committed");
    f.run(ops::op_continue("rebase").env("GIT_EDITOR", "true"));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
    assert_eq!(f.subjects(), vec!["another", "my change", "upstream change", "base"]);
}

/// No conflict means no answer from a model, so there's nothing to read.
#[test]
fn auto_rebase_with_nothing_to_resolve_does_not_stop() {
    use gitem_core::autorebase;
    let f = Fx::new();
    f.write("a.txt", "a
");
    f.commit_all("base");
    f.run(ops::branch_create("work", None, true));
    f.write("b.txt", "b
");
    f.commit_all("mine");
    f.run(ops::checkout("main"));
    f.write("c.txt", "c
");
    f.commit_all("theirs");
    f.run(ops::checkout("work"));
    let sum = autorebase::run(&f.repo, "main", "", true, &|_, _| panic!("no conflict, so no model")).unwrap();
    assert_eq!(sum.paused, None);
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
}

#[test]
fn auto_rebase_keeps_ignored_files_from_the_model() {
    use gitem_core::autorebase;
    let f = Fx::new();
    f.write(".aiignore", "c.TXT
");
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| panic!("this file must not be sent")).unwrap();
    assert!(sum.paused.as_deref().unwrap().contains("ignore rules"), "{:?}", sum.paused);
}

/// The rebase autostashes and restores this work, and `reset --hard` then destroyed it.
#[test]
fn undoing_a_finished_auto_rebase_keeps_uncommitted_work() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    f.write("dirty.txt", "committed\n");
    f.commit_all("add dirty");
    f.write("dirty.txt", "UNCOMMITTED WORK\n");
    f.write("new-untracked.txt", "also mine\n");
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("merged line".to_string())).unwrap();
    assert_eq!(sum.paused, None);
    assert_eq!(f.read("dirty.txt"), "UNCOMMITTED WORK\n", "the rebase restored it");

    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.repo.resolve("HEAD").unwrap(), sum.start);
    assert_eq!(f.read("dirty.txt"), "UNCOMMITTED WORK\n");
    assert_eq!(f.read("new-untracked.txt"), "also mine\n");
    assert_eq!(f.read("c.txt"), "mine\nmiddle\n");
}

#[test]
fn undoing_a_paused_auto_rebase_keeps_uncommitted_work() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    f.write("dirty.txt", "committed\n");
    f.commit_all("add dirty");
    f.write("dirty.txt", "UNCOMMITTED WORK\n");
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("UNSURE".to_string())).unwrap();
    assert!(sum.paused.is_some());

    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
    assert_eq!(f.repo.resolve("HEAD").unwrap(), sum.start);
    assert_eq!(f.read("dirty.txt"), "UNCOMMITTED WORK\n");
}

/// Staged work makes `reset --keep` refuse even with nothing to move, and after an abort
/// HEAD is already home, so the undo must not fail on it.
#[test]
fn undoing_a_paused_auto_rebase_with_staged_work_finishes_cleanly() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    f.write("new-staged.txt", "staged work\n");
    f.run(ops::stage_paths(&["new-staged.txt".to_string()]));
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("UNSURE".to_string())).unwrap();
    assert!(sum.paused.is_some());
    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);
    assert_eq!(f.read("new-staged.txt"), "staged work\n");
}

/// Uncommitted work in a file the undo has to move: it stops, and nothing is lost.
#[test]
fn undo_refuses_when_uncommitted_work_is_in_a_file_it_would_change() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("merged line".to_string())).unwrap();
    let head = f.repo.resolve("HEAD").unwrap();
    f.write("c.txt", "edited after the rebase\n");
    assert!(!f.try_run(autorebase::undo_op(&sum)));
    assert_eq!(f.read("c.txt"), "edited after the rebase\n");
    assert_eq!(f.repo.resolve("HEAD").unwrap(), head, "the branch stays where it was");
}

#[test]
fn undo_refuses_when_the_branch_moved_after_the_rebase() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("merged line".to_string())).unwrap();
    f.write("later.txt", "later\n");
    f.commit_all("made after the rebase");
    let head = f.repo.resolve("HEAD").unwrap();
    assert!(!f.try_run(autorebase::undo_op(&sum)));
    assert_eq!(f.repo.resolve("HEAD").unwrap(), head);
    assert_eq!(f.read("later.txt"), "later\n");
}

#[test]
fn auto_rebase_pauses_when_the_model_is_unsure_or_fails() {
    use gitem_core::autorebase;
    let f = Fx::new();
    stacked_conflict(&f);
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("UNSURE".to_string())).unwrap();
    assert!(sum.paused.as_deref().unwrap().contains("confident"), "{:?}", sum.paused);
    let st = f.repo.status().unwrap();
    assert!(matches!(st.op, RepoOp::Rebase | RepoOp::RebaseInteractive), "left for the user to finish");
    assert_eq!(st.conflicts().count(), 1, "the file isn't touched");
    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::None);

    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Err(gitem_core::Error::Other("HTTP 429: slow down".into()))).unwrap();
    assert!(sum.paused.as_deref().unwrap().contains("429"));
    f.run(autorebase::undo_op(&sum));

    // A model echoing conflict markers isn't accepted.
    let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| Ok("<<<<<<< a\nx\n=======\ny\n>>>>>>> b".to_string())).unwrap();
    assert!(sum.paused.as_deref().unwrap().contains("markers"));
    f.run(autorebase::undo_op(&sum));
    assert_eq!(f.read("c.txt"), "mine\nmiddle\n");
}

#[test]
fn auto_rebase_reports_bad_refs_as_errors() {
    use gitem_core::autorebase;
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit_all("base");
    assert!(autorebase::run(&f.repo, "no-such-ref", "", false, &|_, _| Ok(String::new())).is_err());
}

#[test]
fn mcp_server_answers_with_repository_facts() {
    use gitem_core::mcp::handle;
    use serde_json::json;
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit_all("first commit");
    f.write("a.txt", "two\n");
    let init = handle(&f.repo, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert!(handle(&f.repo, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none(), "notifications get no reply");
    let list = handle(&f.repo, &json!({"id":2,"method":"tools/list"})).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);
    let text = |name: &str, args: serde_json::Value| -> (String, bool) {
        let r = handle(&f.repo, &json!({"id":3,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
        (r["result"]["content"][0]["text"].as_str().unwrap().to_string(), r["result"]["isError"].as_bool().unwrap_or(false))
    };
    assert!(text("git_status", json!({})).0.contains("a.txt"));
    assert!(text("git_log", json!({"limit": 5})).0.contains("first commit"));
    assert!(text("git_diff", json!({})).0.contains("+two"));
    assert!(text("git_blame", json!({"path":"a.txt"})).0.contains("two"));
    assert!(text("git_branches", json!({})).0.contains("* main"));
    assert!(text("no_such_tool", json!({})).1);
    assert_eq!(handle(&f.repo, &json!({"id":4,"method":"nope"})).unwrap()["error"]["code"], -32601);
}

/// Output must survive a machine too busy to schedule the reader promptly: an empty
/// answer from git reads as "nothing there", which is far worse than a slow one.
#[test]
#[ignore = "burns every core for minutes; run with --ignored on its own"]
fn reads_keep_their_output_on_a_busy_machine() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fx::new();
    f.write("a.txt", "x
");
    let head = f.commit_all("first");
    let stop = Arc::new(AtomicBool::new(false));
    let cores = std::thread::available_parallelism().map_or(8, |n| n.get());
    let burners: Vec<_> = (0..cores * 4)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(31).wrapping_add(1);
                    std::hint::black_box(x);
                }
            })
        })
        .collect();
    let readers: Vec<_> = (0..cores)
        .map(|_| {
            let (repo, head) = (f.repo.clone(), head.clone());
            std::thread::spawn(move || (0..25).filter(|_| repo.git.run(Some(&repo.root), &["rev-parse", "HEAD"]).unwrap().text().trim() != head).count())
        })
        .collect();
    let wrong: usize = readers.into_iter().map(|r| r.join().unwrap()).sum();
    stop.store(true, Ordering::Relaxed);
    burners.into_iter().for_each(|b| b.join().unwrap());
    assert_eq!(wrong, 0, "reads came back empty or wrong");
}

/// Asks git itself where its config comes from.
#[test]
fn tests_never_see_the_machines_git_config() {
    let git = GitExe::discover(None).unwrap();
    let scopes = git.run(None, &["config", "--list", "--show-scope", "--name-only"]).unwrap().text();
    assert!(!scopes.lines().any(|l| l.starts_with("system")), "{scopes}");
    let global = git.run(None, &["config", "--global", "--list", "--show-origin"]).unwrap().text();
    assert!(global.contains("pull.rebase=false"), "{global}");
    assert_eq!(global.lines().count(), 1, "only the test's own config: {global}");
    let scratch = std::env::temp_dir();
    assert!(gitem_core::settings::config_dir().starts_with(scratch), "settings are in the scratch folder");
}

/// A merge with one conflict (`c.txt`), and an auto-merged and a new file staged.
fn merge_with_a_conflict(f: &Fx) {
    f.write("c.txt", "base\n");
    f.write("auto.txt", "base\n");
    f.commit_all("base");
    f.run(ops::branch_create("side", None, true));
    f.write("c.txt", "side\n");
    f.write("auto.txt", "side\n");
    f.write("new.txt", "new\n");
    f.commit_all("on side");
    f.run(ops::checkout("main"));
    f.write("c.txt", "main\n");
    f.commit_all("on main");
    assert!(!f.repo.git.run_raw(Some(&f.repo.root), &["merge", "side"]).unwrap().ok(), "the merge should conflict");
}

fn staged_paths(f: &Fx) -> Vec<String> {
    f.repo.status().unwrap().entries.iter().filter(|e| e.is_staged()).map(|e| e.path.clone()).collect()
}

#[test]
fn unstaging_during_a_merge_keeps_the_merge_and_its_conflicts() {
    let f = Fx::new();
    merge_with_a_conflict(&f);
    let staged = staged_paths(&f);
    assert_eq!(staged, ["auto.txt", "new.txt"]);
    f.run(ops::unstage_during_op(&staged));
    let st = f.repo.status().unwrap();
    assert_eq!(st.op, RepoOp::Merge);
    assert!(f.repo.git_dir.join("MERGE_HEAD").exists(), "the merge is still going");
    assert_eq!(st.conflicts().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["c.txt"], "the conflict is still a conflict");
    assert!(staged_paths(&f).is_empty());
    assert_eq!(f.read("auto.txt"), "side\n", "only the index changes");
}

#[test]
fn unstaging_during_a_cherry_pick_keeps_it_going() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit_all("base");
    f.run(ops::branch_create("side", None, true));
    f.write("c.txt", "side\n");
    f.write("other.txt", "other\n");
    let picked = f.commit_all("on side");
    f.run(ops::checkout("main"));
    f.write("c.txt", "main\n");
    f.commit_all("on main");
    assert!(!f.repo.git.run_raw(Some(&f.repo.root), &["cherry-pick", &picked]).unwrap().ok());
    f.run(ops::unstage_during_op(&staged_paths(&f)));
    assert_eq!(f.repo.status().unwrap().op, RepoOp::CherryPick);
    assert!(f.repo.git_dir.join("CHERRY_PICK_HEAD").exists());
}

#[test]
fn rewording_head_leaves_what_is_staged_out_of_the_commit() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("first");
    f.write("staged.txt", "not for this commit\n");
    f.run(ops::stage_paths(&["staged.txt".to_string()]));
    f.run(ops::reword_head("reworded"));
    assert_eq!(f.subjects(), ["reworded"]);
    let files = f.repo.git.run(Some(&f.repo.root), &["show", "--format=", "--name-only", "HEAD"]).unwrap().text();
    assert_eq!(files.trim(), "a.txt");
    assert_eq!(staged_paths(&f), ["staged.txt"]);
}

#[test]
fn tokens_in_urls_stay_out_of_titles_previews_and_the_console() {
    let f = Fx::new();
    let url = "http://user:FAKETOKEN@127.0.0.1:1/x.git";
    let dest = f.repo.root.join("cloned").to_string_lossy().replace('\\', "/");
    let op = ops::clone_repo(url, &dest, None, false, None);
    assert!(!op.title.contains("FAKETOKEN"), "{}", op.title);
    assert!(!op.preview().contains("FAKETOKEN"), "{}", op.preview());
    assert!(op.steps[0].args.iter().any(|a| a == url), "git still gets the real URL");
    assert!(!f.try_run(op), "nothing listens on port 1");
    let log = f.repo.git.log.lock().unwrap();
    let entry = log.iter().rev().find(|e| e.command.contains("clone")).expect("the clone is in the console");
    assert!(!entry.command.contains("FAKETOKEN"), "{}", entry.command);
    assert!(entry.command.contains("127.0.0.1:1"), "{}", entry.command);
}


#[test]
fn commit_files_reads_status_and_counts_in_one_command() {
    let f = Fx::new();
    let body: String = (0..30).map(|i| format!("line {i}\n")).collect();
    f.write("a.txt", &body);
    f.write("gone.txt", "bye\n");
    f.commit_all("base");
    f.run(ops::GitOp::new("mv").step(&["mv", "a.txt", "b.txt"]));
    f.write("b.txt", &format!("{body}more\n"));
    f.write("new file.txt", "1\n2\n");
    std::fs::remove_file(f.repo.root.join("gone.txt")).unwrap();
    let sha = f.commit_all("change");
    let before = f.repo.git.log.lock().unwrap().len();
    let files = f.repo.commit_files(&sha).unwrap();
    assert_eq!(f.repo.git.log.lock().unwrap().len() - before, 1);
    let by = |p: &str| files.iter().find(|c| c.path == p).unwrap_or_else(|| panic!("no {p} in {files:?}"));
    assert_eq!(files.len(), 3);
    assert_eq!(by("b.txt").status, FileStatus::Renamed);
    assert_eq!(by("b.txt").orig_path.as_deref(), Some("a.txt"));
    assert_eq!((by("b.txt").additions, by("b.txt").deletions), (Some(1), Some(0)));
    assert_eq!((by("new file.txt").status, by("new file.txt").additions), (FileStatus::Added, Some(2)));
    assert_eq!((by("gone.txt").status, by("gone.txt").deletions), (FileStatus::Deleted, Some(1)));
}

#[test]
fn commit_files_marks_binary_files_as_having_no_counts() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    std::fs::write(f.repo.root.join("b.bin"), [0u8, 1, 2, 0, 3]).unwrap();
    let sha = f.commit_all("binary");
    let files = f.repo.commit_files(&sha).unwrap();
    assert_eq!((files[0].path.as_str(), files[0].additions), ("b.bin", None));
}

#[test]
fn a_cancelled_git_stops_and_runs_nothing_more() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut repo = f.repo.clone();
    repo.git.cancel = Some(stop);
    let before = f.repo.git.log.lock().unwrap().len();
    assert!(repo.status().is_err());
    assert_eq!(f.repo.git.log.lock().unwrap().len(), before, "it never started");
}

#[test]
fn people_are_read_from_shortlog_with_the_same_answer_as_the_full_list() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("one");
    f.write("a.txt", "b\n");
    f.commit_all("two");
    let quick = f.repo.people("HEAD").unwrap();
    let full = f.repo.contributors("HEAD", false).unwrap();
    assert_eq!(quick.len(), full.len());
    assert_eq!((quick[0].name.as_str(), quick[0].email.as_str(), quick[0].commits, quick[0].is_you), (full[0].name.as_str(), full[0].email.as_str(), full[0].commits, full[0].is_you));
}

/// Unstaging one hunk of a staged rename used to undo the whole rename.
#[test]
fn unstaging_a_hunk_of_a_staged_rename_keeps_the_rename() {
    let f = Fx::new();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    f.write("old.txt", &body);
    f.commit_all("base");
    f.repo.git.run(Some(&f.repo.root), &["mv", "old.txt", "new.txt"]).unwrap();
    f.write("new.txt", &body.replace("line 2\n", "line two\n").replace("line 29\n", "line twenty-nine\n"));
    f.run(ops::stage_all());
    let d = f.repo.diff(&DiffSource::Staged, None, &DiffOpts::default()).unwrap();
    assert!(d[0].is_rename);
    assert_eq!(d[0].hunks.len(), 2);
    let p = build_patch(&d[0], 1, None, PatchMode::Reverse).unwrap();
    f.run(ops::apply_patch("Unstage hunk", p, true, true));
    let st = f.repo.status().unwrap();
    let e = st.entries.iter().find(|e| e.path == "new.txt").unwrap();
    assert_eq!(e.orig_path.as_deref(), Some("old.txt"), "{:?}", st.entries);
    assert_eq!(e.index, FileStatus::Renamed);
    assert_eq!(e.worktree, FileStatus::Modified, "the edit taken back is still on disk");
    assert_eq!(st.entries.len(), 1, "{:?}", st.entries);
    let staged = String::from_utf8(f.repo.git.run(Some(&f.repo.root), &["show", ":new.txt"]).unwrap().stdout).unwrap();
    assert!(staged.contains("line two") && staged.contains("line 29\n"));
}

/// Git holds a patch to what its header says about the whole file.
#[test]
fn part_of_a_new_or_deleted_file_can_be_staged_or_taken_back() {
    let f = Fx::new();
    f.write("gone.txt", "one\ntwo\nthree\n");
    f.commit_all("base");
    std::fs::remove_file(f.repo.root.join("gone.txt")).unwrap();
    let d = f.repo.diff(&DiffSource::Unstaged, Some("gone.txt"), &DiffOpts::default()).unwrap();
    assert!(d[0].is_deleted);
    let del: Vec<usize> = d[0].hunks[0].lines.iter().enumerate().filter(|(_, l)| l.text == "two").map(|(i, _)| i).collect();
    let p = build_patch(&d[0], 0, Some(&del), PatchMode::Forward).unwrap();
    f.run(ops::apply_patch("Stage lines", p, true, false));
    let index = |p: &str| String::from_utf8(f.repo.git.run(Some(&f.repo.root), &["show", &format!(":{p}")]).unwrap().stdout).unwrap();
    assert_eq!(index("gone.txt"), "one\nthree\n");
    assert!(!f.repo.root.join("gone.txt").exists());

    f.write("new.txt", "a\nb\nc\n");
    f.run(ops::stage_paths(&["new.txt".to_string()]));
    let d = f.repo.diff(&DiffSource::Staged, Some("new.txt"), &DiffOpts::default()).unwrap();
    assert!(d[0].is_new);
    let add: Vec<usize> = d[0].hunks[0].lines.iter().enumerate().filter(|(_, l)| l.text == "b").map(|(i, _)| i).collect();
    let p = build_patch(&d[0], 0, Some(&add), PatchMode::Reverse).unwrap();
    f.run(ops::apply_patch("Unstage lines", p, true, true));
    assert_eq!(index("new.txt"), "a\nc\n");
    assert_eq!(f.read("new.txt"), "a\nb\nc\n", "the working file isn't touched");

    // All of it still removes or adds the whole file, as before.
    let d = f.repo.diff(&DiffSource::Staged, Some("new.txt"), &DiffOpts::default()).unwrap();
    let p = build_patch(&d[0], 0, None, PatchMode::Reverse).unwrap();
    f.run(ops::apply_patch("Unstage hunk", p, true, true));
    let st = f.repo.status().unwrap();
    assert_eq!(st.entries.iter().find(|e| e.path == "new.txt").unwrap().worktree, FileStatus::Untracked);
}

#[test]
fn a_change_of_line_ending_alone_says_so_side_by_side() {
    let f = Fx::new();
    f.write("a.txt", "same\nother\n");
    f.commit_all("base");
    f.write("a.txt", "same\r\nother\n");
    let d = f.repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    let rows = gitem_core::diff::side_by_side(&d[0]);
    let row = rows.iter().find(|r| r.left_kind == Some(LineKind::Del)).unwrap();
    assert_eq!(row.left.as_ref().unwrap().1, "same [LF]");
    assert_eq!(row.right.as_ref().unwrap().1, "same [CR]");
}

/// An empty side isn't a deleted one, and the reason given has to say which it is.
#[test]
fn auto_rebase_says_whether_a_side_was_emptied_or_deleted() {
    use gitem_core::autorebase;
    for (emptied, want) in [(true, "emptied"), (false, "deleted")] {
        let f = Fx::new();
        f.write("c.txt", "base\n");
        f.commit_all("base");
        f.run(ops::branch_create("work", None, true));
        f.write("c.txt", "mine\n");
        f.commit_all("mine");
        f.run(ops::checkout("main"));
        if emptied {
            f.write("c.txt", "");
        } else {
            std::fs::remove_file(f.repo.root.join("c.txt")).unwrap();
        }
        f.commit_all("theirs");
        f.run(ops::checkout("work"));
        let sum = autorebase::run(&f.repo, "main", "", false, &|_, _| panic!("this needs a person, not the model")).unwrap();
        let why = sum.paused.expect("it stopped");
        assert!(why.contains(want), "{why}");
    }
}

#[test]
fn auto_rebase_says_what_the_model_really_did() {
    use gitem_core::autorebase::Summary;
    let s = Summary { resolved: vec!["a.txt".into()], merged_without_ai: vec!["b.txt".into()], ..Default::default() };
    let text = s.describe("main");
    let (with, without) = (text.find("Resolved with AI").unwrap(), text.find("Merged without asking").unwrap());
    assert!(text[with..without].contains("a.txt") && text[without..].contains("b.txt"), "{text}");
    assert!(Summary::default().describe("main").contains("No conflicts needed resolving"));
}

#[test]
fn conflict_markers_count_with_or_without_a_label() {
    use gitem_core::merge::has_conflict_markers;
    assert!(has_conflict_markers("a\n<<<<<<<\nb\n=======\nc\n>>>>>>>\n"));
    assert!(has_conflict_markers("<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> side\n"));
    assert!(!has_conflict_markers("<<<<<<<< eight\n>>>>>>>> eight\n"));
    assert!(!has_conflict_markers("a\n=======\nc\n"));
}

/// Commits a staged rename whole, where it used to leave the old name staged for later.
#[test]
fn composing_a_staged_rename_commits_both_names() {
    let f = Fx::new();
    f.write("old.txt", "one\ntwo\nthree\nfour\nfive\n");
    f.commit_all("base");
    f.repo.git.run(Some(&f.repo.root), &["mv", "old.txt", "new.txt"]).unwrap();
    f.write("other.txt", "not in this commit\n");
    f.run(ops::compose_commits(&[ops::ComposedCommit { message: "moved".into(), files: vec!["new.txt".into()], renamed_from: vec!["old.txt".into()] }]));
    let d = f.repo.diff(&DiffSource::Commit(f.repo.resolve("HEAD").unwrap()), None, &DiffOpts::default()).unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].is_rename);
    let st = f.repo.status().unwrap();
    assert_eq!(st.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), vec!["other.txt"], "nothing of the rename is left over");
}

/// Git keeps a stash's untracked files in a third parent, with no parent of its own to compare.
#[test]
fn a_stashes_untracked_files_can_be_shown() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    f.write("a.txt", "b\n");
    f.write("u.txt", "untracked\n");
    f.run(ops::stash_push(&ops::StashOpts { include_untracked: true, ..Default::default() }));
    let third = f.repo.resolve("stash@{0}^3").unwrap();
    let d = f.repo.diff(&DiffSource::Commit(third), None, &DiffOpts::default()).unwrap();
    assert_eq!(d.iter().map(|x| x.display_path()).collect::<Vec<_>>(), vec!["u.txt"]);
    f.run(ops::stash_push(&ops::StashOpts::default()).clone());
    assert!(f.repo.resolve("stash@{0}^3").is_err() || f.repo.stashes().unwrap().len() == 1);
}

#[test]
fn an_op_cleans_up_its_scratch_files_whether_it_works_or_not() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    for (args, ok) in [(vec!["status"], true), (vec!["no-such-command"], false)] {
        let scratch = f.repo.root.join(".git/scratch.txt");
        std::fs::write(&scratch, "x").unwrap();
        let mut op = GitOp::new("x").step(&args);
        op.remove_after = vec![scratch.clone()];
        assert_eq!(f.try_run(op), ok);
        assert!(!scratch.exists());
    }
}

/// Full ref names say which kind each is, so a log needs no other command to label them.
#[test]
fn a_log_is_one_command() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    let before = f.repo.git.log.lock().unwrap().len();
    f.repo.log(&LogQuery { limit: 1, revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    let log = f.repo.git.log.lock().unwrap();
    let ran: Vec<&str> = log.iter().skip(before).map(|e| e.command.as_str()).collect();
    assert_eq!(ran.len(), 1, "{ran:?}");
}

#[test]
fn only_the_graph_pays_for_date_order() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit_all("base");
    let last = |f: &Fx| f.repo.git.log.lock().unwrap().back().unwrap().command.clone();
    f.repo.remotes().unwrap();
    f.repo.log(&LogQuery { limit: 1, revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert!(!last(&f).contains("--date-order"));
    f.repo.log(&LogQuery { limit: 1, revs: vec!["HEAD".into()], date_order: true, ..Default::default() }).unwrap();
    assert!(last(&f).contains("--date-order"));
}

/// CG-26: an AI answer takes minutes, and someone can abort or move on while it's coming.
#[test]
fn an_auto_rebase_answer_is_not_written_after_the_rebase_moved_on() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit_all("base");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "-c", "work"]));
    f.write("c.txt", "mine\n");
    f.commit_all("mine");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "main"]));
    f.write("c.txt", "upstream\n");
    f.commit_all("upstream");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "work"]));
    let repo = f.repo.clone();
    let abort_first = move |_: &str, _: &str| -> gitem_core::Result<String> {
        ops::GitOp::new("abort").step(&["rebase", "--abort"]).run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
        Ok("what the model said".to_string())
    };
    let done = gitem_core::autorebase::run(&f.repo, "main", "", false, &abort_first).unwrap();
    assert!(done.paused.as_deref().unwrap_or_default().contains("moved on"), "{:?}", done.paused);
    assert_eq!(std::fs::read_to_string(f.repo.root.join("c.txt")).unwrap().replace("\r\n", "\n"), "mine\n", "the answer wasn't written");
}

/// Undo needs where it began, whichever step went wrong.
#[test]
fn an_auto_rebase_that_fails_midway_still_says_where_it_began() {
    let f = Fx::new();
    f.write("c.txt", "base\n");
    f.commit_all("base");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "-c", "work"]));
    f.write("c.txt", "mine\n");
    let start = f.commit_all("mine");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "main"]));
    f.write("c.txt", "upstream\n");
    f.commit_all("upstream");
    f.run(ops::GitOp::new("b").step(&["switch", "-q", "work"]));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut repo = f.repo.clone();
    repo.git.cancel = Some(stop.clone());
    let halt = move |_: &str, _: &str| -> gitem_core::Result<String> {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok("what the model said".to_string())
    };
    let done = gitem_core::autorebase::run(&repo, "main", "", false, &halt).unwrap();
    assert_eq!(done.start, start);
    assert!(done.paused.is_some());
    assert!(matches!(f.repo.current_op(), RepoOp::Rebase | RepoOp::RebaseInteractive), "left for the banner to pick up");
}
