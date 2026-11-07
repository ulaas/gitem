//! Everyday features driven through the real UI against real git in throwaway repositories: undo,
//! branch cleanup, and what's added beside them. Clicks and keys are kittest's synthetic input.
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
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn write(&self, p: &str, s: &str) {
        let f = self.root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.root.join(p)).unwrap()
    }
    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }
    /// Beside the repository, in the form git can take as an argument.
    fn beside(&self, name: &str) -> PathBuf {
        self._dir.path().join(name)
    }
    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
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
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_label(h: &mut Harness<GitemApp>, label: &str) {
    wait_for(h, label, |h| h.query_by_label(label).is_some());
}

fn wait_for_text(h: &mut Harness<GitemApp>, text: &str) {
    wait_for(h, text, |h| h.query_by_label_contains(text).is_some());
}

fn click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label(h, label);
    // A dialog takes no clicks for a moment after it appears. A person never notices.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.get_by_label(label).click();
    h.step();
    h.step();
}

fn open(h: &mut Harness<GitemApp>, tab: Tab) {
    h.state_mut().repos[0].open_tab(tab);
    h.step();
    settle(h);
}

#[test]
fn a_hard_reset_is_taken_back_from_the_toolbar() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let tip = f.head();
    f.write("a.txt", "not committed\n");
    let mut h = f.harness(|_| {});
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::reset("HEAD~1", gitem_core::ops::ResetMode::Hard));
    settle(&mut h);
    assert_eq!(f.read("a.txt"), "one\n");
    click(&mut h, "Undo reset");
    wait_for_text(&mut h, "uncommitted changes come back");
    let t = Instant::now();
    while h.query_all_by_label("Undo reset").count() < 2 {
        h.step();
        assert!(t.elapsed() < PATIENCE);
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.query_all_by_label("Undo reset").last().unwrap().click();
    h.step();
    settle(&mut h);
    assert_eq!(f.head(), tip);
    assert_eq!(f.read("a.txt"), "not committed\n");
    assert!(h.query_by_label("Redo reset").is_some(), "and it can be redone");
}

#[test]
fn a_new_operation_replaces_what_can_be_undone() {
    let f = Fx::new();
    f.git(&["branch", "gone"]);
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("gone", false));
    settle(&mut h);
    wait_for_label(&mut h, "Undo delete of branch gone");
    f.write("s.txt", "s\n");
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::stage_all());
    settle(&mut h);
    assert!(h.query_by_label("Undo delete of branch gone").is_none());
}

#[test]
fn a_deleted_branch_comes_back_with_confirmations_off() {
    let f = Fx::new();
    f.git(&["branch", "gone"]);
    let tip = f.head();
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("gone", false));
    settle(&mut h);
    click(&mut h, "Undo delete of branch gone");
    settle(&mut h);
    assert_eq!(f.git(&["rev-parse", "refs/heads/gone"]).trim(), tip);
}

#[test]
fn undo_says_why_when_it_can_no_longer_be_done() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::reset("HEAD~1", gitem_core::ops::ResetMode::Keep));
    settle(&mut h);
    f.write("b.txt", "b\n");
    f.commit("made outside gitem");
    let there = f.head();
    settle(&mut h);
    click(&mut h, "Undo reset");
    settle(&mut h);
    wait_for(&mut h, "the refusal", |h| h.state().errors_showing().iter().any(|e| e.contains("moved")));
    assert_eq!(f.head(), there);
}

#[test]
fn a_remote_branch_whose_name_is_taken_goes_to_the_local_one() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["switch", "-q", "-c", "other"]);
    let mut h = f.harness(|_| {});
    let op = h.state().repos[0].data.check_out_remote("origin/main");
    assert_eq!(op.steps[0].args, ["switch", "main"], "not a second branch called main");
    h.state_mut().repos[0].queue_for_test(op);
    settle(&mut h);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]).trim(), "main");
}

#[test]
fn fast_forward_moves_a_branch_that_is_not_checked_out() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.git(&["switch", "-q", "-c", "side"]);
    f.git(&["push", "-q", "origin", "side:main"]);
    f.write("b.txt", "b\n");
    f.commit("ahead on side");
    f.git(&["push", "-q", "origin", "side:main"]);
    let tip = f.head();
    f.git(&["fetch", "-q", "origin"]);
    let mut h = f.harness(|_| {});
    let remotes = h.state().repos[0].data.remote_names();
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::fast_forward_branch("main", "origin/main", &remotes));
    settle(&mut h);
    assert_eq!(f.git(&["rev-parse", "refs/heads/main"]).trim(), tip);
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]).trim(), "side", "and nothing else moved");
}

