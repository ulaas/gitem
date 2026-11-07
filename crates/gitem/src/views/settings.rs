//! Application settings. Edits go to a working copy and apply immediately through
//! `Action::Settings`, which also saves them, so there's no Save button.
use egui::{Align, Align2, CursorIcon, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};
use gitem_core::ai::AiProvider;
use gitem_core::editors::{self, Editor};
use gitem_core::forge::ForgeKind;
use gitem_core::runner::GitSource;
use gitem_core::settings::{Autolink, CustomHost, Settings, ThemeChoice};
use gitem_core::{ai, cred};

use crate::action::{Action, Cx};
use crate::chrome::icon;
use crate::forms::FormDialog;
use crate::task::Task;
use crate::theme::{ACCENTS, Palette, accent_color, contrast};
use crate::widgets::*;

const REVEAL: &str = "None: reveal in file manager";
const CUSTOM: &str = "Custom";
const NAV_WIDTH: f32 = 176.0;
const CONTENT_WIDTH: f32 = 720.0;
const ZOOMS: [(f32, &str); 7] = [(0.75, "75%"), (0.9, "90%"), (1.0, "100%"), (1.1, "110%"), (1.25, "125%"), (1.5, "150%"), (2.0, "200%")];

#[derive(PartialEq, Clone, Copy, Default)]
enum Page {
    #[default]
    Appearance,
    Git,
    Workflow,
    Blame,
    Hosting,
    Trackers,
    Ai,
    Agents,
}

impl Page {
    const ALL: [Page; 8] = [Page::Appearance, Page::Git, Page::Workflow, Page::Blame, Page::Hosting, Page::Trackers, Page::Ai, Page::Agents];

    fn glyph(self) -> &'static str {
        match self {
            Page::Appearance => icon::PALETTE,
            Page::Git => icon::TERMINAL_WINDOW,
            Page::Workflow => icon::GIT_COMMIT,
            Page::Blame => icon::USER_FOCUS,
            Page::Hosting => icon::CLOUD,
            Page::Trackers => icon::TICKET,
            Page::Ai => icon::SPARKLE,
            Page::Agents => icon::ROBOT,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Git => "Git",
            Page::Workflow => "Workflow",
            Page::Blame => "Blame",
            Page::Hosting => "Hosting",
            Page::Trackers => "Issue trackers",
            Page::Ai => "AI",
            Page::Agents => "Agents and links",
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Page::Appearance => "How gitem looks on this computer.",
            Page::Git => "Which git gitem runs, and the editor it opens files in.",
            Page::Workflow => "Defaults for committing, syncing, and starting work.",
            Page::Blame => "Who changed each line, shown as you read code.",
            Page::Hosting => "Self-hosted servers, and links from commit messages to anywhere else.",
            Page::Trackers => "Issues from these services appear in Launchpad.",
            Page::Ai => "AI features send the relevant diff to this provider, only when you ask. Files matched by .aiignore, .cursorignore, or .aiexclude are left out.",
            Page::Agents => "Coding agents working in your worktrees, and gitem:// links.",
        }
    }
}

#[derive(Default)]
pub struct SettingsView {
    page: Page,
    ai_test: Task<String>,
    git_version: Option<String>,
    editors: Task<Vec<Editor>>,
    editors_looked: bool,
    /// The editor setting last checked, and whether it can be started.
    editor_check: Option<(String, bool)>,
    git_pick: Task<Option<std::path::PathBuf>>,
    editor_pick: Task<Option<std::path::PathBuf>>,
    /// Whether Jira, Linear and Trello have a secret saved, and when that was last looked at.
    tracker_secrets: Task<[bool; 3]>,
    tracker_checked: Option<std::time::Instant>,
}

// ---- building blocks ---------------------------------------------------------

/// The rows of one card, with a hairline between them.
struct Group<'a> {
    ui: &'a mut Ui,
    p: &'a Palette,
    first: bool,
}

impl Group<'_> {
    fn divider(&mut self) {
        if !std::mem::replace(&mut self.first, false) {
            let (r, _) = self.ui.allocate_exact_size(vec2(self.ui.available_width(), 1.0), Sense::hover());
            self.ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, self.p.border));
        }
    }

    fn caption(ui: &mut Ui, width: f32, label: &str, hint: &str, p: &Palette) {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.0;
            // Lines a lone label up with a control beside it.
            if hint.is_empty() {
                ui.add_space(4.0);
            }
            ui.label(label);
            if !hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(hint).size(11.5).color(p.faint)).wrap());
            }
        });
    }

    /// Label and hint on the left, the control on the right. Controls are added from
    /// the right edge inwards.
    fn row(&mut self, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
        self.divider();
        let p = self.p;
        self.ui.add_space(8.0);
        let width = (self.ui.available_width() * 0.42).max(120.0);
        self.ui.horizontal(|ui| {
            Self::caption(ui, width, label, hint, p);
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
        self.ui.add_space(8.0);
    }

    /// Label above a control that needs the full width.
    fn stacked(&mut self, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
        self.divider();
        let p = self.p;
        self.ui.add_space(8.0);
        let width = self.ui.available_width();
        Self::caption(self.ui, width, label, hint, p);
        self.ui.add_space(5.0);
        control(self.ui);
        self.ui.add_space(8.0);
    }

    /// A row with a switch. The whole row is the click target.
    fn switch(&mut self, on: &mut bool, label: &str, hint: &str) {
        self.divider();
        let p = self.p;
        let ui = &mut *self.ui;
        ui.add_space(8.0);
        let width = (ui.available_width() - 56.0).max(120.0);
        let id = ui.id().with(("switch", label));
        let t = ui.ctx().animate_bool_responsive(id.with("anim"), *on);
        let row = ui.horizontal(|ui| {
            Self::caption(ui, width, label, hint, p);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (r, _) = ui.allocate_exact_size(vec2(34.0, 18.0), Sense::hover());
                let fill = if *on { p.accent } else { p.button };
                ui.painter().rect(r, 9.0, fill, Stroke::new(1.0, if *on { p.accent } else { p.button_border }), StrokeKind::Inside);
                let x = egui::lerp((r.left() + 9.0)..=(r.right() - 9.0), t);
                ui.painter().circle_filled(pos2(x, r.center().y), 6.5, if *on { p.on_accent } else { p.dim });
            });
        });
        let hit = row.response.rect.expand2(vec2(0.0, 8.0));
        let resp = ui.interact(hit, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, label));
        if resp.clicked() {
            *on = !*on;
        }
        ui.add_space(8.0);
    }
}

