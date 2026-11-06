//! Property tests: random input, with the failing case shrunk to a small one.
//!
//! Case counts are small so the normal run stays quick. For a longer hunt, set
//! `PROPTEST_CASES`, for example `PROPTEST_CASES=2000 cargo test --test properties`.
//! Tests marked with a bug's ID fail until that bug is fixed: run them with `--ignored`.
gitem_testenv::isolate!();

use std::path::PathBuf;

use gitem_core::diff::{self, FileDiff, LineKind, PatchMode, build_patch};
use gitem_core::forge::parse_remote_url;
use gitem_core::merge::{MergeDoc, Region, Resolution, merge3};
use gitem_core::ops::{self, split_command_line};
use gitem_core::rebase::{RebaseAction, TodoItem, parse_todo, render_todo};
use gitem_core::repo::{DiffOpts, DiffSource};
use gitem_core::{GitExe, Repo, parse, util};
use proptest::prelude::*;
use proptest::sample::{Index, select};
use proptest::test_runner::{Config, TestRunner};

fn config(default_cases: u32) -> Config {
    let cases = std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(default_cases);
    // No regression files: a failure prints its shrunk case, and a bug that stays
    // gets a test here. Shrinking is bounded because each step can run git.
    Config { cases, failure_persistence: None, max_shrink_iters: 300, ..Config::default() }
}

fn check<T: Strategy>(cases: u32, strategy: T, test: impl Fn(T::Value) -> Result<(), TestCaseError>) {
    TestRunner::new(config(cases)).run(&strategy, test).unwrap();
}

/// Real git, in one throwaway repository per test.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
    repo: Repo,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut git = GitExe::discover(None).unwrap();
        git.helper = None;
        git.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false"), ("gc.auto", "0")] {
            git.run(Some(&root), &["config", k, v]).unwrap();
        }
        let repo = Repo::open(git.clone(), &root).unwrap();
        Self { _dir: dir, root, git, repo }
    }
    fn q(&self, args: &[&str]) {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }
    fn write(&self, bytes: &[u8]) {
        std::fs::write(self.root.join("f.txt"), bytes).unwrap();
    }
    fn on_disk(&self) -> Vec<u8> {
        std::fs::read(self.root.join("f.txt")).unwrap()
    }
    fn in_index(&self) -> Vec<u8> {
        self.git.run(Some(&self.root), &["show", ":f.txt"]).unwrap().stdout
    }
    /// HEAD and the index hold `base`, the file holds `edited`, and `edited` is staged too if asked.
    fn prepare(&self, base: &[u8], edited: &[u8], staged: bool) {
        self.write(base);
        self.q(&["add", "f.txt"]);
        self.q(&["commit", "-q", "--allow-empty", "-m", "x"]);
        self.write(edited);
        if staged {
            self.q(&["add", "f.txt"]);
        }
    }
}

// ---- build_patch, through real git apply --------------------------------------------

/// A file as lines, each without its line end. Only the last can lack a newline.
#[derive(Debug, Clone, PartialEq)]
struct Text {
    lines: Vec<String>,
    final_newline: bool,
}

impl Text {
    fn bytes(&self) -> Vec<u8> {
        let mut s = self.lines.join("\n");
        if self.final_newline && !self.lines.is_empty() {
            s.push('\n');
        }
        s.into_bytes()
    }
}

/// Repeated lines and text that looks like diff syntax, where a patch could go wrong.
const LINES: &[&str] = &["a", "b", "c", "", "  indented", "\ttabbed", "-minus", "+plus", "@@ -1 +1 @@", "\\ backslash", "diff --git a/x b/x", "--- a/x", "caf\u{e9}", "crlf\r", "\r", "trailing "];

#[derive(Debug, Clone)]
enum Edit {
    Replace(Index, String),
    Insert(Index, String),
    Delete(Index),
}

