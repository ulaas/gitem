//! Find in files: the text of every file, searched as you type, in the working tree, the
//! changed files, the open editors, or at a revision. The sidebar's Search pane and the tab
//! of its own are both this view; the search itself is `gitem_core::textsearch`.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::text::LayoutJob;
use egui::{Align2, Color32, Key, Modifiers, Rect, RichText, Sense, Stroke, TextFormat, Ui, WidgetInfo, WidgetType, pos2, vec2};
use gitem_core::model::FileStatus;
use gitem_core::textsearch::{self, Event, FileMatches, LineMatch, Query, Request, Scope, Summary};

use crate::action::Cx;
use crate::chrome::{self, icon};
use crate::theme::Palette;
use crate::views::Tab;
use crate::views::filetree::{Mark, file_glyph, mark_for};
use crate::widgets::*;

const ROW_H: f32 = 22.0;
/// How long typing has to pause before the search starts.
const DEBOUNCE: f64 = 0.2;
/// How long files have to stop changing before the results are brought up to date.
const SETTLE: f64 = 0.6;
/// Searches remembered for Up and Down in the box, for every view.
const HISTORY: usize = 30;

/// Which files a search looks in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Within {
    #[default]
    WorkingTree,
    /// What git says has changed, untracked files included.
    Changed,
    OpenEditors,
    /// The files as a commit, branch or tag has them.
    Revision,
}

impl Within {
    const ALL: [Within; 4] = [Within::WorkingTree, Within::Changed, Within::OpenEditors, Within::Revision];

    fn name(self, compact: bool) -> &'static str {
        match (self, compact) {
            (Within::WorkingTree, true) => "All",
            (Within::WorkingTree, false) => "Working tree",
            (Within::Changed, true) => "Changed",
            (Within::Changed, false) => "Changed files",
            (Within::OpenEditors, true) => "Open",
            (Within::OpenEditors, false) => "Open editors",
            (Within::Revision, true) => "Revision",
            (Within::Revision, false) => "At a revision",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Within::WorkingTree => "Every file in the working tree, as it is on disk",
            Within::Changed => "Only files with changes, untracked ones too",
            Within::OpenEditors => "Only files open in editors, with their unsaved edits",
            Within::Revision => "The files as a commit, branch or tag has them",
        }
    }
}

/// Everything a search is for. When any of it changes, it's another search.
#[derive(Clone, Debug, PartialEq)]
struct Asked {
    query: Query,
    within: Within,
    folder: Option<String>,
    rev: String,
}

struct Running {
    search: textsearch::Search,
    asked: Asked,
    /// Bringing what's shown up to date. It stays showing until this is done, so the list
    /// doesn't empty and fill again each time a file is saved.
    refresh: bool,
    found: Vec<FileMatches>,
}

/// A row of the results: a file, or one of its lines, by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    File(usize),
    Line(usize, usize),
}

/// A view of its own, for whichever was used last to take F4.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct FindView {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub include: String,
    pub exclude: String,
    pub include_ignored: bool,
    pub within: Within,
    /// Only under this folder, repository-relative.
    pub folder: Option<String>,
    pub rev: String,
    /// The boxes for files to include and exclude are showing.
    details: bool,
    /// In the sidebar: narrower, with shorter words.
    compact: bool,
    running: Option<Running>,
    /// What's shown, by path.
    files: Vec<FileMatches>,
    summary: Option<Summary>,
    /// What `files` are the answer to.
    shown_for: Option<Asked>,
    error: Option<String>,
    /// When what's asked last changed, for a search once typing pauses.
    edited_at: Option<f64>,
    /// When files last changed under the results.
    stale_since: Option<f64>,
    seen: (u64, u64),
    /// Past the usual limit on matches: Search all was pressed.
    all: bool,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    rows_dirty: bool,
    /// The path and line picked, which stay picked as results arrive and move rows.
    selected: Option<(String, Option<u32>)>,
    marks: Option<(u64, Arc<HashMap<String, Mark>>)>,
    focus_query: bool,
    /// The picked row is to be scrolled to.
    reveal: bool,
    /// Where Up and Down are in the history, and what was typed before they were pressed.
    history_at: Option<(usize, String)>,
    id: u64,
}

