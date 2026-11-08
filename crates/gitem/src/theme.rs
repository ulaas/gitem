//! Colour palettes and egui style. Chart and lane colours aren't enough on their own for
//! colour-blind readers, red against green least of all, so nothing should depend on them
//! alone: a diff also has its + and - signs. Heatmaps use a single-hue sequential ramp.
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

/// Named accents: `(name, on dark themes, on light themes)`. A colour that reads on a
/// dark background is too pale for a light one, so each has both.
pub const ACCENTS: [(&str, u32, u32); 19] = [
    ("Blue", 0x4c95ec, 0x2563c9),
    ("Sky", 0x38bdf8, 0x0369a1),
    ("Cyan", 0x22d3ee, 0x0e7490),
    ("Teal", 0x2dd4bf, 0x0f766e),
    ("Emerald", 0x34d399, 0x047857),
    ("Green", 0x4ade80, 0x15803d),
    ("Lime", 0xa3e635, 0x4d7c0f),
    ("Yellow", 0xfacc15, 0x8f5a06),
    ("Amber", 0xfbbf24, 0xa84d08),
    ("Orange", 0xfb923c, 0xc2410c),
    ("Coral", 0xff7f6e, 0xc8402e),
    ("Red", 0xf87171, 0xc81e1e),
    ("Rose", 0xfb7185, 0xbe123c),
    ("Pink", 0xf472b6, 0xbe185d),
    ("Fuchsia", 0xe879f9, 0xa21caf),
    ("Purple", 0xc084fc, 0x7e22ce),
    ("Violet", 0xa78bfa, 0x6d28d9),
    ("Indigo", 0x818cf8, 0x4338ca),
    ("Gray", 0x9aa7b8, 0x475569),
];

/// The colour of an accent setting, if it's a name or `#rrggbb`.
pub fn accent_color(setting: &str, dark: bool) -> Option<Color32> {
    if let Some(digits) = setting.strip_prefix('#') {
        return (digits.len() == 6).then(|| u32::from_str_radix(digits, 16).ok()).flatten().map(hex);
    }
    ACCENTS.iter().find(|a| a.0 == setting).map(|a| hex(if dark { a.1 } else { a.2 }))
}

/// `top` painted over the opaque `bottom`, as the screen would show it.
pub fn over(top: Color32, bottom: Color32) -> Color32 {
    let keep = 255 - top.a() as u32;
    let mix = |t: u8, b: u8| (t as u32 + (b as u32 * keep + 127) / 255).min(255) as u8;
    Color32::from_rgb(mix(top.r(), bottom.r()), mix(top.g(), bottom.g()), mix(top.b(), bottom.b()))
}

/// WCAG contrast ratio between two opaque colours, from 1 to 21.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// The closest colour to `c`, moving towards `towards`, that reads on `bg` at `ratio`.
pub fn readable(c: Color32, towards: Color32, bg: Color32, ratio: f32) -> Color32 {
    (0..=20).map(|i| mix(c, towards, i as f32 / 20.0)).find(|m| contrast(*m, bg) >= ratio).unwrap_or(towards)
}

/// Black or white, whichever reads better on a fill that could be any colour.
pub fn ink_on(fill: Color32) -> Color32 {
    if contrast(Color32::WHITE, fill) >= contrast(Color32::BLACK, fill) { Color32::WHITE } else { Color32::BLACK }
}

/// The text of a chip, whose background is a tint of `color` over `panel`.
pub fn chip_ink(color: Color32, panel: Color32, text: Color32) -> Color32 {
    readable(color, text, over(color.gamma_multiply(0.14), panel), 4.5)
}

impl Palette {
    /// Behind the merge and rebase banner. Opaque, as the window behind it is near black.
    pub fn banner_fill(&self) -> Color32 {
        over(self.warn.gamma_multiply(0.15), self.panel)
    }

