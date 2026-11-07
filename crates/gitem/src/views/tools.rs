//! Repository maintenance: submodules, hooks, ignore rules, clean, LFS, sparse
//! checkout, garbage collection, and the git config editor.
use egui::{RichText, Ui};
use gitem_core::model::*;
use gitem_core::ops;

use crate::action::Cx;
use crate::forms;
use crate::task::Task;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy, Default)]
enum Page {
    #[default]
    Submodules,
    Hooks,
    Ignore,
    Clean,
    Lfs,
    Sparse,
    Maintenance,
}

/// A hook does nothing on Unix without the executable bit.
fn create_hook(path: &str) -> std::io::Result<()> {
    std::fs::write(path, "#!/bin/sh\n\nexit 0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[derive(Default)]
pub struct ToolsView {
    page: Page,
    hooks: Option<Vec<HookInfo>>,
    /// The hook whose Delete was clicked and awaits a yes.
    confirm_delete_hook: Option<String>,
    ignore_text: Option<String>,
    ignore_test: String,
    ignore_result: Option<Option<String>>,
    clean_dirs: bool,
    clean_ignored: bool,
    clean_list: Task<Vec<String>>,
    clean_gen: u64,
    lfs: Task<Vec<LfsFile>>,
    lfs_started: bool,
    sparse_text: Option<String>,
    sparse_cone: bool,
    objects: Task<String>,
}

impl ToolsView {
    pub fn key(&self) -> String {
        "tools".into()
    }
    pub fn title(&self) -> String {
        "Repository Tools".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        ui.horizontal(|ui| {
            for (pg, t) in [
                (Page::Submodules, "Submodules"),
                (Page::Hooks, "Hooks"),
                (Page::Ignore, ".gitignore"),
                (Page::Clean, "Clean"),
                (Page::Lfs, "Git LFS"),
                (Page::Sparse, "Sparse checkout"),
                (Page::Maintenance, "Maintenance"),
            ] {
                ui.selectable_value(&mut self.page, pg, t);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("tools").auto_shrink([false, false]).show(ui, |ui| match self.page {
            Page::Submodules => self.submodules(ui, cx),
            Page::Hooks => self.hooks_ui(ui, cx),
            Page::Ignore => self.ignore(ui, cx),
            Page::Clean => self.clean(ui, cx),
            Page::Lfs => self.lfs_ui(ui, cx),
            Page::Sparse => self.sparse(ui, cx),
            Page::Maintenance => self.maintenance(ui, cx),
        });
    }

    fn submodules(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        ui.horizontal(|ui| {
            if ui.button("Add…").clicked() {
                cx.form(forms::add_submodule());
            }
            if ui.button("Update all").on_hover_text("Init, then check out the recorded commits").clicked() {
                cx.run(ops::submodule_update(None, true, true, false));
            }
            if ui.button("Update to latest").on_hover_text("Use each submodule's remote branch").clicked() {
                cx.run(ops::submodule_update(None, true, true, true));
            }
            if ui.button("Sync URLs").clicked() {
                cx.run(ops::submodule_sync());
            }
        });
        ui.add_space(4.0);
        let subs = cx.data.submodules().to_vec();
        if subs.is_empty() {
            empty_state(ui, "No submodules", "Add one to embed another repository.", &p);
        }
        for s in subs {
            egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&s.path).strong());
                    match s.state {
                        '-' => chip(ui, "not initialised", p.warn),
                        '+' => chip(ui, "different commit", p.series[3]),
                        'U' => chip(ui, "conflict", p.danger),
                        _ => chip(ui, "in sync", p.good),
                    }
                });
                ui.label(RichText::new(format!("{} {}  {}", gitem_core::util::prefix(&s.sha, 8), s.describe, s.url)).small().color(p.dim));
                ui.horizontal(|ui| {
                    if ui.small_button("Update").clicked() {
                        cx.run(ops::submodule_update(Some(&s.path), true, true, false));
                    }
                    if s.state != '-' && ui.small_button("Open").clicked() {
                        cx.out.push(crate::action::Action::OpenRepo(cx.repo.root.join(&s.path)));
                    }
                    if ui.small_button("Deinit…").clicked() {
                        cx.confirm("Deinitialise submodule", &format!("Remove the checkout of {} from the working tree?", s.path), "Deinit", ops::submodule_deinit(&s.path, true));
                    }
                    if ui.small_button("Remove…").clicked() {
                        cx.confirm("Remove submodule", &format!("Remove {} from this repository?", s.path), "Remove", ops::submodule_remove(&s.path));
                    }
                });
            });
            ui.add_space(3.0);
        }
    }

    fn hooks_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let hooks = self.hooks.get_or_insert_with(|| cx.repo.hooks()).clone();
        ui.label(RichText::new("Disabled hooks are renamed with a .disabled suffix so they're easy to turn back on.").small().color(p.faint));
        let mut reload = false;
        for h in &hooks {
            ui.horizontal(|ui| {
                ui.add_sized([180.0, 20.0], egui::Label::new(RichText::new(&h.name).monospace()));
                if h.exists {
                    let mut on = h.enabled;
                    if ui.checkbox(&mut on, "Enabled").changed() {
                        let target = if on { h.path.trim_end_matches(".disabled").to_string() } else { format!("{}.disabled", h.path) };
                        if let Err(e) = std::fs::rename(&h.path, &target) {
                            cx.error(format!("Could not rename the hook: {e}"));
                        }
                        reload = true;
                    }
                    if ui.small_button("Show").clicked() {
                        cx.out.push(crate::action::Action::OpenFile(h.path.clone().into()));
                    }
                    if self.confirm_delete_hook.as_deref() == Some(h.path.as_str()) {
                        ui.colored_label(p.warn, "Delete for good?");
                        if ui.small_button("Delete it").clicked() {
                            if let Err(e) = std::fs::remove_file(&h.path) {
                                cx.error(format!("Could not delete the hook: {e}"));
                            }
                            self.confirm_delete_hook = None;
                            reload = true;
                        }
                        if ui.small_button("Keep it").clicked() {
                            self.confirm_delete_hook = None;
                        }
                    } else if ui.small_button("Delete…").clicked() {
                        self.confirm_delete_hook = Some(h.path.clone());
                    }
                } else if ui.small_button("Create").clicked() {
                    if let Err(e) = create_hook(&h.path) {
                        cx.error(format!("Could not create the hook: {e}"));
                    }
                    reload = true;
                }
            });
        }
        if reload {
            self.hooks = None;
        }
    }

    fn ignore(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let path = cx.repo.ignore_path();
        let text = self.ignore_text.get_or_insert_with(|| std::fs::read_to_string(&path).unwrap_or_default());
        ui.add(crate::widgets::text_area(text).code_editor().desired_rows(16).desired_width(f32::INFINITY));
        let mut reload = false;
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                match std::fs::write(&path, text.as_bytes()) {
                    Ok(()) => cx.toast("Saved .gitignore"),
                    Err(e) => cx.error(format!("Could not save: {e}")),
                }
            }
            reload = ui.button("Reload").clicked();
        });
        if reload {
            self.ignore_text = None;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Test a path");
            if ui.add(crate::widgets::input(&mut self.ignore_test).hint_text("build/output.log").desired_width(280.0)).changed() {
                let t = self.ignore_test.trim();
                self.ignore_result = (!t.is_empty()).then(|| cx.repo.check_ignore(t));
            }
            match &self.ignore_result {
                Some(Some(rule)) => {
                    ui.colored_label(p.warn, format!("Ignored by {rule}"));
                }
                Some(None) => {
                    ui.colored_label(p.good, "Not ignored");
                }
                None => {}
            }
        });
    }

    fn clean(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.clean_list.poll();
        let mut reload = self.clean_gen != cx.data.status_gen + 1;
        ui.horizontal(|ui| {
            reload |= ui.checkbox(&mut self.clean_dirs, "Include folders").changed();
            reload |= ui.checkbox(&mut self.clean_ignored, "Include ignored files").changed();
        });
        if reload {
            self.clean_gen = cx.data.status_gen + 1;
            let (r, d, i) = (cx.repo.clone(), self.clean_dirs, self.clean_ignored);
            self.clean_list.spawn(ui.ctx(), move || r.clean_preview(d, i));
        }
        let list = self.clean_list.get().cloned().unwrap_or_default();
        ui.label(RichText::new(format!("{} item(s) would be deleted", list.len())).color(p.dim));
        let b = egui::Button::new(RichText::new("Delete these…").color(egui::Color32::WHITE)).fill(p.danger);
        if ui.add_enabled(!list.is_empty(), b).clicked() {
            cx.confirm("Clean working tree", &format!("Permanently delete {} untracked item(s)? This cannot be undone.", list.len()), "Delete", ops::clean(self.clean_dirs, self.clean_ignored));
        }
        ui.add_space(4.0);
        for l in list.iter().take(500) {
            ui.label(RichText::new(l).monospace().small());
        }
        if list.len() > 500 {
            ui.label(RichText::new(format!("…and {} more", list.len() - 500)).small().color(p.faint));
        }
    }

    fn lfs_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.lfs.poll();
        if !self.lfs_started {
            self.lfs_started = true;
            let r = cx.repo.clone();
            self.lfs.spawn(ui.ctx(), move || r.lfs_files());
        }
        ui.horizontal(|ui| {
            if ui.button("Install in this repository").clicked() {
                cx.run(ops::lfs_install());
            }
            if ui.button("Track a pattern…").clicked() {
                cx.form(forms::lfs_track_form());
            }
            if ui.button("Pull objects").clicked() {
                cx.run(ops::lfs_pull());
            }
            if ui.button("Prune old objects").clicked() {
                cx.run(ops::lfs_prune());
            }
            if ui.button("Refresh").clicked() {
                self.lfs_started = false;
            }
        });
        if let Some(e) = &self.lfs.error {
            ui.colored_label(p.warn, format!("Git LFS is unavailable: {e}"));
            return;
        }
        let list = self.lfs.get().cloned().unwrap_or_default();
        ui.label(RichText::new(format!("{} tracked file(s)", list.len())).color(p.dim));
        for f in &list {
            ui.horizontal(|ui| {
                ui.label(RichText::new(gitem_core::util::prefix(&f.oid, 10)).monospace().small().color(p.faint));
                ui.label(&f.path);
                if !f.downloaded {
                    chip(ui, "pointer only", p.warn);
                }
            });
        }
    }

    fn sparse(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let text = self.sparse_text.get_or_insert_with(|| {
            self.sparse_cone = true;
            cx.repo.sparse_patterns().join("\n")
        });
        ui.label(RichText::new("Check out only these folders, one per line. Everything else stays out of the working tree.").color(p.dim));
        ui.add(crate::widgets::text_area(text).code_editor().desired_rows(10).desired_width(f32::INFINITY));
        ui.checkbox(&mut self.sparse_cone, "Cone mode (folders; faster)");
        let mut disabled = false;
        ui.horizontal(|ui| {
            let patterns: Vec<String> = text.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if ui.add_enabled(!patterns.is_empty(), egui::Button::new("Apply")).clicked() {
                cx.run(ops::sparse_set(&patterns, self.sparse_cone));
            }
            if ui.button("Disable sparse checkout").clicked() {
                cx.run(ops::sparse_disable());
                disabled = true;
            }
        });
        if disabled {
            self.sparse_text = None;
        }
    }

    fn maintenance(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.objects.poll();
        if self.objects.value.is_none() && !self.objects.busy() && self.objects.error.is_none() {
            let r = cx.repo.clone();
            self.objects.spawn(ui.ctx(), move || r.count_objects());
        }
        ui.horizontal(|ui| {
            if ui.button("Garbage collect").clicked() {
                cx.run(ops::gc(false, false));
            }
            if ui.button("Aggressive GC").on_hover_text("Slow; recompresses everything").clicked() {
                cx.run(ops::gc(true, false));
            }
            if ui.button("Prune unreachable now…").clicked() {
                cx.confirm("Prune unreachable objects", "Delete all unreachable objects right now? Commits recoverable through the reflog's expired entries will be lost.", "Prune", ops::gc(false, true));
            }
            if ui.button("Run maintenance").clicked() {
                cx.run(ops::maintenance_run());
            }
            if ui.button("Verify (fsck)").clicked() {
                cx.run(ops::fsck());
            }
        });
        ui.add_space(6.0);
        section(ui, "Object storage", &p);
        if let Some(t) = self.objects.get() {
            ui.label(RichText::new(t).monospace());
        }
    }
}

