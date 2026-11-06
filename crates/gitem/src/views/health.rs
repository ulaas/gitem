//! Repository Health: how this repository is tuned, with each optimization
//! explained and reversible, plus on-demand maintenance.
use egui::{RichText, Ui};
use gitem_core::model::RepoHealth;
use gitem_core::ops;

use crate::action::Cx;
use crate::task::Task;
use crate::widgets::*;

#[derive(Default)]
pub struct HealthView {
    health: Task<RepoHealth>,
    loaded_gen: Option<u64>,
}

fn verdict(h: &RepoHealth) -> (&'static str, &'static str, usize) {
    let on = h.items.iter().filter(|i| i.enabled).count();
    let large = h.tracked_files > 20_000 || h.commits > 100_000;
    match (large, on) {
        (false, _) => ("Healthy", "This repository is small enough that git stays fast without tuning.", on),
        (true, n) if n >= 3 => ("Tuned", "This is a large repository, and the main optimizations are on.", on),
        (true, _) => ("Could be faster", "This is a large repository. Turning on the options below will speed up everyday commands.", on),
    }
}

impl HealthView {
    pub fn key(&self) -> String {
        "health".into()
    }
    pub fn title(&self) -> String {
        "Repository Health".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.health.poll();
        if self.loaded_gen != Some(cx.data.status_gen) {
            self.loaded_gen = Some(cx.data.status_gen);
            let r = cx.repo.clone();
            self.health.spawn(ui.ctx(), move || r.health());
        }
        if let Some(e) = &self.health.error {
            error_box(ui, e, &p);
        }
        let Some(h) = self.health.get().cloned() else {
            spinner_row(ui, "Measuring the repository...", &p);
            return;
        };
        let (title, detail, _) = verdict(&h);
        ui.heading(title);
        ui.label(RichText::new(detail).color(p.dim));
        ui.add_space(4.0);
        ui.label(RichText::new(format!("{} tracked files, {} commits, {} loose objects, {} in packs", h.tracked_files, h.commits, h.loose_objects, h.pack_size)).small().color(p.faint));
        ui.add_space(6.0);
        if ui.button("Run maintenance now").on_hover_text("Git's safe housekeeping: packs objects and refreshes caches").clicked() {
            cx.run(ops::GitOp::new("Run maintenance").step(&["maintenance", "run"]));
        }
        section(ui, "Optimizations", &p);
        egui::ScrollArea::vertical().id_salt("health").auto_shrink([false, false]).show(ui, |ui| {
            for item in &h.items {
                egui::Frame::new().fill(p.panel2).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&item.label).strong());
                        if item.enabled {
                            chip(ui, "On", p.good);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if item.enabled {
                                if ui.small_button("Undo").clicked() {
                                    let mut op = ops::GitOp::new(format!("Turn off {}", item.label));
                                    for (k, _) in &item.config {
                                        op = op.step(&["config", "--local", "--unset", k]).allow_fail();
                                    }
                                    cx.run(op);
                                }
                            } else if ui.small_button("Enable").clicked() {
                                let mut op = ops::GitOp::new(format!("Turn on {}", item.label));
                                for (k, v) in &item.config {
                                    op = op.step(&["config", "--local", k, v]);
                                }
                                cx.run(op);
                            }
                        });
                    });
                    ui.label(RichText::new(&item.detail).small().color(p.dim));
                });
                ui.add_space(3.0);
            }
        });
    }
}
