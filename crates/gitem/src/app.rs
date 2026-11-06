//! Top-level application: repository tabs, docking, op queue, dialogs, toasts.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use egui::{Align2, RichText, Ui};
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
}

fn spawn_op(ctx: &egui::Context, git: GitExe, cwd: Option<PathBuf>, op: GitOp, open_after: Option<PathBuf>) -> Running {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let ctx2 = ctx.clone();
    let (title, network) = (op.title.clone(), op.network);
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let ctx3 = ctx2.clone();
        let mut on_line = move |l: &str| {
            let _ = tx2.send(OpMsg::Line(l.to_string()));
            ctx3.request_repaint();
        };
        let res = op.run(&git, cwd.as_deref(), Some(c2), &mut on_line).map(|o| o.last().map(|x| x.text()).unwrap_or_default()).map_err(|e| e.to_string());
        let _ = tx.send(OpMsg::Done(res));
        ctx2.request_repaint();
    });
    Running { title, rx, cancel, last_line: String::new(), network, open_after }
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
    queue: VecDeque<(GitOp, Option<PathBuf>)>,
    running: Option<Running>,
    last_auto_fetch: Instant,
    background: Vec<crate::task::Task<String>>,
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
        let watcher = RepoWatcher::new(ctx, &repo.root, &repo.git_dir);
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
            last_auto_fetch: Instant::now(),
            background: Vec::new(),
        }
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
                self.running = None;
                self.data.refresh(ctx, &self.repo);
                self.data.mark_changed();
                match res {
                    Ok(_) => {
                        if let Some(p) = open_after {
                            out.push(Action::OpenRepo(p));
                        }
                        if self.queue.is_empty() {
                            out.push(Action::Toast(format!("{title}: done")));
                        }
                    }
                    Err(e) => {
                        // Later steps assumed this one worked, so they're dropped.
                        self.queue.clear();
                        out.push(Action::Error(format!("{title} failed:\n{e}")));
                    }
                }
            }
        }
        if self.running.is_none()
            && let Some((op, open)) = self.queue.pop_front() {
                self.running = Some(spawn_op(ctx, self.repo.git.clone(), Some(self.repo.root.clone()), op, open));
            }
    }
}

