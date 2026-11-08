//! File history and line history: commit lists scoped to a path or line range.
use std::collections::HashMap;

use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::repo::DiffSource;

use crate::action::Cx;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

const PAGE: usize = 500;

pub struct HistoryView {
    path: String,
    lines: Option<(u32, u32)>,
    /// The revision the line numbers belong to. `None` is the working file or HEAD.
    rev: Option<String>,
    /// Why the numbers may be off, if they may.
    note: Option<String>,
    commits: Task<Vec<Commit>>,
    /// What each commit called this file, as it may have been renamed since.
    paths: Task<HashMap<String, String>>,
    loaded_gen: u64,
    limit: usize,
    selected: Option<String>,
    follow: bool,
    author: String,
}

impl HistoryView {
    pub fn for_file(path: &str) -> Self {
        Self { path: path.to_string(), lines: None, rev: None, note: None, commits: Task::default(), paths: Task::default(), loaded_gen: u64::MAX, limit: PAGE, selected: None, follow: true, author: String::new() }
    }

    pub fn for_lines(path: &str, a: u32, b: u32) -> Self {
        Self { lines: Some((a, b)), ..Self::for_file(path) }
    }

    /// Line numbers read from this revision of the file.
    pub fn at_rev(mut self, rev: Option<String>) -> Self {
        self.rev = rev;
        self
    }

    /// Something to say above the list about how far to trust it.
    pub fn noting(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }

    pub fn key(&self) -> String {
        format!("history:{}:{:?}:{:?}", self.path, self.lines, self.rev)
    }

    pub fn title(&self) -> String {
        let leaf = self.path.rsplit('/').next().unwrap_or(&self.path);
        match self.lines {
            Some((a, b)) => format!("{leaf}:{a}-{b} history"),
            None => format!("{leaf} history"),
        }
    }

    fn load(&mut self, ctx: &egui::Context, cx: &Cx) {
        let repo = cx.repo.clone();
        let start = self.rev.clone().unwrap_or_else(|| "HEAD".to_string());
        let mut q = LogQuery { revs: vec![start], limit: self.limit, ..Default::default() };
        if let Some((a, b)) = self.lines {
            q.line_range = Some((a, b, self.path.clone()));
        } else {
            q.paths = vec![self.path.clone()];
            q.literal_paths = true;
            q.follow = self.follow;
        }
        if !self.author.trim().is_empty() {
            q.author = Some(self.author.trim().to_string());
        }
        self.commits.spawn(ctx, move || repo.log(&q));
        if self.lines.is_none() && self.follow {
            let (repo, path, limit) = (cx.repo.clone(), self.path.clone(), self.limit);
            self.paths.spawn(ctx, move || repo.file_history_points(&path, limit).map(|points| points.into_iter().map(|p| (p.sha, p.path)).collect()));
        } else {
            self.paths = Task::default();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.commits.poll();
        self.paths.poll();
        if self.loaded_gen != cx.data.refs_gen {
            self.loaded_gen = cx.data.refs_gen;
            self.load(&ui.ctx().clone(), cx);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.title()).strong());
            if self.lines.is_none() && ui.checkbox(&mut self.follow, "Follow renames").changed() {
                self.load(&ui.ctx().clone(), cx);
            }
            let r = ui.add(crate::widgets::input(&mut self.author).hint_text("Filter by author").desired_width(160.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.load(&ui.ctx().clone(), cx);
            }
            if crate::widgets::small_button(ui, "Open file").clicked() {
                cx.open(Tab::file(&self.path, self.rev.clone(), self.lines.map(|x| x.0)));
            }
            if self.lines.is_none() && crate::widgets::small_button(ui, "Visual").clicked() {
                cx.open(Tab::visual(&self.path));
            }
            if self.commits.busy() {
                ui.add(spinner(14.0));
            }
        });
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).small().color(p.warn));
        }
        ui.separator();
        if let Some(e) = &self.commits.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.commits.value.take() else {
            if self.commits.busy() {
                spinner_row(ui, "Loading history…", &p);
            }
            return;
        };
        if list.is_empty() {
            empty_state(ui, "No history", "This path has no commits on the current branch.", &p);
        }
        let path = self.path.clone();
        if list.len() >= self.limit
            && let Some(choice) = load_more_bar(ui, "history-load-more", list.len(), "commits", PAGE, self.commits.busy(), &p)
        {
            self.limit = choice.apply(self.limit, PAGE);
            self.load(&ui.ctx().clone(), cx);
        }
        let paths = self.paths.get();
        commit_list(ui, cx, &list, &mut self.selected, "history-list", |ui, cx, c| {
            // Before a rename, the file had another name, and only that name exists there.
            let then = paths.and_then(|m| m.get(&c.sha)).unwrap_or(&path);
            if ui.button("Open changes to this file").clicked() {
                cx.diff(DiffSource::Commit(c.sha.clone()), Some(then));
                ui.close();
            }
            if ui.button("Open this revision with blame").clicked() {
                cx.open(Tab::file(then, Some(c.sha.clone()), None));
                ui.close();
            }
            if ui.button("Compare with working tree").clicked() {
                cx.diff(DiffSource::WorkingVs(c.sha.clone()), Some(&path));
                ui.close();
            }
            ui.separator();
            menus::commit_menu(ui, cx, c);
        });
        self.commits.value = Some(list);
    }
}

/// Virtualised commit list used by history, search, and compare views.
pub fn commit_list(ui: &mut Ui, cx: &mut Cx, list: &[Commit], selected: &mut Option<String>, id: &str, mut menu: impl FnMut(&mut Ui, &mut Cx, &Commit)) {
    let p = cx.pal.clone();
    let row_h = 38.0;
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_rows(ui, row_h, list.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let c = &list[i];
            let (rect, resp) = keyed_row(ui, row_h, selected.as_deref() == Some(&c.sha), &p, ("history", &c.sha));
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, c.subject.as_str()));
            paint_avatar(ui, egui::pos2(rect.left() + 18.0, rect.center().y), 11.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
            let right = rect.right() - 76.0;
            let top = egui::Rect::from_min_max(egui::pos2(rect.left() + 36.0, rect.top() + 3.0), egui::pos2(right, rect.top() + 20.0));
            paint_text(ui, top, &c.subject, prop(13.0), p.text);
            let bot = egui::Rect::from_min_max(egui::pos2(rect.left() + 36.0, rect.top() + 20.0), egui::pos2(right, rect.bottom() - 2.0));
            paint_text(ui, bot, &format!("{} · {}", c.author_name, time_label(c.commit_time, cx.settings.date_relative)), prop(11.0), p.dim);
            paint_text(ui, egui::Rect::from_min_max(egui::pos2(right + 8.0, rect.top()), rect.max), c.short(), mono(11.5), p.faint);
            if resp.clicked() {
                *selected = Some(c.sha.clone());
                cx.inspect(&c.sha);
            }
            resp.context_menu(|ui| menu(ui, cx, c));
        }
    });
}
