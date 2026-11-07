//! Home: status of this repository, start-work and review shortcuts, recent
//! branches, and multi-repository workspaces with batch actions.
use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::settings::Workspace;
use gitem_core::{Repo, ops};

use crate::action::{Action, Cx};
use crate::forms::{self, FormDialog};
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

#[derive(Clone, Default)]
struct RepoSummary {
    path: String,
    name: String,
    branch: String,
    changes: usize,
    ahead: u32,
    behind: u32,
    error: Option<String>,
}

#[derive(Default)]
pub struct HomeView {
    summaries: Task<Vec<RepoSummary>>,
    summaries_for: Option<String>,
    workspace_idx: usize,
    scan_pick: Task<Option<std::path::PathBuf>>,
    recent: Task<Vec<Commit>>,
    recent_gen: Option<u64>,
    /// Fetch all or Pull all, going through a workspace's repositories.
    sweep: Task<String>,
    /// How many of those have finished: the list of them is out of date after each.
    swept: u64,
}

impl HomeView {
    pub fn key(&self) -> String {
        "home".into()
    }
    pub fn title(&self) -> String {
        "Home".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.summaries.poll();
        egui::ScrollArea::vertical().id_salt("home").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            ui.add_space(6.0);
            ui.heading(cx.repo.name());
            ui.label(RichText::new(cx.repo.root.display().to_string()).small().color(p.faint));
            ui.add_space(8.0);
            ui.columns(2, |c| {
                self.left_column(&mut c[0], cx);
                self.right_column(&mut c[1], cx);
            });
            ui.add_space(10.0);
            self.workspaces(ui, cx);
        });
    }

    fn left_column(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "Current work", &p);
        match cx.status().cloned() {
            None => spinner_row(ui, "Reading status…", &p),
            Some(st) => {
                let branch = st.branch.clone().unwrap_or_else(|| format!("detached at {}", gitem_core::util::prefix(&st.head_sha, 8)));
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(branch).strong().size(15.0));
                    if st.ahead > 0 {
                        chip(ui, &format!("\u{E08E} {} to push", st.ahead), p.outgoing());
                    }
                    if st.behind > 0 {
                        chip(ui, &format!("\u{E03E} {} to pull", st.behind), p.incoming());
                    }
                    if st.upstream.is_none() && st.branch.is_some() {
                        chip(ui, "not published", p.dim);
                    }
                });
                let (staged, unstaged) = (st.entries.iter().filter(|e| e.is_staged()).count(), st.entries.iter().filter(|e| e.is_unstaged()).count());
                let conflicts = st.conflicts().count();
                if st.is_clean() {
                    ui.label(RichText::new("Working tree clean").color(p.dim));
                } else {
                    ui.label(RichText::new(format!("{staged} staged, {unstaged} unstaged, {conflicts} conflicted")).color(p.dim));
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if !st.is_clean() && ui.button("Changes").clicked() {
                        cx.open(Tab::changes());
                    }
                    let stashes = cx.data.stashes().len();
                    if stashes > 0 {
                        chip(ui, &plural(stashes, "stash", "stashes"), p.dim);
                    }
                    let others = cx.data.worktrees().iter().filter(|w| !w.is_current).count();
                    if others > 0 {
                        chip(ui, &plural(others, "other worktree", "other worktrees"), p.dim);
                    }
                });
            }
        }
        section(ui, "Start", &p);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Start work…").on_hover_text("Create a branch, optionally in a new worktree").clicked() {
                let base = cx.data.refs.get().and_then(|r| r.default_branch.clone()).unwrap_or_else(|| "HEAD".to_string());
                let repo_name = cx.repo.name();
                let parent = cx.repo.root.parent().map(|x| x.to_path_buf()).unwrap_or_default();
                cx.form(
                    FormDialog::new("Start work", "Start")
                        .text("name", "Branch name", "")
                        .hint("feature/my-work")
                        .text("base", "Based on", &base)
                        .check("worktree", "Use a new worktree", false)
                        .on_submit(move |v| {
                            let n = v.opt("name").ok_or("A branch name is required.")?;
                            let b = v.opt("base");
                            Ok(if v.on("worktree") {
                                let path = parent.join(format!("{repo_name}.worktrees/{}", n.replace('/', "-"))).to_string_lossy().replace('\\', "/");
                                vec![Action::RunThenOpen(ops::worktree_start_work(&path, &n, b.as_deref()), path.into())]
                            } else {
                                vec![Action::Run(ops::branch_start_work(&n, b.as_deref()))]
                            })
                        }),
                );
            }
            // What leads nowhere isn't offered: no host means no pull requests.
            if cx.forge().is_some() && ui.button("Pull requests").clicked() {
                cx.open(Tab::Launchpad(Box::default()));
            }
            let has_commits = cx.status().is_some_and(|s| !s.head_sha.is_empty());
            if has_commits && ui.button("Compare refs").clicked() {
                cx.open(Tab::Search(Box::new(crate::views::search::SearchView::compare("", ""))));
            }
            if cx.status().is_some_and(|s| !s.is_clean()) && ui.button("Stash…").clicked() {
                cx.form(forms::stash_form(Vec::new()));
            }
        });
        ui.add_space(2.0);
        ui.label(RichText::new("Ctrl+P runs any command by name. F1 lists the keys.").small().color(p.dim));
        self.recent_commits(ui, cx);
    }

    /// What's happened lately on this branch.
    fn recent_commits(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "Recent commits", &p);
        self.recent.poll();
        if self.recent_gen != Some(cx.data.refs_gen) && !self.recent.busy() && cx.status().is_some_and(|s| !s.head_sha.is_empty()) {
            self.recent_gen = Some(cx.data.refs_gen);
            let r = cx.repo.clone();
            self.recent.spawn(ui.ctx(), move || r.log(&LogQuery { revs: vec!["HEAD".to_string()], limit: 6, ..Default::default() }));
        }
        if let Some(e) = &self.recent.error {
            error_box(ui, e, &p);
            return;
        }
        let Some(list) = self.recent.get() else {
            if cx.status().is_some_and(|s| s.head_sha.is_empty()) {
                ui.label(RichText::new("No commits yet.").color(p.dim));
            } else {
                spinner_row(ui, "Reading history…", &p);
            }
            return;
        };
        ui.spacing_mut().item_spacing.y = 0.0;
        for c in list {
            let (rect, resp) = row(ui, 22.0, false, &p);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, c.subject.as_str()));
            paint_text(ui, egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 0.0), egui::pos2(rect.right() - 90.0, rect.bottom())), &c.subject, prop(12.5), p.text);
            paint_text(ui, egui::Rect::from_min_max(egui::pos2(rect.right() - 86.0, rect.top()), rect.max), &time_label(c.author_time, cx.settings.date_relative), prop(11.0), p.dim);
            if resp.on_hover_text("Click to see its details").clicked() {
                cx.open(Tab::graph());
                cx.inspect(&c.sha);
            }
        }
        ui.spacing_mut().item_spacing.y = 4.0;
    }

    fn right_column(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "Recent branches", &p);
        let branches: Vec<Branch> = cx.data.local_branches().take(8).cloned().collect();
        if !cx.data.refs_known() {
            // Not "no branches": nobody knows yet, or git couldn't say.
            match &cx.data.refs.error {
                Some(e) => error_box(ui, e, &p),
                None => spinner_row(ui, "Reading branches…", &p),
            }
        } else if branches.is_empty() {
            ui.label(RichText::new("No branches yet").color(p.dim));
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        for b in branches {
            let (rect, resp) = row(ui, 34.0, b.is_current, &p);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, b.name.as_str()));
            let name = if b.is_current { format!("● {}", b.name) } else { b.name.clone() };
            paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 2.0), egui::vec2(rect.width() - 110.0, 17.0)), &name, prop(13.0), p.text);
            paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 18.0), egui::vec2(rect.width() - 16.0, 14.0)), &format!("{} · {}", time_label(b.date, cx.settings.date_relative), b.subject), prop(11.0), p.dim);
            let track = match (b.ahead, b.behind) {
                (0, 0) => String::new(),
                (a, 0) => format!("\u{E08E}{a}"),
                (0, x) => format!("\u{E03E}{x}"),
                (a, x) => format!("\u{E08E}{a} \u{E03E}{x}"),
            };
            paint_text(ui, egui::Rect::from_min_size(egui::pos2(rect.right() - 90.0, rect.top() + 2.0), egui::vec2(86.0, 17.0)), &track, mono(11.0), p.dim);
            if resp.double_clicked() && !b.is_current {
                cx.run(ops::checkout(&b.name));
            }
            resp.on_hover_text("Double-click to switch; right-click for more").context_menu(|ui| menus::branch_menu(ui, cx, &b));
        }
        ui.spacing_mut().item_spacing.y = 4.0;
    }

    fn start_sweep(&mut self, ctx: &egui::Context, cx: &Cx, repos: Vec<String>, done: (&'static str, &'static str), op: impl Fn() -> ops::GitOp + Send + 'static) {
        let git = cx.repo.git.clone();
        self.sweep.spawn(ctx, move || sweep(git, repos, done, op));
    }

    fn workspaces(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "Workspaces", &p);
        let ws: Vec<Workspace> = cx.settings.workspaces.clone();
        ui.horizontal_wrapped(|ui| {
            for (i, w) in ws.iter().enumerate() {
                if ui.selectable_label(self.workspace_idx == i, w.name.as_str()).clicked() {
                    self.workspace_idx = i;
                    self.summaries_for = None;
                }
            }
            if ui.small_button("New workspace…").clicked() {
                cx.form(new_workspace_form(ws.iter().map(|w| w.name.clone()).collect()));
            }
        });
        if ws.is_empty() {
            ui.label(RichText::new("Group repositories to see their status together and fetch them all at once.").color(p.faint));
            return;
        }
        self.workspace_idx = self.workspace_idx.min(ws.len() - 1);
        let w = &ws[self.workspace_idx];
        if self.sweep.poll() {
            if let Some(m) = self.sweep.value.take() {
                cx.toast(m);
            }
            if let Some(e) = self.sweep.error.take() {
                cx.error(e);
            }
            self.swept += 1;
        }
        // What a sweep did to the others doesn't show in this repository's generation.
        let key = format!("{}:{}:{}", w.name, cx.data.status_gen, self.swept);
        if self.summaries_for.as_deref() != Some(&key) {
            self.summaries_for = Some(key);
            let (git, repos) = (cx.repo.git.clone(), w.repos.clone());
            self.summaries.spawn(ui.ctx(), move || -> Result<Vec<RepoSummary>, String> {
                Ok(repos
                    .iter()
                    .map(|path| {
                        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                        match Repo::open(git.clone(), std::path::Path::new(path)).and_then(|r| r.status()) {
                            Ok(s) => RepoSummary { path: path.clone(), name, branch: s.branch.unwrap_or_else(|| "detached".into()), changes: s.entries.len(), ahead: s.ahead, behind: s.behind, error: None },
                            Err(e) => RepoSummary { path: path.clone(), name, error: Some(e.to_string()), ..Default::default() },
                        }
                    })
                    .collect())
            });
        }
        ui.horizontal(|ui| {
            let busy = self.sweep.busy();
            let why = "One of these is already going through the repositories.";
            if ui.add_enabled(!busy, egui::Button::new("Fetch all").small()).on_disabled_hover_text(why).clicked() {
                let prune = cx.settings.prune_on_fetch;
                self.start_sweep(ui.ctx(), cx, w.repos.clone(), ("Fetched", "fetching"), move || ops::fetch(&ops::FetchOpts { all: true, prune, ..Default::default() }));
            }
            if ui.add_enabled(!busy, egui::Button::new("Pull all").small()).on_hover_text("Fast-forward only, so nothing needs resolving. A repository that can't is left as it is and reported").on_disabled_hover_text(why).clicked() {
                self.start_sweep(ui.ctx(), cx, w.repos.clone(), ("Pulled", "pulling"), || ops::pull(ops::PullMode::FfOnly, false));
            }
            if ui.small_button("Open all").clicked() {
                for r in &w.repos {
                    cx.out.push(Action::OpenRepo(r.into()));
                }
            }
            if ui.small_button("Add current repository").clicked() {
                let name = w.name.clone();
                let path = cx.repo.root.to_string_lossy().replace('\\', "/");
                cx.out.push(Action::Settings(Box::new(move |s| {
                    if let Some(x) = s.workspaces.iter_mut().find(|x| x.name == name)
                        && !x.repos.contains(&path) {
                            x.repos.push(path);
                        }
                })));
                self.summaries_for = None;
            }
            if ui.add_enabled(!self.scan_pick.busy(), egui::Button::new("Scan a folder…").small()).on_disabled_hover_text("The folder picker is already open.").clicked() {
                crate::widgets::pick(&mut self.scan_pick, ui.ctx(), rfd::FileDialog::new(), true);
            }
            if self.scan_pick.poll()
                && let Some(Some(dir)) = self.scan_pick.value.take() {
                    // Off the UI thread, as a big folder takes a while.
                    let name = w.name.clone();
                    cx.out.push(Action::Later(Box::new(move || {
                        let found = gitem_core::repo::find_repos(&dir, 3);
                        Ok(crate::action::Later::Many(vec![
                            crate::action::Later::Toast(format!("Found {} repositories", found.len())),
                            crate::action::Later::Settings(Box::new(move |s| {
                                if let Some(x) = s.workspaces.iter_mut().find(|x| x.name == name) {
                                    for f in found {
                                        let f = f.to_string_lossy().replace('\\', "/");
                                        if !x.repos.contains(&f) {
                                            x.repos.push(f);
                                        }
                                    }
                                }
                            })),
                        ]))
                    })));
                }
            if ui.small_button("Delete workspace…").clicked() {
                let name = w.name.clone();
                cx.form(FormDialog::new("Delete workspace", "Delete").danger().info(format!("Delete the workspace {name} and its list of {}? The repositories themselves aren't touched.", plural(w.repos.len(), "repository", "repositories"))).on_submit(move |_| {
                    let name = name.clone();
                    Ok(vec![Action::Settings(Box::new(move |s| {
                        // One only: older settings can hold two of the same name.
                        if let Some(at) = s.workspaces.iter().position(|x| x.name == name) {
                            s.workspaces.remove(at);
                        }
                    }))])
                }));
            }
            if self.summaries.busy() || self.sweep.busy() {
                ui.add(spinner(13.0));
            }
        });
        if let Some(list) = self.summaries.value.take() {
            ui.spacing_mut().item_spacing.y = 0.0;
            for s in &list {
                let (rect, resp) = row(ui, 26.0, false, &p);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, s.name.as_str()));
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 0.0), egui::vec2(200.0, 26.0)), &s.name, prop(13.0), p.text);
                let detail = match &s.error {
                    Some(e) => e.clone(),
                    None => format!("{} · {} changed · \u{E08E}{} \u{E03E}{}", s.branch, s.changes, s.ahead, s.behind),
                };
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(212.0, 0.0), egui::vec2(rect.width() - 216.0, 26.0)), &detail, prop(12.0), if s.error.is_some() { p.danger } else { p.dim });
                let resp = resp.on_hover_text("Click to open. Right-click to remove it from this workspace.");
                if resp.clicked() {
                    cx.out.push(Action::OpenRepo(s.path.clone().into()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Remove from this workspace").clicked() {
                        let (name, path) = (w.name.clone(), s.path.clone());
                        cx.out.push(Action::Settings(Box::new(move |st| {
                            if let Some(x) = st.workspaces.iter_mut().find(|x| x.name == name) {
                                x.repos.retain(|r| *r != path);
                            }
                        })));
                        self.summaries_for = None;
                        ui.close();
                    }
                });
            }
            ui.spacing_mut().item_spacing.y = 4.0;
            if self.summaries.value.is_none() {
                self.summaries.value = Some(list);
            }
        }
    }
}

