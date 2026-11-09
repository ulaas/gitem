//! Home: status of this repository, start-work and review shortcuts, recent
//! branches, git's own settings for the whole machine, and multi-repository
//! workspaces with batch actions.
use std::collections::HashMap;

use egui::{Align, Align2, CursorIcon, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};
use gitem_core::model::*;
use gitem_core::settings::Workspace;
use gitem_core::{Repo, ops};

use crate::action::{Action, Cx};
use crate::chrome::icon;
use crate::forms::{self, FormDialog};
use crate::menus;
use crate::task::Task;
use crate::theme::Palette;
use crate::views::Tab;
use crate::views::settings::{Group, segmented};
use crate::widgets::*;

/// Widest the page gets: past this, lines are too long to read and cards too wide to scan.
const MAX_WIDTH: f32 = 1120.0;
/// Below this, cards stack instead of standing side by side.
const TWO_COLUMNS: f32 = 720.0;

#[derive(Clone, Default)]
struct RepoSummary {
    path: String,
    name: String,
    branch: String,
    changes: usize,
    ahead: u32,
    behind: u32,
    error: Option<String>,
}

/// Git's config as it stands for this machine and for this repository.
#[derive(Clone, Default)]
struct GitSetup {
    /// What system and global config say, global winning: what every repository starts with.
    machine: HashMap<String, String>,
    /// What this repository says for itself, which wins over the machine's.
    here: HashMap<String, String>,
}

impl GitSetup {
    fn from(list: Vec<ConfigEntry>) -> Self {
        let mut s = Self::default();
        // Keys are case-insensitive in their section and name; git lists them lower-cased.
        for e in list {
            let key = e.key.to_lowercase();
            match e.scope.as_str() {
                "system" | "global" => {
                    s.machine.insert(key, e.value);
                }
                "local" | "worktree" => {
                    s.here.insert(key, e.value);
                }
                _ => {}
            }
        }
        s
    }
}

fn truthy(v: Option<&str>) -> bool {
    v.is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1"))
}

#[derive(Clone, Copy, PartialEq)]
enum PullWay {
    Merge,
    Rebase,
    FastForward,
    Unset,
}

/// What git does on pull, from `pull.ff` and `pull.rebase`. Unset is git's own default,
/// which since 2.33 refuses to pull a branch that has gone two ways.
fn pull_way(ff: Option<&str>, rebase: Option<&str>) -> PullWay {
    if ff.is_some_and(|v| v.eq_ignore_ascii_case("only")) {
        return PullWay::FastForward;
    }
    match rebase.map(str::to_lowercase).as_deref() {
        Some("false" | "no" | "off" | "0") => PullWay::Merge,
        Some(_) => PullWay::Rebase,
        None => PullWay::Unset,
    }
}

/// What a click asked for, done once the frame's drawing is over.
enum Ask {
    StartWork,
    StartKind(gitem_core::branchmodel::Kind),
    PullRequests,
    Compare,
    Stash,
    Open(fn() -> Tab),
}

#[derive(Default)]
pub struct HomeView {
    summaries: Task<Vec<RepoSummary>>,
    summaries_for: Option<String>,
    workspace_idx: usize,
    scan_pick: Task<Option<std::path::PathBuf>>,
    recent: Task<Vec<Commit>>,
    recent_gen: Option<u64>,
    /// Fetch all or Pull all, going through a workspace's repositories.
    sweep: Task<String>,
    /// How many of those have finished: the list of them is out of date after each.
    swept: u64,
    setup: Task<GitSetup>,
    setup_gen: Option<u64>,
    /// Values just set, shown until a read from after the change has them, so a switch
    /// doesn't flick back while git writes. With the generation they were set at.
    pending: HashMap<String, (String, u64)>,
}

impl HomeView {
    pub fn key(&self) -> String {
        "home".into()
    }
    pub fn title(&self) -> String {
        "Home".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.summaries.poll();
        egui::ScrollArea::vertical().id_salt("home").auto_shrink([false, false]).show(ui, |ui| {
            let full = ui.available_width();
            let width = (full - 32.0).clamp(240.0, MAX_WIDTH);
            ui.horizontal_top(|ui| {
                ui.add_space(((full - width) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.add_space(10.0);
                    self.hero(ui, cx);
                    self.stats(ui, cx);
                    let wide = width >= TWO_COLUMNS;
                    if wide {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.columns(2, |c| {
                            c[0].spacing_mut().item_spacing.y = 12.0;
                            self.start_card(&mut c[0], cx);
                            self.commits_card(&mut c[0], cx);
                            c[1].spacing_mut().item_spacing.y = 12.0;
                            self.branches_card(&mut c[1], cx);
                            self.workspaces_card(&mut c[1], cx);
                        });
                    } else {
                        self.start_card(ui, cx);
                        self.branches_card(ui, cx);
                        self.commits_card(ui, cx);
                        self.workspaces_card(ui, cx);
                    }
                    self.setup_card(ui, cx, wide);
                    ui.add_space(16.0);
                });
            });
        });
    }

