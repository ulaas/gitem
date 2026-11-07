//! Command palette (Ctrl+P): every command, branch and file in one fuzzy list, as a
//! centred overlay. A first character narrows it: `>` commands, `@` branches, `/` files,
//! `:` commits. Commands needing input open the same forms as the rest of the UI.
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use gitem_core::model::{Commit, LogQuery};
use gitem_core::ops;
use gitem_core::repo::DiffSource;
use gitem_core::util::{fuzzy_match, fuzzy_score};

use crate::action::{Action, Cx};
use crate::chrome::icon;
use crate::forms;
use crate::task::Task;
use crate::theme::Palette;
use crate::views::Tab;
use crate::widgets::prop;

type Run = Box<dyn FnOnce(&mut Cx)>;

pub struct Item {
    pub category: &'static str,
    pub label: String,
    pub detail: String,
    pub glyph: &'static str,
    pub chord: Option<&'static str>,
    /// Searched when the label doesn't match, such as a file's whole path.
    pub search: Option<String>,
    /// Remembered under Recent once used.
    pub record: bool,
    pub run: Run,
}

/// Icon, one-line description, and shortcut for a command.
fn meta(category: &str, label: &str) -> (&'static str, &'static str, Option<&'static str>) {
    match label {
        "Home" => (icon::HOUSE, "Overview and start work", None),
        "Commit Graph" => (icon::GIT_FORK, "All branches and their history", Some("Ctrl+Shift+H")),
        "Changes" => (icon::GIT_DIFF, "Stage and commit", Some("Ctrl+Shift+G")),
        "Launchpad" => (icon::ROCKET_LAUNCH, "Pull requests and issues", None),
        "Search Commits" => (icon::MAGNIFYING_GLASS, "By message, author, file, or change", None),
        "Compare Refs" => (icon::ARROWS_LEFT_RIGHT, "What differs between two branches or tags", None),
        "Worktrees" => (icon::TREE_STRUCTURE, "Work on several branches at once", None),
        "Contributors" => (icon::USERS_THREE, "People and activity", None),
        "Reflog" => (icon::CLOCK_COUNTER_CLOCKWISE, "Recover lost work", None),
        "Interactive Rebase" => (icon::LIST_NUMBERS, "Reorder, squash, and edit commits", None),
        "Bisect" => (icon::BUG_BEETLE, "Find the commit that broke something", None),
        "Patches" => (icon::BANDAIDS, "Saved patches", None),
        "Commit Composer" => (icon::STACK, "Split work into commits", None),
        "Repository Tools" => (icon::WRENCH, "Submodules, hooks, LFS, clean", None),
        "Git Config" => (icon::SLIDERS_HORIZONTAL, "Git's configuration values", None),
        "Credentials" => (icon::KEY, "Tokens and SSH keys", None),
        "Git Console" => (icon::TERMINAL_WINDOW, "Every git command gitem has run", None),
        "Settings" => (icon::GEAR_SIX, "Preferences", None),
        "Visual History" => (icon::CHART_SCATTER, "History of the repository, a folder, or a file", None),
        "Treemap" => (icon::SQUARES_FOUR, "Files by size or recent activity", None),
        "Repository Health" => (icon::HEARTBEAT, "How well this repository is tuned", None),
        "Agent Sessions" => (icon::ROBOT, "Coding agents in these worktrees", None),
        "Review Changes" => (icon::LIST_CHECKS, "A checklist and notes for a change", None),
        "About gitem" => (icon::INFO, "Version and build", None),
        "Fetch…" => (icon::CLOUD_ARROW_DOWN, "Download commits from a remote", None),
        "Pull…" => (icon::ARROW_LINE_DOWN, "Bring upstream commits into this branch", None),
        "Push…" => (icon::ARROW_LINE_UP, "Send this branch's commits to a remote", None),
        "Create Branch…" => (icon::GIT_BRANCH, "", None),
        "Create Tag…" => (icon::TAG, "", None),
        "Stash…" => (icon::ARCHIVE, "Set your changes aside", None),
        "Stage All" => (icon::CHECK_SQUARE, "", None),
        "Add Remote…" | "Add Remote, Checking the URL…" => (icon::CLOUD, "", None),
        "Add Worktree…" => (icon::TREE_STRUCTURE, "", None),
        "Add Submodule…" => (icon::PACKAGE, "", None),
        "Apply Patch…" | "Apply Copied Patch…" => (icon::BANDAIDS, "", None),
        "Merge a Branch…" => (icon::GIT_MERGE, "", None),
        "Rebase onto a Branch…" => (icon::LIST_NUMBERS, "", None),
        "Reset to a Ref…" | "Undo Last Commit" => (icon::ARROW_COUNTER_CLOCKWISE, "", None),
        "Cherry-pick a Commit…" => (icon::GIT_COMMIT, "", None),
        "Clone Repository…" => (icon::DOWNLOAD_SIMPLE, "", None),
        "Initialize Repository…" => (icon::FOLDER_PLUS, "", None),
        "Copy Working Changes as Patch" | "Copy Repository Link" => (icon::COPY, "", None),
        "Publish Repository…" => (icon::CLOUD_ARROW_UP, "", None),
        "Open File from Remote Link…" => (icon::LINK, "", None),
        "Zen Mode (Hide Side Panels)" => (icon::CORNERS_OUT, "", Some("Ctrl+Shift+Z")),
        "Open Terminal Here" => (icon::TERMINAL_WINDOW, "", None),
        "Refresh" => (icon::ARROWS_CLOCKWISE, "Reload this repository", Some("F5")),
        _ => match category {
            "Show" => (icon::GIT_DIFF, "", None),
            "Remote" => (icon::GLOBE, "", None),
            _ => (icon::LIGHTNING, "", None),
        },
    }
}

