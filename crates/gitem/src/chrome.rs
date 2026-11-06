//! The window's own frame, drawn by the app so it looks the same on every platform:
//! logo, titlebar, window controls, resize edges, and the icon rail of views.
use egui::viewport::ResizeDirection;
use egui::{Align2, Color32, CornerRadius, CursorIcon, IconData, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, Ui, ViewportCommand, WidgetInfo, WidgetType, pos2, vec2};
pub use egui_phosphor::regular as icon;

use crate::logo;
use crate::theme::Palette;
use crate::views::Tab;
use crate::widgets::prop;

pub const TITLE_BAR_HEIGHT: f32 = 34.0;
pub const RAIL_WIDTH: f32 = 44.0;
const RAIL_ITEM: f32 = 34.0;
const CONTROL_WIDTH: f32 = 46.0;
const RESIZE_EDGE: f32 = 4.0;
const RESIZE_CORNER: f32 = 10.0;
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xc4, 0x2b, 0x1c);

// ---- logo --------------------------------------------------------------------

pub fn paint_logo(painter: &egui::Painter, rect: Rect) {
    let at = |(x, y): logo::Point| rect.min + vec2(x * rect.width(), y * rect.height());
    for (points, (r, g, b)) in logo::FACETS {
        let color = Color32::from_rgb(r, g, b);
        let pts: Vec<Pos2> = points.iter().map(|p| at(*p)).collect();
        // The hairline stroke closes the seams between facets.
        painter.add(Shape::convex_polygon(pts, color, Stroke::new(0.4, color)));
    }
    let scale = rect.width();
    for (a, b) in logo::EDGES {
        painter.line_segment([at(a), at(b)], Stroke::new(logo::EDGE_WIDTH * scale, Color32::WHITE));
    }
    for n in logo::NODES {
        painter.circle_filled(at(n), logo::NODE_RADIUS * scale, Color32::WHITE);
    }
}

pub fn logo_icon(size: u32) -> IconData {
    IconData { rgba: logo::rgba(size), width: size, height: size }
}

/// An undecorated window with the logo, as every gitem window is.
pub fn viewport(title: &str, size: [f32; 2]) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default().with_title(title).with_inner_size(size).with_decorations(false).with_icon(std::sync::Arc::new(logo_icon(64)))
}

// ---- buttons -----------------------------------------------------------------

fn tooltip(resp: Response, name: &str, tip: &str, p: &Palette) -> Response {
    let (dim, has_tip) = (p.dim, !tip.is_empty());
    resp.on_hover_ui(|ui| {
        ui.set_max_width(260.0);
        ui.label(RichText::new(name).strong());
        if has_tip {
            ui.label(RichText::new(tip).color(dim));
        }
    })
}

/// A square button showing only an icon. `name` is its tooltip and accessible name.
pub fn icon_button(ui: &mut Ui, glyph: &str, name: &str, tip: &str, on: bool, p: &Palette) -> Response {
    let side = ui.spacing().interact_size.y + 4.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    if ui.is_rect_visible(rect) {
        if on {
            ui.painter().rect_filled(rect, 4.0, p.selection);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 4.0, p.hover);
        }
        let color = if on || resp.hovered() { p.text } else { p.dim };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, prop(16.0), color);
    }
    tooltip(resp, name, tip, p)
}

/// A toolbar button showing only an icon, on the same raised fill as text buttons.
pub fn tool_button(ui: &mut Ui, glyph: &str, tint: Color32, name: &str, tip: &str, on: bool, p: &Palette) -> Response {
    let side = ui.spacing().interact_size.y + 4.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    if ui.is_rect_visible(rect) {
        let (fill, border) = match (on, resp.hovered()) {
            (true, _) => (p.selection, p.accent),
            (false, true) => (p.button_hover, p.accent),
            (false, false) => (p.button, p.button_border),
        };
        ui.painter().rect(rect, 4.0, fill, Stroke::new(1.0, border), egui::StrokeKind::Inside);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, prop(16.0), if on { p.text } else { tint });
    }
    tooltip(resp, name, tip, p)
}

