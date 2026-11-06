//! The commit graph's columns: which show, in what order, and how wide. Drag a divider
//! to resize, drag a header to move, right-click a header to customize. The message
//! column takes whatever width is left, so it's resized through its neighbours.
use std::collections::{HashMap, HashSet};

use egui::{Align2, CursorIcon, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};
use gitem_core::settings::GraphLayout;

use crate::action::{Action, Cx};
use crate::theme::Palette;
use crate::widgets::prop;

const MESSAGE: &str = "message";
const GRAPH: &str = "graph";
const CHANGES: &str = "changes";
const MIN_MESSAGE: f32 = 120.0;
const DIVIDER_HIT: f32 = 7.0;

struct Spec {
    key: &'static str,
    label: &'static str,
    width: f32,
    min: f32,
}

/// In default order. The graph's default is fitted to its lanes, and the message's is
/// whatever is left.
const SPECS: [Spec; 7] = [
    Spec { key: "refs", label: "Branch / tag", width: 190.0, min: 60.0 },
    Spec { key: GRAPH, label: "Graph", width: 0.0, min: 30.0 },
    Spec { key: MESSAGE, label: "Commit message", width: 0.0, min: MIN_MESSAGE },
    Spec { key: CHANGES, label: "Changes", width: 90.0, min: 60.0 },
    Spec { key: "author", label: "Author", width: 130.0, min: 50.0 },
    Spec { key: "date", label: "Date", width: 100.0, min: 50.0 },
    Spec { key: "sha", label: "SHA", width: 66.0, min: 40.0 },
];

fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

/// One visible column, positioned for this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    pub key: &'static str,
    pub label: &'static str,
    pub x: f32,
    pub w: f32,
}

pub struct Columns {
    pub order: Vec<String>,
    pub widths: HashMap<String, f32>,
    pub hidden: HashSet<String>,
    /// Kept in the app's settings, since other parts of the graph depend on it.
    pub changes: bool,
    moving: Option<&'static str>,
}

impl Default for Columns {
    fn default() -> Self {
        Self { order: SPECS.iter().map(|s| s.key.to_string()).collect(), widths: HashMap::new(), hidden: HashSet::new(), changes: false, moving: None }
    }
}

impl Columns {
    /// Saved columns, tolerating names this version doesn't know and adding new ones.
    pub fn apply(&mut self, saved: &GraphLayout) {
        let mut order: Vec<String> = saved.columns.iter().filter(|c| spec(c).is_some()).cloned().collect();
        for (i, s) in SPECS.iter().enumerate() {
            if !order.iter().any(|c| c == s.key) {
                order.insert(i.min(order.len()), s.key.to_string());
            }
        }
        self.order = order;
        self.hidden = saved.hidden.iter().filter(|h| h.as_str() != MESSAGE).cloned().collect();
        self.widths = saved.widths.iter().filter(|(k, w)| spec(k).is_some() && w.is_finite()).map(|(k, w)| (k.clone(), *w)).collect();
    }

    pub fn to_saved(&self) -> GraphLayout {
        GraphLayout { columns: self.order.clone(), hidden: self.hidden.iter().cloned().collect(), widths: self.widths.clone() }
    }

    pub fn shown(&self, key: &str) -> bool {
        if key == CHANGES { self.changes } else { !self.hidden.contains(key) }
    }

    pub fn reset(&mut self) {
        let changes = self.changes;
        *self = Self { changes, ..Self::default() };
    }

