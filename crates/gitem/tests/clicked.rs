//! Menu items and forms clicked through the real UI against real git in throwaway repositories,
//! where earlier tests only ran what's behind them. Clicks are kittest's synthetic input. Fake
//! external tools go in the scratch repository's own config, marked trusted in this binary's
//! scratch settings.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::{NodeT, Queryable};
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

// ---- FG-9: several branches, picked in the sidebar

/// Two picked with Ctrl+click, one merged and one not: the question says which is which, and Undo
/// brings both back, as it does from Clean up branches.
#[test]
fn branches_picked_in_the_sidebar_delete_together_and_come_back() {
    use egui::Modifiers;
    let f = Fx::new();
    f.git(&["branch", "done"]);
    f.git(&["switch", "-q", "-c", "unfinished"]);
    f.write("u.txt", "u\n");
    let unfinished = f.commit("only here");
    f.git(&["switch", "-q", "main"]);
    f.git(&["branch", "keep"]);
    let mut h = f.harness(|_| {});
    settle(&mut h);
    wait_for_label(&mut h, "done");
    let inspected = |h: &Harness<GitemApp>| h.state().repos[0].inspect.sha.clone();
    let before = inspected(&h);
    h.get_by_label("done").click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    assert_eq!(inspected(&h), before, "picking isn't looking");
    wait_for_label(&mut h, "Delete 1 branch…");
    h.get_by_label("unfinished").click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    wait_for_label(&mut h, "Delete 2 branches…");
    // The second click on one takes it off again, and the current one can't be picked.
    h.get_by_label("keep").click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    wait_for_label(&mut h, "Delete 3 branches…");
    h.get_by_label("keep").click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    wait_for_label(&mut h, "Delete 2 branches…");
    click(&mut h, "Delete 2 branches…");
    wait_for_text(&mut h, "aren't merged into main: unfinished");
    assert_ne!(f.git(&["branch", "--list", "done"]), "", "nothing yet");
    click(&mut h, "Delete");
    wait_for(&mut h, "both to go", |_| f.git(&["branch", "--list", "done"]).is_empty() && f.git(&["branch", "--list", "unfinished"]).is_empty());
    settle(&mut h);
    no_errors(&h);
    assert_ne!(f.git(&["branch", "--list", "keep"]), "", "only what was picked");
    assert!(h.query_by_label("Delete 2 branches…").is_none(), "the pick is over");
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::ALT), egui::Key::Z);
    h.step();
    settle(&mut h);
    // The question's button, which comes last: the toolbar's has the same name behind it.
    wait_for_text(&mut h, "Undo delete of 2 branches");
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(300) {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    h.query_all_by_label_contains("Undo delete of 2 branches").last().unwrap().click();
    h.step();
    wait_for(&mut h, "both back", |_| !f.git(&["branch", "--list", "unfinished"]).is_empty());
    assert_eq!(f.git(&["rev-parse", "unfinished"]), unfinished);
    assert_ne!(f.git(&["branch", "--list", "done"]), "");
}

#[test]
fn the_current_branch_and_one_in_a_worktree_cant_be_picked() {
    use egui::Modifiers;
    let f = Fx::new();
    f.git(&["branch", "held"]);
    f.git(&["worktree", "add", "-q", "../wt", "held"]);
    let mut h = f.harness(|_| {});
    settle(&mut h);
    wait_for_label(&mut h, "held");
    h.query_all_by_label("held").next().unwrap().click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    h.query_all_by_label("main").next().unwrap().click_modifiers(Modifiers::COMMAND);
    h.step();
    h.step();
    assert!(h.query_all_by_label_contains("Delete 1 branch").next().is_none());
}

// ---- UX-36

fn with_message(h: &Harness<GitemApp>) -> String {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Changes(v) = t { Some(v.message.clone()) } else { None }).unwrap_or_default()
}

/// Off by default, the row goes to Changes as it always did.
#[test]
fn the_work_in_progress_row_goes_to_changes_unless_turned_on() {
    let f = Fx::new();
    f.write("a.txt", "changed\n");
    let mut h = f.harness(|_| {});
    settle(&mut h);
    click(&mut h, "Work in progress");
    settle(&mut h);
    let front = h.state_mut().repos[0].dock.find_active_focused().map(|(_, t)| t.key());
    assert_eq!(front.as_deref(), Some("changes"));
}

