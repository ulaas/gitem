//! Reflog browser (recover lost work) and the guided bisect view.
use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::DiffSource;

use crate::action::Cx;
use crate::forms;
use crate::task::Task;
use crate::widgets::*;

pub struct ReflogView {
    rev: String,
    entries: Task<Vec<ReflogEntry>>,
    loaded_gen: u64,
    limit: usize,
    filter: String,
}

const REFLOG_PAGE: usize = 1000;

impl Default for ReflogView {
    fn default() -> Self {
        Self { rev: "HEAD".into(), entries: Task::default(), loaded_gen: u64::MAX, limit: REFLOG_PAGE, filter: String::new() }
    }
}

impl ReflogView {
    pub fn key(&self) -> String {
        "reflog".into()
    }
    pub fn title(&self) -> String {
        "Reflog".into()
    }

    fn load(&mut self, ctx: &egui::Context, cx: &Cx) {
        let (r, rev, limit) = (cx.repo.clone(), self.rev.clone(), self.limit);
        self.entries.spawn(ctx, move || r.reflog(&rev, limit));
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.entries.poll();
        if self.loaded_gen != cx.data.status_gen {
            self.loaded_gen = cx.data.status_gen;
            self.load(&ui.ctx().clone(), cx);
        }
        ui.horizontal(|ui| {
            ui.label("Ref");
            let r = ui.add(crate::widgets::input(&mut self.rev).desired_width(160.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.load(&ui.ctx().clone(), cx);
            }
            ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter").desired_width(200.0));
            ui.label(RichText::new("Every position HEAD has been at. Use it to recover lost commits.").small().color(p.faint));
        });
        ui.separator();
        if let Some(e) = &self.entries.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.entries.value.take() else {
            spinner_row(ui, "Reading reflog…", &p);
            return;
        };
        if list.len() >= self.limit
            && let Some(choice) = load_more_bar(ui, "reflog-load-more", list.len(), "entries", REFLOG_PAGE, self.entries.busy(), &p)
        {
            self.limit = choice.apply(self.limit, REFLOG_PAGE);
            self.load(&ui.ctx().clone(), cx);
        }
        let q = self.filter.to_lowercase();
        let shown: Vec<&ReflogEntry> = list.iter().filter(|e| q.is_empty() || e.action.to_lowercase().contains(&q) || e.message.to_lowercase().contains(&q) || e.sha.starts_with(&q)).collect();
        if shown.is_empty() {
            empty_state(ui, "Nothing here", "The reflog is empty or nothing matches.", &p);
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().id_salt("reflog").auto_shrink([false, false]).show_rows(ui, 24.0, shown.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let e = shown[i];
                let (rect, resp) = row(ui, 24.0, false, &p);
                let x = rect.left();
                let cell = |a: f32, w: f32| egui::Rect::from_min_size(egui::pos2(x + a, rect.top()), egui::vec2(w, 24.0));
                paint_text(ui, cell(6.0, 80.0), &e.selector, mono(11.5), p.faint);
                paint_text(ui, cell(90.0, 66.0), gitem_core::util::prefix(&e.sha, 8), mono(11.5), p.dim);
                paint_text(ui, cell(160.0, 110.0), &e.action, prop(12.0), p.series[6]);
                paint_text(ui, cell(274.0, (rect.width() - 400.0).max(60.0)), &e.message, prop(13.0), p.text);
                paint_text(ui, cell(rect.width() - 120.0, 116.0), &time_label(e.date, cx.settings.date_relative), prop(12.0), p.dim);
                if resp.clicked() {
                    cx.inspect(&e.sha);
                }
                resp.context_menu(|ui| {
                    if ui.button("Open changes").clicked() {
                        cx.diff(DiffSource::Commit(e.sha.clone()), None);
                        ui.close();
                    }
                    if ui.button("Create branch here…").clicked() {
                        cx.form(forms::new_branch(Some(e.sha.clone())));
                        ui.close();
                    }
                    if ui.button("Reset current branch here…").clicked() {
                        cx.form(forms::reset_form(e.sha.clone()));
                        ui.close();
                    }
                    if ui.button("Cherry-pick…").clicked() {
                        cx.form(forms::cherry_pick_form(vec![e.sha.clone()]));
                        ui.close();
                    }
                    if ui.button("Copy SHA").clicked() {
                        cx.copy(e.sha.clone());
                        ui.close();
                    }
                });
            }
        });
        self.entries.value = Some(list);
    }
}

#[derive(Default)]
pub struct BisectView {
    state: Option<BisectState>,
    loaded_gen: u64,
    current: Task<Commit>,
    remaining: Task<String>,
    culprit: Task<Commit>,
    run_cmd: String,
}

