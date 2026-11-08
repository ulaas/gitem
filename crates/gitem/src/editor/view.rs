//! Drawing the editor and turning keys and the mouse into commands. Only the rows in view are
//! laid out, so a file of a million lines costs what a screenful does.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Align2, Color32, CursorIcon, Event, EventFilter, Galley, Id, Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use gitem_text::brackets;
use gitem_text::selection::{self, Move};
use gitem_text::{Selection, SelectionSet};

use super::{Cmd, Editor};
use crate::theme::Palette;
use crate::widgets::mono;

/// Space between the gutter and the text.
const PAD: f32 = 6.0;

/// Two clicks this close in time, at the same place, are a double click.
const MULTI_CLICK_SECS: f64 = 0.4;

#[derive(Default)]
pub struct ViewState {
    /// When and where the last click was, and how many in a row, for double and triple clicks.
    last_click: Option<(f64, usize, u8)>,
    drag: Option<Drag>,
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
    /// Changes against the index, by line, and those only against HEAD (staged already).
    pub marks: &'a HashMap<usize, Mark>,
    pub staged_marks: &'a HashMap<usize, Mark>,
    /// Shown after the end of the caret's line, as for blame.
    pub line_note: Option<&'a str>,
    /// The keys the user changed, for Find and the like.
    pub keys: &'a HashMap<String, String>,
}

#[derive(Default)]
pub struct Shown {
    /// Ctrl+S was pressed.
    pub save: bool,
    /// A line number or change mark was clicked.
    pub gutter_click: Option<usize>,
    /// The text area, for a right-click menu.
    pub response: Option<Response>,
    pub focused: bool,
}

/// Lays out one line as it's drawn, with its colours.
fn galley(ui: &Ui, ed: &Editor, line: usize, font_size: f32, base: Color32) -> Arc<Galley> {
    let text = ed.buf.line(line);
    let font = mono(font_size);
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let spans = ed.hl.line(line).map(|s| s.as_slice()).unwrap_or(&[]);
    let mut at = 0;
    for &(a, b, color) in spans {
        // Colours from before an edit may not fit the line as it is now.
        let (a, b) = (a.min(text.len()), b.min(text.len()));
        if a < at || !text.is_char_boundary(a) || !text.is_char_boundary(b) || a >= b {
            continue;
        }
        if a > at {
            job.append(&text[at..a], 0.0, TextFormat { font_id: font.clone(), color: base, ..Default::default() });
        }
        job.append(&text[a..b], 0.0, TextFormat { font_id: font.clone(), color, ..Default::default() });
        at = b;
    }
    if at < text.len() || text.is_empty() {
        job.append(&text[at..], 0.0, TextFormat { font_id: font, color: base, ..Default::default() });
    }
    ui.fonts_mut(|f| f.layout_job(job))
}

