//! Search & Compare: commit search with the rich query syntax, saved searches,
//! and comparison of any two refs (commits in each direction, changed files).
use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::repo::{DiffSource, parse_search};

use crate::action::Cx;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::views::history::commit_list;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Search,
    Compare,
}

struct CompareResult {
    ahead: Vec<Commit>,
    behind: Vec<Commit>,
    files: Vec<FileChange>,
}

pub struct SearchView {
    mode: Mode,
    query: String,
    results: Task<Vec<Commit>>,
    selected: Option<String>,
    limit: usize,
    a: String,
    b: String,
    three_dot: bool,
    compare: Task<CompareResult>,
    auto_run: bool,
    compare_tab: usize,
    compare_limit: usize,
    changelog: Task<String>,
    natural: Task<String>,
}

const SEARCH_PAGE: usize = 500;
const COMPARE_PAGE: usize = 2000;

/// A count that may have stopped at the limit, so it doesn't read as the whole story.
fn counted(n: usize, limit: usize) -> String {
    if n >= limit { format!("{}+", grouped(n)) } else { grouped(n) }
}

impl Default for SearchView {
    fn default() -> Self {
        Self { mode: Mode::Search, query: String::new(), results: Task::default(), selected: None, limit: SEARCH_PAGE, compare_limit: COMPARE_PAGE, a: String::new(), b: String::new(), three_dot: true, compare: Task::default(), auto_run: false, compare_tab: 0, changelog: Task::default(), natural: Task::default() }
    }
}

const HELP: &str = "Words search commit messages, as plain text, and all must be there. Filters:\n  message: or =:   commit message\n  re:              commit message, as a regular expression\n  author: or @:    author name or email\n  commit: or #:    just that commit, by SHA or revision\n  file: or ?:      touched path\n  change: or ~:    added or removed text (regex)\n  changes: or s:   occurrence count changed\n  after:  before:  dates such as 2024-01-01 or \"2 weeks ago\"\n  ref:             search only this branch\n  -is:merge        hide merges";

impl SearchView {
    pub fn search(q: &str) -> Self {
        Self { query: q.to_string(), auto_run: true, ..Default::default() }
    }

    /// With a ref still to pick, there's nothing to run yet.
    pub fn compare(a: &str, b: &str) -> Self {
        Self { mode: Mode::Compare, a: a.to_string(), b: b.to_string(), auto_run: !a.is_empty() && !b.is_empty(), ..Default::default() }
    }

    /// A new search or comparison takes over. Just opening the tab leaves it as it is.
    pub fn retarget(&mut self, new: Self) {
        if new.auto_run {
            self.mode = new.mode;
            self.query = new.query;
            self.a = new.a;
            self.b = new.b;
            self.selected = None;
            self.auto_run = true;
        }
    }

    pub fn key(&self) -> String {
        match self.mode {
            Mode::Search => "search".to_string(),
            Mode::Compare => format!("compare:{}:{}", self.a, self.b),
        }
    }

    pub fn title(&self) -> String {
        match self.mode {
            Mode::Search => "Search".to_string(),
            Mode::Compare if self.a.is_empty() => "Compare".to_string(),
            Mode::Compare => format!("{} ↔ {}", short(&self.a), short(&self.b)),
        }
    }

    /// A new search starts from the first page again.
    fn run_search(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.limit = SEARCH_PAGE;
        self.fetch_search(ctx, cx);
    }

    fn fetch_search(&mut self, ctx: &egui::Context, cx: &Cx) {
        let mut q = parse_search(&self.query);
        q.limit = self.limit;
        // A new search replaces the last one, whose git stops: `change:` can run for minutes.
        self.results.spawn_git(ctx, cx.repo, move |repo| {
            repo.check_search_dates(&q)?;
            repo.log(&q)
        });
    }

    fn run_compare(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.compare_limit = COMPARE_PAGE;
        self.fetch_compare(ctx, cx);
    }

