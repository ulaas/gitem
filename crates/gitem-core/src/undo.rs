//! Taking back the last operation. An op says what it's about to change with an [`UndoPlan`],
//! what that is gets read before it runs, and if it worked an [`UndoEntry`] says how to put it
//! back. Nothing is kept on disk by this: a snapshot is an unreferenced git object, which lasts
//! until git's own garbage collection, and the entry itself lasts until the next operation.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ops::GitOp;
use crate::runner::{GitExe, RunOpts};

/// How much of the working tree a discard will copy into git first. Past this the discard goes
/// ahead as asked, without an undo.
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoPlan {
    /// It moves HEAD: merge, rebase, pull, reset, cherry-pick, revert. `how` is the `reset`
    /// flag that puts it back. With `snapshot`, uncommitted changes go too and are copied first.
    MoveHead { label: String, how: &'static str, snapshot: bool },
    /// These local branches go away.
    DeleteBranches(Vec<String>),
    /// This stash goes away.
    DropStash(String),
    /// All the stashes go away.
    ClearStashes,
    /// These paths in the working tree get overwritten or deleted.
    Discard { label: String, paths: Vec<String> },
}

/// What was read before the op ran.
#[derive(Debug, Clone)]
pub enum Captured {
    Head { label: String, how: &'static str, branch: String, sha: String, snapshot: Option<String> },
    Branches(Vec<(String, String, String)>),
    Stashes(Vec<(String, String)>),
    Files { label: String, paths: Vec<String>, tree: String },
}

/// The way back from the last op.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// `Undo reset`.
    pub label: String,
    /// What it does, for a tooltip and the question before it runs.
    pub detail: String,
    pub op: GitOp,
    check: Check,
}

/// What has to still be true for the undo to do what it says. Read when it's asked for, since
/// anything can have happened since.
#[derive(Debug, Clone)]
enum Check {
    None,
    /// HEAD is on this branch, at this commit.
    Head { branch: String, sha: String },
    /// These files are as the op left them.
    Files { paths: Vec<String>, tree: String },
}

fn text(git: &GitExe, cwd: &Path, args: &[&str]) -> Option<String> {
    git.run_raw(Some(cwd), args).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
}

/// `refs/heads/x`, or empty on a detached HEAD.
fn head_branch(git: &GitExe, cwd: &Path) -> String {
    text(git, cwd, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default()
}

fn head_sha(git: &GitExe, cwd: &Path) -> Option<String> {
    text(git, cwd, &["rev-parse", "--verify", "--quiet", "HEAD"])
}

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    // A checkout of its own isn't part of what's being copied. Git adds it as a pointer.
    std::fs::read_dir(path).map_or(0, |d| d.flatten().filter(|e| e.file_name() != ".git").map(|e| size_of(&e.path())).sum())
}

/// A tree holding what `paths` hold on disk right now, whether git tracks them or not. Written
/// through an index of its own, so the real one isn't touched. Paths that aren't there are left
/// out.
fn snapshot_files(git: &GitExe, cwd: &Path, paths: &[String]) -> crate::Result<String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let index = std::env::temp_dir().join(format!("gitem-undo-{}-{}.index", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let env = vec![("GIT_INDEX_FILE".to_string(), index.to_string_lossy().into_owned())];
    let result = (|| {
        let present: Vec<&String> = paths.iter().filter(|p| std::fs::symlink_metadata(cwd.join(p)).is_ok()).collect();
        if !present.is_empty() {
            let names: Vec<u8> = present.iter().flat_map(|p| [":(literal)".as_bytes(), p.as_bytes(), b"\0"].concat()).collect();
            let args: Vec<String> = ["add", "-f", "--pathspec-from-file=-", "--pathspec-file-nul"].map(String::from).to_vec();
            // Run as a write, so that filters such as LFS's apply as they would to the file.
            let opts = RunOpts { stdin: Some(names), env: env.clone(), is_write: true, no_hooks: true, ..Default::default() };
            git.run_opts(Some(cwd), &args, &opts, &mut |_| {})?.into_result()?;
        }
        let args = vec!["write-tree".to_string()];
        let opts = RunOpts { env: env.clone(), is_write: true, no_hooks: true, ..Default::default() };
        Ok(git.run_opts(Some(cwd), &args, &opts, &mut |_| {})?.into_result()?.text().trim().to_string())
    })();
    let _ = std::fs::remove_file(&index);
    result
}

impl UndoPlan {
    /// Reads what the op is about to change. `Err` says why there will be no undo: the op can
    /// still run, and it's up to the caller to mention it.
    pub fn capture(&self, git: &GitExe, cwd: &Path) -> Result<Captured, String> {
        match self {
            UndoPlan::MoveHead { label, how, snapshot } => {
                let sha = head_sha(git, cwd).ok_or("There's no commit to go back to.")?;
                // Empty when nothing's changed, which is the usual case.
                let copy = if *snapshot { text(git, cwd, &["stash", "create"]).filter(|s| !s.is_empty()) } else { None };
                Ok(Captured::Head { label: label.clone(), how, branch: head_branch(git, cwd), sha, snapshot: copy })
            }
            UndoPlan::DeleteBranches(names) => {
                let mut found = Vec::new();
                for n in names {
                    let full = format!("refs/heads/{}", n.strip_prefix("refs/heads/").unwrap_or(n));
                    let Some(sha) = text(git, cwd, &["rev-parse", "--verify", "--quiet", &full]) else { continue };
                    let upstream = text(git, cwd, &["for-each-ref", "--format=%(upstream:short)", &full]).unwrap_or_default();
                    found.push((n.clone(), sha, upstream));
                }
                Ok(Captured::Branches(found))
            }
            UndoPlan::DropStash(sel) => {
                let sha = text(git, cwd, &["rev-parse", "--verify", "--quiet", sel]).ok_or("The stash wasn't found.")?;
                let all = stashes(git, cwd);
                let message = all.iter().find(|(s, _)| *s == sha).map(|(_, m)| m.clone()).unwrap_or_default();
                Ok(Captured::Stashes(vec![(sha, message)]))
            }
            UndoPlan::ClearStashes => Ok(Captured::Stashes(stashes(git, cwd))),
            UndoPlan::Discard { label, paths } => {
                let size: u64 = paths.iter().map(|p| size_of(&cwd.join(p))).sum();
                if size > MAX_SNAPSHOT_BYTES {
                    return Err(format!("{} too large to keep a copy of, so this one can't be undone.", crate::util::count_of(paths.len(), "file")));
                }
                let tree = snapshot_files(git, cwd, paths).map_err(|e| format!("A copy to undo from couldn't be made, so this one can't be undone: {e}"))?;
                Ok(Captured::Files { label: label.clone(), paths: paths.clone(), tree })
            }
        }
    }
}

/// Newest first, as listed. `stash store` puts the newest on top, so restoring goes in reverse.
fn stashes(git: &GitExe, cwd: &Path) -> Vec<(String, String)> {
    text(git, cwd, &["stash", "list", "--format=%H%x00%gs"]).unwrap_or_default().lines().filter_map(|l| l.split_once('\0')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

impl Captured {
    /// The way back, now that the op has worked. None if it changed nothing.
    pub fn into_entry(self, git: &GitExe, cwd: &Path) -> Option<UndoEntry> {
        match self {
            Captured::Head { label, how, branch, sha, snapshot } => {
                let now = head_sha(git, cwd)?;
                if now == sha && snapshot.is_none() {
                    return None;
                }
                let short = crate::util::prefix(&sha, 7).to_string();
                let mut op = GitOp::new(label.clone()).step(&["reset", how, &sha, "--"]).expect_rev("HEAD", &now, "HEAD has moved since then, so nothing was done. The reflog still has it.");
                let mut detail = format!("Put {} back at {short}.", branch_words(&branch));
                if let Some(copy) = &snapshot {
                    // `--index` needs what was staged to still apply, which it may not.
                    op = op.step(&["stash", "apply", "--index", copy]).or_else(vec!["stash".into(), "apply".into(), copy.clone()], "Your changes are back, but no longer staged.");
                    detail.push_str(" Your uncommitted changes come back too.");
                }
                // Undoing an undo is a redo, and needs no more than this.
                if label.starts_with("Undo ") {
                    op = op.undoable(UndoPlan::MoveHead { label: format!("Redo {}", &label["Undo ".len()..]), how, snapshot: false });
                }
                Some(UndoEntry { label, detail, op: op.lossless(), check: Check::Head { branch, sha: now } })
            }
            Captured::Branches(found) if found.is_empty() => None,
            Captured::Branches(found) => {
                let names: Vec<&str> = found.iter().map(|(n, _, _)| n.as_str()).collect();
                let what = if found.len() == 1 { format!("branch {}", names[0]) } else { format!("{} branches", found.len()) };
                let mut op = GitOp::new(format!("Restore {what}"));
                for (name, sha, upstream) in &found {
                    let short = name.strip_prefix("refs/heads/").unwrap_or(name);
                    op = op.step(&["branch", "--", short, sha]);
                    if !upstream.is_empty() {
                        op = op.step(&["branch", &format!("--set-upstream-to={upstream}"), "--", short]).allow_fail();
                    }
                }
                Some(UndoEntry { label: format!("Undo delete of {what}"), detail: format!("Create {} again, where {} left off.", names.join(", "), if found.len() == 1 { "it" } else { "they" }), op: op.lossless(), check: Check::None })
            }
            Captured::Stashes(all) if all.is_empty() => None,
            Captured::Stashes(all) => {
                let what = if all.len() == 1 { "1 stash".to_string() } else { format!("{} stashes", all.len()) };
                let mut op = GitOp::new(format!("Restore {what}"));
                for (sha, message) in all.iter().rev() {
                    op = op.step(&["stash", "store", "-m", message, sha]);
                }
                Some(UndoEntry { label: format!("Undo drop of {what}"), detail: "Put what was dropped back on the stash list.".to_string(), op: op.lossless(), check: Check::None })
            }
            Captured::Files { label, paths, tree } => {
                let after = snapshot_files(git, cwd, &paths).ok()?;
                if after == tree {
                    return None;
                }
                // Only what the copy holds. Naming anything else would stop `restore` altogether.
                let held: Vec<String> = paths.iter().filter(|p| git.run_raw(Some(cwd), &["cat-file", "-e", &format!("{tree}:{p}")]).is_ok_and(|o| o.ok())).cloned().collect();
                if held.is_empty() {
                    return None;
                }
                let op = crate::ops::restore_worktree_from(&format!("Restore {}", crate::util::count_of(held.len(), "file")), &tree, &held);
                Some(UndoEntry { label, detail: format!("Bring back {} as {} before.", crate::util::count_of(held.len(), "file"), if held.len() == 1 { "it was" } else { "they were" }), op: op.lossless(), check: Check::Files { paths, tree: after } })
            }
        }
    }
}

fn branch_words(branch: &str) -> String {
    match branch.strip_prefix("refs/heads/") {
        Some(b) => b.to_string(),
        None => "HEAD".to_string(),
    }
}

impl UndoEntry {
    /// Why this can't be done any more, if so. Reads the repository, so not for the UI thread.
    pub fn refusal(&self, git: &GitExe, cwd: &Path) -> Option<String> {
        match &self.check {
            Check::None => None,
            Check::Head { branch, sha } => {
                if head_branch(git, cwd) != *branch {
                    return Some(format!("You've moved off {} since then, so nothing was done. The reflog still has it.", branch_words(branch)));
                }
                (head_sha(git, cwd).as_deref() != Some(sha)).then(|| "HEAD has moved since then, so nothing was done. The reflog still has it.".to_string())
            }
            Check::Files { paths, tree } => match snapshot_files(git, cwd, paths) {
                Ok(now) if now == *tree => None,
                Ok(_) => Some("Those files have changed since then, and bringing back the old copy would overwrite that. Nothing was done.".to_string()),
                Err(e) => Some(format!("Couldn't check those files, so nothing was done: {e}")),
            },
        }
    }
}