    /// The repository's name, where it is and where it's hosted.
    fn hero(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        ui.horizontal(|ui| {
            // A tile with the first letter, in the accent: the page is this repository's.
            let name = cx.repo.name();
            let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            ui.painter().rect(r, 10.0, p.accent.gamma_multiply(0.16), Stroke::new(1.0, p.accent.gamma_multiply(0.45)), StrokeKind::Inside);
            let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            ui.painter().text(r.center(), Align2::CENTER_CENTER, letter, prop(21.0), p.accent);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(name).size(22.0).strong().color(p.text));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(icon::FOLDER).size(12.0).color(p.faint));
                    let path = cx.repo.root.display().to_string();
                    ui.add(egui::Label::new(RichText::new(&path).size(12.0).color(p.dim)).truncate()).on_hover_text(path);
                    if let Some(f) = cx.forge() {
                        ui.label(RichText::new("·").color(p.faint));
                        ui.label(RichText::new(icon::CLOUD).size(12.0).color(p.faint));
                        ui.label(RichText::new(format!("{} {}", f.kind.label(), f.path)).size(12.0).color(p.dim));
                    }
                });
            });
        });
    }

    /// Four numbers that say how things stand.
    fn stats(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let Some(st) = cx.status().cloned() else {
            spinner_row(ui, "Reading status…", &p);
            return;
        };
        let stashes = cx.data.stashes().len();
        let worktrees = cx.data.worktrees().iter().filter(|w| !w.is_current).count();
        let staged = st.entries.iter().filter(|e| e.is_staged()).count();
        let unstaged = st.entries.iter().filter(|e| e.is_unstaged()).count();
        let conflicts = st.conflicts().count();

        let branch = st.branch.clone().unwrap_or_else(|| format!("detached at {}", gitem_core::util::prefix(&st.head_sha, 8)));
        let (sync, sync_color) = match (st.ahead, st.behind) {
            _ if st.upstream.is_none() && st.branch.is_some() => ("not published".to_string(), p.dim),
            _ if st.upstream.is_none() => (String::new(), p.dim),
            (0, 0) => (format!("{} up to date", icon::CHECK), p.good),
            (a, 0) => (format!("\u{E08E} {a} to push"), p.outgoing()),
            (0, b) => (format!("\u{E03E} {b} to pull"), p.incoming()),
            (a, b) => (format!("\u{E08E} {a}  \u{E03E} {b}"), p.warn),
        };
        let changes = st.entries.len();
        let (changes_sub, changes_color) = if conflicts > 0 {
            (plural(conflicts, "conflict", "conflicts"), p.danger)
        } else if st.is_clean() {
            ("working tree clean".to_string(), p.good)
        } else {
            (format!("{staged} staged · {unstaged} unstaged"), p.dim)
        };

        let mut open_changes = false;
        let cols = if ui.available_width() >= TWO_COLUMNS { 4 } else { 2 };
        let gap = 12.0;
        let w = (ui.available_width() - gap * (cols - 1) as f32) / cols as f32;
        let tiles: [(&str, &str, String, String, egui::Color32, bool); 4] = [
            (icon::GIT_BRANCH, "Branch", branch, sync, sync_color, false),
            (icon::PENCIL_SIMPLE, "Changes", if changes == 0 { "None".into() } else { changes.to_string() }, changes_sub, changes_color, !st.is_clean()),
            (icon::ARCHIVE, "Stashes", stashes.to_string(), if stashes == 0 { "nothing put aside".into() } else { "put aside for later".into() }, p.dim, false),
            (icon::TREE_STRUCTURE, "Worktrees", worktrees.to_string(), if worktrees == 0 { "just this one".into() } else { "beside this one".into() }, p.dim, false),
        ];
        for line in tiles.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (glyph, label, value, sub, sub_color, click) in line {
                    let sense = if *click { Sense::click() } else { Sense::hover() };
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 74.0), sense);
                    let hovered = *click && resp.hovered();
                    ui.painter().rect(rect, 8.0, if hovered { p.hover } else { p.panel2 }, Stroke::new(1.0, if hovered { p.accent.gamma_multiply(0.6) } else { p.border }), StrokeKind::Inside);
                    let inner = rect.shrink2(vec2(14.0, 11.0));
                    paint_text(ui, Rect::from_min_size(inner.min, vec2(inner.width() - 20.0, 14.0)), &label.to_uppercase(), prop(10.5), p.dim);
                    ui.painter().text(pos2(inner.right(), inner.top() + 6.0), Align2::RIGHT_CENTER, *glyph, prop(14.0), p.faint);
                    paint_text(ui, Rect::from_min_size(inner.min + vec2(0.0, 16.0), vec2(inner.width(), 24.0)), value, prop(18.0), p.text);
                    paint_text(ui, Rect::from_min_size(inner.min + vec2(0.0, 40.0), vec2(inner.width(), 14.0)), sub, prop(11.5), *sub_color);
                    let what = format!("{label}: {value}, {sub}");
                    if *click {
                        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, what.as_str()));
                        focus_ring(ui, &resp, &p);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Open Changes").clicked() {
                            open_changes = true;
                        }
                    } else {
                        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, what.as_str()));
                    }
                }
            });
        }
        if open_changes {
            cx.open(Tab::changes());
        }
    }

    fn start_card(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let mut tiles: Vec<(&str, String, &str, Ask)> = vec![(icon::PLUS, "Start work…".into(), "A new branch, maybe in its own worktree", Ask::StartWork)];
        if cx.settings.branch_model.enabled {
            for kind in gitem_core::branchmodel::Kind::ALL {
                tiles.push((icon::GIT_BRANCH, format!("Start {}…", kind.word()), "Named and based the way your branch model says", Ask::StartKind(kind)));
            }
        }
        // What leads nowhere isn't offered: no host means no pull requests.
        if cx.forge().is_some() {
            tiles.push((icon::GIT_PULL_REQUEST, "Pull requests".into(), "Yours, and those waiting for your review", Ask::PullRequests));
        }
        if cx.status().is_some_and(|s| !s.head_sha.is_empty()) {
            tiles.push((icon::ARROWS_LEFT_RIGHT, "Compare refs".into(), "What one branch or tag has that another hasn't", Ask::Compare));
        }
        if cx.status().is_some_and(|s| !s.is_clean()) {
            tiles.push((icon::ARCHIVE, "Stash…".into(), "Put these changes aside for later", Ask::Stash));
        }
        tiles.push((icon::CLOCK_COUNTER_CLOCKWISE, "History".into(), "Every commit, as a graph", Ask::Open(Tab::graph)));
        let mut asked = None;
        card(ui, &p, icon::ROCKET_LAUNCH, "Quick start", |_| {}, |ui| {
            let cols = if ui.available_width() > 420.0 { 2 } else { 1 };
            let gap = 8.0;
            let w = (ui.available_width() - gap * (cols - 1) as f32) / cols as f32;
            for line in tiles.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (glyph, label, sub, ask) in line {
                        if action_tile(ui, w, glyph, label, sub, &p).clicked() {
                            asked = Some(match ask {
                                Ask::StartKind(k) => Ask::StartKind(*k),
                                Ask::Open(t) => Ask::Open(*t),
                                Ask::StartWork => Ask::StartWork,
                                Ask::PullRequests => Ask::PullRequests,
                                Ask::Compare => Ask::Compare,
                                Ask::Stash => Ask::Stash,
                            });
                        }
                    }
                });
            }
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                key_cap(ui, "Ctrl+P", &p);
                ui.label(RichText::new("runs any command by name.").size(11.5).color(p.dim));
                key_cap(ui, "F1", &p);
                ui.label(RichText::new("lists the keys.").size(11.5).color(p.dim));
            });
        });
        match asked {
            Some(Ask::StartWork) => start_work(cx),
            Some(Ask::StartKind(kind)) => cx.form(forms::start_branch(&cx.settings.branch_model, kind, cx.data)),
            Some(Ask::PullRequests) => cx.open(Tab::Launchpad(Box::default())),
            Some(Ask::Compare) => cx.open(Tab::Search(Box::new(crate::views::search::SearchView::compare("", "")))),
            Some(Ask::Stash) => cx.form(forms::stash_form(Vec::new())),
            Some(Ask::Open(t)) => cx.open(t()),
            None => {}
        }
    }

    /// What's happened lately on this branch.
    fn commits_card(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.recent.poll();
        if self.recent_gen != Some(cx.data.refs_gen) && !self.recent.busy() && cx.status().is_some_and(|s| !s.head_sha.is_empty()) {
            self.recent_gen = Some(cx.data.refs_gen);
            let r = cx.repo.clone();
            self.recent.spawn(ui.ctx(), move || r.log(&LogQuery { revs: vec!["HEAD".to_string()], limit: 6, ..Default::default() }));
        }
        let mut see_all = false;
        card(
            ui,
            &p,
            icon::GIT_COMMIT,
            "Recent commits",
            |ui| see_all = small_button(ui, "All history").on_hover_text("Open the commit graph").clicked(),
            |ui| {
                if let Some(e) = &self.recent.error {
                    error_box(ui, e, &p);
                    return;
                }
                let Some(list) = self.recent.get() else {
                    if cx.status().is_some_and(|s| s.head_sha.is_empty()) {
                        quiet_note(ui, "No commits yet. The first one starts the history.", &p);
                    } else {
                        spinner_row(ui, "Reading history…", &p);
                    }
                    return;
                };
                ui.spacing_mut().item_spacing.y = 0.0;
                for c in list {
                    let (rect, resp) = row(ui, 34.0, false, &p);
                    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, c.subject.as_str()));
                    paint_avatar(ui, pos2(rect.left() + 15.0, rect.center().y), 10.0, &c.author_name, &c.author_email, &p, cx.settings.gravatar);
                    let text = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top()), pos2(rect.right() - 8.0, rect.bottom()));
                    paint_text(ui, Rect::from_min_size(text.min + vec2(0.0, 3.0), vec2(text.width(), 16.0)), &c.subject, prop(12.5), p.text);
                    let meta = format!("{} · {} · {}", c.author_name, time_label(c.author_time, cx.settings.date_relative), c.short());
                    paint_text(ui, Rect::from_min_size(text.min + vec2(0.0, 18.0), vec2(text.width(), 14.0)), &meta, prop(11.0), p.dim);
                    if resp.on_hover_text("Click to see its details").clicked() {
                        cx.open(Tab::graph());
                        cx.inspect(&c.sha);
                    }
                }
            },
        );
        if see_all {
            cx.open(Tab::graph());
        }
    }

    fn branches_card(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let branches: Vec<Branch> = cx.data.local_branches().take(8).cloned().collect();
        card(ui, &p, icon::GIT_BRANCH, "Recent branches", |_| {}, |ui| {
            if !cx.data.refs_known() {
                // Not "no branches": nobody knows yet, or git couldn't say.
                match &cx.data.refs.error {
                    Some(e) => error_box(ui, e, &p),
                    None => spinner_row(ui, "Reading branches…", &p),
                }
            } else if branches.is_empty() {
                quiet_note(ui, "No branches yet", &p);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            for b in branches {
                let (rect, resp) = row(ui, 38.0, b.is_current, &p);
                resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, b.name.as_str()));
                // A dot for each: filled where you are.
                let dot = pos2(rect.left() + 13.0, rect.top() + 12.0);
                if b.is_current {
                    ui.painter().circle_filled(dot, 4.0, p.accent);
                } else {
                    ui.painter().circle_stroke(dot, 3.5, Stroke::new(1.2, p.dim));
                }
                let track = match (b.ahead, b.behind) {
                    (0, 0) => String::new(),
                    (a, 0) => format!("\u{E08E}{a}"),
                    (0, x) => format!("\u{E03E}{x}"),
                    (a, x) => format!("\u{E08E}{a} \u{E03E}{x}"),
                };
                let track_w = if track.is_empty() { 0.0 } else { 70.0 };
                let left = rect.left() + 28.0;
                paint_text(ui, Rect::from_min_size(pos2(left, rect.top() + 3.0), vec2(rect.right() - left - track_w - 8.0, 17.0)), &b.name, prop(13.0), p.text);
                paint_text(ui, Rect::from_min_size(pos2(left, rect.top() + 20.0), vec2(rect.right() - left - 8.0, 14.0)), &format!("{} · {}", time_label(b.date, cx.settings.date_relative), b.subject), prop(11.0), p.dim);
                if !track.is_empty() {
                    ui.painter().text(pos2(rect.right() - 8.0, rect.top() + 11.0), Align2::RIGHT_CENTER, &track, mono(11.0), p.dim);
                }
                if resp.double_clicked() && !b.is_current {
                    cx.run(ops::checkout(&b.name));
                }
                resp.on_hover_text("Double-click to switch; right-click for more").context_menu(|ui| menus::branch_menu(ui, cx, &b));
            }
        });
    }

    /// Git's own settings for every repository on this machine: who you are, and the
    /// handful of choices that change how git behaves day to day.
    fn setup_card(&mut self, ui: &mut Ui, cx: &mut Cx, wide: bool) {
        let p = cx.pal.clone();
        self.setup.poll();
        if self.setup_gen != Some(cx.data.status_gen) && !self.setup.busy() {
            self.setup_gen = Some(cx.data.status_gen);
            self.setup.spawn_git(ui.ctx(), cx.repo, |r| r.config_list().map(GitSetup::from));
        }
        // A read from a later generation than a change has the change, or it didn't work.
        let read_at = self.setup_gen.unwrap_or(0);
        if !self.setup.busy() {
            self.pending.retain(|_, (_, at)| *at > read_at);
        }
        let mut sets: Vec<(&'static str, String)> = Vec::new();
        let mut open: Option<Tab> = None;
        let mut all_config = false;
        let mut identity = false;
        let mut branch_other = false;
        card(
            ui,
            &p,
            icon::SLIDERS_HORIZONTAL,
            "Git on this computer",
            |ui| {
                if small_button(ui, "All config…").on_hover_text("Every key, in every scope").clicked() {
                    all_config = true;
                }
            },
            |ui| {
                ui.label(RichText::new("These are your global settings: every repository uses them unless it says otherwise.").size(11.5).color(p.faint));
                if let Some(e) = &self.setup.error {
                    error_box(ui, e, &p);
                    return;
                }
                let Some(setup) = self.setup.get().cloned() else {
                    spinner_row(ui, "Reading git's config…", &p);
                    return;
                };
                let pending = &self.pending;
                let get = |k: &str| -> Option<String> { pending.get(k).map(|(v, _)| v.clone()).or_else(|| setup.machine.get(k).cloned()) };
                // A repository that sets its own value is worth saying, or a switch would seem not to work here.
                let differs_here = |k: &str| -> Option<String> { setup.here.get(k).filter(|v| Some(v.as_str()) != get(k).as_deref()).map(|v| format!("This repository sets its own: {v}")) };

                // Who you are.
                let (name, email) = (get("user.name").unwrap_or_default(), get("user.email").unwrap_or_default());
                if name.is_empty() || email.is_empty() {
                    egui::Frame::new().fill(p.warn.gamma_multiply(0.10)).stroke(Stroke::new(1.0, p.warn.gamma_multiply(0.5))).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon::WARNING).size(20.0).color(p.warn));
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new("Git doesn't know who you are yet").strong());
                                ui.label(RichText::new("Every commit carries a name and email. Set them once for all your repositories.").size(11.5).color(p.dim));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("Set name and email…").color(p.on_accent)).fill(p.accent)).clicked() {
                                    identity = true;
                                }
                            });
                        });
                    });
                } else {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                        paint_avatar(ui, r.center(), 18.0, &name, &email, &p, cx.settings.gravatar);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add_space(2.0);
                            ui.label(RichText::new("COMMITTING AS").size(10.0).color(p.faint));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&name).size(14.5).strong().color(p.text));
                                ui.label(RichText::new(&email).size(12.5).color(p.dim));
                            });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(format!("{} Edit…", icon::PENCIL_SIMPLE)).on_hover_text("Change your name and email").clicked() {
                                identity = true;
                            }
                        });
                    });
                    for k in ["user.name", "user.email"] {
                        if let Some(note) = differs_here(k) {
                            ui.label(RichText::new(format!("{} {note}", icon::INFO)).size(11.5).color(p.warn));
                        }
                    }
                }
                ui.add_space(2.0);

                let left = |ui: &mut Ui, sets: &mut Vec<(&'static str, String)>, branch_other: &mut bool| {
                    let mut g = Group::new(ui, &p);
                    let branch = get("init.defaultbranch");
                    let hint = match &branch {
                        Some(b) if b != "main" && b != "master" => format!("New repositories start on {b}."),
                        Some(_) => "What a new repository's first branch is called.".to_string(),
                        None => "Not set, so git uses master and reminds you each time.".to_string(),
                    };
                    g.row("Default branch", &hint, |ui| {
                        if small_button(ui, "Other…").clicked() {
                            *branch_other = true;
                        }
                        let mut v = branch.clone().unwrap_or_default();
                        if segmented(ui, &mut v, &[("main".to_string(), "main"), ("master".to_string(), "master")], &p) {
                            sets.push(("init.defaultBranch", v));
                        }
                    });
                    let way = pull_way(get("pull.ff").as_deref(), get("pull.rebase").as_deref());
                    let hint = match way {
                        PullWay::Merge => "Fast-forwards when it can, merges when it can't.",
                        PullWay::Rebase => "Puts your commits on top of what came in.",
                        PullWay::FastForward => "Only ever fast-forwards, and stops if it can't.",
                        PullWay::Unset => "Not set, so git stops when your branch and its upstream have both moved.",
                    };
                    let hint = differs_here("pull.rebase").map(|h| format!("{hint} {h}.")).unwrap_or(hint.to_string());
                    g.row("When pulling", &hint, |ui| {
                        let mut v = way;
                        if segmented(ui, &mut v, &[(PullWay::Merge, "Merge"), (PullWay::Rebase, "Rebase"), (PullWay::FastForward, "Fast-forward only")], &p) {
                            match v {
                                PullWay::Merge => sets.extend([("pull.rebase", "false".into()), ("pull.ff", "true".into())]),
                                PullWay::Rebase => sets.extend([("pull.rebase", "true".into()), ("pull.ff", "true".into())]),
                                PullWay::FastForward => sets.extend([("pull.rebase", "false".into()), ("pull.ff", "only".into())]),
                                PullWay::Unset => {}
                            }
                        }
                    });
                    let crlf = get("core.autocrlf").unwrap_or_else(|| "false".into()).to_lowercase();
                    let hint = differs_here("core.autocrlf").unwrap_or_else(|| "Whether git changes line endings between your files and what's committed.".into());
                    g.row("Line endings", &hint, |ui| {
                        let mut v = if crlf == "input" { crlf.clone() } else if truthy(Some(&crlf)) { "true".to_string() } else { "false".to_string() };
                        if segmented(ui, &mut v, &[("false".to_string(), "As they are"), ("true".to_string(), "CRLF here"), ("input".to_string(), "LF in commits")], &p) {
                            sets.push(("core.autocrlf", v));
                        }
                    });
                };
                let right = |ui: &mut Ui, sets: &mut Vec<(&'static str, String)>, open: &mut Option<Tab>| {
                    let mut g = Group::new(ui, &p);
                    let switches: [(&'static str, &str, &str); 3] = [
                        ("push.autoSetupRemote", "Publish new branches on push", "A first push sets the upstream itself, without -u."),
                        ("fetch.prune", "Prune on fetch", "Remote branches deleted on the server go here too."),
                        ("rerere.enabled", "Remember conflict resolutions", "A conflict you've solved once is solved the same way again."),
                    ];
                    for (key, label, hint) in switches {
                        let was = truthy(get(&key.to_lowercase()).as_deref());
                        let mut on = was;
                        let hint = differs_here(&key.to_lowercase()).map(|h| format!("{hint} {h}.")).unwrap_or(hint.to_string());
                        g.switch(&mut on, label, &hint);
                        if on != was {
                            sets.push((key, on.to_string()));
                        }
                    }
                    if get("user.signingkey").is_some() {
                        let was = truthy(get("commit.gpgsign").as_deref());
                        let mut on = was;
                        g.switch(&mut on, "Sign commits", "Prove a commit is yours with your signing key.");
                        if on != was {
                            sets.push(("commit.gpgsign", on.to_string()));
                        }
                    } else {
                        g.row("Sign commits", "Needs a signing key first.", |ui| {
                            if small_button(ui, format!("{} Set up…", icon::KEY)).clicked() {
                                *open = Some(Tab::Credentials(Box::default()));
                            }
                        });
                    }
                };
                if wide {
                    ui.spacing_mut().item_spacing.x = 32.0;
                    ui.columns(2, |c| {
                        left(&mut c[0], &mut sets, &mut branch_other);
                        right(&mut c[1], &mut sets, &mut open);
                    });
                } else {
                    left(ui, &mut sets, &mut branch_other);
                    right(ui, &mut sets, &mut open);
                }
            },
        );
        let at = cx.data.status_gen;
        for (key, value) in sets {
            self.pending.insert(key.to_lowercase(), (value.clone(), at + 1));
            cx.run(ops::config_set(key, &value, "global"));
        }
        if identity {
            let s = self.setup.get();
            let get = |k: &str| s.and_then(|s| s.machine.get(k).cloned()).unwrap_or_default();
            cx.form(forms::identity_form_with(&get("user.name"), &get("user.email")));
        }
        if branch_other {
            let now = self.setup.get().and_then(|s| s.machine.get("init.defaultbranch").cloned()).unwrap_or_default();
            cx.form(FormDialog::new("Default branch name", "Save").info("What git calls the first branch of a repository you create or initialize from now on.").text("name", "Branch name", &now).hint("trunk").on_submit(|v| {
                let n = v.opt("name").ok_or("A name is required.")?;
                Ok(vec![Action::Run(ops::config_set("init.defaultBranch", &n, "global"))])
            }));
        }
        if all_config {
            open = Some(Tab::Config(Box::default()));
        }
        if let Some(t) = open {
            cx.open(t);
        }
    }

    fn workspaces_card(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        card(ui, &p, icon::SQUARES_FOUR, "Workspaces", |_| {}, |ui| self.workspaces(ui, cx));
    }

    fn start_sweep(&mut self, ctx: &egui::Context, cx: &Cx, repos: Vec<String>, done: (&'static str, &'static str), op: impl Fn() -> ops::GitOp + Send + 'static) {
        let git = cx.repo.git.clone();
        self.sweep.spawn(ctx, move || sweep(git, repos, done, op));
    }

    fn workspaces(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let ws: Vec<Workspace> = cx.settings.workspaces.clone();
        ui.horizontal_wrapped(|ui| {
            for (i, w) in ws.iter().enumerate() {
                if ui.selectable_label(self.workspace_idx == i, w.name.as_str()).clicked() {
                    self.workspace_idx = i;
                    self.summaries_for = None;
                }
            }
            if crate::widgets::small_button(ui, "New workspace…").clicked() {
                cx.form(new_workspace_form(ws.iter().map(|w| w.name.clone()).collect()));
            }
        });
        if ws.is_empty() {
            quiet_note(ui, "Group repositories to see their status together and fetch or pull them all at once.", &p);
            return;
        }
        self.workspace_idx = self.workspace_idx.min(ws.len() - 1);
        let w = &ws[self.workspace_idx];
        if self.sweep.poll() {
            if let Some(m) = self.sweep.value.take() {
                cx.toast(m);
            }
            if let Some(e) = self.sweep.error.take() {
                cx.error(e);
            }
            self.swept += 1;
        }
        // What a sweep did to the others doesn't show in this repository's generation.
        let key = format!("{}:{}:{}", w.name, cx.data.status_gen, self.swept);
        if self.summaries_for.as_deref() != Some(&key) {
            self.summaries_for = Some(key);
            let (git, repos) = (cx.repo.git.clone(), w.repos.clone());
            self.summaries.spawn(ui.ctx(), move || -> Result<Vec<RepoSummary>, String> {
                Ok(repos
                    .iter()
                    .map(|path| {
                        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                        match Repo::open(git.clone(), std::path::Path::new(path)).and_then(|r| r.status()) {
                            Ok(s) => RepoSummary { path: path.clone(), name, branch: s.branch.unwrap_or_else(|| "detached".into()), changes: s.entries.len(), ahead: s.ahead, behind: s.behind, error: None },
                            Err(e) => RepoSummary { path: path.clone(), name, error: Some(e.to_string()), ..Default::default() },
                        }
                    })
                    .collect())
            });
        }
        ui.horizontal(|ui| {
            let busy = self.sweep.busy();
            let why = "One of these is already going through the repositories.";
            if ui.add_enabled(!busy, egui::Button::new("Fetch all")).on_disabled_hover_text(why).clicked() {
                let prune = cx.settings.prune_on_fetch;
                self.start_sweep(ui.ctx(), cx, w.repos.clone(), ("Fetched", "fetching"), move || ops::fetch(&ops::FetchOpts { all: true, prune, ..Default::default() }));
            }
            if ui.add_enabled(!busy, egui::Button::new("Pull all")).on_hover_text("Fast-forward only, so nothing needs resolving. A repository that can't is left as it is and reported").on_disabled_hover_text(why).clicked() {
                self.start_sweep(ui.ctx(), cx, w.repos.clone(), ("Pulled", "pulling"), || ops::pull(ops::PullMode::FfOnly, false));
            }
            if crate::widgets::small_button(ui, "Open all").clicked() {
                for r in &w.repos {
                    cx.out.push(Action::OpenRepo(r.into()));
                }
            }
            if crate::widgets::small_button(ui, "Add current repository").clicked() {
                let name = w.name.clone();
                let path = cx.repo.root.to_string_lossy().replace('\\', "/");
                cx.out.push(Action::Settings(Box::new(move |s| {
                    if let Some(x) = s.workspaces.iter_mut().find(|x| x.name == name)
                        && !x.repos.contains(&path) {
                            x.repos.push(path);
                        }
                })));
                self.summaries_for = None;
            }
            if ui.add_enabled(!self.scan_pick.busy(), egui::Button::new("Scan a folder…")).on_disabled_hover_text("The folder picker is already open.").clicked() {
                crate::widgets::pick(&mut self.scan_pick, ui.ctx(), rfd::FileDialog::new(), true);
            }
            if self.scan_pick.poll()
                && let Some(Some(dir)) = self.scan_pick.value.take() {
                    // Off the UI thread, as a big folder takes a while.
                    let name = w.name.clone();
                    cx.out.push(Action::Later(Box::new(move || {
                        let found = gitem_core::repo::find_repos(&dir, 3);
                        Ok(crate::action::Later::Many(vec![
                            crate::action::Later::Toast(format!("Found {} repositories", found.len())),
                            crate::action::Later::Settings(Box::new(move |s| {
                                if let Some(x) = s.workspaces.iter_mut().find(|x| x.name == name) {
                                    for f in found {
                                        let f = f.to_string_lossy().replace('\\', "/");
                                        if !x.repos.contains(&f) {
                                            x.repos.push(f);
                                        }
                                    }
                                }
                            })),
                        ]))
                    })));
                }
            if crate::widgets::small_button(ui, "Delete workspace…").clicked() {
                let name = w.name.clone();
                cx.form(FormDialog::new("Delete workspace", "Delete").danger().info(format!("Delete the workspace {name} and its list of {}? The repositories themselves aren't touched.", plural(w.repos.len(), "repository", "repositories"))).on_submit(move |_| {
                    let name = name.clone();
                    Ok(vec![Action::Settings(Box::new(move |s| {
                        // One only: older settings can hold two of the same name.
                        if let Some(at) = s.workspaces.iter().position(|x| x.name == name) {
                            s.workspaces.remove(at);
                        }
                    }))])
                }));
            }
            if self.summaries.busy() || self.sweep.busy() {
                ui.add(spinner(13.0));
            }
        });
        if let Some(list) = self.summaries.value.take() {
            ui.spacing_mut().item_spacing.y = 0.0;
            for s in &list {
                let (rect, resp) = row(ui, 26.0, false, &p);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, s.name.as_str()));
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 0.0), egui::vec2(200.0, 26.0)), &s.name, prop(13.0), p.text);
                let detail = match &s.error {
                    Some(e) => e.clone(),
                    None => format!("{} · {} changed · \u{E08E}{} \u{E03E}{}", s.branch, s.changes, s.ahead, s.behind),
                };
                paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(212.0, 0.0), egui::vec2(rect.width() - 216.0, 26.0)), &detail, prop(12.0), if s.error.is_some() { p.danger } else { p.dim });
                let resp = resp.on_hover_text("Click to open. Right-click to remove it from this workspace.");
                if resp.clicked() {
                    cx.out.push(Action::OpenRepo(s.path.clone().into()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Remove from this workspace").clicked() {
                        let (name, path) = (w.name.clone(), s.path.clone());
                        cx.out.push(Action::Settings(Box::new(move |st| {
                            if let Some(x) = st.workspaces.iter_mut().find(|x| x.name == name) {
                                x.repos.retain(|r| *r != path);
                            }
                        })));
                        self.summaries_for = None;
                        ui.close();
                    }
                });
            }
            ui.spacing_mut().item_spacing.y = 4.0;
            if self.summaries.value.is_none() {
                self.summaries.value = Some(list);
            }
        }
    }
}

