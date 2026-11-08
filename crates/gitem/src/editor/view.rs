//! Drawing the editor and turning keys and the mouse into commands. Only the rows in view are
//! laid out, so a file of a million lines costs what a screenful does.
//!
//! Lines are one row each unless wrapping or folding is on. Then a table of where each line
//! starts is kept, rebuilt when the text, the width or the folds change, and each line drawn
//! corrects its own height if wrapping took more rows than the estimate.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use egui::text::{CCursor, CharIndex, LayoutJob, TextFormat};
use egui::{Align2, Color32, CursorIcon, Event, EventFilter, Galley, Id, Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use gitem_text::selection::{self, Move};
use gitem_text::{Selection, SelectionSet};

use super::{Cmd, Editor};
use crate::theme::Palette;
use crate::widgets::mono;

/// Space between the gutter and the text.
const PAD: f32 = 6.0;

/// Two clicks this close in time, at the same place, are a double click.
const MULTI_CLICK_SECS: f64 = 0.4;

const MINIMAP_WIDTH: f32 = 84.0;
/// Each line's height in the minimap.
const MINIMAP_LINE: f32 = 2.0;

#[derive(Default)]
pub struct ViewState {
    /// When and where the last click was, and how many in a row, for double and triple clicks.
    last_click: Option<(f64, usize, u8)>,
    drag: Option<Drag>,
    rows: Rows,
    /// A line to bring to the middle of the view, as from a click in the minimap.
    center_on: Option<usize>,
    /// Where the primary caret wants to be across, moving between wrapped rows, so a short
    /// row on the way doesn't pull it left for good.
    goal_x: Option<f32>,
    /// What an input method is composing, not yet text: shown at the caret, underlined, until
    /// it's committed or given up.
    pub preedit: Option<String>,
}

/// Where lines start, when that isn't simply line times row height.
#[derive(Default)]
struct Rows {
    /// What the table was built for: text version, wrap width in columns, folds, row height.
    built_for: Option<(u64, u32, u64, u32)>,
    /// `tops[l]` is where line `l` starts; one more entry for the end.
    tops: Vec<f32>,
    /// Rows a drawn line really took, where the estimate was wrong.
    measured: HashMap<usize, u32>,
    measured_for: u64,
}

/// A drag under way: the first selection it made, and by what it grows.
struct Drag {
    origin: (usize, usize),
    unit: Unit,
    /// Shift+Alt dragging selects a column: the line and x it started at.
    column: Option<(usize, f32)>,
    /// Alt held: the drag adds a selection rather than replacing them.
    adding: bool,
    /// The selection this drag made last, which the next move replaces.
    last: Selection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

/// A change in the gutter for one line, against the index or only against HEAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were taken out just above this one.
    Deleted,
}

/// How to draw, chosen by the view that owns the editor.
pub struct Look<'a> {
    pub font_size: f32,
    pub pal: &'a Palette,
    pub whitespace: bool,
    pub indent_guides: bool,
    pub wrap: bool,
    pub minimap: bool,
    /// Changes against the index, by line, and those only against HEAD (staged already).
    pub marks: &'a HashMap<usize, Mark>,
    pub staged_marks: &'a HashMap<usize, Mark>,
    /// Shown after the end of the caret's line, as for blame.
    pub line_note: Option<&'a str>,
    /// The keys the user changed, for Find and the like.
    pub keys: &'a HashMap<String, String>,
    /// What a language server found, to underline: line, from and to column, how bad.
    pub underlines: &'a [(usize, usize, usize, super::lsp::Severity)],
}

#[derive(Default)]
pub struct Shown {
    /// Ctrl+S was pressed.
    pub save: bool,
    /// Alt+Z was pressed.
    pub toggle_wrap: bool,
    /// The outline was asked for.
    pub outline: bool,
    /// A line number or change mark was clicked.
    pub gutter_click: Option<usize>,
    /// The text area, for a right-click menu.
    pub response: Option<Response>,
    pub focused: bool,
}

/// What the frame's drawing needs to know about sizes.
struct Metrics {
    row_h: f32,
    char_w: f32,
    font_size: f32,
    /// Wrap width in points, if wrapping.
    wrap: Option<f32>,
}

impl Rows {
    /// Builds the table if wrapping or folds need one. Without either it stays empty and
    /// every line is one row.
    fn update(&mut self, ed: &Editor, m: &Metrics) {
        let cols = m.wrap.map_or(0, |w| (w / m.char_w).floor().max(8.0) as u32);
        if self.measured_for != ed.buf.version {
            self.measured.clear();
            self.measured_for = ed.buf.version;
        }
        let key = (ed.buf.version, cols, ed.folds_gen, m.row_h.to_bits());
        if self.built_for == Some(key) {
            return;
        }
        self.built_for = Some(key);
        if cols == 0 && ed.folds.is_empty() {
            self.tops.clear();
            return;
        }
        let n = ed.buf.line_count();
        self.tops.clear();
        self.tops.reserve(n + 1);
        let mut y = 0.0;
        let hidden = ed.hidden_lines();
        let mut h = hidden.iter().peekable();
        for l in 0..n {
            self.tops.push(y);
            while h.peek().is_some_and(|r| r.end() < &l) {
                h.next();
            }
            if h.peek().is_some_and(|r| r.contains(&l)) {
                continue;
            }
            let rows = match self.measured.get(&l) {
                Some(r) => *r,
                None if cols > 0 => (ed.buf.rope.line(l).len_chars().saturating_sub(1) as u32 / cols + 1).max(1),
                None => 1,
            };
            y += rows as f32 * m.row_h;
        }
        self.tops.push(y);
    }