impl Default for FindView {
    fn default() -> Self {
        Self {
            text: String::new(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            include: String::new(),
            exclude: String::new(),
            include_ignored: false,
            within: Within::WorkingTree,
            folder: None,
            rev: String::new(),
            details: false,
            compact: false,
            running: None,
            files: Vec::new(),
            summary: None,
            shown_for: None,
            error: None,
            edited_at: None,
            stale_since: None,
            seen: (0, 0),
            all: false,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: false,
            selected: None,
            marks: None,
            focus_query: false,
            reveal: false,
            history_at: None,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl FindView {
    /// For the sidebar.
    pub fn compact() -> Self {
        Self { compact: true, ..Default::default() }
    }

    /// Looking for `text`, perhaps only in a folder, with the box ready for typing.
    pub fn looking_for(text: &str, folder: Option<String>) -> Self {
        let mut v = Self::default();
        v.seed(Some(text.to_string()), folder);
        v
    }

    /// Asked again, as Ctrl+Shift+F does: what to look for and where, and the keyboard.
    pub fn seed(&mut self, text: Option<String>, folder: Option<String>) {
        if let Some(t) = text.filter(|t| !t.is_empty() && !t.contains('\n') && *t != self.text) {
            self.text = t;
            self.edited_at = Some(f64::NEG_INFINITY);
        }
        if folder.is_some() {
            if self.folder != folder {
                self.edited_at = Some(f64::NEG_INFINITY);
            }
            self.folder = folder;
            self.within = Within::WorkingTree;
        }
        self.focus_query = true;
    }

    pub fn key(&self) -> String {
        "grep".into()
    }

    pub fn title(&self) -> String {
        "Find in files".into()
    }

    fn asked(&self) -> Asked {
        let query = Query {
            text: self.text.clone(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            include: self.include.clone(),
            exclude: self.exclude.clone(),
            include_ignored: self.include_ignored,
            max_matches: if self.all { 0 } else { textsearch::DEFAULT_MAX_MATCHES },
            ..Default::default()
        };
        Asked { query, within: self.within, folder: self.folder.clone(), rev: self.rev.trim().to_string() }
    }

    /// Whether the result list is out of date with what's asked: something's been changed.
    fn changed(&mut self, now: f64) {
        self.edited_at = Some(now);
        self.all = false;
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let now = ui.input(|i| i.time);
        cx.data.searching.set(true);
        self.receive();
        self.watch_files(cx, now);

        // A tab can be wide; the boxes stay a size that reads as one thing.
        let boxes = if self.compact {
            self.controls(ui, cx, now)
        } else {
            let room = ui.available_width().min(820.0);
            ui.scope(|ui| {
                ui.set_max_width(room);
                self.controls(ui, cx, now)
            })
            .inner
        };
        self.shortcuts(ui, cx, boxes);

        // Typing has paused, or Enter was pressed: search.
        if let Some(t) = self.edited_at {
            let left = DEBOUNCE - (now - t);
            if left <= 0.0 {
                self.edited_at = None;
                self.start(ui.ctx(), cx, false);
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(left));
            }
        }
        if let Some(t) = self.stale_since
            && self.running.is_none()
            && self.edited_at.is_none()
        {
            let left = SETTLE - (now - t);
            if left <= 0.0 {
                self.stale_since = None;
                self.start(ui.ctx(), cx, true);
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(left));
            }
        }

        let room = if self.compact { ui.available_width() } else { ui.available_width().min(820.0) };
        ui.scope(|ui| {
            ui.set_max_width(room);
            self.status_line(ui, cx, &p);
        });
        self.results(ui, cx, &p);
    }

    /// Takes in what the search has found since the last frame.
    fn receive(&mut self) {
        let Some(run) = &mut self.running else { return };
        let mut done = None;
        let mut batch = Vec::new();
        for e in run.search.poll() {
            match e {
                Event::Found(f) if run.refresh => run.found.push(f),
                Event::Found(f) => batch.push(f),
                Event::Done(s) => done = Some(s),
            }
        }
        if !batch.is_empty() {
            // In order by path as they come, a frame's worth at a time: one at a time would
            // move every file after it, each time, which tens of thousands of files can't afford.
            batch.sort_by(|a, b| a.path.cmp(&b.path));
            self.files = merge_by_path(std::mem::take(&mut self.files), batch);
            self.rows_dirty = true;
        }
        if let Some(s) = done {
            let Some(run) = self.running.take() else { return };
            if run.refresh {
                let mut found = run.found;
                found.sort_by(|a, b| a.path.cmp(&b.path));
                self.files = found;
                self.rows_dirty = true;
            }
            self.summary = Some(s);
            self.shown_for = Some(run.asked);
        }
    }

    /// Files changing under results that came from them: search again once they settle.
    fn watch_files(&mut self, cx: &Cx, now: f64) {
        let seen = (cx.data.status_gen, cx.data.editors_gen);
        if seen == self.seen {
            return;
        }
        self.seen = seen;
        let live = self.shown_for.as_ref().is_some_and(|a| a.within != Within::Revision && !a.query.text.is_empty());
        if live {
            self.stale_since = Some(now);
        }
    }

    fn start(&mut self, ctx: &egui::Context, cx: &Cx, refresh: bool) {
        let asked = self.asked();
        if let Some(run) = self.running.take() {
            run.search.cancel();
        }
        if asked.query.text.is_empty() {
            self.files.clear();
            self.summary = None;
            self.shown_for = None;
            self.error = None;
            self.rows_dirty = true;
            return;
        }
        let under = |path: &str| asked.folder.as_deref().is_none_or(|f| path.strip_prefix(f).is_some_and(|rest| rest.starts_with('/')));
        let scope = match asked.within {
            Within::WorkingTree => Scope::WorkingTree { folder: asked.folder.clone() },
            Within::Changed => {
                let entries = cx.status().map(|s| s.entries.as_slice()).unwrap_or_default();
                let gone = |e: &&gitem_core::model::StatusEntry| e.worktree == FileStatus::Deleted || (e.index == FileStatus::Deleted && e.worktree == FileStatus::Unmodified);
                Scope::Files(entries.iter().filter(|e| !gone(e) && !e.submodule && under(&e.path)).map(|e| e.path.clone()).collect())
            }
            Within::OpenEditors => Scope::Files(cx.data.editors.iter().filter(|e| under(&e.path)).map(|e| e.path.clone()).collect()),
            Within::Revision => Scope::Revision(if asked.rev.is_empty() { "HEAD".into() } else { asked.rev.clone() }),
        };
        let overlay = if asked.within == Within::Revision { HashMap::new() } else { cx.data.editors.iter().filter_map(|e| Some((e.path.clone(), e.unsaved.clone()?))).collect() };
        let req = Request { root: cx.repo.root.clone(), query: asked.query.clone(), scope, overlay, git: Some(cx.repo.git.clone()) };
        let c = ctx.clone();
        match textsearch::start(req, move || c.request_repaint()) {
            Ok(search) => {
                self.error = None;
                if !refresh {
                    self.files.clear();
                    self.summary = None;
                    self.shown_for = Some(asked.clone());
                    self.rows_dirty = true;
                }
                self.stale_since = None;
                self.running = Some(Running { search, asked, refresh, found: Vec::new() });
                ctx.data_mut(|d| d.insert_temp(owner_id(), self.id));
            }
            Err(e) => {
                self.error = Some(e);
                self.files.clear();
                self.summary = None;
                self.shown_for = None;
                self.rows_dirty = true;
            }
        }
    }

    /// The boxes and switches. Returns whether one of this view's text boxes has the keyboard.
    fn controls(&mut self, ui: &mut Ui, cx: &mut Cx, now: f64) -> bool {
        let p = cx.pal.clone();
        let mut focused = false;
        let query_id = egui::Id::new(("find-query", self.id));
        ui.add_space(4.0);
        // Up and Down in the box go through earlier searches, before the box sees them.
        if ui.memory(|m| m.has_focus(query_id)) {
            let (up, down) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::ArrowDown)));
            if up || down {
                self.step_history(ui.ctx(), up, now);
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let toggles = 3.0 * (ROW_H + 2.0);
            let width = (ui.available_width() - toggles).max(60.0);
            let mut edit = input(&mut self.text).id(query_id).hint_text(if self.compact { "Search" } else { "Search the text of every file" }).desired_width(width);
            if self.error.is_some() {
                edit = edit.text_color(p.danger);
            }
            let r = ui.add(edit);
            r.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Find in files"));
            if std::mem::take(&mut self.focus_query) {
                r.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), query_id) {
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(self.text.chars().count()))));
                    state.store(ui.ctx(), query_id);
                }
            }
            if r.changed() {
                self.history_at = None;
                self.changed(now);
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                // Now, not after the pause, and kept for Up to bring back.
                self.edited_at = Some(f64::NEG_INFINITY);
                remember(ui.ctx(), &self.text);
                r.request_focus();
            }
            focused |= r.has_focus();
            let mut flipped = false;
            flipped |= option_toggle(ui, &mut self.case_sensitive, "Aa", "Match case", "Alt+C", false, &p);
            flipped |= option_toggle(ui, &mut self.whole_word, "ab", "Match whole word", "Alt+W", true, &p);
            flipped |= option_toggle(ui, &mut self.regex, ".*", "Use regular expression", "Alt+R", false, &p);
            if flipped {
                self.edited_at = Some(now);
                // Typing goes on in the box, as it would after Alt+C.
                ui.memory_mut(|m| m.request_focus(query_id));
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if let Some(w) = scope_pills(ui, self.within, self.compact, &p) {
                self.within = w;
                self.changed(now);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hidden_globs = !self.details && (!self.include.trim().is_empty() || !self.exclude.trim().is_empty() || self.include_ignored);
                let r = chrome::icon_button(ui, icon::DOTS_THREE, "Files to include and exclude", "Globs such as *.rs or src/**, and whether ignored files count", self.details, &p);
                if hidden_globs {
                    ui.painter().circle_filled(r.rect.right_top() + vec2(-5.0, 5.0), 3.0, p.accent);
                }
                if r.clicked() {
                    self.details = !self.details;
                }
            });
        });
        if self.within == Within::Revision {
            ui.add_space(3.0);
            let r = ui.add(input(&mut self.rev).hint_text("HEAD, a branch, a tag or a commit").desired_width(f32::INFINITY));
            r.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "Revision"));
            if r.changed() {
                self.changed(now);
            }
            focused |= r.has_focus();
        }
        if let Some(folder) = self.folder.clone() {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::FOLDER_SIMPLE).color(p.series[3]));
                ui.label(RichText::new("Only in").color(p.dim));
                ui.add(egui::Label::new(RichText::new(&folder).strong()).truncate());
                if small_icon_button(ui, icon::X, "Search everywhere again", "").clicked() {
                    self.folder = None;
                    self.changed(now);
                }
            });
        }
        if self.details {
            ui.add_space(3.0);
            for (which, hint) in [(0, "Files to include, such as *.rs, src/**"), (1, "Files to exclude, such as **/tests/**")] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let room = if which == 1 { ui.available_width() - ROW_H - 4.0 } else { ui.available_width() };
                    let text = if which == 0 { &mut self.include } else { &mut self.exclude };
                    let r = ui.add(input(text).hint_text(hint).desired_width(room));
                    r.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, if which == 0 { "Files to include" } else { "Files to exclude" }));
                    if r.changed() {
                        self.changed(now);
                    }
                    focused |= r.has_focus();
                    if which == 1 {
                        let glyph = if self.include_ignored { icon::EYE } else { icon::EYE_SLASH };
                        let mut on = self.include_ignored;
                        if option_toggle(ui, &mut on, glyph, "Search ignored files too", "", false, &p) {
                            self.include_ignored = on;
                            self.changed(now);
                        }
                    }
                });
                ui.add_space(2.0);
            }
        }
        ui.add_space(2.0);
        focused
    }

    fn step_history(&mut self, ctx: &egui::Context, back: bool, now: f64) {
        let list = history(ctx);
        if list.is_empty() {
            return;
        }
        let at = match (&self.history_at, back) {
            (None, true) => Some(0),
            (None, false) => None,
            (Some((i, _)), true) => Some((i + 1).min(list.len() - 1)),
            (Some((0, _)), false) => {
                // Past the newest: back to what was typed.
                if let Some((_, typed)) = self.history_at.take() {
                    self.text = typed;
                    self.changed(now);
                }
                return;
            }
            (Some((i, _)), false) => Some(i - 1),
        };
        let Some(at) = at else { return };
        let typed = self.history_at.take().map_or_else(|| self.text.clone(), |(_, t)| t);
        self.text = list[at].clone();
        self.history_at = Some((at, typed));
        self.changed(now);
    }

    fn shortcuts(&mut self, ui: &mut Ui, cx: &mut Cx, boxes: bool) {
        if cx.dialog_open {
            return;
        }
        let now = ui.input(|i| i.time);
        if boxes {
            for (key, which) in [(Key::C, 0), (Key::W, 1), (Key::R, 2)] {
                if ui.input_mut(|i| crate::keys::take_chord(i, Modifiers::ALT, key)) {
                    let flag = match which {
                        0 => &mut self.case_sensitive,
                        1 => &mut self.whole_word,
                        _ => &mut self.regex,
                    };
                    *flag = !*flag;
                    self.edited_at = Some(now);
                }
            }
        }
        // F4 goes through the results of whichever was used last.
        if ui.ctx().data(|d| d.get_temp::<u64>(owner_id())) != Some(self.id) {
            return;
        }
        let (next, prev) = ui.input_mut(|i| (crate::keys::take_chord(i, Modifiers::NONE, Key::F4), crate::keys::take_chord(i, Modifiers::SHIFT, Key::F4)));
        if next || prev {
            self.step_result(ui, cx, !prev);
        }
    }

    /// Opens the next or previous line found, after the one picked.
    fn step_result(&mut self, ui: &Ui, cx: &mut Cx, forward: bool) {
        let lines: Vec<(usize, usize)> = self.files.iter().enumerate().flat_map(|(f, file)| (0..file.lines.len()).map(move |l| (f, l))).collect();
        if lines.is_empty() {
            return;
        }
        let here = self.selected.as_ref().and_then(|(path, line)| {
            let f = self.files.iter().position(|x| &x.path == path)?;
            Some((f, line.and_then(|n| self.files[f].lines.iter().position(|l| l.line == n))))
        });
        let at = match here {
            None => if forward { 0 } else { lines.len() - 1 },
            Some((f, None)) => {
                let first = lines.iter().position(|(x, _)| *x == f).unwrap_or(0);
                if forward { first } else { (first + lines.len() - 1) % lines.len() }
            }
            Some((f, Some(l))) => {
                let i = lines.iter().position(|x| *x == (f, l)).unwrap_or(0);
                if forward { (i + 1) % lines.len() } else { (i + lines.len() - 1) % lines.len() }
            }
        };
        let (f, l) = lines[at];
        let path = self.files[f].path.clone();
        if self.collapsed.remove(&path) {
            self.rows_dirty = true;
        }
        self.open(ui.ctx(), cx, f, l, false);
        let line = self.files[f].lines[l].line;
        self.selected = Some((path, Some(line)));
        self.reveal = true;
    }

    fn open(&self, ctx: &egui::Context, cx: &mut Cx, file: usize, line: usize, keep: bool) {
        let Some(f) = self.files.get(file) else { return };
        let Some(m) = f.lines.get(line) else { return };
        let at_rev = self.shown_for.as_ref().filter(|a| a.within == Within::Revision).map(|a| if a.rev.is_empty() { "HEAD".to_string() } else { a.rev.clone() });
        if let Some(t) = &at_rev {
            cx.open(Tab::file(&f.path, Some(t.clone()), Some(m.line)));
        } else if cx.settings.ide_mode {
            let len = m.ranges.first().and_then(|r| m.preview.get(r.start as usize..r.end as usize)).map_or(0, |s| s.chars().count());
            let view = crate::views::editorview::EditorView::at_match(&f.path, (m.line as usize).saturating_sub(1), m.column as usize, len, keep);
            cx.open(Tab::Editor(Box::new(view)));
        } else {
            cx.open(Tab::file(&f.path, None, Some(m.line)));
        }
        remember(ctx, &self.text);
    }
}

