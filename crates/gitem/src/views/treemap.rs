//! Treemaps of the repository: files sized by bytes and coloured by type, or by
//! how often recent commits touched them. Click a block to open the file.
use std::collections::HashMap;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use gitem_core::treemap;

use crate::action::Cx;
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

/// File types beyond this many fold into "Other" rather than inventing hues.
const MAX_TYPES: usize = 7;
const MAX_FILES: usize = 4000;

#[derive(Clone, Copy, PartialEq, Default)]
enum Mode {
    #[default]
    Size,
    Activity,
}

#[derive(Default)]
pub struct TreemapView {
    mode: Mode,
    loaded: Option<(u64, bool)>,
    files: Task<Vec<(String, u64)>>,
    filter: String,
}

fn ext(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|x| x.1).unwrap_or("(none)")
}

impl TreemapView {
    pub fn key(&self) -> String {
        "treemap".into()
    }
    pub fn title(&self) -> String {
        "Treemap".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.files.poll();
        let want = (cx.data.refs_gen, self.mode == Mode::Size);
        if self.loaded != Some(want) {
            self.loaded = Some(want);
            let (r, m) = (cx.repo.clone(), self.mode);
            self.files.spawn(ui.ctx(), move || match m {
                Mode::Size => r.file_sizes("HEAD"),
                Mode::Activity => r.file_activity("HEAD", 2000),
            });
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Size, "Files by size");
            ui.selectable_value(&mut self.mode, Mode::Activity, "Files by recent commits");
            ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter by path").desired_width(220.0));
            if self.files.busy() {
                ui.add(spinner(14.0));
            }
        });
        if let Some(e) = &self.files.error {
            error_box(ui, e, &p);
        }
        let Some(all) = self.files.get() else { return };
        let q = self.filter.to_lowercase();
        let mut items: Vec<&(String, u64)> = all.iter().filter(|(f, v)| *v > 0 && (q.is_empty() || f.to_lowercase().contains(&q))).collect();
        items.sort_by_key(|x| std::cmp::Reverse(x.1));
        let hidden = items.len().saturating_sub(MAX_FILES);
        items.truncate(MAX_FILES);
        if items.is_empty() {
            empty_state(ui, "Nothing to show", "No files match.", &p);
            return;
        }
        // Types in order of total weight; identity stays with the type name.
        let mut weight: HashMap<&str, u64> = HashMap::new();
        for (f, v) in &items {
            *weight.entry(ext(f)).or_default() += v;
        }
        let mut types: Vec<(&str, u64)> = weight.into_iter().collect();
        types.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let kept: Vec<&str> = types.iter().take(MAX_TYPES).map(|x| x.0).collect();
        let color_of = |f: &str| -> Color32 { kept.iter().position(|e| *e == ext(f)).map(|i| p.series[i]).unwrap_or(p.faint) };
        let max = items.first().map(|x| x.1).unwrap_or(1).max(1) as f32;

        ui.horizontal_wrapped(|ui| {
            if self.mode == Mode::Size {
                for (i, e) in kept.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, p.series[i]);
                    ui.label(RichText::new(*e).small().color(p.dim));
                }
                if types.len() > kept.len() {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, p.faint);
                    ui.label(RichText::new("other").small().color(p.dim));
                }
            } else {
                ui.label(RichText::new("Darker to brighter: more commits touched the file").small().color(p.dim));
            }
            if hidden > 0 {
                ui.label(RichText::new(format!("{hidden} smallest not shown")).small().color(p.faint));
            }
        });

        let avail = ui.available_rect_before_wrap();
        let (outer, resp) = ui.allocate_exact_size(avail.size(), Sense::click());
        let values: Vec<f64> = items.iter().map(|x| x.1 as f64).collect();
        let cells = treemap::layout(&values, outer.left(), outer.top(), outer.width(), outer.height());
        let painter = ui.painter_at(outer);
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|h| outer.contains(*h));
        let mut hit: Option<usize> = None;
        for c in &cells {
            let (path, v) = items[c.index];
            let r = Rect::from_min_size(pos2(c.x, c.y), vec2(c.w, c.h));
            let fill = if self.mode == Mode::Size { color_of(path) } else { p.heat((*v as f32 / max).sqrt()) };
            let inner = r.shrink(1.0);
            painter.rect_filled(inner, 2.0, fill);
            if hover.is_some_and(|h| r.contains(h)) {
                hit = Some(c.index);
                painter.rect_stroke(inner, 2.0, Stroke::new(1.5, p.text), egui::StrokeKind::Inside);
            }
            if c.w > 60.0 && c.h > 18.0 {
                let name = path.rsplit('/').next().unwrap_or(path);
                painter.with_clip_rect(inner.shrink(3.0)).text(inner.min + vec2(4.0, 3.0), Align2::LEFT_TOP, name, prop(11.0), Color32::WHITE);
            }
        }
        if let Some(i) = hit {
            let (path, v) = items[i];
            let label = if self.mode == Mode::Size { format!("{v} bytes") } else { format!("{v} commits") };
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), ui.id().with("tm-tip"), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(path.as_str());
                ui.label(RichText::new(label).small().color(p.dim));
            });
            if resp.clicked() {
                cx.open(Tab::file(path, None, None));
            }
        }
    }
}
