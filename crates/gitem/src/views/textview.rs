//! Shows the result of a one-off job in a tab: AI explanations, reviews, changelogs.
use egui::{RichText, Ui};
use gitem_core::{Repo, ai, cred};

use crate::action::{Action, Cx};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

type Job = Box<dyn FnOnce() -> Result<String, String> + Send>;

/// Filled in by a job that can be undone.
pub type UndoSlot = std::sync::Arc<std::sync::Mutex<Option<gitem_core::ops::GitOp>>>;

pub struct TextView {
    title: String,
    job: Option<Job>,
    text: Task<String>,
    undo: Option<UndoSlot>,
}

impl TextView {
    pub fn new(title: &str, job: Job) -> Self {
        Self { title: title.to_string(), job: Some(job), text: Task::default(), undo: None }
    }

    pub fn with_undo(title: &str, job: Job, undo: UndoSlot) -> Self {
        Self { undo: Some(undo), ..Self::new(title, job) }
    }

    pub fn key(&self) -> String {
        format!("text:{}", self.title)
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.text.poll() {
            cx.out.push(Action::Refresh);
        }
        if let Some(job) = self.job.take() {
            self.text.spawn(ui.ctx(), job);
        }
        if let Some(e) = &self.text.error {
            error_box(ui, e, &p);
            return;
        }
        let Some(t) = self.text.get().cloned() else {
            spinner_row(ui, "Asking your AI provider...", &p);
            return;
        };
        ui.horizontal(|ui| {
            if ui.small_button("Copy").clicked() {
                cx.copy(t.clone());
            }
            if let Some(slot) = &self.undo {
                let op = slot.lock().ok().and_then(|g| g.clone());
                if let Some(op) = op
                    && ui.small_button("Undo…").clicked() {
                        cx.confirm("Undo", "Go back to where the branch was before this started? Uncommitted changes are kept. If they're in a file this has to change, it stops and changes nothing, so stash them first.", "Undo", op);
                    }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("text").auto_shrink([false, false]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(t).color(p.text)).selectable(true));
        });
    }
}

/// Open a tab running an AI request whose prompt comes from `build`.
pub fn ai_tab(cx: &mut Cx, title: &str, build: impl FnOnce(&Repo) -> Result<(String, String), String> + Send + 'static) {
    let repo = cx.repo.clone();
    let cfg = cx.settings.ai.clone();
    let job: Job = Box::new(move || {
        let (s, u) = build(&repo)?;
        let key = cred::ai_key(cfg.provider.key_name());
        ai::complete(&cfg, key.as_deref(), &s, &u).map_err(|e| e.to_string())
    });
    cx.out.push(Action::OpenTab(Tab::Text(Box::new(TextView::new(title, job)))));
}
