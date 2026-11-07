//! Per-repository cached state, refreshed in the background and invalidated by
//! a filesystem watcher. Views compare generation counters to know when to reload.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gitem_core::Repo;
use gitem_core::forge::{self, RemoteRepo};
use gitem_core::model::*;
use gitem_core::settings::CustomHost;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;
use notify::Watcher;

use crate::task::Task;

#[derive(Default, Clone)]
pub struct Refs {
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub remotes: Vec<Remote>,
    pub stashes: Vec<Stash>,
    pub worktrees: Vec<Worktree>,
    pub submodules: Vec<Submodule>,
    pub default_branch: Option<String>,
    /// What couldn't be read, with why. One failure doesn't cost the other lists.
    pub problems: Vec<String>,
}

impl Refs {
    fn read(r: &Repo) -> gitem_core::Result<Self> {
        let (branches, tags, remotes, worktrees) = (r.branches(), r.tags(), r.remotes(), r.worktrees());
        // With nothing readable at all, the repository is what's wrong, not these lists.
        if let (Err(e), Err(_), Err(_), Err(_)) = (&branches, &tags, &remotes, &worktrees) {
            return Err(gitem_core::Error::Other(e.to_string()));
        }
        let mut problems = Vec::new();
        let mut keep = |what: &str, e: gitem_core::Error| problems.push(format!("Couldn't read {what}: {e}"));
        let branches = branches.unwrap_or_else(|e| {
            keep("branches", e);
            Vec::new()
        });
        let tags = tags.unwrap_or_else(|e| {
            keep("tags", e);
            Vec::new()
        });
        let remotes = remotes.unwrap_or_else(|e| {
            keep("remotes", e);
            Vec::new()
        });
        let worktrees = worktrees.unwrap_or_else(|e| {
            keep("worktrees", e);
            Vec::new()
        });
        let stashes = r.stashes().unwrap_or_else(|e| {
            keep("stashes", e);
            Vec::new()
        });
        let default_branch = r.default_branch_among(&branches);
        Ok(Refs { branches, tags, remotes, stashes, worktrees, submodules: r.submodules().unwrap_or_default(), default_branch, problems })
    }

    fn signature(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for b in &self.branches {
            (&b.name, &b.sha, b.ahead, b.behind).hash(&mut h);
        }
        for t in &self.tags {
            (&t.name, &t.sha).hash(&mut h);
        }
        for t in &self.stashes {
            t.sha.hash(&mut h);
        }
        for w in &self.worktrees {
            (&w.path, &w.head).hash(&mut h);
        }
        h.finish()
    }
}

fn status_signature(s: &RepoStatus) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}{:?}{:?}{:?}", s.head_sha, s.branch, s.entries, s.op).hash(&mut h);
    h.finish()
}

/// How long git may take before the window says it's still waiting.
pub const STILL_WAITING: Duration = Duration::from_secs(20);

/// Files in each list of the status. Worked out once per status: it can be very long.
#[derive(Default, Clone, Copy)]
pub struct StatusCounts {
    pub staged: usize,
    pub unstaged: usize,
    pub conflicts: usize,
}

#[derive(Default)]
pub struct RepoData {
    pub status: Task<Arc<RepoStatus>>,
    pub refs: Task<Arc<Refs>>,
    /// Bumped when branches, tags, or stashes change.
    pub refs_gen: u64,
    /// Bumped when working-tree or index state changes.
    pub status_gen: u64,
    /// Bumped only when what `status` reports changed, which any change to the set of files
    /// does. Editing a file that's already modified doesn't, so a list of files can use this.
    pub listing_gen: u64,
    signature: Arc<AtomicU64>,
    last_signature: u64,
    status_sig: Arc<AtomicU64>,
    last_status_sig: u64,
    counts: std::cell::Cell<(u64, StatusCounts)>,
    pub forge_repo: Option<RemoteRepo>,
    pub forge_remote: Option<String>,
    pub custom_hosts: Vec<CustomHost>,
    pub last_blame: String,
    /// Writes are queued or running on this repository.
    pub ops_busy: bool,
    /// Long jobs running outside the op queue, such as an auto-rebase. They count as busy.
    pub jobs: Arc<AtomicUsize>,
    pub branch_pr: Option<(u64, String)>,
    /// Whether drafts may be written to disk. Off for tests and screenshots.
    pub persist: bool,
    /// How long the last status took, to notice when the index needs refreshing.
    pub status_took: std::cell::Cell<Duration>,
    refs_again: bool,
    status_again: bool,
}

