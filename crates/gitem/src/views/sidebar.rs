//! Left sidebar: trees of commits, branches, remotes, tags, stashes, worktrees,
//! contributors, submodules, and repository files.
use std::collections::{BTreeMap, HashMap};

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
    /// Commits a branch is missing from its upstream, and the reverse, by branch.
    tracking: HashMap<String, Tracking>,
    /// Branches opened or closed by hand. Otherwise, ones with incoming commits start open.
    expanded: HashMap<String, bool>,
}

#[derive(Default)]
struct Tracking {
    refs_gen: Option<u64>,
    incoming: Task<Vec<Commit>>,
    outgoing: Task<Vec<Commit>>,
}

const ROW: f32 = 22.0;
const SECTION_ROW: f32 = 28.0;
/// How many incoming or outgoing commits a branch lists.
const TRACKING_LIMIT: usize = 50;

impl Sidebar {
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.commits.poll();
        self.files.poll();
        self.contributors.poll();
        for t in self.tracking.values_mut() {
            t.incoming.poll();
            t.outgoing.poll();
        }
        ui.add_space(2.0);
        ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter branches, tags, files…").desired_width(f32::INFINITY));
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
        section(ui, "sb-commits", crate::chrome::icon::GIT_COMMIT, p.series[0], "Commits", &branch, false, &[], &p, |ui| {
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
        let action = section(ui, "sb-branches", crate::chrome::icon::GIT_BRANCH, p.series[0], "Branches", &list.len().to_string(), true, &[(crate::chrome::icon::PLUS, "New branch")], &p, |ui| {
            for b in &list {
                self.branch_row(ui, cx, b, &b.name);
            }
        });
        if action.is_some() {
            cx.form(forms::new_branch(None));
        }
    }

    fn branch_row(&mut self, ui: &mut Ui, cx: &mut Cx, b: &Branch, label: &str) {
        let p = cx.pal.clone();
        let (rect, resp) = row(ui, ROW, b.is_current, &p);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let text_color = if b.is_current { p.text } else { p.dim };
        let can_expand = !b.is_remote && !b.upstream_gone && b.upstream.is_some() && (b.ahead > 0 || b.behind > 0);
        let open = can_expand && self.expanded.get(&b.name).copied().unwrap_or(b.behind > 0);
        if can_expand {
            let hit = egui::Rect::from_min_size(rect.min, vec2(16.0, ROW));
            let caret = ui.interact(hit, resp.id.with("expand"), Sense::click());
            let name = format!("{} {label}", if open { "Collapse" } else { "Expand" });
            caret.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
            let glyph = if open { crate::chrome::icon::CARET_DOWN } else { crate::chrome::icon::CARET_RIGHT };
            ui.painter().text(hit.center(), egui::Align2::CENTER_CENTER, glyph, prop(11.0), if caret.hovered() { p.text } else { p.faint });
            if caret.clicked() {
                self.expanded.insert(b.name.clone(), !open);
            }
        }
        ui.painter().circle(pos2(rect.left() + 21.0, rect.center().y), 3.5, if b.is_current { p.accent } else { p.panel }, egui::Stroke::new(1.0, if b.is_current { p.accent } else { p.faint }), );
        let mut right = rect.right() - 4.0;
        // Incoming comes last, at the edge, so it's the easiest to spot.
        if b.behind > 0 {
            right = count_pill(ui, right, rect.center().y, &format!("↓{}", b.behind), p.incoming());
        }
        if b.ahead > 0 {
            right = count_pill(ui, right, rect.center().y, &format!("↑{}", b.ahead), p.outgoing());
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
        paint_text(ui, egui::Rect::from_min_max(pos2(rect.left() + 30.0, rect.top()), pos2(right, rect.bottom())), label, prop(12.5), text_color);
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
        if open {
            self.tracking_lists(ui, cx, b);
        }
    }

    /// What's on the upstream but not here, then what's here but not on the upstream.
    fn tracking_lists(&mut self, ui: &mut Ui, cx: &mut Cx, b: &Branch) {
        let p = cx.pal.clone();
        let Some(upstream) = b.upstream.clone() else { return };
        let t = self.tracking.entry(b.name.clone()).or_default();
        if t.refs_gen != Some(cx.data.refs_gen) {
            t.refs_gen = Some(cx.data.refs_gen);
            for (task, count, range) in [(&mut t.incoming, b.behind, format!("{}..{upstream}", b.name)), (&mut t.outgoing, b.ahead, format!("{upstream}..{}", b.name))] {
                if count > 0 {
                    let r = cx.repo.clone();
                    task.spawn(ui.ctx(), move || r.log(&LogQuery { revs: vec![range], limit: TRACKING_LIMIT, ..Default::default() }));
                } else {
                    task.value = None;
                }
            }
        }
        if b.behind > 0 {
            if tracking_heading(ui, crate::chrome::icon::ARROW_DOWN, &format!("Incoming from {upstream}"), b.behind, p.incoming(), b.is_current.then_some("Pull")) {
                let mode = match cx.settings.pull_mode.as_str() {
                    "ff-only" => ops::PullMode::FfOnly,
                    "rebase" => ops::PullMode::Rebase,
                    "merge" => ops::PullMode::Merge,
                    _ => ops::PullMode::Default,
                };
                cx.run(ops::pull(mode, false));
            }
            tracking_commits(ui, cx, t.incoming.get(), b.behind);
        }
        if b.ahead > 0 {
            if tracking_heading(ui, crate::chrome::icon::ARROW_UP, &format!("Outgoing to {upstream}"), b.ahead, p.outgoing(), b.is_current.then_some("Push")) {
                cx.run(ops::push(&ops::PushOpts::default()));
            }
            tracking_commits(ui, cx, t.outgoing.get(), b.ahead);
        }
    }

    fn remotes(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let remotes = cx.data.remotes().to_vec();
        let action = section(ui, "sb-remotes", crate::chrome::icon::CLOUD, p.series[6], "Remotes", &remotes.len().to_string(), false, &[(crate::chrome::icon::PLUS, "Add remote")], &p, |ui| {
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
        if action.is_some() {
            cx.form(forms::add_remote());
        }
    }

    fn tags(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list: Vec<Tag> = cx.data.tags().iter().filter(|t| self.matches(&t.name)).cloned().collect();
        let action = section(ui, "sb-tags", crate::chrome::icon::TAG, p.series[3], "Tags", &list.len().to_string(), false, &[(crate::chrome::icon::PLUS, "New tag")], &p, |ui| {
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
        if action.is_some() {
            cx.form(forms::new_tag(None));
        }
    }

    fn stashes(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list = cx.data.stashes().to_vec();
        let mut actions = vec![(crate::chrome::icon::PLUS, "New stash")];
        if !list.is_empty() {
            actions.push((crate::chrome::icon::TRASH, "Clear all stashes"));
        }
        let action = section(ui, "sb-stashes", crate::chrome::icon::ARCHIVE, p.series[4], "Stashes", &list.len().to_string(), false, &actions, &p, |ui| {
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
        match action {
            Some(0) => cx.form(forms::stash_form(Vec::new())),
            Some(_) => cx.confirm("Clear stashes", &format!("Permanently delete all {} stashes?", list.len()), "Clear all", ops::stash_clear()),
            None => {}
        }
    }

    fn worktrees(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let list = cx.data.worktrees().to_vec();
        let action = section(ui, "sb-worktrees", crate::chrome::icon::TREE_STRUCTURE, p.series[2], "Worktrees", &list.len().to_string(), false, &[(crate::chrome::icon::PLUS, "Add worktree")], &p, |ui| {
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
        if action.is_some() {
            cx.form(forms::add_worktree(cx.repo, cx.data.ref_names(), None));
        }
    }

    fn contributor_list(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        section(ui, "sb-contrib", crate::chrome::icon::USERS_THREE, p.series[2], "Contributors", "", false, &[], &p, |ui| {
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
        section(ui, "sb-subs", crate::chrome::icon::PACKAGE, p.series[1], "Submodules", &list.len().to_string(), false, &[], &p, |ui| {
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
        section(ui, "sb-files", crate::chrome::icon::FOLDER, p.series[3], "Files", "", false, &[], &p, |ui| {
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

/// A small coloured count, right-aligned at `right`. Returns the space left of it.
fn count_pill(ui: &Ui, right: f32, y: f32, text: &str, color: egui::Color32) -> f32 {
    let g = ui.painter().layout_no_wrap(text.to_string(), mono(10.5), egui::Color32::WHITE);
    let size = vec2(g.size().x + 8.0, 15.0);
    let r = egui::Rect::from_min_size(pos2(right - size.x, y - size.y / 2.0), size);
    ui.painter().rect_filled(r, 7.0, color);
    ui.painter().galley(pos2(r.left() + 4.0, y - g.size().y / 2.0), g, egui::Color32::WHITE);
    r.left() - 3.0
}

/// Returns whether the action button was clicked.
fn tracking_heading(ui: &mut Ui, glyph: &str, title: &str, count: u32, color: egui::Color32, action: Option<&str>) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    ui.painter().text(pos2(rect.left() + 21.0, rect.center().y), egui::Align2::CENTER_CENTER, glyph, prop(12.0), color);
    let mut right = rect.right() - 4.0;
    let mut clicked = false;
    if let Some(label) = action {
        let btn = egui::Rect::from_min_size(pos2(right - 42.0, rect.top() + 2.0), vec2(42.0, ROW - 4.0));
        clicked = ui.put(btn, egui::Button::new(RichText::new(label).size(11.0))).clicked();
        right = btn.left() - 4.0;
    }
    let text = format!("{title} ({count})");
    let label = ui.interact(egui::Rect::from_min_max(pos2(rect.left() + 30.0, rect.top()), pos2(right, rect.bottom())), ui.id().with(("tracking", title)), Sense::hover());
    label.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
    paint_text(ui, label.rect, &text, prop(11.5), color);
    label.on_hover_text(text.as_str());
    clicked
}

fn tracking_commits(ui: &mut Ui, cx: &mut Cx, commits: Option<&Vec<Commit>>, total: u32) {
    let p = cx.pal.clone();
    let Some(list) = commits else {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        paint_text(ui, egui::Rect::from_min_max(pos2(rect.left() + 30.0, rect.top()), rect.max), "Loading…", prop(12.0), p.faint);
        return;
    };
    for c in list {
        let (rect, resp) = row(ui, ROW, false, &p);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, c.subject.as_str()));
        paint_avatar(ui, pos2(rect.left() + 37.0, rect.center().y), 7.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
        paint_text(ui, egui::Rect::from_min_max(rect.min + vec2(49.0, 0.0), rect.max), &c.subject, prop(12.5), p.text);
        let resp = resp.on_hover_ui(|ui| commit_card(ui, &c.sha, &c.author_name, &c.author_email, c.author_time, &c.subject, &p, cx.settings.gravatar));
        if resp.clicked() {
            cx.inspect(&c.sha);
        }
        resp.context_menu(|ui| menus::commit_menu(ui, cx, c));
    }
    let more = total as usize - list.len().min(total as usize);
    if more > 0 {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        paint_text(ui, egui::Rect::from_min_max(pos2(rect.left() + 30.0, rect.top()), rect.max), &format!("and {more} more"), prop(12.0), p.faint);
    }
}

/// A collapsible section: a tall header that lights up on hover, with icon buttons on
/// its right, which work with the section open or closed. `detail` follows the title
/// in a fainter colour. Returns which action was clicked.
#[allow(clippy::too_many_arguments)]
fn section(ui: &mut Ui, salt: &str, glyph: &str, tint: egui::Color32, title: &str, detail: &str, default_open: bool, actions: &[(&str, &str)], p: &crate::theme::Palette, body: impl FnOnce(&mut Ui)) -> Option<usize> {
    let id = ui.id().with(salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), SECTION_ROW), Sense::click());
    let name = format!("{title} section");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let lit = ui.rect_contains_pointer(rect);
    if lit {
        ui.painter().rect_filled(rect.shrink2(vec2(0.0, 1.0)), 4.0, p.hover);
    }
    let y = rect.center().y;
    let caret = if state.is_open() { crate::chrome::icon::CARET_DOWN } else { crate::chrome::icon::CARET_RIGHT };
    ui.painter().text(pos2(rect.left() + 9.0, y), egui::Align2::CENTER_CENTER, caret, prop(11.0), if lit { p.text } else { p.faint });
    ui.painter().text(pos2(rect.left() + 25.0, y), egui::Align2::CENTER_CENTER, glyph, prop(15.0), tint);

    const ACTION: f32 = 24.0;
    let mut clicked = None;
    let mut right = rect.right() - 2.0;
    for (i, (icon, label)) in actions.iter().enumerate().rev() {
        let r = egui::Rect::from_min_size(pos2(right - ACTION, y - 11.0), vec2(ACTION, 22.0));
        let a = ui.interact(r, id.with(("action", i)), Sense::click());
        a.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *label));
        if a.hovered() {
            ui.painter().rect_filled(r, 4.0, p.button_hover);
        }
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *icon, prop(15.0), if a.hovered() { p.text } else if lit { p.dim } else { p.faint });
        if a.on_hover_text(*label).clicked() {
            clicked = Some(i);
        }
        right = r.left();
    }

    let text = egui::Rect::from_min_max(pos2(rect.left() + 38.0, rect.top()), pos2(right - 2.0, rect.bottom()));
    let painter = ui.painter().with_clip_rect(text.intersect(ui.clip_rect()));
    let shown = painter.text(pos2(text.left(), y), egui::Align2::LEFT_CENTER, title, prop(12.5), if lit { p.text } else { p.dim });
    if !detail.is_empty() {
        painter.text(pos2(shown.right() + 7.0, y), egui::Align2::LEFT_CENTER, detail, prop(11.5), p.faint);
    }
    if resp.clicked() {
        state.toggle(ui);
    }
    state.show_body_unindented(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 0, top: 0, bottom: 2 }).show(ui, body);
    });
    clicked
}
