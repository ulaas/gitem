//! The editor as someone uses it: IDE mode's clicks in the Files tree, typing, saving, the
//! close guard, a file changing on disk, and the gutter's repository marks. Every write is to
//! a repository in a temp folder.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
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
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f.write_bytes("a.rs", b"fn a() {\r\n    one();\r\n}\r\n");
        f.write_bytes("b.txt", b"\xEF\xBB\xBFfirst\nsecond\nthird\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "first"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn write_bytes(&self, p: &str, b: &[u8]) {
        std::fs::write(self.root.join(p), b).unwrap();
    }
    fn read(&self, p: &str) -> Vec<u8> {
        std::fs::read(self.root.join(p)).unwrap()
    }
}

fn harness(f: &Fx, ide: bool) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, Settings { details_on_open: false, ide_mode: ide, ..Default::default() });
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

fn wait_for(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn editors(h: &Harness<GitemApp>) -> Vec<(String, bool)> {
    h.state().repos[0].dock.iter_all_tabs().filter_map(|(_, t)| if let Tab::Editor(e) = t { Some((e.path.clone(), e.pinned)) } else { None }).collect()
}

fn files_sidebar(h: &mut Harness<GitemApp>) {
    h.get_by_label("Files").click();
    settle(h);
}

/// The file's row in the Files tree, not its tab of the same name.
fn tree_row(h: &Harness<GitemApp>, name: &str) {
    let row = h.get_all_by_label(name).min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).unwrap();
    row.click();
}

fn editor_text(h: &Harness<GitemApp>, path: &str) -> Option<String> {
    h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Editor(e) = t { (e.path == path).then(|| e.text()).flatten() } else { None })
}

fn open_in_editor(h: &mut Harness<GitemApp>, path: &str) {
    h.state_mut().repos[0].open_tab(Tab::editor(path, None, true));
    let p = path.to_string();
    wait_for(h, "the file to load", |h| editor_text(h, &p).is_some());
    settle(h);
    h.get_by_label(&format!("Text of {path}")).click();
    h.step();
    h.step();
}

#[test]
fn with_ide_mode_off_a_click_in_the_files_tree_opens_the_read_only_view() {
    let f = Fx::new();
    let mut h = harness(&f, false);
    settle(&mut h);
    files_sidebar(&mut h);
    h.get_by_label("a.rs").click();
    settle(&mut h);
    assert!(editors(&h).is_empty());
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::File(v) if v.path == "a.rs")));
}

#[test]
fn with_ide_mode_on_clicks_preview_and_a_double_click_keeps() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    files_sidebar(&mut h);
    tree_row(&h, "a.rs");
    settle(&mut h);
    assert_eq!(editors(&h), vec![("a.rs".to_string(), false)]);
    tree_row(&h, "b.txt");
    settle(&mut h);
    assert_eq!(editors(&h), vec![("b.txt".to_string(), false)], "the preview gave way");
    tree_row(&h, "b.txt");
    h.step();
    tree_row(&h, "b.txt");
    settle(&mut h);
    assert_eq!(editors(&h), vec![("b.txt".to_string(), true)]);
    tree_row(&h, "a.rs");
    settle(&mut h);
    assert_eq!(editors(&h).len(), 2, "a kept file stays when another is previewed: {:?}", editors(&h));
}

#[test]
fn typing_and_saving_keeps_line_endings_and_the_byte_order_mark() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "b.txt");
    // The click put the caret somewhere on the first line; go to its end.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Home);
    h.key_press(Key::End);
    h.event(Event::Text("!".into()));
    h.key_press(Key::Enter);
    h.event(Event::Text("new".into()));
    h.step();
    assert!(h.state().repos[0].open_tab_unsaved("edit:b.txt").is_some(), "edits make it unsaved");
    assert!(editors(&h)[0].1, "typing keeps a preview");
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(f.read("b.txt"), b"\xEF\xBB\xBFfirst!\nnew\nsecond\nthird\n");
    assert!(h.state().repos[0].open_tab_unsaved("edit:b.txt").is_none());
    wait_for(&mut h, "status to see the save", |h| h.state().repos[0].data.status.get().is_some_and(|s| s.entries.iter().any(|e| e.path == "b.txt")));

    // Undo back to what's saved isn't a change; one more step is.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.step();
    assert!(h.state().repos[0].open_tab_unsaved("edit:b.txt").is_some());
}

