//! Write operations described as data. A [`GitOp`] is a titled list of git
//! invocations; the UI runs it on a worker thread, streams progress, logs it,
//! and refreshes afterwards. This keeps every mutation in one auditable place.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::Result;
use crate::runner::{GitExe, GitOutput, RunOpts};

#[derive(Debug, Clone, Default)]
pub struct Step {
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    /// A failure of this step does not abort the op.
    pub allow_fail: bool,
    /// Runs only if the step before it failed: the fallback to a gentler first try.
    pub only_after_failure: bool,
    /// Skipped if this revision is already at this commit.
    pub unless_at: Option<(String, String)>,
}

/// Checked when the op runs, not when it was offered: a menu or dialog can stay open
/// while a terminal, or another tool, moves things.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    pub rev: String,
    pub sha: String,
    /// What to tell the user when `rev` has moved. Nothing has been done at that point.
    pub why: String,
}

#[derive(Debug, Clone, Default)]
pub struct GitOp {
    pub title: String,
    pub steps: Vec<Step>,
    /// Talks to a remote (shown with a progress bar, can be cancelled).
    pub network: bool,
    pub guards: Vec<Guard>,
    /// Said when a step marked `only_after_failure` was needed and worked.
    pub fallback_note: Option<String>,
    /// A revision or name that would be read as an option. The op refuses to run.
    pub bad_arg: Option<String>,
    /// Why this can't be done, said instead of running anything.
    pub refused: Option<String>,
    /// Nobody asked for this just now, as with a scheduled fetch, so it runs no hooks.
    pub background: bool,
    /// This changes the repository's config by the user's own hand in gitem.
    pub own_config: bool,
    /// Scratch files this op needs only while it runs.
    pub remove_after: Vec<std::path::PathBuf>,
    /// Nothing is lost by saying yes to this, so a question about it needn't look alarming.
    pub lossless: bool,
}

impl GitOp {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), ..Default::default() }
    }
    pub fn lossless(mut self) -> Self {
        self.lossless = true;
        self
    }
    pub fn step<S: AsRef<str>>(mut self, args: &[S]) -> Self {
        self.steps.push(Step { args: args.iter().map(|s| s.as_ref().to_string()).collect(), ..Default::default() });
        self
    }
    pub fn step_v(mut self, args: Vec<String>) -> Self {
        self.steps.push(Step { args, ..Default::default() });
        self
    }
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        if let Some(s) = self.steps.last_mut() {
            s.stdin = Some(data.into());
        }
        self
    }
    pub fn env(mut self, k: &str, v: impl Into<String>) -> Self {
        if let Some(s) = self.steps.last_mut() {
            s.env.push((k.to_string(), v.into()));
        }
        self
    }
    pub fn allow_fail(mut self) -> Self {
        if let Some(s) = self.steps.last_mut() {
            s.allow_fail = true;
        }
        self
    }
    /// Refuse to run at all unless `rev` is at `sha` by then.
    pub fn expect_rev(mut self, rev: &str, sha: &str, why: &str) -> Self {
        self.guards.push(Guard { rev: rev.to_string(), sha: sha.to_string(), why: why.to_string() });
        self
    }
    /// Skip the last step if `rev` is already at `sha`.
    pub fn unless_at(mut self, rev: &str, sha: &str) -> Self {
        if let Some(s) = self.steps.last_mut() {
            s.unless_at = Some((rev.to_string(), sha.to_string()));
        }
        self
    }
    /// A failure of the last step isn't final: try `args` instead, and say so if it works.
    pub fn or_else(mut self, args: Vec<String>, note: &str) -> Self {
        if let Some(s) = self.steps.last_mut() {
            s.allow_fail = true;
        }
        self.steps.push(Step { args, only_after_failure: true, ..Default::default() });
        self.fallback_note = Some(note.to_string());
        self
    }
    /// Treat paths literally, so `pages/[id].tsx` can't match as a glob. Every op
    /// acting on exact paths uses this; several of them discard data. Marked name by name
    /// rather than with `GIT_LITERAL_PATHSPECS`, which every hook the command runs would
    /// inherit: a `post-index-change` or `pre-commit` that runs git with a glob of its own
    /// would find it taken literally.
    pub fn literal(mut self) -> Self {
        for s in &mut self.steps {
            if let Some(at) = s.args.iter().position(|a| a == "--") {
                for a in &mut s.args[at + 1..] {
                    a.insert_str(0, LITERAL);
                }
            }
            if s.args.iter().any(|a| a == "--pathspec-from-file=-")
                && let Some(names) = &s.stdin
            {
                s.stdin = Some(names.split(|b| *b == 0).filter(|n| !n.is_empty()).flat_map(|n| [LITERAL.as_bytes(), n, b"\0"].concat()).collect());
            }
        }
        self
    }
    pub fn net(mut self) -> Self {
        self.network = true;
        self
    }
    pub fn background(mut self) -> Self {
        self.background = true;
        self
    }
    /// Nobody is there to answer. Without this, a sign-in that's needed pops up a window every
    /// few minutes for a fetch nobody asked for: failing is the right thing for it to do.
    pub fn unattended(mut self) -> Self {
        for s in &mut self.steps {
            // Empty and not unset: git would go on to `SSH_ASKPASS`, which is also gitem's window.
            s.env.extend([("GIT_ASKPASS", ""), ("SSH_ASKPASS_REQUIRE", "never"), ("GCM_INTERACTIVE", "never")].map(|(k, v)| (k.to_string(), v.to_string())));
        }
        self
    }
    fn own_config(mut self) -> Self {
        self.own_config = true;
        self
    }
    /// These go on the command line where git looks for options. One starting with a dash
    /// stops the op: a tag called `--hard` must not turn a reset into a hard one.
    fn names(mut self, args: &[&str]) -> Self {
        if let Some(bad) = args.iter().find(|a| a.starts_with('-')) {
            self.bad_arg.get_or_insert_with(|| bad.to_string());
        }
        self
    }

    /// Plain-text summary of the commands, shown in confirmation dialogs.
    pub fn preview(&self) -> String {
        let shown = |a: &String| a.strip_prefix(LITERAL).unwrap_or(a).to_string();
        let step = |s: &Step| {
            // Past a certain length names go over stdin. What matters to whoever's asked is the names.
            let from_stdin = s.args.iter().any(|a| a == "--pathspec-from-file=-");
            let args = s.args.iter().filter(|a| !from_stdin || (*a != "--pathspec-from-file=-" && *a != "--pathspec-file-nul")).map(shown).collect::<Vec<_>>().join(" ");
            let names: Vec<String> = s.stdin.iter().filter(|_| from_stdin).flat_map(|b| b.split(|c| *c == 0)).filter(|n| !n.is_empty()).map(|n| { let n = String::from_utf8_lossy(n); n.strip_prefix(LITERAL).unwrap_or(&n).to_string() }).collect();
            const SHOWN: usize = 5;
            let more = names.len().saturating_sub(SHOWN);
            let tail = names.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(" ");
            match (names.is_empty(), more) {
                (true, _) => format!("git {args}"),
                (false, 0) => format!("git {args} -- {tail}"),
                (false, n) => format!("git {args} -- {tail} and {n} more"),
            }
        };
        crate::util::mask_url_credentials(&self.steps.iter().map(step).collect::<Vec<_>>().join("\n"))
    }

    /// One step, waiting out another program's lock: editors and agents run git beside
    /// this all the time, and their lock is usually gone in a moment. A command that fails
    /// on a lock hasn't started changing anything, so it's safe to run again.
    fn run_step(&self, git: &GitExe, cwd: Option<&Path>, s: &Step, cancel: &Option<Arc<AtomicBool>>, on_line: &mut dyn FnMut(&str)) -> Result<GitOutput> {
        let opts = RunOpts { stdin: s.stdin.clone(), env: s.env.clone(), is_write: true, cancel: cancel.clone(), no_hooks: self.background };
        let started = std::time::Instant::now();
        loop {
            let o = git.run_opts(cwd, &s.args, &opts, on_line)?;
            let stopped = cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
            if o.ok() || stopped || !crate::runner::is_lock_failure(&o.stderr) || started.elapsed() >= LOCK_PATIENCE {
                return Ok(o);
            }
            on_line("Waiting for another git process to finish…");
            std::thread::sleep(LOCK_RETRY_EVERY);
        }
    }

    /// Run all steps in order, stopping on the first hard failure.
    pub fn run(&self, git: &GitExe, cwd: Option<&Path>, cancel: Option<Arc<AtomicBool>>, on_line: &mut dyn FnMut(&str)) -> Result<Vec<GitOutput>> {
        let result = self.run_steps(git, cwd, cancel, on_line);
        for f in &self.remove_after {
            let _ = std::fs::remove_file(f);
        }
        result
    }

    fn run_steps(&self, git: &GitExe, cwd: Option<&Path>, cancel: Option<Arc<AtomicBool>>, on_line: &mut dyn FnMut(&str)) -> Result<Vec<GitOutput>> {
        if let Some(why) = &self.refused {
            return Err(crate::Error::Git(why.clone()));
        }
        if let Some(bad) = &self.bad_arg {
            return Err(crate::Error::Git(format!("\"{bad}\" can't be used here: it starts with a dash, so git would read it as an option. Nothing was done.")));
        }
        for g in &self.guards {
            if !rev_is_at(git, cwd, &g.rev, &g.sha) {
                return Err(crate::Error::Git(g.why.clone()));
            }
        }
        let mut out = Vec::new();
        let mut last_ok = true;
        for s in &self.steps {
            if s.only_after_failure && last_ok {
                continue;
            }
            if let Some((rev, sha)) = &s.unless_at
                && rev_is_at(git, cwd, rev, sha)
            {
                continue;
            }
            let o = self.run_step(git, cwd, s, &cancel, on_line)?;
            last_ok = o.ok();
            if !o.ok() && !s.allow_fail {
                o.into_result()?;
                unreachable!("non-zero exit is an error");
            }
            out.push(o);
        }
        Ok(out)
    }

    /// What to say beyond "done", given what `run` returned.
    pub fn outcome_note(&self, outputs: &[GitOutput]) -> Option<String> {
        self.fallback_note.clone().filter(|_| outputs.len() == self.steps.len()).or_else(|| {
            // `[main 7e6d960] subject` is what a commit prints.
            let is_commit = self.steps.last().and_then(|s| s.args.first()).is_some_and(|a| a == "commit");
            outputs.last().filter(|_| is_commit).and_then(|o| commit_note(&o.text()))
        })
    }
}

