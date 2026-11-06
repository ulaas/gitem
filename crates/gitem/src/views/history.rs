//! File history and line history: commit lists scoped to a path or line range.
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
    commits: Task<Vec<Commit>>,
    loaded_gen: u64,
    limit: usize,
    selected: Option<String>,
    follow: bool,
    author: String,
}

impl HistoryView {
    pub fn for_file(path: &str) -> Self {
        Self { path: path.to_string(), lines: None, commits: Task::default(), loaded_gen: u64::MAX, limit: PAGE, selected: None, follow: true, author: String::new() }
    }

    pub fn for_lines(path: &str, a: u32, b: u32) -> Self {
        Self { lines: Some((a, b)), ..Self::for_file(path) }
    }

    pub fn key(&self) -> String {
        format!("history:{}:{:?}", self.path, self.lines)
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
        let mut q = LogQuery { revs: vec!["HEAD".into()], limit: self.limit, ..Default::default() };
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
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.commits.poll();
        if self.loaded_gen != cx.data.refs_gen {
            self.loaded_gen = cx.data.refs_gen;
            self.load(&ui.ctx().clone(), cx);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.title()).strong());
            if self.lines.is_none() && ui.checkbox(&mut self.follow, "Follow renames").changed() {
                self.load(&ui.ctx().clone(), cx);
            }
            let r = ui.add(egui::TextEdit::singleline(&mut self.author).hint_text("Filter by author").desired_width(160.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.load(&ui.ctx().clone(), cx);
            }
            if ui.small_button("Open file").clicked() {
                cx.open(Tab::file(&self.path, None, self.lines.map(|x| x.0)));
            }
            if self.lines.is_none() && ui.small_button("Visual").clicked() {
                cx.open(Tab::visual(&self.path));
            }
            if self.commits.busy() {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
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
        let more = list.len() >= self.limit;
        commit_list(ui, cx, &list, &mut self.selected, "history-list", |ui, cx, c| {
            if ui.button("Open changes to this file").clicked() {
                cx.diff(DiffSource::Commit(c.sha.clone()), Some(&path));
                ui.close();
            }
            if ui.button("Open this revision with blame").clicked() {
                cx.open(Tab::file(&path, Some(c.sha.clone()), None));
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
        if more && ui.button("Load more").clicked() {
            self.limit += PAGE;
            self.load(&ui.ctx().clone(), cx);
        }
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
            let (rect, resp) = row(ui, row_h, selected.as_deref() == Some(&c.sha), &p);
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
