//! The ways into a repository, for real: the saved session, the command line and links, as
//! `GitemApp::new` and `apply_action` take them, then the question and its two answers. And
//! `gitem --mcp` as a process, since MCP has no UI to ask from. Programs only note a word in
//! a marker file in a temp folder.
gitem_testenv::isolate!();

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use gitem::action::Action;
use gitem::app::GitemApp;
use gitem_core::forge::DeepLink;
use gitem_core::settings::Settings;
use gitem_core::trust::TrustState;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The settings path is process-wide, so these take turns.
fn clean_slate() -> MutexGuard<'static, ()> {
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let path = Settings::path();
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    turn
}

fn q(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git runs");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct Hostile {
    _dir: tempfile::TempDir,
    root: PathBuf,
    marker: PathBuf,
    parent: PathBuf,
}

impl Hostile {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().to_path_buf();
        let root = parent.join("hostile");
        std::fs::create_dir(&root).unwrap();
        q(&root, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            q(&root, &["config", k, v]);
        }
        std::fs::write(root.join(".gitattributes"), "*.txt filter=evil diff=evil\n").unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        q(&root, &["add", "-A"]);
        q(&root, &["commit", "-q", "-m", "base"]);
        let marker = parent.join("marker.txt");
        let prog = |word: &str| format!("sh -c 'echo {word} >> \"{}\"; cat' -", marker.to_string_lossy().replace('\\', "/"));
        q(&root, &["config", "filter.evil.clean", &prog("clean")]);
        q(&root, &["config", "filter.evil.required", "true"]);
        q(&root, &["config", "diff.evil.textconv", &prog("textconv")]);
        q(&root, &["config", "core.hooksPath", ".husky"]);
        let h = Self { _dir: dir, root, marker, parent };
        h.stat_dirty();
        h
    }

    /// What any zip or tar round trip leaves.
    fn stat_dirty(&self) {
        let f = std::fs::OpenOptions::new().write(true).open(self.root.join("a.txt")).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(10)).unwrap();
    }

    fn fired(&self) -> String {
        std::fs::read_to_string(&self.marker).unwrap_or_default()
    }

    fn sha(&self) -> String {
        q(&self.root, &["rev-parse", "HEAD"]).trim().to_string()
    }
}

fn run_app(paths: &[PathBuf]) -> Harness<'static, GitemApp> {
    let ctx = egui::Context::default();
    let app = GitemApp::new(&ctx, paths);
    Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app)
}

fn settled(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    // A view only starts its work when it's drawn.
    for _ in 0..3 {
        h.step();
    }
    while gitem::task::running(&h.ctx) > 0 || !h.state().ops_idle() || h.state().repos.iter().any(|r| r.data.status.busy() || r.data.refs.busy()) {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(180), "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..5 {
        h.step();
    }
}

fn click_button(h: &mut Harness<GitemApp>, label: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, label).click();
    h.step();
    h.step();
}

fn asking(h: &Harness<GitemApp>) -> bool {
    h.query_by_label_contains("These settings came with this folder").is_some()
}

fn save_session(paths: &[&Path]) {
    let s = Settings { open_repos: paths.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect(), ..Default::default() };
    s.save().unwrap();
}

fn delivered_runs_under_plain_git(h: &Hostile) {
    let o = Command::new("git").arg("-C").arg(&h.root).args(["status", "--porcelain"]).output().unwrap();
    assert!(o.status.success());
    assert!(h.fired().contains("clean"), "plain git runs the filter, or these tests prove nothing");
    let _ = std::fs::remove_file(&h.marker);
    h.stat_dirty();
}

#[test]
fn restoring_the_session_runs_nothing_and_asks() {
    let _t = clean_slate();
    let h = Hostile::new();
    delivered_runs_under_plain_git(&h);
    save_session(&[&h.root]);
    let mut ui = run_app(&[]);
    settled(&mut ui);
    assert_eq!(h.fired(), "");
    assert!(asking(&ui));
    assert!(ui.query_by_label_contains("filter.evil.clean").is_some(), "it names the key");
    assert!(ui.query_by_label_contains("For information: core.hooksPath = .husky").is_some());
    assert!(ui.query_by_label_contains(&ui.state().repos[0].repo.root.display().to_string()).is_some(), "and the folder");
}

#[test]
fn the_command_line_runs_nothing_and_asks() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    assert_eq!(h.fired(), "");
    assert!(asking(&ui));
}