fn item(category: &'static str, label: impl Into<String>, run: impl FnOnce(&mut Cx) + 'static) -> Item {
    let label = label.into();
    let (glyph, detail, chord) = meta(category, &label);
    Item { category, detail: detail.to_string(), glyph, chord, search: None, record: true, label, run: Box::new(run) }
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

    v.push(item("View", "About gitem", |cx| cx.out.push(crate::action::Action::About)));

    v.push(item("Git", "Fetch…", |cx| {
        let r = cx.data.remotes().iter().map(|x| x.name.clone()).collect();
        cx.form(forms::fetch_form(r, cx.settings.prune_on_fetch));
    }));
    v.push(item("Git", "Pull…", |cx| cx.form(forms::pull_form())));
    v.push(item("Git", "Push…", |cx| {
        let r = cx.data.remotes().iter().map(|x| x.name.clone()).collect();
        let has = cx.status().is_some_and(|s| s.upstream.is_some());
        cx.form(forms::push_form(cx.current_branch(), r, has, cx.settings.confirm_destructive));
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
    v.push(item("Git", "Apply Copied Patch…", |cx| cx.form(apply_copied_patch_form())));
    v.push(item("Git", "Copy Working Changes as Patch", |cx| {
        let repo = cx.repo.clone();
        cx.out.push(Action::Later(Box::new(move || repo.patch_text(&DiffSource::WorkingVsHead).map(crate::action::Later::Copy).map_err(|e| e.to_string()))));
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
            // Outside the queue: it lasts for as long as the tool stays open.
            Ok(vec![Action::RunDetached(ops::difftool_dir(&v.text("a"), &v.text("b")))])
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

/// Whitespace is the patch's own, and a blank context line at the end is a line of it,
/// so nothing is trimmed. A patch only has to end in a newline.
pub fn apply_copied_patch_form() -> forms::FormDialog {
    forms::FormDialog::new("Apply copied patch", "Apply").multiline("patch", "Paste the patch", "").check("three", "Three-way merge on failure", true).on_submit(|v| {
        let mut t = v.text("patch");
        if t.trim().is_empty() {
            return Err("Paste a patch first.".to_string());
        }
        if !t.ends_with('\n') {
            t.push('\n');
        }
        Ok(vec![Action::Run(ops::apply_patch_text(&t, v.on("three")))])
    })
}

fn pick_ref(title: &str, refs: Vec<String>, then: fn(String) -> forms::FormDialog) -> forms::FormDialog {
    forms::FormDialog::new(title, "Next").pick("r", "Branch, tag or SHA", refs, "").on_submit(move |v| {
        let r = v.opt("r").ok_or("Choose a ref.")?;
        Ok(vec![Action::Form(then(r))])
    })
}

/// What the palette lists, chosen by the first character typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    All,
    Commands,
    Branches,
    Files,
    Commits,
}

impl Mode {
    /// The modes with a prefix, in the footer's order.
    const PREFIXED: [(Mode, char, &'static str); 4] = [(Mode::Commands, '>', "commands"), (Mode::Branches, '@', "branches"), (Mode::Files, '/', "files"), (Mode::Commits, ':', "commits")];

    /// Splits typed text into its mode and what's being searched for.
    pub fn parse(text: &str) -> (Mode, &str) {
        let trimmed = text.trim_start();
        for (mode, prefix, _) in Self::PREFIXED {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return (mode, rest.trim());
            }
        }
        (Mode::All, trimmed.trim_end())
    }

    fn glyph(self) -> &'static str {
        match self {
            Mode::All => icon::MAGNIFYING_GLASS,
            Mode::Commands => icon::COMMAND,
            Mode::Branches => icon::GIT_BRANCH,
            Mode::Files => icon::FILE,
            Mode::Commits => icon::GIT_COMMIT,
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Mode::All => "Search commands, branches, and files",
            Mode::Commands => "Run a command",
            Mode::Branches => "Switch to a branch",
            Mode::Files => "Open a file",
            Mode::Commits => "Find a commit by message, author, or SHA",
        }
    }

    fn chip(self) -> Option<&'static str> {
        match self {
            Mode::All => None,
            Mode::Commands => Some("Commands"),
            Mode::Branches => Some("Branches"),
            Mode::Files => Some("Files"),
            Mode::Commits => Some("Commits"),
        }
    }
}

const ROW_H: f32 = 44.0;
const HEADER_H: f32 = 26.0;
const INPUT_H: f32 = 54.0;
const FOOTER_H: f32 = 36.0;
const MAX_LIST_H: f32 = 430.0;
const MAX_RESULTS: usize = 200;
const MAX_RECENT: usize = 6;
const RECENT: &str = "Recent";
/// Sections in the order they appear before anything is typed.
const SECTIONS: [&str; 10] = [RECENT, "View", "Git", "Show", "Remote", "App", "Switch to", "Check out", "Open file", "Commit"];

fn section_title(section: &str) -> &str {
    match section {
        "View" => "Views",
        "Show" => "Compare",
        "Switch to" => "Switch branch",
        "Check out" => "Remote branches",
        "Open file" => "Files",
        "Commit" => "Commits",
        other => other,
    }
}

fn section_tint(section: &str, p: &Palette) -> Color32 {
    match section {
        "View" | "Switch to" => p.series[0],
        "Git" => p.series[2],
        "Show" | "Remote" | "Check out" => p.series[6],
        "Open file" => p.series[3],
        "Commit" => p.series[1],
        _ => p.dim,
    }
}

/// One line of results, with the characters to emphasize.
struct Hit {
    item: Item,
    hl: Vec<u32>,
    section: &'static str,
}

/// Ranks items for a query. With nothing typed they're grouped by section, with
/// recently used commands first. Otherwise the best match wins.
fn rank(items: Vec<Item>, q: &str, recent: &[String]) -> Vec<Hit> {
    let order = |s: &str| SECTIONS.iter().position(|x| *x == s).unwrap_or(SECTIONS.len());
    if q.is_empty() {
        let mut hits: Vec<(usize, usize, Hit)> = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let used = if item.record { recent.iter().position(|r| *r == item.label) } else { None };
                let section = if used.is_some() { RECENT } else { item.category };
                (order(section), used.unwrap_or(i), Hit { item, hl: Vec::new(), section })
            })
            .collect();
        hits.sort_by_key(|x| (x.0, x.1));
        return hits.into_iter().map(|x| x.2).take(MAX_RESULTS).collect();
    }
    let mut hits: Vec<(i32, Hit)> = items
        .into_iter()
        .filter_map(|item| {
            let on_label = fuzzy_match(q, &item.label);
            let (score, hl) = match on_label {
                Some(m) => m,
                None => {
                    let other = item.search.clone().unwrap_or_else(|| format!("{} {}", item.category, item.label));
                    (fuzzy_match(q, &other)?.0 - 5, Vec::new())
                }
            };
            let boost = if item.record && recent.contains(&item.label) { 8 } else { 0 };
            let penalty = if item.category == "Open file" { 5 } else { 0 };
            let section = item.category;
            Some((score + boost - penalty, Hit { item, hl, section }))
        })
        .collect();
    hits.sort_by_key(|x| std::cmp::Reverse(x.0));
    hits.into_iter().map(|x| x.1).take(MAX_RESULTS).collect()
}

/// A label with the matched characters picked out, cut to fit.
fn highlighted(text: &str, hl: &[u32], color: Color32, accent: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let normal = TextFormat { font_id: prop(13.5), color, ..Default::default() };
    let strong = TextFormat { font_id: prop(13.5), color: accent, underline: Stroke::new(1.0, accent), ..Default::default() };
    let mut run = String::new();
    let mut lit = false;
    for (i, ch) in text.chars().enumerate() {
        let h = hl.binary_search(&(i as u32)).is_ok();
        if h != lit && !run.is_empty() {
            job.append(&run, 0.0, if lit { strong.clone() } else { normal.clone() });
            run.clear();
        }
        lit = h;
        run.push(ch);
    }
    if !run.is_empty() {
        job.append(&run, 0.0, if lit { strong } else { normal });
    }
    job
}

/// Key caps right-aligned at `right`. Returns the left edge.
fn paint_keys(ui: &Ui, keys: &str, right: f32, cy: f32, p: &Palette) -> f32 {
    let mut x = right;
    for key in keys.split('+').rev() {
        let g = ui.painter().layout_no_wrap(key.to_string(), prop(11.0), p.dim);
        let w = (g.size().x + 10.0).max(20.0);
        let r = Rect::from_min_size(pos2(x - w, cy - 10.0), vec2(w, 20.0));
        ui.painter().rect_filled(r.translate(vec2(0.0, 1.0)), 5.0, p.button_border);
        ui.painter().rect(r, 5.0, p.button, Stroke::new(1.0, p.button_border), StrokeKind::Inside);
        ui.painter().galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, p.dim);
        x = r.left() - 3.0;
    }
    x
}

fn keys_width(ui: &Ui, keys: &str) -> f32 {
    keys.split('+').map(|k| (ui.painter().layout_no_wrap(k.to_string(), prop(11.0), Color32::WHITE).size().x + 10.0).max(20.0) + 3.0).sum()
}

fn divider(ui: &mut Ui, p: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, p.border);
}

/// The query, then the generations of the refs, status, and file and commit lists.
type RankKey = (String, u64, u64, u64);

#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    files: Task<Vec<String>>,
    file_hits: Vec<String>,
    hits_for: Option<String>,
    commits: Task<Vec<Commit>>,
    commits_gen: Option<u64>,
    started: bool,
    just_opened: bool,
    cursor_to_end: bool,
    scroll_to_selected: bool,
    opened_at: Option<f64>,
    pointer_at_open: Option<egui::Pos2>,
    /// The ranked list, and what it was worked out from.
    ranked: Option<(RankKey, Vec<Hit>)>,
    /// Bumped when the file or commit list arrives.
    data_gen: u64,
}