    /// Secondary text and status colours, brought up to what's needed to read them on any
    /// surface they appear on. Their hue stays, as far as it can.
    fn tuned(self) -> Self {
        let t = self.text;
        let surfaces = |c: Color32, ratio: f32, tight: f32| {
            let c = readable(c, t, self.panel, ratio);
            let c = readable(c, t, self.panel2, ratio);
            let c = readable(c, t, self.hover, tight);
            readable(c, t, self.selection, tight)
        };
        Self { faint: surfaces(self.faint, 4.5, 3.0), danger: surfaces(self.danger, 4.5, 3.0), warn: surfaces(self.warn, 4.5, 3.0), good: surfaces(self.good, 4.5, 3.0), ..self }
    }

    pub fn get(t: ThemeChoice) -> Self {
        match t {
            ThemeChoice::Dark => Self::dark().tuned(),
            ThemeChoice::Light => Self::light().tuned(),
            ThemeChoice::Midnight => Self::midnight().tuned(),
            ThemeChoice::Black => Self::dark_family([0x000000, 0x0b0b0c, 0x161618, 0x202023, 0x2b2b2f], [0x28282c, 0x38383d, 0x47474d], [0xd0d0d0, 0xa6a6a6, 0x848484], 0x4c95ec),
            ThemeChoice::Slate => Self::dark_family([0x1a2029, 0x202733, 0x2a3341, 0x323c4c, 0x3a4555], [0x3a465b, 0x485671, 0x576583], [0xcbd0d8, 0xa5aebc, 0x8c96a8], 0x5aa2f0),
            ThemeChoice::Forest => Self::dark_family([0x141c18, 0x1a241f, 0x24302a, 0x2c3a33, 0x33443b], [0x35473e, 0x43584d, 0x50675a], [0xcad3cd, 0xa2b0a7, 0x84958b], 0x3fbf8a),
            ThemeChoice::Plum => Self::dark_family([0x1c1722, 0x241d2b, 0x2f2738, 0x382f43, 0x40364c], [0x41364e, 0x504361, 0x5e4f71], [0xd2cdd8, 0xada3b9, 0x93899f], 0xb083f0),
            ThemeChoice::Mocha => Self::dark_family([0x1e1815, 0x26201c, 0x322a25, 0x3b322c, 0x453a33], [0x473c35, 0x584b42, 0x67584e], [0xd6cfc8, 0xb5a99e, 0x998d82], 0xe0954a),
            ThemeChoice::Paper => Self::light_family([0xfbf7ee, 0xf3ecdf, 0xe9e0cf, 0xe2d8c5, 0xd3c8b3], [0xfffcf5, 0xf1e8d8, 0xb3a78f], [0x2b251b, 0x5c5345, 0x756b5a], 0xb4531c),
            ThemeChoice::Mist => Self::light_family([0xf5f8fb, 0xebf0f5, 0xdfe6ee, 0xd6dfe9, 0xc7d2de], [0xffffff, 0xe2edf8, 0xa9b7c6], [0x17212c, 0x465362, 0x63707c], 0x1f6fc4),
            ThemeChoice::Sage => Self::light_family([0xf5f8f3, 0xebf0e7, 0xdfe6da, 0xd5ddcf, 0xc5cfbd], [0xffffff, 0xe4eede, 0xa8b49d], [0x1b241a, 0x4a5546, 0x66715e], 0x1f7a4d),
            ThemeChoice::Blush => Self::light_family([0xfcf6f7, 0xf5ebee, 0xecdfe3, 0xe4d5da, 0xd6c4ca], [0xffffff, 0xf6e5ea, 0xb8a4ab], [0x2a1c20, 0x5e4a50, 0x7a656b], 0xbe2f63),
        }
    }

    /// A theme with an accent setting applied. An empty or unreadable one leaves the
    /// theme's own accent.
    pub fn for_settings(t: ThemeChoice, accent: &str) -> Self {
        let base = Self::get(t);
        match accent_color(accent, base.dark) {
            Some(c) => base.with_accent(c),
            None => base,
        }
    }