/// The id under which the view that searched last is kept, for F4.
fn owner_id() -> egui::Id {
    egui::Id::new("find-in-files-owner")
}

fn history_id() -> egui::Id {
    egui::Id::new("find-in-files-history")
}

fn history(ctx: &egui::Context) -> Vec<String> {
    ctx.data(|d| d.get_temp::<Vec<String>>(history_id())).unwrap_or_default()
}

/// Puts a search first in the history.
pub fn remember(ctx: &egui::Context, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    ctx.data_mut(|d| {
        let list = d.get_temp_mut_or_default::<Vec<String>>(history_id());
        list.retain(|t| t != text);
        list.insert(0, text.to_string());
        list.truncate(HISTORY);
    });
}

impl FindView {
    /// How many found, how long it took, and what's left out.
    fn status_line(&mut self, ui: &mut Ui, cx: &mut Cx, p: &Palette) {
        if let Some(e) = &self.error {
            error_box(ui, e, p);
            return;
        }
        if self.shown_for.is_none() && self.running.is_none() {
            return;
        }
        let searching = self.running.is_some();
        let matches: usize = self.files.iter().map(|f| f.matches).sum();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if searching {
                ui.add(spinner(12.0));
            }
            let text = if self.files.is_empty() && searching {
                "Searching…".to_string()
            } else {
                let mut t = format!("{} in {}", plural(matches, "result", "results"), plural(self.files.len(), "file", "files"));
                if let Some(s) = self.summary.as_ref().filter(|_| !searching && !self.compact) {
                    t.push_str(&format!(" · {} searched · {:.2} s", plural(s.files_searched, "file", "files"), s.elapsed.as_secs_f64()));
                }
                t
            };
            ui.label(RichText::new(text).small().color(p.dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                if self.compact && small_icon_button(ui, icon::ARROW_SQUARE_OUT, "Open in a tab", "").clicked() {
                    let mut tab = FindView::default();
                    tab.copy_query(self);
                    tab.edited_at = Some(f64::NEG_INFINITY);
                    cx.open(Tab::Grep(Box::new(tab)));
                }
                let all_closed = !self.files.is_empty() && self.collapsed.len() >= self.files.len();
                let (glyph, name) = if all_closed { (icon::ARROWS_OUT_LINE_VERTICAL, "Expand all") } else { (icon::ARROWS_IN_LINE_VERTICAL, "Collapse all") };
                if small_icon_button(ui, glyph, name, "").clicked() {
                    if all_closed {
                        self.collapsed.clear();
                    } else {
                        self.collapsed = self.files.iter().map(|f| f.path.clone()).collect();
                    }
                    self.rows_dirty = true;
                }
                if small_icon_button(ui, icon::ARROW_CLOCKWISE, "Search again", "").clicked() {
                    self.edited_at = Some(f64::NEG_INFINITY);
                }
            });
        });
        let Some(s) = self.summary.clone().filter(|_| !searching) else { return };
        if s.limit_hit {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Showing the first {}.", grouped(textsearch::DEFAULT_MAX_MATCHES))).small().color(p.warn));
                if ui.link(RichText::new("Search all").small()).clicked() {
                    self.all = true;
                    self.edited_at = Some(f64::NEG_INFINITY);
                }
            });
        }
        if s.skipped_large > 0 {
            let mb = textsearch::DEFAULT_MAX_FILE_SIZE / (1024 * 1024);
            ui.label(RichText::new(format!("{} over {mb} MB left out.", plural(s.skipped_large, "file", "files"))).small().color(p.faint));
        }
        if !s.errors.is_empty() {
            egui::CollapsingHeader::new(RichText::new(format!("{} couldn't be read", plural(s.errors.len(), "file", "files"))).small().color(p.warn)).id_salt(("find-errors", self.id)).show(ui, |ui| {
                for e in &s.errors {
                    ui.label(RichText::new(e).small().monospace().color(p.dim));
                }
            });
        }
    }

    /// The same search, from another view.
    fn copy_query(&mut self, from: &FindView) {
        self.text = from.text.clone();
        self.regex = from.regex;
        self.case_sensitive = from.case_sensitive;
        self.whole_word = from.whole_word;
        self.include = from.include.clone();
        self.exclude = from.exclude.clone();
        self.include_ignored = from.include_ignored;
        self.within = from.within;
        self.folder = from.folder.clone();
        self.rev = from.rev.clone();
        self.details = from.details;
    }

    fn results(&mut self, ui: &mut Ui, cx: &mut Cx, p: &Palette) {
        if self.shown_for.is_none() && self.running.is_none() {
            if self.error.is_none() {
                let hint = if self.compact {
                    "Type to search the text of every file. What .gitignore leaves out is skipped."
                } else {
                    "Results come as you type. What .gitignore leaves out is skipped. Alt+C, Alt+W and Alt+R switch case, whole words and regular expressions; F4 goes to the next result."
                };
                empty_state(ui, "Find in files", hint, p);
            }
            return;
        }
        if self.files.is_empty() {
            if self.running.is_none() && self.summary.is_some() {
                let hint = if !self.include_ignored && self.within != Within::Revision {
                    "Check the spelling and the switches beside the box. Files .gitignore leaves out aren't searched: ⋯ can include them."
                } else {
                    "Check the spelling and the switches beside the box."
                };
                empty_state(ui, "No results", hint, p);
            }
            return;
        }
        if std::mem::take(&mut self.rows_dirty) {
            self.rows.clear();
            for (f, file) in self.files.iter().enumerate() {
                self.rows.push(Row::File(f));
                if !self.collapsed.contains(&file.path) {
                    self.rows.extend((0..file.lines.len()).map(|l| Row::Line(f, l)));
                }
            }
        }
        if self.marks.as_ref().is_none_or(|(g, _)| *g != cx.data.status_gen) {
            let marks = cx.status().map(|s| s.entries.iter().filter_map(|e| Some((e.path.clone(), mark_for(e)?))).collect()).unwrap_or_default();
            self.marks = Some((cx.data.status_gen, Arc::new(marks)));
        }
        let marks = self.marks.as_ref().map(|(_, m)| m.clone()).unwrap_or_default();
        let picked_row = self.selected.as_ref().and_then(|(path, line)| {
            self.rows.iter().position(|r| match *r {
                Row::File(f) => line.is_none() && &self.files[f].path == path,
                Row::Line(f, l) => line.is_some_and(|n| self.files[f].lines[l].line == n) && &self.files[f].path == path,
            })
        });
        let mut area = egui::ScrollArea::vertical().id_salt(("find-results", self.id)).auto_shrink([false, false]);
        if std::mem::take(&mut self.reveal)
            && let Some(i) = picked_row
        {
            area = area.vertical_scroll_offset((i as f32 * ROW_H - ui.available_height() / 2.0).max(0.0));
        }
        let rows = std::mem::take(&mut self.rows);
        let widest = self.files.iter().flat_map(|f| f.lines.iter().map(|l| l.line)).max().unwrap_or(1);
        let number_w = widest.to_string().len() as f32 * 7.0 + 10.0;
        let mut clicked: Option<(Row, bool)> = None;
        let mut toggled = None;
        let mut dismissed = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
                for i in range {
                    let row = rows[i];
                    let selected = picked_row == Some(i);
                    match row {
                        Row::File(f) => {
                            let file = &self.files[f];
                            let open = !self.collapsed.contains(&file.path);
                            let resp = file_row(ui, file, open, marks.get(&file.path).copied(), selected, (self.id, i), p);
                            if resp.double_clicked() {
                                clicked = Some((Row::Line(f, 0), true));
                            } else if resp.clicked() {
                                clicked = Some((row, false));
                                toggled = Some(file.path.clone());
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Open file").clicked() {
                                    clicked = Some((Row::Line(f, 0), true));
                                    ui.close();
                                }
                                if ui.button("Copy path").clicked() {
                                    cx.copy(file.path.clone());
                                    ui.close();
                                }
                                if ui.button("Dismiss").on_hover_text("Take this file out of the results").clicked() {
                                    dismissed = Some(f);
                                    ui.close();
                                }
                            });
                        }
                        Row::Line(f, l) => {
                            let file = &self.files[f];
                            let m = &file.lines[l];
                            let resp = line_row(ui, m, number_w, selected, (self.id, i), p);
                            if resp.double_clicked() {
                                clicked = Some((row, true));
                            } else if resp.clicked() {
                                clicked = Some((row, false));
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Open").clicked() {
                                    clicked = Some((row, true));
                                    ui.close();
                                }
                                if ui.button("Copy line").clicked() {
                                    cx.copy(m.preview.clone());
                                    ui.close();
                                }
                                if ui.button("Copy path and line").clicked() {
                                    cx.copy(format!("{}:{}", file.path, m.line));
                                    ui.close();
                                }
                            });
                        }
                    }
                }
            });
        });
        self.rows = rows;
        if clicked.is_some() {
            ui.ctx().data_mut(|d| d.insert_temp(owner_id(), self.id));
        }
        if let Some(path) = toggled {
            if !self.collapsed.remove(&path) {
                self.collapsed.insert(path);
            }
            self.rows_dirty = true;
        }
        match clicked {
            Some((Row::File(f), _)) => self.selected = Some((self.files[f].path.clone(), None)),
            Some((Row::Line(f, l), keep)) => {
                self.open(ui.ctx(), cx, f, l, keep);
                self.selected = Some((self.files[f].path.clone(), Some(self.files[f].lines[l].line)));
            }
            None => {}
        }
        if let Some(f) = dismissed {
            self.files.remove(f);
            self.selected = None;
            self.rows_dirty = true;
        }
    }
}

