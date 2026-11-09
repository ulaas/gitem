//! Top-level application: repository tabs, docking, op queue, dialogs, toasts.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use egui::{Align2, RichText, Ui};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, NodePath, TabViewer};
use gitem_core::model::*;
use gitem_core::ops::{self, GitOp};
use gitem_core::settings::Settings;
use gitem_core::{GitExe, Repo};

use crate::action::{Action, Cx};
use crate::data::{RepoData, RepoWatcher};
use crate::forms::{self, FormDialog};
use crate::palette::PaletteState;
use crate::theme::{self, Palette};
use crate::views::inspect::InspectPanel;
use crate::chrome;
use egui::{Color32, Sense};
use crate::views::sidebar::Sidebar;
use crate::views::{Tab, mergeview};
use crate::widgets::*;

enum OpMsg {
    Line(String),
    /// How to take it back, now that it has worked.
    Undo(Box<gitem_core::undo::UndoEntry>),
    /// Why this one will have no way back, said before it runs.
    NoUndo(String),
    Done(Result<String, String>),
}

struct Running {
    title: String,
    /// Its git commands, such as `fetch`, and its [`crate::data::op_signature`]: what the button
    /// that started it looks for.
    commands: Vec<String>,
    signature: String,
    rx: Receiver<OpMsg>,
    cancel: Arc<AtomicBool>,
    last_line: String,
    network: bool,
    open_after: Option<PathBuf>,
    /// Stopping it loses nothing, so it's no reason to ask before closing.
    harmless: bool,
    /// It makes a commit, which can then be taken back.
    is_commit: bool,
    /// It changes the repository's config by the user's own hand in gitem.
    own_config: bool,
    /// It clones, so the folder it leaves behind is gitem's own work.
    is_clone: bool,
    /// What was asked for together with it. See [`Queued`].
    batch: u64,
    started: Instant,
    /// Cancel was pressed, on something where that needs saying twice.
    asking_to_stop: bool,
    /// How to take it back, once it has worked.
    undo: Option<gitem_core::undo::UndoEntry>,
    then: Option<Action>,
}

/// A fetch only updates remote-tracking refs, and can simply be done again.
fn is_harmless(op: &GitOp) -> bool {
    op.custom.is_none() && op.steps.iter().all(|s| s.args.first().is_some_and(|a| a == "fetch"))
}

fn spawn_op(ctx: &egui::Context, git: GitExe, cwd: Option<PathBuf>, op: GitOp, open_after: Option<PathBuf>) -> Running {
    let own_config = op.own_config;
    let is_clone = op.steps.first().and_then(|s| s.args.first()).is_some_and(|a| a == "clone");
    let is_commit = op.steps.last().and_then(|s| s.args.first()).is_some_and(|a| a == "commit");
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let ctx2 = ctx.clone();
    let (title, network, harmless, commands, signature) = (op.title.clone(), op.network, is_harmless(&op), op.commands(), crate::data::op_signature(&op));
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let ctx3 = ctx2.clone();
        let mut on_line = move |l: &str| {
            let _ = tx2.send(OpMsg::Line(l.to_string()));
            ctx3.request_repaint();
        };
        // Read before anything changes. An op with no way back still does what was asked.
        let captured = match (&op.undo, &cwd) {
            (Some(plan), Some(dir)) => plan.capture(&git, dir).map_err(|why| {
                let _ = tx.send(OpMsg::NoUndo(why));
            }).ok(),
            _ => None,
        };
        let res = op.run(&git, cwd.as_deref(), Some(c2), &mut on_line).map(|o| op.outcome_note(&o).unwrap_or_default()).map_err(|e| e.to_string());
        if res.is_ok()
            && let (Some(c), Some(dir)) = (captured, &cwd)
            && let Some(entry) = c.into_entry(&git, dir)
        {
            let _ = tx.send(OpMsg::Undo(Box::new(entry)));
        }
        let _ = tx.send(OpMsg::Done(res));
        ctx2.request_repaint();
    });
    Running { title, commands, signature, rx, cancel, last_line: String::new(), network, open_after, harmless, is_commit, own_config, is_clone, batch: 0, started: Instant::now(), asking_to_stop: false, undo: None, then: None }
}

/// A write waiting its turn. `batch` tells what was asked for together: the steps of one
/// action assume the earlier ones worked, and later actions don't.
struct Queued {
    op: GitOp,
    open_after: Option<PathBuf>,
    batch: u64,
    /// What to do next if this works.
    then: Option<Action>,
}

/// Something done for the user without being asked, beside the queue. It never holds up
/// what they do, never toasts, and its failure is a note in the status bar.
struct Quiet {
    task: crate::task::Task<()>,
    is_fetch: bool,
}

/// A second identical write this soon after the first is a double click. Skip on a rebase,
/// or Good in a bisect, would otherwise act on a state nobody has looked at yet.
const DOUBLE_CLICK: Duration = Duration::from_millis(600);

/// After this many in a row, a scheduled fetch stops getting further apart.
const MAX_FETCH_BACKOFF: u32 = 4;

/// Whether two ops are the same request: same commands, same input.
fn same_op(a: &GitOp, b: &GitOp) -> bool {
    a.title == b.title && a.steps.len() == b.steps.len() && a.steps.iter().zip(&b.steps).all(|(x, y)| x.args == y.args && x.stdin == y.stdin)
}

pub struct RepoTab {
    pub repo: Repo,
    pub data: RepoData,
    watcher: RepoWatcher,
    pub dock: DockState<Tab>,
    /// One pane fills the window; the rest of the layout waits here.
    pub zoom: Option<crate::layouts::Zoom>,
    pub inspect: InspectPanel,
    show_inspect: bool,
    sidebar: Sidebar,
    show_sidebar: bool,
    show_rail: bool,
    queue: VecDeque<Queued>,
    running: Option<Running>,
    /// When the last one was asked for, whether or not it's still in the queue.
    last_asked: Option<(GitOp, Instant)>,
    quiet: Option<Quiet>,
    /// Waiting for the repository to be decided about, then `quiet`.
    maintenance_due: bool,
    last_auto_fetch: Instant,
    /// Scheduled fetches that failed in a row. Each one doubles the wait for the next.
    fetch_failures: u32,
    /// Why the last scheduled fetch failed, for the status bar.
    fetch_note: Option<String>,
    fetch_minutes: u32,
    last_index_refresh: Option<Instant>,
    last_poll: Instant,
    background: Vec<crate::task::Task<String>>,
    later: Vec<crate::task::Task<crate::action::Later>>,
    /// The user asked to see the question about this repository's config again.
    review_trust: bool,
    /// Typed in the toolbar's branch menu.
    branch_filter: String,
    /// Branches switched to here, latest first.
    recent_branches: Vec<String>,
    /// The commit made here last, while nothing else has been run since.
    undoable: Option<String>,
    /// The way back from the last thing done, other than a commit.
    undo_entry: Option<gitem_core::undo::UndoEntry>,
    /// Something long finished, or anything failed, with the window in the background.
    attention_due: bool,
    /// The same, for a notification: what it was, and whether it failed.
    notice_due: Option<(String, bool)>,
    branch_pr_task: crate::task::Task<Option<(String, u64, String)>>,
    /// What that was last asked for: the branch, and the host's lists as of when.
    branch_pr_for: Option<(Option<String>, u64)>,
    /// What there isn't room for beside the view at this window size.
    squeeze: Squeeze,
    /// Which of those is being shown over the view.
    peek: Option<Peek>,
    /// Details is showing Changes' list and commit box, not a commit.
    show_wip: bool,
    /// The tab opened last from the rail's More list, which the next one replaces.
    more_tab: Option<String>,
    /// When FETCH_HEAD was last written, as read for this status.
    fetched: (u64, Option<std::time::SystemTime>),
}

#[derive(Default, Clone, Copy)]
struct Squeeze {
    details: bool,
    sidebar: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Peek {
    Details,
    Sidebar,
}

/// A view narrower than this has lost the point of showing it.
const VIEW_MIN_WIDTH: f32 = 520.0;
const SIDEBAR_WIDTH: f32 = 270.0;
const DETAILS_WIDTH: f32 = 340.0;

/// Which side panels give way, Details first, so the view keeps a usable width.
fn squeeze_for(width: f32, rail: f32, sidebar: bool, details: bool) -> Squeeze {
    let left = width - rail;
    let with_sidebar = if sidebar { left - SIDEBAR_WIDTH } else { left };
    let hide_details = details && with_sidebar - DETAILS_WIDTH < VIEW_MIN_WIDTH;
    let hide_sidebar = sidebar && with_sidebar < VIEW_MIN_WIDTH;
    Squeeze { details: hide_details, sidebar: hide_sidebar }
}

#[cfg(test)]
#[test]
fn details_give_way_first_and_then_the_sidebar() {
    let got = |w: f32| {
        let s = squeeze_for(w, chrome::RAIL_WIDTH, true, true);
        (s.details, s.sidebar)
    };
    assert_eq!(got(1500.0), (false, false));
    assert_eq!(got(1100.0), (true, false));
    assert_eq!(got(800.0), (true, true));
    // What's been turned off has no room to give.
    assert!(!squeeze_for(1000.0, chrome::RAIL_WIDTH, true, false).details);
    // A wider rail takes its room from the view: what just fitted with the narrow one doesn't.
    let just_fits = 44.0 + SIDEBAR_WIDTH + DETAILS_WIDTH + VIEW_MIN_WIDTH;
    assert!(!squeeze_for(just_fits, chrome::RAIL_WIDTH, true, true).details);
    assert!(squeeze_for(just_fits, chrome::RAIL_LABELLED_WIDTH, true, true).details);
}

impl RepoTab {
    fn new(ctx: &egui::Context, repo: Repo, settings: &Settings) -> Self {
        let mut data = RepoData::default();
        data.custom_hosts = settings.custom_hosts.clone();
        data.refresh(ctx, &repo);
        let root = repo.root.to_string_lossy().replace('\\', "/");
        let saved_layout = settings.session_layouts.get(&root).and_then(|j| layout_from_json(j));
        let mut tabs: Vec<Tab> = settings.session_tabs.get(&root).map(|v| v.iter().filter_map(|s| Tab::from_spec(s)).collect()).unwrap_or_default();
        if tabs.is_empty() {
            tabs = vec![Tab::home(), Tab::graph(), Tab::changes()];
        }
        let mut dock = saved_layout.unwrap_or_else(|| DockState::new(tabs));
        if let Some(path) = dock.find_tab_from(|t| matches!(t, Tab::Graph(_))) {
            let _ = dock.set_active_tab(path);
        }
        let watcher = RepoWatcher::new(ctx, &repo);
        Self {
            repo,
            data,
            watcher,
            dock,
            zoom: None,
            inspect: InspectPanel::default(),
            show_inspect: true,
            sidebar: Sidebar::default(),
            show_sidebar: true,
            show_rail: true,
            queue: VecDeque::new(),
            running: None,
            last_asked: None,
            quiet: None,
            maintenance_due: false,
            last_auto_fetch: Instant::now(),
            fetch_failures: 0,
            fetch_note: None,
            fetch_minutes: 0,
            last_index_refresh: None,
            last_poll: Instant::now(),
            background: Vec::new(),
            later: Vec::new(),
            review_trust: false,
            branch_filter: String::new(),
            recent_branches: Vec::new(),
            undoable: None,
            undo_entry: None,
            attention_due: false,
            notice_due: None,
            branch_pr_task: Default::default(),
            branch_pr_for: None,
            squeeze: Squeeze::default(),
            peek: None,
            show_wip: false,
            more_tab: None,
            fetched: (u64::MAX, None),
        }
    }

    /// Every tab, those hidden while a pane fills the window included.
    pub fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.dock.iter_all_tabs().chain(self.zoom.iter().flat_map(|z| z.full.iter_all_tabs())).map(|(_, t)| t)
    }

    /// Does what a tab's menu or a key asked. Tabs with unsaved work stay open: one asks
    /// first, as its close button does, and several are left with a note.
    fn dock_op(&mut self, key: &str, op: crate::layouts::DockOp, guard: &mut Option<CloseGuard>) -> Option<String> {
        use crate::layouts::DockOp;
        match op {
            DockOp::Zoom => {
                let pane = self.dock.find_tab_from(|t| t.key() == key).map(|p| NodePath { surface: p.surface, node: p.node });
                self.toggle_zoom(pane);
            }
            DockOp::Keep => {
                for t in self.all_tabs_mut() {
                    match t {
                        Tab::Diff(d) if d.key() == key => d.pinned = true,
                        Tab::Editor(e) if e.key() == key => e.pinned = true,
                        _ => {}
                    }
                }
            }
            DockOp::Split(dir) => {
                crate::layouts::split_off(&mut self.dock, key, dir);
            }
            DockOp::Close => {
                let unsaved = self.dock.iter_all_tabs().find(|(_, t)| t.key() == key).and_then(|(_, t)| t.unsaved());
                match unsaved {
                    Some(lost) => *guard = Some(CloseGuard { target: CloseTarget::Tab { root: self.repo.root.clone(), key: key.to_string() }, items: vec![lost] }),
                    None => self.close_tab_now(key),
                }
            }
            DockOp::CloseMany(which) => {
                let mut kept = 0;
                for k in crate::layouts::keys_to_close(&self.dock, key, which) {
                    if self.dock.iter_all_tabs().any(|(_, t)| t.key() == k && t.unsaved().is_some()) {
                        kept += 1;
                    } else {
                        self.close_tab_now(&k);
                    }
                }
                if kept > 0 {
                    return Some(format!("{} with unsaved edits left open.", plural(kept, "tab", "tabs")));
                }
            }
        }
        None
    }

    /// Closes a tab, unless it's the last: something has to show where things are.
    fn close_tab_now(&mut self, key: &str) {
        if self.dock.iter_all_tabs().count() > 1
            && let Some(path) = self.dock.find_tab_from(|t| t.key() == key)
        {
            self.dock.remove_tab(path);
        }
    }

    pub fn all_tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.dock.iter_all_tabs_mut().chain(self.zoom.iter_mut().flat_map(|z| z.full.iter_all_tabs_mut())).map(|(_, t)| t)
    }

    /// One pane fills the window, or the whole layout comes back.
    pub fn toggle_zoom(&mut self, pane: Option<NodePath>) {
        match self.zoom.take() {
            Some(z) => crate::layouts::unzoom(&mut self.dock, z),
            None => {
                let pane = pane.or_else(|| self.dock.focused_leaf()).or_else(|| self.dock.iter_leaves().next().map(|(p, _)| p));
                if let Some(p) = pane {
                    self.zoom = crate::layouts::zoom(&mut self.dock, p);
                }
            }
        }
    }

    /// What closing this repository's tab would lose, in a list for the user.
    fn at_stake(&self, running_note: &str) -> Vec<String> {
        let mut items: Vec<String> = self.all_tabs().filter_map(|t| t.unsaved()).collect();
        if let Some(r) = self.running.as_ref().filter(|r| !r.harmless) {
            items.push(format!("{} is running. {running_note}", r.title));
        }
        let queued = self.queue.iter().filter(|q| !is_harmless(&q.op)).count();
        if queued > 0 {
            items.push(format!("{} won't run.", plural(queued, "queued git command", "queued git commands")));
        }
        items
    }

    /// Whether any side panel is showing, or Zen mode has hidden them.
    pub fn panels_showing(&self) -> bool {
        self.show_sidebar || self.show_inspect
    }

    /// A view from the rail's More list takes over from the last one, so going through them
    /// doesn't pile up tabs. It stays if it holds something that closing would lose.
    fn open_from_more(&mut self, tab: Tab) {
        let key = tab.key();
        let already = self.dock.find_tab_from(|t| t.key() == key).is_some();
        if let (false, Some(old)) = (already, self.more_tab.clone())
            && let Some(path) = self.dock.find_tab_from(|t| t.key() == old && t.unsaved().is_none())
        {
            if let Some((_, slot)) = self.dock.iter_all_tabs_mut().find(|(p, _)| *p == path) {
                *slot = tab;
            }
            let _ = self.dock.set_active_tab(path);
        } else {
            self.open_tab(tab);
        }
        self.more_tab = Some(key);
    }

    pub fn open_tab(&mut self, tab: Tab) {
        // A tab that's open but hidden behind a zoomed pane brings the layout back.
        let key = tab.key();
        if self.zoom.as_ref().is_some_and(|z| z.full.find_tab_from(|t| t.key() == key).is_some()) {
            self.toggle_zoom(None);
        }
        // Diffs opened from lists reuse one preview tab, unless it's been pinned.
        // A file clicked in IDE mode is previewed the same way, in one tab, until it's kept.
        // A preview with edits is never replaced: typing keeps it.
        if let Tab::Editor(new) = &tab
            && !new.path.is_empty()
        {
            let key = tab.key();
            let preview = self.dock.iter_all_tabs().find(|(_, t)| matches!(t, Tab::Editor(e) if !e.pinned && !e.is_dirty() && !e.path.is_empty())).map(|(p, _)| p);
            if self.dock.find_tab_from(|t| t.key() == key).is_none()
                && let Some(path) = preview
            {
                if let Some((_, slot)) = self.dock.iter_all_tabs_mut().find(|(p, _)| *p == path) {
                    *slot = tab;
                }
                let _ = self.dock.set_active_tab(path);
                return;
            }
        }
        if let Tab::Diff(new) = &tab {
            let key = tab.key();
            if let Some(path) = self.dock.find_tab_from(|t| t.key() == key) {
                let _ = self.dock.set_active_tab(path);
                return;
            }
            let _ = new;
            let found = self.dock.iter_all_tabs().find(|(_, t)| matches!(t, Tab::Diff(d) if !d.pinned)).map(|(p, _)| p);
            if let Some(path) = found {
                if let Some((_, slot)) = self.dock.iter_all_tabs_mut().find(|(p, _)| *p == path) {
                    *slot = tab;
                }
                let _ = self.dock.set_active_tab(path);
                return;
            }
        } else {
            let key = tab.key();
            if let Some(path) = self.dock.find_tab_from(|t| t.key() == key) {
                let mut tab = Some(tab);
                if let Some((_, open)) = self.dock.iter_all_tabs_mut().find(|(p, _)| *p == path)
                    && let Some(new) = tab.take()
                {
                    tab = open.retarget(new);
                }
                match tab {
                    Some(own) => self.open_tab(own),
                    None => {
                        let _ = self.dock.set_active_tab(path);
                    }
                }
                return;
            }
        }
        self.dock.push_to_focused_leaf(tab);
    }

    fn poll_ops(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if let Some(r) = &mut self.running {
            let mut done = None;
            loop {
                match r.rx.try_recv() {
                    Ok(OpMsg::Line(l)) => r.last_line = l,
                    Ok(OpMsg::Undo(entry)) => r.undo = Some(*entry),
                    Ok(OpMsg::NoUndo(why)) => out.push(Action::Toast(why)),
                    Ok(OpMsg::Done(res)) => {
                        done = Some(res);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done = Some(Err("The operation stopped unexpectedly.".to_string()));
                        break;
                    }
                }
            }
            if let Some(res) = done {
                let title = r.title.clone();
                let open_after = r.open_after.clone();
                // Only straight after it: anything else run since is what an undo would meet.
                self.undoable = res.as_ref().ok().filter(|_| r.is_commit).and_then(|n| n.split_whitespace().next()).map(String::from);
                // A fetch changes nothing an undo would meet. Anything else, and only the newest
                // is on offer: older ones may no longer mean what they say.
                if !r.harmless {
                    self.undo_entry = r.undo.take();
                }
                let (own_config, batch) = (r.own_config, r.batch);
                if wants_attention(r.started.elapsed(), res.is_err(), ctx.input(|i| i.focused)) {
                    self.attention_due = true;
                    self.notice_due = Some((r.title.clone(), res.is_err()));
                }
                let then = r.then.take();
                self.running = None;
                // A pull can bring in config through a tracked `include.path`.
                if let Err(e) = self.repo.recheck_trust(own_config) {
                    out.push(Action::Error(format!("Could not read this repository's config:\n{e}")));
                }
                self.data.refresh(ctx, &self.repo);
                self.data.mark_rewritten();
                match res {
                    Ok(note) => {
                        if let Some(p) = open_after {
                            out.push(Action::OpenRepo(p));
                        }
                        out.extend(then);
                        if !note.is_empty() || self.queue.is_empty() {
                            out.push(Action::Toast(ops::done_message(&title, &note)));
                        }
                    }
                    Err(e) => {
                        // Later steps of the same action assumed this one worked. Other actions
                        // don't depend on it, unless it left the repository in the middle of
                        // something: they were all asked for with no such thing going on.
                        let stopped_midway = self.repo.current_op() != RepoOp::None;
                        let before = self.queue.len();
                        self.queue.retain(|q| !stopped_midway && q.batch != batch);
                        let dropped = before - self.queue.len();
                        let not_run = if dropped > 0 { format!("\n\n{} after it {} not run.", plural(dropped, "queued command", "queued commands"), if dropped == 1 { "was" } else { "were" }) } else { String::new() };
                        // Being cancelled kills git without its cleanup. Either way, if the
                        // lock is why it stopped or what it left, there's a way out to offer.
                        if self.repo.index_lock().is_some() && (e == "cancelled" || gitem_core::runner::is_lock_failure(&e)) {
                            out.push(Action::OfferLockRemoval(self.repo.root.clone(), format!("{title} failed:\n{e}{not_run}")));
                        } else if stopped_midway && e.contains("CONFLICT") {
                            // Not a failure: the banner has what to do next.
                            out.push(Action::Toast(format!("{title} stopped at a conflict. Resolve it, then continue.")));
                            if dropped > 0 {
                                out.push(Action::Error(format!("{title} stopped at a conflict.{not_run}")));
                            }
                        } else if let (Some(target), Some(files), true) = (title.strip_prefix("Check out ").filter(|t| self.data.local_branches().any(|b| b.name == *t)), ops::switch_blocked_by_changes(&e), dropped == 0) {
                            out.push(Action::Form(forms::switch_with_changes(target.to_string(), files)));
                        } else {
                            out.push(Action::Error(format!("{title} failed:\n{e}{not_run}")));
                        }
                    }
                }
            }
        }
        // A fetch and a pull at once fight over the same refs. Anything else can run beside
        // a background fetch, just as with one started from a terminal.
        let held_back = self.quiet.as_ref().is_some_and(|q| q.is_fetch) && self.queue.front().is_some_and(|q| q.op.network);
        if self.running.is_none() && !held_back
            && let Some(q) = self.queue.pop_front() {
                let mut running = spawn_op(ctx, self.repo.git.clone(), Some(self.repo.root.clone()), q.op, q.open_after);
                running.batch = q.batch;
                running.then = q.then;
                self.running = Some(running);
            }
    }

    /// Runs an op as a button would. For tests of what follows it.
    pub fn queue_for_test(&mut self, op: GitOp) {
        self.queue.push_back(Queued { op, open_after: None, batch: 0, then: None });
    }

    /// What closing an open tab would lose. For tests.
    pub fn open_tab_unsaved(&self, key: &str) -> Option<String> {
        self.all_tabs().find(|t| t.key() == key).and_then(|t| t.unsaved())
    }

    /// The current branch's pull request for the status bar, with Launchpad open or not, once
    /// per branch until something says the host's lists have changed. Only when turned on: each
    /// look is a request to the host.
    fn look_up_branch_pr(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.branch_pr_task.poll()
            && let Some(found) = self.branch_pr_task.value.take()
        {
            self.data.branch_pr = found;
        }
        if !settings.status_bar_pr {
            return;
        }
        let branch = self.data.status.get().and_then(|s| s.branch.clone());
        let key = (branch.clone(), self.data.forge_gen);
        if self.branch_pr_for.as_ref() == Some(&key) {
            return;
        }
        let (Some(branch), Some(remote)) = (branch, self.data.forge().cloned()) else { return };
        self.branch_pr_for = Some(key);
        let git = self.repo.git.clone();
        self.branch_pr_task.spawn(ctx, move || -> Result<Option<(String, u64, String)>, String> {
            let list = crate::views::launchpad::resolve_client(&git, &remote)?.list_prs().map_err(|e| e.to_string())?;
            Ok(list.into_iter().find(|p| p.is_on_branch(&branch)).map(|p| (branch, p.number, p.url)))
        });
    }

    /// Brings the next scheduled fetch forward to now, for tests: the shortest wait is a minute.
    pub fn make_fetch_due(&mut self) {
        self.last_auto_fetch = Instant::now().checked_sub(Duration::from_secs(24 * 3600)).unwrap_or_else(Instant::now);
    }

    /// Minutes between scheduled fetches now, with any backing off.
    pub fn fetch_wait_minutes(&self) -> Option<u64> {
        (self.fetch_minutes > 0).then(|| self.fetch_minutes as u64 * 2u64.pow(self.fetch_failures.min(MAX_FETCH_BACKOFF)))
    }

    /// Whether a scheduled fetch is due, and how long until the next one if not.
    fn fetch_wait(&self, minutes: u32) -> Option<Duration> {
        if minutes == 0 {
            return None;
        }
        let every = Duration::from_secs(minutes as u64 * 60) * 2u32.pow(self.fetch_failures.min(MAX_FETCH_BACKOFF));
        Some(every.saturating_sub(self.last_auto_fetch.elapsed()))
    }

    /// The user's work always goes first. A scheduled fetch waits for a moment with nothing
    /// going on, since nobody wants FETCH_HEAD moved under a pull or a rebase.
    fn start_quiet_work(&mut self, ctx: &egui::Context, settings: &Settings) {
        self.fetch_minutes = settings.auto_fetch_minutes;
        if let Some(q) = &mut self.quiet {
            if !q.task.poll() {
                return;
            }
            let (ok, error) = (q.task.error.is_none(), q.task.error.take());
            if q.is_fetch {
                if ok {
                    self.fetch_failures = 0;
                    self.fetch_note = None;
                    self.data.refresh(ctx, &self.repo);
                } else {
                    self.fetch_failures = self.fetch_failures.saturating_add(1);
                    self.fetch_note = error;
                }
            }
            self.quiet = None;
        }
        let busy = self.running.is_some() || !self.queue.is_empty() || self.data.jobs.load(Ordering::Relaxed) > 0 || self.data.status.get().is_none_or(|s| s.op != RepoOp::None);
        // Nobody asked for these, so they wait for the repository to be decided about, and
        // run no hooks.
        if busy || self.repo.trust.restricted() {
            return;
        }
        let run = |tab: &mut Self, op: GitOp, is_fetch: bool, ctx: &egui::Context| {
            let mut task = crate::task::Task::default();
            task.spawn_git(ctx, &tab.repo, move |r| -> Result<(), String> { op.run(&r.git, Some(&r.root), None, &mut |_| {}).map(drop).map_err(|e| e.to_string()) });
            tab.quiet = Some(Quiet { task, is_fetch });
        };
        if std::mem::take(&mut self.maintenance_due) {
            run(self, ops::maintenance_run().background(), false, ctx);
            return;
        }
        if self.fetch_wait(settings.auto_fetch_minutes).is_some_and(|w| w.is_zero()) && !self.data.remotes().is_empty() {
            self.last_auto_fetch = Instant::now();
            run(self, ops::fetch(&ops::FetchOpts { all: true, prune: settings.prune_on_fetch, ..Default::default() }).background().unattended(), true, ctx);
            return;
        }
        // Reads can't save what they learn about the files, so after something touches a
        // lot of them every status hashes them all again, until something writes the index.
        let slow = self.data.status_took.get() >= SLOW_STATUS;
        if slow && self.last_index_refresh.is_none_or(|t| t.elapsed() >= INDEX_REFRESH_EVERY) {
            self.last_index_refresh = Some(Instant::now());
            run(self, ops::refresh_index().background(), false, ctx);
        }
    }
}