/// One op through each repository in turn, off the UI thread. What failed is reported once, all
/// together, and doesn't stop the others. A repository that hasn't been opened and has config
/// that would run a program is left out until someone has decided about it.
fn sweep(git: gitem_core::GitExe, repos: Vec<String>, (done, doing): (&str, &str), op: impl Fn() -> ops::GitOp) -> Result<String, String> {
    let (mut worked, mut failed) = (0, Vec::new());
    for path in &repos {
        let dir = std::path::Path::new(path);
        let result = match Repo::open(git.clone(), dir) {
            Ok(r) if r.trust.restricted() => {
                failed.push(format!("Not {doing} {path}: its config names programs that you haven't said can run. Open it to decide."));
                continue;
            }
            Ok(r) => op().unattended().run(&r.git, Some(dir), None, &mut |_| {}).map(|_| ()).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => worked += 1,
            Err(e) => failed.push(format!("{path}: {}", e.lines().last().unwrap_or(&e))),
        }
    }
    if failed.is_empty() {
        Ok(format!("{done} {}", plural(worked, "repository", "repositories")))
    } else {
        Err(format!("{done} {}. {} didn't work:\n{}", plural(worked, "repository", "repositories"), failed.len(), failed.join("\n")))
    }
}

fn new_workspace_form(taken: Vec<String>) -> FormDialog {
    FormDialog::new("New workspace", "Create").text("name", "Name", "").on_submit(move |v| {
        let name = v.opt("name").ok_or("A name is required.")?;
        // Workspaces are found by name, so a second of the same name could never be reached.
        if taken.contains(&name) {
            return Err(format!("There's already a workspace called {name}."));
        }
        Ok(vec![Action::Settings(Box::new(move |s| s.workspaces.push(Workspace { name, repos: Vec::new() })))])
    })
}

