//! The app as it starts and stops for real: `GitemApp::new`, the settings file, the
//! saved session and `on_exit`. Everything else builds the app without saving.
//!
//! The settings path is process-wide, so these tests take turns.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::App;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use gitem::action::Action;
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::settings::{Settings, ThemeChoice};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Takes the turn and starts from no settings file.
fn clean_slate() -> MutexGuard<'static, ()> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let path = Settings::path();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    turn
}

fn run_app(paths: &[PathBuf]) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let app = GitemApp::new(&ctx, paths);
    Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app)
}

fn repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir(&root).unwrap();
    let git = gitem_core::GitExe::discover(None).unwrap();
    git.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false")] {
        git.run(Some(&root), &["config", k, v]).unwrap();
    }
    std::fs::write(root.join("a.txt"), "1\n").unwrap();
    git.run(Some(&root), &["add", "."]).unwrap();
    git.run(Some(&root), &["commit", "-q", "-m", "first"]).unwrap();
    // As the app names it, which is how sessions store it.
    let top = git.run(Some(&root), &["rev-parse", "--show-toplevel"]).unwrap().text();
    PathBuf::from(top.trim())
}

fn name(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn steps(h: &mut Harness<GitemApp>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn settled(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    while gitem::task::running(&h.ctx) > 0 || !h.state().ops_idle() || h.state().repos.iter().any(|r| r.data.status.busy() || r.data.refs.busy()) {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(180), "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    steps(h, 5);
}

fn open_repos(h: &Harness<GitemApp>) -> Vec<String> {
    h.state().repos.iter().map(|r| name(&r.repo.root)).collect()
}

fn error_dialog_showing(h: &Harness<GitemApp>) -> bool {
    !h.state().errors_showing().is_empty()
}

fn saved() -> Settings {
    let text = std::fs::read_to_string(Settings::path()).expect("a settings file was written");
    serde_json::from_str(&text).expect("the settings file is valid")
}

// ---- starting ------------------------------------------------------------------------

#[test]
fn starting_with_no_settings_file_works_and_writes_nothing_yet() {
    let _turn = clean_slate();
    let mut h = run_app(&[]);
    steps(&mut h, 5);
    assert!(!error_dialog_showing(&h));
    assert!(h.state().repos.is_empty());
    assert_eq!(h.state().settings.theme, Settings::default().theme);
}

#[test]
fn an_argument_opens_that_repository() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), "a");
    let mut h = run_app(std::slice::from_ref(&a));
    settled(&mut h);
    assert_eq!(open_repos(&h), vec![name(&a)]);
    assert!(!error_dialog_showing(&h));
}

#[test]
fn an_argument_that_is_not_a_repository_says_so() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let mut h = run_app(&[d.path().to_path_buf()]);
    steps(&mut h, 5);
    assert!(h.state().repos.is_empty());
    assert!(error_dialog_showing(&h));
}

// ---- the session -----------------------------------------------------------------------

#[test]
fn the_session_comes_back_after_a_restart() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (repo(d.path(), "a"), repo(d.path(), "b"));
    {
        let mut h = run_app(&[a.clone(), b.clone()]);
        settled(&mut h);
        h.state_mut().repos[1].open_tab(Tab::Contributors(Box::default()));
        settled(&mut h);
        h.state_mut().on_exit();
    }
    let on_disk = saved();
    assert_eq!(on_disk.open_repos, vec![name(&a), name(&b)]);
    assert!(on_disk.session_tabs[&name(&b)].iter().any(|t| t == "contributors"), "{:?}", on_disk.session_tabs);
    assert!(on_disk.recent_repos.contains(&name(&a)));

    let mut h = run_app(&[]);
    settled(&mut h);
    assert_eq!(open_repos(&h), vec![name(&a), name(&b)]);
    assert!(h.state().repos[1].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Contributors(_))));
    assert!(!h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Contributors(_))), "each repository has its own tabs");
    assert!(!error_dialog_showing(&h));
}

#[test]
fn a_repository_gone_from_disk_does_not_stop_the_session() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), "a");
    let session = Settings { open_repos: vec![name(&d.path().join("deleted")), name(&a)], ..Default::default() };
    session.save().unwrap();
    let mut h = run_app(&[]);
    settled(&mut h);
    assert_eq!(open_repos(&h), vec![name(&a)]);
    assert!(!error_dialog_showing(&h));
}