    fn top(&self, line: usize, row_h: f32) -> f32 {
        if self.tops.is_empty() { line as f32 * row_h } else { self.tops.get(line).copied().unwrap_or_else(|| *self.tops.last().unwrap_or(&0.0)) }
    }

    fn height(&self, line: usize, row_h: f32) -> f32 {
        if self.tops.is_empty() { row_h } else { self.top(line + 1, row_h) - self.top(line, row_h) }
    }

    fn total(&self, lines: usize, row_h: f32) -> f32 {
        if self.tops.is_empty() { lines as f32 * row_h } else { *self.tops.last().unwrap_or(&0.0) }
    }

    /// The line at a height, skipping folded ones.
    fn line_at(&self, y: f32, lines: usize, row_h: f32) -> usize {
        let last = lines.saturating_sub(1);
        if self.tops.is_empty() {
            return ((y / row_h).floor().max(0.0) as usize).min(last);
        }
        // The last line starting at or above y, of those that take up room.
        let i = self.tops.partition_point(|t| *t <= y).saturating_sub(1).min(last);
        let mut l = i;
        while l > 0 && self.height(l, row_h) <= 0.0 {
            l -= 1;
        }
        l
    }

    /// A drawn line took this many rows. `true` if that changes the table.
    fn measured(&mut self, line: usize, rows: u32, estimate: u32) -> bool {
        if rows == estimate || self.measured.get(&line) == Some(&rows) {
            return false;
        }
        self.measured.insert(line, rows);
        self.built_for = None;
        true
    }
}

/// Lays out one line as it's drawn, with its colours, wrapped if `wrap` is given.
fn galley(ui: &Ui, ed: &Editor, line: usize, m: &Metrics, base: Color32) -> Arc<Galley> {
    let text = ed.buf.line(line);
    let font = mono(m.font_size);
    let mut job = LayoutJob::default();
    job.wrap.max_width = m.wrap.unwrap_or(f32::INFINITY);
    let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, line_height: Some(m.row_h), valign: egui::Align::Center, ..Default::default() };
    let spans = ed.hl.line(line).map(|s| s.as_slice()).unwrap_or(&[]);
    let mut at = 0;
    for &(a, b, color) in spans {
        // Colours from before an edit may not fit the line as it is now.
        let (a, b) = (a.min(text.len()), b.min(text.len()));
        if a < at || !text.is_char_boundary(a) || !text.is_char_boundary(b) || a >= b {
            continue;
        }
        if a > at {
            job.append(&text[at..a], 0.0, fmt(base));
        }
        job.append(&text[a..b], 0.0, fmt(color));
        at = b;
    }
    if at < text.len() || text.is_empty() {
        job.append(&text[at..], 0.0, fmt(base));
    }
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Where a caret before char `col` is, relative to the galley. Where a line wraps, the same
/// position ends one row and starts the next; the caret is shown at the start of the next,
/// as editors do, and Up and Down count it there.
fn caret_rect(g: &Galley, col: usize) -> Rect {
    g.pos_from_cursor(CCursor { index: CharIndex(col), prefer_next_row: true })
}

/// The rectangles covering chars `a..b` of a laid-out line, one per wrapped row.
fn range_rects(g: &Galley, a: usize, b: usize) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut start = 0;
    for row in &g.rows {
        let n = row.char_count_excluding_newline().0;
        let end = start + n;
        let (s, e) = (a.max(start), b.min(end));
        if s < e {
            let x0 = row.pos.x + row.x_offset(CharIndex(s - start));
            let x1 = row.pos.x + row.x_offset(CharIndex(e - start));
            out.push(Rect::from_min_max(pos2(x0, row.min_y()), pos2(x1, row.max_y())));
        }
        start = end;
    }
    out
}

