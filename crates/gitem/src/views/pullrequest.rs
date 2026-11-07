//! One pull request in full: what it says, its checks, its conversation, its changed files, and
//! a verdict or a merge from here. Lists are in Launchpad.
use egui::{RichText, Ui};
use gitem_core::forge::details::{CheckState, MergeStyle, PatchLineKind, PrDetails, Verdict, patch_lines};
use gitem_core::forge::{PullRequest, RemoteRepo};

use crate::action::{Action, Cx, Later};
use crate::forms::FormDialog;
use crate::task::Task;
use crate::views::launchpad::resolve_client;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Overview,
    Files,
    Checks,
    Conversation,
}

pub struct PullRequestView {
    pr: PullRequest,
    /// Who you are on this host, as far as Launchpad found out.
    me: String,
    page: Page,
    details: Task<PrDetails>,
    /// The `forge_gen` of what's loaded, and whether anything has been asked for yet.
    loaded_for: Option<u64>,
    selected_file: usize,
    /// A review being written.
    review: String,
    /// A new-side line in the selected file with a comment being written on it.
    comment_on: Option<u32>,
    comment: String,
    posting: Task<String>,
}

/// A merge, with a choice of method where the host takes one.
pub fn merge_form(cx_git: &gitem_core::GitExe, repo: &RemoteRepo, pr: &PullRequest) -> FormDialog {
    let (n, g, r2) = (pr.number, cx_git.clone(), repo.clone());
    let styles = repo.kind.merge_styles();
    let mut options = vec!["The host's default".to_string()];
    options.extend(styles.iter().map(|s| s.label().to_string()));
    let refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let mut form = FormDialog::new(format!("Merge #{n} on {}", repo.kind.label()), "Merge").danger().info(format!("Merge \"{}\" into {} on the host?", pr.title, pr.base));
    if !styles.is_empty() {
        form = form.choice("style", "How", &refs, 0);
    }
    let picks: Vec<MergeStyle> = styles.to_vec();
    form.on_submit(move |v| {
        let style = if picks.is_empty() { None } else { v.index("style").checked_sub(1).and_then(|i| picks.get(i).copied()) };
        let (g, r2) = (g.clone(), r2.clone());
        Ok(vec![Action::Later(Box::new(move || {
            resolve_client(&g, &r2)?.merge_pr_as(n, style).map_err(|e| e.to_string())?;
            Ok(Later::ForgeChanged(format!("Merged #{n}")))
        }))])
    })
}

impl PullRequestView {
    pub fn new(pr: PullRequest, me: &str) -> Self {
        Self { pr, me: me.to_string(), page: Page::Overview, details: Task::default(), loaded_for: None, selected_file: 0, review: String::new(), comment_on: None, comment: String::new(), posting: Task::default() }
    }

    pub fn key(&self) -> String {
        format!("pr:{}", self.pr.number)
    }

    pub fn title(&self) -> String {
        format!("#{} {}", self.pr.number, gitem_core::util::prefix(&self.pr.title, 24))
    }