/// A titled card of rows.
fn group(ui: &mut Ui, title: &str, p: &Palette, body: impl FnOnce(&mut Group)) {
    ui.add_space(16.0);
    ui.label(RichText::new(title).size(13.0).strong().color(p.dim));
    ui.add_space(5.0);
    egui::Frame::new().fill(p.panel2).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(egui::Margin::symmetric(14, 2)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        body(&mut Group { ui, p, first: true });
    });
}

/// One choice out of a few, as a joined strip. Returns true when it changes.
fn segmented<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[(T, &str)], p: &Palette) -> bool {
    const PAD: f32 = 11.0;
    let galleys: Vec<_> = options.iter().map(|(_, t)| ui.painter().layout_no_wrap(t.to_string(), prop(12.5), p.text)).collect();
    let total: f32 = galleys.iter().map(|g| g.size().x + 2.0 * PAD).sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, 26.0), Sense::hover());
    ui.painter().rect(rect, 5.0, p.button, Stroke::new(1.0, p.button_border), StrokeKind::Inside);
    let mut changed = false;
    let mut x = rect.left();
    for (i, ((v, name), g)) in options.iter().zip(&galleys).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(g.size().x + 2.0 * PAD, rect.height()));
        x = r.right();
        let on = v == value;
        let resp = ui.interact(r, ui.id().with(("segment", *name)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, *name));
        if on {
            ui.painter().rect_filled(r.shrink(1.0), 4.0, p.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r.shrink(1.0), 4.0, p.button_hover);
        }
        if i > 0 && !on && options[i - 1].0 != *value {
            ui.painter().vline(r.left(), r.y_range().shrink(6.0), Stroke::new(1.0, p.button_border));
        }
        let color = if on { p.on_accent } else { p.text };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *name, prop(12.5), color);
        if resp.clicked() && !on {
            *value = v.clone();
            changed = true;
        }
    }
    changed
}

/// Themes as small pictures of the app, so you choose by looking.
fn theme_picker(ui: &mut Ui, value: &mut ThemeChoice, accent: &str, dark: bool, p: &Palette) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(12.0, 10.0);
        for choice in ThemeChoice::ALL {
            let t = Palette::for_settings(choice, accent);
            if t.dark != dark {
                continue;
            }
            let name = choice.name();
            let on = *value == choice;
            let (outer, resp) = ui.allocate_exact_size(vec2(112.0, 94.0), Sense::click());
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, format!("{name} theme")));
            if resp.clicked() {
                *value = choice;
            }
            let pic = Rect::from_min_size(outer.min, vec2(outer.width(), 72.0));
            let painter = ui.painter();
            painter.rect_filled(pic, 6.0, t.bg);
            // Rail, toolbar, and a little graph on the panel.
            let rail = Rect::from_min_size(pic.min + vec2(4.0, 11.0), vec2(9.0, pic.height() - 15.0));
            for (i, c) in [t.accent, t.series[2], t.series[3]].iter().enumerate() {
                painter.circle_filled(pos2(rail.center().x, rail.top() + 6.0 + i as f32 * 10.0), 2.0, *c);
            }
            let panel = Rect::from_min_max(pos2(rail.right() + 2.0, pic.top() + 11.0), pic.max - vec2(4.0, 4.0));
            painter.rect_filled(panel, 3.0, t.panel);
            painter.circle_filled(pic.min + vec2(9.0, 6.0), 2.0, t.accent);
            for (i, (lane, width)) in [(t.accent, 58.0), (t.series[1], 44.0), (t.accent, 66.0), (t.series[2], 36.0)].iter().enumerate() {
                let y = panel.top() + 9.0 + i as f32 * 12.0;
                painter.circle_filled(pos2(panel.left() + 8.0, y), 2.5, *lane);
                painter.rect_filled(Rect::from_min_size(pos2(panel.left() + 16.0, y - 1.5), vec2(*width, 3.0)), 1.0, if i == 0 { t.text } else { t.dim });
            }
            let border = if on { Stroke::new(2.0, p.accent) } else if resp.hovered() { Stroke::new(1.0, p.accent) } else { Stroke::new(1.0, p.button_border) };
            painter.rect_stroke(pic, 6.0, border, StrokeKind::Outside);
            let label = if on { p.text } else { p.dim };
            let text_y = pic.bottom() + 12.0;
            let mut x = outer.left() + 2.0;
            if on {
                let g = painter.text(pos2(x, text_y), Align2::LEFT_CENTER, icon::CHECK_CIRCLE, prop(13.0), p.accent);
                x = g.right() + 4.0;
            }
            painter.text(pos2(x, text_y), Align2::LEFT_CENTER, name, prop(12.5), label);
        }
    });
}

/// One round accent swatch. Returns true when clicked.
fn swatch(ui: &mut Ui, color: egui::Color32, name: &str, on: bool, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, name));
    let c = rect.center();
    if on || resp.hovered() {
        ui.painter().circle_stroke(c, 13.5, Stroke::new(if on { 2.0 } else { 1.0 }, if on { p.text } else { p.dim }));
    }
    ui.painter().circle_filled(c, 10.5, color);
    if on {
        let mark = [egui::Color32::WHITE, egui::Color32::BLACK].into_iter().max_by(|a, b| contrast(*a, color).total_cmp(&contrast(*b, color))).unwrap_or(egui::Color32::WHITE);
        ui.painter().text(c, Align2::CENTER_CENTER, icon::CHECK, prop(13.0), mark);
    }
    resp.on_hover_text(name).clicked()
}

