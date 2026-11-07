//! Read-only repository queries. Mutations live in [`crate::ops`].
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::diff::{FileDiff, parse_unified};
use crate::model::*;
use crate::parse;
use crate::runner::{GitExe, GitOutput, RunOpts};
use crate::{Error, Result};

#[derive(Clone)]
pub struct Repo {
    pub git: GitExe,
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    /// What its config can run, and whether that's allowed. See [`crate::trust`].
    pub trust: crate::trust::RepoTrust,
}

/// A revision as a command-line argument. Anything starting with a dash would be read as
/// an option: `--output=FILE` makes `log`, `show`, `diff` and `blame` overwrite that file.
/// Values come from links, from agents over MCP, from typing, and from ref names a server
/// chose. `--end-of-options` isn't the answer: `git blame --end-of-options HEAD -- a.txt`
/// fails, and blame's revision needs this anyway. A ref that really has such a name is
/// shown and used by its full name, see [`crate::parse::safe_ref_name`].
pub fn rev_arg(rev: &str) -> Result<&str> {
    if rev.starts_with('-') {
        return Err(Error::Other(format!("\"{rev}\" isn't a revision: it starts with a dash, so git would read it as an option.")));
    }
    Ok(rev)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WsMode {
    #[default]
    None,
    All,
    Change,
    Eol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffAlgo {
    Myers,
    Minimal,
    #[default]
    Patience,
    Histogram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOpts {
    pub ws: WsMode,
    pub algo: DiffAlgo,
    pub context: u32,
}

impl Default for DiffOpts {
    fn default() -> Self {
        Self { ws: WsMode::None, algo: DiffAlgo::Patience, context: 3 }
    }
}

impl DiffOpts {
    fn args(&self) -> Vec<String> {
        let mut v = vec![format!("-U{}", self.context), "-M".to_string(), "--no-ext-diff".to_string(), "--no-color".to_string()];
        v.push(
            match self.algo {
                DiffAlgo::Myers => "--diff-algorithm=myers",
                DiffAlgo::Minimal => "--diff-algorithm=minimal",
                DiffAlgo::Patience => "--diff-algorithm=patience",
                DiffAlgo::Histogram => "--diff-algorithm=histogram",
            }
            .to_string(),
        );
        match self.ws {
            WsMode::None => {}
            WsMode::All => v.push("-w".into()),
            WsMode::Change => v.push("-b".into()),
            WsMode::Eol => v.push("--ignore-space-at-eol".into()),
        }
        v
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlameOpts {
    pub ignore_ws: bool,
    /// `-M`: follow lines moved within the file.
    pub moves: bool,
    /// `-C`: follow lines copied from other files.
    pub copies: bool,
}

/// What a diff compares. Every diff in the UI is expressed as one of these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffSource {
    /// Worktree vs. index.
    Unstaged,
    /// Index vs. HEAD.
    Staged,
    /// Worktree vs. HEAD.
    WorkingVsHead,
    /// Commit vs. its first parent.
    Commit(String),
    /// Two-dot compare of two revisions.
    Range(String, String),
    /// Three-dot compare (changes on b since the merge base).
    MergeBase(String, String),
    /// Worktree vs. a revision.
    WorkingVs(String),
    /// Untracked file shown as all-new.
    Untracked,
    /// Two arbitrary paths on disk (`git diff --no-index`).
    Files(String, String),
}

impl Repo {
    /// The way in, from anywhere: it decides what the repository's config may run before
    /// anything reads it, so no caller has to remember to. See [`crate::trust`].
    pub fn open(mut git: GitExe, path: &Path) -> Result<Self> {
        // The caller's may have come from another repository.
        git.neutral = Default::default();
        let o = git.run_raw(Some(path), &["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"])?;
        if !o.ok() {
            return Err(no_work_tree_message(&git, path, &o.stderr).map_or_else(|| Error::Git(o.stderr.trim().to_string()), Error::Other));
        }
        let text = o.text();
        let mut lines = text.lines();
        let root = PathBuf::from(lines.next().unwrap_or_default());
        let git_dir = PathBuf::from(lines.next().unwrap_or_default());
        let common = lines.next().unwrap_or_default();
        let common_dir = if Path::new(common).is_absolute() { PathBuf::from(common) } else { path.join(common) };
        let trust = crate::trust::check(&git, &root)?;
        if trust.restricted() {
            git.neutral = trust.neutral.clone();
        }
        Ok(Self { git, root, git_dir, common_dir, trust })
    }

    /// For commands that need no repository, such as `diff --no-index` outside any.
    pub fn outside_any(git: GitExe, root: PathBuf, git_dir: PathBuf) -> Self {
        Self { git, root, common_dir: git_dir.clone(), git_dir, trust: Default::default() }
    }

    /// The user has answered. The answer is kept, so it isn't asked again until the list changes.
    pub fn set_trusted(&mut self, trusted: bool) -> Result<()> {
        crate::trust::remember(&self.root, &self.trust.hash, trusted)?;
        self.trust.state = if trusted { crate::trust::TrustState::Trusted } else { crate::trust::TrustState::Declined };
        self.git.neutral = if trusted { Default::default() } else { self.trust.neutral.clone() };
        Ok(())
    }

    /// Looks again after a write. A pull can bring in config through a tracked `include.path`.
    /// With `own_change`, the user just changed the config in gitem, so if the repository
    /// was fine before, this is fine too. Says whether anything changed.
    pub fn recheck_trust(&mut self, own_change: bool) -> Result<bool> {
        use crate::trust::TrustState::*;
        if !crate::trust::config_changed(&self.git, &self.root, &self.trust)? {
            return Ok(false);
        }
        let mut next = crate::trust::check(&self.git, &self.root)?;
        if next.hash == self.trust.hash {
            next.state = self.trust.state;
            self.trust = next;
            return Ok(false);
        }
        if own_change && matches!(self.trust.state, Clean | Trusted) && next.state == Undecided {
            crate::trust::remember(&self.root, &next.hash, true)?;
            next.state = Trusted;
        }
        self.git.neutral = if next.restricted() { next.neutral.clone() } else { Default::default() };
        self.trust = next;
        Ok(true)
    }

    pub fn name(&self) -> String {
        self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| self.root.display().to_string())
    }

    fn run(&self, args: &[&str]) -> Result<GitOutput> {
        self.git.run(Some(&self.root), args)
    }

    fn run_v(&self, args: &[String]) -> Result<GitOutput> {
        self.git.run_opts(Some(&self.root), args, &RunOpts::default(), &mut |_| {})?.into_result()
    }

    fn run_raw_v(&self, args: &[String]) -> Result<GitOutput> {
        self.git.run_opts(Some(&self.root), args, &RunOpts::default(), &mut |_| {})
    }

    /// A read where any paths given are exact names, not glob patterns.
    fn run_literal(&self, args: &[String]) -> Result<GitOutput> {
        let opts = RunOpts { env: vec![("GIT_LITERAL_PATHSPECS".to_string(), "1".to_string())], ..Default::default() };
        self.git.run_opts(Some(&self.root), args, &opts, &mut |_| {})
    }

    /// `log --numstat` and the like, which nobody clicked on and which read every file's
    /// contents. In a partial clone each missing one would be downloaded, one by one and
    /// perhaps asking for a password, so here git fails instead. What it had printed by then
    /// is still good. If that's nothing at all, the same log without the line counts is
    /// what there is to show.
    fn run_stats(&self, args: &[String]) -> Result<GitOutput> {
        let env = vec![("GIT_LITERAL_PATHSPECS".to_string(), "1".to_string()), ("GIT_NO_LAZY_FETCH".to_string(), "1".to_string())];
        let opts = RunOpts { env, ..Default::default() };
        let o = self.git.run_opts(Some(&self.root), args, &opts, &mut |_| {})?;
        if o.ok() || !o.stdout.is_empty() || !args.iter().any(|a| a == "--numstat") || !self.is_partial_clone() {
            return Ok(o);
        }
        let plain: Vec<String> = args.iter().filter(|a| *a != "--numstat").cloned().collect();
        self.git.run_opts(Some(&self.root), &plain, &opts, &mut |_| {})
    }

    fn is_partial_clone(&self) -> bool {
        self.git.run_raw(Some(&self.root), &["config", "--get-regexp", r"^remote\..*\.promisor$"]).is_ok_and(|o| o.ok())
    }

    // ---- status ------------------------------------------------------------

    pub fn status(&self) -> Result<RepoStatus> {
        let o = self.run(&["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])?;
        let mut st = parse::parse_status(&o.text());
        if st.head_sha == "(initial)" {
            st.head_sha.clear();
        }
        self.fill_op(&mut st);
        st.squash_pending = st.op == RepoOp::None && self.git_dir.join("SQUASH_MSG").exists();
        Ok(st)
    }

    fn fill_op(&self, st: &mut RepoStatus) {
        let g = &self.git_dir;
        let read = |name: &str| std::fs::read_to_string(g.join(name)).ok().map(|s| s.trim().to_string());
        let num = |name: &str| read(name).and_then(|s| s.parse::<u32>().ok());
        if g.join("rebase-merge").is_dir() {
            st.op = if g.join("rebase-merge/interactive").exists() { RepoOp::RebaseInteractive } else { RepoOp::Rebase };
            st.op_progress = num("rebase-merge/msgnum").zip(num("rebase-merge/end"));
            st.op_target = read("rebase-merge/onto").map(|s| crate::util::prefix(&s, 8).to_string());
            // With no pick under way the rebase is waiting on you, as at an edit step.
            st.op_allows_commits = !g.join("CHERRY_PICK_HEAD").exists() && !g.join("MERGE_HEAD").exists();
        } else if g.join("rebase-apply").is_dir() {
            st.op = if g.join("rebase-apply/applying").exists() { RepoOp::Am } else { RepoOp::Rebase };
            st.op_progress = num("rebase-apply/next").zip(num("rebase-apply/last"));
        } else if g.join("MERGE_HEAD").exists() {
            st.op = RepoOp::Merge;
            st.op_target = read("MERGE_HEAD").map(|s| crate::util::prefix(&s, 8).to_string());
        } else if g.join("CHERRY_PICK_HEAD").exists() {
            st.op = RepoOp::CherryPick;
            st.op_target = read("CHERRY_PICK_HEAD").map(|s| crate::util::prefix(&s, 8).to_string());
        } else if g.join("REVERT_HEAD").exists() {
            st.op = RepoOp::Revert;
            st.op_target = read("REVERT_HEAD").map(|s| crate::util::prefix(&s, 8).to_string());
        } else if let Some((op, left)) = self.sequencer_left() {
            st.op = op;
            st.op_target = Some(format!("{left} commit(s) left"));
        } else if g.join("BISECT_LOG").exists() {
            st.op = RepoOp::Bisect;
        }
    }

    /// A multi-commit pick or revert that's stopped between commits. Committing a resolved
    /// conflict by hand removes `CHERRY_PICK_HEAD` while later commits are still to come,
    /// and without this nothing would say so. The first line is the commit just dealt with.
    fn sequencer_left(&self) -> Option<(RepoOp, usize)> {
        let todo = std::fs::read_to_string(self.git_dir.join("sequencer/todo")).ok()?;
        let lines: Vec<&str> = todo.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
        let op = match lines.first()?.split_whitespace().next()? {
            "pick" | "p" => RepoOp::CherryPick,
            "revert" | "r" => RepoOp::Revert,
            _ => return None,
        };
        Some((op, lines.len().saturating_sub(1).max(1)))
    }

    /// The lock git leaves behind when it's killed or crashes, if there is one.
    pub fn index_lock(&self) -> Option<PathBuf> {
        Some(self.git_dir.join("index.lock")).filter(|p| p.exists())
    }

    /// Identifies the conflict's three versions. Empty once `path` is resolved.
    pub fn unmerged_signature(&self, path: &str) -> String {
        let args: Vec<String> = ["ls-files", "-u", "--", path].iter().map(|s| s.to_string()).collect();
        self.run_literal(&args).map(|o| o.text()).unwrap_or_default()
    }

    /// Full messages of the commits on HEAD after `since`, oldest first. An empty `since`
    /// means all of them, for a branch that had no commits.
    pub fn messages_since(&self, since: &str) -> Result<Vec<String>> {
        let range = if since.is_empty() { "HEAD".to_string() } else { format!("{}..HEAD", rev_arg(since)?) };
        let out = self.run(&["log", "--reverse", "--format=%B%x00", &range])?.text();
        Ok(out.split('\0').map(str::trim).filter(|m| !m.is_empty()).map(String::from).collect())
    }

    /// Message git prepared for the in-progress merge/revert/cherry-pick, or for a squash
    /// merge, which keeps its own file and leaves no other sign.
    pub fn prepared_message(&self) -> Option<String> {
        ["MERGE_MSG", "SQUASH_MSG"].iter().find_map(|f| std::fs::read_to_string(self.git_dir.join(f)).ok()).map(|s| without_conflict_notes(&s))
    }

    // ---- log ---------------------------------------------------------------

    pub fn log(&self, q: &LogQuery) -> Result<Vec<Commit>> {
        let mut a: Vec<String> = vec!["log".into(), "-z".into(), parse::LOG_FORMAT.into(), "--decorate=full".into()];
        if q.limit > 0 {
            a.push(format!("--max-count={}", q.limit));
        }
        if q.skip > 0 {
            a.push(format!("--skip={}", q.skip));
        }
        if q.exclude_stash {
            a.push("--exclude=refs/stash".into());
        }
        for x in &q.excludes {
            a.push(format!("--exclude={x}"));
        }
        if q.all {
            a.push("--all".into());
        }
        if q.all_refs_only {
            a.push("--glob=refs/*".into());
        }
        if q.first_parent {
            a.push("--first-parent".into());
        }
        if q.no_merges {
            a.push("--no-merges".into());
        }
        if q.follow {
            a.push("--follow".into());
        }
        let me = |x: &String| -> String {
            if x == "@me" { self.config_get("user.name").unwrap_or_default() } else { x.clone() }
        };
        if let Some(x) = &q.author {
            a.push(format!("--author={}", me(x)));
        }
        if let Some(x) = &q.committer {
            a.push(format!("--committer={}", me(x)));
        }
        if q.merges_only {
            a.push("--merges".into());
        }
        if q.invert_grep {
            a.push("--invert-grep".into());
        }
        if q.all_match {
            a.push("--all-match".into());
        }
        if let Some(x) = &q.grep {
            // Each its own `--grep`: git takes any of them, or all of them with `--all-match`.
            a.extend(x.split(GREP_TERMS).map(|t| format!("--grep={t}")));
            if !q.fixed_strings {
                a.push("--extended-regexp".into());
            }
        }
        // For `--author` and `--committer` as well, so an email can have a `+` in it.
        if q.fixed_strings {
            a.push("--fixed-strings".into());
        }
        if q.no_walk {
            a.push("--no-walk".into());
        }
        if q.regex_ignore_case {
            a.push("--regexp-ignore-case".into());
        }
        if let Some(x) = &q.since {
            a.push(format!("--since={x}"));
        }
        if let Some(x) = &q.until {
            a.push(format!("--until={x}"));
        }
        if let Some(x) = &q.pickaxe_s {
            a.push(format!("-S{x}"));
        }
        if let Some(x) = &q.pickaxe_g {
            a.push(format!("-G{x}"));
        }
        if let Some((s, e, p)) = &q.line_range {
            a.push(format!("-L{s},{e}:{p}"));
            a.push("-s".into());
        }
        // It walks the whole history before printing anything, so only for those who need it.
        if q.date_order {
            a.push("--date-order".into());
        }
        for r in &q.revs {
            a.push(rev_arg(r)?.to_string());
        }
        // Always, or a branch named like a file or folder beside it is "ambiguous".
        a.push("--".into());
        if q.line_range.is_none() {
            a.extend(q.paths.iter().cloned());
        }
        let o = if q.literal_paths { self.run_literal(&a)? } else { self.run_raw_v(&a)? };
        if !o.ok() {
            // An unborn branch simply has no commits yet.
            if q.revs.iter().any(|r| r == "HEAD") && has_no_commits(&o.stderr) || o.stderr.contains("does not have any commits") || o.stderr.contains("bad default revision") {
                return Ok(Vec::new());
            }
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        Ok(parse::parse_log(&o.text()))
    }

    /// For every merge this query would walk, its first parent. `--no-merges` leaves them out
    /// of the list while their children still name them, which is what lets the graph draw
    /// each child down to a commit that's really there.
    pub fn merge_first_parents(&self, q: &LogQuery) -> Result<HashMap<String, String>> {
        let mut a: Vec<String> = vec!["rev-list".into(), "--merges".into(), "--parents".into()];
        if q.exclude_stash {
            a.push("--exclude=refs/stash".into());
        }
        a.extend(q.excludes.iter().map(|x| format!("--exclude={x}")));
        if q.all {
            a.push("--all".into());
        }
        if q.all_refs_only {
            a.push("--glob=refs/*".into());
        }
        if q.first_parent {
            a.push("--first-parent".into());
        }
        for r in &q.revs {
            a.push(rev_arg(r)?.to_string());
        }
        a.push("--".into());
        let text = self.run_v(&a)?.text();
        Ok(text.lines().filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string()))
        }).collect())
    }

    pub fn commit(&self, rev: &str) -> Result<Commit> {
        let q = LogQuery { revs: vec![rev.to_string()], limit: 1, ..Default::default() };
        self.log(&q)?.into_iter().next().ok_or_else(|| Error::Git(format!("no such commit: {rev}")))
    }

    pub fn signature(&self, rev: &str) -> Result<SignatureInfo> {
        let o = self.run(&["log", "-1", "--format=%G?%x00%GS%x00%GK", rev_arg(rev)?, "--"])?;
        let t = o.text();
        let f: Vec<&str> = t.trim_end().split('\0').collect();
        Ok(SignatureInfo {
            status: f.first().unwrap_or(&"N").to_string(),
            signer: f.get(1).unwrap_or(&"").to_string(),
            key: f.get(2).unwrap_or(&"").to_string(),
        })
    }

    /// Branches and tags containing the commit.
    pub fn containing_refs(&self, sha: &str) -> Result<(Vec<String>, Vec<String>)> {
        let lines = |o: GitOutput, prefix: &str| -> Vec<String> { o.text().lines().map(|s| parse::safe_ref_name(prefix, s.trim())).filter(|s| !s.is_empty()).collect() };
        let sha = rev_arg(sha)?;
        let b = lines(self.run(&["for-each-ref", "--format=%(refname:short)", "--contains", sha, "refs/heads", "refs/remotes"])?, "refs/heads/");
        let t = lines(self.run(&["for-each-ref", "--format=%(refname:short)", "--contains", sha, "refs/tags"])?, "refs/tags/");
        Ok((b, t))
    }

    pub fn commit_note(&self, sha: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["notes", "show", rev_arg(sha).ok()?]).ok().filter(|o| o.ok()).map(|o| o.text())
    }

    pub fn describe(&self, sha: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["describe", "--tags", "--always", rev_arg(sha).ok()?]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
    }

    pub fn resolve(&self, rev: &str) -> Result<String> {
        // Quiet, so git gives no reason of its own.
        let o = self.git.run_raw(Some(&self.root), &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", rev_arg(rev)?)])?;
        if !o.ok() {
            return Err(Error::Git(if rev == "HEAD" { "This branch has no commits yet.".to_string() } else { format!("There's no commit called \"{rev}\".") }));
        }
        Ok(o.text().trim().to_string())
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        Ok(self.run(&["merge-base", rev_arg(a)?, rev_arg(b)?])?.text().trim().to_string())
    }

    /// (a-only, b-only) commit counts.
    pub fn ahead_behind(&self, a: &str, b: &str) -> Result<(u32, u32)> {
        let o = self.run(&["rev-list", "--left-right", "--count", &format!("{}...{}", rev_arg(a)?, rev_arg(b)?), "--"])?.text();
        let mut p = o.split_whitespace().map(|x| x.parse().unwrap_or(0));
        Ok((p.next().unwrap_or(0), p.next().unwrap_or(0)))
    }

    // ---- refs --------------------------------------------------------------

    pub fn branches(&self) -> Result<Vec<Branch>> {
        let o = self.run(&["for-each-ref", parse::BRANCH_FORMAT, "--sort=-committerdate", "refs/heads", "refs/remotes"])?;
        Ok(parse::parse_branches(&o.text()))
    }

    pub fn tags(&self) -> Result<Vec<Tag>> {
        let o = self.run(&["for-each-ref", parse::TAG_FORMAT, "--sort=-creatordate", "refs/tags"])?;
        Ok(parse::parse_tags(&o.text()))
    }

    pub fn remotes(&self) -> Result<Vec<Remote>> {
        Ok(parse::parse_remotes(&self.run(&["remote", "-v"])?.text()))
    }

    pub fn stashes(&self) -> Result<Vec<Stash>> {
        let o = self.git.run_raw(Some(&self.root), &["stash", "list", parse::STASH_FORMAT])?;
        if !o.ok() {
            return Ok(Vec::new());
        }
        Ok(parse::parse_stashes(&o.text()))
    }

    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let o = self.run(&["worktree", "list", "--porcelain"])?;
        Ok(parse::parse_worktrees(&o.text(), &self.root.to_string_lossy()))
    }

    pub fn submodules(&self) -> Result<Vec<Submodule>> {
        // A shell script running seven git commands, for an answer that's almost always none.
        if !self.root.join(".gitmodules").exists() {
            return Ok(Vec::new());
        }
        // A broken `.gitmodules` would otherwise look like having no submodules at all.
        let o = self.run(&["submodule", "status"])?;
        let mut subs = parse::parse_submodules(&o.text());
        if !subs.is_empty() {
            let cfg = self.git.run_raw(Some(&self.root), &["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.(url|path)$"])?;
            let mut urls: HashMap<String, String> = HashMap::new();
            let mut paths: HashMap<String, String> = HashMap::new();
            for l in cfg.text().lines() {
                if let Some((k, v)) = l.split_once(' ') {
                    if let Some(n) = k.strip_prefix("submodule.").and_then(|s| s.strip_suffix(".url")) {
                        urls.insert(n.to_string(), v.to_string());
                    } else if let Some(n) = k.strip_prefix("submodule.").and_then(|s| s.strip_suffix(".path")) {
                        paths.insert(v.to_string(), n.to_string());
                    }
                }
            }
            for s in &mut subs {
                if let Some(u) = paths.get(&s.path).and_then(|n| urls.get(n)) {
                    s.url = u.clone();
                }
            }
        }
        Ok(subs)
    }

    pub fn reflog(&self, rev: &str, limit: usize) -> Result<Vec<ReflogEntry>> {
        let o = self.git.run_raw(Some(&self.root), &["reflog", "show", parse::REFLOG_FORMAT, &format!("--max-count={limit}"), rev_arg(rev)?, "--"])?;
        if !o.ok() {
            if has_no_commits(&o.stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        Ok(parse::parse_reflog(&o.text()))
    }

    // ---- files & diffs -----------------------------------------------------

    pub fn file_at(&self, rev: &str, path: &str) -> Result<Vec<u8>> {
        Ok(self.run(&["show", &format!("{}:{path}", rev_arg(rev)?)])?.stdout)
    }

    /// Conflict stage content: 1 base, 2 ours, 3 theirs. Missing stage gives empty.
    pub fn stage_content(&self, stage: u8, path: &str) -> Vec<u8> {
        self.git.run_raw(Some(&self.root), &["show", &format!(":{stage}:{path}")]).ok().filter(|o| o.ok()).map(|o| o.stdout).unwrap_or_default()
    }

    pub fn worktree_file(&self, path: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.root.join(path))?)
    }

    /// The files git tracks, at a revision or in the index. Not untracked ones, which take
    /// ten times as long to look for here and which `status` lists already.
    /// Whether a file on disk looks binary, from its first few kilobytes.
    pub fn worktree_file_is_binary(&self, path: &str) -> bool {
        use std::io::Read;
        let mut head = [0u8; 8000];
        std::fs::File::open(self.root.join(path)).and_then(|mut f| f.read(&mut head)).is_ok_and(|n| crate::util::is_binary(&head[..n]) && !crate::util::has_utf16_bom(&head[..n]))
    }

    pub fn tracked_files(&self, rev: Option<&str>) -> Result<Vec<String>> {
        let o = match rev {
            Some(r) => {
                let o = self.git.run_raw(Some(&self.root), &["ls-tree", "-r", "--name-only", "-z", rev_arg(r)?])?;
                if !o.ok() && has_no_commits(&o.stderr) {
                    return Ok(Vec::new());
                }
                o.into_result()?
            }
            None => self.run(&["ls-files", "-z", "--cached", "--deduplicate"])?,
        };
        Ok(o.text().split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
    }

    pub fn commit_files(&self, sha: &str) -> Result<Vec<FileChange>> {
        let sha = rev_arg(sha)?;
        // One command for both, since each would detect renames all over again.
        let o = self.run(&["show", "--format=", "-M", "-m", "--first-parent", "--raw", "--numstat", "-z", sha, "--"])?;
        Ok(parse::parse_raw_numstat(&o.text()))
    }

    pub fn compare_files(&self, a: &str, b: &str, three_dot: bool) -> Result<Vec<FileChange>> {
        let (a, b) = (rev_arg(a)?, rev_arg(b)?);
        let spec = if three_dot { format!("{a}...{b}") } else { format!("{a}..{b}") };
        let ns = self.run(&["diff", "-M", "--name-status", "-z", &spec, "--"])?;
        let mut files = parse::parse_name_status(&ns.text());
        let st = self.run(&["diff", "-M", "--numstat", "-z", &spec, "--"])?;
        merge_numstat(&mut files, &st.text());
        Ok(files)
    }

    pub fn diff(&self, src: &DiffSource, path: Option<&str>, opts: &DiffOpts) -> Result<Vec<FileDiff>> {
        check_source_revs(src)?;
        let files = self.diff_paths(src, path.map(|p| vec![p]).unwrap_or_default(), opts)?;
        // Asked about one file, git can't see where a renamed one came from, and says it's
        // new with every line added. A new file is rare enough to ask about the whole change.
        if let (Some(p), [f]) = (path, files.as_slice())
            && f.is_new
            && let Some(old) = self.rename_source(src, p)
        {
            return self.diff_paths(src, vec![p, &old], opts);
        }
        Ok(files)
    }

    fn diff_paths(&self, src: &DiffSource, paths: Vec<&str>, opts: &DiffOpts) -> Result<Vec<FileDiff>> {
        let mut a: Vec<String> = Vec::new();
        let finish = |a: &mut Vec<String>| {
            // Even with no path: a branch named like a file is "ambiguous" otherwise.
            a.push("--".into());
            a.extend(paths.iter().map(|p| p.to_string()));
        };
        match src {
            DiffSource::Unstaged => {
                a.push("diff".into());
                a.extend(opts.args());
                finish(&mut a);
            }
            DiffSource::Staged => {
                a.push("diff".into());
                a.push("--cached".into());
                a.extend(opts.args());
                finish(&mut a);
            }
            DiffSource::WorkingVsHead => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(self.head_or_empty_tree());
                finish(&mut a);
            }
            DiffSource::Commit(sha) => {
                a.extend(["show".to_string(), "--format=".into(), "-m".into(), "--first-parent".into()]);
                a.extend(opts.args());
                a.push(sha.clone());
                finish(&mut a);
            }
            DiffSource::Range(x, y) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(format!("{x}..{y}"));
                finish(&mut a);
            }
            DiffSource::MergeBase(x, y) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(format!("{x}...{y}"));
                finish(&mut a);
            }
            DiffSource::WorkingVs(r) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(r.clone());
                finish(&mut a);
            }
            DiffSource::Untracked => {
                a.push("diff".into());
                a.push("--no-index".into());
                a.extend(opts.args());
                a.push("--".into());
                a.push("/dev/null".into());
                a.push(paths.first().copied().unwrap_or_default().to_string());
            }
            DiffSource::Files(x, y) => {
                a.push("diff".into());
                a.push("--no-index".into());
                a.extend(opts.args());
                a.push("--".into());
                a.push(x.clone());
                a.push(y.clone());
            }
        }
        let o = self.run_literal(&a)?;
        // --no-index exits 1 when files differ.
        if o.code > 1 || (o.code == 1 && !matches!(src, DiffSource::Untracked | DiffSource::Files(..))) {
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        Ok(parse_unified(&o.text()))
    }

    /// The file `path` was renamed from in this change, if it was.
    fn rename_source(&self, src: &DiffSource, path: &str) -> Option<String> {
        let mut a: Vec<String> = match src {
            DiffSource::Staged => vec!["diff".into(), "--cached".into()],
            DiffSource::WorkingVsHead => vec!["diff".into(), self.head_or_empty_tree()],
            DiffSource::Commit(s) => vec!["show".into(), "--format=".into(), "-m".into(), "--first-parent".into(), s.clone()],
            DiffSource::Range(x, y) => vec!["diff".into(), format!("{x}..{y}")],
            DiffSource::MergeBase(x, y) => vec!["diff".into(), format!("{x}...{y}")],
            DiffSource::WorkingVs(r) => vec!["diff".into(), r.clone()],
            DiffSource::Unstaged | DiffSource::Untracked | DiffSource::Files(..) => return None,
        };
        a.extend(["-M".into(), "--name-status".into(), "-z".into(), "--".into()]);
        let o = self.run_raw_v(&a).ok()?;
        parse::parse_name_status(&o.text()).into_iter().find(|f| f.path == path).and_then(|f| f.orig_path)
    }

    /// What "the last commit" is to compare with, also on a branch that doesn't have one yet.
    fn head_or_empty_tree(&self) -> String {
        if self.git.run_raw(Some(&self.root), &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok_and(|o| o.ok()) {
            return "HEAD".to_string();
        }
        // Worked out, not written down: it differs between SHA-1 and SHA-256 repositories.
        let opts = RunOpts { stdin: Some(Vec::new()), ..Default::default() };
        let args = ["hash-object", "-t", "tree", "--stdin"].map(String::from);
        self.git.run_opts(Some(&self.root), &args, &opts, &mut |_| {}).map(|o| o.text().trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "HEAD".to_string())
    }

    /// Full textual diff, used for AI prompts and patch export.
    pub fn diff_text(&self, src: &DiffSource) -> Result<String> {
        check_source_revs(src)?;
        let opts = DiffOpts::default();
        // `color.ui` set by the runner doesn't reach `color.diff`, and a colour code or an
        // external tool's output is no use to anything reading this.
        let flags = ["--no-color".to_string(), "--no-ext-diff".to_string()];
        let mut args: Vec<String> = match src {
            DiffSource::Staged => vec!["diff".into(), "--cached".into(), format!("-U{}", opts.context)],
            DiffSource::Unstaged => vec!["diff".into(), format!("-U{}", opts.context)],
            DiffSource::WorkingVsHead => vec!["diff".into(), self.head_or_empty_tree()],
            DiffSource::Commit(s) => vec!["show".into(), "--format=".into(), "-m".into(), "--first-parent".into(), s.clone()],
            DiffSource::Range(a, b) => vec!["diff".into(), format!("{a}..{b}")],
            DiffSource::MergeBase(a, b) => vec!["diff".into(), format!("{a}...{b}")],
            DiffSource::WorkingVs(r) => vec!["diff".into(), r.clone()],
            _ => return Err(Error::Other("unsupported diff source".into())),
        };
        args.extend(flags);
        args.push("--".into());
        Ok(self.run_v(&args)?.text())
    }

    /// The changes as a patch that applies back exactly: binary files included, bytes as
    /// they are whatever the encoding, and for working changes, new untracked files too.
    /// That's what someone expects to get back after discarding their work.
    pub fn patch_bytes(&self, src: &DiffSource) -> Result<Vec<u8>> {
        check_source_revs(src)?;
        // A converted text is no use as a patch, and it would run a program besides.
        let flags = ["--binary", "--full-index", "--no-textconv", "--no-ext-diff", "--no-color"].map(String::from);
        let mut args: Vec<String> = match src {
            DiffSource::Staged => vec!["diff".into(), "--cached".into()],
            DiffSource::Unstaged => vec!["diff".into()],
            DiffSource::WorkingVsHead => vec!["diff".into(), self.head_or_empty_tree()],
            DiffSource::Commit(s) => vec!["show".into(), "--format=".into(), "-m".into(), "--first-parent".into(), s.clone()],
            DiffSource::Range(a, b) => vec!["diff".into(), format!("{a}..{b}")],
            DiffSource::MergeBase(a, b) => vec!["diff".into(), format!("{a}...{b}")],
            DiffSource::WorkingVs(r) => vec!["diff".into(), r.clone()],
            _ => return Err(Error::Other("unsupported diff source".into())),
        };
        args.extend(flags);
        args.push("--".into());
        if *src == DiffSource::WorkingVsHead {
            return self.diff_with_untracked(&args);
        }
        Ok(self.run_v(&args)?.stdout)
    }

    /// As text, for the clipboard, which can't carry arbitrary bytes. An error rather than
    /// a patch with its bytes changed, which would apply wrongly or not at all.
    pub fn patch_text(&self, src: &DiffSource) -> Result<String> {
        String::from_utf8(self.patch_bytes(src)?).map_err(|_| Error::Other("These changes include text that isn't UTF-8, which the clipboard can't carry exactly. Save them as a patch in the Patches view instead.".into()))
    }

    /// Runs a diff that also sees untracked files, by marking them as intended to be added
    /// in a copy of the index. The real index isn't touched.
    fn diff_with_untracked(&self, args: &[String]) -> Result<Vec<u8>> {
        let untracked: Vec<String> = self.status()?.entries.into_iter().filter(|e| e.worktree == FileStatus::Untracked).map(|e| e.path).collect();
        if untracked.is_empty() {
            return Ok(self.run_v(args)?.stdout);
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("gitem-patch-index-{}-{n}", std::process::id()));
        let result = (|| -> Result<Vec<u8>> {
            match std::fs::copy(self.git_dir.join("index"), &tmp) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::Other(format!("Could not copy the index: {e}"))),
            }
            let env = vec![("GIT_INDEX_FILE".to_string(), tmp.to_string_lossy().into_owned()), ("GIT_LITERAL_PATHSPECS".to_string(), "1".to_string())];
            let mut names = Vec::new();
            for p in &untracked {
                names.extend_from_slice(p.as_bytes());
                names.push(0);
            }
            let add = ["add", "--intent-to-add", "--pathspec-from-file=-", "--pathspec-file-nul"].map(String::from);
            self.git.run_opts(Some(&self.root), &add, &RunOpts { stdin: Some(names), env: env.clone(), ..Default::default() }, &mut |_| {})?.into_result()?;
            Ok(self.git.run_opts(Some(&self.root), args, &RunOpts { env, ..Default::default() }, &mut |_| {})?.into_result()?.stdout)
        })();
        let _ = std::fs::remove_file(&tmp);
        result
    }

    // ---- blame -------------------------------------------------------------

    pub fn blame(&self, path: &str, rev: Option<&str>, opts: BlameOpts) -> Result<Vec<BlameLine>> {
        let mut a: Vec<String> = vec!["blame".into(), "--line-porcelain".into()];
        if opts.ignore_ws {
            a.push("-w".into());
        }
        if opts.moves {
            a.push("-M".into());
        }
        if opts.copies {
            a.push("-C".into());
        }
        if let Some(r) = rev {
            a.push(rev_arg(r)?.to_string());
        }
        a.push("--".into());
        a.push(path.to_string());
        Ok(parse::parse_blame(&self.run_v(&a)?.text()))
    }

    // ---- history & stats ---------------------------------------------------

    /// All commits touching a path, following renames, with line stats.
    pub fn file_history_points(&self, path: &str, limit: usize) -> Result<Vec<FileHistoryPoint>> {
        let o = self
            .run_stats(&[
                "log".into(),
                "-z".into(),
                parse::STAT_LOG_FORMAT.into(),
                "--numstat".into(),
                "--follow".into(),
                format!("--max-count={limit}"),
                "--".into(),
                path.to_string(),
            ])?;
        // Output means a partial clone's missing file cut it short: this much is true.
        if !o.ok() && o.stdout.is_empty() {
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        Ok(parse::parse_stat_log(&o.text())
            .into_iter()
            .map(|c| FileHistoryPoint {
                sha: c.sha,
                author: c.author,
                email: c.email,
                time: c.time,
                subject: c.subject,
                additions: c.additions,
                deletions: c.deletions,
                path: c.path.unwrap_or_else(|| path.to_string()),
            })
            .collect())
    }

    /// Commits touching a folder, a whole branch, or everything, for Visual
    /// History. Renames are only followed for single files.
    pub fn history_points(&self, path: Option<&str>, rev: &str, limit: usize) -> Result<Vec<FileHistoryPoint>> {
        let mut a: Vec<String> = vec!["log".into(), "-z".into(), parse::STAT_LOG_FORMAT.into(), "--numstat".into(), format!("--max-count={limit}"), rev_arg(rev)?.to_string(), "--".into()];
        if let Some(p) = path {
            a.push(p.to_string());
        }
        let o = self.run_stats(&a)?;
        if !o.ok() {
            if rev == "HEAD" && has_no_commits(&o.stderr) {
                return Ok(Vec::new());
            }
            if o.stdout.is_empty() {
                return Err(Error::Git(o.stderr.trim().to_string()));
            }
        }
        Ok(parse::parse_stat_log(&o.text())
            .into_iter()
            .map(|c| FileHistoryPoint { sha: c.sha, author: c.author, email: c.email, time: c.time, subject: c.subject, additions: c.additions, deletions: c.deletions, path: c.path.unwrap_or_default() })
            .collect())
    }

    /// Lines added and removed per commit, for the graph's Changes column.
    pub fn commit_stats(&self, revs: &[String], all: bool, limit: usize) -> Result<std::collections::HashMap<String, (u32, u32)>> {
        let mut a: Vec<String> = vec!["log".into(), "--format=\x01%H".into(), "--numstat".into(), format!("--max-count={limit}")];
        if all {
            a.push("--all".into());
        }
        for r in revs {
            a.push(rev_arg(r)?.to_string());
        }
        a.push("--".into());
        // Commits past a failure simply have no figures, as with a partial clone's missing files.
        let o = self.run_stats(&a)?;
        Ok(parse::parse_commit_stats(&o.text()))
    }

    /// Files that would conflict if `a` and `b` merged, found without touching
    /// the working tree. Only adds unreferenced objects, which `gc` removes.
    pub fn predict_conflicts(&self, a: &str, b: &str) -> Result<Vec<String>> {
        let o = self.git.run_raw(Some(&self.root), &["merge-tree", "--write-tree", "--name-only", "--no-messages", "-z", rev_arg(a)?, rev_arg(b)?])?;
        match o.code {
            0 => Ok(Vec::new()),
            1 => Ok(o.text().split('\0').skip(1).filter(|l| !l.is_empty()).map(String::from).collect()),
            _ => Err(Error::Git(o.stderr.trim().to_string())),
        }
    }

    /// (path, size in bytes) of every file at `rev`.
    pub fn file_sizes(&self, rev: &str) -> Result<Vec<(String, u64)>> {
        let o = self.git.run_raw(Some(&self.root), &["ls-tree", "-r", "-l", "-z", rev_arg(rev)?])?;
        if !o.ok() && rev == "HEAD" && has_no_commits(&o.stderr) {
            return Ok(Vec::new());
        }
        Ok(parse::parse_ls_tree_sizes(&o.into_result()?.text()))
    }

    /// (path, commits touching it) over recent history.
    pub fn file_activity(&self, rev: &str, limit: usize) -> Result<Vec<(String, u64)>> {
        let o = self.git.run_raw(Some(&self.root), &["log", "-z", "--format=", "--name-only", &format!("--max-count={limit}"), rev_arg(rev)?, "--"])?;
        if !o.ok() && rev == "HEAD" && has_no_commits(&o.stderr) {
            return Ok(Vec::new());
        }
        let text = o.into_result()?.text();
        let mut m: HashMap<String, u64> = HashMap::new();
        for l in text.split('\0').map(|l| l.trim_start_matches('\n')).filter(|l| !l.is_empty()) {
            *m.entry(l.to_string()).or_default() += 1;
        }
        // Files since deleted are in that history, but there's nothing left to open.
        let tree = self.git.run_raw(Some(&self.root), &["ls-tree", "-r", "--name-only", "-z", rev_arg(rev)?])?.into_result()?.text();
        let now: std::collections::HashSet<&str> = tree.split('\0').collect();
        Ok(m.into_iter().filter(|(p, _)| now.contains(p.as_str())).collect())
    }

    /// How well this repository is tuned for its size.
    pub fn health(&self) -> Result<RepoHealth> {
        // In the order git reads them, so the last one for a key is the one in force.
        let config = self.config_list()?;
        let in_force = |k: &str| config.iter().rev().find(|e| e.key.eq_ignore_ascii_case(k));
        let is_on = |k: &str| in_force(k).is_some_and(|e| matches!(e.value.to_lowercase().as_str(), "true" | "yes" | "on" | "1"));
        let registered = config.iter().any(|e| e.key.eq_ignore_ascii_case("maintenance.repo") && same_folder(&e.value, &self.root));
        let tracked = self.run(&["ls-files", "-z"])?.stdout.iter().filter(|b| **b == 0).count();
        let commits: u64 = self.git.run_raw(Some(&self.root), &["rev-list", "--count", "--all"])?.text().trim().parse().unwrap_or(0);
        let stats = self.count_objects()?;
        let stat = |key: &str| -> String { stats.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim_start_matches(':').trim().to_string())).unwrap_or_default() };
        let mut items = vec![
            HealthItem { label: "Commit-graph cache".into(), detail: "Speeds up history walks and the commit graph.".into(), config: vec![("core.commitGraph".into(), "true".into()), ("fetch.writeCommitGraph".into(), "true".into())], enabled: is_on("fetch.writeCommitGraph"), ..Default::default() },
            HealthItem { label: "Untracked cache".into(), detail: "Speeds up status by remembering untracked folders.".into(), config: vec![("core.untrackedCache".into(), "true".into())], enabled: is_on("core.untrackedCache"), ..Default::default() },
            HealthItem { label: "File system monitor".into(), detail: "Lets status skip scanning files that didn't change, in your terminal and editor. gitem turns it off for its own reads, since it starts a program named by the repository's config.".into(), config: vec![("core.fsmonitor".into(), "true".into())], enabled: is_on("core.fsmonitor"), ..Default::default() },
            HealthItem { label: "Large-repository index".into(), detail: "Index format for repositories with many files.".into(), config: vec![("feature.manyFiles".into(), "true".into())], enabled: is_on("feature.manyFiles"), ..Default::default() },
            HealthItem { label: "Sparse index".into(), detail: "Smaller index when using sparse checkout.".into(), config: vec![("index.sparse".into(), "true".into())], enabled: is_on("index.sparse"), ..Default::default() },
            HealthItem { label: "Scheduled maintenance".into(), detail: "Git keeps this repository optimized in the background, using a task it adds to your system's scheduler.".into(), enabled: registered, maintenance: true, ..Default::default() },
        ];
        for item in &mut items {
            // Only what's on can be undone, and only here if that's where it's on.
            let scope = item.config.iter().filter_map(|(k, _)| in_force(k)).map(|e| e.scope.as_str()).find(|s| !matches!(*s, "local" | "worktree"));
            item.set_elsewhere = scope.filter(|_| item.enabled).map(String::from);
        }
        Ok(RepoHealth { tracked_files: tracked, commits, loose_objects: stat("count"), pack_size: stat("size-pack"), items })
    }

    /// `.aiignore`-style rules, for keeping files out of AI requests.
    pub fn ai_ignore_patterns(&self) -> Vec<String> {
        let mut v = Vec::new();
        for f in [".aiignore", ".cursorignore", ".aiexclude"] {
            if let Ok(t) = std::fs::read_to_string(self.root.join(f)) {
                v.extend(t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from));
            }
        }
        v
    }

    /// Whether `.aiignore` and its like keep this file out of AI requests.
    pub fn ai_ignored(&self, path: &str) -> bool {
        crate::ai::is_ignored(&self.ai_ignore_patterns(), path)
    }

    /// A diff prepared for an AI request, honouring ignore rules.
    pub fn diff_for_ai(&self, src: &DiffSource) -> Result<String> {
        Ok(crate::ai::filter_diff(&self.diff_text(src)?, &self.ai_ignore_patterns()))
    }

    pub fn contributors(&self, rev: &str, with_stats: bool) -> Result<Vec<Contributor>> {
        let mut a: Vec<String> = vec!["log".into(), "-z".into(), parse::STAT_LOG_FORMAT.into(), rev_arg(rev)?.to_string(), "--".into()];
        if with_stats {
            a.insert(3, "--numstat".into());
        }
        let raw = self.run_stats(&a)?;
        if !raw.ok() && raw.stdout.is_empty() {
            if has_no_commits(&raw.stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::Git(raw.stderr.trim().to_string()));
        }
        let me = self.config_get("user.email").unwrap_or_default().to_lowercase();
        let mut map: HashMap<String, Contributor> = HashMap::new();
        for c in parse::parse_stat_log(&raw.text()) {
            let key = c.email.to_lowercase();
            let e = map.entry(key.clone()).or_insert_with(|| Contributor {
                name: c.author.clone(),
                email: c.email.clone(),
                first: c.time,
                last: c.time,
                is_you: key == me,
                ..Default::default()
            });
            e.commits += 1;
            e.additions += c.additions as u64;
            e.deletions += c.deletions as u64;
            e.first = e.first.min(c.time);
            e.last = e.last.max(c.time);
        }
        let mut v: Vec<Contributor> = map.into_values().collect();
        v.sort_by(|a, b| b.commits.cmp(&a.commits).then_with(|| a.name.cmp(&b.name)));
        Ok(v)
    }

    /// Commit timestamps for activity heatmaps, of one person if an email is given. Git matches
    /// after `.mailmap` has joined a person's addresses, as the Contributors list does. The
    /// brackets stop `ann@x` from also finding `joann@x`.
    pub fn commit_times(&self, rev: &str, author_email: Option<&str>, since: &str) -> Result<Vec<i64>> {
        let mut a: Vec<String> = vec!["log".into(), "--format=%at".into(), format!("--since={since}"), rev_arg(rev)?.to_string()];
        if let Some(x) = author_email {
            a.push(format!("--author=<{x}>"));
            a.push("--fixed-strings".into());
            a.push("--regexp-ignore-case".into());
        }
        a.push("--".into());
        let o = self.run_raw_v(&a)?;
        if !o.ok() {
            if has_no_commits(&o.stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        Ok(o.text().lines().filter_map(|l| l.trim().parse().ok()).collect())
    }

    // ---- config, hooks & misc ----------------------------------------------

    pub fn config_list(&self) -> Result<Vec<ConfigEntry>> {
        let o = self.run(&["config", "--list", "--show-scope", "--show-origin", "-z"])?;
        Ok(parse::parse_config(&o.text()))
    }

    pub fn config_get(&self, key: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["config", "--get", key]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
    }

    /// A path from the user's own config, whatever this repository says.
    pub fn global_config_path(&self, key: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["config", "--global", "--type=path", "--get", key]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
    }

    pub fn hooks(&self) -> Vec<HookInfo> {
        const NAMES: &[&str] = &[
            "applypatch-msg",
            "pre-applypatch",
            "post-applypatch",
            "pre-commit",
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "pre-rebase",
            "post-checkout",
            "post-merge",
            "pre-push",
            "post-rewrite",
            "pre-auto-gc",
            "reference-transaction",
            "push-to-checkout",
            "sendemail-validate",
            "fsmonitor-watchman",
        ];
        // As a path, so `~/` means what it means to git. Hooks run at the top of the working
        // tree, so a relative one starts there. `rev-parse --git-path` would say the same,
        // were it not that reads turn hooks off.
        let hooks_path = self.git.run_raw(Some(&self.root), &["config", "--type=path", "--get", "core.hooksPath"]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string());
        let dir = hooks_path.map(|p| self.root.join(p)).unwrap_or_else(|| self.common_dir.join("hooks"));
        NAMES
            .iter()
            .map(|n| {
                let on = dir.join(n);
                let off = dir.join(format!("{n}.disabled"));
                let (path, enabled, exists) = if on.exists() {
                    (on, true, true)
                } else if off.exists() {
                    (off, false, true)
                } else {
                    (on, false, false)
                };
                HookInfo { name: n.to_string(), path: path.display().to_string(), enabled, exists }
            })
            .collect()
    }

    pub fn bisect_state(&self) -> BisectState {
        let mut s = BisectState::default();
        let Ok(log) = std::fs::read_to_string(self.git_dir.join("BISECT_LOG")) else { return s };
        s.active = true;
        // Git records each verdict as a comment, whether given at start or later.
        let sha_of = |rest: &str| rest.trim_start_matches('[').split(']').next().unwrap_or("").to_string();
        for l in log.lines() {
            if let Some(x) = l.strip_prefix("# good: ") {
                let sha = sha_of(x);
                if !s.good.contains(&sha) {
                    s.good.push(sha);
                }
            } else if let Some(x) = l.strip_prefix("# bad: ") {
                s.bad = Some(sha_of(x));
            }
        }
        s.culprit = culprit_in(&log);
        s.only_skipped = s.culprit.is_none() && log.lines().any(|l| l.starts_with("# only skipped commits left to test"));
        s.log = log;
        s
    }

    pub fn lfs_files(&self) -> Result<Vec<LfsFile>> {
        let o = self.git.run_raw(Some(&self.root), &["lfs", "ls-files", "-l"])?;
        if !o.ok() {
            return Err(Error::Git(if o.stderr.is_empty() { "git-lfs is not available".to_string() } else { o.stderr.trim().to_string() }));
        }
        let mut v = Vec::new();
        for line in o.text().lines() {
            // "<oid> * path" (downloaded) or "<oid> - path" (pointer only)
            let mut p = line.splitn(3, ' ');
            if let (Some(oid), Some(mark), Some(path)) = (p.next(), p.next(), p.next()) {
                v.push(LfsFile { oid: oid.to_string(), path: path.to_string(), downloaded: mark == "*" });
            }
        }
        Ok(v)
    }

    pub fn sparse_patterns(&self) -> Vec<String> {
        self.git
            .run_raw(Some(&self.root), &["sparse-checkout", "list"])
            .ok()
            .filter(|o| o.ok())
            .map(|o| o.text().lines().map(String::from).collect())
            .unwrap_or_default()
    }

    pub fn clean_preview(&self, include_dirs: bool, include_ignored: bool) -> Result<Vec<String>> {
        let mut a = vec!["clean", "-n"];
        if include_dirs {
            a.push("-d");
        }
        if include_ignored {
            a.push("-x");
        }
        let o = self.run(&a)?;
        Ok(o.text().lines().filter_map(|l| l.strip_prefix("Would remove ").map(crate::diff::unquote_path)).collect())
    }

    /// The version of each file `b` has that differs from the merge base of `a` and `b`.
    pub fn compare_blobs(&self, a: &str, b: &str) -> Result<HashMap<String, String>> {
        let range = format!("{}...{}", rev_arg(a)?, rev_arg(b)?);
        let o = self.run_v(&["diff".to_string(), "--raw".to_string(), "--no-abbrev".to_string(), "--no-renames".to_string(), "-z".to_string(), range, "--".to_string()])?;
        let text = o.text();
        let mut fields = text.split('\0');
        let mut out = HashMap::new();
        while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
            if let Some(blob) = meta.split_whitespace().nth(3) {
                out.insert(path.to_string(), blob.to_string());
            }
        }
        Ok(out)
    }

    /// Untracked repositories inside this one. `git clean` leaves them out of what it lists,
    /// and doesn't delete them, without saying so.
    pub fn clean_left_alone(&self) -> Result<Vec<String>> {
        let o = self.run(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
        Ok(o.text().split('\0').filter_map(|e| e.strip_prefix("?? ")).filter(|p| p.ends_with('/') && self.is_nested_repo(p)).map(String::from).collect())
    }

    /// Dates git can't read count as "now" with no complaint, so `after:notadate` finds
    /// nothing and `before:notadate` finds everything. A date that came out as this very
    /// moment is one of those, unless it asked for that.
    pub fn check_search_dates(&self, q: &LogQuery) -> Result<()> {
        for (name, value) in [("after", &q.since), ("before", &q.until)] {
            let Some(value) = value else { continue };
            let low = value.to_lowercase();
            if ["now", "today", "0 "].iter().any(|w| low.contains(w)) {
                continue;
            }
            let out = self.run_v(&["rev-parse".to_string(), format!("--since={value}")])?.text();
            let when: i64 = out.trim().rsplit('=').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if (crate::util::now_ts() - when).abs() < 5 {
                return Err(Error::Other(format!("Git can't read \"{value}\" as a date for {name}:. Try 2024-01-31 or \"2 weeks ago\".")));
            }
        }
        Ok(())
    }

    /// Whether an untracked path is a repository of its own, which `git clean` leaves alone
    /// without saying so and a diff can't look into.
    pub fn is_nested_repo(&self, path: &str) -> bool {
        self.root.join(path).join(".git").exists()
    }

    pub fn count_objects(&self) -> Result<String> {
        Ok(self.run(&["count-objects", "-vH"])?.text())
    }

    /// The operation in progress, read from `.git` without running anything.
    pub fn current_op(&self) -> RepoOp {
        let mut st = RepoStatus::default();
        self.fill_op(&mut st);
        st.op
    }

    pub fn ignore_path(&self) -> PathBuf {
        self.root.join(".gitignore")
    }

    pub fn check_ignore(&self, path: &str) -> Option<String> {
        let o = self.git.run_raw(Some(&self.root), &["check-ignore", "-v", "--", path]).ok()?;
        o.ok().then(|| o.text().trim().to_string())
    }

    /// Next commit that touched `path` after `sha` on the way to HEAD (for
    /// walking a file's revisions forward).
    pub fn next_revision(&self, path: &str, sha: &str) -> Option<String> {
        let args: Vec<String> = ["rev-list", "--reverse", "--ancestry-path", &format!("{}..HEAD", rev_arg(sha).ok()?), "--", path].iter().map(|s| s.to_string()).collect();
        let o = self.run_literal(&args).ok()?;
        o.text().lines().next().map(String::from)
    }

    /// Previous commit that touched `path` before `sha`, with the file's name
    /// there, since it may have been renamed.
    pub fn prev_revision(&self, path: &str, sha: &str) -> Option<(String, String)> {
        let full = self.resolve(sha).ok()?;
        let args: Vec<String> = ["log", "-z", "--follow", "--format=%x01%H", "--name-only", "--max-count=3", &full, "--", path].iter().map(|s| s.to_string()).collect();
        let o = self.run_literal(&args).ok()?;
        let text = o.text();
        let mut records = text.split('\x01').filter(|r| !r.trim().is_empty());
        records.find_map(|r| {
            let mut fields = r.split('\0').map(|l| l.trim_start_matches('\n')).filter(|l| !l.is_empty());
            let found = fields.next()?;
            let name = fields.next()?;
            (found != full).then(|| (found.to_string(), name.to_string()))
        })
    }

    /// Default base branch: origin/HEAD, else main/master/develop if present.
    pub fn default_branch(&self) -> Option<String> {
        if let Some(b) = self.origin_head() {
            return Some(b);
        }
        for c in ["main", "master", "develop", "trunk"] {
            if self.git.run_raw(Some(&self.root), &["rev-parse", "--verify", "--quiet", c]).map(|o| o.ok()).unwrap_or(false) {
                return Some(c.to_string());
            }
        }
        None
    }

    fn origin_head(&self) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
    }

    /// The same answer, given the branches already read, so without asking git about each name.
    pub fn default_branch_among(&self, branches: &[Branch]) -> Option<String> {
        self.origin_head().or_else(|| ["main", "master", "develop", "trunk"].iter().find(|c| branches.iter().any(|b| !b.is_remote && b.name == **c)).map(|c| c.to_string()))
    }

    /// What `log` costs for a list of people, for those who don't need dates or line counts.
    pub fn people(&self, rev: &str) -> Result<Vec<Contributor>> {
        let raw = self.git.run_raw(Some(&self.root), &["shortlog", "-sne", rev_arg(rev)?, "--"])?;
        if !raw.ok() {
            if has_no_commits(&raw.stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::Git(raw.stderr.trim().to_string()));
        }
        let me = self.config_get("user.email").unwrap_or_default().to_lowercase();
        let mut map: HashMap<String, Contributor> = HashMap::new();
        for (commits, name, email) in parse::parse_shortlog(&raw.text()) {
            let key = email.to_lowercase();
            let e = map.entry(key.clone()).or_insert_with(|| Contributor { name, email, is_you: key == me, ..Default::default() });
            e.commits += commits;
        }
        let mut v: Vec<Contributor> = map.into_values().collect();
        v.sort_by(|a, b| b.commits.cmp(&a.commits).then_with(|| a.name.cmp(&b.name)));
        Ok(v)
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        let (Ok(a), Ok(b)) = (rev_arg(a), rev_arg(b)) else { return false };
        self.git.run_raw(Some(&self.root), &["merge-base", "--is-ancestor", a, b]).map(|o| o.ok()).unwrap_or(false)
    }
}

