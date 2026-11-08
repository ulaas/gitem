//! Review sessions: a checklist of changed files with comment threads, AI findings
//! you can reply to or dismiss, and a hand-off of open notes to a coding agent.
//! Saved inside the repository's git folder, so nothing is committed by accident.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    /// The version of each file that was marked, so a later change takes its mark away.
    #[serde(default)]
    reviewed_blobs: HashMap<String, String>,
}

#[derive(Default)]
pub struct ReviewView {
    saved: Saved,
    loaded: bool,
    files: Task<(Vec<FileChange>, HashMap<String, String>)>,
    files_gen: Option<(u64, String)>,
    /// The base the list is for. It catches up with the box once typing pauses.
    settled_base: Option<String>,
    base_typed: Option<Instant>,
    show_dismissed: bool,
    selected: Option<String>,
    new_comment: String,
    replies: Vec<String>,
    findings: Task<String>,
    /// New review was clicked with notes to lose, and awaits a yes.
    confirm_new: bool,
}

const BASE_SETTLES: Duration = Duration::from_millis(400);

fn store_path(cx: &Cx) -> PathBuf {
    cx.repo.git_dir.join("gitem-review.json")
}

impl ReviewView {
    /// As if typed in the new note's box. For tests.
    pub fn write_for_test(&mut self, note: &str) {
        self.new_comment = note.to_string();
    }

    /// What closing this tab would lose. Notes that have been added are saved; what's still
    /// in the boxes isn't.
    pub fn unsaved(&self) -> Option<String> {
        (!self.new_comment.trim().is_empty() || self.replies.iter().any(|r| !r.trim().is_empty())).then(|| "A review note you're typing.".to_string())
    }

    pub fn key(&self) -> String {
        "review".into()
    }
    pub fn title(&self) -> String {
        "Review".into()
    }

    fn save(&self, cx: &mut Cx) {
        let written = serde_json::to_vec_pretty(&self.saved).map_err(std::io::Error::other).and_then(|b| gitem_core::util::write_atomic(&store_path(cx), &b));
        if let Err(e) = written {
            cx.error(format!("Could not save the review: {e}"));
        }
    }

    /// A file that won't read is moved aside and reported, or the next save would finish it off.
    fn load(cx: &mut Cx) -> Saved {
        let path = store_path(cx);
        let Ok(bytes) = std::fs::read(&path) else { return Saved::default() };
        match serde_json::from_slice(&bytes) {
            Ok(saved) => saved,
            Err(e) => {
                let copy = path.with_file_name(format!("gitem-review.json.bad-{}", gitem_core::util::now_ts()));
                match std::fs::rename(&path, &copy) {
                    Ok(()) => cx.error(format!("The saved review couldn't be read ({e}), so this starts a new one. The old file is kept at {}.", copy.display())),
                    Err(move_failed) => cx.error(format!("The saved review couldn't be read ({e}) or moved aside ({move_failed}), so saving this one will replace {}.", path.display())),
                }
                Saved::default()
            }
        }
    }

