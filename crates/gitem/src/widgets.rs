//! Shared drawing helpers: badges, avatars, rows, code lines, status chips.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::text::LayoutJob;
use egui::{Align2, Color32, FontFamily, FontId, Rect, Response, RichText, Sense, Stroke, TextFormat, TextureHandle, Ui, Vec2, pos2, vec2};
use gitem_core::model::*;
use gitem_core::util;

use crate::syntax::Spans;
use crate::theme::Palette;

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn prop(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Left-aligned text, vertically centred, ending in an ellipsis if it doesn't fit, so that a
/// clipped word can't be taken for the whole of it.
pub fn paint_text(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(1.0));
    let g = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, color);
}

/// A folder path cut in the middle to about `chars` characters: its two ends say the most.
pub fn middle_elided(path: &str, chars: usize) -> String {
    let n = path.chars().count();
    if n <= chars || chars < 5 {
        return path.to_string();
    }
    let keep = chars - 1;
    let (head, tail) = (keep / 2, keep - keep / 2);
    let start: String = path.chars().take(head).collect();
    let end: String = path.chars().skip(n - tail).collect();
    format!("{start}\u{2026}{end}")
}

#[cfg(test)]
#[test]
fn a_long_path_keeps_both_its_ends() {
    assert_eq!(middle_elided("short/", 20), "short/");
    assert_eq!(middle_elided("src/views/deep/down/here/", 12), "src/v\u{2026}/here/");
}

pub fn ref_color(kind: RefKind, current: bool, p: &Palette) -> Color32 {
    if current {
        return p.accent;
    }
    match kind {
        RefKind::Branch => p.series[2],
        RefKind::Remote => p.series[6],
        RefKind::Tag => p.series[3],
        RefKind::Stash => p.series[4],
        RefKind::Head => p.accent,
    }
}

pub fn ref_glyph(kind: RefKind) -> String {
    use crate::chrome::icon;
    match kind {
        RefKind::Tag => format!("{} ", icon::TAG),
        RefKind::Remote => format!("{} ", icon::CLOUD),
        RefKind::Stash => format!("{} ", icon::ARCHIVE),
        _ => String::new(),
    }
}

/// Paint a ref badge at `pos`, returning its rectangle.
/// `lane` is the branch's colour in the graph, where it has one: otherwise the kind's colour.
pub fn paint_badge(ui: &Ui, pos: egui::Pos2, h: f32, r: &RefLabel, lane: Option<Color32>, p: &Palette) -> Rect {
    let color = lane.unwrap_or_else(|| ref_color(r.kind, r.current, p));
    let text = format!("{}{}", ref_glyph(r.kind), r.name);
    let galley = ui.painter().layout_no_wrap(text, prop(11.0), p.text);
    let rect = Rect::from_min_size(pos, vec2(galley.size().x + 10.0, h));
    let fill = if r.current { color.gamma_multiply(0.55) } else { color.gamma_multiply(0.22) };
    ui.painter().rect(rect, 3.0, fill, Stroke::new(1.0, color.gamma_multiply(0.7)), egui::StrokeKind::Inside);
    ui.painter().galley(pos2(rect.left() + 5.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
    rect
}

pub fn badge_width(ui: &Ui, r: &RefLabel) -> f32 {
    let text = format!("{}{}", ref_glyph(r.kind), r.name);
    ui.painter().layout_no_wrap(text, prop(11.0), Color32::WHITE).size().x + 10.0
}

pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let ink = crate::theme::chip_ink(color, ui.visuals().panel_fill, ui.visuals().text_color());
    let g = ui.painter().layout_no_wrap(text.to_string(), prop(11.0), ink);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(10.0, 4.0), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter().rect(rect, 3.0, color.gamma_multiply(0.14), Stroke::new(1.0, color.gamma_multiply(0.5)), egui::StrokeKind::Inside);
    ui.painter().galley(rect.min + vec2(5.0, 2.0), g, ink);
}

fn initials(name: &str) -> String {
    let mut it = name.split_whitespace().filter_map(|w| w.chars().next());
    let a = it.next();
    let b = it.next_back();
    match (a, b) {
        (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".to_string(),
    }
}

enum AvatarState {
    Loading,
    Missing,
    Ready(egui::ColorImage),
    Texture(TextureHandle),
}

type AvatarCache = Arc<Mutex<HashMap<String, AvatarState>>>;

fn avatar_cache() -> &'static AvatarCache {
    static C: OnceLock<AvatarCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Fetch a gravatar once per e-mail, in the background (opt-in via Settings).
fn gravatar_texture(ctx: &egui::Context, email: &str) -> Option<TextureHandle> {
    let cache = avatar_cache();
    let mut map = cache.lock().ok()?;
    match map.get_mut(email) {
        Some(AvatarState::Texture(t)) => return Some(t.clone()),
        Some(AvatarState::Ready(_)) => {
            if let Some(AvatarState::Ready(img)) = map.remove(email) {
                let t = ctx.load_texture(format!("avatar:{email}"), img, egui::TextureOptions::LINEAR);
                map.insert(email.to_string(), AvatarState::Texture(t.clone()));
                return Some(t);
            }
        }
        Some(_) => {}
        None => {
            map.insert(email.to_string(), AvatarState::Loading);
            let (email, ctx, cache) = (email.to_string(), ctx.clone(), cache.clone());
            std::thread::spawn(move || {
                let url = gitem_core::forge::gravatar_url(&email, 64);
                let img = fetch_image(&url);
                if let Ok(mut m) = cache.lock() {
                    m.insert(email, img.map(AvatarState::Ready).unwrap_or(AvatarState::Missing));
                }
                ctx.request_repaint();
            });
        }
    }
    None
}

fn fetch_image(url: &str) -> Option<egui::ColorImage> {
    let bytes = gitem_core::forge::http_get_bytes(url)?;
    decode_image(&bytes)
}

/// File names the file view and diffs try `decode_image` on. SVG isn't one: `crate::svg` draws it,
/// and it's text too, so that's a choice beside the text and not in place of it.
pub const PICTURE_TYPES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

pub fn decode_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_flat_samples().as_slice()))
}

