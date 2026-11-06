//! File viewer with annotations: full blame gutter, heatmap, working-tree change
//! bars, current-line inline blame, code lens, find, and revision navigation.
use std::collections::HashMap;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use gitem_core::diff::LineKind;
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::{BlameOpts, DiffOpts, DiffSource};
use gitem_core::util;
use regex::Regex;

use crate::action::{Action, Cx};
use crate::menus;
use crate::syntax::{self, Spans};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Annot {
    None,
    Blame,
    Heatmap,
    Changes,
}

struct FileData {
    lines: Vec<String>,
    binary: bool,
    syntax: Vec<Spans>,
    blame: Vec<BlameLine>,
    /// 0-based new-file line -> change kind compared with HEAD.
    changes: HashMap<usize, LineKind>,
    /// Deleted lines sit between lines; recorded by the following line.
    deletions: HashMap<usize, usize>,
    newest: i64,
    oldest: i64,
    lens: Vec<Lens>,
}

/// A code-lens entry: authorship summary above a symbol.
struct Lens {
    line: usize,
    end: usize,
    text: String,
    sha: String,
}

enum Row {
    Lens(usize),
    Line(usize),
}

pub struct FileView {
    pub path: String,
    pub rev: Option<String>,
    target_line: Option<u32>,
    data: Task<FileData>,
    nav: Task<Option<(String, String)>>,
    /// Where "Older" came from, so "Newer" retraces it even across renames.
    trail: Vec<(Option<String>, String)>,
    annot: Annot,
    current: Option<usize>,
    selection_end: Option<usize>,
    loaded_key: String,
    loaded_gen: u64,
    find: String,
    find_open: bool,
    find_hits: Vec<usize>,
    find_pos: usize,
    scroll_to: Option<usize>,
    ignore_ws: bool,
    moves: bool,
    code_lens: Option<bool>,
}

fn symbol_regex() -> Regex {
    Regex::new(r"^\s*(pub(\([a-z]+\))?\s+)?(async\s+)?(export\s+)?(default\s+)?(public\s+|private\s+|protected\s+|internal\s+|static\s+)*(fn|func|function|def|class|struct|enum|trait|impl|interface|module|namespace|type|sub)\s+\w+").expect("valid regex")
}

fn build_lenses(lines: &[String], blame: &[BlameLine]) -> Vec<Lens> {
    if blame.is_empty() {
        return Vec::new();
    }
    let re = symbol_regex();
    let mut starts: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let now = util::now_ts();
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(lines.len());
        let block = blame.get(s..e.min(blame.len())).unwrap_or(&[]);
        let committed: Vec<&BlameLine> = block.iter().filter(|b| !b.is_uncommitted()).collect();
        let Some(latest) = committed.iter().max_by_key(|b| b.author_time) else { continue };
        let mut authors: Vec<&str> = committed.iter().map(|b| b.author.as_str()).collect();
        authors.sort_unstable();
        authors.dedup();
        let who = if authors.len() == 1 { authors[0].to_string() } else { format!("{} and {} more", latest.author, authors.len() - 1) };
        out.push(Lens { line: s, end: e, text: format!("{} · {} · {} author{}", util::relative_time(latest.author_time, now), who, authors.len(), if authors.len() == 1 { "" } else { "s" }), sha: latest.sha.clone() });
    }
    out
}

impl FileView {
    pub fn new(path: &str, rev: Option<String>, line: Option<u32>) -> Self {
        Self {
            path: path.to_string(),
            rev,
            target_line: line,
            data: Task::default(),
            nav: Task::default(),
            trail: Vec::new(),
            annot: Annot::Blame,
            current: None,
            selection_end: None,
            loaded_key: String::new(),
            loaded_gen: u64::MAX,
            find: String::new(),
            find_open: false,
            find_hits: Vec::new(),
            find_pos: 0,
            scroll_to: None,
            ignore_ws: false,
            moves: false,
            code_lens: None,
        }
    }

    pub fn key(&self) -> String {
        format!("file:{}:{:?}", self.path, self.rev)
    }

