//! Generic modal forms and every parameterised git command built on them.
//! The command palette, context menus, and toolbars all open these same forms.
use std::collections::HashMap;

use egui::{RichText, Ui};
use gitem_core::ops::{self, GitOp};
use gitem_core::rebase::render_todo;
use gitem_core::{Commit, Repo};

use crate::action::Action;
use crate::theme::Palette;

pub enum Field {
    Text { key: &'static str, label: String, value: String, hint: String, password: bool },
    Multiline { key: &'static str, label: String, value: String },
    Check { key: &'static str, label: String, value: bool },
    Choice { key: &'static str, label: String, options: Vec<String>, value: usize },
    /// Text input with a filterable list of suggestions below it.
    Pick { key: &'static str, label: String, options: Vec<String>, value: String },
    Info(String),
    Path { key: &'static str, label: String, value: String, folder: bool },
}

pub struct Values {
    text: HashMap<&'static str, String>,
    choices: HashMap<&'static str, usize>,
}

impl Values {
    pub fn text(&self, k: &str) -> String {
        self.text.get(k).cloned().unwrap_or_default()
    }
    pub fn opt(&self, k: &str) -> Option<String> {
        self.text.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    pub fn on(&self, k: &str) -> bool {
        self.text.get(k).is_some_and(|v| v == "1")
    }
    /// Position of the option chosen in a `Choice`.
    pub fn index(&self, k: &str) -> usize {
        self.choices.get(k).copied().unwrap_or(0)
    }
    /// The value behind the chosen option of a `choice_of`. It goes by position, so
    /// rewording or translating a label can never change which command runs.
    pub fn picked<T: Copy>(&self, k: &str, table: &[(T, &str)]) -> T {
        table.get(self.index(k)).unwrap_or(&table[0]).0
    }
}

type Submit = Box<dyn FnMut(&Values) -> Result<Vec<Action>, String> + Send>;

pub struct FormDialog {
    pub title: String,
    pub fields: Vec<Field>,
    pub submit: String,
    pub danger: bool,
    pub on_submit: Submit,
    pub error: Option<String>,
    /// The values when first shown, to tell whether anything has been typed.
    first_shown: Option<String>,
    /// A file picker that's open, and which field it's for.
    picking: crate::task::Task<Option<std::path::PathBuf>>,
    picking_field: usize,
}

impl FormDialog {
    pub fn new(title: impl Into<String>, submit: impl Into<String>) -> Self {
        Self { title: title.into(), fields: Vec::new(), submit: submit.into(), danger: false, on_submit: Box::new(|_| Ok(Vec::new())), error: None, first_shown: None, picking: Default::default(), picking_field: 0 }
    }

    fn fingerprint(&self) -> String {
        let v = self.values();
        let mut parts: Vec<String> = v.text.iter().map(|(k, x)| format!("{k}={x}")).collect();
        parts.sort();
        parts.join("\n")
    }

    /// Whether anything in the form differs from how it opened.
    pub fn touched(&self) -> bool {
        self.first_shown.as_ref().is_some_and(|first| *first != self.fingerprint())
    }
    pub fn text(mut self, key: &'static str, label: &str, value: &str) -> Self {
        self.fields.push(Field::Text { key, label: label.into(), value: value.into(), hint: String::new(), password: false });
        self
    }
    pub fn hint(mut self, h: &str) -> Self {
        if let Some(Field::Text { hint, .. }) = self.fields.last_mut() {
            *hint = h.into();
        }
        self
    }
    pub fn password(mut self, key: &'static str, label: &str) -> Self {
        self.fields.push(Field::Text { key, label: label.into(), value: String::new(), hint: String::new(), password: true });
        self
    }
    pub fn multiline(mut self, key: &'static str, label: &str, value: &str) -> Self {
        self.fields.push(Field::Multiline { key, label: label.into(), value: value.into() });
        self
    }
    pub fn check(mut self, key: &'static str, label: &str, value: bool) -> Self {
        self.fields.push(Field::Check { key, label: label.into(), value });
        self
    }
    pub fn choice(mut self, key: &'static str, label: &str, options: &[&str], value: usize) -> Self {
        self.fields.push(Field::Choice { key, label: label.into(), options: options.iter().map(|s| s.to_string()).collect(), value });
        self
    }
    /// A choice where each option stands for a value. Read it back with `Values::picked`.
    pub fn choice_of<T>(self, key: &'static str, label: &str, table: &[(T, &str)], value: usize) -> Self {
        let options: Vec<&str> = table.iter().map(|o| o.1).collect();
        self.choice(key, label, &options, value)
    }
    pub fn pick(mut self, key: &'static str, label: &str, options: Vec<String>, value: &str) -> Self {
        self.fields.push(Field::Pick { key, label: label.into(), options, value: value.into() });
        self
    }
    pub fn info(mut self, s: impl Into<String>) -> Self {
        self.fields.push(Field::Info(s.into()));
        self
    }
    pub fn path(mut self, key: &'static str, label: &str, value: &str, folder: bool) -> Self {
        self.fields.push(Field::Path { key, label: label.into(), value: value.into(), folder });
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn on_submit(mut self, f: impl FnMut(&Values) -> Result<Vec<Action>, String> + Send + 'static) -> Self {
        self.on_submit = Box::new(f);
        self
    }

    fn values(&self) -> Values {
        let mut m = HashMap::new();
        let mut choices = HashMap::new();
        for f in &self.fields {
            match f {
                Field::Text { key, value, .. } | Field::Multiline { key, value, .. } | Field::Pick { key, value, .. } | Field::Path { key, value, .. } => {
                    m.insert(*key, value.clone());
                }
                Field::Check { key, value, .. } => {
                    m.insert(*key, if *value { "1".to_string() } else { "0".to_string() });
                }
                Field::Choice { key, options, value, .. } => {
                    m.insert(*key, options.get(*value).cloned().unwrap_or_default());
                    choices.insert(*key, *value);
                }
                Field::Info(_) => {}
            }
        }
        Values { text: m, choices }
    }

    /// Draws the form. Returns actions when submitted, or `None` while open;
    /// `closed` is set when the form should go away.
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, closed: &mut bool) -> Vec<Action> {
        if self.first_shown.is_none() {
            self.first_shown = Some(self.fingerprint());
        }
        let mut first_text = true;
        ui.set_min_width(440.0);
        for (i, f) in self.fields.iter_mut().enumerate() {
            match f {
                Field::Text { label, value, hint, password, .. } => {
                    ui.label(RichText::new(label.as_str()).color(p.dim));
                    let r = ui.add(crate::widgets::input(value).hint_text(hint.as_str()).password(*password).desired_width(f32::INFINITY));
                    if first_text && !ui.ctx().memory(|m| m.focused().is_some()) {
                        r.request_focus();
                        first_text = false;
                    }
                }
                Field::Multiline { label, value, .. } => {
                    ui.label(RichText::new(label.as_str()).color(p.dim));
                    ui.add(crate::widgets::text_area(value).desired_rows(4).desired_width(f32::INFINITY));
                }
                Field::Check { label, value, .. } => {
                    ui.checkbox(value, label.as_str());
                }
                Field::Choice { key, label, options, value } => {
                    ui.label(RichText::new(label.as_str()).color(p.dim));
                    let cur = options.get(*value).cloned().unwrap_or_default();
                    egui::ComboBox::from_id_salt(*key).selected_text(cur).width(ui.available_width() - 8.0).show_ui(ui, |ui| {
                        for (i, o) in options.iter().enumerate() {
                            ui.selectable_value(value, i, o);
                        }
                    });
                }
                Field::Pick { key, label, options, value } => {
                    ui.label(RichText::new(label.as_str()).color(p.dim));
                    ui.add(crate::widgets::input(value).desired_width(f32::INFINITY));
                    let q = value.clone();
                    let mut scored: Vec<(i32, &String)> = options.iter().filter_map(|o| gitem_core::util::fuzzy_score(&q, o).map(|s| (s, o))).collect();
                    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                    egui::ScrollArea::vertical().id_salt(*key).max_height(120.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (_, o) in scored.into_iter().take(100) {
                            if ui.selectable_label(*o == q, o.as_str()).clicked() {
                                *value = o.clone();
                            }
                        }
                    });
                }
                Field::Info(t) => {
                    ui.label(RichText::new(t.as_str()).color(p.dim));
                }
                Field::Path { label, value, folder, .. } => {
                    ui.label(RichText::new(label.as_str()).color(p.dim));
                    ui.horizontal(|ui| {
                        let browse = ui.add_enabled(!self.picking.busy(), egui::Button::new("Browse…")).clicked();
                        ui.add(crate::widgets::input(value).desired_width(f32::INFINITY));
                        if browse {
                            self.picking_field = i;
                            crate::widgets::pick(&mut self.picking, ui.ctx(), rfd::FileDialog::new(), *folder);
                        }
                        if self.picking_field == i
                            && self.picking.poll()
                            && let Some(Some(path)) = self.picking.value.take()
                        {
                            *value = path.to_string_lossy().replace('\\', "/");
                        }
                    });
                }
            }
            ui.add_space(3.0);
        }
        if let Some(e) = &self.error {
            ui.colored_label(p.danger, e);
        }
        ui.add_space(6.0);
        let mut out = Vec::new();
        let mut submit = false;
        ui.horizontal(|ui| {
            let fill = if self.danger { p.danger } else { p.accent };
            let text = if self.danger { egui::Color32::WHITE } else { p.on_accent };
            let b = egui::Button::new(RichText::new(self.submit.as_str()).color(text)).fill(fill);
            submit = ui.add(b).clicked();
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                *closed = true;
            }
        });
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            submit = true;
        }
        if submit {
            let vals = self.values();
            match (self.on_submit)(&vals) {
                Ok(a) => {
                    out = a;
                    *closed = true;
                }
                Err(e) => self.error = Some(e),
            }
        }
        out
    }
}

fn run(op: GitOp) -> Result<Vec<Action>, String> {
    Ok(vec![Action::Run(op)])
}

fn required(v: &Values, k: &str, what: &str) -> Result<String, String> {
    v.opt(k).ok_or_else(|| format!("{what} is required."))
}

// ---- concrete forms --------------------------------------------------------------

pub fn new_branch(start: Option<String>) -> FormDialog {
    let title = match &start {
        Some(s) => format!("Create branch from {}", gitem_core::util::prefix(s, 24)),
        None => "Create branch".to_string(),
    };
    FormDialog::new(title, "Create")
        .text("name", "Branch name", "")
        .hint("feature/my-work")
        .check("switch", "Switch to it", true)
        .on_submit(move |v| run(ops::branch_create(&required(v, "name", "A name")?, start.as_deref(), v.on("switch"))))
}

pub fn rename_branch(old: String) -> FormDialog {
    FormDialog::new(format!("Rename {old}"), "Rename").text("name", "New name", &old).on_submit(move |v| run(ops::branch_rename(&old, &required(v, "name", "A name")?)))
}

pub fn set_upstream(branch: String, candidates: Vec<String>) -> FormDialog {
    FormDialog::new(format!("Set upstream of {branch}"), "Set upstream")
        .pick("up", "Upstream branch", candidates, "")
        .on_submit(move |v| run(ops::branch_set_upstream(&branch, &required(v, "up", "An upstream")?)))
}

pub fn move_branch(branch: String, refs: Vec<String>) -> FormDialog {
    FormDialog::new(format!("Move {branch} to another commit"), "Move")
        .danger()
        .info("Commits only reachable from the old position may become unreferenced.")
        .pick("to", "Target revision", refs, "")
        .on_submit(move |v| run(ops::branch_force_move(&branch, &required(v, "to", "A target")?)))
}

pub fn new_tag(rev: Option<String>) -> FormDialog {
    FormDialog::new("Create tag", "Create")
        .text("name", "Tag name", "")
        .hint("v1.0.0")
        .multiline("message", "Message (makes an annotated tag)", "")
        .check("sign", "Sign with GPG/SSH", false)
        .on_submit(move |v| {
            let name = required(v, "name", "A name")?;
            run(ops::tag_create(&name, rev.as_deref(), v.opt("message").as_deref(), v.on("sign")))
        })
}

const MERGE_MODES: [(ops::MergeMode, &str); 5] = [
    (ops::MergeMode::Default, "Default (fast-forward when possible)"),
    (ops::MergeMode::NoFf, "Always create a merge commit (--no-ff)"),
    (ops::MergeMode::FfOnly, "Fast-forward only (--ff-only)"),
    (ops::MergeMode::Squash, "Squash (--squash)"),
    (ops::MergeMode::NoCommit, "Merge without committing (--no-commit)"),
];

pub fn merge_form(target: String) -> FormDialog {
    FormDialog::new(format!("Merge {target} into the current branch"), "Merge").choice_of("mode", "Strategy", &MERGE_MODES, 0).on_submit(move |v| run(ops::merge(&target, v.picked("mode", &MERGE_MODES))))
}

pub fn rebase_form(onto: String) -> FormDialog {
    FormDialog::new(format!("Rebase the current branch onto {onto}"), "Rebase")
        .check("autostash", "Stash local changes during the rebase", true)
        .check("merges", "Keep merge commits (--rebase-merges)", false)
        .check("update_refs", "Update dependent branches (--update-refs)", false)
        .on_submit(move |v| {
            run(ops::rebase(&ops::RebaseOpts { onto: onto.clone(), autostash: v.on("autostash"), rebase_merges: v.on("merges"), update_refs: v.on("update_refs"), ..Default::default() }))
        })
}

const RESET_MODES: [(ops::ResetMode, &str); 4] = [
    (ops::ResetMode::Mixed, "Mixed: keep file changes, unstage them"),
    (ops::ResetMode::Soft, "Soft: keep file changes and staging"),
    (ops::ResetMode::Keep, "Keep: reset, but keep local changes"),
    (ops::ResetMode::Hard, "Hard: discard all local changes"),
];

pub fn reset_form(rev: String) -> FormDialog {
    FormDialog::new(format!("Reset the current branch to {}", gitem_core::util::prefix(&rev, 10)), "Reset").danger().choice_of("mode", "Mode", &RESET_MODES, 0).on_submit(move |v| run(ops::reset(&rev, v.picked("mode", &RESET_MODES))))
}

pub fn cherry_pick_form(shas: Vec<String>) -> FormDialog {
    FormDialog::new(format!("Cherry-pick {} commit(s)", shas.len()), "Cherry-pick")
        .check("x", "Record the source commit (-x)", false)
        .check("no_commit", "Apply without committing", false)
        .on_submit(move |v| run(ops::cherry_pick(&shas, v.on("no_commit"), v.on("x"))))
}

pub fn revert_form(sha: String, is_merge: bool) -> FormDialog {
    let mut f = FormDialog::new(format!("Revert {}", gitem_core::util::prefix(&sha, 10)), "Revert").check("no_commit", "Apply without committing", false);
    if is_merge {
        f = f.info("This is a merge commit. The revert is relative to its first parent.");
    }
    f.on_submit(move |v| run(ops::revert(&sha, v.on("no_commit"), is_merge.then_some(1))))
}

pub fn stash_form(paths: Vec<String>) -> FormDialog {
    let scope = if paths.is_empty() { "all changes".to_string() } else { format!("{} file(s)", paths.len()) };
    FormDialog::new(format!("Stash {scope}"), "Stash")
        .text("message", "Message", "")
        .check("untracked", "Include untracked files", true)
        .check("keep_index", "Keep staged changes in the working tree", false)
        .check("staged", "Stash only staged changes", false)
        .on_submit(move |v| {
            run(ops::stash_push(&ops::StashOpts {
                message: v.text("message").trim().to_string(),
                include_untracked: v.on("untracked") && !v.on("staged"),
                keep_index: v.on("keep_index"),
                staged_only: v.on("staged"),
                paths: paths.clone(),
            }))
        })
}

pub fn stash_rename(sel: String, sha: String, current: String) -> FormDialog {
    FormDialog::new(format!("Rename {sel}"), "Rename").text("message", "Message", &current).on_submit(move |v| run(ops::stash_rename(&sel, &sha, &required(v, "message", "A message")?)))
}

pub fn stash_to_branch(sel: String, sha: String) -> FormDialog {
    FormDialog::new(format!("Create branch from {sel}"), "Create").text("name", "Branch name", "").on_submit(move |v| run(ops::only_if_stash_is(ops::stash_branch(&required(v, "name", "A name")?, &sel), &sel, &sha)))
}

fn default_worktree_path(repo: &Repo, leaf: &str) -> String {
    let parent = repo.root.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| repo.root.clone());
    let name = format!("{}.worktrees/{}", repo.name(), leaf.replace('/', "-"));
    parent.join(name).to_string_lossy().replace('\\', "/")
}

pub fn add_worktree(repo: &Repo, refs: Vec<String>, initial: Option<String>) -> FormDialog {
    let init = initial.unwrap_or_default();
    let path = default_worktree_path(repo, if init.is_empty() { "new" } else { &init });
    FormDialog::new("Add worktree", "Add")
        .pick("start", "Branch or commit", refs, &init)
        .text("new_branch", "Create new branch (optional)", "")
        .path("path", "Location", &path, true)
        .check("detach", "Detached HEAD", false)
        .on_submit(move |v| {
            let path = required(v, "path", "A location")?;
            let start = v.opt("start");
            let nb = v.opt("new_branch");
            let op = if nb.is_some() {
                ops::worktree_add(&path, None, nb.as_deref(), start.as_deref(), false)
            } else {
                ops::worktree_add(&path, start.as_deref(), None, None, v.on("detach"))
            };
            run(op)
        })
}

pub fn move_worktree(path: String) -> FormDialog {
    FormDialog::new("Move worktree", "Move").path("to", "New location", &path, true).on_submit(move |v| run(ops::worktree_move(&path, &required(v, "to", "A location")?)))
}

pub fn lock_worktree(path: String) -> FormDialog {
    FormDialog::new("Lock worktree", "Lock").text("reason", "Reason (optional)", "").on_submit(move |v| run(ops::worktree_lock(&path, v.opt("reason").as_deref())))
}

pub fn add_remote() -> FormDialog {
    FormDialog::new("Add remote", "Add")
        .text("name", "Name", "origin")
        .text("url", "URL", "")
        .on_submit(|v| run(ops::remote_add(&required(v, "name", "A name")?, &required(v, "url", "A URL")?)))
}

pub fn edit_remote(name: String, url: String) -> FormDialog {
    FormDialog::new(format!("Edit remote {name}"), "Save").text("name", "Name", &name).text("url", "URL", &url).on_submit(move |v| {
        let new_name = required(v, "name", "A name")?;
        let new_url = required(v, "url", "A URL")?;
        let mut out = Vec::new();
        if new_url != url {
            out.push(Action::Run(ops::remote_set_url(&name, &new_url)));
        }
        if new_name != name {
            out.push(Action::Run(ops::remote_rename(&name, &new_name)));
        }
        Ok(out)
    })
}

pub fn fetch_form(remotes: Vec<String>, prune: bool) -> FormDialog {
    let mut opts = vec!["All remotes".to_string()];
    opts.extend(remotes);
    FormDialog {
        fields: vec![Field::Choice { key: "remote", label: "Remote".into(), options: opts, value: 0 }],
        ..FormDialog::new("Fetch", "Fetch")
    }
    .check("prune", "Prune deleted remote branches", prune)
    .check("tags", "Fetch all tags", false)
    .on_submit(|v| {
        let all = v.index("remote") == 0;
        run(ops::fetch(&ops::FetchOpts { all, remote: (!all).then(|| v.text("remote")), prune: v.on("prune"), tags: v.on("tags") }))
    })
}

const PULL_MODES: [(ops::PullMode, &str); 4] = [(ops::PullMode::Default, "Repository default"), (ops::PullMode::FfOnly, "Fast-forward only"), (ops::PullMode::Rebase, "Rebase"), (ops::PullMode::Merge, "Merge")];

const RECONCILE_MODES: [(ops::PullMode, &str); 2] = [(ops::PullMode::Merge, "Merge: join the two with a merge commit"), (ops::PullMode::Rebase, "Rebase: replay your commits on top of the remote's")];

/// Git won't choose for you between these when both sides have commits and nothing says which.
pub fn pull_reconcile_form() -> FormDialog {
    FormDialog::new("Pull: both sides have new commits", "Pull")
        .info("This branch and its remote each have commits the other doesn't. Merging keeps your commits as they are. Rebasing rewrites them on top of the remote's, as if you'd started from there.")
        .choice_of("mode", "How to bring them together", &RECONCILE_MODES, 0)
        .check("autostash", "Stash local changes first", false)
        .check("remember", "Always do this when pulling in this repository", false)
        .on_submit(|v| {
            let mode = v.picked("mode", &RECONCILE_MODES);
            let mut out = Vec::new();
            if v.on("remember") {
                out.push(Action::Run(ops::config_set("pull.rebase", if mode == ops::PullMode::Rebase { "true" } else { "false" }, "local")));
            }
            out.push(Action::Run(ops::pull(mode, v.on("autostash"))));
            Ok(out)
        })
}

impl FormDialog {
    /// Fill in a field by name, as typing or choosing would. For tests outside this file.
    pub fn set_for_test(&mut self, key: &str, value: &str) {
        for f in &mut self.fields {
            match f {
                Field::Check { key: k, value: v, .. } if *k == key => *v = value == "1",
                Field::Choice { key: k, options, value: v, .. } if *k == key => *v = options.iter().position(|o| o == value).expect("that option"),
                Field::Text { key: k, value: v, .. } if *k == key => *v = value.to_string(),
                _ => {}
            }
        }
    }

    /// What pressing the button would do.
    pub fn submit_for_test(&mut self) -> Result<Vec<Action>, String> {
        let values = self.values();
        (self.on_submit)(&values)
    }
}

pub fn pull_form() -> FormDialog {
    FormDialog::new("Pull", "Pull").choice_of("mode", "How to integrate", &PULL_MODES, 0).check("autostash", "Stash local changes first", false).on_submit(|v| run(ops::pull(v.picked("mode", &PULL_MODES), v.on("autostash"))))
}

pub fn push_form(branch: Option<String>, remotes: Vec<String>, has_upstream: bool, confirm_force: bool) -> FormDialog {
    let default_remote = remotes.iter().position(|r| r == "origin").unwrap_or(0);
    let mut f = FormDialog::new(if has_upstream { "Push" } else { "Publish branch" }, if has_upstream { "Push" } else { "Publish" });
    f.fields.push(Field::Choice { key: "remote", label: "Remote".into(), options: remotes, value: default_remote });
    f.text("branch", "Branch", branch.as_deref().unwrap_or(""))
        .check("upstream", "Set as upstream", !has_upstream)
        .check("lease", "Force with lease", false)
        .check("force", "Force (can overwrite others' work)", false)
        .check("tags", "Include annotated tags", false)
        .check("no_verify", "Skip pre-push hook", false)
        .on_submit(move |v| {
            // Git lets `--force` win, so the lease would quietly stop protecting anything.
            if v.on("lease") && v.on("force") {
                return Err("Choose Force with lease or Force, not both.".to_string());
            }
            let op = ops::push(&ops::PushOpts {
                remote: v.opt("remote"),
                refspec: v.opt("branch"),
                set_upstream: v.on("upstream"),
                force_with_lease: v.on("lease"),
                force: v.on("force"),
                tags: v.on("tags"),
                no_verify: v.on("no_verify"),
            });
            if v.on("force") && confirm_force {
                let (branch, remote) = (v.opt("branch").unwrap_or_else(|| "the current branch".to_string()), v.opt("remote").unwrap_or_else(|| "its remote".to_string()));
                return Ok(vec![Action::Confirm { title: "Force push".into(), body: format!("Force push {branch} to {remote}? This replaces the branch there, including any commits others have pushed."), label: "Force push".into(), op }]);
            }
            run(op)
        })
}

pub fn clone_form() -> FormDialog {
    FormDialog::new("Clone repository", "Clone")
        .text("url", "URL", "")
        .hint("https://github.com/owner/repo.git")
        .path("parent", "Into folder", "", true)
        .text("name", "Folder name (optional)", "")
        .text("branch", "Branch (optional)", "")
        .text("depth", "Depth (optional)", "")
        .check("recursive", "Include submodules", true)
        .on_submit(|v| {
            let url = required(v, "url", "A URL")?;
            let parent = required(v, "parent", "A folder")?;
            let name = v.opt("name").unwrap_or_else(|| url.trim_end_matches('/').rsplit('/').next().unwrap_or("repo").trim_end_matches(".git").to_string());
            let dest = format!("{}/{name}", parent.trim_end_matches('/'));
            let depth = match v.opt("depth") {
                Some(d) => Some(d.parse::<u32>().map_err(|_| "Depth must be a number.".to_string())?),
                None => None,
            };
            Ok(vec![Action::RunGlobal { op: ops::clone_repo(&url, &dest, depth, v.on("recursive"), v.opt("branch").as_deref()), open_after: Some(dest.into()) }])
        })
}

pub fn init_form() -> FormDialog {
    FormDialog::new("Initialize repository", "Initialize").path("path", "Folder", "", true).text("branch", "Initial branch", "main").on_submit(|v| {
        let p = required(v, "path", "A folder")?;
        Ok(vec![Action::RunGlobal { op: ops::init_repo(&p, v.opt("branch").as_deref()), open_after: Some(p.into()) }])
    })
}

pub fn add_submodule() -> FormDialog {
    FormDialog::new("Add submodule", "Add")
        .text("url", "URL", "")
        .text("path", "Path", "")
        .on_submit(|v| run(ops::submodule_add(&required(v, "url", "A URL")?, &required(v, "path", "A path")?)))
}

pub fn apply_patch_form() -> FormDialog {
    FormDialog::new("Apply patch", "Apply")
        .path("file", "Patch file", "", false)
        .check("three", "Three-way merge on failure (--3way)", true)
        .check("am", "It's an email patch: create commits (git am)", false)
        .on_submit(|v| {
            let f = required(v, "file", "A file")?;
            run(if v.on("am") { ops::am_patch_file(&f) } else { ops::apply_patch_file(&f, v.on("three"), false) })
        })
}

pub fn format_patch_form(range: String) -> FormDialog {
    FormDialog::new("Export commits as patch files", "Export")
        .text("range", "Revision range", &range)
        .path("dir", "Output folder", "", true)
        .on_submit(|v| run(ops::format_patch(&required(v, "range", "A range")?, &required(v, "dir", "A folder")?)))
}

pub fn archive_form(rev: String) -> FormDialog {
    FormDialog::new(format!("Archive {}", gitem_core::util::prefix(&rev, 12)), "Create").choice("format", "Format", &["zip", "tar", "tar.gz"], 0).path("path", "Output file", "", false).on_submit(move |v| run(ops::archive(&rev, &required(v, "path", "A path")?, &v.text("format"))))
}

pub fn note_form(sha: String, current: String) -> FormDialog {
    FormDialog::new("Edit commit note", "Save").multiline("note", "Note", &current).on_submit(move |v| {
        let t = v.text("note");
        if t.trim().is_empty() {
            run(ops::note_remove(&sha))
        } else {
            run(ops::note_set(&sha, &t))
        }
    })
}

pub fn bisect_start_form(bad: String, refs: Vec<String>) -> FormDialog {
    FormDialog::new("Start bisect", "Start")
        .text("bad", "Known bad revision", &bad)
        .pick("good", "Known good revision", refs, "")
        .on_submit(|v| run(ops::bisect_start(&required(v, "bad", "A bad revision")?, &required(v, "good", "A good revision")?)))
}

pub fn raw_config_form(scope: &'static str) -> FormDialog {
    FormDialog::new(format!("Set {scope} config value"), "Set")
        .text("key", "Key", "")
        .hint("section.name")
        .text("value", "Value", "")
        .on_submit(move |v| run(ops::config_set(&required(v, "key", "A key")?, &v.text("value"), scope)))
}

pub fn lfs_track_form() -> FormDialog {
    FormDialog::new("Track with Git LFS", "Track").text("pattern", "Pattern", "").hint("*.psd").on_submit(|v| run(ops::lfs_track(&required(v, "pattern", "A pattern")?)))
}

/// Write a todo list to a temp file and start the rebase with it.
pub fn start_interactive_rebase(repo: &Repo, base: Option<&str>, items: &[gitem_core::rebase::TodoItem], o: ops::RebaseOpts) -> Result<GitOp, String> {
    let helper = repo.git.helper.clone().ok_or("gitem cannot locate its own executable.")?;
    // Two rebases can start in the same second, in different repositories of one app.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("gitem-todo-{}-{}-{n}.txt", std::process::id(), gitem_core::util::now_ts()));
    std::fs::write(&path, render_todo(items)).map_err(|e| e.to_string())?;
    let mut o = o;
    o.onto = base.unwrap_or_default().to_string();
    let mut op = ops::rebase_interactive(&o, base.is_none(), &path, &helper);
    op.remove_after = vec![path];
    Ok(op)
}

/// Commits a rebase from `base` would touch, oldest first.
pub fn commits_since(repo: &Repo, base: Option<&str>) -> Result<Vec<Commit>, String> {
    let q = gitem_core::LogQuery { revs: vec![base.map(|b| format!("{b}..HEAD")).unwrap_or_else(|| "HEAD".to_string())], no_merges: true, ..Default::default() };
    let mut c = repo.log(&q).map_err(|e| e.to_string())?;
    c.reverse();
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fill a field the way the user would: a check takes "1" or "0", a choice takes
    /// the label of its option, and anything else takes text.
    fn set(form: &mut FormDialog, k: &str, input: &str) {
        for f in &mut form.fields {
            match f {
                Field::Text { key, value, .. } | Field::Multiline { key, value, .. } | Field::Pick { key, value, .. } | Field::Path { key, value, .. } if *key == k => {
                    *value = input.to_string();
                    return;
                }
                Field::Check { key, value, .. } if *key == k => {
                    *value = input == "1";
                    return;
                }
                Field::Choice { key, options, value, .. } if *key == k => {
                    *value = options.iter().position(|o| o == input).unwrap_or_else(|| panic!("{k} has no option {input:?}: {options:?}"));
                    return;
                }
                _ => {}
            }
        }
        panic!("the form has no field {k}");
    }

    fn step_text(s: &ops::Step) -> String {
        let quoted: Vec<String> = s.args.iter().map(|a| if a.contains(' ') || a.is_empty() { format!("'{a}'") } else { a.clone() }).collect();
        let stdin = s.stdin.as_ref().map(|i| format!(" <<< {}", String::from_utf8_lossy(i))).unwrap_or_default();
        format!("{}{stdin}", quoted.join(" "))
    }

    fn op_text(op: &GitOp) -> String {
        op.steps.iter().map(step_text).collect::<Vec<_>>().join(" && ")
    }

    /// What the submitted form would run, as exact arguments.
    fn outcome(actions: &[Action]) -> String {
        let shown: Vec<String> = actions
            .iter()
            .map(|a| match a {
                Action::Run(op) => op_text(op),
                Action::Confirm { op, .. } => format!("ask first: {}", op_text(op)),
                Action::RunGlobal { op, open_after } => format!("{} then open {}", op_text(op), open_after.as_ref().map(|p| p.display().to_string()).unwrap_or_default()),
                _ => panic!("the forms here only run git"),
            })
            .collect();
        shown.join(" ; ")
    }

    fn submit(form: &mut FormDialog) -> Result<Vec<Action>, String> {
        let values = form.values();
        (form.on_submit)(&values)
    }

    fn check(what: &str, mut form: FormDialog, inputs: &[(&str, &str)], expected: &str) {
        for (k, input) in inputs {
            set(&mut form, k, input);
        }
        let got = submit(&mut form).unwrap_or_else(|e| panic!("{what}: refused: {e}"));
        assert_eq!(outcome(&got), expected, "{what}");
    }

    fn s(x: &str) -> String {
        x.to_string()
    }

    fn sv(x: &[&str]) -> Vec<String> {
        x.iter().map(|y| y.to_string()).collect()
    }

    #[test]
    fn branch_forms() {
        check("switch to new", new_branch(None), &[("name", "feature/x")], "switch -c feature/x");
        check("don't switch", new_branch(None), &[("name", "feature/x"), ("switch", "0")], "branch feature/x");
        check("from a commit", new_branch(Some(s("abc"))), &[("name", "x")], "switch -c x abc");
        check("from a commit, stay", new_branch(Some(s("abc"))), &[("name", "x"), ("switch", "0")], "branch x abc");
        check("rename", rename_branch(s("old")), &[("name", "new")], "branch -m old new");
        check("upstream", set_upstream(s("x"), sv(&["origin/x"])), &[("up", "origin/x")], "branch --set-upstream-to=origin/x x");
        check("move", move_branch(s("x"), vec![]), &[("to", "abc")], "branch -f x abc");
        assert!(move_branch(s("x"), vec![]).danger);
    }

    #[test]
    fn tag_form() {
        check("lightweight", new_tag(None), &[("name", "v1")], "tag v1");
        check("annotated, at a commit", new_tag(Some(s("abc"))), &[("name", "v1"), ("message", "a note")], "tag -a -m 'a note' v1 abc");
        check("signed", new_tag(None), &[("name", "v1"), ("sign", "1")], "tag -s -m v1 v1");
        check("signed with a message", new_tag(None), &[("name", "v1"), ("sign", "1"), ("message", "m")], "tag -s -m m v1");
    }

    #[test]
    fn merge_form_runs_each_mode() {
        check("opens on the default", merge_form(s("x")), &[], "merge --no-edit x");
        for (mode, flag) in [("Always create a merge commit (--no-ff)", "--no-ff "), ("Fast-forward only (--ff-only)", "--ff-only "), ("Squash (--squash)", "--squash "), ("Merge without committing (--no-commit)", "--no-commit "), ("Default (fast-forward when possible)", "")] {
            check(mode, merge_form(s("x")), &[("mode", mode)], &format!("merge --no-edit {flag}x"));
        }
    }

    #[test]
    fn reset_form_runs_each_mode() {
        check("opens on mixed, the mode that loses nothing", reset_form(s("abc")), &[], "reset --mixed abc --");
        for (mode, flag) in [("Mixed: keep file changes, unstage them", "--mixed"), ("Soft: keep file changes and staging", "--soft"), ("Keep: reset, but keep local changes", "--keep"), ("Hard: discard all local changes", "--hard")] {
            check(mode, reset_form(s("abc")), &[("mode", mode)], &format!("reset {flag} abc --"));
        }
        assert!(reset_form(s("abc")).danger);
    }

    /// The labels are text for people. Rewording or translating one must not change the command.
    #[test]
    fn a_reworded_choice_still_runs_the_same_command() {
        let relabel = |form: &mut FormDialog, labels: &[&str]| {
            let Some(Field::Choice { options, .. }) = form.fields.iter_mut().find(|f| matches!(f, Field::Choice { .. })) else { panic!("no choice") };
            assert_eq!(options.len(), labels.len());
            *options = sv(labels);
        };
        // Every label now reads like some other mode, which would fool a test of the text.
        let mut f = reset_form(s("abc"));
        relabel(&mut f, &["Hard: A", "Hard: B", "Soft: C", "Soft: D"]);
        for (label, flag) in [("Hard: A", "--mixed"), ("Hard: B", "--soft"), ("Soft: C", "--keep"), ("Soft: D", "--hard")] {
            set(&mut f, "mode", label);
            assert_eq!(outcome(&submit(&mut f).unwrap()), format!("reset {flag} abc --"));
        }
        let mut f = merge_form(s("x"));
        relabel(&mut f, &["squash", "squash no-ff", "no-commit", "Birle\u{15f}tir", "ff-only"]);
        for (label, flag) in [("squash", ""), ("squash no-ff", "--no-ff "), ("no-commit", "--ff-only "), ("Birle\u{15f}tir", "--squash "), ("ff-only", "--no-commit ")] {
            set(&mut f, "mode", label);
            assert_eq!(outcome(&submit(&mut f).unwrap()), format!("merge --no-edit {flag}x"));
        }
        let mut f = pull_form();
        relabel(&mut f, &["Rebase", "Merge", "Fast-forward only", "Repository default"]);
        for (label, flag) in [("Rebase", ""), ("Merge", " --ff-only"), ("Fast-forward only", " --rebase"), ("Repository default", " --no-rebase")] {
            set(&mut f, "mode", label);
            assert_eq!(outcome(&submit(&mut f).unwrap()), format!("pull --progress{flag}"));
        }
    }

    #[test]
    fn rebase_cherry_pick_and_revert_forms() {
        check("stashes by default", rebase_form(s("main")), &[], "rebase --autostash main");
        check("nothing ticked", rebase_form(s("main")), &[("autostash", "0")], "rebase main");
        check("everything ticked", rebase_form(s("main")), &[("merges", "1"), ("update_refs", "1")], "rebase --autostash --rebase-merges --update-refs main");
        check("pick", cherry_pick_form(sv(&["a", "b"])), &[], "cherry-pick a b");
        check("pick with -x", cherry_pick_form(sv(&["a"])), &[("x", "1")], "cherry-pick -x a");
        check("pick without committing", cherry_pick_form(sv(&["a"])), &[("no_commit", "1")], "cherry-pick --no-commit a");
        check("revert", revert_form(s("abc"), false), &[], "revert --no-edit abc");
        check("revert a merge", revert_form(s("abc"), true), &[], "revert --no-edit -m 1 abc");
        check("revert without committing", revert_form(s("abc"), false), &[("no_commit", "1")], "revert --no-edit --no-commit abc");
    }

    #[test]
    fn stash_forms() {
        check("defaults", stash_form(vec![]), &[], "stash push --include-untracked");
        check("message", stash_form(vec![]), &[("message", " wip now "), ("untracked", "0")], "stash push -m 'wip now'");
        check("keep index", stash_form(vec![]), &[("keep_index", "1")], "stash push --include-untracked --keep-index");
        check("staged only leaves untracked out", stash_form(vec![]), &[("staged", "1")], "stash push --staged");
        check("named files", stash_form(sv(&["a b", "c"])), &[("untracked", "0")], "stash push -- ':(literal)a b' :(literal)c");
        check("rename", stash_rename(s("stash@{1}"), s("abc"), s("old")), &[("message", "new")], "stash store -m new abc && stash drop stash@{2}");
        check("to a branch", stash_to_branch(s("stash@{0}"), s("abc")), &[("name", "x")], "stash branch x stash@{0}");
    }

    #[test]
    fn worktree_forms() {
        let d = tempfile::tempdir().unwrap();
        let git = gitem_core::GitExe::discover(None).unwrap();
        git.run(Some(d.path()), &["init", "-q"]).unwrap();
        let repo = Repo::open(git, d.path()).unwrap();
        check("existing branch", add_worktree(&repo, vec![], Some(s("x"))), &[("path", "/w")], "worktree add /w x");
        check("new branch", add_worktree(&repo, vec![], Some(s("main"))), &[("path", "/w"), ("new_branch", "n")], "worktree add -b n /w main");
        check("new branch wins over detach", add_worktree(&repo, vec![], None), &[("path", "/w"), ("new_branch", "n"), ("detach", "1")], "worktree add -b n /w");
        check("detached", add_worktree(&repo, vec![], Some(s("abc"))), &[("path", "/w"), ("detach", "1")], "worktree add --detach /w abc");
        let leaf = add_worktree(&repo, vec![], Some(s("feature/x")));
        let Field::Path { value, .. } = &leaf.fields[2] else { panic!("the third field is the location") };
        assert!(value.ends_with(".worktrees/feature-x"), "a branch's slash doesn't make a folder: {value}");
        check("move", move_worktree(s("/a")), &[("to", "/b")], "worktree move /a /b");
        check("lock", lock_worktree(s("/a")), &[], "worktree lock /a");
        check("lock with a reason", lock_worktree(s("/a")), &[("reason", "on usb")], "worktree lock --reason 'on usb' /a");
    }

    #[test]
    fn remote_forms() {
        check("add", add_remote(), &[("url", "u")], "remote add -- origin u");
        let edit = || edit_remote(s("o"), s("u"));
        check("no change runs nothing", edit(), &[], "");
        check("url", edit(), &[("url", "v")], "remote set-url -- o v");
        check("name", edit(), &[("name", "p")], "remote rename o p");
        // In this order: set-url has to find the remote under its old name.
        check("both", edit(), &[("name", "p"), ("url", "v")], "remote set-url -- o v ; remote rename o p");
        check("all remotes", fetch_form(sv(&["origin"]), false), &[], "fetch --progress --all");
        check("one remote", fetch_form(sv(&["origin"]), false), &[("remote", "origin")], "fetch --progress -- origin");
        check("prune from settings", fetch_form(vec![], true), &[], "fetch --progress --all --prune");
        check("tags", fetch_form(vec![], false), &[("tags", "1")], "fetch --progress --all --tags");
    }

    #[test]
    fn pull_form_runs_each_mode() {
        check("default", pull_form(), &[], "pull --progress");
        check("ff", pull_form(), &[("mode", "Fast-forward only")], "pull --progress --ff-only");
        check("rebase", pull_form(), &[("mode", "Rebase"), ("autostash", "1")], "pull --progress --rebase --autostash");
        check("merge", pull_form(), &[("mode", "Merge")], "pull --progress --no-rebase");
    }

    #[test]
    fn push_form_options() {
        let remotes = || sv(&["fork", "origin"]);
        let main = || Some(s("main"));
        check("prefers origin", push_form(main(), remotes(), true, true), &[], "push --progress -- origin main");
        check("publishes a new branch", push_form(main(), remotes(), false, true), &[("remote", "fork")], "push --progress --set-upstream -- fork main");
        check("lease goes straight through", push_form(main(), remotes(), true, true), &[("lease", "1")], "push --progress --force-with-lease --force-if-includes -- origin main");
        check("force asks", push_form(main(), remotes(), true, true), &[("force", "1")], "ask first: push --progress --force -- origin main");
        check("force, confirmations off", push_form(main(), remotes(), true, false), &[("force", "1")], "push --progress --force -- origin main");
        check("tags and no hook", push_form(main(), remotes(), true, true), &[("tags", "1"), ("no_verify", "1")], "push --progress --follow-tags --no-verify -- origin main");
        let mut both = push_form(main(), remotes(), true, true);
        set(&mut both, "lease", "1");
        set(&mut both, "force", "1");
        assert!(submit(&mut both).is_err(), "force would quietly turn the lease off");
    }

    #[test]
    fn clone_and_init_forms() {
        check("name from the url", clone_form(), &[("url", "https://x.invalid/o/r.git"), ("parent", "/p/")], "clone --progress --recurse-submodules -- https://x.invalid/o/r.git /p/r then open /p/r");
        check("every option", clone_form(), &[("url", "u"), ("parent", "/p"), ("name", "n"), ("branch", "b"), ("depth", "3"), ("recursive", "0")], "clone --progress --depth=3 --branch b -- u /p/n then open /p/n");
        let mut bad_depth = clone_form();
        for (k, input) in [("url", "u"), ("parent", "/p"), ("depth", "lots")] {
            set(&mut bad_depth, k, input);
        }
        assert!(submit(&mut bad_depth).is_err());
        check("init", init_form(), &[("path", "/p")], "init --initial-branch=main /p then open /p");
        check("init, git's default branch", init_form(), &[("path", "/p"), ("branch", "")], "init /p then open /p");
    }

    #[test]
    fn patch_note_bisect_and_other_forms() {
        check("submodule", add_submodule(), &[("url", "u"), ("path", "p")], "submodule add -- u p");
        check("patch, three-way if plain fails", apply_patch_form(), &[("file", "f")], "apply --whitespace=nowarn f && apply --whitespace=nowarn --3way f");
        check("patch", apply_patch_form(), &[("file", "f"), ("three", "0")], "apply --whitespace=nowarn f");
        check("email patch", apply_patch_form(), &[("file", "f"), ("am", "1")], "am --3way f");
        check("export", format_patch_form(s("a..b")), &[("dir", "/o")], "format-patch -o /o a..b --");
        check("archive, zip by default", archive_form(s("abc")), &[("path", "o.zip")], "archive --format=zip -o o.zip abc");
        check("archive tar.gz", archive_form(s("abc")), &[("path", "o"), ("format", "tar.gz")], "archive --format=tar.gz -o o abc");
        check("note", note_form(s("abc"), s("")), &[("note", "hello")], "notes add -f -F - abc <<< hello");
        check("emptying a note removes it", note_form(s("abc"), s("old")), &[("note", "  ")], "notes remove abc");
        check("bisect", bisect_start_form(s("bad1"), vec![]), &[("good", "good1")], "bisect start bad1 good1");
        check("config", raw_config_form("local"), &[("key", "a.b"), ("value", "c d")], "config --local --replace-all -- a.b 'c d'");
        check("global config", raw_config_form("global"), &[("key", "a.b"), ("value", "")], "config --global --replace-all -- a.b ''");
        check("lfs", lfs_track_form(), &[("pattern", "*.psd")], "lfs track *.psd && add .gitattributes");
    }

    /// Each plan needs its own file. Git reads it only when the rebase gets going,
    /// so with a shared one, a second rebase started in the same second, in another
    /// open repository, would hand git the wrong commits.
    #[test]
    fn rebase_plans_started_together_keep_their_own_files() {
        let d = tempfile::tempdir().unwrap();
        let git = gitem_core::GitExe::discover(None).unwrap();
        git.run(Some(d.path()), &["init", "-q"]).unwrap();
        let repo = Repo::open(git, d.path()).unwrap();
        let item = |sha: &str| gitem_core::rebase::TodoItem { action: gitem_core::rebase::RebaseAction::Pick, sha: sha.to_string(), text: "x".to_string() };
        let todo_of = |op: &GitOp| {
            let editor = &op.steps[0].env.iter().find(|(k, _)| k == "GIT_SEQUENCE_EDITOR").unwrap().1;
            editor.rsplit('"').nth(1).unwrap().to_string()
        };
        let first = start_interactive_rebase(&repo, None, &[item("aaa")], ops::RebaseOpts::default()).unwrap();
        let second = start_interactive_rebase(&repo, None, &[item("bbb")], ops::RebaseOpts::default()).unwrap();
        let (a, b) = (todo_of(&first), todo_of(&second));
        assert_ne!(a, b);
        assert!(std::fs::read_to_string(&a).unwrap().contains("aaa"));
        assert!(std::fs::read_to_string(&b).unwrap().contains("bbb"));
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    /// Submitting with a required field left empty, or only spaces, runs nothing.
    #[test]
    fn required_fields_are_refused() {
        let cases: Vec<(&str, FormDialog, &str)> = vec![
            ("new branch", new_branch(None), "name"),
            ("rename", rename_branch(s("x")), "name"),
            ("upstream", set_upstream(s("x"), vec![]), "up"),
            ("move", move_branch(s("x"), vec![]), "to"),
            ("tag", new_tag(None), "name"),
            ("stash rename", stash_rename(s("a"), s("b"), s("c")), "message"),
            ("stash branch", stash_to_branch(s("a"), s("b")), "name"),
            ("move worktree", move_worktree(s("a")), "to"),
            ("remote", add_remote(), "url"),
            ("clone", clone_form(), "url"),
            ("init", init_form(), "path"),
            ("patch", apply_patch_form(), "file"),
            ("export", format_patch_form(s("r")), "dir"),
            ("archive", archive_form(s("r")), "path"),
            ("bisect", bisect_start_form(s("b"), vec![]), "good"),
            ("config", raw_config_form("local"), "key"),
            ("lfs", lfs_track_form(), "pattern"),
        ];
        for (what, mut form, field) in cases {
            set(&mut form, field, "   ");
            assert!(submit(&mut form).is_err(), "{what} ran with nothing in {field}");
        }
    }
}