pub fn paint_avatar(ui: &Ui, center: egui::Pos2, radius: f32, name: &str, email: &str, p: &Palette, gravatar: bool) {
    if gravatar && !email.is_empty()
        && let Some(t) = gravatar_texture(ui.ctx(), email) {
            let rect = Rect::from_center_size(center, Vec2::splat(radius * 2.0));
            ui.painter().image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            return;
        }
    let c = p.person(if email.is_empty() { name } else { email });
    ui.painter().circle_filled(center, radius, c);
    ui.painter().text(center, Align2::CENTER_CENTER, initials(name), prop(radius * 0.95), crate::theme::ink_on(c));
}

/// A full-width clickable row with hover and selection fill.
pub fn row(ui: &mut Ui, h: f32, selected: bool, p: &Palette) -> (Rect, Response) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    row_look(ui, rect, resp, selected, p)
}

/// A row with an id of its own, for a list that only draws what's on screen. An automatic id
/// counts what's been drawn before it, so scrolling would change it and focus would be lost.
/// `key` has to be unique among everything on screen.
pub fn keyed_row(ui: &mut Ui, h: f32, selected: bool, p: &Palette, key: impl std::hash::Hash + std::fmt::Debug) -> (Rect, Response) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let resp = ui.interact(rect, egui::Id::new(key), Sense::click());
    row_look(ui, rect, resp, selected, p)
}

/// Keys move focus among what's drawn, and long lists only draw what's on screen, so rows beyond
/// a focused one have to be there by the next key. A held key leaves about two frames. This
/// isn't `gained_focus`, which an arrow key's move doesn't set.
pub fn keep_neighbours_in_view(ui: &Ui, rect: Rect, resp: &Response) {
    if resp.has_focus() {
        let seen = egui::Id::new("row-with-focus");
        if ui.data(|d| d.get_temp::<egui::Id>(seen)) != Some(resp.id) {
            ui.data_mut(|d| d.insert_temp(seen, resp.id));
            ui.scroll_to_rect_animation(rect.expand2(vec2(0.0, rect.height() * 3.5)), None, egui::style::ScrollAnimation::none());
        }
    }
}

fn row_look(ui: &mut Ui, rect: Rect, resp: Response, selected: bool, p: &Palette) -> (Rect, Response) {
    if selected {
        ui.painter().rect_filled(rect, 0.0, p.selection);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    focus_ring(ui, &resp, p);
    keep_neighbours_in_view(ui, rect, &resp);
    (rect, resp)
}

/// A line around whatever has the keyboard's attention. It has its own look, so that it
/// can't be taken for selected or switched on.
pub fn focus_ring(ui: &Ui, resp: &Response, p: &Palette) {
    if resp.has_focus() {
        ui.painter().rect_stroke(resp.rect.shrink(1.0), 2.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
    }
}

/// The usual right-click menu, which `by_key` also opens, beside the row rather than at a
/// pointer that may be anywhere.
pub fn keyed_context_menu(resp: &Response, by_key: bool, add: impl FnOnce(&mut Ui)) {
    let id = egui::Popup::default_response_id(resp).with("by-key");
    let ctx = resp.ctx.clone();
    if by_key {
        ctx.data_mut(|d| d.insert_temp(id, true));
    }
    if resp.secondary_clicked() {
        ctx.data_mut(|d| d.remove::<bool>(id));
    }
    let mut popup = egui::Popup::context_menu(resp);
    if by_key {
        popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
    }
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        popup = popup.anchor(resp);
    }
    popup.show(add);
}

/// Space between a text box's edge and its text. egui has no style for this, so every
/// text box in the app is made here.
const INPUT_MARGIN: egui::Margin = egui::Margin::symmetric(6, 5);

/// A one-line text box as tall as a button, so it lines up with those beside it. With text in
/// it, a × at its end empties it.
pub fn input(text: &mut dyn egui::TextBuffer) -> Input<'_> {
    Input { text, hint: None, width: None, font: None, id: None, id_salt: None, password: false, color: None }
}