/// Draws the editor in all the room there is and handles its input.
pub fn show(ui: &mut Ui, ed: &mut Editor, look: &Look) -> Shown {
    let p = look.pal;
    let id = ui.make_persistent_id(("code-editor", &ed.path));
    let font = mono(look.font_size);
    let row_h = (look.font_size * 1.5).ceil();
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let lines = ed.buf.line_count();
    let digits = lines.max(1).ilog10() as f32 + 1.0;
    let gutter_w = (digits.max(3.0) * char_w + 30.0).round();
    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());
    let gutter = Rect::from_min_max(outer.min, pos2(outer.min.x + gutter_w, outer.max.y));
    let mut text_rect = Rect::from_min_max(pos2(gutter.max.x, outer.min.y), outer.max);
    let minimap = look.minimap.then(|| {
        let r = Rect::from_min_max(pos2(text_rect.max.x - MINIMAP_WIDTH, text_rect.min.y), text_rect.max);
        text_rect.max.x = r.min.x;
        r
    });
    let wrap = look.wrap.then(|| (text_rect.width() - PAD * 2.0 - 14.0).max(char_w * 8.0));
    let m = Metrics { row_h, char_w, font_size: look.font_size, wrap };
    let mut shown = Shown::default();
    let focused = ui.memory(|mem| mem.has_focus(id));
    shown.focused = focused;
    if focused {
        ui.memory_mut(|mem| mem.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    }
    let now = ui.input(|i| i.time);
    let now_ms = (now * 1000.0) as u64;

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let mut origin = text_rect.min;
    let mut visible = 0..0;
    let mut caret_screen: Option<Rect> = None;
    let mut view_rect = text_rect;
    egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false]).show_viewport(&mut child, |ui, vp| {
        origin = ui.max_rect().min;
        view_rect = Rect::from_min_size(origin + vp.min.to_vec2(), vp.size());
        let page = (vp.height() / row_h).floor().max(1.0) as isize;

        // Keys first, so what's drawn is what they did.
        if focused {
            keyboard(ui, ed, page, now_ms, look, &m, &mut shown);
        }
        let mut rows = std::mem::take(&mut ed.view.rows);
        rows.update(ed, &m);
        ed.view.rows = rows;
        let width = if wrap.is_some() { vp.width() } else { (ed.max_cols as f32 + 4.0) * char_w + PAD * 2.0 };
        // Room to scroll the last line up to the middle, as editors allow.
        let size = vec2(width, ed.view.rows.total(lines, row_h) + (vp.height() * 0.5).max(row_h));
        ui.set_min_size(size);
        // Only what's in view takes the mouse: the whole text can be millions of pixels tall.
        let resp = ui.interact(view_rect, id, Sense::click_and_drag());
        let name = format!("Text of {}", ed.path);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &name));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        mouse(ui, ed, &resp, origin, &m, p, now);
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() || std::mem::take(&mut ed.focus) {
            resp.request_focus();
        }

        let first = ed.view.rows.line_at(vp.min.y, lines, row_h);
        let mut last = first;
        while last < lines && ed.view.rows.top(last, row_h) < vp.max.y {
            last += 1;
        }
        visible = first..last;
        if !ed.hl.ensure(&ed.buf, last + 1, Duration::from_millis(6)) {
            ui.ctx().request_repaint();
        }
        if ed.find.open {
            ed.refresh_matches();
        }
        let painter = ui.painter_at(ui.clip_rect());
        let full_w = size.x.max(vp.width());
        let primary = ed.sels.primary();
        let bracket = if primary.is_empty() { ed.matching_bracket(primary.head) } else { None };
        let unit = ed.indent.unit().replace('\t', "    ").chars().count().max(1);
        let text_x = origin.x + PAD;
        let mut remeasure = false;
        for line in visible.clone() {
            let h = ed.view.rows.height(line, row_h);
            if h <= 0.0 {
                continue;
            }
            let y = origin.y + ed.view.rows.top(line, row_h);
            let g = galley(ui, ed, line, &m, p.text);
            if wrap.is_some() {
                let estimate = (h / row_h).round() as u32;
                remeasure |= ed.view.rows.measured(line, g.rows.len() as u32, estimate);
            }
            let at = |r: Rect| r.translate(vec2(text_x, y));
            let (start, end) = (ed.buf.line_start(line), ed.buf.line_end(line));
            let len = end - start;
            if ed.sels.iter().any(|s| s.is_empty() && ed.buf.line_of(s.head) == line) {
                painter.rect_filled(Rect::from_min_size(pos2(origin.x, y), vec2(full_w, h)), 0.0, p.hover.gamma_multiply(0.55));
            }
            if ed.find.open {
                let ms = &ed.find.matches;
                let from = ms.partition_point(|x| x.1 <= start);
                for mt in ms[from..].iter().take_while(|x| x.0 <= end) {
                    for r in range_rects(&g, mt.0.max(start) - start, mt.1.min(end) - start) {
                        painter.rect_filled(at(r).expand2(vec2(0.0, -1.0)), 2.0, p.warn.gamma_multiply(0.28));
                    }
                }
            }
            for s in ed.sels.iter().filter(|s| !s.is_empty() && s.start() <= end + 1 && s.end() >= start) {
                let (a, b) = (s.start().max(start) - start, s.end().min(end) - start);
                let mut rects = range_rects(&g, a.min(len), b.min(len));
                // The line break is selected too: a sliver past the end says so.
                if s.end() > end {
                    let c = at(caret_rect(&g, len));
                    match rects.last_mut() {
                        Some(r) if (r.max.y - (c.max.y - y)).abs() < 1.0 => r.max.x += char_w * 0.6,
                        _ => rects.push(Rect::from_min_size(c.min - vec2(text_x, y), vec2(char_w * 0.6, c.height()))),
                    }
                }
                for r in rects {
                    painter.rect_filled(at(r), 0.0, p.selection);
                }
            }
            if look.indent_guides {
                let text = ed.buf.line(line);
                let lead = text.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
                if lead < text.len() {
                    for k in (unit..lead).step_by(unit) {
                        let x = (text_x + k as f32 * char_w).round() + 0.5;
                        painter.line_segment([pos2(x, y), pos2(x, y + h)], Stroke::new(1.0, p.border.gamma_multiply(0.7)));
                    }
                }
            }
            painter.galley(pos2(text_x, y), g.clone(), p.text);
            for &(_, from, to, sev) in look.underlines.iter().filter(|u| u.0 == line) {
                let color = match sev {
                    super::lsp::Severity::Error => p.danger,
                    super::lsp::Severity::Warning => p.warn,
                    super::lsp::Severity::Hint => p.dim,
                };
                for r in range_rects(&g, from.min(len), to.min(len.max(from + 1))) {
                    squiggle(&painter, at(r), color);
                }
            }
            if look.whitespace {
                for (i, c) in ed.buf.line(line).chars().enumerate() {
                    let mark = match c {
                        ' ' => "·",
                        '\t' => "→",
                        _ => continue,
                    };
                    if let Some(r) = range_rects(&g, i, i + 1).first() {
                        painter.text(at(*r).center(), Align2::CENTER_CENTER, mark, mono(look.font_size * 0.9), p.faint);
                    }
                }
            }
            if let Some((a, b)) = bracket {
                for pos in [a, b] {
                    if (start..end).contains(&pos) {
                        for r in range_rects(&g, pos - start, pos - start + 1) {
                            painter.rect_stroke(at(r).expand2(vec2(0.0, -1.0)), 2.0, Stroke::new(1.0, p.dim), egui::StrokeKind::Inside);
                        }
                    }
                }
            }
            let end_of_text = at(caret_rect(&g, len));
            if let Some(fold) = ed.folds.iter().find(|f| f.0 == line) {
                let label = format!(" ⋯ {} ", fold.1 - fold.0);
                let r = painter.text(pos2(end_of_text.max.x + char_w, end_of_text.center().y), Align2::LEFT_CENTER, label, mono(look.font_size * 0.85), p.dim);
                painter.rect_stroke(r.expand(1.0), 3.0, Stroke::new(1.0, p.border), egui::StrokeKind::Outside);
            } else if let Some(note) = look.line_note
                && ed.buf.line_of(primary.head) == line
            {
                painter.text(pos2(end_of_text.max.x + char_w * 4.0, end_of_text.center().y), Align2::LEFT_CENTER, note, egui::FontId::proportional(look.font_size * 0.92), p.faint);
            }
            // What's being composed, over the text after the caret, on the panel's colour and
            // underlined, as input methods show it everywhere else.
            if let Some(pre) = &ed.view.preedit
                && ed.buf.line_of(primary.head) == line
            {
                let c = at(caret_rect(&g, primary.head - start));
                let r = painter.text(pos2(c.min.x, c.center().y), Align2::LEFT_CENTER, pre, mono(look.font_size), p.text);
                painter.rect_filled(r.expand(1.0), 2.0, p.panel2);
                painter.text(pos2(c.min.x, c.center().y), Align2::LEFT_CENTER, pre, mono(look.font_size), p.text);
                painter.line_segment([pos2(r.left(), r.bottom()), pos2(r.right(), r.bottom())], Stroke::new(1.0, p.accent));
            }
            for s in ed.sels.iter().filter(|s| ed.buf.line_of(s.head) == line) {
                let c = at(caret_rect(&g, s.head - start));
                let r = Rect::from_min_size(pos2(c.min.x - 0.5, c.min.y + 1.0), vec2(2.0, c.height() - 2.0));
                painter.rect_filled(r, 0.0, if focused { p.accent } else { p.dim });
                if *s == primary {
                    caret_screen = Some(r);
                }
            }
        }
        if remeasure {
            ui.ctx().request_repaint();
        }
        if ed.reveal {
            ed.reveal = false;
            let (line, col) = ed.buf.point(ed.sels.primary().head);
            let g = galley(ui, ed, line, &m, p.text);
            let c = caret_rect(&g, col).translate(vec2(origin.x + PAD, origin.y + ed.view.rows.top(line, row_h)));
            ui.scroll_to_rect(c.expand2(vec2(char_w * 6.0, row_h * 2.0)), None);
        }
        if let Some(line) = ed.view.center_on.take() {
            let y = origin.y + ed.view.rows.top(line, row_h);
            ui.scroll_to_rect(Rect::from_min_size(pos2(origin.x, y), vec2(1.0, row_h)), Some(egui::Align::Center));
        }
        shown.response = Some(resp);
    });

    paint_gutter(ui, ed, look, gutter, origin.y, row_h, visible.clone(), &mut shown, id);
    if let Some(mm) = minimap {
        paint_minimap(ui, ed, p, mm, visible.clone(), id);
    }

    if focused && let Some(r) = caret_screen {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
        ui.output_mut(|o| o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: to_global * text_rect, cursor_rect: to_global * r, should_interrupt_composition: false }));
    }
    shown
}

