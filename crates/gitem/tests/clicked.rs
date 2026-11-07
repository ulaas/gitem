//! Menu items and forms clicked through the real UI against real git in throwaway repositories,
//! where earlier tests only ran what's behind them. Clicks are kittest's synthetic input. Fake
//! external tools go in the scratch repository's own config, marked trusted in this binary's
//! scratch settings.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::settings::Settings;

const PATIENCE: Duration = Duration::from_secs(180);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir(&root).unwrap();
        let mut git = GitExe::discover(None).expect("git");
        git.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write("a.txt", "one\n");
        f.commit("first");
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim().to_string()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }
    /// A bare repository beside this one, as a remote. Relative, as a canonical Windows path has a
    /// prefix git can't take as an argument.
    fn remote(&self, name: &str) -> String {
        let at = format!("../{name}.git");
        self.git(&["init", "-q", "--bare", "-b", "main", &at]);
        self.git(&["remote", "add", name, &at]);
        at
    }
    fn at_remote(&self, name: &str, args: &[&str]) -> String {
        let dir = self.root.parent().unwrap().join(format!("{name}.git"));
        let all: Vec<&str> = ["--git-dir=."].into_iter().chain(args.iter().copied()).collect();
        self.git.run(Some(&dir), &all).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text().trim().to_string()
    }
    /// Its config is trusted before it opens, as answering the question would.
    fn trust(&self) {
        let mut repo = gitem_core::Repo::open(self.git.clone(), &self.root).unwrap();
        repo.set_trusted(true).unwrap();
    }
    fn harness(&self, tweak: impl FnOnce(&mut Settings)) -> Harness<'static, GitemApp> {
        let ctx = egui::Context::default();
        let mut settings = Settings { details_on_open: false, ..Default::default() };
        tweak(&mut settings);
        let mut app = GitemApp::with_settings(&ctx, settings);
        app.git = Ok(self.git.clone());
        let root = self.root.clone();
        let mut opened = false;
        let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
        let mut h = builder.build_ui_state(
            move |ui, app: &mut GitemApp| {
                if !opened {
                    opened = true;
                    let c = ui.ctx().clone();
                    app.open_repo(&c, &root, false);
                }
                app.frame(ui);
            },
            app,
        );
        settle(&mut h);
        h
    }
}

fn settle(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut frames = 0;
    while frames < 5 {
        h.step();
        let loaded = h.state().repos.first().is_none_or(|r| r.data.status.get().is_some() && r.data.refs.get().is_some());
        frames = if loaded && gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() { frames + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}. Errors showing: {:?}", h.state().errors_showing());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_label(h: &mut Harness<GitemApp>, label: &str) {
    wait_for(h, label, |h| h.query_all_by_label(label).next().is_some());
}

fn wait_for_text(h: &mut Harness<GitemApp>, text: &str) {
    wait_for(h, text, |h| h.query_all_by_label_contains(text).next().is_some());
}

/// A dialog takes no clicks for a moment after it appears. A person never notices.
fn a_moment(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The last of that name, which is the dialog's or menu's when something behind it has one too.
fn click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label(h, label);
    a_moment(h);
    h.query_all_by_label(label).last().unwrap().click();
    h.step();
    h.step();
}

/// The first, which is the row rather than a tooltip or a menu.
fn right_click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label(h, label);
    a_moment(h);
    h.query_all_by_label(label).next().unwrap().click_secondary();
    h.step();
    h.step();
}

fn run_command(h: &mut Harness<GitemApp>, name: &str, form_title: &str) {
    h.state_mut().open_palette_with(&format!(">{name}"));
    h.step();
    h.key_press(egui::Key::Enter);
    wait_for_label(h, form_title);
}

fn fill(h: &mut Harness<GitemApp>, key: &str, value: &str) {
    h.state_mut().form_set_for_test(key, value);
    h.step();
}

fn open(h: &mut Harness<GitemApp>, tab: Tab) {
    h.state_mut().repos[0].open_tab(tab);
    h.step();
    settle(h);
}

