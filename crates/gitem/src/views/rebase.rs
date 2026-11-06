//! Interactive rebase editor: reorder by dragging, pick/reword/edit/squash/fixup/
//! drop per commit. Works in-app, and as `git`'s sequence editor in standalone mode.
use std::path::PathBuf;

use egui::{RichText, Ui};
use gitem_core::ops::RebaseOpts;
use gitem_core::rebase::{self, RebaseAction, TodoItem};

use crate::action::{Action, Cx};
use crate::forms;
use crate::task::Task;
use crate::theme::Palette;
use crate::widgets::*;

pub struct RebaseView {
    /// `None` means rebase from the root.
    base: Option<String>,
    base_edit: String,
    /// When set, edit this todo file (git's sequence-editor mode).
    pub todo_file: Option<PathBuf>,
    pub items: Vec<TodoItem>,
    loaded: Task<Vec<TodoItem>>,
    started: bool,
    autostash: bool,
    update_refs: bool,
    /// Rebase from the first commit; `base` stays empty.
    root: bool,
    /// HEAD the plan was built from. A newer HEAD has commits the plan lacks,
    /// and a rebase would silently drop them.
    planned_head: Task<String>,
    stale: bool,
    pending: Option<(String, RebaseAction)>,
    drag: Option<usize>,
    selected: usize,
    pub finished: Option<bool>,
}

impl RebaseView {
    pub fn new(base: Option<String>) -> Self {
        Self {
            base_edit: base.clone().unwrap_or_default(),
            base,
            todo_file: None,
            items: Vec::new(),
            loaded: Task::default(),
            started: false,
            autostash: true,
            update_refs: false,
            root: false,
            planned_head: Task::default(),
            stale: false,
            pending: None,
            drag: None,
            selected: 0,
            finished: None,
        }
    }

    pub fn for_todo_file(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut v = Self::new(None);
        v.items = rebase::parse_todo(&text);
        v.todo_file = Some(path);
        v.started = true;
        v
    }

    /// Choose an action for one commit as soon as the plan loads.
    pub fn preset(&mut self, sha: &str, action: RebaseAction) {
        self.pending = Some((sha.to_string(), action));
    }

    pub fn from_root() -> Self {
        Self { root: true, ..Self::new(None) }
    }

    pub fn key(&self) -> String {
        format!("rebase:{:?}:{}", self.base, self.root)
    }

    pub fn title(&self) -> String {
        "Interactive Rebase".to_string()
    }