    /// Positions the visible columns in `w`. When they don't fit, the ones wider than
    /// their minimum give up width evenly, so the message stays readable.
    pub fn layout(&self, w: f32, graph_fit: f32) -> Vec<Col> {
        let mut cols: Vec<Col> = Vec::with_capacity(self.order.len());
        for key in &self.order {
            let (Some(s), true) = (spec(key), self.shown(key)) else { continue };
            let width = match (self.widths.get(key), s.key) {
                (_, MESSAGE) => 0.0,
                (Some(v), _) => v.max(s.min),
                (None, GRAPH) => graph_fit.max(s.min),
                (None, _) => s.width,
            };
            cols.push(Col { key: s.key, label: s.label, x: 0.0, w: width });
        }
        let mut over = cols.iter().map(|c| c.w).sum::<f32>() + MIN_MESSAGE - w;
        while over > 0.5 {
            let room: Vec<usize> = (0..cols.len()).filter(|i| cols[*i].key != MESSAGE && cols[*i].w > spec(cols[*i].key).map_or(0.0, |s| s.min) + 0.5).collect();
            if room.is_empty() {
                break;
            }
            let each = over / room.len() as f32;
            for i in room {
                let min = spec(cols[i].key).map_or(0.0, |s| s.min);
                let take = each.min(cols[i].w - min);
                cols[i].w -= take;
                over -= take;
            }
        }
        let used: f32 = cols.iter().map(|c| c.w).sum();
        if let Some(m) = cols.iter_mut().find(|c| c.key == MESSAGE) {
            m.w = (w - used).max(MIN_MESSAGE);
        }
        let mut x = 0.0;
        for c in &mut cols {
            c.x = x;
            x += c.w;
        }
        cols
    }

    /// Drags the divider after `cols[i]` by `dx`. Next to the message column, the
    /// other neighbour changes instead, so the divider follows the pointer.
    pub fn resize(&mut self, cols: &[Col], i: usize, dx: f32) {
        let (target, grow) = if cols[i].key == MESSAGE { (i + 1, -dx) } else { (i, dx) };
        let Some(col) = cols.get(target) else { return };
        let min = spec(col.key).map_or(0.0, |s| s.min);
        // Growing stops once the message is down to its minimum.
        let spare = cols.iter().find(|c| c.key == MESSAGE).map_or(f32::INFINITY, |m| (m.w - MIN_MESSAGE).max(0.0));
        let width = (col.w + grow.min(spare)).max(min);
        self.widths.insert(col.key.to_string(), width);
    }

    /// Moves `key` before `before`, or to the end. Hidden columns keep their places
    /// relative to the rest.
    pub fn move_column(&mut self, key: &str, before: Option<&str>) {
        if before == Some(key) {
            return;
        }
        let Some(from) = self.order.iter().position(|c| c == key) else { return };
        let moved = self.order.remove(from);
        let to = before.and_then(|b| self.order.iter().position(|c| c == b)).unwrap_or(self.order.len());
        self.order.insert(to, moved);
    }

    fn persist(&self, cx: &mut Cx) {
        let saved = self.to_saved();
        cx.out.push(Action::Settings(Box::new(move |s| s.graph_layout = Some(saved))));
    }

    /// Show, hide and reorder, for the Columns menu and a header's context menu.
    pub fn menu(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let mut changed = false;
        let (up, down) = (crate::chrome::icon::CARET_UP, crate::chrome::icon::CARET_DOWN);
        for i in 0..self.order.len() {
            let key = self.order[i].clone();
            let Some(s) = spec(&key) else { continue };
            ui.horizontal(|ui| {
                if ui.add_enabled(i > 0, egui::Button::new(up).small()).on_hover_text(format!("Move {} earlier", s.label)).clicked() {
                    self.order.swap(i, i - 1);
                    changed = true;
                }
                if ui.add_enabled(i + 1 < self.order.len(), egui::Button::new(down).small()).on_hover_text(format!("Move {} later", s.label)).clicked() {
                    self.order.swap(i, i + 1);
                    changed = true;
                }
                let mut on = self.shown(&key);
                if ui.add_enabled(key != MESSAGE, egui::Checkbox::new(&mut on, s.label)).changed() {
                    if key == CHANGES {
                        self.changes = on;
                        cx.out.push(Action::Settings(Box::new(move |st| st.graph_changes_column = on)));
                    } else if on {
                        self.hidden.remove(&key);
                    } else {
                        self.hidden.insert(key.clone());
                    }
                    changed = true;
                }
            });
        }
        ui.separator();
        if ui.button("Reset columns").on_hover_text("Default order, visibility and widths").clicked() {
            self.reset();
            changed = true;
            ui.close();
        }
        if changed {
            self.persist(cx);
        }
    }