#[test]
fn crlf_files_get_crlf_for_new_lines_and_saving_nothing_writes_nothing() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    let before = std::fs::metadata(f.root.join("a.rs")).unwrap().modified().unwrap();
    open_in_editor(&mut h, "a.rs");
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(std::fs::metadata(f.root.join("a.rs")).unwrap().modified().unwrap(), before, "a clean file isn't written");
    h.key_press_modifiers(Modifiers::COMMAND, Key::End);
    h.event(Event::Text("// end".into()));
    h.key_press(Key::Enter);
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(f.read("a.rs"), b"fn a() {\r\n    one();\r\n}\r\n// end\r\n");
}

#[test]
fn closing_an_edited_file_asks_first() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "b.txt");
    h.event(Event::Text("x".into()));
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    settle(&mut h);
    assert!(h.query_by_label_contains("Unsaved edits to b.txt").is_some());
    assert_eq!(editors(&h).len(), 1, "still open while asking");
}

#[test]
fn a_change_on_disk_reloads_a_clean_file_and_waits_on_an_edited_one() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "b.txt");
    f.write_bytes("b.txt", b"from outside\n");
    wait_for(&mut h, "the reload", |h| editor_text(h, "b.txt").as_deref() == Some("from outside\n"));

    h.event(Event::Text("mine ".into()));
    h.step();
    f.write_bytes("b.txt", b"outside again\n");
    wait_for(&mut h, "the question", |h| h.query_by_label("Keep mine").is_some());
    assert!(editor_text(&h, "b.txt").unwrap().contains("mine"), "edits aren't thrown away");
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(f.read("b.txt"), b"outside again\n", "saving over it is refused until one is chosen");
    h.get_by_label("Keep mine").click();
    settle(&mut h);
    h.get_by_label("Text of b.txt").click();
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert!(String::from_utf8(f.read("b.txt")).unwrap().contains("mine"));
}