    fn load(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.started = true;
        self.stale = false;
        if self.base.is_none() && !self.root {
            self.items.clear();
            return;
        }
        let r = cx.repo.clone();
        self.planned_head.spawn(ctx, move || r.resolve("HEAD"));
        let repo = cx.repo.clone();
        let base = self.base.clone();
        self.loaded.spawn(ctx, move || -> Result<Vec<TodoItem>, String> {
            Ok(forms::commits_since(&repo, base.as_deref())?.iter().map(TodoItem::from_commit).collect())
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.loaded.poll()
            && let Some(v) = self.loaded.value.take() {
                self.items = v;
                if let Some((sha, action)) = self.pending.take()
                    && let Some(i) = self.items.iter().position(|x| x.sha == sha)
                {
                    self.items[i].action = action;
                    self.selected = i;
                }
            }
        self.planned_head.poll();
        if !self.started {
            self.load(&ui.ctx().clone(), cx);
        }
        if self.todo_file.is_none() {
            ui.horizontal(|ui| {
                ui.label("Rebase onto");
                let r = ui.add(crate::widgets::input(&mut self.base_edit).hint_text("branch, tag or SHA").desired_width(180.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.base = Some(self.base_edit.trim().to_string()).filter(|s| !s.is_empty());
                    self.root = false;
                    self.load(&ui.ctx().clone(), cx);
                }
                let refs = cx.data.ref_names();
                ui.menu_button("▾", |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for r in &refs {
                            if ui.button(r.as_str()).clicked() {
                                self.base_edit = r.clone();
                                self.base = Some(r.clone());
                                self.root = false;
                                self.load(&ui.ctx().clone(), cx);
                                ui.close();
                            }
                        }
                    });
                });
                if ui.small_button("From first commit").on_hover_text("Rewrite the whole history of this branch").clicked() {
                    self.base = None;
                    self.base_edit.clear();
                    self.root = true;
                    self.load(&ui.ctx().clone(), cx);
                }
                if self.loaded.busy() {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.autostash, "Autostash");
                ui.checkbox(&mut self.update_refs, "Update dependent branches");
                ui.label(RichText::new("Merge commits aren't kept: a rebase replays them as linear history.").small().color(p.faint));
            });
            if self.stale {
                ui.horizontal(|ui| {
                    ui.colored_label(p.warn, "HEAD moved since this plan was made, so it was not started.");
                    if ui.small_button("Rebuild plan").clicked() {
                        self.load(&ui.ctx().clone(), cx);
                    }
                });
            }
        }
        if let Some(e) = &self.loaded.error {
            error_box(ui, e, &p);
        }
        ui.horizontal(|ui| {
            if ui.small_button("Squash all into first").clicked() {
                rebase::squash_all_into_first(&mut self.items);
            }
            if ui.small_button("Arrange fixups").on_hover_text("Move fixup! and squash! commits after the commits they target").clicked() {
                rebase::arrange_autosquash(&mut self.items);
            }
            if ui.small_button("Reset to pick").clicked() {
                for i in &mut self.items {
                    if matches!(i.action, RebaseAction::Squash | RebaseAction::Fixup | RebaseAction::Drop | RebaseAction::Reword | RebaseAction::Edit) {
                        i.action = RebaseAction::Pick;
                    }
                }
            }
            if ui.small_button("+ exec").on_hover_text("Run a shell command after the selected step").clicked() {
                let at = (self.selected + 1).min(self.items.len());
                self.items.insert(at, TodoItem { action: RebaseAction::Exec, sha: String::new(), text: String::new() });
            }
            if ui.small_button("+ break").on_hover_text("Stop after the selected step").clicked() {
                let at = (self.selected + 1).min(self.items.len());
                self.items.insert(at, TodoItem { action: RebaseAction::Break, sha: String::new(), text: String::new() });
            }
            ui.label(RichText::new("Keys: p pick · r reword · e edit · s squash · f fixup · d drop · Alt+↑/↓ move").small().color(p.faint));
        });
        ui.separator();

        let errs = rebase::validate(&self.items);
        egui::Panel::bottom("rebase-actions").show(ui, |ui| {
            ui.add_space(4.0);
            for e in &errs {
                ui.colored_label(p.danger, e);
            }
            ui.horizontal(|ui| {
                let ok = errs.is_empty() && !self.items.is_empty() && !self.stale;
                let b = egui::Button::new(RichText::new(if self.todo_file.is_some() { "Start rebase".to_string() } else { format!("Rebase {} commit(s)", self.items.len()) }).color(if ok { p.on_accent } else { p.faint })).fill(if ok { p.accent } else { p.panel2 });
                if ui.add_enabled(ok, b).clicked() {
                    self.start(cx);
                }
                if let Some(path) = &self.todo_file
                    && ui.button("Abort rebase").clicked() {
                        // An empty todo tells git to abort.
                        if let Err(e) = std::fs::write(path, "") {
                            cx.error(e.to_string());
                        }
                        self.finished = Some(false);
                    }
                ui.label(RichText::new("Commits are listed oldest first and applied top to bottom.").small().color(p.faint));
            });
        });

        self.keyboard(ui);
        egui::ScrollArea::vertical().id_salt("rebase-list").auto_shrink([false, false]).show(ui, |ui| {
            if self.items.is_empty() && !self.loaded.busy() {
                empty_state(ui, "Nothing to rebase", "Pick a base with commits between it and HEAD.", &p);
            }
            self.list(ui, &p, cx);
        });
    }

    fn keyboard(&mut self, ui: &Ui) {
        if ui.ctx().memory(|m| m.focused().is_some()) || self.items.is_empty() {
            return;
        }
        let sel = self.selected.min(self.items.len() - 1);
        use egui::Key::*;
        let set = [(P, RebaseAction::Pick), (R, RebaseAction::Reword), (E, RebaseAction::Edit), (S, RebaseAction::Squash), (F, RebaseAction::Fixup), (D, RebaseAction::Drop)];
        for (k, a) in set {
            if ui.input(|i| i.key_pressed(k) && !i.modifiers.ctrl) && RebaseAction::CHOICES.contains(&self.items[sel].action) {
                self.items[sel].action = a;
            }
        }
        let alt = ui.input(|i| i.modifiers.alt);
        if ui.input(|i| i.key_pressed(ArrowUp)) {
            if alt && sel > 0 {
                self.items.swap(sel, sel - 1);
            }
            self.selected = sel.saturating_sub(1);
        }
        if ui.input(|i| i.key_pressed(ArrowDown)) {
            if alt && sel + 1 < self.items.len() {
                self.items.swap(sel, sel + 1);
            }
            self.selected = (sel + 1).min(self.items.len() - 1);
        }
    }

