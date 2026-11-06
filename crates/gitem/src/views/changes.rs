//! Working-tree changes: stage and unstage, commit box with AI message, conflicts,
//! and controls for operations in progress.
use std::collections::HashSet;

use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::DiffSource;
use gitem_core::{ai, cred};

use crate::action::Cx;
use crate::forms;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

#[derive(Default)]
pub struct ChangesView {
    pub message: String,
    amend: bool,
    signoff: bool,
    sign: bool,
    no_verify: bool,
    author: String,
    filter: String,
    ai: Task<String>,
    ai_stash: Task<String>,
    applied_prepared_for: Option<RepoOp>,
    /// Message and HEAD at submission, kept until we know the commit landed.
    submitted: Option<(String, String)>,
    /// Ctrl-clicked paths, for batch actions.
    selected: HashSet<String>,
    init: bool,
}

fn change_of(e: &StatusEntry, staged: bool) -> FileChange {
    FileChange { path: e.path.clone(), orig_path: e.orig_path.clone(), status: if staged { e.index } else { e.worktree }, additions: None, deletions: None }
}

impl ChangesView {
    pub fn key(&self) -> String {
        "changes".into()
    }
    pub fn title(&self) -> String {
        "Changes".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if !self.init {
            self.init = true;
            self.sign = cx.settings.commit_sign;
            self.signoff = cx.settings.commit_signoff;
        }
        if self.ai.poll() {
            if let Some(m) = self.ai.value.take() {
                self.message = ai::clean_message(&m);
            }
            if let Some(e) = self.ai.error.take() {
                cx.error(e);
            }
        }
        if self.ai_stash.poll() {
            if let Some(m) = self.ai_stash.value.take() {
                let message = ai::clean_message(&m).lines().next().unwrap_or("").to_string();
                cx.run(ops::stash_push(&ops::StashOpts { message, include_untracked: true, ..Default::default() }));
            }
            if let Some(e) = self.ai_stash.error.take() {
                cx.error(e);
            }
        }
        // A rejected commit (say, by a hook) must leave the message in the box.
        if self.submitted.is_some() && !cx.data.ops_busy
            && let Some((msg, head)) = self.submitted.take() {
                let now = cx.repo.resolve("HEAD").unwrap_or_default();
                if now != head {
                    if self.message == msg {
                        self.message.clear();
                    }
                    self.amend = false;
                }
            }
        let Some(st) = cx.status().cloned() else {
            spinner_row(ui, "Reading status…", &p);
            return;
        };
        // During a merge or similar, git prepares the commit message.
        if st.op != RepoOp::None && self.applied_prepared_for != Some(st.op) && self.message.is_empty() {
            if let Some(m) = cx.repo.prepared_message() {
                self.message = m;
            }
            self.applied_prepared_for = Some(st.op);
        }
        if st.op == RepoOp::None {
            self.applied_prepared_for = None;
        }

        egui::Panel::bottom("commit-box").resizable(true).default_size(190.0).show(ui, |ui| self.commit_box(ui, cx, &st));

        egui::ScrollArea::vertical().id_salt("changes-list").auto_shrink([false, false]).show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter files").desired_width(f32::INFINITY));
            self.batch_bar(ui, cx, &st);
            let matches = |e: &StatusEntry, f: &str| f.is_empty() || e.path.to_lowercase().contains(&f.to_lowercase());
            let f = self.filter.clone();

            // Conflicts first: they block everything else.
            let conflicts: Vec<&StatusEntry> = st.conflicts().filter(|e| matches(e, &f)).collect();
            if !conflicts.is_empty() {
                section(ui, &format!("Merge conflicts ({})", conflicts.len()), &p);
                ui.spacing_mut().item_spacing.y = 0.0;
                for e in conflicts {
                    conflict_row(ui, cx, e);
                }
                ui.spacing_mut().item_spacing.y = 4.0;
            }

            let staged: Vec<&StatusEntry> = st.entries.iter().filter(|e| e.is_staged() && matches(e, &f)).collect();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                section(ui, &format!("Staged ({})", staged.len()), &p);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !staged.is_empty() {
                        if ui.small_button("Unstage all").clicked() {
                            cx.run(ops::unstage_all(!st.head_sha.is_empty()));
                        }
                        if ui.small_button("View all").clicked() {
                            cx.diff(DiffSource::Staged, None);
                        }
                    }
                });
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for e in &staged {
                staged_row(ui, cx, e, &st, &mut self.selected);
            }
            ui.spacing_mut().item_spacing.y = 4.0;

            let unstaged: Vec<&StatusEntry> = st.entries.iter().filter(|e| e.is_unstaged() && matches(e, &f)).collect();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                section(ui, &format!("Unstaged ({})", unstaged.len()), &p);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !unstaged.is_empty() {
                        if ui.small_button("Stage all").clicked() {
                            cx.run(ops::stage_all());
                        }
                        if ui.small_button("Stash…").clicked() {
                            cx.form(forms::stash_form(Vec::new()));
                        }
                        if ui.add_enabled(!self.ai_stash.busy(), egui::Button::new("Stash with AI message").small()).clicked() {
                            let (repo, cfg) = (cx.repo.clone(), cx.settings.ai.clone());
                            self.ai_stash.spawn(ui.ctx(), move || -> Result<String, String> {
                                let diff = repo.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?;
                                let (s, u) = ai::stash_prompt(&diff);
                                let key = cred::ai_key(cfg.provider.key_name());
                                ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                            });
                        }
                        if ui.small_button("View all").clicked() {
                            cx.diff(DiffSource::Unstaged, None);
                        }
                    }
                });
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for e in &unstaged {
                unstaged_row(ui, cx, e, &mut self.selected);
            }
            ui.spacing_mut().item_spacing.y = 4.0;
            if st.is_clean() {
                empty_state(ui, "Working tree clean", "Changes appear here as you edit files.", &p);
            }
        });
    }

    fn commit_box(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus) {
        let p = cx.pal.clone();
        ui.add_space(4.0);
        let branch = st.branch.clone().unwrap_or_else(|| "detached HEAD".into());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Commit to {branch}")).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.ai.busy();
                if ui.add_enabled(!busy, egui::Button::new("Generate with AI").small()).on_hover_text("Write a message from the staged diff").clicked() {
                    self.generate(ui.ctx(), cx);
                }
                if busy {
                    ui.add(egui::Spinner::new().size(13.0));
                }
                if ui.small_button("Review with AI").on_hover_text("Look for problems in these changes").clicked() {
                    let extra = cx.settings.ai.custom_instructions.clone();
                    crate::views::textview::ai_tab(cx, "AI Review", move |r| Ok(ai::review_prompt(&extra, &r.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?)));
                }
                if ui.small_button("Explain").on_hover_text("Summarise the working changes with AI").clicked() {
                    crate::views::textview::ai_tab(cx, "Explain changes", move |r| Ok(ai::explain_prompt("uncommitted changes", "(not committed yet)", &r.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?)));
                }
                if ui.small_button("Compose…").on_hover_text("Split the changes into several commits").clicked() {
                    cx.open(Tab::Composer(Box::default()));
                }
            });
        });
        egui::ScrollArea::vertical().id_salt("msg").max_height(ui.available_height() - 62.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.message).hint_text("Message (Ctrl+Enter to commit)").desired_rows(3).desired_width(f32::INFINITY));
        });
        let len = self.message.lines().next().unwrap_or("").chars().count();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.amend, "Amend").changed() && self.amend && self.message.trim().is_empty()
                && let Ok(c) = cx.repo.commit("HEAD") {
                    self.message = c.message();
                }
            ui.checkbox(&mut self.signoff, "Sign-off");
            ui.checkbox(&mut self.sign, "GPG/SSH sign");
            ui.checkbox(&mut self.no_verify, "Skip hooks");
            ui.menu_button("Author ▾", |ui| {
                ui.label("Override author (Name <email>)");
                ui.text_edit_singleline(&mut self.author);
            });
            ui.menu_button("Co-authors ▾", |ui| {
                ui.set_max_height(280.0);
                let people = cx.repo.git.log.lock().ok().map(|_| ()).and_then(|_| cx.repo.contributors("HEAD", false).ok()).unwrap_or_default();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for c in people.iter().filter(|c| !c.is_you).take(60) {
                        if ui.button(format!("{} <{}>", c.name, c.email)).clicked() {
                            let t = format!("Co-authored-by: {} <{}>", c.name, c.email);
                            if !self.message.contains(&t) {
                                let sep = if self.message.contains("Co-authored-by:") { "\n" } else { "\n\n" };
                                self.message = format!("{}{sep}{t}", self.message.trim_end());
                            }
                            ui.close();
                        }
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{len}/72")).small().color(if len > 72 { p.warn } else { p.faint }));
            });
        });
        let staged_n = st.entries.iter().filter(|e| e.is_staged()).count();
        let unstaged_n = st.entries.iter().filter(|e| e.is_unstaged()).count();
        let can = !cx.data.ops_busy && self.submitted.is_none() && !self.message.trim().is_empty() && st.conflicts().next().is_none() && (staged_n > 0 || self.amend || st.op == RepoOp::Merge || unstaged_n > 0);
        ui.horizontal(|ui| {
            let label = if self.amend {
                "Amend commit".to_string()
            } else if staged_n > 0 {
                format!("Commit {staged_n} file(s)")
            } else {
                format!("Stage all and commit {unstaged_n} file(s)")
            };
            let fill = if can { p.accent } else { p.panel2 };
            let b = egui::Button::new(RichText::new(label).color(if can { p.on_accent } else { p.faint })).fill(fill);
            let shortcut = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl);
            if (ui.add_enabled(can, b).clicked() || (shortcut && can)) && can {
                self.commit(cx, staged_n == 0 && !self.amend && st.op != RepoOp::Merge);
            }
        });
    }

    /// Actions for files picked with Ctrl+click.
    fn batch_bar(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus) {
        self.selected.retain(|s| st.entries.iter().any(|e| e.path == *s));
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<String> = self.selected.iter().cloned().collect();
        let tracked: Vec<String> = st.entries.iter().filter(|e| self.selected.contains(&e.path) && e.worktree != FileStatus::Untracked && e.is_unstaged()).map(|e| e.path.clone()).collect();
        ui.horizontal(|ui| {
            ui.label(format!("{} selected", paths.len()));
            if ui.small_button("Stage").clicked() {
                cx.run(ops::stage_paths(&paths));
                self.selected.clear();
            }
            if ui.small_button("Unstage").clicked() {
                cx.run(ops::unstage_paths(&paths, !st.head_sha.is_empty()));
                self.selected.clear();
            }
            if ui.small_button("Stash…").clicked() {
                cx.form(forms::stash_form(paths.clone()));
                self.selected.clear();
            }
            if !tracked.is_empty() && ui.small_button("Discard…").clicked() {
                cx.confirm("Discard changes", &format!("Discard unstaged changes in {} file(s)? This cannot be undone.", tracked.len()), "Discard", ops::discard_paths(&tracked));
                self.selected.clear();
            }
            if ui.small_button("Clear").clicked() {
                self.selected.clear();
            }
        });
    }

    fn commit(&mut self, cx: &mut Cx, stage_all: bool) {
        if stage_all {
            cx.run(ops::stage_all());
        }
        cx.run(ops::commit(&ops::CommitOpts {
            message: self.message.clone(),
            amend: self.amend,
            signoff: self.signoff,
            sign: self.sign,
            no_verify: self.no_verify,
            allow_empty: false,
            author: Some(self.author.trim().to_string()).filter(|s| !s.is_empty()),
            all: false,
        }));
        let head = cx.repo.resolve("HEAD").unwrap_or_default();
        self.submitted = Some((self.message.clone(), head));
    }

    fn generate(&mut self, ctx: &egui::Context, cx: &Cx) {
        let repo = cx.repo.clone();
        let cfg = cx.settings.ai.clone();
        self.ai.spawn(ctx, move || -> Result<String, String> {
            let mut diff = repo.diff_for_ai(&DiffSource::Staged).map_err(|e| e.to_string())?;
            if diff.trim().is_empty() {
                diff = repo.diff_for_ai(&DiffSource::Unstaged).map_err(|e| e.to_string())?;
            }
            if diff.trim().is_empty() {
                return Err("There are no changes to describe. New files must be staged first.".to_string());
            }
            let (s, u) = ai::commit_message_prompt(&cfg.commit_style, &diff);
            let key = cred::ai_key(cfg.provider.key_name());
            ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
        });
    }
}

