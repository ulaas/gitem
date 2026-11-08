//! Worktree manager: every checkout of this repository with its branch, state,
//! and uncommitted-change counts, plus create, move, lock, remove, and open.
use std::collections::HashMap;

use egui::{RichText, Ui};
use gitem_core::{Repo, ops};

use crate::action::{Action, Cx};
use crate::forms;
use crate::menus;
use crate::task::Task;
use crate::widgets::*;

#[derive(Default)]
pub struct WorktreesView {
    dirty: Task<HashMap<String, (usize, u32, u32)>>,
    loaded_gen: u64,
    init: bool,
}

impl WorktreesView {
    pub fn key(&self) -> String {
        "worktrees".into()
    }
    pub fn title(&self) -> String {
        "Worktrees".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.dirty.poll();
        if !self.init || self.loaded_gen != cx.data.refs_gen + cx.data.status_gen {
            self.init = true;
            self.loaded_gen = cx.data.refs_gen + cx.data.status_gen;
            let git = cx.repo.git.clone();
            let paths: Vec<String> = cx.data.worktrees().iter().filter(|w| !w.bare && w.prunable.is_none()).map(|w| w.path.clone()).collect();
            self.dirty.spawn(ui.ctx(), move || -> Result<HashMap<String, (usize, u32, u32)>, String> {
                let mut m = HashMap::new();
                for path in paths {
                    if let Ok(r) = Repo::open(git.clone(), std::path::Path::new(&path))
                        && let Ok(s) = r.status() {
                            m.insert(path, (s.entries.len(), s.ahead, s.behind));
                        }
                }
                Ok(m)
            });
        }
        if !cx.data.refs_known() {
            match &cx.data.refs.error {
                Some(e) => error_box(ui, e, &p),
                None => spinner_row(ui, "Reading worktrees…", &p),
            }
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("Add worktree…").clicked() {
                cx.form(forms::add_worktree(cx.repo, cx.data.ref_names(), None));
            }
            if ui.button("Prune stale").on_hover_text("Forget worktrees whose folders are gone").clicked() {
                cx.run(ops::worktree_prune());
            }
            if ui.button("Repair").on_hover_text("Fix links after folders moved").clicked() {
                cx.run(ops::worktree_repair());
            }
            if self.dirty.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.add_space(4.0);
        let info = self.dirty.get();
        egui::ScrollArea::vertical().id_salt("worktrees").auto_shrink([false, false]).show(ui, |ui| {
            for w in cx.data.worktrees().to_vec() {
                let frame = egui::Frame::new().fill(if w.is_current { p.selection } else { p.panel2 }).inner_margin(8.0).corner_radius(4.0);
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let name = w.branch.clone().unwrap_or_else(|| {
                                if w.bare {
                                    "(bare)".to_string()
                                } else {
                                    format!("detached at {}", gitem_core::util::prefix(&w.head, 8))
                                }
                            });
                            ui.label(RichText::new(name).strong());
                            if w.is_main {
                                chip(ui, "main worktree", p.series[0]);
                            }
                            if w.is_current {
                                chip(ui, "open here", p.accent);
                            }
                            if let Some(r) = &w.locked {
                                chip(ui, &if r.is_empty() { "locked".to_string() } else { format!("locked: {r}") }, p.warn);
                            }
                            if w.prunable.is_some() {
                                chip(ui, "folder missing", p.danger);
                            }
                            if let Some((d, a, b)) = info.and_then(|m| m.get(&w.path)) {
                                if *d > 0 {
                                    chip(ui, &format!("{d} changed"), p.series[3]);
                                }
                                if *a > 0 {
                                    chip(ui, &format!("\u{E08E}{a}"), p.series[2]);
                                }
                                if *b > 0 {
                                    chip(ui, &format!("\u{E03E}{b}"), p.series[6]);
                                }
                            }
                        });
                        ui.label(RichText::new(&w.path).small().color(p.dim));
                        ui.horizontal(|ui| {
                            if !w.is_current && w.prunable.is_none() && crate::widgets::small_button(ui, "Open").clicked() {
                                cx.out.push(Action::OpenRepo(w.path.clone().into()));
                            }
                            ui.menu_button("More \u{E136}", |ui| menus::worktree_menu(ui, cx, &w));
                        });
                    })
                    .response;
                resp.context_menu(|ui| menus::worktree_menu(ui, cx, &w));
                ui.add_space(3.0);
            }
        });
    }
}