/// The form behind Start work: a branch from the default one, maybe in a worktree of its own.
fn start_work(cx: &mut Cx) {
    let base = cx.data.refs.get().and_then(|r| r.default_branch.clone()).unwrap_or_else(|| "HEAD".to_string());
    let repo_name = cx.repo.name();
    let parent = cx.repo.root.parent().map(|x| x.to_path_buf()).unwrap_or_default();
    cx.form(
        FormDialog::new("Start work", "Start")
            .text("name", "Branch name", "")
            .hint("feature/my-work")
            .text("base", "Based on", &base)
            .check("worktree", "Use a new worktree", false)
            .on_submit(move |v| {
                let n = v.opt("name").ok_or("A branch name is required.")?;
                let b = v.opt("base");
                Ok(if v.on("worktree") {
                    let path = parent.join(format!("{repo_name}.worktrees/{}", n.replace('/', "-"))).to_string_lossy().replace('\\', "/");
                    vec![Action::RunThenOpen(ops::worktree_start_work(&path, &n, b.as_deref()), path.into())]
                } else {
                    vec![Action::Run(ops::branch_start_work(&n, b.as_deref()))]
                })
            }),
    );
}

/// A rounded panel with a title row: an accent glyph, the title, and whatever `extra` puts
/// at the right end.
fn card(ui: &mut Ui, p: &Palette, glyph: &str, title: &str, extra: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(p.panel2).stroke(Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 14 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.label(RichText::new(glyph).size(15.0).color(p.accent));
            ui.label(RichText::new(title).size(14.0).strong().color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), extra);
        });
        ui.add_space(2.0);
        body(ui);
    });
}

