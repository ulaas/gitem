//! How gitem reads repositories that aren't quite ordinary: odd names, odd config, shallow,
//! partial and bare ones, and commits with unusual bytes in them. Real git, temp repos.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::model::*;
use gitem_core::ops::{self, GitOp};
use gitem_core::repo::{DiffOpts, DiffSource};
use gitem_core::{GitExe, Repo};

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: Repo,
}

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    g.helper = None;
    g
}

fn setup(dir: &Path) {
    let g = git();
    g.run(Some(dir), &["init", "-q", "-b", "main"]).unwrap();
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        g.run(Some(dir), &["config", k, v]).unwrap();
    }
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        setup(dir.path());
        let repo = Repo::open(git(), dir.path()).unwrap();
        Self { root: dir.path().to_path_buf(), _dir: dir, repo }
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn q(&self, args: &[&str]) -> String {
        git().run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim_end().to_string()
    }
    fn commit(&self, msg: &str) -> String {
        self.q(&["add", "-A"]);
        self.q(&["commit", "-qm", msg]);
        self.q(&["rev-parse", "HEAD"])
    }
    fn run(&self, op: GitOp) {
        op.run(&self.repo.git, Some(&self.root), None, &mut |_| {}).unwrap_or_else(|e| panic!("{}: {e}", op.title));
    }
}

// ---- names that collide with other names ----------------------------------------------

/// A `docs` branch beside a `docs` folder is an everyday layout.
#[test]
fn a_branch_named_like_a_folder_works_in_every_read() {
    let f = Fx::new();
    f.write("docs/a.md", "a\n");
    f.commit("base");
    f.q(&["branch", "docs"]);
    f.q(&["reflog", "expire", "--expire=never", "--all"]);
    let rev = "docs";
    assert_eq!(f.repo.log(&LogQuery { revs: vec![rev.into()], ..Default::default() }).unwrap().len(), 1);
    assert_eq!(f.repo.contributors(rev, false).unwrap().len(), 1);
    assert_eq!(f.repo.commit_times(rev, None, "2000-01-01").unwrap().len(), 1);
    assert_eq!(f.repo.history_points(None, rev, 10).unwrap().len(), 1);
    assert_eq!(f.repo.file_activity(rev, 10).unwrap().len(), 1);
    assert_eq!(f.repo.reflog(rev, 10).unwrap().len(), 1);
    assert_eq!(f.repo.people(rev).unwrap().len(), 1);
    assert!(f.repo.diff(&DiffSource::WorkingVs(rev.into()), None, &DiffOpts::default()).unwrap().is_empty());
    assert!(f.repo.diff_text(&DiffSource::WorkingVs(rev.into())).unwrap().is_empty());
    assert!(f.repo.patch_bytes(&DiffSource::WorkingVs(rev.into())).unwrap().is_empty());
    assert!(f.repo.commit_stats(&[rev.to_string()], false, 10).unwrap().len() == 1);
    assert!(f.repo.signature(rev).is_ok());
    f.run(ops::reset(rev, ops::ResetMode::Mixed));
}

#[test]
fn a_file_named_head_doesnt_get_in_the_way() {
    let f = Fx::new();
    f.write("HEAD", "a\n");
    f.commit("base");
    f.write("HEAD", "b\n");
    assert_eq!(f.repo.diff(&DiffSource::WorkingVsHead, None, &DiffOpts::default()).unwrap().len(), 1);
    assert!(f.repo.diff_text(&DiffSource::WorkingVsHead).unwrap().contains("+b"));
    assert_eq!(f.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap().len(), 1);
}

/// These used to report nothing at all for a real failure, which looks like an empty history.
#[test]
fn a_real_failure_is_not_an_empty_result() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    assert!(f.repo.contributors("no-such-branch", false).is_err());
    assert!(f.repo.commit_times("no-such-branch", None, "2000-01-01").is_err());
    assert!(f.repo.reflog("no-such-branch", 10).is_err());
    assert!(f.repo.people("no-such-branch").is_err());
}