#[allow(clippy::too_many_arguments)]
/// A wavy line under a stretch of text, as editors mark problems.
fn squiggle(painter: &egui::Painter, r: Rect, color: Color32) {
    let y = r.bottom() - 2.0;
    let mut points = Vec::new();
    let mut x = r.left();
    let mut up = true;
    while x <= r.right() {
        points.push(pos2(x, if up { y - 1.5 } else { y + 0.5 }));
        x += 2.0;
        up = !up;
    }
    if points.len() > 1 {
        painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_gutter(ui: &mut Ui, ed: &mut Editor, look: &Look, gutter: Rect, top: f32, row_h: f32, visible: std::ops::Range<usize>, shown: &mut Shown, id: Id) {
    let p = look.pal;
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), gutter);
    painter.rect_filled(gutter, 0.0, p.bg);
    let caret_lines: Vec<usize> = ed.sels.iter().map(|s| ed.buf.line_of(s.head)).collect();
    let font = mono(look.font_size * 0.92);
    let fold_x = gutter.right() - 16.0;
    let resp = ui.interact(gutter, id.with("gutter"), Sense::click());
    let hover = resp.hover_pos();
    for line in visible {
        let h = ed.view.rows.height(line, row_h);
        if h <= 0.0 {
            continue;
        }
        let y = top + ed.view.rows.top(line, row_h);
        let on = caret_lines.contains(&line);
        painter.text(pos2(gutter.right() - 24.0, y + row_h / 2.0), Align2::RIGHT_CENTER, (line + 1).to_string(), font.clone(), if on { p.text } else { p.faint });
        // A fold caret where the next lines are indented deeper: always for a folded one,
        // otherwise while the pointer is over the gutter, as editors keep it quiet.
        let folded = ed.folds.iter().any(|f| f.0 == line);
        if folded || (hover.is_some() && ed.can_fold(line)) {
            let glyph = if folded { crate::chrome::icon::CARET_RIGHT } else { crate::chrome::icon::CARET_DOWN };
            painter.text(pos2(fold_x, y + row_h / 2.0), Align2::CENTER_CENTER, glyph, egui::FontId::proportional(look.font_size * 0.85), if folded { p.text } else { p.faint });
        }
        let mark = look.marks.get(&line).map(|m| (*m, 1.0)).or_else(|| look.staged_marks.get(&line).map(|m| (*m, 0.4)));
        if let Some((m, alpha)) = mark {
            let x = gutter.right() - 6.0;
            match m {
                Mark::Added => {
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, h)), 0.0, p.add_fg.gamma_multiply(alpha));
                }
                Mark::Modified => {
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, h)), 0.0, p.series[3].gamma_multiply(alpha));
                }
                Mark::Deleted => {
                    let c = p.del_fg.gamma_multiply(alpha);
                    painter.add(egui::Shape::convex_polygon(vec![pos2(x - 2.0, y - 4.0), pos2(x + 4.0, y), pos2(x - 2.0, y + 4.0)], c, Stroke::NONE));
                }
            }
        }
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let line = ed.view.rows.line_at(pos.y - top, ed.buf.line_count(), row_h);
        if (pos.x - fold_x).abs() < 8.0 && (ed.can_fold(line) || ed.folds.iter().any(|f| f.0 == line)) {
            ed.toggle_fold(line);
        } else {
            shown.gutter_click = Some(line);
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
}