/// What [`input`] makes: egui's one-line box, set up as every box here is, with a way to empty it.
pub struct Input<'t> {
    text: &'t mut dyn egui::TextBuffer,
    hint: Option<egui::WidgetText>,
    width: Option<f32>,
    font: Option<egui::FontSelection>,
    id: Option<egui::Id>,
    id_salt: Option<egui::Id>,
    password: bool,
    color: Option<Color32>,
}

/// The accessible name of the × that empties a box.
pub const CLEAR_TEXT: &str = "Clear text";

impl Input<'_> {
    pub fn hint_text(mut self, hint: impl Into<egui::WidgetText>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn desired_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    pub fn font(mut self, font: impl Into<egui::FontSelection>) -> Self {
        self.font = Some(font.into());
        self
    }
    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(egui::Id::new(salt));
        self
    }
    pub fn password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }
    pub fn text_color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
}

impl egui::Widget for Input<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        const SIDE: f32 = 18.0;
        let clearable = !self.text.as_str().is_empty() && self.text.is_mutable() && ui.is_enabled();
        // The text stops short of the ×, rather than running under it.
        let right = if clearable { 6 + SIDE as i8 + 2 } else { 6 };
        let mut edit = egui::TextEdit::singleline(&mut *self.text).margin(egui::Margin { left: 6, right, top: 2, bottom: 2 }).min_size(vec2(0.0, crate::theme::CONTROL_HEIGHT)).vertical_align(egui::Align::Center).password(self.password);
        if let Some(h) = self.hint {
            edit = edit.hint_text(h);
        }
        if let Some(w) = self.width {
            edit = edit.desired_width(w);
        }
        if let Some(f) = self.font {
            edit = edit.font(f);
        }
        if let Some(id) = self.id {
            edit = edit.id(id);
        }
        if let Some(salt) = self.id_salt {
            edit = edit.id_salt(salt);
        }
        if let Some(c) = self.color {
            edit = edit.text_color(c);
        }
        let mut resp = ui.add(edit);
        if !clearable {
            return resp;
        }
        let rect = Rect::from_center_size(pos2(resp.rect.right() - 4.0 - SIDE / 2.0, resp.rect.center().y), Vec2::splat(SIDE));
        let x = ui.interact(rect, resp.id.with("clear"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        x.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, CLEAR_TEXT));
        let visuals = ui.visuals();
        let (fill, color) = if x.hovered() { (visuals.widgets.hovered.bg_fill, visuals.strong_text_color()) } else { (Color32::TRANSPARENT, visuals.weak_text_color()) };
        ui.painter().circle_filled(rect.center(), SIDE / 2.0, fill);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, egui_phosphor::regular::X, prop(11.5), color);
        if x.on_hover_text("Clear").clicked() {
            self.text.clear();
            resp.mark_changed();
            // Typing goes on in the box.
            ui.memory_mut(|m| m.request_focus(resp.id));
        }
        resp
    }
}

/// A button for a row of small actions. egui's own small button drops the usual height, so
/// it stood shorter than the buttons, menus and boxes beside it.
/// A button that starts `op`. While that op runs, or waits its turn, it's off and says so: a
/// spinner, `label…` or git's percentage, and a bar along its foot; or "queued". So a second
/// press can't start it again. Hovering it says what git last said.
pub fn op_button(ui: &mut Ui, now: &crate::data::OpsNow, label: &str, op: &gitem_core::ops::GitOp) -> Response {
    use crate::data::Busy;
    let Some(busy) = now.state_of(op) else {
        return small_button(ui, label);
    };
    let (text, percent) = match &busy {
        Busy::Running(Some(p)) => (format!("{label} {}%", p.percent), Some(p.percent)),
        Busy::Running(None) => (format!("{label}…"), None),
        Busy::Queued => (format!("{label} (queued)"), None),
    };
    let running = matches!(busy, Busy::Running(_));
    // Room at the start for the spinner.
    let shown = if running { format!("     {text}") } else { text.clone() };
    let why = match (&busy, now.line()) {
        (Busy::Queued, _) => format!("{label} waits for what's running ahead of it."),
        (_, Some(l)) if running => format!("{label}: {l}"),
        _ => format!("{label}…"),
    };
    let resp = ui.add_enabled(false, egui::Button::new(shown).min_size(vec2(0.0, ui.spacing().interact_size.y))).on_disabled_hover_text(why);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, false, &text));
    let tint = ui.visuals().selection.bg_fill;
    if running {
        let side = 12.0;
        let at = Rect::from_center_size(pos2(resp.rect.left() + ui.spacing().button_padding.x + side / 2.0, resp.rect.center().y), vec2(side, side));
        egui::Spinner::new().size(side).paint_at(ui, at);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    if let Some(pct) = percent {
        let r = resp.rect.shrink2(vec2(3.0, 0.0));
        ui.painter().rect_filled(Rect::from_min_max(pos2(r.left(), r.bottom() - 3.0), pos2(r.left() + r.width() * f32::from(pct) / 100.0, r.bottom() - 1.0)), 1.0, tint);
    }
    resp
}

