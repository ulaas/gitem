//! The commit graph: virtualised table with lanes, ref badges, search with
//! highlighting, branch filters, a minimap, and drag-and-drop between refs.
use std::collections::{HashMap, HashSet};

use egui::epaint::CubicBezierShape;
use egui::{Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use gitem_core::graph::{self, GraphRow};
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::parse_search;

use crate::action::Cx;
use crate::forms;
use crate::menus;
use crate::chrome::icon;
use crate::task::Task;
use crate::views::Tab;
use crate::views::graphcols::Columns;
use crate::widgets::*;

const ROW_H: f32 = 24.0;
const LANE_W: f32 = 14.0;
const WIP: &str = "__WIP__";

struct Graph {
    commits: Vec<Commit>,
    rows: Vec<GraphRow>,
    index: HashMap<String, usize>,
    max_width: u16,
    /// Git stopped at the limit, so there may be older commits.
    more: bool,
    /// Commits from git, before the working-changes row and stash clean-up.
    loaded: usize,
}

#[derive(Clone, PartialEq, Default)]
struct Filters {
    remotes: bool,
    tags: bool,
    stashes: bool,
    no_merges: bool,
    first_parent: bool,
    only_current: bool,
    hidden: Vec<String>,
    solo: Option<String>,
    /// Smart or Favorites preset: show exactly these refs.
    preset: Option<Vec<String>>,
}

pub struct GraphView {
    data: Task<Graph>,
    loaded_gen: u64,
    loaded_status_gen: u64,
    limit: usize,
    filters: Filters,
    selected: Vec<String>,
    /// Where a Shift+click range starts. A commit, not a row: rows move on every reload.
    last_clicked: Option<String>,
    scroll_to: Option<usize>,
    /// A row to bring into view only if it isn't: the keyboard moves one row at a time, and
    /// the list jumping about each time would lose your place.
    reveal: Option<usize>,
    /// How far down the list was on the last frame.
    last_scroll_y: f32,
    query: String,
    matches: Task<HashSet<String>>,
    /// A ref being looked up to jump to, and the name asked for.
    jump: Task<(String, String)>,
    egui_ctx: egui::Context,
    match_list: Vec<usize>,
    match_pos: usize,
    filter_to_matches: bool,
    wip: bool,
    match_all: bool,
    match_case: bool,
    regex: bool,
    focus_search: bool,
    finder: Option<String>,
    stats: Task<HashMap<String, (u32, u32)>>,
    stats_gen: Option<u64>,
    /// Filters changed from outside the toolbar.
    pending_reload: bool,
    /// Lanes drawn narrow, by column.
    folded: HashSet<u16>,
    fold_request: std::cell::Cell<Option<Option<u16>>>,
    minimap_lines: bool,
    show_minimap: bool,
    pub cols: Columns,
    layout_applied: bool,
    worktree_counts: Task<HashMap<String, usize>>,
    counts_gen: Option<u64>,
    head_shown: bool,
    /// Shift+F10 was pressed: the selected row's menu opens on the next frame.
    menu_key: bool,
}

const FOLDED_W: f32 = 3.0;

impl Default for GraphView {
    fn default() -> Self {
        Self {
            data: Task::default(),
            loaded_gen: u64::MAX,
            jump: Task::default(),
            egui_ctx: egui::Context::default(),
            loaded_status_gen: u64::MAX,
            limit: 0,
            filters: Filters { remotes: true, tags: true, stashes: true, ..Default::default() },
            selected: Vec::new(),
            last_clicked: None,
            scroll_to: None,
            reveal: None,
            last_scroll_y: 0.0,
            query: String::new(),
            matches: Task::default(),
            match_list: Vec::new(),
            match_pos: 0,
            filter_to_matches: false,
            wip: false,
            match_all: false,
            match_case: false,
            regex: false,
            focus_search: false,
            finder: None,
            stats: Task::default(),
            stats_gen: None,
            pending_reload: false,
            folded: HashSet::new(),
            fold_request: std::cell::Cell::new(None),
            minimap_lines: false,
            show_minimap: true,
            cols: Columns::default(),
            layout_applied: false,
            worktree_counts: Task::default(),
            counts_gen: None,
            head_shown: false,
            menu_key: false,
        }
    }
}

/// A commit's refs in the order that matters: where you are, your branches, what's on the
/// remotes, then tags. `origin/HEAD` says nothing `origin/main` doesn't, and a branch level with
/// its remote twin takes one badge, marked with the bool.
pub fn badges_for(refs: &[RefLabel]) -> Vec<(RefLabel, bool)> {
    let rank = |r: &RefLabel| match r.kind {
        _ if r.current => 0,
        RefKind::Head => 1,
        RefKind::Branch => 2,
        RefKind::Remote => 3,
        RefKind::Tag => 4,
        RefKind::Stash => 5,
    };
    // A remote's name may hold a slash, so it's the branch's name that's looked for at the end.
    let twin = |remote: &str| refs.iter().any(|r| r.kind == RefKind::Branch && remote.len() > r.name.len() && remote.ends_with(&r.name) && remote.as_bytes()[remote.len() - r.name.len() - 1] == b'/');
    let mut out: Vec<(RefLabel, bool)> = refs
        .iter()
        .filter(|r| !(r.kind == RefKind::Remote && (r.name.ends_with("/HEAD") || twin(&r.name))))
        .map(|r| {
            let has_twin = r.kind == RefKind::Branch && refs.iter().any(|o| o.kind == RefKind::Remote && o.name.ends_with(&format!("/{}", r.name)));
            (r.clone(), has_twin)
        })
        .collect();
    out.sort_by_key(|(r, _)| rank(r));
    out
}

#[cfg(test)]
#[test]
fn a_commits_badges_lead_with_the_current_branch_and_drop_the_noise() {
    let l = |kind, name: &str, current| RefLabel { kind, name: name.into(), current };
    let refs = [l(RefKind::Tag, "v1", false), l(RefKind::Remote, "origin/HEAD", false), l(RefKind::Remote, "origin/main", false), l(RefKind::Remote, "origin/other", false), l(RefKind::Branch, "side", false), l(RefKind::Branch, "main", true)];
    let got: Vec<(String, bool)> = badges_for(&refs).into_iter().map(|(r, t)| (r.name, t)).collect();
    assert_eq!(got, [("main".to_string(), true), ("side".to_string(), false), ("origin/other".to_string(), false), ("v1".to_string(), false)]);
}

fn build_query(f: &Filters, limit: usize, stash_shas: &[String], cur: Option<&str>) -> LogQuery {
    let mut q = LogQuery { limit, exclude_stash: true, no_merges: f.no_merges, first_parent: f.first_parent, date_order: true, ..Default::default() };
    if let Some(s) = &f.solo {
        q.revs.push(s.clone());
        return q;
    }
    if let Some(p) = &f.preset {
        q.revs = p.clone();
        return q;
    }
    if f.only_current {
        q.revs.push("HEAD".into());
        if let Some(c) = cur {
            q.revs.push(format!("{c}@{{upstream}}"));
        }
        return q;
    }
    if !f.remotes {
        q.excludes.push("refs/remotes/*".into());
    }
    if !f.tags {
        q.excludes.push("refs/tags/*".into());
    }
    for h in &f.hidden {
        q.excludes.push(format!("refs/heads/{h}"));
        q.excludes.push(format!("refs/remotes/{h}"));
    }
    if f.hidden.is_empty() {
        q.all = true;
    } else {
        // `--all` always brings HEAD back, so hiding the current branch would do nothing.
        q.all_refs_only = true;
        // A detached HEAD may be on no ref at all, and nobody can have hidden it by name.
        if cur.is_none() {
            q.revs.push("HEAD".into());
        }
    }
    if f.stashes {
        q.revs.extend(stash_shas.iter().cloned());
    }
    q
}

impl GraphView {
    pub fn key(&self) -> String {
        "graph".into()
    }
    pub fn title(&self) -> String {
        "Graph".into()
    }

    /// Show only this ref.
    pub fn focus_ref(&mut self, name: &str) {
        self.filters.solo = Some(name.to_string());
        self.filters.preset = None;
        self.pending_reload = true;
    }

    pub fn hide_ref(&mut self, name: &str) {
        if !self.filters.hidden.iter().any(|h| h == name) {
            self.filters.hidden.push(name.to_string());
        }
        self.pending_reload = true;
    }

    /// The commits picked with click, Ctrl+click or Shift+click.
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    /// Commits currently laid out.
    pub fn loaded_commits(&self) -> usize {
        self.data.get().map_or(0, |g| g.commits.len())
    }

    fn reload(&mut self, ctx: &egui::Context, cx: &Cx) {
        if self.limit == 0 {
            self.limit = cx.settings.graph_page_size.max(100);
        }
        let repo = cx.repo.clone();
        let stash_shas: Vec<String> = cx.data.stashes().iter().map(|s| s.sha.clone()).collect();
        let f = self.filters.clone();
        let limit = self.limit;
        let head = cx.status().map(|s| s.head_sha.clone()).unwrap_or_default();
        let branch = cx.current_branch();
        let dirty = cx.status().is_some_and(|s| !s.entries.is_empty());
        self.wip = dirty;
        self.data.spawn(ctx, move || -> Result<Graph, String> {
            let mut q = build_query(&f, limit, &stash_shas, branch.as_deref());
            let mut commits = match repo.log(&q) {
                Ok(c) => c,
                // A branch with no upstream makes the `@{upstream}` rev invalid.
                Err(_) if f.only_current => {
                    q.revs.truncate(1);
                    repo.log(&q).map_err(|e| e.to_string())?
                }
                Err(e) => return Err(e.to_string()),
            };
            let loaded = commits.len();
            let more = loaded >= limit;
            commits = graph::hide_stash_internals(commits, &stash_shas);
            for c in &mut commits {
                if stash_shas.contains(&c.sha) && !c.refs.iter().any(|r| r.kind == RefKind::Stash) {
                    c.refs.push(RefLabel { kind: RefKind::Stash, name: "stash".into(), current: false });
                }
            }
            // With commits filtered out, git still names their real parents, so what's drawn
            // has to be worked out. `commits` keeps the real ones, which menus act on.
            let hidden_merges = if f.no_merges { Some(repo.merge_first_parents(&q).map_err(|e| e.to_string())?) } else { None };
            if dirty && !head.is_empty() {
                // HEAD can be filtered out, and then this row has nothing to connect to.
                let shown = |sha: &str| commits.iter().any(|c| c.sha == sha);
                let target = hidden_merges.as_ref().and_then(|m| m.get(&head)).cloned().unwrap_or_else(|| head.clone());
                let parents = if shown(&head) || shown(&target) { vec![head] } else { Vec::new() };
                commits.insert(0, Commit { sha: WIP.into(), parents, subject: "Work in progress".into(), ..Default::default() });
            }
            let rows = if f.first_parent || f.no_merges { graph::layout(&graph::parents_for_filters(&commits, f.first_parent, hidden_merges.as_ref())) } else { graph::layout(&commits) };
            let max_width = rows.iter().map(|r| r.width).max().unwrap_or(1);
            let index = commits.iter().enumerate().map(|(i, c)| (c.sha.clone(), i)).collect();
            Ok(Graph { commits, rows, index, max_width, more, loaded })
        });
    }

    fn run_search(&mut self, ctx: &egui::Context, cx: &Cx) {
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.matches = Task::default();
            self.match_list.clear();
            return;
        }
        let opts = (self.match_all, self.match_case, self.regex);
        // A new search replaces the last one, whose git stops: `change:` can run for minutes.
        self.matches.spawn_git(ctx, cx.repo, move |repo| -> Result<HashSet<String>, String> {
            let mut lq = parse_search(&q);
            lq.limit = 5000;
            lq.all_match = opts.0;
            lq.regex_ignore_case = !opts.1;
            lq.fixed_strings = lq.fixed_strings && !opts.2;
            Ok(repo.log(&lq).map_err(|e| e.to_string())?.into_iter().map(|c| c.sha).collect())
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx) -> bool {
        let p = cx.pal.clone();
        let mut changed = false;
        // Wraps, or a narrow panel would be stretched and clip the rightmost columns.
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(crate::widgets::input(&mut self.query).hint_text("Search commits").desired_width(220.0)).on_hover_text("Words in the message, or @me, @:author, committer:, file:, change:, #:sha");
            if std::mem::take(&mut self.focus_search) {
                r.request_focus();
            }
            if toggle(ui, &mut self.match_all, "All", "Match all terms") | toggle(ui, &mut self.match_case, "Aa", "Match case") | toggle(ui, &mut self.regex, ".*", "Regular expression") {
                self.run_search(ui.ctx(), cx);
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.run_search(ui.ctx(), cx);
            }
            if self.matches.busy() {
                ui.add(spinner(14.0));
                if ui.small_button("Stop").on_hover_text("Stop this search").clicked() {
                    self.matches.stop();
                }
            }
            if let Some(e) = &self.matches.error {
                let first = e.lines().next().unwrap_or(e).trim_start_matches("fatal: ");
                ui.label(RichText::new(format!("Search failed: {first}")).color(p.danger));
            }
            if !self.match_list.is_empty() || self.matches.get().is_some() {
                ui.label(RichText::new(format!("{} of {}", if self.match_list.is_empty() { 0 } else { self.match_pos + 1 }, self.match_list.len())).color(p.dim));
                if small_icon_button(ui, icon::CARET_UP, "Previous match", "Shift+F3").clicked() && !self.match_list.is_empty() {
                    self.match_pos = (self.match_pos + self.match_list.len() - 1) % self.match_list.len();
                    self.scroll_to = Some(self.match_list[self.match_pos]);
                }
                if small_icon_button(ui, icon::CARET_DOWN, "Next match", "F3").clicked() && !self.match_list.is_empty() {
                    self.match_pos = (self.match_pos + 1) % self.match_list.len();
                    self.scroll_to = Some(self.match_list[self.match_pos]);
                }
                toggle(ui, &mut self.filter_to_matches, "Only matches", "Hide commits that don't match");
                if small_icon_button(ui, icon::X, "Clear search", "").clicked() {
                    self.query.clear();
                    self.matches = Task::default();
                    self.match_list.clear();
                    self.filter_to_matches = false;
                }
            }
            ui.separator();
            // Everything that narrows the list, with a count so it's clear why commits are missing.
            let f = &mut self.filters;
            let on = [f.only_current, f.preset.is_some(), !f.remotes, !f.tags, !f.stashes, f.no_merges, f.first_parent, !f.hidden.is_empty(), f.solo.is_some()].iter().filter(|x| **x).count();
            let title = if on > 0 { format!("Filters ({on}) {}", icon::CARET_DOWN) } else { format!("Filters {}", icon::CARET_DOWN) };
            let keep_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
            egui::containers::menu::MenuButton::new(title).config(keep_open).ui(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(RichText::new("Which branches").small().color(p.dim));
                changed |= toggle(ui, &mut f.only_current, "Current", "Only the current branch and its upstream");
                let mut smart = f.preset.is_some();
                if toggle(ui, &mut smart, "Smart", "Current branch, its upstream, the default branch, and favorites") {
                    f.preset = smart.then(|| {
                        let mut v = vec!["HEAD".to_string()];
                        if let Some(u) = cx.status().and_then(|s| s.upstream.clone()) {
                            v.push(u);
                        }
                        let base = cx.data.base_branch();
                        if cx.data.branches().iter().any(|b| b.name == base) {
                            v.push(base);
                        }
                        v.extend(cx.settings.favorite_branches.iter().cloned());
                        v
                    });
                    changed = true;
                }
                ui.menu_button("Branches", |ui| {
                    ui.set_max_height(400.0);
                    if ui.button("Show all").clicked() {
                        f.hidden.clear();
                        f.solo = None;
                        f.preset = None;
                        changed = true;
                    }
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for b in cx.data.branches() {
                            ui.horizontal(|ui| {
                                let mut vis = !f.hidden.contains(&b.name);
                                if ui.checkbox(&mut vis, b.name.as_str()).changed() {
                                    if vis {
                                        f.hidden.retain(|h| *h != b.name);
                                    } else {
                                        f.hidden.push(b.name.clone());
                                    }
                                    f.solo = None;
                                    changed = true;
                                }
                                if ui.small_button("solo").on_hover_text("Show only this branch").clicked() {
                                    f.solo = Some(b.name.clone());
                                    changed = true;
                                }
                            });
                        }
                    });
                });
                ui.separator();
                ui.label(RichText::new("What to show").small().color(p.dim));
                changed |= toggle(ui, &mut f.remotes, "Remotes", "Show remote branches");
                changed |= toggle(ui, &mut f.tags, "Tags", "Show tags");
                changed |= toggle(ui, &mut f.stashes, "Stashes", "Show stashes");
                changed |= toggle(ui, &mut f.no_merges, "No merges", "Hide merge commits");
                changed |= toggle(ui, &mut f.first_parent, "First parent", "Follow only first parents");
            });
            if self.filters.solo.is_some() {
                let s = self.filters.solo.clone().unwrap_or_default();
                chip(ui, &format!("only {s}"), p.accent);
            }
            let mut jump = None;
            ui.menu_button(format!("Jump {}", icon::CARET_DOWN), |ui| {
                if ui.button("HEAD").on_hover_text("Jump to the current commit").clicked() {
                    jump = Some("HEAD".to_string());
                    ui.close();
                }
                if let Some(u) = cx.status().and_then(|s| s.upstream.clone())
                    && ui.button("Upstream").on_hover_text(format!("Jump to {u}")).clicked() {
                        jump = Some(u);
                        ui.close();
                    }
                if ui.button("Find ref…").on_hover_text("Find a branch, tag, or worktree (/)").clicked() {
                    self.finder = Some(String::new());
                    ui.close();
                }
            });
            if let Some(name) = jump {
                self.jump_to_ref(&name, cx);
            }
            ui.menu_button(format!("Columns {}", icon::CARET_DOWN), |ui| {
                self.cols.changes = cx.settings.graph_changes_column;
                self.cols.menu(ui, cx);
                ui.separator();
                ui.checkbox(&mut self.show_minimap, "Minimap");
                ui.checkbox(&mut self.minimap_lines, "Minimap shows lines changed");
                if !self.folded.is_empty() && ui.button("Unfold all lanes").clicked() {
                    self.folded.clear();
                    ui.close();
                }
                let mut dim = cx.settings.graph_dim_merges;
                if ui.checkbox(&mut dim, "Dim merge commits").changed() {
                    cx.out.push(crate::action::Action::Settings(Box::new(move |s| s.graph_dim_merges = dim)));
                }
            });
            if self.data.busy() {
                ui.add(spinner(14.0));
            }
        });
        changed
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        if self.data.poll() {
            self.recompute_matches();
            // HEAD is what anyone opening this wants to know about. Once: after that, nothing
            // selected means the user cleared it.
            if !std::mem::replace(&mut self.head_shown, true)
                && cx.settings.details_on_open
                && let Some(head) = cx.status().map(|s| s.head_sha.clone()).filter(|h| !h.is_empty())
            {
                self.selected = vec![head.clone()];
                self.last_clicked = Some(head.clone());
                cx.out.push(crate::action::Action::InspectFirst(head));
            }
        }
        if self.matches.poll() {
            // A failed search has no results, and the last one's highlights aren't its answer.
            if self.matches.error.is_some() {
                self.matches.value = None;
            }
            self.recompute_matches();
            self.match_pos = 0;
            self.scroll_to = self.match_list.first().copied();
        }
        // The WIP row only depends on dirtiness, so plain status churn doesn't reload.
        let dirty = cx.status().is_some_and(|s| !s.entries.is_empty());
        let status_changed = self.loaded_status_gen != cx.data.status_gen && (dirty != self.wip);
        // The first load waits for what it depends on: stashes, HEAD, and whether the tree is
        // dirty. Otherwise each of those arriving reloads the whole graph.
        let ready = (cx.data.refs.get().is_some() || cx.data.refs.error.is_some()) && (cx.status().is_some() || cx.data.status.error.is_some());
        if ready && (self.loaded_gen != cx.data.refs_gen || status_changed) {
            self.loaded_gen = cx.data.refs_gen;
            self.loaded_status_gen = cx.data.status_gen;
            self.reload(&ctx, cx);
        }

        if !self.layout_applied {
            self.layout_applied = true;
            if let Some(l) = &cx.settings.graph_layout {
                self.cols.apply(l);
            }
        }
        match self.fold_request.take() {
            Some(Some(c)) => {
                if !self.folded.remove(&c) {
                    self.folded.insert(c);
                }
            }
            Some(None) => self.folded.clear(),
            None => {}
        }
        let changed = self.toolbar(ui, cx);
        self.worktree_pills(ui, cx);
        if changed || std::mem::take(&mut self.pending_reload) {
            self.reload(&ctx, cx);
        }
        self.egui_ctx = ctx.clone();
        self.land_jump(cx);
        self.keys(ui, cx);
        self.ref_finder(ui, cx);
        self.stats.poll();
        if (cx.settings.graph_changes_column || self.minimap_lines) && self.stats_gen != Some(cx.data.refs_gen) {
            self.stats_gen = Some(cx.data.refs_gen);
            let (r, limit) = (cx.repo.clone(), self.limit.max(1));
            self.stats.spawn(&ctx, move || r.commit_stats(&[], true, limit));
        }
        ui.add_space(2.0);

        if let Some(e) = &self.data.error {
            error_box(ui, e, cx.pal);
        }
        let Some(g) = self.data.value.take() else {
            spinner_row(ui, "Loading commits…", cx.pal);
            return;
        };
        // A repository with no commits has no columns worth showing.
        if g.commits.is_empty() && cx.status().is_some_and(|st| st.head_sha.is_empty()) {
            empty_state(ui, "No commits yet", "Add files to this folder, then commit them. Publishing needs a remote.", cx.pal);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                    if ui.button("Open Changes").clicked() {
                        cx.open(Tab::changes());
                    }
                    if ui.button("Add remote…").clicked() {
                        cx.form(forms::add_remote());
                    }
                });
            });
            self.data.value = Some(g);
            return;
        }
        self.table(ui, cx, &g);
        self.data.value = Some(g);
    }

    /// One pill per other worktree, with its uncommitted-change count.
    fn worktree_pills(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let trees = cx.data.worktrees().to_vec();
        if trees.len() < 2 {
            return;
        }
        self.worktree_counts.poll();
        if self.counts_gen != Some(cx.data.status_gen) && !self.worktree_counts.busy() {
            self.counts_gen = Some(cx.data.status_gen);
            let git = cx.repo.git.clone();
            let paths: Vec<String> = trees.iter().filter(|w| !w.bare && w.prunable.is_none()).map(|w| w.path.clone()).collect();
            self.worktree_counts.spawn(ui.ctx(), move || -> Result<HashMap<String, usize>, String> {
                Ok(paths.into_iter().filter_map(|p| gitem_core::Repo::open(git.clone(), std::path::Path::new(&p)).and_then(|r| r.status()).ok().map(|s| (p, s.entries.len()))).collect())
            });
        }
        let mut focus = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Worktrees").small().color(cx.pal.faint));
            for w in &trees {
                let name = w.branch.clone().unwrap_or_else(|| "detached".to_string());
                let n = self.worktree_counts.get().and_then(|m| m.get(&w.path)).copied().unwrap_or(0);
                let text = if n > 0 { format!("{name} ({n})") } else { name.clone() };
                if ui.selectable_label(w.is_current, RichText::new(text).small()).on_hover_text(format!("{}\nClick to focus the graph on it", w.path)).clicked() {
                    // "detached" is only a label: what it's on is a commit.
                    focus = Some(w.branch.clone().unwrap_or_else(|| w.head.clone()));
                }
            }
        });
        if let Some(n) = focus {
            self.focus_ref(&n);
        }
    }

    fn jump_to_ref(&mut self, name: &str, cx: &mut Cx) {
        let name = name.to_string();
        self.jump.spawn_git(&self.egui_ctx, cx.repo, move |r| r.resolve(&name).map(|sha| (name, sha)));
    }

    fn land_jump(&mut self, cx: &mut Cx) {
        if !self.jump.poll() {
            return;
        }
        self.jump.error = None;
        let Some((name, sha)) = self.jump.value.take() else { return };
        if sha.is_empty() {
            return;
        }
        let name = name.as_str();
        match self.data.get().and_then(|g| g.index.get(&sha).copied()) {
            Some(i) => {
                self.scroll_to = Some(i);
                self.selected = vec![sha.clone()];
                self.last_clicked = Some(sha.clone());
                cx.inspect(&sha);
            }
            None => cx.toast(format!("{name} isn't among the commits showing. A filter may be hiding it.")),
        }
    }

    fn keys(&mut self, ui: &Ui, cx: &mut Cx) {
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        let bound = &cx.settings.key_bindings;
        let (find, slash, next, prev) = ui.input(|i| (crate::keys::context_pressed(i, bound, "find"), crate::keys::context_pressed(i, bound, "graph_find_ref"), crate::keys::context_pressed(i, bound, "find_next"), crate::keys::context_pressed(i, bound, "find_prev")));
        if find {
            self.focus_search = true;
        }
        if slash && !typing {
            self.finder = Some(String::new());
        }
        if (next || prev) && !self.match_list.is_empty() {
            let n = self.match_list.len();
            self.match_pos = if next { (self.match_pos + 1) % n } else { (self.match_pos + n - 1) % n };
            self.scroll_to = Some(self.match_list[self.match_pos]);
        }
        let _ = cx;
    }

    fn ref_finder(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let Some(mut q) = self.finder.take() else { return };
        let mut close = false;
        let mut pick = None;
        egui::Frame::new().fill(cx.pal.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
            let r = ui.add(crate::widgets::input(&mut q).hint_text("Jump to a branch, tag, or worktree").desired_width(f32::INFINITY));
            r.request_focus();
            let mut names = cx.data.ref_names();
            names.extend(cx.data.worktrees().iter().filter_map(|w| w.branch.clone()));
            names.dedup();
            let mut scored: Vec<(i32, String)> = names.into_iter().filter_map(|n| gitem_core::util::fuzzy_score(&q, &n).map(|s| (s, n))).collect();
            scored.sort_by_key(|x| std::cmp::Reverse(x.0));
            for (i, (_, n)) in scored.iter().take(8).enumerate() {
                if ui.selectable_label(i == 0, n.as_str()).clicked() {
                    pick = Some(n.clone());
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                pick = scored.first().map(|x| x.1.clone());
            }
            close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        if let Some(n) = pick {
            self.jump_to_ref(&n, cx);
            close = true;
        }
        if !close {
            self.finder = Some(q);
        }
    }

    fn recompute_matches(&mut self) {
        let (Some(g), Some(m)) = (self.data.get(), self.matches.get()) else {
            self.match_list.clear();
            return;
        };
        self.match_list = g.commits.iter().enumerate().filter(|(_, c)| m.contains(&c.sha)).map(|(i, _)| i).collect();
        if self.match_pos >= self.match_list.len() {
            self.match_pos = 0;
        }
    }

    fn table(&mut self, ui: &mut Ui, cx: &mut Cx, g: &Graph) {
        let p = cx.pal.clone();
        let total_w = ui.available_width().min(ui.clip_rect().right() - ui.cursor().left());
        let minimap_w = if self.show_minimap { 14.0 } else { 0.0 };
        let w = total_w - minimap_w;
        // Folded lanes stay visible as thin lines, so history remains connected.
        let mut lane_offsets: Vec<f32> = Vec::with_capacity(g.max_width as usize + 1);
        let mut lane_acc = 10.0;
        for c in 0..=g.max_width {
            lane_offsets.push(lane_acc);
            lane_acc += if self.folded.contains(&c) { FOLDED_W } else { LANE_W };
        }
        let show_changes = cx.settings.graph_changes_column;
        self.cols.changes = show_changes;
        let cols = self.cols.layout(w, lane_acc.clamp(60.0, 260.0));
        let at = |key: &str| cols.iter().find(|c| c.key == key).map_or((0.0, 0.0), |c| (c.x, c.w));
        let ((x_refs, refs_w), (x_graph, graph_w), (x_msg, msg_w)) = (at("refs"), at("graph"), at("message"));
        let ((x_changes, changes_w), (x_author, author_w), (x_date, date_w), (x_sha, sha_w)) = (at("changes"), at("author"), at("date"), at("sha"));
        let (show_refs, show_author, show_date, show_sha) = (refs_w > 0.0, author_w > 0.0, date_w > 0.0, sha_w > 0.0);

        let (hdr, _) = ui.allocate_exact_size(vec2(total_w, 22.0), Sense::hover());
        ui.painter().rect_filled(hdr, 0.0, p.panel2);
        self.cols.header(ui, hdr, &cols, &p, cx);

        if g.more {
            let step = cx.settings.graph_page_size.max(100);
            if let Some(choice) = load_more_bar(ui, "graph-load-more", g.loaded, "commits", step, self.data.busy(), &p) {
                self.limit = choice.apply(self.limit, step);
                // The Changes column and minimap need figures for the new commits too.
                self.stats_gen = None;
                self.reload(&ui.ctx().clone(), cx);
            }
        }

        // Row order: all commits, or only matches when filtering.
        let visible: Vec<usize> = if self.filter_to_matches && self.matches.get().is_some() { self.match_list.clone() } else { (0..g.commits.len()).collect() };
        let match_set: Option<&HashSet<String>> = self.matches.get();

        let area_h = ui.available_height();
        let area_top = ui.cursor().top();
        let mut sa = egui::ScrollArea::vertical().id_salt("graph-rows").auto_shrink([false, false]).max_width(total_w - minimap_w);
        if let Some(target) = self.scroll_to.take() {
            let pos = visible.iter().position(|&v| v == target).unwrap_or(0);
            // Rows sit edge to edge: the spacing set for them below is zero.
            sa = sa.vertical_scroll_offset((pos as f32 * ROW_H - area_h / 3.0).max(0.0));
        }
        if let Some(target) = self.reveal.take()
            && let Some(pos) = visible.iter().position(|&v| v == target)
        {
            let top = pos as f32 * ROW_H;
            if top < self.last_scroll_y {
                sa = sa.vertical_scroll_offset(top);
            } else if top + ROW_H > self.last_scroll_y + area_h {
                sa = sa.vertical_scroll_offset(top + ROW_H - area_h);
            }
        }
        let mut clicked_row: Option<(usize, egui::Modifiers)> = None;
        let mut scroll_y = 0.0;

        let out = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            sa.show_rows(ui, ROW_H, visible.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for vi in range {
                    let i = visible[vi];
                    let c = &g.commits[i];
                    let r = &g.rows[i];
                    let is_wip = c.sha == WIP;
                    let selected = self.selected.contains(&c.sha);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, c.subject.as_str()));
                    let hovered = resp.hovered();
                    let is_match = match_set.is_some_and(|m| m.contains(&c.sha));
                    let bg = if selected {
                        p.selection
                    } else if is_match {
                        p.warn.gamma_multiply(0.16)
                    } else if hovered {
                        p.hover
                    } else {
                        Color32::TRANSPARENT
                    };
                    if bg != Color32::TRANSPARENT {
                        ui.painter().rect_filled(rect, 0.0, bg);
                    }

                    let lane_x = |col: u16| rect.left() + x_graph + lane_offsets[(col as usize).min(lane_offsets.len() - 1)];
                    let graph_clip = Rect::from_min_size(pos2(rect.left() + x_graph, rect.top()), vec2(graph_w, ROW_H));
                    let painter = ui.painter().with_clip_rect(graph_clip.intersect(ui.clip_rect()));
                    let (top, mid, bot) = (rect.top(), rect.center().y, rect.bottom());
                    let draw = |from: Pos2, to: Pos2, color: Color32| {
                        let s = Stroke::new(1.6, color);
                        if (from.x - to.x).abs() < 0.5 {
                            painter.line_segment([from, to], s);
                        } else {
                            let dy = (to.y - from.y) * 0.5;
                            let b = CubicBezierShape::from_points_stroke([from, pos2(from.x, from.y + dy), pos2(to.x, to.y - dy), to], false, Color32::TRANSPARENT, s);
                            painter.add(Shape::CubicBezier(b));
                        }
                    };
                    for e in &r.top {
                        draw(pos2(lane_x(e.from), top), pos2(lane_x(e.to), mid), p.lane(e.color));
                    }
                    for e in &r.bottom {
                        draw(pos2(lane_x(e.from), mid), pos2(lane_x(e.to), bot), p.lane(e.color));
                    }
                    let node = pos2(lane_x(r.column), mid);
                    let node_color = p.lane(r.color);
                    if is_wip {
                        painter.circle(node, 5.0, p.bg, Stroke::new(1.5, p.warn));
                    } else if c.is_merge() {
                        let is_head = c.refs.iter().any(|x| x.current);
                        painter.circle(node, 3.5, node_color, Stroke::new(if is_head { 2.0 } else { 0.0 }, p.text));
                    } else {
                        let is_head = c.refs.iter().any(|x| x.current);
                        painter.circle(node, 5.0, node_color, Stroke::new(if is_head { 2.0 } else { 1.0 }, if is_head { p.text } else { p.bg }));
                    }

                    // Ref badges, draggable onto other rows.
                    if show_refs && !c.refs.is_empty() {
                        let clip = Rect::from_min_size(pos2(rect.left() + x_refs, rect.top()), vec2(refs_w - 4.0, ROW_H));
                        let mut x = clip.left() + 4.0;
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(clip));
                        child.set_clip_rect(clip.intersect(ui.clip_rect()));
                        let badges = badges_for(&c.refs);
                        for (shown_at, (lab, both)) in badges.iter().enumerate() {
                            // The last one fits without room for a count after it.
                            let left = badges.len() - shown_at - 1;
                            let needs = badge_width(ui, lab) + if left > 0 { 30.0 } else { 0.0 };
                            if x + needs > clip.right() && shown_at > 0 {
                                paint_text(&child, Rect::from_min_size(pos2(x + 1.0, rect.top()), vec2(clip.right() - x, ROW_H)), &format!("+{}", badges.len() - shown_at), prop(11.0), p.dim);
                                resp.clone().on_hover_ui(|ui| {
                                    for l in &c.refs {
                                        ui.label(l.name.as_str());
                                    }
                                });
                                break;
                            }
                            // With its remote twin at the same commit, it says so and takes one badge.
                            let shown = RefLabel { name: if *both { format!("{} {}", lab.name, icon::CLOUD_CHECK) } else { lab.name.clone() }, ..lab.clone() };
                            let br = paint_badge(&child, pos2(x, rect.top() + 3.0), ROW_H - 6.0, &shown, &p);
                            let id = ui.id().with(("badge", i, &lab.name));
                            let bresp = ui.interact(br.intersect(clip), id, Sense::drag());
                            if lab.kind != RefKind::Stash {
                                bresp.dnd_set_drag_payload(lab.name.clone());
                            }
                            if bresp.dragged() {
                                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), id.with("drag"), egui::PopupAnchor::Pointer).show(|ui| {
                                    ui.label(format!("Drop {} on a commit to merge or rebase", lab.name));
                                });
                            }
                            x = br.right() + 3.0;
                        }
                    }

                    let text_col = if is_wip {
                        p.warn
                    } else if c.is_merge() {
                        if cx.settings.graph_dim_merges { p.faint } else { p.dim }
                    } else {
                        p.text
                    };
                    if let Some((a, d)) = show_changes.then(|| self.stats.get().and_then(|s| s.get(&c.sha).copied())).flatten() {
                        let cr = Rect::from_min_size(pos2(rect.left() + x_changes + 6.0, rect.top()), vec2(changes_w - 8.0, ROW_H));
                        paint_text(ui, cr, &format!("+{a}"), mono(11.0), p.add_fg);
                        paint_text(ui, Rect::from_min_size(cr.min + vec2(42.0, 0.0), vec2((changes_w - 50.0).max(0.0), ROW_H)), &format!("-{d}"), mono(11.0), p.del_fg);
                    }
                    let mr = Rect::from_min_size(pos2(rect.left() + x_msg + 6.0, rect.top()), vec2(msg_w - 10.0, ROW_H));
                    paint_text(ui, mr, &c.subject, prop(13.0), text_col);
                    if !is_wip {
                        if show_author {
                            let cy = rect.center().y;
                            paint_avatar(ui, pos2(rect.left() + x_author + 15.0, cy), 8.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
                            paint_text(ui, Rect::from_min_size(pos2(rect.left() + x_author + 27.0, rect.top()), vec2((author_w - 30.0).max(0.0), ROW_H)), &c.author_name, prop(12.0), p.dim);
                        }
                        if show_date {
                            let t = time_label(c.commit_time, cx.settings.date_relative);
                            paint_text(ui, Rect::from_min_size(pos2(rect.left() + x_date + 6.0, rect.top()), vec2(date_w - 8.0, ROW_H)), &t, prop(12.0), p.dim);
                        }
                        if show_sha {
                            paint_text(ui, Rect::from_min_size(pos2(rect.left() + x_sha + 6.0, rect.top()), vec2(sha_w - 8.0, ROW_H)), c.short(), mono(11.5), p.faint);
                        }
                    } else if let Some(s) = cx.status().filter(|_| show_author) {
                        paint_text(ui, Rect::from_min_size(pos2(rect.left() + x_author + 6.0, rect.top()), vec2(author_w - 8.0, ROW_H)), &format!("{} file(s)", s.entries.len()), prop(12.0), p.dim);
                    }

                    if resp.clicked() {
                        clicked_row = Some((i, ui.input(|x| x.modifiers)));
                    }
                    if resp.double_clicked() && !is_wip {
                        cx.diff(gitem_core::repo::DiffSource::Commit(c.sha.clone()), None);
                    }
                    if !is_wip {
                        let resp = resp.on_hover_ui(|ui| {
                            commit_card(ui, &c.sha, &c.author_name, &c.author_email, c.author_time, &c.subject, &p, cx.settings.gravatar);
                        });
                        let by_key = self.menu_key && self.selected.last() == Some(&c.sha);
                        keyed_context_menu(&resp, by_key, |ui| {
                            self.row_menu(ui, cx, c, g);
                            ui.separator();
                            let folded = self.folded.contains(&r.column);
                            if ui.button(if folded { "Unfold this lane" } else { "Fold this lane" }).clicked() {
                                self.fold_request.set(Some(Some(r.column)));
                                ui.close();
                            }
                            if !self.folded.is_empty() && ui.button("Unfold all lanes").clicked() {
                                self.fold_request.set(Some(None));
                                ui.close();
                            }
                        });
                        // Drop a dragged ref on this commit.
                        if let Some(name) = resp.dnd_release_payload::<String>() {
                            drop_ref(cx, &name, c);
                        } else if resp.dnd_hover_payload::<String>().is_some() {
                            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
                        }
                    } else {
                        resp.context_menu(|ui| {
                            if ui.button("Open Changes").clicked() {
                                cx.open(Tab::changes());
                                ui.close();
                            }
                            if ui.button("Stash…").clicked() {
                                cx.form(forms::stash_form(Vec::new()));
                                ui.close();
                            }
                        });
                    }
                }
            })
        });
        scroll_y += out.inner.state.offset.y;
        self.last_scroll_y = scroll_y;
        let content_h = visible.len() as f32 * ROW_H;

        if let Some((i, mods)) = clicked_row {
            self.on_click(i, mods, g, cx);
        }
        self.keyboard(ui, g, cx, &visible);

        // Minimap: commit density over the loaded history, with markers.
        let map = Rect::from_min_size(pos2(hdr.right() - minimap_w, area_top), vec2(minimap_w, area_h));
        if self.show_minimap {
            self.minimap(ui, map, &visible, g, content_h, scroll_y, area_h, cx);
        }
    }

    fn row_menu(&self, ui: &mut Ui, cx: &mut Cx, c: &Commit, g: &Graph) {
        if self.selected.len() == 2 && self.selected.contains(&c.sha) {
            if ui.button("Compare the two selected commits").clicked() {
                cx.open(Tab::compare(&self.selected[1], &self.selected[0]));
                ui.close();
            }
            ui.separator();
        }
        if self.selected.len() > 1 && self.selected.contains(&c.sha) && ui.button(format!("Cherry-pick {} selected commits…", self.selected.len())).clicked() {
            // Oldest first, so picks apply in history order.
            let mut shas = self.selected.clone();
            shas.sort_by_key(|s| std::cmp::Reverse(g.index.get(s).copied().unwrap_or(0)));
            cx.form(forms::cherry_pick_form(shas));
            ui.close();
        }
        if self.selected.len() > 1 && self.selected.contains(&c.sha) && ui.button(format!("Revert {} selected commits…", self.selected.len())).clicked() {
            // Newest first: what came later comes off before what it builds on.
            let mut shas = self.selected.clone();
            shas.sort_by_key(|s| g.index.get(s).copied().unwrap_or(0));
            cx.form(forms::revert_many_form(shas));
            ui.close();
        }
        // Branch-specific commands for refs on this commit. A commit can have dozens, so past a
        // few they go in a list of their own, or the menu outgrows the window. Not one that
        // scrolls: egui shuts the whole menu on a press on a submenu's row in a scroll area. So
        // that list stops at what fits on a small window and says how many are left.
        const LIST_ROWS: usize = 12;
        let ref_entries = |ui: &mut Ui, cx: &mut Cx, limit: usize| {
            let branches = c.refs.iter().filter(|r| matches!(r.kind, RefKind::Branch | RefKind::Remote));
            let tags = c.refs.iter().filter(|r| r.kind == RefKind::Tag);
            for r in branches.chain(tags).take(limit) {
                if r.kind == RefKind::Tag {
                    if let Some(t) = cx.data.tags().iter().find(|t| t.name == r.name).cloned() {
                        ui.menu_button(format!("Tag {}", r.name), |ui| menus::tag_menu(ui, cx, &t));
                    }
                } else if let Some(b) = cx.data.branches().iter().find(|b| b.name == r.name).cloned() {
                    ui.menu_button(format!("Branch {}", r.name), |ui| menus::branch_menu(ui, cx, &b));
                }
            }
        };
        if c.refs.len() > 3 {
            ui.menu_button(format!("{} here", plural(c.refs.len(), "ref", "refs")), |ui| {
                ref_entries(ui, cx, LIST_ROWS);
                if c.refs.len() > LIST_ROWS {
                    ui.separator();
                    ui.label(egui::RichText::new(format!("And {} more: filter the sidebar for them", c.refs.len() - LIST_ROWS)).small().color(cx.pal.dim));
                }
            });
        } else {
            ref_entries(ui, cx, usize::MAX);
        }
        if let Some(s) = cx.data.stashes().iter().find(|s| s.sha == c.sha).cloned() {
            ui.menu_button(format!("Stash {}", s.selector), |ui| menus::stash_menu(ui, cx, &s));
            ui.separator();
        } else if !c.refs.is_empty() {
            ui.separator();
        }
        if self.selected.len() > 1 && self.selected.contains(&c.sha) && ui.button("Changelog with AI for selection").clicked() {
            let text: String = g.commits.iter().filter(|x| self.selected.contains(&x.sha)).map(|x| format!("- {}\n", x.message().replace('\n', " "))).collect();
            crate::views::textview::ai_tab(cx, "Changelog", move |_| Ok(gitem_core::ai::changelog_prompt(&text)));
            ui.close();
        }
        menus::commit_menu(ui, cx, c);
    }

    fn on_click(&mut self, i: usize, mods: egui::Modifiers, g: &Graph, cx: &mut Cx) {
        let sha = g.commits[i].sha.clone();
        if sha == WIP {
            self.selected.clear();
            cx.open(Tab::changes());
            return;
        }
        if mods.ctrl || mods.command {
            if let Some(pos) = self.selected.iter().position(|s| *s == sha) {
                self.selected.remove(pos);
            } else {
                self.selected.push(sha.clone());
            }
        } else if mods.shift {
            // The anchor may be gone: filtered out, or dropped by a reset or a rebase.
            match self.last_clicked.as_ref().and_then(|s| g.index.get(s)) {
                Some(&a) => {
                    let (lo, hi) = (a.min(i), a.max(i));
                    self.selected = (lo..=hi).map(|k| g.commits[k].sha.clone()).filter(|s| s != WIP).collect();
                }
                None => self.selected = vec![sha.clone()],
            }
        } else {
            self.selected = vec![sha.clone()];
        }
        self.last_clicked = Some(sha.clone());
        cx.inspect(&sha);
    }

    fn keyboard(&mut self, ui: &Ui, g: &Graph, cx: &mut Cx, visible: &[usize]) {
        self.menu_key = !cx.dialog_open && cx.in_front && ui.ctx().memory(|m| m.focused().is_none()) && ui.input_mut(|i| crate::keys::context_take(i, &cx.settings.key_bindings, "row_menu"));
        if !cx.in_front || ui.ctx().memory(|m| m.focused().is_some()) || visible.is_empty() {
            return;
        }
        let bound = &cx.settings.key_bindings;
        let (down, up) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown) || crate::keys::context_pressed(i, bound, "graph_next"), i.key_pressed(egui::Key::ArrowUp) || crate::keys::context_pressed(i, bound, "graph_prev")));
        if !down && !up {
            return;
        }
        let cur = self.selected.last().and_then(|s| g.index.get(s)).and_then(|i| visible.iter().position(|v| v == i));
        let next = match cur {
            Some(c) if down => (c + 1).min(visible.len() - 1),
            Some(c) => c.saturating_sub(1),
            // The first commit, which isn't the first row with uncommitted changes:
            // the working-changes row can't be selected.
            None => visible.iter().position(|i| g.commits[*i].sha != WIP).unwrap_or(0),
        };
        let i = visible[next];
        if g.commits[i].sha != WIP {
            self.selected = vec![g.commits[i].sha.clone()];
            self.last_clicked = Some(g.commits[i].sha.clone());
            self.reveal = Some(i);
            cx.inspect(&g.commits[i].sha);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn minimap(&mut self, ui: &mut Ui, map: Rect, visible: &[usize], g: &Graph, content_h: f32, scroll_y: f32, area_h: f32, cx: &Cx) {
        let p = cx.pal;
        ui.painter().rect_filled(map, 0.0, p.panel2);
        if visible.is_empty() {
            return;
        }
        let n = visible.len() as f32;
        let buckets = (map.height() / 2.0).max(1.0) as usize;
        let mut counts = vec![0u32; buckets];
        let stats = self.minimap_lines.then(|| self.stats.get()).flatten();
        for (vi, &i) in visible.iter().enumerate() {
            let b = ((vi as f32 / n) * buckets as f32) as usize;
            let weight = match stats {
                Some(s) => s.get(&g.commits[i].sha).map_or(0, |(a, d)| a + d),
                None => 1,
            };
            counts[b.min(buckets - 1)] += weight;
        }
        let max = counts.iter().copied().max().unwrap_or(1).max(1) as f32;
        for (b, c) in counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let y = map.top() + b as f32 * 2.0;
            let width = (*c as f32 / max) * (map.width() - 4.0);
            ui.painter().rect_filled(Rect::from_min_size(pos2(map.left() + 2.0, y), vec2(width.max(1.0), 2.0)), 0.0, p.heat(0.35));
        }
        let mark = |vi: usize, color: Color32, ui: &Ui| {
            let y = map.top() + (vi as f32 / n) * map.height();
            ui.painter().line_segment([pos2(map.left(), y), pos2(map.right(), y)], Stroke::new(1.5, color));
        };
        for (vi, &i) in visible.iter().enumerate() {
            let c = &g.commits[i];
            if c.refs.iter().any(|r| r.current) {
                mark(vi, p.accent, ui);
            } else if self.matches.get().is_some_and(|m| m.contains(&c.sha)) {
                mark(vi, p.warn, ui);
            } else if let Some(r) = c.refs.first() {
                mark(vi, ref_color(r.kind, false, p).gamma_multiply(0.8), ui);
            }
        }
        // Viewport indicator
        if content_h > 0.0 {
            let y0 = map.top() + (scroll_y / content_h).clamp(0.0, 1.0) * map.height();
            let h = (area_h / content_h).clamp(0.02, 1.0) * map.height();
            ui.painter().rect_stroke(Rect::from_min_size(pos2(map.left(), y0), vec2(map.width(), h)), 0.0, Stroke::new(1.0, p.dim), egui::StrokeKind::Inside);
        }
        let resp = ui.interact(map, ui.id().with("minimap"), Sense::click_and_drag());
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos() {
                let t = ((pos.y - map.top()) / map.height()).clamp(0.0, 1.0);
                self.scroll_to = visible.get((t * n) as usize).copied();
            }
    }
}

