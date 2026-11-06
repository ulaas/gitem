//! Agent sessions working in this repository's worktrees, as a board grouped by
//! what needs attention, with an activity treemap. Read from the agents' transcript
//! files, so status is an estimate and permission prompts are answered in the agent.
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, pos2, vec2};
use gitem_core::agents::{self, AgentSession, AgentState};
use gitem_core::{treemap, util};

use crate::action::{Action, Cx};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

const REFRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct AgentsView {
    sessions: Task<Vec<AgentSession>>,
    last: Option<Instant>,
    selected: Option<String>,
}


impl AgentsView {
    pub fn key(&self) -> String {
        "agents".into()
    }
    pub fn title(&self) -> String {
        "Agents".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.sessions.poll();
        ui.ctx().request_repaint_after(REFRESH);
        if self.last.is_none_or(|t| t.elapsed() >= REFRESH) && !self.sessions.busy() {
            self.last = Some(Instant::now());
            let worktrees: Vec<String> = cx.data.worktrees().iter().map(|w| w.path.clone()).collect();
            self.sessions.spawn(ui.ctx(), move || -> Result<Vec<AgentSession>, String> { Ok(agents::scan(&agents::default_roots(), &worktrees, util::now_ts())) });
        }
        ui.horizontal(|ui| {
            let agent = cx.settings.agent_command.clone();
            if !agent.trim().is_empty() && ui.button(format!("Start session ({agent})")).clicked() {
                cx.out.push(Action::OpenTerminal(cx.repo.root.clone(), Some(agent)));
            }
            ui.label(RichText::new("Status is estimated from each agent's transcript file. Answer permission prompts in the agent itself.").small().color(p.faint));
        });
        ui.separator();
        let Some(list) = self.sessions.value.take() else {
            spinner_row(ui, "Looking for agent sessions...", &p);
            return;
        };
        let shown: Vec<&AgentSession> = list.iter().filter(|s| !cx.settings.archived_sessions.contains(&s.id)).collect();
        if shown.is_empty() {
            empty_state(ui, "No agent sessions here", "Claude Code and Codex sessions in this repository's worktrees appear here.", &p);
        } else {
            let selected = self.selected.as_ref().and_then(|id| shown.iter().find(|s| s.id == *id).copied());
            if let Some(s) = selected {
                egui::Panel::bottom("agent-activity").resizable(true).default_size(240.0).show(ui, |ui| activity(ui, cx, s));
            }
            ui.columns(AgentState::ALL.len(), |cols| {
                for (c, state) in cols.iter_mut().zip(AgentState::ALL) {
                    let group: Vec<&&AgentSession> = shown.iter().filter(|s| s.state == state).collect();
                    c.label(RichText::new(format!("{} ({})", state.label(), group.len())).strong());
                    egui::ScrollArea::vertical().id_salt(state.label()).auto_shrink([false, false]).show(c, |ui| {
                        for s in group {
                            self.card(ui, cx, s);
                        }
                    });
                }
            });
        }
        self.sessions.value = Some(list);
    }

    fn card(&mut self, ui: &mut Ui, cx: &mut Cx, s: &AgentSession) {
        let p = cx.pal.clone();
        egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{} · {}", s.agent, if s.branch.is_empty() { "(no branch)" } else { &s.branch })).strong());
            ui.label(RichText::new(util::relative_time(s.modified, util::now_ts())).small().color(p.dim));
            if !s.last_text.is_empty() {
                ui.add(egui::Label::new(RichText::new(&s.last_text).small().color(p.dim)).truncate());
            }
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Resume").on_hover_text(s.resume_command()).clicked() {
                    cx.out.push(Action::OpenTerminal(s.cwd.clone().into(), Some(s.resume_command())));
                }
                if ui.small_button("Activity").clicked() {
                    self.selected = Some(s.id.clone());
                }
                if !s.branch.is_empty() && ui.small_button("Graph").clicked() {
                    cx.out.push(Action::GraphFocus(s.branch.clone()));
                }
                if ui.small_button("Archive").on_hover_text("Hide it here. The transcript isn't touched.").clicked() {
                    let id = s.id.clone();
                    cx.out.push(Action::Settings(Box::new(move |st| st.archived_sessions.push(id))));
                }
            });
        });
        ui.add_space(3.0);
    }
}

/// Files the session read or edited, sized by how often.
fn activity(ui: &mut Ui, cx: &mut Cx, s: &AgentSession) {
    let p = cx.pal.clone();
    ui.label(RichText::new(format!("Files this {} session touched recently", s.agent)).strong());
    if s.files.is_empty() {
        ui.label(RichText::new("None recorded yet.").color(p.dim));
        return;
    }
    let (outer, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
    let values: Vec<f64> = s.files.iter().map(|f| f.1 as f64).collect();
    let max = s.files.first().map_or(1, |f| f.1).max(1) as f32;
    let painter = ui.painter_at(outer);
    let hover = ui.input(|i| i.pointer.hover_pos()).filter(|h| outer.contains(*h));
    let mut hit = None;
    for c in treemap::layout(&values, outer.left(), outer.top(), outer.width(), outer.height()) {
        let (path, n) = &s.files[c.index];
        let r = Rect::from_min_size(pos2(c.x, c.y), vec2(c.w, c.h)).shrink(1.0);
        painter.rect_filled(r, 2.0, p.heat((*n as f32 / max).sqrt()));
        if c.w > 70.0 && c.h > 18.0 {
            painter.with_clip_rect(r.shrink(3.0)).text(r.min + vec2(4.0, 3.0), Align2::LEFT_TOP, path.rsplit('/').next().unwrap_or(path), prop(11.0), Color32::WHITE);
        }
        if hover.is_some_and(|h| r.contains(h)) {
            hit = Some(c.index);
        }
    }
    if let Some(i) = hit {
        let (path, n) = &s.files[i];
        egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), ui.id().with("agent-tip"), egui::PopupAnchor::Pointer).show(|ui| {
            ui.label(format!("{path}: {n} time(s)"));
        });
        let root = cx.repo.root.to_string_lossy().replace('\\', "/");
        if let (true, Some(rel)) = (resp.clicked(), path.strip_prefix(&format!("{root}/"))) {
            cx.open(Tab::file(rel, None, None));
        }
    }
}
