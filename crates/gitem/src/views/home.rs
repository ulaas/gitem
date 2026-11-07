//! Home: status of this repository, start-work and review shortcuts, recent
//! branches, and multi-repository workspaces with batch actions.
use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::settings::Workspace;
use gitem_core::{Repo, ops, util};

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
                        chip(ui, &format!("↑ {} to push", st.ahead), p.series[2]);
                    }
                    if st.behind > 0 {
                        chip(ui, &format!("↓ {} to pull", st.behind), p.series[6]);
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
                    if ui.button("Open changes").clicked() {
                        cx.open(Tab::changes());
                    }
                    if ui.button("Commit graph").clicked() {
                        cx.open(Tab::graph());
                    }
                    if ui.button("Launchpad").clicked() {
                        cx.open(Tab::Launchpad(Box::default()));
                    }
                });
            }
        }
        section(ui, "Start", &p);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Start work…").on_hover_text("Create a branch, optionally in a new worktree").clicked() {
                let base = cx.repo.default_branch().unwrap_or_else(|| "HEAD".to_string());
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
                                vec![Action::RunThenOpen(ops::worktree_add(&path, None, Some(&n), b.as_deref(), false), path.into())]
                            } else {
                                vec![Action::Run(ops::branch_create(&n, b.as_deref(), true))]
                            })
                        }),
                );
            }
            if ui.button("Review a pull request").clicked() {
                cx.open(Tab::Launchpad(Box::default()));
            }
            if ui.button("Compare refs").clicked() {
                cx.open(Tab::Search(Box::new(crate::views::search::SearchView::compare("", ""))));
            }
            if ui.button("Search commits").clicked() {
                cx.open(Tab::Search(Box::default()));
            }
            if ui.button("Stash…").clicked() {
                cx.form(forms::stash_form(Vec::new()));
            }
        });
    }

    fn right_column(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "Recent branches", &p);
        let branches: Vec<Branch> = cx.data.local_branches().take(8).cloned().collect();
        if branches.is_empty() {
            ui.label(RichText::new("No branches yet").color(p.dim));
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        for b in branches {
            let (rect, resp) = row(ui, 34.0, b.is_current, &p);
            let name = if b.is_current { format!("● {}", b.name) } else { b.name.clone() };
            paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 2.0), egui::vec2(rect.width() - 110.0, 17.0)), &name, prop(13.0), p.text);
            paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 18.0), egui::vec2(rect.width() - 16.0, 14.0)), &format!("{} · {}", util::relative_time(b.date, util::now_ts()), b.subject), prop(11.0), p.dim);
            let track = match (b.ahead, b.behind) {
                (0, 0) => String::new(),
                (a, 0) => format!("↑{a}"),
                (0, x) => format!("↓{x}"),
                (a, x) => format!("↑{a} ↓{x}"),
            };
            paint_text(ui, egui::Rect::from_min_size(egui::pos2(rect.right() - 90.0, rect.top() + 2.0), egui::vec2(86.0, 17.0)), &track, mono(11.0), p.dim);
            if resp.double_clicked() && !b.is_current {
                cx.run(ops::checkout(&b.name));
            }
            resp.on_hover_text("Double-click to switch; right-click for more").context_menu(|ui| menus::branch_menu(ui, cx, &b));
        }
        ui.spacing_mut().item_spacing.y = 4.0;
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
                cx.form(new_workspace_form());
            }
        });
        if ws.is_empty() {
            ui.label(RichText::new("Group repositories to see their status together and fetch them all at once.").color(p.faint));
            return;
        }
        self.workspace_idx = self.workspace_idx.min(ws.len() - 1);
        let w = &ws[self.workspace_idx];
        let key = format!("{}:{}", w.name, cx.data.status_gen);
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
            if ui.small_button("Fetch all").clicked() {
                for r in &w.repos {
                    // One of these may never have been opened here, and a fetch can start
                    // what its config names. So the same check as opening it, and no fetch
                    // until that's decided.
                    if Repo::open(cx.repo.git.clone(), std::path::Path::new(r)).is_ok_and(|x| x.trust.restricted()) {
                        cx.error(format!("Not fetching {r}: its config names programs that you haven't said can run. Open it to decide."));
                        continue;
                    }
                    let op = ops::fetch(&ops::FetchOpts { all: true, prune: cx.settings.prune_on_fetch, ..Default::default() });
                    let mut wrapped = ops::GitOp::new(format!("Fetch {r}")).net();
                    for s in op.steps {
                        let mut args = vec!["-C".to_string(), r.clone()];
                        args.extend(s.args);
                        wrapped = wrapped.step_v(args);
                    }
                    cx.out.push(Action::RunGlobal { op: wrapped, open_after: None });
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
            if ui.small_button("Scan a folder…").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    let found = gitem_core::repo::find_repos(&dir, 3);
                    let name = w.name.clone();
                    cx.toast(format!("Found {} repositories", found.len()));
                    cx.out.push(Action::Settings(Box::new(move |s| {
                        if let Some(x) = s.workspaces.iter_mut().find(|x| x.name == name) {
                            for f in found {
                                let f = f.to_string_lossy().replace('\\', "/");
                                if !x.repos.contains(&f) {
                                    x.repos.push(f);
                                }
                            }
                        }
                    })));
                    self.summaries_for = None;
                }
            if ui.small_button("Delete workspace").clicked() {
                let name = w.name.clone();
                cx.out.push(Action::Settings(Box::new(move |s| s.workspaces.retain(|x| x.name != name))));
            }
            if self.summaries.busy() {
                ui.add(egui::Spinner::new().size(13.0));
            }
        });
        if let Some(list) = self.summaries.get() {
            ui.spacing_mut().item_spacing.y = 0.0;
            for s in list {
                let (rect, resp) = row(ui, 26.0, false, &p);
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 0.0), egui::vec2(200.0, 26.0)), &s.name, prop(13.0), p.text);
                let detail = match &s.error {
                    Some(e) => e.clone(),
                    None => format!("{} · {} changed · ↑{} ↓{}", s.branch, s.changes, s.ahead, s.behind),
                };
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(212.0, 0.0), egui::vec2(rect.width() - 216.0, 26.0)), &detail, prop(12.0), if s.error.is_some() { p.danger } else { p.dim });
                if resp.on_hover_text("Click to open").clicked() {
                    cx.out.push(Action::OpenRepo(s.path.clone().into()));
                }
            }
            ui.spacing_mut().item_spacing.y = 4.0;
        }
    }
}

fn new_workspace_form() -> FormDialog {
    FormDialog::new("New workspace", "Create").text("name", "Name", "").on_submit(|v| {
        let name = v.opt("name").ok_or("A name is required.")?;
        Ok(vec![Action::Settings(Box::new(move |s| s.workspaces.push(Workspace { name, repos: Vec::new() })))])
    })
}