#[derive(Default)]
pub struct ConfigView {
    entries: Task<Vec<ConfigEntry>>,
    loaded_gen: Option<u64>,
    filter: String,
    show_system: bool,
    editing: Option<(String, String, String)>,
}

impl ConfigView {
    pub fn key(&self) -> String {
        "config".into()
    }
    pub fn title(&self) -> String {
        "Git Config".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.entries.poll();
        if self.loaded_gen != Some(cx.data.status_gen) || self.entries.value.is_none() && !self.entries.busy() {
            self.loaded_gen = Some(cx.data.status_gen);
            let r = cx.repo.clone();
            self.entries.spawn(ui.ctx(), move || r.config_list());
        }
        ui.horizontal(|ui| {
            ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter keys and values").desired_width(280.0));
            ui.checkbox(&mut self.show_system, "Include system");
            if ui.button("Set local…").clicked() {
                cx.form(forms::raw_config_form("local"));
            }
            if ui.button("Set global…").clicked() {
                cx.form(forms::raw_config_form("global"));
            }
        });
        ui.separator();
        if let Some(e) = &self.entries.error {
            error_box(ui, e, &p);
        }
        let Some(list) = self.entries.value.take() else {
            spinner_row(ui, "Reading config…", &p);
            return;
        };
        let q = self.filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt("config").auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("cfg-grid").striped(true).num_columns(4).show(ui, |ui| {
                for e in &list {
                    if e.scope == "system" && !self.show_system {
                        continue;
                    }
                    if !q.is_empty() && !e.key.to_lowercase().contains(&q) && !e.value.to_lowercase().contains(&q) {
                        continue;
                    }
                    ui.label(RichText::new(&e.scope).small().color(p.dim));
                    ui.label(RichText::new(&e.key).monospace());
                    let editable = matches!(e.scope.as_str(), "local" | "global" | "worktree");
                    let is_editing = self.editing.as_ref().is_some_and(|(s, k, _)| *s == e.scope && *k == e.key);
                    if is_editing {
                        if let Some((_, _, v)) = &mut self.editing {
                            ui.add(crate::widgets::input(v));
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("Save").clicked()
                                && let Some((s, k, v)) = self.editing.take() {
                                    cx.run(ops::config_set(&k, &v, &s));
                                }
                            if ui.small_button("Cancel").clicked() {
                                self.editing = None;
                            }
                        });
                    } else {
                        // Tokens, passwords in URLs and auth headers shouldn't show at a glance.
                        let shown = gitem_core::util::mask_config_value(&e.key, &e.value);
                        ui.add(egui::Label::new(shown).truncate());
                        ui.horizontal(|ui| {
                            if editable {
                                if ui.small_button("Edit").clicked() {
                                    self.editing = Some((e.scope.clone(), e.key.clone(), e.value.clone()));
                                }
                                if ui.small_button("Unset…").clicked() {
                                    cx.confirm("Unset config value", &format!("Remove {} from {} config?", e.key, e.scope), "Unset", ops::config_unset(&e.key, &e.scope));
                                }
                            }
                        });
                    }
                    ui.end_row();
                }
            });
        });
        self.entries.value = Some(list);
    }
}
