//! Context menus for commits, refs, files, stashes and worktrees. Shared by the
//! graph, sidebar, and lists so the same commands appear everywhere.
use egui::Ui;
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::DiffSource;

use crate::action::{Action, Cx};
use crate::forms;
use crate::views::Tab;

fn item(ui: &mut Ui, label: &str) -> bool {
    let c = ui.button(crate::i18n::t(label)).clicked();
    if c {
        ui.close();
    }
    c
}

pub fn commit_menu(ui: &mut Ui, cx: &mut Cx, c: &Commit) {
    let sha = c.sha.clone();
    let short = c.short().to_string();
    if item(ui, "Show details") {
        cx.inspect(&sha);
    }
    if item(ui, "Open changes") {
        cx.diff(DiffSource::Commit(sha.clone()), None);
    }
    if item(ui, "Compare with working tree") {
        cx.diff(DiffSource::WorkingVs(sha.clone()), None);
    }
    if item(ui, "Compare with HEAD") {
        cx.open(Tab::compare(&sha, "HEAD"));
    }
    if item(ui, "Browse files at this commit") {
        cx.open(Tab::tree(&sha));
    }
    ui.separator();
    if item(ui, "Check out (detached)…") {
        cx.confirm("Check out commit", &format!("Check out {short} with a detached HEAD?"), "Check out", ops::checkout_detached(&sha));
    }
    if item(ui, "Create branch here…") {
        cx.form(forms::new_branch(Some(sha.clone())));
    }
    if item(ui, "Create tag here…") {
        cx.form(forms::new_tag(Some(sha.clone())));
    }
    if item(ui, "Create worktree here…") {
        let refs = cx.data.ref_names();
        cx.form(forms::add_worktree(cx.repo, refs, Some(sha.clone())));
    }
    ui.separator();
    let is_head = cx.status().is_some_and(|s| s.head_sha == sha);
    if is_head && item(ui, "Undo commit") {
        cx.run(ops::undo_commit());
    }
    if item(ui, "Reword…") {
        if is_head {
            let msg = c.message();
            cx.form(forms::FormDialog::new("Reword commit", "Save").multiline("message", "Message", &msg).on_submit(|v| {
                let m = v.opt("message").ok_or("A message is required.")?;
                Ok(vec![Action::Run(ops::reword_head(&m))])
            }));
        } else {
            open_plan(cx, c, gitem_core::rebase::RebaseAction::Reword, false);
        }
    }
    if item(ui, "Squash into previous…") {
        open_plan(cx, c, gitem_core::rebase::RebaseAction::Squash, true);
    }
    if item(ui, "Drop…") {
        open_plan(cx, c, gitem_core::rebase::RebaseAction::Drop, false);
    }
    if let (Some(branch), Some(remote)) = (cx.current_branch(), cx.data.forge_remote.clone().or_else(|| cx.data.remotes().first().map(|r| r.name.clone())))
        && item(ui, "Push up to this commit…") {
            cx.confirm("Push to commit", &format!("Push {branch} to {remote} only up to {short}?"), "Push", ops::push_to_commit(&remote, &sha, &branch));
        }
    if item(ui, "Squash fixups from here")
        && let Some(p) = c.parents.first() {
            cx.run(ops::squash_fixups(p));
        }
    if item(ui, "Recompose from here…") {
        recompose(cx, &sha);
    }
    if item(ui, "Cherry-pick…") {
        cx.form(forms::cherry_pick_form(vec![sha.clone()]));
    }
    if item(ui, "Revert…") {
        cx.form(forms::revert_form(sha.clone(), c.is_merge()));
    }
    if item(ui, "Reset current branch to here…") {
        cx.form(forms::reset_form(sha.clone()));
    }
    if item(ui, "Interactive rebase from here…") {
        match c.parents.first() {
            Some(base) => cx.open(Tab::rebase(Some(base.clone()))),
            None => cx.open(Tab::Rebase(Box::new(crate::views::rebase::RebaseView::from_root()))),
        }
    }
    if item(ui, "Rebase current branch onto this…") {
        cx.form(forms::rebase_form(sha.clone()));
    }
    if item(ui, "Merge into current branch…") {
        cx.form(forms::merge_form(sha.clone()));
    }
    if item(ui, "Create fixup commit") {
        cx.run(ops::fixup_commit(&sha));
    }
    ui.separator();
    if item(ui, "Edit note…") {
        let current = cx.repo.commit_note(&sha).unwrap_or_default();
        cx.form(forms::note_form(sha.clone(), current));
    }
    if item(ui, "Save as patch…") {
        cx.open(Tab::Patches(Box::new(crate::views::patches::PatchesView::create_from(DiffSource::Commit(sha.clone()), &c.subject))));
    }
    if item(ui, "Export with format-patch…") {
        cx.form(forms::format_patch_form(format!("{sha}~1..{sha}")));
    }
    if item(ui, "Create archive…") {
        cx.form(forms::archive_form(sha.clone()));
    }
    if item(ui, "Start bisect with this as bad…") {
        let refs = cx.data.ref_names();
        cx.form(forms::bisect_start_form(sha.clone(), refs));
    }
    ui.separator();
    if item(ui, "Copy SHA") {
        cx.copy(sha.clone());
    }
    if item(ui, "Copy short SHA") {
        cx.copy(short);
    }
    if item(ui, "Copy message") {
        cx.copy(c.message());
    }
    if item(ui, "Copy gitem link") {
        let d = gitem_core::forge::DeepLink { repo: cx.repo.root.to_string_lossy().replace('\\', "/"), commit: Some(sha.clone()), ..Default::default() };
        cx.copy(d.to_url());
    }
    if item(ui, "Copy as patch") {
        match cx.repo.diff_text(&DiffSource::Commit(sha.clone())) {
            Ok(p) => cx.copy(p),
            Err(e) => cx.error(e.to_string()),
        }
    }
    if let Some(f) = cx.forge() {
        let url = f.commit_url(&sha);
        if item(ui, "Open on remote") {
            cx.url(url.clone());
        }
        if item(ui, "Copy remote link") {
            cx.copy(url);
        }
    }
}