fn no_errors(h: &Harness<GitemApp>) {
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
}

/// Toolbar Undo, then the question it asks.
fn undo_through_the_toolbar(h: &mut Harness<GitemApp>, label: &str) {
    click(h, label);
    wait_for(h, "the question", |h| h.query_all_by_label(label).count() > 1);
    click(h, label);
    settle(h);
    no_errors(h);
}

fn commands_with(h: &Harness<GitemApp>, what: &str) -> Vec<String> {
    h.state().repos[0].repo.git.log.lock().unwrap().iter().filter(|c| c.command.contains(what)).map(|c| c.command.clone()).collect()
}

// ---- the palette

#[test]
fn stash_apply_and_pop_from_the_palette() {
    let f = Fx::new();
    f.write("a.txt", "saved\n");
    f.git(&["stash", "push", "-q", "-m", "work"]);
    let mut h = f.harness(|_| {});
    let name = {
        let st = &h.state().repos[0].data.stashes()[0];
        format!("{}: {}", st.selector, st.message)
    };
    run_command(&mut h, "Apply a Stash", "Apply a stash");
    fill(&mut h, "s", &name);
    click(&mut h, "Apply");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read("a.txt"), "saved\n");
    assert_eq!(f.git(&["stash", "list", "--format=%gs"]).lines().count(), 1, "apply keeps it");

    f.git(&["checkout", "--", "a.txt"]);
    settle(&mut h);
    run_command(&mut h, "Pop a Stash", "Pop a stash");
    fill(&mut h, "s", &name);
    click(&mut h, "Pop");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read("a.txt"), "saved\n");
    assert_eq!(f.git(&["stash", "list"]), "", "pop takes it off");
}

#[test]
fn tag_delete_and_push_from_the_palette() {
    let f = Fx::new();
    f.remote("origin");
    f.git(&["tag", "v1"]);
    f.git(&["tag", "v2"]);
    let mut h = f.harness(|_| {});
    run_command(&mut h, "Push a Tag", "Push a tag");
    fill(&mut h, "name", "v1");
    click(&mut h, "Next");
    wait_for_label(&mut h, "Push tag v1");
    click(&mut h, "Push");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("origin", &["tag"]), "v1");

    run_command(&mut h, "Push All Tags", "Push all tags");
    click(&mut h, "Push");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("origin", &["tag"]), "v1\nv2");

    run_command(&mut h, "Delete a Tag", "Delete a tag");
    fill(&mut h, "name", "v2");
    click(&mut h, "Next");
    wait_for_text(&mut h, "Delete tag v2 from this repository?");
    click(&mut h, "Delete");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["tag"]), "v1");
}

#[test]
fn rename_and_worktree_removal_from_the_palette() {
    let f = Fx::new();
    f.git(&["branch", "old-name"]);
    f.git(&["worktree", "add", "-q", "-b", "wt-branch", "../wt"]);
    let mut h = f.harness(|_| {});
    run_command(&mut h, "Rename a Branch", "Rename a branch");
    fill(&mut h, "name", "old-name");
    click(&mut h, "Next");
    wait_for_label(&mut h, "Rename old-name");
    fill(&mut h, "name", "new-name");
    click(&mut h, "Rename");
    settle(&mut h);
    no_errors(&h);
    assert!(f.git(&["branch", "--list", "new-name"]).contains("new-name"));
    assert_eq!(f.git(&["branch", "--list", "old-name"]), "");

    let path = h.state().repos[0].data.worktrees().iter().find(|w| !w.is_main && !w.is_current).expect("the other worktree").path.clone();
    run_command(&mut h, "Remove a Worktree", "Remove a worktree");
    fill(&mut h, "path", &path);
    click(&mut h, "Next");
    wait_for_text(&mut h, "Remove the worktree at");
    click(&mut h, "Remove");
    settle(&mut h);
    no_errors(&h);
    assert!(!f.git(&["worktree", "list"]).contains("wt-branch"));
}