impl PaletteState {
    pub fn new() -> Self {
        Self { just_opened: true, ..Default::default() }
    }

    /// Opens with text already typed, for screenshots.
    pub fn with_query(query: &str) -> Self {
        Self { query: query.to_string(), cursor_to_end: true, ..Self::new() }
    }

    fn collect(&mut self, ctx: &egui::Context, cx: &mut Cx, mode: Mode, q: &str) -> Vec<Item> {
        let mut items = Vec::new();
        if matches!(mode, Mode::All | Mode::Commands) {
            items = commands();
        }
        if matches!(mode, Mode::All | Mode::Branches) {
            let current = cx.current_branch();
            for b in cx.data.local_branches() {
                if Some(&b.name) != current.as_ref() {
                    let n = b.name.clone();
                    let track = match (b.ahead, b.behind) {
                        (0, 0) => String::new(),
                        (a, 0) => format!("↑{a}  "),
                        (0, x) => format!("↓{x}  "),
                        (a, x) => format!("↑{a} ↓{x}  "),
                    };
                    let mut it = item("Switch to", b.name.clone(), move |cx| cx.run(ops::checkout(&n)));
                    (it.glyph, it.detail, it.record) = (icon::GIT_BRANCH, format!("{track}{}", b.subject), false);
                    items.push(it);
                }
            }
            for b in cx.data.remote_branches() {
                let n = b.name.clone();
                let mut it = item("Check out", b.name.clone(), move |cx| {
                    let remotes = cx.data.remote_names();
                    cx.run(ops::checkout_remote(&n, &remotes));
                });
                (it.glyph, it.detail, it.record) = (icon::CLOUD, b.subject.clone(), false);
                items.push(it);
            }
        }
        if matches!(mode, Mode::All | Mode::Files) {
            if !self.started {
                self.started = true;
                // `status` has the untracked ones already, and looking for them again would
                // be the slow part.
                let untracked: Vec<String> = cx.status().map(|s| s.entries.iter().filter(|e| e.worktree == gitem_core::FileStatus::Untracked).map(|e| e.path.clone()).collect()).unwrap_or_default();
                self.files.spawn_git(ctx, cx.repo, move |r| {
                    r.tracked_files(None).map(|mut files| {
                        files.extend(untracked);
                        files
                    })
                });
            }
            // Repositories can hold many thousands of files: only rescan on a new query.
            let key = format!("{mode:?} {q}");
            if self.hits_for.as_deref() != Some(&key) && self.files.get().is_some() {
                self.hits_for = Some(key);
                self.file_hits.clear();
                if let Some(files) = self.files.get().filter(|_| mode == Mode::Files || !q.is_empty()) {
                    let mut scored: Vec<(i32, &String)> = files.iter().filter_map(|f| fuzzy_score(q, f).map(|s| (s, f))).collect();
                    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                    self.file_hits = scored.into_iter().take(60).map(|x| x.1.clone()).collect();
                }
            }
            for f in &self.file_hits {
                let path = f.clone();
                let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
                let mut it = item("Open file", name, move |cx| cx.open(Tab::file(&path, None, None)));
                (it.glyph, it.detail, it.search, it.record) = (icon::FILE, dir.to_string(), Some(f.clone()), false);
                items.push(it);
            }
        }
        if mode == Mode::Commits {
            if self.commits_gen != Some(cx.data.refs_gen) {
                self.commits_gen = Some(cx.data.refs_gen);
                let r = cx.repo.clone();
                self.commits.spawn(ctx, move || r.log(&LogQuery { revs: vec!["HEAD".into()], limit: 2000, ..Default::default() }));
            }
            for c in self.commits.get().into_iter().flatten() {
                let sha = c.sha.clone();
                let mut it = item("Commit", c.subject.clone(), move |cx| cx.inspect(&sha));
                let when = crate::widgets::time_label(c.commit_time, cx.settings.date_relative);
                it.detail = format!("{}, {}, {when}", c.short(), c.author_name);
                it.search = Some(format!("{} {} {}", c.subject, c.sha, c.author_name));
                (it.glyph, it.record) = (icon::GIT_COMMIT, false);
                items.push(it);
            }
        }
        items
    }

