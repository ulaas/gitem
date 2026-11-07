//! Picking some hunks of the unstaged changes to stash. What it does with them is in
//! `gitem_core::hunkstash`. This only reads until the button is pressed.
use std::collections::BTreeMap;

use egui::{RichText, Ui};
use gitem_core::diff::{FileDiff, Hunk, LineKind};
use gitem_core::hunkstash;
use gitem_core::repo::{DiffOpts, DiffSource};

use crate::action::Cx;
use crate::task::Task;
use crate::widgets::*;

/// A long hunk still fits on screen. Its whole text goes in the stash.
const LINES_SHOWN: usize = 30;

#[derive(Default)]
pub struct StashHunksView {
    files: Task<Vec<FileDiff>>,
    /// The status this list was read for.
    read_for: Option<u64>,
    /// By path, which hunks. A file whose hunks have changed loses its ticks: they'd mean other text.
    picked: BTreeMap<String, Vec<usize>>,
    /// What was picked from, by path.
    shown: BTreeMap<String, Vec<Hunk>>,
    message: String,
}

impl StashHunksView {
    pub fn key(&self) -> String {
        "stash-hunks".into()
    }
    pub fn title(&self) -> String {
        "Stash Hunks".into()
    }

    /// Ticks a hunk, as clicking would. For tests.
    pub fn pick_for_test(&mut self, path: &str, hunk: usize) {
        self.picked.entry(path.to_string()).or_default().push(hunk);
    }

    fn take_new_list(&mut self) {
        let Some(files) = self.files.get() else { return };
        let now: BTreeMap<String, Vec<Hunk>> = files.iter().map(|f| (f.display_path().to_string(), f.hunks.clone())).collect();
        if now != self.shown {
            let was = std::mem::replace(&mut self.shown, now);
            let shown = &self.shown;
            self.picked.retain(|path, _| was.get(path) == shown.get(path));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.files.poll() {
            self.take_new_list();
        }
        if self.read_for != Some(cx.data.status_gen) && !self.files.busy() {
            self.read_for = Some(cx.data.status_gen);
            self.files.spawn_git(ui.ctx(), cx.repo, |r| r.diff(&DiffSource::Unstaged, None, &DiffOpts::default()));
        }
        let status = cx.status().cloned();
        let staged: Vec<String> = status.iter().flat_map(|s| &s.entries).filter(|e| e.is_staged()).map(|e| e.path.clone()).collect();
        let head = status.as_ref().map(|s| s.head_sha.clone()).unwrap_or_default();
        let in_op = status.as_ref().is_some_and(|s| s.op != gitem_core::RepoOp::None);

        ui.label(RichText::new("Tick the hunks to set aside. They leave these files and go into a stash, and what isn't ticked stays. Apply and Pop on that stash bring them back, with the rest of the file still changed.").color(p.dim));
        ui.add_space(4.0);
        let files = self.files.get().cloned().unwrap_or_default();
        let picks: Vec<(String, Vec<usize>)> = self.picked.iter().map(|(path, hunks)| {
            let mut hunks = hunks.clone();
            hunks.sort_unstable();
            (path.clone(), hunks)
        }).filter(|(_, h)| !h.is_empty()).collect();
        let count: usize = picks.iter().map(|(_, h)| h.len()).sum();
        ui.horizontal(|ui| {
            ui.add(crate::widgets::input(&mut self.message).hint_text("Message (optional)").desired_width(320.0));
            let why = if in_op { "Finish or abort what's going on first: see the banner." } else if head.is_empty() { "There's no commit yet for a stash to sit on." } else { "Tick at least one hunk." };
            let can = count > 0 && !in_op && !head.is_empty();
            if ui.add_enabled(can, egui::Button::new(format!("Stash {}", plural(count, "hunk", "hunks")))).on_disabled_hover_text(why).clicked()
                && let Some(picked) = hunkstash::pick(&files, &picks)
            {
                cx.run(hunkstash::stash(picked, &self.message, &head));
                self.picked.clear();
                self.message.clear();
            }
            if self.files.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.separator();
        if let Some(e) = &self.files.error {
            error_box(ui, e, &p);
            return;
        }
        if files.is_empty() {
            if !self.files.busy() {
                empty_state(ui, "No unstaged changes", "Change a tracked file, and its hunks appear here.", &p);
            }
            return;
        }
        egui::ScrollArea::vertical().id_salt("stash-hunks").auto_shrink([false, false]).show(ui, |ui| {
            for f in &files {
                let path = f.display_path().to_string();
                ui.label(RichText::new(&path).strong());
                if let Some(why) = hunkstash::why_not(f, &staged) {
                    ui.label(RichText::new(why).small().color(p.dim));
                    ui.add_space(6.0);
                    continue;
                }
                for (i, h) in f.hunks.iter().enumerate() {
                    self.hunk_row(ui, &path, i, h, &p);
                }
                ui.add_space(6.0);
            }
        });
    }

    fn hunk_row(&mut self, ui: &mut Ui, path: &str, i: usize, h: &Hunk, p: &crate::theme::Palette) {
        let name = format!("{path}: lines {} to {}", h.new_start, h.new_start + h.new_count.saturating_sub(1));
        let mut on = self.picked.get(path).is_some_and(|v| v.contains(&i));
        if ui.checkbox(&mut on, name).changed() {
            let v = self.picked.entry(path.to_string()).or_default();
            v.retain(|x| *x != i);
            if on {
                v.push(i);
            }
        }
        egui::Frame::new().fill(p.panel2).inner_margin(egui::Margin::symmetric(8, 4)).corner_radius(3.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for l in h.lines.iter().take(LINES_SHOWN) {
                let (sign, color) = match l.kind {
                    LineKind::Add => ('+', p.add_fg),
                    LineKind::Del => ('-', p.del_fg),
                    LineKind::Context => (' ', p.dim),
                    LineKind::NoNewline => continue,
                };
                ui.add(egui::Label::new(RichText::new(format!("{sign}{}", l.text)).monospace().size(11.5).color(color)).truncate());
            }
            if h.lines.len() > LINES_SHOWN {
                ui.label(RichText::new(format!("And {} more lines.", grouped(h.lines.len() - LINES_SHOWN))).small().color(p.dim));
            }
        });
        ui.add_space(2.0);
    }
}
