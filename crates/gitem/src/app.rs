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
    Done(Result<String, String>),
}

struct Running {
    title: String,
    rx: Receiver<OpMsg>,
    cancel: Arc<AtomicBool>,
    last_line: String,
    network: bool,
    open_after: Option<PathBuf>,
    /// Stopping it loses nothing, so it's no reason to ask before closing.
    harmless: bool,
    /// It changes the repository's config by the user's own hand in gitem.
    own_config: bool,
    /// It clones, so the folder it leaves behind is gitem's own work.
    is_clone: bool,
    /// What was asked for together with it. See [`Queued`].
    batch: u64,
    started: Instant,
    /// Cancel was pressed, on something where that needs saying twice.
    asking_to_stop: bool,
}

/// A fetch only updates remote-tracking refs, and can simply be done again.
fn is_harmless(op: &GitOp) -> bool {
    op.steps.iter().all(|s| s.args.first().is_some_and(|a| a == "fetch"))
}

fn spawn_op(ctx: &egui::Context, git: GitExe, cwd: Option<PathBuf>, op: GitOp, open_after: Option<PathBuf>) -> Running {
    let own_config = op.own_config;
    let is_clone = op.steps.first().and_then(|s| s.args.first()).is_some_and(|a| a == "clone");
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let ctx2 = ctx.clone();
    let (title, network, harmless) = (op.title.clone(), op.network, is_harmless(&op));
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let ctx3 = ctx2.clone();
        let mut on_line = move |l: &str| {
            let _ = tx2.send(OpMsg::Line(l.to_string()));
            ctx3.request_repaint();
        };
        let res = op.run(&git, cwd.as_deref(), Some(c2), &mut on_line).map(|o| op.outcome_note(&o).unwrap_or_default()).map_err(|e| e.to_string());
        let _ = tx.send(OpMsg::Done(res));
        ctx2.request_repaint();
    });
    Running { title, rx, cancel, last_line: String::new(), network, open_after, harmless, own_config, is_clone, batch: 0, started: Instant::now(), asking_to_stop: false }
}

/// A write waiting its turn. `batch` tells what was asked for together: the steps of one
/// action assume the earlier ones worked, and later actions don't.
struct Queued {
    op: GitOp,
    open_after: Option<PathBuf>,
    batch: u64,
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
        }
    }

    /// What closing this repository's tab would lose, in a list for the user.
    fn at_stake(&self, running_note: &str) -> Vec<String> {
        let mut items: Vec<String> = self.dock.iter_all_tabs().filter_map(|(_, t)| t.unsaved()).collect();
        if let Some(r) = self.running.as_ref().filter(|r| !r.harmless) {
            items.push(format!("{} is running. {running_note}", r.title));
        }
        let queued = self.queue.iter().filter(|q| !is_harmless(&q.op)).count();
        if queued > 0 {
            items.push(format!("{queued} queued git command(s) won't run."));
        }
        items
    }

    pub fn open_tab(&mut self, tab: Tab) {
        // Diffs opened from lists reuse one preview tab, unless it's been pinned.
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
                let _ = self.dock.set_active_tab(path);
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
                let (own_config, batch) = (r.own_config, r.batch);
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
                        if !note.is_empty() {
                            out.push(Action::Toast(format!("{title}: {note}")));
                        } else if self.queue.is_empty() {
                            out.push(Action::Toast(format!("{title}: done")));
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
                        let not_run = if dropped > 0 { format!("\n\n{dropped} queued command(s) after it were not run.") } else { String::new() };
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
                self.running = Some(running);
            }
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
    Stash,
    AddRemote,
    Credentials,
}

impl ErrorFix {
    fn label(self) -> &'static str {
        match self {
            ErrorFix::SetUpstream => "Set upstream…",
            ErrorFix::PullOptions => "Pull options…",
            ErrorFix::Stash => "Stash…",
            ErrorFix::AddRemote => "Add a remote…",
            ErrorFix::Credentials => "Open Credentials",
        }
    }
}