/// It may only be an unplugged drive, so a failed reopen doesn't end its place in the session.
#[test]
fn a_repository_that_cannot_be_reopened_stays_in_the_session_until_forgotten() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let a = repo(d.path(), "a");
    let away = name(&d.path().join("on-an-unplugged-drive"));
    Settings { open_repos: vec![away.clone(), name(&a)], ..Default::default() }.save().unwrap();
    {
        let mut h = run_app(&[]);
        settled(&mut h);
        assert!(h.query_by_label("Some repositories couldn't be reopened").is_some());
        h.get_by_label("Keep for next time").click();
        steps(&mut h, 3);
        assert!(h.query_by_label("Some repositories couldn't be reopened").is_none());
        h.state_mut().on_exit();
    }
    assert_eq!(saved().open_repos, vec![away.clone(), name(&a)]);
    {
        let mut h = run_app(&[]);
        settled(&mut h);
        h.get_by_label("Forget them").click();
        steps(&mut h, 3);
        h.state_mut().on_exit();
    }
    assert_eq!(saved().open_repos, vec![name(&a)]);
}

/// `open_repos` `[B, C]` became `[A]` the moment `gitem A` ran, so B and C were gone
/// at the next launch.
#[test]
fn opening_by_argument_keeps_the_saved_session() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let (a, b, c) = (repo(d.path(), "a"), repo(d.path(), "b"), repo(d.path(), "c"));
    Settings { open_repos: vec![name(&b), name(&c)], ..Default::default() }.save().unwrap();
    {
        let mut h = run_app(std::slice::from_ref(&a));
        settled(&mut h);
        h.state_mut().on_exit();
    }
    let kept = saved().open_repos;
    assert!(kept.contains(&name(&b)) && kept.contains(&name(&c)), "{kept:?}");
    let h = run_app(&[]);
    assert_eq!(open_repos(&h).len(), 3, "the next plain start brings them all back");
    assert!(h.query_by_label("Some repositories couldn't be reopened").is_none());
}

#[test]
fn opening_a_kept_repository_and_closing_it_takes_it_out_of_the_session() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (repo(d.path(), "a"), repo(d.path(), "b"));
    Settings { open_repos: vec![name(&b)], ..Default::default() }.save().unwrap();
    let mut h = run_app(std::slice::from_ref(&a));
    settled(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().open_repo(&ctx, &b, false);
    settled(&mut h);
    h.state_mut().repos.remove(1);
    h.state_mut().on_exit();
    assert_eq!(saved().open_repos, vec![name(&a)]);
}

// ---- saving ------------------------------------------------------------------------------

#[test]
fn a_changed_setting_reaches_the_file_without_waiting_for_exit() {
    let _turn = clean_slate();
    let mut h = run_app(&[]);
    steps(&mut h, 2);
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::Settings(Box::new(|s| s.theme = ThemeChoice::Light)));
    steps(&mut h, 3);
    assert_eq!(saved().theme, ThemeChoice::Light);
    let dir = Settings::path().parent().unwrap().to_path_buf();
    let temp_files = std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "tmp")).count();
    assert_eq!(temp_files, 0, "a save leaves no temp file behind");
}

#[test]
fn an_app_built_not_to_save_never_writes() {
    let _turn = clean_slate();
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, Settings::default());
    app.on_exit();
    assert!(!Settings::path().exists());
}

#[test]
fn a_failed_save_is_reported_and_the_app_keeps_going() {
    let _turn = clean_slate();
    let mut h = run_app(&[]);
    steps(&mut h, 2);
    // A folder where the file goes makes the final rename fail on every platform.
    std::fs::create_dir(Settings::path()).unwrap();
    h.state_mut().on_exit();
    steps(&mut h, 3);
    assert!(error_dialog_showing(&h));
    std::fs::remove_dir_all(Settings::path()).unwrap();
}

// ---- files from other versions -------------------------------------------------------------