    fn fetch_compare(&mut self, ctx: &egui::Context, cx: &Cx) {
        let (a, b, three) = (self.a.trim().to_string(), self.b.trim().to_string(), self.three_dot);
        let limit = self.compare_limit;
        let repo = cx.repo.clone();
        self.compare.spawn(ctx, move || -> Result<CompareResult, String> {
            let e = |x: gitem_core::Error| x.to_string();
            let log = |spec: String| repo.log(&LogQuery { revs: vec![spec], limit, ..Default::default() }).map_err(e);
            Ok(CompareResult { ahead: log(format!("{a}..{b}"))?, behind: log(format!("{b}..{a}"))?, files: repo.compare_files(&a, &b, three).map_err(e)? })
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        self.results.poll();
        self.compare.poll();
        if self.auto_run {
            self.auto_run = false;
            match self.mode {
                Mode::Search => self.run_search(&ctx, cx),
                Mode::Compare => self.run_compare(&ctx, cx),
            }
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Search, "Search commits");
            ui.selectable_value(&mut self.mode, Mode::Compare, "Compare refs");
        });
        ui.separator();
        match self.mode {
            Mode::Search => self.search_ui(ui, cx),
            Mode::Compare => self.compare_ui(ui, cx),
        }
    }

    fn search_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let ctx = ui.ctx().clone();
        if self.natural.poll() {
            if let Some(q) = self.natural.value.take() {
                self.query = gitem_core::ai::clean_message(&q);
                self.run_search(&ctx, cx);
            }
            if let Some(e) = self.natural.error.take() {
                cx.error(e);
            }
        }
        ui.horizontal(|ui| {
            let r = ui.add(crate::widgets::input(&mut self.query).hint_text("fix login  @:ann  file:src/auth  after:2024-01-01").desired_width(ui.available_width() - 110.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Search").clicked() || enter {
                self.run_search(&ctx, cx);
            }
            if ui.add_enabled(!self.natural.busy() && !self.query.trim().is_empty(), egui::Button::new("Ask AI")).on_hover_text("Treat the box as a plain-language question, like: what did Ann change in auth last month?").clicked() {
                let (q, cfg) = (self.query.clone(), cx.settings.ai.clone());
                let today = gitem_core::util::format_day(gitem_core::util::now_ts());
                self.natural.spawn(&ctx, move || -> Result<String, String> {
                    let (s, u) = gitem_core::ai::search_prompt(&q, &today);
                    let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                    gitem_core::ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                });
            }
            ui.label("?").on_hover_text(HELP);
            if self.results.busy() {
                ui.add(spinner(14.0));
                if ui.small_button("Stop").on_hover_text("Stop this search").clicked() {
                    self.results.stop();
                }
            }
        });
        if let Some(e) = &self.results.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.results.value.take() else {
            ui.add_space(12.0);
            ui.label(RichText::new(HELP).monospace().color(p.dim));
            return;
        };
        ui.label(RichText::new(format!("{} commit(s)", grouped(list.len()))).small().color(p.dim));
        if list.len() >= self.limit
            && let Some(choice) = load_more_bar(ui, "search-load-more", list.len(), "matches", SEARCH_PAGE, self.results.busy(), &p)
        {
            self.limit = choice.apply(self.limit, SEARCH_PAGE);
            self.fetch_search(&ui.ctx().clone(), cx);
        }
        if list.is_empty() {
            empty_state(ui, "No commits found", "Try fewer filters, or search all branches.", &p);
        }
        commit_list(ui, cx, &list, &mut self.selected, "search-results", menus::commit_menu);
        self.results.value = Some(list);
    }