#[test]
fn a_palette_command_with_nothing_to_act_on_says_so() {
    let f = Fx::new();
    let mut h = f.harness(|_| {});
    h.state_mut().open_palette_with(">Remove a Worktree");
    h.step();
    h.key_press(egui::Key::Enter);
    wait_for_text(&mut h, "no other worktree to remove");
}

// ---- menus on a branch and on a commit

#[test]
fn fast_forward_from_a_branchs_menu_moves_only_that_branch() {
    let f = Fx::new();
    f.remote("origin");
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "s\n");
    f.commit("on side");
    f.git(&["push", "-q", "-u", "origin", "side"]);
    f.write("s.txt", "later\n");
    let later = f.commit("later on side");
    f.git(&["push", "-q", "origin", "side"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.git(&["switch", "-q", "main"]);
    let main = f.git(&["rev-parse", "main"]);
    let mut h = f.harness(|_| {});
    right_click(&mut h, "side");
    click(&mut h, "Fast-forward to upstream");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["rev-parse", "side"]), later);
    assert_eq!(f.git(&["rev-parse", "main"]), main);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "main");
}

/// main with a commit changing a.txt, and side changing it otherwise.
fn conflicting_side(f: &Fx) {
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("a.txt", "from side\n");
    f.commit("on side");
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "from main\n");
    f.commit("on main");
}

#[test]
fn the_merge_form_takes_a_side_and_a_message() {
    let f = Fx::new();
    conflicting_side(&f);
    let mut h = f.harness(|_| {});
    right_click(&mut h, "side");
    click(&mut h, "Merge into current…");
    wait_for_label(&mut h, "Merge side into the current branch");
    fill(&mut h, "favour", "Take this branch's side (-X ours)");
    fill(&mut h, "message", "Kept ours on purpose");
    click(&mut h, "Merge");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read("a.txt"), "from main\n");
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Kept ours on purpose");
    assert_eq!(f.git(&["rev-list", "--parents", "-n1", "HEAD"]).split(' ').count(), 3, "a merge commit");
}

#[test]
fn cherry_pick_and_revert_from_a_commits_menu_and_their_undo() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "s\n");
    let picked = f.commit("on side");
    f.git(&["switch", "-q", "main"]);
    let before = f.git(&["rev-parse", "HEAD"]);
    let mut h = f.harness(|_| {});
    right_click(&mut h, "on side");
    click(&mut h, "Cherry-pick…");
    wait_for_label(&mut h, "Cherry-pick 1 commit");
    fill(&mut h, "x", "1");
    click(&mut h, "Cherry-pick");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read("s.txt"), "s\n");
    assert!(f.git(&["log", "-1", "--format=%b"]).contains(&format!("cherry picked from commit {picked}")));
    undo_through_the_toolbar(&mut h, "Undo cherry-pick");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert!(!f.root.join("s.txt").exists());

    f.write("r.txt", "r\n");
    let target = f.commit("to revert");
    settle(&mut h);
    right_click(&mut h, "to revert");
    click(&mut h, "Revert…");
    wait_for_label(&mut h, &format!("Revert {}", &target[..10]));
    click(&mut h, "Revert");
    settle(&mut h);
    no_errors(&h);
    assert!(!f.root.join("r.txt").exists());
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Revert \"to revert\"");
    undo_through_the_toolbar(&mut h, "Undo revert");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), target);
    assert_eq!(f.read("r.txt"), "r\n");
}

#[test]
fn the_graphs_selection_is_cherry_picked_oldest_first_and_reverted_newest_first() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "1\n");
    f.commit("side one");
    f.write("s.txt", "2\n");
    f.commit("side two");
    f.git(&["switch", "-q", "main"]);
    let mut h = f.harness(|_| {});
    wait_for_label(&mut h, "side one");
    a_moment(&mut h);
    h.query_all_by_label("side one").next().unwrap().click();
    h.step();
    h.step();
    h.query_all_by_label("side two").next().unwrap().click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    right_click(&mut h, "side two");
    click(&mut h, "Cherry-pick 2 selected commits…");
    wait_for_label(&mut h, "Cherry-pick 2 commits");
    click(&mut h, "Cherry-pick");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "side two\nside one", "in history order, or the second would conflict");
    assert_eq!(f.read("s.txt"), "2\n");

    // Both of them again, on main this time.
    let tips: Vec<String> = f.git(&["log", "-2", "--format=%H"]).lines().map(String::from).collect();
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::Form(gitem::forms::revert_many_form(tips)));
    wait_for_label(&mut h, "Revert 2 commits");
    click(&mut h, "Revert");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "Revert \"side one\"\nRevert \"side two\"", "newest first");
    assert!(!f.root.join("s.txt").exists());
}