    /// Draws the palette. Returns true when it should close.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut Cx) -> bool {
        let p = cx.pal.clone();
        if self.files.poll() | self.commits.poll() {
            self.data_gen += 1;
        }
        let now = ctx.input(|i| i.time);
        let t = ((now - *self.opened_at.get_or_insert(now)) / 0.14).clamp(0.0, 1.0) as f32;
        let ease = 1.0 - (1.0 - t).powi(3);
        if t < 1.0 {
            ctx.request_repaint();
        }

        // Keys first, before the text field sees them.
        let (mut down, mut up, mut page_down, mut page_up, mut enter, mut esc) = (false, false, false, false, false, false);
        ctx.input_mut(|i| {
            use egui::{Key, Modifiers as M};
            esc = i.consume_key(M::NONE, Key::Escape);
            down = i.consume_key(M::NONE, Key::ArrowDown) || i.consume_key(M::CTRL, Key::J) || i.consume_key(M::CTRL, Key::N);
            up = i.consume_key(M::NONE, Key::ArrowUp) || i.consume_key(M::CTRL, Key::K);
            page_down = i.consume_key(M::NONE, Key::PageDown);
            page_up = i.consume_key(M::NONE, Key::PageUp);
            enter = i.consume_key(M::NONE, Key::Enter);
            // Focus stays in the search box.
            i.consume_key(M::NONE, Key::Tab);
            i.consume_key(M::SHIFT, Key::Tab);
        });
        if esc {
            return true;
        }