/// A status slower than this is worth refreshing the index for.
const SLOW_STATUS: Duration = Duration::from_secs(1);
const INDEX_REFRESH_EVERY: Duration = Duration::from_secs(60);

/// How often to look for changes where the file watcher doesn't work.
const POLL_WITHOUT_WATCHER: Duration = Duration::from_secs(5);

/// A way out of a failed command, when its message is one people often meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorFix {
    SetUpstream,
    PullOptions,
    /// Both sides have commits and nothing says how to join them.
    Reconcile,
    Stash,
    AddRemote,
    Credentials,
    /// No key for the AI provider.
    AiSettings,
    /// Git doesn't know who's committing.
    Identity,
}

impl ErrorFix {
    fn label(self) -> &'static str {
        match self {
            ErrorFix::SetUpstream => "Set upstream…",
            ErrorFix::PullOptions => "Pull options…",
            ErrorFix::Reconcile => "Merge or rebase…",
            ErrorFix::Stash => "Stash…",
            ErrorFix::AddRemote => "Add a remote…",
            ErrorFix::Credentials => "Open Credentials",
            ErrorFix::AiSettings => "Open AI settings",
            ErrorFix::Identity => "Set name and email…",
        }
    }
}

/// Plain words for what git said, and what to do about it. The whole message stays available
/// behind this. Based on git's C-locale messages, which is what gitem makes it print.
pub fn explain_error(text: &str) -> Option<(&'static str, ErrorFix)> {
    let has = |s: &str| text.contains(s);
    if has("No API key for") {
        return Some(("This needs an AI provider with a key, which isn't set up yet.", ErrorFix::AiSettings));
    }
    if has("Please tell me who you are") || has("Author identity unknown") || has("empty ident name") {
        return Some(("Git needs a name and an email to put on commits, and hasn't got them.", ErrorFix::Identity));
    }
    if has("no tracking information") || has("has no upstream branch") {
        Some(("This branch isn't linked to a branch on a remote yet.", ErrorFix::SetUpstream))
    } else if has("would be overwritten by") || has("Please commit your changes or stash them") {
        Some(("You have uncommitted changes in files this would replace. Stash them, or commit them, and try again.", ErrorFix::Stash))
    } else if has("No configured push destination") || has("does not appear to be a git repository") || has("No remote repository specified") {
        Some(("There's no remote to talk to. Add one, or check the name of the one you meant.", ErrorFix::AddRemote))
    } else if has("reconcile divergent branches") {
        Some(("This branch and its remote both have new commits, and git needs to be told how to bring them together.", ErrorFix::Reconcile))
    } else if has("non-fast-forward") || has("fetch first") || has("Not possible to fast-forward") {
        Some(("This branch and its remote have both moved on. Bring in the remote's commits first.", ErrorFix::PullOptions))
    } else if has("Authentication failed") || has("could not read Username") || has("could not read Password") || has("Permission denied (publickey") || has("terminal prompts disabled") {
        Some(("The remote didn't accept your sign-in. Check the saved credentials or your SSH key.", ErrorFix::Credentials))
    } else {
        None
    }
}

/// The folder an "isn't a repository" message is about, if making one there would make sense:
/// it's missing or empty. A folder with files in it may be someone's project in the wrong place.
pub fn folder_to_create(error: &str) -> Option<String> {
    let path = error.strip_suffix(" doesn't exist. It may have been moved, renamed or deleted.").or_else(|| error.split_once(" isn't a git repository:").map(|(p, _)| p))?;
    let empty = std::fs::read_dir(path).map_or(true, |mut d| d.next().is_none());
    empty.then(|| path.to_string())
}

/// The window's title and the text under it, from `Title failed:\ndetails`.
pub fn error_parts(e: &str) -> (&str, &str) {
    match e.split_once('\n') {
        Some((first, rest)) if first.ends_with(" failed:") => (first.trim_end_matches(':'), rest),
        _ => ("Something went wrong", e),
    }
}

use gitem_core::trust::TrustState;

const DOCK_ID: &str = "gitem-dock";

/// What's going on, in the words of the branches involved where they can be found.
pub fn op_headline(st: &RepoStatus, branches: &[Branch]) -> String {
    // The commit a name is known by, if a branch is there.
    let named = |sha: &str| branches.iter().find(|b| !sha.is_empty() && b.sha.starts_with(sha)).map(|b| b.name.clone());
    let target = st.op_target.clone().unwrap_or_default();
    match st.op {
        RepoOp::Merge => {
            let other = st.op_merging.clone().or_else(|| named(&target)).unwrap_or(target);
            match &st.branch {
                Some(here) => format!("Merging {other} into {here}"),
                None => format!("Merging {other}"),
            }
        }
        RepoOp::Rebase | RepoOp::RebaseInteractive => {
            let onto = named(&target).unwrap_or(target);
            match &st.op_branch {
                Some(b) => format!("Rebasing {b} onto {onto}"),
                None => format!("Rebasing onto {onto}"),
            }
        }
        op => format!("{} {target}", op.label()).trim_end().to_string(),
    }
}

/// A non-network op is only offered a Cancel once it looks stuck, since stopping one can
/// leave things half done.
const CANCEL_OFFERED_AFTER: Duration = Duration::from_secs(3);

fn ctx_repaint_at(ui: &Ui, wait: Duration) {
    ui.ctx().request_repaint_after(wait);
}

/// What to expect while a repository's programs are off.
const RESTRICTED_NOTE: &str = "While they're off, files that use a filter can look modified when they aren't, diffs and blame show the raw text, signatures aren't checked, and staging skips those filters.";

/// The id egui_dock gives a tab's title in a pane. It's internal to that crate, so if
/// an upgrade changes it the markers simply disappear; a test watches for that.
pub fn dock_tab_id(path: egui_dock::NodePath, tab: usize) -> egui::Id {
    egui::Id::new(DOCK_ID).with((path.surface, "surface")).with((path.node, "node")).with((tab, "tab"))
}

/// A thin line on top of each pane's active tab, like the repository tabs have. The
/// pane in use gets the accent. egui_dock has no style for this, so it's drawn over.
fn paint_active_tab_markers(ui: &Ui, dock: &DockState<Tab>, p: &Palette) {
    let focused = dock.focused_leaf().or_else(|| dock.iter_leaves().next().map(|(path, _)| path));
    for (path, leaf) in dock.iter_leaves() {
        let Some(tab) = ui.ctx().read_response(dock_tab_id(path, leaf.active.0)) else { continue };
        let line = egui::Rect::from_min_size(tab.rect.min, egui::vec2(tab.rect.width(), 2.0));
        ui.painter().with_clip_rect(leaf.rect.intersect(ui.clip_rect())).rect_filled(line, 0.0, if focused == Some(path) { p.accent } else { p.faint });
    }
}

/// The tab arrangement, splits included, without one-off tabs.
pub fn layout_to_json(dock: &DockState<Tab>) -> Option<String> {
    serde_json::to_string(&dock.filter_map_tabs(|t: &Tab| t.spec())).ok()
}

/// `None` when unreadable or left with no tabs, so a default layout is used.
pub fn layout_from_json(json: &str) -> Option<DockState<Tab>> {
    // Rectangles not laid out yet are NaN, which JSON stores as null. They're
    // recomputed every frame, so zero is a safe stand-in.
    fn fill_nulls(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Null => *v = serde_json::json!(0.0),
            serde_json::Value::Array(a) => a.iter_mut().for_each(fill_nulls),
            serde_json::Value::Object(o) => {
                for (k, x) in o.iter_mut() {
                    if k != "focused_surface" && k != "focused_node" {
                        fill_nulls(x);
                    }
                }
            }
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    fill_nulls(&mut value);
    let specs: DockState<String> = serde_json::from_value(value).ok()?;
    let dock = specs.filter_map_tabs(|s: &String| Tab::from_spec(s));
    let has_tabs = dock.iter_all_tabs().next().is_some();
    has_tabs.then_some(dock)
}

struct Viewer<'a> {
    cx: Cx<'a>,
    /// The key of the tab the user is working in.
    front: Option<String>,
    /// How many tabs share each tab's pane, by key, for what its menu can offer.
    pane_sizes: std::collections::HashMap<String, usize>,
    panes: usize,
    zoomed: bool,
}

/// A tab's title with the icon of its kind before it. The text takes the dock's colour for
/// the tab's state; a dot for unsaved edits takes the accent.
fn tab_title(tab: &Tab, p: &Palette) -> egui::WidgetText {
    use egui::text::{LayoutJob, TextFormat};
    let italic = matches!(tab, Tab::Diff(d) if !d.pinned) || matches!(tab, Tab::Editor(e) if !e.pinned && !e.path.is_empty());
    let font = egui::FontId::proportional(13.0);
    let mut job = LayoutJob::default();
    job.append(chrome::tab_glyph(tab), 0.0, TextFormat { font_id: font.clone(), color: p.dim, ..Default::default() });
    let title = tab.title();
    let (dot, rest) = match title.strip_prefix("\u{25CF} ") {
        Some(rest) => (true, rest.to_string()),
        None => (false, title),
    };
    job.append(&rest, 6.0, TextFormat { font_id: font.clone(), color: Color32::PLACEHOLDER, italics: italic, ..Default::default() });
    if dot {
        job.append("\u{25CF}", 6.0, TextFormat { font_id: egui::FontId::proportional(10.0), color: p.accent, valign: egui::Align::Center, ..Default::default() });
    }
    job.into()
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    /// With the repository in it: egui keeps undo history and scroll positions by id, and
    /// one repository's message mustn't come back with Ctrl+Z in another's commit box.
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new((&self.cx.repo.root, tab.key()))
    }

    /// Slanted for a preview, as a diff or a file clicked in IDE mode is reused for the next
    /// one unless it's been kept.
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab_title(tab, self.cx.pal)
    }

    /// A double click on a preview's tab keeps it, as Keep open in its menu does, and as a
    /// double click in the Files tree does. A click just before it makes it a triple to egui.
    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        if response.double_clicked() || response.triple_clicked() {
            match tab {
                Tab::Diff(d) => d.pinned = true,
                Tab::Editor(e) if !e.path.is_empty() => e.pinned = true,
                _ => {}
            }
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, tab: &mut Tab, _path: NodePath) {
        use crate::layouts::{Closing, DockOp};
        let key = tab.key();
        let mut op = None;
        let preview = matches!(tab, Tab::Diff(d) if !d.pinned) || matches!(tab, Tab::Editor(e) if !e.pinned && !e.path.is_empty());
        if preview && ui.button("Keep open").on_hover_text("Don't reuse this tab for the next one").clicked() {
            op = Some(DockOp::Keep);
        }
        let alone = self.pane_sizes.get(&key).copied().unwrap_or(1) < 2;
        let why = "It's the only tab in its pane.";
        if ui.add_enabled(!alone, egui::Button::new("Split right")).on_disabled_hover_text(why).clicked() {
            op = Some(DockOp::Split(egui_dock::Split::Right));
        }
        if ui.add_enabled(!alone, egui::Button::new("Split down")).on_disabled_hover_text(why).clicked() {
            op = Some(DockOp::Split(egui_dock::Split::Below));
        }
        let label = if self.zoomed { "Restore layout" } else { "Fill the window" };
        if ui.add_enabled(self.zoomed || self.panes > 1, egui::Button::new(label)).on_disabled_hover_text("There's only one pane.").on_hover_text(crate::keys::chord_label(&self.cx.settings.key_bindings, "zoom_pane")).clicked() {
            op = Some(DockOp::Zoom);
        }
        ui.separator();
        if ui.add_enabled(!alone, egui::Button::new("Close others")).on_disabled_hover_text(why).clicked() {
            op = Some(DockOp::CloseMany(Closing::Others));
        }
        if ui.button("Close to the right").clicked() {
            op = Some(DockOp::CloseMany(Closing::ToTheRight));
        }
        if ui.button("Close all in this pane").clicked() {
            op = Some(DockOp::CloseMany(Closing::All));
        }
        let path = match tab {
            Tab::Editor(e) if !e.path.is_empty() => Some(e.path.clone()),
            Tab::File(f) => Some(f.path.clone()),
            _ => None,
        };
        if let Some(path) = path {
            ui.separator();
            if ui.button("Copy path").clicked() {
                self.cx.copy(path);
                ui.close();
            }
        }
        if let Some(op) = op {
            self.cx.out.push(Action::Dock { key, op });
            ui.close();
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        egui::Frame::new().inner_margin(6.0).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            self.cx.in_front = self.front.as_deref().is_none_or(|k| k == tab.key());
            tab.ui(ui, &mut self.cx);
        });
    }

    /// gitem has one window with its own title bar: a tab torn off into a window of its own
    /// would have neither.
    fn allowed_in_windows(&self, _tab: &mut Tab) -> bool {
        false
    }

    fn on_add(&mut self, _path: NodePath) {
        self.cx.out.push(Action::OpenPalette);
    }

    fn on_close(&mut self, tab: &mut Tab) -> OnCloseResponse {
        match tab.unsaved() {
            Some(lost) => {
                self.cx.out.push(Action::AskCloseTab { key: tab.key(), lost });
                OnCloseResponse::Focus
            }
            None => OnCloseResponse::Close,
        }
    }
}

/// A click in the moments after a dialog appears wasn't aimed at it: it's the second half of
/// a double click, or it was on its way to something that was there before.
const DIALOG_SETTLES: f64 = 0.2;

/// How every dialog is put on screen. They sit above the backdrop that dims the app.
fn dialog_window<'a>(title: impl Into<egui::WidgetText>, settled: bool) -> egui::Window<'a> {
    egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).order(egui::Order::Foreground).interactable(settled)
}

/// Dims everything behind a dialog and keeps clicks and the keyboard off it. The window's own
/// controls stay live, and its title bar still moves it, as there's no other way to do those.
fn dialog_backdrop(ctx: &egui::Context, controls_width: f32) {
    let screen = ctx.content_rect();
    let dim = Color32::from_black_alpha(90);
    let top = egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x - controls_width, screen.min.y + chrome::TITLE_BAR_HEIGHT));
    let rest = egui::Rect::from_min_max(egui::pos2(screen.min.x, top.max.y), screen.max);
    for (id, rect, is_title) in [("dialog-backdrop", rest, false), ("dialog-backdrop-title", top, true)] {
        let area = egui::Area::new(id.into()).order(egui::Order::Middle).fixed_pos(rect.min).constrain(false).sense(Sense::click_and_drag()).interactable(true);
        if !is_title {
            ctx.memory_mut(|m| m.set_modal_layer(area.layer()));
        }
        let shown = area.show(ctx, |ui| {
            ui.painter().rect_filled(rect, 0.0, dim);
            ui.allocate_rect(rect, Sense::click_and_drag())
        });
        if is_title && shown.inner.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }
}

/// Git's own message for a missing folder blames git: "The directory name is invalid".
fn could_not_open(path: &Path, why: &str) -> String {
    if path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir() {
        format!("{} is a bare repository: it has no files checked out to show. Clone it, or add a worktree of it from a terminal and open that.", path.display())
    } else if path.exists() {
        format!("{} isn't a git repository:\n{why}", path.display())
    } else {
        format!("{} doesn't exist. It may have been moved, renamed or deleted.", path.display())
    }
}

/// How many folders down Scan for repositories looks.
const SCAN_DEPTH: usize = 4;

struct RepoScan {
    root: PathBuf,
    task: crate::task::Task<Vec<PathBuf>>,
    cancel: Arc<AtomicBool>,
    found: Vec<PathBuf>,
    picked: Vec<bool>,
    done: bool,
}

/// What a yes in the close dialog goes on to close.
enum CloseTarget {
    Tab { root: PathBuf, key: String },
    Repo(PathBuf),
    Window,
}

/// A git command that would rewrite files open with unsaved edits, held until the user says
/// whether to save them first.
struct DirtyGuard {
    root: PathBuf,
    files: Vec<String>,
    action: Action,
}

struct CloseGuard {
    target: CloseTarget,
    /// Everything that would be lost, one line each.
    items: Vec<String>,
}

/// A write failed on a lock, or a cancelled one left it behind.
struct LockPrompt {
    root: PathBuf,
    why: String,
    /// Removal was refused, because a git process is running.
    busy: bool,
}

/// An op from a repository tab that's been closed. Its result still gets reported.
struct Orphan {
    repo: String,
    op: Running,
}

struct Confirm {
    title: String,
    body: String,
    label: String,
    op: GitOp,
    /// The repository this was asked about. Dialogs don't block switching tabs.
    repo: Option<PathBuf>,
}

/// Unattended capture for visual checks: `--shot file.png`.
pub struct Shot {
    pub path: PathBuf,
    pub at: Instant,
    pub sent: bool,
    pub script: Vec<String>,
}

pub struct GitemApp {
    pub git: Result<GitExe, String>,
    pub settings: Settings,
    pal: Palette,
    pub repos: Vec<RepoTab>,
    pub active: usize,
    form: Option<FormDialog>,
    form_repo: Option<PathBuf>,
    confirm: Option<Confirm>,
    errors: Vec<String>,
    toasts: Vec<(String, Instant)>,
    palette: Option<PaletteState>,
    global_queue: VecDeque<(GitOp, Option<PathBuf>)>,
    global_running: Option<Running>,
    applied: Option<(gitem_core::settings::ThemeChoice, u32, String)>,
    /// egui's zoom as last seen, to tell its own Ctrl+Plus from ours still waiting to apply.
    seen_zoom: Option<f32>,
    open_path: String,
    settings_dirty: bool,
    /// Looking for repositories in a folder, and what was found.
    repo_scan: Option<RepoScan>,
    /// The CA file setting last handed to the HTTP clients.
    ca_file_applied: String,
    /// Later launches hand their paths over through this, when that's turned on.
    instance: Option<gitem_core::instance::Instance>,
    /// A look for a newer release, by schedule or from About.
    update_task: crate::task::Task<Option<gitem_core::update::Release>>,
    /// From About, so its answer is shown even when that's "no".
    update_asked: bool,
    /// Whether this run has looked at the schedule. Once is enough.
    update_scheduled: bool,
    update_found: Option<gitem_core::update::Release>,
    update_answer: Option<String>,
    pub shot: Option<Shot>,
    pub shot_tabs: Vec<Tab>,
    pub shot_inspect: Option<String>,
    /// Whether settings and the session are saved. Off for tests and screenshots.
    pub persist: bool,
    about_open: bool,
    about_git: Option<String>,
    /// Reports from earlier crashes that nobody has looked at, newest first.
    pub crash_reports: Vec<PathBuf>,
    /// Git is older than gitem needs, and the user hasn't dismissed this yet.
    git_warning: Option<String>,
    #[cfg(windows)]
    styled_window: bool,
    /// What a palette or a view needs with no repository open, made the first time one does.
    no_repo: Option<Box<NoRepo>>,
    /// The view showing in place of the welcome page: only those that need no repository.
    no_repo_tab: Option<Tab>,
    /// The settings as last read or written, to merge with what another copy changed.
    settings_baseline: Option<serde_json::Value>,
    /// Session repositories not open in this copy, kept for next time. It was started for
    /// a different repository, or they couldn't be reached.
    session_kept: Vec<String>,
    /// Which of those couldn't be reopened and haven't been asked about yet.
    reopen_failed: Vec<String>,
    lock_prompt: Option<LockPrompt>,
    /// A link to a folder gitem doesn't know, waiting for the user to say yes.
    link_prompt: Option<gitem_core::forge::DeepLink>,
    /// Whether git is running anywhere, before removing a lock. Tests replace it, because
    /// tests running side by side always have git running somewhere.
    pub git_running: fn() -> bool,
    /// How a notification reaches the system. Tests replace it, as the real one shows on the desktop.
    pub notifier: crate::notify::Notifier,
    /// Which dialogs are up, and when that last changed.
    dialogs_seen: (String, f64),
    dialogs_settled: bool,
    /// The same, for the error showing.
    errors_seen: (String, f64),
    errors_settled: bool,
    close_guard: Option<CloseGuard>,
    dirty_guard: Option<DirtyGuard>,
    /// When Ctrl+K was pressed, for moving to a pane with an arrow after it.
    pane_chord: Option<f64>,
    /// IDE mode as it was last frame, to notice it being turned on.
    ide_was: bool,
    /// IDE mode was just turned on: a bar offers the layout that goes with it.
    ide_offer: bool,
    /// The next action runs without asking about unsaved edits: the user just answered.
    dirty_answered: bool,
    /// The user has said yes to losing work, so the next close request goes through.
    closing: bool,
    orphans: Vec<Orphan>,
    /// Counts frames: what's asked for within one is one action.
    frame_no: u64,
    window_focused: bool,
    /// Repositories still being opened on their own threads. A share that isn't there can
    /// take 20 seconds to say so.
    opening: Vec<Opening>,
    /// A folder being picked: the dialog runs on its own thread, so this one keeps drawing.
    folder_pick: crate::task::Task<Option<PathBuf>>,
    /// The repository whose folder went missing, while its new place is being picked.
    relocating: Option<PathBuf>,
    /// The list of keys is open, with what's been typed to narrow it.
    help: Option<String>,
}