impl RepoData {
    pub fn refresh(&mut self, ctx: &egui::Context, repo: &Repo) {
        self.refresh_status(ctx, repo);
        // Restarting would discard the running query, and a steady stream of
        // changes could then starve it forever. Finish it, then run once more.
        if self.refs.busy() {
            self.refs_again = true;
            return;
        }
        let sig = self.signature.clone();
        self.refs.spawn_git(ctx, repo, move |r| -> gitem_core::Result<Arc<Refs>> {
            let refs = Refs::read(&r)?;
            sig.store(refs.signature(), Ordering::Relaxed);
            Ok(Arc::new(refs))
        });
    }

    pub fn refresh_status(&mut self, ctx: &egui::Context, repo: &Repo) {
        if self.status.busy() {
            self.status_again = true;
            return;
        }
        let sig = self.status_sig.clone();
        self.status.spawn_git(ctx, repo, move |r| -> gitem_core::Result<Arc<RepoStatus>> {
            let st = r.status()?;
            sig.store(status_signature(&st), Ordering::Relaxed);
            Ok(Arc::new(st))
        });
    }

    pub fn poll(&mut self, ctx: &egui::Context, repo: &Repo) {
        let status_ran_for = self.status.running_for();
        if self.status.poll() {
            self.status_took.set(status_ran_for.unwrap_or_default());
            if std::mem::take(&mut self.status_again) {
                self.refresh_status(ctx, repo);
            }
            if self.status.error.is_none() {
                let sig = self.status_sig.load(Ordering::Relaxed);
                if sig != self.last_status_sig {
                    self.last_status_sig = sig;
                    self.status_gen += 1;
                    self.listing_gen += 1;
                }
            }
        }
        if self.refs.poll() {
            if std::mem::take(&mut self.refs_again) {
                self.refresh(ctx, repo);
            }
            if self.refs.error.is_none() {
                let sig = self.signature.load(Ordering::Relaxed);
                if sig != self.last_signature {
                    self.last_signature = sig;
                    self.refs_gen += 1;
                }
            }
            self.recompute_forge();
        }
    }

    /// What git hasn't answered for a while, and how long it's been. Nothing can show this
    /// by itself: a hung read leaves the last good answer on screen.
    pub fn waiting_on(&self) -> Option<(&'static str, Duration)> {
        [("git status", self.status.running_for()), ("reading branches and tags", self.refs.running_for())].into_iter().find_map(|(what, t)| t.filter(|t| *t >= STILL_WAITING).map(|t| (what, t)))
    }

    /// Gives up on whatever is running and asks again.
    pub fn retry(&mut self, ctx: &egui::Context, repo: &Repo) {
        self.status.stop();
        self.refs.stop();
        self.status_again = false;
        self.refs_again = false;
        self.refresh(ctx, repo);
    }

    /// What's wrong, if git couldn't read this repository at the last attempt.
    pub fn read_failure(&self) -> Option<&str> {
        self.status.error.as_deref().or(self.refs.error.as_deref())
    }

    /// Whether the lists below have had an answer yet. An empty list before that says
    /// nothing, so it mustn't look like a repository with nothing in it.
    pub fn refs_known(&self) -> bool {
        self.refs.get().is_some()
    }

