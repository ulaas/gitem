//! Visual file history: commits on a time axis with one lane per author, where
//! bubble area shows lines changed, and a bar strip below shows churn.
use std::collections::{HashMap, HashSet};

use egui::{Align2, Rect, RichText, Sense, Stroke, Ui, pos2};
use gitem_core::model::*;
use gitem_core::repo::DiffSource;
use gitem_core::util;

use crate::action::Cx;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

/// Authors beyond this many fold into "Other" rather than inventing hues.
const MAX_AUTHORS: usize = 8;
const VISUAL_PAGE: usize = 5000;

pub struct VisualView {
    limit: usize,
    /// A file or folder; empty means the whole branch.
    path: String,
    rev: String,
    points: Task<Vec<FileHistoryPoint>>,
    started: bool,
    selected: Option<String>,
    /// Zoomed time window; `None` shows everything.
    window: Option<(i64, i64)>,
    /// One file was asked for. Otherwise a point's own path is just one of the commit's files.
    one_file: bool,
    /// The commit a menu was opened on. The menu outlives the pointer leaving its bubble.
    menu_for: Option<FileHistoryPoint>,
    /// Worked out once per history: it's every commit, and the frame shouldn't have to.
    lanes: Option<Lanes>,
}

/// Who gets a lane of their own, in order of first appearance.
struct Lanes {
    names: Vec<String>,
    kept: HashSet<String>,
}

fn lanes_of(pts: &[FileHistoryPoint]) -> Lanes {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for x in pts {
        *counts.entry(x.author.as_str()).or_default() += 1;
    }
    let mut by_count: Vec<(&str, usize)> = counts.iter().map(|(a, c)| (*a, *c)).collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let kept: HashSet<String> = by_count.iter().take(MAX_AUTHORS - usize::from(by_count.len() > MAX_AUTHORS)).map(|x| x.0.to_string()).collect();
    let mut names: Vec<String> = Vec::new();
    for x in pts.iter().rev() {
        if kept.contains(&x.author) && !names.contains(&x.author) {
            names.push(x.author.clone());
        }
    }
    if by_count.len() > kept.len() {
        names.push(format!("Other ({})", by_count.len() - kept.len()));
    }
    Lanes { names, kept }
}