#[test]
fn the_graphs_revert_of_a_selection_is_on_its_menu() {
    let f = Fx::new();
    f.write("r.txt", "1\n");
    f.commit("rev one");
    f.write("r.txt", "2\n");
    f.commit("rev two");
    let mut h = f.harness(|_| {});
    wait_for_label(&mut h, "rev one");
    a_moment(&mut h);
    h.query_all_by_label("rev one").next().unwrap().click();
    h.step();
    h.step();
    h.query_all_by_label("rev two").next().unwrap().click_modifiers(egui::Modifiers::COMMAND);
    h.step();
    h.step();
    right_click(&mut h, "rev one");
    click(&mut h, "Revert 2 selected commits…");
    wait_for_label(&mut h, "Revert 2 commits");
    click(&mut h, "Revert");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "Revert \"rev one\"\nRevert \"rev two\"", "newest first");
}

#[test]
fn a_merge_commit_is_cherry_picked_with_a_parent_named_in_the_form() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("on topic");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    f.git(&["merge", "-q", "--no-ff", "-m", "Merged topic", "topic"]);
    let merge = f.git(&["rev-parse", "HEAD"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.git(&["branch", "keep", &merge]);
    let mut h = f.harness(|_| {});
    right_click(&mut h, "Merged topic");
    click(&mut h, "Cherry-pick…");
    wait_for_label(&mut h, "Cherry-pick 1 commit");
    fill(&mut h, "mainline", "x");
    click(&mut h, "Cherry-pick");
    wait_for_text(&mut h, "The parent is a number");
    fill(&mut h, "mainline", "1");
    click(&mut h, "Cherry-pick");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read("t.txt"), "t\n");
    assert_eq!(f.git(&["rev-list", "--parents", "-n1", "HEAD"]).split(' ').count(), 2, "one parent: a pick, not a merge");
}

#[test]
fn a_range_is_cherry_picked_and_a_commit_reverted_from_the_palette() {
    let f = Fx::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("s.txt", "1\n");
    f.commit("side one");
    f.write("s.txt", "2\n");
    f.commit("side two");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("on main");
    let mut h = f.harness(|_| {});
    run_command(&mut h, "Cherry-pick a Range", "Cherry-pick a range");
    fill(&mut h, "from", &base);
    fill(&mut h, "to", "side");
    click(&mut h, "Next");
    wait_for_label(&mut h, "Cherry-pick 1 commit");
    click(&mut h, "Cherry-pick");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "side two\nside one");

    run_command(&mut h, "Revert a Commit", "Revert which commit?");
    fill(&mut h, "r", "HEAD");
    click(&mut h, "Next");
    wait_for_label(&mut h, "Revert HEAD");
    click(&mut h, "Revert");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Revert \"side two\"");
    assert_eq!(f.read("s.txt"), "1\n");
}

// ---- undo for a pull

#[test]
fn a_pull_from_the_toolbar_is_undone_from_the_toolbar() {
    let f = Fx::new();
    f.remote("origin");
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.write("b.txt", "b\n");
    f.commit("upstream work");
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    let before = f.git(&["rev-parse", "HEAD"]);
    let mut h = f.harness(|_| {});
    click(&mut h, "Pull 1");
    settle(&mut h);
    no_errors(&h);
    assert!(f.root.join("b.txt").exists());
    undo_through_the_toolbar(&mut h, "Undo pull");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert!(!f.root.join("b.txt").exists());
}

// ---- tag forms