    pub fn title(&self) -> String {
        let leaf = self.path.rsplit('/').next().unwrap_or(&self.path);
        match &self.rev {
            Some(r) => format!("{leaf} @ {}", gitem_core::util::prefix(r, 7)),
            None => leaf.to_string(),
        }
    }

    fn load(&mut self, ctx: &egui::Context, cx: &Cx) {
        let repo = cx.repo.clone();
        let (path, rev) = (self.path.clone(), self.rev.clone());
        let opts = BlameOpts { ignore_ws: self.ignore_ws, moves: self.moves, copies: false };
        let dark = cx.pal.dark;
        self.data.spawn(ctx, move || -> Result<FileData, String> {
            let bytes = match &rev {
                Some(r) => repo.file_at(r, &path),
                None => repo.worktree_file(&path),
            }
            .map_err(|e| e.to_string())?;
            let binary = util::is_binary(&bytes);
            if binary {
                return Ok(FileData { lines: Vec::new(), binary, syntax: Vec::new(), blame: Vec::new(), changes: HashMap::new(), deletions: HashMap::new(), newest: 0, oldest: 0, lens: Vec::new() });
            }
            let text = util::text(&bytes);
            let lines = util::split_lines(&text);
            let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            let syntax = syntax::highlight(&path, &refs, dark);
            // An untracked file simply has no blame; everything else surfaces errors.
            let blame = repo.blame(&path, rev.as_deref(), opts).unwrap_or_default();
            let (mut changes, mut deletions) = (HashMap::new(), HashMap::new());
            if rev.is_none() {
                let opts = DiffOpts { context: 0, ..Default::default() };
                if let Ok(d) = repo.diff(&DiffSource::WorkingVsHead, Some(&path), &opts) {
                    for f in d {
                        for h in f.hunks {
                            for l in h.lines {
                                match (l.kind, l.new_no) {
                                    (LineKind::Add, Some(n)) => {
                                        changes.insert(n as usize - 1, LineKind::Add);
                                    }
                                    (LineKind::Del, _) => {
                                        *deletions.entry((h.new_start as usize).saturating_sub(if h.new_count == 0 { 0 } else { 1 })).or_insert(0) += 1;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            let committed = blame.iter().filter(|b| !b.is_uncommitted());
            let newest = committed.clone().map(|b| b.author_time).max().unwrap_or(0);
            let oldest = committed.map(|b| b.author_time).min().unwrap_or(0);
            let lens = build_lenses(&lines, &blame);
            Ok(FileData { lines, binary, syntax, blame, changes, deletions, newest, oldest, lens })
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = cx.pal.clone();
        let code_lens = *self.code_lens.get_or_insert(cx.settings.code_lens);
        if self.data.poll() {
            if let Some(l) = self.target_line.take() {
                self.current = Some((l as usize).saturating_sub(1));
                self.scroll_to = self.current;
            }
            self.update_find();
        }
        if self.nav.poll() {
            match self.nav.value.take() {
                Some(Some((sha, path))) => {
                    self.trail.push((self.rev.clone(), self.path.clone()));
                    self.rev = Some(sha);
                    self.path = path;
                }
                Some(None) => cx.toast("No more revisions of this file in that direction."),
                None => {}
            }
        }
        let key = format!("{:?}{}{}{}", self.rev, self.ignore_ws, self.moves, pal.dark);
        // Worktree files follow edits; revisions are immutable.
        let stale_worktree = self.rev.is_none() && self.loaded_gen != cx.data.status_gen;
        if key != self.loaded_key || stale_worktree {
            self.loaded_key = key;
            self.loaded_gen = cx.data.status_gen;
            self.load(&ui.ctx().clone(), cx);
        }
        if ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::B)) {
            self.annot = if self.annot == Annot::Blame { Annot::None } else { Annot::Blame };
        }
        self.toolbar(ui, cx);
        ui.separator();
        if let Some(e) = &self.data.error {
            error_box(ui, e, &pal);
        }
        let Some(d) = self.data.value.take() else {
            spinner_row(ui, "Loading file and blame…", &pal);
            return;
        };
        if d.binary {
            empty_state(ui, "Binary file", "This file can't be shown as text.", &pal);
        } else {
            self.lines(ui, cx, &d, code_lens);
        }
        self.data.value = Some(d);
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = cx.pal.clone();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.annot, Annot::Blame, "Blame");
            ui.selectable_value(&mut self.annot, Annot::Heatmap, "Heatmap");
            ui.selectable_value(&mut self.annot, Annot::Changes, "Changes");
            ui.selectable_value(&mut self.annot, Annot::None, "None");
            ui.separator();
            let mut lens = self.code_lens.unwrap_or(true);
            if toggle(ui, &mut lens, "Lens", "Show recent-change summaries above symbols") {
                self.code_lens = Some(lens);
            }
            ui.menu_button("Blame options ▾", |ui| {
                ui.checkbox(&mut self.ignore_ws, "Ignore whitespace (-w)");
                ui.checkbox(&mut self.moves, "Detect moved lines (-M)");
            });
            ui.separator();
            let repo = cx.repo.clone();
            let path = self.path.clone();
            let cur = self.rev.clone().unwrap_or_else(|| "HEAD".to_string());
            if ui.small_button("◀ Older").on_hover_text("Previous revision of this file").clicked() {
                let at_worktree = self.rev.is_none();
                let c = cur.clone();
                let p = path.clone();
                self.nav.spawn(ui.ctx(), move || -> Result<Option<(String, String)>, String> {
                    if at_worktree {
                        let found = repo.log(&LogQuery { revs: vec!["HEAD".into()], paths: vec![p.clone()], limit: 1, literal_paths: true, ..Default::default() }).map_err(|e| e.to_string())?;
                        return Ok(found.first().map(|c| (c.sha.clone(), p.clone())));
                    }
                    Ok(repo.prev_revision(&p, &c))
                });
            }
            let repo = cx.repo.clone();
            if ui.add_enabled(self.rev.is_some(), egui::Button::new("Newer ▶").small()).on_hover_text("Next revision of this file").clicked() {
                if let Some((rev, p)) = self.trail.pop() {
                    self.rev = rev;
                    self.path = p;
                } else {
                    let c = cur.clone();
                    let p = path.clone();
                    self.nav.spawn(ui.ctx(), move || -> Result<Option<(String, String)>, String> { Ok(repo.next_revision(&p, &c).map(|s| (s, p.clone()))) });
                }
            }
            if self.rev.is_some() && ui.small_button("Working tree").clicked() {
                if let Some((_, first_path)) = self.trail.first() {
                    self.path = first_path.clone();
                }
                self.trail.clear();
                self.rev = None;
            }
            ui.separator();
            if ui.small_button("History").clicked() {
                cx.open(Tab::file_history(&self.path));
            }
            if ui.small_button("Visual").clicked() {
                cx.open(Tab::visual(&self.path));
            }
            if let Some((a, b)) = self.selected_range()
                && ui.small_button(format!("Line history {}-{}", a + 1, b + 1)).clicked() {
                    cx.open(Tab::line_history(&self.path, a as u32 + 1, b as u32 + 1));
                }
            ui.menu_button("Compare ▾", |ui| self.compare_menu(ui, cx));
            ui.menu_button("More ▾", |ui| menus::file_menu(ui, cx, &self.path, self.rev.as_deref()));
            if ui.small_button("Find").on_hover_text("Ctrl+F").clicked() || ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::F)) {
                self.find_open = !self.find_open;
            }
            if self.data.busy() || self.nav.busy() {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.path).strong());
            if let Some(r) = &self.rev {
                chip(ui, &format!("at {}", gitem_core::util::prefix(r, 8)), pal.series[3]);
            }
        });
        if self.find_open {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.find).hint_text("Find in file").desired_width(240.0));
                if r.changed() {
                    self.update_find();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.find_hits.is_empty() {
                    self.find_pos = (self.find_pos + 1) % self.find_hits.len();
                    self.scroll_to = Some(self.find_hits[self.find_pos]);
                    self.current = self.scroll_to;
                    r.request_focus();
                }
                ui.label(RichText::new(if self.find_hits.is_empty() { "No matches".to_string() } else { format!("{} of {}", self.find_pos + 1, self.find_hits.len()) }).color(pal.dim));
                if ui.small_button("✖").clicked() {
                    self.find_open = false;
                }
            });
        }
    }

    fn compare_menu(&self, ui: &mut Ui, cx: &mut Cx) {
        let cur = self.rev.clone().unwrap_or_else(|| "HEAD".to_string());
        if ui.button("With previous revision").clicked() {
            match cx.repo.prev_revision(&self.path, &cur) {
                Some((prev, _)) => cx.diff(DiffSource::Range(prev, cur.clone()), Some(&self.path)),
                None => cx.toast("This is the first revision of the file."),
            }
            ui.close();
        }
        if ui.button("With next revision").clicked() {
            match cx.repo.next_revision(&self.path, &cur) {
                Some(next) => cx.diff(DiffSource::Range(cur.clone(), next), Some(&self.path)),
                None => cx.toast("This is the latest revision of the file."),
            }
            ui.close();
        }
        if ui.button("With working tree").clicked() {
            cx.diff(DiffSource::WorkingVs(cur.clone()), Some(&self.path));
            ui.close();
        }
        ui.separator();
        ui.label("With a branch or tag");
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            for r in cx.data.ref_names() {
                if ui.button(r.as_str()).clicked() {
                    cx.diff(DiffSource::Range(r.clone(), cur.clone()), Some(&self.path));
                    ui.close();
                }
            }
        });
    }

    fn update_find(&mut self) {
        self.find_hits.clear();
        self.find_pos = 0;
        let q = self.find.to_lowercase();
        if q.is_empty() {
            return;
        }
        if let Some(d) = self.data.get() {
            self.find_hits = d.lines.iter().enumerate().filter(|(_, l)| l.to_lowercase().contains(&q)).map(|(i, _)| i).collect();
        }
        if let Some(&first) = self.find_hits.first() {
            self.scroll_to = Some(first);
        }
    }

    fn selected_range(&self) -> Option<(usize, usize)> {
        let a = self.current?;
        let b = self.selection_end.unwrap_or(a);
        Some((a.min(b), a.max(b)))
    }

    fn lines(&mut self, ui: &mut Ui, cx: &mut Cx, d: &FileData, code_lens: bool) {
        let pal = cx.pal.clone();
        let font = cx.settings.editor_font_size;
        let row_h = (font * 1.5).ceil();
        let gutter_annot = match self.annot {
            Annot::Blame => 240.0,
            Annot::Heatmap => 10.0,
            Annot::Changes => 6.0,
            Annot::None => 0.0,
        };
        let digits = (d.lines.len().max(1).ilog10() + 1) as f32;
        let char_w = ui.fonts_mut(|f| f.glyph_width(&mono(font), '0'));
        let lineno_w = digits * char_w + 16.0;
        let text_x = gutter_annot + lineno_w + 6.0;

        let mut rows: Vec<Row> = Vec::with_capacity(d.lines.len() + d.lens.len());
        let mut li = 0;
        for i in 0..d.lines.len() {
            if code_lens {
                while li < d.lens.len() && d.lens[li].line == i {
                    rows.push(Row::Lens(li));
                    li += 1;
                }
            }
            rows.push(Row::Line(i));
        }
        let max_chars = d.lines.iter().map(|l| l.len()).max().unwrap_or(0);
        let width = (text_x + max_chars as f32 * char_w + 320.0).max(ui.available_width() - 8.0);
        let mut sa = egui::ScrollArea::both().id_salt(("file", &self.path)).auto_shrink([false, false]);
        if let Some(t) = self.scroll_to.take() {
            let pos = rows.iter().position(|r| matches!(r, Row::Line(i) if *i == t)).unwrap_or(0);
            sa = sa.vertical_scroll_offset((pos as f32 * row_h - 80.0).max(0.0));
        }
        let span = (d.newest - d.oldest).max(1) as f32;
        let now = util::now_ts();
        let inline_on = cx.settings.inline_blame;
        let fmt = cx.settings.inline_blame_format.clone();
        // Report the current line's blame to the status bar.
        let status_text = self.current.and_then(|c| d.blame.get(c)).map(|b| format_blame(&fmt, b, now)).unwrap_or_default();
        if cx.data.last_blame != status_text {
            cx.out.push(Action::StatusBlame(status_text));
        }

        let sel = self.selected_range();
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.set_min_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            for ri in range {
                match rows[ri] {
                    Row::Lens(k) => {
                        let lens = &d.lens[k];
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                        let col = if resp.hovered() { pal.accent } else { pal.faint };
                        paint_text(ui, Rect::from_min_max(pos2(rect.left() + text_x, rect.top()), rect.max), &lens.text, prop(font - 2.0), col);
                        let resp = resp.on_hover_text("Show the most recent commit in this block. Right-click for its line history.");
                        if resp.clicked() {
                            cx.inspect(&lens.sha);
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Line history of this block").clicked() {
                                cx.open(Tab::line_history(&self.path, lens.line as u32 + 1, lens.end as u32));
                                ui.close();
                            }
                            if ui.button("Open most recent commit").clicked() {
                                cx.diff(DiffSource::Commit(lens.sha.clone()), Some(&self.path));
                                ui.close();
                            }
                        });
                    }
                    Row::Line(i) => self.line_row(ui, cx, d, i, width, row_h, gutter_annot, lineno_w, text_x, font, span, now, sel, inline_on, &fmt),
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn line_row(&mut self, ui: &mut Ui, cx: &mut Cx, d: &FileData, i: usize, width: f32, row_h: f32, gutter: f32, lineno_w: f32, text_x: f32, font: f32, span: f32, now: i64, sel: Option<(usize, usize)>, inline_on: bool, fmt: &str) {
        let pal = cx.pal.clone();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
        let blame = d.blame.get(i);
        let selected = sel.is_some_and(|(a, b)| i >= a && i <= b);
        if selected {
            ui.painter().rect_filled(rect, 0.0, pal.selection.gamma_multiply(0.7));
        } else if d.changes.contains_key(&i) && self.annot != Annot::None {
            ui.painter().rect_filled(rect, 0.0, pal.add_bg);
        } else if self.find_hits.binary_search(&i).is_ok() {
            ui.painter().rect_filled(rect, 0.0, pal.warn.gamma_multiply(0.2));
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, pal.hover.gamma_multiply(0.5));
        }

        match self.annot {
            Annot::Blame => {
                if let Some(b) = blame {
                    let gr = Rect::from_min_size(rect.min, vec2(gutter, row_h));
                    let t = if b.is_uncommitted() { 1.0 } else { (b.author_time - d.oldest) as f32 / span };
                    ui.painter().rect_filled(Rect::from_min_size(gr.min, vec2(3.0, row_h)), 0.0, pal.heat(t));
                    // Text only at the start of a run of lines from one commit.
                    let first = i == 0 || d.blame.get(i - 1).is_none_or(|p| p.sha != b.sha);
                    if first {
                        ui.painter().line_segment([gr.left_top(), rect.right_top()], Stroke::new(1.0, pal.border.gamma_multiply(0.5)));
                        if b.is_uncommitted() {
                            paint_text(ui, Rect::from_min_size(gr.min + vec2(9.0, 0.0), vec2(gutter - 12.0, row_h)), "Not committed yet", prop(font - 2.0), pal.warn);
                        } else {
                            paint_avatar(ui, pos2(gr.left() + 15.0, gr.center().y), 7.0, &b.author, &b.author_email, &pal, cx.settings.gravatar);
                            let date = util::relative_time(b.author_time, now);
                            let text = format!("{} · {}", b.author, date);
                            paint_text(ui, Rect::from_min_size(gr.min + vec2(26.0, 0.0), vec2(gutter - 28.0, row_h)), &text, prop(font - 2.0), pal.dim);
                        }
                    }
                    let gresp = ui.interact(gr, ui.id().with(("blame", i)), Sense::click());
                    if !b.is_uncommitted() {
                        let gresp = gresp.on_hover_ui(|ui| {
                            commit_card(ui, &b.sha, &b.author, &b.author_email, b.author_time, &b.summary, &pal, cx.settings.gravatar);
                        });
                        if gresp.clicked() {
                            cx.inspect(&b.sha);
                        }
                        gresp.context_menu(|ui| self.blame_menu(ui, cx, b));
                    }
                }
            }
            Annot::Heatmap => {
                if let Some(b) = blame {
                    let t = if b.is_uncommitted() { 1.0 } else { (b.author_time - d.oldest) as f32 / span };
                    let r = Rect::from_min_size(rect.min, vec2(gutter - 2.0, row_h));
                    ui.painter().rect_filled(r, 0.0, pal.heat(t));
                    if !b.is_uncommitted() {
                        let g = ui.interact(r, ui.id().with(("heat", i)), Sense::click()).on_hover_ui(|ui| commit_card(ui, &b.sha, &b.author, &b.author_email, b.author_time, &b.summary, &pal, cx.settings.gravatar));
                        if g.clicked() {
                            cx.inspect(&b.sha);
                        }
                    }
                }
            }
            Annot::Changes => {
                if let Some(k) = d.changes.get(&i) {
                    ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(gutter - 2.0, row_h)), 0.0, if *k == LineKind::Add { pal.add_fg } else { pal.series[3] });
                }
            }
            Annot::None => {}
        }
        if self.annot != Annot::None
            && let Some(n) = d.deletions.get(&i) {
                let y = rect.top();
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![pos2(rect.left() + gutter, y - 3.0), pos2(rect.left() + gutter + 6.0, y), pos2(rect.left() + gutter, y + 3.0)],
                    pal.del_fg,
                    Stroke::NONE,
                ));
                let id = ui.id().with(("del", i));
                ui.interact(Rect::from_center_size(pos2(rect.left() + gutter + 3.0, y), vec2(8.0, 8.0)), id, Sense::hover()).on_hover_text(format!("{n} line(s) deleted here"));
            }

        // Line number
        ui.painter().text(pos2(rect.left() + gutter + lineno_w - 8.0, rect.center().y), Align2::RIGHT_CENTER, (i + 1).to_string(), mono(font - 1.0), if selected { pal.text } else { pal.faint });

        // Code
        let line = &d.lines[i];
        let job = code_job(line, font, pal.text, d.syntax.get(i), None, Color32::TRANSPARENT);
        let g = ui.painter().layout_job(job);
        let code_w = g.size().x;
        let code_rect = Rect::from_min_max(pos2(rect.left() + text_x, rect.top()), rect.max).intersect(ui.clip_rect());
        ui.painter().with_clip_rect(code_rect).galley(pos2(rect.left() + text_x, rect.center().y - g.size().y / 2.0), g, pal.text);

        // Inline blame on the current line.
        if inline_on && self.current == Some(i)
            && let Some(b) = blame {
                let s = format_blame(fmt, b, now);
                ui.painter().text(pos2(rect.left() + text_x + code_w + 24.0, rect.center().y), Align2::LEFT_CENTER, s, prop(font - 1.0), pal.faint);
            }

        if resp.clicked() {
            if ui.input(|x| x.modifiers.shift) && self.current.is_some() {
                self.selection_end = Some(i);
            } else {
                self.current = Some(i);
                self.selection_end = None;
            }
        }
        resp.context_menu(|ui| {
            self.current.get_or_insert(i);
            // The file may have shrunk since the selection was made.
            let last = d.lines.len().saturating_sub(1);
            let (a, b) = self.selected_range().map(|(a, b)| (a.min(last), b.min(last))).unwrap_or((i, i));
            if ui.button(format!("Line history ({}-{})", a + 1, b + 1)).clicked() {
                cx.open(Tab::line_history(&self.path, a as u32 + 1, b as u32 + 1));
                ui.close();
            }
            if ui.button("Copy selected lines").clicked() {
                cx.copy(d.lines[a..=b].join("\n"));
                ui.close();
            }
            if let Some(f) = cx.forge() {
                let url = f.file_url(self.rev.as_deref().unwrap_or(cx.status().map(|s| s.head_sha.as_str()).unwrap_or("HEAD")), &self.path, Some((a as u32 + 1, b as u32 + 1)));
                if ui.button("Open lines on remote").clicked() {
                    cx.url(url.clone());
                    ui.close();
                }
                if ui.button("Copy permalink").clicked() {
                    cx.copy(url);
                    ui.close();
                }
            }
            if let Some(bl) = d.blame.get(i).filter(|b| !b.is_uncommitted()) {
                ui.separator();
                self.blame_menu(ui, cx, bl);
            }
        });
    }

    fn blame_menu(&self, ui: &mut Ui, cx: &mut Cx, b: &BlameLine) {
        if ui.button("Show commit").clicked() {
            cx.inspect(&b.sha);
            ui.close();
        }
        if ui.button("Open changes for this file").clicked() {
            cx.diff(DiffSource::Commit(b.sha.clone()), Some(b.orig_path.as_deref().unwrap_or(&self.path)));
            ui.close();
        }
        if ui.button("Open this file at the commit").clicked() {
            cx.open(Tab::file(b.orig_path.as_deref().unwrap_or(&self.path), Some(b.sha.clone()), Some(b.orig_line)));
            ui.close();
        }
        if ui.button("Blame before this commit").clicked() {
            cx.open(Tab::file(b.orig_path.as_deref().unwrap_or(&self.path), Some(format!("{}^", b.sha)), Some(b.orig_line)));
            ui.close();
        }
        if ui.button("Restore this file from the commit…").clicked() {
            cx.confirm("Restore file", &format!("Replace {} with its version from {}?", self.path, gitem_core::util::prefix(&b.sha, 8)), "Restore", ops::restore_file_from(&b.sha, &self.path));
            ui.close();
        }
        if ui.button("Copy SHA").clicked() {
            cx.copy(b.sha.clone());
            ui.close();
        }
    }
}