#[test]
fn a_settings_file_from_an_older_version_loads_with_defaults_for_the_rest() {
    let _turn = clean_slate();
    std::fs::write(Settings::path(), r#"{"theme":"Light","recent_repos":["/old"]}"#).unwrap();
    let mut h = run_app(&[]);
    steps(&mut h, 3);
    assert_eq!(h.state().settings.theme, ThemeChoice::Light);
    assert_eq!(h.state().settings.recent_repos, vec!["/old"]);
    assert_eq!(h.state().settings.language, Settings::default().language);
    assert!(!error_dialog_showing(&h));
}

#[test]
fn a_field_from_a_newer_version_is_ignored() {
    let _turn = clean_slate();
    std::fs::write(Settings::path(), r#"{"theme":"Light","a_setting_from_the_future":{"x":1}}"#).unwrap();
    let mut h = run_app(&[]);
    steps(&mut h, 3);
    assert_eq!(h.state().settings.theme, ThemeChoice::Light);
    assert!(!error_dialog_showing(&h));
}

#[test]
fn a_corrupt_settings_file_still_lets_the_app_start() {
    for broken in ["", "{", r#"{"theme":"Light","workspaces":[{"name":"w","repos":[]}],"#, "not json at all", "\u{0}\u{1}\u{2}", r#"{"theme":17}"#, r#"[1,2,3]"#] {
        let _turn = clean_slate();
        std::fs::write(Settings::path(), broken).unwrap();
        let mut h = run_app(&[]);
        steps(&mut h, 3);
        assert!(h.state().repos.is_empty(), "{broken:?}");
    }
}

fn backups() -> Vec<String> {
    let dir = Settings::path().parent().unwrap().to_path_buf();
    std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".bad-")).map(|e| std::fs::read_to_string(e.path()).unwrap()).collect()
}

