//! The visual diff tool: side-by-side or unified, syntax highlighting, word diff,
//! hunk and line staging, whitespace and algorithm options, image comparison.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

/// A line this long isn't compared word by word: that's quadratic, and it can't all be drawn.
const MAX_WORD_DIFF_LINE: usize = 20_000;

/// A long line's colours are worth keeping between frames. Below this, egui's own cache is enough.
const LONG_LINE: usize = 1_000;
const MAX_LONG_JOBS: usize = 400;

#[derive(Default)]
struct Prepared {
    /// Worked out once, as they mean walking every line of the file's diff.
    additions: usize,
    deletions: usize,
    /// Unified rows: a hunk's header, then each of its lines.
    unified: Vec<(usize, Option<usize>)>,
    unified_chars: usize,
    /// The text of each unified row, for wrapping. A hunk's header has none.
    unified_texts: Vec<Option<String>>,
    side: Vec<SideRow>,
    /// (hunk, line index) -> changed byte ranges within that line.
    words: HashMap<(usize, usize), Vec<(usize, usize)>>,
    /// (hunk, line index) -> syntax colour spans.
    syntax: HashMap<(usize, usize), Spans>,
    /// The widest line as drawn, in characters. Worked out once: there can be many lines.
    max_chars: usize,
    /// Lines shown with how they end, because that's all that changed about them.
    marked: HashMap<(usize, usize), String>,
}

/// The rows holding `q`, already lower case, as they're laid out. Ascending.
fn find_rows(pr: &Prepared, unified: bool, q: &str) -> Vec<usize> {
    if unified {
        pr.unified_texts.iter().enumerate().filter(|(_, t)| t.as_ref().is_some_and(|t| t.to_lowercase().contains(q))).map(|(i, _)| i).collect()
    } else {
        pr.side.iter().enumerate().filter(|(_, r)| [&r.left, &r.right].into_iter().flatten().any(|(_, t)| t.to_lowercase().contains(q))).map(|(i, _)| i).collect()
    }
}