#[test]
fn answering_keep_off_stays_quiet_with_a_banner_and_asks_no_more() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    assert!(ui.query_all_by_label_contains("look modified").next().is_some(), "the false M is explained");
    click_button(&mut ui, "Keep them off");
    settled(&mut ui);
    assert!(!asking(&ui));
    assert_eq!(h.fired(), "");
    assert_eq!(ui.state().repos[0].repo.trust.state, TrustState::Declined);
    assert!(ui.query_by_label_contains("Programs from this repository's config are turned off").is_some());
    assert!(ui.query_all_by_label_contains("look modified").next().is_some(), "still explained");

    let mut again = run_app(std::slice::from_ref(&h.root));
    settled(&mut again);
    assert!(!asking(&again), "the answer was kept");
    assert_eq!(h.fired(), "");

    // It can be changed later.
    click_button(&mut again, "Review…");
    assert!(asking(&again));
    click_button(&mut again, "Let them run");
    settled(&mut again);
    assert_eq!(again.state().repos[0].repo.trust.state, TrustState::Trusted);
}

#[test]
fn answering_yes_runs_them_and_hides_the_banner() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    click_button(&mut ui, "Let them run");
    h.stat_dirty();
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::Refresh);
    settled(&mut ui);
    assert!(!asking(&ui));
    assert!(ui.query_by_label_contains("Programs from this repository's config").is_none());
    assert!(h.fired().contains("clean"), "they run now");
    let mut again = run_app(std::slice::from_ref(&h.root));
    settled(&mut again);
    assert!(!asking(&again), "and that was remembered");
}

#[test]
fn a_new_key_asks_again_through_the_whole_app() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    click_button(&mut ui, "Let them run");
    settled(&mut ui);
    q(&h.root, &["config", "core.sshCommand", "something-new"]);
    let mut again = run_app(std::slice::from_ref(&h.root));
    settled(&mut again);
    assert!(asking(&again));
    assert!(again.query_by_label_contains("core.sshcommand = something-new").is_some());
}

/// Home reads the status of every repository in a workspace in the background, unasked.
#[test]
fn a_workspace_on_the_home_page_runs_nothing_and_does_not_fetch_it() {
    let _t = clean_slate();
    let h = Hostile::new();
    let shown = h.parent.join("shown");
    std::fs::create_dir(&shown).unwrap();
    q(&shown, &["init", "-q", "-b", "main"]);
    let s = Settings { workspaces: vec![gitem_core::settings::Workspace { name: "w".into(), repos: vec![h.root.to_string_lossy().replace('\\', "/")] }], ..Default::default() };
    s.save().unwrap();
    let mut ui = run_app(&[shown]);
    settled(&mut ui);
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::OpenTab(gitem::views::Tab::home()));
    settled(&mut ui);
    assert_eq!(h.fired(), "");
    let read = |ui: &Harness<GitemApp>, what: &str| ui.state().repos[0].repo.git.log.lock().unwrap().iter().any(|e| e.command.contains(what) && (e.cwd.contains("hostile") || e.command.contains("hostile")));
    assert!(read(&ui, "status"), "the test only means something if Home read it");

    click_button(&mut ui, "Fetch all");
    settled(&mut ui);
    assert!(!read(&ui, "fetch"), "it fetched a repository nobody has said anything about");
    assert!(ui.query_by_label_contains("Not fetching").is_some(), "and says why");
}

/// Maintenance on open is a write nobody asked for.
#[test]
fn automatic_maintenance_waits_for_an_answer() {
    let _t = clean_slate();
    let h = Hostile::new();
    let s = Settings { auto_maintenance: true, ..Default::default() };
    s.save().unwrap();
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    let log = ui.state().repos[0].repo.git.log.lock().unwrap();
    assert!(!log.iter().any(|e| e.command.contains("maintenance")), "it ran on an undecided repository");
}

#[test]
fn a_link_to_an_unknown_folder_asks_before_touching_it() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(&[]);
    settled(&mut ui);
    let d = DeepLink { repo: h.root.to_string_lossy().into_owned(), commit: Some(h.sha()), ..Default::default() };
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::DeepLink(d));
    settled(&mut ui);
    assert!(ui.state().repos.is_empty(), "nothing opened yet");
    assert!(ui.query_by_label_contains(&h.root.display().to_string()).is_some(), "it shows the full path");
    click_button(&mut ui, "Open it");
    settled(&mut ui);
    assert_eq!(ui.state().repos.len(), 1);
    assert!(asking(&ui), "and then the same question as any other way in");
    assert_eq!(h.fired(), "");
}

#[test]
fn declining_a_link_opens_nothing_and_a_known_folder_needs_no_question() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut ui = run_app(&[]);
    settled(&mut ui);
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::DeepLink(DeepLink { repo: h.root.to_string_lossy().into_owned(), ..Default::default() }));
    settled(&mut ui);
    click_button(&mut ui, "Don't open");
    settled(&mut ui);
    assert!(ui.state().repos.is_empty());

    let mut s = Settings::default();
    s.touch_recent(&h.root.to_string_lossy().replace('\\', "/"));
    s.save().unwrap();
    let mut ui = run_app(&[]);
    settled(&mut ui);
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::DeepLink(DeepLink { repo: h.root.to_string_lossy().into_owned(), ..Default::default() }));
    settled(&mut ui);
    assert_eq!(ui.state().repos.len(), 1, "in recents, so it opened");
    assert!(asking(&ui), "though its config still gets asked about");
    assert_eq!(h.fired(), "");
}

