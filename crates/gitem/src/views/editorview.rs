//! A file of the working tree, open for editing. The text is the editor's; this view reads and
//! writes the file, watches for it changing on disk, and keeps the repository in view: what
//! changed against the index and HEAD, beside each line, and who last touched the line.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use egui::{RichText, Ui};
use gitem_core::diff::{LineChange, hunk_patch, line_changes};
use gitem_core::model::BlameLine;
use gitem_core::ops;
use gitem_core::repo::{BlameOpts, DiffSource};
use gitem_text::indent::Indent;
use gitem_text::{Buffer, Edit, Encoding, LineEnding, Transaction, Unsupported};

use crate::action::{Action, Cx};
use crate::editor::view::{self, Look, Mark};
use crate::editor::{Cmd, Editor};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

/// A line longer than this makes the file read-only: laying it out on every keystroke would
/// stall the window, and nobody edits a minified bundle by hand.
const LONGEST_EDITABLE_LINE: usize = 100_000;

/// Past this many chars, gutter marks are worked out only after typing pauses.
const LIVE_MARKS_CHARS: usize = 1_000_000;

fn hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The outline box: what's typed, the definitions, and which is picked.
struct Outline {
    query: String,
    symbols: Vec<crate::editor::outline::Symbol>,
    picked: usize,
    focus: bool,
}

enum Opened {
    Text(Buffer, u64),
    /// Not text that can be written back as it was.
    Unsupported(Unsupported),
}

/// The file as the index and HEAD have it, for the gutter.
#[derive(Default)]
struct Bases {
    index: Option<String>,
    head: Option<String>,
}

#[derive(Default)]
pub struct EditorView {
    /// Relative to the repository root. Empty for the welcome page.
    pub path: String,
    /// Kept open. A preview, not yet kept, gives way to the next file clicked.
    pub pinned: bool,
    ed: Option<Editor>,
    load: Task<Opened>,
    started: bool,
    /// The bytes on disk as last read or written, by hash, to notice a change under us.
    disk: Option<u64>,
    /// What's wrong with opening this file, said above it.
    problem: Option<String>,
    /// The file's new bytes, changed on disk while there were edits here.
    conflict: Option<Vec<u8>>,
    /// The file is gone from disk. Saving puts it back.
    gone: bool,
    check: Task<Option<Vec<u8>>>,
    seen_gen: u64,
    goto: Option<usize>,
    dark: bool,
    bases_task: Task<Bases>,
    bases: Bases,
    bases_gen: u64,
    marks: HashMap<usize, Mark>,
    staged_marks: HashMap<usize, Mark>,
    /// The changes against the index, for staging and reverting from the gutter.
    changes: Vec<LineChange>,
    marks_for: Option<(u64, u64)>,
    last_edit: f64,
    blame_task: Task<Vec<BlameLine>>,
    blame: Vec<BlameLine>,
    /// The text blame was for: the file as saved.
    blamed_text: String,
    /// The change whose actions show above the text.
    picked: Option<LineChange>,
    /// Unsaved edits against the blamed text, for the text's version they were worked out for.
    blame_map: Option<(u64, Vec<LineChange>)>,
    /// The encoding was changed: the file differs from what's on disk though the text doesn't.
    recoded: bool,
    /// Blame is out of date: the file was just opened or saved.
    reblame: bool,
    /// The list of definitions, while it's open.
    outline: Option<Outline>,
    /// A language server for this file, when one is attached. None ships with gitem: this is
    /// where one would go (see `editor::lsp`).
    pub lsp: Option<Box<dyn crate::editor::lsp::LanguageServer>>,
    /// The text version the server was last told about.
    lsp_version: u64,
    /// The go-to box was showing last frame. It takes the keyboard once, as it opens.
    goto_shown: bool,
}

impl EditorView {
    pub fn new(path: &str, line: Option<usize>, pinned: bool) -> Self {
        Self { path: path.to_string(), goto: line, pinned, ..Default::default() }
    }

    pub fn key(&self) -> String {
        format!("edit:{}", self.path)
    }

    pub fn title(&self) -> String {
        if self.path.is_empty() {
            return "Editor".into();
        }
        let leaf = self.path.rsplit('/').next().unwrap_or(&self.path);
        if self.is_dirty() { format!("● {leaf}") } else { leaf.to_string() }
    }

    pub fn is_dirty(&self) -> bool {
        self.ed.as_ref().is_some_and(|e| e.is_dirty()) || self.recoded
    }

    pub fn unsaved(&self) -> Option<String> {
        self.is_dirty().then(|| format!("Unsaved edits to {}", self.path))
    }

    /// Asked to open again, perhaps at a line: goes there, and a double click keeps it.
    pub fn retarget(&mut self, new: Self) {
        if new.goto.is_some() {
            self.goto = new.goto;
        }
        self.pinned |= new.pinned;
    }