/// A file in the results: open or closed, its name, its folder, what git says, how many.
fn file_row(ui: &mut Ui, file: &FileMatches, open: bool, mark: Option<Mark>, selected: bool, key: (u64, usize), p: &Palette) -> egui::Response {
    let (rect, resp) = keyed_row(ui, ROW_H, selected, p, ("find-row", key));
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, file.path.as_str()));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let y = rect.center().y;
    let x = rect.left() + 4.0;
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.text(pos2(x + 6.0, y), Align2::CENTER_CENTER, if open { icon::CARET_DOWN } else { icon::CARET_RIGHT }, prop(10.5), p.faint);
    let (name, folder) = file.path.rsplit_once('/').map_or((file.path.as_str(), ""), |(d, n)| (n, d));
    let tint = mark.map_or(p.faint, |m| status_color(m.status, p));
    painter.text(pos2(x + 21.0, y), Align2::CENTER_CENTER, file_glyph(name), prop(14.0), tint);
    // The count and the mark on the right, then the name in what's left.
    let count = painter.layout_no_wrap(grouped(file.matches), prop(11.0), p.text);
    let badge = Rect::from_center_size(pos2(rect.right() - 12.0 - count.size().x / 2.0, y), vec2(count.size().x + 12.0, 16.0));
    painter.rect_filled(badge, 8.0, p.selection);
    painter.galley(badge.center() - count.size() / 2.0, count, p.text);
    let mut right = badge.left() - 6.0;
    if let Some(m) = mark {
        let g = painter.text(pos2(right, y), Align2::RIGHT_CENTER, m.letter.to_string(), mono(11.0), status_color(m.status, p));
        right = g.left() - 6.0;
    }
    if file.unsaved {
        let g = painter.text(pos2(right, y), Align2::RIGHT_CENTER, "●", prop(10.0), p.warn);
        right = g.left() - 6.0;
    }
    let text = ui.painter().with_clip_rect(Rect::from_min_max(pos2(x + 31.0, rect.top()), pos2(right, rect.bottom())).intersect(ui.clip_rect()));
    let n = text.text(pos2(x + 31.0, y), Align2::LEFT_CENTER, name, prop(12.5), mark.map_or(p.text, |m| status_color(m.status, p)));
    if !folder.is_empty() {
        text.text(pos2(n.right() + 6.0, y), Align2::LEFT_CENTER, folder, prop(11.0), p.faint);
    }
    if file.unsaved { resp.on_hover_text("Searched with the unsaved edits in its editor") } else { resp }
}