fn x_of(g: &Galley, col: usize) -> f32 {
    g.pos_from_cursor(CCursor::new(col)).min.x
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
    let gutter_w = (digits.max(3.0) * char_w + 22.0).round();
    let outer = ui.available_rect_before_wrap();
    ui.allocate_rect(outer, Sense::hover());
    let gutter = Rect::from_min_max(outer.min, pos2(outer.min.x + gutter_w, outer.max.y));
    let text_rect = Rect::from_min_max(pos2(gutter.max.x, outer.min.y), outer.max);
    let mut shown = Shown::default();
    let focused = ui.memory(|m| m.has_focus(id));
    shown.focused = focused;
    if focused {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    }
    let now = ui.input(|i| i.time);
    let now_ms = (now * 1000.0) as u64;

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let mut origin = text_rect.min;
    let mut visible = 0..0;
    let mut caret_screen: Option<Rect> = None;
    egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false]).show_viewport(&mut child, |ui, vp| {
        origin = ui.max_rect().min;
        let page = (vp.height() / row_h).floor().max(1.0) as isize;
        // Room to scroll the last line up to the middle, as editors allow.
        let size = vec2((ed.max_cols as f32 + 4.0) * char_w + PAD * 2.0, lines as f32 * row_h + (vp.height() * 0.5).max(row_h));
        ui.set_min_size(size);
        let area = Rect::from_min_size(origin, size.max(vp.size()));
        // Only what's in view takes the mouse: the whole text can be millions of pixels tall.
        let shown_area = Rect::from_min_size(origin + vp.min.to_vec2(), vp.size());
        let resp = ui.interact(shown_area, id, Sense::click_and_drag());
        let name = format!("Text of {}", ed.path);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &name));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }

        // Keys first, so what's drawn is what they did.
        if focused {
            keyboard(ui, ed, page, now_ms, look, &mut shown);
        }
        mouse(ui, ed, &resp, origin, row_h, look.font_size, p, now);
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() || std::mem::take(&mut ed.focus) {
            resp.request_focus();
        }

        let first = ((vp.min.y / row_h).floor().max(0.0)) as usize;
        let last = (((vp.max.y / row_h).ceil()) as usize + 1).min(lines);
        visible = first..last.max(first);
        if !ed.hl.ensure(&ed.buf, last + 1, Duration::from_millis(6)) {
            ui.ctx().request_repaint();
        }
        if ed.find.open {
            ed.refresh_matches();
        }
        let painter = ui.painter_at(ui.clip_rect());
        let width = area.width().max(vp.width());
        let primary = ed.sels.primary();
        let bracket = if primary.is_empty() { brackets::matching(&ed.buf, primary.head) } else { None };
        let unit = ed.indent.unit().replace('\t', "    ").chars().count().max(1);
        for line in visible.clone() {
            let y = origin.y + line as f32 * row_h;
            let row = Rect::from_min_size(pos2(origin.x, y), vec2(width, row_h));
            let g = galley(ui, ed, line, look.font_size, p.text);
            let text_x = origin.x + PAD;
            let ty = y + (row_h - g.size().y) / 2.0;
            let (start, end) = (ed.buf.line_start(line), ed.buf.line_end(line));
            let len = end - start;
            if ed.sels.iter().any(|s| s.is_empty() && ed.buf.line_of(s.head) == line) {
                painter.rect_filled(row, 0.0, p.hover.gamma_multiply(0.55));
            }
            // Matches of the find box on this line.
            if ed.find.open {
                let ms = &ed.find.matches;
                let from = ms.partition_point(|m| m.1 <= start);
                for m in ms[from..].iter().take_while(|m| m.0 <= end) {
                    let (a, b) = (m.0.max(start) - start, m.1.min(end) - start);
                    let r = Rect::from_min_max(pos2(text_x + x_of(&g, a), y + 1.0), pos2(text_x + x_of(&g, b).max(x_of(&g, a) + 2.0), y + row_h - 1.0));
                    painter.rect_filled(r, 2.0, p.warn.gamma_multiply(0.28));
                }
            }
            for s in ed.sels.iter().filter(|s| !s.is_empty() && s.start() <= end + 1 && s.end() >= start) {
                let a = s.start().max(start) - start;
                let into_next = s.end() > end;
                let b = s.end().min(end) - start;
                let x0 = text_x + x_of(&g, a.min(len));
                let x1 = text_x + x_of(&g, b.min(len)) + if into_next { char_w * 0.6 } else { 0.0 };
                if x1 > x0 {
                    painter.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + row_h)), 0.0, p.selection);
                }
            }
            if look.indent_guides {
                let text = ed.buf.line(line);
                let lead = text.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
                if lead < text.len() || text.trim().is_empty() {
                    for k in (unit..lead).step_by(unit) {
                        let x = (text_x + k as f32 * char_w).round() + 0.5;
                        painter.line_segment([pos2(x, y), pos2(x, y + row_h)], Stroke::new(1.0, p.border.gamma_multiply(0.7)));
                    }
                }
            }
            painter.galley(pos2(text_x, ty), g.clone(), p.text);
            if look.whitespace {
                for (i, c) in ed.buf.line(line).chars().enumerate() {
                    let mark = match c {
                        ' ' => "·",
                        '\t' => "→",
                        _ => continue,
                    };
                    let x = text_x + (x_of(&g, i) + x_of(&g, i + 1)) / 2.0;
                    painter.text(pos2(x, y + row_h / 2.0), Align2::CENTER_CENTER, mark, mono(look.font_size * 0.9), p.faint);
                }
            }
            if let Some((a, b)) = bracket {
                for at in [a, b] {
                    if (start..=end).contains(&at) {
                        let c = at - start;
                        let r = Rect::from_min_max(pos2(text_x + x_of(&g, c), y + 1.0), pos2(text_x + x_of(&g, c + 1), y + row_h - 1.0));
                        painter.rect_stroke(r, 2.0, Stroke::new(1.0, p.dim), egui::StrokeKind::Inside);
                    }
                }
            }
            if let Some(note) = look.line_note
                && ed.buf.line_of(primary.head) == line
            {
                let x = text_x + g.size().x + char_w * 4.0;
                painter.text(pos2(x, y + row_h / 2.0), Align2::LEFT_CENTER, note, egui::FontId::proportional(look.font_size * 0.92), p.faint);
            }
            for s in ed.sels.iter().filter(|s| ed.buf.line_of(s.head) == line) {
                let x = text_x + x_of(&g, s.head - start);
                let r = Rect::from_min_size(pos2(x - 0.5, y + 1.0), vec2(2.0, row_h - 2.0));
                painter.rect_filled(r, 0.0, if focused { p.accent } else { p.dim });
                if *s == primary {
                    caret_screen = Some(r);
                }
            }
        }
        if ed.reveal {
            ed.reveal = false;
            let (line, col) = ed.buf.point(ed.sels.primary().head);
            let g = galley(ui, ed, line, look.font_size, p.text);
            let x = origin.x + PAD + x_of(&g, col);
            let r = Rect::from_min_size(pos2(x, origin.y + line as f32 * row_h), vec2(2.0, row_h));
            ui.scroll_to_rect(r.expand2(vec2(char_w * 6.0, row_h * 2.0)), None);
        }
        shown.response = Some(resp);
    });

    paint_gutter(ui, ed, look, gutter, origin.y, row_h, visible.clone(), &mut shown, id);

    if focused && let Some(r) = caret_screen {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
        ui.output_mut(|o| o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect: to_global * text_rect, cursor_rect: to_global * r, should_interrupt_composition: false }));
    }
    shown
}