pub fn format_blame(fmt: &str, b: &BlameLine, now: i64) -> String {
    if b.is_uncommitted() {
        return "You · Uncommitted changes".to_string();
    }
    fmt.replace("{author}", &b.author)
        .replace("{ago}", &util::relative_time(b.author_time, now))
        .replace("{date}", &util::format_date(b.author_time))
        .replace("{subject}", &b.summary)
        .replace("{sha}", gitem_core::util::prefix(&b.sha, 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_blame_format() {
        let b = BlameLine { sha: "abcdef1234".into(), author: "Ann".into(), summary: "Fix".into(), author_time: 0, ..Default::default() };
        assert_eq!(format_blame("{author} {sha} {subject}", &b, 10), "Ann abcdef12 Fix");
        let u = BlameLine { sha: "0000000".into(), ..Default::default() };
        assert!(format_blame("{author}", &u, 0).contains("Uncommitted"));
    }

    #[test]
    fn lens_blocks() {
        let lines: Vec<String> = ["use x;", "pub fn a() {", "}", "class B {", "}"].iter().map(|s| s.to_string()).collect();
        let bl = |a: &str, t| BlameLine { sha: "aaaa".into(), author: a.into(), author_time: t, ..Default::default() };
        let blame = vec![bl("Ann", 1), bl("Ann", 2), bl("Bob", 3), bl("Bob", 4), bl("Bob", 5)];
        let l = build_lenses(&lines, &blame);
        assert_eq!(l.iter().map(|x| x.line).collect::<Vec<_>>(), vec![0, 1, 3]);
        assert!(l[1].text.contains("2 authors"));
        assert!(l[2].text.contains("Bob"));
    }
}