/// The whole file in miniature, each line a strip of its colours, with the part in view
/// marked. A click or drag there scrolls to it.
fn paint_minimap(ui: &mut Ui, ed: &mut Editor, p: &Palette, rect: Rect, visible: std::ops::Range<usize>, id: Id) {
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), rect);
    painter.rect_filled(rect, 0.0, p.panel);
    painter.line_segment([rect.left_top(), rect.left_bottom()], Stroke::new(1.0, p.border));
    let lines = ed.buf.line_count();
    let fits = (rect.height() / MINIMAP_LINE).floor().max(1.0) as usize;
    // Longer files show the stretch around the view, as it scrolls.
    let shown_lines = visible.end.saturating_sub(visible.start);
    let first = if lines <= fits { 0 } else { visible.start.saturating_sub(fits.saturating_sub(shown_lines) / 2).min(lines - fits) };
    let last = (first + fits).min(lines);
    if !ed.hl.ensure(&ed.buf, last, Duration::from_millis(3)) {
        ui.ctx().request_repaint();
    }
    let x0 = rect.left() + 6.0;
    let per_char = 0.9;
    let max_x = rect.right() - 4.0;
    for line in first..last {
        let y = rect.top() + (line - first) as f32 * MINIMAP_LINE;
        let text = ed.buf.line(line);
        let span_list = ed.hl.line(line).cloned().unwrap_or_default();
        let mut runs: Vec<(usize, usize, Color32)> = span_list.into_iter().filter(|s| s.0 < text.len()).collect();
        if runs.is_empty() && !text.trim().is_empty() {
            runs.push((0, text.len(), p.dim));
        }
        for (a, b, color) in runs {
            let piece = &text[a.min(text.len())..b.min(text.len())];
            if piece.trim().is_empty() {
                continue;
            }
            let lead = piece.len() - piece.trim_start().len();
            let xa = x0 + (a + lead) as f32 * per_char;
            let xb = (x0 + (a + piece.trim_end().len()) as f32 * per_char).min(max_x);
            if xa < xb {
                painter.rect_filled(Rect::from_min_max(pos2(xa, y), pos2(xb, y + MINIMAP_LINE - 0.5)), 0.0, color.gamma_multiply(0.75));
            }
        }
    }
    let vy0 = rect.top() + visible.start.saturating_sub(first) as f32 * MINIMAP_LINE;
    let vy1 = rect.top() + visible.end.saturating_sub(first) as f32 * MINIMAP_LINE;
    painter.rect_filled(Rect::from_min_max(pos2(rect.left() + 1.0, vy0), pos2(rect.right(), vy1.max(vy0 + 4.0))), 0.0, p.hover.gamma_multiply(0.6));
    let resp = ui.interact(rect, id.with("minimap"), Sense::click_and_drag());
    if (resp.clicked() || resp.dragged())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let line = first + ((pos.y - rect.top()) / MINIMAP_LINE).max(0.0) as usize;
        ed.view.center_on = Some(line.min(lines.saturating_sub(1)));
        ui.ctx().request_repaint();
    }
}

/// Where in the text a point on screen is.
fn hit(ui: &Ui, ed: &Editor, origin: Pos2, m: &Metrics, base: Color32, at: Pos2) -> usize {
    let line = ed.view.rows.line_at(at.y - origin.y, ed.buf.line_count(), m.row_h);
    let g = galley(ui, ed, line, m, base);
    let top = ed.view.rows.top(line, m.row_h);
    let col = g.cursor_from_pos(vec2(at.x - origin.x - PAD, (at.y - origin.y - top).max(0.0))).index.0;
    ed.buf.pos(line, col)
}

