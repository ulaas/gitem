//! The layout: splitting a tab off, closing several, one pane filling the window, the two
//! built-in arrangements and saved ones, and moving between panes from the keyboard.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::action::Action;
use gitem::app::GitemApp;
use gitem::layouts::{Closing, DockOp, Preset};
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::settings::Settings;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let git = GitExe::discover(None).unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let run = |args: &[&str]| git.run(Some(&root), args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false")] {
            run(&["config", k, v]);
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        Self { _dir: dir, root, git }
    }
}

fn harness(f: &Fx) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, Settings { details_on_open: false, ..Default::default() });
    app.git = Ok(f.git.clone());
    let root = f.root.clone();
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(5).with_step_dt(1.0 / 60.0);
    builder.build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    )
}

fn settle(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    loop {
        h.step();
        if gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() {
            for _ in 0..4 {
                h.step();
            }
            if gitem::task::running(&h.ctx) == 0 {
                return;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(60), "never settled");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn act(h: &mut Harness<GitemApp>, a: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, a);
    settle(h);
}

fn dock(h: &mut Harness<GitemApp>, key: &str, op: DockOp) {
    act(h, Action::Dock { key: key.into(), op });
}

/// Each pane's tab keys, in order.
fn panes(h: &Harness<GitemApp>) -> Vec<Vec<String>> {
    h.state().repos[0].dock.iter_leaves().map(|(_, l)| l.tabs.iter().map(Tab::key).collect()).collect()
}

fn focused(h: &Harness<GitemApp>) -> Vec<String> {
    let d = &h.state().repos[0].dock;
    let p = d.focused_leaf().unwrap();
    d.leaf(p).unwrap().tabs.iter().map(Tab::key).collect()
}

/// An editor with text typed into it and not saved.
fn unsaved_editor(h: &mut Harness<GitemApp>) {
    h.state_mut().repos[0].open_tab(Tab::editor("a.txt", None, true));
    let t = Instant::now();
    while !h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Editor(e) if e.text().is_some())) {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(30));
    }
    settle(h);
    for (_, t) in h.state_mut().repos[0].dock.iter_all_tabs_mut() {
        if let Tab::Editor(e) = t
            && let Some(ed) = e.editor_mut()
        {
            ed.run(gitem::editor::Cmd::Type("x".into()), 0);
        }
    }
    h.step();
}

#[test]
fn a_tab_split_off_gets_a_pane_of_its_own_unless_it_is_alone() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    dock(&mut h, "changes", DockOp::Split(egui_dock::Split::Right));
    assert_eq!(panes(&h), vec![vec!["home", "graph"], vec!["changes"]]);
    assert_eq!(focused(&h), vec!["changes"], "the new pane has the keyboard");
    dock(&mut h, "changes", DockOp::Split(egui_dock::Split::Below));
    assert_eq!(panes(&h).len(), 2, "a pane's only tab stays where it is");
}

#[test]
fn closing_several_leaves_unsaved_work_open() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    unsaved_editor(&mut h);
    h.state_mut().repos[0].open_tab(Tab::Reflog(Box::default()));
    settle(&mut h);
    dock(&mut h, "graph", DockOp::CloseMany(Closing::ToTheRight));
    let left = panes(&h).concat();
    assert!(left.contains(&"edit:a.txt".to_string()) && !left.contains(&"reflog".to_string()) && !left.contains(&"changes".to_string()), "{left:?}");
    dock(&mut h, "edit:a.txt", DockOp::CloseMany(Closing::Others));
    assert_eq!(panes(&h).concat(), vec!["edit:a.txt"]);
    // One alone asks, as its close button does.
    dock(&mut h, "edit:a.txt", DockOp::Close);
    assert!(h.query_by_label("Close with work not saved?").is_some());
}

