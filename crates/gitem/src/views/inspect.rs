//! Commit details panel ("Inspect"): metadata, message with autolinks, signature,
//! containing refs, notes, changed files, AI explanation, and quick actions.
use std::time::{Duration, Instant};

use egui::{RichText, Sense, Ui, vec2};
use gitem_core::cred;
use gitem_core::model::*;
use gitem_core::repo::DiffSource;
use gitem_core::{ai, forge, util};

use crate::action::Cx;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

pub struct Details {
    pub commit: Commit,
    pub files: Vec<FileChange>,
    pub signature: SignatureInfo,
    pub note: Option<String>,
}

/// What it takes every ref in the repository to answer, loaded when asked for.
#[derive(Default)]
struct RefsInfo {
    branches: Vec<String>,
    tags: Vec<String>,
    describe: Option<String>,
}

/// Selections closer together than this are someone moving through the list.
const SELECTION_SETTLES: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct InspectPanel {
    pub sha: Option<String>,
    loaded: Option<String>,
    /// What had been written when it was loaded. A note, branch or tag can change what it shows.
    loaded_at: (u64, u64),
    details: Task<Details>,
    explanation: Task<String>,
    explained_for: Option<String>,
    selected_file: Option<String>,
    show_refs: bool,
    /// The refs that hold `loaded`, once someone has asked.
    refs_info: Task<RefsInfo>,
    refs_info_for: Option<String>,
    /// When the selection last changed, and to what.
    last_change: Option<(String, Instant)>,
    moving: bool,
    /// Past the first few refs, shown on request.
    all_refs: bool,
    /// The pull requests the commit came in by, once someone has asked. For that commit only.
    prs: Task<Vec<gitem_core::forge::details::CommitPr>>,
    prs_for: Option<String>,
}

fn load_details(repo: &gitem_core::Repo, sha: &str) -> Result<Details, String> {
    let e = |x: gitem_core::Error| x.to_string();
    let commit = repo.commit(sha).map_err(e)?;
    let files = repo.commit_files(sha).map_err(e)?;
    let signature = repo.signature(sha).unwrap_or_default();
    let note = repo.commit_note(sha);
    Ok(Details { commit, files, signature, note })
}

impl InspectPanel {
    pub fn set(&mut self, sha: &str) {
        self.sha = Some(sha.to_string());
    }

    /// The commit whose details are on hand to show.
    pub fn showing(&self) -> Option<&str> {
        self.details.get().map(|d| d.commit.sha.as_str())
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.details.poll();
        self.explanation.poll();
        self.refs_info.poll();
        let Some(sha) = self.sha.clone() else {
            empty_state(ui, "No commit selected", "Pick a commit to inspect it here.", &p);
            return;
        };
        let written = (cx.data.refs_gen, cx.data.writes);
        if self.loaded.as_deref() == Some(&sha) && self.loaded_at != written && !self.details.busy() {
            // Same commit, so what's on screen stays until the new details are here.
            self.loaded_at = written;
            self.refs_info = Task::default();
            self.refs_info_for = None;
            let s = sha.clone();
            self.details.spawn_git(ui.ctx(), cx.repo, move |repo| load_details(&repo, &s));
        }
        if self.loaded.as_deref() != Some(&sha) {
            // Holding an arrow key mustn't start a load per commit. Nothing about the last
            // commit stays on screen meanwhile, so it can't be taken for this one's.
            if self.last_change.as_ref().is_none_or(|(s, _)| *s != sha) {
                // A click after a pause loads at once. One soon after the last is a key held.
                self.moving = self.last_change.as_ref().is_some_and(|(_, t)| t.elapsed() < SELECTION_SETTLES * 2);
                self.last_change = Some((sha.clone(), Instant::now()));
            }
            let since_change = self.last_change.as_ref().map_or(SELECTION_SETTLES, |(_, t)| t.elapsed());
            if self.moving && since_change < SELECTION_SETTLES {
                self.details.stop();
                self.details.value = None;
                self.details.error = None;
                ui.ctx().request_repaint_after(SELECTION_SETTLES - since_change);
                spinner_row(ui, "Loading commit…", &p);
                return;
            }
            self.loaded = Some(sha.clone());
            self.loaded_at = written;
            self.selected_file = None;
            let s = sha.clone();
            self.details.value = None;
            self.details.error = None;
            self.refs_info = Task::default();
            self.refs_info_for = None;
            self.details.spawn_git(ui.ctx(), cx.repo, move |repo| load_details(&repo, &s));
        }
        if let Some(e) = &self.details.error {
            error_box(ui, e, &p);
            return;
        }
        let Some(d) = self.details.value.take() else {
            spinner_row(ui, "Loading commit…", &p);
            return;
        };
        egui::ScrollArea::vertical().id_salt("inspect").auto_shrink([false, false]).show(ui, |ui| {
            self.content(ui, cx, &d);
        });
        self.details.value = Some(d);
    }