/// A release often has a branch and a tag both called `v1.2`.
#[test]
fn a_branch_and_tag_of_one_name_can_each_be_told_apart() {
    let f = Fx::new();
    f.write("a", "old\n");
    let old = f.commit("old");
    f.q(&["tag", "foo"]);
    f.q(&["switch", "-qc", "foo"]);
    f.write("a", "new\n");
    let new = f.commit("new");
    f.q(&["switch", "-q", "main"]);

    let tags = f.repo.tags().unwrap();
    assert_eq!(tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["foo"], "not tags/foo");
    let branch = f.repo.branches().unwrap().into_iter().find(|b| b.name == "foo").unwrap();
    let rev = branch.rev_among(&tags);
    assert_eq!(rev, "refs/heads/foo");
    assert_eq!(f.repo.log(&LogQuery { revs: vec![rev.clone()], limit: 1, ..Default::default() }).unwrap()[0].sha, new);
    assert_eq!(f.repo.commit("refs/tags/foo").unwrap().sha, old);

    f.run(ops::merge(&rev, ops::MergeMode::Default));
    assert_eq!(f.q(&["rev-parse", "HEAD"]), new, "merged the branch, not the older tag");
    f.q(&["switch", "-q", "foo"]);
    assert_eq!(f.q(&["rev-parse", "HEAD"]), new);
    f.q(&["switch", "-q", "main"]);
    f.run(ops::tag_delete(&tags[0].name));
    assert_eq!(f.q(&["tag"]), "");
}

#[test]
fn a_branch_with_no_tag_of_its_name_stays_short() {
    let b = Branch { name: "topic".into(), ..Default::default() };
    assert_eq!(b.rev_among(&[]), "topic");
    let r = Branch { name: "origin/topic".into(), is_remote: true, ..Default::default() };
    assert_eq!(r.rev_among(&[Tag { name: "origin/topic".into(), ..Default::default() }]), "refs/remotes/origin/topic");
}

#[test]
fn switching_to_a_branch_named_with_a_dash_works() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    f.q(&["update-ref", "refs/heads/--dash", "HEAD"]);
    let b = f.repo.branches().unwrap().into_iter().find(|b| b.name.contains("--dash")).unwrap();
    f.run(ops::checkout(&b.name));
    assert_eq!(f.q(&["symbolic-ref", "--short", "HEAD"]), "--dash");
    f.write("a", "b\n");
    f.q(&["commit", "-qam", "there"]);
    f.q(&["switch", "-q", "main"]);
    assert_eq!(f.q(&["log", "-1", "--format=%s", "refs/heads/--dash"]), "there");
}

/// `fork/x` is a local branch even with a remote called `fork`, and `%D` in its short form
/// can't tell the two apart. The mark on a shallow boundary isn't a branch either.
#[test]
fn refs_on_a_log_are_labelled_by_what_they_are() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    f.q(&["remote", "add", "fork", "https://example.invalid/x.git"]);
    f.q(&["update-ref", "refs/remotes/fork/y", "HEAD"]);
    f.q(&["branch", "fork/x"]);
    f.q(&["tag", "v1"]);
    let c = f.repo.commit("HEAD").unwrap();
    let kind = |n: &str, k: RefKind| c.refs.iter().any(|r| r.name == n && r.kind == k);
    assert!(kind("fork/x", RefKind::Branch), "{:?}", c.refs);
    assert!(kind("fork/y", RefKind::Remote), "{:?}", c.refs);
    assert!(kind("v1", RefKind::Tag), "{:?}", c.refs);
    assert!(kind("main", RefKind::Branch) && c.refs.iter().any(|r| r.current), "{:?}", c.refs);
}

#[test]
fn a_shallow_clone_shows_no_branch_called_grafted() {
    let f = Fx::new();
    f.write("a", "1\n");
    f.commit("one");
    f.write("a", "2\n");
    f.commit("two");
    let dest = tempfile::tempdir().unwrap();
    let url = format!("file:///{}", f.root.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
    git().run(Some(dest.path()), &["clone", "-q", "--depth=1", &url, "c"]).unwrap();
    let repo = Repo::open(git(), &dest.path().join("c")).unwrap();
    let c = repo.commit("HEAD").unwrap();
    assert!(!c.refs.iter().any(|r| r.name == "grafted"), "{:?}", c.refs);
}

#[test]
fn deleting_a_branch_on_a_remote_with_a_slash_in_its_name_names_that_remote() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    let bare = tempfile::tempdir().unwrap();
    git().run(Some(bare.path()), &["init", "-q", "--bare", "-b", "main"]).unwrap();
    let url = bare.path().to_string_lossy().replace('\\', "/");
    let bare_dir = format!("--git-dir={url}");
    f.q(&["remote", "add", "a/b", &url]);
    f.q(&["push", "-q", "a/b", "main:topic"]);
    let remotes = vec!["a".to_string(), "a/b".to_string()];
    f.run(ops::branch_delete_remote("a/b/topic", &remotes));
    assert_eq!(git().run(None, &[&bare_dir, "branch", "--list"]).unwrap().text().trim(), "");
}

// ---- graph filters --------------------------------------------------------------------

