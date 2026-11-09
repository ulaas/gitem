//! Find in files as someone uses it: Ctrl+Shift+F, typing, the switches beside the box, where
//! to look, F4, and results that keep up with the files. Every repository is in a temp folder.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::{Role, Toggled};
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::settings::Settings;

const PATIENCE: Duration = Duration::from_secs(120);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let git = GitExe::discover(None).unwrap();
        let root = dir.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir(&root).unwrap();
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write("src/lib.rs", "pub fn alpha() {\n    let needle = 1;\n}\n");
        f.write("src/main.rs", "fn main() {\n    // Needle in a comment\n    alpha();\n}\n");
        f.write("docs/guide.md", "# Guide\n\nNo match here.\n");
        f.write(".gitignore", "build/\n");
        f.write("build/out.txt", "needle in what's ignored\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "first"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
}

fn harness(f: &Fx, ide: bool) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, Settings { details_on_open: false, ide_mode: ide, ..Default::default() });
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(5).with_step_dt(1.0 / 60.0);
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
    wait_for(&mut h, "the repository", |h| h.state().repos.first().is_some_and(|r| r.data.status.get().is_some() && r.data.refs.get().is_some()));
    h
}

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        if t.elapsed() > PATIENCE {
            let labels: Vec<String> = h.root().children_recursive().filter_map(|n| n.accesskit_node().label().or_else(|| n.accesskit_node().value()).map(|l| format!("{:?} {l}", n.accesskit_node().role()))).collect();
            panic!("timed out waiting for {what}; showing: {labels:#?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shows(h: &Harness<GitemApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

/// Ctrl+Shift+F, then the text, as typed.
fn find(h: &mut Harness<GitemApp>, text: &str) {
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::F);
    h.step();
    h.step();
    h.event(Event::Text(text.into()));
    h.step();
}

fn summary(h: &Harness<GitemApp>) -> Option<String> {
    h.query_all_by_role(Role::Label).filter_map(|n| n.accesskit_node().label().or_else(|| n.accesskit_node().value())).find(|l| l.contains(" in ") && l.contains("result"))
}

fn wait_summary(h: &mut Harness<GitemApp>, want: &str) {
    let w = want.to_string();
    wait_for(h, want, move |h| summary(h).is_some_and(|s| s.starts_with(&w)) && summary(h).is_some_and(|s| !s.contains("Searching")));
}

fn toggle(h: &mut Harness<GitemApp>, name: &str) {
    h.query_all_by_label(name).find(|n| n.accesskit_node().role() == Role::CheckBox).unwrap().click();
    h.step();
    h.step();
}

#[test]
fn ctrl_shift_f_searches_as_you_type_and_a_result_opens_at_the_match() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    find(&mut h, "needle");
    // Both files, case aside; not the ignored one.
    wait_summary(&mut h, "2 results in 2 files");
    assert!(shows(&h, "src/lib.rs") && shows(&h, "src/main.rs"));
    assert!(!shows(&h, "build/out.txt"), "ignored files aren't searched");
    assert!(h.query_all_by_label("Search").any(|n| n.accesskit_node().role() == Role::RadioButton && n.accesskit_node().toggled() == Some(Toggled::True)), "in the sidebar");

    h.get_by_label("2: let needle = 1;").click();
    wait_for(&mut h, "the editor", |h| h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Editor(e) if e.path == "src/lib.rs" && e.text().is_some())));
    for _ in 0..3 {
        h.step();
    }
    let picked = h.state_mut().repos[0].dock.iter_all_tabs_mut().find_map(|(_, t)| if let Tab::Editor(e) = t { (e.path == "src/lib.rs").then(|| e.editor_mut().map(|ed| (ed.sels.primary().start(), ed.sels.primary().end()))).flatten() } else { None });
    // "pub fn alpha() {\n" is 17 chars, then four spaces and "let ".
    assert_eq!(picked, Some((17 + 8, 17 + 14)), "the match is selected");
}