#[allow(clippy::too_many_arguments)]
fn mouse(ui: &Ui, ed: &mut Editor, resp: &Response, origin: Pos2, m: &Metrics, p: &Palette, now: f64) {
    let Some(at) = ui.input(|i| i.pointer.interact_pos()) else { return };
    let mods = ui.input(|i| i.modifiers);
    let pressed = resp.hovered() && ui.input(|i| i.pointer.primary_pressed());
    if pressed {
        let pos = hit(ui, ed, origin, m, p.text, at);
        let count = match ed.view.last_click {
            Some((t, at_pos, n)) if now - t < MULTI_CLICK_SECS && at_pos == pos => n % 3 + 1,
            _ => 1,
        };
        ed.view.last_click = Some((now, pos, count));
        ed.hist.break_run();
        let unit = match count {
            2 => Unit::Word,
            3 => Unit::Line,
            _ => Unit::Char,
        };
        let (s, e) = unit_range(ed, pos, unit);
        let column = (mods.alt && mods.shift).then(|| (ed.buf.line_of(pos), at.x));
        if mods.shift && !mods.alt {
            let anchor = ed.sels.primary().anchor;
            ed.sels = SelectionSet::single(Selection::new(anchor, pos));
            ed.view.drag = Some(Drag { origin: (anchor, anchor), unit: Unit::Char, column: None, adding: false, last: Selection::new(anchor, pos) });
        } else {
            let sel = Selection::new(s, e);
            if mods.alt && !mods.shift {
                ed.sels.add(sel);
            } else {
                ed.sels = SelectionSet::single(sel);
            }
            ed.view.drag = Some(Drag { origin: (s, e), unit, column, adding: mods.alt && !mods.shift, last: sel });
        }
        return;
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        ed.view.drag = None;
    }
    if resp.secondary_clicked() {
        let pos = hit(ui, ed, origin, m, p.text, at);
        if !ed.sels.iter().any(|s| s.start() <= pos && pos <= s.end()) {
            ed.sels = SelectionSet::single(Selection::caret(pos));
        }
        return;
    }
    let Some(drag) = &ed.view.drag else { return };
    if !resp.dragged() {
        return;
    }
    let (origin_range, unit, column, adding, last) = (drag.origin, drag.unit, drag.column, drag.adding, drag.last);
    let pos = hit(ui, ed, origin, m, p.text, at);
    if let Some((line0, x0)) = column {
        // One selection per line, between the two x positions.
        let line1 = ed.buf.line_of(pos);
        let (a, b) = (line0.min(line1), line0.max(line1));
        let mut sels = Vec::new();
        for l in a..=b {
            let y = origin.y + ed.view.rows.top(l, m.row_h) + m.row_h / 2.0;
            let s = hit(ui, ed, origin, m, p.text, pos2(x0, y));
            let e = hit(ui, ed, origin, m, p.text, pos2(at.x, y));
            sels.push(Selection::new(s, e));
        }
        ed.sels = SelectionSet::from_vec(sels, if line1 >= line0 { b - a } else { 0 });
    } else {
        let (os, oe) = origin_range;
        let (s, e) = unit_range(ed, pos, unit);
        let sel = if s < os { Selection::new(oe, s) } else { Selection::new(os, e.max(oe)) };
        if adding {
            let mut kept: Vec<Selection> = ed.sels.iter().copied().filter(|x| *x != last).collect();
            kept.push(sel);
            let n = kept.len() - 1;
            ed.sels = SelectionSet::from_vec(kept, n);
        } else {
            ed.sels = SelectionSet::single(sel);
        }
        if let Some(d) = &mut ed.view.drag {
            d.last = sel;
        }
    }
    // Dragging past the edge scrolls.
    ui.scroll_to_rect(Rect::from_center_size(at, vec2(4.0, m.row_h)), None);
}

/// Each caret a wrapped row up or down: within its line if the line has another row that
/// way, else into the next shown line's nearest row. Folded lines are passed over.
fn visual_move(ui: &Ui, ed: &mut Editor, m: &Metrics, up: bool, extend: bool) {
    let primary = ed.sels.primary();
    let goal = ed.view.goal_x;
    let mut primary_x = None;
    let mut moved = Vec::new();
    for s in ed.sels.iter().copied().collect::<Vec<_>>() {
        let (line, col) = ed.buf.point(s.head);
        let g = galley(ui, ed, line, m, Color32::WHITE);
        let here = caret_rect(&g, col);
        let x = if s == primary { goal.unwrap_or(here.center().x) } else { here.center().x };
        if s == primary {
            primary_x = Some(x);
        }
        let row = g.rows.iter().position(|r| here.center().y >= r.min_y() && here.center().y <= r.max_y()).unwrap_or(0);
        let target = if up { row.checked_sub(1).map(|r| (line, r)) } else { (row + 1 < g.rows.len()).then_some((line, row + 1)) };
        let head = match target {
            Some((l, r)) => {
                let y = g.rows[r].rect().center().y;
                ed.buf.pos(l, g.cursor_from_pos(vec2(x, y)).index.0)
            }
            None => {
                // The next line that isn't folded away, entering at its last row going up or
                // its first going down.
                let mut l = line;
                let next = loop {
                    l = if up { match l.checked_sub(1) { Some(v) => v, None => break None } } else { l + 1 };
                    if l >= ed.buf.line_count() {
                        break None;
                    }
                    if !ed.folds.iter().any(|f| l > f.0 && l <= f.1) {
                        break Some(l);
                    }
                };
                match next {
                    Some(l) => {
                        let g2 = galley(ui, ed, l, m, Color32::WHITE);
                        let r = if up { g2.rows.len().saturating_sub(1) } else { 0 };
                        let y = g2.rows.get(r).map_or(0.0, |row| row.rect().center().y);
                        ed.buf.pos(l, g2.cursor_from_pos(vec2(x, y)).index.0)
                    }
                    None if up => 0,
                    None => ed.buf.len_chars(),
                }
            }
        };
        moved.push(Selection { anchor: if extend { s.anchor } else { head }, head, goal: None });
    }
    let primary_index = ed.sels.primary_index();
    ed.sels = SelectionSet::from_vec(moved, primary_index);
    ed.view.goal_x = primary_x;
    ed.hist.break_run();
    ed.reveal = true;
}