#[test]
fn the_gutter_stages_one_change_and_reverts_another() {
    let f = Fx::new();
    f.write_bytes("c.txt", b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    f.git(&["add", "c.txt"]);
    f.git(&["commit", "-q", "-m", "c"]);
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "c.txt");
    // Line 2 and line 9 changed.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Home);
    h.key_press(Key::ArrowDown);
    h.key_press(Key::End);
    h.event(Event::Text("b".into()));
    for _ in 0..7 {
        h.key_press(Key::ArrowDown);
    }
    h.key_press(Key::End);
    h.event(Event::Text("i".into()));
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    // The first change's bar, from a click in the gutter beside line 2.
    let row = h.get_by_label("Text of c.txt").rect();
    let gutter_x = row.left() - 10.0;
    let line_h = (13.0f32 * 1.5).ceil();
    h.event(Event::PointerMoved(egui::pos2(gutter_x, row.top() + line_h * 1.5)));
    h.event(Event::PointerButton { pos: egui::pos2(gutter_x, row.top() + line_h * 1.5), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: egui::pos2(gutter_x, row.top() + line_h * 1.5), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert!(h.query_by_label_contains("Line 2: 1 line changed").is_some(), "the change's bar shows");
    h.get_by_label("Stage").click();
    settle(&mut h);
    let staged = f.git(&["diff", "--cached", "-U0"]);
    assert!(staged.contains("+2b") && !staged.contains("+9i"), "only that change is staged:\n{staged}");

    // The other one, reverted from its bar, is one undoable step and leaves the file unsaved.
    h.event(Event::PointerButton { pos: egui::pos2(gutter_x, row.top() + line_h * 8.5), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: egui::pos2(gutter_x, row.top() + line_h * 8.5), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    h.get_by_label("Revert").click();
    settle(&mut h);
    assert_eq!(editor_text(&h, "c.txt").unwrap(), "1\n2b\n3\n4\n5\n6\n7\n8\n9\n10\n");
}

/// Only the rows in view are laid out, so a huge file costs what a screenful does. Laying out
/// the whole file, as a text box does, takes seconds a frame at this size.
#[test]
fn a_huge_file_scrolls_and_types_in_a_frame() {
    let f = Fx::new();
    let text: String = (0..300_000).map(|i| format!("let value_{i} = compute({i}, \"text\"); // line {i}\n")).collect();
    f.write_bytes("big.rs", text.as_bytes());
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "big.rs");
    h.key_press_modifiers(Modifiers::COMMAND, Key::G);
    h.step();
    h.step();
    h.event(Event::Text("150000".into()));
    h.key_press(Key::Enter);
    for _ in 0..30 {
        h.step();
    }
    let mut slowest = Duration::ZERO;
    for _ in 0..10 {
        h.key_press(Key::PageDown);
        h.event(Event::Text("x".into()));
        let t = Instant::now();
        h.step();
        slowest = slowest.max(t.elapsed());
    }
    eprintln!("slowest frame while paging and typing in 300,000 lines: {slowest:?}");
    let text = editor_text(&h, "big.rs").unwrap();
    assert_eq!(text.matches('x').count(), 300_000 + 10, "each line's own x, and the ten typed after going to a line");
    assert!(slowest < Duration::from_millis(250), "a frame took {slowest:?}");
}

#[test]
fn a_checkout_waits_for_unsaved_edits_and_a_fetch_does_not() {
    let f = Fx::new();
    f.git(&["branch", "other"]);
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "b.txt");
    h.event(Event::Text("edit ".into()));
    h.step();
    let ctx = h.ctx.clone();

    // Fetching changes no file, so it doesn't ask.
    h.state_mut().apply_action(&ctx, gitem::action::Action::Run(gitem_core::ops::fetch(&Default::default())));
    h.step();
    assert!(h.query_by_label("Save your edits first?").is_none());
    settle(&mut h);

    h.state_mut().apply_action(&ctx, gitem::action::Action::Run(gitem_core::ops::checkout("other")));
    h.step();
    h.step();
    assert!(h.query_by_label("Save your edits first?").is_some());
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(f.git(&["branch", "--show-current"]).trim(), "main", "Cancel runs nothing");

    h.state_mut().apply_action(&ctx, gitem::action::Action::Run(gitem_core::ops::checkout("other")));
    h.step();
    h.step();
    h.get_by_label("Save all and continue").click();
    settle(&mut h);
    assert!(String::from_utf8(f.read("b.txt")).unwrap().contains("edit "), "saved first");
    assert_eq!(f.git(&["branch", "--show-current"]).trim(), "other");
}

#[test]
fn an_empty_file_takes_its_line_endings_from_gitattributes() {
    let f = Fx::new();
    f.write_bytes(".gitattributes", b"*.txt text eol=crlf\n");
    f.write_bytes("new.txt", b"");
    f.write_bytes("new.md", b"");
    let mut h = harness(&f, true);
    settle(&mut h);
    for path in ["new.txt", "new.md"] {
        open_in_editor(&mut h, path);
        h.event(Event::Text("a".into()));
        h.key_press(Key::Enter);
        h.event(Event::Text("b".into()));
        h.key_press_modifiers(Modifiers::COMMAND, Key::S);
        settle(&mut h);
    }
    assert_eq!(f.read("new.txt"), b"a\r\nb");
    assert_eq!(f.read("new.md"), b"a\nb", "core.autocrlf is false here, and nothing else says CRLF");
}

#[test]
fn a_file_with_conflicts_opens_where_they_are_resolved() {
    let f = Fx::new();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write_bytes("b.txt", b"side\n");
    f.git(&["commit", "-q", "-am", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write_bytes("b.txt", b"main\n");
    f.git(&["commit", "-q", "-am", "main"]);
    let _ = f.git.run(Some(&f.root), &["merge", "side"]);
    let mut h = harness(&f, true);
    settle(&mut h);
    files_sidebar(&mut h);
    tree_row(&h, "b.txt");
    settle(&mut h);
    assert!(editors(&h).is_empty());
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Merge(_))));
}

#[test]
fn a_windows_1252_file_is_edited_and_saved_in_its_own_encoding() {
    let f = Fx::new();
    f.write_bytes("latin.txt", b"caf\xe9\n");
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "latin.txt");
    assert_eq!(editor_text(&h, "latin.txt").unwrap(), "café\n");
    h.key_press_modifiers(Modifiers::COMMAND, Key::Home);
    h.key_press(Key::End);
    h.event(Event::Text("s €".into()));
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(f.read("latin.txt"), b"caf\xe9s \x80\n");
    // A character Windows-1252 hasn't got is refused, and nothing is written.
    h.event(Event::Text("中".into()));
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    assert_eq!(f.read("latin.txt"), b"caf\xe9s \x80\n");
    assert!(!h.state().errors_showing().is_empty());
}