    pub fn counts(&self) -> StatusCounts {
        let (at, known) = self.counts.get();
        if at == self.status_gen + 1 {
            return known;
        }
        let mut c = StatusCounts::default();
        if let Some(s) = self.status.get() {
            for e in &s.entries {
                c.staged += e.is_staged() as usize;
                c.unstaged += e.is_unstaged() as usize;
                c.conflicts += e.conflict.is_some() as usize;
            }
        }
        self.counts.set((self.status_gen + 1, c));
        c
    }

    /// Files or the index changed in a way the status summary may not show: a
    /// second edit to a modified file looks identical there. Views must reload.
    pub fn mark_changed(&mut self) {
        self.status_gen += 1;
    }

    /// Anything that isn't a plain read may have changed the set of files.
    pub fn mark_rewritten(&mut self) {
        self.status_gen += 1;
        self.listing_gen += 1;
    }

    pub fn recompute_forge(&mut self) {
        let Some(r) = self.refs.get() else { return };
        let pick = r.remotes.iter().find(|m| m.name == "origin").or(r.remotes.first());
        self.forge_repo = pick.and_then(|m| forge::identify(&m.fetch_url, &self.custom_hosts));
        self.forge_remote = pick.map(|m| m.name.clone());
    }

    /// Default base branch, short name without a remote prefix.
    pub fn base_branch(&self) -> String {
        self.refs.get().and_then(|r| r.default_branch.as_deref()).map(|d| d.rsplit('/').next().unwrap_or(d).to_string()).unwrap_or_else(|| "main".to_string())
    }

    pub fn forge(&self) -> Option<&RemoteRepo> {
        self.forge_repo.as_ref()
    }

    pub fn branches(&self) -> &[Branch] {
        self.refs.get().map(|r| r.branches.as_slice()).unwrap_or(&[])
    }

    pub fn local_branches(&self) -> impl Iterator<Item = &Branch> {
        self.branches().iter().filter(|b| !b.is_remote)
    }

    pub fn remote_branches(&self) -> impl Iterator<Item = &Branch> {
        self.branches().iter().filter(|b| b.is_remote)
    }

    pub fn tags(&self) -> &[Tag] {
        self.refs.get().map(|r| r.tags.as_slice()).unwrap_or(&[])
    }

    pub fn remotes(&self) -> &[Remote] {
        self.refs.get().map(|r| r.remotes.as_slice()).unwrap_or(&[])
    }

    pub fn stashes(&self) -> &[Stash] {
        self.refs.get().map(|r| r.stashes.as_slice()).unwrap_or(&[])
    }

    pub fn worktrees(&self) -> &[Worktree] {
        self.refs.get().map(|r| r.worktrees.as_slice()).unwrap_or(&[])
    }

    pub fn submodules(&self) -> &[Submodule] {
        self.refs.get().map(|r| r.submodules.as_slice()).unwrap_or(&[])
    }

    /// All ref names for pickers: local, remote, then tags.
    pub fn ref_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.local_branches().map(|b| b.name.clone()).collect();
        v.extend(self.remote_branches().map(|b| b.name.clone()));
        v.extend(self.tags().iter().map(|t| t.name.clone()));
        v
    }
}

/// The ignore rules git applies in a work tree: `.gitignore` in each folder, then
/// `info/exclude` and the user's own excludes file. Used to tell a build's churn from a
/// change anyone can see, so it has to run git no more than once.
struct IgnoreRules {
    root: PathBuf,
    /// The lowest priority: `info/exclude` and the user's excludes file.
    outer: Gitignore,
    git_dir: PathBuf,
    git: gitem_core::GitExe,
    outer_loaded: bool,
    by_dir: HashMap<PathBuf, Option<Gitignore>>,
}

impl IgnoreRules {
    fn new(root: &Path, git_dir: &Path, git: gitem_core::GitExe) -> Self {
        Self { root: root.to_path_buf(), outer: Gitignore::empty(), git_dir: git_dir.to_path_buf(), git, outer_loaded: false, by_dir: HashMap::new() }
    }