/// The theme's own accent, every named one, and a custom colour.
fn accent_picker(ui: &mut Ui, accent: &mut String, theme: ThemeChoice, p: &Palette) {
    let own = Palette::get(theme).accent;
    let dark = p.dark;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
        if swatch(ui, own, "Theme default accent", accent.is_empty(), p) {
            accent.clear();
        }
        ui.add_space(8.0);
        for (name, _, _) in ACCENTS {
            let Some(color) = accent_color(name, dark) else { continue };
            if swatch(ui, color, &format!("{name} accent"), *accent == name, p) {
                *accent = name.to_string();
            }
        }
    });
}

/// A single-line field that won't push past its row.
fn field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> egui::Response {
    let w = width.min(ui.available_width());
    ui.add(input(text).hint_text(hint).desired_width(w))
}

fn wide_field(ui: &mut Ui, text: &mut String, hint: &str) -> egui::Response {
    ui.add(input(text).hint_text(hint).desired_width(f32::INFINITY))
}

fn add_button(ui: &mut Ui, label: &str, p: &Palette) -> bool {
    ui.add_space(8.0);
    let clicked = crate::chrome::icon_text_button(ui, icon::PLUS, p.accent, label, "", false, p).clicked();
    ui.add_space(10.0);
    clicked
}

fn note(ui: &mut Ui, text: &str, p: &Palette) {
    ui.add_space(8.0);
    ui.add(egui::Label::new(RichText::new(text).size(11.5).color(p.faint)).wrap());
    ui.add_space(8.0);
}

// ---- the view ----------------------------------------------------------------