#[test]
fn cleaning_up_deletes_whats_ticked_and_can_be_undone() {
    let f = Fx::new();
    f.git(&["branch", "merged-one"]);
    f.git(&["branch", "merged-two"]);
    f.git(&["switch", "-q", "-c", "keep-me"]);
    f.write("k.txt", "k\n");
    f.commit("not merged");
    f.git(&["switch", "-q", "main"]);
    let mut h = f.harness(|s| s.confirm_destructive = false);
    open(&mut h, Tab::Prune(Box::default()));
    wait_for(&mut h, "the list", |h| h.query_by_role_and_label(Role::CheckBox, "merged-one").is_some());
    assert!(h.query_by_role_and_label(Role::CheckBox, "keep-me").is_none(), "work in progress isn't offered");
    h.get_by_role_and_label(Role::CheckBox, "merged-two").click();
    h.step();
    h.step();
    click(&mut h, "Delete 1 branch");
    settle(&mut h);
    assert_eq!(f.git(&["branch", "--list", "merged-one"]), "");
    assert_ne!(f.git(&["branch", "--list", "merged-two"]), "", "it was unticked");
    click(&mut h, "Undo delete of branch merged-one");
    settle(&mut h);
    assert_ne!(f.git(&["branch", "--list", "merged-one"]), "");
}

#[test]
fn the_toolbars_pull_stashes_when_the_setting_says_to() {
    let f = Fx::new();
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare_path.to_string_lossy()]).unwrap();
    f.git(&["remote", "add", "origin", &bare_path.to_string_lossy()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f.write("a.txt", "two\n");
    f.commit("second");
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.write("a.txt", "mine\n");
    f.git(&["fetch", "-q", "origin"]);
    // In the way of what's coming, so without the stash the pull is refused.
    let mut h = f.harness(|s| {
        s.pull_autostash = true;
        s.pull_mode = "merge".to_string();
    });
    click(&mut h, "Pull 1");
    settle(&mut h);
    let log = f.git(&["log", "--format=%s", "-1"]);
    assert_eq!(log.trim(), "second", "{:?}", h.state().errors_showing());
    assert_ne!(f.git(&["status", "--porcelain"]), "", "its changes came back, in conflict or merged");
}

#[test]
fn undo_commit_can_be_redone() {
    let f = Fx::new();
    f.write("a.txt", "two\n");
    f.commit("second");
    let tip = f.head();
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::undo_commit());
    settle(&mut h);
    assert_ne!(f.head(), tip);
    click(&mut h, "Redo commit");
    settle(&mut h);
    assert_eq!(f.head(), tip);
}

// ---- the update check, against a mock on this machine

fn release_server(reply: String) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = asked.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
        }
    });
    (url, asked)
}

fn newer_release() -> String {
    serde_json::json!({"tag_name":"v99.0.0","html_url":"http://127.0.0.1:9/r","body":"Notes"}).to_string()
}