    fn list(&mut self, ui: &mut Ui, p: &Palette, cx: &mut Cx) {
        let mut remove = None;
        let mut drop_at: Option<(usize, usize)> = None;
        let released = ui.input(|i| i.pointer.any_released());
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mut rects = Vec::new();
        for i in 0..self.items.len() {
            let selected = i == self.selected;
            let frame = egui::Frame::new().fill(if selected { p.selection } else { p.panel2 }).inner_margin(egui::Margin::symmetric(6, 3)).corner_radius(3.0);
            let r = frame
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let handle = ui.add(egui::Label::new(RichText::new("⠿").color(p.faint)).sense(egui::Sense::drag()));
                        if handle.drag_started() {
                            self.drag = Some(i);
                        }
                        let item = &mut self.items[i];
                        match item.action {
                            RebaseAction::Exec => {
                                ui.label(RichText::new("exec").color(action_color(item.action, p)));
                                ui.add(crate::widgets::input(&mut item.text).hint_text("shell command").desired_width(f32::INFINITY));
                            }
                            RebaseAction::Break | RebaseAction::Other => {
                                ui.label(RichText::new(if item.action == RebaseAction::Break { "break" } else { item.text.as_str() }).color(p.dim).monospace());
                            }
                            _ => {
                                egui::ComboBox::from_id_salt(("action", i)).selected_text(RichText::new(item.action.keyword()).color(action_color(item.action, p))).width(74.0).show_ui(ui, |ui| {
                                    for a in RebaseAction::CHOICES {
                                        ui.selectable_value(&mut item.action, a, RichText::new(a.keyword()).color(action_color(a, p)));
                                    }
                                });
                                ui.label(RichText::new(gitem_core::util::prefix(&item.sha, 8)).monospace().color(p.faint));
                                let crossed = item.action == RebaseAction::Drop;
                                let mut text = RichText::new(&item.text);
                                if crossed {
                                    text = text.strikethrough().color(p.faint);
                                }
                                let l = ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click()));
                                if l.double_clicked() && self.todo_file.is_none() {
                                    cx.inspect(&item.sha);
                                }
                                if l.clicked() {
                                    self.selected = i;
                                }
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !RebaseAction::CHOICES.contains(&self.items[i].action) && ui.small_button("✖").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                })
                .response;
            if ui.interact(r.rect, ui.id().with(("rb-row", i)), egui::Sense::click()).clicked() {
                self.selected = i;
            }
            rects.push(r.rect);
            ui.add_space(1.0);
        }
        // Drop indicator and reorder.
        if let (Some(from), Some(pos)) = (self.drag, pointer) {
            let to = rects.iter().position(|r| pos.y < r.center().y).unwrap_or(rects.len());
            if let Some(r) = rects.get(to.min(rects.len().saturating_sub(1))) {
                let y = if to < rects.len() { r.top() } else { r.bottom() };
                ui.painter().hline(r.x_range(), y, egui::Stroke::new(2.0, p.accent));
            }
            if released {
                drop_at = Some((from, to));
            }
        }
        if released {
            self.drag = None;
        }
        if let Some((from, to)) = drop_at {
            let item = self.items.remove(from);
            let to = if to > from { to - 1 } else { to };
            self.items.insert(to.min(self.items.len()), item);
            self.selected = to.min(self.items.len() - 1);
        }
        if let Some(r) = remove {
            self.items.remove(r);
        }
    }

    fn start(&mut self, cx: &mut Cx) {
        if let Some(path) = &self.todo_file {
            match std::fs::write(path, rebase::render_todo(&self.items)) {
                Ok(()) => self.finished = Some(true),
                Err(e) => cx.error(format!("Could not write the todo file: {e}")),
            }
            return;
        }
        let now = cx.repo.resolve("HEAD").unwrap_or_default();
        if self.planned_head.get().is_none_or(|h| *h != now) {
            self.stale = true;
            return;
        }
        let o = RebaseOpts { autostash: self.autostash, update_refs: self.update_refs, ..Default::default() };
        match forms::start_interactive_rebase(cx.repo, self.base.as_deref(), &self.items, o) {
            Ok(op) => {
                cx.out.push(Action::Run(op));
                self.started = false;
            }
            Err(e) => cx.error(e),
        }
    }
}

fn action_color(a: RebaseAction, p: &Palette) -> egui::Color32 {
    match a {
        RebaseAction::Pick => p.text,
        RebaseAction::Reword => p.series[0],
        RebaseAction::Edit => p.series[6],
        RebaseAction::Squash => p.series[3],
        RebaseAction::Fixup => p.series[4],
        RebaseAction::Drop => p.del_fg,
        RebaseAction::Exec => p.series[2],
        _ => p.dim,
    }
}