        let text = self.query.clone();
        let (mode, q) = Mode::parse(&text);
        let key = (text.clone(), cx.data.refs_gen, cx.data.status_gen, self.data_gen);
        let mut hits = match self.ranked.take() {
            Some((k, hits)) if k == key => hits,
            _ => {
                let items = self.collect(ctx, cx, mode, q);
                rank(items, q, &cx.settings.palette_recent)
            }
        };
        let n = hits.len();
        const PAGE: usize = 8;
        self.selected = self.selected.min(n.saturating_sub(1));
        if n > 0 {
            if down {
                self.selected = (self.selected + 1) % n;
            }
            if up {
                self.selected = (self.selected + n - 1) % n;
            }
            if page_down {
                self.selected = (self.selected + PAGE).min(n - 1);
            }
            if page_up {
                self.selected = self.selected.saturating_sub(PAGE);
            }
        }
        self.scroll_to_selected |= down || up || page_down || page_up;
        let mut chosen = enter.then_some(self.selected);

        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).clamp(320.0, 660.0);
        let top = (screen.height() * 0.11).clamp(24.0, 110.0) - 8.0 * (1.0 - ease);
        let id = egui::Id::new("gitem-palette");
        let frame = egui::Frame::new().fill(p.panel).stroke(Stroke::new(1.0, p.button_border)).corner_radius(12).shadow(egui::Shadow { offset: [0, 14], blur: 40, spread: 0, color: Color32::from_black_alpha(if p.dark { 150 } else { 60 }) });
        let backdrop = Color32::from_black_alpha(((if p.dark { 130.0 } else { 70.0 }) * ease) as u8);
        let resp = egui::Modal::new(id).area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, top))).frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
            ui.set_width(width);
            ui.set_opacity(ease);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            self.input_row(ui, mode, width, &p);
            divider(ui, &p);
            self.list(ui, &hits, q, mode, width, &p, &mut chosen);
            divider(ui, &p);
            self.footer(ui, mode, width, &p);
        });
        if self.query != text {
            self.selected = 0;
            self.scroll_to_selected = true;
        }
        if self.query == text && chosen.is_none_or(|i| i >= hits.len()) {
            self.ranked = Some((key, hits));
            return resp.backdrop_response.clicked();
        }
        if let Some(i) = chosen
            && i < hits.len()
        {
            let hit = hits.swap_remove(i);
            if hit.item.record {
                let label = hit.item.label.clone();
                cx.out.push(Action::Settings(Box::new(move |s| {
                    s.palette_recent.retain(|x| *x != label);
                    s.palette_recent.insert(0, label);
                    s.palette_recent.truncate(MAX_RECENT);
                })));
            }
            (hit.item.run)(cx);
            return true;
        }
        resp.backdrop_response.clicked()
    }

    fn input_row(&mut self, ui: &mut Ui, mode: Mode, width: f32, p: &Palette) {
        let (rect, _) = ui.allocate_exact_size(vec2(width, INPUT_H), Sense::hover());
        let y = rect.center().y;
        ui.painter().text(pos2(rect.left() + 27.0, y), Align2::CENTER_CENTER, mode.glyph(), prop(19.0), p.accent);
        let mut x = rect.left() + 48.0;
        if let Some(label) = mode.chip() {
            let g = ui.painter().layout_no_wrap(label.to_string(), prop(11.5), p.accent);
            let chip = Rect::from_min_size(pos2(x, y - 11.0), vec2(g.size().x + 16.0, 22.0));
            ui.painter().rect_filled(chip, 11.0, p.accent.gamma_multiply(0.18));
            ui.painter().galley(pos2(chip.left() + 8.0, y - g.size().y / 2.0), g, p.accent);
            x = chip.right() + 8.0;
        }
        let esc_left = paint_keys(ui, "Esc", rect.right() - 16.0, y, p);
        let edit = Rect::from_min_max(pos2(x, rect.top() + 8.0), pos2(esc_left - 10.0, rect.bottom() - 8.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(edit).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let out = egui::TextEdit::singleline(&mut self.query).id(egui::Id::new("gitem-palette-input")).frame(egui::Frame::NONE).font(prop(16.0)).text_color(p.text).hint_text(RichText::new(mode.placeholder()).color(p.faint).size(15.0)).desired_width(edit.width()).margin(egui::Margin::symmetric(0, 4)).show(&mut child);
        if self.just_opened || !out.response.has_focus() {
            self.just_opened = false;
            out.response.request_focus();
        }
        if std::mem::take(&mut self.cursor_to_end) {
            let mut state = out.state.clone();
            let end = egui::text::CCursor::new(self.query.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), out.response.id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(&mut self, ui: &mut Ui, hits: &[Hit], q: &str, mode: Mode, width: f32, p: &Palette, chosen: &mut Option<usize>) {
        if hits.is_empty() {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 120.0), Sense::hover());
            let (title, hint) = match mode {
                Mode::Commits if self.commits.busy() => ("Loading commits…", ""),
                Mode::Files if self.files.busy() => ("Loading files…", ""),
                _ => ("No matches", "Check the spelling, or try fewer letters."),
            };
            ui.painter().text(rect.center() - vec2(0.0, 22.0), Align2::CENTER_CENTER, icon::MAGNIFYING_GLASS, prop(24.0), p.faint);
            ui.painter().text(rect.center() + vec2(0.0, 8.0), Align2::CENTER_CENTER, title, prop(13.0), p.dim);
            ui.painter().text(rect.center() + vec2(0.0, 27.0), Align2::CENTER_CENTER, hint, prop(11.5), p.faint);
            return;
        }
        let flat = !q.is_empty();
        let mut total = 8.0 + if flat { HEADER_H } else { 0.0 };
        let mut last = None;
        for h in hits {
            if !flat && last != Some(h.section) {
                total += HEADER_H;
                last = Some(h.section);
            }
            total += ROW_H;
        }
        let height = total.min(MAX_LIST_H);
        // Hover only selects once the pointer moves, not because the palette opened under it.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let moved = match (self.pointer_at_open, pointer) {
            (None, Some(at)) => {
                self.pointer_at_open = Some(at);
                false
            }
            (Some(a), Some(b)) => a.distance(b) > 3.0,
            _ => false,
        };
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let selected = self.selected;
        let mut hovered = None;
        let header = |ui: &mut Ui, title: &str| {
            let (r, _) = ui.allocate_exact_size(vec2(width, HEADER_H), Sense::hover());
            ui.painter().text(pos2(r.left() + 20.0, r.bottom() - 6.0), Align2::LEFT_BOTTOM, title, prop(11.5), p.faint);
        };
        egui::ScrollArea::vertical().id_salt("palette-results").max_height(height).min_scrolled_height(height).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(width);
            ui.add_space(4.0);
            if flat {
                header(ui, "Best matches");
            }
            let mut last = None;
            for (i, h) in hits.iter().enumerate() {
                if !flat && last != Some(h.section) {
                    header(ui, section_title(h.section));
                    last = Some(h.section);
                }
                let (outer, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, i == selected, h.item.label.as_str()));
                if scroll_to && i == selected {
                    ui.scroll_to_rect(outer.expand2(vec2(0.0, 6.0)), None);
                }
                if moved && resp.hovered() && i != selected {
                    hovered = Some(i);
                }
                if resp.clicked() {
                    *chosen = Some(i);
                }
                if !ui.is_rect_visible(outer) {
                    continue;
                }
                let r = outer.shrink2(vec2(8.0, 2.0));
                if i == selected {
                    ui.painter().rect_filled(r, 8.0, p.accent.gamma_multiply(0.2));
                    ui.painter().rect_filled(Rect::from_min_size(pos2(r.left(), r.center().y - 9.0), vec2(3.0, 18.0)), 2.0, p.accent);
                }
                row(ui, &h.item, &h.hl, flat, i == selected, r, p);
            }
            ui.add_space(4.0);
        });
        if let Some(i) = hovered {
            self.selected = i;
        }
    }

    fn footer(&mut self, ui: &mut Ui, mode: Mode, width: f32, p: &Palette) {
        let (rect, _) = ui.allocate_exact_size(vec2(width, FOOTER_H), Sense::hover());
        ui.painter().rect_filled(rect, egui::CornerRadius { nw: 0, ne: 0, sw: 12, se: 12 }, p.panel2);
        let y = rect.center().y;
        let mut x = rect.left() + 16.0;
        for (keys, label) in [("↑+↓", "Move"), ("Enter", "Run")] {
            let w = keys_width(ui, keys);
            paint_keys(ui, keys, x + w, y, p);
            x += w + 3.0;
            let g = ui.painter().text(pos2(x, y), Align2::LEFT_CENTER, label, prop(11.5), p.faint);
            x = g.right() + 12.0;
        }
        let mut right = rect.right() - 10.0;
        for (m, prefix, legend) in Mode::PREFIXED.iter().rev() {
            let label = format!("{prefix} {legend}");
            let g = ui.painter().layout_no_wrap(label.clone(), prop(11.5), p.dim);
            let r = Rect::from_min_size(pos2(right - g.size().x - 14.0, y - 11.0), vec2(g.size().x + 14.0, 22.0));
            if r.left() < x {
                break;
            }
            let resp = ui.interact(r, ui.id().with(("palette-mode", *prefix)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let on = mode == *m;
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label.as_str()));
            if on {
                ui.painter().rect_filled(r, 11.0, p.accent.gamma_multiply(0.18));
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 11.0, p.hover);
            }
            ui.painter().rect_stroke(r, 11.0, Stroke::new(1.0, if on { p.accent } else { p.border }), StrokeKind::Inside);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, prop(11.5), if on { p.accent } else { p.dim });
            if resp.clicked() {
                let (_, q) = Mode::parse(&self.query);
                self.query = if on { q.to_string() } else { format!("{prefix}{q}") };
                self.cursor_to_end = true;
            }
            right = r.left() - 5.0;
        }
    }
}