    fn content(&mut self, ui: &mut Ui, cx: &mut Cx, d: &Details) {
        let p = cx.pal.clone();
        let c = &d.commit;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
            paint_avatar(ui, r.center(), 16.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
            ui.vertical(|ui| {
                ui.label(RichText::new(&c.author_name).strong()).on_hover_text(&c.author_email);
                ui.label(RichText::new(format!("{} · {}", util::relative_time(c.author_time, util::now_ts()), util::format_date(c.author_time))).small().color(p.dim));
            });
        });
        if c.committer_email != c.author_email {
            ui.label(RichText::new(format!("Committed by {}", c.committer_name)).small().color(p.dim));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(RichText::new(c.short()).monospace()).on_hover_text("Copy full SHA").clicked() {
                cx.copy(c.sha.clone());
            }
            for par in &c.parents {
                if crate::widgets::small_button(ui, RichText::new(format!("\u{E08E} {}", gitem_core::util::prefix(par, 7))).monospace()).on_hover_text("Go to parent").clicked() {
                    cx.inspect(par);
                }
            }
            if let Some(f) = cx.forge() {
                let url = f.commit_url(&c.sha);
                if crate::widgets::small_button(ui, "Open on remote").clicked() {
                    cx.url(url);
                }
            }
            ui.menu_button("More \u{E136}", |ui| menus::commit_menu(ui, cx, c));
            if crate::chrome::icon_button(ui, crate::chrome::icon::GIT_BRANCH, "Check out this commit", "With a detached HEAD", false, &p).clicked() {
                cx.confirm("Check out commit", &format!("Check out {} with a detached HEAD?", c.short()), "Check out", gitem_core::ops::checkout_detached(&c.sha).lossless());
            }
            if crate::chrome::icon_button(ui, crate::chrome::icon::ARROW_ELBOW_DOWN_RIGHT, "Cherry-pick this commit", "Apply it to the current branch", false, &p).clicked() {
                cx.form(crate::forms::cherry_pick_form(vec![c.sha.clone()]));
            }
            if crate::chrome::icon_button(ui, crate::chrome::icon::ARROW_U_UP_LEFT, "Revert this commit", "Add a commit that undoes it", false, &p).clicked() {
                cx.form(crate::forms::revert_form(c.sha.clone(), c.is_merge()));
            }
        });
        self.prs.poll();
        if let Some(remote) = cx.forge().cloned() {
            let asked = self.prs_for.as_deref() == Some(c.sha.as_str());
            ui.horizontal_wrapped(|ui| {
                if !asked && crate::widgets::small_button(ui, "Find pull request").on_hover_text("Asks the host").clicked() {
                    self.prs_for = Some(c.sha.clone());
                    let (git, sha) = (cx.repo.git.clone(), c.sha.clone());
                    self.prs.spawn(ui.ctx(), move || -> Result<Vec<gitem_core::forge::details::CommitPr>, String> { crate::views::launchpad::resolve_client(&git, &remote)?.prs_for_commit(&sha).map_err(|e| e.to_string()) });
                }
                if asked {
                    if self.prs.busy() {
                        ui.add(spinner(13.0));
                    } else if let Some(e) = &self.prs.error {
                        ui.colored_label(p.warn, e);
                    } else if let Some(list) = self.prs.get() {
                        if list.is_empty() {
                            ui.label(RichText::new("No pull request has this commit.").small().color(p.dim));
                        }
                        for pr in list {
                            crate::widgets::web_link(ui, format!("#{} {}", pr.number, pr.title), &pr.url);
                            chip(ui, &pr.state.to_lowercase(), p.dim);
                        }
                    }
                }
            });
        }
        match d.signature.status.as_str() {
            "G" => chip(ui, &format!("\u{E182} Signed by {}", d.signature.signer), p.good),
            "U" | "X" | "Y" => chip(ui, &format!("Signature from {} is untrusted or expired", d.signature.signer), p.warn),
            "B" | "R" => chip(ui, "\u{E4F6} Bad signature", p.danger),
            "E" => chip(ui, &format!("Signed, but key {} is unavailable", d.signature.key), p.warn),
            _ => {}
        }
        ui.add_space(4.0);

        // Message with autolinks.
        let forge_repo = cx.forge().cloned();
        let links = forge::find_links(&c.subject, &cx.settings.autolinks, forge_repo.as_ref());
        ui.scope(|ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
            linked_text(ui, &c.subject, &links, p.text);
        });
        if !c.body.is_empty() {
            // Hard wrapped for a terminal, it would read raggedly in a panel of any other width.
            let body = util::unwrap_paragraphs(&c.body);
            let links = forge::find_links(&body, &cx.settings.autolinks, forge_repo.as_ref());
            linked_text(ui, &body, &links, p.dim);
        }
        if let Some(n) = &d.note {
            ui.add_space(4.0);
            egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(3.0).show(ui, |ui| {
                ui.label(RichText::new("Note").small().color(p.faint));
                ui.label(n.trim());
            });
        }

        // Refs on this commit.
        if !c.refs.is_empty() {
            ui.add_space(4.0);
            // A commit with dozens of refs mustn't push its files out of sight.
            const SHOWN: usize = 6;
            let badges = crate::views::graph::badges_for(&c.refs);
            let more = badges.len().saturating_sub(SHOWN);
            ui.horizontal_wrapped(|ui| {
                for (r, _) in badges.iter().take(if self.all_refs { badges.len() } else { SHOWN }) {
                    let w = badge_width(ui, r);
                    let (rect, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
                    paint_badge(ui, rect.min, 18.0, r, &p);
                }
                if more > 0 && crate::widgets::small_button(ui, if self.all_refs { "Fewer".to_string() } else { format!("+{more}") }).clicked() {
                    self.all_refs = !self.all_refs;
                }
            });
        }
        // These look at every ref in the repository, about a second and a half with thousands
        // of them, so only when someone wants to know.
        if self.show_refs && self.refs_info_for.as_deref() != Some(&c.sha) {
            self.refs_info_for = Some(c.sha.clone());
            let s = c.sha.clone();
            self.refs_info.spawn_git(ui.ctx(), cx.repo, move |repo| -> Result<RefsInfo, String> {
                let (branches, tags) = repo.containing_refs(&s).map_err(|e| e.to_string())?;
                Ok(RefsInfo { branches, tags, describe: repo.describe(&s) })
            });
        }
        let info = self.refs_info.get().filter(|_| self.refs_info_for.as_deref() == Some(&c.sha));
        ui.horizontal(|ui| {
            let text = match info {
                Some(i) => format!("On {}, {}", plural(i.branches.len(), "branch", "branches"), plural(i.tags.len(), "tag", "tags")),
                None => "Branches and tags with this commit".to_string(),
            };
            let caret = if self.show_refs { crate::chrome::icon::CARET_DOWN } else { crate::chrome::icon::CARET_RIGHT };
            let r = ui.add(egui::Button::new(RichText::new(format!("{caret} {text}")).small().color(p.dim)).frame_when_inactive(false).small());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text.as_str()));
            if r.clicked() {
                self.show_refs = !self.show_refs;
            }
            if self.show_refs && self.refs_info.busy() {
                ui.add(spinner(12.0));
            }
        });
        if self.show_refs {
            if let Some(e) = &self.refs_info.error {
                error_box(ui, e, &p);
            }
            if let Some(i) = info {
                if let Some(x) = &i.describe {
                    ui.label(RichText::new(format!("Describes as {x}")).small().color(p.faint));
                }
                for b in i.branches.iter().take(50) {
                    ui.label(RichText::new(format!("  {b}")).small().color(p.dim));
                }
                for t in i.tags.iter().take(50) {
                    ui.label(RichText::new(format!("  ◆ {t}")).small().color(p.dim));
                }
            }
        }

        // AI explain
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.explanation.busy();
            if ui.add_enabled(!busy, egui::Button::new("Explain with AI")).on_disabled_hover_text("Already explaining this commit.").clicked() {
                self.explain(ui.ctx(), cx, c);
            }
            if busy {
                ui.add(spinner(13.0));
            }
        });
        if self.explained_for.as_deref() == Some(&c.sha) {
            if let Some(e) = &self.explanation.error {
                error_box(ui, e, &p);
            } else if let Some(t) = self.explanation.get() {
                egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(3.0).show(ui, |ui| {
                    ui.label(t.as_str());
                });
            }
        }

        // Files
        let (add, del) = d.files.iter().fold((0u32, 0u32), |(a, b), f| (a + f.additions.unwrap_or(0), b + f.deletions.unwrap_or(0)));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} changed", plural(d.files.len(), "file", "files"))).strong());
            ui.label(RichText::new(format!("+{add}")).color(p.add_fg).monospace());
            ui.label(RichText::new(format!("-{del}")).color(p.del_fg).monospace());
            if crate::widgets::small_button(ui, "Open all").clicked() {
                cx.diff(DiffSource::Commit(c.sha.clone()), None);
            }
        });
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        visible_rows(ui, 22.0, d.files.len(), |ui, range| {
            for f in &d.files[range] {
                let sel = self.selected_file.as_deref() == Some(&f.path);
                let resp = keyed_file_row(ui, f, sel, &p, "details files").on_hover_text(&f.path);
                if resp.clicked() {
                    self.selected_file = Some(f.path.clone());
                    cx.diff(DiffSource::Commit(c.sha.clone()), Some(&f.path));
                }
                resp.context_menu(|ui| {
                    if ui.button("Open changes").clicked() {
                        cx.diff(DiffSource::Commit(c.sha.clone()), Some(&f.path));
                        ui.close();
                    }
                    if ui.button("Open this revision").clicked() {
                        cx.open(Tab::file(&f.path, Some(c.sha.clone()), None));
                        ui.close();
                    }
                    menus::file_menu(ui, cx, &f.path, Some(&c.sha));
                });
            }
        });
    }

    fn explain(&mut self, ctx: &egui::Context, cx: &Cx, c: &Commit) {
        self.explained_for = Some(c.sha.clone());
        let repo = cx.repo.clone();
        let cfg = cx.settings.ai.clone();
        let (sha, msg) = (c.sha.clone(), c.message());
        self.explanation.value = None;
        self.explanation.spawn(ctx, move || -> Result<String, String> {
            let diff = repo.diff_for_ai(&DiffSource::Commit(sha)).map_err(|e| e.to_string())?;
            let (s, u) = ai::explain_prompt("a git commit", &msg, &diff);
            let key = cred::ai_key(cfg.provider.key_name());
            ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
        });
    }
}
