//! Search file contents, in the working tree or at any revision. Commit search is in Search.
use egui::{RichText, Ui};
use gitem_core::repo::GrepHit;

use crate::action::Cx;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

/// More than anyone reads, and each row costs a frame's time.
const SHOWN: usize = 2000;

#[derive(Default)]
pub struct GrepView {
    query: String,
    rev: String,
    ignore_case: bool,
    regex: bool,
    found: Task<(Vec<GrepHit>, String)>,
}

impl GrepView {
    /// With its boxes filled in, ready to search.
    pub fn looking_for(query: &str, rev: &str) -> Self {
        Self { query: query.to_string(), rev: rev.to_string(), ..Default::default() }
    }

    pub fn key(&self) -> String {
        "grep".into()
    }
    pub fn title(&self) -> String {
        "Search contents".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.found.poll();
        let mut go = false;
        ui.horizontal(|ui| {
            let r = ui.add(crate::widgets::input(&mut self.query).hint_text("Search file contents").desired_width(320.0));
            go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add(crate::widgets::input(&mut self.rev).hint_text("Revision (working tree if empty)").desired_width(220.0));
            ui.checkbox(&mut self.ignore_case, "Ignore case");
            ui.checkbox(&mut self.regex, "Regex");
            let ready = !self.query.is_empty() && !self.found.busy();
            go |= ui.add_enabled(ready, egui::Button::new("Search")).on_disabled_hover_text("Type something to look for, and wait for any search running.").clicked();
            if self.found.busy() {
                ui.add(spinner(14.0));
            }
        });
        if go && !self.query.is_empty() && !self.found.busy() {
            let (repo, q, rev, case, re) = (cx.repo.clone(), self.query.clone(), self.rev.trim().to_string(), self.ignore_case, self.regex);
            self.found.spawn(ui.ctx(), move || {
                let at = Some(rev.as_str()).filter(|r| !r.is_empty());
                repo.grep(&q, at, case, re).map(|hits| (hits, rev.clone())).map_err(|e| e.to_string())
            });
        }
        ui.separator();
        if let Some(e) = &self.found.error {
            error_box(ui, e, &p);
        }
        let Some((hits, rev)) = self.found.get() else {
            empty_state(ui, "Search file contents", "Finds text in tracked files, as git grep does, here or at any revision.", &p);
            return;
        };
        if hits.is_empty() {
            empty_state(ui, "No matches", "Nothing in the tracked files matches that.", &p);
            return;
        }
        let shown = hits.len().min(SHOWN);
        let more = if hits.len() > SHOWN { format!(", showing the first {SHOWN}") } else { String::new() };
        ui.label(RichText::new(format!("{}{more}", plural(hits.len(), "match", "matches"))).small().color(p.dim));
        let rev = Some(rev.clone()).filter(|r| !r.is_empty());
        let mut open = None;
        egui::ScrollArea::vertical().id_salt("grep-hits").auto_shrink([false, false]).show_rows(ui, 20.0, shown, |ui, range| {
            for hit in &hits[range] {
                ui.horizontal(|ui| {
                    if ui.small_button(format!("{}:{}", hit.path, hit.line)).clicked() {
                        open = Some((hit.path.clone(), hit.line));
                    }
                    ui.add(egui::Label::new(RichText::new(gitem_core::util::prefix(hit.text.trim(), 400)).monospace().color(p.dim)).truncate());
                });
            }
        });
        if let Some((path, line)) = open {
            cx.open(Tab::file(&path, rev, Some(line)));
        }
    }
}