/// Turned on, one draft: what's typed in Details is what Changes has, what Changes had is what
/// Details shows, there's only ever one box, and committing from Details clears the one draft.
#[test]
fn staging_in_details_shares_one_draft_with_changes() {
    let f = Fx::new();
    f.write("a.txt", "changed\n");
    let mut h = f.harness(|s| s.wip_in_details = true);
    settle(&mut h);
    // Changes first, with something written there.
    h.state_mut().repos[0].open_tab(Tab::changes());
    settle(&mut h);
    let boxes = |h: &Harness<GitemApp>| h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).count();
    assert_eq!(boxes(&h), 1, "the commit box");
    if let Some(Tab::Changes(v)) = h.state_mut().repos[0].dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| matches!(t, Tab::Changes(_))) {
        v.message = "written in Changes".to_string();
    }
    h.state_mut().repos[0].open_tab(Tab::graph());
    settle(&mut h);
    assert_eq!(boxes(&h), 0, "none beside the graph yet");

    click(&mut h, "Work in progress");
    settle(&mut h);
    wait_for_label(&mut h, "Open Changes");
    assert_eq!(boxes(&h), 1, "one, in Details");
    let in_box = h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).next().unwrap().accesskit_node().value().unwrap_or_default().to_string();
    assert_eq!(in_box, "written in Changes", "the same draft");
    let front = h.state_mut().repos[0].dock.find_active_focused().map(|(_, t)| t.key());
    assert_eq!(front.as_deref(), Some("graph"), "it doesn't leave the graph");

    // Staged from there, and committed from there, with the one message.
    click(&mut h, "Stage all");
    wait_for(&mut h, "it to be staged", |_| f.git(&["status", "--short"]).starts_with('M'));
    settle(&mut h);
    click(&mut h, "Commit 1 file");
    wait_for(&mut h, "the commit", |_| f.git(&["log", "-1", "--format=%s"]) == "written in Changes");
    settle(&mut h);
    no_errors(&h);
    wait_for(&mut h, "the draft to clear", |h| with_message(h).is_empty());

    // A commit picked goes back to its details.
    let sha = f.git(&["rev-parse", "HEAD"]);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::Inspect(sha));
    settle(&mut h);
    assert!(h.query_by_label("Open Changes").is_none());
}

/// A split with Changes showing already has the box: Details doesn't draw another.
#[test]
fn with_changes_in_a_pane_details_points_to_it() {
    let f = Fx::new();
    f.write("a.txt", "changed\n");
    let mut h = f.harness(|s| s.wip_in_details = true);
    settle(&mut h);
    let r = &mut h.state_mut().repos[0];
    let at = r.dock.find_tab_from(|t| matches!(t, Tab::Changes(_))).unwrap();
    let tab = r.dock.remove_tab(at).unwrap();
    let first = r.dock.iter_leaves().next().map(|(p, _)| p).unwrap();
    r.dock.split(first, egui_dock::Split::Below, 0.5, egui_dock::Node::leaf(tab));
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::ShowWip);
    settle(&mut h);
    assert_eq!(h.query_all_by_role(egui::accesskit::Role::MultilineTextInput).count(), 1, "one box");
}

// ---- SVG

/// A hostile file is drawn at the largest size or refused, and its text is never far away.
#[test]
fn an_svg_shows_as_a_picture_and_its_source_is_a_click_away() {
    let f = Fx::new();
    f.write("drawing.svg", r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#);
    f.write("hostile.svg", &format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{}{}</svg>"#, "<g>".repeat(5000), "</g>".repeat(5000)));
    f.commit("two drawings");
    let mut h = f.harness(|_| {});
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("drawing.svg", None, None));
    wait_for_label(&mut h, "Picture");
    settle(&mut h);
    assert!(h.query_by_label_contains("xmlns").is_none(), "the picture, not its text");
    click(&mut h, "Source");
    settle(&mut h);
    click(&mut h, "Picture");
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("hostile.svg", None, None));
    wait_for_text(&mut h, "This SVG can't be drawn");
    wait_for_text(&mut h, "It nests too deeply");
    no_errors(&h);

    // Its diff is text until asked, and asking draws both sides.
    f.write("drawing.svg", r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="blue"/></svg>"#);
    h.state_mut().repos[0].open_tab(Tab::diff(gitem_core::repo::DiffSource::Unstaged, Some("drawing.svg".to_string())));
    settle(&mut h);
    wait_for_label(&mut h, "Pictures");
    click(&mut h, "Pictures");
    wait_for_text(&mut h, "40 × 20 px");
    assert!(h.query_by_label("Before").is_some() && h.query_by_label("After").is_some());
    no_errors(&h);
}

// ---- FG-39 and FG-35

fn hunk_file(f: &Fx) -> String {
    let base: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
    f.write("h.txt", &(base.join("\n") + "\n"));
    f.commit("the file");
    let mut changed = base.clone();
    changed[3] = "early change".to_string();
    changed[30] = "late change".to_string();
    let text = changed.join("\n") + "\n";
    f.write("h.txt", &text);
    text
}

/// Ticked by a click, stashed by a click, popped from the stash's menu, back to the same bytes.
#[test]
fn stash_hunks_is_clicked_through_and_comes_back() {
    let f = Fx::new();
    let text = hunk_file(&f);
    let mut h = f.harness(|_| {});
    h.state_mut().repos[0].open_tab(Tab::StashHunks(Box::default()));
    settle(&mut h);
    wait_for_text(&mut h, "h.txt: lines");
    let first = h.query_all_by_label_contains("h.txt: lines").last().unwrap().accesskit_node().label().unwrap().to_string();
    click(&mut h, &first);
    h.step();
    click(&mut h, "Stash 1 hunk");
    wait_for(&mut h, "the stash", |_| !f.git(&["stash", "list"]).is_empty());
    settle(&mut h);
    assert!(f.read("h.txt").contains("early change"), "the first stays");
    assert!(!f.read("h.txt").contains("late change"), "the second left");
    assert!(f.git(&["stash", "list"]).ends_with("[hunks]"), "{}", f.git(&["stash", "list"]));
    no_errors(&h);
    // Popping is the palette's or a menu's. The palette's goes the same way for this one.
    let sha = f.git(&["rev-parse", "stash@{0}"]);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::only_if_stash_is(gitem_core::hunkstash::apply("stash@{0}", true), "stash@{0}", &sha));
    settle(&mut h);
    assert_eq!(f.read("h.txt"), text);
    assert_eq!(f.git(&["stash", "list"]), "");
    no_errors(&h);
}

