//! Stashing some hunks, against real git in throwaway repositories: what's picked leaves, what
//! isn't stays, and it comes back to exactly the bytes it was. A failure at each step leaves things
//! as they were.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};

use gitem_core::GitExe;
use gitem_core::diff::FileDiff;
use gitem_core::hunkstash::{self, Picked};
use gitem_core::repo::{DiffOpts, DiffSource, Repo};

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

/// Two changes far enough apart to be separate hunks, in a file with repeated lines, where a patch
/// numbered wrongly could land on the wrong copy.
fn numbered() -> String {
    (1..=40).map(|i| format!("same line {}\n", i % 3)).collect()
}

fn with_two_hunks() -> String {
    let mut lines: Vec<String> = numbered().lines().map(String::from).collect();
    lines[4] = "first change".to_string();
    lines[30] = "second change".to_string();
    lines.join("\n") + "\n"
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let git = GitExe::discover(None).unwrap();
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write("a.txt", &numbered());
        f.write("b.txt", "bee\n");
        f.write("gone.txt", "going\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "first"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim_end().to_string()
    }
    fn write(&self, p: &str, s: &str) {
        std::fs::write(self.root.join(p), s).unwrap();
    }
    fn read(&self, p: &str) -> Vec<u8> {
        std::fs::read(self.root.join(p)).unwrap()
    }
    fn repo(&self) -> Repo {
        Repo::open(self.git.clone(), &self.root).unwrap()
    }
    fn unstaged(&self) -> Vec<FileDiff> {
        self.repo().diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap()
    }
    fn picked(&self, picks: &[(&str, Vec<usize>)]) -> Picked {
        let picks: Vec<(String, Vec<usize>)> = picks.iter().map(|(p, h)| (p.to_string(), h.clone())).collect();
        hunkstash::pick(&self.unstaged(), &picks).expect("something to stash")
    }
    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }
    fn run(&self, op: gitem_core::ops::GitOp) -> Result<(), String> {
        op.run(&self.git, Some(&self.root), None, &mut |_| {}).map(drop).map_err(|e| e.to_string())
    }
    fn stash(&self, picks: &[(&str, Vec<usize>)], message: &str) -> Result<(), String> {
        self.run(hunkstash::stash(self.picked(picks), message, &self.head()))
    }
    /// Everything a stash could disturb, as it stands.
    fn state(&self) -> (String, String, Vec<u8>, Vec<u8>) {
        (self.git(&["status", "--porcelain"]), self.git(&["diff", "--cached"]), self.read("a.txt"), self.read("b.txt"))
    }
    fn stashes(&self) -> usize {
        self.git(&["stash", "list"]).lines().count()
    }
}

#[test]
fn one_hunk_of_two_leaves_and_comes_back_to_the_same_bytes() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let before = f.state();
    f.stash(&[("a.txt", vec![1])], "the second").unwrap();

    let now = String::from_utf8(f.read("a.txt")).unwrap();
    assert!(now.contains("first change"), "what wasn't picked stays");
    assert!(!now.contains("second change"), "what was picked leaves");
    assert_eq!(f.git(&["stash", "list"]), "stash@{0}: On main: the second [hunks]");
    let shown = f.git(&["stash", "show", "-p", "stash@{0}"]);
    assert!(shown.contains("+second change") && !shown.contains("first change"), "{shown}");

    // A plain pop refuses here, with the rest of the file still changed, so it isn't what's used.
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert_eq!(f.state(), before);
    assert_eq!(f.stashes(), 0, "a pop leaves no stash");
}

#[test]
fn applying_keeps_the_stash_and_popping_on_a_clean_tree_works_too() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let before = f.state();
    f.stash(&[("a.txt", vec![0, 1])], "both").unwrap();
    assert_eq!(f.read("a.txt"), numbered().into_bytes(), "both hunks left, so the file's as committed");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    f.run(hunkstash::apply("stash@{0}", false)).unwrap();
    assert_eq!(f.state(), before);
    assert_eq!(f.stashes(), 1, "apply keeps it");
    f.git(&["checkout", "--", "a.txt"]);
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert_eq!(f.state(), before);
    assert_eq!(f.stashes(), 0);
}

#[test]
fn it_goes_by_a_merge_when_head_has_moved_and_refuses_rather_than_overwrite() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    f.stash(&[("a.txt", vec![1])], "later").unwrap();
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-q", "-m", "the first change"]);
    // Another file moves on as well, and the patch still fits.
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert!(String::from_utf8(f.read("a.txt")).unwrap().contains("second change"));
    assert_eq!(f.stashes(), 0);

    // A change right on top of a stashed one: it doesn't fit, a merge refuses, and it all stays.
    f.git(&["checkout", "--", "a.txt"]);
    f.write("a.txt", &String::from_utf8(f.read("a.txt")).unwrap().replace("same line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\n", "stashed here\n"));
    let picks = f.picked(&[("a.txt", vec![0])]);
    f.run(hunkstash::stash(picks, "here", &f.head())).unwrap();
    f.write("a.txt", &String::from_utf8(f.read("a.txt")).unwrap().replace("same line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\nsame line 0\nsame line 1\nsame line 2\n", "and something else\n"));
    let before = f.state();
    assert!(f.run(hunkstash::apply("stash@{0}", true)).is_err());
    assert_eq!(f.state(), before, "nothing changed");
    assert_eq!(f.stashes(), 1, "and it's still there");
}

