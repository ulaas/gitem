//! Working-tree changes: stage and unstage, commit box with AI message, conflicts,
//! and controls for operations in progress.
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{RichText, Ui, vec2};
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::DiffSource;
use gitem_core::{ai, cred};

use crate::action::{Action, Cx};
use crate::chrome::{self, icon};
use crate::forms;
use crate::menus;
use crate::task::Task;
use crate::views::Tab;
use crate::views::changes_tree::{self, Node, Row, RowKind};
use crate::views::diffview::DiffView;
use crate::widgets::*;

/// Below this a list and a diff side by side would both be too narrow to read, so a click
/// on a file opens its diff in a tab of its own, as it does in a narrow pane of a split.
const PREVIEW_MIN_WIDTH: f32 = 700.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Conflicts,
    Staged,
    Unstaged,
}

impl Section {
    const ALL: [Section; 3] = [Section::Conflicts, Section::Staged, Section::Unstaged];

    fn holds(self, e: &StatusEntry) -> bool {
        match self {
            Section::Conflicts => e.conflict.is_some(),
            Section::Staged => e.is_staged(),
            Section::Unstaged => e.is_unstaged(),
        }
    }
}

/// How a file row shows in the tree: its depth and bare name.
type Leaf<'a> = Option<(usize, &'a str)>;

/// The file being looked at. A file can be in two lists, staged and unstaged at once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Current {
    section: Section,
    path: String,
}

/// What a file row needs besides the file.
struct RowState<'a> {
    selected: &'a mut HashSet<String>,
    current: Option<&'a Current>,
    /// Set when the row was clicked.
    picked: &'a mut Option<Current>,
    /// Shift+F10 was pressed, which opens the current row's menu.
    menu_key: bool,
}

impl RowState<'_> {
    fn is_current(&self, section: Section, path: &str) -> bool {
        self.current.is_some_and(|c| c.section == section && c.path == path)
    }
}

/// The trees for one status and filter.
struct TreeCache {
    status_gen: u64,
    filter: String,
    nodes: [Node; 3],
}

/// Every row is this tall, so a list of any length only costs what's on screen.
const ROW_H: f32 = 22.0;

/// A line of the list, in the order drawn.
#[derive(Clone, Copy)]
enum Line {
    Heading(Section),
    /// A position in the status list.
    File(Section, usize),
    /// A position in the section's tree rows.
    Tree(Section, usize),
}

/// The list as worked out from the status, kept until something it depends on changes.
struct ListCache {
    key: (u64, String, bool, u64),
    lines: Vec<Line>,
    trees: [Vec<Row>; 3],
    shown: [usize; 3],
}

/// How long typing can pause before the draft goes to disk.
const DRAFT_AFTER: Duration = Duration::from_secs(2);

/// Where the commit message being written for this repository is kept, so a crash or a power
/// cut doesn't cost it.
fn draft_path(root: &std::path::Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().hash(&mut h);
    gitem_core::settings::config_dir().join("drafts").join(format!("{:016x}.txt", h.finish()))
}

/// The draft kept for this repository is no longer wanted.
pub fn forget_draft(root: &std::path::Path) {
    let _ = std::fs::remove_file(draft_path(root));
}

#[derive(Default)]
pub struct ChangesView {
    pub message: String,
    amend: bool,
    reset_author: bool,
    reset_date: bool,
    /// The commit template and your recent messages, read when the menu's opened.
    earlier: Task<(Option<String>, Vec<String>)>,
    earlier_read: bool,
    signoff: bool,
    /// The two settings as last seen, so changing one reaches a Changes tab that's open.
    sign_settings: (bool, bool),
    sign: bool,
    no_verify: bool,
    author: String,
    filter: String,
    ai: Task<String>,
    ai_stash: Task<String>,
    applied_prepared_for: Option<RepoOp>,
    /// Everyone who has committed, for the Co-authors menu, and the refs it was read at.
    people: Task<Vec<Contributor>>,
    people_gen: Option<u64>,
    /// Message and HEAD at submission, kept until we know the commit landed.
    submitted: Option<(String, String)>,
    /// A message put in the box for the user, which is no loss if the box closes.
    prefilled: String,
    /// Ctrl-clicked paths, for batch actions.
    selected: HashSet<String>,
    init: bool,
    tree: Option<TreeCache>,
    list: Option<ListCache>,
    /// Bumped when a folder opens or closes.
    fold_gen: u64,
    /// Closed folders in the tree, per section: everything starts open, so a change
    /// never hides inside a folder nobody chose to close.
    collapsed: [HashSet<String>; 3],
    /// What the box held when the AI was asked. Anything else there since is the user's.
    ai_asked_with: String,
    /// The AI's message, when the user has typed in the box since asking for it.
    ai_offer: Option<String>,
    /// The status when "Stash with AI message" was pressed.
    ai_stash_gen: u64,
    /// HEAD after a commit finished, to compare with `submitted`.
    head_now: Task<String>,
    amend_message: Task<String>,
    /// The message as last written to disk, and when the box last differed from it.
    draft_saved: String,
    draft_changed: Option<Instant>,
    /// The file picked in the list, shown beside it when there's room.
    current: Option<Current>,
    preview: Option<Box<DiffView>>,
    /// The list's scroll position and height, for keeping the current file on screen.
    list_scroll: (f32, f32),
    /// Move the list to show the current file, on the next frame.
    reveal: bool,
    /// A file to show as soon as the status is known, for screenshots.
    show_first: Option<String>,
    /// How tall the commit box's controls under the message are.
    commit_tail: f32,
}

fn change_of(e: &StatusEntry, staged: bool) -> FileChange {
    FileChange { path: e.path.clone(), orig_path: e.orig_path.clone(), status: if staged { e.index } else { e.worktree }, additions: None, deletions: None }
}

impl ChangesView {
    /// With this file's unstaged changes showing, for screenshots.
    pub fn showing(path: &str) -> Self {
        Self { show_first: Some(path.to_string()), ..Default::default() }
    }

    /// Asked for again, for another file.
    pub fn retarget(&mut self, new: ChangesView) {
        if new.show_first.is_some() {
            self.show_first = new.show_first;
        }
    }

    pub fn key(&self) -> String {
        "changes".into()
    }
    pub fn title(&self) -> String {
        "Changes".into()
    }

    /// What closing this tab would lose: only what was written here.
    pub fn unsaved(&self) -> Option<String> {
        let message = self.message.trim();
        (!message.is_empty() && message != self.prefilled.trim()).then(|| "A commit message you've written.".to_string())
    }