#[test]
fn nothing_asks_about_updates_unless_it_was_turned_on() {
    let (url, asked) = release_server(newer_release());
    let f = Fx::new();
    let mut h = f.harness(|s| {
        s.check_updates = false;
        s.update_url = url;
    });
    settle(&mut h);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn a_scheduled_check_says_what_it_found_once_a_day_only() {
    let (url, asked) = release_server(newer_release());
    let f = Fx::new();
    let mut h = f.harness(|s| {
        s.check_updates = true;
        s.update_url = url.clone();
    });
    wait_for_text(&mut h, "gitem 99.0.0 is available");
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(h.state().settings.last_update_check > 0, "so the next run waits");

    let (url2, asked2) = release_server(newer_release());
    let mut h = f.harness(|s| {
        s.check_updates = true;
        s.update_url = url2;
        s.last_update_check = gitem_core::util::now_ts() - 60;
    });
    settle(&mut h);
    assert_eq!(asked2.load(std::sync::atomic::Ordering::SeqCst), 0, "asked a minute ago");
}

#[test]
fn asking_from_about_answers_either_way_and_opens_nothing_by_itself() {
    let (url, asked) = release_server(serde_json::json!({"tag_name":"v0.0.1","html_url":"http://127.0.0.1:9/r"}).to_string());
    let f = Fx::new();
    let mut h = f.harness(|s| s.update_url = url);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    h.state_mut().apply_about_for_test();
    click(&mut h, "Check for updates");
    wait_for_text(&mut h, "This is the newest release.");

    let (url, _) = release_server(newer_release());
    let mut h = f.harness(|s| s.update_url = url);
    h.state_mut().apply_about_for_test();
    click(&mut h, "Check for updates");
    wait_for_label(&mut h, "Open its page");
    assert!(h.query_by_label_contains("Version 99.0.0 is available").is_some());

    // A broken address is said in the box and isn't an error window.
    let mut h = f.harness(|s| s.update_url = "http://example.invalid/latest".to_string());
    h.state_mut().apply_about_for_test();
    click(&mut h, "Check for updates");
    wait_for_text(&mut h, "Couldn't find out");
    assert!(h.state().errors_showing().is_empty());
}

// ---- workspaces

fn other_repo(f: &Fx, name: &str, remote: &std::path::Path) -> PathBuf {
    let path = f.beside(name);
    let p = path.to_string_lossy().replace('\\', "/");
    f.git.run(None, &["clone", "-q", &remote.to_string_lossy().replace('\\', "/"), &p]).unwrap();
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com")] {
        f.git.run(Some(&path), &["config", k, v]).unwrap();
    }
    path
}

#[test]
fn pull_all_goes_through_a_workspace_and_reports_failures_once() {
    let f = Fx::new();
    let bare = f.beside("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare.to_string_lossy().replace('\\', "/")]).unwrap();
    f.git(&["remote", "add", "origin", &bare.to_string_lossy().replace('\\', "/")]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    let (one, two) = (other_repo(&f, "one", &bare), other_repo(&f, "two", &bare));
    f.write("a.txt", "new\n");
    f.commit("on the remote");
    f.git(&["push", "-q", "origin", "main"]);
    let tip = f.head();
    let missing = f.beside("not-there");
    let list: Vec<String> = [&one, &two, &missing, &one.join("no")].iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
    let mut h = f.harness(|s| s.workspaces = vec![gitem_core::settings::Workspace { name: "All".to_string(), repos: list }]);
    open(&mut h, Tab::home());
    click(&mut h, "All");
    settle(&mut h);
    click(&mut h, "Pull all");
    wait_for(&mut h, "the report", |h| !h.state().errors_showing().is_empty());
    let shown = h.state().errors_showing().to_vec();
    assert_eq!(shown.len(), 1, "one report for all of them: {shown:?}");
    assert!(shown[0].contains("Pulled 2 repositories") && shown[0].contains("2 didn't work"), "{shown:?}");
    for p in [&one, &two] {
        assert_eq!(f.git.run(Some(p), &["rev-parse", "HEAD"]).unwrap().text().trim(), tip);
    }
}

// ---- small things

#[test]
fn a_bare_repository_is_called_that() {
    let f = Fx::new();
    let bare = f.beside("b.git");
    f.git.run(None, &["init", "-q", "--bare", &bare.to_string_lossy().replace('\\', "/")]).unwrap();
    let mut h = f.harness(|_| {});
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::OpenRepo(bare));
    wait_for(&mut h, "the message", |h| !h.state().errors_showing().is_empty());
    assert!(h.state().errors_showing()[0].contains("is a bare repository"), "{:?}", h.state().errors_showing());
}

#[test]
fn the_palette_reaches_a_stash_drop_and_it_can_be_undone() {
    let f = Fx::new();
    f.write("a.txt", "stashed\n");
    f.git(&["stash", "push", "-q", "-m", "saved work"]);
    let mut h = f.harness(|_| {});
    h.state_mut().open_palette_with(">Drop a Stash");
    h.step();
    h.key_press(egui::Key::Enter);
    wait_for_label(&mut h, "Drop a stash");
    let name = {
        let st = &h.state().repos[0].data.stashes()[0];
        format!("{}: {}", st.selector, st.message)
    };
    h.state_mut().form_set_for_test("s", &name);
    click(&mut h, "Drop");
    wait_for_text(&mut h, "Drop stash@{0}");
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
    }
    h.query_all_by_label("Drop").last().unwrap().click();
    h.step();
    settle(&mut h);
    assert_eq!(f.git(&["stash", "list"]), "");
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::UndoLast);
    wait_for_text(&mut h, "Undo drop of 1 stash");
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
    }
    h.query_all_by_label("Undo drop of 1 stash").last().unwrap().click();
    h.step();
    settle(&mut h);
    assert!(f.git(&["stash", "list"]).contains("saved work"));
}