#[test]
fn tag_forms_choose_a_remote_replace_and_delete_there() {
    let f = Fx::new();
    f.remote("origin");
    f.remote("other");
    f.git(&["tag", "v1"]);
    let first = f.git(&["rev-parse", "v1"]);
    let mut h = f.harness(|_| {});
    click(&mut h, "Tags section");

    right_click(&mut h, "v1");
    click(&mut h, "Push to…");
    wait_for_label(&mut h, "Push tag v1");
    fill(&mut h, "remote", "other");
    click(&mut h, "Push");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("other", &["tag"]), "v1");
    assert_eq!(f.at_remote("origin", &["tag"]), "", "only the one chosen");

    // Moved here: without Replace the remote says no, and with it the tag goes.
    f.write("a.txt", "two\n");
    let second = f.commit("second");
    f.git(&["tag", "-f", "v1", &second]);
    settle(&mut h);
    right_click(&mut h, "v1");
    click(&mut h, "Push to…");
    wait_for_label(&mut h, "Push tag v1");
    fill(&mut h, "remote", "other");
    click(&mut h, "Push");
    wait_for(&mut h, "the refusal", |h| !h.state().errors_showing().is_empty());
    assert_eq!(f.at_remote("other", &["rev-parse", "v1"]), first);
    h.state_mut().errors_clear_for_test();
    settle(&mut h);
    right_click(&mut h, "v1");
    click(&mut h, "Push to…");
    wait_for_label(&mut h, "Push tag v1");
    fill(&mut h, "remote", "other");
    fill(&mut h, "force", "1");
    click(&mut h, "Push");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("other", &["rev-parse", "v1"]), second);

    f.git(&["tag", "v2"]);
    settle(&mut h);
    right_click(&mut h, "v2");
    click(&mut h, "Push all tags…");
    wait_for_label(&mut h, "Push all tags");
    fill(&mut h, "remote", "origin");
    click(&mut h, "Push");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("origin", &["tag"]), "v1\nv2");

    right_click(&mut h, "v2");
    click(&mut h, "Delete on a remote…");
    wait_for_label(&mut h, "Delete tag v2 on a remote");
    fill(&mut h, "remote", "origin");
    click(&mut h, "Delete on remote");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.at_remote("origin", &["tag"]), "v1");
    assert_eq!(f.git(&["tag"]), "v1\nv2", "it stays here");
}

// ---- external tools

#[test]
fn the_external_diff_tool_runs_from_a_files_menu() {
    let f = Fx::new();
    f.git(&["config", "difftool.fake.cmd", "cat \"$REMOTE\" > ../from-the-tool.txt"]);
    f.write("a.txt", "edited\n");
    f.trust();
    let mut h = f.harness(|s| s.external_diff_tool = "fake".to_string());
    open(&mut h, Tab::changes());
    right_click(&mut h, "a.txt");
    click(&mut h, "Open in external diff tool");
    let out = f.root.parent().unwrap().join("from-the-tool.txt");
    wait_for(&mut h, "the tool to run", |_| out.exists() && std::fs::read_to_string(&out).unwrap_or_default() == "edited\n");
    settle(&mut h);
    no_errors(&h);
}

#[test]
fn an_external_tool_that_fails_says_so() {
    let f = Fx::new();
    f.write("a.txt", "edited\n");
    let mut h = f.harness(|s| s.external_diff_tool = "no-such-tool".to_string());
    open(&mut h, Tab::changes());
    right_click(&mut h, "a.txt");
    click(&mut h, "Open in external diff tool");
    wait_for(&mut h, "the failure", |h| !h.state().errors_showing().is_empty());
    assert!(h.state().errors_showing()[0].contains("Open a.txt") || h.state().errors_showing()[0].contains("no-such-tool"), "{:?}", h.state().errors_showing());
}