/// Stands between message terms in `LogQuery::grep`. Nobody types it, unlike `|`.
pub const GREP_TERMS: char = '\u{1f}';

/// Whether a path from git's config is this folder, however each was spelled.
fn same_folder(a: &str, b: &Path) -> bool {
    let plain = |p: &str| {
        let p = p.strip_prefix(r"\\?\").unwrap_or(p).replace('\\', "/");
        let p = p.trim_end_matches('/');
        if cfg!(windows) { p.to_lowercase() } else { p.to_string() }
    };
    plain(a) == plain(&b.to_string_lossy())
}

/// The commit git's verdict names. Its log says `# first 'bad' commit: [sha] subject`, or
/// without the quotes in older versions, or with the user's own word for "bad".
fn culprit_in(log: &str) -> Option<String> {
    log.lines().find_map(|l| {
        let rest = l.strip_prefix("# first ")?;
        let (_, after) = rest.split_once(" commit: [")?;
        after.split(']').next().map(String::from)
    })
}

/// A regular expression matching just this text.
fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Whether a command failed because the revision it was given can't exist yet, on a
/// branch with no commits. An ordinary state, and not worth a raw `fatal:`.
fn has_no_commits(stderr: &str) -> bool {
    ["does not have any commits", "bad default revision", "bad revision 'HEAD'", "unknown revision", "Not a valid object name HEAD", "ambiguous argument 'HEAD'"].iter().any(|m| stderr.contains(m))
}

/// A prepared message without git's note on what conflicted, the last thing in the file. Only
/// that goes: a line starting with `#` can be the subject, as in "#123 fix login". The note
/// starts with whatever `core.commentChar` is, which a `# Conflicts:` check would miss, and
/// that kept in the message. The lines after it are each a comment too, which the check
/// here uses so that a word in the message itself can't be taken for it.
fn without_conflict_notes(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let note = lines.iter().rposition(|l| l.strip_suffix(" Conflicts:").is_some_and(|c| !c.is_empty() && !c.contains(char::is_alphanumeric)));
    let kept = match note {
        Some(at) => {
            let mark = lines[at].strip_suffix(" Conflicts:").unwrap_or_default();
            if lines[at + 1..].iter().all(|l| l.starts_with(mark)) { &lines[..at] } else { &lines[..] }
        }
        None => &lines[..],
    };
    kept.join("\n").trim_end().to_string()
}

/// What to say when a folder holds a repository but no working tree to show. `None` if
/// that's not why git refused.
fn no_work_tree_message(git: &GitExe, path: &Path, stderr: &str) -> Option<String> {
    if !stderr.contains("must be run in a work tree") && !stderr.contains("cannot use bare repository") {
        return None;
    }
    if path.file_name().is_some_and(|n| n == ".git") {
        return Some(format!("{} is the .git folder inside a repository. Open the folder above it instead.", path.display()));
    }
    // Named outright, which is allowed where a bare repository found by looking isn't.
    let dir = format!("--git-dir={}", path.display());
    let trees = git.run_raw(Some(path), &[&dir, "worktree", "list", "--porcelain"]).ok().filter(|o| o.ok()).map(|o| parse::parse_worktrees(&o.text(), "")).unwrap_or_default();
    let trees: Vec<String> = trees.into_iter().filter(|w| !w.bare).map(|w| w.path).collect();
    let advice = if trees.is_empty() { "It has no linked worktree yet. Clone it, or add a worktree from a terminal, to work with its files.".to_string() } else { format!("Open one of its worktrees instead:\n{}", trees.join("\n")) };
    Some(format!("{} is a bare repository: it holds history but no working files for gitem to show. {advice}", path.display()))
}

/// Each side of a range counts, because the left one is what starts the argument.
fn check_source_revs(src: &DiffSource) -> Result<()> {
    match src {
        DiffSource::Commit(r) | DiffSource::WorkingVs(r) => {
            rev_arg(r)?;
        }
        DiffSource::Range(a, b) | DiffSource::MergeBase(a, b) => {
            rev_arg(a)?;
            rev_arg(b)?;
        }
        _ => {}
    }
    Ok(())
}

/// `git init` target check and repo discovery for the workspace scanner.
pub fn find_repos(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(p: &Path, depth: usize, max: usize, out: &mut Vec<PathBuf>) {
        if p.join(".git").exists() {
            out.push(p.to_path_buf());
            return;
        }
        if depth >= max {
            return;
        }
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name();
            let n = name.to_string_lossy();
            if path.is_dir() && !n.starts_with('.') && n != "node_modules" && n != "target" {
                walk(&path, depth + 1, max, out);
            }
        }
    }
    walk(root, 0, max_depth, &mut out);
    out
}

