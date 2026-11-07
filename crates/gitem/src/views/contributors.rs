//! Contributors with a commit-count bar per person and a year-long activity
//! heatmap. Selecting a person filters the heatmap to their commits.
use chrono::{Datelike, TimeZone};
use egui::{Align2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use gitem_core::model::*;
use gitem_core::util;

use crate::action::Cx;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

const WEEKS: i64 = 53;

pub struct ContributorsView {
    people: Task<Vec<Contributor>>,
    times: Task<Vec<i64>>,
    rev: String,
    with_stats: bool,
    selected: Option<String>,
    loaded_gen: u64,
    filter: String,
}

impl Default for ContributorsView {
    fn default() -> Self {
        Self { people: Task::default(), times: Task::default(), rev: "HEAD".into(), with_stats: false, selected: None, loaded_gen: u64::MAX, filter: String::new() }
    }
}

impl ContributorsView {
    pub fn key(&self) -> String {
        "contributors".into()
    }
    pub fn title(&self) -> String {
        "Contributors".into()
    }

    fn load(&mut self, ctx: &egui::Context, cx: &Cx) {
        let (r, rev, stats) = (cx.repo.clone(), self.rev.clone(), self.with_stats);
        self.people.spawn(ctx, move || r.contributors(&rev, stats));
        self.load_times(ctx, cx);
    }

    fn load_times(&mut self, ctx: &egui::Context, cx: &Cx) {
        let (r, rev, who) = (cx.repo.clone(), self.rev.clone(), self.selected.clone());
        self.times.spawn(ctx, move || r.commit_times(&rev, who.as_deref(), "1 year ago"));
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let ctx = ui.ctx().clone();
        self.people.poll();
        self.times.poll();
        if self.loaded_gen != cx.data.refs_gen {
            self.loaded_gen = cx.data.refs_gen;
            self.load(&ctx, cx);
        }
        ui.horizontal(|ui| {
            ui.label("Revision");
            let r = ui.add(crate::widgets::input(&mut self.rev).desired_width(140.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.load(&ctx, cx);
            }
            if ui.checkbox(&mut self.with_stats, "Lines added and removed").on_hover_text("Slower on large histories").changed() {
                self.load(&ctx, cx);
            }
            ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter").desired_width(180.0));
            if self.people.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.add_space(4.0);

        let title = match &self.selected {
            Some(w) => format!("Commits in the last year by {w}"),
            None => "Commits in the last year, all contributors".to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            if self.selected.is_some() && ui.small_button("Show everyone").clicked() {
                self.selected = None;
                self.load_times(&ctx, cx);
            }
        });
        if let Some(t) = self.times.get() {
            heatmap(ui, t, cx);
        }
        ui.separator();
        if let Some(e) = &self.people.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.people.value.take() else {
            spinner_row(ui, "Counting commits…", &p);
            return;
        };
        let q = self.filter.to_lowercase();
        let max = list.iter().map(|c| c.commits).max().unwrap_or(1).max(1) as f32;
        let total: u32 = list.iter().map(|c| c.commits).sum();
        ui.label(RichText::new(format!("{} people, {total} commits", list.len())).small().color(p.dim));
        let shown: Vec<&Contributor> = list.iter().filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.email.to_lowercase().contains(&q)).collect();
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut clicked = None;
        egui::ScrollArea::vertical().id_salt("contribs").auto_shrink([false, false]).show_rows(ui, 34.0, shown.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let c = shown[i];
                let sel = self.selected.as_deref() == Some(&c.email);
                let (rect, resp) = row(ui, 34.0, sel, &p);
                paint_avatar(ui, pos2(rect.left() + 18.0, rect.center().y), 11.0, &c.name, &c.email, &p, cx.settings.gravatar);
                let name = if c.is_you { format!("{} (you)", c.name) } else { c.name.clone() };
                paint_text(ui, Rect::from_min_size(pos2(rect.left() + 36.0, rect.top() + 2.0), vec2(230.0, 16.0)), &name, prop(13.0), p.text);
                paint_text(ui, Rect::from_min_size(pos2(rect.left() + 36.0, rect.top() + 18.0), vec2(230.0, 14.0)), &c.email, prop(11.0), p.faint);
                // Bar anchored at a shared baseline, value directly labelled.
                let bx = rect.left() + 280.0;
                let bw = (rect.width() - 280.0 - 300.0).max(40.0);
                let bar = Rect::from_min_size(pos2(bx, rect.center().y - 5.0), vec2((c.commits as f32 / max * bw).max(2.0), 10.0));
                ui.painter().rect_filled(bar, 2.0, p.person(&c.email));
                paint_text(ui, Rect::from_min_size(pos2(bar.right() + 6.0, rect.top()), vec2(60.0, 34.0)), &c.commits.to_string(), mono(12.0), p.text);
                let rx = rect.right() - 290.0 + 60.0;
                if self.with_stats {
                    paint_text(ui, Rect::from_min_size(pos2(rx, rect.top()), vec2(60.0, 34.0)), &format!("+{}", c.additions), mono(11.0), p.add_fg);
                    paint_text(ui, Rect::from_min_size(pos2(rx + 62.0, rect.top()), vec2(60.0, 34.0)), &format!("-{}", c.deletions), mono(11.0), p.del_fg);
                }
                paint_text(ui, Rect::from_min_size(pos2(rect.right() - 106.0, rect.top()), vec2(100.0, 34.0)), &time_label(c.last, cx.settings.date_relative), prop(12.0), p.dim);
                if resp.clicked() {
                    clicked = Some(c.email.clone());
                }
                resp.context_menu(|ui| {
                    if ui.button("Search their commits").clicked() {
                        // In brackets: `ann@x` alone would find `joann@x` as well.
                        cx.open(Tab::search(&format!("@:<{}>", c.email)));
                        ui.close();
                    }
                    if ui.button("Copy name and email").clicked() {
                        cx.copy(format!("{} <{}>", c.name, c.email));
                        ui.close();
                    }
                    if ui.button("Copy Co-authored-by line").clicked() {
                        cx.copy(format!("Co-authored-by: {} <{}>", c.name, c.email));
                        ui.close();
                    }
                });
            }
        });
        self.people.value = Some(list);
        if let Some(c) = clicked {
            self.selected = if self.selected.as_deref() == Some(&c) { None } else { Some(c) };
            self.load_times(&ctx, cx);
        }
    }
}