fn conflict_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry) {
    let p = cx.pal.clone();
    let code = e.conflict.clone().unwrap_or_default();
    let (rect, resp) = row(ui, 24.0, false, &p);
    let tr = egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 0.0), egui::pos2(rect.right() - 40.0, rect.bottom()));
    paint_path(ui, tr, &e.path, &p);
    paint_text(ui, egui::Rect::from_min_max(egui::pos2(rect.right() - 30.0, rect.top()), rect.max), &code, mono(12.0), p.warn);
    let resp = resp.on_hover_text(conflict_help(&code));
    // During a rebase "ours" is the upstream, and "theirs" is your own commit.
    let rebasing = cx.status().is_some_and(|s| matches!(s.op, RepoOp::Rebase | RepoOp::RebaseInteractive));
    let (ours, theirs) = if rebasing { ("upstream's version", "your commit's version") } else { ("ours", "theirs") };
    if resp.clicked() {
        cx.open(Tab::merge(&e.path));
    }
    resp.context_menu(|ui| {
        if ui.button("Resolve in merge tool").clicked() {
            cx.open(Tab::merge(&e.path));
            ui.close();
        }
        if ui.button(format!("Take {ours}…")).clicked() {
            cx.confirm("Resolve conflict", &format!("Replace {} with {ours}? Changes from the other side are discarded.", e.path), "Replace", ops::checkout_conflict_side(&e.path, false));
            ui.close();
        }
        if ui.button(format!("Take {theirs}…")).clicked() {
            cx.confirm("Resolve conflict", &format!("Replace {} with {theirs}? Changes from the other side are discarded.", e.path), "Replace", ops::checkout_conflict_side(&e.path, true));
            ui.close();
        }
        if ui.button("Mark as resolved").clicked() {
            cx.run(ops::mark_resolved(&e.path));
            ui.close();
        }
        if ui.button("Delete file…").clicked() {
            cx.confirm("Delete file", &format!("Resolve the conflict by deleting {}?", e.path), "Delete", ops::remove_conflicted(&e.path));
            ui.close();
        }
    });
}