    /// What goes on whichever place this is showing in, up to the status to show.
    fn prepare(&mut self, ui: &mut Ui, cx: &mut Cx) -> Option<std::sync::Arc<RepoStatus>> {
        let p = cx.pal.clone();
        let sign_settings = (cx.settings.commit_sign, cx.settings.commit_signoff);
        if !self.init || self.sign_settings != sign_settings {
            self.sign_settings = sign_settings;
            (self.sign, self.signoff) = sign_settings;
        }
        if !self.init {
            self.init = true;
            if cx.data.persist && self.message.is_empty()
                && let Ok(draft) = std::fs::read_to_string(draft_path(&cx.repo.root))
            {
                self.message = draft.clone();
                self.draft_saved = draft;
                cx.toast("Restored the commit message you were writing.");
            }
        }
        self.save_draft(ui.ctx(), cx);
        if self.ai.poll() {
            if let Some(m) = self.ai.value.take() {
                let m = ai::clean_message(&m);
                // A reply that took a minute mustn't replace what's been typed meanwhile.
                if self.message == self.ai_asked_with {
                    self.message = m;
                } else {
                    self.ai_offer = Some(m);
                }
            }
            if let Some(e) = self.ai.error.take() {
                cx.error(e);
            }
        }
        if self.ai_stash.poll() {
            if let Some(m) = self.ai_stash.value.take() {
                let message = ai::clean_message(&m).lines().next().unwrap_or("").to_string();
                let op = ops::stash_push(&ops::StashOpts { message: message.clone(), include_untracked: true, ..Default::default() });
                // This stashes everything in the tree, which may not be what was there when
                // the button was pressed: this only runs when the tab is next drawn.
                if self.ai_stash_gen == cx.data.status_gen {
                    cx.run(op);
                } else {
                    cx.out.push(Action::Confirm { title: "Files changed since you asked".into(), body: format!("Stash everything in the working tree now, as \"{message}\"? That includes changes made after you pressed the button."), label: "Stash".into(), op: op.lossless() });
                }
            }
            if let Some(e) = self.ai_stash.error.take() {
                cx.error(e);
            }
        }
        // A rejected commit (say, by a hook) must leave the message in the box.
        if self.head_now.poll() {
            let now = self.head_now.value.take().unwrap_or_default();
            if let Some((msg, head)) = self.submitted.take()
                && now != head
            {
                if self.message == msg {
                    self.message.clear();
                }
                self.amend = false;
            }
        }
        if self.submitted.is_some() && !cx.data.ops_busy && !self.head_now.busy() {
            self.head_now.spawn_git(ui.ctx(), cx.repo, |r| r.resolve("HEAD"));
        }
        if self.amend_message.poll()
            && let Some(m) = self.amend_message.value.take()
            && self.amend
            && self.message.trim().is_empty()
        {
            self.message = m;
            self.prefilled = self.message.clone();
        }
        let Some(st) = cx.data.status.value.clone() else {
            // Git may have failed rather than be slow, and then this isn't coming.
            match &cx.data.status.error {
                Some(e) => {
                    error_box(ui, &format!("Git couldn't read the status of this repository:\n{e}"), &p);
                    if ui.button("Try again").clicked() {
                        cx.out.push(Action::Retry);
                    }
                }
                None => spinner_row(ui, "Reading status…", &p),
            }
            return None;
        };
        let held = st.clone();
        let st: &RepoStatus = &st;
        // During a merge or similar, git prepares the commit message. A squash merge does too,
        // though its only sign is a file. That file can outlive what was staged, so it counts
        // only while there's something to commit.
        let squash = squash_waiting(st);
        if (st.op != RepoOp::None || squash) && self.applied_prepared_for != Some(st.op) && self.message.is_empty() {
            if let Some(m) = cx.repo.prepared_message() {
                self.prefilled = m.clone();
                self.message = m;
            }
            self.applied_prepared_for = Some(st.op);
        }
        if st.op == RepoOp::None && !squash {
            // Aborted: that message was for an operation that isn't happening any more.
            if self.applied_prepared_for.take().is_some() && self.message == self.prefilled {
                self.message.clear();
            }
        }
        Some(held)
    }