/// A button with a coloured icon and text, named by the text alone. `primary` fills
/// it with the accent, for when there's work waiting behind it.
pub fn icon_text_button(ui: &mut Ui, glyph: &str, tint: Color32, text: &str, tip: &str, primary: bool, p: &Palette) -> Response {
    let (icon, label) = if primary {
        (RichText::new(glyph).color(p.on_accent), RichText::new(text).color(p.on_accent).strong())
    } else {
        (RichText::new(glyph).color(tint), RichText::new(text))
    };
    let mut button = egui::Button::new((icon, label));
    if primary {
        button = button.fill(p.accent).stroke(Stroke::new(1.0, p.accent));
    }
    let resp = ui.add(button);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    if tip.is_empty() { resp } else { tooltip(resp, text, tip, p) }
}

// ---- titlebar ----------------------------------------------------------------

/// Everything in the bar that isn't a widget moves the window. Call this first, so
/// widgets added afterwards sit on top of it.
pub fn drag_region(ui: &mut Ui) {
    let resp = ui.interact(ui.max_rect(), ui.id().with("window-drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// One open repository in the titlebar. Returns whether its close button was clicked.
pub fn repo_tab(ui: &mut Ui, name: &str, dirty: bool, selected: bool, path: &str, p: &Palette) -> (Response, bool) {
    const TOP_GAP: f32 = 5.0;
    const CLOSE: f32 = 16.0;
    let color = if selected { p.text } else { p.dim };
    let galley = ui.painter().layout_no_wrap(name.to_string(), prop(12.5), color);
    let width = 12.0 + galley.size().x + if dirty { 11.0 } else { 0.0 } + 4.0 + CLOSE + 6.0;
    let (outer, resp) = ui.allocate_exact_size(vec2(width, TITLE_BAR_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    let rect = Rect::from_min_max(pos2(outer.left(), outer.top() + TOP_GAP), outer.max);
    let close_rect = Rect::from_center_size(pos2(rect.right() - 6.0 - CLOSE / 2.0, rect.center().y), vec2(CLOSE, CLOSE));
    let close = ui.interact(close_rect, resp.id.with("close"), Sense::click());
    close.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Close {name}")));
    let hovered = ui.rect_contains_pointer(rect);
    let corners = CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
    if selected {
        ui.painter().rect_filled(rect, corners, p.panel);
        let line = Rect::from_min_size(rect.left_top() + vec2(6.0, 0.0), vec2(rect.width() - 12.0, 2.0));
        ui.painter().rect_filled(line, 1.0, p.accent);
    } else if hovered {
        ui.painter().rect_filled(rect, corners, p.hover);
    }
    let mut x = rect.left() + 12.0;
    ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley.clone(), color);
    x += galley.size().x;
    if dirty {
        ui.painter().circle_filled(pos2(x + 7.0, rect.center().y), 2.5, p.warn);
    }
    if hovered || selected {
        if close.hovered() {
            ui.painter().rect_filled(close_rect, 3.0, p.panel2);
        }
        ui.painter().text(close_rect.center(), Align2::CENTER_CENTER, icon::X, prop(11.0), if close.hovered() { p.text } else { p.faint });
    }
    let clicked = close.clicked();
    (resp.on_hover_text(path), clicked)
}

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn control(ui: &mut Ui, kind: Control, name: &str, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(CONTROL_WIDTH, TITLE_BAR_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    let closing = matches!(kind, Control::Close) && resp.hovered();
    if closing {
        ui.painter().rect_filled(rect, 0.0, CLOSE_HOVER);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    let stroke = Stroke::new(1.0, if closing { Color32::WHITE } else { p.dim });
    let c = rect.center();
    const R: f32 = 5.0;
    let square = Rect::from_center_size(c, vec2(2.0 * R, 2.0 * R));
    let painter = ui.painter();
    match kind {
        Control::Minimize => {
            painter.line_segment([c + vec2(-R, 0.0), c + vec2(R, 0.0)], stroke);
        }
        Control::Maximize => {
            painter.rect_stroke(square, 1.0, stroke, egui::StrokeKind::Inside);
        }
        Control::Restore => {
            let front = square.translate(vec2(-1.5, 1.5)).shrink(0.5);
            let back = square.translate(vec2(1.5, -1.5)).shrink(0.5);
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Inside);
        }
        Control::Close => {
            painter.line_segment([c + vec2(-R, -R), c + vec2(R, R)], stroke);
            painter.line_segment([c + vec2(-R, R), c + vec2(R, -R)], stroke);
        }
    }
    resp
}

/// Minimize, maximize and close, for a right-to-left layout.
pub fn window_controls(ui: &mut Ui, p: &Palette) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    ui.spacing_mut().item_spacing.x = 0.0;
    if control(ui, Control::Close, "Close window", p).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let kind = if maximized { Control::Restore } else { Control::Maximize };
    if control(ui, kind, if maximized { "Restore window" } else { "Maximize window" }, p).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if control(ui, Control::Minimize, "Minimize window", p).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

/// The titlebar of a helper window: logo, what it's for, and window controls.
/// Prompts pass `compact`, which leaves only the close button.
pub fn helper_title_bar(ui: &mut Ui, title: &str, compact: bool, p: &Palette) {
    drag_region(ui);
    ui.horizontal(|ui| {
        ui.add_space(9.0);
        let (rect, _) = ui.allocate_exact_size(vec2(20.0, TITLE_BAR_HEIGHT), Sense::hover());
        paint_logo(ui.painter(), Rect::from_center_size(rect.center(), vec2(18.0, 18.0)));
        ui.add_space(4.0);
        // Not a widget that takes clicks, so the window can be dragged by its title.
        let (rect, _) = ui.allocate_exact_size(vec2(0.0, TITLE_BAR_HEIGHT), Sense::hover());
        ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, title, prop(12.5), p.dim);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if compact {
                ui.spacing_mut().item_spacing.x = 0.0;
                if control(ui, Control::Close, "Close window", p).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            } else {
                window_controls(ui, p);
            }
        });
    });
}

/// The side or corner of a window `size` that `pos` is resizing from, if any.
pub fn resize_direction(pos: Pos2, size: egui::Vec2) -> Option<ResizeDirection> {
    let near = |d: f32, range: f32| d >= 0.0 && d <= range;
    let (l, r, t, b) = (pos.x, size.x - pos.x, pos.y, size.y - pos.y);
    let (cl, cr, ct, cb) = (near(l, RESIZE_CORNER), near(r, RESIZE_CORNER), near(t, RESIZE_CORNER), near(b, RESIZE_CORNER));
    let (el, er, et, eb) = (near(l, RESIZE_EDGE), near(r, RESIZE_EDGE), near(t, RESIZE_EDGE), near(b, RESIZE_EDGE));
    Some(match () {
        () if (et && cl) || (el && ct) => ResizeDirection::NorthWest,
        () if (et && cr) || (er && ct) => ResizeDirection::NorthEast,
        () if (eb && cl) || (el && cb) => ResizeDirection::SouthWest,
        () if (eb && cr) || (er && cb) => ResizeDirection::SouthEast,
        () if et => ResizeDirection::North,
        () if eb => ResizeDirection::South,
        () if el => ResizeDirection::West,
        () if er => ResizeDirection::East,
        () => return None,
    })
}

/// Resizing from the window's edges, and its outline. Call this after everything
/// else has been drawn, so the cursor it sets wins.
pub fn window_edges(ctx: &egui::Context, p: &Palette) {
    let (maximized, fullscreen) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
    if maximized || fullscreen {
        return;
    }
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("window-outline"));
    ctx.layer_painter(layer).rect_stroke(screen, 0.0, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);

    let (pos, pressed, busy) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.primary_pressed(), i.pointer.any_down() && !i.pointer.primary_pressed()));
    if busy {
        return;
    }
    let Some(dir) = pos.and_then(|at| resize_direction(at - screen.min.to_vec2(), screen.size())) else { return };
    ctx.set_cursor_icon(match dir {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    });
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