pub fn small_button(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Button::new(text).min_size(vec2(0.0, ui.spacing().interact_size.y)))
}

pub fn text_area(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::multiline(text).margin(INPUT_MARGIN)
}

/// Asks git for everything: the largest count it accepts.
pub const LOAD_ALL: usize = i32::MAX as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMore {
    More,
    All,
}

impl LoadMore {
    /// The new limit, from the current one.
    pub fn apply(self, limit: usize, step: usize) -> usize {
        match self {
            LoadMore::More => limit.saturating_add(step),
            LoadMore::All => LOAD_ALL,
        }
    }
}

/// "1 file", "2 files".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// 2000 as "2,000".
pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A pinned bar for a list that stopped at a limit. Call it before drawing the list, so
/// the list takes the space above it.
pub fn load_more_bar(ui: &mut Ui, id: &str, shown: usize, noun: &str, step: usize, busy: bool, p: &Palette) -> Option<LoadMore> {
    let mut choice = None;
    let frame = egui::Frame::new().fill(p.panel2).inner_margin(egui::Margin::symmetric(10, 5));
    egui::Panel::bottom(egui::Id::new(id)).frame(frame).resizable(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Showing the latest {} {noun}", grouped(shown))).color(p.dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!busy, egui::Button::new("Load all")).on_disabled_hover_text("Still loading.").on_hover_text("Can take a while in a very long history").clicked() {
                    choice = Some(LoadMore::All);
                }
                if ui.add_enabled(!busy, egui::Button::new(format!("Load {} more", grouped(step)))).on_disabled_hover_text("Still loading.").clicked() {
                    choice = Some(LoadMore::More);
                }
                if busy {
                    ui.add(spinner(14.0));
                }
            });
        });
    });
    choice
}

/// A row at the end of a sidebar list. `left` is how many remain, when that's known.
pub fn load_more_row(ui: &mut Ui, step: usize, left: Option<usize>, busy: bool) -> bool {
    let count = left.map_or(step, |l| l.min(step));
    let label = match left {
        Some(l) if l > step => format!("Load {} more of {}", grouped(count), grouped(l)),
        _ => format!("Load {} more", grouped(count)),
    };
    let mut clicked = false;
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        clicked = ui.add_enabled(!busy, egui::Button::new(RichText::new(label).size(11.5))).on_disabled_hover_text("Still loading.").clicked();
        if busy {
            ui.add(spinner(12.0));
        }
    });
    ui.add_space(4.0);
    clicked
}

pub fn section(ui: &mut Ui, title: &str, p: &Palette) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(p.dim).strong());
    ui.add_space(2.0);
}

pub fn status_color(s: FileStatus, p: &Palette) -> Color32 {
    match s {
        FileStatus::Added | FileStatus::Untracked => p.add_fg,
        FileStatus::Deleted => p.del_fg,
        FileStatus::Modified | FileStatus::TypeChanged => p.series_text(3),
        FileStatus::Renamed | FileStatus::Copied => p.series_text(6),
        FileStatus::Unmerged => p.warn,
        _ => p.dim,
    }
}

/// `dir/name` shown with the file name prominent and the directory muted.
pub fn paint_path(ui: &Ui, rect: Rect, path: &str, p: &Palette) {
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let mut job = LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(rect.width().max(1.0)), ..Default::default() };
    job.append(name, 0.0, TextFormat { font_id: prop(13.0), color: p.text, ..Default::default() });
    if !dir.is_empty() {
        // About what fits after the name, at this font's average width.
        let room = (rect.width() - name.chars().count() as f32 * 6.5 - 8.0) / 5.5;
        job.append(&format!("  {}", middle_elided(dir, room.max(0.0) as usize)), 0.0, TextFormat { font_id: prop(11.0), color: p.faint, ..Default::default() });
    }
    let g = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, p.text);
}

/// Row showing a changed file. Returns its response for click and context menu.
pub fn file_row(ui: &mut Ui, change: &FileChange, selected: bool, p: &Palette) -> Response {
    let (rect, resp) = row(ui, 22.0, selected, p);
    file_row_body(ui, rect, resp, change, p)
}

/// The same in a list that only draws what's on screen. See [`keyed_row`]. `list` tells it from
/// another list on screen that could show the same file.
pub fn keyed_file_row(ui: &mut Ui, change: &FileChange, selected: bool, p: &Palette, list: &str) -> Response {
    let (rect, resp) = keyed_row(ui, 22.0, selected, p, (list, change.path.as_str()));
    file_row_body(ui, rect, resp, change, p)
}