struct Opening {
    path: PathBuf,
    task: crate::task::Task<Repo>,
    /// From the saved session: if it can't be reached, it's only kept for next time.
    quiet: bool,
    /// A link to follow once it's open.
    then: Option<gitem_core::forge::DeepLink>,
}

struct NoRepo {
    repo: Repo,
    data: RepoData,
}

/// A view that can show with no repository, in the form that needs none, or None if it can't.
/// Every other view reads the repository from its first frame.
fn without_repo(tab: Tab) -> Option<Tab> {
    match tab {
        Tab::Settings(_) => Some(tab),
        Tab::Credentials(_) => Some(Tab::Credentials(Box::new(crate::views::credentials::CredentialsView::outside_repo()))),
        Tab::Launchpad(_) => Some(Tab::Launchpad(Box::new(crate::views::launchpad::LaunchpadView::everything_only()))),
        _ => None,
    }
}

/// Version, commit, and whether the build had uncommitted changes.
pub fn version_line() -> String {
    let sha = env!("GITEM_BUILD_SHA");
    let commit = if sha.is_empty() { "unknown commit" } else { sha };
    let dirty = if env!("GITEM_BUILD_DIRTY").is_empty() { "" } else { ", with uncommitted changes" };
    format!("{} ({commit}{dirty})", env!("CARGO_PKG_VERSION"))
}

impl GitemApp {
    pub fn new(ctx: &egui::Context, paths: &[PathBuf]) -> Self {
        let loaded = Settings::load_checked();
        let mut app = Self::with_settings(ctx, loaded.settings);
        app.settings_baseline = Some(loaded.baseline);
        app.errors.extend(loaded.problem);
        if let Ok(git) = &app.git {
            crate::crash::watch_git_log(git.log.clone());
        }
        app.crash_reports = crate::crash::pending(&gitem_core::settings::config_dir());
        let session = app.settings.open_repos.clone();
        if paths.is_empty() {
            app.reopen_session(ctx, &session);
        } else {
            // `gitem some-repo` shows that repository, and the session stays for next time.
            app.session_kept = session;
            for p in paths {
                app.open_repo(ctx, p, false);
            }
        }
        app.active = 0;
        app.persist = !loaded.save_blocked;
        app
    }

    /// An app that neither restores nor saves session state.
    pub fn with_settings(ctx: &egui::Context, settings: Settings) -> Self {
        theme::install_fonts(ctx);
        let git = GitExe::discover((!settings.git_path.is_empty()).then(|| Path::new(&settings.git_path))).map_err(|e| e.to_string());
        let pal = Palette::for_settings(settings.theme, &settings.accent);
        let git_warning = git.as_ref().ok().and_then(GitExe::version_warning);
        // Already on at start isn't being turned on: no offer.
        let ide_was = settings.ide_mode;
        Self {
            git_warning,
            git,
            settings,
            pal,
            repos: Vec::new(),
            active: 0,
            form: None,
            form_repo: None,
            confirm: None,
            errors: Vec::new(),
            toasts: Vec::new(),
            palette: None,
            global_queue: VecDeque::new(),
            global_running: None,
            applied: None,
            seen_zoom: None,
            open_path: String::new(),
            settings_dirty: false,
            repo_scan: None,
            ca_file_applied: String::new(),
            instance: None,
            update_task: Default::default(),
            update_asked: false,
            update_scheduled: false,
            update_found: None,
            update_answer: None,
            shot: None,
            shot_tabs: Vec::new(),
            shot_inspect: None,
            persist: false,
            about_open: false,
            about_git: None,
            crash_reports: Vec::new(),
            #[cfg(windows)]
            styled_window: false,
            no_repo: None,
            no_repo_tab: None,
            settings_baseline: None,
            session_kept: Vec::new(),
            reopen_failed: Vec::new(),
            lock_prompt: None,
            link_prompt: None,
            git_running: gitem_core::runner::git_processes_running,
            notifier: crate::notify::show,
            dialogs_seen: (String::new(), 0.0),
            dialogs_settled: true,
            errors_seen: (String::new(), 0.0),
            errors_settled: true,
            close_guard: None,
            dirty_guard: None,
            pane_chord: None,
            ide_was,
            ide_offer: false,
            dirty_answered: false,
            closing: false,
            orphans: Vec::new(),
            frame_no: 0,
            window_focused: true,
            opening: Vec::new(),
            folder_pick: Default::default(),
            relocating: None,
            help: None,
        }
    }

    /// The errors waiting to be seen, oldest first.
    /// Sets a field of the open form as typing or choosing would. For tests.
    pub fn form_set_for_test(&mut self, key: &str, value: &str) {
        if let Some(f) = &mut self.form {
            f.set_for_test(key, value);
        }
    }

    /// Swaps two neighbouring repository tabs. The one showing stays the one showing.
    pub fn move_repo_tab(&mut self, from: usize, to: usize) {
        if from.max(to) >= self.repos.len() {
            return;
        }
        self.repos.swap(from, to);
        if self.active == from {
            self.active = to;
        } else if self.active == to {
            self.active = from;
        }
        self.settings_dirty = true;
    }

    /// The version a look for updates found, if any.
    pub fn update_found_version(&self) -> Option<&str> {
        self.update_found.as_ref().map(|r| r.version.as_str())
    }

    /// Opens About, as its button would. For tests.
    pub fn apply_about_for_test(&mut self) {
        self.about_open = true;
    }

    /// Dismisses what's showing, as its window would. For tests.
    pub fn errors_clear_for_test(&mut self) {
        self.errors.clear();
    }

    pub fn errors_showing(&self) -> &[String] {
        &self.errors
    }