impl VisualView {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_string(), rev: "HEAD".to_string(), points: Task::default(), started: false, selected: None, window: None, limit: VISUAL_PAGE, one_file: false, menu_for: None, lanes: None }
    }

    /// The history of another branch or commit.
    pub fn on_rev(mut self, rev: &str) -> Self {
        self.rev = rev.to_string();
        self
    }

    pub fn key(&self) -> String {
        format!("visual:{}:{}", self.path, self.rev)
    }

    pub fn title(&self) -> String {
        if self.path.is_empty() { "Visual".to_string() } else { format!("{} visual", self.path.rsplit('/').next().unwrap_or(&self.path)) }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.points.poll() {
            self.lanes = None;
        }
        if !self.started {
            self.started = true;
            self.selected = None;
            let repo = cx.repo.clone();
            let (path, rev, limit) = (self.path.clone(), self.rev.clone(), self.limit);
            // Renames can only be followed for a single file on the current history.
            let on_disk = !path.is_empty() && repo.root.join(&path).is_file();
            self.one_file = on_disk;
            let is_file = on_disk && rev == "HEAD";
            self.points.spawn(ui.ctx(), move || {
                if is_file {
                    repo.file_history_points(&path, limit)
                } else {
                    repo.history_points((!path.is_empty()).then_some(path.as_str()), &rev, limit)
                }
            });
        }
        if let Some(n) = self.points.get().map(Vec::len).filter(|n| *n >= self.limit)
            && let Some(choice) = crate::widgets::load_more_bar(ui, "visual-load-more", n, "commits", VISUAL_PAGE, self.points.busy(), &p)
        {
            self.limit = choice.apply(self.limit, VISUAL_PAGE);
            self.started = false;
        }
        ui.horizontal(|ui| {
            ui.label("Path");
            let a = ui.add(crate::widgets::input(&mut self.path).hint_text("whole repository").desired_width(200.0));
            ui.label("Branch");
            let b = ui.add(crate::widgets::input(&mut self.rev).desired_width(120.0));
            if (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.started = false;
                self.window = None;
                self.limit = VISUAL_PAGE;
            }
            for (label, days) in [("1 month", 30), ("1 year", 365), ("All", 0)] {
                if ui.small_button(label).clicked() {
                    self.window = (days > 0).then(|| {
                        let now = util::now_ts();
                        (now - days * 86400, now)
                    });
                }
            }
            ui.label(RichText::new("Scroll to zoom, drag to pan, click a bubble to inspect").small().color(p.faint));
        });
        if let Some(e) = &self.points.error {
            error_box(ui, e, &p);
        }
        let Some(pts) = self.points.value.take() else {
            if self.points.busy() {
                spinner_row(ui, "Reading file history…", &p);
            }
            return;
        };
        if pts.is_empty() {
            empty_state(ui, "No history", "This file has no commits yet.", &p);
        } else {
            self.chart(ui, cx, &pts);
        }
        self.points.value = Some(pts);
    }

    fn chart(&mut self, ui: &mut Ui, cx: &mut Cx, pts: &[FileHistoryPoint]) {
        let p = cx.pal.clone();
        // Lanes ordered by first appearance so an author keeps lane and colour
        // regardless of zoom. Busiest authors get their own lane.
        let Lanes { names: lanes, kept } = self.lanes.take().unwrap_or_else(|| lanes_of(pts));
        let lane_of = |author: &str| -> usize { lanes.iter().position(|l| l == author).unwrap_or(lanes.len() - 1) };

        let (mut t0, mut t1) = pts.iter().fold((i64::MAX, i64::MIN), |(a, b), x| (a.min(x.time), b.max(x.time)));
        if t1 <= t0 {
            t0 -= 86400;
            t1 += 86400;
        }
        let pad = (t1 - t0) / 30 + 1;
        let (full0, full1) = (t0 - pad, t1 + pad);
        let (w0, w1) = self.window.unwrap_or((full0, full1));

        let avail = ui.available_rect_before_wrap();
        let (outer, resp) = ui.allocate_exact_size(avail.size(), Sense::click_and_drag());
        let legend_w = 150.0;
        let bars_h = (outer.height() * 0.2).clamp(50.0, 110.0);
        let axis_h = 20.0;
        let plot = Rect::from_min_max(pos2(outer.left() + legend_w, outer.top() + 6.0), pos2(outer.right() - 12.0, outer.bottom() - bars_h - axis_h));
        let bars = Rect::from_min_max(pos2(plot.left(), outer.bottom() - bars_h), pos2(plot.right(), outer.bottom() - 2.0));
        let painter = ui.painter_at(outer);
        let x_of = |t: i64| plot.left() + ((t - w0) as f32 / (w1 - w0).max(1) as f32) * plot.width();

        // Zoom and pan.
        if resp.hovered() {
            let (scroll, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.pointer.hover_pos()));
            if scroll != 0.0
                && let Some(h) = hover {
                    let f = ((h.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
                    let span = (w1 - w0) as f64;
                    let ns = (span * (1.0 - scroll as f64 * 0.002)).clamp(3600.0, ((full1 - full0) as f64) * 1.2);
                    let c = w0 as f64 + span * f;
                    self.window = Some(((c - ns * f) as i64, (c + ns * (1.0 - f)) as i64));
                }
        }
        if resp.dragged() {
            let dx = resp.drag_delta().x;
            let dt = (dx / plot.width() * (w1 - w0) as f32) as i64;
            self.window = Some((w0 - dt, w1 - dt));
        }

        // Recessive grid and lanes.
        let lane_h = plot.height() / lanes.len().max(1) as f32;
        for (i, name) in lanes.iter().enumerate() {
            let y = plot.top() + (i as f32 + 0.5) * lane_h;
            painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, p.border.gamma_multiply(0.6)));
            let color = p.lane(i as u16);
            painter.circle_filled(pos2(outer.left() + 10.0, y), 4.0, color);
            let clip = painter.with_clip_rect(Rect::from_min_max(pos2(outer.left(), y - 10.0), pos2(plot.left() - 4.0, y + 10.0)));
            clip.text(pos2(outer.left() + 20.0, y), Align2::LEFT_CENTER, name, prop(12.0), p.dim);
        }
        // Time axis: month or year ticks fitted to the window.
        let span_days = (w1 - w0) as f32 / 86400.0;
        let ticks = (plot.width() / 110.0).max(2.0) as i64;
        let step = ((w1 - w0) / ticks).max(1);
        for k in 0..=ticks {
            let t = w0 + step * k;
            let x = x_of(t);
            painter.line_segment([pos2(x, plot.top()), pos2(x, bars.bottom())], Stroke::new(1.0, p.border.gamma_multiply(0.25)));
            // The ends stay inside, not half off the plot.
            let align = if k == 0 { Align2::LEFT_TOP } else if k == ticks { Align2::RIGHT_TOP } else { Align2::CENTER_TOP };
            painter.text(pos2(x, plot.bottom() + 3.0), align, tick_label(t, span_days), prop(11.0), p.faint);
        }

        // Bars: additions up, deletions down, from a shared baseline.
        let in_window: Vec<&FileHistoryPoint> = pts.iter().filter(|x| x.time >= w0 && x.time <= w1).collect();
        let max_churn = in_window.iter().map(|x| x.additions.max(x.deletions)).max().unwrap_or(1).max(1) as f32;
        let baseline = bars.center().y;
        painter.line_segment([pos2(plot.left(), baseline), pos2(plot.right(), baseline)], Stroke::new(1.0, p.border));
        let bar_w = (plot.width() / in_window.len().max(1) as f32 * 0.5).clamp(2.0, 8.0);
        // Thousands of marks land on a few hundred pixels, each costing a mesh. Where several
        // would share one, only the largest is drawn: the others couldn't be seen.
        let pixel = |t: i64| x_of(t) as i32;
        let mut tallest_bar: HashMap<i32, u32> = HashMap::new();
        for x in &in_window {
            let at = tallest_bar.entry(pixel(x.time)).or_default();
            *at = (*at).max(x.additions.max(x.deletions));
        }
        for x in &in_window {
            if tallest_bar.get(&pixel(x.time)).is_some_and(|m| *m != x.additions.max(x.deletions)) {
                continue;
            }
            tallest_bar.remove(&pixel(x.time));
            let cx_x = x_of(x.time);
            // On a log scale, or one huge first commit would flatten all the others.
            let scale = |n: u32| (n as f32 + 1.0).ln() / (max_churn + 1.0).ln() * (bars.height() / 2.0 - 2.0);
            let (a, d) = (if x.additions > 0 { scale(x.additions) } else { 0.0 }, if x.deletions > 0 { scale(x.deletions) } else { 0.0 });
            if a > 0.0 {
                painter.rect_filled(Rect::from_min_max(pos2(cx_x - bar_w / 2.0, baseline - a.max(1.0)), pos2(cx_x + bar_w / 2.0, baseline - 1.0)), 2.0, p.add_fg);
            }
            if d > 0.0 {
                painter.rect_filled(Rect::from_min_max(pos2(cx_x - bar_w / 2.0, baseline + 1.0), pos2(cx_x + bar_w / 2.0, baseline + d.max(1.0))), 2.0, p.del_fg);
            }
        }
        painter.text(pos2(outer.left() + 20.0, baseline - 12.0), Align2::LEFT_CENTER, format!("+ lines added, log scale (max {max_churn:.0})"), prop(11.0), p.dim);
        painter.text(pos2(outer.left() + 20.0, baseline + 12.0), Align2::LEFT_CENTER, "− lines removed", prop(11.0), p.dim);

        // Bubbles. Area, not radius, encodes churn. A surface ring keeps overlaps legible.
        let max_total = in_window.iter().map(|x| x.additions + x.deletions).max().unwrap_or(1).max(1) as f32;
        let mut hovered: Option<&FileHistoryPoint> = None;
        let hover_pos = ui.input(|i| i.pointer.hover_pos()).filter(|h| plot.contains(*h));
        let clip = painter.with_clip_rect(plot);
        let mut best_d = f32::MAX;
        let lane_at = |x: &FileHistoryPoint| if kept.contains(&x.author) { lane_of(&x.author) } else { lanes.len() - 1 };
        let mut largest: HashMap<(usize, i32), u32> = HashMap::new();
        for x in &in_window {
            let at = largest.entry((lane_at(x), pixel(x.time))).or_default();
            *at = (*at).max(x.additions + x.deletions);
        }
        for x in &in_window {
            let lane = lane_at(x);
            let y = plot.top() + (lane as f32 + 0.5) * lane_h;
            let cx_x = x_of(x.time);
            let frac = (x.additions + x.deletions) as f32 / max_total;
            let r = 4.0 + frac.sqrt() * (lane_h.min(40.0) * 0.45 - 4.0).max(2.0);
            let color = p.lane(lane as u16);
            let selected = self.selected.as_deref() == Some(&x.sha);
            let key = (lane, pixel(x.time));
            if selected || largest.get(&key) == Some(&(x.additions + x.deletions)) {
                largest.remove(&key);
                clip.circle(pos2(cx_x, y), r, color.gamma_multiply(if selected { 1.0 } else { 0.85 }), Stroke::new(if selected { 2.0 } else { 1.5 }, if selected { p.text } else { p.bg }));
            }
            if let Some(h) = hover_pos {
                let dist = pos2(cx_x, y).distance(h);
                // Hit targets are larger than the mark.
                if dist < (r + 5.0) && dist < best_d {
                    best_d = dist;
                    hovered = Some(x);
                }
            }
        }
        if let Some(x) = hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), ui.id().with("visual-tip"), egui::PopupAnchor::Pointer).show(|ui| {
                commit_card(ui, &x.sha, &x.author, &x.email, x.time, &x.subject, &p, cx.settings.gravatar);
                ui.label(RichText::new(format!("+{} −{} lines", x.additions, x.deletions)).monospace());
                // A renamed file. For a folder or the whole repository, it'd be one file picked
                // out of the commit for no reason.
                if self.one_file && x.path != self.path {
                    ui.label(RichText::new(format!("as {}", x.path)).small().color(p.faint));
                }
            });
            if resp.clicked() {
                self.selected = Some(x.sha.clone());
                cx.inspect(&x.sha);
            }
            if resp.double_clicked() {
                cx.diff(DiffSource::Commit(x.sha.clone()), self.one_file.then_some(x.path.as_str()));
            }
        }
        // Outside the check on what's hovered, or the menu would go as the pointer moved to it.
        if resp.secondary_clicked() {
            self.menu_for = hovered.cloned();
        }
        if let Some(x) = self.menu_for.clone() {
            let file = self.one_file.then(|| x.path.clone()).filter(|p| !p.is_empty());
            resp.context_menu(|ui| {
                if ui.button("Open changes").clicked() {
                    cx.diff(DiffSource::Commit(x.sha.clone()), file.as_deref());
                    ui.close();
                }
                if ui.button("Show details").clicked() {
                    cx.inspect(&x.sha);
                    ui.close();
                }
                if let Some(f) = &file
                    && ui.button("Open this revision").clicked()
                {
                    cx.open(Tab::file(f, Some(x.sha.clone()), None));
                    ui.close();
                }
            });
        }
        self.lanes = Some(Lanes { names: lanes, kept });
    }
}

fn tick_label(t: i64, span_days: f32) -> String {
    use chrono::TimeZone;
    let Some(d) = chrono::Local.timestamp_opt(t, 0).single() else { return String::new() };
    if span_days < 3.0 {
        d.format("%b %d %H:%M").to_string()
    } else if span_days < 400.0 {
        d.format("%b %d").to_string()
    } else {
        d.format("%b %Y").to_string()
    }
}