/// Gives the undecorated window the rounded corners and shadow Windows gives others.
/// Older versions of Windows ignore the corner request, which is harmless.
#[cfg(windows)]
pub fn native_window_style(window: &impl raw_window_handle::HasWindowHandle) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
    use windows_sys::Win32::UI::Controls::MARGINS;
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = h.hwnd.get() as *mut core::ffi::c_void;
    let corner = DWMWCP_ROUND;
    // A one-pixel margin is what makes DWM draw a shadow; the app paints over it.
    let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 1 };
    // SAFETY: `hwnd` is this app's live window and both pointers outlive the calls.
    unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, (&raw const corner).cast(), size_of_val(&corner) as u32);
        DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

// ---- the rail ----------------------------------------------------------------

pub struct RailItem {
    pub glyph: &'static str,
    pub name: &'static str,
    pub tip: &'static str,
    pub make: fn() -> Tab,
    pub is: fn(&Tab) -> bool,
}

macro_rules! item {
    ($glyph:ident, $name:literal, $tip:literal, $make:expr, $var:ident) => {
        RailItem { glyph: icon::$glyph, name: $name, tip: $tip, make: $make, is: |t| matches!(t, Tab::$var(_)) }
    };
}

/// Views grouped by what you're doing, most used first. The separators matter:
/// 22 unmarked icons would be hard to scan.
pub fn rail_top() -> [&'static [RailItem]; 4] {
    static WORK: [RailItem; 5] = [
        item!(HOUSE, "Home", "Overview and start work", Tab::home, Home),
        item!(GIT_FORK, "Graph", "Commit graph", Tab::graph, Graph),
        item!(GIT_DIFF, "Changes", "Stage and commit", Tab::changes, Changes),
        item!(MAGNIFYING_GLASS, "Search", "Search and compare", || Tab::Search(Box::default()), Search),
        item!(ROCKET_LAUNCH, "Launchpad", "Pull requests and issues", || Tab::Launchpad(Box::default()), Launchpad),
    ];
    static TEAM: [RailItem; 3] = [
        item!(TREE_STRUCTURE, "Worktrees", "Manage worktrees", || Tab::Worktrees(Box::default()), Worktrees),
        item!(ROBOT, "Agents", "Coding-agent sessions in these worktrees", || Tab::Agents(Box::default()), Agents),
        item!(LIST_CHECKS, "Review", "Review changes with a checklist and notes", || Tab::Review(Box::default()), Review),
    ];
    static HISTORY: [RailItem; 5] = [
        item!(CLOCK_COUNTER_CLOCKWISE, "Reflog", "Recover lost work", || Tab::Reflog(Box::default()), Reflog),
        item!(LIST_NUMBERS, "Rebase", "Interactive rebase", || Tab::rebase(None), Rebase),
        item!(BUG_BEETLE, "Bisect", "Find a regression", || Tab::Bisect(Box::default()), Bisect),
        item!(BANDAIDS, "Patches", "Saved patches", || Tab::Patches(Box::default()), Patches),
        item!(STACK, "Composer", "Split work into commits", || Tab::Composer(Box::default()), Composer),
    ];
    static INSIGHT: [RailItem; 4] = [
        item!(CHART_SCATTER, "Visual", "Visual history of the repository, a folder, or a file", || Tab::visual(""), Visual),
        item!(SQUARES_FOUR, "Treemap", "Files by size or recent activity", || Tab::Treemap(Box::default()), Treemap),
        item!(USERS_THREE, "Contributors", "People and activity", || Tab::Contributors(Box::default()), Contributors),
        item!(HEARTBEAT, "Health", "How well this repository is tuned", || Tab::Health(Box::default()), Health),
    ];
    [&WORK, &TEAM, &HISTORY, &INSIGHT]
}