/// A line with matches on it: its number, and the text with the matches marked.
fn line_row(ui: &mut Ui, m: &LineMatch, number_w: f32, selected: bool, key: (u64, usize), p: &Palette) -> egui::Response {
    let (rect, resp) = keyed_row(ui, ROW_H, selected, p, ("find-row", key));
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{}: {}", m.line, m.preview)));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let y = rect.center().y;
    let left = rect.left() + 22.0;
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.text(pos2(left + number_w - 8.0, y), Align2::RIGHT_CENTER, m.line.to_string(), mono(11.0), p.faint);
    let galley = painter.layout_job(preview_job(m, p));
    let text_left = left + number_w;
    let clip = Rect::from_min_max(pos2(text_left, rect.top()), pos2(rect.right() - 4.0, rect.bottom())).intersect(ui.clip_rect());
    ui.painter().with_clip_rect(clip).galley(pos2(text_left, y - galley.size().y / 2.0), galley, p.text);
    resp
}

/// The preview's text, with the matches on a background of their own.
fn preview_job(m: &LineMatch, p: &Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let plain = TextFormat { font_id: prop(12.5), color: p.text, ..Default::default() };
    let hit = TextFormat { background: p.warn.gamma_multiply(0.35), ..plain.clone() };
    if m.cut_before {
        job.append("… ", 0.0, TextFormat { color: p.faint, ..plain.clone() });
    }
    let text = m.preview.as_str();
    let mut at = 0;
    for r in &m.ranges {
        let (a, b) = (r.start as usize, (r.end as usize).min(text.len()));
        if a < at || a > b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        job.append(&text[at..a], 0.0, plain.clone());
        job.append(&text[a..b], 0.0, hit.clone());
        at = b;
    }
    job.append(&text[at..], 0.0, plain.clone());
    if m.cut_after {
        job.append(" …", 0.0, TextFormat { color: p.faint, ..plain });
    }
    job
}