#[test]
fn zen_mode_comes_through_its_own_action() {
    let f = Fx::new();
    let mut h = f.harness(|_| {});
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::ToggleZen);
    assert!(!h.state().repos[0].panels_showing());
    h.state_mut().apply_action(&ctx, gitem::action::Action::ToggleZen);
    assert!(h.state().repos[0].panels_showing());
}

#[test]
fn searching_contents_opens_the_file_and_works_at_a_revision() {
    let f = Fx::new();
    f.write("src.txt", "alpha\nneedle here\nomega\n");
    f.commit("with a needle");
    f.write("src.txt", "alpha\nomega\n");
    f.commit("without it");
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    assert!(repo.grep("needle", None, false, false).unwrap().is_empty());
    let found = repo.grep("NEEDLE", Some("HEAD~1"), true, false).unwrap();
    assert_eq!(found, vec![gitem_core::repo::GrepHit { path: "src.txt".into(), line: 2, text: "needle here".into() }]);
    assert_eq!(repo.grep("n[e]+dle", Some("HEAD~1"), false, true).unwrap().len(), 1);
    assert!(repo.grep("n[e]+dle", Some("HEAD~1"), false, false).unwrap().is_empty(), "plain text unless asked");
    assert!(repo.grep("x", Some("--output=evil"), false, false).is_err(), "a revision can't be an option");

    let mut h = f.harness(|_| {});
    h.state_mut().repos[0].open_tab(Tab::Grep(Box::new(gitem::views::grep::GrepView::looking_for("needle", "HEAD~1"))));
    h.step();
    wait_for(&mut h, "the view", |h| h.query_all_by_label("Search").count() > 1);
    h.query_all_by_label("Search").last().unwrap().click();
    h.step();
    wait_for_label(&mut h, "src.txt:2");
    click(&mut h, "src.txt:2");
    settle(&mut h);
    assert!(h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::File(v) if v.path == "src.txt" && v.rev.as_deref() == Some("HEAD~1"))), "at the revision it was found in");
}