fn prepare(f: &FileDiff, dark: bool) -> Prepared {
    let side = diff::side_by_side(f);
    let mut words = HashMap::new();
    for r in &side {
        if let (Some(li), Some(ri), Some((_, lt)), Some((_, rt))) = (r.left_idx, r.right_idx, &r.left, &r.right)
            && r.left_kind == Some(LineKind::Del) && r.right_kind == Some(LineKind::Add)
            && lt.len().max(rt.len()) <= MAX_WORD_DIFF_LINE {
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
    // The limit is for the whole diff, not each hunk: a diff can have thousands of hunks.
    let mut budget = syntax::MAX_BYTES;
    for (hi, h) in f.hunks.iter().enumerate() {
        let lines: Vec<&str> = h.lines.iter().map(|l| l.text.as_str()).collect();
        let size: usize = lines.iter().map(|l| l.len().min(MAX_SHOWN_LINE_BYTES)).sum();
        if size > budget {
            break;
        }
        budget -= size;
        for (li, s) in syntax::highlight(&path, &lines, dark).into_iter().enumerate() {
            syn.insert((hi, li), s);
        }
    }
    let max_chars = side.iter().flat_map(|r| [r.left.as_ref(), r.right.as_ref()]).flatten().map(|x| shown_chars(&x.1)).max().unwrap_or(0);
    let mut unified = Vec::new();
    for (hi, h) in f.hunks.iter().enumerate() {
        unified.push((hi, None));
        unified.extend((0..h.lines.len()).map(|li| (hi, Some(li))));
    }
    let unified_chars = f.hunks.iter().flat_map(|h| &h.lines).map(|l| shown_chars(&l.text)).max().unwrap_or(0);
    // The side-by-side rows have worked out which pairs differ only by their line ending.
    let mut marked = HashMap::new();
    for r in &side {
        for (idx, cell) in [(r.left_idx, &r.left), (r.right_idx, &r.right)] {
            if let (Some(i), Some((_, text))) = (idx, cell)
                && f.hunks[r.hunk].lines[i].text != *text
            {
                marked.insert((r.hunk, i), text.clone());
            }
        }
    }
    let unified_texts = unified.iter().map(|(h, l)| l.map(|l| marked.get(&(*h, l)).unwrap_or(&f.hunks[*h].lines[l].text).clone())).collect();
    Prepared { additions: f.additions(), deletions: f.deletions(), unified, unified_chars, unified_texts, side, words, syntax: syn, max_chars, marked }
}

fn change_for(f: &FileDiff) -> gitem_core::FileChange {
    let status = if f.is_new {
        gitem_core::FileStatus::Added
    } else if f.is_deleted {
        gitem_core::FileStatus::Deleted
    } else if f.is_rename {
        gitem_core::FileStatus::Renamed
    } else {
        gitem_core::FileStatus::Modified
    };
    gitem_core::FileChange { path: f.display_path().to_string(), orig_path: None, status, additions: Some(f.additions() as u32), deletions: Some(f.deletions() as u32) }
}

/// A loaded diff. Each file is made ready for showing when it's first shown: a commit of
/// 20,000 files has 20,000 of these, and one screen.
#[derive(Default)]
struct Loaded {
    files: Vec<Arc<FileDiff>>,
    /// One row per file, in the same order, for the list.
    rows: Vec<gitem_core::FileChange>,
    prepared: HashMap<usize, Arc<Prepared>>,
}

#[derive(Default)]
struct ImagePair {
    old: Option<egui::TextureHandle>,
    new: Option<egui::TextureHandle>,
    loaded_for: Option<String>,
    /// Both versions read and decoded, on their way to becoming textures.
    decoding: Task<PicturePair>,
    /// What to say about what's left out or couldn't be drawn, with an SVG.
    why: Option<String>,
}

/// Before, after, and what `ImagePair::why` will say.
type PicturePair = (Option<egui::ColorImage>, Option<egui::ColorImage>, Option<String>);

/// Side by side or not, the width and file they were worked out for, and each row's top.
type WrappedTops = (bool, (u32, u32), usize, std::sync::Arc<Vec<f32>>);

pub struct DiffView {
    pub src: DiffSource,
    pub path: Option<String>,
    files: Task<Loaded>,
    /// The file being made ready, and the load it belongs to.
    preparing: Task<(usize, Arc<Prepared>)>,
    /// Which files match the filter, and for what filter over which load.
    filtered: Option<(String, u64, Vec<usize>)>,
    load_no: u64,
    /// Long lines' jobs, by hunk, line and font size. Jobs and not galleys, which are tied
    /// to the font atlas of the moment.
    long_jobs: HashMap<(usize, usize, usize, u32), egui::text::LayoutJob>,
    loaded_status_gen: u64,
    mode: Mode,
    /// How far the side-by-side text is scrolled sideways.
    split_x: f32,
    opts: DiffOpts,
    full_file: bool,
    selected_file: usize,
    selected_lines: HashSet<(usize, usize)>,
    /// What the selection was made on, to tell whether a reload changed anything under it.
    selected_on: Option<(usize, Vec<diff::Hunk>)>,
    scroll_to_row: Option<usize>,
    /// A row of the unified list, for a match found there.
    scroll_to_unified: Option<usize>,
    find_open: bool,
    find: String,
    /// Rows with a match, in the layout showing, and which of them is current.
    find_hits: Vec<usize>,
    find_pos: usize,
    find_key: Option<(String, usize, bool, bool)>,
    images: ImagePair,
    /// An SVG's diff shows its two pictures, not its text.
    svg_pictures: bool,
    wrap_key: String,
    dark: Option<bool>,
    needs_load: bool,
    file_filter: String,
    preparing_for: Option<usize>,
    /// Unpinned diffs are replaced by the next one opened from a list.
    pub pinned: bool,
    /// Part of another view, rather than a tab of its own.
    embedded: bool,
    /// Long lines run on underneath, rather than off to the side.
    wrap: bool,
    /// Top of each row and the end of the last, while wrapping, with what they're for:
    /// side by side or not, the text's width in points and font size, and the file.
    wrapped_tops: Option<WrappedTops>,
}

impl DiffView {
    pub fn new(src: DiffSource, path: Option<String>) -> Self {
        Self {
            src,
            path,
            files: Task::default(),
            preparing: Task::default(),
            filtered: None,
            load_no: 0,
            long_jobs: HashMap::new(),
            loaded_status_gen: u64::MAX,
            mode: Mode::Split,
            split_x: 0.0,
            opts: DiffOpts::default(),
            full_file: false,
            selected_file: 0,
            selected_lines: HashSet::new(),
            selected_on: None,
            scroll_to_row: None,
            scroll_to_unified: None,
            find_open: false,
            find: String::new(),
            find_hits: Vec::new(),
            find_pos: 0,
            find_key: None,
            images: ImagePair::default(),
            svg_pictures: false,
            wrap_key: String::new(),
            dark: None,
            needs_load: true,
            file_filter: String::new(),
            preparing_for: None,
            pinned: false,
            embedded: false,
            wrap: false,
            wrapped_tops: None,
        }
    }

    /// For showing inside another view. It starts unified, as half of a pane is little room.
    pub fn embedded(mut self) -> Self {
        self.embedded = true;
        self.mode = Mode::Unified;
        self
    }

    /// Shows files the way `other` was showing them.
    pub fn look_like(&mut self, other: &DiffView) {
        self.mode = other.mode;
        self.opts = other.opts;
        self.full_file = other.full_file;
        self.wrap = other.wrap;
    }

    /// The changed lines once loaded, or the reason it couldn't load. `None` while loading.
    pub fn loaded_changes(&self) -> Option<Result<String, String>> {
        if let Some(e) = &self.files.error {
            return Some(Err(e.clone()));
        }
        let loaded = self.files.get()?;
        let lines = loaded.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines).filter(|l| matches!(l.kind, LineKind::Add | LineKind::Del));
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

    /// Whether a patch can be made from what's on screen. A diff that ignores whitespace
    /// doesn't match the file, so a patch from it wouldn't apply.
    fn can_patch(&self) -> bool {
        self.can_stage() && self.opts.ws == WsMode::None
    }

    /// The selected lines, as a list a test can read.
    pub fn selected_line_count(&self) -> usize {
        self.selected_lines.len()
    }

    fn reload(&mut self, ctx: &egui::Context, cx: &Cx) {
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
        // Any file saved anywhere reloads this, which is no reason to lose a selection. Once
        // the new diff is here, it stays if those lines are still what they were.
        self.selected_on = (!self.selected_lines.is_empty()).then(|| self.files.get().and_then(|l| l.files.get(self.selected_file)).map(|f| (self.selected_file, f.hunks.clone()))).flatten();
        self.preparing.stop();
        self.long_jobs.clear();
        self.load_no += 1;
        self.wrapped_tops = None;
        // The file on screen comes ready with the load, so a reload swaps it in one piece.
        let selected = self.selected_file;
        self.files.spawn_git(ctx, cx.repo, move |repo| -> Result<Loaded, String> {
            let files: Vec<Arc<FileDiff>> = repo.diff(&src, path.as_deref(), &opts).map_err(|e| e.to_string())?.into_iter().map(Arc::new).collect();
            let rows = files.iter().map(|f| change_for(f)).collect();
            let at = if selected < files.len() { selected } else { 0 };
            let prepared = files.get(at).map(|f| (at, Arc::new(prepare(f, dark)))).into_iter().collect();
            Ok(Loaded { files, rows, prepared })
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        if self.files.poll() {
            let same = self.selected_on.take().is_some_and(|(at, hunks)| at == self.selected_file && self.files.get().and_then(|l| l.files.get(at)).is_some_and(|f| f.hunks == hunks));
            if !same {
                self.selected_lines.clear();
            }
        }
        if self.preparing.poll()
            && let (Some((at, p)), Some(loaded)) = (self.preparing.value.take(), self.files.value.as_mut())
        {
            loaded.prepared.insert(at, p);
        }
        let live = matches!(self.src, DiffSource::Unstaged | DiffSource::Staged | DiffSource::WorkingVsHead | DiffSource::WorkingVs(_) | DiffSource::Untracked);
        // A load under way finishes first, and this starts another when it lands.
        let status_changed = live && self.loaded_status_gen != cx.data.status_gen && !self.files.busy();
        if self.needs_load || status_changed || self.dark != Some(cx.pal.dark) {
            self.needs_load = false;
            self.loaded_status_gen = cx.data.status_gen;
            self.reload(&ui.ctx().clone(), cx);
        }

        self.toolbar(ui, cx);
        self.find_bar(ui, cx);
        ui.separator();

        if let Some(e) = &self.files.error {
            error_box(ui, e, cx.pal);
        }
        let Some(loaded) = self.files.value.take() else {
            spinner_row(ui, "Computing diff…", cx.pal);
            return;
        };
        if loaded.files.is_empty() {
            let nested = self.src == DiffSource::Untracked && self.path.as_deref().is_some_and(|p| cx.repo.is_nested_repo(p));
            let hint = if nested {
                "This folder is a Git repository of its own, so its files have no diff here."
            } else if self.opts.ws != WsMode::None {
                "Only whitespace changed, or the file matches its source."
            } else {
                "These two versions are identical."
            };
            empty_state(ui, "No differences", hint, cx.pal);
        } else {
            if loaded.files.len() > 1 {
                egui::Panel::left("diff-files").resizable(true).default_size(240.0).show(ui, |ui| self.file_list(ui, cx, &loaded));
            }
            if self.selected_file >= loaded.files.len() {
                self.selected_file = 0;
            }
            let f = loaded.files[self.selected_file].clone();
            match loaded.prepared.get(&self.selected_file).cloned() {
                Some(prepared) => self.content(ui, cx, &f, &prepared),
                None => {
                    if !self.preparing.busy() || self.preparing_for != Some(self.selected_file) {
                        self.preparing_for = Some(self.selected_file);
                        let (at, dark) = (self.selected_file, cx.pal.dark);
                        self.preparing.spawn(ui.ctx(), move || -> Result<(usize, Arc<Prepared>), String> { Ok((at, Arc::new(prepare(&f, dark)))) });
                    }
                    spinner_row(ui, "Getting this file ready…", cx.pal);
                }
            }
        }
        self.files.value = Some(loaded);
    }

    fn file_list(&mut self, ui: &mut Ui, cx: &mut Cx, loaded: &Loaded) {
        let p = cx.pal.clone();
        ui.add(crate::widgets::input(&mut self.file_filter).hint_text("Filter files").desired_width(f32::INFINITY));
        if self.filtered.as_ref().is_none_or(|(q, n, _)| *q != self.file_filter || *n != self.load_no) {
            let q = self.file_filter.to_lowercase();
            let hits = loaded.rows.iter().enumerate().filter(|(_, c)| q.is_empty() || c.path.to_lowercase().contains(&q)).map(|(i, _)| i).collect();
            self.filtered = Some((self.file_filter.clone(), self.load_no, hits));
        }
        let Some((_, _, hits)) = self.filtered.take() else { return };
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().id_salt("diff-file-list").auto_shrink([false, false]).show_rows(ui, 22.0, hits.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in hits[range].iter().copied() {
                let ch = &loaded.rows[i];
                if keyed_file_row(ui, ch, i == self.selected_file, &p, "diff files").on_hover_text(ch.path.as_str()).clicked() {
                    self.selected_file = i;
                    self.selected_lines.clear();
                    self.scroll_to_row = None;
                }
            }
        });
        self.filtered = Some((self.file_filter.clone(), self.load_no, hits));
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let mut reload = false;
        // Wraps, or the buttons at the end would run underneath the Details panel.
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Split, "Side by side");
            ui.selectable_value(&mut self.mode, Mode::Unified, "Unified");
            ui.separator();
            let label = match self.opts.ws {
                WsMode::None => "Whitespace \u{E136}",
                WsMode::All => "Whitespace: ignore all \u{E136}",
                WsMode::Change => "Whitespace: ignore changes \u{E136}",
                WsMode::Eol => "Whitespace: ignore at EOL \u{E136}",
            };
            ui.menu_button(label, |ui| {
                for (w, t) in [(WsMode::None, "Show"), (WsMode::All, "Ignore all"), (WsMode::Change, "Ignore changes in amount"), (WsMode::Eol, "Ignore at end of line")] {
                    if ui.selectable_value(&mut self.opts.ws, w, t).clicked() {
                        reload = true;
                        ui.close();
                    }
                }
            });
            ui.menu_button("Algorithm \u{E136}", |ui| {
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
            if small_icon_button(ui, "\u{E13C}", "Previous change", &crate::keys::context_label(&cx.settings.key_bindings, "diff_prev")).clicked() || ui.input(|i| crate::keys::context_pressed(i, &cx.settings.key_bindings, "diff_prev")) {
                self.jump(-1);
            }
            if small_icon_button(ui, "\u{E136}", "Next change", &crate::keys::context_label(&cx.settings.key_bindings, "diff_next")).clicked() || ui.input(|i| crate::keys::context_pressed(i, &cx.settings.key_bindings, "diff_next")) {
                self.jump(1);
            }
            if let Some(path) = &self.path {
                if crate::widgets::small_button(ui, "Blame").clicked() {
                    let rev = if let DiffSource::Commit(s) = &self.src { Some(s.clone()) } else { None };
                    cx.open(Tab::file(path, rev, None));
                }
                if crate::widgets::small_button(ui, "History").clicked() {
                    cx.open(Tab::file_history(path));
                }
            }
            if self.can_stage() && self.opts.ws != WsMode::None {
                ui.label(RichText::new("Showing whitespace is needed to stage parts of a file").small().color(cx.pal.dim));
            } else if self.can_stage() && !self.selected_lines.is_empty() && !self.selection_is_exact() {
                // As on the hunk buttons: a patch from this text would rewrite the file's bytes.
                ui.label(RichText::new("Not UTF-8: stage the whole file").small().color(cx.pal.warn));
            } else if self.can_stage() && !self.selected_lines.is_empty() {
                let verb = if self.src == DiffSource::Unstaged { "Stage" } else { "Unstage" };
                if ui.button(format!("{verb} {}", plural(self.selected_lines.len(), "selected line", "selected lines"))).clicked() {
                    self.apply_selected_lines(cx);
                }
                if self.src == DiffSource::Unstaged && ui.button("Discard selected…").clicked() {
                    self.discard_selected_lines(cx);
                }
            }
            toggle(ui, &mut self.wrap, "Wrap", "Run long lines on underneath, rather than off to the side");
            if self.embedded {
                if crate::widgets::small_button(ui, "Open in tab").on_hover_text("Open this diff in a tab of its own").clicked() {
                    cx.diff(self.src.clone(), self.path.as_deref());
                }
            } else {
                toggle(ui, &mut self.pinned, "Keep open", "Keep this tab when opening another diff");
            }
            if self.files.busy() {
                ui.add(spinner(14.0));
            }
        });
        if reload {
            self.reload(&ui.ctx().clone(), cx);
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, cx: &Cx) {
        if cx.in_front && !cx.dialog_open && ui.input(|i| crate::keys::context_pressed(i, &cx.settings.key_bindings, "find")) {
            self.find_open = !self.find_open;
        }
        if !self.find_open {
            return;
        }
        let mut changed = false;
        let mut step = 0;
        ui.horizontal(|ui| {
            let r = ui.add(crate::widgets::input(&mut self.find).hint_text("Find in this file's diff").desired_width(240.0));
            changed = r.changed();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                step = 1;
                r.request_focus();
            }
            if ui.input(|i| crate::keys::context_pressed(i, &cx.settings.key_bindings, "find_next")) {
                step = 1;
            }
            if ui.input(|i| crate::keys::context_pressed(i, &cx.settings.key_bindings, "find_prev")) {
                step = -1;
            }
            if small_icon_button(ui, "\u{E13C}", "Previous match", &crate::keys::context_label(&cx.settings.key_bindings, "find_prev")).clicked() {
                step = -1;
            }
            if small_icon_button(ui, "\u{E136}", "Next match", &crate::keys::context_label(&cx.settings.key_bindings, "find_next")).clicked() {
                step = 1;
            }
            let says = if self.find.is_empty() { String::new() } else if self.find_hits.is_empty() { "No matches".to_string() } else { format!("{} of {}", self.find_pos + 1, self.find_hits.len()) };
            ui.label(RichText::new(says).small().color(cx.pal.dim));
            if crate::widgets::small_button(ui, "Close").clicked() {
                self.find_open = false;
                self.find_hits.clear();
            }
        });
        // Each frame only while the answer can change: the text, the file or the layout.
        let key = (self.find.clone(), self.selected_file, self.mode == Mode::Unified, self.files.get().is_some());
        if changed || self.find_key.as_ref() != Some(&key) {
            self.find_key = Some(key);
            self.update_find();
        }
        if step != 0 && !self.find_hits.is_empty() {
            let n = self.find_hits.len() as i32;
            self.find_pos = (self.find_pos as i32 + step).rem_euclid(n) as usize;
            self.scroll_to_hit();
        }
    }

    fn update_find(&mut self) {
        self.find_hits.clear();
        self.find_pos = 0;
        let q = self.find.to_lowercase();
        if q.is_empty() {
            return;
        }
        let hits = self.files.get().and_then(|l| l.prepared.get(&self.selected_file)).map(|pr| find_rows(pr, self.mode == Mode::Unified, &q));
        self.find_hits = hits.unwrap_or_default();
        self.scroll_to_hit();
    }

    fn scroll_to_hit(&mut self) {
        let Some(&row) = self.find_hits.get(self.find_pos) else { return };
        if self.mode == Mode::Unified && !self.wrap {
            self.scroll_to_unified = Some(row);
        } else {
            self.scroll_to_row = Some(row);
        }
    }

    fn paint_hit(&self, ui: &Ui, rect: Rect, i: usize, pal: &crate::theme::Palette) {
        if self.find_hits.binary_search(&i).is_ok() {
            let current = self.find_hits.get(self.find_pos) == Some(&i);
            ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(if current { 2.0 } else { 1.0 }, if current { pal.accent } else { pal.accent.gamma_multiply(0.5) }), egui::StrokeKind::Inside);
        }
    }

    fn jump(&mut self, dir: i32) {
        let Some(loaded) = self.files.get() else { return };
        let Some(pr) = loaded.prepared.get(&self.selected_file) else { return };
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

    /// Whether every hunk with a selected line can be turned back into an exact patch.
    fn selection_is_exact(&self) -> bool {
        let Some(f) = self.files.get().and_then(|l| l.files.get(self.selected_file)) else { return true };
        self.selected_lines.iter().all(|(h, _)| f.hunks.get(*h).is_none_or(diff::hunk_is_exact))
    }

    fn apply_selected_lines(&mut self, cx: &mut Cx) {
        let Some(loaded) = self.files.get() else { return };
        let Some(f) = loaded.files.get(self.selected_file) else { return };
        let (mode, reverse, title) = if self.src == DiffSource::Unstaged { (PatchMode::Forward, false, "Stage lines") } else { (PatchMode::Reverse, true, "Unstage lines") };
        for (h, lines) in self.selected_by_hunk() {
            if let Some(patch) = diff::build_patch(f, h, Some(&lines), mode) {
                cx.run(ops::apply_patch(title, patch, true, reverse));
            }
        }
        self.selected_lines.clear();
    }

    fn discard_selected_lines(&mut self, cx: &mut Cx) {
        let Some(loaded) = self.files.get() else { return };
        let Some(f) = loaded.files.get(self.selected_file) else { return };
        // One confirmation covering every hunk, applied from the bottom up.
        let mut op = ops::GitOp::new("Discard lines");
        for (h, lines) in self.selected_by_hunk() {
            if let Some(patch) = diff::build_patch(f, h, Some(&lines), PatchMode::Reverse) {
                op.steps.extend(ops::apply_patch("Discard lines", patch, false, true).steps);
            }
        }
        if !op.steps.is_empty() {
            op = op.undoable(gitem_core::undo::UndoPlan::Discard { label: "Undo discard".to_string(), paths: vec![f.new_path.clone()] });
            let n = self.selected_lines.len();
            cx.confirm("Discard lines", &format!("Discard {} from the working tree? The toolbar's Undo can bring them back straight afterwards.", plural(n, "selected line", "selected lines")), "Discard", op);
        }
        self.selected_lines.clear();
    }

    fn hunk_buttons(&self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, hunk: usize) {
        if !self.can_patch() {
            return;
        }
        // Text that isn't UTF-8 can't be rebuilt into an exact patch.
        if self.can_stage() && f.hunks.get(hunk).is_some_and(|h| !diff::hunk_is_exact(h)) {
            ui.label(RichText::new("Not UTF-8: stage the whole file").small().color(cx.pal.warn));
            return;
        }
        match self.src {
            DiffSource::Unstaged => {
                if crate::widgets::small_button(ui, "Stage hunk").clicked()
                    && let Some(p) = diff::build_patch(f, hunk, None, PatchMode::Forward) {
                        cx.run(ops::apply_patch("Stage hunk", p, true, false));
                    }
                if crate::widgets::small_button(ui, "Discard hunk…").clicked()
                    && let Some(p) = diff::build_patch(f, hunk, None, PatchMode::Reverse) {
                        cx.confirm("Discard hunk", "Discard this hunk from the working tree? The toolbar's Undo can bring it back straight afterwards.", "Discard", ops::apply_patch("Discard hunk", p, false, true).undoable(gitem_core::undo::UndoPlan::Discard { label: "Undo discard".to_string(), paths: vec![f.new_path.clone()] }));
                    }
            }
            DiffSource::Staged => {
                if crate::widgets::small_button(ui, "Unstage hunk").clicked()
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
            ui.label(RichText::new(format!("+{}", pr.additions)).color(pal.add_fg).monospace());
            ui.label(RichText::new(format!("-{}", pr.deletions)).color(pal.del_fg).monospace());
            if let Some(s) = f.similarity {
                ui.label(RichText::new(format!("{s}% similar")).small().color(pal.faint));
            }
            if let (Some(a), Some(b)) = (&f.old_mode, &f.new_mode)
                && !f.is_new && !f.is_deleted {
                    ui.label(RichText::new(format!("mode {a} → {b}")).small().color(pal.faint));
                }
            // Whatever the change is: a binary file, a mode or a plain rename has no hunks.
            let conflicted = f.header.first().is_some_and(|l| l.starts_with("diff --cc "));
            if self.can_stage() && !conflicted {
                let path = f.display_path().to_string();
                if self.src == DiffSource::Unstaged {
                    if crate::widgets::small_button(ui, "Stage file").clicked() {
                        cx.run(ops::stage_paths(&[path]));
                    }
                } else if crate::widgets::small_button(ui, "Unstage file").clicked() {
                    // A rename goes back whole, or its old name stays staged as a deletion.
                    let mut names = vec![path];
                    if f.is_rename {
                        names.push(f.old_path.clone());
                    }
                    let has_head = cx.status().is_some_and(|s| !s.head_sha.is_empty());
                    cx.run(ops::unstage_paths(&names, has_head));
                }
            }
        });
        if crate::svg::is_svg(f.display_path()) && !f.is_binary {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.svg_pictures, false, "Text");
                ui.selectable_value(&mut self.svg_pictures, true, "Pictures");
            });
        }
        if f.is_binary || (self.svg_pictures && crate::svg::is_svg(f.display_path())) {
            self.binary(ui, cx, f);
            return;
        }
        // Moved to a file with no pictures, or back to Text: dropping this kills what's drawing them.
        if self.images.decoding.busy() {
            self.images = ImagePair::default();
        }
        if f.header.first().is_some_and(|l| l.starts_with("diff --cc ")) {
            empty_state(ui, "This file is in conflict", "Git can't show one diff of it until the conflict is settled.", &pal);
            ui.vertical_centered(|ui| {
                if ui.button("Resolve in merge tool").clicked() {
                    cx.open(Tab::merge(f.display_path()));
                }
            });
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
        // The first line's middle, which is the row's while nothing wraps.
        let first_y = rect.top() + (font * 1.45).ceil() / 2.0;
        if let Some(n) = no {
            ui.painter().text(pos2(rect.left() + gutter - 6.0, first_y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
        }
        let changed = matches!(kind, Some(LineKind::Add | LineKind::Del));
        // Red and green can look alike. Unified has these signs beside its line numbers.
        match kind.filter(|_| no.is_some()) {
            Some(LineKind::Add) => {
                ui.painter().text(pos2(rect.left() + 6.0, first_y), Align2::LEFT_CENTER, "+", mono(font - 1.5), pal.add_fg);
            }
            Some(LineKind::Del) => {
                ui.painter().text(pos2(rect.left() + 6.0, first_y), Align2::LEFT_CENTER, "-", mono(font - 1.5), pal.del_fg);
            }
            _ => {}
        }
        if changed && self.can_patch()
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
                        // Work has started here, which the next diff opened shouldn't replace.
                        self.pinned = true;
                    }
                }
            }
        if let Some(t) = text {
            let spans = key.and_then(|k| pr.syntax.get(&k));
            let words = key.and_then(|k| pr.words.get(&k)).map(|v| v.as_slice());
            // A screen of these fits many times over, and each can be hundreds of kilobytes.
            if self.long_jobs.len() >= MAX_LONG_JOBS {
                self.long_jobs.clear();
            }
            let mut job = match key.filter(|_| t.len() >= LONG_LINE) {
                Some((h, l)) => self.long_jobs.entry((self.selected_file, h, l, font.to_bits())).or_insert_with(|| code_job(t, font, pal.text, spans, words, word_bg)).clone(),
                None => code_job(t, font, pal.text, spans, words, word_bg),
            };
            if self.wrap {
                job.wrap.max_width = rect.width() - gutter - 8.0;
                job.wrap.break_anywhere = true;
            }
            let g = ui.painter().layout_job(job);
            let clip = Rect::from_min_max(pos2(rect.left() + gutter, rect.top()), rect.max).intersect(ui.clip_rect());
            // Wrapped lines hang from the first one: the row is tall enough for them.
            let top = if self.wrap { rect.top() + 1.0 } else { rect.center().y - g.size().y / 2.0 };
            ui.painter().with_clip_rect(clip).galley(pos2(rect.left() + gutter + 4.0 - scroll_x, top), g, pal.text);
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
        if self.wrap {
            let half = (avail - 10.0) / 2.0;
            let tops = self.tops_for(ui, true, half - 1.0 - gutter - 8.0, font, row_h, pr);
            self.wrapped_rows(ui, &tops, row_h, ("split-wrapped", self.selected_file), half * 2.0, |view, ui, rect, i| view.split_row(ui, cx, f, pr, rect, i, half, gutter, 0.0, font, &pal));
            return;
        }
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
                let (rect, _) = ui.allocate_exact_size(vec2(half * 2.0, row_h), Sense::hover());
                self.split_row(ui, cx, f, pr, rect, i, half, gutter, scroll_x, font, &pal);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn split_row(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared, rect: Rect, i: usize, half: f32, gutter: f32, scroll_x: f32, font: f32, pal: &crate::theme::Palette) {
        let r = &pr.side[i];
        if r.is_hunk_header {
            ui.painter().rect_filled(rect, 0.0, pal.panel2);
            let vis = ui.clip_rect();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(vis.left() + 8.0, rect.top()), pos2(vis.right(), rect.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.label(RichText::new(&r.header).monospace().size(font - 1.5).color(pal.dim));
            child.add_space(8.0);
            self.hunk_buttons(&mut child, cx, f, r.hunk);
            return;
        }
        let lrect = Rect::from_min_size(rect.min, vec2(half - 1.0, rect.height()));
        let rrect = Rect::from_min_size(pos2(rect.left() + half + 1.0, rect.top()), vec2(half - 1.0, rect.height()));
        let id = ui.id().with(("row", i));
        self.draw_cell(ui, pal, lrect, r.left.as_ref().map(|x| x.0), r.left.as_ref().map(|x| x.1.as_str()), r.left_kind, r.left_idx.map(|l| (r.hunk, l)), pr, font, gutter, scroll_x, id.with("l"));
        self.paint_hit(ui, rect, i, pal);
        self.draw_cell(ui, pal, rrect, r.right.as_ref().map(|x| x.0), r.right.as_ref().map(|x| x.1.as_str()), r.right_kind, r.right_idx.map(|l| (r.hunk, l)), pr, font, gutter, scroll_x, id.with("r"));
    }

    /// Where each row starts while wrapping, kept until the text or its width changes: this
    /// walks every line.
    fn tops_for(&mut self, ui: &Ui, split: bool, width: f32, font: f32, row_h: f32, pr: &Prepared) -> std::sync::Arc<Vec<f32>> {
        let rows = if split { pr.side.len() } else { pr.unified.len() };
        let key = (width.to_bits(), font.to_bits());
        if let Some((s, k, file, tops)) = &self.wrapped_tops
            && *s == split
            && *k == key
            && *file == self.selected_file
            && tops.len() == rows + 1
        {
            return tops.clone();
        }
        let lines = |text: Option<&str>| text.map_or(1, |t| wrapped_line_count(ui.ctx(), t, width, font));
        let heights: Vec<f32> = if split {
            pr.side.iter().map(|r| lines(r.left.as_ref().map(|x| x.1.as_str())).max(lines(r.right.as_ref().map(|x| x.1.as_str()))) as f32 * row_h).collect()
        } else {
            pr.unified_texts.iter().map(|t| lines(t.as_deref()) as f32 * row_h).collect()
        };
        let mut tops = Vec::with_capacity(heights.len() + 1);
        let mut y = 0.0;
        tops.push(y);
        for h in heights {
            y += h;
            tops.push(y);
        }
        let tops = std::sync::Arc::new(tops);
        self.wrapped_tops = Some((split, key, self.selected_file, tops.clone()));
        tops
    }

    /// Rows of differing height in one long list. Only those on screen are drawn.
    fn wrapped_rows(&mut self, ui: &mut Ui, tops: &[f32], row_h: f32, id: (&'static str, usize), width: f32, mut draw: impl FnMut(&mut Self, &mut Ui, Rect, usize)) {
        let mut sa = egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]);
        if let Some(r) = self.scroll_to_row.take() {
            sa = sa.vertical_scroll_offset((tops.get(r).copied().unwrap_or(0.0) - 40.0).max(0.0));
        }
        let total = tops.last().copied().unwrap_or(0.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_viewport(ui, |ui, view| {
            ui.set_min_size(vec2(width, total.max(row_h)));
            let origin = ui.min_rect().min;
            let first = tops.partition_point(|t| *t <= view.top()).saturating_sub(1);
            let last = tops.partition_point(|t| *t < view.bottom()).min(tops.len().saturating_sub(1));
            for i in first..last {
                let rect = Rect::from_min_size(origin + vec2(0.0, tops[i]), vec2(width, tops[i + 1] - tops[i]));
                draw(self, ui, rect, i);
            }
        });
    }

    fn unified(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared) {
        let pal = cx.pal.clone();
        let font = cx.settings.editor_font_size;
        let row_h = (font * 1.45).ceil();
        let gutter = 96.0;
        let (rows, max_chars) = (&pr.unified, pr.unified_chars);
        let char_w = ui.fonts_mut(|fx| fx.glyph_width(&mono(font), 'm'));
        if self.wrap {
            let width = ui.available_width() - 10.0;
            let tops = self.tops_for(ui, false, width - gutter - 8.0, font, row_h, pr);
            self.wrapped_rows(ui, &tops, row_h, ("unified-wrapped", self.selected_file), width, |view, ui, rect, i| view.unified_row(ui, cx, f, pr, rect, i, gutter, font, &pal));
            return;
        }
        let width = ((max_chars as f32 * char_w) + gutter + 24.0).max(ui.available_width() - 10.0);
        let mut sa = egui::ScrollArea::both().id_salt(("unified", self.selected_file)).auto_shrink([false, false]);
        if let Some(y) = self.scroll_to_unified.take() {
            sa = sa.vertical_scroll_offset((y as f32 * row_h - 40.0).max(0.0));
        } else if let Some(r) = self.scroll_to_row.take() {
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
                let (rect, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
                self.unified_row(ui, cx, f, pr, rect, i, gutter, font, &pal);
                self.paint_hit(ui, rect, i, &pal);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn unified_row(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff, pr: &Prepared, rect: Rect, i: usize, gutter: f32, font: f32, pal: &crate::theme::Palette) {
        let (hi, li) = pr.unified[i];
        let h = &f.hunks[hi];
        let Some(li) = li else {
            ui.painter().rect_filled(rect, 0.0, pal.panel2);
            let vis = ui.clip_rect();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(vis.left() + 8.0, rect.top()), pos2(vis.right(), rect.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.label(RichText::new(format!("@@ -{},{} +{},{} @@ {}", h.old_start, h.old_count, h.new_start, h.new_count, h.section)).monospace().size(font - 1.5).color(pal.dim));
            child.add_space(8.0);
            self.hunk_buttons(&mut child, cx, f, hi);
            return;
        };
        let l = &h.lines[li];
        if l.kind == LineKind::NoNewline {
            paint_text(ui, Rect::from_min_max(pos2(rect.left() + gutter, rect.top()), rect.max), &format!("\\ {}", l.text), mono(font - 1.5), pal.faint);
            return;
        }
        let id = ui.id().with(("urow", i));
        let text = pr.marked.get(&(hi, li)).unwrap_or(&l.text);
        self.draw_cell(ui, pal, rect, None, Some(text), Some(l.kind), Some((hi, li)), pr, font, gutter, 0.0, id);
        let first_y = rect.top() + (font * 1.45).ceil() / 2.0;
        if let Some(n) = l.old_no {
            ui.painter().text(pos2(rect.left() + 42.0, first_y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
        }
        if let Some(n) = l.new_no {
            ui.painter().text(pos2(rect.left() + 84.0, first_y), Align2::RIGHT_CENTER, n.to_string(), mono(font - 1.5), pal.faint);
        }
        let sign = match l.kind {
            LineKind::Add => "+",
            LineKind::Del => "-",
            _ => "",
        };
        ui.painter().text(pos2(rect.left() + gutter - 4.0, first_y), Align2::RIGHT_CENTER, sign, mono(font), pal.dim);
    }

    /// Binary file: show images side by side, otherwise a message.
    fn binary(&mut self, ui: &mut Ui, cx: &mut Cx, f: &FileDiff) {
        let pal = cx.pal.clone();
        let path = f.display_path().to_string();
        let svg = crate::svg::is_svg(&path);
        let is_image = svg || crate::widgets::PICTURE_TYPES.iter().any(|e| path.to_lowercase().ends_with(&format!(".{e}")));
        if !is_image {
            empty_state(ui, "Binary file", "This file can't be compared as text.", &pal);
            return;
        }
        let key = format!("{:?}:{path}", self.src);
        if self.images.loaded_for.as_deref() != Some(&key) {
            self.images = ImagePair { loaded_for: Some(key), ..Default::default() };
            let (src, old_path, new_path) = (self.src.clone(), f.old_path.clone(), path.clone());
            // Up to 32 MB each to read and decode, which is no job for a frame.
            self.images.decoding.spawn_git(ui.ctx(), cx.repo, move |repo| -> Result<PicturePair, String> {
                let (old_rev, new_rev): (Option<String>, Option<String>) = match &src {
                    DiffSource::Commit(s) => (Some(format!("{s}^")), Some(s.clone())),
                    DiffSource::Range(a, b) => (Some(a.clone()), Some(b.clone())),
                    DiffSource::MergeBase(a, b) => (repo.merge_base(a, b).ok(), Some(b.clone())),
                    DiffSource::Staged => (Some("HEAD".into()), Some(String::new())),
                    DiffSource::Unstaged => (Some(String::new()), None),
                    DiffSource::WorkingVsHead => (Some("HEAD".into()), None),
                    DiffSource::WorkingVs(r) => (Some(r.clone()), None),
                    _ => (None, None),
                };
                let old = old_rev.and_then(|r| repo.file_at(&r, &old_path).ok());
                let new = match new_rev {
                    Some(r) => repo.file_at(&r, &new_path).ok(),
                    None => repo.worktree_file(&new_path).ok(),
                };
                let mut why = None;
                let mut decode = |bytes: Option<Vec<u8>>| {
                    let b = bytes.filter(|b| b.len() < MAX_INLINE_IMAGE)?;
                    if !svg {
                        return decode_image(&b);
                    }
                    // Set when this pair is replaced or the view closes, which kills the renderer.
                    match crate::svg::render(&b, repo.git.cancel.as_deref()) {
                        Ok(d) => {
                            if d.font_missing {
                                why = Some(crate::svg::FONT_MISSING.to_string());
                            }
                            Some(d.image)
                        }
                        Err(e) => {
                            why = Some(format!("One of these can't be drawn: {e}"));
                            None
                        }
                    }
                };
                let (old, new) = (decode(old), decode(new));
                Ok((old, new, why))
            });
        }
        if self.images.decoding.poll()
            && let Some((old, new, why)) = self.images.decoding.value.take()
        {
            self.images.why = why;
            let load = |img: Option<egui::ColorImage>, name: &str| img.map(|i| ui.ctx().load_texture(name, i, egui::TextureOptions::LINEAR));
            self.images.old = load(old, "diff-old");
            self.images.new = load(new, "diff-new");
        }
        if self.images.decoding.busy() {
            spinner_row(ui, "Loading images…", &pal);
            return;
        }
        if let Some(why) = &self.images.why {
            ui.colored_label(pal.warn, why);
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
                        // An SVG that couldn't be drawn is there all the same, and the line above says why.
                        ui.label(RichText::new(if svg && self.images.why.is_some() { "Not present, or not drawn" } else { "Not present" }).color(pal.faint));
                    }
                }
            }
        });
    }
}

/// Characters of a monospace font that fit in a width, at least a few.
fn wrap_columns(width: f32, char_w: f32) -> usize {
    ((width / char_w.max(1.0)).floor() as usize).max(8)
}

/// Rows a line of this many characters takes when wrapped.
pub fn wrapped_lines(chars: usize, columns: usize) -> usize {
    chars.max(1).div_ceil(columns.max(1))
}

/// Rows a line takes when wrapped to `width`, as `code_job` draws it. Plain ASCII is one width
/// in a monospace font, so it's counted. Anything else can come from a fallback font twice as
/// wide, as CJK does, and is measured glyph by glyph the way the layout breaks a line.
pub fn wrapped_line_count(ctx: &egui::Context, text: &str, width: f32, font: f32) -> usize {
    let id = mono(font);
    if text.is_ascii() && !text.contains('\t') {
        let char_w = ctx.fonts_mut(|fx| fx.glyph_width(&id, 'm'));
        return wrapped_lines(shown_chars(text), wrap_columns(width, char_w));
    }
    let shown = gitem_core::util::prefix(text, crate::widgets::MAX_SHOWN_LINE_BYTES);
    ctx.fonts_mut(|fx| {
        let (mut lines, mut x) = (1, 0.0);
        for c in shown.chars() {
            let w = fx.glyph_width(&id, c);
            if x + w > width && x > 0.0 {
                lines += 1;
                x = 0.0;
            }
            x += w;
        }
        lines
    })
}

fn short(s: &str) -> &str {
    gitem_core::util::short_rev(s)
}

fn leaf(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}
