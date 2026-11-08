//! Context menus for commits, refs, files, stashes and worktrees. Shared by the
//! graph, sidebar, and lists so the same commands appear everywhere.
use egui::Ui;
use gitem_core::model::*;
use gitem_core::ops;
use gitem_core::repo::DiffSource;

use crate::action::{Action, Cx, Later};
use crate::forms;
use crate::views::Tab;

fn item(ui: &mut Ui, label: &str) -> bool {
    let c = ui.button(crate::i18n::t(label)).clicked();
    if c {
        ui.close();
    }
    c
}

/// As `item`, for what loses something or rewrites history.
fn risky(ui: &mut Ui, cx: &Cx, label: &str) -> bool {
    let c = ui.button(egui::RichText::new(crate::i18n::t(label)).color(cx.pal.danger)).clicked();
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
    if item(ui, "Check out (detached)…") {
        cx.confirm("Check out commit", &format!("Check out {short} with a detached HEAD?"), "Check out", ops::checkout_detached(&sha).lossless());
    }
    if item(ui, "Cherry-pick…") {
        cx.form(forms::cherry_pick_form(vec![sha.clone()]));
    }
    if item(ui, "Revert…") {
        cx.form(forms::revert_form(sha.clone(), c.is_merge()));
    }
    if item(ui, "Create branch here…") {
        cx.form(forms::new_branch(Some(sha.clone())));
    }
    if item(ui, "Copy SHA") {
        cx.copy(sha.clone());
    }
    if let Some(f) = cx.forge() {
        let url = f.commit_url(&sha);
        if item(ui, "Open on remote") {
            cx.url(url);
        }
    }
    let is_head = cx.status().is_some_and(|s| s.head_sha == sha);
    if is_head && item(ui, "Undo commit") {
        undo_commit(cx);
    }
    ui.separator();
    ui.menu_button("Compare", |ui| {
        if item(ui, "Compare with working tree") {
            cx.diff(DiffSource::WorkingVs(sha.clone()), None);
        }
        if item(ui, "Compare with HEAD") {
            cx.open(Tab::compare(&sha, "HEAD"));
        }
        if item(ui, "Browse files at this commit") {
            cx.open(Tab::tree(&sha));
        }
    });
    ui.menu_button("Rewrite history", |ui| {
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
        if item(ui, "Create fixup commit") {
            cx.run(ops::fixup_commit(&sha));
        }
        // Not on a first commit: there's nothing before it to rebase onto.
        if let Some(p) = c.parents.first()
            && item(ui, "Squash fixups from here") {
                cx.run(ops::squash_fixups(p));
            }
        if item(ui, "Recompose from here…") {
            recompose(cx, &sha);
        }
        if risky(ui, cx, "Drop…") {
            open_plan(cx, c, gitem_core::rebase::RebaseAction::Drop, false);
        }
    });
    ui.menu_button("Reset, rebase and merge", |ui| {
        if item(ui, "Merge into current branch…") {
            cx.form(forms::merge_form(sha.clone()));
        }
        if item(ui, "Rebase current branch onto this…") {
            cx.form(forms::rebase_form(sha.clone()));
        }
        if item(ui, "Interactive rebase from here…") {
            match c.parents.first() {
                Some(base) => cx.open(Tab::rebase(Some(base.clone()))),
                None => cx.open(Tab::Rebase(Box::new(crate::views::rebase::RebaseView::from_root()))),
            }
        }
        if let (Some(branch), Some(remote)) = (cx.current_branch(), cx.data.forge_remote.clone().or_else(|| cx.data.remotes().first().map(|r| r.name.clone())))
            && item(ui, "Push up to this commit…") {
                cx.confirm("Push to commit", &format!("Push {branch} to {remote} only up to {short}?"), "Push", ops::push_to_commit(&remote, &sha, &branch).lossless());
            }
        if risky(ui, cx, "Reset current branch to here…") {
            cx.form(forms::reset_form(sha.clone()));
        }
    });
    ui.menu_button("Create", |ui| {
        if item(ui, "Create tag here…") {
            cx.form(forms::new_tag(Some(sha.clone())));
        }
        if item(ui, "Create worktree here…") {
            let refs = cx.data.ref_names();
            cx.form(forms::add_worktree(cx.repo, refs, Some(sha.clone())));
        }
        if item(ui, "Edit note…") {
            let (repo, sha) = (cx.repo.clone(), sha.clone());
            cx.out.push(Action::Later(Box::new(move || {
                let current = repo.commit_note(&sha).unwrap_or_default();
                Ok(Later::Form(Box::new(move || forms::note_form(sha, current))))
            })));
        }
        if item(ui, "Start bisect with this as bad…") {
            let refs = cx.data.ref_names();
            cx.form(forms::bisect_start_form(sha.clone(), refs));
        }
    });
    ui.menu_button("Copy", |ui| {
        if item(ui, "Copy short SHA") {
            cx.copy(short.clone());
        }
        if item(ui, "Copy message") {
            cx.copy(c.message());
        }
        if item(ui, "Copy gitem link") {
            let d = gitem_core::forge::DeepLink { repo: cx.repo.root.to_string_lossy().replace('\\', "/"), commit: Some(sha.clone()), ..Default::default() };
            cx.copy(d.to_url());
        }
        if item(ui, "Copy as patch") {
            // 16 seconds on a commit of 100,000 files, which would freeze the window.
            let (repo, sha) = (cx.repo.clone(), sha.clone());
            cx.out.push(Action::Later(Box::new(move || repo.patch_text(&DiffSource::Commit(sha)).map(crate::action::Later::Copy).map_err(|e| e.to_string()))));
        }
        if let Some(f) = cx.forge() {
            let url = f.commit_url(&sha);
            if item(ui, "Copy remote link") {
                cx.copy(url);
            }
        }
    });
    ui.menu_button("Export", |ui| {
        if item(ui, "Save as patch…") {
            cx.open(Tab::Patches(Box::new(crate::views::patches::PatchesView::create_from(DiffSource::Commit(sha.clone()), &c.subject))));
        }
        if item(ui, "Export with format-patch…") {
            cx.form(forms::format_patch_form(format!("{sha}~1..{sha}")));
        }
        if item(ui, "Create archive…") {
            cx.form(forms::archive_form(sha.clone()));
        }
    });
}