    /// The way out of a lock that's in the way. Removing one that a live process holds can
    /// damage the index, so it's only ever done on request, with no git process running.
    fn lock_dialog(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &self.lock_prompt else { return };
        let repo = self.repos.iter().find(|r| r.repo.root == prompt.root);
        let lock = repo.and_then(|r| r.repo.index_lock());
        let Some(lock) = lock else {
            // Gone by itself, or the repository was closed. Nothing left to decide.
            self.lock_prompt = None;
            return;
        };
        let age = std::fs::metadata(&lock).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
        let (mut remove, mut leave) = (false, false);
        let warn = self.pal.warn;
        dialog_window("The repository is locked", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.add(egui::Label::new(RichText::new(prompt.why.as_str()).monospace().small()).selectable(true));
            ui.add_space(6.0);
            ui.label("git refuses to change anything while this file exists:");
            ui.add(egui::Label::new(RichText::new(lock.display().to_string()).monospace().small()).selectable(true));
            if let Some(s) = age {
                ui.label(format!("It was last changed {} ago.", plural(s as usize, "second", "seconds")));
            }
            ui.add_space(4.0);
            ui.label("It may belong to another git program working here, such as an editor or a terminal. Or git may have stopped without cleaning up, which is what cancelling does: tracked files can then be left half updated, so look through Changes afterwards.");
            if prompt.busy {
                ui.add_space(4.0);
                ui.colored_label(warn, "A git process is running on this computer, so the lock may be in use. Try again when it has finished.");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                remove = ui.button("Remove the lock").clicked();
                leave = ui.button("Leave it").clicked();
            });
        });
        if leave {
            self.lock_prompt = None;
        } else if remove {
            // Asked at the click, and the repository's own queue counts too.
            let own_busy = repo.is_some_and(|r| r.running.is_some());
            if own_busy || (self.git_running)() {
                if let Some(p) = &mut self.lock_prompt {
                    p.busy = true;
                }
                return;
            }
            self.lock_prompt = None;
            match std::fs::remove_file(&lock) {
                Ok(()) => self.toasts.push(("Lock removed. Try that again.".to_string(), Instant::now())),
                Err(e) => self.errors.push(format!("Could not remove {}: {e}", lock.display())),
            }
        }
    }

    /// Opens the session's repositories side by side, in their saved order. Normally all of
    /// them are done within moments. Any that aren't arrive by themselves when they answer,
    /// and meanwhile the window is up and usable.
    fn reopen_session(&mut self, ctx: &egui::Context, session: &[String]) {
        const WAIT: Duration = Duration::from_secs(2);
        let Ok(git) = self.git.clone() else {
            self.session_kept.extend(session.iter().cloned());
            return;
        };
        let mut tasks: Vec<(String, crate::task::Task<Repo>)> = session
            .iter()
            .map(|p| {
                let mut task = crate::task::Task::default();
                let (git, path) = (git.clone(), PathBuf::from(p));
                task.spawn(ctx, move || Repo::open(git, &path));
                (p.clone(), task)
            })
            .collect();
        let start = Instant::now();
        while start.elapsed() < WAIT && tasks.iter().any(|(_, t)| t.busy()) {
            std::thread::sleep(Duration::from_millis(5));
            tasks.iter_mut().for_each(|(_, t)| {
                t.poll();
            });
        }
        for (p, mut task) in tasks {
            if let Some(repo) = task.value.take() {
                self.add_repo(ctx, repo);
            } else if task.busy() {
                // Kept in the session meanwhile, in case the window closes first.
                self.session_kept.push(p.clone());
                self.opening.push(Opening { path: p.into(), task, quiet: true, then: None });
            } else {
                self.session_kept.push(p.clone());
                self.reopen_failed.push(p);
            }
        }
    }

    /// Repositories that have answered since the last frame. They don't take the focus.
    fn poll_opening(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        self.opening.retain_mut(|o| {
            if !o.task.poll() {
                return true;
            }
            done.push((o.path.clone(), o.task.value.take(), o.task.error.take(), o.quiet, o.then.take()));
            false
        });
        for (path, repo, error, quiet, then) in done {
            let was_empty = self.repos.is_empty();
            let shown = self.active;
            let name = path.to_string_lossy().replace('\\', "/");
            match repo {
                Some(repo) => {
                    self.session_kept.retain(|k| *k != name);
                    let before = self.repos.len();
                    self.add_repo(ctx, repo);
                    let added = self.repos.len() > before;
                    // One already open has just been made active, as whoever asked would want.
                    if added {
                        self.active = if was_empty || !quiet { self.repos.len() - 1 } else { shown };
                    } else if quiet {
                        self.active = shown;
                    }
                    if let Some(link) = then {
                        self.follow_link(ctx, link);
                    }
                }
                None if quiet => self.reopen_failed.push(name),
                None => self.errors.push(error.unwrap_or_default()),
            }
        }
    }

    /// The same, for one the user asked for. Off the UI thread whatever it looks like: a mapped
    /// drive or a sleeping disk can take as long as a share to answer, and no path says so.
    fn open_in_background(&mut self, ctx: &egui::Context, path: PathBuf, then: Option<gitem_core::forge::DeepLink>) {
        let Ok(git) = self.git.clone() else { return };
        let mut task = crate::task::Task::default();
        let p = path.clone();
        task.spawn(ctx, move || Repo::open(git, &p).map_err(|e| could_not_open(&p, &e.to_string())));
        self.opening.push(Opening { path, task, quiet: false, then });
    }

    fn add_repo(&mut self, ctx: &egui::Context, repo: Repo) {
        if let Some(i) = self.repos.iter().position(|r| r.repo.root == repo.root) {
            self.active = i;
            return;
        }
        let root = repo.root.to_string_lossy().replace('\\', "/");
        self.session_kept.retain(|k| *k != root);
        self.settings.touch_recent(&root);
        self.no_repo_tab = None;
        let mut tab = RepoTab::new(ctx, repo, &self.settings);
        tab.maintenance_due = self.settings.auto_maintenance;
        self.repos.push(tab);
        self.active = self.repos.len() - 1;
        self.settings_dirty = true;
    }

    /// Whether the repository is open now.
    pub fn open_repo(&mut self, ctx: &egui::Context, path: &Path, quiet: bool) -> bool {
        let Ok(git) = self.git.clone() else { return false };
        match Repo::open(git, path) {
            Ok(repo) => {
                self.add_repo(ctx, repo);
                true
            }
            Err(e) => {
                if !quiet {
                    self.errors.push(could_not_open(path, &e.to_string()));
                }
                false
            }
        }
    }

    /// Whether the user has opened this folder before. Anything else gets asked about.
    fn knows_path(&self, path: &str) -> bool {
        let norm = |p: &str| {
            let p = p.replace('\\', "/");
            let p = p.trim_end_matches('/');
            if cfg!(windows) { p.to_lowercase() } else { p.to_string() }
        };
        let want = norm(path);
        self.settings.recent_repos.iter().chain(&self.settings.open_repos).any(|r| norm(r) == want) || self.repos.iter().any(|r| norm(&r.repo.root.to_string_lossy()) == want)
    }

    /// The rest of a link only makes sense in its own repository, so this comes once that's open.
    fn follow_link(&mut self, ctx: &egui::Context, d: gitem_core::forge::DeepLink) {
        if let Some(c) = &d.commit {
            self.apply_action(ctx, Action::Inspect(c.clone()));
        }
        if let Some(f) = &d.file {
            self.apply_action(ctx, Action::OpenTab(Tab::file(f, d.commit.clone(), d.line)));
        }
    }

    fn link_dialog(&mut self, ctx: &egui::Context) {
        let Some(link) = self.link_prompt.clone() else { return };
        let (mut open, mut cancel) = (false, false);
        dialog_window("Open this repository?", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label("A link is asking gitem to open this folder, which you haven't opened in gitem before:");
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&link.repo).monospace()).selectable(true));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                open = ui.button("Open it").clicked();
                cancel = ui.button("Don't open").clicked();
            });
        });
        if open {
            self.link_prompt = None;
            self.open_in_background(ctx, PathBuf::from(&link.repo), Some(link));
        } else if cancel {
            self.link_prompt = None;
        }
    }

    /// Whether the active repository needs an answer about what its config may run.
    fn trust_question_open(&self) -> bool {
        self.repos.get(self.active).is_some_and(|r| r.repo.trust.state == TrustState::Undecided || (r.review_trust && r.repo.trust.restricted()))
    }

    /// Its programs stay off until there's a yes. Nothing here depends on an answer coming.
    fn trust_dialog(&mut self, ctx: &egui::Context) {
        if !self.trust_question_open() {
            return;
        }
        let settled = self.dialogs_settled;
        let Some(tab) = self.repos.get_mut(self.active) else { return };
        let (mut allow, mut keep_off) = (false, false);
        let warn = self.pal.warn;
        let dim = self.pal.dim;
        dialog_window("This repository's config can run programs", settled).show(ctx, |ui| {
            ui.set_max_width(640.0);
            ui.add(egui::Label::new(RichText::new(tab.repo.root.display().to_string()).monospace()).selectable(true));
            ui.add_space(4.0);
            ui.label("These settings came with this folder, not from your own git config. Git would run them just to read the repository, so gitem hasn't. Only let them run if you trust where this folder came from.");
            ui.add_space(6.0);
            const SHOWN: usize = 100;
            egui::ScrollArea::vertical().id_salt("trust-keys").max_height(220.0).show(ui, |ui| {
                for r in tab.repo.trust.risky.iter().take(SHOWN) {
                    ui.add(egui::Label::new(RichText::new(r.describe()).monospace().color(warn)).selectable(true));
                }
                let more = tab.repo.trust.risky.len().saturating_sub(SHOWN);
                if more > 0 {
                    ui.label(format!("And {more} more."));
                }
            });
            if let Some(h) = &tab.repo.trust.hooks_path {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("For information: core.hooksPath = {h}. Reading never runs hooks. Commits and other changes you start do, as in git, whatever you answer here.")).small().color(dim));
            }
            ui.add_space(6.0);
            ui.label(RichText::new(RESTRICTED_NOTE).small().color(dim));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                keep_off = ui.button("Keep them off").clicked();
                allow = ui.button("Let them run").clicked();
            });
        });
        if allow || keep_off {
            tab.review_trust = false;
            if let Err(e) = tab.repo.set_trusted(allow) {
                self.errors.push(format!("Could not save that answer: {e}"));
            }
            tab.data.refresh(ctx, &tab.repo);
            tab.data.mark_rewritten();
        }
    }

    fn save_settings(&mut self) {
        if !self.persist {
            return;
        }
        self.settings.open_repos = self.session_kept.iter().cloned().chain(self.repos.iter().map(|r| r.repo.root.to_string_lossy().replace('\\', "/"))).collect();
        for r in &self.repos {
            let tabs: Vec<String> = r.all_tabs().filter_map(|t| t.spec()).collect();
            let root = r.repo.root.to_string_lossy().replace('\\', "/");
            self.settings.session_tabs.insert(root.clone(), tabs);
            // A zoomed pane is a moment's view: the layout kept is the one from before it.
            if r.zoom.is_none()
                && let Some(j) = layout_to_json(&r.dock)
            {
                self.settings.session_layouts.insert(root, j);
            }
        }
        let settings = &mut self.settings;
        let baseline = self.settings_baseline.get_or_insert_with(|| serde_json::to_value(Settings::default()).unwrap_or_default());
        match settings.save_merged(baseline) {
            Ok(note) => self.errors.extend(note),
            Err(e) => self.errors.push(format!("Could not save settings: {e}")),
        }
    }

    /// Session repositories that couldn't be opened stay in the session, in case it's
    /// only an unplugged drive, until the user says otherwise.
    fn reopen_dialog(&mut self, ctx: &egui::Context) {
        if self.reopen_failed.is_empty() {
            return;
        }
        let (mut forget, mut keep) = (false, false);
        dialog_window("Some repositories couldn't be reopened", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(520.0);
            for p in &self.reopen_failed {
                ui.add(egui::Label::new(RichText::new(p).monospace().small()).selectable(true));
            }
            ui.add_space(4.0);
            ui.label("They may be on a drive that isn't connected. They'll be tried again next time.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                keep = ui.button("Keep for next time").clicked();
                forget = ui.button("Forget them").clicked();
            });
        });
        if forget {
            let gone = std::mem::take(&mut self.reopen_failed);
            self.session_kept.retain(|k| !gone.contains(k));
            self.settings_dirty = true;
        } else if keep {
            self.reopen_failed.clear();
        }
    }

    /// No git write is queued or running anywhere.
    pub fn ops_idle(&self) -> bool {
        self.global_queue.is_empty() && self.global_running.is_none() && self.orphans.is_empty() && self.repos.iter().all(|r| r.queue.is_empty() && r.running.is_none() && r.quiet.is_none())
    }

    /// Everything closing the window would lose. Empty means nothing to ask about.
    pub fn window_at_stake(&self) -> Vec<String> {
        let several = self.repos.len() > 1;
        let mut items = Vec::new();
        for r in &self.repos {
            for item in r.at_stake("It stops after its current step.") {
                items.push(if several { format!("{}: {item}", r.repo.name()) } else { item });
            }
        }
        for o in self.orphans.iter().filter(|o| !o.op.harmless) {
            items.push(format!("{}: {} is still finishing. It stops after its current step.", o.repo, o.op.title));
        }
        if let Some(g) = self.global_running.as_ref().filter(|g| !g.harmless) {
            items.push(format!("{} is running. It stops after its current step.", g.title));
        }
        if !self.global_queue.is_empty() {
            items.push(format!("{} won't run.", plural(self.global_queue.len(), "queued git command", "queued git commands")));
        }
        if let Some(f) = self.form.as_ref().filter(|f| f.touched()) {
            items.push(format!("The \"{}\" dialog, with what's been typed in it.", f.title));
        }
        items
    }

    /// Closes a repository's tab, or asks first if that would lose something.
    pub fn request_close_repo(&mut self, i: usize) {
        let Some(r) = self.repos.get(i) else { return };
        let mut items = r.at_stake("It finishes in the background, and a failure is still reported.");
        if self.form_repo.as_ref() == Some(&r.repo.root)
            && let Some(f) = self.form.as_ref().filter(|f| f.touched())
        {
            items.push(format!("The \"{}\" dialog, with what's been typed in it.", f.title));
        }
        if items.is_empty() {
            self.close_repo(i);
        } else {
            self.close_guard = Some(CloseGuard { target: CloseTarget::Repo(r.repo.root.clone()), items });
        }
    }

    fn close_repo(&mut self, i: usize) {
        let mut r = self.repos.remove(i);
        // The worker can't be stopped mid-step, and nobody should miss that it failed.
        if let Some(op) = r.running.take() {
            self.orphans.push(Orphan { repo: r.repo.name(), op });
        }
        if self.form_repo.as_ref() == Some(&r.repo.root) {
            self.form = None;
            self.form_repo = None;
        }
        // The one on show stays on show, wherever it now is in the list.
        if i < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.repos.len().saturating_sub(1));
        self.settings_dirty = true;
    }

    /// Results of ops whose repository tab has closed.
    fn poll_orphans(&mut self) {
        let mut errors = Vec::new();
        self.orphans.retain_mut(|o| {
            loop {
                match o.op.rx.try_recv() {
                    Ok(OpMsg::Line(_)) => {}
                    Ok(OpMsg::Done(res)) => {
                        if let Err(e) = res {
                            errors.push(format!("{} in {} failed:\n{e}", o.op.title, o.repo));
                        }
                        return false;
                    }
                    Ok(OpMsg::Undo(_) | OpMsg::NoUndo(_)) => {}
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => return false,
                }
            }
        });
        self.errors.extend(errors);
    }

    /// The window is asked to close, by its own button or the system. Nothing at stake
    /// means it just closes, with no dialog in the way.
    fn check_window_close(&mut self, ctx: &egui::Context) {
        if self.closing || self.shot.is_some() || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let items = self.window_at_stake();
        if !items.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_guard = Some(CloseGuard { target: CloseTarget::Window, items });
        }
    }

    /// Asks what to do with unsaved edits before a git command rewrites the files.
    fn dirty_dialog(&mut self, ctx: &egui::Context) {
        let Some(g) = &self.dirty_guard else { return };
        let (mut save, mut go, mut stay) = (false, false, ctx.input(|i| i.key_pressed(egui::Key::Escape)));
        let title = match &g.action {
            Action::Run(op) | Action::RunThen(op, _) | Action::RunThenOpen(op, _) => op.title.clone(),
            _ => String::new(),
        };
        dialog_window("Save your edits first?", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(format!("{title} can change files on disk. These have edits that aren't saved:"));
            for f in &g.files {
                ui.label(format!("•  {f}"));
            }
            ui.label(RichText::new("Without saving, the editor keeps your text and says when the file on disk is different.").small().color(self.pal.dim));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                save = ui.button("Save all and continue").clicked();
                go = ui.button("Continue without saving").clicked();
                stay |= ui.button("Cancel").clicked();
            });
        });
        if save {
            let Some(g) = self.dirty_guard.take() else { return };
            let mut out = Vec::new();
            let mut saved = true;
            if let Some(r) = self.repos.iter_mut().find(|r| r.repo.root == g.root) {
                let RepoTab { repo, data, dock, zoom, .. } = r;
                let mut cx = Cx { repo, data, settings: &self.settings, pal: &self.pal, out: &mut out, dialog_open: true, in_front: false };
                for (_, t) in dock.iter_all_tabs_mut().chain(zoom.iter_mut().flat_map(|z| z.full.iter_all_tabs_mut())) {
                    if let Tab::Editor(e) = t
                        && e.is_dirty()
                    {
                        saved &= e.save(&mut cx);
                    }
                }
            }
            for a in out {
                self.apply_action(ctx, a);
            }
            // A file that changed on disk can't be saved over; the command waits for that.
            if saved {
                self.dirty_answered = true;
                self.apply_action(ctx, g.action);
            } else {
                self.toasts.push(("Not run: a file couldn't be saved.".to_string(), Instant::now()));
            }
        } else if go {
            let Some(g) = self.dirty_guard.take() else { return };
            self.dirty_answered = true;
            self.apply_action(ctx, g.action);
        } else if stay {
            self.dirty_guard = None;
        }
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let Some(g) = &self.close_guard else { return };
        let (mut go, mut stay) = (false, ctx.input(|i| i.key_pressed(egui::Key::Escape)));
        let danger = self.pal.danger;
        let dim = self.pal.dim;
        dialog_window("Close with work not saved?", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(RichText::new("Closing now loses:").color(dim));
            for item in &g.items {
                ui.label(format!("•  {item}"));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                go = ui.add(egui::Button::new(RichText::new("Close anyway").color(theme::ink_on(danger))).fill(danger)).clicked();
                stay |= ui.button("Keep it open").clicked();
            });
        });
        if go {
            let Some(g) = self.close_guard.take() else { return };
            // The dialog said this is lost, so the draft kept against a crash goes with it.
            match g.target {
                CloseTarget::Tab { root, key } => {
                    if let Some(r) = self.repos.iter_mut().find(|r| r.repo.root == root)
                        && let Some(path) = r.dock.find_tab_from(|t| t.key() == key)
                    {
                        r.dock.remove_tab(path);
                    }
                    if key == "changes" {
                        crate::views::changes::forget_draft(&root);
                    }
                }
                CloseTarget::Repo(root) => {
                    crate::views::changes::forget_draft(&root);
                    if let Some(i) = self.repos.iter().position(|r| r.repo.root == root) {
                        self.close_repo(i);
                    }
                }
                CloseTarget::Window => {
                    for r in &self.repos {
                        crate::views::changes::forget_draft(&r.repo.root);
                    }
                    self.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        } else if stay {
            self.close_guard = None;
        }
    }

    /// No folder picker is open or waiting.
    pub fn folder_pick_idle(&self) -> bool {
        !self.folder_pick.busy()
    }

    pub fn palette_is_open(&self) -> bool {
        self.palette.is_some()
    }

    /// Opens the palette with text typed, for screenshots.
    pub fn open_palette_with(&mut self, query: &str) {
        self.palette = Some(PaletteState::with_query(query));
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let want = (self.settings.theme, (self.settings.ui_scale * 100.0) as u32, format!("{}:{}:{}", self.settings.accent, self.settings.colorblind_diff, self.settings.reduce_motion));
        if self.applied.as_ref() != Some(&want) {
            self.applied = Some(want);
            self.pal = Palette::for_settings(self.settings.theme, &self.settings.accent).colorblind(self.settings.colorblind_diff);
            theme::apply(ctx, &self.pal, self.settings.ui_scale, self.settings.reduce_motion);
        }
        crate::i18n::set_language(&self.settings.language);
    }

    /// Run something on this repository's graph, opening the graph if needed.
    fn with_graph(&mut self, f: impl FnOnce(&mut crate::views::graph::GraphView)) {
        let Some(r) = self.repos.get_mut(self.active) else { return };
        r.open_tab(Tab::graph());
        for (_, t) in r.dock.iter_all_tabs_mut() {
            if let Tab::Graph(g) = t {
                f(g);
                return;
            }
        }
    }

    pub fn apply_action(&mut self, ctx: &egui::Context, a: Action) {
        // A command that rewrites files waits while the editor has unsaved edits to them.
        let op = match &a {
            Action::Run(op) | Action::RunThen(op, _) | Action::RunThenOpen(op, _) => Some(op),
            _ => None,
        };
        if let Some(op) = op
            && !std::mem::take(&mut self.dirty_answered)
            && op.rewrites_worktree()
            && let Some(r) = self.repos.get(self.active)
        {
            let files: Vec<String> = r.all_tabs().filter_map(|t| match t {
                Tab::Editor(e) if e.is_dirty() => Some(e.path.clone()),
                _ => None,
            }).collect();
            if !files.is_empty() {
                self.dirty_guard = Some(DirtyGuard { root: r.repo.root.clone(), files, action: a });
                return;
            }
        }
        match a {
            Action::Run(op) => {
                let batch = self.frame_no;
                let op = if self.settings.recurse_submodules { ops::recursing_submodules(op) } else { op };
                if let Some(r) = self.repos.get_mut(self.active) {
                    // Two of the same in one frame are two parts of one action.
                    let doubled = r.last_asked.as_ref().is_some_and(|(last, at)| same_op(last, &op) && at.elapsed() < DOUBLE_CLICK && r.queue.back().is_none_or(|q| q.batch != batch));
                    r.last_asked = Some((op.clone(), Instant::now()));
                    if !doubled {
                        r.queue.push_back(Queued { op, open_after: None, batch, then: None });
                    }
                } else {
                    self.global_queue.push_back((op, None));
                }
            }
            Action::RunThenOpen(op, path) => {
                let batch = self.frame_no;
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.queue.push_back(Queued { op, open_after: Some(path), batch, then: None });
                } else {
                    self.global_queue.push_back((op, Some(path)));
                }
            }
            Action::Background(job) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let mut t = crate::task::Task::default();
                    t.spawn(ctx, job);
                    r.background.push(t);
                }
            }
            Action::RunDetached(op) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let mut t = crate::task::Task::default();
                    let (git, root) = (r.repo.git.clone(), r.repo.root.clone());
                    t.spawn(ctx, move || -> Result<String, String> { op.run(&git, Some(&root), None, &mut |_| {}).map(|_| String::new()).map_err(|e| format!("{} failed:\n{e}", op.title)) });
                    r.background.push(t);
                }
            }
            Action::Later(job) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let mut t = crate::task::Task::default();
                    t.spawn(ctx, move || -> Result<crate::action::Later, String> { job() });
                    r.later.push(t);
                }
            }
            Action::OpenFile(path) => open_file(&path, &self.settings.default_external_editor, &mut self.errors),
            // Dialogs don't block the window, so something behind one can ask for another.
            // A question already showing stays, and so do values typed into a form.
            Action::Confirm { .. } if self.confirm.is_some() => self.toasts.push(("Answer the open question first.".to_string(), Instant::now())),
            Action::Confirm { title, body, label, op } => {
                let repo = self.repos.get(self.active).map(|r| r.repo.root.clone());
                self.confirm = Some(Confirm { title, body, label, op, repo });
            }
            Action::Form(_) if self.form.as_ref().is_some_and(FormDialog::touched) => self.toasts.push(("Finish or cancel the open dialog first.".to_string(), Instant::now())),
            Action::Form(f) => {
                self.form_repo = self.repos.get(self.active).map(|r| r.repo.root.clone());
                self.form = Some(f);
            }
            Action::OfferLockRemoval(root, why) => self.lock_prompt = Some(LockPrompt { root, why, busy: false }),
            Action::AskCloseTab { key, lost } => {
                if let Some(r) = self.repos.get(self.active) {
                    self.close_guard = Some(CloseGuard { target: CloseTarget::Tab { root: r.repo.root.clone(), key }, items: vec![lost] });
                }
            }
            Action::ScanRepos(root) => {
                let cancel = Arc::new(AtomicBool::new(false));
                let mut task = crate::task::Task::default();
                let (dir, stop) = (root.clone(), cancel.clone());
                task.spawn(ctx, move || -> Result<Vec<PathBuf>, String> { Ok(gitem_core::repo::find_repos_until(&dir, SCAN_DEPTH, &stop)) });
                self.repo_scan = Some(RepoScan { root, task, cancel, picked: Vec::new(), found: Vec::new(), done: false });
            }
            Action::Many(all) => {
                for a in all {
                    self.apply_action(ctx, a);
                }
            }
            Action::ForRepo(root, inner) => match self.repos.iter().position(|r| r.repo.root == root) {
                Some(i) => {
                    let shown = self.active;
                    self.active = i;
                    self.apply_action(ctx, *inner);
                    // Opening a repository moves focus on purpose; otherwise stay put.
                    if self.active == i {
                        self.active = shown;
                    }
                }
                None => self.errors.push(format!("{} is no longer open, so nothing was changed.", root.display())),
            },
            Action::OpenTab(t) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.open_tab(t);
                } else {
                    match without_repo(t) {
                        Some(t) => self.no_repo_tab = Some(t),
                        None => self.toasts.push(("That needs a repository. Open or clone one first.".to_string(), Instant::now())),
                    }
                }
            }
            Action::Inspect(sha) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.inspect.set(&sha);
                    r.show_inspect = true;
                    r.show_wip = false;
                }
            }
            Action::ShowWip => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    // It needs the tab, whose draft it shows, and room beside the graph.
                    if !r.squeeze.details && r.dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Changes(_))) {
                        r.show_wip = true;
                        r.show_inspect = true;
                    } else {
                        r.open_tab(Tab::changes());
                    }
                }
            }
            Action::InspectFirst(sha) => {
                if let Some(r) = self.repos.get_mut(self.active)
                    && r.inspect.sha.is_none()
                {
                    r.inspect.set(&sha);
                }
            }
            // Only web links reach the browser opener; nothing goes through a shell.
            Action::OpenUrl(u) => {
                if u.starts_with("https://") || u.starts_with("http://") {
                    ctx.open_url(egui::OpenUrl::new_tab(u));
                } else {
                    self.errors.push(format!("Not opening an unsupported link: {u}"));
                }
            }
            Action::Copy(s) => {
                ctx.copy_text(s);
                self.toasts.push(("Copied".to_string(), Instant::now()));
            }
            Action::Toast(s) => self.toasts.push((s, Instant::now())),
            Action::Error(e) => self.errors.push(e),
            Action::OpenRepo(p) => self.open_in_background(ctx, p, None),
            Action::DeepLink(d) => {
                if gitem_core::forge::is_network_path(&d.repo) {
                    self.errors.push(format!("Not opening a link to a network path: {}", d.repo));
                } else if self.knows_path(&d.repo) {
                    self.open_in_background(ctx, PathBuf::from(&d.repo), Some(d));
                } else {
                    self.link_prompt = Some(d);
                }
            }
            Action::ReviewTrust => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.review_trust = true;
                }
            }
            Action::About => self.about_open = true,
            Action::OpenSettings => {
                if self.repos.is_empty() {
                    self.no_repo_tab = if matches!(self.no_repo_tab, Some(Tab::Settings(_))) { None } else { Some(Tab::Settings(Box::default())) };
                } else {
                    self.apply_action(ctx, Action::OpenTab(Tab::Settings(Box::default())));
                }
            }
            Action::RunGlobal { op, open_after } => self.global_queue.push_back((op, open_after)),
            Action::ReloadRepository => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.refresh(ctx, &r.repo);
                }
            }
            Action::Dock { key, op } => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let toast = r.dock_op(&key, op, &mut self.close_guard);
                    if let Some(t) = toast {
                        self.toasts.push((t, Instant::now()));
                    }
                }
            }
            Action::Layout(preset) => {
                let saved = match &preset {
                    crate::layouts::Preset::Saved(name) => self.settings.dock_layouts.get(name).cloned(),
                    _ => None,
                };
                if let Some(r) = self.repos.get_mut(self.active) {
                    if r.zoom.is_some() {
                        r.toggle_zoom(None);
                    }
                    let into = match &preset {
                        crate::layouts::Preset::Git => Some(crate::layouts::git_layout()),
                        crate::layouts::Preset::Ide => Some(crate::layouts::ide_layout(Tab::Editor(Box::default()))),
                        crate::layouts::Preset::Saved(_) => saved.as_deref().and_then(layout_from_json),
                    };
                    match into {
                        Some(into) => {
                            let open = crate::layouts::take_all(&mut r.dock);
                            r.dock = crate::layouts::arrange(into, open);
                            if preset == crate::layouts::Preset::Ide {
                                r.show_sidebar = true;
                                self.settings.sidebar_files = true;
                                self.settings_dirty = true;
                            }
                        }
                        None => self.toasts.push(("That layout can't be read any more.".to_string(), Instant::now())),
                    }
                }
            }
            Action::SaveLayout(name) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    if r.zoom.is_some() {
                        r.toggle_zoom(None);
                    }
                    if let Some(j) = layout_to_json(&r.dock) {
                        self.settings.dock_layouts.insert(name, j);
                        self.settings_dirty = true;
                    }
                }
            }
            Action::Editor(req) => {
                let mut out = Vec::new();
                let mut found = false;
                if let Some(r) = self.repos.get_mut(self.active) {
                    let RepoTab { repo, data, dock, .. } = r;
                    let is_file = |t: &Tab| matches!(t, Tab::Editor(e) if !e.path.is_empty());
                    let front = dock.focused_leaf().and_then(|p| dock.leaf(p).ok().map(|l| egui_dock::TabPath { surface: p.surface, node: p.node, tab: l.active })).filter(|tp| dock.iter_all_tabs().any(|(x, t)| x == *tp && is_file(t)));
                    let path = front.or_else(|| dock.iter_all_tabs().find(|(_, t)| is_file(t)).map(|(p, _)| p));
                    if let Some(path) = path {
                        let _ = dock.set_active_tab(path);
                        if let Some((_, Tab::Editor(e))) = dock.iter_all_tabs_mut().find(|(p, _)| *p == path) {
                            let mut cx = Cx { repo, data, settings: &self.settings, pal: &self.pal, out: &mut out, dialog_open: false, in_front: true };
                            e.request(&mut cx, req);
                            found = true;
                        }
                    }
                }
                if !found {
                    self.toasts.push(("No file is open in the editor.".to_string(), Instant::now()));
                }
                for a in out {
                    self.apply_action(ctx, a);
                }
            }
            Action::FindInFiles { text, folder } => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    if r.squeeze.sidebar {
                        // No room for the sidebar beside the view: a tab has it.
                        let view = crate::views::find::FindView::looking_for(text.as_deref().unwrap_or(""), folder);
                        self.apply_action(ctx, Action::OpenTab(Tab::Grep(Box::new(view))));
                    } else {
                        r.show_sidebar = true;
                        r.sidebar.find_in_files(text, folder);
                    }
                }
            }
            Action::SaveAll => {
                let mut out = Vec::new();
                for r in self.repos.iter_mut() {
                    let RepoTab { repo, data, dock, zoom, .. } = r;
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &self.pal, out: &mut out, dialog_open: false, in_front: false };
                    for (_, t) in dock.iter_all_tabs_mut().chain(zoom.iter_mut().flat_map(|z| z.full.iter_all_tabs_mut())) {
                        if let Tab::Editor(e) = t
                            && e.is_dirty()
                        {
                            e.save(&mut cx);
                        }
                    }
                }
                for a in out {
                    self.apply_action(ctx, a);
                }
            }
            Action::FileSaved(_) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.refresh_status(ctx, &r.repo);
                }
            }
            Action::Refresh => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.refresh(ctx, &r.repo);
                    r.data.forge_gen += 1;
                }
            }
            Action::ForgeChanged(message) => {
                self.toasts.push((message, Instant::now()));
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.refresh(ctx, &r.repo);
                    r.data.forge_gen += 1;
                }
            }
            Action::Retry => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.retry(ctx, &r.repo);
                }
            }
            Action::StatusBlame(s) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.last_blame = s;
                }
            }
            Action::Settings(f) => {
                f(&mut self.settings);
                self.settings_dirty = true;
                for r in &mut self.repos {
                    r.data.custom_hosts = self.settings.custom_hosts.clone();
                    r.data.recompute_forge();
                }
            }
            Action::OpenPalette => self.palette = Some(PaletteState::new()),
            Action::PickFolder => crate::widgets::pick(&mut self.folder_pick, ctx, rfd::FileDialog::new(), true),
            Action::GraphFocus(name) | Action::GraphHide(name) if self.repos.is_empty() => drop(name),
            Action::GraphFocus(name) => self.with_graph(|g| g.focus_ref(&name)),
            Action::GraphHide(name) => self.with_graph(|g| g.hide_ref(&name)),
            Action::ToggleZen => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let on = !(r.show_sidebar || r.show_inspect);
                    r.show_sidebar = on;
                    r.show_rail = on;
                    r.show_inspect = on;
                }
            }
            Action::OpenTerminal(dir, command) => open_terminal(&dir, command.as_deref(), &self.settings.terminal, &mut self.errors),
            Action::CloseRepo(root) => {
                if let Some(i) = self.repos.iter().position(|r| r.repo.root == root) {
                    self.request_close_repo(i);
                }
            }
            Action::LocateRepo(root) => {
                self.relocating = Some(root);
                crate::widgets::pick(&mut self.folder_pick, ctx, rfd::FileDialog::new(), true);
            }
            Action::OpenFromMore(t) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.open_from_more(t);
                }
            }
            Action::RunThen(op, then) => {
                let batch = self.frame_no;
                let op = if self.settings.recurse_submodules { ops::recursing_submodules(op) } else { op };
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.queue.push_back(Queued { op, open_after: None, batch, then: Some(*then) });
                }
            }
            Action::UndoLast => {
                let confirming = self.settings.confirm_destructive;
                let found = self.repos.get(self.active).map(|r| (r.repo.clone(), r.undo_entry.clone()));
                match found {
                    Some((repo, Some(entry))) => self.apply_action(ctx, undo_action(&repo, entry, confirming)),
                    Some((_, None)) => self.toasts.push(("There's nothing to undo right now. The reflog has older states.".to_string(), Instant::now())),
                    None => {}
                }
            }
            Action::BranchPr(pr) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.branch_pr = pr;
                }
            }
        }
    }

    fn title_bar(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let p = self.pal.clone();
        chrome::drag_region(ui);
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
        let bar = ui.max_rect();
        let has_repo = !self.repos.is_empty();
        // The right end is laid out first: it holds the way to close the window, so however
        // many repositories are open it has to stay on screen.
        let field = if bar.width() >= 1000.0 { chrome::COMMAND_FIELD_WIDTH } else { 30.0 };
        // With none open the rail isn't there, so Settings sits beside the field.
        let extra = if has_repo { 0.0 } else { 30.0 };
        let right = egui::Rect::from_min_max(egui::pos2(bar.right() - chrome::window_controls_width() - 8.0 - field - extra, bar.top()), bar.max);
        let left = egui::Rect::from_min_max(bar.min, egui::pos2(right.left() - chrome::TITLE_DRAG_ROOM, bar.bottom()));
        ui.scope_builder(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            chrome::window_controls(ui, &p);
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            if !has_repo {
                // With a repository open the rail has this.
                if chrome::icon_button(ui, chrome::icon::GEAR_SIX, crate::i18n::t("Settings"), "Preferences", matches!(self.no_repo_tab, Some(Tab::Settings(_))), &p).clicked() {
                    out.push(Action::OpenSettings);
                }
            }
            if field > 30.0 {
                if chrome::command_field(ui, field, &p).clicked() {
                    self.palette = Some(PaletteState::new());
                }
            } else if chrome::icon_button(ui, chrome::icon::LIST_MAGNIFYING_GLASS, "Command palette", "Run any command (Ctrl+P)", self.palette.is_some(), &p).clicked() {
                self.palette = Some(PaletteState::new());
            }
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.set_clip_rect(left.intersect(ui.clip_rect()));
            ui.add_space(9.0);
            let (rect, logo) = ui.allocate_exact_size(egui::vec2(20.0, chrome::TITLE_BAR_HEIGHT), Sense::click());
            chrome::paint_logo(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0)));
            logo.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About gitem"));
            crate::widgets::focus_ring(ui, &logo, &p);
            if logo.on_hover_text("About gitem").clicked() {
                out.push(Action::About);
            }
            ui.add_space(8.0);
            let mut close = None;
            let mut reorder = None;
            let names: Vec<String> = self.repos.iter().map(|r| r.repo.name()).collect();
            let dirty: Vec<bool> = self.repos.iter().map(|r| r.data.status.get().is_some_and(|s| !s.entries.is_empty())).collect();
            let natural: Vec<f32> = names.iter().zip(&dirty).map(|(n, d)| chrome::repo_tab_width(ui, n, *d)).collect();
            // Room for what comes after the tabs: "more", "+", and any still opening.
            let room = ui.available_width() - 64.0 - self.opening.len() as f32 * 130.0;
            let (shown, cap) = chrome::fit_tabs(&natural, room, self.active);
            let active = &mut self.active;
            for &i in &shown {
                let r = &self.repos[i];
                let (resp, closing) = chrome::repo_tab(ui, &names[i], dirty[i], i == *active, &r.repo.root.display().to_string(), cap, &p);
                if closing || resp.middle_clicked() {
                    close = Some(i);
                } else if resp.clicked() {
                    *active = i;
                }
                resp.context_menu(|ui| {
                    if i > 0 && ui.button("Move left").clicked() {
                        reorder = Some((i, i - 1));
                        ui.close();
                    }
                    if i + 1 < names.len() && ui.button("Move right").clicked() {
                        reorder = Some((i, i + 1));
                        ui.close();
                    }
                    if ui.button("Close repository").clicked() {
                        close = Some(i);
                        ui.close();
                    }
                });
            }
            if shown.len() < names.len() {
                let hidden = names.len() - shown.len();
                ui.menu_button(format!("{} {hidden}", chrome::icon::DOTS_THREE), |ui| {
                    for (i, name) in names.iter().enumerate().filter(|(i, _)| !shown.contains(i)) {
                        if ui.button(name.as_str()).clicked() {
                            *active = i;
                            ui.close();
                        }
                    }
                }).response.on_hover_text(format!("{} more open", plural(hidden, "repository", "repositories")));
            }
            if let Some((from, to)) = reorder {
                self.move_repo_tab(from, to);
            }
            if let Some(i) = close {
                self.request_close_repo(i);
            }
            for o in &self.opening {
                ui.add(spinner(12.0));
                let name = o.path.file_name().map_or_else(|| o.path.display().to_string(), |n| n.to_string_lossy().into_owned());
                ui.label(RichText::new(format!("Opening {name}…")).small().color(p.dim)).on_hover_text(o.path.display().to_string());
                ui.add_space(4.0);
            }
            ui.add_space(2.0);
            ui.scope(|ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                let plus = ui.menu_button(chrome::icon::PLUS, |ui| self.open_menu(ui, out)).response;
                plus.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Open a repository"));
                plus.on_hover_text("Open a repository (Ctrl+O)");
            });
        });
    }

    fn make_no_repo(git: &Result<GitExe, String>) -> Box<NoRepo> {
        let git = git.clone().unwrap_or_else(|_| GitExe { path: "git not found".into(), source: gitem_core::runner::GitSource::System, log: Default::default(), helper: None, neutral: Default::default(), cancel: None });
        let root = PathBuf::from(".");
        Box::new(NoRepo { repo: Repo::outside_any(git, root.clone(), root), data: RepoData::default() })
    }

    fn about(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if !self.about_open {
            self.about_git = None;
            return;
        }
        let p = self.pal.clone();
        let mut open = true;
        let mut dismiss = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        // Asked once each time the box opens: it starts a process.
        let git = self.about_git.get_or_insert_with(|| self.git.as_ref().map(|g| g.version()).unwrap_or_else(|e| e.clone())).clone();
        dialog_window("About gitem", self.dialogs_settled).open(&mut open).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).default_width(340.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
                chrome::paint_logo(ui.painter(), rect);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new("gitem").size(20.0).strong());
                    ui.label(RichText::new("A visual Git client").color(p.dim));
                });
            });
            ui.add_space(10.0);
            egui::Grid::new("about-grid").num_columns(2).spacing(egui::vec2(14.0, 6.0)).show(ui, |ui| {
                let key = |ui: &mut Ui, t: &str| {
                    ui.label(RichText::new(t).color(p.dim));
                };
                key(ui, "Version");
                ui.label(env!("CARGO_PKG_VERSION"));
                ui.end_row();
                key(ui, "Commit");
                let sha = env!("GITEM_BUILD_SHA");
                if sha.is_empty() {
                    ui.label("Unknown");
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(sha).monospace());
                        if chrome::icon_button(ui, chrome::icon::COPY, "Copy commit", "", false, &p).clicked() {
                            out.push(Action::Copy(sha.to_string()));
                        }
                    });
                }
                ui.end_row();
                if !env!("GITEM_BUILD_DATE").is_empty() {
                    key(ui, "Commit date");
                    ui.label(env!("GITEM_BUILD_DATE"));
                    ui.end_row();
                }
                key(ui, "Git");
                ui.label(git.trim());
                ui.end_row();
            });
            if !env!("GITEM_BUILD_DIRTY").is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Built with uncommitted changes, so this commit doesn't fully describe it.").small().color(p.warn));
            }
            ui.add_space(8.0);
            if let Some(r) = self.update_found.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(p.good, format!("Version {} is available.", r.version));
                    if ui.button("Open its page").on_hover_text("Nothing is downloaded or run from here.").clicked() {
                        out.push(Action::OpenUrl(r.page.clone()));
                    }
                });
            } else if let Some(answer) = &self.update_answer {
                ui.label(RichText::new(answer).small().color(p.dim));
            }
            let url = self.settings.update_url.trim().to_string();
            let reason = if url.is_empty() { "Give an address to ask in Settings > Workflow." } else { "Already asking." };
            if ui.add_enabled(!url.is_empty() && !self.update_task.busy(), egui::Button::new("Check for updates")).on_disabled_hover_text(reason).clicked() {
                self.update_asked = true;
                self.update_answer = None;
                self.update_task.spawn(ctx, move || gitem_core::update::check(&url, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string()));
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    dismiss = true;
                }
            });
        });
        self.about_open = open && !dismiss;
    }

    fn toolbar(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let p = self.pal.clone();
        let Some(tab) = self.repos.get_mut(self.active) else { return };
        ui.horizontal(|ui| {
            let (branch, ahead, behind, upstream, has_status) = match tab.data.status.get() {
                Some(s) => (s.branch.clone(), s.ahead, s.behind, s.upstream.clone(), true),
                None => (None, 0, 0, None, false),
            };
            // Mid-rebase HEAD is detached, which is true and alarming and not what's going on.
            let op_state = tab.data.status.get().map_or(RepoOp::None, |s| s.op);
            let rebasing_branch = tab.data.status.get().and_then(|s| s.op_branch.clone()).filter(|_| matches!(op_state, RepoOp::Rebase | RepoOp::RebaseInteractive));
            let label = rebasing_branch.map(|b| format!("{b} (rebasing)")).or_else(|| branch.clone()).unwrap_or_else(|| if has_status { "detached HEAD".into() } else { "…".into() });
            let data = &tab.data;
            let filter = &mut tab.branch_filter;
            let recent = &tab.recent_branches;
            ui.menu_button((RichText::new(chrome::icon::GIT_BRANCH).color(p.accent), RichText::new(format!("{label}  {}", chrome::icon::CARET_DOWN)).strong()), |ui| {
                ui.set_min_width(260.0);
                if ui.button("+ New branch…").clicked() {
                    out.push(Action::Form(forms::new_branch(None)));
                    ui.close();
                }
                let typed = ui.add(crate::widgets::input(filter).hint_text("Filter branches").desired_width(f32::INFINITY));
                if !ui.memory(|m| m.focused().is_some()) {
                    typed.request_focus();
                }
                ui.separator();
                let needle = filter.to_lowercase();
                let hit = |b: &Branch| needle.is_empty() || b.name.to_lowercase().contains(&needle);
                let dirty = data.status.get().is_some_and(|s| !s.entries.is_empty());
                let mut choose = |ui: &mut Ui, b: &Branch| {
                    let r = ui.selectable_label(b.is_current, b.name.as_str());
                    if r.clicked() {
                        if !b.is_current {
                            out.push(Action::Run(ops::checkout(&b.name)));
                        }
                        ui.close();
                    }
                };
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    let locals: Vec<&Branch> = data.local_branches().filter(|b| hit(b)).collect();
                    let recents: Vec<&Branch> = recent.iter().filter_map(|n| locals.iter().find(|b| b.name == *n).copied()).collect();
                    if !recents.is_empty() {
                        ui.label(RichText::new("Recent").small().color(p.dim));
                        recents.iter().for_each(|b| choose(ui, b));
                        ui.separator();
                        ui.label(RichText::new("All").small().color(p.dim));
                    }
                    locals.iter().for_each(|b| choose(ui, b));
                    if locals.is_empty() {
                        ui.label(RichText::new(if data.refs_known() { "No branch matches" } else { "Reading branches…" }).color(p.dim));
                    }
                });
                if dirty {
                    ui.separator();
                    ui.label(RichText::new("You have uncommitted changes. Git keeps them if it can, and refuses if switching would overwrite them.").small().color(p.dim));
                }
            });
            ui.separator();
            let no_remote = tab.data.refs_known() && tab.data.remotes().is_empty();
            let in_op = op_state != RepoOp::None && op_state != RepoOp::Bisect;
            let (sync_off, sync_why) = if no_remote { (true, "This repository has no remote yet. Add one from Remotes in the sidebar.") } else if in_op { (true, "Finish or abort what's going on first: see the banner.") } else { (false, "") };
            let prune = self.settings.prune_on_fetch;
            let remotes: Vec<String> = tab.data.remotes().iter().map(|r| r.name.clone()).collect();
            let can_fetch = !no_remote;
            // Each shows its own action under way, with git's progress, and can't start it twice.
            let now = &tab.data.ops_now;
            let line = now.line();
            let fetch_busy = now.state("fetch");
            let fetch = ui.add_enabled_ui(can_fetch, |ui| chrome::action_button(ui, chrome::icon::CLOUD_ARROW_DOWN, p.accent, "Fetch", "Fetching", "Fetch all remotes (Ctrl+Shift+F)", false, fetch_busy.as_ref(), line, &p)).inner;
            if fetch.on_disabled_hover_text(sync_why).clicked() {
                out.push(Action::Run(ops::fetch(&ops::FetchOpts { all: true, prune, ..Default::default() })));
            }
            let pull_label = if behind > 0 { format!("Pull {behind}") } else { "Pull".to_string() };
            let pull_busy = now.state("pull");
            let pull = ui.add_enabled_ui(!sync_off, |ui| chrome::action_button(ui, chrome::icon::ARROW_LINE_DOWN, p.incoming(), &pull_label, "Pulling", "Bring in the upstream's commits (Ctrl+Shift+L)", behind > 0, pull_busy.as_ref(), line, &p)).inner;
            if pull.on_disabled_hover_text(sync_why).clicked() {
                out.push(Action::Run(ops::pull(pull_mode(&self.settings.pull_mode), self.settings.pull_autostash)));
            }
            let pull_more = crate::widgets::small_button(ui, chrome::icon::CARET_DOWN);
            pull_more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Pull options"));
            if pull_more.on_hover_text("Pull options").clicked() {
                out.push(Action::Form(forms::pull_form()));
            }
            let push_label = if upstream.is_none() && branch.is_some() { "Publish".to_string() } else if ahead > 0 { format!("Push {ahead}") } else { "Push".to_string() };
            // Publishing with no remote leads to adding one, so only an operation stops it.
            let push_off = if upstream.is_none() && branch.is_some() { in_op } else { sync_off };
            let push_busy = now.state("push");
            let pushing = if upstream.is_none() && branch.is_some() { "Publishing" } else { "Pushing" };
            let push = ui.add_enabled_ui(!push_off, |ui| chrome::action_button(ui, chrome::icon::ARROW_LINE_UP, p.outgoing(), &push_label, pushing, "Send this branch's commits to its remote (Ctrl+Shift+K)", ahead > 0, push_busy.as_ref(), line, &p)).inner;
            if push.on_disabled_hover_text(sync_why).clicked() {
                if no_remote {
                    out.push(Action::Form(forms::add_remote()));
                } else if upstream.is_some() {
                    let names: Vec<&str> = remotes.iter().map(String::as_str).collect();
                    out.push(Action::Run(ops::push_to_upstream(branch.as_deref(), upstream.as_deref(), &names)));
                } else {
                    out.push(Action::Form(forms::push_form_guarding(branch.clone(), remotes.clone(), false, self.settings.confirm_destructive, tab.data.base_branch())));
                }
            }
            let push_more = crate::widgets::small_button(ui, chrome::icon::CARET_DOWN);
            push_more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Push options"));
            if push_more.on_hover_text("Push options").clicked() {
                out.push(Action::Form(forms::push_form_guarding(branch.clone(), remotes.clone(), upstream.is_some(), self.settings.confirm_destructive, tab.data.base_branch())));
            }
            ui.separator();
            if chrome::tool_button(ui, chrome::icon::ARCHIVE, p.series[6], crate::i18n::t("Stash"), "Stash changes", false, &p).clicked() {
                out.push(Action::Form(forms::stash_form(Vec::new())));
            }
            if chrome::tool_button(ui, chrome::icon::ARROWS_CLOCKWISE, p.dim, crate::i18n::t("Refresh"), "Reload this repository (F5)", false, &p).clicked() {
                out.push(Action::Refresh);
            }
            if let Some(short) = tab.undoable.clone() {
                let name = format!("Undo commit {short}");
                if chrome::tool_button(ui, chrome::icon::ARROW_COUNTER_CLOCKWISE, p.dim, &name, "Take it back, keeping its changes staged", false, &p).clicked() {
                    out.push(crate::menus::undo_commit_action(&tab.repo, tab.data.status.get().is_some_and(|s| s.upstream.is_some() && s.ahead == 0), self.settings.confirm_destructive));
                }
            } else if let Some(entry) = tab.undo_entry.clone()
                && chrome::tool_button(ui, chrome::icon::ARROW_COUNTER_CLOCKWISE, p.dim, &entry.label, &entry.detail, false, &p).clicked()
            {
                out.push(undo_action(&tab.repo, entry, self.settings.confirm_destructive));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Shown over the view when there's no room to put them beside it.
                let details_on = if tab.squeeze.details { tab.peek == Some(Peek::Details) } else { tab.show_inspect };
                if chrome::tool_button(ui, chrome::icon::ARTICLE, p.dim, crate::i18n::t("Details"), "Show the selected commit's details", details_on, &p).clicked() {
                    if tab.squeeze.details {
                        tab.peek = if details_on { None } else { Some(Peek::Details) };
                    } else {
                        tab.show_inspect = !tab.show_inspect;
                    }
                }
                let sidebar_on = if tab.squeeze.sidebar { tab.peek == Some(Peek::Sidebar) } else { tab.show_sidebar };
                if chrome::tool_button(ui, chrome::icon::SIDEBAR_SIMPLE, p.dim, crate::i18n::t("Sidebar"), "Show branches, tags, and files (Ctrl+B)", sidebar_on, &p).clicked() {
                    if tab.squeeze.sidebar {
                        tab.peek = if sidebar_on { None } else { Some(Peek::Sidebar) };
                    } else {
                        tab.show_sidebar = !tab.show_sidebar;
                    }
                }
            });
        });
    }

    fn open_menu(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        if ui.button("Open folder…").clicked() {
            crate::widgets::pick(&mut self.folder_pick, ui.ctx(), rfd::FileDialog::new(), true);
            ui.close();
        }
        if ui.button("Clone…").clicked() {
            out.push(Action::Form(forms::clone_form()));
            ui.close();
        }
        if ui.button("Initialize…").clicked() {
            out.push(Action::Form(forms::init_form()));
            ui.close();
        }
        if ui.button("Scan for repositories…").clicked() {
            out.push(Action::Form(forms::scan_form()));
            ui.close();
        }
        if !self.settings.recent_repos.is_empty() {
            ui.separator();
            ui.label(RichText::new("Recent").small().color(self.pal.faint));
            for r in self.settings.recent_repos.clone().iter().take(12) {
                ui.horizontal(|ui| {
                    if ui.button(r.as_str()).clicked() {
                        out.push(Action::OpenRepo(r.into()));
                        ui.close();
                    }
                    if crate::widgets::small_button(ui, "\u{E4F6}").on_hover_text("Remove from this list. The folder isn't touched.").clicked() {
                        out.push(forget_recent(r));
                    }
                });
            }
        }
    }

    fn welcome(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let p = self.pal.clone();
        ui.add_space(50.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), Sense::hover());
            chrome::paint_logo(ui.painter(), rect);
            ui.add_space(4.0);
            ui.label(RichText::new("gitem").size(30.0).strong());
            ui.label(RichText::new("A Git client. Open a repository, clone one, or start a new one.").color(p.dim));
            ui.label(RichText::new("Ctrl+P finds any command, and F1 lists the keys and the words used here.").small().color(p.faint));
            ui.add_space(14.0);
            if let Err(e) = &self.git {
                error_box(ui, e, &p);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Path to git");
                    ui.add(crate::widgets::input(&mut self.settings.git_path));
                    if ui.button("Use").clicked() {
                        self.git = GitExe::discover(Some(Path::new(&self.settings.git_path))).map_err(|e| e.to_string());
                        self.git_warning = self.git.as_ref().ok().and_then(GitExe::version_warning);
                        self.no_repo = None;
                        self.settings_dirty = true;
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 470.0).max(0.0) / 2.0);
                if ui.add(egui::Button::new(RichText::new("Clone…").color(p.on_accent)).fill(p.accent)).clicked() {
                    out.push(Action::Form(forms::clone_form()));
                }
                if ui.button("Open folder…").clicked() {
                    crate::widgets::pick(&mut self.folder_pick, ui.ctx(), rfd::FileDialog::new(), true);
                }
                if ui.button("Initialize…").clicked() {
                    out.push(Action::Form(forms::init_form()));
                }
                if ui.button("Scan for repositories…").on_hover_text("Find the repositories in a folder, and open the ones you want").clicked() {
                    out.push(Action::Form(forms::scan_form()));
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 240.0).max(0.0) / 2.0);
                if ui.button("Pull requests I'm in").on_hover_text("Across the hosts your recent repositories use. Nothing is asked until you say so.").clicked() {
                    out.push(Action::OpenTab(Tab::Launchpad(Box::default())));
                }
                if ui.button("Credentials").on_hover_text("Saved tokens and SSH keys").clicked() {
                    out.push(Action::OpenTab(Tab::Credentials(Box::default())));
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 500.0).max(0.0) / 2.0);
                let r = ui.add(crate::widgets::input(&mut self.open_path).hint_text("Or type a path and press Enter, or drop a folder here").desired_width(500.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.open_path.trim().is_empty() {
                    out.push(Action::OpenRepo(self.open_path.trim().into()));
                }
            });
            if !self.settings.recent_repos.is_empty() {
                ui.add_space(18.0);
                ui.label(RichText::new("Recent").color(p.faint));
                for r in self.settings.recent_repos.clone().iter().take(12) {
                    let link = ui.link(r.as_str()).on_hover_text("Right-click to remove it from this list");
                    if link.clicked() {
                        out.push(Action::OpenRepo(r.into()));
                    }
                    link.context_menu(|ui| {
                        if ui.button("Remove from this list").on_hover_text("The folder isn't touched.").clicked() {
                            out.push(forget_recent(r));
                            ui.close();
                        }
                    });
                }
            }
        });
    }

    fn op_banner(ui: &mut Ui, tab_status: &RepoStatus, branches: &[Branch], ops_busy: bool, out: &mut Vec<Action>, p: &Palette) {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                // A second Skip would act on a state nobody has looked at yet.
                if ops_busy {
                    ui.disable();
                }
                let mut text = op_headline(tab_status, branches);
                if let Some((a, b)) = tab_status.op_progress {
                    text.push_str(&format!(" (step {a} of {b})"));
                }
                ui.label(RichText::new(text).strong());
                let conflicts: Vec<String> = tab_status.conflicts().map(|e| e.path.clone()).collect();
                if !conflicts.is_empty() {
                    // Not a chip: its warning-coloured text fails contrast on this fill in light themes.
                    ui.label(RichText::new(plural(conflicts.len(), "conflict", "conflicts")).strong().color(p.text));
                    if ui.button("Resolve").clicked() {
                        out.push(Action::OpenTab(Tab::merge(&conflicts[0])));
                    }
                }
                if tab_status.op == RepoOp::Bisect {
                    if ui.button("Open Bisect").clicked() {
                        out.push(Action::OpenTab(Tab::Bisect(Box::default())));
                    }
                } else if let Some(cmd) = tab_status.op.command() {
                    if conflicts.is_empty() && ui.button("Continue").clicked() {
                        out.push(Action::Run(ops::op_continue(cmd)));
                    }
                    if tab_status.op == RepoOp::RebaseInteractive && ui.button("Edit steps").on_hover_text("Change the remaining rebase steps").clicked() {
                        out.push(Action::Run(GitOp::new("Edit rebase steps").step(&["rebase", "--edit-todo"])));
                    }
                    if cmd != "merge" && ui.button("Skip").clicked() {
                        out.push(Action::Run(ops::op_skip(cmd)));
                    }
                    if ui.button("Abort").clicked() {
                        out.push(Action::Confirm {
                            title: format!("Abort {}", tab_status.op.label().to_lowercase()),
                            body: "Abandon this operation and go back to where you started? Conflict resolutions made so far are lost.".into(),
                            label: "Abort".into(),
                            op: ops::op_abort(cmd),
                        });
                    }
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui, out: &mut Vec<Action>) {
        let p = self.pal.clone();
        let across = (self.repos.len() > 1).then(|| across_open(&self.repos));
        let mut go_to = None;
        let Some(tab) = self.repos.get_mut(self.active) else {
            if let Some(g) = &self.global_running {
                ui.horizontal(|ui| {
                    ui.add(spinner(13.0));
                    ui.label(format!("{}: {}", g.title, g.last_line));
                    // Nothing's on screen yet to lose: a clone that's gone wrong just stops.
                    if crate::widgets::small_button(ui, "Cancel").clicked() {
                        g.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            return;
        };
        ui.horizontal(|ui| {
            if let Some(r) = &mut tab.running {
                ui.add(spinner(13.0));
                if r.asking_to_stop {
                    // Only a network op leaves nothing half done when it's killed.
                    ui.label(RichText::new("Stop git now? Files it was changing can be left half updated, and a lock left behind.").small().color(p.warn));
                    if crate::widgets::small_button(ui, "Stop it").clicked() {
                        r.cancel.store(true, Ordering::Relaxed);
                        r.asking_to_stop = false;
                    }
                    if crate::widgets::small_button(ui, "Keep waiting").clicked() {
                        r.asking_to_stop = false;
                    }
                } else {
                    ui.add(egui::Label::new(RichText::new(format!("{}: {}", r.title, r.last_line)).small()).truncate());
                    if r.network {
                        if crate::widgets::small_button(ui, "Cancel").clicked() {
                            r.cancel.store(true, Ordering::Relaxed);
                        }
                    } else if r.started.elapsed() >= CANCEL_OFFERED_AFTER {
                        // A hook that never returns would otherwise hold up everything behind it.
                        if crate::widgets::small_button(ui, "Cancel…").clicked() {
                            r.asking_to_stop = true;
                        }
                    } else {
                        ctx_repaint_at(ui, CANCEL_OFFERED_AFTER.saturating_sub(r.started.elapsed()));
                    }
                }
                if !tab.queue.is_empty() {
                    ui.label(RichText::new(format!("+{} queued", tab.queue.len())).small().color(p.dim));
                }
            } else if tab.queue.front().is_some_and(|q| q.op.network) && tab.quiet.is_some() {
                ui.add(spinner(13.0));
                ui.label(RichText::new(format!("{}: waiting for the background fetch to finish", tab.queue[0].op.title)).small());
            } else if let Some(note) = tab.fetch_note.as_ref().filter(|_| self.settings.auto_fetch_minutes > 0) {
                // Being offline isn't worth a dialog every few minutes.
                let first = note.lines().next().unwrap_or("");
                let retry = tab.fetch_wait(self.settings.auto_fetch_minutes).map_or(0, |w| w.as_secs() / 60 + 1);
                ui.label(RichText::new(format!("Background fetch failed, trying again in about {retry} min")).small().color(p.warn)).on_hover_text(first);
            } else if let Some(g) = &self.global_running {
                ui.add(spinner(13.0));
                ui.add(egui::Label::new(RichText::new(format!("{}: {}", g.title, g.last_line)).small()).truncate());
                if crate::widgets::small_button(ui, "Cancel").clicked() {
                    g.cancel.store(true, Ordering::Relaxed);
                }
            } else if !tab.data.last_blame.is_empty() && matches!(tab.dock.find_active_focused(), Some((_, Tab::File(_)))) {
                ui.add(egui::Label::new(RichText::new(&tab.data.last_blame).small().color(p.dim)).truncate());
            } else {
                let last = tab.repo.git.log.lock().ok().and_then(|l| l.iter().rev().find(|e| e.is_write).map(|e| (e.command.clone(), e.code)));
                if let Some((c, code)) = last {
                    ui.label(RichText::new(if code == 0 { chrome::icon::CHECK } else { chrome::icon::X }).small().color(if code == 0 { p.good } else { p.danger }));
                    // The rest of what's been run is in the Console.
                    let r = ui.add(egui::Label::new(RichText::new(c).small().color(p.faint).monospace()).truncate().sense(Sense::click()));
                    if r.on_hover_text("Open the Console, which has every command gitem has run").clicked() {
                        out.push(Action::OpenTab(Tab::Console(Box::default())));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.settings.ide_mode {
                    let r = ui.add(egui::Label::new(RichText::new(format!("{} IDE", chrome::icon::CODE)).small().color(p.accent)).sense(Sense::click()));
                    if r.on_hover_text("IDE mode: a click in the Files tree opens the file for editing. Click to turn it off (Ctrl+Shift+E)").clicked() {
                        out.push(Action::Settings(Box::new(|s| s.ide_mode = false)));
                    }
                }
                if tab.data.status.get().is_some() {
                    let n = tab.data.counts();
                    let (st, un, c) = (n.staged, n.unstaged, n.conflicts);
                    if st + un + c > 0 && crate::widgets::small_button(ui, format!("{st} staged, {un} unstaged{}", if c > 0 { format!(", {}", plural(c, "conflict", "conflicts")) } else { String::new() })).clicked() {
                        out.push(Action::OpenTab(Tab::changes()));
                    }
                }
                if let Some(s) = tab.data.status.get() {
                    if tab.fetched.0 != tab.data.status_gen {
                        // Once per status, not per frame: this can be a slow disk.
                        tab.fetched = (tab.data.status_gen, std::fs::metadata(tab.repo.git_dir.join("FETCH_HEAD")).and_then(|m| m.modified()).ok());
                    }
                    if let Some(at) = tab.fetched.1.and_then(|t| t.elapsed().ok()) {
                        ui.label(RichText::new(format!("Fetched {}", gitem_core::util::relative_time(gitem_core::util::now_ts() - at.as_secs() as i64, gitem_core::util::now_ts()))).small().color(p.faint));
                    }
                    if s.behind > 0 {
                        ui.label(RichText::new(format!("{} {}", chrome::icon::ARROW_DOWN, s.behind)).small().color(p.text)).on_hover_text(format!("{} to pull", plural(s.behind as usize, "commit", "commits")));
                    }
                    if s.ahead > 0 {
                        ui.label(RichText::new(format!("{} {}", chrome::icon::ARROW_UP, s.ahead)).small().color(p.text)).on_hover_text(format!("{} to push", plural(s.ahead as usize, "commit", "commits")));
                    }
                    if let Some(b) = &s.branch {
                        ui.label(RichText::new(format!("{} {b}", chrome::icon::GIT_BRANCH)).small().color(p.text));
                    }
                }
                if tab.watcher.failed.load(Ordering::Relaxed) {
                    ui.label(RichText::new("Not notified of changes here, so looking every few seconds").small().color(p.faint));
                }
                if let Some(f) = tab.data.forge() {
                    ui.label(RichText::new(format!("{} {}", f.kind.label(), f.path)).small().color(p.faint));
                }
                let stale_submodules = tab.data.submodules().iter().filter(|m| m.state == '+').count();
                if stale_submodules > 0 && crate::widgets::small_button(ui, RichText::new(plural(stale_submodules, "submodule to update", "submodules to update")).small()).on_hover_text("They're at other commits than this one expects. This updates them").clicked() {
                    out.push(Action::Run(ops::submodule_update(None, true, true, false)));
                }
                // Found for one branch, it says nothing about another.
                let current = tab.data.status.get().and_then(|s| s.branch.clone());
                if let Some((_, n, url)) = tab.data.branch_pr.as_ref().filter(|(b, _, _)| Some(b) == current.as_ref()) {
                    crate::widgets::web_link(ui, RichText::new(format!("PR #{n}")).small(), url);
                }
                if let Some((summary, rows)) = &across {
                    ui.menu_button(RichText::new(summary).small(), |ui| {
                        for (i, row) in rows.iter().enumerate() {
                            if ui.button(row).clicked() {
                                go_to = Some(i);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Open repositories, as last read. Choosing one goes to it.");
                }
            });
        });
        if let Some(i) = go_to {
            self.active = i;
        }
    }

    /// Names the dialogs up now that the backdrop goes behind, so a change can be noticed.
    fn modal_dialogs(&self) -> String {
        let mut names = String::new();
        let mut add = |on: bool, name: &str| {
            if on {
                names.push_str(name);
                names.push('\n');
            }
        };
        add(self.confirm.is_some(), self.confirm.as_ref().map_or("", |c| c.title.as_str()));
        add(self.form.is_some(), self.form.as_ref().map_or("", |f| f.title.as_str()));
        add(self.about_open, "about");
        add(!self.crash_reports.is_empty(), "crash");
        add(self.git_warning.is_some(), "git");
        add(!self.reopen_failed.is_empty(), "reopen");
        add(self.close_guard.is_some(), "close");
        add(self.lock_prompt.is_some(), "lock");
        add(self.link_prompt.is_some(), "link");
        add(self.trust_question_open(), "trust");
        add(self.help.is_some(), "help");
        add(self.repo_scan.is_some(), "scan");
        names
    }

    /// Clicks count once what's on screen has been there long enough to have been meant.
    fn settle_dialogs(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let settle = |seen: &mut (String, f64), key: String| {
            if seen.0 != key {
                *seen = (key, now);
            }
            let left = DIALOG_SETTLES - (now - seen.1);
            if left > 0.0 {
                ctx.request_repaint_after(Duration::from_secs_f64(left));
            }
            left <= 0.0
        };
        let modal = self.modal_dialogs();
        self.dialogs_settled = settle(&mut self.dialogs_seen, modal.clone());
        self.errors_settled = settle(&mut self.errors_seen, self.errors.first().cloned().unwrap_or_default());
        if !modal.is_empty() {
            dialog_backdrop(ctx, chrome::window_controls_width());
        }
    }

    fn dialog_open(&self) -> bool {
        self.confirm.is_some() || self.form.is_some() || self.palette.is_some() || self.about_open || !self.errors.is_empty() || !self.crash_reports.is_empty() || self.git_warning.is_some() || !self.reopen_failed.is_empty() || self.close_guard.is_some() || self.dirty_guard.is_some() || self.lock_prompt.is_some() || self.link_prompt.is_some() || self.help.is_some() || self.trust_question_open()
    }

    /// Old git still does most things, so this warns and lets the user carry on.
    fn git_warning_dialog(&mut self, ctx: &egui::Context) {
        let Some(warning) = self.git_warning.clone() else { return };
        let mut dismiss = false;
        dialog_window("Your git is older than gitem needs", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(warning);
            ui.add_space(8.0);
            dismiss = ui.button("Dismiss").clicked();
        });
        if dismiss {
            self.git_warning = None;
        }
    }

    /// Offers the report from a crash. It stays on disk until the user has seen this.
    fn crash_dialog(&mut self, ctx: &egui::Context) {
        let Some(newest) = self.crash_reports.first().cloned() else { return };
        let more = self.crash_reports.len() - 1;
        let (mut dismiss, mut reveal, mut copy) = (false, false, false);
        dialog_window("gitem crashed last time", self.dialogs_settled).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label("A report was saved on this computer. Nothing was sent anywhere. If you share it, read it first: it names folders and the last git commands.");
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(newest.display().to_string()).monospace().small()).selectable(true));
            if more > 0 {
                ui.label(RichText::new(format!("And {} in the same folder.", plural(more, "older report", "older reports"))).small().color(self.pal.dim));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                reveal = ui.button("Show the report").clicked();
                copy = ui.button("Copy it").clicked();
                dismiss = ui.button("Dismiss").clicked();
            });
        });
        if reveal {
            open_file(&newest, "", &mut self.errors);
        }
        if copy {
            match std::fs::read_to_string(&newest) {
                Ok(t) => ctx.copy_text(t),
                Err(e) => self.errors.push(format!("Could not read {}: {e}", newest.display())),
            }
        }
        if dismiss {
            for report in std::mem::take(&mut self.crash_reports) {
                crate::crash::mark_seen(&report);
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        let p = self.pal.clone();
        let several = self.repos.len() > 1;
        if let Some(c) = &self.confirm {
            let mut close = false;
            let mut go = false;
            let settled = self.dialogs_settled;
            dialog_window(c.title.as_str(), settled).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(c.body.as_str());
                ui.add_space(4.0);
                ui.label(RichText::new(c.op.preview()).monospace().small().color(p.faint));
                if let Some(root) = c.repo.as_ref().filter(|_| several) {
                    ui.label(RichText::new(format!("In {}", root.display())).small().color(p.warn));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    // Red is kept for what can't be got back, and only there does Enter not answer.
                    let (fill, ink) = if c.op.lossless { (p.accent, p.on_accent) } else { (p.danger, theme::ink_on(p.danger)) };
                    go = ui.add(egui::Button::new(RichText::new(c.label.as_str()).color(ink)).fill(fill)).clicked();
                    go |= c.op.lossless && settled && ui.ctx().memory(|m| m.focused().is_none()) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    close = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
            if go {
                if let Some(c) = self.confirm.take() {
                    out.push(match c.repo {
                        Some(root) => Action::ForRepo(root, Box::new(Action::Run(c.op))),
                        None => Action::Run(c.op),
                    });
                }
            } else if close {
                self.confirm = None;
            }
        }
        if let Some(f) = &mut self.form {
            let mut closed = false;
            let mut result = Vec::new();
            let form_repo = self.form_repo.clone();
            dialog_window(f.title.clone(), self.dialogs_settled).anchor(Align2::CENTER_TOP, [0.0, 70.0]).show(ctx, |ui| {
                if let Some(root) = form_repo.as_ref().filter(|_| several) {
                    ui.label(RichText::new(format!("In {}", root.display())).small().color(p.warn));
                }
                result = f.ui(ui, &p, &mut closed);
            });
            for a in result.drain(..) {
                out.push(match &form_repo {
                    Some(root) => Action::ForRepo(root.clone(), Box::new(a)),
                    None => a,
                });
            }
            if closed {
                self.form = None;
            }
        }
        // Errors are shown in full until dismissed: they're often long git output.
        if let Some(e) = self.errors.first().cloned() {
            let mut close = self.confirm.is_none() && self.form.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape));
            let waiting = self.errors.len();
            let (title, details) = error_parts(&e);
            let help = explain_error(details);
            let mut fix = None;
            let make_here = folder_to_create(&e);
            let mut create_here = None;
            // Not behind a backdrop: this can come from something in the background, and it
            // mustn't take the keyboard from someone typing. It's above any that's there.
            dialog_window(title, self.errors_settled).anchor(Align2::CENTER_BOTTOM, [0.0, -40.0]).show(ctx, |ui| {
                ui.set_max_width(560.0);
                if waiting > 1 {
                    ui.label(RichText::new(format!("1 of {waiting}")).small().color(p.dim));
                }
                let raw = |ui: &mut Ui| {
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(details).monospace().small()).selectable(true));
                    });
                };
                match help {
                    Some((plain, _)) => {
                        ui.label(plain);
                        ui.add_space(4.0);
                        egui::CollapsingHeader::new(RichText::new("What git said").small().color(p.dim)).show(ui, raw);
                    }
                    None => raw(ui),
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    close |= ui.button("Dismiss").clicked();
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(e.clone());
                    }
                    if let Some((_, f)) = help
                        && ui.button(f.label()).clicked()
                    {
                        fix = Some(f);
                    }
                    if let Some(folder) = &make_here
                        && ui.button("Create repository here…").on_hover_text(folder.as_str()).clicked()
                    {
                        create_here = Some(folder.clone());
                    }
                });
            });
            if let Some(folder) = create_here {
                close = true;
                out.push(Action::Form(forms::init_form_at(&folder)));
            }
            if let Some(f) = fix {
                close = true;
                let repo = self.repos.get(self.active);
                let branch = repo.and_then(|r| r.data.status.get().and_then(|s| s.branch.clone()));
                out.push(match f {
                    ErrorFix::SetUpstream => {
                        let candidates = repo.map(|r| r.data.remote_branches().map(|b| b.name.clone()).collect()).unwrap_or_default();
                        Action::Form(forms::set_upstream(branch.unwrap_or_default(), candidates))
                    }
                    ErrorFix::PullOptions => Action::Form(forms::pull_form()),
                    ErrorFix::Reconcile => Action::Form(forms::pull_reconcile_form()),
                    ErrorFix::Stash => Action::Form(forms::stash_form(Vec::new())),
                    ErrorFix::AddRemote => Action::Form(forms::add_remote()),
                    ErrorFix::Credentials => Action::OpenTab(Tab::Credentials(Box::default())),
                    ErrorFix::AiSettings => Action::OpenTab(Tab::Settings(Box::new(crate::views::settings::SettingsView::on_page("AI")))),
                    ErrorFix::Identity => Action::Form(forms::identity_form()),
                });
            }
            if close {
                self.errors.remove(0);
            }
        }
        // Toasts
        self.toasts.retain(|(_, t)| t.elapsed() < Duration::from_secs(4));
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(500));
            let shown = egui::Area::new("toasts".into()).anchor(Align2::RIGHT_BOTTOM, [-14.0, -34.0]).order(egui::Order::Foreground).show(ctx, |ui| {
                for (t, _) in &self.toasts {
                    egui::Frame::new().fill(p.panel2).stroke(egui::Stroke::new(1.0, p.border)).inner_margin(egui::Margin::symmetric(10, 5)).corner_radius(4.0).show(ui, |ui| {
                        ui.add(egui::Label::new(t.as_str()).selectable(true));
                    });
                }
            });
            // Being read, or copied from.
            if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|at| shown.response.rect.contains(at)) {
                let now = Instant::now();
                self.toasts.iter_mut().for_each(|(_, t)| *t = now);
            }
        }
    }

    /// Off the UI thread: the file can be on a drive that's slow to answer, and typing a path
    /// comes through here at each key. One read at a time, and only of the newest value. Until
    /// it lands, the system's store checks certificates.
    fn apply_ca_file(&mut self, ctx: &egui::Context) {
        if self.ca_file_applied == self.settings.ca_file {
            return;
        }
        self.ca_file_applied = self.settings.ca_file.clone();
        let (path, ctx) = (self.ca_file_applied.clone(), ctx.clone());
        static NEWEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static READING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let turn = NEWEST.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
            if NEWEST.load(std::sync::atomic::Ordering::SeqCst) == turn {
                let _ = gitem_core::forge::set_ca_file(&path);
                ctx.request_repaint();
            }
        });
    }

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        // In a text field the same keys are Redo.
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::Z)) {
            out.push(Action::ToggleZen);
        }
        let bound = self.settings.key_bindings.clone();
        let hit = |name: &str| crate::keys::chord_for(&bound, name).is_some_and(|(m, k)| ctx.input(|i| i.modifiers.matches_logically(m) && i.key_pressed(k)));
        let (palette, refresh, sidebar, changes, graph) = (hit("palette"), hit("refresh"), hit("sidebar"), hit("changes"), hit("graph"));
        // Behind a dialog these would act on what's dimmed. Typing isn't in their way: every
        // one needs a key held, and egui's text boxes have no use for them.
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.help = if self.help.is_some() { None } else { Some(String::new()) };
        }
        if !self.dialog_open() {
            self.more_shortcuts(ctx, out);
        }
        if palette {
            self.palette = if self.palette.is_some() { None } else { Some(PaletteState::new()) };
        }
        if refresh {
            out.push(Action::Refresh);
        }
        if sidebar
            && let Some(t) = self.repos.get_mut(self.active) {
                t.show_sidebar = !t.show_sidebar;
            }
        if changes {
            out.push(Action::OpenTab(Tab::changes()));
        }
        if graph {
            out.push(Action::OpenTab(Tab::graph()));
        }
    }

    fn more_shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        use egui::{Key, Modifiers};
        const CS: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let bound = self.settings.key_bindings.clone();
        let take = |name: &str| crate::keys::chord_for(&bound, name).is_some_and(|(m, k)| ctx.input_mut(|i| crate::keys::take_chord(i, m, k)));
        let (settings, open, close_tab, fetch, pull, push, undo) = (take("settings"), take("open"), take("close_tab"), take("fetch"), take("pull"), take("push"), take("undo_last"));
        if take("find_in_files") {
            // From an editor, what's at its caret is what to look for.
            let in_editor = self.repos.get_mut(self.active).and_then(|r| r.dock.find_active_focused()).is_some_and(|(_, t)| matches!(t, Tab::Editor(e) if !e.path.is_empty()));
            out.push(if in_editor { Action::Editor(crate::editor::Request::FindInFiles) } else { Action::FindInFiles { text: None, folder: None } });
        }
        if take("ide_mode") {
            out.push(Action::Settings(Box::new(|s| s.ide_mode = !s.ide_mode)));
        }
        let (zoom, next_pane, prev_pane) = (take("zoom_pane"), take("next_pane"), take("prev_pane"));
        // Ctrl+K, then an arrow within a second and a half: the pane that way.
        let now = ctx.input(|i| i.time);
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
            self.pane_chord = Some(now);
        }
        let mut toward = None;
        if self.pane_chord.is_some_and(|t| now - t < 1.5) {
            for (key, dir) in [(Key::ArrowLeft, crate::layouts::Direction::Left), (Key::ArrowRight, crate::layouts::Direction::Right), (Key::ArrowUp, crate::layouts::Direction::Up), (Key::ArrowDown, crate::layouts::Direction::Down)] {
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                    toward = Some(dir);
                    self.pane_chord = None;
                }
            }
        }
        if let Some(r) = self.repos.get_mut(self.active) {
            if zoom {
                r.toggle_zoom(None);
            }
            let to = match (toward, next_pane, prev_pane) {
                (Some(d), _, _) => crate::layouts::neighbour(&r.dock, d),
                (None, true, _) => crate::layouts::next_pane(&r.dock, false),
                (None, _, true) => crate::layouts::next_pane(&r.dock, true),
                _ => None,
            };
            if let Some(p) = to {
                r.dock.set_focused_node_and_surface(p);
            }
        }
        let (next, prev) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::Tab), i.consume_key(CS, Key::Tab)));
        if undo {
            out.push(Action::UndoLast);
        }
        const NUMBERS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        if let Some(n) = NUMBERS.iter().position(|k| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, *k)))
            && n < self.repos.len()
        {
            self.active = n;
        }
        if settings {
            out.push(Action::OpenSettings);
        }
        if open {
            crate::widgets::pick(&mut self.folder_pick, ctx, rfd::FileDialog::new(), true);
        }
        let Some(r) = self.repos.get_mut(self.active) else { return };
        if close_tab {
            let focused = r.dock.find_active_focused().map(|(_, t)| (t.key(), t.unsaved())).or_else(|| r.dock.iter_leaves().next().and_then(|(_, l)| l.tabs().get(l.active.0)).map(|t| (t.key(), t.unsaved())));
            // The last one stays: nothing would be left to show where things are.
            if r.dock.iter_all_tabs().count() > 1
                && let Some((key, unsaved)) = focused
            {
                match unsaved {
                    Some(lost) => out.push(Action::AskCloseTab { key, lost }),
                    None => {
                        if let Some(path) = r.dock.find_tab_from(|t| t.key() == key) {
                            r.dock.remove_tab(path);
                        }
                    }
                }
            }
        }
        if next || prev {
            let leaf = r.dock.focused_leaf().or_else(|| r.dock.iter_leaves().next().map(|(p, _)| p));
            let target = leaf.and_then(|path| {
                let (_, l) = r.dock.iter_leaves().find(|(p, _)| *p == path)?;
                let n = l.tabs().len().max(1);
                Some((path, if next { (l.active.0 + 1) % n } else { (l.active.0 + n - 1) % n }))
            });
            if let Some((path, at)) = target {
                let _ = r.dock.set_active_tab(egui_dock::TabPath::new(path.surface, path.node, egui_dock::TabIndex(at)));
            }
        }
        let st = r.data.status.get();
        // As with the toolbar's buttons: nothing a second time while the first is under way.
        let (fetch, pull, push) = (fetch && r.data.ops_now.state("fetch").is_none(), pull && r.data.ops_now.state("pull").is_none(), push && r.data.ops_now.state("push").is_none());
        if fetch && !r.data.remotes().is_empty() {
            out.push(Action::Run(ops::fetch(&ops::FetchOpts { all: true, prune: self.settings.prune_on_fetch, ..Default::default() })));
        }
        let idle = st.is_some_and(|s| s.op == RepoOp::None);
        if pull && idle && !r.data.remotes().is_empty() {
            out.push(Action::Run(ops::pull(pull_mode(&self.settings.pull_mode), self.settings.pull_autostash)));
        }
        if push && idle && !r.data.remotes().is_empty() {
            if st.is_some_and(|s| s.upstream.is_some()) {
                let names: Vec<&str> = r.data.remotes().iter().map(|x| x.name.as_str()).collect();
                out.push(Action::Run(ops::push_to_upstream(st.and_then(|s| s.branch.as_deref()), st.and_then(|s| s.upstream.as_deref()), &names)));
            } else {
                let remotes = r.data.remotes().iter().map(|x| x.name.clone()).collect();
                out.push(Action::Form(forms::push_form(st.and_then(|s| s.branch.clone()), remotes, false, self.settings.confirm_destructive)));
            }
        }
    }

    /// Every key, from one table, narrowed by what's typed.
    fn repo_scan_dialog(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        let settled = self.dialogs_settled;
        let p = self.pal.clone();
        let Some(scan) = &mut self.repo_scan else { return };
        if scan.task.poll() {
            scan.found = scan.task.value.take().unwrap_or_default();
            // Those already open don't need asking about.
            let open: Vec<&Path> = self.repos.iter().map(|r| r.repo.root.as_path()).collect();
            scan.picked = scan.found.iter().map(|f| !open.contains(&f.as_path())).collect();
            scan.done = true;
        }
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut open = false;
        dialog_window("Scan for repositories", settled).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(scan.root.to_string_lossy()).color(p.dim));
            ui.add_space(4.0);
            if !scan.done {
                spinner_row(ui, "Looking…", &p);
            } else if let Some(e) = &scan.task.error {
                error_box(ui, e, &p);
            } else if scan.found.is_empty() {
                ui.label("No repositories in this folder.");
            } else {
                egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 280.0).show(ui, |ui| {
                    for (path, on) in scan.found.iter().zip(scan.picked.iter_mut()) {
                        let name = path.strip_prefix(&scan.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
                        ui.checkbox(on, if name.is_empty() { ".".to_string() } else { name });
                    }
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let n = scan.picked.iter().filter(|x| **x).count();
                if scan.done && !scan.found.is_empty() {
                    open = ui.add_enabled(n > 0, egui::Button::new(format!("Open {}", plural(n, "repository", "repositories")))).on_disabled_hover_text("Tick the ones to open.").clicked();
                }
                close |= ui.button(if scan.done { "Close" } else { "Stop" }).clicked();
            });
        });
        if open {
            out.extend(scan.found.iter().zip(&scan.picked).filter(|(_, on)| **on).map(|(path, _)| Action::OpenRepo(path.clone())));
            close = true;
        }
        if close {
            scan.cancel.store(true, Ordering::Relaxed);
            self.repo_scan = None;
        }
    }

    fn help_dialog(&mut self, ctx: &egui::Context) {
        let Some(query) = &mut self.help else { return };
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let p = self.pal.clone();
        dialog_window("Keyboard shortcuts", self.dialogs_settled).anchor(Align2::CENTER_TOP, [0.0, 60.0]).show(ctx, |ui| {
            // As wide as the list, whose longest line sets the window's width.
            ui.set_width(580.0);
            let r = ui.add(crate::widgets::input(query).hint_text("Search").desired_width(f32::INFINITY));
            if !ui.memory(|m| m.focused().is_some()) {
                r.request_focus();
            }
            let needle = query.to_lowercase();
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 220.0).show(ui, |ui| {
                egui::Grid::new("keys").num_columns(2).spacing(egui::vec2(14.0, 4.0)).striped(true).show(ui, |ui| {
                    for (keys, what) in crate::keys::TABLE.iter().filter(|(k, w)| needle.is_empty() || k.to_lowercase().contains(&needle) || w.to_lowercase().contains(&needle)) {
                        ui.label(RichText::new(crate::keys::shown_keys(&self.settings.key_bindings, keys)).monospace().color(p.text));
                        ui.label(RichText::new(*what).color(p.dim));
                        ui.end_row();
                    }
                });
                let words: Vec<_> = crate::keys::WORDS.iter().filter(|(k, w)| needle.is_empty() || k.to_lowercase().contains(&needle) || w.to_lowercase().contains(&needle)).collect();
                if !words.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Words").strong());
                    egui::Grid::new("words").num_columns(2).spacing(egui::vec2(14.0, 4.0)).striped(true).show(ui, |ui| {
                        for (word, what) in words {
                            ui.label(RichText::new(*word).color(p.text));
                            ui.label(RichText::new(*what).color(p.dim));
                            ui.end_row();
                        }
                    });
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                close |= ui.button("Close").clicked();
                ui.label(RichText::new("Ctrl+P runs any command by name.").small().color(p.dim));
            });
        });
        if close {
            self.help = None;
        }
    }

    fn poll_global(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if let Some(g) = &mut self.global_running {
            let mut done = None;
            loop {
                match g.rx.try_recv() {
                    Ok(OpMsg::Line(l)) => g.last_line = l,
                    Ok(OpMsg::Done(r)) => done = Some(r),
                    Ok(OpMsg::Undo(_) | OpMsg::NoUndo(_)) => {}
                    Err(TryRecvError::Empty) => break,
                    // Its thread is gone. Without this the spinner stays and nothing else starts.
                    Err(TryRecvError::Disconnected) => {
                        done.get_or_insert_with(|| Err("The operation stopped unexpectedly.".to_string()));
                        break;
                    }
                }
            }
            if let Some(res) = done {
                let (title, open, is_clone) = (g.title.clone(), g.open_after.clone(), g.is_clone);
                if self.settings.notify_when_done && self.shot.is_none() && wants_attention(g.started.elapsed(), res.is_err(), ctx.input(|i| i.focused)) {
                    let (head, body) = crate::notify::text(&title, "gitem", res.is_err());
                    (self.notifier)(&head, &body);
                }
                self.global_running = None;
                match res {
                    Ok(_) => {
                        out.push(Action::Toast(ops::done_message(&title, "")));
                        if let Some(p) = open {
                            // Only a clone. `git init` can run in a folder that's already a repository.
                            if let (true, Ok(git)) = (is_clone, &self.git) {
                                let _ = gitem_core::trust::adopt_new_repo(git, &p);
                            }
                            out.push(Action::OpenRepo(p));
                        }
                    }
                    Err(e) => out.push(Action::Error(format!("{title} failed:\n{e}"))),
                }
            }
        }
        if self.global_running.is_none()
            && let (Some((op, open)), Ok(git)) = (self.global_queue.pop_front(), self.git.clone()) {
                self.global_running = Some(spawn_op(ctx, git, None, op, open));
            }
    }

    fn handle_shot(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let Some(s) = &mut self.shot else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        if !s.sent && s.at.elapsed() > Duration::from_secs(3) {
            s.sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
        });
        if let Some(img) = image {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = ::image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                let _ = buf.save(&s.path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Starts taking what later launches hand over. Only the real window does, since a test or
    /// a screenshot shouldn't take anyone's launches.
    pub fn listen_for_launches(&mut self, ctx: &egui::Context) {
        let wake = ctx.clone();
        match gitem_core::instance::listen(&gitem_core::settings::config_dir(), move || wake.request_repaint()) {
            Ok(i) => self.instance = Some(i),
            // Only means later launches open windows of their own.
            Err(e) => eprintln!("gitem: not listening for other launches: {e}"),
        }
    }

    /// For a test: take what's handed over in this folder.
    pub fn listen_in_for_test(&mut self, ctx: &egui::Context, dir: &Path) {
        let wake = ctx.clone();
        self.instance = gitem_core::instance::listen(dir, move || wake.request_repaint()).ok();
    }

    fn take_handed_over(&mut self, ctx: &egui::Context) {
        let arrived: Vec<Vec<String>> = self.instance.as_ref().map(|i| i.incoming.try_iter().collect()).unwrap_or_default();
        for args in arrived {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            for a in args {
                if a.starts_with("gitem://") {
                    if let Some(d) = gitem_core::forge::DeepLink::parse(&a) {
                        self.apply_action(ctx, Action::DeepLink(d));
                    }
                } else {
                    self.apply_action(ctx, Action::OpenRepo(PathBuf::from(a)));
                }
            }
        }
    }

    /// A scheduled look, if the user turned that on, and what any look found. A screenshot and a
    /// helper window never get here with the setting doing anything: neither saves, or asks.
    fn look_for_updates(&mut self, ctx: &egui::Context) {
        if !self.update_scheduled && self.shot.is_none() {
            self.update_scheduled = true;
            let now = gitem_core::util::now_ts();
            if gitem_core::update::due(self.settings.check_updates, &self.settings.update_url, self.settings.last_update_check, now) {
                self.settings.last_update_check = now;
                self.settings_dirty = true;
                let url = self.settings.update_url.trim().to_string();
                self.update_task.spawn(ctx, move || gitem_core::update::check(&url, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string()));
            }
        }
        if !self.update_task.poll() {
            return;
        }
        let asked = std::mem::take(&mut self.update_asked);
        if let Some(found) = self.update_task.value.take() {
            match found {
                Some(r) => {
                    self.toasts.push((format!("gitem {} is available. About has a link to it.", r.version), Instant::now()));
                    self.update_found = Some(r);
                }
                None if asked => self.update_answer = Some("This is the newest release.".to_string()),
                None => {}
            }
        }
        // Being offline is no reason to interrupt anyone who didn't ask.
        if let Some(e) = self.update_task.error.take()
            && asked
        {
            self.update_answer = Some(format!("Couldn't find out: {e}"));
        }
    }

    /// The whole UI for one frame. Separated from `eframe::App` for testing.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.apply_ca_file(&ctx);
        self.look_for_updates(&ctx);
        self.take_handed_over(&ctx);
        self.handle_shot(ui);
        // The window flashes in the taskbar until it's looked at. No more than that: a pop-up on
        // every fetch would be the reason to turn it off.
        let mut flash = false;
        for r in &mut self.repos {
            flash |= std::mem::take(&mut r.attention_due);
            if let Some((title, failed)) = r.notice_due.take()
                && self.settings.notify_when_done
                && self.shot.is_none()
            {
                let (head, body) = crate::notify::text(&title, &r.repo.name(), failed);
                (self.notifier)(&head, &body);
            }
            r.look_up_branch_pr(&ctx, &self.settings);
        }
        if flash && self.settings.attention_when_done {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
        self.check_window_close(&ctx);
        let mut out: Vec<Action> = Vec::new();
        // egui's own Ctrl+Plus and Ctrl+Minus change this without telling anyone. A zoom we set
        // only takes effect next frame, so until then egui's differs from the setting; reading
        // it back then would undo the change, and the two would swap every frame after. Only a
        // change on egui's side counts.
        let zoom = ctx.zoom_factor();
        if let Some(seen) = self.seen_zoom.replace(zoom)
            && (zoom - seen).abs() > 0.001
            && (zoom - self.settings.ui_scale.clamp(0.6, 3.0)).abs() > 0.001
        {
            self.settings.ui_scale = zoom;
            self.settings_dirty = true;
        }

        if let Some(sha) = self.shot_inspect.take() {
            out.push(Action::Inspect(sha));
        }
        for tab in std::mem::take(&mut self.shot_tabs) {
            if let Some(r) = self.repos.get_mut(self.active) {
                r.open_tab(tab);
            }
        }

        // What a window that's come back to may have missed, as when a build ran meanwhile
        // on a drive that sends no notifications.
        let focused = ctx.input(|i| i.focused);
        let was_focused = std::mem::replace(&mut self.window_focused, focused);
        let regained = focused && !was_focused;
        // Going to another window saves the editor's files, when asked to.
        if was_focused && !focused && self.settings.editor_autosave {
            out.push(Action::SaveAll);
        }
        self.frame_no += 1;
        let persist = self.persist;

        // Background upkeep for every open repository.
        for r in self.repos.iter_mut() {
            r.data.poll(&ctx, &r.repo);
            r.data.persist = persist;
            let mut local = Vec::new();
            r.poll_ops(&ctx, &mut local);
            out.extend(local);
            r.start_quiet_work(&ctx, &self.settings);
            if let Some(b) = r.data.status.get().and_then(|s| s.branch.clone())
                && r.recent_branches.first() != Some(&b)
            {
                r.recent_branches.retain(|x| *x != b);
                r.recent_branches.insert(0, b);
                r.recent_branches.truncate(6);
            }
            let job_running = r.data.jobs.load(Ordering::Relaxed) > 0;
            r.data.ops_busy = r.running.is_some() || !r.queue.is_empty() || job_running;
            r.data.ops_now = crate::data::OpsNow {
                running: r.running.as_ref().map(|x| (x.commands.clone(), x.signature.clone(), x.last_line.clone())),
                queued: r.queue.iter().map(|q| (q.op.commands(), crate::data::op_signature(&q.op))).collect(),
                background_fetch: r.quiet.as_ref().is_some_and(|q| q.is_fetch),
            };
            let seen = r.watcher.take();
            let polling = r.watcher.failed.load(Ordering::Relaxed) && r.last_poll.elapsed() >= POLL_WITHOUT_WATCHER;
            if polling {
                r.last_poll = Instant::now();
                ctx.request_repaint_after(POLL_WITHOUT_WATCHER);
            }
            if seen.work || seen.refs {
                r.data.mark_changed();
            }
            if seen.refs || polling || regained {
                r.data.refresh(&ctx, &r.repo);
            } else if seen.work || seen.ignored {
                r.data.refresh_status(&ctx, &r.repo);
            }
            // Frames only come with input, so nothing would look at these timers otherwise.
            if let Some(wait) = r.fetch_wait(self.settings.auto_fetch_minutes) {
                ctx.request_repaint_after(wait.max(Duration::from_secs(1)));
            }
            if let Some((_, t)) = r.data.waiting_on() {
                ctx.request_repaint_after(Duration::from_secs(1).max(t / 60));
            } else if r.data.status.busy() || r.data.refs.busy() {
                ctx.request_repaint_after(crate::data::STILL_WAITING);
            }
            let root = r.repo.root.clone();
            r.later.retain_mut(|t| {
                if !t.poll() {
                    return true;
                }
                if let Some(l) = t.value.take() {
                    out.push(Action::ForRepo(root.clone(), Box::new(l.into_action())));
                }
                if let Some(e) = t.error.take() {
                    out.push(Action::Error(e));
                }
                false
            });
            r.background.retain_mut(|t| {
                if !t.poll() {
                    return true;
                }
                if let Some(m) = t.value.take().filter(|m| !m.is_empty()) {
                    out.push(Action::Toast(m));
                }
                if let Some(e) = t.error.take() {
                    out.push(Action::Error(e));
                }
                r.data.refresh(&ctx, &r.repo);
                false
            });
        }
        self.poll_global(&ctx, &mut out);
        self.poll_orphans();
        self.poll_opening(&ctx);
        if self.folder_pick.poll() {
            let picked = self.folder_pick.value.take().flatten();
            let moved = self.relocating.take();
            if let Some(d) = picked {
                // Open first: if that's not it, the tab stays and says what's wrong.
                if let Some(old) = moved {
                    out.push(Action::CloseRepo(old));
                }
                out.push(Action::OpenRepo(d));
            }
        }
        self.shortcuts(&ctx, &mut out);
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let pal = self.pal.clone();
            egui::Area::new("drop-hint".into()).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).order(egui::Order::Foreground).interactable(false).show(&ctx, |ui| {
                egui::Frame::new().fill(pal.panel2).stroke(egui::Stroke::new(1.0, pal.accent)).inner_margin(egui::Margin::symmetric(16, 10)).corner_radius(6.0).show(ui, |ui| ui.label("Drop to open its repository"));
            });
        }
        // A folder dropped anywhere on the window. A file means the repository it's in.
        for dropped in ctx.input(|i| i.raw.dropped_files.clone()) {
            let path = dropped.path().to_path_buf();
            if !path.as_os_str().is_empty() {
                out.push(Action::OpenRepo(if path.is_dir() { path } else { path.parent().map(Path::to_path_buf).unwrap_or(path) }));
            }
        }

        let bar_frame = egui::Frame::new().fill(self.pal.panel).inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("title-bar").frame(egui::Frame::new().fill(self.pal.bg)).resizable(false).exact_size(chrome::TITLE_BAR_HEIGHT).show_separator_line(false).show(ui, |ui| self.title_bar(ui, &mut out));
        egui::Panel::bottom("status-bar").frame(egui::Frame::new().fill(self.pal.status_bg).inner_margin(egui::Margin::symmetric(8, 3))).show_separator_line(false).show(ui, |ui| self.status_bar(ui, &mut out));
        let pal = self.pal.clone();
        let dialog_open = self.dialog_open();
        if let Some(tab) = self.repos.get_mut(self.active)
            && tab.show_rail
        {
            let name_of = |t: &Tab| chrome::rail_top().into_iter().flatten().chain(chrome::rail_bottom()).find(|i| (i.is)(t)).map(|i| i.name);
            // Until a pane has been clicked nothing has focus, so use the first one.
            let focused = match tab.dock.find_active_focused() {
                Some((_, t)) => name_of(t),
                None => tab.dock.iter_leaves().next().and_then(|(_, l)| l.tabs().get(l.active.0)).and_then(name_of),
            };
            egui::Panel::left("rail").frame(egui::Frame::new().fill(pal.bg)).resizable(false).exact_size(chrome::rail_width(self.settings.rail_labels)).show(ui, |ui| {
                let places = chrome::RailPlaces { full: self.settings.rail_full, pinned: &self.settings.rail_pinned, hidden: &self.settings.rail_hidden };
                let ide = self.settings.ide_mode;
                // The Editor item is lit while IDE mode is on, as the switch it is.
                let lit = |i: &chrome::RailItem| if i.name == chrome::IDE_ITEM { ide } else { focused == Some(i.name) };
                match chrome::rail(ui, &lit, &places, self.settings.rail_labels, &pal) {
                    Some(chrome::RailEvent::IdeMode) => out.push(Action::Settings(Box::new(|s| s.ide_mode = !s.ide_mode))),
                    Some(chrome::RailEvent::Open(t, true)) => out.push(Action::OpenFromMore(t)),
                    Some(chrome::RailEvent::Open(t, false)) => out.push(Action::OpenTab(t)),
                    Some(chrome::RailEvent::Place(name, on)) => out.push(Action::Settings(Box::new(move |s| {
                        chrome::RailPlaces::set(name, on, &mut s.rail_pinned, &mut s.rail_hidden);
                    }))),
                    None => {}
                }
            });
        }
        if !self.repos.is_empty() {
            egui::Panel::top("top-bar").frame(bar_frame).show_separator_line(false).show(ui, |ui| self.toolbar(ui, &mut out));
        }
        // Turned on just now, by any of its ways: offer the layout made for it, once.
        if self.settings.ide_mode && !self.ide_was {
            self.ide_offer = !self.repos.is_empty();
        }
        if !self.settings.ide_mode {
            self.ide_offer = false;
        }
        self.ide_was = self.settings.ide_mode;
        if self.ide_offer {
            let pal = self.pal.clone();
            egui::Panel::top("ide-offer").frame(egui::Frame::new().fill(pal.panel2).inner_margin(egui::Margin::symmetric(10, 4))).show_separator_line(false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} IDE mode is on: a click in the Files tree opens the file for editing.", chrome::icon::CODE)).color(pal.text));
                    if crate::widgets::small_button(ui, "Use the IDE layout").on_hover_text("The editor, Changes beside it, the Console below and the Files sidebar. Every open tab is kept").clicked() {
                        out.push(Action::Layout(crate::layouts::Preset::Ide));
                        self.ide_offer = false;
                    }
                    if crate::widgets::small_button(ui, "Keep this layout").clicked() {
                        self.ide_offer = false;
                    }
                });
            });
        }

        if let Some(tab) = self.repos.get_mut(self.active) {
            // What find in files searches: the open editors, with unsaved text while one's drawn.
            let with_text = tab.data.searching.replace(false);
            let open = tab.dock.iter_all_tabs_mut().chain(tab.zoom.iter_mut().flat_map(|z| z.full.iter_all_tabs_mut())).filter_map(|(_, t)| if let Tab::Editor(v) = t { v.for_search(with_text) } else { None }).collect();
            tab.data.set_editors(open);
        }
        if let Some(tab) = self.repos.get_mut(self.active) {
            let RepoTab { repo, data, dock, zoom, inspect, show_inspect, sidebar, show_sidebar, show_rail, squeeze: tab_squeeze, peek, show_wip, .. } = tab;
            if let Some(s) = data.status.get()
                && s.op != RepoOp::None {
                    let frame = egui::Frame::new().fill(pal.banner_fill());
                    let s = s.clone();
                    let ops_busy = data.ops_busy;
                    egui::Panel::top("op-banner").frame(frame).show(ui, |ui| Self::op_banner(ui, &s, data.branches(), ops_busy, &mut out, &pal));
                }
            if let Some(problem) = data.read_failure().map(str::to_string).or_else(|| data.waiting_on().map(|(what, t)| format!("Still waiting: {what} has been running for {} s.", t.as_secs()))) {
                let frame = egui::Frame::new().fill(pal.banner_fill()).inner_margin(egui::Margin::symmetric(8, 4));
                let gone = !repo.root.exists();
                egui::Panel::top("read-banner").frame(frame).show_separator_line(false).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let title = if gone { "This repository's folder is gone. It may have been moved, renamed or deleted." } else if data.read_failure().is_some() { "Git can't read this repository. What's showing may be out of date." } else { "Git isn't answering." };
                        ui.colored_label(pal.warn, title);
                        ui.add(egui::Label::new(RichText::new(problem.as_str()).small().monospace().color(pal.dim)).selectable(true));
                        let label = if data.read_failure().is_some() { "Try again" } else { "Stop and try again" };
                        if crate::widgets::small_button(ui, label).clicked() {
                            out.push(Action::Retry);
                        }
                        if crate::widgets::small_button(ui, "Copy").clicked() {
                            out.push(Action::Copy(problem.clone()));
                        }
                        if gone {
                            if crate::widgets::small_button(ui, "Locate…").on_hover_text("Find where it went, and open it from there").clicked() {
                                out.push(Action::LocateRepo(repo.root.clone()));
                            }
                            if crate::widgets::small_button(ui, "Close").on_hover_text("Close this repository's tab. Nothing on disk is touched.").clicked() {
                                out.push(Action::CloseRepo(repo.root.clone()));
                            }
                        }
                    });
                });
            }
            if repo.trust.restricted() {
                let frame = egui::Frame::new().fill(pal.banner_fill()).inner_margin(egui::Margin::symmetric(8, 4));
                egui::Panel::top("trust-banner").frame(frame).show_separator_line(false).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(pal.warn, "Programs from this repository's config are turned off.");
                        ui.label(RichText::new(RESTRICTED_NOTE).small().color(pal.dim));
                        if crate::widgets::small_button(ui, "Review…").clicked() {
                            out.push(Action::ReviewTrust);
                        }
                    });
                });
            }
            let screen_w = ctx.content_rect().width();
            let rail_room = if *show_rail { chrome::rail_width(self.settings.rail_labels) } else { 0.0 };
            let squeeze = squeeze_for(screen_w, rail_room, *show_sidebar, *show_inspect);
            *tab_squeeze = squeeze;
            if !squeeze.details && *peek == Some(Peek::Details) || !squeeze.sidebar && *peek == Some(Peek::Sidebar) {
                *peek = None;
            }
            if *show_sidebar && !squeeze.sidebar {
                egui::Panel::left("sidebar").resizable(true).default_size(SIDEBAR_WIDTH).size_range(180.0..=600.0).show(ui, |ui| {
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: true };
                    ui.push_id(&cx.repo.root, |ui| sidebar.ui(ui, &mut cx));
                });
            }
            // With nothing picked, or beside a view with no commits, it's only lost room.
            let front_shows_commits = dock.find_active_focused().map(|(_, t)| t.shows_commits()).or_else(|| dock.iter_leaves().next().and_then(|(_, l)| l.tabs().get(l.active.0)).map(Tab::shows_commits)).unwrap_or(true);
            // The tab it draws from can have been closed since.
            *show_wip = *show_wip && dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Changes(_)));
            if *show_inspect && !squeeze.details && (inspect.sha.is_some() || *show_wip) && front_shows_commits {
                egui::Panel::right("inspect").resizable(true).default_size(DETAILS_WIDTH).size_range(220.0..=800.0).show(ui, |ui| {
                    // A short message mustn't shrink the panel and move everything beside it.
                    ui.set_min_width(ui.available_width());
                    if *show_wip {
                        // One commit box on screen at a time. In a split it can be showing already.
                        let in_a_pane = dock.iter_leaves().any(|(_, l)| l.tabs().get(l.active.0).is_some_and(|t| matches!(t, Tab::Changes(_))));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Work in progress").strong());
                            if crate::widgets::small_button(ui, "Open Changes").clicked() {
                                out.push(Action::OpenTab(Tab::changes()));
                            }
                        });
                        ui.separator();
                        if in_a_pane {
                            empty_state(ui, "Changes is showing", "Its commit box is beside this.", &pal);
                        } else if let Some(view) = dock.iter_all_tabs_mut().find_map(|(_, t)| if let Tab::Changes(v) = t { Some(v) } else { None }) {
                            // The keyboard stays with the graph.
                            let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: false };
                            ui.push_id((&cx.repo.root, "wip"), |ui| view.details_ui(ui, &mut cx));
                        }
                    } else {
                        let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: true };
                        ui.push_id(&cx.repo.root, |ui| inspect.ui(ui, &mut cx));
                    }
                });
            }
            let style = theme::dock_style(ui.style(), &pal);
            let front = dock.find_active_focused().map(|(_, t)| t.key()).or_else(|| dock.iter_leaves().next().and_then(|(_, l)| l.tabs().get(l.active.0)).map(Tab::key));
            let pane_sizes = dock.iter_leaves().flat_map(|(_, l)| l.tabs.iter().map(|t| (t.key(), l.tabs.len())).collect::<Vec<_>>()).collect();
            let panes = dock.iter_leaves().count();
            let zoomed = zoom.is_some();
            if zoomed {
                egui::Panel::top("zoom-bar").frame(egui::Frame::new().fill(pal.panel2).inner_margin(egui::Margin::symmetric(8, 3))).show_separator_line(false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} One pane fills the window. The rest of the layout is kept.", chrome::icon::CORNERS_OUT)).color(pal.dim));
                        if crate::widgets::small_button(ui, "Restore layout").on_hover_text(crate::keys::chord_label(&self.settings.key_bindings, "zoom_pane")).clicked() {
                            out.push(Action::Dock { key: String::new(), op: crate::layouts::DockOp::Zoom });
                        }
                    });
                });
            }
            let mut viewer = Viewer { cx: Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: true }, front, pane_sizes, panes, zoomed };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                DockArea::new(dock).id(egui::Id::new(DOCK_ID)).style(style).show_add_buttons(true).show_leaf_collapse_buttons(false).show_leaf_close_all_buttons(false).show_inside(ui, &mut viewer);
                paint_active_tab_markers(ui, dock, &pal);
                // egui_dock shows a grab cursor on any hovered tab. Keep the usual pointer
                // until a tab is actually being dragged, which has its own cursor.
                if ui.ctx().output(|o| o.cursor_icon) == egui::CursorIcon::Grab {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                }
            });

            // Over the view, where there's no room beside it. A click elsewhere puts it away.
            if let Some(which) = *peek {
                let width = (screen_w - 60.0).min(if which == Peek::Details { DETAILS_WIDTH } else { SIDEBAR_WIDTH });
                let top = chrome::TITLE_BAR_HEIGHT + 34.0;
                let (anchor, x) = if which == Peek::Details { (Align2::RIGHT_TOP, -2.0) } else { (Align2::LEFT_TOP, rail_room) };
                let shown = egui::Area::new("side-peek".into()).order(egui::Order::Middle).anchor(anchor, [x, top]).show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).fill(pal.panel).show(ui, |ui| {
                        ui.set_width(width);
                        ui.set_height(ctx.content_rect().height() - top - 40.0);
                        let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: false };
                        ui.push_id(&cx.repo.root, |ui| if which == Peek::Details { inspect.ui(ui, &mut cx) } else { sidebar.ui(ui, &mut cx) });
                    });
                });
                if shown.response.clicked_elsewhere() && !egui::Popup::is_any_open(&ctx) {
                    *peek = None;
                }
            }
            if let Some(pl) = &mut self.palette {
                pl.outside = None;
                let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: true };
                if pl.show(&ctx, &mut cx) {
                    self.palette = None;
                }
            }
        } else {
            if self.no_repo.is_none() && (self.no_repo_tab.is_some() || self.palette.is_some()) {
                self.no_repo = Some(Self::make_no_repo(&self.git));
            }
            if let (Some(tab), Some(s)) = (&mut self.no_repo_tab, &self.no_repo) {
                let mut back = false;
                egui::Panel::top("no-repo-back").frame(bar_frame).show_separator_line(false).show(ui, |ui| {
                    back = ui.button(format!("{} Back to the start", chrome::icon::ARROW_LEFT)).clicked();
                });
                let settings = &self.settings;
                egui::CentralPanel::default().show(ui, |ui| {
                    let NoRepo { repo, data } = &**s;
                    let mut cx = Cx { repo, data, settings, pal: &pal, out: &mut out, dialog_open, in_front: true };
                    tab.ui(ui, &mut cx);
                });
                if back {
                    self.no_repo_tab = None;
                }
            } else {
                egui::CentralPanel::default().show(ui, |ui| self.welcome(ui, &mut out));
            }
            if let (Some(pl), Some(s)) = (&mut self.palette, &self.no_repo) {
                pl.outside = Some(self.git.is_ok());
                let NoRepo { repo, data } = &**s;
                let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open, in_front: true };
                if pl.show(&ctx, &mut cx) {
                    self.palette = None;
                }
            }
        }

        self.settle_dialogs(&ctx);
        self.dialogs(&ctx, &mut out);
        self.crash_dialog(&ctx);
        self.git_warning_dialog(&ctx);
        self.reopen_dialog(&ctx);
        self.close_dialog(&ctx);
        self.dirty_dialog(&ctx);
        self.lock_dialog(&ctx);
        self.trust_dialog(&ctx);
        self.link_dialog(&ctx);
        self.about(&ctx, &mut out);
        self.help_dialog(&ctx);
        self.repo_scan_dialog(&ctx, &mut out);
        chrome::window_edges(&ctx, &pal);
        for a in out {
            self.apply_action(&ctx, a);
        }
        if std::mem::take(&mut self.settings_dirty) {
            self.save_settings();
        }
    }
}

