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

pub struct Values(HashMap<&'static str, String>);

impl Values {
    pub fn text(&self, k: &str) -> String {
        self.0.get(k).cloned().unwrap_or_default()
    }
    pub fn opt(&self, k: &str) -> Option<String> {
        self.0.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    pub fn on(&self, k: &str) -> bool {
        self.0.get(k).is_some_and(|v| v == "1")
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
}

impl FormDialog {
    pub fn new(title: impl Into<String>, submit: impl Into<String>) -> Self {
        Self { title: title.into(), fields: Vec::new(), submit: submit.into(), danger: false, on_submit: Box::new(|_| Ok(Vec::new())), error: None }
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
                }
                Field::Info(_) => {}
            }
        }
        Values(m)
    }

    /// Draws the form. Returns actions when submitted, or `None` while open;
    /// `closed` is set when the form should go away.
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, closed: &mut bool) -> Vec<Action> {
        let mut first_text = true;
        ui.set_min_width(440.0);
        for f in &mut self.fields {
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
                        let browse = ui.button("Browse…").clicked();
                        ui.add(crate::widgets::input(value).desired_width(f32::INFINITY));
                        if browse {
                            let d = rfd::FileDialog::new();
                            let picked = if *folder { d.pick_folder() } else { d.pick_file() };
                            if let Some(path) = picked {
                                *value = path.to_string_lossy().replace('\\', "/");
                            }
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
            let b = egui::Button::new(RichText::new(self.submit.as_str()).color(p.on_accent)).fill(fill);
            submit = ui.add(b).clicked();
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                *closed = true;
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl) {
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

pub fn merge_form(target: String) -> FormDialog {
    let modes = ["Default (fast-forward when possible)", "Always create a merge commit (--no-ff)", "Fast-forward only (--ff-only)", "Squash (--squash)", "Merge without committing (--no-commit)"];
    FormDialog::new(format!("Merge {target} into the current branch"), "Merge").choice("mode", "Strategy", &modes, 0).on_submit(move |v| {
        let m = v.text("mode");
        let mode = if m.contains("no-ff") {
            ops::MergeMode::NoFf
        } else if m.contains("ff-only") {
            ops::MergeMode::FfOnly
        } else if m.contains("squash") {
            ops::MergeMode::Squash
        } else if m.contains("no-commit") {
            ops::MergeMode::NoCommit
        } else {
            ops::MergeMode::Default
        };
        run(ops::merge(&target, mode))
    })
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

pub fn reset_form(rev: String) -> FormDialog {
    let modes = ["Mixed: keep file changes, unstage them", "Soft: keep file changes and staging", "Keep: reset, but keep local changes", "Hard: discard all local changes"];
    FormDialog::new(format!("Reset the current branch to {}", gitem_core::util::prefix(&rev, 10)), "Reset").danger().choice("mode", "Mode", &modes, 0).on_submit(move |v| {
        let m = v.text("mode");
        let mode = match m.split(':').next().unwrap_or("") {
            "Soft" => ops::ResetMode::Soft,
            "Hard" => ops::ResetMode::Hard,
            "Keep" => ops::ResetMode::Keep,
            _ => ops::ResetMode::Mixed,
        };
        run(ops::reset(&rev, mode))
    })
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

pub fn stash_to_branch(sel: String) -> FormDialog {
    FormDialog::new(format!("Create branch from {sel}"), "Create").text("name", "Branch name", "").on_submit(move |v| run(ops::stash_branch(&required(v, "name", "A name")?, &sel)))
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
        let r = v.text("remote");
        run(ops::fetch(&ops::FetchOpts { all: r == "All remotes", remote: (r != "All remotes").then_some(r), prune: v.on("prune"), tags: v.on("tags") }))
    })
}

pub fn pull_form() -> FormDialog {
    let modes = ["Repository default", "Fast-forward only", "Rebase", "Merge"];
    FormDialog::new("Pull", "Pull").choice("mode", "How to integrate", &modes, 0).check("autostash", "Stash local changes first", false).on_submit(|v| {
        let mode = match v.text("mode").as_str() {
            "Fast-forward only" => ops::PullMode::FfOnly,
            "Rebase" => ops::PullMode::Rebase,
            "Merge" => ops::PullMode::Merge,
            _ => ops::PullMode::Default,
        };
        run(ops::pull(mode, v.on("autostash")))
    })
}

pub fn push_form(branch: Option<String>, remotes: Vec<String>, has_upstream: bool) -> FormDialog {
    let default_remote = remotes.iter().position(|r| r == "origin").unwrap_or(0);
    let mut f = FormDialog::new(if has_upstream { "Push" } else { "Publish branch" }, if has_upstream { "Push" } else { "Publish" });
    f.fields.push(Field::Choice { key: "remote", label: "Remote".into(), options: remotes, value: default_remote });
    f.text("branch", "Branch", branch.as_deref().unwrap_or(""))
        .check("upstream", "Set as upstream", !has_upstream)
        .check("lease", "Force with lease", false)
        .check("force", "Force (can overwrite others' work)", false)
        .check("tags", "Include annotated tags", false)
        .check("no_verify", "Skip pre-push hook", false)
        .on_submit(|v| {
            run(ops::push(&ops::PushOpts {
                remote: v.opt("remote"),
                refspec: v.opt("branch"),
                set_upstream: v.on("upstream"),
                force_with_lease: v.on("lease"),
                force: v.on("force"),
                tags: v.on("tags"),
                no_verify: v.on("no_verify"),
            }))
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
    let path = std::env::temp_dir().join(format!("gitem-todo-{}-{}.txt", std::process::id(), gitem_core::util::now_ts()));
    std::fs::write(&path, render_todo(items)).map_err(|e| e.to_string())?;
    let mut o = o;
    o.onto = base.unwrap_or_default().to_string();
    Ok(ops::rebase_interactive(&o, base.is_none(), &path, &helper))
}

/// Commits a rebase from `base` would touch, oldest first.
pub fn commits_since(repo: &Repo, base: Option<&str>) -> Result<Vec<Commit>, String> {
    let q = gitem_core::LogQuery { revs: vec![base.map(|b| format!("{b}..HEAD")).unwrap_or_else(|| "HEAD".to_string())], no_merges: true, ..Default::default() };
    let mut c = repo.log(&q).map_err(|e| e.to_string())?;
    c.reverse();
    Ok(c)
}