fn merge_numstat(files: &mut [FileChange], numstat: &str) {
    let stats: HashMap<String, (Option<u32>, Option<u32>)> = parse::parse_numstat(numstat).into_iter().map(|(p, a, d)| (p, (a, d))).collect();
    for f in files {
        if let Some((a, d)) = stats.get(&f.path) {
            f.additions = *a;
            f.deletions = *d;
        }
    }
}

/// GitLens-style search syntax: `message:` / `=:`, `author:` / `@:`, `commit:` / `#:`,
/// `file:` / `?:`, `change:` / `~:`, `after:`, `before:`, `is:merge`, `-is:merge`,
/// `ref:`. Bare words search the message, as plain text, and all of them must be there.
/// `re:` takes a regular expression, for anything else, such as `re:fix|feat`.
pub fn parse_search(input: &str) -> LogQuery {
    // Stash commits are how git stores them, not anybody's history.
    let mut q = LogQuery { regex_ignore_case: true, exclude_stash: true, fixed_strings: true, ..Default::default() };
    let mut msg: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut commits = false;
    let mut refs = false;
    for tok in tokenize(input) {
        let (k, v) = match tok.split_once(':') {
            Some((k, v)) if !v.is_empty() => (k.to_string(), v.to_string()),
            _ => (String::new(), tok.clone()),
        };
        match k.as_str() {
            "message" | "=" => msg.push(v),
            "author" | "@" => q.author = Some(v),
            "committer" => q.committer = Some(v),
            "-message" | "-=" => {
                msg.push(v);
                q.invert_grep = true;
            }
            "re" | "regex" => patterns.push(v),
            "commit" | "#" => {
                commits = true;
                q.revs.push(v);
            }
            "file" | "?" => q.paths.push(v),
            "change" | "~" => q.pickaxe_g = Some(v),
            "changes" | "s" => q.pickaxe_s = Some(v),
            "after" | "since" => q.since = Some(v),
            "before" | "until" => q.until = Some(v),
            "ref" => {
                refs = true;
                q.revs.push(v);
            }
            "is" | "type" if v == "merge" => q.merges_only = true,
            "" if v == "@me" => q.author = Some(v),
            "-is" if v == "merge" => q.no_merges = true,
            _ => msg.push(tok),
        }
    }
    // Git has one setting for the whole search, so with a pattern in it the words are escaped.
    if !patterns.is_empty() {
        q.fixed_strings = false;
        msg = msg.iter().map(|m| regex_escape(m)).chain(patterns).collect();
    }
    if !msg.is_empty() {
        // Words narrow a search down. With `-message:` it's commits that have none of them.
        q.all_match = msg.len() > 1 && !q.invert_grep;
        q.grep = Some(msg.join(&GREP_TERMS.to_string()));
    }
    // A commit asked for by name is that commit. With a branch too, the branch is the history.
    q.no_walk = commits && !refs;
    if q.revs.is_empty() {
        q.all = true;
    }
    q
}