#[test]
fn the_external_merge_tool_resolves_from_a_conflicts_menu_and_the_view_catches_up() {
    let f = Fx::new();
    conflicting_side(&f);
    f.git(&["config", "mergetool.fake.cmd", "cat \"$REMOTE\" > \"$MERGED\""]);
    f.git(&["config", "mergetool.fake.trustExitCode", "true"]);
    f.git(&["config", "mergetool.keepBackup", "false"]);
    assert!(f.git.run_raw(Some(&f.root), &["merge", "side"]).is_ok());
    f.trust();
    let mut h = f.harness(|s| s.external_merge_tool = "fake".to_string());
    open(&mut h, Tab::changes());
    assert_eq!(h.state().repos[0].data.counts().conflicts, 1);
    right_click(&mut h, "a.txt");
    click(&mut h, "Open in external merge tool");
    wait_for(&mut h, "the conflict to clear in the view", |h| h.state().repos[0].data.counts().conflicts == 0);
    assert_eq!(f.read("a.txt"), "from side\n");
    no_errors(&h);
}

// ---- the commit box

#[test]
fn the_commit_template_goes_in_the_box_and_commits() {
    let f = Fx::new();
    f.write("tmpl.txt", "# a comment for you\nTitle from the template\n\nBody\n");
    f.git(&["config", "commit.template", "tmpl.txt"]);
    f.trust();
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::changes());
    click(&mut h, "Message \u{E136}");
    click(&mut h, "Use the commit template");
    wait_for(&mut h, "the box to fill", |h| h.get_by_role(Role::MultilineTextInput).value().as_deref() == Some("Title from the template\n\nBody"));
    wait_for(&mut h, "the commit button", |h| h.query_all_by_label_contains("commit 1 file").next().is_some());
    h.query_all_by_label_contains("commit 1 file").next().unwrap().click();
    h.step();
    wait_for(&mut h, "the commit", |_| f.git(&["log", "-1", "--format=%s"]) == "Title from the template");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.git(&["log", "-1", "--format=%b"]), "Body");
}

// ---- ignoring

#[test]
fn the_ignore_form_writes_the_chosen_rule_where_asked() {
    let f = Fx::new();
    f.write("junk.log", "x\n");
    f.write("other.tmp", "x\n");
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::changes());
    right_click(&mut h, "junk.log");
    click(&mut h, "Ignore…");
    wait_for_label(&mut h, "Ignore junk.log");
    fill(&mut h, "pattern", "*.log");
    click(&mut h, "Ignore");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(f.read(".gitignore"), "*.log\n");
    assert!(!f.git(&["status", "--porcelain"]).contains("junk.log"));

    right_click(&mut h, "other.tmp");
    click(&mut h, "Ignore…");
    wait_for_label(&mut h, "Ignore other.tmp");
    fill(&mut h, "place", "This clone only (.git/info/exclude)");
    click(&mut h, "Ignore");
    settle(&mut h);
    no_errors(&h);
    assert!(f.read(".git/info/exclude").contains("other.tmp"));
    assert!(!f.read(".gitignore").contains("other.tmp"), "it isn't shared");
    // "Every repository of mine" writes the real global ignore file, so it isn't clicked here.
    // `global_ignore_path` and the append are the same code as these two above.
}

// ---- blame

#[test]
fn blames_boxes_each_change_the_blame_that_runs() {
    let f = Fx::new();
    f.write(".git-blame-ignore-revs", "# none\n");
    f.commit("revs file");
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::file("a.txt", None, None));
    let boxes = [
        ("Ignore whitespace (-w)", " -w"),
        ("Detect moved lines (-M)", " -M"),
        ("Detect lines copied from other files (-C)", " -C"),
        ("Look through commits in .git-blame-ignore-revs", "--ignore-revs-file"),
    ];
    for (label, flag) in boxes {
        let before = commands_with(&h, "blame").len();
        assert!(commands_with(&h, "blame").iter().all(|c| !c.contains(flag)), "{flag} isn't on yet");
        click(&mut h, "Blame options \u{E136}");
        click(&mut h, label);
        wait_for(&mut h, "a new blame", |h| commands_with(h, "blame").len() > before);
        assert!(commands_with(&h, "blame").last().unwrap().contains(flag), "{:?}", commands_with(&h, "blame").last());
        settle(&mut h);
        h.key_press(egui::Key::Escape);
        h.step();
        h.step();
    }
    no_errors(&h);
}

