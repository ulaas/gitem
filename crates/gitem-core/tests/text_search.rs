//! Find in files, against real files in a throwaway repository: what's found, what's left
//! out, and that a search stops when asked.
gitem_testenv::isolate!();

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gitem_core::GitExe;
use gitem_core::textsearch::{self, FileMatches, Query, Request, Scope, Summary};
use proptest::prelude::*;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    /// An empty repository.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git().run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
        Self { _dir: dir, root }
    }
    fn write(&self, rel: &str, content: impl AsRef<[u8]>) -> &Self {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
        self
    }
    fn git(&self, args: &[&str]) {
        git().run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }
    fn commit_all(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "commit", "-qm", msg]);
    }
    fn search(&self, q: Query) -> (Vec<FileMatches>, Summary) {
        self.search_in(q, Scope::WorkingTree { folder: None }, HashMap::new())
    }
    fn search_in(&self, q: Query, scope: Scope, overlay: HashMap<String, Arc<str>>) -> (Vec<FileMatches>, Summary) {
        let req = Request { root: self.root.clone(), query: q, scope, overlay, git: Some(git()) };
        let (mut files, summary) = textsearch::start(req, || {}).unwrap().collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        (files, summary)
    }
}

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git");
    g.helper = None;
    g
}

fn q(text: &str) -> Query {
    Query { text: text.into(), ..Default::default() }
}

fn paths(files: &[FileMatches]) -> Vec<&str> {
    files.iter().map(|f| f.path.as_str()).collect()
}

/// Every highlighted piece of every line, in order.
fn hits(files: &[FileMatches]) -> Vec<String> {
    files.iter().flat_map(|f| f.lines.iter().flat_map(|l| l.ranges.iter().map(|r| l.preview[r.start as usize..r.end as usize].to_string()))).collect()
}

#[test]
fn text_is_found_in_every_file_with_line_column_and_count() {
    let f = Fx::new();
    f.write("a.txt", "one\nfind me here\nand find me again, find me\n").write("deep/er/b.rs", "// find me\n").write("c.txt", "nothing\n");
    let (files, s) = f.search(q("find me"));
    assert_eq!(paths(&files), ["a.txt", "deep/er/b.rs"]);
    let a = &files[0];
    assert_eq!(a.matches, 3);
    assert_eq!(a.lines.iter().map(|l| (l.line, l.column)).collect::<Vec<_>>(), [(2, 0), (3, 4)]);
    assert_eq!(a.lines[1].ranges.len(), 2);
    assert_eq!((s.files_matched, s.matches, s.files_searched), (2, 4, 3));
    assert!(!s.limit_hit && !s.cancelled && s.errors.is_empty(), "{s:?}");
}

#[test]
fn case_and_whole_words_are_as_asked() {
    let f = Fx::new();
    f.write("a.txt", "Word word WORD wordy sword\n");
    assert_eq!(hits(&f.search(q("word")).0), ["Word", "word", "WORD", "word", "word"], "any case, anywhere, by default");
    assert_eq!(hits(&f.search(Query { case_sensitive: true, ..q("word") }).0), ["word", "word", "word"]);
    assert_eq!(hits(&f.search(Query { whole_word: true, ..q("word") }).0), ["Word", "word", "WORD"]);
}

#[test]
fn a_regex_is_a_regex_only_when_asked() {
    let f = Fx::new();
    f.write("a.txt", "fn main() {}\nfn helper_2() {}\nf.n()\n");
    assert_eq!(hits(&f.search(Query { regex: true, ..q(r"fn \w+\(") }).0), ["fn main(", "fn helper_2("]);
    assert!(f.search(q(r"fn \w+\(")).0.is_empty(), "as text, nothing has a backslash");
    assert_eq!(hits(&f.search(q("f.n")).0), ["f.n"], "a dot is a dot");
}

#[test]
fn line_ends_dont_show_and_dollar_still_matches_before_crlf() {
    let f = Fx::new();
    f.write("win.txt", "this is the end\r\nend is not last here\r\n");
    let (files, _) = f.search(Query { regex: true, ..q("end$") });
    assert_eq!(files[0].lines.len(), 1);
    assert_eq!(files[0].lines[0].preview, "this is the end", "no \r shown");
    assert_eq!(files[0].lines[0].line, 1);
}

#[test]
fn a_search_across_lines_finds_both() {
    let f = Fx::new();
    f.write("a.txt", "x\nfoo\nbar\ny\nfoo\nbaz\n");
    let (files, s) = f.search(Query { regex: true, ..q(r"foo\nbar") });
    assert_eq!(s.matches, 1);
    assert_eq!(files[0].lines.iter().map(|l| (l.line, l.preview.as_str())).collect::<Vec<_>>(), [(2, "foo"), (3, "bar")]);
    // Pasted text with a line break in it, too.
    assert_eq!(f.search(q("foo\nbaz")).1.matches, 1);
}