/// Open the rebase editor with one commit's action already chosen, for review.
fn open_plan(cx: &mut Cx, c: &Commit, action: gitem_core::rebase::RebaseAction, needs_previous: bool) {
    use crate::views::rebase::RebaseView;
    // Neither can be in a plan: a rebase replays merges as linear history, and starting one
    // from another branch's commit would rebase this branch onto that commit, unasked.
    if c.is_merge() {
        cx.error(format!("{} is a merge commit, and a rebase can't change one on its own. It replays merges as plain history.", c.short()));
        return;
    }
    let (repo, sha, short, parent) = (cx.repo.clone(), c.sha.clone(), c.short().to_string(), c.parents.first().cloned());
    cx.out.push(Action::Later(Box::new(move || {
        if !repo.is_ancestor(&sha, "HEAD") {
            return Err(format!("{short} isn't on the current branch, so rebasing can't change it. Switch to a branch that has it first."));
        }
        // A squash also rewrites the commit it folds into, so the plan starts one earlier.
        let base = if needs_previous { parent.map(|p| format!("{p}~1")).filter(|b| repo.resolve(b).is_ok()) } else { parent };
        Ok(Later::Tab(Box::new(move || {
            let mut view = match base {
                Some(b) => RebaseView::new(Some(b)),
                None => RebaseView::from_root(),
            };
            view.preset(&sha, action);
            Tab::Rebase(Box::new(view))
        })))
    })));
}

/// Its tab comes with the go-ahead: saying no leaves everything as it was.
fn recompose_form(base: String) -> forms::FormDialog {
    forms::FormDialog::new("Recompose", "Recompose").info(format!("Undo the commits after {}, keeping all their changes in the working tree, then arrange them into new commits? Commit messages are not kept. The reflog can restore them.", gitem_core::util::prefix(&base, 10))).on_submit(move |_| Ok(vec![Action::Run(ops::recompose_reset(&base)), Action::OpenTab(Tab::Composer(Box::default()))]))
}

fn recompose(cx: &mut Cx, base: &str) {
    cx.form(recompose_form(base.to_string()));
}

/// Keeps the changes, but asks first once the commit is on a remote, since it would then
/// have to be pushed over. A first commit has nothing before it to go back to.
pub fn undo_commit(cx: &mut Cx) {
    let pushed = cx.status().is_some_and(|s| s.upstream.is_some() && s.ahead == 0);
    let action = undo_commit_action(cx.repo, pushed, cx.settings.confirm_destructive);
    cx.out.push(action);
}