fn file_row_body(ui: &mut Ui, rect: Rect, resp: Response, change: &FileChange, p: &Palette) -> Response {
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, change.path.as_str()));
    let letter = change.status.letter().to_string();
    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::CENTER_CENTER, letter, mono(12.0), status_color(change.status, p));
    let mut right = rect.right() - 6.0;
    if let (Some(a), Some(d)) = (change.additions, change.deletions) {
        let t = format!("-{d}");
        let g = ui.painter().layout_no_wrap(t, mono(11.0), p.del_fg);
        right -= g.size().x;
        ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.del_fg);
        let t = format!("+{a}");
        let g = ui.painter().layout_no_wrap(t, mono(11.0), p.add_fg);
        right -= g.size().x + 5.0;
        ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, p.add_fg);
        right -= 8.0;
    } else if change.additions.is_none() && change.deletions.is_none() && change.status != FileStatus::Unmodified {
        // Stats are absent for binary files and while loading; nothing to draw.
    }
    let tr = Rect::from_min_max(pos2(rect.left() + 20.0, rect.top()), pos2(right, rect.bottom()));
    paint_path(ui, tr, &change.path, p);
    resp
}

/// A small action at the right end of a row, there while the pointer is over the row.
/// The row's click doesn't fire with it, since this is on top of the row.
pub fn row_button(ui: &mut Ui, row: &Response, glyph: &str, name: &str, selected: bool, p: &Palette) -> bool {
    row_buttons(ui, row, &[(glyph, name)], selected, p) == Some(0)
}

/// Icon buttons at a row's right end, there only while the pointer is over the row: the
/// first of `buttons` is rightmost, where a single one would be. Each is `(glyph, name)`,
/// the name its tooltip and what a screen reader says. Which was clicked, if one was.
pub fn row_buttons(ui: &mut Ui, row: &Response, buttons: &[(&str, &str)], selected: bool, p: &Palette) -> Option<usize> {
    if buttons.is_empty() || !ui.rect_contains_pointer(row.rect) {
        return None;
    }
    const W: f32 = 22.0;
    const STEP: f32 = 24.0;
    let h = row.rect.height() - 2.0;
    let right = row.rect.right() - 3.0;
    // One opaque strip under them all, as the file name may run underneath.
    let strip = Rect::from_min_max(pos2(right - W - STEP * (buttons.len() - 1) as f32 - 2.0, row.rect.center().y - h / 2.0), pos2(right, row.rect.center().y + h / 2.0));
    ui.painter().rect_filled(strip, 3.0, if selected { p.selection } else { p.hover });
    let mut clicked = None;
    for (i, (glyph, name)) in buttons.iter().enumerate() {
        let rect = Rect::from_center_size(pos2(right - W / 2.0 - STEP * i as f32, row.rect.center().y), vec2(W, h));
        let resp = ui.interact(rect, row.id.with(("row-button", i)), Sense::click()).on_hover_text(*name);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *name));
        if resp.hovered() {
            ui.painter().rect(rect, 3.0, p.button, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        }
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, *glyph, prop(13.0), if resp.hovered() { p.text } else { p.dim });
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Rows of one height inside a longer scrolling list, where `show_rows` can't help because
/// other things scroll with them. Only those on screen are drawn. Needs no item spacing.
pub fn visible_rows(ui: &mut Ui, row_h: f32, n: usize, draw: impl FnOnce(&mut Ui, std::ops::Range<usize>)) {
    let top = ui.cursor().top();
    let clip = ui.clip_rect();
    let first = (((clip.top() - top) / row_h).floor().max(0.0) as usize).min(n);
    let last = (((clip.bottom() - top) / row_h).ceil().max(0.0) as usize).clamp(first, n);
    if first > 0 {
        ui.add_space(first as f32 * row_h);
    }
    draw(ui, first..last);
    if last < n {
        ui.add_space((n - last) as f32 * row_h);
    }
}

/// Width of one level of a tree list.
pub const TREE_INDENT: f32 = 14.0;

/// Guides that show which folder a row belongs to.
pub fn tree_guides(ui: &Ui, rect: Rect, depth: usize, p: &Palette) {
    for level in 0..depth {
        ui.painter().vline(rect.left() + 10.0 + level as f32 * TREE_INDENT, rect.y_range(), egui::Stroke::new(1.0, p.border));
    }
}

/// A changed file in a tree list: indented, with its name alone, since the folders above
/// it say where it is. It keeps the full path as its accessible label.
pub fn tree_file_row(ui: &mut Ui, change: &FileChange, name: &str, depth: usize, selected: bool, p: &Palette) -> Response {
    let (rect, resp) = row(ui, 22.0, selected, p);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, change.path.as_str()));
    tree_guides(ui, rect, depth, p);
    let x = rect.left() + depth as f32 * TREE_INDENT;
    ui.painter().text(pos2(x + 10.0, rect.center().y), Align2::CENTER_CENTER, change.status.letter().to_string(), mono(12.0), status_color(change.status, p));
    paint_path(ui, Rect::from_min_max(pos2(x + 20.0, rect.top()), pos2(rect.right() - 6.0, rect.bottom())), name, p);
    resp
}