/// A branch with merges from short-lived side branches.
fn merges(f: &Fx, n: usize) {
    f.write("a", "a\n");
    f.commit("root");
    for i in 0..n {
        f.q(&["switch", "-qc", &format!("s{i}")]);
        f.write(&format!("s{i}"), "s\n");
        f.commit(&format!("side {i}"));
        f.q(&["switch", "-q", "main"]);
        f.write(&format!("m{i}"), "m\n");
        f.commit(&format!("main {i}"));
        f.q(&["merge", "-q", "--no-ff", "-m", &format!("merge {i}"), &format!("s{i}")]);
    }
}

fn widest(commits: &[Commit]) -> u16 {
    gitem_core::graph::layout(commits).iter().map(|r| r.width).max().unwrap_or(0)
}

/// Each hidden parent used to start a lane that never ended, so the graph got wider all the
/// way down.
#[test]
fn graph_filters_dont_leave_lanes_running_to_nowhere() {
    let f = Fx::new();
    merges(&f, 6);
    let q = LogQuery { revs: vec!["HEAD".into()], ..Default::default() };
    let all = f.repo.log(&q).unwrap();
    assert!(widest(&all) <= 3, "as a yardstick: {}", widest(&all));

    let q = LogQuery { revs: vec!["HEAD".into()], first_parent: true, ..Default::default() };
    let c = f.repo.log(&q).unwrap();
    assert!(widest(&c) > 2, "the bug this is for needs to show up: {}", widest(&c));
    let shaped = gitem_core::graph::parents_for_filters(&c, true, None);
    assert_eq!(widest(&shaped), 1);

    let q = LogQuery { revs: vec!["HEAD".into()], no_merges: true, ..Default::default() };
    let c = f.repo.log(&q).unwrap();
    assert!(c.iter().all(|x| !x.is_merge()));
    assert!(widest(&c) > 3, "the bug this is for needs to show up: {}", widest(&c));
    let map = f.repo.merge_first_parents(&q).unwrap();
    assert_eq!(map.len(), 6);
    let shaped = gitem_core::graph::parents_for_filters(&c, false, Some(&map));
    assert!(widest(&shaped) <= 2, "{}", widest(&shaped));
    let shas: Vec<&str> = c.iter().map(|x| x.sha.as_str()).collect();
    for x in &shaped {
        for p in &x.parents {
            assert!(shas.contains(&p.as_str()), "{} has a parent that isn't in the list", x.subject);
        }
    }
    assert!(c.iter().any(|x| x.subject == "main 5" && x.parents.len() == 1), "what menus act on keeps its real parents");
}

#[test]
fn hiding_only_needs_the_refs_that_remain() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("shared");
    f.q(&["switch", "-qc", "other"]);
    f.write("o", "o\n");
    f.commit("on other");
    f.q(&["switch", "-q", "main"]);
    f.write("m", "m\n");
    f.commit("on main");
    let q = LogQuery { all: true, excludes: vec!["refs/heads/main".into()], ..Default::default() };
    assert_eq!(f.repo.log(&q).unwrap().len(), 3, "--all brings HEAD back, which is what this is about");
    let q = LogQuery { all_refs_only: true, excludes: vec!["refs/heads/main".into()], ..Default::default() };
    let got: Vec<String> = f.repo.log(&q).unwrap().into_iter().map(|c| c.subject).collect();
    assert_eq!(got, vec!["on other", "shared"]);
}

// ---- commit data ----------------------------------------------------------------------

/// Commits with a record separator in them used to vanish, or lose their body.
#[test]
fn a_record_separator_in_a_commit_is_just_text() {
    let f = Fx::new();
    f.write("a", "1\n");
    let plain = f.commit("plain");
    f.write("a", "2\n");
    f.q(&["add", "-A"]);
    f.q(&["commit", "-qm", "subject\x1ewith separator"]);
    let in_subject = f.q(&["rev-parse", "HEAD"]);
    f.write("a", "3\n");
    f.q(&["add", "-A"]);
    f.q(&["commit", "-qm", "body sep\n\nbody one\x1ebody two"]);
    let in_body = f.q(&["rev-parse", "HEAD"]);

    let all = f.repo.log(&LogQuery { revs: vec!["HEAD".into()], ..Default::default() }).unwrap();
    assert_eq!(all.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), vec![in_body.as_str(), in_subject.as_str(), plain.as_str()]);
    assert_eq!(all[0].body, "body one\x1ebody two");
    assert_eq!(f.repo.commit(&in_subject).unwrap().sha, in_subject);
}