/// Open the rebase editor with one commit's action already chosen, for review.
fn open_plan(cx: &mut Cx, c: &Commit, action: gitem_core::rebase::RebaseAction, needs_previous: bool) {
    use crate::views::rebase::RebaseView;
    // A squash also rewrites the commit it folds into, so the plan starts one earlier.
    let base = if needs_previous { c.parents.first().map(|p| format!("{p}~1")).filter(|b| cx.repo.resolve(b).is_ok()) } else { c.parents.first().cloned() };
    let mut view = match base {
        Some(b) => RebaseView::new(Some(b)),
        None => RebaseView::from_root(),
    };
    view.preset(&c.sha, action);
    cx.open(Tab::Rebase(Box::new(view)));
}

fn recompose(cx: &mut Cx, base: &str) {
    cx.confirm("Recompose", &format!("Undo the commits after {}, keeping all their changes in the working tree, then arrange them into new commits? Commit messages are not kept. The reflog can restore them.", gitem_core::util::prefix(base, 10)), "Recompose", ops::recompose_reset(base));
    cx.open(Tab::Composer(Box::default()));
}

pub fn branch_menu(ui: &mut Ui, cx: &mut Cx, b: &Branch) {
    let name = b.name.clone();
    if !b.is_current {
        if b.is_remote {
            if item(ui, "Check out as local branch") {
                cx.run(ops::checkout_remote(&name));
            }
        } else if item(ui, "Switch to") {
            cx.run(ops::checkout(&name));
        }
        if item(ui, "Merge into current…") {
            cx.form(forms::merge_form(name.clone()));
        }
        if item(ui, "Rebase current onto this…") {
            cx.form(forms::rebase_form(name.clone()));
        }
        if item(ui, "Interactive rebase onto this…") {
            cx.open(Tab::rebase(Some(name.clone())));
        }
        ui.separator();
    }
    if item(ui, "Focus in graph") {
        cx.out.push(Action::GraphFocus(name.clone()));
    }
    if item(ui, "Hide in graph") {
        cx.out.push(Action::GraphHide(name.clone()));
    }
    let fav = cx.settings.favorite_branches.contains(&name);
    if item(ui, if fav { "Remove from favorites" } else { "Add to favorites" }) {
        let n = name.clone();
        cx.out.push(Action::Settings(Box::new(move |s| {
            if fav {
                s.favorite_branches.retain(|x| *x != n);
            } else {
                s.favorite_branches.push(n);
            }
        })));
    }
    ui.separator();
    if item(ui, "Compare with HEAD") {
        cx.open(Tab::compare("HEAD", &name));
    }
    if item(ui, "Compare with working tree") {
        cx.diff(DiffSource::WorkingVs(name.clone()), None);
    }
    if !b.is_current {
        if item(ui, "Open all changes with common base") {
            cx.diff(DiffSource::MergeBase("HEAD".to_string(), name.clone()), None);
        }
        if item(ui, "Auto-rebase current onto this with AI…") {
            let (n, r, cfg) = (name.clone(), cx.repo.clone(), cx.settings.ai.clone());
            let undo = crate::views::textview::UndoSlot::default();
            let slot = undo.clone();
            let job: Box<dyn FnOnce() -> Result<String, String> + Send> = Box::new(move || {
                let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                let done = gitem_core::autorebase::run(&r, &n, &cfg.custom_instructions, &|s, u| gitem_core::ai::complete(&cfg, key.as_deref(), s, u)).map_err(|e| e.to_string())?;
                if let Ok(mut u) = slot.lock() {
                    *u = Some(gitem_core::autorebase::undo_op(&done));
                }
                Ok(done.describe(&n))
            });
            let tab = Tab::Text(Box::new(crate::views::textview::TextView::with_undo(&format!("Auto-rebase onto {name}"), job, undo)));
            cx.form(forms::FormDialog::new(format!("Auto-rebase onto {name}"), "Start").info("Rebases the current branch, sending each conflict to your AI provider. It stops and leaves the rebase to you if the model is unsure. You can undo it afterwards, unless your uncommitted changes are in a file the undo has to change: then it stops and changes nothing.").on_submit({
                let mut tab = Some(tab);
                move |_| Ok(tab.take().map(Action::OpenTab).into_iter().collect())
            }));
        }
        if item(ui, "Check for conflicts with current") {
            let (r, n) = (cx.repo.clone(), name.clone());
            cx.out.push(Action::Background(Box::new(move || {
                let files = r.predict_conflicts("HEAD", &n).map_err(|e| e.to_string())?;
                Ok(if files.is_empty() { format!("{n} would merge cleanly.") } else { format!("{n} would conflict in {} file(s): {}", files.len(), files.join(", ")) })
            })));
        }
    }
    if item(ui, "Explain with AI") {
        let (n, base) = (name.clone(), cx.data.base_branch());
        crate::views::textview::ai_tab(cx, &format!("Explain {n}"), move |r| {
            let e = |x: gitem_core::Error| x.to_string();
            let commits = r.log(&LogQuery { revs: vec![format!("{base}..{n}")], limit: 200, ..Default::default() }).map_err(e)?;
            let text: String = commits.iter().map(|c| format!("- {}\n", c.subject)).collect();
            Ok(gitem_core::ai::explain_prompt("a git branch", &text, &r.diff_for_ai(&DiffSource::MergeBase(base, n)).map_err(e)?))
        });
    }
    if b.is_current {
        let base = cx.data.base_branch();
        if name != base && item(ui, "Recompose branch…") {
            match cx.repo.merge_base(&base, "HEAD") {
                Ok(mb) => recompose(cx, &mb),
                Err(e) => cx.error(e.to_string()),
            }
        }
    }
    if item(ui, "Search commits on this branch") {
        cx.open(Tab::search(&format!("ref:{name}")));
    }
    if item(ui, "Create branch from this…") {
        cx.form(forms::new_branch(Some(name.clone())));
    }
    if item(ui, "Create worktree…") {
        let refs = cx.data.ref_names();
        cx.form(forms::add_worktree(cx.repo, refs, Some(name.clone())));
    }
    if item(ui, "Create tag…") {
        cx.form(forms::new_tag(Some(name.clone())));
    }
    ui.separator();
    if !b.is_remote {
        if item(ui, "Rename…") {
            cx.form(forms::rename_branch(name.clone()));
        }
        if item(ui, "Set upstream…") {
            let c: Vec<String> = cx.data.remote_branches().map(|x| x.name.clone()).collect();
            cx.form(forms::set_upstream(name.clone(), c));
        }
        if b.upstream.is_some() && item(ui, "Unset upstream") {
            cx.run(ops::branch_unset_upstream(&name));
        }
        if !b.is_current {
            if item(ui, "Move to another commit…") {
                cx.form(forms::move_branch(name.clone(), cx.data.ref_names()));
            }
            if item(ui, "Delete…") {
                cx.confirm("Delete branch", &format!("Delete {name}? This fails if it has unmerged commits."), "Delete", ops::branch_delete(&name, false));
            }
            if item(ui, "Force delete…") {
                cx.confirm("Force delete branch", &format!("Delete {name} even if it has unmerged commits? Those commits may become unreachable."), "Force delete", ops::branch_delete(&name, true));
            }
        }
        if item(ui, "Push…") {
            let remotes = cx.data.remotes().iter().map(|r| r.name.clone()).collect();
            cx.form(forms::push_form(Some(name.clone()), remotes, b.upstream.is_some(), cx.settings.confirm_destructive));
        }
    } else if item(ui, "Delete on remote…") {
        cx.confirm("Delete remote branch", &format!("Delete {name} on the remote? This affects everyone using it."), "Delete on remote", ops::branch_delete_remote(&name));
    }
    ui.separator();
    if item(ui, "Copy name") {
        cx.copy(name.clone());
    }
    if let Some(f) = cx.forge() {
        let short = if b.is_remote { name.split_once('/').map(|x| x.1).unwrap_or(&name).to_string() } else { name.clone() };
        let branch_url = f.branch_url(&short);
        let base = cx.data.base_branch();
        let pr_url = f.new_pr_url(&base, &short);
        if item(ui, "Open on remote") {
            cx.url(branch_url);
        }
        if item(ui, "Create pull request…") {
            cx.open(Tab::Launchpad(Box::new(crate::views::launchpad::LaunchpadView::create_pr_for(&short, &base))));
        }
        if item(ui, "Create pull request in browser") {
            cx.url(pr_url);
        }
    }
}