/// The most of one line that is drawn. All of a line's text goes into the frame, however
/// little of it shows, at 80 bytes of vertex data per character. egui-wgpu doubles its
/// buffer as soon as a frame grows, and the limit is 256 MiB, so a frame over 128 MiB
/// can crash the app. This keeps a 4K screen of long lines on both sides of a diff,
/// about 230 of them, under that.
pub const MAX_SHOWN_LINE_BYTES: usize = 5_000;

/// Characters a line takes on screen, for sizing the column it's in.
pub fn shown_chars(text: &str) -> usize {
    // A tab takes the room of four characters, which a plain count would cut off at the end.
    let width = |s: &str| s.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
    if text.len() > MAX_SHOWN_LINE_BYTES {
        width(util::prefix(text, MAX_SHOWN_LINE_BYTES)) + truncation_marker(text.len()).chars().count()
    } else {
        width(text)
    }
}

#[cfg(test)]
#[test]
fn a_tab_counts_as_the_room_it_takes() {
    assert_eq!(shown_chars("ab"), 2);
    assert_eq!(shown_chars("\t\tx"), 9);
    assert_eq!(shown_chars("\u{e9}"), 1);
}

fn truncation_marker(len: usize) -> String {
    format!("  \u{2026} line truncated: showing {} of {} bytes", grouped(MAX_SHOWN_LINE_BYTES), grouped(len))
}

/// Build a line of code with syntax colours and intra-line change backgrounds. Spans and
/// word ranges must be in order and not overlap, as `syntax` and `word_diff` make them.
/// This runs every frame, so it walks them once. It only affects what's drawn: saving,
/// staging, copying and Find all use the whole line.
pub fn code_job(text: &str, size: f32, base: Color32, spans: Option<&Spans>, words: Option<&[(usize, usize)]>, word_bg: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let font = mono(size);
    if text.is_empty() {
        job.append(" ", 0.0, TextFormat { font_id: font, color: base, ..Default::default() });
        return job;
    }
    let shown = util::prefix(text, MAX_SHOWN_LINE_BYTES);
    let spans: &[(usize, usize, Color32)] = spans.map_or(&[], |s| s.as_slice());
    let words = words.unwrap_or(&[]);
    // Merge all boundaries within the part that's drawn.
    let mut cuts: Vec<usize> = vec![0, shown.len()];
    for (a, b, _) in spans.iter().take_while(|s| s.0 < shown.len()) {
        cuts.extend([*a, (*b).min(shown.len())]);
    }
    for (a, b) in words.iter().take_while(|w| w.0 < shown.len()) {
        cuts.extend([*a, (*b).min(shown.len())]);
    }
    cuts.retain(|c| shown.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    let (mut span, mut word) = (0, 0);
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        while spans.get(span).is_some_and(|s| s.1 <= a) {
            span += 1;
        }
        while words.get(word).is_some_and(|w| w.1 <= a) {
            word += 1;
        }
        let color = spans.get(span).filter(|s| s.0 <= a && b <= s.1).map_or(base, |s| s.2);
        let background = if words.get(word).is_some_and(|w| w.0 <= a && b <= w.1) { word_bg } else { Color32::TRANSPARENT };
        job.append(&shown[a..b], 0.0, TextFormat { font_id: font.clone(), color, background, ..Default::default() });
    }
    if shown.len() < text.len() {
        // A warning colour would mean a theme lookup here. Dimmed text reads as a note.
        job.append(&truncation_marker(text.len()), 0.0, TextFormat { font_id: font, color: base.gamma_multiply(0.6), italics: true, ..Default::default() });
    }
    job
}

/// The main window, which file pickers belong to, so they can't end up behind it.
#[cfg(windows)]
static DIALOG_PARENT: OnceLock<isize> = OnceLock::new();

#[cfg(windows)]
pub fn set_dialog_parent(frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(h) = frame.window_handle()
        && let RawWindowHandle::Win32(w) = h.as_raw()
    {
        let _ = DIALOG_PARENT.set(w.hwnd.get());
    }
}

/// Makes a stored window handle something `rfd` can take.
#[cfg(windows)]
struct DialogParent(std::num::NonZeroIsize);

#[cfg(windows)]
impl raw_window_handle::HasWindowHandle for DialogParent {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(self.0));
        // The main window lasts as long as the process, and only `rfd` reads this, at once.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(windows)]
impl raw_window_handle::HasDisplayHandle for DialogParent {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}