/// Both of these are in the graph's way when `-z` isn't used: a body's last newline, and a
/// line-range log's own format.
#[test]
fn every_kind_of_log_still_parses() {
    let f = Fx::new();
    f.write("a.txt", "1\n2\n");
    f.commit("one\n\nbody\n\nmore");
    f.write("a.txt", "1\nchanged\n");
    f.commit("two");
    let by_lines = f.repo.log(&LogQuery { revs: vec!["HEAD".into()], line_range: Some((2, 2, "a.txt".into())), ..Default::default() }).unwrap();
    assert_eq!(by_lines.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), vec!["two", "one"]);
    let followed = f.repo.log(&LogQuery { revs: vec!["HEAD".into()], paths: vec!["a.txt".into()], follow: true, literal_paths: true, ..Default::default() }).unwrap();
    assert_eq!(followed.len(), 2);
    assert_eq!(followed[1].body, "body\n\nmore");
}

#[test]
fn history_names_each_commit_by_the_file_it_had_then() {
    let f = Fx::new();
    f.write("src/old.txt", "one\ntwo\nthree\nfour\nfive\n");
    let created = f.commit("create");
    f.q(&["mv", "src/old.txt", "src/new.txt"]);
    let moved = f.commit("move");
    f.write("src/new.txt", "one\ntwo\nthree\nfour\nFIVE\n");
    let edited = f.commit("edit");
    let points = f.repo.file_history_points("src/new.txt", 10).unwrap();
    let got: Vec<(&str, &str)> = points.iter().map(|p| (p.sha.as_str(), p.path.as_str())).collect();
    assert_eq!(got, vec![(edited.as_str(), "src/new.txt"), (moved.as_str(), "src/new.txt"), (created.as_str(), "src/old.txt")]);
    for p in &points {
        f.repo.file_at(&p.sha, &p.path).unwrap();
    }
    assert_eq!(f.repo.prev_revision("src/new.txt", &moved).map(|x| x.1).as_deref(), Some("src/old.txt"));
}

#[test]
fn a_renamed_file_clicked_on_its_own_still_shows_as_a_rename() {
    let f = Fx::new();
    f.write("old.txt", "one\ntwo\nthree\nfour\nfive\n");
    f.commit("create");
    f.q(&["mv", "old.txt", "new.txt"]);
    f.write("new.txt", "one\ntwo\nthree\nfour\nFIVE\n");
    f.q(&["add", "-A"]);
    let staged = f.repo.diff(&DiffSource::Staged, Some("new.txt"), &DiffOpts::default()).unwrap();
    assert!(staged[0].is_rename && !staged[0].is_new, "{:?}", staged[0].header);
    assert_eq!((staged[0].additions(), staged[0].deletions()), (1, 1));
    let sha = f.commit("rename and edit");
    let shown = f.repo.diff(&DiffSource::Commit(sha), Some("new.txt"), &DiffOpts::default()).unwrap();
    assert!(shown[0].is_rename && !shown[0].is_new);
    assert_eq!(shown[0].old_path, "old.txt");
    // A file that really is new has no rename to find.
    f.write("fresh.txt", "completely different\n");
    let sha = f.commit("add");
    let fresh = f.repo.diff(&DiffSource::Commit(sha), Some("fresh.txt"), &DiffOpts::default()).unwrap();
    assert!(fresh[0].is_new);
}

#[test]
fn names_with_quotes_and_tabs_arrive_whole() {
    if cfg!(windows) {
        // Windows can't have these names in a working tree. Through the index they can
        // only reach `file_activity`, which is what's checked here.
        let f = Fx::new();
        f.write("a", "a\n");
        f.commit("base");
        let blob = f.q(&["rev-parse", "HEAD:a"]);
        f.q(&["-c", "core.protectNTFS=false", "update-index", "--add", "--cacheinfo", &format!("100644,{blob},q\"uo te.txt")]);
        f.q(&["-c", "core.protectNTFS=false", "commit", "-qm", "odd name"]);
        let names: Vec<String> = f.repo.file_activity("HEAD", 10).unwrap().into_iter().map(|x| x.0).collect();
        assert!(names.contains(&"q\"uo te.txt".to_string()), "{names:?}");
    }
}

#[test]
fn clean_preview_lists_plain_names() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    f.write("my file.txt", "x\n");
    assert_eq!(f.repo.clean_preview(false, false).unwrap(), vec!["my file.txt"]);
}

#[test]
fn a_nested_repository_is_recognised() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    setup(&f.root.join("nested").tap_mkdir());
    assert!(f.repo.is_nested_repo("nested/"));
    assert!(!f.repo.is_nested_repo("a"));
}

trait TapMkdir {
    fn tap_mkdir(self) -> Self;
}
impl TapMkdir for PathBuf {
    fn tap_mkdir(self) -> Self {
        std::fs::create_dir_all(&self).unwrap();
        self
    }
}

// ---- config that changes what git prints ---------------------------------------------