    /// The list and the commit box in Details, for the working-changes row in the graph. This is
    /// the Changes tab itself drawn elsewhere, so it's one draft and one way of committing, not a
    /// second copy of them. A file's diff opens in a tab, as there's no room for it here.
    pub fn details_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let Some(st) = self.prepare(ui, cx) else { return };
        self.follow_current(&st);
        self.files_column(ui, cx, &st, false);
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let Some(st) = self.prepare(ui, cx) else { return };
        let st: &RepoStatus = &st;
        let room = ui.available_width();
        let beside = room >= PREVIEW_MIN_WIDTH;
        if let Some(path) = self.show_first.take() {
            let section = if st.entries.iter().any(|e| e.path == path && e.is_unstaged()) { Section::Unstaged } else { Section::Staged };
            self.pick(Current { section, path }, cx, st, beside);
        }
        self.follow_current(st);
        if !beside {
            // Nowhere to show it. Its diff comes back when the pane is wide again.
            self.files_column(ui, cx, st, false);
            return;
        }
        let widest = (room - 320.0).max(240.0);
        egui::Panel::left("changes-files").resizable(true).default_size((room * 0.36).clamp(280.0, 460.0)).size_range(240.0..=widest).show(ui, |ui| self.files_column(ui, cx, st, true));
        self.preview_pane(ui, cx);
    }

    /// A file staged while it's on show moves to the other list, and its diff goes with it.
    fn follow_current(&mut self, st: &RepoStatus) {
        let Some(cur) = self.current.clone() else { return };
        let Some(e) = st.entries.iter().find(|e| e.path == cur.path) else {
            self.current = None;
            self.preview = None;
            return;
        };
        if cur.section.holds(e) {
            return;
        }
        let other = Section::ALL.into_iter().find(|s| s.holds(e));
        self.current = other.map(|section| Current { section, path: cur.path });
        self.reveal = self.current.is_some();
        let want = self.current.clone();
        self.show_preview(want.as_ref(), Some(e));
    }

    /// Which diff a file in a list means.
    fn source_of(section: Section, e: &StatusEntry) -> DiffSource {
        match section {
            Section::Staged => DiffSource::Staged,
            _ if e.worktree == FileStatus::Untracked => DiffSource::Untracked,
            _ => DiffSource::Unstaged,
        }
    }

    /// Keeps how the diff was being looked at, as one file after another goes by.
    fn show_preview(&mut self, cur: Option<&Current>, e: Option<&StatusEntry>) {
        let (Some(cur), Some(e)) = (cur, e) else {
            self.preview = None;
            return;
        };
        if cur.section == Section::Conflicts {
            self.preview = None;
            return;
        }
        let mut view = DiffView::new(Self::source_of(cur.section, e), Some(cur.path.clone())).embedded();
        if let Some(old) = &self.preview {
            view.look_like(old);
        }
        self.preview = Some(Box::new(view));
    }

    fn preview_pane(&mut self, ui: &mut Ui, cx: &mut Cx) {
        match &mut self.preview {
            Some(view) => {
                ui.push_id("changes-preview", |ui| view.ui(ui, cx));
            }
            None => empty_state(ui, "Select a file to see its changes", "Click a file, or use the Up and Down keys. Space stages it.", cx.pal),
        }
    }

    /// The row after or before the current file, among files: not headings or folders.
    fn next_file(&self, st: &RepoStatus, down: bool) -> Option<Current> {
        let list = self.list.as_ref()?;
        let current_at = self.current.as_ref().and_then(|c| list.lines.iter().position(|l| line_file(list, st, *l).is_some_and(|(s, e)| s == c.section && e.path == c.path)));
        let order: Box<dyn Iterator<Item = usize>> = match (current_at, down) {
            (Some(at), true) => Box::new(at + 1..list.lines.len()),
            (Some(at), false) => Box::new((0..at).rev()),
            (None, true) => Box::new(0..list.lines.len()),
            (None, false) => Box::new((0..list.lines.len()).rev()),
        };
        order.into_iter().find_map(|i| line_file(list, st, list.lines[i]).map(|(section, e)| Current { section, path: e.path.clone() }))
    }

    /// Up and Down walk the files, Space stages or unstages the current one, Enter opens
    /// its diff in a tab, and Shift+F10 is its menu. Not while typing, behind a dialog, or
    /// when another pane has the user's attention. Says whether the menu was asked for.
    fn keyboard(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, beside: bool) -> bool {
        if cx.dialog_open || !cx.in_front || ui.ctx().memory(|m| m.focused().is_some()) {
            return false;
        }
        use egui::{Key, Modifiers};
        let (down, up, space, enter, menu) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), crate::keys::context_take(i, &cx.settings.key_bindings, "changes_stage"), i.consume_key(Modifiers::NONE, Key::Enter), crate::keys::context_take(i, &cx.settings.key_bindings, "row_menu")));
        if (down || up)
            && let Some(next) = self.next_file(st, down)
        {
            self.reveal = true;
            self.pick(next, cx, st, beside);
        }
        let on_current = self.current.clone().and_then(|c| st.entries.iter().find(|e| e.path == c.path).map(|e| (c.section, e)));
        if let (true, Some((section, e))) = (space, on_current) {
            match section {
                Section::Unstaged => cx.run(ops::stage_paths(std::slice::from_ref(&e.path))),
                Section::Staged => cx.run(ops::unstage_paths(&paths_to_unstage(e), !st.head_sha.is_empty())),
                Section::Conflicts => cx.open(Tab::merge(&e.path)),
            }
        }
        if let (true, Some((section, e))) = (enter, on_current) {
            if section == Section::Conflicts {
                cx.open(Tab::merge(&e.path));
            } else {
                cx.diff(Self::source_of(section, e), Some(&e.path));
            }
        }
        menu
    }

    /// A click or a key landed on this file.
    fn pick(&mut self, picked: Current, cx: &mut Cx, st: &RepoStatus, beside: bool) {
        let Some(e) = st.entries.iter().find(|e| e.path == picked.path) else { return };
        if picked.section == Section::Conflicts {
            cx.open(Tab::merge(&picked.path));
        } else if !beside {
            cx.diff(Self::source_of(picked.section, e), Some(&picked.path));
        } else if self.current.as_ref() != Some(&picked) || self.preview.is_none() {
            self.show_preview(Some(&picked), Some(e));
        }
        self.current = Some(picked);
    }

    fn files_column(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, beside: bool) {
        let p = cx.pal.clone();
        egui::Panel::bottom("commit-box").resizable(true).default_size(190.0).show(ui, |ui| self.commit_box(ui, cx, st));

        let as_tree = cx.settings.changes_tree;
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if as_tree {
                    self.fold_button(ui, &p);
                }
                if chrome::icon_button(ui, icon::TREE_VIEW, "Show as tree", "Group changed files by folder", as_tree, &p).clicked() {
                    cx.out.push(Action::Settings(Box::new(move |s| s.changes_tree = !as_tree)));
                }
                ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter files").desired_width(f32::INFINITY));
            });
        });
        self.update_list(cx.data.status_gen, st, as_tree);
        let menu_key = self.keyboard(ui, cx, st, beside);
        self.batch_bar(ui, cx, st);
        ui.add_space(2.0);

        let Some(list) = self.list.take() else { return };
        let mut picked = None;
        if st.is_clean() {
            egui::ScrollArea::vertical().id_salt("changes-list").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                self.heading(ui, cx, st, Section::Staged, 0);
                self.heading(ui, cx, st, Section::Unstaged, 0);
                ui.spacing_mut().item_spacing.y = 4.0;
                empty_state(ui, "Working tree clean", "Changes appear here as you edit files.", &p);
            });
        } else {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut area = egui::ScrollArea::vertical().id_salt("changes-list").auto_shrink([false, false]);
            if std::mem::take(&mut self.reveal)
                && let Some(y) = self.current.as_ref().and_then(|c| list.lines.iter().position(|l| line_file(&list, st, *l).is_some_and(|(s, e)| s == c.section && e.path == c.path))).map(|i| i as f32 * ROW_H)
            {
                let (top, height) = self.list_scroll;
                if y < top {
                    area = area.vertical_scroll_offset(y);
                } else if y + ROW_H > top + height {
                    area = area.vertical_scroll_offset(y + ROW_H - height);
                }
            }
            let shown = area.show_rows(ui, ROW_H, list.lines.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for at in range {
                    self.line(ui, cx, st, &list, at, &mut picked, menu_key);
                }
            });
            self.list_scroll = (shown.state.offset.y, shown.inner_rect.height());
        }
        self.list = Some(list);
        if let Some(picked) = picked {
            self.pick(picked, cx, st, beside);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn line(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, list: &ListCache, at: usize, picked: &mut Option<Current>, menu_key: bool) {
        match list.lines[at] {
            Line::Heading(section) => self.heading(ui, cx, st, section, list.shown[section as usize]),
            Line::File(section, index) => {
                let Some(e) = st.entries.get(index) else {
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), egui::Sense::hover());
                    return;
                };
                let current = self.current.clone();
                let mut rows = RowState { selected: &mut self.selected, current: current.as_ref(), picked, menu_key };
                match section {
                    Section::Conflicts => conflict_row(ui, cx, e, None, &mut rows),
                    Section::Staged => staged_row(ui, cx, e, st, &mut rows, None),
                    Section::Unstaged => unstaged_row(ui, cx, e, &mut rows, None),
                }
            }
            Line::Tree(section, row) => self.tree_row(ui, cx, st, &list.trees[section as usize], section, row, picked, menu_key),
        }
    }

    /// A section's title and its actions, the same height as a file.
    fn heading(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, section: Section, shown: usize) {
        let p = cx.pal.clone();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), egui::Sense::hover());
        let mut ui = ui.new_child(egui::UiBuilder::new().id_salt(("changes-heading", section as usize)).max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut ui;
        let title = match section {
            Section::Conflicts => "Conflicts",
            Section::Staged => "Staged",
            Section::Unstaged => "Unstaged",
        };
        ui.label(RichText::new(format!("{title} ({shown})").to_uppercase()).size(11.0).color(p.dim).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match section {
            Section::Conflicts => {}
            Section::Staged => {
                if shown > 0 {
                    if crate::widgets::small_button(ui, "Unstage all").clicked() {
                        if st.op == RepoOp::None {
                            cx.run(ops::unstage_all(!st.head_sha.is_empty()));
                        } else {
                            // `reset` would cancel the operation. This also leaves conflicts as they are.
                            let paths: Vec<String> = st.entries.iter().filter(|e| e.is_staged()).flat_map(paths_to_unstage).collect();
                            cx.run(ops::unstage_during_op(&paths));
                        }
                    }
                    if crate::widgets::small_button(ui, "View all").clicked() {
                        cx.diff(DiffSource::Staged, None);
                    }
                }
            }
            Section::Unstaged => {
                if shown > 0 {
                    if crate::widgets::small_button(ui, "Stage all").clicked() {
                        cx.run(ops::stage_all());
                    }
                    if crate::widgets::small_button(ui, "View all").clicked() {
                        cx.diff(DiffSource::Unstaged, None);
                    }
                    // The heading is one row of a narrow column, and these are the rare ones.
                    ui.menu_button(icon::DOTS_THREE, |ui| {
                        if ui.button("Stash…").clicked() {
                            cx.form(forms::stash_form(Vec::new()));
                            ui.close();
                        }
                        if ui.add_enabled(!self.ai_stash.busy(), egui::Button::new("Stash with AI message")).on_disabled_hover_text("Already writing a message for this stash.").clicked() {
                            self.ai_stash_gen = cx.data.status_gen;
                            let (repo, cfg) = (cx.repo.clone(), cx.settings.ai.clone());
                            self.ai_stash.spawn(ui.ctx(), move || -> Result<String, String> {
                                let diff = repo.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?;
                                let (s, u) = ai::stash_prompt(&diff);
                                let key = cred::ai_key(cfg.provider.key_name());
                                ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
                            });
                            ui.close();
                        }
                    }).response.on_hover_text("More for these files");
                }
            }
        });
    }

    /// Worked out when the status, the filter or a folder changes, not on every frame: at
    /// 100,000 files that alone would be most of a frame.
    fn update_list(&mut self, status_gen: u64, st: &RepoStatus, as_tree: bool) {
        let f = self.filter.clone();
        let key = (status_gen, f.clone(), as_tree, self.fold_gen);
        if self.list.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        self.selected.retain(|s| st.entries.iter().any(|e| e.path == *s));
        if as_tree {
            self.update_tree(status_gen, st, &f);
        }
        let needle = f.to_lowercase();
        let matches = |e: &StatusEntry| needle.is_empty() || e.path.to_lowercase().contains(&needle);
        let (mut lines, mut trees, mut shown) = (Vec::new(), [Vec::new(), Vec::new(), Vec::new()], [0, 0, 0]);
        for section in Section::ALL {
            let i = section as usize;
            let files: Vec<usize> = st.entries.iter().enumerate().filter(|(_, e)| section.holds(e) && matches(e)).map(|(n, _)| n).collect();
            shown[i] = files.len();
            // Conflicts have a heading only when there are any: they block everything else.
            if section != Section::Conflicts || !files.is_empty() {
                lines.push(Line::Heading(section));
            }
            match self.tree.as_ref().filter(|_| as_tree) {
                Some(tree) => {
                    // A filter looks for files, so nothing it finds stays hidden in a closed folder.
                    trees[i] = tree.nodes[i].rows(if f.is_empty() { Some(&self.collapsed[i]) } else { None });
                    lines.extend((0..trees[i].len()).map(|row| Line::Tree(section, row)));
                }
                None => lines.extend(files.into_iter().map(|n| Line::File(section, n))),
            }
        }
        self.list = Some(ListCache { key, lines, trees, shown });
    }

    /// Rebuild the trees only when the status or the filter changed.
    fn update_tree(&mut self, status_gen: u64, st: &RepoStatus, filter: &str) {
        if self.tree.as_ref().is_some_and(|t| t.status_gen == status_gen && t.filter == filter) {
            return;
        }
        let needle = filter.to_lowercase();
        let nodes = Section::ALL.map(|section| Node::build(st.entries.iter().enumerate().filter(|(_, e)| section.holds(e) && (needle.is_empty() || e.path.to_lowercase().contains(&needle))).map(|(i, e)| (i, e.path.as_str()))));
        self.tree = Some(TreeCache { status_gen, filter: filter.to_string(), nodes });
    }

    /// Writes the message after a pause in typing, and removes the file when it's gone.
    fn save_draft(&mut self, ctx: &egui::Context, cx: &Cx) {
        if !cx.data.persist {
            return;
        }
        let unsaved = self.unsaved().is_some();
        let want = if unsaved { self.message.as_str() } else { "" };
        if want == self.draft_saved {
            self.draft_changed = None;
            return;
        }
        let since = *self.draft_changed.get_or_insert_with(Instant::now);
        if since.elapsed() < DRAFT_AFTER {
            ctx.request_repaint_after(DRAFT_AFTER);
            return;
        }
        let path = draft_path(&cx.repo.root);
        // A failure costs nothing but this safety net, which is no reason to interrupt anyone.
        let _ = if want.is_empty() {
            std::fs::remove_file(&path)
        } else {
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, want))
        };
        self.draft_saved = want.to_string();
        self.draft_changed = None;
    }

    /// Closes every folder, or opens them all again if any is closed.
    fn fold_button(&mut self, ui: &mut Ui, p: &crate::theme::Palette) {
        let any_closed = self.collapsed.iter().any(|c| !c.is_empty());
        let (glyph, name) = if any_closed { (icon::ARROWS_OUT_LINE_VERTICAL, "Expand folders") } else { (icon::ARROWS_IN_LINE_VERTICAL, "Collapse folders") };
        if chrome::icon_button(ui, glyph, name, "", false, p).clicked() {
            self.fold_gen += 1;
            if any_closed {
                self.collapsed.iter_mut().for_each(HashSet::clear);
            } else if let Some(tree) = &self.tree {
                for (closed, node) in self.collapsed.iter_mut().zip(&tree.nodes) {
                    closed.extend(node.folders());
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_row(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, rows: &[Row], section: Section, at: usize, picked: &mut Option<Current>, menu_key: bool) {
        let i = section as usize;
        let r = &rows[at];
        let filtering = !self.filter.is_empty();
        match r.kind {
            RowKind::File(index) => {
                let Some(e) = st.entries.get(index) else {
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), egui::Sense::hover());
                    return;
                };
                let leaf = Some((r.depth, r.name.as_str()));
                let current = self.current.clone();
                let mut state = RowState { selected: &mut self.selected, current: current.as_ref(), picked, menu_key };
                match section {
                    Section::Conflicts => conflict_row(ui, cx, e, leaf, &mut state),
                    Section::Staged => staged_row(ui, cx, e, st, &mut state, leaf),
                    Section::Unstaged => unstaged_row(ui, cx, e, &mut state, leaf),
                }
            }
            RowKind::Dir { open, count } => {
                let resp = folder_row(ui, r, open, count, cx.pal);
                folder_buttons(ui, cx, st, &resp, section, rows, at, count, filtering);
                if resp.clicked() {
                    if !self.collapsed[i].remove(&r.path) {
                        self.collapsed[i].insert(r.path.clone());
                    }
                    self.fold_gen += 1;
                }
                resp.context_menu(|ui| folder_menu(ui, cx, st, section, rows, at, count, filtering));
            }
        }
    }

    fn commit_box(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus) {
        let p = cx.pal.clone();
        ui.add_space(4.0);
        let branch = st.op_branch.as_ref().map(|b| format!("{b} (rebasing)")).or_else(|| st.branch.clone()).unwrap_or_else(|| "detached HEAD".into());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Commit to {branch}")).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.ai.busy();
                ui.menu_button(format!("AI {}", icon::CARET_DOWN), |ui| {
                    if ui.add_enabled(!busy, egui::Button::new("Generate with AI")).on_disabled_hover_text("Already writing a message.").on_hover_text("Write a message from the staged diff").clicked() {
                        self.generate(ui.ctx(), cx);
                        ui.close();
                    }
                    if ui.button("Review with AI").on_hover_text("Look for problems in these changes").clicked() {
                        let extra = cx.settings.ai.custom_instructions.clone();
                        crate::views::textview::ai_tab(cx, "AI Review", move |r| Ok(ai::review_prompt(&extra, &r.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?)));
                        ui.close();
                    }
                    if ui.button("Explain with AI").on_hover_text("Summarise the working changes with AI").clicked() {
                        crate::views::textview::ai_tab(cx, "Explain changes", move |r| Ok(ai::explain_prompt("uncommitted changes", "(not committed yet)", &r.diff_for_ai(&DiffSource::WorkingVsHead).map_err(|e| e.to_string())?)));
                        ui.close();
                    }
                }).response.on_hover_text("These send your changes to the AI provider set in Settings");
                if busy {
                    ui.add(spinner(13.0));
                }
                if crate::widgets::small_button(ui, "Compose…").on_hover_text("Split the changes into several commits").clicked() {
                    cx.open(Tab::Composer(Box::default()));
                }
            });
        });
        let mut box_focused = false;
        egui::ScrollArea::vertical().id_salt("msg").max_height((ui.available_height() - self.commit_tail).max(40.0)).show(ui, |ui| {
            box_focused = ui.add(crate::widgets::text_area(&mut self.message).hint_text("Message (Ctrl+Enter to commit)").desired_rows(3).desired_width(f32::INFINITY)).has_focus();
        });
        let tail_top = ui.cursor().top();
        let len = self.message.lines().next().unwrap_or("").chars().count();
        if squash_waiting(st) {
            ui.label(RichText::new("A squash merge is staged. Git has made it no commit yet: committing this is what records it.").small().color(p.dim));
        }
        if self.amend
            && let Some(up) = st.upstream.as_ref().filter(|_| st.ahead == 0)
        {
            ui.label(RichText::new(format!("This commit is already on {up}. Amending it means the next push has to overwrite it, and anyone who has it has to catch up.")).small().color(p.warn));
        }
        if self.message.lines().nth(1).is_some_and(|l| !l.trim().is_empty()) {
            ui.label(RichText::new("Leave the second line blank: git and most tools read the first line alone as the subject.").small().color(p.warn));
        }
        ui.horizontal_wrapped(|ui| {
            if ui.checkbox(&mut self.amend, "Amend").changed() && self.amend && self.message.trim().is_empty() {
                self.amend_message.spawn_git(ui.ctx(), cx.repo, |r| r.commit("HEAD").map(|c| c.message()));
            }
            if self.amend {
                ui.checkbox(&mut self.reset_author, "Reset author").on_hover_text("You become its author, as of now");
                if !self.reset_author {
                    ui.checkbox(&mut self.reset_date, "Reset date").on_hover_text("Its author stays, and its date becomes now");
                }
            }
            ui.menu_button("Message \u{E136}", |ui| {
                ui.set_max_width(420.0);
                self.earlier.poll();
                if !std::mem::replace(&mut self.earlier_read, true) {
                    let repo = cx.repo.clone();
                    self.earlier.spawn(ui.ctx(), move || -> Result<(Option<String>, Vec<String>), String> { Ok((repo.commit_template(), repo.recent_messages(15).map_err(|e| e.to_string())?)) });
                }
                let Some((template, recent)) = self.earlier.get().cloned() else {
                    spinner_row(ui, "Reading…", &p);
                    return;
                };
                if let Some(t) = template
                    && ui.button("Use the commit template").clicked()
                {
                    self.message = t;
                    ui.close();
                }
                if recent.is_empty() {
                    ui.label(RichText::new("You've no earlier messages here.").small().color(p.dim));
                }
                for m in recent.iter() {
                    let first = m.lines().next().unwrap_or("");
                    if ui.button(first).on_hover_text(m.as_str()).clicked() {
                        self.message = m.clone();
                        ui.close();
                    }
                }
            });
            ui.checkbox(&mut self.signoff, "Sign-off");
            ui.checkbox(&mut self.sign, "GPG/SSH sign");
            ui.checkbox(&mut self.no_verify, "Skip hooks");
            ui.menu_button("Author \u{E136}", |ui| {
                ui.label("Override author (Name <email>)");
                ui.add(crate::widgets::input(&mut self.author));
            });
            ui.menu_button("Co-authors \u{E136}", |ui| {
                ui.set_max_height(280.0);
                self.people.poll();
                // Walks the whole history, so never on a frame, and only when it can have changed.
                if self.people_gen != Some(cx.data.refs_gen) {
                    self.people_gen = Some(cx.data.refs_gen);
                    let repo = cx.repo.clone();
                    self.people.spawn(ui.ctx(), move || repo.people("HEAD"));
                }
                if self.people.busy() {
                    spinner_row(ui, "Reading history…", &p);
                }
                let people = self.people.value.as_deref().unwrap_or_default();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for c in people.iter().filter(|c| !c.is_you).take(60) {
                        if ui.button(format!("{} <{}>", c.name, c.email)).clicked() {
                            let t = format!("Co-authored-by: {} <{}>", c.name, c.email);
                            if !self.message.contains(&t) {
                                let sep = if self.message.contains("Co-authored-by:") { "\n" } else { "\n\n" };
                                self.message = format!("{}{sep}{t}", self.message.trim_end());
                            }
                            ui.close();
                        }
                    }
                });
            });
            ui.label(RichText::new(format!("{len}/72")).small().color(if len > 72 { p.warn } else { p.faint })).on_hover_text("Characters in the first line. Git tools show about 72.");
        });
        if let Some(offer) = self.ai_offer.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("The AI's message is ready, and you've typed since asking.").small().color(p.dim)).on_hover_text(offer.as_str());
                if crate::widgets::small_button(ui, "Use it").clicked() {
                    self.message = offer;
                    self.ai_offer = None;
                } else if crate::widgets::small_button(ui, "Keep mine").clicked() {
                    self.ai_offer = None;
                }
            });
        }
        let counts = cx.data.counts();
        let (staged_n, unstaged_n) = (counts.staged, counts.unstaged);
        // Mid-rebase and the like, what comes next is Continue, not a commit of your own.
        if let Some(cmd) = st.op.command().filter(|_| st.op != RepoOp::Merge && !st.op_allows_commits) {
            ui.horizontal(|ui| {
                let clear = counts.conflicts == 0 && !cx.data.ops_busy;
                let b = egui::Button::new(RichText::new(format!("Continue {cmd}")).color(if clear { p.on_accent } else { p.faint })).fill(if clear { p.accent } else { p.panel2 });
                if ui.add_enabled(clear, b).on_disabled_hover_text("Resolve the conflicts first.").clicked() {
                    cx.run(ops::op_continue(cmd));
                }
                if ui.button("Abort…").clicked() {
                    cx.out.push(Action::Confirm { title: format!("Abort {}", st.op.label().to_lowercase()), body: "Abandon this operation and go back to where you started? Conflict resolutions made so far are lost.".into(), label: "Abort".into(), op: ops::op_abort(cmd) });
                }
            });
            self.commit_tail = (ui.cursor().top() - tail_top).max(40.0) + 8.0;
            return;
        }
        let can = !cx.data.ops_busy && self.submitted.is_none() && !self.message.trim().is_empty() && counts.conflicts == 0 && (staged_n > 0 || self.amend || st.op == RepoOp::Merge || unstaged_n > 0);
        // A commit under way says so, beside a spinner, rather than only looking unavailable.
        let committing = matches!(cx.data.ops_now.state("commit"), Some(crate::data::Busy::Running(_)));
        ui.horizontal(|ui| {
            let label = if committing {
                if self.amend { "Amending…".to_string() } else { "Committing…".to_string() }
            } else if self.amend {
                "Amend commit".to_string()
            } else if staged_n > 0 {
                format!("Commit {}", plural(staged_n, "file", "files"))
            } else if st.op == RepoOp::Merge {
                // Finishing a merge stages nothing by itself, whatever is left unstaged.
                "Finish merge".to_string()
            } else {
                format!("Stage all and commit {}", plural(unstaged_n, "file", "files"))
            };
            let fill = if can { p.accent } else { p.panel2 };
            let b = egui::Button::new(RichText::new(label).color(if can { p.on_accent } else { p.faint })).fill(fill);
            // Dialogs draw after this and have the same shortcut. It belongs to them,
            // unless the commit box is where the typing is.
            let shortcut = can && (box_focused || !cx.dialog_open) && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
            let why = if counts.conflicts > 0 {
                "Resolve the conflicts first."
            } else if self.message.trim().is_empty() {
                "Write a commit message first."
            } else if cx.data.ops_busy || self.submitted.is_some() {
                "Git is still working on the last thing asked for."
            } else {
                "There's nothing to commit."
            };
            if ui.add_enabled(can, b).on_disabled_hover_text(why).clicked() || shortcut {
                self.commit(cx, staged_n == 0 && !self.amend && st.op != RepoOp::Merge);
            }
            if committing {
                ui.add(crate::widgets::spinner(14.0));
            }
        });
        // What's below the message box, for the next frame to leave room for: it wraps.
        self.commit_tail = (ui.cursor().top() - tail_top).max(40.0) + 8.0;
    }

    /// Actions for files picked with Ctrl+click.
    fn batch_bar(&mut self, ui: &mut Ui, cx: &mut Cx, st: &RepoStatus) {
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<String> = self.selected.iter().cloned().collect();
        let tracked: Vec<String> = st.entries.iter().filter(|e| self.selected.contains(&e.path) && e.worktree != FileStatus::Untracked && e.is_unstaged()).map(|e| e.path.clone()).collect();
        // Only what's staged: git refuses the whole list over one name it doesn't know.
        let staged: Vec<String> = st.entries.iter().filter(|e| self.selected.contains(&e.path) && e.is_staged()).flat_map(paths_to_unstage).collect();
        ui.horizontal(|ui| {
            ui.label(format!("{} selected", paths.len()));
            if crate::widgets::small_button(ui, "Stage").clicked() {
                cx.run(ops::stage_paths(&paths));
                self.selected.clear();
            }
            if !staged.is_empty() && crate::widgets::small_button(ui, "Unstage").clicked() {
                cx.run(ops::unstage_paths(&staged, !st.head_sha.is_empty()));
                self.selected.clear();
            }
            if crate::widgets::small_button(ui, "Stash…").clicked() {
                cx.form(forms::stash_form(paths.clone()));
                self.selected.clear();
            }
            if !tracked.is_empty() && crate::widgets::small_button(ui, "Discard…").clicked() {
                cx.confirm("Discard changes", &format!("Discard unstaged changes in {}? The toolbar's Undo can bring them back straight afterwards.", plural(tracked.len(), "file", "files")), "Discard", ops::discard_paths(&tracked));
                self.selected.clear();
            }
            if crate::widgets::small_button(ui, "Clear").clicked() {
                self.selected.clear();
            }
        });
    }

    fn commit(&mut self, cx: &mut Cx, stage_all: bool) {
        if stage_all {
            cx.run(ops::stage_all());
        }
        cx.run(ops::commit(&ops::CommitOpts {
            message: self.message.clone(),
            amend: self.amend,
            signoff: self.signoff,
            sign: self.sign,
            no_verify: self.no_verify,
            allow_empty: false,
            author: Some(self.author.trim().to_string()).filter(|s| !s.is_empty()),
            all: false,
            reset_author: self.amend && self.reset_author,
            reset_date: self.amend && self.reset_date && !self.reset_author,
        }));
        // The status is read after every write, so this is HEAD as of the last one.
        let head = cx.status().map(|s| s.head_sha.clone()).unwrap_or_default();
        self.submitted = Some((self.message.clone(), head));
    }

    fn generate(&mut self, ctx: &egui::Context, cx: &Cx) {
        let repo = cx.repo.clone();
        let cfg = cx.settings.ai.clone();
        self.ai_asked_with = self.message.clone();
        self.ai_offer = None;
        self.ai.spawn(ctx, move || -> Result<String, String> {
            let mut diff = repo.diff_for_ai(&DiffSource::Staged).map_err(|e| e.to_string())?;
            if diff.trim().is_empty() {
                diff = repo.diff_for_ai(&DiffSource::Unstaged).map_err(|e| e.to_string())?;
            }
            if diff.trim().is_empty() {
                return Err("There are no changes to describe. New files must be staged first.".to_string());
            }
            let (s, u) = ai::commit_message_prompt(&cfg.commit_style, &diff);
            let key = cred::ai_key(cfg.provider.key_name());
            ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
        });
    }
}