#[test]
fn line_endings_come_back_as_they_were_with_autocrlf() {
    let f = Fx::new();
    f.git(&["config", "core.autocrlf", "true"]);
    let crlf = |s: &str| s.replace('\n', "\r\n");
    f.write("a.txt", &crlf(&numbered()));
    f.git(&["add", "a.txt"]);
    // Git only compares, and the content is the same.
    f.write("a.txt", &crlf(&with_two_hunks()));
    let before = f.state();
    f.stash(&[("a.txt", vec![0])], "crlf").unwrap();
    let now = String::from_utf8(f.read("a.txt")).unwrap();
    assert!(!now.contains("first change") && now.contains("second change\r\n"), "{}", &now[..60.min(now.len())]);
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert_eq!(f.state(), before);
}

#[test]
fn staged_work_elsewhere_stays_staged_and_out_of_the_stash() {
    let f = Fx::new();
    f.write("b.txt", "staged bee\n");
    f.git(&["add", "b.txt"]);
    f.write("a.txt", &with_two_hunks());
    let before = f.state();
    f.stash(&[("a.txt", vec![0])], "just a").unwrap();
    assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "b.txt", "still staged");
    assert_eq!(f.read("b.txt"), b"staged bee\n");
    assert!(!f.git(&["stash", "show", "-p", "stash@{0}"]).contains("bee"), "not in the stash");
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert_eq!(f.state(), before);
}

#[test]
fn a_deleted_file_is_a_hunk_too_and_comes_back() {
    let f = Fx::new();
    std::fs::remove_file(f.root.join("gone.txt")).unwrap();
    f.stash(&[("gone.txt", vec![0])], "keep it").unwrap();
    assert_eq!(f.read("gone.txt"), b"going\n", "the deletion left, so the file's back");
    f.run(hunkstash::apply("stash@{0}", true)).unwrap();
    assert!(!f.root.join("gone.txt").exists());
    assert_eq!(f.git(&["status", "--porcelain"]), " D gone.txt");
}

// ---- failing at each step

/// What each failure below has to leave: the same as before, no stash, and no scratch index.
fn left_as_it_was(f: &Fx, before: &(String, String, Vec<u8>, Vec<u8>)) {
    assert_eq!(&f.state(), before);
    assert_eq!(f.stashes(), 0);
    let git_dir = PathBuf::from(f.git(&["rev-parse", "--absolute-git-dir"]));
    let left: Vec<_> = std::fs::read_dir(git_dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("gitem-hunks-")).collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn head_having_moved_stops_it_first() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let picked = f.picked(&[("a.txt", vec![0])]);
    let then = f.head();
    f.git(&["commit", "-q", "--allow-empty", "-m", "moved"]);
    let before = f.state();
    let e = f.run(hunkstash::stash(picked, "x", &then)).unwrap_err();
    assert!(e.contains("HEAD has moved"), "{e}");
    left_as_it_was(&f, &before);
}

#[test]
fn a_file_staged_since_stops_it() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let picked = f.picked(&[("a.txt", vec![0])]);
    f.git(&["add", "a.txt"]);
    let before = f.state();
    let e = f.run(hunkstash::stash(picked, "x", &f.head())).unwrap_err();
    assert!(e.contains("staged"), "{e}");
    left_as_it_was(&f, &before);
}

#[test]
fn a_file_changed_since_so_the_hunk_doesnt_fit_stops_it_before_the_stash() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let picked = f.picked(&[("a.txt", vec![0])]);
    f.write("a.txt", &with_two_hunks().replace("first change", "edited in the editor since"));
    let before = f.state();
    let e = f.run(hunkstash::stash(picked, "x", &f.head())).unwrap_err();
    assert!(e.contains("don't match"), "{e}");
    left_as_it_was(&f, &before);
}

#[test]
fn a_patch_git_cant_read_stops_it_in_the_scratch_index() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    let mut picked = f.picked(&[("a.txt", vec![0])]);
    picked.forward = "not a patch at all\n".to_string();
    let before = f.state();
    assert!(f.run(hunkstash::stash(picked, "x", &f.head())).is_err());
    left_as_it_was(&f, &before);
}

/// The one that matters: git really does stop partway through the files, having already removed
/// the first to rewrite it. Each file goes back, and the stash goes.
#[test]
fn stopping_partway_through_the_files_puts_them_all_back_and_keeps_no_stash() {
    let f = Fx::new();
    f.write("a.txt", &with_two_hunks());
    f.write("b.txt", "changed bee\n");
    let before = f.state();
    let picked = f.picked(&[("a.txt", vec![0]), ("b.txt", vec![0])]);
    let head = f.head();
    let locked = hold_against_deletion(&f.root.join("b.txt"));
    let result = f.run(hunkstash::stash(picked, "x", &head));
    drop(locked);
    let e = result.unwrap_err();
    assert!(e.contains("as they were"), "{e}");
    left_as_it_was(&f, &before);
}

/// Reading stays possible, as that's how it's told the file needs nothing.
#[cfg(windows)]
fn hold_against_deletion(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).unwrap()
}

/// A folder that can't be changed stops the same removal. Here that stops the first file as well,
/// so on these systems this shows the failure's path and not a real partway.
#[cfg(unix)]
fn hold_against_deletion(path: &Path) -> impl Drop {
    use std::os::unix::fs::PermissionsExt;
    struct Back(PathBuf);
    impl Drop for Back {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let dir = path.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    Back(dir)
}