#[test]
fn a_link_to_a_network_path_is_refused_with_no_question() {
    let _t = clean_slate();
    for p in ["\\\\evil.invalid\\share\\repo", "//evil.invalid/share/repo"] {
        let mut ui = run_app(&[]);
        settled(&mut ui);
        let ctx = ui.ctx.clone();
        ui.state_mut().apply_action(&ctx, Action::DeepLink(DeepLink { repo: p.to_string(), ..Default::default() }));
        settled(&mut ui);
        assert!(ui.state().repos.is_empty());
        assert!(ui.query_by_label_contains("Open it").is_none(), "no way to say yes");
        assert!(ui.query_by_label_contains("network path").is_some());
    }
}

/// AS-17: the rest of the link was acted on in whichever repository was showing.
#[test]
fn a_link_that_fails_to_open_does_nothing_to_the_repository_on_screen() {
    let _t = clean_slate();
    let shown = Hostile::new();
    q(&shown.root, &["config", "--unset", "filter.evil.clean"]);
    q(&shown.root, &["config", "--unset", "diff.evil.textconv"]);
    let mut ui = run_app(std::slice::from_ref(&shown.root));
    settled(&mut ui);
    let tabs_before = ui.state().repos[0].dock.iter_all_tabs().count();
    let gone = shown.parent.join("gone");
    let mut s = Settings::load();
    s.touch_recent(&gone.to_string_lossy().replace('\\', "/"));
    ui.state_mut().settings = s;
    let ctx = ui.ctx.clone();
    ui.state_mut().apply_action(&ctx, Action::DeepLink(DeepLink { repo: gone.to_string_lossy().into_owned(), commit: Some("abcd".into()), file: Some("a.txt".into()), line: None }));
    settled(&mut ui);
    assert!(ui.query_by_label_contains("doesn't exist").is_some(), "it says so");
    assert_eq!(ui.state().repos[0].dock.iter_all_tabs().count(), tabs_before, "no File tab");
    assert!(ui.state().repos[0].inspect.sha.is_none(), "nothing being inspected");
}

/// A web page's link, parsed as `main.rs` parses it.
#[test]
fn a_link_with_an_option_for_a_commit_never_reaches_git() {
    let victim = std::env::temp_dir().join("gitem-link-victim.txt");
    assert_eq!(DeepLink::parse(&format!("gitem://open?repo=C:/r&commit=--output={}", victim.display())), None);
    let exe = env!("CARGO_BIN_EXE_gitem");
    let o = Command::new(exe).arg(format!("gitem://open?repo=C:/no/such/repo&commit=--output={}", victim.display())).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "refused before any window");
    assert!(!victim.exists());
}