fn row(ui: &Ui, it: &Item, hl: &[u32], show_section: bool, selected: bool, r: Rect, p: &Palette) {
    let tint = section_tint(it.category, p);
    let tile = Rect::from_center_size(pos2(r.left() + 25.0, r.center().y), vec2(28.0, 28.0));
    ui.painter().rect_filled(tile, 7.0, tint.gamma_multiply(0.18));
    ui.painter().text(tile.center(), Align2::CENTER_CENTER, it.glyph, prop(16.0), tint);
    let mut right = r.right() - 12.0;
    if let Some(keys) = it.chord {
        right = paint_keys(ui, keys, right, r.center().y, p) - 6.0;
    }
    if show_section {
        let g = ui.painter().layout_no_wrap(section_title(it.category).to_string(), prop(11.0), p.faint);
        let at = pos2(right - g.size().x, r.center().y - g.size().y / 2.0);
        right = at.x - 10.0;
        ui.painter().galley(at, g, p.faint);
    }
    let left = tile.right() + 12.0;
    let width = (right - left).max(40.0);
    let color = if selected { p.text } else { p.text.gamma_multiply(0.92) };
    let title = ui.painter().layout_job(highlighted(&it.label, hl, color, p.accent, width));
    let has_detail = !it.detail.is_empty();
    let y = if has_detail { r.center().y - 9.0 } else { r.center().y };
    ui.painter().galley(pos2(left, y - title.size().y / 2.0), title, color);
    if has_detail {
        let job = LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(width), ..LayoutJob::simple_singleline(it.detail.clone(), prop(11.5), p.faint) };
        let g = ui.painter().layout_job(job);
        ui.painter().galley(pos2(left, r.center().y + 9.0 - g.size().y / 2.0), g, p.faint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.item.label.as_str()).collect()
    }

    #[test]
    fn prefixes_choose_the_mode() {
        assert_eq!(Mode::parse("fetch"), (Mode::All, "fetch"));
        assert_eq!(Mode::parse(">  fetch "), (Mode::Commands, "fetch"));
        assert_eq!(Mode::parse("@main"), (Mode::Branches, "main"));
        assert_eq!(Mode::parse("/src/a.rs"), (Mode::Files, "src/a.rs"));
        assert_eq!(Mode::parse(":fix"), (Mode::Commits, "fix"));
        assert_eq!(Mode::parse(""), (Mode::All, ""));
    }

    #[test]
    fn with_nothing_typed_recent_commands_lead_in_the_order_used() {
        let recent = vec!["Bisect".to_string(), "Reflog".to_string()];
        let hits = rank(commands(), "", &recent);
        assert_eq!(labels(&hits)[..2], ["Bisect", "Reflog"]);
        assert_eq!(hits[0].section, RECENT);
        assert_eq!(hits[2].section, "View");
        assert_eq!(hits.iter().filter(|h| h.item.label == "Bisect").count(), 1);
    }

    #[test]
    fn sections_stay_together_even_when_commands_are_listed_apart() {
        let hits = rank(commands(), "", &[]);
        let mut seen: Vec<&str> = Vec::new();
        for h in &hits {
            if seen.last() != Some(&h.section) {
                assert!(!seen.contains(&h.section), "{} appears twice", h.section);
                seen.push(h.section);
            }
        }
    }

    #[test]
    fn typing_ranks_and_highlights_the_label() {
        let hits = rank(commands(), "reflog", &[]);
        assert_eq!(hits[0].item.label, "Reflog");
        assert_eq!(hits[0].hl, (0..6).collect::<Vec<u32>>());
        assert!(rank(commands(), "zzzzqq", &[]).is_empty());
    }

    #[test]
    fn a_recently_used_command_wins_a_close_match() {
        let plain = rank(commands(), "push", &[]);
        assert_eq!(plain[0].item.label, "Push…");
        let hits = rank(commands(), "re", &["Reset to a Ref…".to_string()]);
        assert!(labels(&hits).iter().position(|l| *l == "Reset to a Ref…").unwrap() < labels(&plain).len());
    }

    #[test]
    fn commands_know_their_shortcuts_and_have_icons() {
        let all = commands();
        let by = |l: &str| all.iter().find(|i| i.label == l).unwrap();
        assert_eq!(by("Commit Graph").chord, Some("Ctrl+Shift+H"));
        assert_eq!(by("Changes").chord, Some("Ctrl+Shift+G"));
        assert_eq!(by("Refresh").chord, Some("F5"));
        assert!(all.iter().all(|i| !i.glyph.is_empty()));
    }
}