#[test]
fn palette_commands_act_on_the_editor_in_front() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "a.rs");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::Editor(gitem::editor::Request::Run(gitem::editor::Cmd::SelectAll)));
    h.state_mut().apply_action(&ctx, gitem::action::Action::Editor(gitem::editor::Request::Run(gitem::editor::Cmd::ToggleComment)));
    settle(&mut h);
    assert_eq!(editor_text(&h, "a.rs").unwrap(), "// fn a() {\r\n//     one();\r\n// }\r\n");
    h.state_mut().apply_action(&ctx, gitem::action::Action::SaveAll);
    settle(&mut h);
    assert!(f.read("a.rs").starts_with(b"// fn a()"));
}

#[test]
fn the_outline_goes_to_a_definition_by_name() {
    let f = Fx::new();
    f.write_bytes("m.rs", b"fn first() {}\n\nstruct Holder;\n\nfn second_one() {}\n");
    let mut h = harness(&f, true);
    settle(&mut h);
    open_in_editor(&mut h, "m.rs");
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::O);
    h.step();
    h.step();
    h.event(Event::Text("secon".into()));
    h.step();
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().folder_pick_idle(), "Ctrl+Shift+O isn't Ctrl+O: no folder picker");
    let line = h.state_mut().repos[0].dock.iter_all_tabs_mut().find_map(|(_, t)| if let Tab::Editor(e) = t { e.editor_mut().map(|ed| ed.buf.line_of(ed.sels.primary().head)) } else { None });
    assert_eq!(line, Some(4));
    assert!(h.query_by_label("Go to a definition").is_none(), "the box closed");
    // Typing goes to the text again.
    h.event(Event::Text("x".into()));
    h.step();
    assert!(editor_text(&h, "m.rs").unwrap().contains("xfn second_one"));
}

/// Stands in for a language server, remembering what the editor told it.
struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl gitem::editor::lsp::LanguageServer for Recorder {
    fn did_open(&mut self, path: &str, _: &str) {
        self.0.lock().unwrap().push(format!("open {path}"));
    }
    fn did_change(&mut self, path: &str, _: u64, text: &str) {
        self.0.lock().unwrap().push(format!("change {path} {}", text.len()));
    }
    fn did_save(&mut self, path: &str) {
        self.0.lock().unwrap().push(format!("save {path}"));
    }
    fn did_close(&mut self, _: &str) {}
    fn diagnostics(&self, _: &str) -> Vec<gitem::editor::lsp::Diagnostic> {
        Vec::new()
    }
    fn completions(&mut self, _: &str, _: gitem::editor::lsp::Position) -> Vec<gitem::editor::lsp::Completion> {
        Vec::new()
    }
    fn hover(&mut self, _: &str, _: gitem::editor::lsp::Position) -> Option<String> {
        None
    }
    fn definition(&mut self, _: &str, _: gitem::editor::lsp::Position) -> Option<gitem::editor::lsp::Location> {
        None
    }
}

#[test]
fn a_language_server_hears_of_opening_changing_and_saving() {
    let f = Fx::new();
    let mut h = harness(&f, true);
    settle(&mut h);
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut tab = Tab::editor("b.txt", None, true);
    if let Tab::Editor(e) = &mut tab {
        e.lsp = Some(Box::new(Recorder(heard.clone())));
    }
    h.state_mut().repos[0].open_tab(tab);
    wait_for(&mut h, "the file to load", |h| editor_text(h, "b.txt").is_some());
    settle(&mut h);
    h.get_by_label("Text of b.txt").click();
    h.step();
    h.event(Event::Text("x".into()));
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    settle(&mut h);
    let heard = heard.lock().unwrap().clone();
    assert_eq!(heard.first().map(String::as_str), Some("open b.txt"));
    assert!(heard.iter().any(|c| c.starts_with("change b.txt")), "{heard:?}");
    assert_eq!(heard.last().map(String::as_str), Some("save b.txt"), "{heard:?}");
}