fn mcp_session(dir: &Path, requests: &[serde_json::Value], env: &[(&str, &str)]) -> Vec<serde_json::Value> {
    let mut c = Command::new(env!("CARGO_BIN_EXE_gitem"));
    c.arg("--mcp").arg(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    {
        let mut si = child.stdin.take().unwrap();
        for r in requests {
            writeln!(si, "{r}").unwrap();
        }
    }
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn tool(id: u32, name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

#[test]
fn the_mcp_process_runs_nothing_writes_nothing_and_says_nothing_of_a_prompt() {
    let h = Hostile::new();
    delivered_runs_under_plain_git(&h);
    let victim = h.parent.join("victim.txt");
    let inject = format!("--output={}", victim.to_string_lossy().replace('\\', "/"));
    let replies = mcp_session(&h.root, &[
        tool(1, "git_status", serde_json::json!({})),
        tool(2, "git_diff", serde_json::json!({})),
        tool(3, "git_diff", serde_json::json!({"commit": h.sha()})),
        tool(4, "git_blame", serde_json::json!({"path": "a.txt"})),
        tool(5, "git_log", serde_json::json!({"query": format!("ref:{inject}")})),
        tool(6, "git_diff", serde_json::json!({"commit": inject})),
    ], &[]);
    assert_eq!(replies.len(), 6);
    assert_eq!(h.fired(), "", "an agent started the repository's programs");
    assert!(!victim.exists(), "an agent wrote a file");
    assert!(replies[0].to_string().contains("branch: main"), "status still answers: {}", replies[0]);
    assert_eq!(replies[4].pointer("/result/isError"), Some(&serde_json::json!(true)));
}

#[test]
fn mcp_stays_off_until_the_user_has_trusted_the_repository_in_gitem() {
    let _t = clean_slate();
    let h = Hostile::new();
    let mut repo = gitem_core::Repo::open(gitem_core::GitExe::discover(None).unwrap(), &h.root).unwrap();
    repo.set_trusted(true).unwrap();
    let replies = mcp_session(&h.root, &[tool(1, "git_status", serde_json::json!({}))], &[]);
    assert_eq!(replies.len(), 1);
    assert!(h.fired().contains("clean"), "trusted, so it works as in git");
}

/// The server has to name a version it speaks, not repeat whatever it was sent.
#[test]
fn mcp_answers_with_a_protocol_version_it_supports() {
    let h = Hostile::new();
    let init = |id: u32, v: &str| serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": {"protocolVersion": v}});
    let replies = mcp_session(&h.root, &[init(1, "2025-03-26"), init(2, "1999-01-01"), serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {}})], &[]);
    let got = |i: usize| replies[i].pointer("/result/protocolVersion").and_then(|v| v.as_str()).unwrap().to_string();
    assert_eq!(got(0), "2025-03-26");
    assert_eq!(got(1), "2025-06-18");
    assert_eq!(got(2), "2025-06-18");
}

/// A hook or a shell can leave these set. They must not make gitem work on another repository.
#[test]
fn inherited_git_variables_do_not_redirect_gitem() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().to_path_buf();
    let make = |name: &str, msg: &str| {
        let r = parent.join(name);
        std::fs::create_dir(&r).unwrap();
        q(&r, &["init", "-q", "-b", name]);
        q(&r, &["config", "user.name", "T"]);
        q(&r, &["config", "user.email", "t@example.com"]);
        std::fs::write(r.join(format!("in-{name}.txt")), "x\n").unwrap();
        q(&r, &["add", "-A"]);
        q(&r, &["commit", "-q", "-m", msg]);
        r
    };
    let (a, b) = (make("aaa", "commit-in-a"), make("bbb", "commit-in-b"));
    let a_git = a.join(".git").to_string_lossy().into_owned();
    let a_str = a.to_string_lossy().into_owned();
    let a_index = a.join(".git/index").to_string_lossy().into_owned();
    let env = [("GIT_DIR", a_git.as_str()), ("GIT_WORK_TREE", a_str.as_str()), ("GIT_INDEX_FILE", a_index.as_str())];
    let replies = mcp_session(&b, &[tool(1, "git_status", serde_json::json!({})), tool(2, "git_log", serde_json::json!({}))], &env);
    assert!(replies[0].to_string().contains("branch: bbb"), "{}", replies[0]);
    assert!(!replies[0].to_string().contains("in-"), "a borrowed index would make every file look changed: {}", replies[0]);
    assert!(replies[1].to_string().contains("commit-in-b") && !replies[1].to_string().contains("commit-in-a"), "{}", replies[1]);
}

/// A server chooses these URLs. A `file:` one is shown, but it's no link.
#[test]
fn only_web_links_can_be_clicked_through() {
    assert!(gitem::widgets::is_web_link("https://example.invalid/pr/1") && gitem::widgets::is_web_link("http://example.invalid/"));
    for bad in ["file:///C:/Windows/System32/calc.exe", "javascript:alert(1)", "C:/x", "ms-settings:", "", "HTTPS-not://x"] {
        assert!(!gitem::widgets::is_web_link(bad), "{bad}");
    }
    let clicked = |url: &'static str| {
        let mut h = Harness::builder().with_size(vec2(300.0, 100.0)).build_ui(move |ui| gitem::widgets::web_link(ui, "#12", url));
        h.run();
        h.get_by_label("#12").click();
        h.step();
        h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::OpenUrl(_)))
    };
    assert!(!clicked("file:///C:/Windows/System32/calc.exe"));
    assert!(clicked("https://example.invalid/pr/1"), "the check above would pass for anything if this failed");
}


/// Phase 4 read this from code, because it needs a timer.
#[test]
fn a_scheduled_fetch_waits_for_an_answer() {
    let _t = clean_slate();
    let h = Hostile::new();
    q(&h.root, &["remote", "add", "origin", "https://example.invalid/r.git"]);
    let mut ui = run_app(std::slice::from_ref(&h.root));
    settled(&mut ui);
    assert!(asking(&ui));
    ui.state_mut().settings.auto_fetch_minutes = 5;
    ui.state_mut().repos[0].make_fetch_due();
    for _ in 0..30 {
        ui.step();
    }
    settled(&mut ui);
    let log = ui.state().repos[0].repo.git.log.lock().unwrap();
    assert!(!log.iter().any(|e| e.command.starts_with("git fetch")), "it ran on an undecided repository");
}
