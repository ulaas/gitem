//! Patch library: save changes as named patches, apply them to any repository,
//! and share them as a secret GitHub gist. No account or cloud service required.
use egui::{RichText, Ui};
use gitem_core::patches::{self, PatchMeta};
use gitem_core::repo::DiffSource;
use gitem_core::{ops, util};

use crate::action::Cx;
use crate::forms;
use crate::task::Task;
use crate::views::launchpad::resolve_client;
use crate::widgets::*;

#[derive(Default)]
pub struct PatchesView {
    list: Vec<PatchMeta>,
    loaded: bool,
    selected: Option<String>,
    /// The selected patch for reading. Copying and sharing need exact text, see `exact`.
    content: String,
    /// `content` is the patch to the byte. It isn't if the patch holds other encodings.
    exact: bool,
    create_src: Option<DiffSource>,
    title: String,
    description: String,
    share: Task<(String, String)>,
    /// The patch being read from git, for Save: a big one takes as long as `git show` does.
    saving: Task<Vec<u8>>,
    listing: Task<Vec<PatchMeta>>,
    /// Delete was clicked and awaits a yes: there's no getting a patch back.
    confirm_delete: bool,
}


impl PatchesView {
    pub fn create_from(src: DiffSource, title: &str) -> Self {
        Self { create_src: Some(src), title: title.to_string(), ..Default::default() }
    }

    /// Opens with a saved patch selected.
    pub fn showing(id: &str) -> Self {
        let mut v = Self::default();
        v.select(id);
        v
    }

    /// "Save as patch" with this tab open still means a new patch, from that source.
    pub fn retarget(&mut self, new: Self) {
        if new.create_src.is_some() {
            self.create_src = new.create_src;
            self.title = new.title;
            self.description.clear();
        }
        if let Some(id) = new.selected {
            self.select(&id);
            self.loaded = false;
        }
    }

    fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        let bytes = patches::content(id).unwrap_or_default();
        self.exact = std::str::from_utf8(&bytes).is_ok();
        self.content = String::from_utf8_lossy(&bytes).into_owned();
        self.confirm_delete = false;
    }

    pub fn key(&self) -> String {
        "patches".into()
    }
    pub fn title(&self) -> String {
        "Patches".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.share.poll() {
            if let Some((id, url)) = self.share.value.take() {
                if let Some(m) = self.list.iter_mut().find(|m| m.id == id) {
                    m.gist_url = url.clone();
                    if let Err(e) = patches::save_meta(m) {
                        cx.error(e.to_string());
                    }
                }
                cx.copy(url);
                cx.toast("Gist created; link copied");
            }
            if let Some(e) = self.share.error.take() {
                cx.error(e);
            }
        }
        if !self.loaded {
            self.loaded = true;
            self.listing.spawn(ui.ctx(), || -> Result<Vec<PatchMeta>, String> { Ok(patches::list()) });
        }
        if self.listing.poll()
            && let Some(list) = self.listing.value.take()
        {
            self.list = list;
        }
        if self.saving.poll() {
            if let Some(e) = self.saving.error.take() {
                cx.error(e);
            }
            match self.saving.value.take() {
                Some(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => cx.error("That selection has no changes to save."),
                Some(bytes) => {
                    let now = util::now_ts();
                    let meta = PatchMeta {
                        id: format!("{now}-{}", std::process::id()),
                        title: self.title.trim().to_string(),
                        description: self.description.trim().to_string(),
                        repo: cx.repo.name(),
                        created: now,
                        base: cx.status().map(|s| s.head_sha.clone()).unwrap_or_default(),
                        gist_url: String::new(),
                    };
                    match patches::save(&meta, &bytes) {
                        Ok(()) => {
                            self.select(&meta.id);
                            self.create_src = None;
                            self.description.clear();
                            self.loaded = false;
                        }
                        Err(e) => cx.error(e.to_string()),
                    }
                }
                None => {}
            }
        }
        ui.horizontal(|ui| {
            if ui.button("New from staged").clicked() {
                self.create_src = Some(DiffSource::Staged);
                self.title.clear();
            }
            if ui.button("New from all changes").clicked() {
                self.create_src = Some(DiffSource::WorkingVsHead);
                self.title.clear();
            }
            if ui.button("Apply a patch file…").clicked() {
                cx.form(forms::apply_patch_form());
            }
            if self.share.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.separator();
        if let Some(src) = self.create_src.clone() {
            egui::Frame::new().fill(p.panel2).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("New patch").strong());
                ui.add(crate::widgets::input(&mut self.title).hint_text("Title").desired_width(f32::INFINITY));
                ui.add(crate::widgets::text_area(&mut self.description).hint_text("Description").desired_rows(2).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.title.trim().is_empty() && !self.saving.busy(), egui::Button::new("Save")).on_disabled_hover_text("It needs a title, and one being saved has to finish first.").clicked() {
                        let src = src.clone();
                        self.saving.spawn_git(ui.ctx(), cx.repo, move |r| r.patch_bytes(&src));
                    }
                    if self.saving.busy() {
                        ui.add(spinner(14.0));
                    }
                    if ui.button("Cancel").clicked() {
                        self.create_src = None;
                    }
                });
            });
        }
        let mut clicked = None;
        egui::Panel::left("patch-list").resizable(true).default_size(280.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("patch-items").auto_shrink([false, false]).show(ui, |ui| {
                if self.list.is_empty() {
                    empty_state(ui, "No patches yet", "Save changes here to reuse or share them.", &p);
                }
                ui.spacing_mut().item_spacing.y = 0.0;
                for m in &self.list {
                    let (rect, resp) = row(ui, 38.0, self.selected.as_deref() == Some(&m.id), &p);
                    paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 3.0), egui::vec2(rect.width() - 12.0, 17.0)), &m.title, prop(13.0), p.text);
                    paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 20.0), egui::vec2(rect.width() - 12.0, 15.0)), &format!("{} · {}", m.repo, time_label(m.created, cx.settings.date_relative)), prop(11.0), p.dim);
                    if resp.clicked() {
                        clicked = Some(m.id.clone());
                    }
                }
            });
        });
        if let Some(id) = clicked {
            self.select(&id);
        }
        let Some(m) = self.selected.clone().and_then(|id| self.list.iter().find(|m| m.id == id).cloned()) else {
            // Beside a list saying there are none, it would only say so twice.
            if self.list.is_empty() {
                return;
            }
            empty_state(ui, "Select a patch", "Preview, apply, or share it.", &p);
            return;
        };
        ui.label(RichText::new(&m.title).strong());
        if !m.description.is_empty() {
            ui.label(RichText::new(&m.description).color(p.dim));
        }
        ui.horizontal(|ui| {
            let file = patches::patch_path(&m.id).to_string_lossy().replace('\\', "/");
            if ui.button("Apply").clicked() {
                cx.run(ops::apply_patch_file(&file, true, false));
            }
            let not_text = "It holds text that isn't UTF-8, which only the patch file keeps exactly";
            if ui.add_enabled(self.exact, egui::Button::new("Copy")).on_disabled_hover_text(not_text).clicked() {
                cx.copy(self.content.clone());
            }
            if m.gist_url.is_empty() {
                let shareable = self.exact && cx.forge().is_some_and(|f| f.kind == gitem_core::forge::ForgeKind::GitHub);
                if ui.add_enabled(shareable, egui::Button::new("Share as secret gist")).on_disabled_hover_text(if self.exact { "Needs a GitHub remote and token" } else { not_text }).clicked() {
                    let (git, repo, id, title, content) = (cx.repo.git.clone(), cx.forge().cloned(), m.id.clone(), m.title.clone(), self.content.clone());
                    self.share.spawn(ui.ctx(), move || -> Result<(String, String), String> {
                        let r = repo.ok_or("No GitHub remote.")?;
                        let url = resolve_client(&git, &r)?.create_gist(&title, "change.patch", &content, false).map_err(|e| e.to_string())?;
                        Ok((id, url))
                    });
                }
            } else {
                crate::widgets::web_link(ui, "Open gist", &m.gist_url);
            }
            if self.confirm_delete {
                ui.colored_label(p.warn, "Delete this patch for good?");
                if ui.button("Delete it").clicked() {
                    if let Err(e) = patches::delete(&m.id) {
                        cx.error(e.to_string());
                    }
                    self.selected = None;
                    self.loaded = false;
                    self.confirm_delete = false;
                }
                if ui.button("Keep it").clicked() {
                    self.confirm_delete = false;
                }
            } else if ui.button("Delete…").clicked() {
                self.confirm_delete = true;
            }
        });
        ui.separator();
        egui::ScrollArea::both().id_salt("patch-preview").auto_shrink([false, false]).show(ui, |ui| {
            for l in self.content.lines().take(5000) {
                let color = if l.starts_with('+') && !l.starts_with("+++") {
                    p.add_fg
                } else if l.starts_with('-') && !l.starts_with("---") {
                    p.del_fg
                } else if l.starts_with("@@") {
                    p.series[6]
                } else {
                    p.dim
                };
                ui.label(RichText::new(l).monospace().color(color));
            }
        });
    }
}