    fn forget(&mut self) {
        self.by_dir.clear();
        self.outer_loaded = false;
    }

    fn load_outer(&mut self) {
        self.outer_loaded = true;
        let mut b = GitignoreBuilder::new(&self.root);
        b.add(self.git_dir.join("info/exclude"));
        let configured = self.git.run_raw(Some(&self.root), &["config", "--type=path", "--get", "core.excludesFile"]).ok().filter(|o| o.ok()).map(|o| PathBuf::from(o.text().trim()));
        let default = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join(".config"))).map(|d| d.join("git/ignore"));
        if let Some(f) = configured.or(default) {
            b.add(f);
        }
        self.outer = b.build().unwrap_or_else(|_| Gitignore::empty());
    }

    fn rules_in(&mut self, dir: &Path) -> Option<&Gitignore> {
        self.by_dir
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let file = dir.join(".gitignore");
                file.is_file().then(|| {
                    let mut b = GitignoreBuilder::new(dir);
                    b.add(file);
                    b.build().ok()
                })?
            })
            .as_ref()
    }

    /// Whether git would ignore this path if it were untracked. A folder that's ignored takes
    /// everything in it along, as in git, whatever a deeper rule says.
    fn is_ignored(&mut self, path: &Path) -> bool {
        if !self.outer_loaded {
            self.load_outer();
        }
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        let leaf_is_dir = path.is_dir();
        let names: Vec<_> = rel.components().collect();
        let mut at = self.root.clone();
        for (i, name) in names.iter().enumerate() {
            at.push(name);
            let is_dir = i + 1 < names.len() || leaf_is_dir;
            let dirs: Vec<PathBuf> = at.ancestors().skip(1).take(i + 1).map(Path::to_path_buf).collect();
            let mut verdict = Match::None;
            for dir in dirs {
                verdict = self.rules_in(&dir).map_or(Match::None, |g| g.matched(&at, is_dir).map(|_| ()));
                if !verdict.is_none() {
                    break;
                }
            }
            if verdict.is_none() {
                verdict = self.outer.matched(&at, is_dir).map(|_| ());
            }
            if verdict.is_ignore() {
                return true;
            }
        }
        false
    }
}

/// What a change inside a git directory can have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitChange {
    /// Nothing anyone can see: objects, reflogs, locks, a commit message being written.
    Nothing,
    /// The index or an operation in progress. Status covers those.
    Status,
    /// What the branch lists show.
    Refs,
}

/// `rest` is the path inside the git directory. A full refresh of the refs runs several
/// commands, so it's for what can actually change them.
pub fn classify_git_path(rest: &str) -> GitChange {
    if rest.ends_with(".lock") {
        return GitChange::Nothing;
    }
    let first = rest.split('/').next().unwrap_or(rest);
    match first {
        "HEAD" | "refs" | "packed-refs" | "reftable" | "config" | "config.worktree" | "shallow" => GitChange::Refs,
        "worktrees" if rest.ends_with("/HEAD") || rest.ends_with("/gitdir") || rest.ends_with("/locked") || rest == "worktrees" => GitChange::Refs,
        "index" | "sharedindex" | "MERGE_HEAD" | "MERGE_MSG" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "BISECT_LOG" | "rebase-merge" | "rebase-apply" | "sequencer" | "ORIG_HEAD" => GitChange::Status,
        // A submodule moving shows in this repository's status.
        "modules" if rest.ends_with("/HEAD") || rest.ends_with("/index") => GitChange::Status,
        _ => GitChange::Nothing,
    }
}

/// What the watcher has seen since it was last asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seen {
    /// Files someone can see changed, or the index.
    pub work: bool,
    pub refs: bool,
    /// Only ignored files changed. Worth a look now and then, not on every write: a tracked
    /// file that matches an ignore rule still shows in `status`.
    pub ignored: bool,
}