impl SettingsView {
    /// Opens on a page by its name, for screenshots.
    pub fn on_page(name: &str) -> Self {
        Self { page: Page::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(name)).unwrap_or_default(), ..Self::default() }
    }

    pub fn key(&self) -> String {
        "settings".into()
    }
    pub fn title(&self) -> String {
        "Settings".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        // A fresh copy each frame: one kept for the tab's lifetime would overwrite everything
        // changed elsewhere since it was taken, as soon as anything here was edited.
        let mut s = cx.settings.clone();
        let before = serde_json::to_string(&s).unwrap_or_default();

        egui::Panel::left("settings-nav").resizable(false).exact_size(NAV_WIDTH).frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(8, 10))).show(ui, |ui| self.nav(ui, &p));

        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("settings").auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 24 }).show(ui, |ui| {
                    ui.set_max_width(CONTENT_WIDTH.min(ui.available_width()));
                    ui.label(RichText::new(self.page.name()).size(20.0).strong());
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(RichText::new(self.page.intro()).color(p.dim)).wrap());
                    match self.page {
                        Page::Appearance => self.appearance(ui, &mut s, &p),
                        Page::Git => self.git(ui, cx, &mut s, &p),
                        Page::Workflow => self.workflow(ui, &mut s, &p),
                        Page::Blame => self.blame(ui, &mut s, &p),
                        Page::Hosting => self.hosting(ui, &mut s, &p),
                        Page::Trackers => self.trackers(ui, cx, &mut s, &p),
                        Page::Ai => self.ai(ui, cx, &mut s, &p),
                        Page::Agents => self.agents(ui, cx, &mut s, &p),
                    }
                });
            });
        });

        if serde_json::to_string(&s).unwrap_or_default() != before {
            let c = s.clone();
            cx.out.push(Action::Settings(Box::new(move |t| *t = c)));
        }
    }

    fn nav(&mut self, ui: &mut Ui, p: &Palette) {
        ui.spacing_mut().item_spacing.y = 2.0;
        for page in Page::ALL {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
            resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, self.page == page, page.name()));
            let on = self.page == page;
            if on {
                ui.painter().rect_filled(rect, 6.0, p.selection);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 6.0, p.hover);
            }
            let color = if on || resp.hovered() { p.text } else { p.dim };
            ui.painter().text(pos2(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, page.glyph(), prop(16.0), color);
            ui.painter().text(pos2(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, page.name(), prop(13.0), color);
            if resp.clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add(egui::Label::new(RichText::new("Changes save automatically.").size(11.5).color(p.faint)).wrap());
        });
    }

    fn appearance(&mut self, ui: &mut Ui, s: &mut Settings, p: &Palette) {
        group(ui, "Theme", p, |g| {
            g.stacked("Dark", "", |ui| theme_picker(ui, &mut s.theme, &s.accent, true, p));
            g.stacked("Light", "", |ui| theme_picker(ui, &mut s.theme, &s.accent, false, p));
        });
        group(ui, "Accent color", p, |g| {
            g.stacked("Accent", "Buttons, selections, and highlights. Named colors are tuned for dark and light themes.", |ui| accent_picker(ui, &mut s.accent, s.theme, p));
            g.row("Custom color", "Used as is in every theme.", |ui| {
                let mut color = p.accent;
                if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque).changed() {
                    s.accent = format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b());
                }
            });
        });
        group(ui, "Text and size", p, |g| {
            g.row("Interface zoom", "", |ui| {
                let options: Vec<(u32, &str)> = ZOOMS.iter().map(|(z, t)| ((z * 100.0).round() as u32, *t)).collect();
                let mut current = (s.ui_scale * 100.0).round() as u32;
                if segmented(ui, &mut current, &options, p) {
                    s.ui_scale = current as f32 / 100.0;
                }
            });
            g.row("Code font size", "", |ui| {
                ui.add(egui::Slider::new(&mut s.editor_font_size, 9.0..=22.0).step_by(1.0).suffix(" pt"));
            });
            g.switch(&mut s.colorblind_diff, "Colour-blind safe diffs", "Blue and orange in place of green and red.");
            g.switch(&mut s.reduce_motion, "Reduce motion", "No spinning or animated transitions.");
            g.switch(&mut s.rail_full, "All views on the rail", "Every view gets an icon of its own, in place of the More list.");
            g.switch(&mut s.rail_labels, "Names on the rail", "A wider rail, with each view's name beside its icon.");
            g.switch(&mut s.details_on_open, "Show details on opening the graph", "Opens the Details panel on the current commit.");
            // Half-translated menus look broken, so this isn't offered until asked for.
            let showing = s.show_experimental || s.language != gitem_core::settings::Settings::default().language;
            g.switch(&mut s.show_experimental, "Show experimental settings", "");
            if showing {
                g.row("Language", "Experimental and partial: the rail, some toolbar buttons, and some menu items.", |ui| {
                    let options: Vec<(String, &str)> = crate::i18n::LANGUAGES.iter().map(|(code, name)| (code.to_string(), *name)).collect();
                    segmented(ui, &mut s.language, &options, p);
                });
            }
        });
        group(ui, "Commits", p, |g| {
            g.switch(&mut s.date_relative, "Relative dates", "\"3 hours ago\" instead of a date.");
            g.switch(&mut s.gravatar, "Gravatar avatars", "Sends a hash of each author's email to gravatar.com. Otherwise initials are used.");
        });
    }

    fn git(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings, p: &Palette) {
        if self.git_pick.poll()
            && let Some(Some(f)) = self.git_pick.value.take()
        {
            s.git_path = f.to_string_lossy().into_owned();
        }
        if self.editor_pick.poll()
            && let Some(Some(f)) = self.editor_pick.value.take()
        {
            s.default_external_editor = f.to_string_lossy().into_owned();
        }
        group(ui, "Git executable", p, |g| {
            g.row("In use", "", |ui| {
                let source = match cx.repo.git.source {
                    GitSource::Configured => "from your setting",
                    GitSource::Embedded => "beside gitem",
                    GitSource::System => "from your system",
                };
                let version = self.git_version.get_or_insert_with(|| cx.repo.git.version()).trim().to_string();
                ui.label(RichText::new(format!("{version}, {source}")).color(p.dim));
            });
            g.stacked("Custom path", "Leave blank to search: a git folder beside gitem, then PATH, then the usual install folders. Restart to apply.", |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Browse…").clicked() {
                            crate::widgets::pick(&mut self.git_pick, ui.ctx(), rfd::FileDialog::new(), false);
                        }
                        ui.add(input(&mut s.git_path).hint_text(cx.repo.git.path.display().to_string()).desired_width(f32::INFINITY));
                    });
                });
            });
        });
        self.editors.poll();
        if !std::mem::replace(&mut self.editors_looked, true) {
            self.editors.spawn(ui.ctx(), || -> Result<Vec<Editor>, String> { Ok(editors::detect()) });
        }
        let found: Vec<Editor> = self.editors.get().cloned().unwrap_or_default();
        let current = s.default_external_editor.trim().to_string();
        if self.editor_check.as_ref().is_none_or(|(c, _)| *c != current) {
            let ok = current.is_empty() || editors::runnable(&editors::resolve(&current));
            self.editor_check = Some((current.clone(), ok));
        }
        let usable = self.editor_check.as_ref().is_none_or(|c| c.1);
        group(ui, "External editor", p, |g| {
            g.row("Editor", "Where \"Open in external editor\" opens files.", |ui| {
                let selected = if current.is_empty() { REVEAL } else { editors::matching(&current, &found).map_or(CUSTOM, |e| e.name) };
                egui::ComboBox::from_id_salt("external-editor").selected_text(selected).width(260.0).show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_empty(), REVEAL).clicked() {
                        s.default_external_editor.clear();
                    }
                    for e in &found {
                        if ui.selectable_label(selected == e.name, e.name).clicked() {
                            s.default_external_editor = e.path.to_string_lossy().into_owned();
                        }
                    }
                    if self.editors.busy() {
                        ui.label(RichText::new("Looking for editors…").color(p.faint));
                    }
                });
            });
            g.stacked("Path", "A program, or a command on your PATH. Choosing an editor above fills this in.", |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Browse…").clicked() {
                            let mut dialog = rfd::FileDialog::new().set_title("Choose an editor");
                            if cfg!(windows) {
                                dialog = dialog.add_filter("Programs", &["exe", "cmd", "bat"]);
                            }
                            crate::widgets::pick(&mut self.editor_pick, ui.ctx(), dialog, false);
                        }
                        ui.add(input(&mut s.default_external_editor).hint_text("Reveal files in the file manager").desired_width(f32::INFINITY));
                    });
                });
                if !usable {
                    ui.add_space(5.0);
                    ui.label(RichText::new(format!("{} This program can't be found, so opening files will fail.", icon::WARNING)).color(p.warn));
                }
            });
        });
        group(ui, "Files", p, |g| {
            g.stacked("Settings file", "", |ui| {
                ui.add(egui::Label::new(RichText::new(Settings::path().display().to_string()).monospace().size(11.5).color(p.dim)).wrap());
                ui.horizontal(|ui| {
                    if ui.small_button("Export…").on_hover_text("A copy for another machine. Tokens and passwords aren't in it").clicked() {
                        cx.form(export_settings_form(s.clone()));
                    }
                    if ui.small_button("Import…").on_hover_text("Takes on another copy's settings, and keeps what's open and recent here").clicked() {
                        cx.form(import_settings_form());
                    }
                });
            });
            g.row("Reset preferences", "Back to how gitem came. Open and recent repositories, workspaces, favorites, and saved secrets stay.", |ui| {
                if ui.button("Reset…").clicked() {
                    cx.form(FormDialog::new("Reset preferences", "Reset").danger().info("Put every preference back to how gitem came? That includes the theme, AI provider, self-hosted servers, and autolinks. Open and recent repositories, workspaces, favorites, and saved secrets stay.").on_submit(|_| Ok(vec![Action::Settings(Box::new(|s| *s = s.with_default_preferences()))])));
                }
            });
        });
    }

    fn workflow(&mut self, ui: &mut Ui, s: &mut Settings, p: &Palette) {
        group(ui, "Sync", p, |g| {
            g.row("Pull button", "How the toolbar's Pull integrates commits.", |ui| {
                let options = [("default".to_string(), "Git default"), ("ff-only".to_string(), "Fast-forward"), ("rebase".to_string(), "Rebase"), ("merge".to_string(), "Merge")];
                segmented(ui, &mut s.pull_mode, &options, p);
            });
            g.row("Fetch automatically", "Keeps incoming commits up to date. 0 turns it off.", |ui| {
                ui.label("minutes");
                ui.add(egui::DragValue::new(&mut s.auto_fetch_minutes).range(0..=240));
            });
            g.switch(&mut s.prune_on_fetch, "Prune when fetching", "Removes remote branches that were deleted on the server.");
            g.switch(&mut s.status_bar_pr, "Show the branch's pull request in the status bar", "Asks the host when the branch changes, whether Launchpad is open or not.");
            g.switch(&mut s.pull_autostash, "Stash changes around a pull", "The toolbar's Pull puts uncommitted changes aside and brings them back after.");
            g.switch(&mut s.recurse_submodules, "Bring submodules along", "Pull and check out also update submodules, as --recurse-submodules does.");
            g.switch(&mut s.attention_when_done, "Flash the taskbar when something's done", "When something that took a while finishes, or anything fails, while you're in another window.");
            g.switch(&mut s.notify_when_done, "Show a system notification too", "On Windows, at the same moments: what it was and whether it worked, with none of git's output. Nothing is watched or asked of any host for this.");
        });
        group(ui, "Commits", p, |g| {
            g.switch(&mut s.commit_sign, "Sign commits", "On by default in the commit box.");
            g.switch(&mut s.commit_signoff, "Add Signed-off-by", "On by default in the commit box.");
            g.row("Start Work branch name", "Tokens: ${id} ${title}", |ui| {
                field(ui, &mut s.start_work_format, "${id}-${title}", 240.0);
            });
        });
        group(ui, "Keyboard", p, |g| {
            for (name, keys, what) in crate::keys::BINDABLE {
                g.row(what, keys, |ui| {
                    let mut text = s.key_bindings.get(*name).cloned().unwrap_or_default();
                    if field(ui, &mut text, keys, 160.0).changed() {
                        if text.trim().is_empty() {
                            s.key_bindings.remove(*name);
                        } else {
                            s.key_bindings.insert(name.to_string(), text);
                        }
                    }
                });
            }
        });
        group(ui, "Keyboard, in a view", p, |g| {
            for c in crate::keys::CONTEXT {
                g.row(c.what, c.default, |ui| {
                    let mut text = s.key_bindings.get(c.name).cloned().unwrap_or_default();
                    if field(ui, &mut text, c.default, 160.0).changed() {
                        if text.trim().is_empty() {
                            s.key_bindings.remove(c.name);
                        } else {
                            s.key_bindings.insert(c.name.to_string(), text);
                        }
                    }
                });
            }
        });
        for problem in crate::keys::binding_problems(&s.key_bindings) {
            ui.colored_label(p.warn, problem);
        }
        note(ui, crate::keys::FIXED_KEYS, p);
        group(ui, "Launching", p, |g| {
            g.switch(&mut s.single_window, "Use one window", "Opening a folder from a terminal or Explorer goes to the window already open, once it's been restarted with this on.");
        });
        group(ui, "Updates", p, |g| {
            g.switch(&mut s.check_updates, "Check for updates", "Asks once a day whether there's a newer release, and tells you. Nothing is downloaded. About has a button for asking now.");
            g.row("Where to ask", "A releases/latest address in GitHub's format.", |ui| {
                field(ui, &mut s.update_url, "https://…/releases/latest", 320.0);
            });
        });
        group(ui, "Safety and speed", p, |g| {
            g.switch(&mut s.confirm_destructive, "Ask before destructive commands", "Such as deleting a branch or discarding changes.");
            g.switch(&mut s.auto_maintenance, "Run maintenance when opening a repository", "Git's safe upkeep, in the background.");
            g.row("Graph page size", "Commits loaded at a time.", |ui| {
                ui.add(egui::DragValue::new(&mut s.graph_page_size).range(100..=50000).speed(100));
            });
        });
    }

    fn blame(&mut self, ui: &mut Ui, s: &mut Settings, p: &Palette) {
        group(ui, "In the editor", p, |g| {
            g.switch(&mut s.inline_blame, "Blame at the end of the current line", "");
            g.switch(&mut s.code_lens, "Recent-change lens above symbols", "");
        });
        group(ui, "Format", p, |g| {
            g.stacked("Inline and status bar", "Tokens: {author} {ago} {date} {subject} {sha}", |ui| {
                wide_field(ui, &mut s.inline_blame_format, "");
            });
        });
    }

    fn hosting(&mut self, ui: &mut Ui, s: &mut Settings, p: &Palette) {
        group(ui, "Self-hosted servers", p, |g| {
            let problem = gitem_core::forge::ca_file_problem();
            g.row("Certificate authorities", "A PEM file, for a server whose authority isn't in the system's store. It replaces the store for hosting and AI requests, so add public roots to it if you also use public hosts. Git's own connections go by git's settings.", |ui| {
                field(ui, &mut s.ca_file, "The system's store", 360.0);
            });
            if let Some(why) = problem {
                note(g.ui, &format!("Using the system's store: {why}"), p);
            }
            g.divider();
            note(g.ui, "github.com, gitlab.com, bitbucket.org, dev.azure.com, and codeberg.org work automatically. Tokens are managed in Credentials.", p);
            let mut remove = None;
            for (i, h) in s.custom_hosts.iter_mut().enumerate() {
                g.first = false;
                g.divider();
                g.ui.add_space(8.0);
                g.ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if crate::chrome::icon_button(ui, icon::TRASH, "Remove server", "", false, p).clicked() {
                            remove = Some(i);
                        }
                        egui::ComboBox::from_id_salt(("kind", i)).selected_text(h.kind.label()).width(130.0).show_ui(ui, |ui| {
                            for k in ForgeKind::ALL {
                                ui.selectable_value(&mut h.kind, k, k.label());
                            }
                        });
                        let half = (ui.available_width() - 8.0) / 2.0;
                        ui.add(input(&mut h.api_base).hint_text("API URL (optional)").desired_width(half));
                        ui.add(input(&mut h.host).hint_text("git.example.com").desired_width(half));
                    });
                });
                g.ui.add_space(4.0);
                egui::CollapsingHeader::new(RichText::new("Custom links").color(p.dim)).id_salt(("links", i)).show(g.ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.add(egui::Label::new(RichText::new("Optional. Tokens: ${repo} ${sha} ${file} ${line} ${branch} ${base} ${head}").size(11.5).color(p.faint)).wrap());
                    wide_field(ui, &mut h.templates.commit, "Commit link");
                    wide_field(ui, &mut h.templates.file, "File link");
                    wide_field(ui, &mut h.templates.branch, "Branch link");
                    wide_field(ui, &mut h.templates.compare, "Compare link");
                });
                g.ui.add_space(6.0);
            }
            if let Some(i) = remove {
                s.custom_hosts.remove(i);
            }
            g.first = false;
            g.divider();
            if add_button(g.ui, "Add server", p) {
                s.custom_hosts.push(CustomHost { host: String::new(), kind: ForgeKind::GitLab, api_base: String::new(), templates: Default::default() });
            }
        });
        group(ui, "Autolinks", p, |g| {
            note(g.ui, "Turn references in commit messages into links. Use <num> in the URL for the matched id.", p);
            let mut remove = None;
            for (i, a) in s.autolinks.iter_mut().enumerate() {
                g.first = false;
                g.divider();
                g.ui.add_space(6.0);
                g.ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if crate::chrome::icon_button(ui, icon::TRASH, "Remove autolink", "", false, p).clicked() {
                            remove = Some(i);
                        }
                        ui.checkbox(&mut a.alphanumeric, "Letters too");
                        let w = ui.available_width() - 100.0;
                        ui.add(input(&mut a.url).hint_text("https://example.com/browse/JIRA-<num>").desired_width(w));
                        ui.add(input(&mut a.prefix).hint_text("JIRA-").desired_width(f32::INFINITY));
                    });
                });
                g.ui.add_space(6.0);
            }
            if let Some(i) = remove {
                s.autolinks.remove(i);
            }
            g.first = false;
            g.divider();
            if add_button(g.ui, "Add autolink", p) {
                s.autolinks.push(Autolink { prefix: String::new(), url: String::new(), alphanumeric: false });
            }
        });
    }

    fn trackers(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings, p: &Palette) {
        // Saving one happens in a dialog, so this looks again every so often while it's open.
        self.tracker_secrets.poll();
        if self.tracker_checked.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(3)) && !self.tracker_secrets.busy() {
            self.tracker_checked = Some(std::time::Instant::now());
            self.tracker_secrets.spawn(ui.ctx(), || -> Result<[bool; 3], String> {
                let saved = |name: &str| cred::get(&format!("{}{name}", cred::PREFIX_FORGE)).ok().flatten().is_some();
                Ok([saved("jira"), saved("linear"), saved("trello")])
            });
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        let saved = self.tracker_secrets.get().copied();
        let note = |i: usize| match saved.map(|s| s[i]) {
            Some(true) => "Saved in the system's credential store.",
            Some(false) => "Not set. Kept in the system's credential store.",
            None => "Kept in the system's credential store.",
        };
        let t = &mut s.issue_trackers;
        group(ui, "Jira", p, |g| {
            g.row("Site", "", |ui| {
                field(ui, &mut t.jira_base_url, "https://you.atlassian.net", 300.0);
            });
            g.row("Account email", "", |ui| {
                field(ui, &mut t.jira_email, "you@example.com", 300.0);
            });
            g.stacked("Search", "JQL. By default: unresolved, assigned to you.", |ui| {
                wide_field(ui, &mut t.jira_jql, "assignee = currentUser() AND resolution = Unresolved");
            });
            g.switch(&mut t.jira_server, "Self-hosted", "Jira Data Center or Server.");
            g.row("API token", note(0), |ui| {
                if ui.button("Set token…").clicked() {
                    cx.form(secret_form("Jira API token", "jira"));
                }
            });
        });
        group(ui, "Linear", p, |g| {
            g.switch(&mut t.linear_enabled, "Show assigned issues", "");
            g.row("API key", note(1), |ui| {
                if ui.button("Set key…").clicked() {
                    cx.form(secret_form("Linear API key", "linear"));
                }
            });
        });
        group(ui, "Trello", p, |g| {
            g.switch(&mut t.trello_enabled, "Show my cards", "");
            g.row("Credential", if saved.is_some_and(|s| s[2]) { "Saved in the system's credential store." } else { "Your API key and token, written as apikey:token." }, |ui| {
                if ui.button("Set credential…").clicked() {
                    cx.form(secret_form("Trello credential", "trello"));
                }
            });
        });
    }

    fn ai(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings, p: &Palette) {
        self.ai_test.poll();
        let provider = s.ai.provider;
        let has = cred::ai_key(provider.key_name()).is_some();
        group(ui, "Provider", p, |g| {
            g.row("Service", "", |ui| {
                egui::ComboBox::from_id_salt("ai-provider").selected_text(s.ai.provider.label()).width(260.0).show_ui(ui, |ui| {
                    for a in AiProvider::ALL {
                        ui.selectable_value(&mut s.ai.provider, a, a.label());
                    }
                });
            });
            // Only about the default, so it goes once a model is named.
            let about_default = s.ai.model.trim().is_empty().then(|| s.ai.provider.default_model_note(gitem_core::util::now_ts())).flatten();
            g.row("Model", about_default.as_deref().unwrap_or(""), |ui| {
                let hint = s.ai.provider.default_model();
                field(ui, &mut s.ai.model, hint, 260.0);
            });
            g.row("API URL", "Leave blank for the provider's default.", |ui| {
                field(ui, &mut s.ai.base_url, "Default", 260.0);
            });
            let status = if has { "Saved in the system's credential store." } else if provider.needs_key() { "Needed for this provider." } else { "Optional for this provider." };
            g.row("API key", status, |ui| {
                if ui.button(if has { "Replace…" } else { "Set key…" }).clicked() {
                    let name = provider.key_name();
                    cx.form(FormDialog::new(format!("{} API key", provider.label()), "Save").password("key", "Key").on_submit(move |v| {
                        let k = v.opt("key").ok_or("A key is required.")?;
                        cred::set_ai_key(name, &k).map_err(|e| e.to_string())?;
                        Ok(vec![Action::Toast("API key saved".into())])
                    }));
                }
                if has && ui.button("Remove").clicked() {
                    let _ = cred::delete(&format!("{}{}", cred::PREFIX_AI, provider.key_name()));
                }
            });
            let mut test = false;
            g.row("Connection", "Sends a one-word request.", |ui| {
                test = ui.add_enabled(!self.ai_test.busy(), egui::Button::new("Test connection")).on_disabled_hover_text("Already testing it.").clicked();
                if self.ai_test.busy() {
                    ui.add(spinner(14.0));
                }
            });
            if test {
                let cfg = s.ai.clone();
                self.ai_test.spawn(g.ui.ctx(), move || -> Result<String, String> {
                    let k = cred::ai_key(cfg.provider.key_name());
                    ai::complete(&cfg, k.as_deref(), "Reply with the single word: ready", "Are you there?").map_err(|e| e.to_string())
                });
            }
            if let Some(e) = &self.ai_test.error {
                error_box(g.ui, e, p);
                g.ui.add_space(8.0);
            } else if let Some(t) = self.ai_test.get() {
                g.ui.label(RichText::new(format!("Connected. The reply was \"{}\".", t.trim())).color(p.good));
                g.ui.add_space(8.0);
            }
        });
        group(ui, "Instructions", p, |g| {
            g.stacked("Reviews and conflict resolution", "Added to those requests.", |ui| {
                ui.add(text_area(&mut s.ai.custom_instructions).desired_rows(3).desired_width(f32::INFINITY));
            });
            g.stacked("Commit message conventions", "", |ui| {
                ui.add(text_area(&mut s.ai.commit_style).hint_text("e.g. Conventional Commits: feat:, fix:, chore:").desired_rows(3).desired_width(f32::INFINITY));
            });
        });
    }

    fn agents(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings, p: &Palette) {
        group(ui, "Worktree commands", p, |g| {
            g.row("Agent command", "Run in a new terminal by Start agent session.", |ui| {
                field(ui, &mut s.agent_command, "claude", 240.0);
            });
            g.row("Worktree task", "Run in a new terminal by Run task on worktree.", |ui| {
                field(ui, &mut s.worktree_task, "npm test", 240.0);
            });
            g.row("Terminal", "cmd, powershell, pwsh, wt or git-bash on Windows, or a program and its arguments. Empty is the system's plain one.", |ui| {
                field(ui, &mut s.terminal, "wt", 240.0);
            });
        });
        group(ui, "Your own tools", p, |g| {
            g.row("Diff tool", "A name git knows, for Open in external diff tool. Empty means git's diff.tool.", |ui| {
                field(ui, &mut s.external_diff_tool, "bc", 240.0);
            });
            g.row("Merge tool", "A name git knows, for Open in external merge tool. Empty means git's merge.tool.", |ui| {
                field(ui, &mut s.external_merge_tool, "bc", 240.0);
            });
        });
        let exe = std::env::current_exe().map(|e| e.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let mcp = format!("claude mcp add gitem -- \"{exe}\" --mcp");
        group(ui, "MCP server", p, |g| {
            g.stacked("Add to Claude Code", "Lets agents query this repository through gitem, read-only. Run this once.", |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Copy").clicked() {
                            cx.copy(mcp.clone());
                        }
                        ui.add(egui::Label::new(RichText::new(&mcp).monospace().size(11.5).color(p.dim)).truncate());
                    });
                });
            });
        });
        if cfg!(windows) {
            group(ui, "Explorer", p, |g| {
                g.row("Open in gitem", "Adds an entry to folders' right-click menu in Explorer, for you only.", |ui| {
                    if ui.button("Add").clicked() {
                        match run_reg(&explorer_entries(&exe)) {
                            Ok(()) => cx.toast("Folders now have Open in gitem"),
                            Err(e) => cx.error(format!("Could not add it: {e}")),
                        }
                    }
                    if ui.button("Remove").clicked() {
                        match run_reg(&removal_entries(false)) {
                            Ok(()) => cx.toast("Open in gitem is gone from Explorer"),
                            Err(e) => cx.error(format!("Could not remove it: {e}")),
                        }
                    }
                });
            });
            group(ui, "gitem:// links", p, |g| {
                g.row("Link handler", "Makes \"Copy gitem link\" URLs open here. Adds a per-user registry entry.", |ui| {
                    if ui.button("Unregister").clicked() {
                        match run_reg(&removal_entries(true)) {
                            Ok(()) => cx.toast("gitem:// links no longer open here"),
                            Err(e) => cx.error(format!("Could not remove it: {e}")),
                        }
                    }
                    if ui.button("Register").clicked() {
                        match register_links(&exe) {
                            Ok(()) => cx.toast("gitem:// links now open here"),
                            Err(e) => cx.error(e),
                        }
                    }
                });
            });
        }
    }
}