/// The file on a line of the list, if it's a file's line.
fn line_file<'s>(list: &ListCache, st: &'s RepoStatus, line: Line) -> Option<(Section, &'s StatusEntry)> {
    match line {
        Line::Heading(_) => None,
        Line::File(section, n) => st.entries.get(n).map(|e| (section, e)),
        Line::Tree(section, row) => match list.trees[section as usize].get(row)?.kind {
            RowKind::File(n) => st.entries.get(n).map(|e| (section, e)),
            RowKind::Dir { .. } => None,
        },
    }
}

fn folder_row(ui: &mut Ui, r: &Row, open: bool, count: usize, p: &crate::theme::Palette) -> egui::Response {
    let (rect, resp) = row(ui, ROW_H, false, p);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, r.name.as_str()));
    tree_guides(ui, rect, r.depth, p);
    let x = rect.left() + r.depth as f32 * TREE_INDENT;
    let y = rect.center().y;
    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    ui.painter().text(egui::pos2(x + 10.0, y), egui::Align2::CENTER_CENTER, caret, prop(10.5), p.faint);
    ui.painter().text(egui::pos2(x + 27.0, y), egui::Align2::CENTER_CENTER, if open { icon::FOLDER_OPEN } else { icon::FOLDER }, prop(14.0), p.series[3]);
    let n = ui.painter().text(egui::pos2(rect.right() - 6.0, y), egui::Align2::RIGHT_CENTER, count.to_string(), mono(11.0), p.faint);
    let text = egui::Rect::from_min_max(egui::pos2(x + 37.0, rect.top()), egui::pos2(n.left() - 6.0, rect.bottom()));
    ui.painter().with_clip_rect(text.intersect(ui.clip_rect())).text(egui::pos2(text.left(), y), egui::Align2::LEFT_CENTER, &r.name, prop(13.0), p.text);
    resp
}