    /// Everything that follows the accent: selections, the status bar, and text on it.
    pub fn with_accent(self, accent: Color32) -> Self {
        let on_accent = [Color32::WHITE, hex(0x111318)].into_iter().find(|c| contrast(*c, accent) >= 4.5).unwrap_or(if contrast(Color32::WHITE, accent) > contrast(hex(0x111318), accent) { Color32::WHITE } else { hex(0x111318) });
        let (selection, status) = if self.dark { (0.3, 0.24) } else { (0.24, 0.2) };
        Self { accent, on_accent, selection: mix(self.bg, accent, selection), status_bg: mix(self.bg, accent, status), ..self }.tuned()
    }

    /// Surfaces darkest first: `bg, panel, panel2, hover, border`. Then buttons:
    /// `fill, hover, border`, and text: `text, dim, faint`.
    fn dark_family(surface: [u32; 5], button: [u32; 3], text: [u32; 3], accent: u32) -> Self {
        Self { bg: hex(surface[0]), panel: hex(surface[1]), panel2: hex(surface[2]), hover: hex(surface[3]), border: hex(surface[4]), button: hex(button[0]), button_hover: hex(button[1]), button_border: hex(button[2]), text: hex(text[0]), dim: hex(text[1]), faint: hex(text[2]), ..Self::dark() }.with_accent(hex(accent))
    }

    fn light_family(surface: [u32; 5], button: [u32; 3], text: [u32; 3], accent: u32) -> Self {
        Self { bg: hex(surface[0]), panel: hex(surface[1]), panel2: hex(surface[2]), hover: hex(surface[3]), border: hex(surface[4]), button: hex(button[0]), button_hover: hex(button[1]), button_border: hex(button[2]), text: hex(text[0]), dim: hex(text[1]), faint: hex(text[2]), ..Self::light() }.with_accent(hex(accent))
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: hex(0x17191d),
            panel: hex(0x1d2025),
            panel2: hex(0x25292f),
            hover: hex(0x2b3037),
            selection: hex(0x1f3d6b),
            border: hex(0x32363d),
            button: hex(0x343b46),
            button_hover: hex(0x424b59),
            button_border: hex(0x4b5462),
            status_bg: hex(0x1b3454),
            text: hex(0xcdcecb),
            dim: hex(0xa8a9a3),
            faint: hex(0x7d7e79),
            accent: hex(0x2875d3),
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
            accent: hex(0x2875d3),
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

    /// A series colour for text and icons on a panel, where it has to be read and not just seen.
    pub fn series_text(&self, i: usize) -> Color32 {
        readable(self.series[i % self.series.len()], self.text, self.panel, 4.5)
    }

    /// Blue and orange in place of green and red in diffs, for anyone who can't tell those
    /// apart. Red-green is the common kind, and these stay distinct under it.
    pub fn colorblind(self, on: bool) -> Self {
        if !on {
            return self;
        }
        let (add, del) = if self.dark { (hex(0x5aa9e6), hex(0xf0a13c)) } else { (hex(0x1b6fb8), hex(0xb35f00)) };
        let tint = |c: Color32, a: u8| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
        Self { add_fg: add, add_bg: tint(add, 40), add_word: tint(add, 110), del_fg: del, del_bg: tint(del, 40), del_word: tint(del, 110), ..self }
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
    const CANDIDATES: &[&str] = &["C:/Windows/Fonts/seguisym.ttf", "C:/Windows/Fonts/segoeui.ttf", "C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/YuGothM.ttc", "C:/Windows/Fonts/malgun.ttf"];
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &["/System/Library/Fonts/Apple Symbols.ttf", "/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/PingFang.ttc"];
    #[cfg(all(unix, not(target_os = "macos")))]
    const CANDIDATES: &[&str] = &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/dejavu/DejaVuSans.ttf", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"];
    let mut fonts = egui::FontDefinitions::default();
    // Icons come first: they're the same on every platform.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    for path in CANDIDATES {
        if let Some(data) = system_font(path) {
            let name = format!("system:{path}");
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(data));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.clone());
            }
        }
    }
    // The bundled face is a light weight, thin at these sizes. The system's reads better.
    #[cfg(windows)]
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional)
        && let Some(at) = list.iter().position(|f| f == "system:C:/Windows/Fonts/segoeui.ttf")
    {
        let face = list.remove(at);
        list.insert(0, face);
    }
    ctx.set_fonts(fonts);
}