/// `7e6d960 on main`, from what `git commit` prints first.
pub fn commit_note(printed: &str) -> Option<String> {
    let inside = printed.lines().next()?.strip_prefix('[')?.split_once(']')?.0;
    let mut words: Vec<&str> = inside.split_whitespace().collect();
    let sha = words.pop()?;
    let branch = words.first()?;
    Some(format!("{sha} on {branch}"))
}

/// How long a write waits for another process's lock before giving up.
const LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(4);
const LOCK_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

fn rev_is_at(git: &GitExe, cwd: Option<&Path>, rev: &str, sha: &str) -> bool {
    git.run_raw(cwd, &["rev-parse", "--verify", "--quiet", rev]).is_ok_and(|o| o.ok() && o.text().trim() == sha)
}

fn v<S: AsRef<str>>(a: &[S]) -> Vec<String> {
    a.iter().map(|s| s.as_ref().to_string()).collect()
}

/// The pathspec magic that makes a name mean just that name.
pub const LITERAL: &str = ":(literal)";

/// How many bytes of names go on a command line. Windows allows 32,767 characters for the
/// whole of it, and git's own wrapper and options take some of that.
const MAX_NAMES_ON_COMMAND_LINE: usize = 20_000;

fn too_long_for_command_line(paths: &[String]) -> bool {
    paths.iter().map(|p| p.len() + LITERAL.len() + 3).sum::<usize>() > MAX_NAMES_ON_COMMAND_LINE
}

fn nul_separated(paths: &[String]) -> Vec<u8> {
    let mut names = Vec::new();
    for p in paths {
        names.extend_from_slice(p.as_bytes());
        names.push(0);
    }
    names
}

/// One step of `base` over `paths`, with the names shown in its command line where they fit,
/// and given over stdin where they don't.
fn step_on_paths(op: GitOp, base: Vec<String>, paths: &[String]) -> GitOp {
    let mut a = base;
    if too_long_for_command_line(paths) {
        a.extend(v(&["--pathspec-from-file=-", "--pathspec-file-nul"]));
        return op.step_v(a).stdin(nul_separated(paths));
    }
    a.push("--".into());
    a.extend_from_slice(paths);
    op.step_v(a)
}

// ---- staging -----------------------------------------------------------------

pub fn stage_paths(paths: &[String]) -> GitOp {
    step_on_paths(GitOp::new(format!("Stage {}", crate::util::count_of(paths.len(), "file"))), v(&["add"]), paths).literal()
}

pub fn stage_all() -> GitOp {
    GitOp::new("Stage all changes").step(&["add", "-A"])
}

pub fn unstage_paths(paths: &[String], has_head: bool) -> GitOp {
    let base = if has_head { v(&["restore", "--staged"]) } else { v(&["rm", "--cached", "-r", "--quiet"]) };
    step_on_paths(GitOp::new(format!("Unstage {}", crate::util::count_of(paths.len(), "file"))), base, paths).literal()
}

pub fn unstage_all(has_head: bool) -> GitOp {
    if has_head {
        GitOp::new("Unstage all").step(&["reset", "--quiet", "HEAD"])
    } else {
        GitOp::new("Unstage all").step(&["rm", "--cached", "-r", "--quiet", "."])
    }
}