/// Runs a file picker on its own thread, so the window keeps drawing and its git keeps
/// reporting. The choice lands in `task`, and `None` means it was cancelled.
pub fn pick(task: &mut crate::task::Task<Option<std::path::PathBuf>>, ctx: &egui::Context, dialog: rfd::FileDialog, folder: bool) {
    if task.busy() {
        return;
    }
    #[cfg(windows)]
    let dialog = match DIALOG_PARENT.get().and_then(|h| std::num::NonZeroIsize::new(*h)) {
        Some(h) => dialog.set_parent(&DialogParent(h)),
        None => dialog,
    };
    task.spawn(ctx, move || -> Result<Option<std::path::PathBuf>, String> { Ok(if folder { dialog.pick_folder() } else { dialog.pick_file() }) });
}

/// As [`pick`], for a file that's yet to be made.
pub fn pick_new_file(task: &mut crate::task::Task<Option<std::path::PathBuf>>, ctx: &egui::Context, dialog: rfd::FileDialog) {
    if task.busy() {
        return;
    }
    #[cfg(windows)]
    let dialog = match DIALOG_PARENT.get().and_then(|h| std::num::NonZeroIsize::new(*h)) {
        Some(h) => dialog.set_parent(&DialogParent(h)),
        None => dialog,
    };
    task.spawn(ctx, move || -> Result<Option<std::path::PathBuf>, String> { Ok(dialog.save_file()) });
}

pub fn time_label(ts: i64, relative: bool) -> String {
    if relative { util::relative_time(ts, util::now_ts()) } else { util::format_date(ts) }
}

/// Small commit card shown in tooltips.
pub fn commit_card(ui: &mut Ui, sha: &str, author: &str, email: &str, time: i64, subject: &str, p: &Palette, gravatar: bool) {
    ui.set_max_width(420.0);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        paint_avatar(ui, r.center(), 11.0, author, email, p, gravatar);
        ui.vertical(|ui| {
            ui.label(RichText::new(author).strong());
            ui.label(RichText::new(format!("{}  ·  {}", util::relative_time(time, util::now_ts()), util::format_date(time))).small().color(p.dim));
        });
    });
    ui.add_space(4.0);
    ui.label(subject);
    ui.add_space(2.0);
    ui.label(RichText::new(gitem_core::util::prefix(sha, 10)).monospace().color(p.dim));
}

pub fn empty_state(ui: &mut Ui, title: &str, hint: &str, p: &Palette) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(15.0).color(p.dim));
        ui.add_space(4.0);
        ui.label(RichText::new(hint).color(p.faint));
    });
}

pub fn error_box(ui: &mut Ui, msg: &str, p: &Palette) {
    let frame = egui::Frame::new().fill(p.danger.gamma_multiply(0.12)).stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.6))).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
        ui.label(RichText::new(msg).color(p.text));
    });
    // Says what it is to a screen reader, and lets a test tell a view that failed
    // to load from one that loaded.
    frame.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{ERROR_LABEL}{msg}")));
}

/// How the accessible label of every `error_box` starts.
pub const ERROR_LABEL: &str = "Error: ";

/// Frames per second a spinner asks for. egui's own asks for all the display can do, which
/// costs a tenth of a core at 120 Hz for as long as a blame takes.
const SPINNER_FPS: u64 = 15;

/// A spinner that doesn't keep the whole window drawing at full speed for what can be minutes.
pub fn spinner(size: f32) -> impl egui::Widget {
    move |ui: &mut Ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        if ui.is_rect_visible(rect) {
            let color = ui.visuals().strong_text_color();
            let radius = rect.height() / 2.0 - 2.0;
            // Motion turned off in Settings: a still ring says the same.
            if ui.style().animation_time == 0.0 {
                ui.painter().circle_stroke(rect.center(), radius, Stroke::new(1.5, color));
                return resp;
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000 / SPINNER_FPS));
            let time = ui.input(|i| i.time);
            let start = time * std::f64::consts::TAU;
            let end = start + 240f64.to_radians() * ((time / 3.0).sin() * 0.5 + 0.75);
            let points: Vec<egui::Pos2> = (0..=16).map(|i| {
                let angle = start + (end - start) * i as f64 / 16.0;
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * vec2(cos as f32, sin as f32)
            }).collect();
            ui.painter().add(egui::Shape::line(points, Stroke::new(1.5, color)));
        }
        resp
    }
}

pub fn spinner_row(ui: &mut Ui, text: &str, p: &Palette) {
    ui.horizontal(|ui| {
        ui.add(spinner(14.0));
        ui.label(RichText::new(text).color(p.dim));
    });
}

/// A small button showing a glyph, with a proper name for anyone who can't see that.
pub fn small_icon_button(ui: &mut Ui, glyph: &str, name: &str, keys: &str) -> Response {
    let r = crate::widgets::small_button(ui, glyph);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    if keys.is_empty() { r.on_hover_text(name) } else { r.on_hover_text(format!("{name} ({keys})")) }
}

/// Compact toolbar toggle. It looks like something to press when it's off, too.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str, tip: &str) -> bool {
    let r = ui.add(egui::Button::new(label).selected(*on)).on_hover_text(tip);
    if r.clicked() {
        *on = !*on;
        true
    } else {
        false
    }
}

