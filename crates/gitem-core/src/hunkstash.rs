//! Stashing some hunks of the unstaged changes, and bringing them back.
//!
//! Each step that can fail comes before anything the user could lose. The stash's commits are built
//! in a scratch index, from `HEAD` and the chosen hunks, without touching the real index or the
//! files. It's stored before the hunks leave the files, so they're always somewhere. If taking them
//! out stops partway, each file goes back to the bytes it had, and the stash goes away again.
//!
//! A file with staged changes isn't offered: the stash would have to hold them too, or its hunks
//! wouldn't apply to `HEAD`. Git's own `stash --patch` leaves files that way, and popping one
//! refuses while the rest of the file is still changed. This pops by patch, which doesn't mind.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::diff::{self, FileDiff, PatchMode};
use crate::ops::{Custom, GitOp};
use crate::runner::{GitExe, GitOutput, RunOpts};
use crate::{Error, Result};

/// Ends a stash's message, so that Apply and Pop know to go by patch. It's a plain part of the
/// message, and without it they do what they do for any stash.
pub const MARK: &str = "[hunks]";

pub fn is_hunk_stash(message: &str) -> bool {
    message.trim_end().ends_with(MARK)
}

/// Why this file's hunks can't be picked, if they can't.
pub fn why_not(f: &FileDiff, staged: &[String]) -> Option<&'static str> {
    let path = f.display_path();
    if staged.iter().any(|s| s == path || *s == f.old_path) {
        return Some("It has staged changes too. Unstage them first, or stash what's staged.");
    }
    if f.is_binary || f.hunks.is_empty() {
        return Some("It has no hunks: stash the whole file instead.");
    }
    if f.is_new || f.is_rename {
        return Some("It's new here, so there's nothing for a hunk to apply to. Stash the whole file instead.");
    }
    if f.header.iter().any(|l| l.contains("160000") || l.contains("120000")) {
        return Some("It's a submodule or a link: stash the whole file instead.");
    }
    if !f.hunks.iter().all(diff::hunk_is_exact) {
        return Some("It isn't UTF-8, so its hunks can't be written back exactly. Stash the whole file instead.");
    }
    if f.header.first().is_some_and(|l| l.starts_with("diff --cc ")) {
        return Some("It has a conflict to resolve first.");
    }
    None
}

