//! Review sessions: a checklist of changed files with comment threads, AI findings
//! you can reply to or dismiss, and a hand-off of open notes to a coding agent.
//! Saved inside the repository's git folder, so nothing is committed by accident.
use std::collections::HashSet;
use std::path::PathBuf;

use egui::{RichText, Ui};
use gitem_core::ai;
use gitem_core::model::FileChange;
use gitem_core::repo::DiffSource;
use serde::{Deserialize, Serialize};

use crate::action::{Action, Cx};
use crate::task::Task;
use crate::views::textview::ai_tab;
use crate::widgets::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Thread {
    pub path: String,
    pub text: String,
    pub author: String,
    pub dismissed: bool,
    #[serde(default)]
    pub replies: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    base: String,
    reviewed: HashSet<String>,
    threads: Vec<Thread>,
}

#[derive(Default)]
pub struct ReviewView {
    saved: Saved,
    loaded: bool,
    files: Task<Vec<FileChange>>,
    files_gen: Option<(u64, String)>,
    selected: Option<String>,
    new_comment: String,
    replies: Vec<String>,
    findings: Task<String>,
}

fn store_path(cx: &Cx) -> PathBuf {
    cx.repo.git_dir.join("gitem-review.json")
}

impl ReviewView {
    pub fn key(&self) -> String {
        "review".into()
    }
    pub fn title(&self) -> String {
        "Review".into()
    }

    fn save(&self, cx: &mut Cx) {
        let written = serde_json::to_vec_pretty(&self.saved).map_err(std::io::Error::other).and_then(|b| std::fs::write(store_path(cx), b));
        if let Err(e) = written {
            cx.error(format!("Could not save the review: {e}"));
        }
    }

