//! Command palette (Ctrl+P): every command, branch and file in one fuzzy list.
//! Commands needing input open the same forms used by the rest of the UI.
use egui::{RichText, Ui};
use gitem_core::ops;
use gitem_core::repo::DiffSource;
use gitem_core::util::fuzzy_score;

use crate::action::{Action, Cx};
use crate::forms;
use crate::task::Task;
use crate::views::Tab;

type Run = Box<dyn FnOnce(&mut Cx)>;

pub struct Item {
    pub category: &'static str,
    pub label: String,
    pub run: Run,
}

fn item(category: &'static str, label: impl Into<String>, run: impl FnOnce(&mut Cx) + 'static) -> Item {
    Item { category, label: label.into(), run: Box::new(run) }
}

/// All commands that don't depend on loaded lists.
pub fn commands() -> Vec<Item> {
    let mut v = Vec::new();
    macro_rules! view {
        ($label:expr, $tab:expr) => {
            v.push(item("View", $label, |cx| cx.open($tab)));
        };
    }
    view!("Home", Tab::home());
    view!("Commit Graph", Tab::graph());
    view!("Changes", Tab::changes());
    view!("Launchpad", Tab::Launchpad(Box::default()));
    view!("Search Commits", Tab::Search(Box::default()));
    view!("Compare Refs", Tab::Search(Box::new(crate::views::search::SearchView::compare("", ""))));
    view!("Worktrees", Tab::Worktrees(Box::default()));
    view!("Contributors", Tab::Contributors(Box::default()));
    view!("Reflog", Tab::Reflog(Box::default()));
    view!("Interactive Rebase", Tab::rebase(None));
    view!("Bisect", Tab::Bisect(Box::default()));
    view!("Patches", Tab::Patches(Box::default()));
    view!("Commit Composer", Tab::Composer(Box::default()));
    view!("Repository Tools", Tab::Tools(Box::default()));
    view!("Git Config", Tab::Config(Box::default()));
    view!("Credentials", Tab::Credentials(Box::default()));
    view!("Git Console", Tab::Console(Box::default()));
    view!("Settings", Tab::Settings(Box::default()));
    view!("Visual History", Tab::visual(""));
    view!("Treemap", Tab::Treemap(Box::default()));
    view!("Repository Health", Tab::Health(Box::default()));
    view!("Agent Sessions", Tab::Agents(Box::default()));
    view!("Review Changes", Tab::Review(Box::default()));

    v.push(item("Git", "Fetch…", |cx| {
        let r = cx.data.remotes().iter().map(|x| x.name.clone()).collect();
        cx.form(forms::fetch_form(r, cx.settings.prune_on_fetch));
    }));
    v.push(item("Git", "Pull…", |cx| cx.form(forms::pull_form())));
    v.push(item("Git", "Push…", |cx| {
        let r = cx.data.remotes().iter().map(|x| x.name.clone()).collect();
        let has = cx.status().is_some_and(|s| s.upstream.is_some());
        cx.form(forms::push_form(cx.current_branch(), r, has));
    }));
    v.push(item("Git", "Create Branch…", |cx| cx.form(forms::new_branch(None))));
    v.push(item("Git", "Create Tag…", |cx| cx.form(forms::new_tag(None))));
    v.push(item("Git", "Stash…", |cx| cx.form(forms::stash_form(Vec::new()))));
    v.push(item("Git", "Stage All", |cx| cx.run(ops::stage_all())));
    v.push(item("Git", "Add Remote…", |cx| cx.form(forms::add_remote())));
    v.push(item("Git", "Add Worktree…", |cx| cx.form(forms::add_worktree(cx.repo, cx.data.ref_names(), None))));
    v.push(item("Git", "Add Submodule…", |cx| cx.form(forms::add_submodule())));
    v.push(item("Git", "Apply Patch…", |cx| cx.form(forms::apply_patch_form())));
    v.push(item("Git", "Merge a Branch…", |cx| {
        let refs = cx.data.ref_names();
        cx.form(pick_ref("Merge which ref?", refs, forms::merge_form));
    }));
    v.push(item("Git", "Rebase onto a Branch…", |cx| {
        let refs = cx.data.ref_names();
        cx.form(pick_ref("Rebase onto which ref?", refs, forms::rebase_form));
    }));
    v.push(item("Git", "Reset to a Ref…", |cx| {
        let refs = cx.data.ref_names();
        cx.form(pick_ref("Reset to which ref?", refs, forms::reset_form));
    }));
    v.push(item("Git", "Cherry-pick a Commit…", |cx| {
        let refs = cx.data.ref_names();
        cx.form(pick_ref("Cherry-pick which commit?", refs, |r| forms::cherry_pick_form(vec![r])));
    }));
    v.push(item("Git", "Clone Repository…", |cx| cx.form(forms::clone_form())));
    v.push(item("Git", "Initialize Repository…", |cx| cx.form(forms::init_form())));
    v.push(item("Show", "Staged Diff", |cx| cx.diff(DiffSource::Staged, None)));
    v.push(item("Show", "Unstaged Diff", |cx| cx.diff(DiffSource::Unstaged, None)));
    v.push(item("Show", "All Changes vs HEAD", |cx| cx.diff(DiffSource::WorkingVsHead, None)));
    v.push(item("Git", "Undo Last Commit", |cx| cx.run(ops::undo_commit())));
    v.push(item("Git", "Apply Copied Patch…", |cx| {
        cx.form(forms::FormDialog::new("Apply copied patch", "Apply").multiline("patch", "Paste the patch", "").check("three", "Three-way merge on failure", true).on_submit(|v| {
            let t = v.text("patch");
            if t.trim().is_empty() {
                return Err("Paste a patch first.".to_string());
            }
            Ok(vec![Action::Run(ops::apply_patch_text(&format!("{}\n", t.trim_end()), v.on("three")))])
        }));
    }));
    v.push(item("Git", "Copy Working Changes as Patch", |cx| match cx.repo.diff_text(&DiffSource::WorkingVsHead) {
        Ok(p) => cx.copy(p),
        Err(e) => cx.error(e.to_string()),
    }));
    v.push(item("Git", "Publish Repository…", |cx| {
        let branch = cx.current_branch().unwrap_or_else(|| "main".to_string());
        cx.form(forms::FormDialog::new("Publish repository", "Publish").text("name", "Remote name", "origin").text("url", "URL", "").on_submit(move |v| {
            let u = v.opt("url").ok_or("A URL is required.")?;
            Ok(vec![Action::Run(ops::publish_repository(&v.text("name"), &u, &branch))])
        }));
    }));
    v.push(item("Git", "Add Remote, Checking the URL…", |cx| {
        cx.form(forms::FormDialog::new("Add remote", "Add").text("name", "Name", "origin").text("url", "URL", "").on_submit(|v| {
            let u = v.opt("url").ok_or("A URL is required.")?;
            Ok(vec![Action::Run(ops::remote_add_checked(&v.text("name"), &u))])
        }));
    }));
    v.push(item("Show", "Open Directory Compare…", |cx| {
        cx.form(forms::FormDialog::new("Directory compare", "Open").text("a", "From", "HEAD~1").text("b", "To", "HEAD").on_submit(|v| {
            Ok(vec![Action::Run(ops::difftool_dir(&v.text("a"), &v.text("b")))])
        }));
    }));
    v.push(item("Remote", "Open Repository on Remote", |cx| match cx.forge().map(|f| f.web_url()) {
        Some(u) => cx.url(u),
        None => cx.toast("No hosting provider among the remotes."),
    }));
    v.push(item("Remote", "Copy Repository Link", |cx| match cx.forge().map(|f| f.web_url()) {
        Some(u) => cx.copy(u),
        None => cx.toast("No hosting provider among the remotes."),
    }));
    v.push(item("Remote", "Open Current Branch on Remote", |cx| {
        let b = cx.current_branch().unwrap_or_default();
        match cx.forge().map(|f| f.branch_url(&b)) {
            Some(u) => cx.url(u),
            None => cx.toast("No hosting provider among the remotes."),
        }
    }));
    v.push(item("Remote", "Open File from Remote Link…", |cx| {
        cx.form(forms::FormDialog::new("Open file from a link", "Open").text("url", "Link to a file on GitHub, GitLab, and so on", "").on_submit(|v| {
            let u = v.text("url");
            let (rev, path, line) = gitem_core::forge::parse_file_url(u.trim()).ok_or("That doesn't look like a link to a file.")?;
            Ok(vec![Action::OpenTab(Tab::file(&path, Some(rev), line))])
        }));
    }));
    v.push(item("App", "Zen Mode (Hide Side Panels)", |cx| cx.out.push(Action::OpenTerminal(std::path::PathBuf::new(), Some("__zen__".to_string())))));
    v.push(item("App", "Open Terminal Here", |cx| cx.out.push(Action::OpenTerminal(cx.repo.root.clone(), None))));
    v.push(item("App", "Refresh", |cx| cx.out.push(Action::Refresh)));
    v
}