    /// What closing this tab would lose: a review or a comment being written.
    pub fn unsaved(&self) -> Option<String> {
        (!self.review.trim().is_empty() || !self.comment.trim().is_empty()).then(|| "A review you're writing.".to_string())
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.details.poll();
        if self.posting.poll() {
            if let Some(m) = self.posting.value.take() {
                self.review.clear();
                self.comment.clear();
                self.comment_on = None;
                cx.out.push(Action::ForgeChanged(m));
            }
            if let Some(e) = self.posting.error.take() {
                cx.error(e);
            }
        }
        let Some(repo) = cx.forge().cloned() else {
            empty_state(ui, "No hosting provider", "This repository no longer has a remote on a supported host.", &p);
            return;
        };
        if self.loaded_for != Some(cx.data.forge_gen) {
            self.loaded_for = Some(cx.data.forge_gen);
            let (g, r, n) = (cx.repo.git.clone(), repo.clone(), self.pr.number);
            self.details.spawn(ui.ctx(), move || -> Result<PrDetails, String> { resolve_client(&g, &r)?.pr_details(n).map_err(|e| e.to_string()) });
        }
        ui.horizontal(|ui| {
            web_link(ui, format!("#{}", self.pr.number), &self.pr.url);
            ui.add(egui::Label::new(RichText::new(&self.pr.title).strong()).truncate());
        });
        ui.label(RichText::new(format!("{} · {} → {}", self.pr.author, self.pr.head, self.pr.base)).small().color(p.dim));
        let (files, checks, talk) = self.details.get().map_or((0, 0, 0), |d| (d.files.len(), d.checks.len(), d.comments.len()));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::Overview, "Overview");
            ui.selectable_value(&mut self.page, Page::Files, format!("Files ({files})"));
            ui.selectable_value(&mut self.page, Page::Checks, format!("Checks ({checks})"));
            ui.selectable_value(&mut self.page, Page::Conversation, format!("Conversation ({talk})"));
            if self.details.busy() || self.posting.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.separator();
        if let Some(e) = &self.details.error {
            error_box(ui, e, &p);
        }
        for why in self.details.get().map(|d| d.problems.clone()).unwrap_or_default() {
            ui.colored_label(p.warn, why);
        }
        match self.page {
            Page::Overview => self.overview(ui, cx, &repo),
            Page::Files => self.files(ui, cx, &repo),
            Page::Checks => self.checks(ui, cx),
            Page::Conversation => self.conversation(ui, cx),
        }
    }

    fn overview(&mut self, ui: &mut Ui, cx: &mut Cx, repo: &RemoteRepo) {
        let p = cx.pal.clone();
        egui::ScrollArea::vertical().id_salt("pr-overview").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.pr.draft {
                    chip(ui, "Draft", p.dim);
                }
                if self.pr.mergeable == "CONFLICTING" {
                    chip(ui, "⚠ Conflicts", p.danger);
                }
                match self.pr.review_decision.as_str() {
                    "APPROVED" => chip(ui, "Approved", p.good),
                    "CHANGES_REQUESTED" => chip(ui, "Changes requested", p.warn),
                    _ => {}
                }
                if let Some(d) = self.details.get() {
                    let failed = d.checks.iter().filter(|c| c.state == CheckState::Failed).count();
                    let running = d.checks.iter().filter(|c| c.state == CheckState::Running).count();
                    if failed > 0 {
                        chip(ui, &format!("{failed} checks failing"), p.danger);
                    } else if running > 0 {
                        chip(ui, &format!("{running} checks running"), p.warn);
                    } else if !d.checks.is_empty() {
                        chip(ui, "Checks pass", p.good);
                    }
                }
            });
            ui.add_space(4.0);
            if self.pr.body.trim().is_empty() {
                ui.label(RichText::new("No description.").italics().color(p.dim));
            } else {
                ui.add(egui::Label::new(&self.pr.body).wrap());
            }
            ui.separator();
            let refusal = self.pr.merge_refusal();
            if ui.add_enabled(refusal.is_none(), egui::Button::new("Merge…")).on_disabled_hover_text(refusal.unwrap_or_default()).clicked() {
                cx.form(merge_form(&cx.repo.git, repo, &self.pr));
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Review").strong());
            ui.add(crate::widgets::text_area(&mut self.review).hint_text("What you have to say about it (Markdown)").desired_rows(4).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                let yours = self.pr.is_by(&self.me);
                let why = "It's your own pull request, which hosts don't let you approve or reject.";
                if ui.add_enabled(!yours, egui::Button::new("Approve")).on_disabled_hover_text(why).clicked() {
                    self.send(cx, ui.ctx(), repo, Verdict::Approve);
                }
                if repo.kind.can_request_changes() && ui.add_enabled(!yours && !self.review.trim().is_empty(), egui::Button::new("Request changes")).on_disabled_hover_text(if yours { why } else { "Say what needs changing first." }).clicked() {
                    self.send(cx, ui.ctx(), repo, Verdict::RequestChanges);
                }
                if ui.add_enabled(!self.review.trim().is_empty(), egui::Button::new("Comment")).on_disabled_hover_text("Write something first.").clicked() {
                    self.send(cx, ui.ctx(), repo, Verdict::Comment);
                }
            });
        });
    }

    /// What was written stays in its box until the host has taken it.
    fn send(&mut self, cx: &mut Cx, ctx: &egui::Context, repo: &RemoteRepo, verdict: Verdict) {
        let (g, r, n, body) = (cx.repo.git.clone(), repo.clone(), self.pr.number, self.review.clone());
        let done = match verdict {
            Verdict::Approve => "Approved",
            Verdict::RequestChanges => "Requested changes on",
            Verdict::Comment => "Commented on",
        };
        self.posting.spawn(ctx, move || -> Result<String, String> {
            resolve_client(&g, &r)?.review_pr(n, verdict, &body).map_err(|e| e.to_string())?;
            Ok(format!("{done} #{n}"))
        });
    }

    fn files(&mut self, ui: &mut Ui, cx: &mut Cx, repo: &RemoteRepo) {
        let p = cx.pal.clone();
        let Some(d) = self.details.get().cloned() else {
            spinner_row(ui, "Loading its files…", &p);
            return;
        };
        if d.files.is_empty() {
            empty_state(ui, "No files listed", "The host listed no changed files.", &p);
            return;
        }
        self.selected_file = self.selected_file.min(d.files.len() - 1);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(260.0);
                egui::ScrollArea::vertical().id_salt("pr-files").auto_shrink([false, false]).show(ui, |ui| {
                    for (i, f) in d.files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(i == self.selected_file, gitem_core::util::prefix(&f.path, 200)).on_hover_text(&f.path).clicked() {
                                self.selected_file = i;
                                self.comment_on = None;
                            }
                            if f.additions + f.deletions > 0 {
                                ui.label(RichText::new(format!("+{} -{}", f.additions, f.deletions)).small().color(p.dim));
                            }
                        });
                    }
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                let f = &d.files[self.selected_file];
                ui.label(RichText::new(if f.old_path.is_empty() || f.old_path == f.path { f.path.clone() } else { format!("{} → {}", f.old_path, f.path) }).strong());
                if f.patch.is_empty() {
                    ui.label(RichText::new("The host gives no diff for this file, either because it's binary or large, or because this host doesn't give any. Check out the pull request and use Compare to read it here.").color(p.dim));
                    return;
                }
                let lines = patch_lines(&f.patch);
                let noted: Vec<(u32, String)> = d.comments.iter().filter(|c| c.path.as_deref() == Some(f.path.as_str())).filter_map(|c| c.line.map(|l| (l, format!("{}: {}", c.author, c.body)))).collect();
                egui::ScrollArea::both().id_salt(("pr-patch", self.selected_file)).auto_shrink([false, false]).show(ui, |ui| {
                    for l in &lines {
                        let (colour, sign) = match l.kind {
                            PatchLineKind::Hunk => (p.dim, ""),
                            PatchLineKind::Added => (p.add_fg, "+"),
                            PatchLineKind::Removed => (p.del_fg, "-"),
                            PatchLineKind::Context => (p.text, " "),
                        };
                        ui.horizontal(|ui| {
                            let number = l.new.or(l.old).map_or(String::new(), |n| format!("{n:>5}"));
                            // A comment goes on the new side, so that's where there's a button.
                            if let Some(n) = l.new {
                                if ui.small_button(RichText::new(&number).monospace().color(p.faint)).on_hover_text("Comment on this line").clicked() {
                                    self.comment_on = Some(n);
                                }
                            } else {
                                ui.label(RichText::new(&number).monospace().color(p.faint));
                            }
                            ui.label(RichText::new(format!("{sign}{}", l.text)).monospace().color(colour));
                        });
                        if let Some(n) = l.new {
                            for (_, note) in noted.iter().filter(|(at, _)| *at == n) {
                                ui.label(RichText::new(note).small().color(p.series_text(6)));
                            }
                            if self.comment_on == Some(n) {
                                ui.add(crate::widgets::text_area(&mut self.comment).hint_text(format!("Comment on line {n}")).desired_rows(3).desired_width(480.0));
                                ui.horizontal(|ui| {
                                    if ui.add_enabled(!self.comment.trim().is_empty(), egui::Button::new("Post comment")).on_disabled_hover_text("Write something first.").clicked() {
                                        let (g, r, num, details, file, body) = (cx.repo.git.clone(), repo.clone(), self.pr.number, d.clone(), f.clone(), self.comment.clone());
                                        self.posting.spawn(ui.ctx(), move || -> Result<String, String> {
                                            resolve_client(&g, &r)?.comment_line(num, &details, &file, n, &body).map_err(|e| e.to_string())?;
                                            Ok(format!("Commented on line {n} of {}", file.path))
                                        });
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.comment_on = None;
                                        self.comment.clear();
                                    }
                                });
                            }
                        }
                    }
                });
            });
        });
    }

    fn checks(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let Some(d) = self.details.get().cloned() else {
            spinner_row(ui, "Loading its checks…", &p);
            return;
        };
        if d.checks.is_empty() {
            empty_state(ui, "No checks", "The host reports no checks on this pull request's latest commit.", &p);
            return;
        }
        ui.label(RichText::new("A check's name opens its log, which stays on the host.").small().color(p.faint));
        egui::ScrollArea::vertical().id_salt("pr-checks").auto_shrink([false, false]).show(ui, |ui| {
            for c in &d.checks {
                ui.horizontal(|ui| {
                    let (word, colour) = match c.state {
                        CheckState::Passed => ("Passed", p.good),
                        CheckState::Failed => ("Failed", p.danger),
                        CheckState::Running => ("Running", p.warn),
                        CheckState::Other => ("Other", p.dim),
                    };
                    chip(ui, word, colour);
                    if c.url.is_empty() {
                        ui.label(&c.name);
                    } else {
                        web_link(ui, &c.name, &c.url);
                    }
                    if !c.detail.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&c.detail).small().color(p.dim)).truncate());
                    }
                });
            }
        });
    }

    fn conversation(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let Some(d) = self.details.get().cloned() else {
            spinner_row(ui, "Loading its conversation…", &p);
            return;
        };
        if d.comments.is_empty() {
            empty_state(ui, "No conversation yet", "Comments and reviews appear here.", &p);
            return;
        }
        egui::ScrollArea::vertical().id_salt("pr-talk").auto_shrink([false, false]).show(ui, |ui| {
            for c in &d.comments {
                egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&c.author).strong());
                        if c.when > 0 {
                            ui.label(RichText::new(time_label(c.when, cx.settings.date_relative)).small().color(p.dim));
                        }
                        match c.verdict.as_str() {
                            "" | "COMMENTED" | "COMMENT" => {}
                            "APPROVED" => chip(ui, "Approved", p.good),
                            "CHANGES_REQUESTED" | "REQUEST_CHANGES" => chip(ui, "Changes requested", p.warn),
                            other => chip(ui, other, p.dim),
                        }
                        if let (Some(path), line) = (&c.path, c.line) {
                            let where_ = line.map_or(path.clone(), |l| format!("{path}:{l}"));
                            if ui.small_button(where_).on_hover_text("Show this file").clicked()
                                && let Some(i) = d.files.iter().position(|f| f.path == *path)
                            {
                                self.selected_file = i;
                                self.page = Page::Files;
                            }
                        }
                    });
                    if !c.body.trim().is_empty() {
                        ui.add(egui::Label::new(&c.body).wrap());
                    }
                });
                ui.add_space(3.0);
            }
        });
    }
}