#[test]
fn colour_settings_and_an_external_tool_stay_out_of_patches() {
    let f = Fx::new();
    f.write("z.txt", "a\n");
    f.commit("base");
    f.write("z.txt", "b\n");
    f.q(&["config", "color.diff", "always"]);
    let t = f.repo.diff_text(&DiffSource::WorkingVsHead).unwrap();
    assert!(t.starts_with("diff --git a/z.txt b/z.txt"), "{t:?}");
    assert!(!String::from_utf8_lossy(&f.repo.patch_bytes(&DiffSource::WorkingVsHead).unwrap()).contains('\x1b'));
    f.q(&["config", "--unset", "color.diff"]);

    let tool = f.root.join(".git/ext.sh");
    std::fs::write(&tool, "#!/bin/sh\necho EXTERNAL-TOOL-RAN\n").unwrap();
    f.q(&["config", "diff.external", &tool.to_string_lossy().replace('\\', "/")]);
    let repo = Repo::open(git(), &f.root).unwrap();
    assert!(!repo.diff_text(&DiffSource::Unstaged).unwrap().contains("EXTERNAL"));
    f.q(&["config", "--unset", "diff.external"]);

    let sha = f.q(&["rev-parse", "HEAD"]);
    f.q(&["config", "color.diff", "always"]);
    assert!(!f.repo.diff_text(&DiffSource::Commit(sha)).unwrap().contains('\x1b'));
}

#[test]
fn prefix_settings_stay_out_of_paths() {
    let f = Fx::new();
    f.write("z.txt", "a\n");
    f.commit("base");
    f.write("z.txt", "b\n");
    f.q(&["config", "diff.srcPrefix", "SRC/"]);
    f.q(&["config", "diff.dstPrefix", "DST/"]);
    let d = f.repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    assert_eq!((d[0].old_path.as_str(), d[0].new_path.as_str()), ("z.txt", "z.txt"));
    assert!(f.repo.diff_text(&DiffSource::Unstaged).unwrap().starts_with("diff --git a/z.txt b/z.txt"));
}

#[test]
fn a_root_commit_shows_its_files_whatever_log_show_root_says() {
    let f = Fx::new();
    f.write("z.txt", "a\n");
    let sha = f.commit("base");
    f.q(&["config", "log.showRoot", "false"]);
    assert_eq!(f.repo.commit_files(&sha).unwrap().len(), 1);
    assert_eq!(f.repo.diff(&DiffSource::Commit(sha), None, &DiffOpts::default()).unwrap().len(), 1);
}

#[test]
fn suppress_blank_empty_doesnt_cut_a_hunk_short() {
    let f = Fx::new();
    f.write("z.txt", "a\n\nb\n\nc\n");
    f.commit("base");
    f.write("z.txt", "a\n\nB\n\nc\n");
    f.q(&["config", "diff.suppressBlankEmpty", "true"]);
    let d = f.repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    assert_eq!(d[0].hunks[0].lines.len(), 6, "{:?}", d[0].hunks[0].lines);
    let patch = gitem_core::diff::build_patch(&d[0], 0, None, gitem_core::diff::PatchMode::Forward).unwrap();
    f.run(ops::apply_patch("Stage", patch, true, false));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), "z.txt");
}

/// Config lists only what someone set, and reads back what the repository really says.
#[test]
fn the_config_list_has_no_entries_of_gitems_own() {
    let f = Fx::new();
    let list = f.repo.config_list().unwrap();
    let phantom: Vec<String> = list.iter().filter(|e| e.scope == "command").map(|e| e.key.clone()).collect();
    assert!(phantom.is_empty(), "{phantom:?}");
    f.q(&["config", "core.pager", "less"]);
    assert_eq!(f.repo.config_get("core.pager").as_deref(), Some("less"));
}

#[test]
fn hooks_path_follows_home_as_git_does() {
    let f = Fx::new();
    f.q(&["config", "core.hooksPath", "~/my-hooks"]);
    let hooks = f.repo.hooks();
    assert!(!hooks[0].path.contains('~'), "{}", hooks[0].path);
    assert!(hooks[0].path.contains("my-hooks"), "{}", hooks[0].path);
    f.q(&["config", "core.hooksPath", "relative-hooks"]);
    let shown = f.repo.hooks()[0].path.replace('\\', "/");
    let want = format!("{}/relative-hooks", f.repo.root.to_string_lossy().replace('\\', "/"));
    assert!(shown.starts_with(&want), "{shown} under {want}");
}

// ---- prepared messages ----------------------------------------------------------------