/// A big button: a glyph in a tinted square, a name, and a line on what it's for.
fn action_tile(ui: &mut Ui, w: f32, glyph: &str, label: &str, sub: &str, p: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 50.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(sub);
    let hovered = resp.hovered();
    ui.painter().rect(rect, 8.0, if hovered { p.button_hover } else { p.button }, Stroke::new(1.0, if hovered { p.accent.gamma_multiply(0.6) } else { p.button_border }), StrokeKind::Inside);
    let square = Rect::from_center_size(pos2(rect.left() + 25.0, rect.center().y), vec2(32.0, 32.0));
    ui.painter().rect_filled(square, 7.0, p.accent.gamma_multiply(if hovered { 0.24 } else { 0.15 }));
    ui.painter().text(square.center(), Align2::CENTER_CENTER, glyph, prop(16.0), p.accent);
    let x = square.right() + 10.0;
    let tw = rect.right() - x - 8.0;
    paint_text(ui, Rect::from_min_size(pos2(x, rect.top() + 8.0), vec2(tw, 17.0)), label, prop(13.0), p.text);
    paint_text(ui, Rect::from_min_size(pos2(x, rect.top() + 26.0), vec2(tw, 15.0)), sub, prop(11.0), p.dim);
    focus_ring(ui, &resp, p);
    resp
}

/// A key name drawn like a key.
fn key_cap(ui: &mut Ui, keys: &str, p: &Palette) {
    let g = ui.painter().layout_no_wrap(keys.to_string(), mono(10.5), p.text);
    let (r, resp) = ui.allocate_exact_size(g.size() + vec2(10.0, 4.0), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, keys));
    ui.painter().rect(r, 4.0, p.button, Stroke::new(1.0, p.button_border), StrokeKind::Inside);
    ui.painter().galley(r.min + vec2(5.0, 2.0), g, p.text);
}

fn quiet_note(ui: &mut Ui, text: &str, p: &Palette) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).color(p.dim));
    ui.add_space(4.0);
}