/// Plain words for what git said, and what to do about it. The whole message stays available
/// behind this. Based on git's C-locale messages, which is what gitem makes it print.
pub fn explain_error(text: &str) -> Option<(&'static str, ErrorFix)> {
    let has = |s: &str| text.contains(s);
    if has("no tracking information") || has("has no upstream branch") {
        Some(("This branch isn't linked to a branch on a remote yet.", ErrorFix::SetUpstream))
    } else if has("would be overwritten by") || has("Please commit your changes or stash them") {
        Some(("You have uncommitted changes in files this would replace. Stash them, or commit them, and try again.", ErrorFix::Stash))
    } else if has("No configured push destination") || has("does not appear to be a git repository") || has("No remote repository specified") {
        Some(("There's no remote to talk to. Add one, or check the name of the one you meant.", ErrorFix::AddRemote))
    } else if has("reconcile divergent branches") || has("non-fast-forward") || has("fetch first") || has("Not possible to fast-forward") {
        Some(("This branch and its remote have both moved on. Bring in the remote's commits first.", ErrorFix::PullOptions))
    } else if has("Authentication failed") || has("could not read Username") || has("could not read Password") || has("Permission denied (publickey") || has("terminal prompts disabled") {
        Some(("The remote didn't accept your sign-in. Check the saved credentials or your SSH key.", ErrorFix::Credentials))
    } else {
        None
    }
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
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(tab.key())
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        egui::Frame::new().inner_margin(6.0).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            tab.ui(ui, &mut self.cx);
        });
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

/// What a yes in the close dialog goes on to close.
enum CloseTarget {
    Tab { root: PathBuf, key: String },
    Repo(PathBuf),
    Window,
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
    open_path: String,
    settings_dirty: bool,
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
    /// Settings shown with no repository open.
    no_repo_settings: Option<Box<NoRepoSettings>>,
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
    close_guard: Option<CloseGuard>,
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
}

struct Opening {
    path: PathBuf,
    task: crate::task::Task<Repo>,
    /// From the saved session: if it can't be reached, it's only kept for next time.
    quiet: bool,
}

