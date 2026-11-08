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
/// With each view's name beside its icon, for those who turn that on.
pub const RAIL_LABELLED_WIDTH: f32 = 132.0;
const RAIL_ITEM: f32 = 34.0;
const CONTROL_WIDTH: f32 = 46.0;
/// Clear title bar kept for moving the window, however many repositories are open.
pub const TITLE_DRAG_ROOM: f32 = 28.0;
pub const COMMAND_FIELD_WIDTH: f32 = 250.0;
/// A repository's tab doesn't get narrower than this. Past that they go in a list.
const MIN_REPO_TAB: f32 = 96.0;
const RESIZE_EDGE: f32 = 6.0;
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
    let side = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    crate::widgets::focus_ring(ui, &resp, p);
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
    let side = ui.spacing().interact_size.y;
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
        crate::widgets::focus_ring(ui, &resp, p);
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

const REPO_TAB_CLOSE: f32 = 20.0;

/// What a repository's tab takes around its name.
fn repo_tab_chrome(dirty: bool) -> f32 {
    12.0 + if dirty { 11.0 } else { 0.0 } + 4.0 + REPO_TAB_CLOSE + 6.0
}

/// How wide a repository's tab is with its whole name showing.
pub fn repo_tab_width(ui: &Ui, name: &str, dirty: bool) -> f32 {
    ui.painter().layout_no_wrap(name.to_string(), prop(12.5), Color32::WHITE).size().x + repo_tab_chrome(dirty)
}

/// Which tabs fit in `room`, and the widest any may be. They shrink before any goes in the
/// overflow list, and the one in use is always among those showing.
pub fn fit_tabs(natural: &[f32], room: f32, active: usize) -> (Vec<usize>, f32) {
    if natural.iter().sum::<f32>() <= room {
        return ((0..natural.len()).collect(), f32::INFINITY);
    }
    let fit = ((room / MIN_REPO_TAB).floor() as usize).max(1);
    if fit >= natural.len() {
        // The narrow ones keep their width and the wide ones share what's left.
        let mut widths: Vec<f32> = natural.to_vec();
        widths.sort_by(f32::total_cmp);
        let (mut left, mut count, mut cap) = (room, widths.len(), room / widths.len() as f32);
        for w in widths {
            if w > cap {
                break;
            }
            left -= w;
            count -= 1;
            cap = left / count.max(1) as f32;
        }
        return ((0..natural.len()).collect(), cap);
    }
    let mut shown: Vec<usize> = (0..fit).collect();
    if active >= fit {
        shown[fit - 1] = active;
    }
    (shown, room / fit as f32)
}

#[cfg(test)]
#[test]
fn tabs_shrink_then_overflow_and_the_active_one_stays() {
    let wide = [300.0; 9];
    let (shown, cap) = fit_tabs(&wide[..2], 1000.0, 0);
    assert_eq!((shown.len(), cap), (2, f32::INFINITY));
    let (shown, cap) = fit_tabs(&wide[..4], 800.0, 0);
    assert_eq!((shown.len(), cap), (4, 200.0));
    // One short tab keeps what it needs.
    let (shown, cap) = fit_tabs(&[100.0, 500.0, 500.0], 700.0, 0);
    assert_eq!((shown.len(), cap), (3, 300.0));
    let (shown, _) = fit_tabs(&wide, 500.0, 8);
    assert_eq!(shown, vec![0, 1, 2, 3, 8]);
}

/// The palette's place in the title bar, looking like the search box it is.
pub fn command_field(ui: &mut Ui, width: f32, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Command palette"));
    if ui.is_rect_visible(rect) {
        ui.painter().rect(rect, 4.0, if resp.hovered() { p.hover } else { p.panel }, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        ui.painter().text(rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, icon::MAGNIFYING_GLASS, prop(13.0), p.dim);
        ui.painter().text(rect.left_center() + vec2(26.0, 0.0), Align2::LEFT_CENTER, "Search or run a command", prop(12.0), p.dim);
        ui.painter().text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, "Ctrl+P", prop(11.0), p.faint);
        crate::widgets::focus_ring(ui, &resp, p);
    }
    tooltip(resp, "Command palette", "Every command, branch and file", p)
}