/// Unstage named files while a merge, cherry-pick or rebase is going. `reset` would end
/// the operation, and `restore --staged -- .` would turn its conflicts into plain edits,
/// so `paths` must leave out conflicted files. The list goes over stdin, since a large
/// merge can hold more names than fit on a command line.
pub fn unstage_during_op(paths: &[String]) -> GitOp {
    GitOp::new(format!("Unstage {}", crate::util::count_of(paths.len(), "file"))).step(&["restore", "--staged", "--pathspec-from-file=-", "--pathspec-file-nul"]).stdin(nul_separated(paths)).literal()
}

/// Discard worktree changes of tracked files.
pub fn discard_paths(paths: &[String]) -> GitOp {
    step_on_paths(GitOp::new(format!("Discard changes in {}", crate::util::count_of(paths.len(), "file"))), v(&["restore", "--worktree"]), paths).literal()
}

/// Delete untracked paths through `git clean`, so nothing else can be affected.
pub fn delete_untracked(paths: &[String]) -> GitOp {
    let mut op = GitOp::new(format!("Delete {}", crate::util::count_of(paths.len(), "untracked file")));
    // `clean` is the one of these that can't read its names from stdin.
    let mut batch: Vec<String> = Vec::new();
    for p in paths {
        batch.push(p.clone());
        if too_long_for_command_line(&batch) {
            let last = batch.pop().unwrap_or_default();
            op = step_on_paths(op, v(&["clean", "-f", "-d"]), &batch);
            batch = vec![last];
        }
    }
    if !batch.is_empty() {
        op = step_on_paths(op, v(&["clean", "-f", "-d"]), &batch);
    }
    op.literal()
}

/// Apply a patch produced by `diff::build_patch` to the index or worktree.
pub fn apply_patch(title: &str, patch: String, cached: bool, reverse: bool) -> GitOp {
    let mut a = v(&["apply", "--recount", "--whitespace=nowarn"]);
    if cached {
        a.push("--cached".into());
    }
    if reverse {
        a.push("--reverse".into());
    }
    a.push("-".into());
    GitOp::new(title).step_v(a).stdin(patch)
}

const THREE_WAY_NOTE: &str = "It didn't apply as it stood, so a three-way merge was used, and its changes are staged.";

/// With `three_way`, only as a fallback: `--3way` from the start refuses files with
/// unrelated local edits, and stages what it applies. Plain `git apply` is all or nothing,
/// so a failed first try leaves nothing for the second to trip over.
pub fn apply_patch_file(path: &str, three_way: bool, index: bool) -> GitOp {
    let mut a = v(&["apply", "--whitespace=nowarn"]);
    if index && !three_way {
        a.push("--index".into());
    }
    a.push(path.to_string());
    let op = GitOp::new("Apply patch").step_v(a);
    if three_way { op.or_else(v(&["apply", "--whitespace=nowarn", "--3way", path]), THREE_WAY_NOTE) } else { op }
}

pub fn am_patch_file(path: &str) -> GitOp {
    GitOp::new("Apply mailbox patch").step(&["am", "--3way", path])
}

// ---- commit ------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct CommitOpts {
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    pub sign: bool,
    pub no_verify: bool,
    pub allow_empty: bool,
    pub author: Option<String>,
    pub all: bool,
}

pub fn commit(o: &CommitOpts) -> GitOp {
    // Not `strip`: that would delete message lines starting with `#`, as in "#123".
    let mut a = v(&["commit", "--cleanup=whitespace", "-F", "-"]);
    if o.amend {
        a.push("--amend".into());
    }
    if o.signoff {
        a.push("--signoff".into());
    }
    if o.sign {
        a.push("--gpg-sign".into());
    }
    if o.no_verify {
        a.push("--no-verify".into());
    }
    if o.allow_empty {
        a.push("--allow-empty".into());
    }
    if o.all {
        a.push("--all".into());
    }
    if let Some(x) = &o.author {
        a.push(format!("--author={x}"));
    }
    GitOp::new(if o.amend { "Amend commit" } else { "Commit" }).step_v(a).stdin(o.message.clone().into_bytes())
}

/// A series of commits, each of exactly its own files. `commit --only` leaves anything
/// else staged as it is, so nothing needs unstaging first. `add` comes before it because
/// `--only` needs paths git knows, and a new file isn't one yet. The names go over stdin,
/// since a large plan can hold more than fit on a command line. All one op, so nothing
/// queued can land in the middle, and a failure stops whatever is left.
///
/// A staged rename's old name has to be in the commit too, or the commit adds the new file
/// and leaves the old one behind. It goes only to `commit`: it's gone from the disk, so
/// `add` has nothing to find.
pub fn compose_commits(plan: &[ComposedCommit]) -> GitOp {
    let mut op = GitOp::new(format!("Create {}", crate::util::count_of(plan.len(), "commit")));
    for c in plan {
        let mut all = c.files.clone();
        all.extend(c.renamed_from.iter().cloned());
        op = op.step(&["add", "--pathspec-from-file=-", "--pathspec-file-nul"]).stdin(nul_separated(&c.files));
        op = op.step(&["commit", "--cleanup=whitespace", "-m", &c.message, "--only", "--pathspec-from-file=-", "--pathspec-file-nul"]).stdin(nul_separated(&all));
    }
    op.literal()
}

/// One commit of a plan for [`compose_commits`].
#[derive(Debug, Clone, Default)]
pub struct ComposedCommit {
    pub message: String,
    pub files: Vec<String>,
    /// The old names of any staged renames among `files`.
    pub renamed_from: Vec<String>,
}

pub fn fixup_commit(sha: &str) -> GitOp {
    GitOp::new(format!("Fixup {}", short(sha))).names(&[sha]).step(&["commit", "--fixup", sha])
}

fn short(s: &str) -> &str {
    crate::util::prefix(s, 8)
}

// ---- branches ----------------------------------------------------------------

pub fn checkout(target: &str) -> GitOp {
    // A name a server delivered, shown in full, which `switch` won't take as a branch.
    if let Some(short) = target.strip_prefix("refs/heads/") {
        return GitOp::new(format!("Switch to {short}")).step(&["switch", "--end-of-options", short]);
    }
    GitOp::new(format!("Switch to {target}")).names(&[target]).step(&["switch", target])
}

pub fn checkout_detached(rev: &str) -> GitOp {
    GitOp::new(format!("Checkout {} (detached)", short(rev))).names(&[rev]).step(&["switch", "--detach", rev])
}

/// Track a remote branch with a new local branch of the short name.
pub fn checkout_remote(remote_branch: &str, remotes: &[String]) -> GitOp {
    let local = crate::parse::split_remote_ref(remote_branch, remotes).1;
    GitOp::new(format!("Check out {remote_branch}")).names(&[local, remote_branch]).step(&["switch", "-c", local, "--track", remote_branch])
}