const DOCK_ID: &str = "gitem-dock";

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
    applied: Option<(gitem_core::settings::ThemeChoice, u32)>,
    open_path: String,
    settings_dirty: bool,
    pub shot: Option<Shot>,
    pub shot_tabs: Vec<Tab>,
    pub shot_inspect: Option<String>,
    persist: bool,
    about_open: bool,
    #[cfg(windows)]
    styled_window: bool,
    /// Settings shown with no repository open.
    no_repo_settings: Option<Box<NoRepoSettings>>,
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
        let settings = Settings::load();
        let mut app = Self::with_settings(ctx, settings);
        let restore: Vec<PathBuf> = if paths.is_empty() { app.settings.open_repos.iter().map(PathBuf::from).collect() } else { paths.to_vec() };
        for p in restore {
            app.open_repo(ctx, &p, paths.is_empty());
        }
        app.active = 0;
        app.persist = true;
        app
    }

    /// An app that neither restores nor saves session state.
    pub fn with_settings(ctx: &egui::Context, settings: Settings) -> Self {
        theme::install_fonts(ctx);
        let git = GitExe::discover((!settings.git_path.is_empty()).then(|| Path::new(&settings.git_path))).map_err(|e| e.to_string());
        let pal = Palette::get(settings.theme);
        Self {
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
            #[cfg(windows)]
            styled_window: false,
            no_repo_settings: None,
        }
    }

    pub fn open_repo(&mut self, ctx: &egui::Context, path: &Path, quiet: bool) {
        let Ok(git) = self.git.clone() else { return };
        match Repo::open(git, path) {
            Ok(repo) => {
                if let Some(i) = self.repos.iter().position(|r| r.repo.root == repo.root) {
                    self.active = i;
                    return;
                }
                let root = repo.root.to_string_lossy().replace('\\', "/");
                self.settings.touch_recent(&root);
                let mut tab = RepoTab::new(ctx, repo, &self.settings);
                if self.settings.auto_maintenance {
                    tab.queue.push_back((ops::maintenance_run(), None));
                }
                self.repos.push(tab);
                self.active = self.repos.len() - 1;
                self.settings_dirty = true;
            }
            Err(e) if !quiet => self.errors.push(format!("{} isn't a git repository:\n{e}", path.display())),
            Err(_) => {}
        }
    }

    fn save_settings(&mut self) {
        if !self.persist {
            return;
        }
        self.settings.open_repos = self.repos.iter().map(|r| r.repo.root.to_string_lossy().replace('\\', "/")).collect();
        for r in &self.repos {
            let tabs: Vec<String> = r.dock.iter_all_tabs().filter_map(|(_, t)| t.spec()).collect();
            let root = r.repo.root.to_string_lossy().replace('\\', "/");
            self.settings.session_tabs.insert(root.clone(), tabs);
            if let Some(j) = layout_to_json(&r.dock) {
                self.settings.session_layouts.insert(root, j);
            }
        }
        if let Err(e) = self.settings.save() {
            self.errors.push(format!("Could not save settings: {e}"));
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
        let want = (self.settings.theme, (self.settings.ui_scale * 100.0) as u32);
        if self.applied != Some(want) {
            self.applied = Some(want);
            self.pal = Palette::get(self.settings.theme);
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
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.queue.push_back((op, None));
                } else {
                    self.global_queue.push_back((op, None));
                }
            }
            Action::RunThenOpen(op, path) => {
                if let Some(r) = self.repos.get_mut(self.active) {
                    r.queue.push_back((op, Some(path)));
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
            Action::OpenFile(path) => open_file(&path, &self.settings.default_external_editor, &mut self.errors),
            Action::Confirm { title, body, label, op } => {
                let repo = self.repos.get(self.active).map(|r| r.repo.root.clone());
                self.confirm = Some(Confirm { title, body, label, op, repo });
            }
            Action::Form(f) => {
                self.form_repo = self.repos.get(self.active).map(|r| r.repo.root.clone());
                self.form = Some(f);
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
            Action::OpenRepo(p) => self.open_repo(ctx, &p, false),
            Action::DeepLink(d) => {
                self.open_repo(ctx, std::path::Path::new(&d.repo), false);
                if let Some(c) = &d.commit {
                    self.apply_action(ctx, Action::Inspect(c.clone()));
                }
                if let Some(f) = &d.file {
                    self.apply_action(ctx, Action::OpenTab(Tab::file(f, d.commit.clone(), d.line)));
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
                self.repos.remove(i);
                self.active = self.active.min(self.repos.len().saturating_sub(1));
                self.settings_dirty = true;
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
        let git = self.git.clone().unwrap_or_else(|_| GitExe { path: "git not found".into(), source: gitem_core::runner::GitSource::System, log: Default::default(), helper: None });
        let root = PathBuf::from(".");
        self.no_repo_settings = Some(Box::new(NoRepoSettings { repo: Repo { git, git_dir: root.clone(), common_dir: root.clone(), root }, data: RepoData::default(), view: Default::default() }));
    }

    fn about(&mut self, ctx: &egui::Context, out: &mut Vec<Action>) {
        if !self.about_open {
            return;
        }
        let p = self.pal.clone();
        let mut open = true;
        let mut dismiss = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let git = self.git.as_ref().map(|g| g.version()).unwrap_or_else(|e| e.clone());
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
            let names: Vec<Branch> = tab.data.local_branches().cloned().collect();
            ui.menu_button((RichText::new(chrome::icon::GIT_BRANCH).color(p.accent), RichText::new(format!("{label}  {}", chrome::icon::CARET_DOWN)).strong()), |ui| {
                if ui.button("+ New branch…").clicked() {
                    out.push(Action::Form(forms::new_branch(None)));
                    ui.close();
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for b in &names {
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
                    out.push(Action::Form(forms::push_form(branch.clone(), remotes.clone(), false)));
                }
            }
            if ui.small_button(chrome::icon::CARET_DOWN).on_hover_text("Push options").clicked() {
                out.push(Action::Form(forms::push_form(branch.clone(), remotes.clone(), upstream.is_some())));
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
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                out.push(Action::OpenRepo(d));
            }
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
                        self.settings_dirty = true;
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 300.0).max(0.0) / 2.0);
                if ui.button("Open folder…").clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder() {
                        out.push(Action::OpenRepo(d));
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

    fn op_banner(ui: &mut Ui, tab_status: &RepoStatus, out: &mut Vec<Action>, p: &Palette) {
        egui::Frame::new().fill(p.warn.gamma_multiply(0.15)).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
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
                    chip(ui, &format!("{} conflict(s)", conflicts.len()), p.warn);
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
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(format!("{}: {}", g.title, g.last_line));
                });
            }
            return;
        };
        ui.horizontal(|ui| {
            if let Some(r) = &tab.running {
                ui.add(egui::Spinner::new().size(13.0));
                ui.add(egui::Label::new(RichText::new(format!("{}: {}", r.title, r.last_line)).small()).truncate());
                if r.network && ui.small_button("Cancel").clicked() {
                    r.cancel.store(true, Ordering::Relaxed);
                }
                if !tab.queue.is_empty() {
                    ui.label(RichText::new(format!("+{} queued", tab.queue.len())).small().color(p.dim));
                }
            } else if let Some(g) = &self.global_running {
                ui.add(egui::Spinner::new().size(13.0));
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
                if let Some(s) = tab.data.status.get() {
                    let (st, un, c) = (s.entries.iter().filter(|e| e.is_staged()).count(), s.entries.iter().filter(|e| e.is_unstaged()).count(), s.conflicts().count());
                    if st + un + c > 0 && ui.small_button(format!("{st} staged, {un} changed{}", if c > 0 { format!(", {c} conflicts") } else { String::new() })).clicked() {
                        out.push(Action::OpenTab(Tab::changes()));
                    }
                }
                if let Some(f) = tab.data.forge() {
                    ui.label(RichText::new(format!("{} {}", f.kind.label(), f.path)).small().color(p.faint));
                }
                if let Some((n, url)) = &tab.data.branch_pr {
                    ui.hyperlink_to(RichText::new(format!("PR #{n}")).small(), url);
                }
            });
        });
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
                    go = ui.add(egui::Button::new(RichText::new(c.label.as_str()).color(p.on_accent)).fill(p.danger)).clicked();
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
            let mut close = false;
            egui::Window::new("Something went wrong").collapsible(false).resizable(true).default_width(560.0).anchor(Align2::CENTER_BOTTOM, [0.0, -40.0]).show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&e).monospace().small()).selectable(true));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    close = ui.button("Dismiss").clicked();
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(e.clone());
                    }
                });
            });
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
            while let Ok(m) = g.rx.try_recv() {
                match m {
                    OpMsg::Line(l) => g.last_line = l,
                    OpMsg::Done(r) => done = Some(r),
                }
            }
            if let Some(res) = done {
                let (title, open) = (g.title.clone(), g.open_after.clone());
                self.global_running = None;
                match res {
                    Ok(_) => {
                        out.push(Action::Toast(format!("{title}: done")));
                        if let Some(p) = open {
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
        let mut out: Vec<Action> = Vec::new();

        if let Some(sha) = self.shot_inspect.take() {
            out.push(Action::Inspect(sha));
        }
        for tab in std::mem::take(&mut self.shot_tabs) {
            if let Some(r) = self.repos.get_mut(self.active) {
                r.open_tab(tab);
            }
        }

        // Background upkeep for every open repository.
        for (i, r) in self.repos.iter_mut().enumerate() {
            r.data.poll(&ctx, &r.repo);
            let mut local = Vec::new();
            r.poll_ops(&ctx, &mut local);
            out.extend(local);
            r.data.ops_busy = r.running.is_some() || !r.queue.is_empty();
            let (w, g) = r.watcher.take();
            if w || g {
                r.data.mark_changed();
            }
            if g {
                r.data.refresh(&ctx, &r.repo);
            } else if w {
                r.data.refresh_status(&ctx, &r.repo);
            }
            let mins = self.settings.auto_fetch_minutes;
            if mins > 0 && r.last_auto_fetch.elapsed() > Duration::from_secs(mins as u64 * 60) {
                r.last_auto_fetch = Instant::now();
                if !r.data.remotes().is_empty() {
                    r.queue.push_back((ops::fetch(&ops::FetchOpts { all: true, prune: self.settings.prune_on_fetch, ..Default::default() }), None));
                }
            }
            let _ = i;
            r.background.retain_mut(|t| {
                if !t.poll() {
                    return true;
                }
                if let Some(m) = t.value.take() {
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
        self.shortcuts(&ctx, &mut out);

        let bar_frame = egui::Frame::new().fill(self.pal.panel).inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("title-bar").frame(egui::Frame::new().fill(self.pal.bg)).resizable(false).exact_size(chrome::TITLE_BAR_HEIGHT).show_separator_line(false).show(ui, |ui| self.title_bar(ui, &mut out));
        egui::Panel::bottom("status-bar").frame(egui::Frame::new().fill(self.pal.status_bg).inner_margin(egui::Margin::symmetric(8, 3))).show_separator_line(false).show(ui, |ui| self.status_bar(ui, &mut out));
        let pal = self.pal.clone();
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
                    let frame = egui::Frame::new();
                    let s = s.clone();
                    egui::Panel::top("op-banner").frame(frame).show(ui, |ui| Self::op_banner(ui, &s, &mut out, &pal));
                }
            if *show_sidebar {
                egui::Panel::left("sidebar").resizable(true).default_size(270.0).size_range(180.0..=600.0).show(ui, |ui| {
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out };
                    sidebar.ui(ui, &mut cx);
                });
            }
            if *show_inspect {
                egui::Panel::right("inspect").resizable(true).default_size(340.0).size_range(220.0..=800.0).show(ui, |ui| {
                    let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out };
                    inspect.ui(ui, &mut cx);
                });
            }
            let style = theme::dock_style(ui.style(), &pal);
            let mut viewer = Viewer { cx: Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out } };
            egui::CentralPanel::no_frame().show(ui, |ui| {
                DockArea::new(dock).id(egui::Id::new(DOCK_ID)).style(style).show_add_buttons(true).show_leaf_collapse_buttons(false).show_inside(ui, &mut viewer);
                paint_active_tab_markers(ui, dock, &pal);
            });

            if let Some(pl) = &mut self.palette {
                let mut cx = Cx { repo, data, settings: &self.settings, pal: &pal, out: &mut out };
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
                    let mut cx = Cx { repo, data, settings, pal: &pal, out: &mut out };
                    view.ui(ui, &mut cx);
                });
            } else {
                egui::CentralPanel::default().show(ui, |ui| self.welcome(ui, &mut out));
            }
        }

        self.dialogs(&ctx, &mut out);
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
    #[cfg(windows)]
    let mut c = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd.exe");
        if let Some(cmd) = command {
            c.arg("/K").arg(cmd);
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
    if let Err(e) = c.spawn() {
        errors.push(format!("Could not open a terminal: {e}"));
    }
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
