//! Left sidebar: trees of commits, branches, remotes, tags, stashes, worktrees,
//! contributors, submodules, and repository files.
use std::collections::BTreeMap;

use egui::{RichText, Sense, Ui, pos2, vec2};
use gitem_core::model::*;
use gitem_core::ops;

use crate::action::Cx;
use crate::forms;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

struct Files {
    list: Vec<String>,
    tree: Dir,
}

#[derive(Default)]
pub struct Sidebar {
    filter: String,
    commits: Task<Vec<Commit>>,
    commits_gen: Option<u64>,
    files: Task<Files>,
    files_gen: Option<u64>,
    contributors: Task<Vec<Contributor>>,
    contributors_gen: Option<u64>,
}

const ROW: f32 = 22.0;

impl Sidebar {
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.commits.poll();
        self.files.poll();
        self.contributors.poll();
        ui.add_space(2.0);
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter branches, tags, files…").desired_width(f32::INFINITY));
        ui.add_space(2.0);
        egui::ScrollArea::vertical().id_salt("sidebar").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.current_commits(ui, cx);
            self.branches(ui, cx);
            self.remotes(ui, cx);
            self.tags(ui, cx);
            self.stashes(ui, cx);
            self.worktrees(ui, cx);
            self.contributor_list(ui, cx);
            self.submodules(ui, cx);
            self.file_tree(ui, cx);
        });
    }

    fn matches(&self, s: &str) -> bool {
        self.filter.is_empty() || s.to_lowercase().contains(&self.filter.to_lowercase())
    }

    fn current_commits(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let branch = cx.current_branch().unwrap_or_else(|| "HEAD".into());
        egui::CollapsingHeader::new(RichText::new(format!("COMMITS ({branch})")).small().strong().color(p.dim)).id_salt("sb-commits").default_open(false).show(ui, |ui| {
            if self.commits_gen != Some(cx.data.refs_gen) {
                self.commits_gen = Some(cx.data.refs_gen);
                let r = cx.repo.clone();
                self.commits.spawn(ui.ctx(), move || r.log(&LogQuery { revs: vec!["HEAD".into()], limit: 100, ..Default::default() }));
            }
            if let Some(list) = self.commits.value.take() {
                for c in &list {
                    if !self.matches(&c.subject) {
                        continue;
                    }
                    let (rect, resp) = row(ui, ROW, false, &p);
                    paint_avatar(ui, pos2(rect.left() + 12.0, rect.center().y), 7.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
                    paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(24.0, 0.0), rect.max), &c.subject, prop(12.5), p.text);
                    let resp = resp.on_hover_ui(|ui| commit_card(ui, &c.sha, &c.author_name, &c.author_email, c.author_time, &c.subject, &p, cx.settings.gravatar));
                    if resp.clicked() {
                        cx.inspect(&c.sha);
                    }
                    resp.context_menu(|ui| menus::commit_menu(ui, cx, c));
                }
                self.commits.value = Some(list);
            }
        });
    }

    fn branches(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list: Vec<Branch> = cx.data.local_branches().filter(|b| self.matches(&b.name)).cloned().collect();
        egui::CollapsingHeader::new(RichText::new(format!("BRANCHES ({})", list.len())).small().strong().color(p.dim)).id_salt("sb-branches").default_open(true).show(ui, |ui| {
            if ui.small_button("+ New branch").clicked() {
                cx.form(forms::new_branch(None));
            }
            for b in &list {
                self.branch_row(ui, cx, b, &b.name);
            }
        });
    }

    fn branch_row(&self, ui: &mut Ui, cx: &mut Cx, b: &Branch, label: &str) {
        let p = cx.pal.clone();
        let (rect, resp) = row(ui, ROW, b.is_current, &p);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let text_color = if b.is_current { p.text } else { p.dim };
        ui.painter().circle(pos2(rect.left() + 10.0, rect.center().y), 3.5, if b.is_current { p.accent } else { p.panel }, egui::Stroke::new(1.0, if b.is_current { p.accent } else { p.faint }), );
        let mut right = rect.right() - 4.0;
        let track = match (b.ahead, b.behind) {
            (0, 0) => String::new(),
            (a, 0) => format!("↑{a}"),
            (0, x) => format!("↓{x}"),
            (a, x) => format!("↑{a}↓{x}"),
        };
        if !track.is_empty() {
            let g = ui.painter().layout_no_wrap(track, mono(10.5), p.faint);
            right -= g.size().x;
            ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.faint);
            right -= 4.0;
        }
        if b.upstream_gone {
            let g = ui.painter().layout_no_wrap("gone".to_string(), prop(10.5), p.warn);
            right -= g.size().x;
            ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.warn);
            right -= 4.0;
        }
        if b.worktree.is_some() && !b.is_current {
            let g = ui.painter().layout_no_wrap("wt".to_string(), prop(10.5), p.series[6]);
            right -= g.size().x;
            ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.series[6]);
            right -= 4.0;
        }
        paint_text(ui, egui::Rect::from_min_max(pos2(rect.left() + 20.0, rect.top()), pos2(right, rect.bottom())), label, prop(12.5), text_color);
        let resp = resp.on_hover_text(format!("{}\n{}\n{} · {}", b.name, b.subject, b.author, gitem_core::util::relative_time(b.date, gitem_core::util::now_ts())));
        if resp.clicked() {
            cx.inspect(&b.sha);
        }
        if resp.double_clicked() && !b.is_current {
            if b.is_remote {
                cx.run(ops::checkout_remote(&b.name));
            } else {
                cx.run(ops::checkout(&b.name));
            }
        }
        resp.context_menu(|ui| menus::branch_menu(ui, cx, b));
    }

    fn remotes(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let remotes = cx.data.remotes().to_vec();
        egui::CollapsingHeader::new(RichText::new(format!("REMOTES ({})", remotes.len())).small().strong().color(p.dim)).id_salt("sb-remotes").default_open(false).show(ui, |ui| {
            if ui.small_button("+ Add remote").clicked() {
                cx.form(forms::add_remote());
            }
            for r in &remotes {
                let header = egui::CollapsingHeader::new(RichText::new(&r.name).size(12.5)).id_salt(("remote", &r.name)).show(ui, |ui| {
                    let prefix = format!("{}/", r.name);
                    let list: Vec<Branch> = cx.data.remote_branches().filter(|b| b.name.starts_with(&prefix) && self.matches(&b.name)).cloned().collect();
                    for b in &list {
                        self.branch_row(ui, cx, b, b.name.strip_prefix(&prefix).unwrap_or(&b.name));
                    }
                });
                header.header_response.on_hover_text(&r.fetch_url).context_menu(|ui| {
                    if ui.button("Fetch").clicked() {
                        cx.run(ops::fetch(&ops::FetchOpts { remote: Some(r.name.clone()), prune: cx.settings.prune_on_fetch, ..Default::default() }));
                        ui.close();
                    }
                    if ui.button("Prune").clicked() {
                        cx.run(ops::remote_prune(&r.name));
                        ui.close();
                    }
                    if ui.button("Edit…").clicked() {
                        cx.form(forms::edit_remote(r.name.clone(), r.fetch_url.clone()));
                        ui.close();
                    }
                    if ui.button("Copy URL").clicked() {
                        cx.copy(r.fetch_url.clone());
                        ui.close();
                    }
                    if let Some(f) = gitem_core::forge::identify(&r.fetch_url, &cx.settings.custom_hosts)
                        && ui.button("Open in browser").clicked() {
                            cx.url(f.web_url());
                            ui.close();
                        }
                    if ui.button("Remove…").clicked() {
                        cx.confirm("Remove remote", &format!("Remove the remote {}? Its remote-tracking branches go too.", r.name), "Remove", ops::remote_remove(&r.name));
                        ui.close();
                    }
                });
            }
        });
    }

    fn tags(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list: Vec<Tag> = cx.data.tags().iter().filter(|t| self.matches(&t.name)).cloned().collect();
        egui::CollapsingHeader::new(RichText::new(format!("TAGS ({})", list.len())).small().strong().color(p.dim)).id_salt("sb-tags").default_open(false).show(ui, |ui| {
            if ui.small_button("+ New tag").clicked() {
                cx.form(forms::new_tag(None));
            }
            for t in &list {
                let (rect, resp) = row(ui, ROW, false, &p);
                paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(8.0, 0.0), rect.max), &format!("◆ {}", t.name), prop(12.5), p.dim);
                let resp = if t.subject.is_empty() { resp } else { resp.on_hover_text(&t.subject) };
                if resp.clicked() {
                    cx.inspect(&t.sha);
                }
                resp.context_menu(|ui| menus::tag_menu(ui, cx, t));
            }
        });
    }

    fn stashes(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list = cx.data.stashes().to_vec();
        egui::CollapsingHeader::new(RichText::new(format!("STASHES ({})", list.len())).small().strong().color(p.dim)).id_salt("sb-stashes").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("+ Stash").clicked() {
                    cx.form(forms::stash_form(Vec::new()));
                }
                if !list.is_empty() && ui.small_button("Clear all…").clicked() {
                    cx.confirm("Clear stashes", &format!("Permanently delete all {} stashes?", list.len()), "Clear all", ops::stash_clear());
                }
            });
            for s in &list {
                let (rect, resp) = row(ui, ROW, false, &p);
                paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(8.0, 0.0), rect.max), &s.message, prop(12.5), p.dim);
                let resp = resp.on_hover_text(format!("{} on {}", s.selector, s.branch));
                if resp.clicked() {
                    cx.inspect(&s.sha);
                }
                resp.context_menu(|ui| menus::stash_menu(ui, cx, s));
            }
        });
    }

    fn worktrees(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list = cx.data.worktrees().to_vec();
        egui::CollapsingHeader::new(RichText::new(format!("WORKTREES ({})", list.len())).small().strong().color(p.dim)).id_salt("sb-worktrees").default_open(false).show(ui, |ui| {
            if ui.small_button("+ Add worktree").clicked() {
                cx.form(forms::add_worktree(cx.repo, cx.data.ref_names(), None));
            }
            for w in &list {
                let (rect, resp) = row(ui, ROW, w.is_current, &p);
                let name = w.branch.clone().unwrap_or_else(|| "detached".to_string());
                paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(8.0, 0.0), rect.max), &name, prop(12.5), p.dim);
                let resp = resp.on_hover_text(&w.path);
                if resp.double_clicked() && !w.is_current {
                    cx.out.push(crate::action::Action::OpenRepo(w.path.clone().into()));
                }
                resp.context_menu(|ui| menus::worktree_menu(ui, cx, w));
            }
        });
    }

    fn contributor_list(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        egui::CollapsingHeader::new(RichText::new("CONTRIBUTORS").small().strong().color(p.dim)).id_salt("sb-contrib").default_open(false).show(ui, |ui| {
            if self.contributors_gen != Some(cx.data.refs_gen) {
                self.contributors_gen = Some(cx.data.refs_gen);
                let r = cx.repo.clone();
                self.contributors.spawn(ui.ctx(), move || r.contributors("HEAD", false));
            }
            if let Some(list) = self.contributors.value.take() {
                for c in list.iter().take(100) {
                    if !self.matches(&c.name) {
                        continue;
                    }
                    let (rect, resp) = row(ui, ROW, false, &p);
                    paint_avatar(ui, pos2(rect.left() + 12.0, rect.center().y), 7.0, &c.name, &c.email, &p, cx.settings.gravatar);
                    paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(24.0, 0.0), egui::pos2(rect.right() - 40.0, rect.bottom())), &c.name, prop(12.5), p.dim);
                    paint_text(ui, egui::Rect::from_min_max(egui::pos2(rect.right() - 36.0, rect.top()), rect.max), &c.commits.to_string(), mono(11.0), p.faint);
                    if resp.on_hover_text(&c.email).clicked() {
                        cx.open(Tab::search(&format!("@:{}", c.email)));
                    }
                }
                self.contributors.value = Some(list);
            }
        });
    }

    fn submodules(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list = cx.data.submodules().to_vec();
        if list.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(RichText::new(format!("SUBMODULES ({})", list.len())).small().strong().color(p.dim)).id_salt("sb-subs").default_open(false).show(ui, |ui| {
            for s in &list {
                let (rect, resp) = row(ui, ROW, false, &p);
                paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(8.0, 0.0), rect.max), &s.path, prop(12.5), p.dim);
                if resp.on_hover_text(&s.url).clicked() {
                    cx.open(Tab::Tools(Box::default()));
                }
            }
        });
    }

    fn file_tree(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        egui::CollapsingHeader::new(RichText::new("FILES").small().strong().color(p.dim)).id_salt("sb-files").default_open(false).show(ui, |ui| {
            if self.files_gen != Some(cx.data.status_gen) {
                self.files_gen = Some(cx.data.status_gen);
                let r = cx.repo.clone();
                self.files.spawn(ui.ctx(), move || -> gitem_core::Result<Files> {
                    let list = r.tracked_files(None)?;
                    let mut tree = Dir::default();
                    for f in &list {
                        tree.insert(f);
                    }
                    Ok(Files { list, tree })
                });
            }
            let Some(files) = self.files.value.take() else { return };
            if self.filter.is_empty() {
                files.tree.ui(ui, cx, "");
            } else {
                for f in files.list.iter().filter(|f| self.matches(f)).take(500) {
                    file_entry(ui, cx, f, f);
                }
            }
            self.files.value = Some(files);
        });
    }
}

fn file_entry(ui: &mut Ui, cx: &mut Cx, path: &str, label: &str) {
    let p = cx.pal.clone();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(8.0, 0.0), rect.max), label, prop(12.5), p.dim);
    if resp.clicked() {
        cx.open(Tab::file(path, None, None));
    }
    resp.context_menu(|ui| menus::file_menu(ui, cx, path, None));
}

#[derive(Default)]
pub struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, String)>,
}

impl Dir {
    pub fn insert(&mut self, path: &str) {
        let mut cur = self;
        let mut rest = path;
        while let Some((d, r)) = rest.split_once('/') {
            cur = cur.dirs.entry(d.to_string()).or_default();
            rest = r;
        }
        cur.files.push((rest.to_string(), path.to_string()));
    }

    fn ui(&self, ui: &mut Ui, cx: &mut Cx, prefix: &str) {
        for (name, d) in &self.dirs {
            let full = format!("{prefix}{name}/");
            egui::CollapsingHeader::new(RichText::new(name).size(12.5)).id_salt(("dir", &full)).show(ui, |ui| d.ui(ui, cx, &full));
        }
        for (name, path) in &self.files {
            file_entry(ui, cx, path, name);
        }
    }
}
