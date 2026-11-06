//! The visual diff tool: side-by-side or unified, syntax highlighting, word diff,
//! hunk and line staging, whitespace and algorithm options, image comparison.
use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, pos2, vec2};
use gitem_core::diff::{self, FileDiff, LineKind, PatchMode, SideRow};
use gitem_core::ops;
use gitem_core::repo::{DiffAlgo, DiffOpts, DiffSource, WsMode};

use crate::action::Cx;
use crate::syntax::{self, Spans};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

const FULL_CONTEXT: u32 = 100_000;
const MAX_INLINE_IMAGE: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Split,
    Unified,
}

/// Below this each half of a side-by-side diff has room for little but its line numbers.
const MIN_SPLIT_WIDTH: f32 = 560.0;

#[derive(Default)]
struct Prepared {
    side: Vec<SideRow>,
    /// (hunk, line index) -> changed byte ranges within that line.
    words: HashMap<(usize, usize), Vec<(usize, usize)>>,
    /// (hunk, line index) -> syntax colour spans.
    syntax: HashMap<(usize, usize), Spans>,
    /// The widest line as drawn, in characters. Worked out once: there can be many lines.
    max_chars: usize,
}

fn prepare(f: &FileDiff, dark: bool) -> Prepared {
    let side = diff::side_by_side(f);
    let mut words = HashMap::new();
    for r in &side {
        if let (Some(li), Some(ri), Some((_, lt)), Some((_, rt))) = (r.left_idx, r.right_idx, &r.left, &r.right)
            && r.left_kind == Some(LineKind::Del) && r.right_kind == Some(LineKind::Add) {
                let (o, n) = diff::word_diff(lt, rt);
                // A line replaced wholesale gains nothing from word highlights.
                if o.iter().map(|x| x.1 - x.0).sum::<usize>() * 10 < lt.len() * 9 || n.iter().map(|x| x.1 - x.0).sum::<usize>() * 10 < rt.len() * 9 {
                    words.insert((r.hunk, li), o);
                    words.insert((r.hunk, ri), n);
                }
            }
    }
    let mut syn = HashMap::new();
    let path = f.display_path().to_string();
    for (hi, h) in f.hunks.iter().enumerate() {
        let lines: Vec<&str> = h.lines.iter().map(|l| l.text.as_str()).collect();
        for (li, s) in syntax::highlight(&path, &lines, dark).into_iter().enumerate() {
            syn.insert((hi, li), s);
        }
    }
    let max_chars = side.iter().flat_map(|r| [r.left.as_ref(), r.right.as_ref()]).flatten().map(|x| shown_chars(&x.1)).max().unwrap_or(0);
    Prepared { side, words, syntax: syn, max_chars }
}

#[derive(Default)]
struct ImagePair {
    old: Option<egui::TextureHandle>,
    new: Option<egui::TextureHandle>,
    loaded_for: Option<String>,
}

pub struct DiffView {
    pub src: DiffSource,
    pub path: Option<String>,
    files: Task<Vec<(FileDiff, Prepared)>>,
    loaded_status_gen: u64,
    mode: Mode,
    /// How far the side-by-side text is scrolled sideways.
    split_x: f32,
    opts: DiffOpts,
    full_file: bool,
    selected_file: usize,
    selected_lines: HashSet<(usize, usize)>,
    scroll_to_row: Option<usize>,
    images: ImagePair,
    wrap_key: String,
    dark: Option<bool>,
    needs_load: bool,
    file_filter: String,
    /// Unpinned diffs are replaced by the next one opened from a list.
    pub pinned: bool,
}

impl DiffView {
    pub fn new(src: DiffSource, path: Option<String>) -> Self {
        Self {
            src,
            path,
            files: Task::default(),
            loaded_status_gen: u64::MAX,
            mode: Mode::Split,
            split_x: 0.0,
            opts: DiffOpts::default(),
            full_file: false,
            selected_file: 0,
            selected_lines: HashSet::new(),
            scroll_to_row: None,
            images: ImagePair::default(),
            wrap_key: String::new(),
            dark: None,
            needs_load: true,
            file_filter: String::new(),
            pinned: false,
        }
    }