pub fn branch_create(name: &str, start: Option<&str>, switch: bool) -> GitOp {
    let mut a = if switch { v(&["switch", "-c", name]) } else { v(&["branch", name]) };
    if let Some(s) = start {
        a.push(s.to_string());
    }
    GitOp::new(format!("Create branch {name}")).names(&[name, start.unwrap_or_default()]).step_v(a)
}

/// A new branch of the user's own, whatever it starts from. Git would otherwise set a remote
/// branch as its upstream, which makes Push refuse, and counts ahead and behind against it.
pub fn branch_start_work(name: &str, start: Option<&str>) -> GitOp {
    let mut a = v(&["switch", "--no-track", "-c", name]);
    if let Some(s) = start {
        a.push(s.to_string());
    }
    GitOp::new(format!("Create branch {name}")).names(&[name, start.unwrap_or_default()]).step_v(a)
}

pub fn branch_rename(old: &str, new: &str) -> GitOp {
    GitOp::new(format!("Rename {old} to {new}")).names(&[old, new]).step(&["branch", "-m", old, new])
}

pub fn branch_delete(name: &str, force: bool) -> GitOp {
    let flag = if force { "-D" } else { "-d" };
    // A name a server delivered, shown in full. Getting rid of it has to work.
    if let Some(short) = name.strip_prefix("refs/heads/") {
        return GitOp::new(format!("Delete branch {name}")).step(&["branch", flag, "--", short]);
    }
    GitOp::new(format!("Delete branch {name}")).names(&[name]).step(&["branch", flag, name])
}

pub fn branch_delete_remote(remote_branch: &str, remotes: &[String]) -> GitOp {
    let (remote, b) = crate::parse::split_remote_ref(remote_branch, remotes);
    GitOp::new(format!("Delete remote branch {remote_branch}")).step(&["push", "--delete", "--", remote, b]).net()
}

pub fn branch_set_upstream(branch: &str, upstream: &str) -> GitOp {
    GitOp::new(format!("Set upstream of {branch}")).names(&[branch]).step(&["branch", &format!("--set-upstream-to={upstream}"), branch])
}

pub fn branch_unset_upstream(branch: &str) -> GitOp {
    GitOp::new(format!("Unset upstream of {branch}")).names(&[branch]).step(&["branch", "--unset-upstream", branch])
}

/// Move a branch that is not checked out to another commit.
pub fn branch_force_move(name: &str, target: &str) -> GitOp {
    GitOp::new(format!("Move {name} to {}", short(target))).names(&[name, target]).step(&["branch", "-f", name, target])
}

// ---- merge / rebase / pick / revert / reset ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeMode {
    #[default]
    Default,
    NoFf,
    FfOnly,
    Squash,
    NoCommit,
}

pub fn merge(rev: &str, mode: MergeMode) -> GitOp {
    let mut a = v(&["merge", "--no-edit"]);
    match mode {
        MergeMode::Default => {}
        MergeMode::NoFf => a.push("--no-ff".into()),
        MergeMode::FfOnly => a.push("--ff-only".into()),
        MergeMode::Squash => a.push("--squash".into()),
        MergeMode::NoCommit => a.push("--no-commit".into()),
    }
    a.push(rev.to_string());
    GitOp::new(format!("Merge {rev}")).names(&[rev]).step_v(a)
}

#[derive(Debug, Clone, Default)]
pub struct RebaseOpts {
    pub onto: String,
    pub autostash: bool,
    pub autosquash: bool,
    pub rebase_merges: bool,
    pub update_refs: bool,
    pub ignore_date: bool,
}

fn rebase_flags(o: &RebaseOpts, a: &mut Vec<String>) {
    if o.autostash {
        a.push("--autostash".into());
    }
    if o.rebase_merges {
        a.push("--rebase-merges".into());
    }
    if o.update_refs {
        a.push("--update-refs".into());
    }
    if o.ignore_date {
        a.push("--ignore-date".into());
    }
}

pub fn rebase(o: &RebaseOpts) -> GitOp {
    let mut a = v(&["rebase"]);
    rebase_flags(o, &mut a);
    a.push(o.onto.clone());
    GitOp::new(format!("Rebase onto {}", o.onto)).names(&[&o.onto]).step_v(a)
}

/// Interactive rebase with a todo list already decided in the UI: our own
/// binary, as the sequence editor, copies `todo_file` over git's todo.
pub fn rebase_interactive(o: &RebaseOpts, root: bool, todo_file: &Path, helper: &Path) -> GitOp {
    let mut a = v(&["rebase", "-i"]);
    rebase_flags(o, &mut a);
    if o.autosquash {
        a.push("--autosquash".into());
    }
    if root {
        a.push("--root".into());
    } else {
        a.push(o.onto.clone());
    }
    let helper = helper.to_string_lossy().replace('\\', "/");
    let todo = todo_file.to_string_lossy().replace('\\', "/");
    GitOp::new("Interactive rebase").names(&[if root { "" } else { &o.onto }]).step_v(a).env("GIT_SEQUENCE_EDITOR", format!("\"{helper}\" --apply-todo \"{todo}\""))
}

pub fn op_continue(cmd: &str) -> GitOp {
    if cmd == "merge" {
        // `merge --continue` is `commit` with the prepared message.
        return GitOp::new("Continue merge").step(&["commit", "--no-edit"]);
    }
    let op = GitOp::new(format!("Continue {cmd}")).step(&[cmd, "--continue"]);
    // Picks and reverts keep their message. A rebase may need the editor for
    // reword steps still ahead, so it keeps the normal editor window.
    if matches!(cmd, "cherry-pick" | "revert") { op.env("GIT_EDITOR", "true") } else { op }
}

pub fn op_abort(cmd: &str) -> GitOp {
    GitOp::new(format!("Abort {cmd}")).step(&[cmd, "--abort"])
}

pub fn op_skip(cmd: &str) -> GitOp {
    GitOp::new(format!("Skip in {cmd}")).step(&[cmd, "--skip"])
}

pub fn cherry_pick(shas: &[String], no_commit: bool, x_trailer: bool) -> GitOp {
    let mut a = v(&["cherry-pick"]);
    if no_commit {
        a.push("--no-commit".into());
    }
    if x_trailer {
        a.push("-x".into());
    }
    a.extend_from_slice(shas);
    let names: Vec<&str> = shas.iter().map(String::as_str).collect();
    GitOp::new(format!("Cherry-pick {}", crate::util::count_of(shas.len(), "commit"))).names(&names).step_v(a)
}

pub fn revert(sha: &str, no_commit: bool, mainline: Option<u32>) -> GitOp {
    let mut a = v(&["revert", "--no-edit"]);
    if no_commit {
        a.push("--no-commit".into());
    }
    if let Some(m) = mainline {
        a.push("-m".into());
        a.push(m.to_string());
    }
    a.push(sha.to_string());
    GitOp::new(format!("Revert {}", short(sha))).names(&[sha]).step_v(a)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
    Keep,
}