fn pick_ref(title: &str, refs: Vec<String>, then: fn(String) -> forms::FormDialog) -> forms::FormDialog {
    forms::FormDialog::new(title, "Next").pick("r", "Branch, tag or SHA", refs, "").on_submit(move |v| {
        let r = v.opt("r").ok_or("Choose a ref.")?;
        Ok(vec![Action::Form(then(r))])
    })
}

#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    files: Task<Vec<String>>,
    file_hits: Vec<String>,
    hits_for: Option<String>,
    started: bool,
    just_opened: bool,
}

impl PaletteState {
    pub fn new() -> Self {
        Self { just_opened: true, ..Default::default() }
    }

    /// Returns true when the palette should close.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) -> bool {
        let p = cx.pal.clone();
        self.files.poll();
        if !self.started {
            self.started = true;
            let r = cx.repo.clone();
            self.files.spawn(ui.ctx(), move || r.tracked_files(None));
        }
        ui.set_width(620.0);
        let r = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Run a command, switch branch, or open a file").desired_width(f32::INFINITY));
        if self.just_opened {
            r.request_focus();
            self.just_opened = false;
        }
        if r.changed() {
            self.selected = 0;
        }
        let mut items = commands();
        let current = cx.current_branch();
        for b in cx.data.local_branches() {
            if Some(&b.name) != current.as_ref() {
                let n = b.name.clone();
                items.push(item("Switch to", b.name.clone(), move |cx| cx.run(ops::checkout(&n))));
            }
        }
        for b in cx.data.remote_branches() {
            let n = b.name.clone();
            items.push(item("Check out", b.name.clone(), move |cx| cx.run(ops::checkout_remote(&n))));
        }
        // Repositories can hold many thousands of files: only rescan on a new query.
        if self.hits_for.as_deref() != Some(&self.query) && self.files.get().is_some() {
            self.hits_for = Some(self.query.clone());
            self.file_hits.clear();
            if !self.query.is_empty()
                && let Some(files) = self.files.get() {
                    let mut scored: Vec<(i32, &String)> = files.iter().filter_map(|f| fuzzy_score(&self.query, f).map(|s| (s, f))).collect();
                    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                    self.file_hits = scored.into_iter().take(60).map(|x| x.1.clone()).collect();
                }
        }
        for f in &self.file_hits {
            let path = f.clone();
            items.push(item("Open file", f.clone(), move |cx| cx.open(Tab::file(&path, None, None))));
        }
        let q = self.query.clone();
        let mut scored: Vec<(i32, Item)> = items
            .into_iter()
            .filter_map(|it| {
                let hay = format!("{} {}", it.category, it.label);
                fuzzy_score(&q, &hay).map(|s| (s + if it.category == "Open file" { -5 } else { 0 }, it))
            })
            .collect();
        scored.sort_by_key(|x| std::cmp::Reverse(x.0));
        scored.truncate(200);

        let n = scored.len();
        let (down, up, enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if down && n > 0 {
            self.selected = (self.selected + 1).min(n - 1);
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        let mut chosen = enter.then_some(self.selected);
        let sel = self.selected;
        egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (_, it)) in scored.iter().enumerate() {
                let (rect, resp) = crate::widgets::row(ui, 24.0, i == sel, &p);
                crate::widgets::paint_text(ui, egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 0.0), egui::vec2(80.0, 24.0)), it.category, crate::widgets::prop(11.0), p.faint);
                crate::widgets::paint_text(ui, egui::Rect::from_min_max(rect.min + egui::vec2(92.0, 0.0), rect.max), &it.label, crate::widgets::prop(13.0), p.text);
                if i == sel && (down || up) {
                    ui.scroll_to_rect(rect, None);
                }
                if resp.clicked() {
                    chosen = Some(i);
                }
            }
            if scored.is_empty() {
                ui.label(RichText::new("No matches").color(p.dim));
            }
        });
        if let Some(i) = chosen
            && i < scored.len() {
                let (_, it) = scored.swap_remove(i);
                (it.run)(cx);
                return true;
            }
        esc
    }
}