impl eframe::App for GitemApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if !std::mem::replace(&mut self.styled_window, true) {
            chrome::native_window_style(frame);
            crate::notify::set_window(frame);
            crate::widgets::set_dialog_parent(frame);
        }
        #[cfg(not(windows))]
        {
            let _ = frame;
            crate::notify::window_is_up();
        }
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        crate::notify::clear();
        self.save_settings();
    }
}

/// A long job finishing, or anything failing, while you're working elsewhere is worth a mention.
fn wants_attention(took: Duration, failed: bool, focused: bool) -> bool {
    !focused && (failed || took >= Duration::from_secs(10))
}

#[test]
fn only_what_finishes_out_of_sight_and_matters_asks_for_attention() {
    let (short, long) = (Duration::from_secs(1), Duration::from_secs(30));
    assert!(!wants_attention(long, true, true), "you're looking at it");
    assert!(!wants_attention(short, false, false), "a moment's work");
    assert!(wants_attention(long, false, false));
    assert!(wants_attention(short, true, false));
}

/// What's going on in one open repository, or None until it's been read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoGlance {
    pub changed: usize,
    pub conflicts: usize,
    pub ahead: u32,
    pub behind: u32,
    /// `Rebasing` and the like.
    pub doing: &'static str,
}

/// `3 open: 1 with conflicts, 2 changed, 1 to pull, 1 to push`.
pub fn glance_summary(all: &[Option<RepoGlance>]) -> String {
    let read: Vec<&RepoGlance> = all.iter().flatten().collect();
    let count = |f: fn(&RepoGlance) -> bool| read.iter().filter(|g| f(g)).count();
    let parts: Vec<String> = [
        (count(|g| g.conflicts > 0), "with conflicts"),
        (count(|g| !g.doing.is_empty() && g.conflicts == 0), "mid-operation"),
        (count(|g| g.changed > 0), "changed"),
        (count(|g| g.behind > 0), "to pull"),
        (count(|g| g.ahead > 0), "to push"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, w)| format!("{n} {w}"))
    .collect();
    if !parts.is_empty() {
        format!("{} open: {}", all.len(), parts.join(", "))
    } else if read.len() == all.len() {
        format!("{} open, all clean", all.len())
    } else {
        format!("{} open", all.len())
    }
}