/// Which files to look in, as a row of choices.
fn scope_pills(ui: &mut Ui, within: Within, compact: bool, p: &Palette) -> Option<Within> {
    let mut picked = None;
    for w in Within::ALL {
        let on = w == within;
        let g = ui.painter().layout_no_wrap(w.name(compact).to_string(), prop(12.0), p.text);
        let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, ROW_H), Sense::click());
        resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, w.name(false)));
        let (fill, stroke, color) = if on {
            (p.accent.gamma_multiply(0.18), p.accent, p.text)
        } else if resp.hovered() {
            (p.hover, p.border, p.text)
        } else {
            (Color32::TRANSPARENT, p.border, p.dim)
        };
        ui.painter().rect(rect, ROW_H / 2.0, fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, w.name(compact), prop(12.0), color);
        focus_ring(ui, &resp, p);
        if resp.on_hover_text(w.tip()).clicked() && !on {
            picked = Some(w);
        }
    }
    picked
}

/// A small switch beside a box, as VS Code has for case and regular expressions.
fn option_toggle(ui: &mut Ui, on: &mut bool, glyph: &str, name: &str, keys: &str, underline: bool, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ROW_H, ROW_H), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, name));
    if ui.is_rect_visible(rect) {
        if *on {
            ui.painter().rect(rect, 4.0, p.accent.gamma_multiply(0.22), Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 4.0, p.hover);
        }
        let color = if *on || resp.hovered() { p.text } else { p.dim };
        let g = ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, prop(13.0), color);
        if underline {
            ui.painter().hline(g.x_range().shrink(1.0), g.bottom() - 1.0, Stroke::new(1.0, color));
        }
        focus_ring(ui, &resp, p);
    }
    let tip = if keys.is_empty() { name.to_string() } else { format!("{name} ({keys})") };
    if resp.on_hover_text(tip).clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// Two lists in order by path, as one.