#[test]
fn the_switches_narrow_what_matches() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    find(&mut h, "needle");
    wait_summary(&mut h, "2 results");
    // Alt+C from the box: case counts, so the comment's Needle goes.
    h.key_press_modifiers(Modifiers::ALT, Key::C);
    wait_summary(&mut h, "1 result in 1 file");
    assert!(h.query_all_by_label("Match case").any(|n| n.accesskit_node().role() == Role::CheckBox && n.accesskit_node().toggled() == Some(Toggled::True)));
    toggle(&mut h, "Match case");
    wait_summary(&mut h, "2 results");

    // A regular expression, and one that's wrong says so instead of searching.
    toggle(&mut h, "Use regular expression");
    h.event(Event::Text("|alpha\\(".into()));
    wait_summary(&mut h, "4 results in 2 files");
    h.event(Event::Text("[".into()));
    wait_for(&mut h, "the error", |h| h.query_by_label_contains("Error: ").is_some());

    // Whole words: "alph" is in "alpha", but not as a word.
    toggle(&mut h, "Use regular expression");
    for _ in 0.."needle|alpha\\([".len() {
        h.key_press(Key::Backspace);
    }
    h.event(Event::Text("alph".into()));
    wait_summary(&mut h, "2 results in 2 files");
    toggle(&mut h, "Match whole word");
    wait_for(&mut h, "no results", |h| shows(h, "No results"));

    // Ignored files, from the options under the dots.
    toggle(&mut h, "Match whole word");
    for _ in 0.."alph".len() {
        h.key_press(Key::Backspace);
    }
    h.event(Event::Text("needle".into()));
    wait_summary(&mut h, "2 results");
    h.get_by_label("Files to include and exclude").click();
    h.step();
    h.step();
    toggle(&mut h, "Search ignored files too");
    wait_summary(&mut h, "3 results in 3 files");
    assert!(shows(&h, "build/out.txt"));
}

#[test]
fn where_to_look_changed_files_open_editors_and_a_revision() {
    let f = Fx::new();
    f.write("src/main.rs", "fn main() {\n    alpha();\n}\n");
    f.write("new.txt", "a needle, untracked\n");
    let mut h = harness(&f, true);
    wait_for(&mut h, "the change", |h| h.state().repos[0].data.status.get().is_some_and(|s| s.entries.len() >= 2));
    find(&mut h, "needle");
    wait_summary(&mut h, "2 results in 2 files");
    assert!(shows(&h, "new.txt") && shows(&h, "src/lib.rs"));

    h.get_by_label("Changed files").click();
    wait_summary(&mut h, "1 result in 1 file");
    assert!(shows(&h, "new.txt"));

    // Open editors, with what's typed and not saved.
    h.state_mut().repos[0].open_tab(Tab::editor("docs/guide.md", None, true));
    wait_for(&mut h, "the editor", |h| h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Editor(e) if e.path == "docs/guide.md" && e.text().is_some())));
    h.get_by_label("Open editors").click();
    wait_for(&mut h, "no results", |h| shows(h, "No results"));
    h.get_by_label("Text of docs/guide.md").click();
    h.step();
    h.event(Event::Text("needle ".into()));
    wait_summary(&mut h, "1 result in 1 file");
    assert!(shows(&h, "docs/guide.md"), "the unsaved text was searched");
    assert_eq!(std::fs::read_to_string(f.root.join("docs/guide.md")).unwrap(), "# Guide\n\nNo match here.\n", "and nothing was saved");

    // At the first commit, the comment still has its Needle.
    h.get_by_label("At a revision").click();
    wait_summary(&mut h, "2 results in 2 files");
    h.get_by_label("2: // Needle in a comment").click();
    wait_for(&mut h, "the file at HEAD", |h| h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::File(v) if v.path == "src/main.rs" && v.rev.as_deref() == Some("HEAD"))));
}

#[test]
fn f4_goes_through_the_results_and_they_keep_up_with_the_files() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    find(&mut h, "needle");
    wait_summary(&mut h, "2 results in 2 files");
    let open_at = |h: &Harness<GitemApp>| h.state().repos[0].dock.iter_all_tabs().filter_map(|(_, t)| if let Tab::Editor(e) = t { Some(e.path.clone()) } else { None }).collect::<Vec<_>>();
    h.key_press(Key::F4);
    wait_for(&mut h, "the first", |h| open_at(h).contains(&"src/lib.rs".to_string()));
    h.key_press(Key::F4);
    wait_for(&mut h, "the second", |h| open_at(h).contains(&"src/main.rs".to_string()));

    // A file saved elsewhere: the results catch up on their own.
    f.write("docs/guide.md", "# Guide\n\nA needle now.\n");
    wait_summary(&mut h, "3 results in 3 files");
}

#[test]
fn find_in_folder_looks_only_there_and_the_tab_has_the_same_search() {
    let f = Fx::new();
    f.write("docs/more.md", "needle in the docs\n");
    let mut h = harness(&f, true);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::FindInFiles { text: Some("needle".into()), folder: Some("docs".into()) });
    wait_summary(&mut h, "1 result in 1 file");
    assert!(shows(&h, "docs/more.md"));
    h.get_by_label("Search everywhere again").click();
    wait_summary(&mut h, "3 results in 3 files");

    h.get_by_label("Open in a tab").click();
    wait_for(&mut h, "the tab", |h| h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Grep(_))));
    wait_for(&mut h, "its results", |h| h.query_all_by_label("docs/more.md").count() == 2);
}