/// One open repository in the titlebar. Returns whether its close button was clicked.
pub fn repo_tab(ui: &mut Ui, name: &str, dirty: bool, selected: bool, path: &str, cap: f32, p: &Palette) -> (Response, bool) {
    const TOP_GAP: f32 = 5.0;
    const CLOSE: f32 = REPO_TAB_CLOSE;
    let color = if selected { p.text } else { p.dim };
    let room = (cap - repo_tab_chrome(dirty)).max(16.0);
    // Cut in the middle: repositories often differ only at the end of their names.
    let natural = ui.painter().layout_no_wrap(name.to_string(), prop(12.5), color).size().x;
    let shown = if natural > room { crate::widgets::middle_elided(name, ((room / natural) * name.chars().count() as f32) as usize) } else { name.to_string() };
    let mut job = egui::text::LayoutJob::simple_singleline(shown, prop(12.5), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let galley = ui.painter().layout_job(job);
    let width = galley.size().x + repo_tab_chrome(dirty);
    let (outer, resp) = ui.allocate_exact_size(vec2(width, TITLE_BAR_HEIGHT), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    crate::widgets::focus_ring(ui, &resp, p);
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
    let hover = if dirty { format!("{path}\nHas uncommitted changes") } else { path.to_string() };
    (resp.on_hover_text(hover), clicked)
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
    crate::widgets::focus_ring(ui, &resp, p);
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

/// How much of the title bar's right end the window controls take.
pub fn window_controls_width() -> f32 {
    CONTROL_WIDTH * 3.0
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
    static WORK: [RailItem; 6] = [
        item!(HOUSE, "Home", "Overview and start work", Tab::home, Home),
        item!(GIT_FORK, "Graph", "Commit graph", Tab::graph, Graph),
        item!(NOTE_PENCIL, "Changes", "Stage and commit", Tab::changes, Changes),
        item!(CODE, "Editor", "IDE mode: a click in the Files tree opens the file for editing. Click to turn it on or off (Ctrl+Shift+E).", || Tab::Editor(Box::default()), Editor),
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
        item!(FILE_ARROW_DOWN, "Patches", "Saved patches", || Tab::Patches(Box::default()), Patches),
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

/// The icon of a tab's kind: the rail's for the views on it, and one of their own for files,
/// diffs and the other views that open from something else.
pub fn tab_glyph(tab: &Tab) -> &'static str {
    if let Some(item) = rail_top().into_iter().flatten().chain(rail_bottom()).find(|i| (i.is)(tab)) {
        return item.glyph;
    }
    match tab {
        Tab::File(_) => icon::FILE_TEXT,
        Tab::Diff(_) => icon::GIT_DIFF,
        Tab::Merge(_) => icon::GIT_MERGE,
        Tab::History(_) => icon::CLOCK,
        Tab::PullRequest(_) => icon::GIT_PULL_REQUEST,
        Tab::Tree(_) => icon::FOLDER_OPEN,
        Tab::Grep(_) => icon::TEXT_AA,
        Tab::Prune(_) => icon::BROOM,
        Tab::StashHunks(_) => icon::ARCHIVE,
        _ => icon::SQUARE,
    }
}

/// One hue per rail group, drawn from the palette's fixed order.
fn group_tint(group: usize, p: &Palette) -> Color32 {
    // These are graphics on the rail's background, which need 3:1.
    crate::theme::readable(p.series[[0, 2, 3, 6][group % 4]], p.text, p.bg, 3.0)
}

pub fn rail_width(labels: bool) -> f32 {
    if labels { RAIL_LABELLED_WIDTH } else { RAIL_WIDTH }
}

fn rail_button(ui: &mut Ui, item: &RailItem, tint: Color32, active: bool, labels: bool, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(rail_width(labels), RAIL_ITEM), Sense::click());
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
        if labels {
            ui.painter().text(pos2(rect.left() + RAIL_WIDTH / 2.0, rect.center().y), Align2::CENTER_CENTER, item.glyph, prop(20.0), color);
            let ink = if active || resp.hovered() { p.text } else { p.dim };
            ui.painter().with_clip_rect(rect).text(pos2(rect.left() + RAIL_WIDTH - 2.0, rect.center().y), Align2::LEFT_CENTER, name, prop(12.5), ink);
        } else {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, item.glyph, prop(20.0), color);
        }
        crate::widgets::focus_ring(ui, &resp, p);
    }
    tooltip(resp, name, item.tip, p)
}

/// Names for the groups in the More list, in `rail_top`'s order.
const MORE_GROUPS: [&str; 4] = ["Work", "Together", "History", "Insight"];

/// From the bottom of the rail, what stays when the rest go in More.
const KEPT_ON_RAIL: [&str; 2] = ["Console", "Settings"];

/// Where each view goes: on the rail, or behind More. Built from the settings each frame.
pub struct RailPlaces<'a> {
    /// Every view on the rail, rather than the first group and a More list.
    pub full: bool,
    /// Taken from More and given a place on the rail.
    pub pinned: &'a [String],
    /// Taken off the rail, so in More.
    pub hidden: &'a [String],
}