/// Calendar heatmap: weeks across, weekdays down, sequential single hue.
fn heatmap(ui: &mut Ui, times: &[i64], cx: &Cx) {
    let p = cx.pal;
    let now = util::now_ts();
    let today = chrono::Local.timestamp_opt(now, 0).single().map(|d| d.date_naive()).unwrap_or_default();
    let start = today - chrono::Duration::days(today.weekday().num_days_from_sunday() as i64) - chrono::Duration::weeks(WEEKS - 1);
    let mut counts = vec![0u32; (WEEKS * 7) as usize];
    for t in times {
        if let Some(d) = chrono::Local.timestamp_opt(*t, 0).single().map(|d| d.date_naive()) {
            let idx = (d - start).num_days();
            if idx >= 0 && (idx as usize) < counts.len() {
                counts[idx as usize] += 1;
            }
        }
    }
    let max = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
    let cell = 12.0;
    let gap = 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(30.0 + (cell + gap) * WEEKS as f32, 18.0 + (cell + gap) * 7.0 + 18.0), Sense::hover());
    for (i, n) in [(1, "Mon"), (3, "Wed"), (5, "Fri")] {
        ui.painter().text(pos2(rect.left(), rect.top() + 16.0 + (cell + gap) * i as f32 + cell / 2.0), Align2::LEFT_CENTER, n, prop(10.0), p.faint);
    }
    let mut last_month = 13;
    for w in 0..WEEKS {
        let d0 = start + chrono::Duration::weeks(w);
        if d0.month() != last_month {
            last_month = d0.month();
            ui.painter().text(pos2(rect.left() + 30.0 + (cell + gap) * w as f32, rect.top()), Align2::LEFT_TOP, d0.format("%b").to_string(), prop(10.0), p.faint);
        }
        for d in 0..7 {
            let idx = (w * 7 + d) as usize;
            let date = d0 + chrono::Duration::days(d);
            if date > today {
                continue;
            }
            let c = counts[idx];
            let r = Rect::from_min_size(pos2(rect.left() + 30.0 + (cell + gap) * w as f32, rect.top() + 16.0 + (cell + gap) * d as f32), vec2(cell, cell));
            if c == 0 {
                ui.painter().rect_filled(r, 2.0, p.panel2);
            } else {
                ui.painter().rect_filled(r, 2.0, p.heat((c as f32 / max).sqrt()));
            }
            let resp = ui.interact(r.expand(1.0), ui.id().with(("hm", idx)), Sense::hover());
            if resp.hovered() {
                ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, p.text), egui::StrokeKind::Outside);
                resp.on_hover_text(format!("{c} commit{} on {}", if c == 1 { "" } else { "s" }, date.format("%a, %b %-d %Y")));
            }
        }
    }
    // Legend, placed beneath the grid.
    let ly = rect.top() + 16.0 + (cell + gap) * 7.0 + 3.0;
    ui.painter().text(pos2(rect.left() + 30.0, ly), Align2::LEFT_TOP, "Fewer", prop(10.0), p.faint);
    for i in 0..5 {
        let r = Rect::from_min_size(pos2(rect.left() + 64.0 + i as f32 * (cell + gap), ly), vec2(cell, cell));
        ui.painter().rect_filled(r, 2.0, p.heat(i as f32 / 4.0));
    }
    ui.painter().text(pos2(rect.left() + 64.0 + 5.0 * (cell + gap) + 4.0, ly), Align2::LEFT_TOP, format!("More (busiest day: {max:.0})"), prop(10.0), p.faint);
}