/// A system font, mapped rather than read where that's safe, so only the parts in use take
/// memory: the fallback CJK fonts are 33 MB, on a machine that may never show a character
/// from them.
fn system_font(path: &str) -> Option<egui::FontData> {
    #[cfg(windows)]
    {
        let file = std::fs::File::open(path).ok()?;
        // Windows won't let a mapped file be changed or removed, so this can't vanish under
        // the program, as it could on other systems. It lives until the process ends.
        let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        Some(egui::FontData::from_static(Box::leak(Box::new(map))))
    }
    #[cfg(not(windows))]
    {
        std::fs::read(path).ok().map(egui::FontData::from_owned)
    }
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

/// The one height for buttons, menus, text boxes and icon buttons, so a row of them lines up.
pub const CONTROL_HEIGHT: f32 = 24.0;

pub fn apply(ctx: &egui::Context, p: &Palette, scale: f32, reduce_motion: bool) {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.panel2;
    v.code_bg_color = p.panel2;
    v.hyperlink_color = readable(p.accent, p.text, p.panel2, 4.5);
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
    // Being pressed, or having the keyboard. Selected and switched-on things use the
    // selection's fill, so this mustn't look like that: a thick outline says "here".
    w.active.bg_fill = p.button_hover;
    w.active.weak_bg_fill = p.button_hover;
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.bg_stroke = Stroke::new(2.0, p.accent);
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
    // Every control in a row has this height: buttons, menus, text boxes and icon buttons
    // alike. `widgets::small_button` and `widgets::input` keep to it too.
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    // Small things are fiddly with a mouse. Their targets reach further than they show.
    style.interaction.interact_radius = 6.0;
    if reduce_motion {
        style.animation_time = 0.0;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_colours_used_as_text_can_be_read_in_every_theme() {
        for t in ThemeChoice::ALL {
            let p = Palette::get(t);
            for i in 0..p.series.len() {
                let c = contrast(p.series_text(i), p.panel);
                assert!(c >= 4.5, "{} series {i}: {c:.2}", t.name());
            }
        }
    }

    #[test]
    fn the_colour_blind_palette_is_readable_and_has_no_red_or_green() {
        for t in ThemeChoice::ALL {
            let p = Palette::get(t).colorblind(true);
            assert!(contrast(p.add_fg, p.panel) >= 3.0, "{} add", t.name());
            assert!(contrast(p.del_fg, p.panel) >= 3.0, "{} del", t.name());
            assert!(p.add_fg.b() > p.add_fg.g() && p.add_fg.g() > p.add_fg.r(), "{} add is blue", t.name());
            assert!(p.del_fg.r() > p.del_fg.g() && p.del_fg.g() > p.del_fg.b(), "{} del is orange", t.name());
        }
    }

    #[test]
    fn the_operation_banner_is_opaque_and_readable_in_every_theme() {
        let mut report = Vec::new();
        let mut failures = Vec::new();
        for t in ThemeChoice::ALL {
            let p = Palette::get(t);
            let fill = p.banner_fill();
            assert_eq!(fill.a(), 255, "{}: a see-through fill shows the window's black clear colour", t.name());
            // WCAG AA for its text, and for the text of the buttons on it.
            let (text, button) = (contrast(p.text, fill), contrast(p.text, p.button));
            report.push(format!("{}: text {text:.2}, button {button:.2}", t.name()));
            if text < 4.5 || button < 4.5 {
                failures.push(report.last().unwrap().clone());
            }
        }
        println!("{}", report.join("\n"));
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn over_blends_like_the_screen() {
        assert_eq!(over(Color32::from_rgb(9, 8, 7), Color32::WHITE), Color32::from_rgb(9, 8, 7));
        assert_eq!(over(Color32::TRANSPARENT, Color32::from_rgb(9, 8, 7)), Color32::from_rgb(9, 8, 7));
        let half_white = Color32::WHITE.gamma_multiply(0.5);
        assert_eq!(over(half_white, Color32::BLACK).r(), half_white.r());
    }

    fn check(name: &str, p: &Palette, failures: &mut Vec<String>) {
        let mut need = |what: &str, a: Color32, b: Color32, min: f32| {
            let got = contrast(a, b);
            if got < min {
                failures.push(format!("{name}: {what} {got:.2} < {min}"));
            }
        };
        need("text on bg", p.text, p.bg, 7.0);
        need("text on panel", p.text, p.panel, 7.0);
        need("text on button", p.text, p.button, 4.5);
        need("text on selection", p.text, p.selection, 4.5);
        need("text on status bar", p.text, p.status_bg, 4.5);
        need("dim on panel", p.dim, p.panel, 4.5);
        need("faint on panel", p.faint, p.panel, 4.5);
        need("faint on a card", p.faint, p.panel2, 4.5);
        need("faint on a hovered row", p.faint, p.hover, 3.0);
        need("faint on a selected row", p.faint, p.selection, 3.0);
        need("danger on panel", p.danger, p.panel, 4.5);
        need("danger on a card", p.danger, p.panel2, 4.5);
        need("warning on panel", p.warn, p.panel, 4.5);
        need("good on panel", p.good, p.panel, 4.5);
        need("words on a Good button", ink_on(p.good), p.good, 4.5);
        need("words on a danger button", ink_on(p.danger), p.danger, 4.5);
        for (i, s) in p.series.iter().enumerate() {
            need(&format!("chip in series {i}"), chip_ink(*s, p.panel, p.text), over(s.gamma_multiply(0.14), p.panel), 4.5);
        }
        for t in p.seq {
            need("a label on a heat colour", ink_on(t), t, 4.5);
        }
        need("accent text on its button", p.on_accent, p.accent, 4.5);
        need("accent on panel", p.accent, p.panel, 3.0);
        need("accent on selection", p.accent, p.selection, 1.7);
    }

    #[test]
    fn every_theme_and_accent_stays_readable() {
        let mut failures = Vec::new();
        for t in ThemeChoice::ALL {
            check(t.name(), &Palette::get(t), &mut failures);
            for (name, _, _) in ACCENTS {
                check(&format!("{} + {name}", t.name()), &Palette::for_settings(t, name), &mut failures);
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn accent_settings() {
        assert_eq!(accent_color("#102030", true), Some(hex(0x102030)));
        assert_eq!(accent_color("#10203", true), None);
        assert_eq!(accent_color("#zzzzzz", true), None);
        assert_eq!(accent_color("", true), None);
        assert_eq!(accent_color("No such accent", false), None);
        assert_ne!(accent_color("Teal", true), accent_color("Teal", false), "tuned for each background");
        let own = Palette::get(ThemeChoice::Dark);
        assert_eq!(Palette::for_settings(ThemeChoice::Dark, "nonsense").accent, own.accent);
        assert_ne!(Palette::for_settings(ThemeChoice::Dark, "Teal").accent, own.accent);
    }

    #[test]
    fn themes_are_distinct_and_know_their_family() {
        let mut seen = std::collections::HashSet::new();
        for t in ThemeChoice::ALL {
            assert!(seen.insert(Palette::get(t).bg.to_array()), "{} shares a background", t.name());
        }
        let dark = ThemeChoice::ALL.iter().filter(|t| Palette::get(**t).dark).count();
        assert_eq!((dark, ThemeChoice::ALL.len() - dark), (7, 5));
    }
}