impl RailPlaces<'_> {
    /// Whether this view is on the rail before anyone pins or removes anything.
    fn by_default(&self, item: &RailItem) -> bool {
        self.full || rail_top()[0].iter().any(|i| i.name == item.name) || KEPT_ON_RAIL.contains(&item.name)
    }

    pub fn on_rail(&self, item: &RailItem) -> bool {
        // Settings can't go: it's where the rest are brought back from when More is empty.
        let named = |list: &[String]| list.iter().any(|n| n == item.name);
        item.name == "Settings" || ((self.by_default(item) || named(self.pinned)) && !named(self.hidden))
    }

    /// Changes the settings to put a view on the rail or take it off. Only one of the two
    /// lists ever names a view: pinned for one that isn't there by default, hidden for one
    /// that is. So switching "All views" on and off keeps what was chosen for each.
    pub fn set(name: &str, on: bool, pinned: &mut Vec<String>, hidden: &mut Vec<String>) {
        let Some(item) = rail_top().into_iter().flatten().chain(rail_bottom()).find(|i| i.name == name) else { return };
        let default = rail_top()[0].iter().any(|i| i.name == item.name) || KEPT_ON_RAIL.contains(&item.name);
        pinned.retain(|n| n != name);
        hidden.retain(|n| n != name);
        if on && !default {
            pinned.push(name.to_string());
        } else if !on {
            hidden.push(name.to_string());
        }
    }
}

/// What a click on the rail asks for.
pub enum RailEvent {
    /// Open this view; `true` when it came from the More list.
    Open(Tab, bool),
    /// Put this view on the rail, or take it off.
    Place(&'static str, bool),
    /// The Editor item: IDE mode on or off. Editing is a way of working rather than a view,
    /// and an empty editor tab said nothing about how to open a file in it.
    IdeMode,
}

/// The rail item that switches IDE mode rather than opening a view.
pub const IDE_ITEM: &str = "Editor";

fn click_event(item: &RailItem, from_more: bool) -> RailEvent {
    if item.name == IDE_ITEM { RailEvent::IdeMode } else { RailEvent::Open((item.make)(), from_more) }
}

/// The views that sit behind the More button, grouped as on the rail, each with a pin.
fn more_list(ui: &mut Ui, places: &RailPlaces, is_focused: &dyn Fn(&RailItem) -> bool, p: &Palette) -> Option<RailEvent> {
    let mut event = None;
    let pin = crate::i18n::t("Pin to rail");
    let mut entry = |ui: &mut Ui, item: &'static RailItem, tint: Color32| {
        let name = crate::i18n::t(item.name);
        ui.horizontal(|ui| {
            let r = ui.add(egui::Button::new((RichText::new(item.glyph).color(tint), RichText::new(name))).selected(is_focused(item)).frame_when_inactive(false).min_size(vec2(150.0, 0.0)));
            r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
            let r = r.on_hover_text(item.tip);
            r.context_menu(|ui| {
                if ui.button(pin).clicked() {
                    event = Some(RailEvent::Place(item.name, true));
                }
            });
            if r.clicked() {
                event = Some(click_event(item, true));
                ui.close();
            }
            let b = ui.add(egui::Button::new(RichText::new(icon::PUSH_PIN).color(p.dim)).frame_when_inactive(false));
            b.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{pin}: {name}")));
            if b.on_hover_text(pin).clicked() {
                event = Some(RailEvent::Place(item.name, true));
            }
        });
    };
    let mut first = true;
    let mut heading = |ui: &mut Ui, title: &str| {
        if !std::mem::take(&mut first) {
            ui.separator();
        }
        ui.label(RichText::new(crate::i18n::t(title)).small().color(p.dim));
    };
    for (i, group) in rail_top().into_iter().enumerate() {
        let off: Vec<&'static RailItem> = group.iter().filter(|it| !places.on_rail(it)).collect();
        if off.is_empty() {
            continue;
        }
        heading(ui, MORE_GROUPS[i]);
        for item in off {
            entry(ui, item, group_tint(i, p));
        }
    }
    let off: Vec<&'static RailItem> = rail_bottom().iter().filter(|it| !places.on_rail(it)).collect();
    if !off.is_empty() {
        heading(ui, "Repository");
        for item in off {
            entry(ui, item, p.dim);
        }
    }
    event
}