fn tokenize(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    v.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_syntax() {
        let q = parse_search("author:ann file:src/a.rs \"fix bug\" change:foo after:2024-01-01 -is:merge");
        assert_eq!(q.author.as_deref(), Some("ann"));
        assert_eq!(q.paths, vec!["src/a.rs"]);
        assert_eq!(q.grep.as_deref(), Some("fix bug"));
        assert_eq!(q.pickaxe_g.as_deref(), Some("foo"));
        assert_eq!(q.since.as_deref(), Some("2024-01-01"));
        assert!(q.no_merges);
        assert!(q.all);
    }

    #[test]
    fn search_shorthand() {
        let q = parse_search("@:bob =:hello #:abc123");
        assert_eq!(q.author.as_deref(), Some("bob"));
        assert_eq!(q.grep.as_deref(), Some("hello"));
        assert_eq!(q.revs, vec!["abc123"]);
        assert!(!q.all);
        assert!(q.no_walk, "a commit by name is that commit alone");
    }

    #[test]
    fn search_words_are_plain_text_and_all_required() {
        let q = parse_search("fix(auth) C++");
        assert!(q.fixed_strings && q.all_match);
        assert_eq!(q.grep.as_deref().unwrap().split(GREP_TERMS).collect::<Vec<_>>(), ["fix(auth)", "C++"]);
        assert!(q.exclude_stash, "stash commits aren't history");

        let q = parse_search("fix(auth) re:feat|chore");
        assert!(!q.fixed_strings, "a pattern means regular expressions");
        assert_eq!(q.grep.as_deref().unwrap().split(GREP_TERMS).collect::<Vec<_>>(), [r"fix\(auth\)", "feat|chore"], "the plain word stays plain");

        let q = parse_search("-message:wip -message:temp");
        assert!(q.invert_grep && !q.all_match, "none of them, not just not all of them");
    }

    #[test]
    fn a_branch_search_walks_history() {
        assert!(!parse_search("ref:main").no_walk);
        assert!(!parse_search("ref:main #:abc").no_walk);
    }

    #[test]
    fn bisect_verdicts_are_read_from_the_log() {
        assert_eq!(culprit_in("# bad: [aaa] x\n# first 'bad' commit: [abc123] the bug\n").as_deref(), Some("abc123"));
        assert_eq!(culprit_in("# first bad commit: [def] old git\n").as_deref(), Some("def"));
        assert_eq!(culprit_in("# first 'broken' commit: [fed] own words\n").as_deref(), Some("fed"));
        assert_eq!(culprit_in("# possible first 'bad' commit: [abc] only skipped\n"), None);
        assert_eq!(culprit_in("git bisect good abc\n"), None);
    }
}