    fn source(&self) -> DiffSource {
        DiffSource::MergeBase(self.saved.base.clone(), "HEAD".to_string())
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        // With no commit yet, git's answer about the range would be a raw "fatal".
        if cx.status().is_some_and(|s| s.head_sha.is_empty()) {
            crate::widgets::empty_state(ui, "Nothing to review yet", "Review compares this branch's commits with its base. This repository has no commits.", &p);
            return;
        }
        if !self.loaded {
            self.loaded = true;
            self.saved = Self::load(cx);
            if self.saved.base.is_empty() {
                self.saved.base = cx.data.base_branch();
            }
        }
        if self.files.poll()
            && let Some((_, blobs)) = self.files.get()
        {
            let saved = &mut self.saved;
            let before = saved.reviewed.len();
            saved.reviewed.retain(|f| saved.reviewed_blobs.get(f).is_none_or(|b| blobs.get(f) == Some(b)));
            saved.reviewed_blobs.retain(|f, _| saved.reviewed.contains(f));
            if saved.reviewed.len() != before {
                cx.toast("A file changed since you marked it reviewed, so it's unmarked.");
                self.save(cx);
            }
        }
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
        // Not per key: each half-typed name would be a git command and an error on screen.
        if self.settled_base.is_none() || self.base_typed.is_some_and(|t| t.elapsed() >= BASE_SETTLES) {
            self.settled_base = Some(self.saved.base.clone());
            self.base_typed = None;
        } else if let Some(t) = self.base_typed {
            ui.ctx().request_repaint_after(BASE_SETTLES.saturating_sub(t.elapsed()));
        }
        let base = self.settled_base.clone().unwrap_or_default();
        let want = (cx.data.status_gen, base.clone());
        if self.files_gen.as_ref() != Some(&want) {
            self.files_gen = Some(want);
            let (r, b) = (cx.repo.clone(), base.clone());
            self.files.spawn(ui.ctx(), move || -> gitem_core::Result<_> { Ok((r.compare_files(&b, "HEAD", true)?, r.compare_blobs(&b, "HEAD").unwrap_or_default())) });
        }
        ui.horizontal(|ui| {
            ui.label("Changes since");
            if ui.add(crate::widgets::input(&mut self.saved.base).desired_width(160.0)).changed() {
                self.base_typed = Some(Instant::now());
            }
            let busy = self.findings.busy();
            // On the base branch itself there's nothing between the two.
            let nothing = self.files.get().is_some_and(|(f, _)| f.is_empty());
            let why = "There are no changes since that base.";
            if ui.add_enabled(!busy && !nothing, egui::Button::new("Review with AI")).on_hover_text("Adds findings as threads").on_disabled_hover_text(why).clicked() {
                let (r, src, cfg) = (cx.repo.clone(), self.source(), cx.settings.ai.clone());
                self.findings.spawn(ui.ctx(), move || -> Result<String, String> {
                    let diff = r.diff_for_ai(&src).map_err(|e| e.to_string())?;
                    let (s, u) = ai::review_prompt(&cfg.custom_instructions, &diff);
                    let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                    ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                });
            }
            if busy {
                ui.add(spinner(14.0));
            }
            if ui.add_enabled(!nothing, egui::Button::new("Orient")).on_hover_text("Ask AI for a sensible reading order").on_disabled_hover_text(why).clicked() {
                let src = self.source();
                ai_tab(cx, "Reading order", move |r| {
                    let d = r.diff_for_ai(&src).map_err(|e| e.to_string())?;
                    Ok(("You help a reviewer approach a change. Give a short reading order of the changed files, with one line on why each comes where it does.".to_string(), format!("Diff:\n\n{d}")))
                });
            }
            let open = self.saved.threads.iter().filter(|t| !t.dismissed).count();
            let agent = cx.settings.agent_command.clone();
            if ui.add_enabled(open > 0 && !agent.trim().is_empty(), egui::Button::new(if open == 0 { "Hand notes to agent".to_string() } else { format!("Hand {} to agent", plural(open, "note", "notes")) })).on_disabled_hover_text(if open == 0 { "There are no open notes." } else { "Set an agent command in Settings." }).clicked() {
                self.hand_off(cx, &agent);
            }
            if ui.button("New review").on_hover_text("Clear the checklist and threads").clicked() {
                self.confirm_new = true;
            }
        });
        if self.files.get().is_some_and(|(f, _)| f.is_empty()) && self.saved.threads.is_empty() {
            ui.label(RichText::new("No changes to review").strong());
            ui.label(RichText::new(format!("This branch has nothing that {} doesn't. Pick another base above.", self.saved.base)).color(p.dim));
        }
        // Nothing to lose means nothing to ask. On its own row: this one is often full.
        let mut clear = self.confirm_new && self.saved.reviewed.is_empty() && self.saved.threads.is_empty();
        if self.confirm_new && !clear {
            ui.horizontal(|ui| {
                ui.colored_label(p.warn, "Clear the checklist and every note?");
                clear = ui.button("Clear them").clicked();
                if ui.button("Keep them").clicked() {
                    self.confirm_new = false;
                }
            });
        }
        if clear {
            self.confirm_new = false;
            self.saved.reviewed.clear();
            self.saved.threads.clear();
            self.save(cx);
        }
        ui.separator();
        if let Some(e) = &self.files.error {
            // Most often nothing's wrong but the name: a repository's main branch can be called anything.
            if ["ambiguous argument", "unknown revision", "bad revision", "Not a valid object name", "no merge base"].iter().any(|m| e.contains(m)) {
                ui.colored_label(p.warn, format!("Can't compare with \"{base}\". Type the name of a branch or commit it should be compared with."));
            } else {
                error_box(ui, e, &p);
            }
        }
        let (files, blobs) = self.files.get().cloned().unwrap_or_default();
        egui::Panel::left("review-files").resizable(true).default_size(300.0).show(ui, |ui| {
            let done = files.iter().filter(|f| self.saved.reviewed.contains(&f.path)).count();
            ui.label(RichText::new(format!("{done} of {} files reviewed", files.len())).small().color(p.dim));
            if ui.selectable_label(self.selected.is_none(), "All files").on_hover_text("General notes, and notes on every file").clicked() {
                self.selected = None;
            }
            egui::ScrollArea::vertical().id_salt("review-list").auto_shrink([false, false]).show(ui, |ui| {
                for f in &files {
                    ui.horizontal(|ui| {
                        let mut on = self.saved.reviewed.contains(&f.path);
                        if ui.checkbox(&mut on, "").changed() {
                            if on {
                                self.saved.reviewed.insert(f.path.clone());
                                if let Some(b) = blobs.get(&f.path) {
                                    self.saved.reviewed_blobs.insert(f.path.clone(), b.clone());
                                }
                            } else {
                                self.saved.reviewed.remove(&f.path);
                                self.saved.reviewed_blobs.remove(&f.path);
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
                if ui.add_enabled(!self.new_comment.trim().is_empty(), egui::Button::new("Add")).on_disabled_hover_text("Type a comment first.").clicked() {
                    let text = std::mem::take(&mut self.new_comment);
                    self.saved.threads.push(Thread { path: target.clone(), text: text.trim().to_string(), author: "You".to_string(), ..Default::default() });
                    self.save(cx);
                }
            });
            let dismissed = self.saved.threads.iter().filter(|t| t.dismissed).count();
            if dismissed > 0 || self.show_dismissed {
                ui.checkbox(&mut self.show_dismissed, format!("Show dismissed ({dismissed})"));
            }
            self.replies.resize(self.saved.threads.len(), String::new());
            let mut changed = false;
            for i in 0..self.saved.threads.len() {
                let t = self.saved.threads[i].clone();
                if (t.dismissed && !self.show_dismissed) || (!target.is_empty() && !t.path.is_empty() && t.path != target) {
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
                        if crate::widgets::small_button(ui, "Reply").clicked() && !self.replies[i].trim().is_empty() {
                            let r = std::mem::take(&mut self.replies[i]);
                            self.saved.threads[i].replies.push(r.trim().to_string());
                            changed = true;
                        }
                        if t.dismissed {
                            if crate::widgets::small_button(ui, "Restore").clicked() {
                                self.saved.threads[i].dismissed = false;
                                changed = true;
                            }
                        } else if crate::widgets::small_button(ui, "Dismiss").clicked() {
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