pub fn tag_menu(ui: &mut Ui, cx: &mut Cx, t: &Tag) {
    let name = t.name.clone();
    if item(ui, "Show commit") {
        cx.inspect(&t.sha);
    }
    if item(ui, "Check out (detached)…") {
        cx.confirm("Check out tag", &format!("Check out {name} with a detached HEAD?"), "Check out", ops::checkout_detached(&name));
    }
    if item(ui, "Compare with HEAD") {
        cx.open(Tab::compare(&name, "HEAD"));
    }
    if item(ui, "Create branch from this…") {
        cx.form(forms::new_branch(Some(name.clone())));
    }
    if item(ui, "Merge into current…") {
        cx.form(forms::merge_form(name.clone()));
    }
    ui.separator();
    let remote = cx.data.forge_remote.clone().unwrap_or_else(|| "origin".into());
    if item(ui, "Push to remote") {
        cx.run(ops::tag_push(&remote, &name));
    }
    if item(ui, "Delete locally…") {
        cx.confirm("Delete tag", &format!("Delete tag {name} from this repository?"), "Delete", ops::tag_delete(&name));
    }
    if item(ui, "Delete on remote…") {
        cx.confirm("Delete remote tag", &format!("Delete tag {name} on {remote}?"), "Delete on remote", ops::tag_delete_remote(&remote, &name));
    }
    ui.separator();
    if item(ui, "Copy name") {
        cx.copy(name.clone());
    }
    if let Some(f) = cx.forge() {
        let u = f.tag_url(&name);
        if item(ui, "Open on remote") {
            cx.url(u);
        }
    }
}