    /// The changed lines once loaded, or the reason it couldn't load. `None` while loading.
    pub fn loaded_changes(&self) -> Option<Result<String, String>> {
        if let Some(e) = &self.files.error {
            return Some(Err(e.clone()));
        }
        let files = self.files.get()?;
        let lines = files.iter().flat_map(|(f, _)| &f.hunks).flat_map(|h| &h.lines).filter(|l| matches!(l.kind, LineKind::Add | LineKind::Del));
        Some(Ok(lines.map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")))
    }

    pub fn key(&self) -> String {
        format!("diff:{:?}:{:?}", self.src, self.path)
    }

    pub fn title(&self) -> String {
        let what = match &self.src {
            DiffSource::Unstaged => "Unstaged".to_string(),
            DiffSource::Staged => "Staged".to_string(),
            DiffSource::WorkingVsHead => "Working".to_string(),
            DiffSource::Commit(s) => gitem_core::util::prefix(s, 7).to_string(),
            DiffSource::Range(a, b) => format!("{}..{}", short(a), short(b)),
            DiffSource::MergeBase(a, b) => format!("{}...{}", short(a), short(b)),
            DiffSource::WorkingVs(r) => format!("Working vs {}", short(r)),
            DiffSource::Untracked => "New".to_string(),
            DiffSource::Files(a, b) => format!("{} ↔ {}", leaf(a), leaf(b)),
        };
        match &self.path {
            Some(p) => format!("{} ({what})", leaf(p)),
            None => format!("Diff {what}"),
        }
    }

    fn can_stage(&self) -> bool {
        matches!(self.src, DiffSource::Unstaged | DiffSource::Staged)
    }

    fn reload(&mut self, ctx: &egui::Context, cx: &Cx) {
        let repo = cx.repo.clone();
        let (src, path) = (self.src.clone(), self.path.clone());
        let mut opts = self.opts;
        if self.full_file {
            opts.context = FULL_CONTEXT;
        }
        // Patches built from zero-context hunks don't apply where they should.
        if self.can_stage() {
            opts.context = opts.context.max(1);
        }
        let dark = cx.pal.dark;
        self.dark = Some(dark);
        self.selected_lines.clear();
        self.files.spawn(ctx, move || -> Result<Vec<(FileDiff, Prepared)>, String> {
            let files = repo.diff(&src, path.as_deref(), &opts).map_err(|e| e.to_string())?;
            Ok(files.into_iter().map(|f| { let p = prepare(&f, dark); (f, p) }).collect())
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.files.poll();
        let live = matches!(self.src, DiffSource::Unstaged | DiffSource::Staged | DiffSource::WorkingVsHead | DiffSource::WorkingVs(_) | DiffSource::Untracked);
        let status_changed = live && self.loaded_status_gen != cx.data.status_gen;
        if self.needs_load || status_changed || self.dark != Some(cx.pal.dark) {
            self.needs_load = false;
            self.loaded_status_gen = cx.data.status_gen;
            self.reload(&ui.ctx().clone(), cx);
        }

        self.toolbar(ui, cx);
        ui.separator();

        if let Some(e) = &self.files.error {
            error_box(ui, e, cx.pal);
        }
        let Some(files) = self.files.value.take() else {
            spinner_row(ui, "Computing diff…", cx.pal);
            return;
        };
        if files.is_empty() {
            let hint = if self.opts.ws != WsMode::None { "Only whitespace changed, or the file matches its source." } else { "These two versions are identical." };
            empty_state(ui, "No differences", hint, cx.pal);
        } else {
            if files.len() > 1 {
                egui::Panel::left("diff-files").resizable(true).default_size(240.0).show(ui, |ui| self.file_list(ui, cx, &files));
            }
            if self.selected_file >= files.len() {
                self.selected_file = 0;
            }
            let (f, prepared) = &files[self.selected_file];
            self.content(ui, cx, f, prepared);
        }
        self.files.value = Some(files);
    }

    fn file_list(&mut self, ui: &mut Ui, cx: &mut Cx, files: &[(FileDiff, Prepared)]) {
        let p = cx.pal.clone();
        ui.add(crate::widgets::input(&mut self.file_filter).hint_text("Filter files").desired_width(f32::INFINITY));
        egui::ScrollArea::vertical().id_salt("diff-file-list").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let q = self.file_filter.to_lowercase();
            for (i, (f, _)) in files.iter().enumerate() {
                if !q.is_empty() && !f.display_path().to_lowercase().contains(&q) {
                    continue;
                }
                let status = if f.is_new {
                    gitem_core::FileStatus::Added
                } else if f.is_deleted {
                    gitem_core::FileStatus::Deleted
                } else if f.is_rename {
                    gitem_core::FileStatus::Renamed
                } else {
                    gitem_core::FileStatus::Modified
                };
                let ch = gitem_core::FileChange { path: f.display_path().to_string(), orig_path: None, status, additions: Some(f.additions() as u32), deletions: Some(f.deletions() as u32) };
                if file_row(ui, &ch, i == self.selected_file, &p).on_hover_text(f.display_path()).clicked() {
                    self.selected_file = i;
                    self.selected_lines.clear();
                }
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let mut reload = false;
        // Wraps, or the buttons at the end would run underneath the Details panel.
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Split, "Side by side");
            ui.selectable_value(&mut self.mode, Mode::Unified, "Unified");
            ui.separator();
            let label = match self.opts.ws {
                WsMode::None => "Whitespace: show",
                WsMode::All => "Whitespace: ignore all",
                WsMode::Change => "Whitespace: ignore changes",
                WsMode::Eol => "Whitespace: ignore at EOL",
            };
            ui.menu_button(label, |ui| {
                for (w, t) in [(WsMode::None, "Show"), (WsMode::All, "Ignore all"), (WsMode::Change, "Ignore changes in amount"), (WsMode::Eol, "Ignore at end of line")] {
                    if ui.selectable_value(&mut self.opts.ws, w, t).clicked() {
                        reload = true;
                        ui.close();
                    }
                }
            });
            ui.menu_button("Algorithm ▾", |ui| {
                for (a, t) in [(DiffAlgo::Patience, "Patience"), (DiffAlgo::Histogram, "Histogram"), (DiffAlgo::Myers, "Myers"), (DiffAlgo::Minimal, "Minimal")] {
                    if ui.selectable_value(&mut self.opts.algo, a, t).clicked() {
                        reload = true;
                        ui.close();
                    }
                }
            });
            reload |= toggle(ui, &mut self.full_file, "Whole file", "Show the whole file instead of just changes");
            if !self.full_file {
                ui.label(RichText::new("Context").color(cx.pal.dim));
                reload |= ui.add(egui::DragValue::new(&mut self.opts.context).range(0..=200)).changed();
            }
            ui.separator();
            if ui.small_button("▲").on_hover_text("Previous change (Alt+Up)").clicked() || ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp)) {
                self.jump(-1);
            }
            if ui.small_button("▼").on_hover_text("Next change (Alt+Down)").clicked() || ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown)) {
                self.jump(1);
            }
            if let Some(path) = &self.path {
                if ui.small_button("Blame").clicked() {
                    let rev = if let DiffSource::Commit(s) = &self.src { Some(s.clone()) } else { None };
                    cx.open(Tab::file(path, rev, None));
                }
                if ui.small_button("History").clicked() {
                    cx.open(Tab::file_history(path));
                }
            }
            if self.can_stage() && !self.selected_lines.is_empty() {
                let verb = if self.src == DiffSource::Unstaged { "Stage" } else { "Unstage" };
                if ui.button(format!("{verb} {} selected line(s)", self.selected_lines.len())).clicked() {
                    self.apply_selected_lines(cx);
                }
                if self.src == DiffSource::Unstaged && ui.button("Discard selected…").clicked() {
                    self.discard_selected_lines(cx);
                }
            }
            toggle(ui, &mut self.pinned, "Keep open", "Keep this tab when opening another diff");
            if self.files.busy() {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        if reload {
            self.reload(&ui.ctx().clone(), cx);
        }
    }

    fn jump(&mut self, dir: i32) {
        let Some(files) = self.files.get() else { return };
        let Some((_, pr)) = files.get(self.selected_file) else { return };
        let headers: Vec<usize> = pr.side.iter().enumerate().filter(|(_, r)| r.is_hunk_header).map(|(i, _)| i).collect();
        if headers.is_empty() {
            return;
        }
        let cur = self.wrap_key.parse::<usize>().unwrap_or(0);
        let next = (cur as i32 + dir).rem_euclid(headers.len() as i32) as usize;
        self.wrap_key = next.to_string();
        self.scroll_to_row = Some(headers[next]);
    }

    fn selected_by_hunk(&self) -> Vec<(usize, Vec<usize>)> {
        let mut m: HashMap<usize, Vec<usize>> = HashMap::new();
        for (h, l) in &self.selected_lines {
            m.entry(*h).or_default().push(*l);
        }
        let mut v: Vec<_> = m.into_iter().collect();
        // Apply from the bottom so earlier hunks keep their line numbers.
        v.sort_by_key(|x| std::cmp::Reverse(x.0));
        v
    }

    fn apply_selected_lines(&mut self, cx: &mut Cx) {
        let Some(files) = self.files.get() else { return };
        let Some((f, _)) = files.get(self.selected_file) else { return };
        let (mode, reverse, title) = if self.src == DiffSource::Unstaged { (PatchMode::Forward, false, "Stage lines") } else { (PatchMode::Reverse, true, "Unstage lines") };
        for (h, lines) in self.selected_by_hunk() {
            if let Some(patch) = diff::build_patch(f, h, Some(&lines), mode) {
                cx.run(ops::apply_patch(title, patch, true, reverse));
            }
        }
        self.selected_lines.clear();
    }

    fn discard_selected_lines(&mut self, cx: &mut Cx) {
        let Some(files) = self.files.get() else { return };
        let Some((f, _)) = files.get(self.selected_file) else { return };
        // One confirmation covering every hunk, applied from the bottom up.
        let mut op = ops::GitOp::new("Discard lines");
        for (h, lines) in self.selected_by_hunk() {
            if let Some(patch) = diff::build_patch(f, h, Some(&lines), PatchMode::Reverse) {
                op.steps.extend(ops::apply_patch("Discard lines", patch, false, true).steps);
            }
        }
        if !op.steps.is_empty() {
            let n = self.selected_lines.len();
            cx.confirm("Discard lines", &format!("Discard {n} selected line(s) from the working tree? This cannot be undone."), "Discard", op);
        }
        self.selected_lines.clear();
    }

    fn hunk_buttons(&self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, hunk: usize) {
        // Text that isn't UTF-8 can't be rebuilt into an exact patch.
        if self.can_stage() && f.hunks.get(hunk).is_some_and(|h| h.lines.iter().any(|l| l.text.contains('\u{FFFD}'))) {
            ui.label(RichText::new("Not UTF-8: stage the whole file").small().color(cx.pal.warn));
            return;
        }
        match self.src {
            DiffSource::Unstaged => {
                if ui.small_button("Stage hunk").clicked()
                    && let Some(p) = diff::build_patch(f, hunk, None, PatchMode::Forward) {
                        cx.run(ops::apply_patch("Stage hunk", p, true, false));
                    }
                if ui.small_button("Discard hunk…").clicked()
                    && let Some(p) = diff::build_patch(f, hunk, None, PatchMode::Reverse) {
                        cx.confirm("Discard hunk", "Discard this hunk from the working tree? This cannot be undone.", "Discard", ops::apply_patch("Discard hunk", p, false, true));
                    }
            }
            DiffSource::Staged => {
                if ui.small_button("Unstage hunk").clicked()
                    && let Some(p) = diff::build_patch(f, hunk, None, PatchMode::Reverse) {
                        cx.run(ops::apply_patch("Unstage hunk", p, true, true));
                    }
            }
            _ => {}
        }
    }

    fn content(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared) {
        let pal = cx.pal.clone();
        ui.horizontal(|ui| {
            let name = if f.is_rename { format!("{} → {}", f.old_path, f.new_path) } else { f.display_path().to_string() };
            ui.label(RichText::new(name).strong());
            ui.label(RichText::new(format!("+{}", f.additions())).color(pal.add_fg).monospace());
            ui.label(RichText::new(format!("-{}", f.deletions())).color(pal.del_fg).monospace());
            if let Some(s) = f.similarity {
                ui.label(RichText::new(format!("{s}% similar")).small().color(pal.faint));
            }
            if let (Some(a), Some(b)) = (&f.old_mode, &f.new_mode)
                && !f.is_new && !f.is_deleted {
                    ui.label(RichText::new(format!("mode {a} → {b}")).small().color(pal.faint));
                }
            if self.can_stage() && !f.hunks.is_empty() {
                let path = f.display_path().to_string();
                if self.src == DiffSource::Unstaged {
                    if ui.small_button("Stage file").clicked() {
                        cx.run(ops::stage_paths(&[path]));
                    }
                } else if ui.small_button("Unstage file").clicked() {
                    cx.run(ops::unstage_paths(&[path], true));
                }
            }
        });
        if f.is_binary {
            self.binary(ui, cx, f);
            return;
        }
        if f.hunks.is_empty() {
            empty_state(ui, "No text changes", "Only metadata differs (mode or rename).", &pal);
            return;
        }
        match self.mode {
            Mode::Split if ui.available_width() >= MIN_SPLIT_WIDTH => self.split(ui, cx, f, pr),
            _ => self.unified(ui, cx, f, pr),
        }
    }

    fn line_colors(pal: &crate::theme::Palette, kind: LineKind) -> (Color32, Color32) {
        match kind {
            LineKind::Add => (pal.add_bg, pal.add_word),
            LineKind::Del => (pal.del_bg, pal.del_word),
            _ => (Color32::TRANSPARENT, Color32::TRANSPARENT),
        }
    }

    /// One side of a row. Returns true if the gutter was clicked.
    #[allow(clippy::too_many_arguments)]
    fn draw_cell(&mut self, ui: &mut Ui, pal: &crate::theme::Palette, rect: Rect, no: Option<u32>, text: Option<&str>, kind: Option<LineKind>, key: Option<(usize, usize)>, pr: &Prepared, font: f32, gutter: f32, scroll_x: f32, row_id: egui::Id) {
        let (bg, word_bg) = kind.map(|k| Self::line_colors(pal, k)).unwrap_or((pal.panel, Color32::TRANSPARENT));
        ui.painter().rect_filled(rect, 0.0, if kind.is_none() { pal.panel2 } else { bg });
        let selected = key.is_some_and(|k| self.selected_lines.contains(&k));
        if selected {
            ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, pal.accent);
        }
        if let Some(n) = no {
            ui.painter().text(pos2(rect.left() + gutter - 6.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
        }
        let changed = matches!(kind, Some(LineKind::Add | LineKind::Del));
        if changed && self.can_stage()
            && let Some(k) = key {
                let gr = Rect::from_min_size(rect.min, vec2(gutter, rect.height()));
                let r = ui.interact(gr, row_id.with("g"), Sense::click()).on_hover_text("Click to select this line for staging");
                if r.hovered() {
                    ui.painter().rect_filled(gr, 0.0, pal.accent.gamma_multiply(0.2));
                }
                if r.clicked() {
                    if selected {
                        self.selected_lines.remove(&k);
                    } else {
                        self.selected_lines.insert(k);
                    }
                }
            }
        if let Some(t) = text {
            let spans = key.and_then(|k| pr.syntax.get(&k));
            let words = key.and_then(|k| pr.words.get(&k)).map(|v| v.as_slice());
            let job = code_job(t, font, pal.text, spans, words, word_bg);
            let g = ui.painter().layout_job(job);
            let clip = Rect::from_min_max(pos2(rect.left() + gutter, rect.top()), rect.max).intersect(ui.clip_rect());
            ui.painter().with_clip_rect(clip).galley(pos2(rect.left() + gutter + 4.0 - scroll_x, rect.center().y - g.size().y / 2.0), g, pal.text);
        }
    }

    fn split(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared) {
        let pal = cx.pal.clone();
        let font = cx.settings.editor_font_size;
        let row_h = (font * 1.45).ceil();
        let gutter = 52.0;
        let n = pr.side.len();
        let mut sa = egui::ScrollArea::vertical().id_salt(("split", self.selected_file)).auto_shrink([false, false]);
        if let Some(r) = self.scroll_to_row.take() {
            sa = sa.vertical_scroll_offset((r as f32 * row_h - 40.0).max(0.0));
        }
        let char_w = ui.fonts_mut(|fx| fx.glyph_width(&mono(font), 'm'));
        let avail = ui.available_width();
        // Each side keeps half the pane, however long a line is, so the new side never
        // starts off screen. A long line scrolls within its half, both halves together.
        let half = (avail - 10.0) / 2.0;
        let max_x = (pr.max_chars as f32 * char_w + 24.0 - (half - gutter)).max(0.0);
        self.split_x = self.split_x.clamp(0.0, max_x);
        if max_x > 0.0 {
            egui::Panel::bottom(ui.id().with(("split-x", self.selected_file))).resizable(false).show_separator_line(false).show(ui, |ui| {
                let w = ui.available_width();
                let out = egui::ScrollArea::horizontal().id_salt(("split-x-bar", self.selected_file)).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).horizontal_scroll_offset(self.split_x).show(ui, |ui| {
                    ui.allocate_exact_size(vec2(w + max_x, 10.0), Sense::hover());
                });
                self.split_x = out.state.offset.x;
            });
        }
        let area = ui.available_rect_before_wrap();
        if ui.rect_contains_pointer(area) {
            // Sideways wheel and Shift+wheel, which the vertical list leaves alone.
            self.split_x = (self.split_x - ui.input(|i| i.smooth_scroll_delta.x)).clamp(0.0, max_x);
        }
        let scroll_x = self.split_x;
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, row_h, n, |ui, range| {
            ui.set_min_width(half * 2.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let r = &pr.side[i];
                let (rect, _) = ui.allocate_exact_size(vec2(half * 2.0, row_h), Sense::hover());
                if r.is_hunk_header {
                    ui.painter().rect_filled(rect, 0.0, pal.panel2);
                    let vis = ui.clip_rect();
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(vis.left() + 8.0, rect.top()), pos2(vis.right(), rect.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    child.label(RichText::new(&r.header).monospace().size(font - 1.5).color(pal.dim));
                    child.add_space(8.0);
                    self.hunk_buttons(&mut child, cx, f, r.hunk);
                    continue;
                }
                let lrect = Rect::from_min_size(rect.min, vec2(half - 1.0, row_h));
                let rrect = Rect::from_min_size(pos2(rect.left() + half + 1.0, rect.top()), vec2(half - 1.0, row_h));
                let id = ui.id().with(("row", i));
                self.draw_cell(ui, &pal, lrect, r.left.as_ref().map(|x| x.0), r.left.as_ref().map(|x| x.1.as_str()), r.left_kind, r.left_idx.map(|l| (r.hunk, l)), pr, font, gutter, scroll_x, id.with("l"));
                self.draw_cell(ui, &pal, rrect, r.right.as_ref().map(|x| x.0), r.right.as_ref().map(|x| x.1.as_str()), r.right_kind, r.right_idx.map(|l| (r.hunk, l)), pr, font, gutter, scroll_x, id.with("r"));
            }
        });
    }

    fn unified(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared) {
        let pal = cx.pal.clone();
        let font = cx.settings.editor_font_size;
        let row_h = (font * 1.45).ceil();
        let gutter = 96.0;
        // Flat row list: hunk headers plus every line.
        let mut rows: Vec<(usize, Option<usize>)> = Vec::new();
        for (hi, h) in f.hunks.iter().enumerate() {
            rows.push((hi, None));
            for li in 0..h.lines.len() {
                rows.push((hi, Some(li)));
            }
        }
        let max_chars = f.hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.len().min(MAX_SHOWN_LINE_BYTES + 64)).max().unwrap_or(0);
        let char_w = ui.fonts_mut(|fx| fx.glyph_width(&mono(font), 'm'));
        let width = ((max_chars as f32 * char_w) + gutter + 24.0).max(ui.available_width() - 10.0);
        let mut sa = egui::ScrollArea::both().id_salt(("unified", self.selected_file)).auto_shrink([false, false]);
        if let Some(r) = self.scroll_to_row.take() {
            // The stored index is a side-by-side row; map through its hunk.
            let hunk = pr.side.get(r).map(|x| x.hunk).unwrap_or(0);
            let y = rows.iter().position(|x| x.0 == hunk && x.1.is_none()).unwrap_or(0);
            sa = sa.vertical_scroll_offset((y as f32 * row_h - 40.0).max(0.0));
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.set_min_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let (hi, li) = rows[i];
                let h = &f.hunks[hi];
                let (rect, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
                let Some(li) = li else {
                    ui.painter().rect_filled(rect, 0.0, pal.panel2);
                    let vis = ui.clip_rect();
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(vis.left() + 8.0, rect.top()), pos2(vis.right(), rect.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    child.label(RichText::new(format!("@@ -{},{} +{},{} @@ {}", h.old_start, h.old_count, h.new_start, h.new_count, h.section)).monospace().size(font - 1.5).color(pal.dim));
                    child.add_space(8.0);
                    self.hunk_buttons(&mut child, cx, f, hi);
                    continue;
                };
                let l = &h.lines[li];
                if l.kind == LineKind::NoNewline {
                    paint_text(ui, Rect::from_min_max(pos2(rect.left() + gutter, rect.top()), rect.max), &format!("\\ {}", l.text), mono(font - 1.5), pal.faint);
                    continue;
                }
                let id = ui.id().with(("urow", i));
                self.draw_cell(ui, &pal, rect, None, Some(&l.text), Some(l.kind), Some((hi, li)), pr, font, gutter, 0.0, id);
                if let Some(n) = l.old_no {
                    ui.painter().text(pos2(rect.left() + 42.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
                }
                if let Some(n) = l.new_no {
                    ui.painter().text(pos2(rect.left() + 84.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
                }
                let sign = match l.kind {
                    LineKind::Add => "+",
                    LineKind::Del => "-",
                    _ => "",
                };
                ui.painter().text(pos2(rect.left() + gutter - 4.0, rect.center().y), Align2::RIGHT_CENTER, sign, mono(font), pal.dim);
            }
        });
    }

    /// Binary file: show images side by side, otherwise a message.
    fn binary(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff) {
        let pal = cx.pal.clone();
        let path = f.display_path().to_string();
        let is_image = ["png", "jpg", "jpeg"].iter().any(|e| path.to_lowercase().ends_with(e));
        if !is_image {
            empty_state(ui, "Binary file", "This file can't be compared as text.", &pal);
            return;
        }
        let key = format!("{:?}:{path}", self.src);
        if self.images.loaded_for.as_deref() != Some(&key) {
            self.images = ImagePair { loaded_for: Some(key), ..Default::default() };
            let load = |bytes: Option<Vec<u8>>, name: &str, ui: &Ui| -> Option<egui::TextureHandle> {
                let b = bytes.filter(|b| b.len() < MAX_INLINE_IMAGE)?;
                let img = decode_image(&b)?;
                Some(ui.ctx().load_texture(name, img, egui::TextureOptions::LINEAR))
            };
            let (old_rev, new_rev): (Option<String>, Option<String>) = match &self.src {
                DiffSource::Commit(s) => (Some(format!("{s}^")), Some(s.clone())),
                DiffSource::Range(a, b) => (Some(a.clone()), Some(b.clone())),
                DiffSource::MergeBase(a, b) => (cx.repo.merge_base(a, b).ok(), Some(b.clone())),
                DiffSource::Staged => (Some("HEAD".into()), Some(String::new())),
                DiffSource::Unstaged => (Some(String::new()), None),
                DiffSource::WorkingVsHead => (Some("HEAD".into()), None),
                DiffSource::WorkingVs(r) => (Some(r.clone()), None),
                _ => (None, None),
            };
            let old = old_rev.and_then(|r| cx.repo.file_at(&r, &f.old_path).ok());
            let new = match new_rev {
                Some(r) => cx.repo.file_at(&r, &path).ok(),
                None => cx.repo.worktree_file(&path).ok(),
            };
            self.images.old = load(old, "diff-old", ui);
            self.images.new = load(new, "diff-new", ui);
        }
        ui.columns(2, |c| {
            for (col, (tex, name)) in [(self.images.old.as_ref(), "Before"), (self.images.new.as_ref(), "After")].into_iter().enumerate() {
                let ui = &mut c[col];
                ui.label(RichText::new(name).color(pal.dim));
                match tex {
                    Some(t) => {
                        let size = t.size_vec2();
                        let scale = (ui.available_width() / size.x).min(1.0);
                        ui.image((t.id(), size * scale));
                        ui.label(RichText::new(format!("{} × {} px", size.x as u32, size.y as u32)).small().color(pal.faint));
                    }
                    None => {
                        ui.label(RichText::new("Not present").color(pal.faint));
                    }
                }
            }
        });
    }
}

fn short(s: &str) -> &str {
    gitem_core::util::short_rev(s)
}

fn leaf(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}