/// Pinned to the bottom of the rail, like settings in an editor.
pub fn rail_bottom() -> &'static [RailItem] {
    static BOTTOM: [RailItem; 5] = [
        item!(WRENCH, "Tools", "Submodules, hooks, LFS, clean", || Tab::Tools(Box::default()), Tools),
        item!(SLIDERS_HORIZONTAL, "Config", "Git configuration", || Tab::Config(Box::default()), Config),
        item!(KEY, "Credentials", "Tokens and SSH keys", || Tab::Credentials(Box::default()), Credentials),
        item!(TERMINAL_WINDOW, "Console", "Command log", || Tab::Console(Box::default()), Console),
        item!(GEAR_SIX, "Settings", "Preferences", || Tab::Settings(Box::default()), Settings),
    ];
    &BOTTOM
}

/// One hue per rail group, drawn from the palette's fixed order.
fn group_tint(group: usize, p: &Palette) -> Color32 {
    p.series[[0, 2, 3, 6][group % 4]]
}

fn rail_button(ui: &mut Ui, item: &RailItem, tint: Color32, active: bool, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(RAIL_WIDTH, RAIL_ITEM), Sense::click());
    let name = crate::i18n::t(item.name);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    if ui.is_rect_visible(rect) {
        let tile = rect.shrink2(vec2(5.0, 1.0));
        if active {
            ui.painter().rect_filled(tile, 5.0, tint.gamma_multiply(0.22));
            let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 7.0), vec2(2.5, RAIL_ITEM - 14.0));
            ui.painter().rect_filled(bar, 1.0, tint);
        } else if resp.hovered() {
            ui.painter().rect_filled(tile, 5.0, p.hover);
        }
        let color = if active { tint } else if resp.hovered() { p.text } else { tint.gamma_multiply(0.8) };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, item.glyph, prop(20.0), color);
    }
    tooltip(resp, name, item.tip, p)
}