    fn source(&self) -> DiffSource {
        DiffSource::MergeBase(self.saved.base.clone(), "HEAD".to_string())
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if !self.loaded {
            self.loaded = true;
            self.saved = std::fs::read(store_path(cx)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            if self.saved.base.is_empty() {
                self.saved.base = cx.data.base_branch();
            }
        }
        self.files.poll();
        if self.findings.poll() {
            if let Some(t) = self.findings.value.take() {
                for (path, text) in ai::parse_review_findings(&t) {
                    self.saved.threads.push(Thread { path, text, author: "AI".to_string(), ..Default::default() });
                }
                self.save(cx);
            }
            if let Some(e) = self.findings.error.take() {
                cx.error(e);
            }
        }
        let want = (cx.data.status_gen, self.saved.base.clone());
        if self.files_gen.as_ref() != Some(&want) {
            self.files_gen = Some(want);
            let (r, b) = (cx.repo.clone(), self.saved.base.clone());
            self.files.spawn(ui.ctx(), move || r.compare_files(&b, "HEAD", true));
        }
        ui.horizontal(|ui| {
            ui.label("Changes since");
            ui.add(crate::widgets::input(&mut self.saved.base).desired_width(160.0));
            let busy = self.findings.busy();
            if ui.add_enabled(!busy, egui::Button::new("Review with AI")).on_hover_text("Adds findings as threads").clicked() {
                let (r, src, cfg) = (cx.repo.clone(), self.source(), cx.settings.ai.clone());
                self.findings.spawn(ui.ctx(), move || -> Result<String, String> {
                    let diff = r.diff_for_ai(&src).map_err(|e| e.to_string())?;
                    let (s, u) = ai::review_prompt(&cfg.custom_instructions, &diff);
                    let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                    ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                });
            }
            if busy {
                ui.add(egui::Spinner::new().size(14.0));
            }
            if ui.button("Orient").on_hover_text("Ask AI for a sensible reading order").clicked() {
                let src = self.source();
                ai_tab(cx, "Reading order", move |r| {
                    let d = r.diff_for_ai(&src).map_err(|e| e.to_string())?;
                    Ok(("You help a reviewer approach a change. Give a short reading order of the changed files, with one line on why each comes where it does.".to_string(), format!("Diff:\n\n{d}")))
                });
            }
            let open = self.saved.threads.iter().filter(|t| !t.dismissed).count();
            let agent = cx.settings.agent_command.clone();
            if ui.add_enabled(open > 0 && !agent.trim().is_empty(), egui::Button::new(format!("Hand {open} note(s) to agent"))).clicked() {
                self.hand_off(cx, &agent);
            }
            if ui.button("New review").on_hover_text("Clear the checklist and threads").clicked() {
                self.saved.reviewed.clear();
                self.saved.threads.clear();
                self.save(cx);
            }
        });
        ui.separator();
        if let Some(e) = &self.files.error {
            error_box(ui, e, &p);
        }
        let files = self.files.get().cloned().unwrap_or_default();
        egui::Panel::left("review-files").resizable(true).default_size(300.0).show(ui, |ui| {
            let done = files.iter().filter(|f| self.saved.reviewed.contains(&f.path)).count();
            ui.label(RichText::new(format!("{done} of {} files reviewed", files.len())).small().color(p.dim));
            egui::ScrollArea::vertical().id_salt("review-list").auto_shrink([false, false]).show(ui, |ui| {
                for f in &files {
                    ui.horizontal(|ui| {
                        let mut on = self.saved.reviewed.contains(&f.path);
                        if ui.checkbox(&mut on, "").changed() {
                            if on {
                                self.saved.reviewed.insert(f.path.clone());
                            } else {
                                self.saved.reviewed.remove(&f.path);
                            }
                            self.save(cx);
                        }
                        let notes = self.saved.threads.iter().filter(|t| t.path == f.path && !t.dismissed).count();
                        let label = if notes > 0 { format!("{} ({notes})", f.path) } else { f.path.clone() };
                        if ui.selectable_label(self.selected.as_deref() == Some(&f.path), label).clicked() {
                            self.selected = Some(f.path.clone());
                            cx.diff(self.source(), Some(&f.path));
                        }
                    });
                }
            });
        });
        egui::ScrollArea::vertical().id_salt("review-threads").auto_shrink([false, false]).show(ui, |ui| {
            let target = self.selected.clone().unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add(crate::widgets::input(&mut self.new_comment).hint_text(if target.is_empty() { "Add a general note" } else { "Add a note on this file" }).desired_width(ui.available_width() - 60.0));
                if ui.add_enabled(!self.new_comment.trim().is_empty(), egui::Button::new("Add")).clicked() {
                    let text = std::mem::take(&mut self.new_comment);
                    self.saved.threads.push(Thread { path: target.clone(), text: text.trim().to_string(), author: "You".to_string(), ..Default::default() });
                    self.save(cx);
                }
            });
            self.replies.resize(self.saved.threads.len(), String::new());
            let mut changed = false;
            for i in 0..self.saved.threads.len() {
                let t = self.saved.threads[i].clone();
                if t.dismissed || (!target.is_empty() && !t.path.is_empty() && t.path != target) {
                    continue;
                }
                egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("{} {}", t.author, t.path)).small().color(p.dim));
                    ui.label(t.text.as_str());
                    for r in &t.replies {
                        ui.label(RichText::new(format!("  {r}")).color(p.dim));
                    }
                    ui.horizontal(|ui| {
                        ui.add(crate::widgets::input(&mut self.replies[i]).hint_text("Reply").id_salt(("reply", i)).desired_width(ui.available_width() - 140.0));
                        if ui.small_button("Reply").clicked() && !self.replies[i].trim().is_empty() {
                            let r = std::mem::take(&mut self.replies[i]);
                            self.saved.threads[i].replies.push(r.trim().to_string());
                            changed = true;
                        }
                        if ui.small_button("Dismiss").clicked() {
                            self.saved.threads[i].dismissed = true;
                            changed = true;
                        }
                    });
                });
                ui.add_space(3.0);
            }
            if changed {
                self.save(cx);
            }
        });
    }

    /// The notes go in a file, so no repository text passes through a shell.
    fn hand_off(&self, cx: &mut Cx, agent: &str) {
        let mut notes = String::from("# Review notes\n\nPlease address these review notes in this repository.\n\n");
        for t in self.saved.threads.iter().filter(|t| !t.dismissed) {
            notes.push_str(&format!("- {} ({}): {}\n", if t.path.is_empty() { "general" } else { &t.path }, t.author, t.text));
            for r in &t.replies {
                notes.push_str(&format!("  - {r}\n"));
            }
        }
        let file = cx.repo.git_dir.join("gitem-review-notes.md");
        match std::fs::write(&file, notes) {
            Ok(()) => {
                let path = file.to_string_lossy().replace('\\', "/");
                cx.out.push(Action::OpenTerminal(cx.repo.root.clone(), Some(format!("{agent} \"Address the review notes in {path}\""))));
            }
            Err(e) => cx.error(format!("Could not write the notes: {e}")),
        }
    }
}
