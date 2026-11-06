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
    pub fn open(git: GitExe, path: &Path) -> Result<Self> {
        let o = git.run(Some(path), &["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"])?;
        let text = o.text();
        let mut lines = text.lines();
        let root = PathBuf::from(lines.next().unwrap_or_default());
        let git_dir = PathBuf::from(lines.next().unwrap_or_default());
        let common = lines.next().unwrap_or_default();
        let common_dir = if Path::new(common).is_absolute() { PathBuf::from(common) } else { path.join(common) };
        Ok(Self { git, root, git_dir, common_dir })
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

    // ---- status ------------------------------------------------------------

    pub fn status(&self) -> Result<RepoStatus> {
        let o = self.run(&["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"])?;
        let mut st = parse::parse_status(&o.text());
        if st.head_sha == "(initial)" {
            st.head_sha.clear();
        }
        self.fill_op(&mut st);
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
        } else if g.join("BISECT_LOG").exists() {
            st.op = RepoOp::Bisect;
        }
    }

    /// Identifies the conflict's three versions. Empty once `path` is resolved.
    pub fn unmerged_signature(&self, path: &str) -> String {
        let args: Vec<String> = ["ls-files", "-u", "--", path].iter().map(|s| s.to_string()).collect();
        self.run_literal(&args).map(|o| o.text()).unwrap_or_default()
    }

    /// Full messages of the commits on HEAD after `since`, oldest first. An empty `since`
    /// means all of them, for a branch that had no commits.
    pub fn messages_since(&self, since: &str) -> Result<Vec<String>> {
        let range = if since.is_empty() { "HEAD".to_string() } else { format!("{since}..HEAD") };
        let out = self.run(&["log", "--reverse", "--format=%B%x00", &range])?.text();
        Ok(out.split('\0').map(str::trim).filter(|m| !m.is_empty()).map(String::from).collect())
    }

    /// Message git prepared for the in-progress merge/revert/cherry-pick.
    pub fn prepared_message(&self) -> Option<String> {
        std::fs::read_to_string(self.git_dir.join("MERGE_MSG")).ok().map(|s| {
            s.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n").trim_end().to_string()
        })
    }

    // ---- log ---------------------------------------------------------------

    pub fn log(&self, q: &LogQuery) -> Result<Vec<Commit>> {
        let mut a: Vec<String> = vec!["log".into(), parse::LOG_FORMAT.into(), "--decorate=short".into()];
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
            if q.all_match {
                a.extend(x.split('|').map(|t| format!("--grep={t}")));
            } else {
                a.push(format!("--grep={x}"));
            }
            a.push(if q.fixed_strings { "--fixed-strings".into() } else { "--extended-regexp".into() });
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
        a.push("--date-order".into());
        a.extend(q.revs.iter().cloned());
        if !q.paths.is_empty() && q.line_range.is_none() {
            a.push("--".into());
            a.extend(q.paths.iter().cloned());
        }
        let o = if q.literal_paths { self.run_literal(&a)? } else { self.run_raw_v(&a)? };
        if !o.ok() {
            // An unborn branch simply has no commits yet.
            let unborn_head = q.revs.iter().any(|r| r == "HEAD") && o.stderr.contains("unknown revision");
            if unborn_head || o.stderr.contains("does not have any commits") || o.stderr.contains("bad default revision") {
                return Ok(Vec::new());
            }
            return Err(Error::Git(o.stderr.trim().to_string()));
        }
        let mut commits = parse::parse_log(&o.text());
        let remotes = self.remote_names();
        parse::mark_remote_refs(&mut commits, &remotes);
        Ok(commits)
    }

    pub fn commit(&self, rev: &str) -> Result<Commit> {
        let q = LogQuery { revs: vec![rev.to_string()], limit: 1, ..Default::default() };
        self.log(&q)?.into_iter().next().ok_or_else(|| Error::Git(format!("no such commit: {rev}")))
    }

    fn remote_names(&self) -> Vec<String> {
        self.run(&["remote"]).map(|o| o.text().lines().map(String::from).collect()).unwrap_or_default()
    }

    pub fn signature(&self, rev: &str) -> Result<SignatureInfo> {
        let o = self.run(&["log", "-1", "--format=%G?%x00%GS%x00%GK", rev])?;
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
        let lines = |o: GitOutput| -> Vec<String> { o.text().lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
        let b = lines(self.run(&["for-each-ref", "--format=%(refname:short)", "--contains", sha, "refs/heads", "refs/remotes"])?);
        let t = lines(self.run(&["for-each-ref", "--format=%(refname:short)", "--contains", sha, "refs/tags"])?);
        Ok((b, t))
    }

    pub fn commit_note(&self, sha: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["notes", "show", sha]).ok().filter(|o| o.ok()).map(|o| o.text())
    }

    pub fn describe(&self, sha: &str) -> Option<String> {
        self.git.run_raw(Some(&self.root), &["describe", "--tags", "--always", sha]).ok().filter(|o| o.ok()).map(|o| o.text().trim().to_string())
    }

    pub fn resolve(&self, rev: &str) -> Result<String> {
        Ok(self.run(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])?.text().trim().to_string())
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        Ok(self.run(&["merge-base", a, b])?.text().trim().to_string())
    }

    /// (a-only, b-only) commit counts.
    pub fn ahead_behind(&self, a: &str, b: &str) -> Result<(u32, u32)> {
        let o = self.run(&["rev-list", "--left-right", "--count", &format!("{a}...{b}")])?.text();
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
        let o = self.git.run_raw(Some(&self.root), &["submodule", "status"])?;
        if !o.ok() {
            return Ok(Vec::new());
        }
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
        let o = self.git.run_raw(Some(&self.root), &["reflog", "show", parse::REFLOG_FORMAT, &format!("--max-count={limit}"), rev])?;
        if !o.ok() {
            return Ok(Vec::new());
        }
        Ok(parse::parse_reflog(&o.text()))
    }

    // ---- files & diffs -----------------------------------------------------

    pub fn file_at(&self, rev: &str, path: &str) -> Result<Vec<u8>> {
        Ok(self.run(&["show", &format!("{rev}:{path}")])?.stdout)
    }

    /// Conflict stage content: 1 base, 2 ours, 3 theirs. Missing stage gives empty.
    pub fn stage_content(&self, stage: u8, path: &str) -> Vec<u8> {
        self.git.run_raw(Some(&self.root), &["show", &format!(":{stage}:{path}")]).ok().filter(|o| o.ok()).map(|o| o.stdout).unwrap_or_default()
    }

    pub fn worktree_file(&self, path: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.root.join(path))?)
    }

    pub fn tracked_files(&self, rev: Option<&str>) -> Result<Vec<String>> {
        let o = match rev {
            Some(r) => self.run(&["ls-tree", "-r", "--name-only", "-z", r])?,
            None => self.run(&["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?,
        };
        Ok(o.text().split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
    }

    pub fn commit_files(&self, sha: &str) -> Result<Vec<FileChange>> {
        let ns = self.run(&["show", "--format=", "-M", "-m", "--first-parent", "--name-status", "-z", sha])?;
        let mut files = parse::parse_name_status(&ns.text());
        let st = self.run(&["show", "--format=", "-M", "-m", "--first-parent", "--numstat", "-z", sha])?;
        merge_numstat(&mut files, &st.text());
        Ok(files)
    }

    pub fn compare_files(&self, a: &str, b: &str, three_dot: bool) -> Result<Vec<FileChange>> {
        let spec = if three_dot { format!("{a}...{b}") } else { format!("{a}..{b}") };
        let ns = self.run(&["diff", "-M", "--name-status", "-z", &spec])?;
        let mut files = parse::parse_name_status(&ns.text());
        let st = self.run(&["diff", "-M", "--numstat", "-z", &spec])?;
        merge_numstat(&mut files, &st.text());
        Ok(files)
    }

    pub fn diff(&self, src: &DiffSource, path: Option<&str>, opts: &DiffOpts) -> Result<Vec<FileDiff>> {
        let mut a: Vec<String> = Vec::new();
        let add_path = |a: &mut Vec<String>| {
            if let Some(p) = path {
                a.push("--".into());
                a.push(p.to_string());
            }
        };
        match src {
            DiffSource::Unstaged => {
                a.push("diff".into());
                a.extend(opts.args());
                add_path(&mut a);
            }
            DiffSource::Staged => {
                a.push("diff".into());
                a.push("--cached".into());
                a.extend(opts.args());
                add_path(&mut a);
            }
            DiffSource::WorkingVsHead => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push("HEAD".into());
                add_path(&mut a);
            }
            DiffSource::Commit(sha) => {
                a.extend(["show".to_string(), "--format=".into(), "-m".into(), "--first-parent".into()]);
                a.extend(opts.args());
                a.push(sha.clone());
                add_path(&mut a);
            }
            DiffSource::Range(x, y) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(format!("{x}..{y}"));
                add_path(&mut a);
            }
            DiffSource::MergeBase(x, y) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(format!("{x}...{y}"));
                add_path(&mut a);
            }
            DiffSource::WorkingVs(r) => {
                a.push("diff".into());
                a.extend(opts.args());
                a.push(r.clone());
                add_path(&mut a);
            }
            DiffSource::Untracked => {
                a.push("diff".into());
                a.push("--no-index".into());
                a.extend(opts.args());
                a.push("--".into());
                a.push("/dev/null".into());
                a.push(path.unwrap_or_default().to_string());
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

    /// Full textual diff, used for AI prompts and patch export.
    pub fn diff_text(&self, src: &DiffSource) -> Result<String> {
        let opts = DiffOpts::default();
        let args: Vec<String> = match src {
            DiffSource::Staged => vec!["diff".into(), "--cached".into(), format!("-U{}", opts.context)],
            DiffSource::Unstaged => vec!["diff".into(), format!("-U{}", opts.context)],
            DiffSource::WorkingVsHead => vec!["diff".into(), "HEAD".into()],
            DiffSource::Commit(s) => vec!["show".into(), "--format=".into(), "-m".into(), "--first-parent".into(), s.clone()],
            DiffSource::Range(a, b) => vec!["diff".into(), format!("{a}..{b}")],
            DiffSource::MergeBase(a, b) => vec!["diff".into(), format!("{a}...{b}")],
            DiffSource::WorkingVs(r) => vec!["diff".into(), r.clone()],
            _ => return Err(Error::Other("unsupported diff source".into())),
        };
        Ok(self.run_v(&args)?.text())
    }

    /// The changes as a patch that applies back exactly: binary files included, bytes as
    /// they are whatever the encoding, and for working changes, new untracked files too.
    /// That's what someone expects to get back after discarding their work.
    pub fn patch_bytes(&self, src: &DiffSource) -> Result<Vec<u8>> {
        let flags = ["--binary".to_string(), "--full-index".to_string()];
        let mut args: Vec<String> = match src {
            DiffSource::Staged => vec!["diff".into(), "--cached".into()],
            DiffSource::Unstaged => vec!["diff".into()],
            DiffSource::WorkingVsHead => vec!["diff".into()],
            DiffSource::Commit(s) => vec!["show".into(), "--format=".into(), "-m".into(), "--first-parent".into()].into_iter().chain(flags.iter().cloned()).chain([s.clone()]).collect(),
            DiffSource::Range(a, b) => vec!["diff".into(), format!("{a}..{b}")],
            DiffSource::MergeBase(a, b) => vec!["diff".into(), format!("{a}...{b}")],
            DiffSource::WorkingVs(r) => vec!["diff".into(), r.clone()],
            _ => return Err(Error::Other("unsupported diff source".into())),
        };
        if !matches!(src, DiffSource::Commit(_)) {
            args.extend(flags);
        }
        if *src == DiffSource::WorkingVsHead {
            args.push("HEAD".into());
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
            a.push(r.to_string());
        }
        a.push("--".into());
        a.push(path.to_string());
        Ok(parse::parse_blame(&self.run_v(&a)?.text()))
    }

    // ---- history & stats ---------------------------------------------------

    /// All commits touching a path, following renames, with line stats.
    pub fn file_history_points(&self, path: &str, limit: usize) -> Result<Vec<FileHistoryPoint>> {
        let o = self
            .run_literal(&[
                "log".into(),
                parse::STAT_LOG_FORMAT.into(),
                "--numstat".into(),
                "--follow".into(),
                format!("--max-count={limit}"),
                "--".into(),
                path.to_string(),
            ])?
            .into_result()?;
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
        let mut a: Vec<String> = vec!["log".into(), parse::STAT_LOG_FORMAT.into(), "--numstat".into(), format!("--max-count={limit}"), rev.to_string()];
        if let Some(p) = path {
            a.push("--".into());
            a.push(p.to_string());
        }
        let o = self.run_literal(&a)?.into_result()?;
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
        a.extend(revs.iter().cloned());
        let o = self.run_raw_v(&a)?;
        Ok(parse::parse_commit_stats(&o.text()))
    }

    /// Files that would conflict if `a` and `b` merged, found without touching
    /// the working tree. Only adds unreferenced objects, which `gc` removes.
    pub fn predict_conflicts(&self, a: &str, b: &str) -> Result<Vec<String>> {
        let o = self.git.run_raw(Some(&self.root), &["merge-tree", "--write-tree", "--name-only", "--no-messages", a, b])?;
        match o.code {
            0 => Ok(Vec::new()),
            1 => Ok(o.text().lines().skip(1).filter(|l| !l.is_empty()).map(String::from).collect()),
            _ => Err(Error::Git(o.stderr.trim().to_string())),
        }
    }

    /// (path, size in bytes) of every file at `rev`.
    pub fn file_sizes(&self, rev: &str) -> Result<Vec<(String, u64)>> {
        let o = self.run(&["ls-tree", "-r", "-l", "-z", rev])?;
        Ok(parse::parse_ls_tree_sizes(&o.text()))
    }

    /// (path, commits touching it) over recent history.
    pub fn file_activity(&self, rev: &str, limit: usize) -> Result<Vec<(String, u64)>> {
        let o = self.run(&["log", "--format=", "--name-only", &format!("--max-count={limit}"), rev])?;
        let mut m: HashMap<String, u64> = HashMap::new();
        for l in o.text().lines().filter(|l| !l.is_empty()) {
            *m.entry(l.to_string()).or_default() += 1;
        }
        Ok(m.into_iter().collect())
    }

    /// How well this repository is tuned for its size.
    pub fn health(&self) -> Result<RepoHealth> {
        let is_on = |k: &str| self.config_get(k).is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1"));
        let tracked = self.run(&["ls-files", "-z"])?.stdout.iter().filter(|b| **b == 0).count();
        let commits: u64 = self.git.run_raw(Some(&self.root), &["rev-list", "--count", "--all"])?.text().trim().parse().unwrap_or(0);
        let stats = self.count_objects()?;
        let stat = |key: &str| -> String { stats.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim_start_matches(':').trim().to_string())).unwrap_or_default() };
        let items = vec![
            HealthItem { label: "Commit-graph cache".into(), detail: "Speeds up history walks and the commit graph.".into(), config: vec![("core.commitGraph".into(), "true".into()), ("fetch.writeCommitGraph".into(), "true".into())], enabled: is_on("fetch.writeCommitGraph") },
            HealthItem { label: "Untracked cache".into(), detail: "Speeds up status by remembering untracked folders.".into(), config: vec![("core.untrackedCache".into(), "true".into())], enabled: is_on("core.untrackedCache") },
            HealthItem { label: "File system monitor".into(), detail: "Lets status skip scanning files that didn't change.".into(), config: vec![("core.fsmonitor".into(), "true".into())], enabled: is_on("core.fsmonitor") },
            HealthItem { label: "Large-repository index".into(), detail: "Index format for repositories with many files.".into(), config: vec![("feature.manyFiles".into(), "true".into())], enabled: is_on("feature.manyFiles") },
            HealthItem { label: "Sparse index".into(), detail: "Smaller index when using sparse checkout.".into(), config: vec![("index.sparse".into(), "true".into())], enabled: is_on("index.sparse") },
            HealthItem { label: "Scheduled maintenance".into(), detail: "Git keeps this repository optimized in the background.".into(), config: vec![("maintenance.auto".into(), "true".into()), ("maintenance.strategy".into(), "incremental".into())], enabled: self.config_get("maintenance.strategy").is_some() },
        ];
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

    /// A diff prepared for an AI request, honouring ignore rules.
    pub fn diff_for_ai(&self, src: &DiffSource) -> Result<String> {
        Ok(crate::ai::filter_diff(&self.diff_text(src)?, &self.ai_ignore_patterns()))
    }

    pub fn contributors(&self, rev: &str, with_stats: bool) -> Result<Vec<Contributor>> {
        let mut a: Vec<String> = vec!["log".into(), parse::STAT_LOG_FORMAT.into(), rev.to_string()];
        if with_stats {
            a.push("--numstat".into());
        }
        let raw = self.run_raw_v(&a)?;
        if !raw.ok() {
            return Ok(Vec::new());
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

    /// Commit timestamps for activity heatmaps.
    pub fn commit_times(&self, rev: &str, author: Option<&str>, since: &str) -> Result<Vec<i64>> {
        let mut a: Vec<String> = vec!["log".into(), "--format=%at".into(), format!("--since={since}"), rev.to_string()];
        if let Some(x) = author {
            a.push(format!("--author={x}"));
        }
        let o = self.run_raw_v(&a)?;
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
        let dir = self.config_get("core.hooksPath").map(|p| self.root.join(p)).unwrap_or_else(|| self.common_dir.join("hooks"));
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

    pub fn lfs_available(&self) -> bool {
        self.git.run_raw(None, &["lfs", "version"]).map(|o| o.ok()).unwrap_or(false)
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
        Ok(o.text().lines().filter_map(|l| l.strip_prefix("Would remove ").map(String::from)).collect())
    }

    pub fn count_objects(&self) -> Result<String> {
        Ok(self.run(&["count-objects", "-vH"])?.text())
    }

    pub fn ignore_path(&self) -> PathBuf {
        self.root.join(".gitignore")
    }

    pub fn check_ignore(&self, path: &str) -> Option<String> {
        let o = self.git.run_raw(Some(&self.root), &["check-ignore", "-v", path]).ok()?;
        o.ok().then(|| o.text().trim().to_string())
    }

    /// Next commit that touched `path` after `sha` on the way to HEAD (for
    /// walking a file's revisions forward).
    pub fn next_revision(&self, path: &str, sha: &str) -> Option<String> {
        let args: Vec<String> = ["rev-list", "--reverse", "--ancestry-path", &format!("{sha}..HEAD"), "--", path].iter().map(|s| s.to_string()).collect();
        let o = self.run_literal(&args).ok()?;
        o.text().lines().next().map(String::from)
    }

    /// Previous commit that touched `path` before `sha`, with the file's name
    /// there, since it may have been renamed.
    pub fn prev_revision(&self, path: &str, sha: &str) -> Option<(String, String)> {
        let full = self.resolve(sha).ok()?;
        let args: Vec<String> = ["log", "--follow", "--format=%x01%H", "--name-only", "--max-count=3", &full, "--", path].iter().map(|s| s.to_string()).collect();
        let o = self.run_literal(&args).ok()?;
        let text = o.text();
        let mut records = text.split('\x01').filter(|r| !r.trim().is_empty());
        records.find_map(|r| {
            let mut lines = r.lines().filter(|l| !l.trim().is_empty());
            let found = lines.next()?.trim();
            let name = lines.next()?.trim();
            (found != full).then(|| (found.to_string(), name.to_string()))
        })
    }

    /// Default base branch: origin/HEAD, else main/master/develop if present.
    pub fn default_branch(&self) -> Option<String> {
        if let Ok(o) = self.git.run_raw(Some(&self.root), &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            && o.ok() {
                return Some(o.text().trim().to_string());
            }
        for c in ["main", "master", "develop", "trunk"] {
            if self.git.run_raw(Some(&self.root), &["rev-parse", "--verify", "--quiet", c]).map(|o| o.ok()).unwrap_or(false) {
                return Some(c.to_string());
            }
        }
        None
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.git.run_raw(Some(&self.root), &["merge-base", "--is-ancestor", a, b]).map(|o| o.ok()).unwrap_or(false)
    }
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
/// `ref:`. Bare words search the message.
pub fn parse_search(input: &str) -> LogQuery {
    let mut q = LogQuery { regex_ignore_case: true, ..Default::default() };
    let mut msg: Vec<String> = Vec::new();
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
            "commit" | "#" => q.revs.push(v),
            "file" | "?" => q.paths.push(v),
            "change" | "~" => q.pickaxe_g = Some(v),
            "changes" | "s" => q.pickaxe_s = Some(v),
            "after" | "since" => q.since = Some(v),
            "before" | "until" => q.until = Some(v),
            "ref" => q.revs.push(v),
            "is" | "type" if v == "merge" => q.merges_only = true,
            "" if v == "@me" => q.author = Some(v),
            "-is" if v == "merge" => q.no_merges = true,
            _ => msg.push(tok),
        }
    }
    if !msg.is_empty() {
        q.grep = Some(msg.join("|"));
    }
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
    }
}
