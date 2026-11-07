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

/// The plan a dry run was for, and the commit it would stop at with the files in conflict.
type Prediction = (String, Option<(String, Vec<String>)>);

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
    /// HEAD as it is now, being read so the plan can be checked against it.
    head_now: Task<String>,
    pending: Option<(String, RebaseAction)>,
    drag: Option<usize>,
    selected: usize,
    pub finished: Option<bool>,
    /// The plan as it came, to tell whether anyone has changed it.
    plan_as_loaded: String,
    /// What a dry run found, and the plan it was for: changing the plan makes it out of date.
    prediction: Task<Prediction>,
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
            head_now: Task::default(),
            pending: None,
            drag: None,
            selected: 0,
            finished: None,
            plan_as_loaded: String::new(),
            prediction: Task::default(),
        }
    }

    pub fn for_todo_file(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut v = Self::new(None);
        v.items = rebase::parse_todo(&text);
        v.plan_as_loaded = rebase::render_todo(&v.items);
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

    /// What closing this tab would lose: a plan someone has changed and not started.
    pub fn unsaved(&self) -> Option<String> {
        (self.finished.is_none() && !self.items.is_empty() && rebase::render_todo(&self.items) != self.plan_as_loaded).then(|| "A rebase plan you've changed and not started.".to_string())
    }

    pub fn key(&self) -> String {
        format!("rebase:{:?}:{}", self.base, self.root)
    }

    pub fn title(&self) -> String {
        "Rebase".to_string()
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
                self.plan_as_loaded = rebase::render_todo(&v);
                self.items = v;
                if let Some((sha, action)) = self.pending.take() {
                    match self.items.iter().position(|x| x.sha == sha) {
                        Some(i) => {
                            self.items[i].action = action;
                            self.selected = i;
                        }
                        // Better said than a plan that looks as if it will do what was asked.
                        None => cx.error(format!("{} isn't one of these commits, so \"{}\" hasn't been set on anything.", gitem_core::util::prefix(&sha, 8), action.keyword())),
                    }
                }
            }
        self.planned_head.poll();
        self.prediction.poll();
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
                ui.menu_button("\u{E136}", |ui| {
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
                    ui.add(spinner(14.0));
                }
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.autostash, "Autostash");
                ui.checkbox(&mut self.update_refs, "Update dependent branches");
                let can_check = self.base.is_some() && !self.items.is_empty() && !self.prediction.busy();
                if ui.add_enabled(can_check, egui::Button::new("Check for conflicts").small()).on_hover_text("Tries the plan out without touching anything").on_disabled_hover_text("Pick a base with commits to replay first.").clicked()
                    && let Some(base) = self.base.clone()
                {
                    let (repo, plan) = (cx.repo.clone(), rebase::render_todo(&self.items));
                    let shas: Vec<String> = self.items.iter().filter(|i| i.action.replays()).map(|i| i.sha.clone()).collect();
                    self.prediction.spawn(ui.ctx(), move || repo.predict_replay(&base, &shas).map(|found| (plan, found)).map_err(|e| e.to_string()));
                }
                if self.prediction.busy() {
                    ui.add(spinner(14.0));
                }
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
        if let Some(e) = &self.prediction.error {
            ui.colored_label(p.warn, format!("Couldn't check for conflicts: {e}"));
        }
        if let Some((_, found)) = self.prediction.get().filter(|(plan, _)| *plan == rebase::render_todo(&self.items)) {
            match found {
                None => {
                    ui.colored_label(p.good, "No conflicts expected.");
                }
                Some((sha, files)) => {
                    let which = self.items.iter().find(|i| i.sha == *sha).map_or(sha.as_str(), |i| i.text.as_str());
                    ui.colored_label(p.warn, format!("\"{which}\" will conflict in {}. Anything after that depends on how it's resolved.", files.join(", ")));
                }
            }
        }
        if self.todo_file.is_none()
            && let Some(st) = cx.status()
            && let Some(up) = &st.upstream
            && (st.ahead as usize) < self.items.len()
        {
            ui.colored_label(p.warn, format!("{} already on {up}. Rewriting them means the next push has to overwrite it.", plural(self.items.len() - st.ahead as usize, "of these commits is", "of these commits are")));
        }
        ui.horizontal(|ui| {
            // With nothing in the plan, these have nothing to work on.
            if self.items.is_empty() {
                ui.disable();
            }
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
            ui.label(RichText::new("Keys: p pick · r reword · e edit · s squash · f fixup · d drop · Alt+\u{E08E}/\u{E03E} move").small().color(p.faint));
        });
        ui.separator();

        self.start_if_head_unchanged(cx);
        let errs = rebase::validate(&self.items);
        egui::Panel::bottom("rebase-actions").show(ui, |ui| {
            ui.add_space(4.0);
            for e in &errs {
                ui.colored_label(p.danger, e);
            }
            ui.horizontal(|ui| {
                let ok = errs.is_empty() && !self.items.is_empty() && !self.stale && !self.head_now.busy();
                let b = egui::Button::new(RichText::new(if self.todo_file.is_some() { "Start rebase".to_string() } else { if self.items.is_empty() { "Rebase".to_string() } else { format!("Rebase {}", plural(self.items.len(), "commit", "commits")) } }).color(if ok { p.on_accent } else { p.faint })).fill(if ok { p.accent } else { p.panel2 });
                let why = if self.items.is_empty() { "Pick a base with commits between it and HEAD." } else if self.stale { "HEAD has moved. Rebuild the plan first." } else if !errs.is_empty() { "Fix what's listed above first." } else { "Checking that HEAD hasn't moved." };
                if ui.add_enabled(ok, b).on_disabled_hover_text(why).clicked() {
                    self.start(ui.ctx(), cx);
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

        self.keyboard(ui, &cx.settings.key_bindings);
        egui::ScrollArea::vertical().id_salt("rebase-list").auto_shrink([false, false]).show(ui, |ui| {
            if self.items.is_empty() && !self.loaded.busy() {
                empty_state(ui, "Nothing to rebase", "Pick a base with commits between it and HEAD.", &p);
            }
            self.list(ui, &p, cx);
        });
    }

    fn keyboard(&mut self, ui: &Ui, bound: &std::collections::HashMap<String, String>) {
        if ui.ctx().memory(|m| m.focused().is_some()) || self.items.is_empty() {
            return;
        }
        let sel = self.selected.min(self.items.len() - 1);
        use egui::Key::*;
        let set = [("rebase_pick", RebaseAction::Pick), ("rebase_reword", RebaseAction::Reword), ("rebase_edit", RebaseAction::Edit), ("rebase_squash", RebaseAction::Squash), ("rebase_fixup", RebaseAction::Fixup), ("rebase_drop", RebaseAction::Drop)];
        for (name, a) in set {
            if ui.input(|i| crate::keys::context_pressed(i, bound, name)) && RebaseAction::CHOICES.contains(&self.items[sel].action) {
                self.items[sel].action = a;
            }
        }
        let move_up = ui.input(|i| crate::keys::context_pressed(i, bound, "rebase_up"));
        let move_down = ui.input(|i| crate::keys::context_pressed(i, bound, "rebase_down"));
        if move_up && sel > 0 {
            self.items.swap(sel, sel - 1);
        }
        if move_down && sel + 1 < self.items.len() {
            self.items.swap(sel, sel + 1);
        }
        // Plain arrows, or the keys that moved the step, which goes along with it.
        if move_up || ui.input(|i| i.modifiers.is_none() && i.key_pressed(ArrowUp)) {
            self.selected = sel.saturating_sub(1);
        }
        if move_down || ui.input(|i| i.modifiers.is_none() && i.key_pressed(ArrowDown)) {
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
            // The row's click area has to be behind its contents. Added after them, it
            // would take every click, and the action box and handle could never get one.
            let row = ui.scope_builder(egui::UiBuilder::new().id_salt(("rb-row", i)).sense(egui::Sense::click()), |ui| {
                frame.show(ui, |ui| {
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
                            // An `amend!` commit is a raw line, and each choice here makes it plain.
                            RebaseAction::Other if rebase::parse_amend(&item.text).is_some() => {
                                let (sha, subject) = rebase::parse_amend(&item.text).unwrap_or_default();
                                let mut changed = None;
                                egui::ComboBox::from_id_salt(("action", i)).selected_text(RichText::new("amend").color(p.series_text(4))).width(74.0).show_ui(ui, |ui| {
                                    for a in RebaseAction::CHOICES {
                                        if ui.selectable_label(false, RichText::new(a.keyword()).color(action_color(a, p))).clicked() {
                                            changed = Some(a);
                                        }
                                    }
                                }).response.on_hover_text("Replaces the commit before it, message and all, as git does for an amend! commit");
                                ui.label(RichText::new(gitem_core::util::prefix(&sha, 8)).monospace().color(p.faint));
                                ui.add(egui::Label::new(RichText::new(&subject)).truncate());
                                if let Some(action) = changed {
                                    *item = TodoItem { action, sha, text: subject };
                                }
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
                            if !RebaseAction::CHOICES.contains(&self.items[i].action) && ui.small_button("\u{E4F6}").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                });
            });
            if row.response.clicked() {
                self.selected = i;
            }
            rects.push(row.response.rect);
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

    fn start(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        if let Some(path) = &self.todo_file {
            match std::fs::write(path, rebase::render_todo(&self.items)) {
                Ok(()) => self.finished = Some(true),
                Err(e) => cx.error(format!("Could not write the todo file: {e}")),
            }
            return;
        }
        if !self.head_now.busy() {
            let r = cx.repo.clone();
            self.head_now.spawn(ctx, move || r.resolve("HEAD"));
        }
    }

    /// The plan is good only for the HEAD it was made from.
    fn start_if_head_unchanged(&mut self, cx: &mut Cx) {
        if !self.head_now.poll() {
            return;
        }
        self.head_now.error = None;
        let now = self.head_now.value.take().unwrap_or_default();
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
        RebaseAction::Reword => p.series_text(0),
        RebaseAction::Edit => p.series_text(6),
        RebaseAction::Squash => p.series_text(3),
        RebaseAction::Fixup => p.series_text(4),
        RebaseAction::Drop => p.del_fg,
        RebaseAction::Exec => p.series_text(2),
        _ => p.dim,
    }
}