/// The views rail. `is_focused` lights up the icon of the tab in front; a click
/// returns the view to open.
pub fn rail(ui: &mut Ui, is_focused: &dyn Fn(&RailItem) -> bool, p: &Palette) -> Option<Tab> {
    let mut clicked = None;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let bottom = rail_bottom();
    let pinned = bottom.len() as f32 * RAIL_ITEM + 9.0;
    let height = (ui.available_height() - pinned).max(RAIL_ITEM);
    ui.allocate_ui_with_layout(vec2(RAIL_WIDTH, height), egui::Layout::top_down(egui::Align::Min), |ui| {
        egui::ScrollArea::vertical().id_salt("rail").max_height(height).auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.add_space(4.0);
            for (i, group) in rail_top().into_iter().enumerate() {
                if i > 0 {
                    separator(ui, p);
                }
                for item in group {
                    if rail_button(ui, item, group_tint(i, p), is_focused(item), p).clicked() {
                        clicked = Some(item);
                    }
                }
            }
        });
    });
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        for item in bottom.iter().rev() {
            if rail_button(ui, item, p.dim, is_focused(item), p).clicked() {
                clicked = Some(item);
            }
        }
    });
    clicked.map(|i| (i.make)())
}

fn separator(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(RAIL_WIDTH, 9.0), Sense::hover());
    let y = rect.center().y;
    ui.painter().line_segment([pos2(rect.left() + 12.0, y), pos2(rect.right() - 12.0, y)], Stroke::new(1.0, p.border));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_edges_and_corners() {
        let size = vec2(1000.0, 600.0);
        assert!(resize_direction(pos2(500.0, 300.0), size).is_none());
        assert_eq!(resize_direction(pos2(2.0, 300.0), size), Some(ResizeDirection::West));
        assert_eq!(resize_direction(pos2(998.0, 300.0), size), Some(ResizeDirection::East));
        assert_eq!(resize_direction(pos2(500.0, 2.0), size), Some(ResizeDirection::North));
        assert_eq!(resize_direction(pos2(500.0, 598.0), size), Some(ResizeDirection::South));
        assert_eq!(resize_direction(pos2(1.0, 1.0), size), Some(ResizeDirection::NorthWest));
        assert_eq!(resize_direction(pos2(996.0, 598.0), size), Some(ResizeDirection::SouthEast));
        assert_eq!(resize_direction(pos2(8.0, 598.0), size), Some(ResizeDirection::SouthWest));
    }

    #[test]
    fn the_icon_is_opaque_inside_and_clear_outside() {
        let i = logo_icon(32);
        assert_eq!(i.rgba.len(), 32 * 32 * 4);
        let alpha = |x: usize, y: usize| i.rgba[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corner");
        assert_eq!(alpha(31, 31), 0, "corner");
        assert_eq!(alpha(16, 20), 255, "body");
    }

    #[test]
    fn every_view_has_a_distinct_icon() {
        let mut all: Vec<&RailItem> = rail_top().into_iter().flatten().collect();
        all.extend(rail_bottom());
        assert_eq!(all.len(), 22);
        let glyphs: std::collections::HashSet<_> = all.iter().map(|i| i.glyph).collect();
        assert_eq!(glyphs.len(), all.len());
        for i in &all {
            assert!((i.is)(&(i.make)()), "{} doesn't recognise its own tab", i.name);
        }
    }
}