    /// Text and bytes, for tests and for the window-close check.
    pub fn text(&self) -> Option<String> {
        self.ed.as_ref().map(|e| e.buf.text())
    }

    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        self.ed.as_mut()
    }

    fn start_load(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.started = true;
        let path = self.path.clone();
        self.load.spawn_git(ctx, cx.repo, move |repo| -> Result<Opened, String> {
            let bytes = repo.worktree_file(&path).map_err(|e| e.to_string())?;
            let h = hash(&bytes);
            Ok(match Buffer::from_bytes(&bytes) {
                Ok(mut b) => {
                    // No line break to go by: git's settings for the file decide.
                    if !b.eol_was_found() {
                        b.eol = if repo.crlf_for(&path) { LineEnding::CrLf } else { LineEnding::Lf };
                    }
                    Opened::Text(b, h)
                }
                Err(why) => Opened::Unsupported(why),
            })
        });
        self.load_bases(ctx, cx);
    }

    fn load_bases(&mut self, ctx: &egui::Context, cx: &Cx) {
        let path = self.path.clone();
        self.bases_task.spawn_git(ctx, cx.repo, move |repo| -> Result<Bases, String> {
            let text = |b: Vec<u8>| gitem_text::encoding::decode(&b).ok().map(|t| t.0);
            let tracked = repo.is_tracked(&path);
            let index = if tracked { text(repo.stage_content(0, &path)) } else { None };
            let head = repo.file_at("HEAD", &path).ok().and_then(text);
            Ok(Bases { index, head })
        });
    }

    fn load_blame(&mut self, ctx: &egui::Context, cx: &Cx) {
        // As in the file view: blame of a file this big takes minutes.
        if !cx.settings.inline_blame || self.ed.as_ref().is_some_and(|e| e.buf.len_chars() > 10_000_000) {
            return;
        }
        let path = self.path.clone();
        self.blamed_text = self.ed.as_ref().map(|e| e.buf.text()).unwrap_or_default();
        self.blame_map = None;
        self.blame_task.spawn_git(ctx, cx.repo, move |repo| repo.blame(&path, None, BlameOpts::default()).map_err(|e| e.to_string()));
    }

    fn default_indent(cx: &Cx) -> Indent {
        if cx.settings.editor_insert_spaces { Indent::Spaces(cx.settings.editor_tab_size.clamp(1, 16)) } else { Indent::Tabs }
    }

    fn opened(&mut self, cx: &mut Cx, o: Opened) {
        match o {
            Opened::Text(buf, h) => {
                let longest = (0..buf.line_count()).map(|i| buf.rope.line(i).len_chars()).max().unwrap_or(0);
                let mut ed = Editor::new(&self.path, buf, cx.pal.dark, Self::default_indent(cx));
                if longest > LONGEST_EDITABLE_LINE {
                    ed.read_only = true;
                    self.problem = Some(format!("A line of this file is {} characters long, so it's shown without editing.", longest));
                }
                if let Some(line) = self.goto.take() {
                    ed.go_to_line(line);
                }
                if let Some(s) = &mut self.lsp {
                    s.did_open(&self.path, &ed.buf.text());
                }
                self.lsp_version = ed.buf.version;
                self.ed = Some(ed);
                self.disk = Some(h);
                self.reblame = true;
                self.remember(cx);
            }
            Opened::Unsupported(Unsupported::Binary) => self.problem = Some("This is a binary file, which can't be edited as text.".into()),
        }
        self.dark = cx.pal.dark;
    }

    /// Puts this file first in the repository's recent files.
    fn remember(&self, cx: &mut Cx) {
        let (key, path) = (cx.repo.root.to_string_lossy().replace('\\', "/"), self.path.clone());
        cx.out.push(Action::Settings(Box::new(move |s| {
            let list = s.editor_recent.entry(key).or_default();
            list.retain(|p| *p != path);
            list.insert(0, path);
            list.truncate(20);
        })));
    }

    /// Writes the text to disk, unless the file changed there since it was read.
    pub fn save(&mut self, cx: &mut Cx) -> bool {
        let recoded = self.recoded;
        let Some(ed) = &mut self.ed else { return false };
        if ed.read_only || !(ed.is_dirty() || recoded) {
            return false;
        }
        if self.conflict.is_some() {
            cx.error("This file changed on disk. Choose to keep your text or load theirs first.");
            return false;
        }
        if cx.settings.editor_trim_on_save {
            let t = gitem_text::lines::trim_trailing(&ed.buf);
            ed.transform(t, 0);
        }
        let bytes = match ed.buf.to_bytes() {
            Ok(b) => b,
            Err(why) => {
                cx.error(format!("{} can't be saved: {why}", self.path));
                return false;
            }
        };
        let abs = cx.repo.root.join(&self.path);
        if let (Ok(now), Some(seen)) = (std::fs::read(&abs), self.disk)
            && hash(&now) != seen
        {
            self.conflict = Some(now);
            return false;
        }
        match gitem_core::util::write_file_atomically(&abs, &bytes) {
            Ok(in_place) => {
                if in_place {
                    cx.toast(format!("Saved {} in place: it couldn't be swapped for a new copy.", self.path));
                }
                self.disk = Some(hash(&bytes));
                self.gone = false;
                self.recoded = false;
                ed.hist.mark_saved();
                self.pinned = true;
                cx.out.push(Action::FileSaved(self.path.clone()));
                self.reblame = true;
                if let Some(s) = &mut self.lsp {
                    s.did_save(&self.path);
                }
                true
            }
            Err(e) => {
                cx.error(format!("Couldn't save {}: {e}", self.path));
                false
            }
        }
    }

    /// Whether the file on disk is still what was read, after the repository changed.
    fn look_at_disk(&mut self, ctx: &egui::Context, cx: &Cx) {
        let generation = cx.data.status_gen;
        if generation == self.seen_gen || self.ed.is_none() {
            return;
        }
        self.seen_gen = generation;
        let path = self.path.clone();
        self.check.spawn_git(ctx, cx.repo, move |repo| -> Result<Option<Vec<u8>>, String> { Ok(repo.worktree_file(&path).ok()) });
        // The index or HEAD may have moved too: a commit, a stage, a checkout.
        self.load_bases(ctx, cx);
    }

    fn checked(&mut self, ctx: &egui::Context, cx: &mut Cx, now: Option<Vec<u8>>) {
        let Some(ed) = &mut self.ed else { return };
        let Some(bytes) = now else {
            self.gone = true;
            return;
        };
        self.gone = false;
        if Some(hash(&bytes)) == self.disk {
            return;
        }
        if ed.is_dirty() {
            self.conflict = Some(bytes);
            return;
        }
        self.take_from_disk(ctx, cx, bytes);
    }

    fn take_from_disk(&mut self, ctx: &egui::Context, cx: &mut Cx, bytes: Vec<u8>) {
        let Some(ed) = &mut self.ed else { return };
        match Buffer::from_bytes(&bytes) {
            Ok(b) => {
                ed.reload(b);
                self.disk = Some(hash(&bytes));
                self.conflict = None;
                self.load_blame(ctx, cx);
            }
            Err(_) => self.problem = Some("The file on disk can't be edited as text any more.".into()),
        }
    }

    /// Gutter marks for the text as it is, against the index and against HEAD.
    fn refresh_marks(&mut self, now: f64) {
        let Some(ed) = &self.ed else { return };
        let key = (ed.buf.version, self.bases_gen);
        if self.marks_for == Some(key) {
            return;
        }
        // A big file waits for a pause in typing.
        if ed.buf.len_chars() > LIVE_MARKS_CHARS && now - self.last_edit < 0.5 {
            return;
        }
        self.marks_for = Some(key);
        let text = ed.buf.text();
        self.marks.clear();
        self.staged_marks.clear();
        self.changes.clear();
        let Some(index) = &self.bases.index else {
            // Not in the index: all of it is new.
            if !self.bases_task.busy() {
                for l in 0..ed.buf.line_count() {
                    self.marks.insert(l, Mark::Added);
                }
            }
            return;
        };
        self.changes = line_changes(index, &comparable(&text, index));
        put_marks(&self.changes, &mut self.marks);
        if let Some(head) = &self.bases.head {
            let mut against_head = HashMap::new();
            put_marks(&line_changes(head, &comparable(&text, head)), &mut against_head);
            for (l, m) in against_head {
                if !self.marks.contains_key(&l) {
                    self.staged_marks.insert(l, m);
                }
            }
        }
    }

    /// The change that a line is in, or that sits just above it as a deletion.
    fn change_at(&self, line: usize) -> Option<LineChange> {
        self.changes.iter().copied().find(|c| (c.new_len == 0 && c.new_start == line) || (c.new_start..c.new_start + c.new_len).contains(&line))
    }

    fn stage_change(&mut self, cx: &mut Cx, c: LineChange) {
        if self.is_dirty() && !self.save(cx) {
            return;
        }
        let (Some(ed), Some(index)) = (&self.ed, &self.bases.index) else { return };
        let text = comparable(&ed.buf.text(), index);
        let patch = hunk_patch(&self.path, index, &text, &c);
        cx.run(ops::apply_patch("Stage change", patch, true, false));
    }

    /// The lines of a change put back as the index has them, as one undoable edit.
    fn revert_change(&mut self, c: LineChange, now_ms: u64) {
        let (Some(ed), Some(index)) = (&mut self.ed, &self.bases.index) else { return };
        let old: Vec<&str> = index.split_inclusive('\n').collect();
        let eol = ed.buf.eol.as_str();
        let mut text: String = old[c.old_start..c.old_start + c.old_len].iter().map(|l| format!("{}{eol}", l.trim_end_matches(['\n', '\r']))).collect();
        let start = ed.buf.line_start(c.new_start);
        let end = if c.new_start + c.new_len < ed.buf.line_count() { ed.buf.line_start(c.new_start + c.new_len) } else { ed.buf.len_chars() };
        // At the end of a file with no last line break, the old text shouldn't add one.
        if end == ed.buf.len_chars() && !ed.buf.text().ends_with('\n') && c.old_start + c.old_len == old.len() && !index.ends_with('\n') {
            text.truncate(text.len() - eol.len());
        }
        ed.transform(Transaction::new(vec![Edit::replace(start, end, text)]), now_ms);
    }

    /// The blame line for a line of the text as it is now, through any unsaved edits.
    fn blame_for(&mut self, line: usize, fmt: &str, now: f64) -> Option<String> {
        let ed = self.ed.as_ref()?;
        if self.blame.is_empty() {
            return None;
        }
        let version = ed.buf.version;
        if self.blame_map.as_ref().is_none_or(|(v, _)| *v != version) {
            // Worked out once per change, and for a big file once typing pauses.
            if ed.buf.len_chars() > LIVE_MARKS_CHARS && now - self.last_edit < 0.5 {
                return None;
            }
            let unsaved = if ed.is_dirty() { line_changes(&self.blamed_text, &ed.buf.text()) } else { Vec::new() };
            self.blame_map = Some((version, unsaved));
        }
        let unsaved = &self.blame_map.as_ref()?.1;
        let saved_line = map_back(unsaved, line)?;
        let b = self.blame.get(saved_line)?;
        let now = chrono_now();
        Some(crate::views::fileview::format_blame(fmt, b, now))
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        if self.path.is_empty() {
            return self.welcome(ui, cx);
        }
        let ctx = ui.ctx().clone();
        if !self.started {
            self.start_load(&ctx, cx);
            self.seen_gen = cx.data.status_gen;
        }
        if self.load.poll()
            && let Some(o) = self.load.value.take()
        {
            self.opened(cx, o);
        }
        if self.check.poll()
            && let Some(now) = self.check.value.take()
        {
            self.checked(&ctx, cx, now);
        }
        if self.bases_task.poll()
            && let Some(b) = self.bases_task.value.take()
        {
            self.bases = b;
            self.bases_gen += 1;
        }
        if self.blame_task.poll()
            && let Some(b) = self.blame_task.value.take()
        {
            self.blame = b;
        }
        if let Some(e) = &self.load.error {
            empty_state(ui, "This file can't be opened", e, cx.pal);
            return;
        }
        self.look_at_disk(&ctx, cx);
        if std::mem::take(&mut self.reblame) {
            self.load_blame(&ctx, cx);
        }
        // Colours follow the theme.
        if let Some(ed) = &mut self.ed
            && self.dark != cx.pal.dark
        {
            self.dark = cx.pal.dark;
            ed.hl = crate::editor::highlight::Highlight::new(&self.path, self.dark);
        }
        let now = ui.input(|i| i.time);
        let now_ms = (now * 1000.0) as u64;
        self.refresh_marks(now);
        self.toolbar(ui, cx, now_ms);
        self.banners(ui, cx);
        let Some(ed) = &self.ed else {
            if self.problem.is_none() {
                spinner_row(ui, "Opening…", cx.pal);
            }
            return;
        };
        if ed.find.open {
            self.find_bar(ui, cx, now_ms);
        }
        if self.ed.as_ref().is_some_and(|e| e.goto.is_some()) {
            self.goto_bar(ui, cx);
        }
        if let Some(c) = self.picked {
            self.change_bar(ui, cx, c, now_ms);
        }
        if self.outline.is_some() {
            self.outline_box(ui, cx);
        }
        #[cfg(feature = "tree-sitter")]
        if let Some(ed) = &mut self.ed
            && ed.syntax_for != ed.buf.version
            && ed.buf.len_chars() < 4_000_000
            && now - self.last_edit > 0.4
        {
            let text = ed.buf.text();
            ed.syntax_folds = crate::editor::treesitter::fold_ranges(&self.path, &text).unwrap_or_default();
            ed.syntax_brackets = crate::editor::treesitter::bracket_pairs(&self.path, &text).unwrap_or_default();
            ed.syntax_for = ed.buf.version;
        }
        let caret_line = self.ed.as_ref().filter(|e| e.sels.primary().is_empty()).map(|e| e.buf.line_of(e.sels.primary().head));
        let note = caret_line.and_then(|l| self.blame_for(l, &cx.settings.inline_blame_format, now));
        let Some(ed) = &mut self.ed else { return };
        let version = ed.buf.version;
        if let Some(s) = &mut self.lsp
            && self.lsp_version != version
        {
            s.did_change(&self.path, version, &ed.buf.text());
            self.lsp_version = version;
        }
        let underlines = match &self.lsp {
            Some(s) => crate::editor::lsp::underlines(&s.diagnostics(&self.path), |l| ed.buf.line(l).chars().count()),
            None => Vec::new(),
        };
        let look = Look {
            font_size: cx.settings.editor_font_size,
            pal: cx.pal,
            whitespace: cx.settings.editor_show_whitespace,
            indent_guides: cx.settings.editor_indent_guides,
            wrap: cx.settings.editor_word_wrap,
            minimap: cx.settings.editor_minimap,
            marks: &self.marks,
            staged_marks: &self.staged_marks,
            line_note: note.as_deref(),
            keys: &cx.settings.key_bindings,
            underlines: &underlines,
        };
        let shown = view::show(ui, ed, &look);
        if ed.buf.version != version {
            self.last_edit = now;
            // Typing keeps a preview open.
            self.pinned = true;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(600));
        }
        if shown.save {
            self.save(cx);
        }
        if shown.outline {
            self.open_outline();
        }
        if shown.toggle_wrap {
            cx.out.push(Action::Settings(Box::new(|s| s.editor_word_wrap = !s.editor_word_wrap)));
        }
        if let Some(line) = shown.gutter_click {
            self.picked = self.change_at(line);
        }
        if let Some(resp) = shown.response {
            resp.context_menu(|ui| self.text_menu(ui, cx, now_ms));
        }
    }

    /// Carries out a request from the palette for this, the editor in front.
    pub fn request(&mut self, cx: &mut Cx, r: crate::editor::Request) {
        use crate::editor::Request;
        if r == Request::Save {
            self.save(cx);
            return;
        }
        let Some(ed) = &mut self.ed else { return };
        match r {
            Request::Run(cmd) => ed.run(cmd, 0),
            Request::Find | Request::Replace => {
                ed.find.open = true;
                ed.find.replacing = r == Request::Replace;
                ed.find.focus_query = true;
            }
            Request::GoToLine => ed.goto = Some(String::new()),
            Request::TrimTrailing => {
                let t = gitem_text::lines::trim_trailing(&ed.buf);
                ed.transform(t, 0);
            }
            Request::Outline => {
                self.open_outline();
                return;
            }
            Request::FindInFiles => cx.open(Tab::Grep(Box::new(crate::views::grep::GrepView::looking_for(&ed.search_seed(), "")))),
            Request::Save => {}
        }
        ed.focus = true;
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx, now_ms: u64) {
        let p = cx.pal;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.path).color(p.dim));
            let dirty = self.is_dirty();
            if ui.add_enabled(dirty && self.ed.as_ref().is_some_and(|e| !e.read_only), egui::Button::new("Save")).on_disabled_hover_text("Nothing to save, or the file is read-only.").on_hover_text(crate::keys::context_label(&cx.settings.key_bindings, "editor_save")).clicked() {
                self.save(cx);
            }
            if small_button(ui, "Diff").on_hover_text("This file's uncommitted changes").clicked() {
                cx.diff(DiffSource::WorkingVsHead, Some(&self.path));
            }
            if small_button(ui, "Blame").on_hover_text("This file with who changed each line, and when").clicked() {
                let line = self.ed.as_ref().map(|e| e.buf.point(e.sels.primary().head).0 as u32 + 1);
                cx.open(Tab::file(&self.path, None, line));
            }
            if small_button(ui, "History").on_hover_text("The commits that changed this file").clicked() {
                cx.open(Tab::file_history(&self.path));
            }
            let path = self.path.clone();
            ui.menu_button("More", |ui| crate::menus::file_menu(ui, cx, &path, None));
            let Some(ed) = &mut self.ed else { return };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The file's encoding, line endings and indent, each a menu to change it.
                let (enc, bom) = (ed.buf.encoding, ed.buf.bom);
                let mut to = None;
                ui.menu_button(RichText::new(enc.name(bom)).small(), |ui| {
                    ui.label(RichText::new("Save as").small().color(p.dim));
                    for (e, b) in [(Encoding::Utf8, false), (Encoding::Utf8, true), (Encoding::Utf16Le, true), (Encoding::Windows1252, false)] {
                        if ui.add_enabled(!ed.read_only, egui::Button::selectable((e, b) == (enc, bom), e.name(b))).on_disabled_hover_text("This file is read-only.").clicked() {
                            to = Some((e, b));
                            ui.close();
                        }
                    }
                });
                if let Some((e, b)) = to
                    && (e, b) != (enc, bom)
                {
                    // Nothing in the text changed, but the file will: it counts as unsaved.
                    ed.buf.encoding = e;
                    ed.buf.bom = b;
                    self.recoded = true;
                }
                let eol = ed.buf.eol;
                ui.menu_button(RichText::new(eol.name()).small(), |ui| {
                    for (to, name) in [(LineEnding::Lf, "Convert to LF"), (LineEnding::CrLf, "Convert to CRLF")] {
                        if ui.add_enabled(!ed.read_only, egui::Button::new(name)).on_disabled_hover_text("This file is read-only.").clicked() {
                            let t = ed.buf.convert_eol(to);
                            ed.buf.eol = to;
                            ed.transform(t, now_ms);
                            ui.close();
                        }
                    }
                });
                ui.menu_button(RichText::new(ed.indent.name()).small(), |ui| {
                    for choice in [Indent::Tabs, Indent::Spaces(2), Indent::Spaces(4), Indent::Spaces(8)] {
                        if ui.selectable_label(ed.indent == choice, choice.name()).clicked() {
                            ed.indent = choice;
                            ui.close();
                        }
                    }
                });
                let p0 = ed.sels.primary();
                let (line, col) = ed.buf.point(p0.head);
                let mut pos = format!("Ln {}, Col {}", line + 1, col + 1);
                if ed.sels.len() > 1 {
                    pos.push_str(&format!(" · {} carets", ed.sels.len()));
                } else if !p0.is_empty() {
                    pos.push_str(&format!(" · {} selected", p0.end() - p0.start()));
                }
                if ui.add(egui::Label::new(RichText::new(pos).small().color(p.dim)).sense(egui::Sense::click())).on_hover_text("Go to line").clicked() {
                    ed.goto = Some(String::new());
                }
            });
        });
    }

    fn banners(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal;
        if let Some(why) = &self.problem {
            ui.colored_label(p.warn, why.as_str());
        }
        if self.gone {
            ui.colored_label(p.warn, "This file was deleted on disk. Saving puts it back.");
        }
        if self.conflict.is_some() {
            let ctx = ui.ctx().clone();
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(p.warn, "This file changed on disk while you were editing it.");
                if small_button(ui, "Load from disk").on_hover_text("Your edits here are lost").clicked()
                    && let Some(b) = self.conflict.take()
                {
                    self.take_from_disk(&ctx, cx, b);
                }
                if small_button(ui, "Keep mine").on_hover_text("Saving will replace what's on disk").clicked()
                    && let Some(b) = self.conflict.take()
                {
                    self.disk = Some(hash(&b));
                }
                if small_button(ui, "Compare").clicked()
                    && let (Some(b), Some(ed)) = (&self.conflict, &self.ed)
                {
                    let theirs = String::from_utf8_lossy(b).into_owned();
                    let mine = ed.buf.text();
                    let path = self.path.clone();
                    let title = format!("{} on disk and here", self.path.rsplit('/').next().unwrap_or(&self.path));
                    cx.open(Tab::Text(Box::new(crate::views::textview::TextView::new(&title, Box::new(move || Ok(similar::TextDiff::from_lines(&theirs, &mine).unified_diff().header(&format!("{path} on disk"), &format!("{path} here")).to_string()))))));
                }
            });
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, cx: &mut Cx, now_ms: u64) {
        let Some(ed) = &mut self.ed else { return };
        let p = cx.pal;
        ui.horizontal(|ui| {
            let r = ui.add(input(&mut ed.find.query.text).hint_text("Find").desired_width(220.0).id(egui::Id::new(("editor-find", &self.path))));
            if std::mem::take(&mut ed.find.focus_query) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                ed.find_step(ui.input(|i| i.modifiers.shift));
                r.request_focus();
            }
            toggle(ui, &mut ed.find.query.case_sensitive, "Aa", "Match case");
            toggle(ui, &mut ed.find.query.whole_word, "W", "Whole words");
            toggle(ui, &mut ed.find.query.regex, ".*", "Regular expression");
            ed.refresh_matches();
            let n = ed.find.matches.len();
            let at = ed.find.matches.iter().position(|m| m.0 == ed.sels.primary().start() && m.1 == ed.sels.primary().end());
            let count = match (n, at) {
                (0, _) if !ed.find.query.text.is_empty() => "No matches".to_string(),
                (0, _) => String::new(),
                (n, Some(i)) => format!("{} of {n}", i + 1),
                (n, None) => format!("{n} found"),
            };
            if let Some(e) = &ed.find.error {
                ui.colored_label(p.danger, e.lines().last().unwrap_or(e));
            } else {
                ui.label(RichText::new(count).small().color(p.dim));
            }
            if small_button(ui, "↑").on_hover_text("Previous (Shift+F3)").clicked() {
                ed.find_step(true);
            }
            if small_button(ui, "↓").on_hover_text("Next (F3)").clicked() {
                ed.find_step(false);
            }
            if small_button(ui, "Replace…").clicked() {
                ed.find.replacing = !ed.find.replacing;
            }
            if small_button(ui, "✕").on_hover_text("Close (Esc)").clicked() || (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape))) {
                ed.find.open = false;
                ed.focus = true;
            }
        });
        if ed.find.replacing {
            ui.horizontal(|ui| {
                ui.add(input(&mut ed.find.with).hint_text("Replace with").desired_width(220.0));
                let can = !ed.read_only && !ed.find.matches.is_empty();
                if ui.add_enabled(can, egui::Button::new("Replace")).on_disabled_hover_text("Nothing matches, or the file is read-only.").clicked() {
                    ed.replace_one(now_ms);
                }
                if ui.add_enabled(can, egui::Button::new("Replace all")).on_disabled_hover_text("Nothing matches, or the file is read-only.").clicked() {
                    ed.replace_all(now_ms);
                }
            });
        }
    }

    fn goto_bar(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let Some(ed) = &mut self.ed else { return };
        let mut close = false;
        ui.horizontal(|ui| {
            let Some(text) = &mut ed.goto else { return };
            ui.label("Go to line");
            let r = ui.add(input(text).hint_text("Line[:column]").desired_width(120.0).id(egui::Id::new(("editor-goto", &self.path))));
            if !std::mem::replace(&mut self.goto_shown, true) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let mut parts = text.split(':').map(|s| s.trim().parse::<usize>().ok());
                if let Some(Some(line)) = parts.next() {
                    let col = parts.next().flatten().unwrap_or(1);
                    let pos = ed.buf.pos(line.saturating_sub(1), col.saturating_sub(1));
                    ed.sels = gitem_text::SelectionSet::single(gitem_text::Selection::caret(pos));
                    ed.reveal = true;
                }
                close = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) || small_button(ui, "✕").clicked() {
                close = true;
            }
            ui.label(RichText::new(format!("of {}", ed.buf.line_count())).small().color(cx.pal.dim));
        });
        if close {
            ed.goto = None;
            ed.focus = true;
            self.goto_shown = false;
        }
    }

    fn open_outline(&mut self) {
        let Some(ed) = &self.ed else { return };
        let symbols = crate::editor::outline::symbols(&self.path, &ed.buf);
        // Starts at the definition the caret is in.
        let line = ed.buf.line_of(ed.sels.primary().head);
        let picked = symbols.iter().rposition(|s| s.line <= line).unwrap_or(0);
        self.outline = Some(Outline { query: String::new(), symbols, picked, focus: true });
    }

    /// A box listing the definitions, narrowed by typing. Enter or a click goes to one.
    fn outline_box(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal;
        let Some(o) = &mut self.outline else { return };
        let mut go = None;
        let mut close = false;
        egui::Frame::new().fill(p.panel2).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
            let r = ui.add(input(&mut o.query).hint_text("Go to a definition").desired_width(f32::INFINITY).id(egui::Id::new(("editor-outline", &self.path))));
            if std::mem::take(&mut o.focus) {
                r.request_focus();
            }
            if r.changed() {
                o.picked = 0;
            }
            let shown = crate::editor::outline::filter(&o.symbols, &o.query);
            let (up, down, enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if r.has_focus() || r.lost_focus() {
                if down {
                    o.picked = (o.picked + 1).min(shown.len().saturating_sub(1));
                }
                if up {
                    o.picked = o.picked.saturating_sub(1);
                }
                if enter && let Some(s) = shown.get(o.picked) {
                    go = Some(s.line);
                }
                close |= esc;
            }
            if shown.is_empty() {
                ui.label(RichText::new(if o.symbols.is_empty() { "No definitions found in this file." } else { "Nothing matches." }).color(p.dim));
            }
            egui::ScrollArea::vertical().max_height(260.0).id_salt("outline-list").show(ui, |ui| {
                for (i, s) in shown.iter().enumerate() {
                    let text = format!("{}{}  {}", "    ".repeat(s.depth.min(6)), s.name, s.kind);
                    let resp = ui.add(egui::Button::selectable(i == o.picked, RichText::new(text).monospace()).min_size(egui::vec2(ui.available_width(), 0.0)));
                    let resp = resp.on_hover_text(format!("Line {}", s.line + 1));
                    if i == o.picked {
                        resp.scroll_to_me(None);
                    }
                    if resp.clicked() {
                        go = Some(s.line);
                    }
                }
            });
        });
        if let Some(line) = go {
            if let Some(ed) = &mut self.ed {
                ed.go_to_line(line);
                ed.focus = true;
            }
            close = true;
        }
        if close {
            self.outline = None;
            if let Some(ed) = &mut self.ed {
                ed.focus = true;
            }
        }
    }

    fn change_bar(&mut self, ui: &mut Ui, cx: &mut Cx, c: LineChange, now_ms: u64) {
        let p = cx.pal;
        let what = match (c.old_len, c.new_len) {
            (0, n) => format!("{n} line{} added", if n == 1 { "" } else { "s" }),
            (n, 0) => format!("{n} line{} removed", if n == 1 { "" } else { "s" }),
            (_, n) => format!("{n} line{} changed", if n == 1 { "" } else { "s" }),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Line {}: {what}", c.new_start + 1)).color(p.dim));
            let label = if self.is_dirty() { "Save and stage" } else { "Stage" };
            if small_button(ui, label).on_hover_text("Stage this change alone").clicked() {
                self.stage_change(cx, c);
                self.picked = None;
            }
            if small_button(ui, "Revert").on_hover_text("Put these lines back as staged. Undo brings them back").clicked() {
                self.revert_change(c, now_ms);
                self.picked = None;
            }
            if small_button(ui, "Diff").clicked() {
                cx.diff(DiffSource::WorkingVsHead, Some(&self.path));
            }
            if small_button(ui, "✕").clicked() {
                self.picked = None;
            }
        });
    }

    fn text_menu(&mut self, ui: &mut Ui, cx: &mut Cx, now_ms: u64) {
        let Some(ed) = &mut self.ed else { return };
        let ro = ed.read_only;
        let has_sel = ed.sels.iter().any(|s| !s.is_empty());
        if ui.add_enabled(!ro && has_sel, egui::Button::new("Cut")).on_disabled_hover_text("Nothing is selected, or the file is read-only.").clicked() {
            ui.ctx().copy_text(ed.copied());
            ed.run(Cmd::Backspace { word: false }, now_ms);
            ui.close();
        }
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(ed.copied());
            ui.close();
        }
        if ui.button("Select all").clicked() {
            ed.run(Cmd::SelectAll, now_ms);
            ui.close();
        }
        ui.separator();
        for (label, cmd) in [("Toggle comment", Cmd::ToggleComment), ("Duplicate lines", Cmd::DuplicateLines), ("Delete lines", Cmd::DeleteLines), ("Select all matches", Cmd::SelectAllMatches)] {
            let edits = !matches!(cmd, Cmd::SelectAllMatches);
            if ui.add_enabled(!(ro && edits), egui::Button::new(label)).on_disabled_hover_text("This file is read-only.").clicked() {
                ed.run(cmd, now_ms);
                ui.close();
            }
        }
        if ui.button("Find in files").clicked() {
            cx.open(Tab::Grep(Box::new(crate::views::grep::GrepView::looking_for(&ed.search_seed(), ""))));
            ui.close();
        }
        ui.separator();
        let line = ed.buf.line_of(ed.sels.primary().head);
        if let Some(c) = self.change_at(line)
            && ui.button("This change…").clicked()
        {
            self.picked = Some(c);
            ui.close();
        }
        if ui.button("Line history").clicked() {
            cx.open(Tab::line_history(&self.path, line as u32 + 1, line as u32 + 1));
            ui.close();
        }
    }

    fn welcome(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal;
        ui.heading("Editor");
        ui.label(RichText::new("Open a file from the Files sidebar or the command palette. With IDE mode on in Settings, a click in the Files tree opens it here.").color(p.dim));
        ui.add_space(8.0);
        let changed: Vec<String> = cx.status().map(|s| s.entries.iter().filter(|e| e.conflict.is_none() && !matches!(e.worktree, gitem_core::model::FileStatus::Deleted)).map(|e| e.path.clone()).collect()).unwrap_or_default();
        let key = cx.repo.root.to_string_lossy().replace('\\', "/");
        let recent: Vec<String> = cx.settings.editor_recent.get(&key).cloned().unwrap_or_default();
        for (title, files) in [("Changed", changed), ("Recent", recent)] {
            if files.is_empty() {
                continue;
            }
            ui.label(RichText::new(title).small().color(p.dim));
            for f in files.iter().take(20) {
                if ui.link(f).clicked() {
                    cx.open(Tab::editor(f, None, true));
                }
            }
            ui.add_space(6.0);
        }
    }
}