#[cfg(test)]
mod visual_tests {
    use super::*;

    /// With wrapping, Down goes to the next row of the same line before the next line.
    #[test]
    fn down_goes_a_wrapped_row_at_a_time() {
        let ctx = egui::Context::default();
        let text = format!("{}\nnext\n", "word ".repeat(40));
        let mut ed = Editor::new("a.txt", gitem_text::Buffer::from_text(&text), true, gitem_text::indent::Indent::Spaces(4));
        let mut rows_seen = Vec::new();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let char_w = ui.fonts_mut(|f| f.glyph_width(&mono(13.0), '0'));
            let m = Metrics { row_h: 20.0, char_w, font_size: 13.0, wrap: Some(char_w * 30.0) };
            let rows = galley(ui, &ed, 0, &m, Color32::WHITE).rows.len();
            assert!(rows >= 6, "200 chars at 30 a row: {rows}");
            for _ in 0..rows {
                visual_move(ui, &mut ed, &m, false, false);
                rows_seen.push(ed.buf.point(ed.sels.primary().head));
            }
            visual_move(ui, &mut ed, &m, true, false);
            rows_seen.push(ed.buf.point(ed.sels.primary().head));
        });
        out.textures_delta.clear();
        let on_first: Vec<_> = rows_seen.iter().take_while(|p| p.0 == 0).collect();
        assert_eq!(on_first.len() + 1, rows_seen.len() - 1, "every row of line 1 first, then line 2: {rows_seen:?}");
        assert_eq!(rows_seen[rows_seen.len() - 2].0, 1);
        assert_eq!(rows_seen.last().unwrap().0, 0, "and back up to the last row of line 1");
        assert!(rows_seen.windows(2).take(on_first.len()).all(|w| w[1].1 > w[0].1 || w[1].0 > w[0].0), "{rows_seen:?}");
    }
}

