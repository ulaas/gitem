//! Colour palettes and egui style. Chart/lane colours use a CVD-validated
//! categorical order; heatmaps use a single-hue sequential ramp.
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use gitem_core::settings::ThemeChoice;

#[derive(Clone)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub panel2: Color32,
    pub hover: Color32,
    pub selection: Color32,
    pub border: Color32,
    /// Buttons sit clearly above every panel colour.
    pub button: Color32,
    pub button_hover: Color32,
    pub button_border: Color32,
    pub status_bg: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub add_bg: Color32,
    pub add_word: Color32,
    pub add_fg: Color32,
    pub del_bg: Color32,
    pub del_word: Color32,
    pub del_fg: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub conflict_bg: Color32,
    pub series: [Color32; 8],
    pub seq: [Color32; 6],
}

const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

impl Palette {
    pub fn get(t: ThemeChoice) -> Self {
        match t {
            ThemeChoice::Dark => Self::dark(),
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Midnight => Self::midnight(),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: hex(0x17191d),
            panel: hex(0x1d2025),
            panel2: hex(0x25292f),
            hover: hex(0x2b3037),
            selection: hex(0x274b80),
            border: hex(0x32363d),
            button: hex(0x343b46),
            button_hover: hex(0x424b59),
            button_border: hex(0x4b5462),
            status_bg: hex(0x1b3454),
            text: hex(0xe6e6e3),
            dim: hex(0xa8a9a3),
            faint: hex(0x7d7e79),
            accent: hex(0x3987e5),
            on_accent: Color32::WHITE,
            add_bg: Color32::from_rgba_unmultiplied(25, 158, 112, 40),
            add_word: Color32::from_rgba_unmultiplied(25, 158, 112, 110),
            add_fg: hex(0x38c793),
            del_bg: Color32::from_rgba_unmultiplied(230, 103, 103, 40),
            del_word: Color32::from_rgba_unmultiplied(230, 103, 103, 110),
            del_fg: hex(0xe88080),
            good: hex(0x0ca30c),
            warn: hex(0xfab219),
            danger: hex(0xd03b3b),
            conflict_bg: Color32::from_rgba_unmultiplied(250, 178, 25, 34),
            series: [hex(0x3987e5), hex(0xd95926), hex(0x199e70), hex(0xc98500), hex(0xd55181), hex(0x008300), hex(0x9085e9), hex(0xe66767)],
            seq: [hex(0x184f95), hex(0x1c5cab), hex(0x2a78d6), hex(0x5598e7), hex(0x86b6ef), hex(0xb7d3f6)],
        }
    }

    pub fn midnight() -> Self {
        Self { bg: hex(0x0c0e14), panel: hex(0x11141c), panel2: hex(0x181c27), hover: hex(0x1f2431), border: hex(0x262b38), selection: hex(0x1f4176), button: hex(0x252c3b), button_hover: hex(0x323b4e), button_border: hex(0x3b4559), status_bg: hex(0x142a4a), ..Self::dark() }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            bg: hex(0xfcfcfb),
            panel: hex(0xf4f4f1),
            panel2: hex(0xeaeae6),
            hover: hex(0xe4e4df),
            selection: hex(0xcde2fb),
            border: hex(0xd6d5ce),
            button: hex(0xffffff),
            button_hover: hex(0xe6effb),
            button_border: hex(0xb6b5ad),
            status_bg: hex(0xd3e4f9),
            text: hex(0x1a1a18),
            dim: hex(0x52514e),
            faint: hex(0x7b7a75),
            accent: hex(0x2a78d6),
            on_accent: Color32::WHITE,
            add_bg: Color32::from_rgba_unmultiplied(27, 175, 122, 40),
            add_word: Color32::from_rgba_unmultiplied(27, 175, 122, 110),
            add_fg: hex(0x0b7a52),
            del_bg: Color32::from_rgba_unmultiplied(227, 73, 72, 34),
            del_word: Color32::from_rgba_unmultiplied(227, 73, 72, 100),
            del_fg: hex(0xc03030),
            good: hex(0x008300),
            warn: hex(0xb97800),
            danger: hex(0xd03b3b),
            conflict_bg: Color32::from_rgba_unmultiplied(237, 161, 0, 40),
            series: [hex(0x2a78d6), hex(0xeb6834), hex(0x1baf7a), hex(0xeda100), hex(0xe87ba4), hex(0x008300), hex(0x6250d6), hex(0xe34948)],
            seq: [hex(0xb7d3f6), hex(0x86b6ef), hex(0x5598e7), hex(0x2a78d6), hex(0x1c5cab), hex(0x104281)],
        }
    }

    /// Commits to pull: the same green wherever they appear.
    pub fn incoming(&self) -> Color32 {
        self.series[2]
    }

    /// Commits to push: the same orange wherever they appear.
    pub fn outgoing(&self) -> Color32 {
        self.series[1]
    }

    /// Lane or entity colour, fixed by index.
    pub fn lane(&self, i: u16) -> Color32 {
        self.series[i as usize % self.series.len()]
    }

    /// Stable colour for a person, derived from their identity and not their rank.
    pub fn person(&self, key: &str) -> Color32 {
        let mut h: u32 = 2166136261;
        for b in key.to_lowercase().bytes() {
            h = (h ^ b as u32).wrapping_mul(16777619);
        }
        self.series[h as usize % self.series.len()]
    }

    /// Sequential heat colour for 0.0 (cold, old) to 1.0 (hot, recent).
    pub fn heat(&self, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        let i = ((t * (self.seq.len() - 1) as f32).round() as usize).min(self.seq.len() - 1);
        self.seq[i]
    }
}