fn merge_by_path(a: Vec<FileMatches>, b: Vec<FileMatches>) -> Vec<FileMatches> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    loop {
        let take_a = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => x.path <= y.path,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return out,
        };
        out.extend(if take_a { a.next() } else { b.next() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileMatches {
        FileMatches { path: path.into(), lines: Vec::new(), matches: 1, unsaved: false }
    }

    #[test]
    fn batches_merge_in_order_by_path() {
        let paths = |v: &[FileMatches]| v.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        let merged = merge_by_path(vec![file("a"), file("c/x"), file("e")], vec![file("b"), file("c/w"), file("f")]);
        assert_eq!(paths(&merged), ["a", "b", "c/w", "c/x", "e", "f"]);
        assert_eq!(paths(&merge_by_path(Vec::new(), vec![file("z")])), ["z"]);
    }

    #[test]
    fn a_preview_marks_its_matches_and_where_it_was_cut() {
        let p = Palette::for_settings(gitem_core::settings::ThemeChoice::Dark, "");
        let m = LineMatch { line: 3, column: 4, preview: "let needle = needle;".into(), ranges: vec![4..10, 13..19], cut_before: true, cut_after: false };
        let job = preview_job(&m, &p);
        let marked: Vec<&str> = job.sections.iter().filter(|s| s.format.background != Color32::TRANSPARENT).map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
        assert_eq!(marked, ["needle", "needle"]);
        assert!(job.text.starts_with("… let"));
        // A range that isn't on a character's edge is left unmarked, not a panic.
        let odd = LineMatch { preview: "é".into(), ranges: std::iter::once(1..2).collect(), cut_before: false, ..m };
        assert_eq!(preview_job(&odd, &p).text, "é");
    }
}