#[derive(Clone, Copy)]
enum DropChoice {
    Merge,
    Rebase,
    Compare,
    Move,
    Reset,
}

fn drop_ref(cx: &mut Cx, name: &str, target: &Commit) {
    let short = target.short().to_string();
    let target_sha = target.sha.clone();
    let name_s = name.to_string();
    let is_local = cx.data.local_branches().any(|b| b.name == name);
    let is_current = cx.current_branch().as_deref() == Some(name);
    let mut f = forms::FormDialog::new(format!("{name} → {short}"), "Run");
    let mut options = vec![(DropChoice::Merge, "Merge the dragged ref into the current branch"), (DropChoice::Rebase, "Rebase the current branch onto this commit"), (DropChoice::Compare, "Compare the dragged ref with this commit")];
    if is_local && !is_current {
        options.push((DropChoice::Move, "Move the dragged branch to this commit"));
    }
    if is_local && is_current {
        options.push((DropChoice::Reset, "Reset the current branch to this commit"));
    }
    f = f.choice_of("what", "What should happen?", &options, 0).on_submit(move |v| {
        use crate::action::Action;
        Ok(vec![match v.picked("what", &options) {
            DropChoice::Merge => Action::Form(forms::merge_form(name_s.clone())),
            DropChoice::Rebase => Action::Form(forms::rebase_form(target_sha.clone())),
            DropChoice::Compare => Action::OpenTab(Tab::compare(&name_s, &target_sha)),
            DropChoice::Move => Action::Confirm {
                title: "Move branch".into(),
                body: format!("Move {name_s} to {}? Commits only reachable from its old position may be lost.", gitem_core::util::prefix(&target_sha, 8)),
                label: "Move".into(),
                op: ops::branch_force_move(&name_s, &target_sha),
            },
            DropChoice::Reset => Action::Form(forms::reset_form(target_sha.clone())),
        }])
    });
    cx.form(f);
}
