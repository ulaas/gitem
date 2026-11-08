//! Clean up branches: the local ones whose work is finished, ticked off and deleted together.
use std::collections::BTreeSet;

use egui::{RichText, Ui};
use gitem_core::model::Branch;
use gitem_core::ops;

use crate::action::Cx;
use crate::task::Task;
use crate::widgets::*;

/// Why a branch is offered for deleting. What's ticked to begin with is only what loses nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    /// Its commits are all on the base branch.
    pub merged: bool,
    /// Its upstream was deleted on the remote.
    pub gone: bool,
    pub subject: String,
}

/// The branches worth asking about. Not the current one, the base, or one a worktree has out,
/// which git refuses. A branch with an upstream still there is someone's work in progress, so
/// only `merged` brings it in.
pub fn candidates(branches: &[Branch], merged: &BTreeSet<String>, base: &str) -> Vec<Candidate> {
    branches
        .iter()
        .filter(|b| !b.is_remote && !b.is_current && b.name != base && b.worktree.is_none())
        .filter_map(|b| {
            let is_merged = merged.contains(&b.name);
            (is_merged || b.upstream_gone).then(|| Candidate { name: b.name.clone(), merged: is_merged, gone: b.upstream_gone, subject: b.subject.clone() })
        })
        .collect()
}

/// The question before deleting several, here and from the sidebar: `rest` isn't merged into `base`.
pub fn deleting_says(safe: &[String], rest: &[String], base: &str) -> String {
    let mut body = format!("Delete {}?", plural(safe.len() + rest.len(), "branch", "branches"));
    if !rest.is_empty() {
        body.push_str(&format!(" {} commits that aren't merged into {base}: {}.", if rest.len() == 1 { "This one has".to_string() } else { format!("{} of them have", rest.len()) }, rest.join(", ")));
    }
    body.push_str(" The toolbar's Undo can restore them straight afterwards.");
    body
}

/// Deleting branches picked elsewhere: finds out off the UI thread which are merged, then asks as
/// Clean up branches does, and has the same Undo.
pub fn delete_picked(repo: gitem_core::Repo, names: Vec<String>, base: String, confirming: bool) -> crate::action::Action {
    crate::action::Action::Later(Box::new(move || {
        // With no such base here, nothing counts as merged, and the question names them all.
        let merged: BTreeSet<String> = if repo.resolve(&base).is_ok() { repo.merged_branches(&base).map_err(|e| e.to_string())?.into_iter().collect() } else { BTreeSet::new() };
        let (safe, rest): (Vec<String>, Vec<String>) = names.into_iter().partition(|n| merged.contains(n));
        let op = ops::branches_delete(&safe, &rest);
        Ok(if confirming {
            crate::action::Later::Confirm { title: "Delete branches".to_string(), body: deleting_says(&safe, &rest, &base), label: "Delete".to_string(), op }
        } else {
            crate::action::Later::Run(op)
        })
    }))
}

#[derive(Default)]
pub struct PruneView {
    merged: Task<BTreeSet<String>>,
    loaded_for: Option<u64>,
    /// Unticked by hand, or ticked though it isn't merged.
    unticked: BTreeSet<String>,
    ticked: BTreeSet<String>,
}

impl PruneView {
    pub fn key(&self) -> String {
        "prune".into()
    }
    pub fn title(&self) -> String {
        "Clean up branches".into()
    }