pub fn stash_menu(ui: &mut Ui, cx: &mut Cx, s: &Stash) {
    let sel = s.selector.clone();
    if item(ui, "Show details") {
        cx.inspect(&s.sha);
    }
    if item(ui, "Open changes") {
        cx.diff(DiffSource::Commit(s.sha.clone()), None);
    }
    if item(ui, "Apply") {
        cx.run(ops::stash_apply(&sel, false, false));
    }
    if item(ui, "Apply and restore staging") {
        cx.run(ops::stash_apply(&sel, false, true));
    }
    if item(ui, "Pop") {
        cx.run(ops::stash_apply(&sel, true, false));
    }
    if item(ui, "Rename…") {
        cx.form(forms::stash_rename(sel.clone(), s.sha.clone(), s.message.clone()));
    }
    if item(ui, "Create branch…") {
        cx.form(forms::stash_to_branch(sel.clone()));
    }
    if item(ui, "Explain with AI") {
        let (sha, msg) = (s.sha.clone(), s.message.clone());
        crate::views::textview::ai_tab(cx, &format!("Explain {sel}"), move |r| Ok(gitem_core::ai::explain_prompt("a git stash", &msg, &r.diff_for_ai(&DiffSource::Commit(sha)).map_err(|e| e.to_string())?)));
    }
    let others: Vec<String> = cx.data.worktrees().iter().filter(|w| !w.is_current && !w.bare).map(|w| w.path.clone()).collect();
    if !others.is_empty() {
        ui.menu_button("Apply in worktree", |ui| {
            for w in &others {
                if item(ui, w) {
                    cx.run(ops::stash_apply_in(w, &sel));
                }
            }
        });
    }
    ui.separator();
    if item(ui, "Drop…") {
        cx.confirm("Drop stash", &format!("Permanently drop {sel} ({})?", s.message), "Drop", ops::stash_drop(&sel));
    }
}