/// `gitem: main, 3 changed, 1 to push`.
pub fn glance_row(name: &str, branch: &str, g: Option<&RepoGlance>) -> String {
    let Some(g) = g else { return format!("{name}: not read yet") };
    let mut bits = vec![if branch.is_empty() { "detached".to_string() } else { branch.to_string() }];
    if !g.doing.is_empty() {
        bits.push(g.doing.to_lowercase());
    }
    if g.conflicts > 0 {
        bits.push(plural(g.conflicts, "conflict", "conflicts"));
    }
    if g.changed > 0 {
        bits.push(format!("{} changed", grouped(g.changed)));
    }
    if g.behind > 0 {
        bits.push(format!("{} to pull", g.behind));
    }
    if g.ahead > 0 {
        bits.push(format!("{} to push", g.ahead));
    }
    format!("{name}: {}", bits.join(", "))
}

/// The status bar's line about every open repository, and a row for each. Only from what each one
/// last read, and `counts` keeps its answer until the next read, so a frame costs next to nothing:
/// no git, no disk, no network.
fn across_open(repos: &[RepoTab]) -> (String, Vec<String>) {
    let glances: Vec<Option<RepoGlance>> = repos
        .iter()
        .map(|r| {
            r.data.status.get().map(|s| {
                let n = r.data.counts();
                RepoGlance { changed: s.entries.len().saturating_sub(n.conflicts), conflicts: n.conflicts, ahead: s.ahead, behind: s.behind, doing: s.op.label() }
            })
        })
        .collect();
    let rows = repos.iter().zip(&glances).map(|(r, g)| glance_row(&r.repo.name(), r.data.status.get().and_then(|s| s.branch.as_deref()).unwrap_or(""), g.as_ref())).collect();
    (glance_summary(&glances), rows)
}