/// A rail icon's right-click menu, for taking it off the rail.
fn rail_menu(resp: &Response, item: &'static RailItem, event: &mut Option<RailEvent>) {
    if item.name == "Settings" {
        return;
    }
    resp.context_menu(|ui| {
        if ui.button(crate::i18n::t("Remove from rail")).clicked() {
            *event = Some(RailEvent::Place(item.name, false));
        }
    });
}

/// One icon on the rail, with its menu.
fn rail_entry(ui: &mut Ui, item: &'static RailItem, tint: Color32, is_focused: &dyn Fn(&RailItem) -> bool, labels: bool, p: &Palette, event: &mut Option<RailEvent>) {
    let resp = rail_button(ui, item, tint, is_focused(item), labels, p);
    rail_menu(&resp, item, event);
    if resp.clicked() {
        *event = Some(click_event(item, false));
    }
}

/// The views rail. `is_focused` lights up the icon of the tab in front. Views not on it are
/// in the More list, where each can be pinned; a right-click takes one off again. The top
/// part scrolls if the window is too short. With `labels`, each has its name beside it.
pub fn rail(ui: &mut Ui, is_focused: &dyn Fn(&RailItem) -> bool, places: &RailPlaces, labels: bool, p: &Palette) -> Option<RailEvent> {
    let mut event = None;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let bottom: Vec<&'static RailItem> = rail_bottom().iter().filter(|i| places.on_rail(i)).collect();
    let pinned = bottom.len() as f32 * RAIL_ITEM + 9.0;
    let height = (ui.available_height() - pinned).max(RAIL_ITEM);
    let groups: Vec<(usize, Vec<&'static RailItem>)> = rail_top().into_iter().enumerate().map(|(i, g)| (i, g.iter().filter(|it| places.on_rail(it)).collect::<Vec<_>>())).filter(|(_, g)| !g.is_empty()).collect();
    let off: Vec<&'static RailItem> = rail_top().into_iter().flatten().chain(rail_bottom()).filter(|i| !places.on_rail(i)).collect();
    let show_more = !off.is_empty();
    ui.allocate_ui_with_layout(vec2(rail_width(labels), height), egui::Layout::top_down(egui::Align::Min), |ui| {
        let content = groups.iter().map(|(_, g)| g.len() as f32 * RAIL_ITEM + 9.0).sum::<f32>() + if show_more { RAIL_ITEM } else { 0.0 };
        let area = egui::ScrollArea::vertical().id_salt("rail").max_height(height).auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.add_space(4.0);
            for (n, (i, group)) in groups.iter().enumerate() {
                if n > 0 {
                    separator(ui, labels, p);
                }
                for item in group {
                    rail_entry(ui, item, group_tint(*i, p), is_focused, labels, p, &mut event);
                }
            }
            if show_more {
                let hidden_on = off.iter().any(|i| is_focused(i));
                let more = RailItem { glyph: icon::DOTS_THREE_CIRCLE, name: "More", tip: "Every other view. Pin one to keep it on the rail.", make: Tab::home, is: |_| false };
                let resp = rail_button(ui, &more, p.dim, hidden_on, labels, p);
                egui::Popup::menu(&resp).align(egui::RectAlign::RIGHT_START).show(|ui| {
                    if let Some(e) = more_list(ui, places, is_focused, p) {
                        event = Some(e);
                    }
                });
            }
        });
        // With no scroll bar, this says there's more below when the window is short.
        if content > height && area.state.offset.y + height < content - 1.0 {
            let r = area.inner_rect;
            let fade = Rect::from_min_max(pos2(r.left(), r.bottom() - 14.0), r.max);
            ui.painter().rect_filled(fade, 0.0, p.bg);
            ui.painter().text(fade.center(), Align2::CENTER_CENTER, icon::CARET_DOWN, prop(11.0), p.dim);
        }
    });
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        for item in bottom.iter().rev() {
            rail_entry(ui, item, p.dim, is_focused, labels, p, &mut event);
        }
    });
    event
}

