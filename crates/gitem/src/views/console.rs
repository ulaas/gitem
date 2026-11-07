//! Git console: a transcript of every git command gitem runs, with the ability
//! to run your own. Output of your commands appears inline.
use egui::{RichText, Ui};
use gitem_core::ops;

use crate::action::Cx;
use crate::widgets::*;

/// How many entries are drawn at first, and how many more each time someone asks. Each one
/// can be a page of git's output, and they have different heights.
const SHOWN_STEP: usize = 200;

pub struct ConsoleView {
    input: String,
    show_reads: bool,
    filter: String,
    stick: bool,
    shown: usize,
}

impl Default for ConsoleView {
    fn default() -> Self {
        Self { input: String::new(), show_reads: false, filter: String::new(), stick: true, shown: SHOWN_STEP }
    }
}

impl ConsoleView {
    pub fn key(&self) -> String {
        "console".into()
    }
    pub fn title(&self) -> String {
        "Git Console".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_reads, "Show background reads");
            ui.checkbox(&mut self.stick, "Follow");
            ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter").desired_width(200.0));
            if ui.small_button("Clear").clicked()
                && let Ok(mut l) = cx.repo.git.log.lock() {
                    l.clear();
                }
        });
        ui.separator();
        egui::Panel::bottom("console-input").show(ui, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("git").monospace().color(p.accent));
                let r = ui.add(crate::widgets::input(&mut self.input).font(egui::TextStyle::Monospace).hint_text("log --oneline -5").desired_width(ui.available_width() - 60.0));
                let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Run").clicked() || go) && !self.input.trim().is_empty() {
                    let line = self.input.trim().strip_prefix("git ").unwrap_or(self.input.trim()).to_string();
                    let args = ops::split_command_line(&line);
                    cx.run(ops::raw(args));
                    self.input.clear();
                    r.request_focus();
                }
            });
            ui.add_space(2.0);
        });
        let q = self.filter.to_lowercase();
        let root = cx.repo.root.display().to_string();
        // Up to 2,000 entries of 8 KB each, so no copy of them all per frame. Workers wait for
        // this lock no longer than a frame, and only to add a line.
        let log = cx.repo.git.log.clone();
        let Ok(entries) = log.lock() else { return };
        let matching: Vec<&gitem_core::runner::LogEntry> = entries.iter().filter(|e| (self.show_reads || e.is_write) && (q.is_empty() || e.command.to_lowercase().contains(&q))).collect();
        let older = matching.len().saturating_sub(self.shown);
        egui::ScrollArea::vertical().id_salt("console").stick_to_bottom(self.stick).auto_shrink([false, false]).show(ui, |ui| {
            if older > 0 && ui.small_button(format!("Show {} older", older.min(SHOWN_STEP))).clicked() {
                self.shown += SHOWN_STEP;
            }
            for e in &matching[older..] {
                if !e.cwd.is_empty() && e.cwd != root {
                    ui.label(RichText::new(&e.cwd).small().color(p.faint));
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if e.code == 0 { "✔" } else { "✖" }).color(if e.code == 0 { p.good } else { p.danger }));
                    ui.add(egui::Label::new(RichText::new(&e.command).monospace().color(if e.is_write { p.text } else { p.dim })).wrap());
                    ui.label(RichText::new(format!("{} ms", e.millis)).small().color(p.faint));
                });
                if !e.output.trim().is_empty() {
                    ui.add(egui::Label::new(RichText::new(e.output.trim_end()).monospace().small().color(p.dim)).selectable(true));
                }
            }
            if entries.is_empty() {
                empty_state(ui, "No commands yet", "Everything gitem runs is recorded here.", &p);
            }
        });
    }
}