pub fn undo_commit_action(repo: &gitem_core::Repo, pushed: bool, confirming: bool) -> Action {
    let repo = repo.clone();
    Action::Later(Box::new(move || {
        if repo.resolve("HEAD~1").is_err() {
            return Ok(Later::Toast("This is the first commit on this branch, so there's nothing to go back to.".to_string()));
        }
        Ok(if pushed {
            Later::Confirm { title: "Undo a pushed commit".to_string(), body: "This commit is already on the remote. Undoing it here means the next push has to overwrite it. Your changes stay in the working tree.".to_string(), label: "Undo commit".to_string(), op: ops::undo_commit() }
        } else if confirming {
            Later::Confirm { title: "Undo last commit".to_string(), body: "Take the last commit back? Its changes stay staged.".to_string(), label: "Undo commit".to_string(), op: ops::undo_commit().lossless() }
        } else {
            Later::Run(ops::undo_commit())
        })
    }))
}

pub fn branch_menu(ui: &mut Ui, cx: &mut Cx, b: &Branch) {
    let name = b.name.clone();
    // The short name would mean a tag of the same name, if there is one.
    let rev = b.rev_among(cx.data.tags());
    if !b.is_current {
        if b.is_remote {
            if item(ui, "Check out as local branch") {
                cx.run(cx.data.check_out_remote(&name));
            }
        } else if item(ui, "Check out") {
            cx.run(ops::checkout(&name));
        }
        if item(ui, "Merge into current…") {
            cx.form(forms::merge_form(rev.clone()));
        }
        if item(ui, "Rebase current onto this…") {
            cx.form(forms::rebase_form(rev.clone()));
        }
        if item(ui, "Interactive rebase onto this…") {
            cx.open(Tab::rebase(Some(rev.clone())));
        }
        ui.separator();
    }
    if cx.settings.branch_model.enabled
        && !b.is_remote
        && let Some((kind, short)) = cx.settings.branch_model.kind_of(&name)
        && item(ui, &format!("Finish {}…", kind.word()))
    {
        cx.form(forms::finish_branch(&cx.settings.branch_model, kind, cx.data, cx.status(), Some(short)));
    }
    if item(ui, "Focus in graph") {
        cx.out.push(Action::GraphFocus(rev.clone()));
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
    ui.menu_button("Compare", |ui| {
        if item(ui, "Compare with HEAD") {
            cx.open(Tab::compare("HEAD", &rev));
        }
        if item(ui, "Compare with working tree") {
            cx.diff(DiffSource::WorkingVs(rev.clone()), None);
        }
        if !b.is_current {
            if item(ui, "Open all changes with common base") {
                cx.diff(DiffSource::MergeBase("HEAD".to_string(), rev.clone()), None);
            }
            if item(ui, "Check for conflicts with current") {
                let (r, n) = (cx.repo.clone(), rev.clone());
                cx.out.push(Action::Background(Box::new(move || {
                    let files = r.predict_conflicts("HEAD", &n).map_err(|e| e.to_string())?;
                    Ok(if files.is_empty() { format!("{n} would merge cleanly.") } else { format!("{n} would conflict in {}: {}", crate::widgets::plural(files.len(), "file", "files"), files.join(", ")) })
                })));
            }
        }
        if item(ui, "Search commits on this branch") {
            cx.open(Tab::search(&format!("ref:{rev}")));
        }
    });
    if !b.is_current && item(ui, "Auto-rebase current onto this with AI…") {
        let (n, r, cfg) = (rev.clone(), cx.repo.clone(), cx.settings.ai.clone());
        let title = format!("Auto-rebase onto {name}");
        let jobs = cx.data.jobs.clone();
        cx.form(forms::FormDialog::new(title.clone(), "Start").info("Rebases the current branch, sending each conflict to your AI provider. It stops and leaves the rebase to you if the model is unsure, and also once it has resolved a commit's conflicts, so you can read the result before anything is committed from it. You can undo it afterwards, unless your uncommitted changes are in a file the undo has to change: then it stops and changes nothing.").check("unattended", "Don't stop for review: commit the model's answers and carry on", false).on_submit(move |v| {
            let (n, mut r, cfg) = (n.clone(), r.clone(), cfg.clone());
            let review = !v.on("unattended");
            // Its steps are minutes apart, so nothing else in the app knows one is running.
            if jobs.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                return Err("An auto-rebase is already running in this repository.".to_string());
            }
            let busy = crate::views::textview::JobGuard::new(jobs.clone());
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            r.git.cancel = Some(stop.clone());
            let undo = crate::views::textview::UndoSlot::default();
            let slot = undo.clone();
            let job: Box<dyn FnOnce() -> Result<String, String> + Send> = Box::new(move || {
                let _busy = busy;
                let key = gitem_core::cred::ai_key(cfg.provider.key_name());
                let done = gitem_core::autorebase::run(&r, &n, &cfg.custom_instructions, review, &|s, u| gitem_core::ai::complete(&cfg, key.as_deref(), s, u)).map_err(|e| e.to_string())?;
                if let Ok(mut u) = slot.lock() {
                    *u = Some(gitem_core::autorebase::undo_op(&done));
                }
                Ok(done.describe(&n))
            });
            Ok(vec![Action::OpenTab(Tab::Text(Box::new(crate::views::textview::TextView::with_undo(&title, job, undo).stoppable(stop))))])
        }));
    }
    if item(ui, "Explain with AI") {
        let (n, base) = (rev.clone(), cx.data.base_branch());
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
            let repo = cx.repo.clone();
            cx.out.push(Action::Later(Box::new(move || {
                let mb = repo.merge_base(&base, "HEAD").map_err(|e| e.to_string())?;
                Ok(Later::Form(Box::new(move || recompose_form(mb))))
            })));
        }
    }
    ui.menu_button("Create", |ui| {
        if item(ui, "Create branch from this…") {
            cx.form(forms::new_branch(Some(rev.clone())));
        }
        if item(ui, "Create worktree…") {
            let refs = cx.data.ref_names();
            cx.form(forms::add_worktree(cx.repo, refs, Some(name.clone())));
        }
        if item(ui, "Create tag…") {
            cx.form(forms::new_tag(Some(rev.clone())));
        }
    });
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
            if b.behind > 0 && b.ahead == 0
                && let Some(up) = &b.upstream
                && !b.upstream_gone
                && item(ui, "Fast-forward to upstream")
            {
                cx.run(ops::fast_forward_branch(&name, up, &cx.data.remote_names()));
            }
            if item(ui, "Move to another commit…") {
                cx.form(forms::move_branch(name.clone(), cx.data.ref_names()));
            }
            if risky(ui, cx, "Delete…") {
                cx.confirm("Delete branch", &format!("Delete {name}? This fails if it has unmerged commits."), "Delete", ops::branch_delete(&name, false));
            }
            if risky(ui, cx, "Force delete…") {
                cx.confirm("Force delete branch", &format!("Delete {name} even if it has unmerged commits? Those commits are then reachable only through the reflog, until git expires them."), "Force delete", ops::branch_delete(&name, true));
            }
        }
        if item(ui, "Clean up branches…") {
            cx.open(Tab::Prune(Box::default()));
        }
        if item(ui, "Push…") {
            let remotes = cx.data.remotes().iter().map(|r| r.name.clone()).collect();
            cx.form(forms::push_form_guarding(Some(name.clone()), remotes, b.upstream.is_some(), cx.settings.confirm_destructive, cx.data.base_branch()));
        }
    } else if risky(ui, cx, "Delete on remote…") {
        cx.confirm("Delete remote branch", &format!("Delete {name} on the remote? This affects everyone using it."), "Delete on remote", ops::branch_delete_remote(&name, &cx.data.remote_names()));
    }
    ui.separator();
    if item(ui, "Copy name") {
        cx.copy(name.clone());
    }
    if let Some(f) = cx.forge() {
        let short = if b.is_remote { gitem_core::parse::split_remote_ref(&name, &cx.data.remote_names()).1.to_string() } else { name.clone() };
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
        cx.confirm("Check out tag", &format!("Check out {name} with a detached HEAD?"), "Check out", ops::checkout_detached(&name).lossless());
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
    let remotes = cx.data.remote_names();
    if item(ui, "Push to remote") {
        cx.run(ops::tag_push(&remote, &name));
    }
    if !remotes.is_empty() {
        if item(ui, "Push to…") {
            cx.form(forms::tag_push_form(name.clone(), remotes.clone(), remote.clone()));
        }
        if item(ui, "Push all tags…") {
            cx.form(forms::tags_push_all_form(remotes.clone(), remote.clone()));
        }
    }
    if risky(ui, cx, "Delete locally…") {
        cx.confirm("Delete tag", &format!("Delete tag {name} from this repository?"), "Delete", ops::tag_delete(&name));
    }
    if remotes.len() > 1 {
        if risky(ui, cx, "Delete on a remote…") {
            cx.form(forms::tag_delete_remote_form(name.clone(), remotes, remote.clone()));
        }
    } else if risky(ui, cx, "Delete on remote…") {
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
    if item(ui, "Open untracked files") {
        // Kept in a third parent commit of its own, which only some stashes have.
        let (repo, sha) = (cx.repo.clone(), s.sha.clone());
        cx.out.push(Action::Later(Box::new(move || {
            let third = format!("{sha}^3");
            Ok(match repo.resolve(&third) {
                Ok(found) => crate::action::Later::Diff(DiffSource::Commit(found), None),
                Err(_) => crate::action::Later::Toast("This stash has no untracked files.".to_string()),
            })
        })));
    }
    if item(ui, "Apply") {
        cx.run(ops::only_if_stash_is(if gitem_core::hunkstash::is_hunk_stash(&s.message) { gitem_core::hunkstash::apply(&sel, false) } else { ops::stash_apply(&sel, false, false) }, &sel, &s.sha));
    }
    if item(ui, "Apply and restore staging") {
        cx.run(ops::only_if_stash_is(ops::stash_apply(&sel, false, true), &sel, &s.sha));
    }
    if item(ui, "Pop") {
        cx.run(ops::only_if_stash_is(if gitem_core::hunkstash::is_hunk_stash(&s.message) { gitem_core::hunkstash::apply(&sel, true) } else { ops::stash_apply(&sel, true, false) }, &sel, &s.sha));
    }
    if item(ui, "Rename…") {
        cx.form(forms::stash_rename(sel.clone(), s.sha.clone(), s.message.clone()));
    }
    if item(ui, "Create branch…") {
        cx.form(forms::stash_to_branch(sel.clone(), s.sha.clone()));
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
                    cx.run(ops::only_if_stash_is(ops::stash_apply_in(w, &sel), &sel, &s.sha));
                }
            }
        });
    }
    ui.separator();
    if risky(ui, cx, "Drop…") {
        cx.confirm("Drop stash", &format!("Permanently drop {sel} ({})?", s.message), "Drop", ops::only_if_stash_is(ops::stash_drop(&sel), &sel, &s.sha));
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
        let (repo, path) = (cx.repo.clone(), path.clone());
        cx.out.push(Action::Later(Box::new(move || match repo.patch_bytes(&DiffSource::WorkingVsHead) {
            Ok(p) if p.iter().all(u8::is_ascii_whitespace) => Ok(crate::action::Later::Toast("There are no changes to copy.".to_string())),
            Ok(p) => Ok(crate::action::Later::Run(ops::apply_patch_in(&path, &p))),
            Err(e) => Err(e.to_string()),
        })));
    }
    // Not the one this tab has open: its files are in use, so git would empty the folder and
    // then fail to delete it, leaving the tab on nothing.
    if !w.is_main && w.is_current {
        ui.separator();
        ui.add_enabled(false, egui::Button::new("Move or remove from another worktree")).on_disabled_hover_text("It's the one open in this tab. Open the main worktree to do this.");
    }
    if !w.is_main && !w.is_current {
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
        if w.locked.is_some() {
            // Git refuses anything less for a locked one.
            if item(ui, "Force remove…") {
                cx.confirm("Force remove locked worktree", &format!("{path} is locked. Remove it anyway, and discard any uncommitted changes in it?"), "Force remove", ops::worktree_remove_locked(&path));
            }
        } else {
            if item(ui, "Remove…") {
                cx.confirm("Remove worktree", &format!("Remove the worktree at {path}?"), "Remove", ops::worktree_remove(&path, false));
            }
            if item(ui, "Force remove…") {
                cx.confirm("Force remove worktree", &format!("Remove {path} and discard any uncommitted changes in it?"), "Force remove", ops::worktree_remove(&path, true));
            }
        }
    }
}

pub fn file_menu(ui: &mut Ui, cx: &mut Cx, path: &str, rev: Option<&str>) {
    // Only the working tree's copy can be edited; a revision is history.
    if rev.is_none() && item(ui, "Edit") {
        cx.open(Tab::editor(path, None, true));
    }
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
    if item(ui, "Open in external diff tool") {
        cx.out.push(Action::RunDetached(ops::difftool_file(&cx.settings.external_diff_tool, rev, path)));
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