/// These hunks of this file as one patch: its header once, then each hunk in order.
fn file_patch(f: &FileDiff, hunks: &[usize], mode: PatchMode) -> Option<String> {
    let mut out = String::new();
    for &h in hunks {
        let one = diff::build_patch(f, h, None, mode)?;
        if out.is_empty() {
            out = one;
        } else {
            let at = one.find("\n@@ ")? + 1;
            out.push_str(&one[at..]);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// What's been picked, ready to stash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picked {
    /// To build the stash from `HEAD`.
    pub forward: String,
    /// To take them out of the files.
    pub reverse: String,
    pub paths: Vec<String>,
    pub hunks: usize,
}

/// `picks` holds a file's path and the indices of its hunks, in order. `None` if that comes to nothing.
pub fn pick(files: &[FileDiff], picks: &[(String, Vec<usize>)]) -> Option<Picked> {
    let mut p = Picked::default();
    for (path, hunks) in picks.iter().filter(|(_, h)| !h.is_empty()) {
        let f = files.iter().find(|f| f.display_path() == path)?;
        p.forward.push_str(&file_patch(f, hunks, PatchMode::Forward)?);
        p.reverse.push_str(&file_patch(f, hunks, PatchMode::Reverse)?);
        p.paths.push(path.clone());
        p.hunks += hunks.len();
    }
    (p.hunks > 0).then_some(p)
}

fn write_opts(cancel: &Option<Arc<AtomicBool>>, stdin: Option<&str>) -> RunOpts {
    RunOpts { stdin: stdin.map(|s| s.as_bytes().to_vec()), is_write: true, cancel: cancel.clone(), ..Default::default() }
}

fn git_strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// A scratch index, gone with this.
struct ScratchIndex(std::path::PathBuf);

impl ScratchIndex {
    fn new(git_dir: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(git_dir.join(format!("gitem-hunks-{}-{}.index", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))))
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let mut lock = self.0.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

/// A file's bytes as they were, or that it wasn't there.
struct Kept {
    path: std::path::PathBuf,
    bytes: Option<Vec<u8>>,
    permissions: Option<std::fs::Permissions>,
}

fn keep(root: &Path, paths: &[String]) -> Result<Vec<Kept>> {
    paths
        .iter()
        .map(|p| {
            let path = root.join(p);
            match std::fs::read(&path) {
                Ok(bytes) => Ok(Kept { permissions: std::fs::metadata(&path).ok().map(|m| m.permissions()), path, bytes: Some(bytes) }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Kept { path, bytes: None, permissions: None }),
                Err(e) => Err(Error::Other(format!("Could not read {p}, so nothing was done: {e}"))),
            }
        })
        .collect()
}

/// Whichever files aren't as they were get what they had. Says which couldn't.
fn put_back(kept: &[Kept]) -> Vec<String> {
    let mut failed = Vec::new();
    for k in kept {
        let now = std::fs::read(&k.path).ok();
        if now == k.bytes {
            continue;
        }
        let done = match &k.bytes {
            Some(b) => k.path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&k.path, b)).and_then(|()| match &k.permissions {
                Some(p) => std::fs::set_permissions(&k.path, p.clone()),
                None => Ok(()),
            }),
            None => std::fs::remove_file(&k.path),
        };
        if let Err(e) = done {
            failed.push(format!("{}: {e}", k.path.display()));
        }
    }
    failed
}

fn run_stash(git: &GitExe, cwd: &Path, picked: &Picked, message: &str, cancel: Option<Arc<AtomicBool>>, on_line: &mut dyn FnMut(&str)) -> Result<Vec<GitOutput>> {
    let read = |args: &[&str]| -> Result<String> { Ok(git.run(Some(cwd), args)?.text().trim().to_string()) };
    let head = read(&["rev-parse", "--verify", "HEAD^{commit}"])?;
    if !read(&["ls-files", "--unmerged"])?.is_empty() {
        return Err(Error::Git("There are conflicts to resolve first, so nothing was done.".into()));
    }
    let mut staged = git_strings(&["diff", "--cached", "--name-only", "--"]);
    staged.extend(picked.paths.iter().map(|p| format!(":(literal){p}")));
    let staged_now = git.run_opts(Some(cwd), &staged, &RunOpts::default(), &mut |_| {})?.into_result()?.text();
    if !staged_now.trim().is_empty() {
        return Err(Error::Git(format!("These have staged changes now, so nothing was done:\n{}", staged_now.trim())));
    }
    let branch = read(&["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    let place = if branch.is_empty() { "(no branch)".to_string() } else { branch };
    let line = read(&["log", "-1", "--format=%h %s", &head])?;
    let said = if message.trim().is_empty() { format!("WIP on {place}: {line} {MARK}") } else { format!("On {place}: {} {MARK}", message.trim()) };

    // Nothing but new objects, which are harmless if anything below stops this.
    let git_dir = read(&["rev-parse", "--absolute-git-dir"])?;
    let index = ScratchIndex::new(Path::new(&git_dir));
    let scratch = |args: &[&str], stdin: Option<&str>| -> Result<String> {
        let opts = RunOpts { stdin: stdin.map(|s| s.as_bytes().to_vec()), env: vec![("GIT_INDEX_FILE".to_string(), index.0.to_string_lossy().into_owned())], no_hooks: true, cancel: cancel.clone(), ..Default::default() };
        Ok(git.run_opts(Some(cwd), &git_strings(args), &opts, &mut |_| {})?.into_result()?.text().trim().to_string())
    };
    scratch(&["read-tree", &head], None)?;
    scratch(&["apply", "--cached", "--recount", "--whitespace=nowarn", "-"], Some(&picked.forward))?;
    let tree = scratch(&["write-tree"], None)?;
    let base_tree = read(&["rev-parse", &format!("{head}^{{tree}}")])?;
    let index_commit = read(&["commit-tree", &base_tree, "-p", &head, "-m", &format!("index on {place}: {line}")])?;
    let work_commit = read(&["commit-tree", &tree, "-p", &head, "-p", &index_commit, "-m", &said])?;

    let take_out = ["apply", "--reverse", "--recount", "--whitespace=nowarn", "-"];
    let check: Vec<String> = git_strings(&["apply", "--check", "--reverse", "--recount", "--whitespace=nowarn", "-"]);
    git.run_opts(Some(cwd), &check, &RunOpts { stdin: Some(picked.reverse.clone().into_bytes()), ..Default::default() }, &mut |_| {})?.into_result().map_err(|e| Error::Git(format!("These hunks don't match the files any more, so nothing was done. Look again and retry.\n{e}")))?;
    let kept = keep(cwd, &picked.paths)?;

    let stored = git_strings(&["stash", "store", "-m", &said, &work_commit]);
    let first = git.run_opts(Some(cwd), &stored, &write_opts(&cancel, None), on_line)?.into_result()?;

    // From here the hunks are in the stash. Stop now and they mustn't be in only half the files.
    let failed = match git.run_opts(Some(cwd), &git_strings(&take_out), &write_opts(&cancel, Some(&picked.reverse)), on_line) {
        Ok(o) => match o.into_result() {
            Ok(o) => return Ok(vec![first, o]),
            Err(e) => e.to_string(),
        },
        Err(e) => e.to_string(),
    };
    let not_back = put_back(&kept);
    if !not_back.is_empty() {
        return Err(Error::Git(format!("Taking the hunks out of the files failed:\n{failed}\n\nThese couldn't be put back as they were:\n{}\n\nThe hunks are safe in stash@{{0}}.", not_back.join("\n"))));
    }
    // Only ours, and it stays if that can't be told.
    if read(&["rev-parse", "--verify", "-q", "stash@{0}"]).ok().as_deref() == Some(work_commit.as_str()) {
        let _ = git.run_opts(Some(cwd), &git_strings(&["stash", "drop", "stash@{0}"]), &RunOpts { is_write: true, ..Default::default() }, &mut |_| {});
    }
    Err(Error::Git(format!("Taking the hunks out of the files failed, so they're as they were, and nothing was stashed.\n{failed}")))
}

/// Stash what's been picked, with `HEAD` where it was when they were.
pub fn stash(picked: Picked, message: &str, head: &str) -> GitOp {
    let message = message.to_string();
    let title = if picked.hunks == 1 { "Stash 1 hunk".to_string() } else { format!("Stash {} hunks", picked.hunks) };
    let mut op = GitOp::new(title).lossless().expect_rev("HEAD", head, "HEAD has moved since these were picked, so nothing was done. Look again and retry.");
    op.custom = Some(Custom::new(move |git, cwd, cancel, on_line| {
        let cwd = cwd.ok_or_else(|| Error::Other("Stashing hunks needs a repository.".into()))?;
        run_stash(git, cwd, &picked, &message, cancel, on_line)
    }));
    op
}

fn run_apply(git: &GitExe, cwd: &Path, sel: &str, pop: bool, cancel: Option<Arc<AtomicBool>>, on_line: &mut dyn FnMut(&str)) -> Result<Vec<GitOutput>> {
    let read = |args: &[&str]| -> Result<String> { Ok(git.run(Some(cwd), args)?.text().trim().to_string()) };
    let work = read(&["rev-parse", "--verify", &format!("{sel}^{{commit}}")])?;
    let base = read(&["rev-parse", "--verify", &format!("{work}^1")])?;
    // Whatever the user's config says about diffs, this is for `git apply` to read.
    let patch = git.run(Some(cwd), &["diff", "--no-color", "--no-ext-diff", "--binary", "--full-index", "--src-prefix=a/", "--dst-prefix=b/", &base, &work])?.text();
    let check = git_strings(&["apply", "--check", "--whitespace=nowarn", "-"]);
    let fits = git.run_opts(Some(cwd), &check, &RunOpts { stdin: Some(patch.clone().into_bytes()), ..Default::default() }, &mut |_| {})?.ok();
    let applied = if fits {
        git.run_opts(Some(cwd), &git_strings(&["apply", "--whitespace=nowarn", "-"]), &write_opts(&cancel, Some(&patch)), on_line)?.into_result()?
    } else {
        // `HEAD` has moved on, say. A merge can take that, and refuses if it would overwrite anything.
        git.run_opts(Some(cwd), &git_strings(&["stash", "apply", sel]), &write_opts(&cancel, None), on_line)?.into_result()?
    };
    if pop {
        // Applying leaves the list as it was, so this is still the one.
        git.run_opts(Some(cwd), &git_strings(&["stash", "drop", sel]), &write_opts(&cancel, None), on_line)?.into_result()?;
    }
    Ok(vec![applied])
}

/// Apply or Pop for a stash of hunks: the rest of a file can still be changed, which a merge refuses.
pub fn apply(sel: &str, pop: bool) -> GitOp {
    let sel = sel.to_string();
    let mut op = GitOp::new(format!("{} {sel}", if pop { "Pop" } else { "Apply" }));
    op.custom = Some(Custom::new(move |git, cwd, cancel, on_line| {
        let cwd = cwd.ok_or_else(|| Error::Other("Applying a stash needs a repository.".into()))?;
        run_apply(git, cwd, &sel, pop, cancel, on_line)
    }));
    op
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_mark_is_what_tells_a_stash_of_hunks() {
        assert!(is_hunk_stash("On main: tidy [hunks]"));
        assert!(!is_hunk_stash("On main: tidy"));
        assert!(!is_hunk_stash("[hunks] came first"));
    }

    #[test]
    fn a_file_only_offers_hunks_when_they_can_be_put_back_exactly() {
        let good = diff::parse_unified("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n").remove(0);
        assert_eq!(why_not(&good, &[]), None);
        assert!(why_not(&good, &["a".to_string()]).is_some_and(|w| w.contains("staged")));
        let new = diff::parse_unified("diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+y\n").remove(0);
        assert!(why_not(&new, &[]).is_some_and(|w| w.contains("new here")));
        let link = diff::parse_unified("diff --git a/l b/l\nindex 111..222 120000\n--- a/l\n+++ b/l\n@@ -1 +1 @@\n-x\n+y\n").remove(0);
        assert!(why_not(&link, &[]).is_some_and(|w| w.contains("link")));
        let not_utf8 = diff::parse_unified("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+\u{FFFD}\n").remove(0);
        assert!(why_not(&not_utf8, &[]).is_some_and(|w| w.contains("UTF-8")));
    }

    #[test]
    fn hunks_of_one_file_share_a_header_and_each_way_has_its_own_numbers() {
        let f = diff::parse_unified("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n x\n+a\n@@ -10 +11,2 @@\n y\n+b\n@@ -20 +22,2 @@\n z\n+c\n").remove(0);
        let p = pick(&[f], &[("a".to_string(), vec![0, 2])]).unwrap();
        assert_eq!(p.hunks, 2);
        assert_eq!(p.paths, ["a"]);
        assert_eq!(p.forward.matches("diff --git").count(), 1, "{}", p.forward);
        assert!(p.forward.contains("@@ -20,1 +20,2 @@"), "{}", p.forward);
        assert!(p.reverse.contains("@@ -22,1 +22,2 @@"), "{}", p.reverse);
        assert!(!p.forward.contains("+b"), "{}", p.forward);
        assert_eq!(pick(&[], &[("a".to_string(), vec![])]), None);
    }
}
