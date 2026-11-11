//! The offer of an update and the button that looks for one, drawn the same in About, in
//! Settings and in the notice in the corner. It draws from a picture of where the update is,
//! taken once a frame, and answers with actions: the app does the rest.

use egui::{RichText, Ui};

use crate::action::Action;
use crate::theme::Palette;

/// A release newer than this copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub version: String,
    /// The release's page.
    pub page: String,
}

/// Where the update is, for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateStatus {
    pub found: Option<Found>,
    /// It's in place and starts the next time gitem does.
    pub installed: bool,
    /// Being downloaded or put in place.
    pub progress: Option<Progress>,
    /// Whether this copy can be replaced from here.
    pub can_install: bool,
    /// Why it can't, in words.
    pub why_not: Option<String>,
    /// What went wrong installing it.
    pub error: Option<String>,
    /// What a look that was asked for found, when that wasn't a release.
    pub answer: Option<String>,
    pub checking: bool,
    /// Whether Settings names an address to ask.
    pub can_check: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub installing: bool,
}

/// Nothing known: for windows that never look.
pub static NONE: UpdateStatus = UpdateStatus { found: None, installed: false, progress: None, can_install: false, why_not: None, error: None, answer: None, checking: false, can_check: false };

/// The release that was found and what can be done with it. Nothing when none was found.
/// `notice`: in the corner, where it can be put away.
pub fn offer(ui: &mut Ui, s: &UpdateStatus, p: &Palette, out: &mut Vec<Action>, notice: bool) {
    let Some(r) = &s.found else { return };
    if s.installed {
        ui.horizontal(|ui| {
            ui.colored_label(p.good, format!("Version {} is installed. It starts the next time gitem does.", r.version));
            if ui.button("Restart now").clicked() {
                out.push(Action::UpdateRestart);
            }
            if notice && ui.button("Later").clicked() {
                out.push(Action::UpdateLater);
            }
        });
        return;
    }
    if let Some(pr) = s.progress {
        ui.horizontal(|ui| {
            let mb = |n: u64| n as f64 / (1024.0 * 1024.0);
            let text = if pr.installing {
                format!("Installing version {}…", r.version)
            } else if pr.total > 0 {
                format!("Downloading version {}: {:.1} of {:.1} MB", r.version, mb(pr.done), mb(pr.total))
            } else {
                format!("Downloading version {}…", r.version)
            };
            ui.label(text);
            if !pr.installing && ui.button("Stop").clicked() {
                out.push(Action::UpdateStop);
            }
        });
        if !pr.installing && pr.total > 0 {
            ui.add(egui::ProgressBar::new(pr.done as f32 / pr.total as f32).desired_width(260.0).desired_height(4.0));
        }
        return;
    }
    ui.horizontal(|ui| {
        ui.colored_label(p.good, format!("Version {} is available.", r.version));
        if s.can_install && ui.button("Update and restart").on_hover_text("Downloads it from the release, checks it against the release's checksums, replaces this copy and starts it again.").clicked() {
            out.push(Action::UpdateStart);
        }
        if ui.button("Open its page").on_hover_text("The release's page, in the browser.").clicked() {
            out.push(Action::OpenUrl(r.page.clone()));
        }
        if notice && ui.button("Later").on_hover_text("About and Settings have it until then.").clicked() {
            out.push(Action::UpdateLater);
        }
    });
    if let Some(why) = &s.why_not {
        ui.label(RichText::new(why).small().color(p.dim));
    }
    if let Some(e) = &s.error {
        ui.add(egui::Label::new(RichText::new(format!("The update wasn't installed: {e}")).small().color(p.warn)).wrap());
    }
}

/// The button that looks for a release now, named `label`, and what the last look that was
/// asked for found.
pub fn check(ui: &mut Ui, s: &UpdateStatus, p: &Palette, out: &mut Vec<Action>, label: &str) {
    if s.found.is_none()
        && let Some(answer) = &s.answer
    {
        ui.label(RichText::new(answer).small().color(p.dim));
    }
    let reason = if s.can_check { "Already asking." } else { "Give an address to ask in Settings > Workflow." };
    if ui.add_enabled(s.can_check && !s.checking, egui::Button::new(label)).on_disabled_hover_text(reason).clicked() {
        out.push(Action::UpdateCheck);
    }
}