#[test]
fn ignored_files_and_gits_own_folder_are_left_out_unless_asked() {
    let f = Fx::new();
    f.write(".gitignore", "target/\n*.log\n").write("src/a.rs", "needle\n").write("target/out.rs", "needle\n").write("x.log", "needle\n").write(".hidden/h.txt", "needle\n");
    let (files, _) = f.search(q("needle"));
    assert_eq!(paths(&files), [".hidden/h.txt", "src/a.rs"], "hidden files count, ignored ones don't");
    let (files, _) = f.search(Query { include_ignored: true, ..q("needle") });
    assert_eq!(paths(&files), [".hidden/h.txt", "src/a.rs", "target/out.rs", "x.log"]);
    // The repository's own config and objects never.
    assert!(f.search(Query { include_ignored: true, ..q("repositoryformatversion") }).0.is_empty());
}

#[test]
fn include_and_exclude_globs_work_as_in_vs_code() {
    let f = Fx::new();
    for p in ["src/a.rs", "src/b.txt", "src/vendor/c.rs", "docs/d.md", "e.rs"] {
        f.write(p, "needle\n");
    }
    let only = |include: &str, exclude: &str| {
        let (files, _) = f.search(Query { include: include.into(), exclude: exclude.into(), ..q("needle") });
        paths(&files).into_iter().map(String::from).collect::<Vec<_>>()
    };
    assert_eq!(only("*.rs", ""), ["e.rs", "src/a.rs", "src/vendor/c.rs"]);
    assert_eq!(only("src", ""), ["src/a.rs", "src/b.txt", "src/vendor/c.rs"], "a folder's name means everything in it");
    assert_eq!(only("", "vendor"), ["docs/d.md", "e.rs", "src/a.rs", "src/b.txt"]);
    assert_eq!(only("*.rs, *.md", "vendor/**"), ["docs/d.md", "e.rs", "src/a.rs"]);
    assert_eq!(only("{docs,src}/*.{md,txt}", ""), ["docs/d.md", "src/b.txt"]);
}

#[test]
fn a_wrong_glob_is_said_up_front() {
    let f = Fx::new();
    let req = Request { root: f.root.clone(), query: Query { include: "a{b".into(), ..q("x") }, scope: Scope::WorkingTree { folder: None }, overlay: HashMap::new(), git: None };
    assert!(textsearch::start(req, || {}).is_err());
}

#[test]
fn binary_files_are_left_out_even_where_text_comes_first() {
    let f = Fx::new();
    let mut late_nul = b"needle\n".repeat(20_000);
    late_nul.extend_from_slice(b"\0binary");
    f.write("bin.dat", b"needle\0\x01\x02").write("late.dat", late_nul).write("ok.txt", "needle\n");
    assert_eq!(paths(&f.search(q("needle")).0), ["ok.txt"]);
}

#[test]
fn utf16_with_a_bom_is_searched_as_text() {
    let f = Fx::new();
    let mut bytes = vec![0xFF, 0xFE];
    for u in "first\r\nsecond needle\r\n".encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    f.write("wide.txt", bytes);
    let (files, _) = f.search(q("needle"));
    assert_eq!(paths(&files), ["wide.txt"]);
    assert_eq!((files[0].lines[0].line, files[0].lines[0].preview.as_str()), (2, "second needle"));
}

#[test]
fn files_over_the_size_limit_are_counted_not_searched() {
    let f = Fx::new();
    f.write("big.txt", "needle\n".repeat(1000)).write("small.txt", "needle\n");
    let (files, s) = f.search(Query { max_file_size: 1000, ..q("needle") });
    assert_eq!(paths(&files), ["small.txt"]);
    assert_eq!(s.skipped_large, 1);
    assert_eq!(f.search(Query { max_file_size: 0, ..q("needle") }).0.len(), 2, "0 is no limit");
}

#[test]
fn unsaved_editor_text_is_searched_in_place_of_the_file() {
    let f = Fx::new();
    f.write("a.txt", "old needle\n").write("b.txt", "needle\n");
    let overlay: HashMap<String, Arc<str>> = [("a.txt".to_string(), Arc::from("edited\nnew needle\n")), ("new/unsaved.txt".to_string(), Arc::from("needle too"))].into();
    let (files, _) = f.search_in(q("needle"), Scope::WorkingTree { folder: None }, overlay);
    assert_eq!(paths(&files), ["a.txt", "b.txt", "new/unsaved.txt"]);
    assert_eq!((files[0].lines[0].line, files[0].lines[0].preview.as_str(), files[0].unsaved), (2, "new needle", true));
    assert!(!files[1].unsaved);
}