#[test]
fn a_file_with_staged_changes_says_why_it_cant_offer_hunks() {
    let f = Fx::new();
    hunk_file(&f);
    f.git(&["add", "h.txt"]);
    f.write("h.txt", &(f.read("h.txt") + "more\n"));
    let mut h = f.harness(|_| {});
    h.state_mut().repos[0].open_tab(Tab::StashHunks(Box::default()));
    settle(&mut h);
    wait_for_text(&mut h, "It has staged changes too");
    assert!(h.query_all_by_label_contains("h.txt: lines").next().is_none());
    assert!(h.query_by_label("Stash 0 hunks").is_some_and(|n| n.accesskit_node().is_disabled()));
}

/// Off, none of it is there. On, a feature is started and finished by clicks, as far as a form.
#[test]
fn the_branch_model_is_absent_until_turned_on_then_clicks_through() {
    let f = Fx::new();
    f.git(&["branch", "develop"]);
    let mut off = f.harness(|_| {});
    off.state_mut().repos[0].open_tab(Tab::home());
    settle(&mut off);
    assert!(off.query_by_label("Start feature…").is_none());
    off.state_mut().open_palette_with("Start Feature");
    settle(&mut off);
    assert!(off.query_by_label("Start Feature…").is_none());
    drop(off);

    let mut h = f.harness(|s| s.branch_model.enabled = true);
    h.state_mut().repos[0].open_tab(Tab::home());
    settle(&mut h);
    click(&mut h, "Start feature…");
    wait_for_label(&mut h, "Start");
    h.state_mut().form_set_for_test("name", "login");
    click(&mut h, "Start");
    wait_for(&mut h, "the branch", |_| f.git(&["symbolic-ref", "--short", "HEAD"]) == "feature/login");
    settle(&mut h);
    f.write("login.txt", "l\n");
    f.commit("login");
    settle(&mut h);
    h.state_mut().open_palette_with("Finish Feature");
    settle(&mut h);
    click(&mut h, "Finish Feature…");
    wait_for_label(&mut h, "Next");
    click(&mut h, "Next");
    wait_for_label(&mut h, "Finish");
    wait_for_text(&mut h, "Merges feature/login into develop");
    click(&mut h, "Finish");
    wait_for(&mut h, "the finish", |_| f.git(&["branch", "--list", "feature/login"]).is_empty());
    settle(&mut h);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "develop");
    no_errors(&h);
}

#[test]
fn finishing_with_changes_in_the_tree_is_refused_in_the_form() {
    let f = Fx::new();
    f.git(&["branch", "develop"]);
    f.git(&["switch", "-q", "-c", "feature/x"]);
    f.write("a.txt", "dirty\n");
    let mut h = f.harness(|s| s.branch_model.enabled = true);
    settle(&mut h);
    h.state_mut().open_palette_with("Finish Feature");
    settle(&mut h);
    click(&mut h, "Finish Feature…");
    wait_for_label(&mut h, "Next");
    click(&mut h, "Next");
    wait_for_text(&mut h, "Commit or stash your changes first");
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "feature/x");
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
