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

/// A small action at the right end of a row, there while the pointer is over the row.
/// The row's click doesn't fire with it, since this is on top of the row.
pub fn row_button(ui: &mut Ui, row: &Response, glyph: &str, name: &str, selected: bool, p: &Palette) -> bool {
    if !ui.rect_contains_pointer(row.rect) {
        return false;
    }
    let rect = Rect::from_center_size(pos2(row.rect.right() - 14.0, row.rect.center().y), vec2(22.0, row.rect.height() - 2.0));
    let resp = ui.interact(rect, row.id.with("row-button"), Sense::click()).on_hover_text(name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    // Opaque, as the file name may run underneath.
    ui.painter().rect_filled(rect, 3.0, if selected { p.selection } else { p.hover });
    if resp.hovered() {
        ui.painter().rect(rect, 3.0, p.button, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, prop(13.0), p.text);
    resp.clicked()
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
    if text.len() > MAX_SHOWN_LINE_BYTES {
        util::prefix(text, MAX_SHOWN_LINE_BYTES).chars().count() + truncation_marker(text.len()).chars().count()
    } else {
        text.chars().count()
    }
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
