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

/// Clip to the rect and paint left-aligned text, vertically centred.
pub fn paint_text(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, text, font, color);
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

pub fn ref_glyph(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Tag => "◆ ",
        RefKind::Remote => "☁ ",
        RefKind::Stash => "▣ ",
        _ => "",
    }
}

/// Paint a ref badge at `pos`, returning its rectangle.
pub fn paint_badge(ui: &Ui, pos: egui::Pos2, h: f32, r: &RefLabel, p: &Palette) -> Rect {
    let color = ref_color(r.kind, r.current, p);
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
    let g = ui.painter().layout_no_wrap(text.to_string(), prop(11.0), color);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(10.0, 4.0), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter().rect(rect, 3.0, color.gamma_multiply(0.14), Stroke::new(1.0, color.gamma_multiply(0.5)), egui::StrokeKind::Inside);
    ui.painter().galley(rect.min + vec2(5.0, 2.0), g, color);
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
    ui.painter().text(center, Align2::CENTER_CENTER, initials(name), prop(radius * 0.95), Color32::WHITE);
}

/// A full-width clickable row with hover and selection fill.
pub fn row(ui: &mut Ui, h: f32, selected: bool, p: &Palette) -> (Rect, Response) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0.0, p.selection);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    (rect, resp)
}

/// Space between a text box's edge and its text. egui has no style for this, so every
/// text box in the app is made here.
const INPUT_MARGIN: egui::Margin = egui::Margin::symmetric(6, 5);

pub fn input(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(INPUT_MARGIN)
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
                if ui.add_enabled(!busy, egui::Button::new("Load all")).on_hover_text("Can take a while in a very long history").clicked() {
                    choice = Some(LoadMore::All);
                }
                if ui.add_enabled(!busy, egui::Button::new(format!("Load {} more", grouped(step)))).clicked() {
                    choice = Some(LoadMore::More);
                }
                if busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
        });
    });
    choice
}

/// A row at the end of a sidebar list. `left` is how many remain, when that's known.
pub fn load_more_row(ui: &mut Ui, step: usize, left: Option<usize>, busy: bool, p: &Palette) -> bool {
    let count = left.map_or(step, |l| l.min(step));
    let label = match left {
        Some(l) if l > step => format!("Load {} more of {}", grouped(count), grouped(l)),
        _ => format!("Load {} more", grouped(count)),
    };
    let mut clicked = false;
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        clicked = ui.add_enabled(!busy, egui::Button::new(RichText::new(label).size(11.5))).clicked();
        if busy {
            ui.add(egui::Spinner::new().size(12.0).color(p.dim));
        }
    });
    ui.add_space(4.0);
    clicked
}

pub fn section(ui: &mut Ui, title: &str, p: &Palette) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(p.faint).strong());
    ui.add_space(2.0);
}

pub fn status_color(s: FileStatus, p: &Palette) -> Color32 {
    match s {
        FileStatus::Added | FileStatus::Untracked => p.add_fg,
        FileStatus::Deleted => p.del_fg,
        FileStatus::Modified | FileStatus::TypeChanged => p.series[3],
        FileStatus::Renamed | FileStatus::Copied => p.series[6],
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
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    job.append(name, 0.0, TextFormat { font_id: prop(13.0), color: p.text, ..Default::default() });
    if !dir.is_empty() {
        job.append(&format!("  {dir}"), 0.0, TextFormat { font_id: prop(11.0), color: p.faint, ..Default::default() });
    }
    let g = ui.painter().layout_job(job);
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, p.text);
}

/// Row showing a changed file. Returns its response for click and context menu.
pub fn file_row(ui: &mut Ui, change: &FileChange, selected: bool, p: &Palette) -> Response {
    let (rect, resp) = row(ui, 22.0, selected, p);
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

/// Build a line of code with syntax colours and intra-line change backgrounds.
pub fn code_job(text: &str, size: f32, base: Color32, spans: Option<&Spans>, words: Option<&[(usize, usize)]>, word_bg: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let font = mono(size);
    if text.is_empty() {
        job.append(" ", 0.0, TextFormat { font_id: font, color: base, ..Default::default() });
        return job;
    }
    // Merge all span boundaries.
    let mut cuts: Vec<usize> = vec![0, text.len()];
    if let Some(s) = spans {
        for (a, b, _) in s {
            cuts.push((*a).min(text.len()));
            cuts.push((*b).min(text.len()));
        }
    }
    if let Some(w) = words {
        for (a, b) in w {
            cuts.push((*a).min(text.len()));
            cuts.push((*b).min(text.len()));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        let color = spans.and_then(|s| s.iter().find(|(x, y, _)| *x <= a && b <= *y)).map(|x| x.2).unwrap_or(base);
        let bg = if words.is_some_and(|w| w.iter().any(|(x, y)| *x <= a && b <= *y)) { word_bg } else { Color32::TRANSPARENT };
        job.append(&text[a..b], 0.0, TextFormat { font_id: font.clone(), color, background: bg, ..Default::default() });
    }
    job
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
    egui::Frame::new().fill(p.danger.gamma_multiply(0.12)).stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.6))).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
        ui.label(RichText::new(msg).color(p.text));
    });
}

pub fn spinner_row(ui: &mut Ui, text: &str, p: &Palette) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0));
        ui.label(RichText::new(text).color(p.dim));
    });
}

/// Compact toolbar toggle.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str, tip: &str) -> bool {
    let r = ui.selectable_label(*on, label).on_hover_text(tip);
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
            ui.hyperlink_to(&text[l.start..l.end], &l.url);
            pos = l.end;
        }
        if pos < text.len() {
            ui.label(RichText::new(&text[pos..]).color(color));
        }
    });
}