/// The buffer's text as git would store it, when the stored copy has plain line breaks and
/// this one CRLF: what `core.autocrlf` does. Without it every line would show as changed.
fn comparable(text: &str, base: &str) -> String {
    if !base.contains('\r') && text.contains("\r\n") { text.replace("\r\n", "\n") } else { text.to_string() }
}

fn put_marks(changes: &[LineChange], marks: &mut HashMap<usize, Mark>) {
    for c in changes {
        if c.new_len == 0 {
            marks.insert(c.new_start, Mark::Deleted);
            continue;
        }
        let kind = if c.old_len == 0 { Mark::Added } else { Mark::Modified };
        for l in c.new_start..c.new_start + c.new_len {
            marks.insert(l, kind);
        }
    }
}

/// Which line of the old text a line of the new one was, through `changes`. `None` for a line
/// the changes put in.
pub fn map_back(changes: &[LineChange], line: usize) -> Option<usize> {
    let mut shift: isize = 0;
    for c in changes {
        if line < c.new_start {
            break;
        }
        if line < c.new_start + c.new_len {
            return None;
        }
        shift = (c.old_start + c.old_len) as isize - (c.new_start + c.new_len) as isize;
    }
    Some((line as isize + shift) as usize)
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_map_back_through_edits() {
        let ch = line_changes("a\nb\nc\nd\n", "a\nX\nY\nc\nd\n");
        assert_eq!(map_back(&ch, 0), Some(0));
        assert_eq!(map_back(&ch, 1), None);
        assert_eq!(map_back(&ch, 2), None);
        assert_eq!(map_back(&ch, 3), Some(2));
        assert_eq!(map_back(&ch, 4), Some(3));
        let gone = line_changes("a\nb\nc\n", "a\nc\n");
        assert_eq!(map_back(&gone, 1), Some(2));
    }

    #[test]
    fn crlf_text_compares_with_what_git_stored() {
        assert_eq!(comparable("a\r\nb\r\n", "a\nb\n"), "a\nb\n");
        assert_eq!(comparable("a\r\nb\r\n", "a\r\nb\r\n"), "a\r\nb\r\n");
        let mut marks = HashMap::new();
        put_marks(&line_changes("a\nb\n", &comparable("a\r\nB\r\nc\r\n", "a\nb\n")), &mut marks);
        assert_eq!(marks.get(&0), None);
        assert_eq!(marks.get(&1), Some(&Mark::Modified));
        assert_eq!(marks.get(&2), Some(&Mark::Modified));
    }
}
