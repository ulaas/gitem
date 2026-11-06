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

/// A plan handed to git, until it's known which of its commits landed.
struct Running {
    /// HEAD before the first commit. Empty on an unborn branch.
    start: String,
    messages: Vec<String>,
}

#[derive(Default)]
pub struct ComposerView {
    groups: Vec<ComposedCommit>,
    suggest: Task<Vec<ComposedCommit>>,
    running: Option<Running>,
    /// Asked to go ahead although these files have only some of their changes staged.
    confirm_partial: Vec<String>,
}

/// A commit message as `--cleanup=whitespace` leaves it, for matching one against the log.
fn cleaned(message: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in message.lines().map(str::trim_end) {
        if !(line.is_empty() && out.last().is_none_or(|l: &&str| l.is_empty())) {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

impl ComposerView {
    pub fn key(&self) -> String {
        "composer".into()
    }
    pub fn title(&self) -> String {
        "Commit Composer".into()
    }

    /// What closing this tab would lose. An empty plan with nothing typed is nothing.
    pub fn unsaved(&self) -> Option<String> {
        let planned = self.groups.iter().filter(|g| !g.message.trim().is_empty() || !g.files.is_empty()).count();
        (planned > 0).then(|| format!("Commit Composer: a plan of {planned} commit(s)"))
    }

    /// Once the queue is done: the commits that landed leave the plan and the rest stay,
    /// with their messages, because a hook or a failed step earlier may have stopped them.
    fn settle_run(&mut self, cx: &mut Cx) {
        if cx.data.ops_busy {
            return;
        }
        let Some(run) = self.running.take() else { return };
        let landed = cx.repo.messages_since(&run.start).unwrap_or_default();
        let mut landed = landed.iter().map(|m| cleaned(m));
        let mut next = landed.next();
        for message in &run.messages {
            if next.as_deref() != Some(cleaned(message).as_str()) {
                break;
            }
            next = landed.next();
            if let Some(i) = self.groups.iter().position(|g| g.message == *message) {
                self.groups.remove(i);
            }
        }
    }

    fn create(&mut self, cx: &mut Cx, live: &[String], st: &gitem_core::model::RepoStatus) {
        let plan: Vec<(String, Vec<String>)> = self.groups.iter().map(|g| (g.message.clone(), g.files.iter().filter(|f| live.contains(f)).cloned().collect())).collect();
        self.running = Some(Running { start: st.head_sha.clone(), messages: plan.iter().map(|(m, _)| m.clone()).collect() });
        self.confirm_partial.clear();
        cx.run(ops::compose_commits(&plan));
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
        self.settle_run(cx);
        let Some(st) = cx.status().cloned() else { return };
        let changed: Vec<String> = st.entries.iter().filter(|e| e.conflict.is_none()).map(|e| e.path.clone()).collect();
        // A file can leave the status for a moment (stash and pop, an edit undone and redone).
        // It stays in its group, marked, and isn't committed while it has no changes.
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
            // A commit now would end a merge or cherry-pick early. A rebase stopped for you,
            // as at an edit step, is the exception: making commits there is how to split one.
            let mid_op = st.op != RepoOp::None && !(st.op_allows_commits && st.conflicts().next().is_none());
            let working = self.running.is_some() || cx.data.ops_busy;
            let ready = !mid_op && !working && !self.groups.is_empty() && self.groups.iter().all(|g| !g.message.trim().is_empty() && g.files.iter().any(|f| changed.contains(f)));
            let b = egui::Button::new(RichText::new(format!("Create {} commit(s)", self.groups.len())).color(if ready { p.on_accent } else { p.faint })).fill(if ready { p.accent } else { p.panel2 });
            if ui.add_enabled(ready, b).on_disabled_hover_text(if mid_op { "Finish or abort the operation in progress first" } else { "Every commit needs a message and at least one changed file" }).clicked() {
                // Each commit takes its files whole, so a line left unstaged on purpose would go in.
                let partial: Vec<String> = st.entries.iter().filter(|e| e.is_staged() && e.is_unstaged() && assigned.contains(&e.path)).map(|e| e.path.clone()).collect();
                if partial.is_empty() {
                    self.create(cx, &changed, &st);
                } else {
                    self.confirm_partial = partial;
                }
            }
            if mid_op {
                ui.colored_label(p.warn, format!("{} is in progress. Finish or abort it first.", st.op.label()));
            }
        });
        if !self.confirm_partial.is_empty() {
            let names = self.confirm_partial.join(", ");
            ui.colored_label(p.warn, format!("Only some of the changes in {names} are staged. Each commit takes its files whole, so all of them would be committed."));
            ui.horizontal(|ui| {
                if ui.button("Commit the whole files").clicked() {
                    self.create(cx, &changed, &st);
                }
                if ui.button("Go back").clicked() {
                    self.confirm_partial.clear();
                }
            });
        }
        ui.separator();
        if changed.is_empty() && self.groups.is_empty() {
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
                            if !changed.contains(&f) {
                                ui.label(RichText::new("no changes now").small().color(p.faint));
                            }
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