fn unit_range(ed: &Editor, pos: usize, unit: Unit) -> (usize, usize) {
    match unit {
        Unit::Char => (pos, pos),
        Unit::Word => selection::word_at(&ed.buf, pos),
        Unit::Line => {
            let l = ed.buf.line_of(pos);
            let end = if l + 1 < ed.buf.line_count() { ed.buf.line_start(l + 1) } else { ed.buf.len_chars() };
            (ed.buf.line_start(l), end)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn keyboard(ui: &Ui, ed: &mut Editor, page: isize, now_ms: u64, look: &Look, m: &Metrics, shown: &mut Shown) {
    let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false };
    let events = ui.input(|i| i.filtered_events(&filter));
    let pressed = |name: &str| ui.input(|i| crate::keys::context_pressed(i, look.keys, name));
    if pressed("editor_save") {
        shown.save = true;
    }
    if pressed("find") {
        open_find(ed, false);
    }
    if pressed("editor_replace") {
        open_find(ed, true);
    }
    if pressed("editor_goto") {
        ed.goto = Some(String::new());
    }
    if pressed("editor_outline") {
        shown.outline = true;
    }
    if pressed("find_next") {
        ed.find_step(false);
    }
    if pressed("find_prev") {
        ed.find_step(true);
    }
    let mac = cfg!(target_os = "macos");
    for e in events {
        match e {
            Event::Text(t) => ed.run(Cmd::Type(t), now_ms),
            Event::Paste(t) => ed.run(Cmd::Paste(t), now_ms),
            Event::Copy => ui.ctx().copy_text(ed.copied()),
            Event::Cut => {
                ui.ctx().copy_text(ed.copied());
                if ed.sels.iter().all(|s| s.is_empty()) {
                    ed.run(Cmd::DeleteLines, now_ms);
                } else {
                    ed.run(Cmd::Backspace { word: false }, now_ms);
                }
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => {
                ed.view.preedit = None;
                if !t.is_empty() {
                    ed.run(Cmd::Type(t), now_ms);
                }
            }
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => ed.view.preedit = (!text.is_empty()).then_some(text),
            // Some input methods replace what's around the caret, as when a word is corrected.
            Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars, after_chars }) => {
                let p = ed.sels.primary().head;
                let (s, e) = (p.saturating_sub(before_chars), (p + after_chars).min(ed.buf.len_chars()));
                if s < e {
                    ed.sels = SelectionSet::single(Selection::new(s, e));
                    ed.run(Cmd::Backspace { word: false }, now_ms);
                }
            }
            // While composing, the input method has the keys: Enter, arrows and Backspace work
            // on what it's composing, not on the text.
            Event::Key { .. } if ed.view.preedit.is_some() => {}
            Event::Key { key: Key::Z, pressed: true, modifiers, .. } if modifiers.alt && !modifiers.command && !modifiers.shift => shown.toggle_wrap = true,
            // With wrapping, Up and Down go a row at a time, as the text is seen.
            Event::Key { key: key @ (Key::ArrowUp | Key::ArrowDown), pressed: true, modifiers, .. } if m.wrap.is_some() && !modifiers.alt && !modifiers.command => {
                visual_move(ui, ed, m, key == Key::ArrowUp, modifiers.shift);
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                ed.view.goal_x = None;
                if let Some(cmd) = key_command(key, modifiers, page, mac) {
                    ed.run(cmd, now_ms);
                }
            }
            _ => {}
        }
    }
}

fn open_find(ed: &mut Editor, replacing: bool) {
    ed.find.open = true;
    ed.find.replacing = replacing;
    ed.find.focus_query = true;
    let seed = ed.search_seed();
    if !seed.is_empty() && !ed.sels.primary().is_empty() {
        ed.find.query.text = seed;
    }
}

/// The command a key press means in the editor, if any.
pub fn key_command(key: Key, m: Modifiers, page: isize, mac: bool) -> Option<Cmd> {
    let shift = m.shift;
    let word = if mac { m.alt } else { m.command };
    Some(match key {
        Key::ArrowLeft if m.command && mac => Cmd::Move(Move::Home, shift),
        Key::ArrowRight if m.command && mac => Cmd::Move(Move::End, shift),
        Key::ArrowLeft => Cmd::Move(if word { Move::WordLeft } else { Move::Left }, shift),
        Key::ArrowRight => Cmd::Move(if word { Move::WordRight } else { Move::Right }, shift),
        Key::ArrowUp if m.alt && shift => Cmd::DuplicateLines,
        Key::ArrowDown if m.alt && shift => Cmd::DuplicateLines,
        Key::ArrowUp if m.alt => Cmd::MoveLines { up: true },
        Key::ArrowDown if m.alt => Cmd::MoveLines { up: false },
        Key::ArrowUp if m.command && shift => Cmd::AddCaret { up: true },
        Key::ArrowDown if m.command && shift => Cmd::AddCaret { up: false },
        Key::ArrowUp if m.command && mac => Cmd::Move(Move::DocStart, shift),
        Key::ArrowDown if m.command && mac => Cmd::Move(Move::DocEnd, shift),
        Key::ArrowUp => Cmd::Move(Move::Up, shift),
        Key::ArrowDown => Cmd::Move(Move::Down, shift),
        Key::Home => Cmd::Move(if m.command { Move::DocStart } else { Move::Home }, shift),
        Key::End => Cmd::Move(if m.command { Move::DocEnd } else { Move::End }, shift),
        Key::PageUp => Cmd::Move(Move::Lines(-page), shift),
        Key::PageDown => Cmd::Move(Move::Lines(page), shift),
        Key::Backspace => Cmd::Backspace { word },
        Key::Delete => Cmd::Delete { word },
        Key::Enter if m.command => Cmd::NewlineBelow,
        Key::Enter => Cmd::Newline,
        Key::Tab if shift => Cmd::Outdent,
        Key::Tab => Cmd::Tab,
        Key::Escape => Cmd::Collapse,
        Key::A if m.command => Cmd::SelectAll,
        Key::Z if m.command && shift => Cmd::Redo,
        Key::Z if m.command => Cmd::Undo,
        Key::Y if m.command && !mac => Cmd::Redo,
        Key::D if m.command => Cmd::SelectNext,
        Key::F2 if m.command => Cmd::SelectAllMatches,
        Key::L if m.command => Cmd::SelectLine,
        Key::J if m.command => Cmd::JoinLines,
        Key::Slash if m.command => Cmd::ToggleComment,
        Key::OpenBracket if m.command && shift => Cmd::Fold,
        Key::CloseBracket if m.command && shift => Cmd::Unfold,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_table_skips_folds_and_counts_wrapped_rows() {
        let text = "fn a() {\n    one\n    two\n}\nshort\nlong line here that wraps\n";
        let mut ed = Editor::new("a.rs", gitem_text::Buffer::from_text(text), true, gitem_text::indent::Indent::Spaces(4));
        let m = Metrics { row_h: 10.0, char_w: 1.0, font_size: 10.0, wrap: None };
        let mut rows = Rows::default();
        rows.update(&ed, &m);
        assert!(rows.tops.is_empty(), "one row a line needs no table");
        assert_eq!(rows.top(4, 10.0), 40.0);
        ed.toggle_fold(0);
        rows.update(&ed, &m);
        assert_eq!(rows.height(1, 10.0), 0.0);
        assert_eq!(rows.top(4, 10.0), 20.0, "lines 1 and 2 are folded away");
        assert_eq!(rows.line_at(15.0, ed.buf.line_count(), 10.0), 3);
        let m = Metrics { wrap: Some(10.0), ..m };
        rows.update(&ed, &m);
        assert_eq!(rows.height(5, 10.0), 30.0, "25 chars at 10 a row is 3 rows");
        assert!(rows.measured(5, 2, 3));
        rows.update(&ed, &m);
        assert_eq!(rows.height(5, 10.0), 20.0, "what was drawn wins over the estimate");
    }
}