    /// The header row: labels you can drag to reorder, and dividers to resize.
    pub fn header(&mut self, ui: &mut Ui, hdr: Rect, cols: &[Col], p: &Palette, cx: &mut Cx) {
        let base = ui.id().with("graph-columns");
        let at = |c: &Col| Rect::from_min_size(pos2(hdr.left() + c.x, hdr.top()), vec2(c.w, hdr.height()));
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let mut dropped: Option<(&'static str, Option<&'static str>)> = None;
        for c in cols {
            let cell = at(c);
            let resp = ui.interact(cell, base.with(c.key), Sense::click_and_drag());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{} column", c.label)));
            if self.moving == Some(c.key) {
                ui.painter().rect_filled(cell, 0.0, p.selection);
            } else if resp.hovered() {
                ui.painter().rect_filled(cell, 0.0, p.hover);
            }
            let color = if resp.hovered() || self.moving == Some(c.key) { p.text } else { p.dim };
            ui.painter().with_clip_rect(cell.shrink(2.0)).text(pos2(cell.left() + 6.0, cell.center().y), Align2::LEFT_CENTER, c.label, prop(11.5), color);
            if resp.drag_started() {
                self.moving = Some(c.key);
            }
            if resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            if resp.drag_stopped() {
                self.moving = None;
                if let Some(pos) = pointer {
                    dropped = Some((c.key, insert_before(cols, pos.x - hdr.left())));
                }
            }
            header_menu(self, resp, cx);
        }
        // The insertion point while a header is being dragged.
        if self.moving.is_some()
            && let Some(pos) = pointer
        {
            let x = match insert_before(cols, pos.x - hdr.left()).and_then(|k| cols.iter().find(|c| c.key == k)) {
                Some(c) => hdr.left() + c.x,
                None => hdr.left() + cols.last().map_or(0.0, |c| c.x + c.w),
            };
            ui.painter().vline(x, hdr.y_range(), Stroke::new(2.0, p.accent));
        }

        for i in 0..cols.len().saturating_sub(1) {
            let x = hdr.left() + cols[i].x + cols[i].w;
            let hit = Rect::from_center_size(pos2(x, hdr.center().y), vec2(DIVIDER_HIT, hdr.height()));
            let resp = ui.interact(hit, base.with(("divider", cols[i].key)), Sense::click_and_drag());
            let changing = if cols[i].key == MESSAGE { cols[i + 1].label } else { cols[i].label };
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Resize {changing} column")));
            let active = resp.hovered() || resp.dragged();
            if active {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            let bottom = if resp.dragged() { ui.max_rect().bottom() } else { hdr.bottom() };
            let line = if active { Stroke::new(2.0, p.accent) } else { Stroke::new(1.0, p.border) };
            ui.painter().vline(x, hdr.top()..=bottom, line);
            if resp.dragged() {
                self.resize(cols, i, resp.drag_delta().x);
            }
            if resp.drag_stopped() {
                self.persist(cx);
            }
            if resp.double_clicked() {
                let key = if cols[i].key == MESSAGE { cols[i + 1].key } else { cols[i].key };
                self.widths.remove(key);
                self.persist(cx);
            }
        }
        if let Some((key, before)) = dropped {
            self.move_column(key, before);
            self.persist(cx);
        }
    }
}

fn header_menu(columns: &mut Columns, resp: Response, cx: &mut Cx) {
    resp.context_menu(|ui| columns.menu(ui, cx));
}

/// Which column a header dropped at `x` goes in front of; `None` means the end.
fn insert_before(cols: &[Col], x: f32) -> Option<&'static str> {
    cols.iter().find(|c| x < c.x + c.w / 2.0).map(|c| c.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(c: &Columns) -> Vec<&str> {
        c.order.iter().map(String::as_str).collect()
    }

    #[test]
    fn message_takes_what_is_left() {
        let c = Columns::default();
        let l = c.layout(1000.0, 80.0);
        assert_eq!(l.iter().map(|c| c.key).collect::<Vec<_>>(), ["refs", "graph", "message", "author", "date", "sha"], "changes is off by default");
        let m = l.iter().find(|c| c.key == MESSAGE).unwrap();
        assert_eq!(m.w, 1000.0 - 190.0 - 80.0 - 130.0 - 100.0 - 66.0);
        assert_eq!(l.iter().map(|c| c.w).sum::<f32>(), 1000.0);
        assert_eq!(l[1].x, 190.0);
    }

    #[test]
    fn narrow_panels_shrink_columns_before_the_message() {
        let c = Columns::default();
        let l = c.layout(500.0, 80.0);
        assert_eq!(l.iter().map(|c| c.w).sum::<f32>().round(), 500.0);
        assert_eq!(l.iter().find(|c| c.key == MESSAGE).unwrap().w.round(), MIN_MESSAGE);
        for col in &l {
            assert!(col.w >= spec(col.key).unwrap().min - 0.5, "{col:?}");
        }
    }

    #[test]
    fn too_narrow_even_for_minimums_overflows_rather_than_hanging() {
        let l = Columns::default().layout(100.0, 80.0);
        assert!(l.iter().map(|c| c.w).sum::<f32>() > 100.0);
    }

    #[test]
    fn dividers_resize_the_left_column_or_the_one_past_the_message() {
        let mut c = Columns::default();
        let l = c.layout(1000.0, 80.0);
        c.resize(&l, 0, 40.0);
        assert_eq!(c.widths["refs"], 230.0);
        let l = c.layout(1000.0, 80.0);
        let msg = l.iter().position(|x| x.key == MESSAGE).unwrap();
        c.resize(&l, msg, 30.0);
        assert_eq!(c.widths["author"], 100.0, "dragging right shrinks what's past the message");
    }

    #[test]
    fn resizing_respects_both_minimums() {
        let mut c = Columns::default();
        let l = c.layout(1000.0, 80.0);
        c.resize(&l, 0, -1000.0);
        assert_eq!(c.widths["refs"], 60.0);
        let l = c.layout(1000.0, 80.0);
        c.resize(&l, 0, 5000.0);
        let l = c.layout(1000.0, 80.0);
        assert_eq!(l.iter().find(|x| x.key == MESSAGE).unwrap().w.round(), MIN_MESSAGE);
        assert_eq!(l.iter().map(|x| x.w).sum::<f32>().round(), 1000.0);
    }

    #[test]
    fn moving_columns() {
        let mut c = Columns::default();
        c.move_column("sha", Some("refs"));
        assert_eq!(keys(&c)[0], "sha");
        c.move_column("sha", None);
        assert_eq!(*keys(&c).last().unwrap(), "sha");
        let before = c.order.clone();
        c.move_column("sha", Some("sha"));
        assert_eq!(c.order, before);
    }

    #[test]
    fn insertion_point_follows_column_midpoints() {
        let l = Columns::default().layout(1000.0, 80.0);
        assert_eq!(insert_before(&l, 10.0), Some("refs"));
        assert_eq!(insert_before(&l, 150.0), Some("graph"));
        assert_eq!(insert_before(&l, 999.0), None);
    }

    #[test]
    fn saved_layouts_round_trip_and_tolerate_old_or_odd_data() {
        let mut a = Columns::default();
        a.widths.insert("date".into(), 77.0);
        a.hidden.insert("sha".into());
        a.move_column("date", Some("refs"));
        let mut b = Columns::default();
        b.apply(&a.to_saved());
        assert_eq!((b.order.clone(), b.widths.clone(), b.hidden.clone()), (a.order.clone(), a.widths.clone(), a.hidden.clone()));

        // An older save has no changes column or widths, and a name we've never heard of.
        let old = GraphLayout { columns: vec!["sha".into(), "bogus".into(), "message".into()], hidden: vec!["message".into()], widths: HashMap::from([("nope".into(), 5.0), ("sha".into(), f32::NAN)]) };
        let mut c = Columns::default();
        c.apply(&old);
        assert_eq!(c.order.len(), SPECS.len());
        assert!(!c.order.iter().any(|k| k == "bogus"));
        assert!(c.shown(MESSAGE), "the message can't be hidden");
        assert!(c.widths.is_empty());
    }

    #[test]
    fn reset_restores_defaults_but_not_the_changes_setting() {
        let mut c = Columns { changes: true, ..Columns::default() };
        c.widths.insert("sha".into(), 200.0);
        c.hidden.insert("date".into());
        c.move_column("sha", Some("refs"));
        c.reset();
        assert!(c.changes);
        assert!(c.widths.is_empty() && c.hidden.is_empty());
        assert_eq!(keys(&c), SPECS.iter().map(|s| s.key).collect::<Vec<_>>());
    }
}
