//! Browse the repository as it was at any commit.
use egui::{RichText, Ui};

use crate::action::Cx;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

pub struct TreeView {
    rev: String,
    files: Task<Vec<String>>,
    started: bool,
    filter: String,
}

impl TreeView {
    pub fn new(rev: &str) -> Self {
        Self { rev: rev.to_string(), files: Task::default(), started: false, filter: String::new() }
    }

    pub fn key(&self) -> String {
        format!("tree:{}", self.rev)
    }

    pub fn title(&self) -> String {
        format!("Files @ {}", gitem_core::util::prefix(&self.rev, 7))
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.files.poll();
        if !self.started {
            self.started = true;
            let (r, rev) = (cx.repo.clone(), self.rev.clone());
            self.files.spawn(ui.ctx(), move || r.tracked_files(Some(&rev)));
        }
        ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter files").desired_width(f32::INFINITY));
        if let Some(e) = &self.files.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.files.value.take() else {
            spinner_row(ui, "Listing files…", &p);
            return;
        };
        let q = self.filter.to_lowercase();
        let shown: Vec<&String> = list.iter().filter(|f| q.is_empty() || f.to_lowercase().contains(&q)).collect();
        ui.label(RichText::new(format!("{} at this commit", plural(shown.len(), "file", "files"))).small().color(p.dim));
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().id_salt("tree").auto_shrink([false, false]).show_rows(ui, 22.0, shown.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let f = shown[i];
                let (rect, resp) = row(ui, 22.0, false, &p);
                paint_path(ui, egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 0.0), rect.max), f, &p);
                if resp.clicked() {
                    cx.open(Tab::file(f, Some(self.rev.clone()), None));
                }
                resp.context_menu(|ui| menus::file_menu(ui, cx, f, Some(&self.rev)));
            }
        });
        self.files.value = Some(list);
    }
}