#[test]
fn a_list_of_files_and_a_folder_are_searched_alone() {
    let f = Fx::new();
    for p in ["a.txt", "b.txt", "sub/c.txt", "sub/deeper/d.txt", "subway.txt"] {
        f.write(p, "needle\n");
    }
    let list = vec!["b.txt".to_string(), "sub/c.txt".to_string(), "gone.txt".to_string()];
    let (files, s) = f.search_in(q("needle"), Scope::Files(list), HashMap::new());
    assert_eq!(paths(&files), ["b.txt", "sub/c.txt"]);
    assert!(s.errors.is_empty(), "a file deleted since it was listed isn't an error: {s:?}");
    let (files, _) = f.search_in(q("needle"), Scope::Files(vec!["a.txt".into(), "sub/c.txt".into()]), HashMap::new());
    assert_eq!(files.len(), 2);
    let (files, _) = f.search_in(Query { exclude: "a.txt".into(), ..q("needle") }, Scope::Files(vec!["a.txt".into(), "b.txt".into()]), HashMap::new());
    assert_eq!(paths(&files), ["b.txt"], "globs apply to a list too");
    let (files, _) = f.search_in(q("needle"), Scope::WorkingTree { folder: Some("sub".into()) }, HashMap::new());
    assert_eq!(paths(&files), ["sub/c.txt", "sub/deeper/d.txt"], "paths still from the root, and subway isn't in sub");
}

#[test]
fn it_stops_at_the_limit_and_says_so() {
    let f = Fx::new();
    for i in 0..50 {
        f.write(&format!("f{i}.txt"), "needle needle\n".repeat(10));
    }
    let (files, s) = f.search(Query { max_matches: 25, ..q("needle") });
    assert!(s.limit_hit);
    assert_eq!(s.matches, 25);
    let kept: usize = files.iter().map(|f| f.matches).sum();
    assert!((25..=26).contains(&kept), "a line's two matches go together: {kept}");
}

#[test]
fn a_search_stops_when_cancelled_and_still_finishes() {
    let f = Fx::new();
    for i in 0..400 {
        f.write(&format!("d{}/f{i}.txt", i % 20), "needle\n".repeat(2000));
    }
    let req = Request { root: f.root.clone(), query: q("needle"), scope: Scope::WorkingTree { folder: None }, overlay: HashMap::new(), git: None };
    let search = textsearch::start(req, || {}).unwrap();
    search.cancel();
    let (files, s) = search.collect();
    assert!(s.cancelled);
    assert!(files.len() < 400, "it stopped before the end: {}", files.len());
}