/// The folder's own hover buttons: everything in it staged or unstaged, and the changes to its
/// tracked files discarded, after asking. New files in it are deleted one by one, from their rows.
#[allow(clippy::too_many_arguments)]
fn folder_buttons(ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, resp: &egui::Response, section: Section, rows: &[Row], at: usize, count: usize, filtering: bool) {
    if section == Section::Conflicts || !ui.rect_contains_pointer(resp.rect) {
        return;
    }
    let p = cx.pal.clone();
    let folder = rows[at].path.clone();
    let files: Vec<&StatusEntry> = changes_tree::files_in(rows, at).into_iter().filter_map(|i| st.entries.get(i)).collect();
    let n = plural(count, "file", "files");
    if section == Section::Staged {
        let name = format!("Unstage {n} in {folder}");
        if crate::widgets::row_buttons(ui, resp, &[(icon::MINUS, name.as_str())], false, &p) == Some(0) {
            let mut names: Vec<String> = if filtering { files.iter().map(|e| e.path.clone()).collect() } else { vec![folder.clone()] };
            // A rename into this folder from elsewhere goes back whole.
            names.extend(files.iter().filter_map(|e| e.orig_path.clone()));
            cx.run(ops::unstage_paths(&names, !st.head_sha.is_empty()));
        }
        return;
    }
    let tracked: Vec<String> = files.iter().filter(|e| e.worktree != FileStatus::Untracked).map(|e| e.path.clone()).collect();
    let stage = format!("Stage {n} in {folder}");
    let discard = format!("Discard changes to {} in {folder}…", plural(tracked.len(), "tracked file", "tracked files"));
    let mut buttons = vec![(icon::PLUS, stage.as_str())];
    if !tracked.is_empty() {
        buttons.push((icon::ARROW_COUNTER_CLOCKWISE, discard.as_str()));
    }
    match crate::widgets::row_buttons(ui, resp, &buttons, false, &p) {
        Some(0) => cx.run(ops::stage_paths(&if filtering { files.iter().map(|e| e.path.clone()).collect() } else { vec![folder] })),
        Some(1) => cx.confirm(
            "Discard changes",
            &format!("Discard unstaged changes in {} in {folder}? New files in it stay. The toolbar's Undo can bring them back straight afterwards.", plural(tracked.len(), "file", "files")),
            "Discard",
            ops::discard_paths(&tracked),
        ),
        _ => {}
    }
}