fn conflicted_pick(f: &Fx, msg: &str) {
    f.write("c.txt", "base\n");
    f.commit("base");
    f.q(&["switch", "-qc", "other"]);
    f.write("c.txt", "theirs\n");
    let sha = f.commit(msg);
    f.q(&["switch", "-q", "main"]);
    f.write("c.txt", "ours\n");
    f.commit("ours");
    let o = git().run_raw(Some(&f.root), &["cherry-pick", &sha]).unwrap();
    assert!(!o.ok());
}

#[test]
fn a_subject_starting_with_a_hash_survives_into_the_prepared_message() {
    let f = Fx::new();
    conflicted_pick(&f, "#123 fix login\n\nSee #456 for context\n#789 is related");
    let m = f.repo.prepared_message().unwrap();
    assert_eq!(m, "#123 fix login\n\nSee #456 for context\n#789 is related");
}

#[test]
fn another_comment_character_doesnt_leak_the_conflict_list() {
    let f = Fx::new();
    f.q(&["config", "core.commentChar", ";"]);
    conflicted_pick(&f, "plain subject\n\n# a heading kept in the body");
    let m = f.repo.prepared_message().unwrap();
    assert_eq!(m, "plain subject\n\n# a heading kept in the body");
}

#[test]
fn a_message_that_mentions_conflicts_keeps_it() {
    let f = Fx::new();
    conflicted_pick(&f, "fix\n\nResolves Conflicts:\nnone really");
    assert_eq!(f.repo.prepared_message().unwrap(), "fix\n\nResolves Conflicts:\nnone really");
}

#[test]
fn a_squash_merge_offers_its_message_and_says_it_is_pending() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    f.q(&["switch", "-qc", "side"]);
    f.write("s", "s\n");
    f.commit("side work");
    f.q(&["switch", "-q", "main"]);
    assert!(!f.repo.status().unwrap().squash_pending);
    f.run(ops::merge("side", ops::MergeMode::Squash));
    let st = f.repo.status().unwrap();
    assert!(st.squash_pending);
    assert_eq!(st.op, RepoOp::None);
    assert!(f.repo.prepared_message().unwrap().contains("side work"));
    f.q(&["commit", "-qm", "squashed"]);
    assert!(!f.repo.status().unwrap().squash_pending, "git clears it on commit");
}

// ---- remotes --------------------------------------------------------------------------

#[test]
fn a_remote_at_a_path_with_spaces_reads_whole() {
    let f = Fx::new();
    f.q(&["remote", "add", "origin", "C:/my repos/x.git"]);
    let r = f.repo.remotes().unwrap();
    assert_eq!((r[0].fetch_url.as_str(), r[0].push_url.as_str()), ("C:/my repos/x.git", "C:/my repos/x.git"));
}

// ---- many files -----------------------------------------------------------------------

/// Well past what fits on a Windows command line, and with names that would be globs.
#[test]
fn thousands_of_files_can_be_staged_unstaged_discarded_and_deleted() {
    let f = Fx::new();
    let long = "a-rather-long-folder-name-for-this-test/another-rather-long-folder-name-here";
    let names: Vec<String> = (0..1200).map(|i| format!("{long}/[{i}]-with-a-long-file-name-too.txt")).collect();
    std::fs::create_dir_all(f.root.join(long)).unwrap();
    for n in &names {
        std::fs::write(f.root.join(n), "one\n").unwrap();
    }
    assert!(names.iter().map(String::len).sum::<usize>() > 100_000);
    f.write("base", "b\n");
    f.q(&["add", "base"]);
    f.q(&["commit", "-qm", "base"]);

    f.run(ops::stage_paths(&names));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]).lines().count(), 1200);
    f.run(ops::unstage_paths(&names, true));
    assert_eq!(f.q(&["diff", "--cached", "--name-only"]), "");
    f.run(ops::stage_paths(&names));
    f.q(&["commit", "-qm", "all"]);

    for n in &names {
        std::fs::write(f.root.join(n), "two\n").unwrap();
    }
    // `stash` passes the names to another git on its command line, however it got them.
    let too_many = ops::stash_push(&ops::StashOpts { paths: names.clone(), ..Default::default() });
    assert!(too_many.run(&f.repo.git, Some(&f.root), None, &mut |_| {}).unwrap_err().to_string().contains("too many"));
    assert_eq!(f.q(&["stash", "list"]), "", "nothing was stashed");
    assert_eq!(f.q(&["diff", "--name-only"]).lines().count(), 1200, "and nothing was taken out of the tree");
    f.run(ops::stash_push(&ops::StashOpts { paths: names[..100].to_vec(), ..Default::default() }));
    assert_eq!(f.q(&["diff", "--name-only"]).lines().count(), 1100);
    f.q(&["stash", "drop", "-q"]);
    for n in &names {
        std::fs::write(f.root.join(n), "two\n").unwrap();
    }
    f.run(ops::discard_paths(&names));
    assert_eq!(f.q(&["status", "--porcelain"]), "");

    let new: Vec<String> = (0..1200).map(|i| format!("{long}/new-{i}-with-a-long-file-name-too.txt")).collect();
    for n in &new {
        std::fs::write(f.root.join(n), "x\n").unwrap();
    }
    let op = ops::delete_untracked(&new);
    assert!(op.steps.len() > 1, "git clean takes no list from stdin, so this comes in batches");
    f.run(op);
    assert_eq!(f.q(&["status", "--porcelain"]), "");
}