#[allow(clippy::too_many_arguments)]
fn paint_gutter(ui: &mut Ui, ed: &Editor, look: &Look, gutter: Rect, top: f32, row_h: f32, visible: std::ops::Range<usize>, shown: &mut Shown, id: Id) {
    let p = look.pal;
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), gutter);
    painter.rect_filled(gutter, 0.0, p.bg);
    let caret_lines: Vec<usize> = ed.sels.iter().map(|s| ed.buf.line_of(s.head)).collect();
    let font = mono(look.font_size * 0.92);
    for line in visible {
        let y = top + line as f32 * row_h;
        let on = caret_lines.contains(&line);
        painter.text(pos2(gutter.right() - 12.0, y + row_h / 2.0), Align2::RIGHT_CENTER, (line + 1).to_string(), font.clone(), if on { p.text } else { p.faint });
        let mark = look.marks.get(&line).map(|m| (*m, 1.0)).or_else(|| look.staged_marks.get(&line).map(|m| (*m, 0.4)));
        if let Some((m, alpha)) = mark {
            let x = gutter.right() - 6.0;
            match m {
                Mark::Added => {
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, row_h)), 0.0, p.add_fg.gamma_multiply(alpha));
                }
                Mark::Modified => {
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, row_h)), 0.0, p.series[3].gamma_multiply(alpha));
                }
                Mark::Deleted => {
                    let c = p.del_fg.gamma_multiply(alpha);
                    painter.add(egui::Shape::convex_polygon(vec![pos2(x - 2.0, y - 4.0), pos2(x + 4.0, y), pos2(x - 2.0, y + 4.0)], c, Stroke::NONE));
                }
            }
        }
    }
    let resp = ui.interact(gutter, id.with("gutter"), Sense::click());
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let line = ((pos.y - top) / row_h).floor().max(0.0) as usize;
        if line < ed.buf.line_count() {
            shown.gutter_click = Some(line);
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
}

/// Where in the text a point on screen is.
fn hit(ui: &Ui, ed: &Editor, origin: Pos2, row_h: f32, font_size: f32, base: Color32, at: Pos2) -> usize {
    let line = (((at.y - origin.y) / row_h).floor().max(0.0) as usize).min(ed.buf.line_count().saturating_sub(1));
    let g = galley(ui, ed, line, font_size, base);
    let col = g.cursor_from_pos(vec2(at.x - origin.x - PAD, row_h / 2.0)).index.0;
    ed.buf.pos(line, col)
}

#[allow(clippy::too_many_arguments)]
fn mouse(ui: &Ui, ed: &mut Editor, resp: &Response, origin: Pos2, row_h: f32, font_size: f32, p: &Palette, now: f64) {
    let Some(at) = ui.input(|i| i.pointer.interact_pos()) else { return };
    let mods = ui.input(|i| i.modifiers);
    let pressed = resp.hovered() && ui.input(|i| i.pointer.primary_pressed());
    if pressed {
        let pos = hit(ui, ed, origin, row_h, font_size, p.text, at);
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
            let p = ed.sels.primary();
            let anchor = p.anchor;
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
        let pos = hit(ui, ed, origin, row_h, font_size, p.text, at);
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
    let pos = hit(ui, ed, origin, row_h, font_size, p.text, at);
    if let Some((line0, x0)) = column {
        // One selection per line, between the two x positions.
        let line1 = ed.buf.line_of(pos);
        let (a, b) = (line0.min(line1), line0.max(line1));
        let mut sels = Vec::new();
        for l in a..=b {
            let y = origin.y + l as f32 * row_h + row_h / 2.0;
            let s = hit(ui, ed, origin, row_h, font_size, p.text, pos2(x0, y));
            let e = hit(ui, ed, origin, row_h, font_size, p.text, pos2(at.x, y));
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
    ui.scroll_to_rect(Rect::from_center_size(at, vec2(4.0, row_h)), None);
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

fn keyboard(ui: &Ui, ed: &mut Editor, page: isize, now_ms: u64, look: &Look, shown: &mut Shown) {
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
            Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => ed.run(Cmd::Type(t), now_ms),
            Event::Key { key, pressed: true, modifiers, .. } => {
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
        _ => return None,
    })
}