#[test]
fn a_commit_template_and_recent_messages_are_there_to_use() {
    let f = Fx::new();
    let t = f.beside("tmpl.txt");
    std::fs::write(&t, "# a comment\nfeat: \n\nBecause\n").unwrap();
    f.git(&["config", "commit.template", &t.to_string_lossy().replace('\\', "/")]);
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    assert_eq!(repo.commit_template().as_deref(), Some("feat: \n\nBecause"));
    assert_eq!(repo.recent_messages(5).unwrap(), vec!["first".to_string()]);
    f.write("a.txt", "b\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "first"]);
    f.write("a.txt", "c\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "--author=Someone <s@example.com>", "-m", "not mine"]);
    assert_eq!(repo.recent_messages(5).unwrap(), vec!["first".to_string()], "repeats go, and others' too");
}

#[test]
fn amend_can_reset_author_and_date() {
    let f = Fx::new();
    f.git(&["commit", "-q", "--amend", "--no-edit", "--author=Other <o@example.com>", "--date=2001-01-01T00:00:00"]);
    let run = |o: gitem_core::ops::CommitOpts| gitem_core::ops::commit(&o).run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    run(gitem_core::ops::CommitOpts { message: "m".into(), amend: true, reset_date: true, ..Default::default() });
    let (who, year) = (f.git(&["log", "-1", "--format=%ae"]), f.git(&["log", "-1", "--format=%ad", "--date=format:%Y"]));
    assert_eq!(who.trim(), "o@example.com");
    assert_ne!(year.trim(), "2001");
    run(gitem_core::ops::CommitOpts { message: "m".into(), amend: true, reset_author: true, ..Default::default() });
    assert_eq!(f.git(&["log", "-1", "--format=%ae"]).trim(), "test@example.com");
}

#[test]
fn amending_a_pushed_commit_says_what_that_means() {
    let f = Fx::new();
    let bare = f.beside("r.git");
    f.git.run(None, &["init", "-q", "--bare", "-b", "main", &bare.to_string_lossy().replace('\\', "/")]).unwrap();
    f.git(&["remote", "add", "origin", &bare.to_string_lossy().replace('\\', "/")]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::changes());
    click(&mut h, "Amend");
    wait_for_text(&mut h, "already on origin/main");
}

#[test]
fn a_force_push_of_the_default_branch_is_asked_about_even_with_a_lease() {
    use gitem::action::Action;
    let mut form = gitem::forms::push_form_guarding(Some("main".into()), vec!["origin".into()], true, true, "main".into());
    form.set_for_test("lease", "1");
    let got = form.submit_for_test().unwrap();
    assert!(matches!(&got[0], Action::Confirm { title, .. } if title == "Rewrite the default branch"));
    let mut other = gitem::forms::push_form_guarding(Some("topic".into()), vec!["origin".into()], true, true, "main".into());
    other.set_for_test("lease", "1");
    assert!(matches!(&other.submit_for_test().unwrap()[0], Action::Run(_)), "any other branch goes straight through");
}

#[test]
fn start_work_can_start_the_agent_and_only_its_own_command_reaches_the_terminal() {
    use gitem::action::Action;
    let f = Fx::new();
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let issue = gitem_core::forge::Issue { id: "#5".into(), title: "& del everything".into(), ..Default::default() };
    let mut form = gitem::views::launchpad::start_work_form_with_agent(&repo, &issue, "", None, "fake-agent");
    form.set_for_test("agent", "1");
    let got = form.submit_for_test().unwrap();
    let Action::RunThen(_, then) = &got[0] else { panic!("the agent starts after the branch does") };
    let Action::OpenTerminal(dir, Some(cmd)) = &**then else { panic!("a terminal") };
    assert_eq!(cmd, "fake-agent");
    assert_eq!(dir, &repo.root);
    let mut plain = gitem::views::launchpad::start_work_form_with_agent(&repo, &issue, "", None, "");
    assert!(matches!(&plain.submit_for_test().unwrap()[0], Action::Run(_)));
}

#[test]
fn run_then_goes_on_only_if_it_worked() {
    use gitem::action::Action;
    let f = Fx::new();
    let mut h = f.harness(|_| {});
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, Action::RunThen(gitem_core::ops::branch_create("made", None, false), Box::new(Action::Toast("then this".into()))));
    wait_for_text(&mut h, "then this");
    settle(&mut h);
    h.state_mut().apply_action(&ctx, Action::RunThen(gitem_core::ops::branch_create("made", None, false), Box::new(Action::Toast("not this".into()))));
    settle(&mut h);
    assert!(h.query_by_label_contains("not this").is_none());
    assert!(!h.state().errors_showing().is_empty());
}

#[test]
fn settings_export_and_import_keep_what_belongs_here() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("copy.json");
    let there = Settings { start_work_format: "x-${id}".into(), recent_repos: vec!["there".into()], ..Default::default() };
    there.export_to(&file).unwrap();
    let mut here = Settings { recent_repos: vec!["here".into()], ..Default::default() };
    here.import_from(&file).unwrap();
    assert_eq!(here.start_work_format, "x-${id}");
    assert_eq!(here.recent_repos, ["here"]);

    std::fs::write(&file, "not json").unwrap();
    let before = here.start_work_format.clone();
    assert!(here.import_from(&file).unwrap_err().contains("isn't a gitem settings file"));
    assert_eq!(here.start_work_format, before);
    let mut form = gitem::views::settings::import_settings_form();
    form.set_for_test("path", &file.to_string_lossy());
    assert!(form.submit_for_test().is_err(), "said in the form");
}

#[test]
fn recursing_adds_itself_to_pull_and_check_out_only() {
    use gitem_core::ops;
    assert_eq!(ops::recursing_submodules(ops::checkout("x")).steps[0].args[..2], ["switch", "--recurse-submodules"]);
    assert_eq!(ops::recursing_submodules(ops::pull(ops::PullMode::FfOnly, false)).steps[0].args[..2], ["pull", "--recurse-submodules"]);
    assert_eq!(ops::recursing_submodules(ops::stage_all()).steps[0].args, ops::stage_all().steps[0].args);
}

#[test]
fn a_conflict_is_predicted_at_the_commit_it_would_stop_at() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("harmless");
    let harmless = f.head();
    f.write("a.txt", "topic\n");
    f.commit("touches a");
    let touching = f.head();
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("on main");
    let repo = gitem_core::Repo::open(f.git.clone(), &f.root).unwrap();
    let before = (f.git(&["status", "--porcelain"]), f.head());
    assert_eq!(repo.predict_replay("main", std::slice::from_ref(&harmless)).unwrap(), None);
    assert_eq!(repo.predict_replay("main", &[harmless, touching.clone()]).unwrap(), Some((touching, vec!["a.txt".to_string()])));
    assert_eq!((f.git(&["status", "--porcelain"]), f.head()), before, "and nothing moved");
}