impl BisectView {
    pub fn key(&self) -> String {
        "bisect".into()
    }
    pub fn title(&self) -> String {
        "Bisect".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.current.poll();
        self.remaining.poll();
        self.culprit.poll();
        if self.state.is_none() || self.loaded_gen != cx.data.status_gen {
            self.loaded_gen = cx.data.status_gen;
            let state = cx.repo.bisect_state();
            if let Some(sha) = state.culprit.clone() {
                let r = cx.repo.clone();
                self.culprit.spawn(ui.ctx(), move || r.commit(&sha));
            }
            self.state = Some(state);
            let r = cx.repo.clone();
            self.current.spawn(ui.ctx(), move || r.commit("HEAD"));
            let r = cx.repo.clone();
            self.remaining.spawn(ui.ctx(), move || -> Result<String, String> {
                let o = r.git.run_raw(Some(&r.root), &["bisect", "visualize", "--oneline"]).map_err(|e| e.to_string())?;
                Ok(o.text().lines().count().to_string())
            });
        }
        let st = self.state.clone().unwrap_or_default();
        if !st.active {
            ui.add_space(8.0);
            ui.heading("Find the commit that introduced a bug");
            ui.label(RichText::new("Bisect walks your history by binary search. You mark commits good or bad until one remains.").color(p.dim));
            ui.add_space(8.0);
            if ui.button("Start bisect…").clicked() {
                cx.form(forms::bisect_start_form("HEAD".to_string(), cx.data.ref_names()));
            }
            return;
        }
        if let Some(sha) = &st.culprit {
            ui.add_space(4.0);
            ui.heading("Bisect found the commit");
            ui.label(RichText::new("This is the first bad one. Ending the bisect goes back to where you started.").color(p.dim));
            ui.add_space(6.0);
            egui::Frame::new().fill(p.panel2).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.label(RichText::new("First bad commit").small().color(p.faint));
                match self.culprit.get() {
                    Some(c) => {
                        ui.label(RichText::new(&c.subject).strong());
                        ui.label(RichText::new(format!("{} · {} · {}", c.short(), c.author_name, time_label(c.commit_time, cx.settings.date_relative))).small().color(p.dim));
                    }
                    None => {
                        ui.label(RichText::new(gitem_core::util::prefix(sha, 10)).monospace());
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Inspect").clicked() {
                    cx.inspect(sha);
                }
                if ui.button("Open changes").clicked() {
                    cx.diff(DiffSource::Commit(sha.clone()), None);
                }
                if ui.button("End bisect").clicked() {
                    cx.run(ops::bisect_reset());
                }
            });
            ui.add_space(8.0);
            egui::CollapsingHeader::new("Bisect log").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.label(RichText::new(&st.log).monospace().small());
                });
            });
            return;
        }
        ui.add_space(4.0);
        ui.heading("Bisect in progress");
        if st.only_skipped {
            ui.label(RichText::new("Only skipped commits are left, so git can't narrow it down any further. Its log below names the candidates.").color(p.warn));
        } else if let Some(n) = self.remaining.get() {
            ui.label(RichText::new(format!("About {n} commit(s) left to check")).color(p.dim));
        }
        if let Some(c) = self.current.get() {
            ui.add_space(6.0);
            egui::Frame::new().fill(p.panel2).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.label(RichText::new("Now testing").small().color(p.faint));
                ui.label(RichText::new(&c.subject).strong());
                ui.label(RichText::new(format!("{} · {} · {}", c.short(), c.author_name, time_label(c.commit_time, cx.settings.date_relative))).small().color(p.dim));
                if ui.small_button("Inspect").clicked() {
                    cx.inspect(&c.sha);
                }
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // A verdict names the commit it's about. Git checks out the next one to test as
            // soon as it gets one, so a second click would otherwise land on a commit never
            // looked at. While the last mark runs, the commit shown is out of date as well.
            let shown = self.current.get().map(|c| c.sha.clone()).filter(|_| !self.current.busy() && !cx.data.ops_busy);
            let mut mark = |ui: &mut Ui, button: egui::Button, verdict: &str| {
                if ui.add_enabled(shown.is_some(), button).clicked()
                    && let Some(sha) = &shown
                {
                    cx.run(ops::bisect_mark_commit(verdict, sha));
                }
            };
            mark(ui, egui::Button::new(RichText::new("✔ Good").color(crate::theme::ink_on(p.good))).fill(p.good), "good");
            mark(ui, egui::Button::new(RichText::new("✖ Bad").color(crate::theme::ink_on(p.danger))).fill(p.danger), "bad");
            mark(ui, egui::Button::new("Skip"), "skip");
            if ui.button("End bisect").clicked() {
                cx.run(ops::bisect_reset());
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Automate with a test command");
            ui.add(crate::widgets::input(&mut self.run_cmd).hint_text("cargo test failing_case").desired_width(280.0));
            if ui.add_enabled(!self.run_cmd.trim().is_empty(), egui::Button::new("Run")).clicked() {
                cx.run(ops::bisect_run(self.run_cmd.trim()));
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new(format!("Marked good: {}", st.good.len())).small().color(p.dim));
        if let Some(b) = &st.bad {
            ui.label(RichText::new(format!("Marked bad: {}", gitem_core::util::prefix(b, 10))).small().color(p.dim));
        }
        egui::CollapsingHeader::new("Bisect log").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                ui.label(RichText::new(&st.log).monospace().small());
            });
        });
    }
}