/// How often ignored files changing leads to another look at the status.
const IGNORED_EVERY: Duration = Duration::from_secs(30);

/// Watches the worktree and sets flags the app turns into refreshes, debounced.
pub struct RepoWatcher {
    _watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    work_dirty: Arc<AtomicBool>,
    git_dirty: Arc<AtomicBool>,
    ignored_dirty: Arc<AtomicBool>,
    /// Watching isn't working here, as on some network drives. The app polls instead.
    pub failed: Arc<AtomicBool>,
    last: Instant,
    last_ignored: Instant,
}

impl RepoWatcher {
    pub fn new(ctx: &egui::Context, repo: &Repo) -> Self {
        let work_dirty = Arc::new(AtomicBool::new(false));
        let git_dirty = Arc::new(AtomicBool::new(false));
        let ignored_dirty = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (w, g, i, c) = (work_dirty.clone(), git_dirty.clone(), ignored_dirty.clone(), ctx.clone());
        let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let (root, git_dir, common_dir) = (repo.root.clone(), repo.git_dir.clone(), repo.common_dir.clone());
        let own_prefixes = [format!("{}/", norm(&git_dir)), format!("{}/", norm(&common_dir)), format!("{}/.git/", norm(&root))];
        let mut rules = IgnoreRules::new(&root, &git_dir, repo.git.clone());
        let handler = move |res: notify::Result<notify::Event>| {
            // Lost events, or an overflow: anything may have changed.
            let Ok(ev) = res else {
                w.store(true, Ordering::Relaxed);
                g.store(true, Ordering::Relaxed);
                c.request_repaint_after(Duration::from_millis(400));
                return;
            };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            if ev.paths.is_empty() || ev.need_rescan() {
                w.store(true, Ordering::Relaxed);
                g.store(true, Ordering::Relaxed);
                c.request_repaint_after(Duration::from_millis(400));
                return;
            }
            for p in &ev.paths {
                let s = p.to_string_lossy().replace('\\', "/");
                if let Some(rest) = own_prefixes.iter().find_map(|o| s.strip_prefix(o.as_str())) {
                    if rest == "info/exclude" {
                        rules.forget();
                    }
                    match classify_git_path(rest) {
                        GitChange::Nothing => continue,
                        GitChange::Status => w.store(true, Ordering::Relaxed),
                        GitChange::Refs => g.store(true, Ordering::Relaxed),
                    }
                } else if s.ends_with("/.git") {
                    continue;
                } else if let Some(i) = s.find("/.git/") {
                    // Another repository inside this one: its HEAD can show in this status.
                    if classify_git_path(&s[i + 6..]) == GitChange::Nothing {
                        continue;
                    }
                    w.store(true, Ordering::Relaxed);
                } else {
                    if s.ends_with("/.gitignore") {
                        rules.forget();
                    } else if rules.is_ignored(p) {
                        i.store(true, Ordering::Relaxed);
                        c.request_repaint_after(IGNORED_EVERY);
                        continue;
                    }
                    w.store(true, Ordering::Relaxed);
                }
                c.request_repaint_after(Duration::from_millis(400));
            }
        };
        let holder: Arc<Mutex<Option<notify::RecommendedWatcher>>> = Arc::default();
        let (slot, fail, dirty) = (holder.clone(), failed.clone(), work_dirty.clone());
        let start = move || {
            let Ok(mut watcher) = notify::recommended_watcher(handler) else {
                fail.store(true, Ordering::Relaxed);
                return;
            };
            let mut ok = watcher.watch(&root, notify::RecursiveMode::Recursive).is_ok();
            // A linked worktree's git dir lives outside its root, and its branches in the
            // main repository's, where any other worktree can move them.
            let outside: Vec<&PathBuf> = if git_dir.starts_with(&common_dir) { vec![&common_dir] } else { vec![&git_dir, &common_dir] };
            for dir in outside.into_iter().filter(|d| !d.starts_with(&root)) {
                ok &= watcher.watch(dir, notify::RecursiveMode::Recursive).is_ok();
            }
            if !ok {
                fail.store(true, Ordering::Relaxed);
            }
            if let Ok(mut s) = slot.lock() {
                *s = Some(watcher);
            }
            dirty.store(true, Ordering::Relaxed);
        };
        // On Linux this walks the whole tree, one watch per folder.
        if cfg!(target_os = "linux") {
            std::thread::spawn(start);
        } else {
            start();
            work_dirty.store(false, Ordering::Relaxed);
        }
        Self { _watcher: holder, work_dirty, git_dirty, ignored_dirty, failed, last: Instant::now(), last_ignored: Instant::now() }
    }