#[test]
fn the_line_across_repositories_says_what_needs_you_and_nothing_else() {
    let g = |changed, conflicts, ahead, behind, doing| Some(RepoGlance { changed, conflicts, ahead, behind, doing });
    let clean = g(0, 0, 0, 0, "");
    assert_eq!(glance_summary(&[clean.clone(), clean.clone()]), "2 open, all clean");
    assert_eq!(glance_summary(&[clean.clone(), None]), "2 open", "it can't say all clean until it's read them all");
    assert_eq!(glance_summary(&[g(2, 1, 0, 0, "Merging"), g(3, 0, 1, 0, ""), g(0, 0, 1, 2, "Rebasing")]), "3 open: 1 with conflicts, 1 mid-operation, 2 changed, 1 to pull, 2 to push");
    assert_eq!(glance_row("a", "main", g(1200, 0, 1, 0, "").as_ref()), "a: main, 1,200 changed, 1 to push");
    assert_eq!(glance_row("a", "", g(0, 2, 0, 0, "Merging").as_ref()), "a: detached, merging, 2 conflicts");
    assert_eq!(glance_row("a", "main", None), "a: not read yet");
}

/// Reads whether it can still be done, off the UI thread, then does it.
fn undo_action(repo: &Repo, entry: gitem_core::undo::UndoEntry, confirming: bool) -> Action {
    let repo = repo.clone();
    Action::Later(Box::new(move || {
        if let Some(why) = entry.refusal(&repo.git, &repo.root) {
            return Err(why);
        }
        let entry = entry.as_it_stands(&repo.git, &repo.root);
        Ok(if confirming {
            crate::action::Later::Confirm { title: entry.label.clone(), body: entry.detail.clone(), label: entry.label, op: entry.op }
        } else {
            crate::action::Later::Run(entry.op)
        })
    }))
}