fn secret_form(title: &str, host: &'static str) -> FormDialog {
    FormDialog::new(title, "Save").password("key", "Value").on_submit(move |v| {
        let k = v.opt("key").ok_or("A value is required.")?;
        cred::set_forge_token(host, "", &k).map_err(|e| e.to_string())?;
        Ok(vec![Action::Toast("Saved".into())])
    })
}

/// Register the `gitem://` scheme for this user only, without a shell.
pub fn export_settings_form(current: Settings) -> crate::forms::FormDialog {
    crate::forms::FormDialog::new("Export settings", "Export").new_file("path", "Save as", "").on_submit(move |v| {
        let path = v.opt("path").ok_or("Choose where to save it.")?;
        current.export_to(std::path::Path::new(&path)).map_err(|e| format!("Could not write {path}: {e}"))?;
        Ok(vec![crate::action::Action::Toast(format!("Settings saved to {path}"))])
    })
}

pub fn import_settings_form() -> crate::forms::FormDialog {
    crate::forms::FormDialog::new("Import settings", "Import").danger().info("Replaces these settings with what the file says. What's open and recent here stays.").path("path", "File", "", false).on_submit(|v| {
        let path = v.opt("path").ok_or("Choose a file.")?;
        // Read here, so that a bad file is said in the form with nothing changed.
        Settings::default().import_from(std::path::Path::new(&path))?;
        Ok(vec![
            crate::action::Action::Settings(Box::new(move |s| {
                let _ = s.import_from(std::path::Path::new(&path));
            })),
            crate::action::Action::Toast("Settings imported".to_string()),
        ])
    })
}