/// `rows[at]` is the folder. Git takes the folder itself where it can, which also keeps
/// the command short for a folder of thousands of files. A filter narrows the list to
/// fewer files than the folder holds, so then the action names exactly those.
#[allow(clippy::too_many_arguments)]
fn folder_menu(ui: &mut Ui, cx: &mut Cx, st: &RepoStatus, section: Section, rows: &[Row], at: usize, count: usize, filtering: bool) {
    let folder = rows[at].path.clone();
    let targets = || -> Vec<String> {
        if filtering {
            changes_tree::files_in(rows, at).into_iter().filter_map(|i| st.entries.get(i)).map(|e| e.path.clone()).collect()
        } else {
            vec![folder.clone()]
        }
    };
    match section {
        Section::Staged => {
            if ui.button(format!("Unstage {}", plural(count, "file", "files"))).clicked() {
                let mut names = targets();
                // A rename into this folder from elsewhere goes back whole.
                let inside = format!("{folder}/");
                names.extend(st.entries.iter().filter(|e| e.is_staged() && e.path.starts_with(&inside)).filter_map(|e| e.orig_path.clone()));
                cx.run(ops::unstage_paths(&names, !st.head_sha.is_empty()));
                ui.close();
            }
            ui.separator();
        }
        Section::Unstaged => {
            if ui.button(format!("Stage {}", plural(count, "file", "files"))).clicked() {
                cx.run(ops::stage_paths(&targets()));
                ui.close();
            }
            ui.separator();
        }
        Section::Conflicts => {}
    }
    if ui.button("Copy path").clicked() {
        cx.copy(folder.clone());
        ui.close();
    }
}

