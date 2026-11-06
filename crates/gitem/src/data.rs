//! Per-repository cached state, refreshed in the background and invalidated by
//! a filesystem watcher. Views compare generation counters to know when to reload.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gitem_core::Repo;
use gitem_core::forge::{self, RemoteRepo};
use gitem_core::model::*;
use gitem_core::settings::CustomHost;
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
}

impl Refs {
    fn signature(&self) -> String {
        let mut s = String::new();
        for b in &self.branches {
            s.push_str(&b.name);
            s.push_str(&b.sha);
            s.push_str(&b.ahead.to_string());
            s.push_str(&b.behind.to_string());
        }
        for t in &self.tags {
            s.push_str(&t.name);
            s.push_str(&t.sha);
        }
        for t in &self.stashes {
            s.push_str(&t.sha);
        }
        for w in &self.worktrees {
            s.push_str(&w.path);
            s.push_str(&w.head);
        }
        s
    }
}

#[derive(Default)]
pub struct RepoData {
    pub status: Task<RepoStatus>,
    pub refs: Task<Refs>,
    /// Bumped when branches, tags, or stashes change.
    pub refs_gen: u64,
    /// Bumped when working-tree or index state changes.
    pub status_gen: u64,
    signature: String,
    status_sig: String,
    pub forge_repo: Option<RemoteRepo>,
    pub forge_remote: Option<String>,
    pub custom_hosts: Vec<CustomHost>,
    pub last_blame: String,
    /// Writes are queued or running on this repository.
    pub ops_busy: bool,
    pub branch_pr: Option<(u64, String)>,
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
        let r = repo.clone();
        self.refs.spawn(ctx, move || -> gitem_core::Result<Refs> {
            Ok(Refs {
                branches: r.branches()?,
                tags: r.tags()?,
                remotes: r.remotes()?,
                stashes: r.stashes()?,
                worktrees: r.worktrees()?,
                submodules: r.submodules().unwrap_or_default(),
                default_branch: r.default_branch(),
            })
        });
    }

    pub fn refresh_status(&mut self, ctx: &egui::Context, repo: &Repo) {
        if self.status.busy() {
            self.status_again = true;
            return;
        }
        let r = repo.clone();
        self.status.spawn(ctx, move || r.status());
    }

    pub fn poll(&mut self, ctx: &egui::Context, repo: &Repo) {
        if self.status.poll() {
            if std::mem::take(&mut self.status_again) {
                self.refresh_status(ctx, repo);
            }
            if let Some(s) = self.status.get() {
                let sig = format!("{:?}{:?}{:?}{:?}", s.head_sha, s.branch, s.entries, s.op);
                if sig != self.status_sig {
                    self.status_sig = sig;
                    self.status_gen += 1;
                }
            }
        }
        if self.refs.poll() {
            if std::mem::take(&mut self.refs_again) {
                self.refresh(ctx, repo);
            }
            if let Some(r) = self.refs.get() {
                let sig = r.signature();
                if sig != self.signature {
                    self.signature = sig;
                    self.refs_gen += 1;
                }
            }
            self.recompute_forge();
        }
    }

    /// Files or the index changed in a way the status summary may not show: a
    /// second edit to a modified file looks identical there. Views must reload.
    pub fn mark_changed(&mut self) {
        self.status_gen += 1;
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

/// Watches the worktree and sets flags the app turns into refreshes, debounced.
pub struct RepoWatcher {
    _watcher: Option<notify::RecommendedWatcher>,
    work_dirty: Arc<AtomicBool>,
    git_dirty: Arc<AtomicBool>,
    last: Instant,
}

impl RepoWatcher {
    pub fn new(ctx: &egui::Context, root: &Path, git_dir: &Path) -> Self {
        let work_dirty = Arc::new(AtomicBool::new(false));
        let git_dirty = Arc::new(AtomicBool::new(false));
        let (w, g, c) = (work_dirty.clone(), git_dirty.clone(), ctx.clone());
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            for p in &ev.paths {
                let s = p.to_string_lossy().replace('\\', "/");
                if let Some(i) = s.find("/.git/") {
                    let rest = &s[i + 6..];
                    // Churn that never changes what we display.
                    if rest.starts_with("objects") || rest.ends_with(".lock") || rest.starts_with("logs") {
                        continue;
                    }
                    g.store(true, Ordering::Relaxed);
                } else if s.ends_with("/.git") {
                    continue;
                } else {
                    w.store(true, Ordering::Relaxed);
                }
                c.request_repaint_after(Duration::from_millis(400));
            }
        };
        let mut watcher = notify::recommended_watcher(handler).ok();
        if let Some(wt) = &mut watcher {
            let _ = wt.watch(root, notify::RecursiveMode::Recursive);
            // A linked worktree's git dir lives outside its root.
            if !git_dir.starts_with(root) {
                let _ = wt.watch(git_dir, notify::RecursiveMode::Recursive);
            }
        }
        Self { _watcher: watcher, work_dirty, git_dirty, last: Instant::now() }
    }

    /// Returns (work tree changed, refs may have changed), at most every 400 ms.
    pub fn take(&mut self) -> (bool, bool) {
        if self.last.elapsed() < Duration::from_millis(400) {
            return (false, false);
        }
        let w = self.work_dirty.swap(false, Ordering::Relaxed);
        let g = self.git_dirty.swap(false, Ordering::Relaxed);
        if w || g {
            self.last = Instant::now();
        }
        (w, g)
    }
}