pub fn worktree_menu(ui: &mut Ui, cx: &mut Cx, w: &Worktree) {
    let path = w.path.clone();
    if !w.is_current && item(ui, "Open in gitem") {
        cx.out.push(Action::OpenRepo(path.clone().into()));
    }
    if item(ui, "Reveal in file manager") {
        cx.out.push(Action::OpenFile(path.clone().into()));
    }
    if item(ui, "Copy path") {
        cx.copy(path.clone());
    }
    if item(ui, "Open terminal here") {
        cx.out.push(Action::OpenTerminal(path.clone().into(), None));
    }
    let agent = cx.settings.agent_command.clone();
    if !agent.trim().is_empty() && item(ui, &format!("Start agent session ({agent})")) {
        cx.out.push(Action::OpenTerminal(path.clone().into(), Some(agent)));
    }
    let task = cx.settings.worktree_task.clone();
    if !task.trim().is_empty() && item(ui, &format!("Run task ({task})")) {
        cx.out.push(Action::OpenTerminal(path.clone().into(), Some(task)));
    }
    if let Some(b) = w.branch.clone()
        && item(ui, "Focus in graph") {
            cx.out.push(Action::GraphFocus(b));
        }
    if !w.is_current && item(ui, "Copy my changes here") {
        match cx.repo.diff_text(&DiffSource::WorkingVsHead) {
            Ok(p) if p.trim().is_empty() => cx.toast("There are no tracked changes to copy."),
            Ok(p) => cx.run(ops::apply_patch_in(&path, &p)),
            Err(e) => cx.error(e.to_string()),
        }
    }
    if !w.is_main {
        ui.separator();
        if item(ui, "Move…") {
            cx.form(forms::move_worktree(path.clone()));
        }
        if w.locked.is_some() {
            if item(ui, "Unlock") {
                cx.run(ops::worktree_unlock(&path));
            }
        } else if item(ui, "Lock…") {
            cx.form(forms::lock_worktree(path.clone()));
        }
        if item(ui, "Remove…") {
            cx.confirm("Remove worktree", &format!("Remove the worktree at {path}?"), "Remove", ops::worktree_remove(&path, false));
        }
        if item(ui, "Force remove…") {
            cx.confirm("Force remove worktree", &format!("Remove {path} and discard any uncommitted changes in it?"), "Force remove", ops::worktree_remove(&path, true));
        }
    }
}

pub fn file_menu(ui: &mut Ui, cx: &mut Cx, path: &str, rev: Option<&str>) {
    if item(ui, "Open with blame") {
        cx.open(Tab::file(path, rev.map(String::from), None));
    }
    if item(ui, "File history") {
        cx.open(Tab::file_history(path));
    }
    if item(ui, "Visual file history") {
        cx.open(Tab::visual(path));
    }
    if item(ui, "Compare with working tree") {
        cx.diff(DiffSource::WorkingVs(rev.unwrap_or("HEAD").to_string()), Some(path));
    }
    if let Some(r) = rev
        && item(ui, "Restore this version…") {
            cx.confirm("Restore file", &format!("Replace {path} in the working tree and index with the version from {}?", gitem_core::util::prefix(r, 10)), "Restore", ops::restore_file_from(r, path));
        }
    ui.separator();
    if item(ui, "Copy path") {
        cx.copy(path.to_string());
    }
    let label = if cx.settings.default_external_editor.is_empty() { "Reveal in file manager" } else { "Open in external editor" };
    if item(ui, label) {
        cx.out.push(Action::OpenFile(cx.repo.root.join(path)));
    }
    if let Some(f) = cx.forge() {
        let u = f.file_url(rev.unwrap_or("HEAD"), path, None);
        if item(ui, "Open on remote") {
            cx.url(u);
        }
    }
}