    /// What changed, at most every 400 ms, and for ignored files much less often.
    pub fn take(&mut self) -> Seen {
        let mut seen = Seen::default();
        if self.last.elapsed() >= Duration::from_millis(400) {
            seen.work = self.work_dirty.swap(false, Ordering::Relaxed);
            seen.refs = self.git_dirty.swap(false, Ordering::Relaxed);
            if seen.work || seen.refs {
                self.last = Instant::now();
            }
        }
        if self.last_ignored.elapsed() >= IGNORED_EVERY && self.ignored_dirty.swap(false, Ordering::Relaxed) {
            self.last_ignored = Instant::now();
            seen.ignored = true;
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules are ours only to save running git on every event, so they must agree with it.
    #[test]
    fn ignore_rules_agree_with_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = gitem_core::GitExe::discover(None).unwrap();
        git.run(Some(&root), &["init", "-q"]).unwrap();
        let write = |p: &str, s: &str| {
            let path = root.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, s).unwrap();
        };
        write(".gitignore", "target/\n*.log\n!keep.log\n/rooted.txt\nbuild\ndocs/*.tmp\n");
        write("sub/.gitignore", "local.txt\n!reinstated.log\n");
        write(".git/info/exclude", "excluded.txt\n");
        let paths = ["a.txt", "target/x/y.o", "other/target/z", "app.log", "keep.log", "sub/keep.log", "rooted.txt", "sub/rooted.txt", "build", "x/build/out", "docs/a.tmp", "docs/deep/a.tmp", "sub/local.txt", "local.txt", "sub/reinstated.log", "excluded.txt", "sub/excluded.txt", "target"];
        for p in paths {
            if p != "target" && p != "build" {
                write(p, "x");
            }
        }
        std::fs::create_dir_all(root.join("build")).unwrap();
        let mut rules = IgnoreRules::new(&root, &root.join(".git"), git.clone());
        for p in paths {
            let says = git.run_raw(Some(&root), &["check-ignore", "-q", "--", p]).unwrap().ok();
            assert_eq!(rules.is_ignored(&root.join(p)), says, "{p}");
        }
        // Rules change: asked again after being told.
        write(".gitignore", "a.txt\n");
        rules.forget();
        assert!(rules.is_ignored(&root.join("a.txt")));
        assert!(!rules.is_ignored(&root.join("app.log")));
    }

    #[test]
    fn only_what_can_move_a_ref_refreshes_the_refs() {
        for refs in ["HEAD", "refs/heads/x", "packed-refs", "config", "worktrees/w/HEAD"] {
            assert_eq!(classify_git_path(refs), GitChange::Refs, "{refs}");
        }
        for status in ["index", "MERGE_HEAD", "rebase-merge/msgnum", "BISECT_LOG", "ORIG_HEAD"] {
            assert_eq!(classify_git_path(status), GitChange::Status, "{status}");
        }
        for nothing in ["objects/ab/cd", "logs/HEAD", "index.lock", "refs/heads/x.lock", "COMMIT_EDITMSG", "FETCH_HEAD", "worktrees/w/index"] {
            assert_eq!(classify_git_path(nothing), GitChange::Nothing, "{nothing}");
        }
    }
}