fn edit() -> impl Strategy<Value = Edit> {
    let line = || select(LINES).prop_map(String::from);
    prop_oneof![(any::<Index>(), line()).prop_map(|(i, l)| Edit::Replace(i, l)), (any::<Index>(), line()).prop_map(|(i, l)| Edit::Insert(i, l)), any::<Index>().prop_map(Edit::Delete)]
}

fn edited(base: &[String], edits: &[Edit]) -> Vec<String> {
    let mut out = base.to_vec();
    for e in edits {
        match e {
            Edit::Replace(i, l) if !out.is_empty() => {
                let at = i.index(out.len());
                out[at] = l.clone();
            }
            Edit::Insert(i, l) => {
                let at = i.index(out.len() + 1);
                out.insert(at, l.clone());
            }
            Edit::Delete(i) if !out.is_empty() => {
                let at = i.index(out.len());
                out.remove(at);
            }
            _ => {}
        }
    }
    out
}

/// A file, a version of it with a few edits, and which changed lines get picked.
#[derive(Debug, Clone)]
struct Case {
    old: Text,
    new: Text,
    context: u32,
    picks: Vec<bool>,
}

fn case(no_newline: bool) -> impl Strategy<Value = Case> {
    let eol = move || if no_newline { any::<bool>().boxed() } else { Just(true).boxed() };
    (proptest::collection::vec(select(LINES).prop_map(String::from), 0..30), proptest::collection::vec(edit(), 1..7), eol(), eol(), 1u32..5, proptest::collection::vec(any::<bool>(), 16)).prop_map(|(base, edits, old_eol, new_eol, context, picks)| {
        let new = edited(&base, &edits);
        Case { old: Text { lines: base, final_newline: old_eol }, new: Text { lines: new, final_newline: new_eol }, context, picks }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Route {
    Stage,
    Unstage,
    Discard,
}

/// The changed lines picked in each hunk, as the view passes them. `whole` leaves
/// every line of every hunk picked; the view's hunk buttons pass `None`, tested apart.
fn picked(f: &FileDiff, c: &Case, whole: bool) -> Vec<Vec<usize>> {
    let mut next = c.picks.iter().cycle();
    f.hunks
        .iter()
        .map(|h| {
            let changed = h.lines.iter().enumerate().filter(|(_, l)| matches!(l.kind, LineKind::Add | LineKind::Del));
            changed.filter(|_| whole || *next.next().unwrap()).map(|(i, _)| i).collect()
        })
        .collect()
}

/// What the patched file should say, worked out from the diff's line numbers and the
/// two versions, with no patch involved. Forward starts from the old version and takes
/// picked changes. Reverse starts from the new one and undoes them.
fn expected(c: &Case, f: &FileDiff, picks: &[Vec<usize>], mode: PatchMode) -> Vec<u8> {
    let (src, src_eol) = if mode == PatchMode::Forward { (&c.old.lines, c.old.final_newline) } else { (&c.new.lines, c.new.final_newline) };
    let mut out: Vec<(String, bool)> = Vec::new();
    let line_of = |from: &Text, n: usize| (from.lines[n].clone(), n + 1 < from.lines.len() || from.final_newline);
    let keep = |i: usize| (src[i].clone(), i + 1 < src.len() || src_eol);
    let mut i = 0usize;
    for (hi, h) in f.hunks.iter().enumerate() {
        let (start, count) = if mode == PatchMode::Forward { (h.old_start as usize, h.old_count) } else { (h.new_start as usize, h.new_count) };
        // With nothing on this side, its start names the line before the hunk.
        let first = if count == 0 { start } else { start - 1 };
        while i < first {
            out.push(keep(i));
            i += 1;
        }
        for (k, l) in h.lines.iter().enumerate() {
            let selected = picks[hi].contains(&k);
            match (l.kind, mode) {
                (LineKind::NoNewline, _) => {}
                (LineKind::Context, _) => {
                    out.push(keep(i));
                    i += 1;
                }
                (LineKind::Del, PatchMode::Forward) | (LineKind::Add, PatchMode::Reverse) => {
                    if !selected {
                        out.push(keep(i));
                    }
                    i += 1;
                }
                (LineKind::Add, PatchMode::Forward) => {
                    if selected {
                        out.push(line_of(&c.new, l.new_no.unwrap() as usize - 1));
                    }
                }
                (LineKind::Del, PatchMode::Reverse) => {
                    if selected {
                        out.push(line_of(&c.old, l.old_no.unwrap() as usize - 1));
                    }
                }
            }
        }
    }
    while i < src.len() {
        out.push(keep(i));
        i += 1;
    }
    // A line that used to be last, and is no longer, has to gain a newline.
    let last = out.len().wrapping_sub(1);
    let mut bytes = Vec::new();
    for (n, (text, eol)) in out.iter().enumerate() {
        bytes.extend_from_slice(text.as_bytes());
        if *eol || n != last {
            bytes.push(b'\n');
        }
    }
    bytes
}

/// One run of the view's flow: read the diff, then apply each hunk's patch, bottom up.
fn round_trip(fx: &Fx, c: &Case, route: Route, whole: bool) -> Result<(), TestCaseError> {
    let (old, new) = (c.old.bytes(), c.new.bytes());
    fx.prepare(&old, &new, route == Route::Unstage);
    let src = if route == Route::Unstage { DiffSource::Staged } else { DiffSource::Unstaged };
    let opts = DiffOpts { context: c.context, ..DiffOpts::default() };
    let files = fx.repo.diff(&src, Some("f.txt"), &opts).unwrap();
    let Some(f) = files.first() else { return Ok(()) };
    let picks = picked(f, c, whole);
    let mode = if route == Route::Stage { PatchMode::Forward } else { PatchMode::Reverse };
    let want = expected(c, f, &picks, mode);
    let (before_index, before_disk) = (fx.in_index(), fx.on_disk());
    for h in (0..f.hunks.len()).rev() {
        let patch = if whole { build_patch(f, h, None, mode) } else { build_patch(f, h, Some(&picks[h]), mode) };
        let Some(patch) = patch else { continue };
        let op = ops::apply_patch("apply", patch.clone(), route != Route::Discard, route != Route::Stage);
        let applied = op.run(&fx.git, Some(&fx.root), None, &mut |_| {});
        prop_assert!(applied.is_ok(), "git refused the patch: {applied:?}\n{patch}");
    }
    let got = if route == Route::Discard { fx.on_disk() } else { fx.in_index() };
    prop_assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "the wrong text after {:?}", route);
    if route == Route::Discard {
        prop_assert_eq!(fx.in_index(), before_index, "a discard leaves the index alone");
    } else {
        prop_assert_eq!(fx.on_disk(), before_disk, "staging leaves the file alone");
    }
    Ok(())
}

#[test]
fn staging_picked_lines_puts_exactly_those_in_the_index() {
    let fx = Fx::new();
    check(16, case(false), |c| round_trip(&fx, &c, Route::Stage, false));
}

#[test]
fn unstaging_picked_lines_takes_exactly_those_out_of_the_index() {
    let fx = Fx::new();
    check(16, case(false), |c| round_trip(&fx, &c, Route::Unstage, false));
}

#[test]
fn discarding_picked_lines_undoes_exactly_those_in_the_file() {
    let fx = Fx::new();
    check(16, case(false), |c| round_trip(&fx, &c, Route::Discard, false));
}

/// Whole hunks keep their relationship to the end of the file, so a missing final
/// newline can't move into the middle of it. That case is the next test.
#[test]
fn whole_hunks_apply_exactly_even_without_a_final_newline() {
    let fx = Fx::new();
    check(6, case(true), |c| {
        round_trip(&fx, &c, Route::Stage, true)?;
        round_trip(&fx, &c, Route::Unstage, true)?;
        round_trip(&fx, &c, Route::Discard, true)
    });
}

/// `keep\nold` changed to `keep\nnew`, picking only `+new`, gives `keep\noldnew`.
#[test]
#[ignore = "VC-6, Phase 3"]
fn picked_lines_next_to_a_missing_final_newline_stay_separate_lines() {
    let fx = Fx::new();
    check(32, case(true), |c| {
        round_trip(&fx, &c, Route::Stage, false)?;
        round_trip(&fx, &c, Route::Unstage, false)?;
        round_trip(&fx, &c, Route::Discard, false)
    });
}

/// The smallest case behind the property above, spelled out.
#[test]
#[ignore = "VC-6, Phase 3"]
fn staging_a_line_beside_a_missing_final_newline_keeps_both_lines_apart() {
    let fx = Fx::new();
    fx.prepare(b"keep\nold", b"keep\nnew", false);
    let f = &fx.repo.diff(&DiffSource::Unstaged, Some("f.txt"), &DiffOpts::default()).unwrap()[0];
    let added = f.hunks[0].lines.iter().position(|l| l.kind == LineKind::Add).unwrap();
    let patch = build_patch(f, 0, Some(&[added]), PatchMode::Forward).unwrap();
    ops::apply_patch("stage", patch, true, false).run(&fx.git, Some(&fx.root), None, &mut |_| {}).unwrap();
    assert_eq!(String::from_utf8_lossy(&fx.in_index()), "keep\nold\nnew");
}

/// A Phase 3 fix may refuse these lines in the view, as the hunk buttons already do.
/// If so, this check belongs with that refusal.
#[test]
#[ignore = "VC-7, Phase 3"]
fn staging_a_line_keeps_bytes_that_are_not_utf8() {
    let fx = Fx::new();
    fx.prepare(b"keep\n", b"keep\ncaf\xe9 added\n", false);
    let f = &fx.repo.diff(&DiffSource::Unstaged, Some("f.txt"), &DiffOpts::default()).unwrap()[0];
    let added = f.hunks[0].lines.iter().position(|l| l.kind == LineKind::Add).unwrap();
    let patch = build_patch(f, 0, Some(&[added]), PatchMode::Forward).unwrap();
    ops::apply_patch("stage", patch, true, false).run(&fx.git, Some(&fx.root), None, &mut |_| {}).unwrap();
    assert_eq!(fx.in_index(), b"keep\ncaf\xe9 added\n");
}

// ---- merge3 --------------------------------------------------------------------------

fn merge_lines() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(select(&["a", "b", "c", "d", "e", ""][..]).prop_map(String::from), 0..14)
}

fn doc_with(regions: Vec<Region>) -> MergeDoc {
    MergeDoc { regions, eol_crlf: false, final_newline: false, bom: false }
}

fn rendered(lines: &[String]) -> String {
    lines.join("\n")
}

/// Rebuilds one input from the regions: nothing was lost, added or moved.
fn side_of(regions: &[Region], side: char) -> Vec<String> {
    let mut out = Vec::new();
    for r in regions {
        let part: &[String] = match (r, side) {
            (Region::Stable(l), _) => l,
            (Region::Auto { base, .. } | Region::Same { base, .. }, 'b') => base,
            (Region::Auto { lines, from_theirs: true, .. }, 't') => lines,
            (Region::Auto { lines, from_theirs: false, .. }, 'o') => lines,
            (Region::Auto { base, .. }, _) => base,
            (Region::Same { lines, .. }, _) => lines,
            (Region::Conflict { base, .. }, 'b') => base,
            (Region::Conflict { ours, .. }, 'o') => ours,
            (Region::Conflict { theirs, .. }, _) => theirs,
        };
        out.extend_from_slice(part);
    }
    out
}

#[test]
fn merge_regions_add_up_to_each_of_the_three_inputs() {
    check(400, (merge_lines(), merge_lines(), merge_lines()), |(b, o, t)| {
        let regions = merge3(&b, &o, &t);
        prop_assert_eq!(side_of(&regions, 'b'), b);
        prop_assert_eq!(side_of(&regions, 'o'), o);
        prop_assert_eq!(side_of(&regions, 't'), t);
        Ok(())
    });
}

#[test]
fn merging_with_an_unchanged_side_gives_the_other_side() {
    check(400, (merge_lines(), merge_lines()), |(b, x)| {
        for (o, t) in [(&b, &x), (&x, &b)] {
            let d = doc_with(merge3(&b, o, t));
            prop_assert_eq!(d.conflict_count(), 0);
            prop_assert_eq!(d.render("o", "t"), rendered(&x));
        }
        Ok(())
    });
}

#[test]
fn merging_the_same_change_on_both_sides_gives_that_change() {
    check(400, (merge_lines(), merge_lines()), |(b, x)| {
        let d = doc_with(merge3(&b, &x, &x));
        prop_assert_eq!(d.conflict_count(), 0);
        prop_assert_eq!(d.render("o", "t"), rendered(&x));
        Ok(())
    });
}

#[test]
fn resolving_every_conflict_takes_that_side_only_where_they_conflict() {
    check(400, (merge_lines(), merge_lines(), merge_lines()), |(b, o, t)| {
        let clean = doc_with(merge3(&b, &o, &t));
        for (res, side) in [(Resolution::Ours, 'o'), (Resolution::Theirs, 't'), (Resolution::Base, 'b')] {
            let mut d = doc_with(clean.regions.clone());
            d.resolve_all(res);
            prop_assert_eq!(d.unresolved_count(), 0);
            // Regions with no conflict stay as they merged, so build what's expected from those.
            let mut want = Vec::new();
            for r in &clean.regions {
                match r {
                    Region::Conflict { .. } => want.extend(side_of(std::slice::from_ref(r), side)),
                    Region::Stable(l) | Region::Auto { lines: l, .. } | Region::Same { lines: l, .. } => want.extend_from_slice(l),
                }
            }
            prop_assert_eq!(d.render("o", "t"), rendered(&want));
        }
        Ok(())
    });
}

/// Git's own answer: does it find a conflict merging these three?
fn git_finds_a_conflict(fx: &Fx, b: &str, o: &str, t: &str) -> bool {
    for (name, text) in [("base.txt", b), ("ours.txt", o), ("theirs.txt", t)] {
        std::fs::write(fx.root.join(name), text).unwrap();
    }
    // Its exit code counts the conflicts.
    fx.git.run_raw(Some(&fx.root), &["merge-file", "-p", "ours.txt", "base.txt", "theirs.txt"]).unwrap().code > 0
}

/// This engine merges these clean while git reports a conflict, so the tool would
/// offer to save the result as resolved.
#[test]
#[ignore = "VC-17, Phase 3"]
fn a_known_conflict_in_git_is_a_conflict_here() {
    let fx = Fx::new();
    let (b, o, t) = ("e\n\nd\nb\na\n", "n0\ne\n\nd\n", "e\n\na\nb\nd\n");
    assert!(git_finds_a_conflict(&fx, b, o, t));
    assert!(MergeDoc::new(b, o, t).conflict_count() > 0, "{}", MergeDoc::new(b, o, t).render("o", "t"));
}

/// Rare: the review saw it in 5 of 400 small files, so this runs many.
#[test]
#[ignore = "VC-17, Phase 3"]
fn a_conflict_in_git_is_a_conflict_here() {
    let fx = Fx::new();
    let text = |l: &[String]| if l.is_empty() { String::new() } else { format!("{}\n", l.join("\n")) };
    check(600, (merge_lines(), merge_lines(), merge_lines()), |(b, o, t)| {
        let (b, o, t) = (text(&b), text(&o), text(&t));
        let found_here = MergeDoc::new(&b, &o, &t).conflict_count() > 0;
        prop_assert!(found_here || !git_finds_a_conflict(&fx, &b, &o, &t), "git found a conflict and this engine didn't");
        Ok(())
    });
}

/// The final newline is taken from ours, whichever side's last line is kept.
#[test]
#[ignore = "VC-16, Phase 3"]
fn taking_theirs_with_ours_unchanged_keeps_their_final_newline() {
    assert_eq!(MergeDoc::new("a\nb", "a\nb", "a\nb\nc\n").render("o", "t"), "a\nb\nc\n");
}

// ---- parsers must survive anything ----------------------------------------------------

/// Every parser over the same text. Their job is to survive git output they didn't
/// expect, including a future version's.
fn all_parsers(s: &str) {
    let mut commits = parse::parse_log(s);
    parse::mark_remote_refs(&mut commits, &["origin".to_string(), s.to_string()]);
    parse::parse_decoration(s);
    parse::parse_branches(s);
    parse::parse_track(s);
    parse::parse_tags(s);
    parse::parse_remotes(s);
    parse::parse_stashes(s);
    parse::parse_worktrees(s, "/current");
    parse::parse_submodules(s);
    parse::parse_status(s);
    parse::parse_blame(s);
    parse::parse_name_status(s);
    parse::parse_numstat(s);
    parse::parse_reflog(s);
    parse::parse_config(s);
    parse::parse_stat_log(s);
    parse::parse_commit_stats(s);
    parse::parse_ls_tree_sizes(s);
    for f in diff::parse_unified(s) {
        diff::side_by_side(&f);
        for h in 0..f.hunks.len() {
            for mode in [PatchMode::Forward, PatchMode::Reverse] {
                build_patch(&f, h, None, mode);
                build_patch(&f, h, Some(&[0, 1, usize::MAX]), mode);
            }
        }
    }
    parse_todo(s);
    split_command_line(s);
    parse_remote_url(s);
}

/// The tokens the parsers split on, to get deeper than random bytes ever would.
const TOKENS: &[&str] = &[
    "\0", "\n", "\r\n", "\t", " ", "\x1e", ",", ":", "/", "(", ")", "[", "]", "-", "+", "\\", "\"", "@", "=", "..", "...", "é", "日本", "0", "1", "42", "-1", "99999999999999999999",
    "abc123", "HEAD", "HEAD -> ", "tag: ", "refs/stash", "origin/x", "# branch.oid ", "# branch.head ", "# branch.upstream ", "# branch.ab +1 -2", "1 .M N... 100644 100644 100644 a b ", "2 R. N... 100644 100644 100644 a b R100 ", "u UU N... 1 1 1 1 a b c ", "? ", "! ",
    "worktree ", "branch refs/heads/", "detached", "bare", "locked", "prunable", "@@ -", " +", " @@", "@@ -1,2 +1,2 @@\n", "diff --git a/x b/x\n", "diff --cc x\n", "--- a/x\n", "+++ b/x\n", "\\ No newline at end of file\n", "new file mode 100644\n", "rename from a\n", "rename to b\n", "similarity index 90%\n", "Binary files a and b differ\n", "author ", "author-time ", "committer ", "summary ", "filename ", "previous ", "ahead 1", "behind 2", "gone", "=>", "{", "}", "stash@{0}",
];

fn token_soup() -> impl Strategy<Value = String> {
    proptest::collection::vec(select(TOKENS), 0..40).prop_map(|t| t.concat())
}

#[test]
fn parsers_survive_arbitrary_bytes() {
    check(400, proptest::collection::vec(any::<u8>(), 0..300), |bytes| {
        all_parsers(&util::text(&bytes));
        Ok(())
    });
}

#[test]
fn parsers_survive_text_that_looks_like_their_input() {
    check(1500, token_soup(), |s| {
        all_parsers(&s);
        Ok(())
    });
}

#[test]
fn line_helpers_survive_arbitrary_text() {
    check(300, (any::<String>(), any::<String>(), 0usize..40), |(a, b, n)| {
        diff::word_diff(&a, &b);
        util::prefix(&a, n);
        let lines = util::split_lines(&a);
        prop_assert!(lines.iter().all(|l| !l.contains('\n')));
        Ok(())
    });
}

// ---- rebase todo -----------------------------------------------------------------------

fn todo_item() -> impl Strategy<Value = TodoItem> {
    let sha = "[0-9a-f]{7,40}";
    let subject = "[A-Za-z0-9#:!/._-]([A-Za-z0-9 #:!/._-]{0,30}[A-Za-z0-9#:!/._-])?|";
    let with_sha = (select(&RebaseAction::CHOICES[..]), sha, subject).prop_map(|(action, sha, text)| TodoItem { action, sha, text });
    let command = "[a-z]([a-z0-9 ./-]{0,20}[a-z0-9])?";
    let exec = command.prop_map(|text| TodoItem { action: RebaseAction::Exec, sha: String::new(), text });
    let raw = "(label|reset|update-ref) [a-z/]{1,12}|fixup -[Cc] [0-9a-f]{7} [a-z]{1,8}".prop_map(|text| TodoItem { action: RebaseAction::Other, sha: String::new(), text });
    let pause = Just(TodoItem { action: RebaseAction::Break, sha: String::new(), text: String::new() });
    prop_oneof![6 => with_sha, 1 => exec, 1 => raw, 1 => pause]
}

#[test]
fn a_rendered_todo_reads_back_the_same() {
    check(300, proptest::collection::vec(todo_item(), 0..12), |items| {
        prop_assert_eq!(parse_todo(&render_todo(&items)), items);
        Ok(())
    });
}

#[test]
fn rendering_a_parsed_todo_settles_after_one_pass() {
    check(500, any::<String>(), |text| {
        let once = render_todo(&parse_todo(&text));
        prop_assert_eq!(render_todo(&parse_todo(&once)), once);
        Ok(())
    });
}

// ---- console command line --------------------------------------------------------------

/// Quoted the way a person would, for the characters that matter inside double quotes.
fn quoted(arg: &str) -> String {
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn a_quoted_command_line_splits_back_into_the_same_arguments() {
    check(500, proptest::collection::vec(any::<String>(), 0..6), |args| {
        let line = args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(" ");
        prop_assert_eq!(split_command_line(&line), args);
        Ok(())
    });
}

#[test]
fn plain_words_split_on_whitespace() {
    check(300, proptest::collection::vec("[A-Za-z0-9=./_-]{1,12}", 0..8), |words| {
        prop_assert_eq!(split_command_line(&format!("  {}\t", words.join("  \t "))), words);
        Ok(())
    });
}

// ---- remote urls ---------------------------------------------------------------------

#[test]
fn any_form_of_a_remote_url_gives_the_same_host_and_path() {
    let host = "[a-z][a-z0-9-]{0,10}(\\.[a-z]{2,5}){1,2}";
    let path = "[a-zA-Z][a-zA-Z0-9_-]{0,8}(/[a-zA-Z][a-zA-Z0-9_-]{0,8}){0,2}";
    check(500, (host, path, any::<bool>(), any::<bool>()), |(host, path, dot_git, upper)| {
        let suffix = if dot_git { ".git" } else { "" };
        let shown_host = if upper { host.to_uppercase() } else { host.clone() };
        let want = Some((host.clone(), path.clone()));
        for url in [
            format!("https://{shown_host}/{path}{suffix}"),
            format!("https://{shown_host}/{path}{suffix}/"),
            format!("https://{shown_host}:8443/{path}{suffix}"),
            format!("https://user@{shown_host}/{path}{suffix}"),
            format!("https://user:p@ss:w0rd@{shown_host}/{path}{suffix}"),
            format!("ssh://git@{shown_host}/{path}{suffix}"),
            format!("ssh://git@{shown_host}:2222/{path}{suffix}"),
            format!("git@{shown_host}:{path}{suffix}"),
            format!("{shown_host}:{path}{suffix}"),
            format!("  https://{shown_host}/{path}{suffix}\n"),
        ] {
            prop_assert_eq!(parse_remote_url(&url), want.clone(), "{}", url);
        }
        Ok(())
    });
}

#[test]
fn local_paths_are_not_remote_urls() {
    check(200, "[a-zA-Z0-9_/ -]{0,20}", |p| {
        for url in [format!("C:/{p}"), format!("C:\\{p}"), format!("file:///{p}"), format!("/{p}"), format!("./{p}"), format!("../{p}")] {
            prop_assert_eq!(parse_remote_url(&url), None, "{}", url);
        }
        Ok(())
    });
}