pub fn pull_mode(setting: &str) -> ops::PullMode {
    match setting {
        "ff-only" => ops::PullMode::FfOnly,
        "rebase" => ops::PullMode::Rebase,
        "merge" => ops::PullMode::Merge,
        _ => ops::PullMode::Default,
    }
}

fn forget_recent(path: &str) -> Action {
    let path = path.to_string();
    Action::Settings(Box::new(move |s| s.recent_repos.retain(|r| *r != path)))
}

/// A new terminal window in `dir`, optionally running a command there. The folder
/// is passed as the working directory, so its name is never parsed by a shell.
fn open_terminal(dir: &Path, command: Option<&str>, choice: &str, errors: &mut Vec<String>) {
    if let Err(e) = chosen_terminal(dir, command, choice).unwrap_or_else(|| terminal_command(dir, command)).spawn() {
        errors.push(format!("Could not open a terminal: {e}"));
    }
}

/// Quoted for a POSIX shell, as a single word.
#[cfg_attr(windows, allow(dead_code))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// AppleScript for Terminal to run a command in a folder.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_terminal_script(dir: &Path, cmd: &str) -> String {
    let shell = format!("cd {} && {cmd}", sh_quote(&dir.to_string_lossy()));
    format!("tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell", shell.replace('\\', r"\\").replace('"', r#"\""#))
}

#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn unix_terminal_args(cmd: &str) -> Vec<String> {
    vec!["-e".to_string(), "sh".to_string(), "-c".to_string(), format!("{cmd}; exec \"${{SHELL:-sh}}\"")]
}

/// The program and arguments for a terminal the user chose. None means the system's plain one.
/// Names work out what they mean, and anything else is taken as a program with its arguments.
fn terminal_parts(choice: &str, command: Option<&str>, dir: &Path) -> Option<(String, Vec<String>)> {
    let choice = choice.trim();
    let dir_text = dir.to_string_lossy();
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    Some(match (choice, cfg!(windows)) {
        ("", _) | ("cmd", true) => return None,
        ("powershell" | "pwsh", true) => {
            let mut a = owned(&["-NoExit"]);
            if let Some(c) = command {
                a.extend(owned(&["-Command", c]));
            }
            (format!("{choice}.exe"), a)
        }
        ("wt", true) => {
            let mut a = vec!["-d".to_string(), dir_text.into_owned()];
            if let Some(c) = command {
                a.extend(owned(&["cmd", "/K", c]));
            }
            ("wt.exe".to_string(), a)
        }
        ("git-bash", true) => {
            let root = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
            let mut a = vec![format!("--cd={dir_text}")];
            if let Some(c) = command {
                a.push("-c".to_string());
                a.push(format!("{c}; exec bash"));
            }
            (format!(r"{root}\Git\git-bash.exe"), a)
        }
        _ => {
            let mut words = gitem_core::ops::split_command_line(choice);
            if words.is_empty() {
                return None;
            }
            let program = words.remove(0);
            words.extend(command.map(String::from));
            (program, words)
        }
    })
}

fn chosen_terminal(dir: &Path, command: Option<&str>, choice: &str) -> Option<std::process::Command> {
    let (program, args) = terminal_parts(choice, command, dir)?;
    let mut c = std::process::Command::new(program);
    c.args(args).current_dir(dir).env("GITEM_WORKTREE_PATH", dir);
    // Its command is found on PATH, not in the repository it opens in, whatever the terminal.
    if command.is_some() {
        c.env("NoDefaultCurrentDirectoryInExePath", "1");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
    }
    Some(c)
}

fn terminal_command(dir: &Path, command: Option<&str>) -> std::process::Command {
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd.exe");
        if let Some(cmd) = command {
            // As written: Rust's quoting is for programs that parse arguments the C way, and
            // cmd sees `\"` as a quote that ends one and leaves the prompt split into words.
            // `/S` takes the outer quotes off and runs what's between them just as it is.
            c.raw_arg(format!("/S /K \"{cmd}\""));
            // cmd looks in the current folder before PATH, and the folder is a repository,
            // so a tracked `claude.cmd` would run in place of the real one. A plain
            // terminal stays as the user expects it, since they type there themselves.
            c.env("NoDefaultCurrentDirectoryInExePath", "1");
        }
        c.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
        c
    };
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c;
        match command {
            // `open` can only show a folder, so a command needs Terminal asked directly.
            Some(cmd) => {
                c = std::process::Command::new("osascript");
                c.args(["-e", &macos_terminal_script(dir, cmd)]);
            }
            None => {
                c = std::process::Command::new("open");
                c.args(["-a", "Terminal"]).arg(dir);
            }
        }
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = {
        let mut c = std::process::Command::new("x-terminal-emulator");
        if let Some(cmd) = command {
            // Emulators differ on whether `-e` takes one string or a program and arguments,
            // but all of them run this form. The shell at the end keeps the window open.
            c.args(unix_terminal_args(cmd));
        }
        c
    };
    c.current_dir(dir).env("GITEM_WORKTREE_PATH", dir);
    c
}

/// Open a file in the configured editor, otherwise only reveal it. Repository
/// content is untrusted: a file is never launched through its own association,
/// which could run an executable from the repo.
/// Opens `path` in the editor, or reveals it in the file manager if none is set.
pub fn open_file(path: &Path, editor: &str, errors: &mut Vec<String>) {
    let mut c;
    if !editor.trim().is_empty() {
        let program = gitem_core::editors::resolve(editor);
        c = std::process::Command::new(&program);
        c.arg(path);
        // A launcher script would otherwise flash a console window.
        #[cfg(windows)]
        if program.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")) {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
    } else {
        #[cfg(windows)]
        {
            // A file manager registered for folders, such as Directory Opus, Files or XYplorer,
            // gets the folder. Only a folder goes to the shell, never the file: a repository's
            // file is never started through its own association.
            if let Some(handler) = folder_handler() {
                let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
                if !shell_open_folder(dir) {
                    errors.push(format!("Could not open {} in {handler}", dir.display()));
                }
                return;
            }
            c = std::process::Command::new("explorer");
            if path.is_dir() {
                c.arg(path);
            } else {
                use std::os::windows::process::CommandExt;
                c.raw_arg(format!("/select,\"{}\"", path.to_string_lossy().replace('/', "\\")));
            }
        }
        #[cfg(target_os = "macos")]
        {
            c = std::process::Command::new("open");
            c.arg("-R").arg(path);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            c = std::process::Command::new("xdg-open");
            c.arg(if path.is_dir() { path } else { path.parent().unwrap_or(path) });
        }
    }
    if let Err(e) = c.spawn() {
        errors.push(format!("Could not open {}: {e}", path.display()));
    }
}

/// The command Windows runs to open a folder, as the user's file manager registered it. File
/// managers register on `Folder` or on `Directory`. Explorer's own default has no command (it
/// starts through COM), which reads as none here.
#[cfg(windows)]
fn folder_handler() -> Option<String> {
    use windows_sys::Win32::UI::Shell::{ASSOCF_NOTRUNCATE, ASSOCSTR_COMMAND, AssocQueryStringW};
    ["Folder", "Directory"].into_iter().find_map(|class| {
        let class: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
        let mut buf = vec![0u16; 2048];
        let mut len = buf.len() as u32;
        // SAFETY: `class` is NUL-terminated and `buf` holds `len` UTF-16 units.
        let hr = unsafe { AssocQueryStringW(ASSOCF_NOTRUNCATE, ASSOCSTR_COMMAND, class.as_ptr(), std::ptr::null(), buf.as_mut_ptr(), &mut len) };
        if hr != 0 {
            return None;
        }
        let end = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end])).filter(|c| !c.trim().is_empty() && !is_explorer(c))
    })
}

/// Whether that command is Explorer's own, which `explorer /select` can do better: it also
/// selects the file.
pub fn is_explorer(command: &str) -> bool {
    let program = match command.trim_start().strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program).to_ascii_lowercase();
    name == "explorer.exe" || name == "explorer"
}

/// Opens a folder with its default verb, which is the registered file manager's.
#[cfg(windows)]
fn shell_open_folder(dir: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if !dir.is_dir() {
        return false;
    }
    let wide: Vec<u16> = dir.as_os_str().to_string_lossy().replace('/', "\\").encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated; a null verb means the folder's default one.
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), wide.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    r as usize > 32
}

#[cfg(test)]
mod reveal_tests {
    use super::is_explorer;

    #[test]
    fn explorer_is_told_from_other_file_managers() {
        assert!(is_explorer(r"%SystemRoot%\Explorer.exe"));
        assert!(is_explorer(r#""C:\Windows\explorer.exe" /idlist,%I,%L"#));
        assert!(is_explorer("explorer"));
        assert!(!is_explorer(r#""C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe" /cmd Go "%1" NEW"#));
        assert!(!is_explorer(r#""C:\Users\me\AppData\Local\Files\Files.exe" "%1""#));
        assert!(!is_explorer(r"C:\Tools\xyplorer.exe %1"));
    }
}

pub use mergeview::MergeSource;

#[cfg(test)]
mod terminal_choice_tests {
    use super::*;

    #[test]
    fn empty_means_the_plain_one_and_a_program_keeps_its_arguments() {
        let dir = Path::new("some dir");
        assert_eq!(terminal_parts("", Some("x"), dir), None);
        assert_eq!(terminal_parts("  ", None, dir), None);
        let (program, args) = terminal_parts("alacritty --hold", Some("run me"), dir).unwrap();
        assert_eq!((program.as_str(), args), ("alacritty", vec!["--hold".to_string(), "run me".to_string()]));
    }

    #[cfg(windows)]
    #[test]
    fn each_known_terminal_is_told_where_and_what() {
        let dir = Path::new("C:/a b");
        assert_eq!(terminal_parts("cmd", None, dir), None);
        assert_eq!(terminal_parts("pwsh", Some("t"), dir), Some(("pwsh.exe".to_string(), ["-NoExit", "-Command", "t"].map(String::from).to_vec())));
        assert_eq!(terminal_parts("powershell", None, dir), Some(("powershell.exe".to_string(), vec!["-NoExit".to_string()])));
        assert_eq!(terminal_parts("wt", Some("t"), dir), Some(("wt.exe".to_string(), ["-d", "C:/a b", "cmd", "/K", "t"].map(String::from).to_vec())));
        let (bash, args) = terminal_parts("git-bash", Some("t"), dir).unwrap();
        assert!(bash.ends_with(r"Git\git-bash.exe"), "{bash}");
        assert_eq!(args, ["--cd=C:/a b", "-c", "t; exec bash"].map(String::from).to_vec());
    }
}

#[cfg(all(test, windows))]
mod terminal_tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    /// A repository holding a tracked-looking command, and a PATH holding the real one.
    /// Each leaves a file behind saying which one ran.
    struct Agents {
        repo: tempfile::TempDir,
        path_dir: tempfile::TempDir,
    }

    const NAME: &str = "gitemfakeagent";

    fn agents() -> Agents {
        let (repo, path_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(repo.path().join(format!("{NAME}.cmd")), "@echo repo> \"%~dp0ran-from-repo.txt\"\r\n").unwrap();
        std::fs::write(path_dir.path().join(format!("{NAME}.cmd")), "@echo path> \"%~dp0ran-from-path.txt\"\r\n").unwrap();
        Agents { repo, path_dir }
    }

    fn ran(a: &Agents, mut c: std::process::Command) -> (bool, bool) {
        let path = format!("{};{}", a.path_dir.path().display(), std::env::var("PATH").unwrap());
        // No window, and `& exit` in the command, so this ends by itself.
        assert!(c.env("PATH", path).stdin(std::process::Stdio::null()).creation_flags(0x0800_0000).status().unwrap().success());
        (a.repo.path().join("ran-from-repo.txt").exists(), a.path_dir.path().join("ran-from-path.txt").exists())
    }

    #[test]
    fn a_command_named_in_settings_comes_from_path_not_from_the_repository() {
        let a = agents();
        let c = terminal_command(a.repo.path(), Some(&format!("{NAME} & exit")));
        assert_eq!(ran(&a, c), (false, true));
    }

    /// Shows the test above can tell the difference, whatever this machine's defaults are.
    #[test]
    fn without_the_variable_cmd_runs_the_one_in_the_repository() {
        let a = agents();
        let mut c = terminal_command(a.repo.path(), Some(&format!("{NAME} & exit")));
        c.env_remove("NoDefaultCurrentDirectoryInExePath");
        assert_eq!(ran(&a, c), (true, false));
    }

    #[test]
    fn a_plain_terminal_is_left_as_it_was() {
        let c = terminal_command(Path::new("."), None);
        assert!(!c.get_envs().any(|(k, _)| k == "NoDefaultCurrentDirectoryInExePath"));
        assert_eq!(c.get_args().count(), 0);
    }

    /// A prompt in quotes reaches the program as one argument. cmd is what parses it, so
    /// it has to run for real.
    #[test]
    fn a_quoted_prompt_reaches_the_agent_in_one_piece() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("args.txt");
        // Its own exe shows what a program would be given, with no quoting of ours in the way.
        let script = d.path().join("show.cmd");
        std::fs::write(&script, format!("@echo [%1]>\"{}\"\r\n@echo [%2]>>\"{}\"\r\n", out.display(), out.display())).unwrap();
        let bin = d.path().to_string_lossy().into_owned();
        let mut c = terminal_command(d.path(), Some("show \"Address the review notes in C:/repo/.git/notes.md\" second & exit"));
        c.env("PATH", format!("{bin};{}", std::env::var("PATH").unwrap_or_default()));
        use std::os::windows::process::CommandExt;
        assert!(c.stdin(std::process::Stdio::null()).creation_flags(0x0800_0000).status().unwrap().success());
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines, ["[\"Address the review notes in C:/repo/.git/notes.md\"]", "[second]"]);
    }
}

#[cfg(test)]
mod terminal_script_tests {
    use super::*;

    #[test]
    fn a_macos_terminal_runs_the_command_in_the_folder() {
        let s = macos_terminal_script(Path::new("/tmp/it's a repo"), "claude \"do the thing\"");
        assert!(s.contains("do script"), "{s}");
        assert!(s.contains(r#"it'\\''s a repo"#), "{s}");
        assert!(s.contains(r#"claude \"do the thing\"#), "{s}");
    }

    #[test]
    fn a_unix_terminal_gets_a_program_and_arguments() {
        let a = unix_terminal_args("claude --resume 1");
        assert_eq!(&a[..3], ["-e", "sh", "-c"]);
        assert!(a[3].starts_with("claude --resume 1; exec"), "{}", a[3]);
    }
}