fn conflict_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry, leaf: Leaf, state: &mut RowState) {
    let p = cx.pal.clone();
    let code = e.conflict.clone().unwrap_or_default();
    let is_current = state.is_current(Section::Conflicts, &e.path);
    let (rect, resp) = row(ui, ROW_H, is_current, &p);
    let (depth, shown) = leaf.unwrap_or((0, &e.path));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, e.path.as_str()));
    if leaf.is_some() {
        tree_guides(ui, rect, depth, &p);
    }
    let tr = egui::Rect::from_min_max(rect.min + egui::vec2(8.0 + depth as f32 * TREE_INDENT, 0.0), egui::pos2(rect.right() - 40.0, rect.bottom()));
    paint_path(ui, tr, shown, &p);
    paint_text(ui, egui::Rect::from_min_max(egui::pos2(rect.right() - 30.0, rect.top()), rect.max), &code, mono(12.0), p.warn);
    let resp = resp.on_hover_text(conflict_help(&code));
    // During a rebase "ours" is the upstream, and "theirs" is your own commit.
    let rebasing = cx.status().is_some_and(|s| matches!(s.op, RepoOp::Rebase | RepoOp::RebaseInteractive));
    let (ours, theirs) = if rebasing { ("upstream's version", "your commit's version") } else { ("ours", "theirs") };
    if resp.clicked() {
        *state.picked = Some(Current { section: Section::Conflicts, path: e.path.clone() });
    }
    keyed_context_menu(&resp, is_current && state.menu_key, |ui| {
        if ui.button("Resolve in merge tool").clicked() {
            cx.open(Tab::merge(&e.path));
            ui.close();
        }
        if ui.button("Open in external merge tool").on_hover_text("The one git is set up with, or the one named in Settings").clicked() {
            cx.out.push(crate::action::Action::RunDetached(ops::mergetool_file(&cx.settings.external_merge_tool, &e.path)));
            ui.close();
        }
        // A side with no version of the file has nothing to take.
        let (has_ours, has_theirs) = conflict_sides(&code);
        if has_ours && ui.button(format!("Take {ours}…")).clicked() {
            cx.confirm("Resolve conflict", &format!("Replace {} with {ours}? Changes from the other side are discarded.", e.path), "Replace", ops::checkout_conflict_side(&e.path, false));
            ui.close();
        }
        if has_theirs && ui.button(format!("Take {theirs}…")).clicked() {
            cx.confirm("Resolve conflict", &format!("Replace {} with {theirs}? Changes from the other side are discarded.", e.path), "Replace", ops::checkout_conflict_side(&e.path, true));
            ui.close();
        }
        if ui.button("Mark as resolved").clicked() {
            let text = std::fs::read(cx.repo.root.join(&e.path)).unwrap_or_default();
            if gitem_core::merge::has_conflict_markers(&String::from_utf8_lossy(&text)) {
                // Always asked, whatever the setting: this is how markers get committed.
                cx.out.push(Action::Confirm { title: format!("{} still has conflict markers", e.path), body: "Marking it resolved stages the markers too, and the next commit would include them.".into(), label: "Mark resolved anyway".into(), op: ops::mark_resolved(&e.path) });
            } else {
                cx.run(ops::mark_resolved(&e.path));
            }
            ui.close();
        }
        if ui.button("Delete file…").clicked() {
            cx.confirm("Delete file", &format!("Resolve the conflict by deleting {}?", e.path), "Delete", ops::remove_conflicted(&e.path));
            ui.close();
        }
    });
}

/// Whether (ours, theirs) have a version of a conflicted file, from its status code.
pub fn conflict_sides(code: &str) -> (bool, bool) {
    match code {
        "DD" => (false, false),
        "DU" | "UA" => (false, true),
        "UD" | "AU" => (true, false),
        _ => (true, true),
    }
}

/// A squash merge staged and not yet committed.
fn squash_waiting(st: &RepoStatus) -> bool {
    st.squash_pending && st.entries.iter().any(|e| e.is_staged())
}

/// The names that take a staged file back out of the index. Its old name too if it's a
/// rename, or that half stays staged as a deletion for the next commit to pick up.
fn paths_to_unstage(e: &StatusEntry) -> Vec<String> {
    std::iter::once(e.path.clone()).chain(e.orig_path.clone()).collect()
}

fn conflict_help(code: &str) -> &'static str {
    match code {
        "UU" => "Both sides modified this file",
        "AA" => "Both sides added this file",
        "DD" => "Both sides deleted this file",
        "AU" => "Added by us, changed by them",
        "UA" => "Added by them, changed by us",
        "DU" => "Deleted by us, modified by them",
        "UD" => "Modified by us, deleted by them",
        _ => "Unmerged",
    }
}

/// Ctrl-click toggles the batch selection. Returns true if it was consumed.
fn select_click(ui: &Ui, resp: &egui::Response, path: &str, selected: &mut HashSet<String>) -> bool {
    if resp.clicked() && ui.input(|i| i.modifiers.command) {
        if !selected.remove(path) {
            selected.insert(path.to_string());
        }
        return true;
    }
    false
}

fn changed_file_row(ui: &mut Ui, ch: &FileChange, selected: bool, leaf: Leaf, p: &crate::theme::Palette) -> egui::Response {
    let resp = match leaf {
        Some((depth, name)) => tree_file_row(ui, ch, name, depth, selected, p),
        None => file_row(ui, ch, selected, p),
    };
    // A long name doesn't fit, and in a tree the row has only the last part of it.
    resp.on_hover_text(ch.path.as_str())
}

fn staged_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry, st: &RepoStatus, state: &mut RowState, leaf: Leaf) {
    let p = cx.pal.clone();
    let ch = change_of(e, true);
    let is_current = state.is_current(Section::Staged, &e.path);
    let is_selected = state.selected.contains(&e.path) || is_current;
    let resp = changed_file_row(ui, &ch, is_selected, leaf, &p);
    if ui.rect_contains_pointer(resp.rect) {
        let (unstage, open) = (format!("Unstage {}", e.path), format!("Open {} in the editor", e.path));
        let mut buttons = vec![(icon::MINUS, unstage.as_str())];
        let can_open = cx.repo.root.join(&e.path).is_file();
        if can_open {
            buttons.push((icon::PENCIL_SIMPLE, open.as_str()));
        }
        match crate::widgets::row_buttons(ui, &resp, &buttons, is_selected, &p) {
            Some(0) => cx.run(ops::unstage_paths(&paths_to_unstage(e), !st.head_sha.is_empty())),
            Some(1) => cx.open(Tab::editor(&e.path, None, true)),
            _ => {}
        }
    }
    if select_click(ui, &resp, &e.path, state.selected) {
    } else if resp.clicked() {
        *state.picked = Some(Current { section: Section::Staged, path: e.path.clone() });
    }
    if resp.double_clicked() {
        cx.run(ops::unstage_paths(&paths_to_unstage(e), !st.head_sha.is_empty()));
    }
    keyed_context_menu(&resp, is_current && state.menu_key, |ui| {
        if ui.button("Unstage").clicked() {
            cx.run(ops::unstage_paths(&paths_to_unstage(e), !st.head_sha.is_empty()));
            ui.close();
        }
        if ui.button("Open changes").clicked() {
            cx.diff(DiffSource::Staged, Some(&e.path));
            ui.close();
        }
        ui.separator();
        menus::file_menu(ui, cx, &e.path, None);
    });
}