/// A short list still shows its names, because the confirm dialog is where they're read.
#[test]
fn a_short_list_keeps_its_names_on_show() {
    let op = ops::discard_paths(&["a b.txt".to_string()]);
    assert_eq!(op.preview(), "git restore --worktree -- a b.txt");
    assert_eq!(op.steps[0].args.last().unwrap(), ":(literal)a b.txt");
}

// ---- hooks -----------------------------------------------------------------------------

/// The environment a hook sees, for each kind of op that names files.
#[test]
fn hooks_of_literal_ops_dont_inherit_literal_pathspecs() {
    let f = Fx::new();
    f.write("[a].txt", "1\n");
    f.commit("base");
    let log = f.root.join(".git/hook.log");
    let script = format!("#!/bin/sh\necho \"literal=[$GIT_LITERAL_PATHSPECS]\" >> \"{}\"\n", log.to_string_lossy().replace('\\', "/"));
    for hook in ["post-index-change", "post-checkout"] {
        std::fs::write(f.root.join(".git/hooks").join(hook), &script).unwrap();
    }
    f.write("[a].txt", "2\n");
    f.write("a.txt", "keep\n");
    f.q(&["add", "a.txt"]);
    f.q(&["commit", "-qm", "another"]);
    let lines = |f: &Fx| std::fs::read_to_string(f.root.join(".git/hook.log")).unwrap_or_default();
    std::fs::remove_file(&log).ok();

    f.run(ops::stage_paths(&["[a].txt".to_string()]));
    assert!(!lines(&f).is_empty(), "the hook has to have run for this to mean anything");
    f.run(ops::unstage_paths(&["[a].txt".to_string()], true));
    f.run(ops::discard_paths(&["[a].txt".to_string()]));
    f.write("[a].txt", "3\n");
    f.run(ops::stash_push(&ops::StashOpts { paths: vec!["[a].txt".to_string()], ..Default::default() }));
    f.run(ops::restore_file_from("HEAD", "[a].txt"));
    assert!(lines(&f).lines().all(|l| l == "literal=[]"), "{}", lines(&f));
    assert_eq!(f.q(&["log", "-1", "--format=%s"]), "another", "and the bracket name never matched a.txt as a glob");
}

// ---- repositories of other shapes ------------------------------------------------------

#[test]
fn a_bare_repository_says_so() {
    let dir = tempfile::tempdir().unwrap();
    git().run(Some(dir.path()), &["init", "-q", "--bare", "-b", "main"]).unwrap();
    let e = Repo::open(git(), dir.path()).err().expect("no working tree to open").to_string();
    assert!(e.contains("bare repository") && !e.contains("fatal"), "{e}");
}

#[test]
fn a_git_folder_says_where_to_go_instead() {
    let f = Fx::new();
    let e = Repo::open(git(), &f.root.join(".git")).err().expect("no working tree to open").to_string();
    assert!(e.contains(".git folder") && !e.contains("fatal"), "{e}");
}

#[test]
fn a_bare_repository_lists_its_worktrees() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    let bare = tempfile::tempdir().unwrap();
    let url = f.root.to_string_lossy().replace('\\', "/");
    git().run(Some(bare.path()), &["clone", "-q", "--bare", &url, "b.git"]).unwrap();
    let b = bare.path().join("b.git");
    let wt = bare.path().join("tree");
    let dir = format!("--git-dir={}", b.to_string_lossy().replace('\\', "/"));
    git().run(None, &[&dir, "worktree", "add", "-q", &wt.to_string_lossy().replace('\\', "/"), "main"]).unwrap();
    let e = Repo::open(git(), &b).err().expect("no working tree to open").to_string();
    assert!(e.contains("tree"), "{e}");
}