// ---- health

#[test]
fn healths_toggle_turns_a_setting_on_here_and_back_off() {
    let f = Fx::new();
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::Health(Box::default()));
    wait_for_label(&mut h, "Remember conflict resolutions");
    a_moment(&mut h);
    // In the order they're listed. The last is scheduled maintenance, left alone below.
    let enables = h.query_all_by_label("Enable").count();
    assert!(enables >= 2);
    h.query_all_by_label("Enable").nth(enables - 2).unwrap().click();
    h.step();
    h.step();
    wait_for(&mut h, "the setting", |_| f.git.run_raw(Some(&f.root), &["config", "--local", "rerere.enabled"]).unwrap().text().trim() == "true");
    settle(&mut h);
    no_errors(&h);
    click(&mut h, "Undo");
    wait_for(&mut h, "it to go", |_| !f.git.run_raw(Some(&f.root), &["config", "--local", "rerere.enabled"]).unwrap().ok());
    settle(&mut h);
    no_errors(&h);
    // Scheduled maintenance's Enable writes the global config and the system's scheduler, so it's
    // not clicked. `ops::maintenance_unregister` and its op are tested in destructive_ops.rs.
}

// ---- submodules

#[test]
fn the_status_bars_submodule_button_brings_them_to_the_commit_expected() {
    let f = Fx::new();
    let sub = f.root.parent().unwrap().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let in_sub = |dir: &std::path::Path, a: &[&str]| f.git.run(Some(dir), a).unwrap_or_else(|e| panic!("git {a:?}: {e}")).text().trim().to_string();
    in_sub(&sub, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com")] {
        in_sub(&sub, &["config", k, v]);
    }
    std::fs::write(sub.join("s.txt"), "1\n").unwrap();
    in_sub(&sub, &["add", "-A"]);
    in_sub(&sub, &["commit", "-q", "-m", "one"]);
    std::fs::write(sub.join("s.txt"), "2\n").unwrap();
    in_sub(&sub, &["add", "-A"]);
    in_sub(&sub, &["commit", "-q", "-m", "two"]);
    f.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", "../sub", "inner"]);
    f.commit("add the submodule");
    let inside = f.root.join("inner");
    let wanted = in_sub(&inside, &["rev-parse", "HEAD"]);
    in_sub(&inside, &["checkout", "-q", "HEAD~1"]);
    let mut h = f.harness(|_| {});
    wait_for_label(&mut h, "1 submodule to update");
    click(&mut h, "1 submodule to update");
    settle(&mut h);
    no_errors(&h);
    assert_eq!(in_sub(&inside, &["rev-parse", "HEAD"]), wanted);
    wait_for(&mut h, "the button to go", |h| h.query_by_label("1 submodule to update").is_none());
}

// ---- scanning

#[test]
fn scans_own_scan_button_looks_in_the_folder_typed() {
    let f = Fx::new();
    let top = tempfile::tempdir().unwrap();
    let found = top.path().join("found");
    std::fs::create_dir(&found).unwrap();
    f.git.run(Some(&found), &["init", "-q"]).unwrap();
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, Settings { details_on_open: false, ..Default::default() });
    app.git = Ok(f.git.clone());
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
    let mut h = builder.build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app);
    click(&mut h, "Scan for repositories…");
    wait_for_label(&mut h, "Folder");
    a_moment(&mut h);
    click(&mut h, "Scan");
    wait_for_text(&mut h, "Choose a folder.");
    let boxes = h.query_all_by_role(Role::TextInput).count();
    assert!(boxes >= 1);
    h.query_all_by_role(Role::TextInput).last().unwrap().focus();
    h.step();
    h.query_all_by_role(Role::TextInput).last().unwrap().type_text(&top.path().to_string_lossy());
    h.step();
    h.step();
    click(&mut h, "Scan");
    wait_for_label(&mut h, "Open 1 repository");
    click(&mut h, "Open 1 repository");
    wait_for(&mut h, "it to open", |h| h.state().repos.len() == 1);
}