    fn compare_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let ctx = ui.ctx().clone();
        let refs = cx.data.ref_names();
        ui.horizontal(|ui| {
            ref_picker(ui, "cmp-a", &mut self.a, &refs);
            if ui.small_button("⇄").on_hover_text("Swap").clicked() {
                std::mem::swap(&mut self.a, &mut self.b);
                self.run_compare(&ctx, cx);
            }
            ref_picker(ui, "cmp-b", &mut self.b, &refs);
            if ui.checkbox(&mut self.three_dot, "From merge base").on_hover_text("Show only what changed on the right since the two diverged").changed() {
                self.run_compare(&ctx, cx);
            }
            let ready = !self.a.trim().is_empty() && !self.b.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Compare")).clicked() {
                self.run_compare(&ctx, cx);
            }
            if self.compare.busy() {
                ui.add(spinner(14.0));
            }
        });
        if let Some(e) = &self.compare.error {
            error_box(ui, e, &p);
        }
        let Some(res) = self.compare.value.take() else {
            empty_state(ui, "Pick two refs", "See commits on each side and every changed file.", &p);
            return;
        };
        self.changelog.poll();
        let (a, b) = (self.a.clone(), self.b.clone());
        let src = if self.three_dot { DiffSource::MergeBase(a.clone(), b.clone()) } else { DiffSource::Range(a.clone(), b.clone()) };
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.compare_tab, 0, format!("Files ({})", res.files.len()));
            ui.selectable_value(&mut self.compare_tab, 1, format!("{} only ({})", short(&b), counted(res.ahead.len(), self.compare_limit)));
            ui.selectable_value(&mut self.compare_tab, 2, format!("{} only ({})", short(&a), counted(res.behind.len(), self.compare_limit)));
            if ui.small_button("Open all diffs").clicked() {
                cx.diff(src.clone(), None);
            }
            if let Some(f) = cx.forge() {
                let url = f.compare_url(&a, &b);
                if ui.small_button("Open on remote").clicked() {
                    cx.url(url);
                }
            }
            if ui.add_enabled(!self.changelog.busy(), egui::Button::new("Changelog with AI").small()).on_hover_text("Summarise the commits on the right-hand side").clicked() {
                let text: String = res.ahead.iter().map(|c| format!("- {}\n", c.message().replace('\n', " "))).collect();
                let cfg = cx.settings.ai.clone();
                self.changelog.spawn(ui.ctx(), move || -> Result<String, String> {
                    let (s, u) = gitem_core::ai::changelog_prompt(&text);
                    let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                    gitem_core::ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                });
            }
            if ui.small_button("Save as patch…").clicked() {
                cx.open(Tab::Patches(Box::new(crate::views::patches::PatchesView::create_from(src.clone(), &format!("{a} to {b}")))));
            }
        });
        if let Some(e) = &self.changelog.error {
            error_box(ui, e, &p);
        } else if let Some(t) = self.changelog.get().cloned() {
            egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(3.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Changelog").strong());
                    if ui.small_button("Copy").clicked() {
                        cx.copy(t.clone());
                    }
                });
                egui::ScrollArea::vertical().id_salt("changelog").max_height(180.0).show(ui, |ui| {
                    ui.label(t.as_str());
                });
            });
        }
        match self.compare_tab {
            0 => {
                egui::ScrollArea::vertical().id_salt("cmp-files").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for f in &res.files {
                        let resp = file_row(ui, f, false, &p);
                        if resp.clicked() {
                            cx.diff(src.clone(), Some(&f.path));
                        }
                        resp.context_menu(|ui| menus::file_menu(ui, cx, &f.path, Some(&b)));
                    }
                });
            }
            tab => {
                let list = if tab == 1 { &res.ahead } else { &res.behind };
                if list.len() >= self.compare_limit
                    && let Some(choice) = load_more_bar(ui, "compare-load-more", list.len(), "commits", COMPARE_PAGE, self.compare.busy(), &p)
                {
                    self.compare_limit = choice.apply(self.compare_limit, COMPARE_PAGE);
                    self.fetch_compare(&ctx, cx);
                }
                commit_list(ui, cx, list, &mut self.selected, if tab == 1 { "cmp-ahead" } else { "cmp-behind" }, menus::commit_menu);
            }
        }
        self.compare.value = Some(res);
    }
}

fn short(s: &str) -> &str {
    gitem_core::util::short_rev(s)
}

/// Text input with a dropdown of matching refs.
pub fn ref_picker(ui: &mut Ui, id: &str, value: &mut String, refs: &[String]) {
    ui.add(crate::widgets::input(value).id_salt(id).hint_text("branch, tag or SHA").desired_width(180.0));
    ui.menu_button("▾", |ui| {
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for r in refs {
                if ui.button(r.as_str()).clicked() {
                    *value = r.clone();
                    ui.close();
                }
            }
        });
    });
}