/// Extend egui's bundled fonts with system fonts so arrows, symbols, and non-Latin
/// names in commits and paths render. Missing files are skipped.
pub fn install_fonts(ctx: &egui::Context) {
    #[cfg(windows)]
    const CANDIDATES: &[&str] = &["C:/Windows/Fonts/seguisym.ttf", "C:/Windows/Fonts/segoeui.ttf", "C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/malgun.ttf"];
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &["/System/Library/Fonts/Apple Symbols.ttf", "/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/PingFang.ttc"];
    #[cfg(all(unix, not(target_os = "macos")))]
    const CANDIDATES: &[&str] = &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/dejavu/DejaVuSans.ttf", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"];
    let mut fonts = egui::FontDefinitions::default();
    // Icons come first: they're the same on every platform.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("system:{path}");
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.clone());
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// Flat dock tabs: the active one shares the panel's colour and runs into its content,
/// the rest sit in a darker bar. An outline only appears for keyboard focus.
pub fn dock_style(style: &egui::Style, p: &Palette) -> egui_dock::Style {
    use egui_dock::TabInteractionStyle;
    let tab = |bg_fill: Color32, text_color: Color32, outline_color: Color32| TabInteractionStyle { outline_color, corner_radius: CornerRadius::ZERO, bg_fill, text_color };
    let none = Color32::TRANSPARENT;
    let mut s = egui_dock::Style::from_egui(style);
    s.tab_bar.bg_fill = p.bg;
    s.tab_bar.height = 30.0;
    s.tab_bar.hline_color = p.border;
    s.tab.hline_below_active_tab_name = false;
    s.tab.spacing = 0.0;
    s.tab.active = tab(p.panel, p.text, none);
    s.tab.focused = tab(p.panel, p.text, none);
    s.tab.inactive = tab(p.bg, p.dim, none);
    s.tab.hovered = tab(p.hover, p.text, none);
    s.tab.active_with_kb_focus = tab(p.panel, p.text, p.accent);
    s.tab.focused_with_kb_focus = tab(p.panel, p.text, p.accent);
    s.tab.inactive_with_kb_focus = tab(p.bg, p.text, p.accent);
    s.tab.tab_body.stroke = Stroke::NONE;
    s.tab.tab_body.bg_fill = p.panel;
    s.tab.tab_body.corner_radius = CornerRadius::ZERO;
    s.buttons.close_tab_color = p.faint;
    s.buttons.close_tab_active_color = p.text;
    s.buttons.close_tab_bg_fill = p.hover;
    s.buttons.add_tab_color = p.faint;
    s.buttons.add_tab_active_color = p.text;
    s.buttons.add_tab_bg_fill = p.hover;
    s.buttons.add_tab_border_color = none;
    s.buttons.close_all_tabs_color = p.faint;
    s.buttons.close_all_tabs_active_color = p.text;
    s.buttons.close_all_tabs_bg_fill = p.hover;
    s.buttons.close_all_tabs_border_color = none;
    s.separator.color_idle = p.border;
    s.separator.color_hovered = p.accent;
    s.separator.color_dragged = p.accent;
    s
}

pub fn apply(ctx: &egui::Context, p: &Palette, scale: f32) {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.panel2;
    v.code_bg_color = p.panel2;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(5);
    v.override_text_color = Some(p.text);
    let w = &mut v.widgets;
    let radius = CornerRadius::same(3);
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    w.inactive.bg_fill = p.button;
    w.inactive.weak_bg_fill = p.button;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_stroke = Stroke::new(1.0, p.button_border);
    w.inactive.corner_radius = radius;
    w.hovered.bg_fill = p.button_hover;
    w.hovered.weak_bg_fill = p.button_hover;
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    w.hovered.corner_radius = radius;
    w.active.bg_fill = p.selection;
    w.active.weak_bg_fill = p.selection;
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.corner_radius = radius;
    w.open.bg_fill = p.button_hover;
    w.open.weak_bg_fill = p.button_hover;
    w.open.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;

    let mut style = (*ctx.style_of(if p.dark { egui::Theme::Dark } else { egui::Theme::Light })).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(7.0, 3.0);
    style.spacing.interact_size.y = 22.0;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.floating = true;
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
    ]
    .into();
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_style_of(if p.dark { egui::Theme::Dark } else { egui::Theme::Light }, style);
    ctx.set_zoom_factor(scale.clamp(0.6, 3.0));
}