#[test]
fn the_rebase_view_reports_what_it_predicts() {
    let f = Fx::new();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic\n");
    f.commit("touches a");
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("on main");
    f.git(&["switch", "-q", "topic"]);
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::rebase(Some("main".to_string())));
    click(&mut h, "Check for conflicts");
    wait_for_text(&mut h, "will conflict in a.txt");
}

#[test]
fn find_in_a_diff_counts_and_moves_through_matches() {
    let f = Fx::new();
    let many: String = (0..40).map(|i| format!("line {i}\n")).collect();
    f.write("a.txt", &many);
    f.commit("long");
    f.write("a.txt", &many.replace("line 3\n", "needle 3\n").replace("line 30\n", "needle 30\n"));
    let mut h = f.harness(|_| {});
    open(&mut h, Tab::diff(gitem_core::repo::DiffSource::Unstaged, Some("a.txt".to_string())));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.step();
    wait_for(&mut h, "the find box", |h| h.query_by_label_contains("Next match").is_some());
    let inputs = h.query_all_by_role(Role::TextInput).count();
    assert!(inputs >= 1);
    h.query_all_by_role(Role::TextInput).last().unwrap().focus();
    h.step();
    h.query_all_by_role(Role::TextInput).last().unwrap().type_text("NEEDLE");
    h.step();
    h.step();
    wait_for_text(&mut h, "1 of 2");
    click(&mut h, "Next match");
    wait_for_text(&mut h, "2 of 2");
    click(&mut h, "Next match");
    wait_for_text(&mut h, "1 of 2");
}

// ---- one window, and keys

#[test]
fn a_launch_handed_over_opens_in_this_window() {
    let f = Fx::new();
    let other = f.beside("other");
    std::fs::create_dir(&other).unwrap();
    f.git.run(Some(&other), &["init", "-q", "-b", "main"]).unwrap();
    let mut h = f.harness(|_| {});
    let dir = tempfile::tempdir().unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().listen_in_for_test(&ctx, dir.path());
    assert!(gitem_core::instance::hand_off(dir.path(), &[other.to_string_lossy().into_owned()]));
    wait_for(&mut h, "the second tab", |h| h.state().repos.len() == 2);
}

#[test]
fn a_changed_key_does_the_command_and_the_old_one_no_longer_does() {
    use egui::{Key, Modifiers};
    let f = Fx::new();
    let mut h = f.harness(|s| {
        s.key_bindings.insert("settings".to_string(), "Alt+F7".to_string());
    });
    let has_settings = |h: &Harness<GitemApp>| h.state().repos[0].dock.iter_all_tabs().any(|(_, t)| matches!(t, Tab::Settings(_)));
    assert!(!has_settings(&h));
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.step();
    h.step();
    assert!(!has_settings(&h), "the old keys do nothing now");
    h.key_press_modifiers(Modifiers::ALT, Key::F7);
    h.step();
    h.step();
    assert!(has_settings(&h));
}

#[test]
fn undo_has_a_key() {
    use egui::{Key, Modifiers};
    let f = Fx::new();
    f.git(&["branch", "gone"]);
    let mut h = f.harness(|s| s.confirm_destructive = false);
    h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("gone", false));
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::ALT), Key::Z);
    h.step();
    settle(&mut h);
    assert_ne!(f.git(&["branch", "--list", "gone"]), "");
}

#[test]
fn repository_tabs_can_be_moved_and_the_one_showing_stays() {
    let f = Fx::new();
    let other = f.beside("other");
    std::fs::create_dir(&other).unwrap();
    f.git.run(Some(&other), &["init", "-q", "-b", "main"]).unwrap();
    let mut h = f.harness(|_| {});
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::OpenRepo(other));
    wait_for(&mut h, "the second tab", |h| h.state().repos.len() == 2);
    settle(&mut h);
    let first = h.state().repos[0].repo.root.clone();
    let showing = h.state().active;
    h.state_mut().move_repo_tab(0, 1);
    assert_eq!(h.state().repos[1].repo.root, first);
    assert_eq!(h.state().active, 1 - showing, "the same repository is showing");
}
