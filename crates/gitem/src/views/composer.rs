//! Commit composer: arrange uncommitted files into a series of commits, by hand
//! or from an AI suggestion, then create them in one step.
use std::collections::HashSet;

use egui::{RichText, Ui};
use gitem_core::ai::{self, ComposedCommit};
use gitem_core::model::RepoOp;
use gitem_core::repo::DiffSource;
use gitem_core::{cred, ops};

use crate::action::Cx;
use crate::task::Task;
use crate::widgets::*;

#[derive(Default)]
pub struct ComposerView {
    groups: Vec<ComposedCommit>,
    suggest: Task<Vec<ComposedCommit>>,
}

impl ComposerView {
    pub fn key(&self) -> String {
        "composer".into()
    }
    pub fn title(&self) -> String {
        "Commit Composer".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.suggest.poll() {
            if let Some(g) = self.suggest.value.take() {
                self.groups = g;
            }
            if let Some(e) = self.suggest.error.take() {
                cx.error(e);
            }
        }
        let Some(st) = cx.status().cloned() else { return };
        let changed: Vec<String> = st.entries.iter().filter(|e| e.conflict.is_none()).map(|e| e.path.clone()).collect();
        // Files that vanished (committed, discarded) drop out of the groups.
        for g in &mut self.groups {
            g.files.retain(|f| changed.contains(f));
        }
        let assigned: HashSet<String> = self.groups.iter().flat_map(|g| g.files.iter().cloned()).collect();
        let unassigned: Vec<String> = changed.iter().filter(|f| !assigned.contains(*f)).cloned().collect();

        ui.horizontal(|ui| {
            if ui.button("New commit").clicked() {
                self.groups.push(ComposedCommit { message: String::new(), files: Vec::new() });
            }
            let busy = self.suggest.busy();
            if ui.add_enabled(!busy && !changed.is_empty(), egui::Button::new("Suggest with AI")).on_hover_text("Sends the diff of tracked files to your AI provider").clicked() {
                let (repo, cfg) = (cx.repo.clone(), cx.settings.ai.clone());
                let paths = changed.clone();
                self.suggest.spawn(ui.ctx(), move || -> Result<Vec<ComposedCommit>, String> {
                    let mut diff = repo.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?;
                    // Untracked files have no diff, so list them by name.
                    let tracked: Vec<&String> = paths.iter().filter(|p| !diff.contains(p.as_str())).collect();
                    if !tracked.is_empty() {
                        diff.push_str("\n\nNew untracked files:\n");
                        for t in tracked {
                            diff.push_str(t);
                            diff.push('\n');
                        }
                    }
                    let (s, u) = ai::composer_prompt(&diff);
                    let key = cred::ai_key(cfg.provider.key_name());
                    let reply = ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())?;
                    ai::parse_composer(&reply).map_err(|e| e.to_string())
                });
            }
            if busy {
                ui.add(egui::Spinner::new().size(14.0));
            }
            ui.separator();
            // Its clean start would cancel a merge or cherry-pick and clear a rebase's conflicts,
            // and its first commit would end the operation early.
            let mid_op = st.op != RepoOp::None;
            let ready = !mid_op && !self.groups.is_empty() && self.groups.iter().all(|g| !g.message.trim().is_empty() && !g.files.is_empty());
            let staged_exists = st.entries.iter().any(|e| e.is_staged());
            let b = egui::Button::new(RichText::new(format!("Create {} commit(s)", self.groups.len())).color(if ready { p.on_accent } else { p.faint })).fill(if ready { p.accent } else { p.panel2 });
            if ui.add_enabled(ready, b).on_disabled_hover_text(if mid_op { "Finish or abort the operation in progress first" } else { "Every commit needs a message and at least one file" }).clicked() {
                // Start from a clean index so each commit contains only its group.
                if staged_exists {
                    cx.run(ops::unstage_all(!st.head_sha.is_empty()));
                }
                for g in &self.groups {
                    cx.run(ops::stage_paths(&g.files));
                    cx.run(ops::commit(&ops::CommitOpts { message: g.message.clone(), ..Default::default() }));
                }
                self.groups.clear();
            }
            if mid_op {
                ui.colored_label(p.warn, format!("{} is in progress. Finish or abort it first.", st.op.label()));
            }
        });
        ui.separator();
        if changed.is_empty() {
            empty_state(ui, "Nothing to compose", "Make some changes first.", &p);
            return;
        }
        egui::Panel::left("composer-unassigned").resizable(true).default_size(280.0).show(ui, |ui| {
            section(ui, &format!("Not in a commit ({})", unassigned.len()), &p);
            egui::ScrollArea::vertical().id_salt("c-un").auto_shrink([false, false]).show(ui, |ui| {
                for f in &unassigned {
                    self.file_item(ui, f, self.groups.len());
                }
            });
        });
        let mut remove = None;
        let mut swap = None;
        let n = self.groups.len();
        let mut moves: Vec<(String, Option<usize>)> = Vec::new();
        egui::ScrollArea::vertical().id_salt("c-groups").auto_shrink([false, false]).show(ui, |ui| {
            if self.groups.is_empty() {
                empty_state(ui, "No commits planned", "Add a commit, or ask AI for a suggestion.", &p);
            }
            for i in 0..n {
                egui::Frame::new().fill(p.panel2).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Commit {}", i + 1)).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✖").on_hover_text("Remove this commit from the plan").clicked() {
                                remove = Some(i);
                            }
                            if i + 1 < n && ui.small_button("▼").clicked() {
                                swap = Some((i, i + 1));
                            }
                            if i > 0 && ui.small_button("▲").clicked() {
                                swap = Some((i, i - 1));
                            }
                        });
                    });
                    ui.add(crate::widgets::text_area(&mut self.groups[i].message).hint_text("Commit message").desired_rows(2).desired_width(f32::INFINITY));
                    for f in self.groups[i].files.clone() {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(&f).monospace().small()).truncate());
                            ui.menu_button("▾", |ui| {
                                if ui.button("Remove from commit").clicked() {
                                    moves.push((f.clone(), None));
                                    ui.close();
                                }
                                for k in (0..n).filter(|k| *k != i) {
                                    if ui.button(format!("Move to commit {}", k + 1)).clicked() {
                                        moves.push((f.clone(), Some(k)));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    }
                });
                ui.add_space(4.0);
            }
        });
        for (f, target) in moves {
            for g in &mut self.groups {
                g.files.retain(|x| *x != f);
            }
            if let Some(t) = target {
                self.groups[t].files.push(f);
            }
        }
        if let Some((a, b)) = swap {
            self.groups.swap(a, b);
        }
        if let Some(r) = remove {
            self.groups.remove(r);
        }
    }

    fn file_item(&mut self, ui: &mut Ui, f: &str, n: usize) {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(f).monospace().small()).truncate());
            ui.menu_button("Add ▾", |ui| {
                for k in 0..n {
                    if ui.button(format!("Commit {}", k + 1)).clicked() {
                        self.groups[k].files.push(f.to_string());
                        ui.close();
                    }
                }
                if ui.button("New commit").clicked() {
                    self.groups.push(ComposedCommit { message: String::new(), files: vec![f.to_string()] });
                    ui.close();
                }
            });
        });
    }
}