#[test]
fn dropping_a_search_stops_it() {
    let f = Fx::new();
    for i in 0..200 {
        f.write(&format!("f{i}.txt"), "needle\n".repeat(5000));
    }
    let req = Request { root: f.root.clone(), query: q("needle"), scope: Scope::WorkingTree { folder: None }, overlay: HashMap::new(), git: None };
    let t = std::time::Instant::now();
    drop(textsearch::start(req, || {}).unwrap());
    // Nothing to wait on; what matters is that the threads end, which the next test run's
    // temp folder cleanup would trip over if they didn't. Give them a moment and move on.
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn at_a_revision_the_files_are_as_they_were() {
    let f = Fx::new();
    f.write("a.txt", "the old needle\n").write("b.md", "needle in b\n").write("skip.log", "needle\n");
    f.commit_all("first");
    f.write("a.txt", "rewritten\n");
    let at_head = |query: Query| f.search_in(query, Scope::Revision("HEAD".into()), HashMap::new());
    let (files, s) = at_head(Query { exclude: "*.log".into(), ..q("NEEDLE") });
    assert_eq!(paths(&files), ["a.txt", "b.md"], "{s:?}");
    assert_eq!(hits(&files), ["needle", "needle"]);
    assert_eq!((files[0].lines[0].line, files[0].lines[0].column), (1, 8));
    let (files, _) = at_head(Query { regex: true, whole_word: true, ..q(r"n\w+e") });
    assert_eq!(files.len(), 3);
    assert!(at_head(Query { case_sensitive: true, ..q("NEEDLE") }).0.is_empty());
    // An option dressed as a revision isn't run.
    let (files, s) = f.search_in(q("needle"), Scope::Revision("--output=x".into()), HashMap::new());
    assert!(files.is_empty() && !s.errors.is_empty());
    assert!(!f.root.join("x").exists());
}

/// At a revision, the same searcher reads the files, so everything the working tree has
/// works there too: lines joined by a search, the size limit, binary files, and the regex
/// syntax, which isn't git's.
#[test]
fn a_revision_is_searched_as_the_working_tree_is() {
    let f = Fx::new();
    f.write("a.txt", "first line\nsecond line\n").write("big.txt", format!("{}needle\n", "x".repeat(4000))).write("bin.dat", b"needle\x00\x01\x02");
    f.write("crlf.txt", "ends with needle\r\nnext\r\n");
    f.commit_all("first");
    for p in ["a.txt", "big.txt", "bin.dat", "crlf.txt"] {
        std::fs::remove_file(f.root.join(p)).unwrap();
    }
    let at_head = |query: Query| f.search_in(query, Scope::Revision("HEAD".into()), HashMap::new());
    let (files, s) = at_head(q("line\nsecond"));
    assert_eq!(paths(&files), ["a.txt"], "{s:?}");
    assert_eq!(files[0].lines.iter().map(|l| l.line).collect::<Vec<_>>(), [1, 2]);
    let (files, s) = at_head(Query { max_file_size: 1000, ..q("needle") });
    assert_eq!(paths(&files), ["crlf.txt"], "big is over the limit and bin is binary");
    assert_eq!(s.skipped_large, 1);
    // `$` before a CRLF, and a class git's own regex syntax doesn't have.
    assert_eq!(paths(&at_head(Query { regex: true, ..q(r"needle$") }).0), ["big.txt", "crlf.txt"]);
    assert_eq!(hits(&at_head(Query { regex: true, ..q(r"\p{Lu}*sec\w+") }).0), ["second"]);
}

#[test]
fn a_big_revision_is_read_in_many_batches_and_all_of_it_is_found() {
    let f = Fx::new();
    // More files than one batch takes, a few with matches far apart.
    for i in 0..1200 {
        f.write(&format!("d{}/f{i}.txt", i % 7), if i % 300 == 3 { format!("file {i}\nthe needle\n") } else { format!("file {i}\n") });
    }
    f.commit_all("many");
    let (files, s) = f.search_in(Query { threads: 3, ..q("needle") }, Scope::Revision("HEAD".into()), HashMap::new());
    assert_eq!(files.len(), 4, "{s:?}");
    assert_eq!(s.files_searched, 1200);
    assert!(s.errors.is_empty(), "{s:?}");
    // Plain text has git name the files to read; a regex reads them all. Same answer.
    let rev = |query: Query| f.search_in(query, Scope::Revision("HEAD".into()), HashMap::new());
    for (text, case) in [("NEEDLE", false), ("needle", true), ("file 9", false), ("absent", false)] {
        let plain = rev(Query { case_sensitive: case, ..q(text) });
        let read_all = rev(Query { case_sensitive: case, regex: true, ..q(text) });
        assert_eq!(plain.0, read_all.0, "{text}");
        assert_eq!(plain.1.files_searched, read_all.1.files_searched, "{text}: every file counts as looked at");
    }
    // And with one thread, the same.
    assert_eq!(f.search(Query { threads: 1, ..q("needle") }).0.len(), 4);
}

#[test]
fn notify_is_called_and_results_come_before_the_end() {
    let f = Fx::new();
    for i in 0..30 {
        f.write(&format!("f{i}.txt"), "needle\n");
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let req = Request { root: f.root.clone(), query: q("needle"), scope: Scope::WorkingTree { folder: None }, overlay: HashMap::new(), git: None };
    let (files, _) = textsearch::start(req, move || {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    })
    .unwrap()
    .collect();
    assert_eq!(files.len(), 30);
    // The last call comes just after `Done` is sent, so the results are there when it does.
    let t = std::time::Instant::now();
    while calls.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "at least for the end");
        std::thread::yield_now();
    }
}

/// The obvious way: each line, every non-overlapping place the needle is, left to right.
fn naive(text: &str, needle: &str) -> Vec<(u32, usize)> {
    text.split('\n').enumerate().flat_map(|(i, line)| line.match_indices(needle).map(move |(at, _)| (i as u32 + 1, at)).collect::<Vec<_>>()).collect()
}

fn found(dir: &Path, needle: &str) -> Vec<(u32, usize)> {
    let req = Request { root: dir.to_path_buf(), query: Query { case_sensitive: true, ..q(needle) }, scope: Scope::WorkingTree { folder: None }, overlay: HashMap::new(), git: None };
    let (files, _) = textsearch::start(req, || {}).unwrap().collect();
    files.iter().flat_map(|f| f.lines.iter().flat_map(|l| l.ranges.iter().map(move |r| (l.line, l.column as usize + (r.start as usize) - (l.ranges[0].start as usize))))).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    #[test]
    fn literal_search_finds_what_the_obvious_way_does(text in "[ab \n]{0,300}", needle in "[ab]{1,3}") {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.txt"), &text).unwrap();
        prop_assert_eq!(found(dir.path(), &needle), naive(&text, &needle));
    }
}