fn conflict_help(code: &str) -> &'static str {
    match code {
        "UU" => "Both sides modified this file",
        "AA" => "Both sides added this file",
        "DD" => "Both sides deleted this file",
        "AU" => "Added by us, changed by them",
        "UA" => "Added by them, changed by us",
        "DU" => "Deleted by us, modified by them",
        "UD" => "Modified by us, deleted by them",
        _ => "Unmerged",
    }
}

/// Ctrl-click toggles the batch selection. Returns true if it was consumed.
fn select_click(ui: &Ui, resp: &egui::Response, path: &str, selected: &mut HashSet<String>) -> bool {
    if resp.clicked() && ui.input(|i| i.modifiers.command) {
        if !selected.remove(path) {
            selected.insert(path.to_string());
        }
        return true;
    }
    false
}

fn staged_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry, st: &RepoStatus, selected: &mut HashSet<String>) {
    let p = cx.pal.clone();
    let ch = change_of(e, true);
    let resp = file_row(ui, &ch, selected.contains(&e.path), &p);
    if select_click(ui, &resp, &e.path, selected) {
    } else if resp.clicked() {
        cx.diff(DiffSource::Staged, Some(&e.path));
    }
    if resp.double_clicked() {
        cx.run(ops::unstage_paths(std::slice::from_ref(&e.path), !st.head_sha.is_empty()));
    }
    resp.context_menu(|ui| {
        if ui.button("Unstage").clicked() {
            cx.run(ops::unstage_paths(std::slice::from_ref(&e.path), !st.head_sha.is_empty()));
            ui.close();
        }
        if ui.button("Open changes").clicked() {
            cx.diff(DiffSource::Staged, Some(&e.path));
            ui.close();
        }
        ui.separator();
        menus::file_menu(ui, cx, &e.path, None);
    });
}