/// One op through each repository in turn, off the UI thread. What failed is reported once, all
/// together, and doesn't stop the others. A repository that hasn't been opened and has config
/// that would run a program is left out until someone has decided about it.
fn sweep(git: gitem_core::GitExe, repos: Vec<String>, (done, doing): (&str, &str), op: impl Fn() -> ops::GitOp) -> Result<String, String> {
    let (mut worked, mut failed) = (0, Vec::new());
    for path in &repos {
        let dir = std::path::Path::new(path);
        let result = match Repo::open(git.clone(), dir) {
            Ok(r) if r.trust.restricted() => {
                failed.push(format!("Not {doing} {path}: its config names programs that you haven't said can run. Open it to decide."));
                continue;
            }
            Ok(r) => op().unattended().run(&r.git, Some(dir), None, &mut |_| {}).map(|_| ()).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => worked += 1,
            Err(e) => failed.push(format!("{path}: {}", e.lines().last().unwrap_or(&e))),
        }
    }
    if failed.is_empty() {
        Ok(format!("{done} {}", plural(worked, "repository", "repositories")))
    } else {
        Err(format!("{done} {}. {} didn't work:\n{}", plural(worked, "repository", "repositories"), failed.len(), failed.join("\n")))
    }
}

fn new_workspace_form(taken: Vec<String>) -> FormDialog {
    FormDialog::new("New workspace", "Create").text("name", "Name", "").on_submit(move |v| {
        let name = v.opt("name").ok_or("A name is required.")?;
        // Workspaces are found by name, so a second of the same name could never be reached.
        if taken.contains(&name) {
            return Err(format!("There's already a workspace called {name}."));
        }
        Ok(vec![Action::Settings(Box::new(move |s| s.workspaces.push(Workspace { name, repos: Vec::new() })))])
    })
}