    fn is_ticked(&self, c: &Candidate) -> bool {
        if c.merged { !self.unticked.contains(&c.name) } else { self.ticked.contains(&c.name) }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.merged.poll();
        let base = cx.data.base_branch();
        if self.loaded_for != Some(cx.data.refs_gen) {
            self.loaded_for = Some(cx.data.refs_gen);
            let (repo, b) = (cx.repo.clone(), base.clone());
            self.merged.spawn(ui.ctx(), move || {
                // With no commits yet, or no such branch here, nothing is merged into it.
                if repo.resolve(&b).is_err() {
                    return Ok(BTreeSet::new());
                }
                repo.merged_branches(&b).map(|v| v.into_iter().collect()).map_err(|e| e.to_string())
            });
        }
        if let Some(e) = &self.merged.error {
            error_box(ui, &format!("Couldn't work out what's merged into {base}: {e}"), &p);
        }
        let Some(merged) = self.merged.get() else {
            spinner_row(ui, "Looking at branches…", &p);
            return;
        };
        let list = candidates(cx.data.branches(), merged, &base);
        ui.label(RichText::new(format!("Local branches already merged into {base}, or whose branch on the remote has gone. Commits on a branch that isn't merged are reachable only through the reflog afterwards.")).color(p.dim));
        ui.horizontal(|ui| {
            let op = ops::fetch(&ops::FetchOpts { all: true, prune: true, ..Default::default() });
            if crate::widgets::op_button(ui, &cx.data.ops_now, "Fetch and prune first", &op).on_hover_text("Find out which branches have gone from the remote").clicked() {
                cx.run(op);
            }
            let chosen: Vec<&Candidate> = list.iter().filter(|c| self.is_ticked(c)).collect();
            let label = format!("Delete {}", plural(chosen.len(), "branch", "branches"));
            if ui.add_enabled(!chosen.is_empty(), egui::Button::new(RichText::new(label).color(if chosen.is_empty() { p.faint } else { p.danger }))).on_disabled_hover_text("Tick a branch below first.").clicked() {
                let (safe, unsafe_): (Vec<&Candidate>, Vec<&Candidate>) = chosen.iter().partition(|c| c.merged);
                let safe: Vec<String> = safe.iter().map(|c| c.name.clone()).collect();
                let rest: Vec<String> = unsafe_.iter().map(|c| c.name.clone()).collect();
                cx.confirm("Clean up branches", &deleting_says(&safe, &rest, &base), "Delete", ops::branches_delete(&safe, &rest));
            }
            if self.merged.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.separator();
        if list.is_empty() {
            empty_state(ui, "Nothing to clean up", &format!("No local branch is merged into {base} or has lost its remote branch."), &p);
            return;
        }
        egui::ScrollArea::vertical().id_salt("prune").auto_shrink([false, false]).show(ui, |ui| {
            for c in &list {
                ui.horizontal(|ui| {
                    let mut on = self.is_ticked(c);
                    if ui.checkbox(&mut on, RichText::new(&c.name).strong()).changed() {
                        let set = if c.merged { &mut self.unticked } else { &mut self.ticked };
                        if on == c.merged {
                            set.remove(&c.name);
                        } else {
                            set.insert(c.name.clone());
                        }
                    }
                    if c.merged {
                        chip(ui, "merged", p.good);
                    } else {
                        chip(ui, "not merged", p.warn);
                    }
                    if c.gone {
                        chip(ui, "gone from remote", p.dim);
                    }
                    ui.add(egui::Label::new(RichText::new(&c.subject).small().color(p.dim)).truncate());
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str) -> Branch {
        Branch { name: name.to_string(), ..Default::default() }
    }

    #[test]
    fn the_current_base_and_checked_out_elsewhere_are_never_offered() {
        let merged: BTreeSet<String> = ["main", "here", "wt", "done"].map(String::from).into_iter().collect();
        let list = vec![Branch { is_current: true, ..branch("here") }, branch("main"), Branch { worktree: Some("x".into()), ..branch("wt") }, branch("done"), Branch { upstream: Some("origin/w".into()), ..branch("wip") }, Branch { upstream_gone: true, ..branch("left") }, Branch { is_remote: true, ..branch("origin/done") }];
        let got = candidates(&list, &merged, "main");
        let names: Vec<(&str, bool)> = got.iter().map(|c| (c.name.as_str(), c.merged)).collect();
        assert_eq!(names, vec![("done", true), ("left", false)]);
    }
}