fn unstaged_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry, selected: &mut HashSet<String>) {
    let p = cx.pal.clone();
    let untracked = e.worktree == FileStatus::Untracked;
    let ch = change_of(e, false);
    let resp = file_row(ui, &ch, selected.contains(&e.path), &p);
    let src = if untracked { DiffSource::Untracked } else { DiffSource::Unstaged };
    if select_click(ui, &resp, &e.path, selected) {
    } else if resp.clicked() {
        cx.diff(src.clone(), Some(&e.path));
    }
    if resp.double_clicked() {
        cx.run(ops::stage_paths(std::slice::from_ref(&e.path)));
    }
    resp.context_menu(|ui| {
        if ui.button("Stage").clicked() {
            cx.run(ops::stage_paths(std::slice::from_ref(&e.path)));
            ui.close();
        }
        if ui.button("Open changes").clicked() {
            cx.diff(src.clone(), Some(&e.path));
            ui.close();
        }
        if ui.button("Stash this file…").clicked() {
            cx.form(forms::stash_form(vec![e.path.clone()]));
            ui.close();
        }
        if untracked {
            if ui.button("Add to .gitignore").clicked() {
                add_to_gitignore(cx, &e.path);
                ui.close();
            }
            if ui.button("Delete file…").clicked() {
                cx.confirm("Delete file", &format!("Permanently delete the untracked file {}?", e.path), "Delete", ops::delete_untracked(std::slice::from_ref(&e.path)));
                ui.close();
            }
        } else if ui.button("Discard changes…").clicked() {
            cx.confirm("Discard changes", &format!("Discard all unstaged changes in {}? This cannot be undone.", e.path), "Discard", ops::discard_paths(std::slice::from_ref(&e.path)));
            ui.close();
        }
        ui.separator();
        menus::file_menu(ui, cx, &e.path, None);
    });
}

pub fn add_to_gitignore(cx: &mut Cx, path: &str) {
    use std::io::Write;
    let file = cx.repo.ignore_path();
    let needs_nl = std::fs::read(&file).map(|b| !b.is_empty() && !b.ends_with(b"\n")).unwrap_or(false);
    let r = std::fs::OpenOptions::new().create(true).append(true).open(&file).and_then(|mut f| {
        if needs_nl {
            f.write_all(b"\n")?;
        }
        f.write_all(format!("/{path}\n").as_bytes())
    });
    match r {
        Ok(()) => cx.toast(format!("Ignored {path}")),
        Err(e) => cx.error(format!("Could not update .gitignore: {e}")),
    }
}