pub fn reset(rev: &str, mode: ResetMode) -> GitOp {
    let flag = match mode {
        ResetMode::Soft => "--soft",
        ResetMode::Mixed => "--mixed",
        ResetMode::Hard => "--hard",
        ResetMode::Keep => "--keep",
    };
    GitOp::new(format!("Reset {flag} to {}", short(rev))).names(&[rev]).step(&["reset", flag, rev, "--"])
}

pub fn restore_file_from(rev: &str, path: &str) -> GitOp {
    GitOp::new(format!("Restore {path} from {}", short(rev))).names(&[rev]).step(&["restore", "--source", rev, "--staged", "--worktree", "--", path]).literal()
}

/// Resolve an unmerged path by taking one side outright.
pub fn checkout_conflict_side(path: &str, theirs: bool) -> GitOp {
    GitOp::new(format!("Take {} for {path}", if theirs { "theirs" } else { "ours" }))
        .step(&["checkout", if theirs { "--theirs" } else { "--ours" }, "--", path])
        .step(&["add", "--", path])
        .literal()
}

pub fn mark_resolved(path: &str) -> GitOp {
    GitOp::new(format!("Mark {path} resolved")).step(&["add", "--", path]).literal()
}

pub fn remove_conflicted(path: &str) -> GitOp {
    GitOp::new(format!("Delete {path}")).step(&["rm", "--quiet", "--", path]).literal()
}

// ---- tags --------------------------------------------------------------------

pub fn tag_create(name: &str, rev: Option<&str>, message: Option<&str>, sign: bool) -> GitOp {
    let mut a = v(&["tag"]);
    if sign {
        a.push("-s".into());
    } else if message.is_some() {
        a.push("-a".into());
    }
    if let Some(m) = message {
        a.push("-m".into());
        a.push(m.to_string());
    } else if sign {
        a.push("-m".into());
        a.push(name.to_string());
    }
    a.push(name.to_string());
    if let Some(r) = rev {
        a.push(r.to_string());
    }
    GitOp::new(format!("Create tag {name}")).names(&[name, rev.unwrap_or_default()]).step_v(a)
}

pub fn tag_delete(name: &str) -> GitOp {
    // A name a server delivered, shown in full. `tag -d` won't take it, and getting rid
    // of it has to work. This is what `tag -d` does underneath.
    if name.starts_with("refs/tags/") {
        return GitOp::new(format!("Delete tag {name}")).step(&["update-ref", "-d", name]);
    }
    GitOp::new(format!("Delete tag {name}")).names(&[name]).step(&["tag", "-d", name])
}

pub fn tag_delete_remote(remote: &str, name: &str) -> GitOp {
    GitOp::new(format!("Delete tag {name} on {remote}")).step(&["push", "--delete", "--", remote, &tag_ref(name)]).net()
}

pub fn tag_push(remote: &str, name: &str) -> GitOp {
    GitOp::new(format!("Push tag {name}")).step(&["push", "--", remote, &tag_ref(name)]).net()
}

fn tag_ref(name: &str) -> String {
    if name.starts_with("refs/tags/") { name.to_string() } else { format!("refs/tags/{name}") }
}

// ---- stash -------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct StashOpts {
    pub message: String,
    pub include_untracked: bool,
    pub keep_index: bool,
    pub staged_only: bool,
    pub paths: Vec<String>,
}

pub fn stash_push(o: &StashOpts) -> GitOp {
    let mut a = v(&["stash", "push"]);
    if !o.message.is_empty() {
        a.push("-m".into());
        a.push(o.message.clone());
    }
    if o.include_untracked {
        a.push("--include-untracked".into());
    }
    if o.keep_index {
        a.push("--keep-index".into());
    }
    if o.staged_only {
        a.push("--staged".into());
    }
    let mut op = GitOp::new("Stash changes");
    if o.paths.is_empty() {
        return op.step_v(a);
    }
    // `stash` runs git again with all the names on its command line, whichever way it was
    // given them. Too many, and it fails after saving the stash and before taking the
    // changes out of the tree, so they'd be in both places.
    if too_long_for_command_line(&o.paths) {
        op.refused = Some(format!("That's too many files to stash by name ({}). Nothing was done. Stage them and stash what's staged, or stash everything.", o.paths.len()));
    }
    step_on_paths(op, a, &o.paths).literal()
}

pub fn stash_apply(sel: &str, pop: bool, index: bool) -> GitOp {
    let mut a = v(&["stash", if pop { "pop" } else { "apply" }]);
    if index {
        a.push("--index".into());
    }
    a.push(sel.to_string());
    GitOp::new(format!("{} {sel}", if pop { "Pop" } else { "Apply" })).names(&[sel]).step_v(a)
}

pub fn stash_drop(sel: &str) -> GitOp {
    GitOp::new(format!("Drop {sel}")).names(&[sel]).step(&["stash", "drop", sel])
}

/// `stash@{N}` means whatever is Nth by the time it runs, and anything pushing a stash,
/// such as a terminal, moves them all along. This makes an op refuse if `sel` isn't `sha`.
pub fn only_if_stash_is(op: GitOp, sel: &str, sha: &str) -> GitOp {
    op.expect_rev(sel, sha, "The stash list changed since this was offered, so nothing was done. Look again and retry.")
}

pub fn stash_clear() -> GitOp {
    GitOp::new("Clear all stashes").step(&["stash", "clear"])
}

pub fn stash_branch(name: &str, sel: &str) -> GitOp {
    GitOp::new(format!("Branch {name} from {sel}")).names(&[name, sel]).step(&["stash", "branch", name, sel])
}

/// Rename by re-storing the same stash commit under a new message. Stored first where that
/// can be done, so a failure leaves two entries at worst, never none: the new one goes on
/// top and the old one moves down by one.
///
/// The newest stash can't go that way, because storing the commit the ref already points
/// at adds no entry. It has to be dropped first, so a ref holds on to it meanwhile. If the
/// steps are cut short, that ref keeps the stash in reach and in the graph.
pub fn stash_rename(sel: &str, sha: &str, message: &str) -> GitOp {
    let op = only_if_stash_is(GitOp::new(format!("Rename {sel}")), sel, sha);
    match stash_index(sel) {
        Some(0) => op
            .step(&["update-ref", STASH_RENAME_REF, sha])
            .step(&["stash", "drop", sel])
            .step(&["stash", "store", "-m", message, sha])
            .step(&["update-ref", "-d", STASH_RENAME_REF]),
        Some(n) => op.step(&["stash", "store", "-m", message, sha]).step(&["stash", "drop", &format!("stash@{{{}}}", n + 1)]),
        None => op.step(&["stash", "store", "-m", message, sha]).step(&["stash", "drop", sel]),
    }
}