fn separator(ui: &mut Ui, labels: bool, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(rail_width(labels), 9.0), Sense::hover());
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
    fn rail_icons_are_readable_in_every_theme() {
        for t in gitem_core::settings::ThemeChoice::ALL {
            let p = Palette::get(t);
            for group in 0..4 {
                let c = crate::theme::contrast(group_tint(group, &p), p.bg);
                assert!(c >= 3.0, "{} group {group}: {c:.2}", t.name());
            }
        }
    }

    fn on(full: bool, pinned: &[String], hidden: &[String]) -> Vec<&'static str> {
        let places = RailPlaces { full, pinned, hidden };
        rail_top().into_iter().flatten().chain(rail_bottom()).filter(|i| places.on_rail(i)).map(|i| i.name).collect()
    }

    #[test]
    fn pinning_and_removing_rail_views() {
        let (mut pinned, mut hidden) = (Vec::new(), Vec::new());
        let start = on(false, &pinned, &hidden);
        assert!(start.contains(&"Graph") && start.contains(&"Console") && !start.contains(&"Reflog"));

        RailPlaces::set("Reflog", true, &mut pinned, &mut hidden);
        RailPlaces::set("Graph", false, &mut pinned, &mut hidden);
        RailPlaces::set("Console", false, &mut pinned, &mut hidden);
        let now = on(false, &pinned, &hidden);
        assert!(now.contains(&"Reflog") && !now.contains(&"Graph") && !now.contains(&"Console"), "{now:?}");
        assert_eq!((pinned.clone(), hidden.clone()), (vec!["Reflog".to_string()], vec!["Graph".to_string(), "Console".to_string()]));

        // Back where it started, with nothing left in either list.
        RailPlaces::set("Reflog", false, &mut pinned, &mut hidden);
        RailPlaces::set("Graph", true, &mut pinned, &mut hidden);
        RailPlaces::set("Console", true, &mut pinned, &mut hidden);
        RailPlaces::set("Reflog", true, &mut pinned, &mut hidden);
        RailPlaces::set("Reflog", false, &mut pinned, &mut hidden);
        assert_eq!(on(false, &pinned, &hidden).len(), start.len(), "{pinned:?} {hidden:?}");
        assert!(pinned.is_empty());
    }

    #[test]
    fn a_view_removed_with_every_view_on_still_goes_and_settings_never_does() {
        let hidden = vec!["Reflog".to_string(), "Settings".to_string()];
        let now = on(true, &[], &hidden);
        assert!(!now.contains(&"Reflog") && now.contains(&"Settings") && now.contains(&"Health"));
        RailPlaces::set("nothing by that name", true, &mut Vec::new(), &mut Vec::new());
    }

    #[test]
    fn every_view_has_a_distinct_icon() {
        let mut all: Vec<&RailItem> = rail_top().into_iter().flatten().collect();
        all.extend(rail_bottom());
        assert_eq!(all.len(), 23);
        let glyphs: std::collections::HashSet<_> = all.iter().map(|i| i.glyph).collect();
        assert_eq!(glyphs.len(), all.len());
        for i in &all {
            assert!((i.is)(&(i.make)()), "{} doesn't recognise its own tab", i.name);
        }
    }
}