fn unstaged_row(ui: &mut Ui, cx: &mut Cx, e: &StatusEntry, state: &mut RowState, leaf: Leaf) {
    let p = cx.pal.clone();
    let untracked = e.worktree == FileStatus::Untracked;
    let ch = change_of(e, false);
    let is_current = state.is_current(Section::Unstaged, &e.path);
    let is_selected = state.selected.contains(&e.path) || is_current;
    let resp = changed_file_row(ui, &ch, is_selected, leaf, &p);
    if ui.rect_contains_pointer(resp.rect) {
        let stage = format!("Stage {}", e.path);
        let throw = if untracked { format!("Delete {}…", e.path) } else { format!("Discard changes in {}…", e.path) };
        let open = format!("Open {} in the editor", e.path);
        let mut buttons = vec![(icon::PLUS, stage.as_str()), (if untracked { icon::TRASH } else { icon::ARROW_COUNTER_CLOCKWISE }, throw.as_str())];
        if cx.repo.root.join(&e.path).is_file() {
            buttons.push((icon::PENCIL_SIMPLE, open.as_str()));
        }
        match crate::widgets::row_buttons(ui, &resp, &buttons, is_selected, &p) {
            Some(0) => cx.run(ops::stage_paths(std::slice::from_ref(&e.path))),
            Some(1) if untracked => delete_untracked(cx, &e.path),
            Some(1) => discard_file(cx, &e.path),
            Some(2) => cx.open(Tab::editor(&e.path, None, true)),
            _ => {}
        }
    }
    let src = if untracked { DiffSource::Untracked } else { DiffSource::Unstaged };
    if select_click(ui, &resp, &e.path, state.selected) {
    } else if resp.clicked() {
        *state.picked = Some(Current { section: Section::Unstaged, path: e.path.clone() });
    }
    if resp.double_clicked() {
        cx.run(ops::stage_paths(std::slice::from_ref(&e.path)));
    }
    keyed_context_menu(&resp, is_current && state.menu_key, |ui| {
        if ui.button("Stage").clicked() {
            cx.run(ops::stage_paths(std::slice::from_ref(&e.path)));
            ui.close();
        }
        if ui.button("Open changes").clicked() {
            cx.diff(src.clone(), Some(&e.path));
            ui.close();
        }
        if ui.button("Stash this file…").clicked() {
            cx.form(forms::stash_form(vec![e.path.clone()]));
        }
        if !e.is_staged() && ui.button("Stash some of its hunks…").clicked() {
            cx.open(Tab::StashHunks(Box::default()));
            ui.close();
        }
        if untracked {
            if ui.button("Add to .gitignore").clicked() {
                add_to_gitignore(cx, &e.path);
                ui.close();
            }
            if ui.button("Ignore…").on_hover_text("A pattern instead of this one path, or a list that isn't shared").clicked() {
                cx.form(ignore_form(cx.repo, &e.path));
                ui.close();
            }
            if ui.button("Delete file…").clicked() {
                delete_untracked(cx, &e.path);
                ui.close();
            }
        } else if ui.button("Discard changes…").clicked() {
            discard_file(cx, &e.path);
            ui.close();
        }
        ui.separator();
        menus::file_menu(ui, cx, &e.path, None);
    });
}

/// Asks first; the toolbar's Undo brings them back.
fn discard_file(cx: &mut Cx, path: &str) {
    cx.confirm("Discard changes", &format!("Discard all unstaged changes in {path}? The toolbar's Undo can bring them back straight afterwards."), "Discard", ops::discard_paths(&[path.to_string()]));
}

/// Asks first, and never for a repository of its own inside this one.
fn delete_untracked(cx: &mut Cx, path: &str) {
    if cx.repo.is_nested_repo(path) {
        // `git clean` would leave it alone and report success.
        cx.error(format!("{path} is a Git repository of its own, with its own history. gitem won't delete that: remove the folder by hand if you're sure."));
    } else {
        cx.confirm("Delete file", &format!("Delete the untracked file {path}? The toolbar's Undo can bring it back straight afterwards."), "Delete", ops::delete_untracked(&[path.to_string()]));
    }
}

/// A rule matching exactly this path. The leading slash ties it to the top of the repository,
/// and takes care of a name starting with `#` or `!`.
pub fn gitignore_entry(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 1);
    out.push('/');
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    // Trailing spaces are dropped from a rule unless marked.
    let spaces = out.len() - out.trim_end_matches(' ').len();
    if spaces > 0 {
        out.truncate(out.len() - spaces);
        out.push_str(&"\\ ".repeat(spaces));
    }
    out
}

/// Rules for this file as it could be ignored: itself, its folder, or everything of its kind.
pub fn ignore_patterns(path: &str) -> Vec<String> {
    let mut v = vec![gitignore_entry(path)];
    if let Some((folder, _)) = path.trim_end_matches('/').rsplit_once('/') {
        v.push(format!("{}/", gitignore_entry(folder)));
    }
    if let Some((_, ext)) = path.rsplit_once('/').map_or(path, |x| x.1).rsplit_once('.')
        && !ext.is_empty()
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        v.push(format!("*.{ext}"));
    }
    v
}

pub fn ignore_form(repo: &gitem_core::Repo, path: &str) -> forms::FormDialog {
    const WHERE: [&str; 3] = ["This repository's .gitignore, which is shared", "This clone only (.git/info/exclude)", "Every repository of mine (Git's global ignore file)"];
    let patterns = ignore_patterns(path);
    let refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
    let repo = repo.clone();
    forms::FormDialog::new(format!("Ignore {path}"), "Ignore").choice("pattern", "Rule", &refs, 0).choice("place", "Where", &WHERE, 0).on_submit(move |v| {
        let rule = v.text("pattern");
        let file = match v.index("place") {
            0 => repo.ignore_path(),
            1 => repo.info_exclude_path(),
            _ => repo.global_ignore_path().map_err(|e| e.to_string())?,
        };
        append_rule(&file, &rule).map_err(|e| format!("Could not update {}: {e}", file.display()))?;
        Ok(vec![crate::action::Action::Toast(format!("Added {rule} to {}", file.display())), crate::action::Action::ReloadRepository])
    })
}

fn append_rule(file: &std::path::Path, rule: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let needs_nl = std::fs::read(file).map(|b| !b.is_empty() && !b.ends_with(b"\n")).unwrap_or(false);
    std::fs::OpenOptions::new().create(true).append(true).open(file).and_then(|mut f| {
        if needs_nl {
            f.write_all(b"\n")?;
        }
        f.write_all(format!("{rule}\n").as_bytes())
    })
}

pub fn add_to_gitignore(cx: &mut Cx, path: &str) {
    let file = cx.repo.ignore_path();
    let r = append_rule(&file, &gitignore_entry(path));
    match r {
        Ok(()) => cx.toast(format!("Ignored {path}")),
        Err(e) => cx.error(format!("Could not update .gitignore: {e}")),
    }
}

#[cfg(test)]
mod ignore_tests {
    use super::*;

    #[test]
    fn a_file_can_be_ignored_by_itself_its_folder_or_its_kind() {
        assert_eq!(ignore_patterns("src/gen/a.log"), ["/src/gen/a.log", "/src/gen/", "*.log"]);
        assert_eq!(ignore_patterns("Makefile"), ["/Makefile"]);
        assert_eq!(ignore_patterns("build/"), ["/build/"]);
    }

    #[test]
    fn a_rule_starts_on_a_line_of_its_own() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("info").join("exclude");
        append_rule(&f, "*.a").unwrap();
        std::fs::write(&f, "no newline").unwrap();
        append_rule(&f, "*.b").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "no newline\n*.b\n");
    }
}