/// Holds a stash while it's being renamed.
pub const STASH_RENAME_REF: &str = "refs/gitem/stash-being-renamed";

fn stash_index(sel: &str) -> Option<usize> {
    sel.strip_prefix("stash@{")?.strip_suffix('}')?.parse().ok()
}

// ---- worktrees ---------------------------------------------------------------

pub fn worktree_add(path: &str, branch: Option<&str>, new_branch: Option<&str>, start: Option<&str>, detach: bool) -> GitOp {
    let mut a = v(&["worktree", "add"]);
    if detach {
        a.push("--detach".into());
    }
    if let Some(n) = new_branch {
        a.push("-b".into());
        a.push(n.to_string());
    }
    a.push(path.to_string());
    if let Some(b) = branch {
        a.push(b.to_string());
    } else if let Some(s) = start {
        a.push(s.to_string());
    }
    GitOp::new(format!("Add worktree {path}")).names(&[path, branch.unwrap_or_default(), new_branch.unwrap_or_default(), start.unwrap_or_default()]).step_v(a)
}

/// A new worktree on a new branch, with no upstream, as for [`branch_start_work`].
pub fn worktree_start_work(path: &str, name: &str, start: Option<&str>) -> GitOp {
    let mut a = v(&["worktree", "add", "--no-track", "-b", name, path]);
    if let Some(s) = start {
        a.push(s.to_string());
    }
    GitOp::new(format!("Add worktree {path}")).names(&[path, name, start.unwrap_or_default()]).step_v(a)
}

/// Git asks for force twice to remove a locked worktree.
pub fn worktree_remove_locked(path: &str) -> GitOp {
    GitOp::new(format!("Remove locked worktree {path}")).step(&["worktree", "remove", "--force", "--force", path])
}

pub fn worktree_remove(path: &str, force: bool) -> GitOp {
    let mut a = v(&["worktree", "remove"]);
    if force {
        a.push("--force".into());
    }
    a.push(path.to_string());
    GitOp::new(format!("Remove worktree {path}")).step_v(a)
}

pub fn worktree_move(path: &str, to: &str) -> GitOp {
    GitOp::new(format!("Move worktree to {to}")).step(&["worktree", "move", path, to])
}

pub fn worktree_lock(path: &str, reason: Option<&str>) -> GitOp {
    let mut a = v(&["worktree", "lock"]);
    if let Some(r) = reason.filter(|r| !r.is_empty()) {
        a.push("--reason".into());
        a.push(r.to_string());
    }
    a.push(path.to_string());
    GitOp::new("Lock worktree").step_v(a)
}

pub fn worktree_unlock(path: &str) -> GitOp {
    GitOp::new("Unlock worktree").step(&["worktree", "unlock", path])
}

pub fn worktree_prune() -> GitOp {
    GitOp::new("Prune worktrees").step(&["worktree", "prune"])
}

pub fn worktree_repair() -> GitOp {
    GitOp::new("Repair worktrees").step(&["worktree", "repair"])
}

// ---- remotes -----------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct FetchOpts {
    pub remote: Option<String>,
    pub all: bool,
    pub prune: bool,
    pub tags: bool,
}

/// Saves what a read of the working tree would otherwise work out again each time. Reads
/// can't, since they don't take the index lock.
pub fn refresh_index() -> GitOp {
    GitOp::new("Refresh index").step(&["update-index", "-q", "--refresh"]).allow_fail()
}