#[test]
fn an_empty_repository_gives_empty_answers_rather_than_errors() {
    let f = Fx::new();
    assert!(f.repo.history_points(None, "HEAD", 10).unwrap().is_empty());
    assert!(f.repo.file_sizes("HEAD").unwrap().is_empty());
    assert!(f.repo.file_activity("HEAD", 10).unwrap().is_empty());
    assert!(f.repo.contributors("HEAD", true).unwrap().is_empty());
    assert!(f.repo.commit_times("HEAD", None, "2000-01-01").unwrap().is_empty());
    assert!(f.repo.reflog("HEAD", 10).unwrap().is_empty());
    assert!(f.repo.tracked_files(Some("HEAD")).unwrap().is_empty());
    assert!(f.repo.people("HEAD").unwrap().is_empty());
    assert!(f.repo.resolve("HEAD").unwrap_err().to_string().contains("no commits yet"));
    // Nothing staged yet: nothing to show, and no error.
    assert!(f.repo.diff(&DiffSource::WorkingVsHead, None, &DiffOpts::default()).unwrap().is_empty());
    f.write("new.txt", "hello\n");
    f.q(&["add", "new.txt"]);
    assert!(f.repo.diff_text(&DiffSource::WorkingVsHead).unwrap().contains("+hello"));
    let p = String::from_utf8(f.repo.patch_bytes(&DiffSource::WorkingVsHead).unwrap()).unwrap();
    assert!(p.contains("+hello"), "{p}");
    assert_eq!(f.repo.diff(&DiffSource::WorkingVsHead, Some("new.txt"), &DiffOpts::default()).unwrap().len(), 1);
}

#[test]
fn a_broken_submodule_mapping_is_an_error_not_an_empty_list() {
    let f = Fx::new();
    f.write("a", "a\n");
    f.commit("base");
    let inner = tempfile::tempdir().unwrap();
    setup(inner.path());
    git().run(Some(inner.path()), &["commit", "-q", "--allow-empty", "-m", "x"]).unwrap();
    let sha = git().run(Some(inner.path()), &["rev-parse", "HEAD"]).unwrap().text().trim().to_string();
    f.q(&["update-index", "--add", "--cacheinfo", &format!("160000,{sha},lib")]);
    f.write(".gitmodules", "[submodule \"other\"]\n\tpath = elsewhere\n\turl = https://example.invalid/x\n");
    let e = f.repo.submodules().unwrap_err().to_string();
    assert!(e.contains("lib"), "{e}");
}

/// A blobless clone has only history. Line counts mustn't download its files one at a time.
#[test]
fn line_counts_dont_download_a_partial_clones_files() {
    let f = Fx::new();
    for i in 0..4 {
        f.write("a.txt", &format!("{i}\n"));
        f.commit(&format!("c{i}"));
    }
    f.q(&["config", "uploadpack.allowFilter", "true"]);
    let dest = tempfile::tempdir().unwrap();
    let url = format!("file:///{}", f.root.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
    git().run(Some(dest.path()), &["clone", "-q", "--filter=blob:none", "--no-checkout", &url, "c"]).unwrap();
    let root = dest.path().join("c");
    let g = git();
    let missing = || g.run(Some(&root), &["rev-list", "--objects", "--all", "--missing=print", "--quiet"]).unwrap().text().lines().filter(|l| l.starts_with('?')).count();
    let before = missing();
    assert!(before > 0, "the clone has to be missing something for this to mean anything");
    let repo = Repo::open(git(), &root).unwrap();

    let stats = repo.commit_stats(&["HEAD".to_string()], false, 10);
    assert_eq!(missing(), before, "the graph's Changes column fetched files: {stats:?}");

    let history = repo.history_points(None, "HEAD", 10).unwrap();
    assert_eq!(history.len(), 4, "the commits still show, whatever their counts");
    let file = repo.file_history_points("a.txt", 10);
    let people = repo.contributors("HEAD", true).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(missing(), before, "a background read fetched files: {file:?}");

    // Asking for a commit's own diff is someone asking, so that does fetch.
    let head = repo.resolve("HEAD").unwrap();
    assert_eq!(repo.diff(&DiffSource::Commit(head), None, &DiffOpts::default()).unwrap().len(), 1);
    assert!(missing() < before);
}

#[test]
fn a_confirm_preview_names_the_files_even_when_they_go_over_stdin() {
    let names: Vec<String> = (0..1200).map(|i| format!("folder/a-really-rather-long-file-name-number-{i}.txt")).collect();
    let preview = gitem_core::ops::discard_paths(&names).preview();
    assert!(!preview.contains("--pathspec-from-file"), "{}", &preview[..200.min(preview.len())]);
    assert!(preview.contains("number-0.txt") && preview.contains("and 1195 more"), "{}", &preview[..200.min(preview.len())]);
    assert!(preview.len() < 1000, "a question stays short: {}", preview.len());
}