#[test]
fn a_pane_fills_the_window_and_the_layout_comes_back_with_what_was_opened() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    unsaved_editor(&mut h);
    dock(&mut h, "changes", DockOp::Split(egui_dock::Split::Right));
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::M);
    settle(&mut h);
    assert_eq!(panes(&h), vec![vec!["changes"]]);
    assert!(h.query_by_label("Restore layout").is_some());
    // Work hidden behind it still counts when closing the window.
    assert!(h.state().window_at_stake().iter().any(|i| i.contains("a.txt")), "{:?}", h.state().window_at_stake());
    h.state_mut().repos[0].open_tab(Tab::Console(Box::default()));
    settle(&mut h);
    h.get_by_label("Restore layout").click();
    settle(&mut h);
    let p = panes(&h);
    assert_eq!(p.len(), 2);
    assert_eq!(p[1], vec!["changes", "console"], "opened while it filled the window, so it's in that pane");
    // Asking for a tab hidden behind it brings everything back.
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::M);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::graph());
    settle(&mut h);
    assert_eq!(panes(&h).len(), 2);
}

#[test]
fn the_ide_layout_keeps_every_tab_and_shows_the_files() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    unsaved_editor(&mut h);
    act(&mut h, Action::Layout(Preset::Ide));
    let mut p = panes(&h);
    p.sort();
    assert_eq!(p.len(), 3, "{p:?}");
    assert!(p.contains(&vec!["changes".to_string()]) && p.contains(&vec!["console".to_string()]), "{p:?}");
    assert!(p.iter().any(|pane| pane.contains(&"edit:a.txt".to_string()) && pane.contains(&"graph".to_string())), "{p:?}");
    assert!(h.state().settings.sidebar_files);
    assert!(h.state().repos[0].open_tab_unsaved("edit:a.txt").is_some(), "the unsaved text came along");

    act(&mut h, Action::SaveLayout("mine".into()));
    act(&mut h, Action::Layout(Preset::Git));
    assert_eq!(panes(&h).len(), 1);
    act(&mut h, Action::Layout(Preset::Saved("mine".into())));
    assert_eq!(panes(&h).len(), 3);
    assert!(h.state().repos[0].open_tab_unsaved("edit:a.txt").is_some());
}

#[test]
fn the_keyboard_moves_between_panes() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    dock(&mut h, "changes", DockOp::Split(egui_dock::Split::Right));
    assert_eq!(focused(&h), vec!["changes"]);
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.key_press(Key::ArrowLeft);
    settle(&mut h);
    assert_eq!(focused(&h), vec!["home", "graph"]);
    h.key_press(Key::F6);
    settle(&mut h);
    assert_eq!(focused(&h), vec!["changes"]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::F6);
    settle(&mut h);
    assert_eq!(focused(&h), vec!["home", "graph"]);
}

#[test]
fn turning_ide_mode_on_offers_its_layout_once() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    assert!(h.query_by_label("Use the IDE layout").is_none());
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::E);
    settle(&mut h);
    assert!(h.state().settings.ide_mode);
    h.get_by_label("Use the IDE layout").click();
    settle(&mut h);
    assert_eq!(panes(&h).len(), 3);
    assert!(h.query_by_label("Use the IDE layout").is_none(), "asked once");
}

#[test]
fn a_tab_dragged_onto_another_pane_moves_there() {
    let f = Fx::new();
    let mut h = harness(&f);
    settle(&mut h);
    act(&mut h, Action::Layout(Preset::Ide));
    h.state_mut().repos[0].open_tab(Tab::graph());
    settle(&mut h);
    let d = &h.state().repos[0].dock;
    let path = d.find_tab_from(|t| t.key() == "graph").unwrap();
    let id = gitem::app::dock_tab_id(egui_dock::NodePath { surface: path.surface, node: path.node }, path.tab.0);
    let from = h.ctx.read_response(id).expect("the tab was drawn").rect.center();
    let to = d.iter_leaves().find(|(_, l)| l.tabs.iter().any(|t| t.key() == "console")).unwrap().1.rect.center();
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=16 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 16.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    let console_pane = panes(&h).into_iter().find(|p| p.contains(&"console".to_string())).unwrap();
    assert!(console_pane.contains(&"graph".to_string()), "{:?}", panes(&h));
}