pub fn fetch(o: &FetchOpts) -> GitOp {
    let mut a = v(&["fetch", "--progress"]);
    if o.all {
        a.push("--all".into());
    }
    if o.prune {
        a.push("--prune".into());
    }
    if o.tags {
        a.push("--tags".into());
    }
    if let Some(r) = &o.remote {
        a.push("--".into());
        a.push(r.clone());
    }
    GitOp::new(match &o.remote {
        Some(r) => format!("Fetch {r}"),
        None => "Fetch".to_string(),
    })
    .step_v(a)
    .net()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullMode {
    #[default]
    Default,
    FfOnly,
    Rebase,
    Merge,
}

pub fn pull(mode: PullMode, autostash: bool) -> GitOp {
    let mut a = v(&["pull", "--progress"]);
    match mode {
        PullMode::Default => {}
        PullMode::FfOnly => a.push("--ff-only".into()),
        PullMode::Rebase => a.push("--rebase".into()),
        PullMode::Merge => a.push("--no-rebase".into()),
    }
    if autostash {
        a.push("--autostash".into());
    }
    GitOp::new("Pull").step_v(a).net()
}

#[derive(Debug, Clone, Default)]
pub struct PushOpts {
    pub remote: Option<String>,
    pub refspec: Option<String>,
    pub set_upstream: bool,
    pub force_with_lease: bool,
    pub force: bool,
    pub tags: bool,
    pub no_verify: bool,
}

pub fn push(o: &PushOpts) -> GitOp {
    let mut a = v(&["push", "--progress"]);
    if o.set_upstream {
        a.push("--set-upstream".into());
    }
    if o.force_with_lease {
        // A bare lease only compares with the remote-tracking ref, which any fetch moves,
        // so after one it would overwrite work nobody here has seen. This also needs the
        // remote's tip to have been part of this branch at some point.
        a.push("--force-with-lease".into());
        a.push("--force-if-includes".into());
    }
    if o.force {
        a.push("--force".into());
    }
    if o.tags {
        a.push("--follow-tags".into());
    }
    if o.no_verify {
        a.push("--no-verify".into());
    }
    if let Some(r) = &o.remote {
        a.push("--".into());
        a.push(r.clone());
        if let Some(s) = &o.refspec {
            a.push(s.clone());
        }
    }
    GitOp::new(if o.force_with_lease || o.force { "Force push" } else { "Push" }).step_v(a).net()
}

pub fn remote_add(name: &str, url: &str) -> GitOp {
    GitOp::new(format!("Add remote {name}")).step(&["remote", "add", "--", name, url])
}

pub fn remote_remove(name: &str) -> GitOp {
    GitOp::new(format!("Remove remote {name}")).step(&["remote", "remove", name])
}

pub fn remote_rename(old: &str, new: &str) -> GitOp {
    GitOp::new(format!("Rename remote {old}")).step(&["remote", "rename", old, new])
}

pub fn remote_set_url(name: &str, url: &str) -> GitOp {
    GitOp::new(format!("Set URL of {name}")).step(&["remote", "set-url", "--", name, url])
}

pub fn remote_prune(name: &str) -> GitOp {
    GitOp::new(format!("Prune {name}")).step(&["remote", "prune", name]).net()
}

/// Fetch a pull/merge request head into a local branch.
pub fn fetch_pr(remote: &str, ref_spec: &str, local: &str) -> GitOp {
    GitOp::new(format!("Fetch into {local}")).step(&["fetch", "--", remote, &format!("{ref_spec}:{local}")]).net()
}

pub fn clone_repo(url: &str, dest: &str, depth: Option<u32>, recursive: bool, branch: Option<&str>) -> GitOp {
    let mut a = v(&["clone", "--progress"]);
    if let Some(d) = depth {
        a.push(format!("--depth={d}"));
    }
    if recursive {
        a.push("--recurse-submodules".into());
    }
    if let Some(b) = branch.filter(|b| !b.is_empty()) {
        a.push("--branch".into());
        a.push(b.to_string());
    }
    a.push("--".into());
    a.push(url.to_string());
    a.push(dest.to_string());
    GitOp::new(format!("Clone {}", crate::util::mask_url_credentials(url))).step_v(a).net()
}

pub fn init_repo(path: &str, initial_branch: Option<&str>) -> GitOp {
    let mut a = v(&["init"]);
    if let Some(b) = initial_branch.filter(|b| !b.is_empty()) {
        a.push(format!("--initial-branch={b}"));
    }
    a.push(path.to_string());
    GitOp::new(format!("Initialize {path}")).step_v(a)
}

// ---- submodules --------------------------------------------------------------

pub fn submodule_add(url: &str, path: &str) -> GitOp {
    GitOp::new(format!("Add submodule {path}")).step(&["submodule", "add", "--", url, path]).net()
}

pub fn submodule_update(path: Option<&str>, init: bool, recursive: bool, remote: bool) -> GitOp {
    let mut a = v(&["submodule", "update", "--progress"]);
    if init {
        a.push("--init".into());
    }
    if recursive {
        a.push("--recursive".into());
    }
    if remote {
        a.push("--remote".into());
    }
    if let Some(p) = path {
        a.push("--".into());
        a.push(p.to_string());
    }
    GitOp::new("Update submodules").step_v(a).net()
}

pub fn submodule_sync() -> GitOp {
    GitOp::new("Sync submodules").step(&["submodule", "sync", "--recursive"])
}

pub fn submodule_deinit(path: &str, force: bool) -> GitOp {
    let mut a = v(&["submodule", "deinit"]);
    if force {
        a.push("--force".into());
    }
    a.push("--".into());
    a.push(path.to_string());
    GitOp::new(format!("Deinit {path}")).step_v(a)
}

pub fn submodule_remove(path: &str) -> GitOp {
    GitOp::new(format!("Remove submodule {path}"))
        .step(&["submodule", "deinit", "--force", "--", path])
        .allow_fail()
        .step(&["rm", "--force", "--", path])
}

// ---- bisect ------------------------------------------------------------------

pub fn bisect_start(bad: &str, good: &str) -> GitOp {
    GitOp::new("Start bisect").names(&[bad, good]).step(&["bisect", "start", bad, good])
}

pub fn bisect_mark(verdict: &str) -> GitOp {
    GitOp::new(format!("Bisect: {verdict}")).step(&["bisect", verdict])
}

pub fn bisect_mark_commit(verdict: &str, sha: &str) -> GitOp {
    GitOp::new(format!("Bisect: {verdict}")).names(&[sha]).step(&["bisect", verdict, sha])
}

pub fn bisect_reset() -> GitOp {
    GitOp::new("End bisect").step(&["bisect", "reset"])
}

pub fn bisect_run(cmd: &str) -> GitOp {
    GitOp::new(format!("Bisect run: {cmd}")).step(&["bisect", "run", "sh", "-c", cmd])
}

// ---- notes, maintenance, misc -------------------------------------------------

pub fn note_set(sha: &str, text: &str) -> GitOp {
    GitOp::new(format!("Set note on {}", short(sha))).names(&[sha]).step(&["notes", "add", "-f", "-F", "-", sha]).stdin(text.as_bytes().to_vec())
}

pub fn note_remove(sha: &str) -> GitOp {
    GitOp::new(format!("Remove note on {}", short(sha))).names(&[sha]).step(&["notes", "remove", sha])
}

pub fn clean(include_dirs: bool, include_ignored: bool) -> GitOp {
    let mut a = v(&["clean", "-f"]);
    if include_dirs {
        a.push("-d".into());
    }
    if include_ignored {
        a.push("-x".into());
    }
    GitOp::new("Clean working tree").step_v(a)
}

pub fn gc(aggressive: bool, prune_now: bool) -> GitOp {
    let mut a = v(&["gc"]);
    if aggressive {
        a.push("--aggressive".into());
    }
    if prune_now {
        a.push("--prune=now".into());
    }
    GitOp::new("Garbage collect").step_v(a)
}

pub fn maintenance_run() -> GitOp {
    GitOp::new("Run maintenance").step(&["maintenance", "run", "--auto"])
}

pub fn fsck() -> GitOp {
    GitOp::new("Verify repository").step(&["fsck", "--full"])
}

pub fn config_set(key: &str, value: &str, scope: &str) -> GitOp {
    // `--replace-all` because a plain set refuses a key with several values, and `--` so a
    // key or value starting with a dash isn't an option.
    GitOp::new(format!("Set {key}")).step(&["config", &format!("--{scope}"), "--replace-all", "--", key, value]).own_config()
}

pub fn config_unset(key: &str, scope: &str) -> GitOp {
    GitOp::new(format!("Unset {key}")).step(&["config", &format!("--{scope}"), "--unset-all", "--", key]).own_config()
}

pub fn archive(rev: &str, output: &str, format: &str) -> GitOp {
    GitOp::new(format!("Archive {}", short(rev))).names(&[rev]).step(&["archive", &format!("--format={format}"), "-o", output, rev])
}

pub fn format_patch(range: &str, out_dir: &str) -> GitOp {
    GitOp::new("Create patches").names(&[range]).step(&["format-patch", "-o", out_dir, range, "--"])
}

pub fn sparse_set(patterns: &[String], cone: bool) -> GitOp {
    let mut a = v(&["sparse-checkout", "set"]);
    a.push(if cone { "--cone".into() } else { "--no-cone".into() });
    a.extend_from_slice(patterns);
    GitOp::new("Set sparse checkout").step_v(a)
}

pub fn sparse_disable() -> GitOp {
    GitOp::new("Disable sparse checkout").step(&["sparse-checkout", "disable"])
}

pub fn lfs_install() -> GitOp {
    GitOp::new("Install Git LFS").step(&["lfs", "install", "--local"]).own_config()
}

pub fn lfs_track(pattern: &str) -> GitOp {
    GitOp::new(format!("LFS track {pattern}")).step(&["lfs", "track", pattern]).step(&["add", ".gitattributes"])
}

pub fn lfs_pull() -> GitOp {
    GitOp::new("LFS pull").step(&["lfs", "pull"]).net()
}

pub fn lfs_prune() -> GitOp {
    GitOp::new("LFS prune").step(&["lfs", "prune"])
}

/// Free-form command typed into the Console. `args` are already split.
pub fn raw(args: Vec<String>) -> GitOp {
    let network = matches!(args.first().map(String::as_str), Some("fetch" | "pull" | "push" | "clone" | "ls-remote"));
    let mut op = GitOp::new(format!("git {}", args.join(" "))).step_v(args);
    op.network = network;
    op
}

/// Split a console command line honouring single and double quotes.
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut q: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (q, c) {
            (Some(x), c) if c == x => q = None,
            (Some('"'), '\\') if matches!(chars.peek(), Some('"' | '\\')) => cur.push(chars.next().unwrap()),
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                q = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---- GitLens-parity commands ---------------------------------------------------

/// Undo the last commit, keeping its changes staged.
/// Adds this repository to git's list, and git's task to the system scheduler if it isn't there.
pub fn maintenance_start() -> GitOp {
    GitOp::new("Turn on scheduled maintenance").step(&["maintenance", "start"])
}

/// Takes this repository off the list. The task stays, as other repositories may be on it.
/// `start` also set the two settings, which aren't worth leaving behind.
pub fn maintenance_unregister() -> GitOp {
    GitOp::new("Turn off scheduled maintenance").step(&["maintenance", "unregister"]).step(&["config", "--local", "--unset", "maintenance.auto"]).allow_fail().step(&["config", "--local", "--unset", "maintenance.strategy"]).allow_fail()
}

pub fn undo_commit() -> GitOp {
    GitOp::new("Undo commit").step(&["reset", "--soft", "HEAD~1"])
}

/// Push a branch only up to a given commit.
pub fn push_to_commit(remote: &str, sha: &str, branch: &str) -> GitOp {
    GitOp::new(format!("Push {branch} up to {}", crate::util::prefix(sha, 8))).step(&["push", "--progress", "--", remote, &format!("{sha}:refs/heads/{branch}")]).net()
}

/// Fold existing `fixup!` and `squash!` commits into their targets.
pub fn squash_fixups(base: &str) -> GitOp {
    GitOp::new("Squash fixups").step(&["rebase", "--interactive", "--autosquash", "--autostash", base]).env("GIT_SEQUENCE_EDITOR", "true").env("GIT_EDITOR", "true")
}

/// Change the message of the current commit. `--only` with no paths keeps what is
/// staged out of it, so a message edit can't change the commit's content.
pub fn reword_head(message: &str) -> GitOp {
    GitOp::new("Reword commit").step(&["commit", "--amend", "--only", "--cleanup=whitespace", "-F", "-"]).stdin(message.as_bytes().to_vec())
}

/// See [`apply_patch_file`] for how `three_way` works.
pub fn apply_patch_text(patch: &str, three_way: bool) -> GitOp {
    let op = GitOp::new("Apply patch").step(&["apply", "--whitespace=nowarn", "-"]).stdin(patch.as_bytes().to_vec());
    if three_way { op.or_else(v(&["apply", "--whitespace=nowarn", "--3way", "-"]), THREE_WAY_NOTE).stdin(patch.as_bytes().to_vec()) } else { op }
}

/// Apply a patch in another worktree of this repository.
pub fn apply_patch_in(worktree: &str, patch: &[u8]) -> GitOp {
    GitOp::new(format!("Copy changes to {worktree}")).step(&["-C", worktree, "apply", "--3way", "--whitespace=nowarn", "-"]).stdin(patch.to_vec())
}

/// Stashes are shared between worktrees, so any of them can apply one.
pub fn stash_apply_in(worktree: &str, sel: &str) -> GitOp {
    GitOp::new(format!("Apply {sel} in {worktree}")).step(&["-C", worktree, "stash", "apply", sel])
}

/// Soft-reset a branch to where it split off, so its work can be recomposed.
pub fn recompose_reset(base: &str) -> GitOp {
    GitOp::new("Prepare to recompose").step(&["reset", "--soft", base]).step(&["restore", "--staged", "."])
}

/// Add a remote, optionally confirming the URL answers first.
pub fn remote_add_checked(name: &str, url: &str) -> GitOp {
    let mut op = GitOp::new(format!("Add remote {name}")).step(&["ls-remote", "--heads", "--", url]);
    op.network = true;
    op.step(&["remote", "add", "--", name, url])
}

/// Add a remote and push the current branch to it.
pub fn publish_repository(name: &str, url: &str, branch: &str) -> GitOp {
    GitOp::new(format!("Publish to {name}")).step(&["remote", "add", "--", name, url]).step(&["push", "--progress", "--set-upstream", "--", name, branch]).net()
}

/// Folder comparison in the configured difftool. Real copies, since making a symlink
/// fails on Windows without the right privilege, even where `core.symlinks` is on.
pub fn difftool_dir(a: &str, b: &str) -> GitOp {
    GitOp::new(format!("Open directory compare of {a} and {b}")).step(&["difftool", "--dir-diff", "--no-symlinks", "--no-prompt", a, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(op: &GitOp) -> String {
        op.steps.iter().map(|s| s.args.join(" ")).collect::<Vec<_>>().join(" && ")
    }

    #[test]
    fn network_ops_build_expected_args_and_are_flagged() {
        let p = push(&PushOpts { remote: Some("origin".into()), refspec: Some("main".into()), set_upstream: true, force_with_lease: true, ..Default::default() });
        assert_eq!(args(&p), "push --progress --set-upstream --force-with-lease --force-if-includes -- origin main");
        assert!(p.network);
        let f = fetch(&FetchOpts { all: true, prune: true, ..Default::default() });
        assert_eq!(args(&f), "fetch --progress --all --prune");
        assert!(f.network);
        let l = pull(PullMode::Rebase, true);
        assert_eq!(args(&l), "pull --progress --rebase --autostash");
        assert!(l.network);
        assert_eq!(args(&branch_delete_remote("origin/feature/x", &[])), "push --delete -- origin feature/x");
        assert_eq!(args(&branch_delete_remote("a/b/c", &["a".to_string(), "a/b".to_string()])), "push --delete -- a/b c");
        assert_eq!(args(&fetch_pr("origin", "pull/7/head", "pr/7")), "fetch -- origin pull/7/head:pr/7");
    }

    #[test]
    fn remote_branch_checkout_strips_remote() {
        assert_eq!(args(&checkout_remote("origin/feature/x", &[])), "switch -c feature/x --track origin/feature/x");
        assert_eq!(args(&checkout_remote("a/b/c", &["a/b".to_string()])), "switch -c c --track a/b/c");
    }

    #[test]
    fn command_line() {
        assert_eq!(split_command_line(r#"log --grep="a b" -n 3 'c d' """#), vec!["log", "--grep=a b", "-n", "3", "c d", ""]);
    }

    #[test]
    fn raw_flags_network() {
        assert!(raw(vec!["push".into()]).network);
        assert!(!raw(vec!["status".into()]).network);
    }
}