/// What `reg` is run with to add "Open in gitem" to folders in Explorer, for this user.
pub fn explorer_entries(exe: &str) -> Vec<Vec<String>> {
    let exe = exe.replace('/', "\\");
    let mut runs = Vec::new();
    for (key, arg) in [(r"HKCU\Software\Classes\Directory\shell\gitem", "%1"), (r"HKCU\Software\Classes\Directory\Background\shell\gitem", "%V")] {
        let run = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        runs.push(run(&["add", key, "/ve", "/d", "Open in gitem", "/f"]));
        runs.push(run(&["add", key, "/v", "Icon", "/d", &exe, "/f"]));
        runs.push(run(&["add", &format!(r"{key}\command"), "/ve", "/d", &format!("\"{exe}\" \"{arg}\""), "/f"]));
    }
    runs
}

/// And what takes that back, and the link handler if that's what's named.
pub fn removal_entries(links: bool) -> Vec<Vec<String>> {
    let keys: &[&str] = if links { &[r"HKCU\Software\Classes\gitem"] } else { &[r"HKCU\Software\Classes\Directory\shell\gitem", r"HKCU\Software\Classes\Directory\Background\shell\gitem"] };
    keys.iter().map(|k| vec!["delete".to_string(), k.to_string(), "/f".to_string()]).collect()
}