/// Render text with clickable links for autolinks, issue references, and URLs.
pub fn linked_text(ui: &mut Ui, text: &str, links: &[gitem_core::forge::LinkSpan], color: Color32) {
    if links.is_empty() {
        ui.label(RichText::new(text).color(color));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut pos = 0;
        for l in links {
            if l.start > pos {
                ui.label(RichText::new(&text[pos..l.start]).color(color));
            }
            web_link(ui, RichText::new(&text[l.start..l.end]), &l.url);
            pos = l.end;
        }
        if pos < text.len() {
            ui.label(RichText::new(&text[pos..]).color(color));
        }
    });
}

/// Only web links reach the browser opener. Servers choose the URLs shown in Launchpad, and
/// could send a `file:` one. Anything else is shown as plain text, so it's visibly no link.
pub fn is_web_link(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub fn web_link(ui: &mut Ui, text: impl Into<egui::WidgetText>, url: &str) {
    if is_web_link(url) {
        ui.hyperlink_to(text, url);
    } else {
        ui.label(text).on_hover_text("This isn't a web link, so gitem won't open it.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORD_BG: Color32 = Color32::from_rgb(1, 2, 3);

    fn parts(job: &LayoutJob) -> Vec<(String, Color32, Color32)> {
        job.sections.iter().map(|s| (job.text[s.byte_range.start.0..s.byte_range.end.0].to_string(), s.format.color, s.format.background)).collect()
    }

    #[test]
    fn colours_and_word_backgrounds_land_on_the_right_text() {
        let (red, blue) = (Color32::RED, Color32::BLUE);
        let spans: Spans = vec![(0, 3, red), (3, 4, Color32::WHITE), (4, 9, blue)];
        let job = code_job("foo bar baz", 13.0, Color32::GRAY, Some(&spans), Some(&[(1, 2), (5, 7)]), WORD_BG);
        let none = Color32::TRANSPARENT;
        assert_eq!(parts(&job), [
            ("f".to_string(), red, none),
            ("o".to_string(), red, WORD_BG),
            ("o".to_string(), red, none),
            (" ".to_string(), Color32::WHITE, none),
            ("b".to_string(), blue, none),
            ("ar".to_string(), blue, WORD_BG),
            (" b".to_string(), blue, none),
            ("az".to_string(), Color32::GRAY, none),
        ]);
    }

    #[test]
    fn a_range_ending_inside_a_character_loses_no_text() {
        let spans: Spans = vec![(0, 2, Color32::RED), (2, 4, Color32::BLUE)];
        assert_eq!(code_job("a\u{e9}\u{e9}", 13.0, Color32::GRAY, Some(&spans), None, WORD_BG).text, "a\u{e9}\u{e9}");
    }

    #[test]
    fn a_short_line_is_left_alone() {
        let line = "x".repeat(MAX_SHOWN_LINE_BYTES);
        let job = code_job(&line, 13.0, Color32::GRAY, None, None, WORD_BG);
        assert_eq!(job.text, line);
        assert_eq!(shown_chars(&line), MAX_SHOWN_LINE_BYTES);
    }

    #[test]
    fn a_long_line_is_cut_on_a_character_with_a_marker() {
        for unit in ["x", "\u{e9}", "\u{20ac}", "\u{1f600}"] {
            let line = unit.repeat(MAX_SHOWN_LINE_BYTES + 1);
            let job = code_job(&line, 13.0, Color32::GRAY, None, None, WORD_BG);
            let (kept, marker) = job.text.split_once("  \u{2026} ").expect("a marker");
            assert!(kept.len() <= MAX_SHOWN_LINE_BYTES && kept.len() + unit.len() > MAX_SHOWN_LINE_BYTES, "{unit}: {}", kept.len());
            assert!(kept.chars().all(|c| c.to_string() == unit));
            assert!(marker.starts_with("line truncated"), "{marker}");
            assert_eq!(shown_chars(&line), job.text.chars().count());
        }
    }

    #[test]
    fn ranges_past_the_cut_are_dropped_and_one_across_it_is_clipped() {
        let line = "x".repeat(MAX_SHOWN_LINE_BYTES * 2);
        let spans: Spans = vec![(0, 10, Color32::RED), (10, MAX_SHOWN_LINE_BYTES + 5, Color32::BLUE), (MAX_SHOWN_LINE_BYTES + 5, line.len(), Color32::GREEN)];
        let job = code_job(&line, 13.0, Color32::GRAY, Some(&spans), Some(&[(MAX_SHOWN_LINE_BYTES + 1, line.len())]), WORD_BG);
        let got = parts(&job);
        assert_eq!(got.len(), 3, "two colours and the marker");
        assert_eq!((got[0].0.len(), got[0].1), (10, Color32::RED));
        assert_eq!((got[1].0.len(), got[1].1, got[1].2), (MAX_SHOWN_LINE_BYTES - 10, Color32::BLUE, Color32::TRANSPARENT));
        assert!(got[2].0.contains("line truncated"));
    }
}