struct NoRepoSettings {
    repo: Repo,
    data: RepoData,
    view: crate::views::settings::SettingsView,
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
            open_path: String::new(),
            settings_dirty: false,
            shot: None,
            shot_tabs: Vec::new(),
            shot_inspect: None,
            persist: false,
            about_open: false,
            about_git: None,
            crash_reports: Vec::new(),
            #[cfg(windows)]
            styled_window: false,
            no_repo_settings: None,
            settings_baseline: None,
            session_kept: Vec::new(),
            reopen_failed: Vec::new(),
            lock_prompt: None,
            link_prompt: None,
            git_running: gitem_core::runner::git_processes_running,
            close_guard: None,
            closing: false,
            orphans: Vec::new(),
            frame_no: 0,
            window_focused: true,
            opening: Vec::new(),
            folder_pick: Default::default(),
        }
    }

    /// The errors waiting to be seen, oldest first.
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
        egui::Window::new("The repository is locked").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.add(egui::Label::new(RichText::new(prompt.why.as_str()).monospace().small()).selectable(true));
            ui.add_space(6.0);
            ui.label("git refuses to change anything while this file exists:");
            ui.add(egui::Label::new(RichText::new(lock.display().to_string()).monospace().small()).selectable(true));
            if let Some(s) = age {
                ui.label(format!("It was last changed {s} second(s) ago."));
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
                self.opening.push(Opening { path: p.into(), task, quiet: true });
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
            done.push((o.path.clone(), o.task.value.take(), o.task.error.take(), o.quiet));
            false
        });
        for (path, repo, error, quiet) in done {
            let was_empty = self.repos.is_empty();
            let shown = self.active;
            let name = path.to_string_lossy().replace('\\', "/");
            match repo {
                Some(repo) => {
                    self.session_kept.retain(|k| *k != name);
                    self.add_repo(ctx, repo);
                    self.active = if was_empty || !quiet { self.repos.len() - 1 } else { shown };
                }
                None if quiet => self.reopen_failed.push(name),
                None => self.errors.push(format!("{} isn't a git repository:\n{}", path.display(), error.unwrap_or_default())),
            }
        }
    }

    /// The same, for one the user asked for that may be on a network share.
    fn open_in_background(&mut self, ctx: &egui::Context, path: PathBuf) {
        let Ok(git) = self.git.clone() else { return };
        let mut task = crate::task::Task::default();
        let p = path.clone();
        task.spawn(ctx, move || Repo::open(git, &p));
        self.opening.push(Opening { path, task, quiet: false });
    }

    fn add_repo(&mut self, ctx: &egui::Context, repo: Repo) {
        if let Some(i) = self.repos.iter().position(|r| r.repo.root == repo.root) {
            self.active = i;
            return;
        }
        let root = repo.root.to_string_lossy().replace('\\', "/");
        self.session_kept.retain(|k| *k != root);
        self.settings.touch_recent(&root);
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
                    // Git's own message for this blames git: "The directory name is invalid".
                    self.errors.push(if path.exists() { format!("{} isn't a git repository:\n{e}", path.display()) } else { format!("{} doesn't exist. It may have been moved, renamed or deleted.", path.display()) });
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

    /// The rest of a link only makes sense in its own repository. If that didn't open, the
    /// one on screen is some other repository and has nothing to do with it.
    fn follow_link(&mut self, ctx: &egui::Context, d: gitem_core::forge::DeepLink) {
        if !self.open_repo(ctx, std::path::Path::new(&d.repo), false) {
            return;
        }
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
        egui::Window::new("Open this repository?").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
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
            self.follow_link(ctx, link);
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
        let Some(tab) = self.repos.get_mut(self.active) else { return };
        let (mut allow, mut keep_off) = (false, false);
        let warn = self.pal.warn;
        let dim = self.pal.dim;
        egui::Window::new("This repository's config can run programs").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
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
            let tabs: Vec<String> = r.dock.iter_all_tabs().filter_map(|(_, t)| t.spec()).collect();
            let root = r.repo.root.to_string_lossy().replace('\\', "/");
            self.settings.session_tabs.insert(root.clone(), tabs);
            if let Some(j) = layout_to_json(&r.dock) {
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
        egui::Window::new("Some repositories couldn't be reopened").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
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
            items.push(format!("{} queued git command(s) won't run.", self.global_queue.len()));
        }
        if let Some(f) = self.form.as_ref().filter(|f| f.touched()) {
            items.push(format!("The \"{}\" dialog, with what's been typed in it.", f.title));
        }
        items
    }

    /// Closes a repository's tab, or asks first if that would lose something.
    fn request_close_repo(&mut self, i: usize) {
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

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let Some(g) = &self.close_guard else { return };
        let (mut go, mut stay) = (false, ctx.input(|i| i.key_pressed(egui::Key::Escape)));
        let danger = self.pal.danger;
        let dim = self.pal.dim;
        egui::Window::new("Close with work not saved?").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(RichText::new("Closing now loses:").color(dim));
            for item in &g.items {
                ui.label(format!("•  {item}"));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                go = ui.add(egui::Button::new(RichText::new("Close anyway").color(Color32::WHITE)).fill(danger)).clicked();
                stay |= ui.button("Keep it open").clicked();
            });
        });
        if go {
            let Some(g) = self.close_guard.take() else { return };
            match g.target {
                CloseTarget::Tab { root, key } => {
                    if let Some(r) = self.repos.iter_mut().find(|r| r.repo.root == root)
                        && let Some(path) = r.dock.find_tab_from(|t| t.key() == key)
                    {
                        r.dock.remove_tab(path);
                    }
                }
                CloseTarget::Repo(root) => {
                    if let Some(i) = self.repos.iter().position(|r| r.repo.root == root) {
                        self.close_repo(i);
                    }
                }
                CloseTarget::Window => {
                    self.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        } else if stay {
            self.close_guard = None;
        }
    }

    pub fn palette_is_open(&self) -> bool {
        self.palette.is_some()
    }

    /// Opens the palette with text typed, for screenshots.
    pub fn open_palette_with(&mut self, query: &str) {
        self.palette = Some(PaletteState::with_query(query));
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let want = (self.settings.theme, (self.settings.ui_scale * 100.0) as u32, self.settings.accent.clone());
        if self.applied.as_ref() != Some(&want) {
            self.applied = Some(want);
            self.pal = Palette::for_settings(self.settings.theme, &self.settings.accent);
            theme::apply(ctx, &self.pal, self.settings.ui_scale);
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
        match a {
            Action::Run(op) => {
                let batch = self.frame_no;
                if let Some(r) = self.repos.get_mut(self.active) {
                    // Two of the same in one frame are two parts of one action.
                    let doubled = r.last_asked.as_ref().is_some_and(|(last, at)| same_op(last, &op) && at.elapsed() < DOUBLE_CLICK && r.queue.back().is_none_or(|q| q.batch != batch));
                    r.last_asked = Some((op.clone(), Instant::now()));
                    if !doubled {
                        r.queue.push_back(Queued { op, open_after: None, batch });
                    }
                } else {
                    self.global_queue.push_back((op, None));
                }
            }
            Action::RunThenOpen(op, path) => {
                let batch = self.frame_no;
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.queue.push_back(Queued { op, open_after: Some(path), batch });
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
                }
            }
            Action::Inspect(sha) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.inspect.set(&sha);
                    r.show_inspect = true;
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
            Action::OpenRepo(p) => {
                if gitem_core::forge::is_network_path(&p.to_string_lossy()) {
                    self.open_in_background(ctx, p);
                } else {
                    self.open_repo(ctx, &p, false);
                }
            }
            Action::DeepLink(d) => {
                if gitem_core::forge::is_network_path(&d.repo) {
                    self.errors.push(format!("Not opening a link to a network path: {}", d.repo));
                } else if self.knows_path(&d.repo) {
                    self.follow_link(ctx, d);
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
                    self.toggle_no_repo_settings();
                } else {
                    self.apply_action(ctx, Action::OpenTab(Tab::Settings(Box::default())));
                }
            }
            Action::RunGlobal { op, open_after } => self.global_queue.push_back((op, open_after)),
            Action::Refresh => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.data.refresh(ctx, &r.repo);
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
            Action::SetCommitMessage(m) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    for (_, t) in r.dock.iter_all_tabs_mut() {
                        if let Tab::Changes(c) = t {
                            c.message = m.clone();
                        }
                    }
                }
            }
            Action::OpenPalette => self.palette = Some(PaletteState::new()),
            Action::GraphFocus(name) | Action::GraphHide(name) if self.repos.is_empty() => drop(name),
            Action::GraphFocus(name) => self.with_graph(|g| g.focus_ref(&name)),
            Action::GraphHide(name) => self.with_graph(|g| g.hide_ref(&name)),
            Action::OpenTerminal(_, Some(c)) if c == "__zen__" => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    let on = !(r.show_sidebar || r.show_inspect);
                    r.show_sidebar = on;
                    r.show_rail = on;
                    r.show_inspect = on;
                }
            }
            Action::OpenTerminal(dir, command) => open_terminal(&dir, command.as_deref(), &mut self.errors),
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
        ui.horizontal(|ui| {
            ui.add_space(9.0);
            let (rect, logo) = ui.allocate_exact_size(egui::vec2(20.0, chrome::TITLE_BAR_HEIGHT), Sense::click());
            chrome::paint_logo(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0)));
            logo.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About gitem"));
            if logo.on_hover_text("About gitem").clicked() {
                out.push(Action::About);
            }
            ui.add_space(8.0);
            let mut close = None;
            let (repos, active) = (&self.repos, &mut self.active);
            for (i, r) in repos.iter().enumerate() {
                let dirty = r.data.status.get().is_some_and(|s| !s.entries.is_empty());
                let (resp, closing) = chrome::repo_tab(ui, &r.repo.name(), dirty, i == *active, &r.repo.root.display().to_string(), &p);
                if closing || resp.middle_clicked() {
                    close = Some(i);
                } else if resp.clicked() {
                    *active = i;
                }
                resp.context_menu(|ui| {
                    if ui.button("Close repository").clicked() {
                        close = Some(i);
                        ui.close();
                    }
                });
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
                ui.menu_button(chrome::icon::PLUS, |ui| self.open_menu(ui, out)).response.on_hover_text("Open a repository");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                chrome::window_controls(ui, &p);
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.x = 2.0;
                if chrome::icon_button(ui, chrome::icon::INFO, "About", "Version and build", self.about_open, &p).clicked() {
                    out.push(Action::About);
                }
                if chrome::icon_button(ui, chrome::icon::GEAR_SIX, crate::i18n::t("Settings"), "Preferences", self.no_repo_settings.is_some(), &p).clicked() {
                    out.push(Action::OpenSettings);
                }
                if !self.repos.is_empty() && chrome::icon_button(ui, chrome::icon::LIST_MAGNIFYING_GLASS, "Command palette", "Run any command (Ctrl+P)", self.palette.is_some(), &p).clicked() {
                    self.palette = Some(PaletteState::new());
                }
            });
        });
    }

    fn toggle_no_repo_settings(&mut self) {
        if self.no_repo_settings.take().is_some() {
            return;
        }
        let git = self.git.clone().unwrap_or_else(|_| GitExe { path: "git not found".into(), source: gitem_core::runner::GitSource::System, log: Default::default(), helper: None, neutral: Default::default(), cancel: None });
        let root = PathBuf::from(".");
        self.no_repo_settings = Some(Box::new(NoRepoSettings { repo: Repo::outside_any(git, root.clone(), root), data: RepoData::default(), view: Default::default() }));
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
        egui::Window::new("About gitem").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).default_width(340.0).show(ctx, |ui| {
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
            let label = branch.clone().unwrap_or_else(|| if has_status { "detached HEAD".into() } else { "…".into() });
            let data = &tab.data;
            ui.menu_button((RichText::new(chrome::icon::GIT_BRANCH).color(p.accent), RichText::new(format!("{label}  {}", chrome::icon::CARET_DOWN)).strong()), |ui| {
                if ui.button("+ New branch…").clicked() {
                    out.push(Action::Form(forms::new_branch(None)));
                    ui.close();
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for b in data.local_branches() {
                        if ui.selectable_label(b.is_current, b.name.as_str()).clicked() {
                            if !b.is_current {
                                out.push(Action::Run(ops::checkout(&b.name)));
                            }
                            ui.close();
                        }
                    }
                });
            });
            ui.separator();
            let prune = self.settings.prune_on_fetch;
            let remotes: Vec<String> = tab.data.remotes().iter().map(|r| r.name.clone()).collect();
            if chrome::icon_text_button(ui, chrome::icon::CLOUD_ARROW_DOWN, p.accent, "Fetch", "Fetch all remotes", false, &p).clicked() {
                out.push(Action::Run(ops::fetch(&ops::FetchOpts { all: true, prune, ..Default::default() })));
            }
            let pull_label = if behind > 0 { format!("Pull {behind}") } else { "Pull".to_string() };
            if chrome::icon_text_button(ui, chrome::icon::ARROW_LINE_DOWN, p.incoming(), &pull_label, "", behind > 0, &p).clicked() {
                let mode = match self.settings.pull_mode.as_str() {
                    "ff-only" => ops::PullMode::FfOnly,
                    "rebase" => ops::PullMode::Rebase,
                    "merge" => ops::PullMode::Merge,
                    _ => ops::PullMode::Default,
                };
                out.push(Action::Run(ops::pull(mode, false)));
            }
            if ui.small_button(chrome::icon::CARET_DOWN).on_hover_text("Pull options").clicked() {
                out.push(Action::Form(forms::pull_form()));
            }
            let push_label = if upstream.is_none() && branch.is_some() { "Publish".to_string() } else if ahead > 0 { format!("Push {ahead}") } else { "Push".to_string() };
            if chrome::icon_text_button(ui, chrome::icon::ARROW_LINE_UP, p.outgoing(), &push_label, "", ahead > 0, &p).clicked() {
                if upstream.is_some() {
                    out.push(Action::Run(ops::push(&ops::PushOpts::default())));
                } else {
                    out.push(Action::Form(forms::push_form(branch.clone(), remotes.clone(), false, self.settings.confirm_destructive)));
                }
            }
            if ui.small_button(chrome::icon::CARET_DOWN).on_hover_text("Push options").clicked() {
                out.push(Action::Form(forms::push_form(branch.clone(), remotes.clone(), upstream.is_some(), self.settings.confirm_destructive)));
            }
            ui.separator();
            if chrome::tool_button(ui, chrome::icon::ARCHIVE, p.series[6], crate::i18n::t("Stash"), "Stash changes", false, &p).clicked() {
                out.push(Action::Form(forms::stash_form(Vec::new())));
            }
            if chrome::tool_button(ui, chrome::icon::ARROWS_CLOCKWISE, p.dim, crate::i18n::t("Refresh"), "Reload this repository (F5)", false, &p).clicked() {
                out.push(Action::Refresh);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                chrome::tool_button(ui, chrome::icon::ARTICLE, p.dim, crate::i18n::t("Details"), "Show the selected commit's details", tab.show_inspect, &p).clicked().then(|| tab.show_inspect = !tab.show_inspect);
                chrome::tool_button(ui, chrome::icon::SIDEBAR_SIMPLE, p.dim, crate::i18n::t("Sidebar"), "Show branches, tags, and files (Ctrl+B)", tab.show_sidebar, &p).clicked().then(|| tab.show_sidebar = !tab.show_sidebar);
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
        if !self.settings.recent_repos.is_empty() {
            ui.separator();
            ui.label(RichText::new("Recent").small().color(self.pal.faint));
            for r in self.settings.recent_repos.clone().iter().take(12) {
                if ui.button(r.as_str()).clicked() {
                    out.push(Action::OpenRepo(r.into()));
                    ui.close();
                }
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
            ui.label(RichText::new("See your repository's history and everything around it").color(p.dim));
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
                        self.settings_dirty = true;
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 300.0).max(0.0) / 2.0);
                if ui.button("Open folder…").clicked() {
                    crate::widgets::pick(&mut self.folder_pick, ui.ctx(), rfd::FileDialog::new(), true);
                }
                if ui.button("Clone…").clicked() {
                    out.push(Action::Form(forms::clone_form()));
                }
                if ui.button("Initialize…").clicked() {
                    out.push(Action::Form(forms::init_form()));
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 500.0).max(0.0) / 2.0);
                let r = ui.add(crate::widgets::input(&mut self.open_path).hint_text("Or type a path and press Enter").desired_width(500.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.open_path.trim().is_empty() {
                    out.push(Action::OpenRepo(self.open_path.trim().into()));
                }
            });
            if !self.settings.recent_repos.is_empty() {
                ui.add_space(18.0);
                ui.label(RichText::new("Recent").color(p.faint));
                for r in self.settings.recent_repos.clone().iter().take(12) {
                    if ui.link(r.as_str()).clicked() {
                        out.push(Action::OpenRepo(r.into()));
                    }
                }
            }
        });
    }

    fn op_banner(ui: &mut Ui, tab_status: &RepoStatus, ops_busy: bool, out: &mut Vec<Action>, p: &Palette) {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                // A second Skip would act on a state nobody has looked at yet.
                if ops_busy {
                    ui.disable();
                }
                let mut text = tab_status.op.label().to_string();
                if let Some(t) = &tab_status.op_target {
                    text.push_str(&format!(" {t}"));
                }
                if let Some((a, b)) = tab_status.op_progress {
                    text.push_str(&format!(" (step {a} of {b})"));
                }
                ui.label(RichText::new(text).strong());
                let conflicts: Vec<String> = tab_status.conflicts().map(|e| e.path.clone()).collect();
                if !conflicts.is_empty() {
                    // Not a chip: its warning-coloured text fails contrast on this fill in light themes.
                    ui.label(RichText::new(format!("{} conflict(s)", conflicts.len())).strong().color(p.text));
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
        let Some(tab) = self.repos.get_mut(self.active) else {
            if let Some(g) = &self.global_running {
                ui.horizontal(|ui| {
                    ui.add(spinner(13.0));
                    ui.label(format!("{}: {}", g.title, g.last_line));
                    // Nothing's on screen yet to lose: a clone that's gone wrong just stops.
                    if ui.small_button("Cancel").clicked() {
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
                    if ui.small_button("Stop it").clicked() {
                        r.cancel.store(true, Ordering::Relaxed);
                        r.asking_to_stop = false;
                    }
                    if ui.small_button("Keep waiting").clicked() {
                        r.asking_to_stop = false;
                    }
                } else {
                    ui.add(egui::Label::new(RichText::new(format!("{}: {}", r.title, r.last_line)).small()).truncate());
                    if r.network {
                        if ui.small_button("Cancel").clicked() {
                            r.cancel.store(true, Ordering::Relaxed);
                        }
                    } else if r.started.elapsed() >= CANCEL_OFFERED_AFTER {
                        // A hook that never returns would otherwise hold up everything behind it.
                        if ui.small_button("Cancel…").clicked() {
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
                if ui.small_button("Cancel").clicked() {
                    g.cancel.store(true, Ordering::Relaxed);
                }
            } else if !tab.data.last_blame.is_empty() {
                ui.add(egui::Label::new(RichText::new(&tab.data.last_blame).small().color(p.dim)).truncate());
            } else {
                let last = tab.repo.git.log.lock().ok().and_then(|l| l.iter().rev().find(|e| e.is_write).map(|e| (e.command.clone(), e.code)));
                if let Some((c, code)) = last {
                    ui.label(RichText::new(if code == 0 { "✔" } else { "✖" }).small().color(if code == 0 { p.good } else { p.danger }));
                    ui.add(egui::Label::new(RichText::new(c).small().color(p.faint).monospace()).truncate());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tab.data.status.get().is_some() {
                    let n = tab.data.counts();
                    let (st, un, c) = (n.staged, n.unstaged, n.conflicts);
                    if st + un + c > 0 && ui.small_button(format!("{st} staged, {un} changed{}", if c > 0 { format!(", {c} conflicts") } else { String::new() })).clicked() {
                        out.push(Action::OpenTab(Tab::changes()));
                    }
                }
                if tab.watcher.failed.load(Ordering::Relaxed) {
                    ui.label(RichText::new("Not notified of changes here, so looking every few seconds").small().color(p.faint));
                }
                if let Some(f) = tab.data.forge() {
                    ui.label(RichText::new(format!("{} {}", f.kind.label(), f.path)).small().color(p.faint));
                }
                if let Some((n, url)) = &tab.data.branch_pr {
                    crate::widgets::web_link(ui, RichText::new(format!("PR #{n}")).small(), url);
                }
            });
        });
    }

    fn dialog_open(&self) -> bool {
        self.confirm.is_some() || self.form.is_some() || self.palette.is_some() || self.about_open || !self.errors.is_empty() || !self.crash_reports.is_empty() || self.git_warning.is_some() || !self.reopen_failed.is_empty() || self.close_guard.is_some() || self.lock_prompt.is_some() || self.link_prompt.is_some() || self.trust_question_open()
    }

    /// Old git still does most things, so this warns and lets the user carry on.
    fn git_warning_dialog(&mut self, ctx: &egui::Context) {
        let Some(warning) = self.git_warning.clone() else { return };
        let mut dismiss = false;
        egui::Window::new("Your git is older than gitem needs").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
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
        egui::Window::new("gitem crashed last time").collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label("A report was saved on this computer. Nothing was sent anywhere. If you share it, read it first: it names folders and the last git commands.");
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(newest.display().to_string()).monospace().small()).selectable(true));
            if more > 0 {
                ui.label(RichText::new(format!("And {more} older report(s) in the same folder.")).small().color(self.pal.dim));
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
            egui::Window::new(c.title.as_str()).collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(c.body.as_str());
                ui.add_space(4.0);
                ui.label(RichText::new(c.op.preview()).monospace().small().color(p.faint));
                if let Some(root) = c.repo.as_ref().filter(|_| several) {
                    ui.label(RichText::new(format!("In {}", root.display())).small().color(p.warn));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    go = ui.add(egui::Button::new(RichText::new(c.label.as_str()).color(Color32::WHITE)).fill(p.danger)).clicked();
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
            egui::Window::new(f.title.clone()).collapsible(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 70.0]).show(ctx, |ui| {
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
            egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_BOTTOM, [0.0, -40.0]).show(ctx, |ui| {
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
                });
            });
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
                    ErrorFix::Stash => Action::Form(forms::stash_form(Vec::new())),
                    ErrorFix::AddRemote => Action::Form(forms::add_remote()),
                    ErrorFix::Credentials => Action::OpenTab(Tab::Credentials(Box::default())),
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
            egui::Area::new("toasts".into()).anchor(Align2::RIGHT_BOTTOM, [-14.0, -34.0]).interactable(false).show(ctx, |ui| {
                for (t, _) in &self.toasts {
                    egui::Frame::new().fill(p.panel2).stroke(egui::Stroke::new(1.0, p.border)).inner_margin(egui::Margin::symmetric(10, 5)).corner_radius(4.0).show(ui, |ui| {
                        ui.label(t);
                    });
                }
            });
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::Z))
            && let Some(r) = self.repos.get_mut(self.active) {
                let on = !(r.show_sidebar || r.show_inspect);
                r.show_sidebar = on;
                r.show_rail = on;
                r.show_inspect = on;
            }
        let (palette, refresh, sidebar, changes, graph) = ctx.input(|i| {
            (i.modifiers.command && i.key_pressed(egui::Key::P), i.key_pressed(egui::Key::F5), i.modifiers.command && i.key_pressed(egui::Key::B), i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::G), i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::H))
        });
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

    fn poll_global(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if let Some(g) = &mut self.global_running {
            let mut done = None;
            loop {
                match g.rx.try_recv() {
                    Ok(OpMsg::Line(l)) => g.last_line = l,
                    Ok(OpMsg::Done(r)) => done = Some(r),
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
                self.global_running = None;
                match res {
                    Ok(_) => {
                        out.push(Action::Toast(format!("{title}: done")));
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

    /// The whole UI for one frame. Separated from `eframe::App` for testing.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.handle_shot(ui);
        self.check_window_close(&ctx);
        let mut out: Vec<Action> = Vec::new();

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
        let regained = focused && !std::mem::replace(&mut self.window_focused, focused);
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
            let job_running = r.data.jobs.load(Ordering::Relaxed) > 0;
            r.data.ops_busy = r.running.is_some() || !r.queue.is_empty() || job_running;
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
        if self.folder_pick.poll()
            && let Some(Some(d)) = self.folder_pick.value.take()
        {
            out.push(Action::OpenRepo(d));
        }
        self.shortcuts(&ctx, &mut out);

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
            egui::Panel::left("rail").frame(egui::Frame::new().fill(pal.bg)).resizable(false).exact_size(chrome::RAIL_WIDTH).show(ui, |ui| {
                if let Some(t) = chrome::rail(ui, &|i| focused == Some(i.name), &pal) {
                    out.push(Action::OpenTab(t));
                }
            });
        }
        if !self.repos.is_empty() {
            egui::Panel::top("top-bar").frame(bar_frame).show_separator_line(false).show(ui, |ui| self.toolbar(ui, &mut out));
        }

        if let Some(tab) = self.repos.get_mut(self.active) {
            let RepoTab { repo, data, dock, inspect, show_inspect, sidebar, show_sidebar, .. } = tab;
            if let Some(s) = data.status.get()
                && s.op != RepoOp::None {
                    let frame = egui::Frame::new().fill(pal.banner_fill());
                    let s = s.clone();
                    let ops_busy = data.ops_busy;
                    egui::Panel::top("op-banner").frame(frame).show(ui, |ui| Self::op_banner(ui, &s, ops_busy, &mut out, &pal));
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
                        if ui.small_button(label).clicked() {
                            out.push(Action::Retry);
                        }
                        if ui.small_button("Copy").clicked() {
                            out.push(Action::Copy(problem.clone()));
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
                        if ui.small_button("Review…").clicked() {
                            out.push(Action::ReviewTrust);
                        }
                    });
                });
            }
            if *show_sidebar {
                egui::Panel::left("sidebar").resizable(true).default_size(270.0).size_range(180.0..=600.0).show(ui, |ui| {
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open };
                    sidebar.ui(ui, &mut cx);
                });
            }
            if *show_inspect {
                egui::Panel::right("inspect").resizable(true).default_size(340.0).size_range(220.0..=800.0).show(ui, |ui| {
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open };
                    inspect.ui(ui, &mut cx);
                });
            }
            let style = theme::dock_style(ui.style(), &pal);
            let mut viewer = Viewer { cx: Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open } };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                DockArea::new(dock).id(egui::Id::new(DOCK_ID)).style(style).show_add_buttons(true).show_leaf_collapse_buttons(false).show_inside(ui, &mut viewer);
                paint_active_tab_markers(ui, dock, &pal);
                // egui_dock shows a grab cursor on any hovered tab. Keep the usual pointer
                // until a tab is actually being dragged, which has its own cursor.
                if ui.ctx().output(|o| o.cursor_icon) == egui::CursorIcon::Grab {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                }
            });

            if let Some(pl) = &mut self.palette {
                let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out, dialog_open };
                if pl.show(&ctx, &mut cx) {
                    self.palette = None;
                }
            }
        } else {
            self.palette = None;
            if let Some(s) = &mut self.no_repo_settings {
                let settings = &self.settings;
                egui::CentralPanel::default().show(ui, |ui| {
                    let NoRepoSettings { repo, data, view } = &mut **s;
                    let mut cx = Cx { repo, data, settings, pal: &pal, out: &mut out, dialog_open };
                    view.ui(ui, &mut cx);
                });
            } else {
                egui::CentralPanel::default().show(ui, |ui| self.welcome(ui, &mut out));
            }
        }

        self.dialogs(&ctx, &mut out);
        self.crash_dialog(&ctx);
        self.git_warning_dialog(&ctx);
        self.reopen_dialog(&ctx);
        self.close_dialog(&ctx);
        self.lock_dialog(&ctx);
        self.trust_dialog(&ctx);
        self.link_dialog(&ctx);
        self.about(&ctx, &mut out);
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
            crate::widgets::set_dialog_parent(frame);
        }
        #[cfg(not(windows))]
        let _ = frame;
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        self.save_settings();
    }
}

/// A new terminal window in `dir`, optionally running a command there. The folder
/// is passed as the working directory, so its name is never parsed by a shell.
fn open_terminal(dir: &Path, command: Option<&str>, errors: &mut Vec<String>) {
    if let Err(e) = terminal_command(dir, command).spawn() {
        errors.push(format!("Could not open a terminal: {e}"));
    }
}

fn terminal_command(dir: &Path, command: Option<&str>) -> std::process::Command {
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd.exe");
        if let Some(cmd) = command {
            c.arg("/K").arg(cmd);
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
        let mut c = std::process::Command::new("open");
        c.args(["-a", "Terminal"]).arg(dir);
        let _ = command;
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = {
        let mut c = std::process::Command::new("x-terminal-emulator");
        if let Some(cmd) = command {
            c.args(["-e", cmd]);
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

pub use mergeview::MergeSource;

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
}