fn run_reg(runs: &[Vec<String>]) -> Result<(), String> {
    for args in runs {
        let o = std::process::Command::new("reg").args(args).output().map_err(|e| format!("Could not run reg: {e}"))?;
        // Taking away what isn't there is what was wanted.
        if !o.status.success() && args.first().is_none_or(|a| a != "delete") {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
    }
    Ok(())
}

pub fn register_links(exe: &str) -> Result<(), String> {
    let exe = exe.replace('/', "\\");
    let root = r"HKCU\Software\Classes\gitem";
    let open_key = format!(r"{root}\shell\open\command");
    let open_cmd = format!("\"{exe}\" \"%1\"");
    let runs: [Vec<&str>; 3] = [vec!["add", root, "/ve", "/d", "URL:gitem", "/f"], vec!["add", root, "/v", "URL Protocol", "/d", "", "/f"], vec!["add", &open_key, "/ve", "/d", &open_cmd, "/f"]];
    for args in runs {
        let o = std::process::Command::new("reg").args(&args).output().map_err(|e| format!("Could not run reg: {e}"))?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod shell_tests {
    use super::*;

    /// What would be written, without writing it: this is the test machine's registry otherwise.
    #[test]
    fn explorer_entries_stay_in_this_users_classes_and_quote_the_program() {
        let runs = explorer_entries("C:/Program Files/gitem/gitem.exe");
        assert_eq!(runs.len(), 6);
        for r in &runs {
            assert_eq!(r[0], "add");
            assert!(r[1].starts_with(r"HKCU\Software\Classes\Directory\"), "{r:?}");
        }
        assert!(runs.iter().any(|r| r.iter().any(|a| a == "\"C:\\Program Files\\gitem\\gitem.exe\" \"%1\"")), "{runs:?}");
        assert!(runs.iter().any(|r| r.iter().any(|a| a == "\"C:\\Program Files\\gitem\\gitem.exe\" \"%V\"")), "{runs:?}");
    }

    #[test]
    fn removal_names_only_what_was_added() {
        assert_eq!(removal_entries(true), vec![vec!["delete".to_string(), r"HKCU\Software\Classes\gitem".to_string(), "/f".to_string()]]);
        for r in removal_entries(false) {
            assert!(r[1].ends_with(r"\shell\gitem"), "{r:?}");
        }
    }
}