fn remove_backups() {
    let dir = Settings::path().parent().unwrap().to_path_buf();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.file_name().to_string_lossy().contains(".bad-") {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
}

/// A truncated file used to read as defaults, and the next save destroyed the original.
#[test]
fn a_corrupt_settings_file_is_kept_and_the_user_is_told() {
    let _turn = clean_slate();
    remove_backups();
    let broken = r#"{"theme":"Light","workspaces":[{"name":"w","repos":[]}],"#;
    std::fs::write(Settings::path(), broken).unwrap();
    let mut h = run_app(&[]);
    steps(&mut h, 3);
    assert!(error_dialog_showing(&h), "the user hears about it");
    h.state_mut().on_exit();
    assert_eq!(backups(), vec![broken]);
    assert_eq!(saved().theme, Settings::default().theme);
    remove_backups();
}

/// One enum value this build doesn't know used to throw away every other setting.
#[test]
fn an_unknown_theme_from_a_newer_version_keeps_every_other_setting() {
    let _turn = clean_slate();
    remove_backups();
    let text = r#"{"theme":"Neon","language":"tr","recent_repos":["/a"]}"#;
    std::fs::write(Settings::path(), text).unwrap();
    let mut h = run_app(&[]);
    assert_eq!(h.state().settings.language, "tr");
    assert_eq!(h.state().settings.recent_repos, vec!["/a"]);
    steps(&mut h, 3);
    assert!(error_dialog_showing(&h));
    assert_eq!(backups(), vec![text]);
    // Going back to the version that knows it still finds it.
    h.state_mut().on_exit();
    assert!(std::fs::read_to_string(Settings::path()).unwrap().contains("Neon"));
    remove_backups();
}

#[test]
fn a_byte_order_mark_from_an_editor_changes_nothing() {
    let _turn = clean_slate();
    remove_backups();
    std::fs::write(Settings::path(), "\u{feff}{\"theme\":\"Light\"}").unwrap();
    let mut h = run_app(&[]);
    steps(&mut h, 3);
    assert_eq!(h.state().settings.theme, ThemeChoice::Light);
    assert!(!error_dialog_showing(&h));
    assert!(backups().is_empty());
}

/// Some editor or crash can break the file while the app runs. That copy still isn't lost.
#[test]
fn a_file_broken_while_running_is_kept_before_saving_over_it() {
    let _turn = clean_slate();
    remove_backups();
    let mut h = run_app(&[]);
    steps(&mut h, 2);
    std::fs::write(Settings::path(), "{ half written").unwrap();
    h.state_mut().on_exit();
    steps(&mut h, 3);
    assert_eq!(backups(), vec!["{ half written"]);
    assert!(error_dialog_showing(&h));
    assert!(serde_json::from_str::<Settings>(&std::fs::read_to_string(Settings::path()).unwrap()).is_ok());
    remove_backups();
}

/// Saving used to rewrite the file from what this app knows, so a downgrade lost the rest.
#[test]
fn a_field_from_a_newer_version_survives_a_save() {
    let _turn = clean_slate();
    std::fs::write(Settings::path(), r#"{"theme":"Light","a_setting_from_the_future":{"x":1}}"#).unwrap();
    let mut h = run_app(&[]);
    steps(&mut h, 3);
    h.state_mut().on_exit();
    assert!(std::fs::read_to_string(Settings::path()).unwrap().contains("a_setting_from_the_future"));
}

// ---- two copies running --------------------------------------------------------------------

#[test]
fn two_copies_saving_leave_a_valid_file() {
    let _turn = clean_slate();
    let mut first = run_app(&[]);
    let mut second = run_app(&[]);
    steps(&mut first, 2);
    steps(&mut second, 2);
    first.state_mut().settings.theme = ThemeChoice::Light;
    second.state_mut().settings.theme = ThemeChoice::Forest;
    first.state_mut().on_exit();
    second.state_mut().on_exit();
    assert_eq!(saved().theme, ThemeChoice::Forest, "both changed it, so the last to save wins, for that setting only");
}

#[test]
fn each_copys_session_tabs_survive_the_others_save() {
    let _turn = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (repo(d.path(), "a"), repo(d.path(), "b"));
    let mut first = run_app(std::slice::from_ref(&a));
    let mut second = run_app(std::slice::from_ref(&b));
    settled(&mut first);
    settled(&mut second);
    first.state_mut().on_exit();
    second.state_mut().on_exit();
    let on_disk = saved();
    assert!(on_disk.session_tabs.contains_key(&name(&a)) && on_disk.session_tabs.contains_key(&name(&b)), "{:?}", on_disk.session_tabs.keys());
}

/// A running copy used to save its older view over what another copy just changed.
#[test]
fn a_copy_exiting_keeps_a_setting_another_copy_changed() {
    let _turn = clean_slate();
    let mut first = run_app(&[]);
    let mut second = run_app(&[]);
    steps(&mut first, 2);
    steps(&mut second, 2);
    let ctx = second.ctx.clone();
    second.state_mut().apply_action(&ctx, Action::Settings(Box::new(|s| s.language = "tr".to_string())));
    steps(&mut second, 3);
    assert_eq!(saved().language, "tr");
    first.state_mut().on_exit();
    assert_eq!(saved().language, "tr", "the other copy's change is gone");
    assert_eq!(first.state().settings.language, "tr", "and this copy now has it too");
}


/// AS-3: nothing in the window can be saved after a panic, or a power cut, so it's written
/// while it's being typed.
#[test]
fn a_commit_message_being_written_survives_a_crash() {
    let _t = clean_slate();
    let d = tempfile::tempdir().unwrap();
    let root = repo(d.path(), "r");
    let mut ui = run_app(std::slice::from_ref(&root));
    ui.state_mut().persist = true;
    settled(&mut ui);
    ui.state_mut().repos[0].open_tab(gitem::views::Tab::changes());
    steps(&mut ui, 3);
    for (_, t) in ui.state_mut().repos[0].dock.iter_all_tabs_mut() {
        if let gitem::views::Tab::Changes(c) = t {
            c.message = "half written".into();
        }
    }
    let t = Instant::now();
    let drafts = Settings::path().parent().unwrap().join("drafts");
    while std::fs::read_dir(&drafts).map_or(true, |mut e| e.next().is_none()) {
        ui.step();
        assert!(t.elapsed() < Duration::from_secs(60), "no draft written");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The first window is gone without saying goodbye.
    std::mem::forget(ui);
    let mut next = run_app(std::slice::from_ref(&root));
    next.state_mut().persist = true;
    settled(&mut next);
    next.state_mut().repos[0].open_tab(gitem::views::Tab::changes());
    steps(&mut next, 3);
    let text = next.get_by_role(egui::accesskit::Role::MultilineTextInput).value().unwrap_or_default().to_string();
    assert_eq!(text, "half written");
    let _ = std::fs::remove_dir_all(&drafts);
}
