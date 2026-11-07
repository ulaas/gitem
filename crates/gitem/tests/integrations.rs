//! Launchpad, Agents and Credentials driven through the real UI, against a mock host on
//! localhost. The mock answers API calls as Gitea's API is documented to and serves a real bare
//! repository over plain HTTP, so clicks reach real git and real sockets. It says nothing about
//! a live service: its replies were written by hand. Clicks and keys are kittest's synthetic
//! input. Secrets are fake and live in this binary's scratch folder.
gitem_testenv::isolate!();

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem::views::launchpad::LaunchpadView;
use gitem_core::GitExe;
use gitem_core::forge::{ForgeKind, identify};
use gitem_core::settings::{CustomHost, Settings};
use serde_json::{Value, json};

const PATIENCE: Duration = Duration::from_secs(180);
const SECRET: &str = "FAKE_SECRET_VALUE";

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    auth: String,
    body: String,
}

/// What the mock host holds, which a test or a request can change.
#[derive(Default)]
struct Host {
    prs: Vec<Value>,
    issues: Vec<Value>,
    log: Vec<Req>,
    /// For a path that starts so, this reply instead.
    fail: Vec<(String, u16, String)>,
    /// A GraphQL reply, when the host is being GitHub.
    graphql: Option<Value>,
}

type Shared = Arc<Mutex<Host>>;

fn api(host: &Shared, r: &Req) -> (u16, Vec<u8>) {
    let mut h = host.lock().unwrap();
    h.log.push(r.clone());
    if let Some((_, status, body)) = h.fail.iter().find(|(p, _, _)| r.path.starts_with(p.as_str())) {
        return (*status, body.clone().into_bytes());
    }
    let path = r.path.split('?').next().unwrap_or("");
    let ok = |v: Value| (200, v.to_string().into_bytes());
    match (r.method.as_str(), path) {
        ("POST", "/api/graphql") => ok(h.graphql.clone().unwrap_or(json!({}))),
        ("GET", "/api/v1/user") | ("GET", "/api/v3/user") => ok(json!({"login":"me"})),
        ("GET", "/api/v1/repos/o/r/pulls") => ok(Value::Array(h.prs.clone())),
        ("GET", "/api/v1/repos/o/r/issues") => ok(Value::Array(h.issues.clone())),
        ("POST", "/api/v1/repos/o/r/pulls") => (201, json!({"html_url":"http://127.0.0.1:9/created/1"}).to_string().into_bytes()),
        ("GET", "/api/v1/repos/o/r/pulls/7") => ok(json!({"head":{"sha":"headsha"},"base":{"sha":"basesha"}})),
        ("GET", "/api/v1/repos/o/r/pulls/7.diff") => (200, b"diff --git a/topic.txt b/topic.txt\n--- /dev/null\n+++ b/topic.txt\n@@ -0,0 +1,2 @@\n+first added line\n+second added line\n".to_vec()),
        ("GET", "/api/v1/repos/o/r/pulls/7/files") => ok(json!([{"filename":"topic.txt","status":"added","additions":2,"deletions":0}])),
        ("GET", "/api/v1/repos/o/r/commits/headsha/statuses") => ok(json!([{"context":"build-job","status":"failure","description":"It broke","target_url":"http://127.0.0.1:9/ci/1"}])),
        ("GET", "/api/v1/repos/o/r/issues/7/comments") => ok(json!([{"user":{"login":"bea"},"body":"A thought about this","created_at":"2026-01-02T00:00:00Z"}])),
        ("GET", "/api/v1/repos/o/r/pulls/7/reviews") => ok(json!([])),
        ("POST", p) if p.starts_with("/api/v1/repos/o/r/pulls/") && p.ends_with("/merge") => {
            let n: u64 = p.split('/').nth(7).and_then(|x| x.parse().ok()).unwrap_or(0);
            h.prs.retain(|x| x["number"] != n);
            (200, Vec::new())
        }
        ("POST", p) if p.starts_with("/api/v1/repos/o/r/") => (201, b"{}".to_vec()),
        ("GET", p) if p.starts_with("/jira/rest/") => ok(json!({"issues":[{"key":"JIRA-1","fields":{"summary":"From Jira","status":{"name":"Open"}}}]})),
        _ => (404, format!("{{\"message\":\"no route {}\"}}", r.path).into_bytes()),
    }
}

/// The API, and the bare repository's files under `/o/r.git/`, which is all git needs to fetch.
fn serve(bare: PathBuf) -> (String, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let host: Shared = Arc::default();
    let shared = host.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let (shared, bare) = (shared.clone(), bare.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    return;
                }
                let mut parts = first.split_whitespace();
                let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                        if k.eq_ignore_ascii_case("authorization") {
                            auth = v.trim().to_string();
                        }
                    }
                }
                let mut b = vec![0u8; len];
                if r.read_exact(&mut b).is_err() {
                    return;
                }
                let (status, reply) = if let Some(file) = path.strip_prefix("/o/r.git/") {
                    match std::fs::read(bare.join(file.split('?').next().unwrap_or(""))) {
                        Ok(bytes) => (200, bytes),
                        Err(_) => (404, Vec::new()),
                    }
                } else {
                    api(&shared, &Req { method, path, auth, body: String::from_utf8_lossy(&b).into_owned() })
                };
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len());
                let _ = s.write_all(&reply);
            });
        }
    });
    (base, host)
}

/// Someone's copy to push from, the bare repository behind the mock host, and a user's clone
/// whose `origin` is the mock host. Pull request 7 is the branch `topic`.
struct Fx {
    _dir: tempfile::TempDir,
    author: PathBuf,
    root: PathBuf,
    git: GitExe,
    base: String,
    host: Shared,
}

fn run(git: &GitExe, dir: &Path, args: &[&str]) -> String {
    git.run(Some(dir), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
}

fn identity(git: &GitExe, dir: &Path) {
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false"), ("http.proxy", "")] {
        run(git, dir, &["config", k, v]);
    }
}

fn gitea_pr(number: u64, head: &str, title: &str) -> Value {
    json!({"number":number,"title":title,"html_url":format!("http://127.0.0.1:9/pulls/{number}"),"user":{"login":"me"},"state":"open","mergeable":true,
        "head":{"ref":head,"repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}}})
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        let git = GitExe::discover(None).unwrap();
        let (bare, author, root) = (top.join("bare.git"), top.join("author"), top.join("user"));
        std::fs::create_dir_all(&author).unwrap();
        run(&git, &author, &["init", "-q", "-b", "main"]);
        identity(&git, &author);
        std::fs::write(author.join("a.txt"), "one\n").unwrap();
        run(&git, &author, &["add", "-A"]);
        run(&git, &author, &["commit", "-q", "-m", "first"]);
        // Relative: a canonical Windows path has a prefix that git can't take as an argument.
        run(&git, &top, &["clone", "-q", "--bare", "--no-local", "author", "bare.git"]);
        run(&git, &top, &["clone", "-q", "bare.git", "user"]);
        identity(&git, &root);
        let (base, host) = serve(bare.clone());
        run(&git, &root, &["remote", "set-url", "origin", &format!("{base}/o/r.git")]);
        let f = Self { _dir: dir, author, root, git, base, host };
        run(&f.git, &f.author, &["switch", "-q", "-c", "topic"]);
        f.author_commits("topic one");
        f.publish_topic(false);
        f.host.lock().unwrap().prs = vec![gitea_pr(7, "topic", "The topic")];
        f
    }

    fn author_commits(&self, subject: &str) -> String {
        std::fs::write(self.author.join("topic.txt"), format!("{subject}\n")).unwrap();
        run(&self.git, &self.author, &["add", "-A"]);
        run(&self.git, &self.author, &["commit", "-q", "-m", subject]);
        run(&self.git, &self.author, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// The branch and the pull request ref, and what a dumb HTTP client reads to find them.
    fn publish_topic(&self, force: bool) {
        let plus = if force { "+" } else { "" };
        run(&self.git, &self.author, &["push", "-q", "../bare.git", &format!("{plus}topic:refs/heads/topic"), &format!("{plus}topic:refs/pull/7/head")]);
        run(&self.git, &self.author, &["--git-dir=../bare.git", "update-server-info"]);
    }

    /// The author rewrites their branch.
    fn author_force_pushes(&self, subject: &str) -> String {
        run(&self.git, &self.author, &["reset", "-q", "--hard", "HEAD~1"]);
        let tip = self.author_commits(subject);
        self.publish_topic(true);
        tip
    }

    fn bare(&self) -> PathBuf {
        self.root.parent().unwrap().join("bare.git")
    }

    fn in_user(&self, args: &[&str]) -> String {
        run(&self.git, &self.root, args).trim().to_string()
    }

    fn custom(&self, kind: ForgeKind) -> CustomHost {
        CustomHost { host: "127.0.0.1".into(), kind, api_base: String::new(), templates: Default::default() }
    }

    fn harness(&self, kind: ForgeKind, tweak: impl FnOnce(&mut Settings)) -> Harness<'static, GitemApp> {
        let custom = self.custom(kind);
        let repo = identify(&format!("{}/o/r.git", self.base), std::slice::from_ref(&custom)).unwrap();
        gitem_core::cred::set_forge_token(&repo.token_host(), "", SECRET).unwrap();
        let mut settings = Settings { details_on_open: false, custom_hosts: vec![custom], ..Default::default() };
        tweak(&mut settings);
        let ctx = egui::Context::default();
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
        pump(&mut h, |a| a.repos.first().is_some_and(|r| r.data.status.get().is_some() && r.data.refs.get().is_some()), "the repository");
        quiet(&mut h);
        h
    }

    fn requests(&self, method: &str, path: &str) -> Vec<Req> {
        self.host.lock().unwrap().log.iter().filter(|r| r.method == method && r.path.starts_with(path)).cloned().collect()
    }
}

fn pump(h: &mut Harness<GitemApp>, cond: impl Fn(&GitemApp) -> bool, what: &str) {
    let t = Instant::now();
    loop {
        h.step();
        if cond(h.state()) {
            h.step();
            return;
        }
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn quiet(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut frames = 0;
    while frames < 5 {
        h.step();
        frames = if gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() { frames + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_label(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_by_label(label).is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn wait_for_text(h: &mut Harness<GitemApp>, text: &str) {
    let t = Instant::now();
    while h.query_by_label_contains(text).is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "nothing says {text:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label(h, label);
    // A dialog takes no clicks for a moment after it appears. A person never notices.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(250) {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    h.get_by_label(label).click();
    h.step();
    h.step();
}

/// For a label the title bar has too, whose copy comes first.
fn click_last(h: &mut Harness<GitemApp>, label: &str) {
    wait_for_label_any(h, label);
    h.query_all_by_label(label).last().unwrap().click();
    h.step();
    h.step();
}

fn wait_for_label_any(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_all_by_label(label).next().is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "no widget labelled {label:?}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn open_launchpad(h: &mut Harness<GitemApp>) {
    h.state_mut().repos[0].open_tab(Tab::Launchpad(Box::default()));
    wait_for_label(h, "Pull requests");
    quiet(h);
}

fn error_boxes(h: &Harness<GitemApp>) -> Vec<String> {
    use egui_kittest::kittest::NodeT;
    h.query_all_by_label_contains(gitem::widgets::ERROR_LABEL).map(|n| n.accesskit_node().value().unwrap_or_default()).collect()
}

fn no_errors(h: &Harness<GitemApp>) {
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
    assert!(error_boxes(h).is_empty(), "{:?}", error_boxes(h));
}

// ---- Pull requests, against the mock

#[test]
fn the_list_loads_with_the_token_and_shows_a_card() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    wait_for_text(&mut h, "The topic");
    wait_for_text(&mut h, "Waiting for review (1)");
    no_errors(&h);
    let calls = f.requests("GET", "/api/v1/repos/o/r/pulls");
    assert!(!calls.is_empty());
    assert!(calls.iter().all(|r| r.auth == format!("token {SECRET}")), "{calls:?}");
}

/// FG-3 and VS-4, from Check out through the author force-pushing, with real git over HTTP.
#[test]
fn a_checked_out_pull_request_follows_its_branch_through_a_force_push() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Check out");
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.branch.as_deref() == Some("pr/7")), "the switch to pr/7");
    quiet(&mut h);
    no_errors(&h);
    let tip = f.in_user(&["rev-parse", "pr/7"]);
    assert_eq!(tip, f.in_user(&["rev-parse", "origin/topic"]));
    assert_eq!(f.in_user(&["rev-parse", "--abbrev-ref", "pr/7@{u}"]), "origin/topic", "so Pull and the counts work");
    wait_for_label(&mut h, "Current branch");
    assert!(h.query_by_label("Check out").is_none(), "it's what's checked out");
    pump(&mut h, |a| a.repos[0].data.branch_pr.as_ref().is_some_and(|p| p.1 == 7), "the status bar's link");

    // New commits come in by Update.
    let newer = f.author_commits("topic two");
    f.publish_topic(false);
    click(&mut h, "Update");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), newer);

    // A force-push: Update refuses, as nothing may be lost without asking.
    let rewritten = f.author_force_pushes("topic rewritten");
    click(&mut h, "Update");
    quiet(&mut h);
    assert!(!h.state().errors_showing().is_empty(), "it should say it couldn't");
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), newer, "nothing changed");
    click(&mut h, "Dismiss");

    // Asking to reset takes confirming, and does it.
    click(&mut h, "Reset to its tip…");
    wait_for_text(&mut h, "Uncommitted changes are lost");
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), newer, "not yet");
    click(&mut h, "Reset");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), rewritten);

    // And off that branch, a second Check out after another force-push works: the review's
    // `! [rejected] (non-fast-forward)`.
    f.in_user(&["switch", "main"]);
    let again = f.author_force_pushes("topic again");
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.branch.as_deref() == Some("main")), "the switch back");
    quiet(&mut h);
    click(&mut h, "Check out");
    pump(&mut h, |a| a.repos[0].data.status.get().is_some_and(|s| s.branch.as_deref() == Some("pr/7")), "the second switch");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), again);
}

/// What the user adds to `pr/7`, after Check out made it.
fn checked_out_with_a_commit_of_mine(f: &Fx, h: &mut Harness<'static, GitemApp>) -> String {
    click(h, "Check out");
    wait_for_label(h, "Current branch");
    quiet(h);
    std::fs::write(f.root.join("mine.txt"), "mine\n").unwrap();
    f.in_user(&["add", "-A"]);
    f.in_user(&["commit", "-q", "-m", "my own work"]);
    f.in_user(&["rev-parse", "HEAD"])
}

fn back_on_main_and_reloaded(f: &Fx, h: &mut Harness<'static, GitemApp>) {
    f.in_user(&["switch", "-q", "main"]);
    h.key_press(egui::Key::F5);
    wait_for_label(h, "Check out");
    quiet(h);
}

#[test]
fn check_out_over_commits_of_mine_asks_and_keeps_them() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    let mine = checked_out_with_a_commit_of_mine(&f, &mut h);
    back_on_main_and_reloaded(&f, &mut h);
    click(&mut h, "Check out");
    wait_for_text(&mut h, "pr/7 has commits of yours");
    wait_for_text(&mut h, "my own work");
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), mine, "nothing changes before the answer");
    click(&mut h, "Continue");
    wait_for_label(&mut h, "Current branch");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7-saved"]), mine);
    assert_eq!(f.in_user(&["rev-parse", "HEAD"]), f.in_user(&["rev-parse", "refs/remotes/origin/topic"]));
    assert_eq!(f.in_user(&["branch", "--show-current"]), "pr/7");
}

#[test]
fn check_out_over_commits_of_mine_can_be_cancelled_or_told_to_replace() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    let mine = checked_out_with_a_commit_of_mine(&f, &mut h);
    back_on_main_and_reloaded(&f, &mut h);
    click(&mut h, "Check out");
    wait_for_text(&mut h, "pr/7 has commits of yours");
    click(&mut h, "Cancel");
    quiet(&mut h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), mine);
    assert_eq!(f.in_user(&["branch", "--show-current"]), "main");
    click(&mut h, "Check out");
    wait_for_text(&mut h, "pr/7 has commits of yours");
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    h.step();
    h.step();
    click(&mut h, "Replace them. The reflog has them for a while");
    click(&mut h, "Continue");
    wait_for_label(&mut h, "Current branch");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["rev-parse", "HEAD"]), f.in_user(&["rev-parse", "refs/remotes/origin/topic"]));
    assert!(f.in_user(&["branch", "--list", "pr/7-saved"]).is_empty());
}

#[test]
fn checking_out_again_with_nothing_of_mine_there_just_does_it() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Check out");
    wait_for_label(&mut h, "Current branch");
    quiet(&mut h);
    back_on_main_and_reloaded(&f, &mut h);
    f.author_force_pushes("rewritten");
    click(&mut h, "Check out");
    wait_for_label(&mut h, "Current branch");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["log", "-1", "--format=%s"]), "rewritten");
}

/// git refuses a plain push from `pr/7` to `origin/topic`. Pushing goes to a real bare repository.
#[test]
fn push_from_a_pull_requests_branch_goes_back_to_its_own_branch() {
    let f = Fx::new();
    f.in_user(&["remote", "set-url", "--push", "origin", "../bare.git"]);
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    let mine = checked_out_with_a_commit_of_mine(&f, &mut h);
    h.key_press(egui::Key::F5);
    wait_for_label(&mut h, "Push 1");
    quiet(&mut h);
    click(&mut h, "Push 1");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["--git-dir=../bare.git", "rev-parse", "refs/heads/topic"]), mine);
    assert!(f.in_user(&["--git-dir=../bare.git", "for-each-ref", "--format=%(refname)", "refs/heads/pr"]).is_empty(), "no pr/7 on the remote");
}

#[test]
fn a_forks_pull_request_pushes_back_to_a_remote_for_the_fork() {
    let f = Fx::new();
    let mut pr = gitea_pr(7, "their-branch", "From a fork");
    pr["head"]["repo"]["full_name"] = json!("them/r");
    f.host.lock().unwrap().prs = vec![pr];
    run(&f.git, f.bare().parent().unwrap(), &["clone", "-q", "--bare", "--no-local", "bare.git", "fork.git"]);
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Check out");
    wait_for_label(&mut h, "Current branch");
    quiet(&mut h);
    std::fs::write(f.root.join("fix.txt"), "fix\n").unwrap();
    f.in_user(&["add", "-A"]);
    f.in_user(&["commit", "-q", "-m", "a fix for them"]);
    let mine = f.in_user(&["rev-parse", "HEAD"]);
    click(&mut h, "Push to the fork…");
    wait_for_label(&mut h, "Fork's address");
    use egui_kittest::kittest::NodeT;
    // Worked out from the remote's own address.
    let shown = format!("{}/them/r.git", f.base);
    let input = egui::accesskit::Role::TextInput;
    let field = h.query_all_by_role(input).last().unwrap();
    assert_eq!(field.accesskit_node().value().unwrap_or_default(), shown);
    field.focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    // The mock can't take a push, so it goes to a real bare repository instead.
    h.query_all_by_role(input).last().unwrap().type_text("../fork.git");
    h.step();
    h.step();
    click_last(&mut h, "Push");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["--git-dir=../fork.git", "rev-parse", "refs/heads/their-branch"]), mine);
    assert_eq!(f.in_user(&["rev-parse", "--abbrev-ref", "@{upstream}"]), "them/their-branch");
}

#[test]
fn open_in_worktree_fetches_and_offers_the_form() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Open in worktree");
    wait_for_label(&mut h, "Add");
    quiet(&mut h);
    assert_eq!(f.in_user(&["rev-parse", "pr/7"]), f.in_user(&["rev-parse", "origin/topic"]));
    click(&mut h, "Add");
    quiet(&mut h);
    no_errors(&h);
    assert!(f.in_user(&["worktree", "list"]).contains("[pr/7]"), "{}", f.in_user(&["worktree", "list"]));
}

#[test]
fn merging_asks_then_posts_and_the_list_reloads() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    let lists_before = f.requests("GET", "/api/v1/repos/o/r/pulls").len();
    click(&mut h, "Merge…");
    wait_for_label(&mut h, "How");
    assert!(f.requests("POST", "/api/v1/repos/o/r/pulls/7/merge").is_empty(), "not before it's confirmed");
    click(&mut h, "Merge");
    wait_for_text(&mut h, "Merged #7");
    quiet(&mut h);
    no_errors(&h);
    let merged = f.requests("POST", "/api/v1/repos/o/r/pulls/7/merge");
    assert_eq!(merged.len(), 1);
    assert_eq!(serde_json::from_str::<Value>(&merged[0].body).unwrap()["Do"], "merge");
    assert!(f.requests("GET", "/api/v1/repos/o/r/pulls").len() > lists_before, "reloads after a merge");
    wait_for_text(&mut h, "No open pull requests");
}

#[test]
fn a_merge_the_host_refuses_says_why() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    f.host.lock().unwrap().fail.push(("/api/v1/repos/o/r/pulls/7/merge".to_string(), 405, json!({"message":"Not mergeable yet"}).to_string()));
    click(&mut h, "Merge…");
    click(&mut h, "Merge");
    pump(&mut h, |a| !a.errors_showing().is_empty(), "the error");
    assert!(h.state().errors_showing()[0].contains("Not mergeable yet"), "{:?}", h.state().errors_showing());
    assert!(h.query_by_label_contains("Merged #7").is_none());
}

#[test]
fn commenting_posts_what_was_typed() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Comment…");
    wait_for_label(&mut h, "Post");
    quiet(&mut h);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("Looks right");
    h.step();
    h.step();
    click(&mut h, "Post");
    wait_for_text(&mut h, "Commented on #7");
    quiet(&mut h);
    no_errors(&h);
    let posted = f.requests("POST", "/api/v1/repos/o/r/issues/7/comments");
    assert_eq!(serde_json::from_str::<Value>(&posted[0].body).unwrap()["body"], "Looks right");
}

#[test]
fn approving_posts_and_says_so() {
    let f = Fx::new();
    f.host.lock().unwrap().prs[0]["user"]["login"] = json!("someone-else");
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    click(&mut h, "Other open (1)");
    click(&mut h, "Approve");
    wait_for_text(&mut h, "Approved #7");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(serde_json::from_str::<Value>(&f.requests("POST", "/api/v1/repos/o/r/pulls/7/reviews")[0].body).unwrap()["event"], "APPROVED");
}

#[test]
fn your_own_pull_request_offers_no_approve() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    wait_for_label(&mut h, "Merge…");
    assert!(h.query_by_label("Approve").is_none());
}

#[test]
fn a_draft_or_conflicted_pull_request_cannot_be_merged_from_here() {
    use egui_kittest::kittest::NodeT;
    let f = Fx::new();
    f.host.lock().unwrap().prs = vec![{
        let mut p = gitea_pr(7, "topic", "The topic");
        p["draft"] = json!(true);
        p
    }];
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    wait_for_label(&mut h, "Merge…");
    assert!(h.get_by_label("Merge…").accesskit_node().is_disabled(), "a draft");
    f.host.lock().unwrap().prs = vec![{
        let mut p = gitea_pr(7, "topic", "The topic");
        p["mergeable"] = json!(false);
        p
    }];
    click_last(&mut h, "Refresh");
    quiet(&mut h);
    wait_for_text(&mut h, "Conflicts");
    assert!(h.get_by_label("Merge…").accesskit_node().is_disabled(), "with conflicts");
    click_last(&mut h, "Pin");
    no_errors(&h);
}

fn open_details(f: &Fx, h: &mut Harness<'static, GitemApp>) {
    f.host.lock().unwrap().prs[0]["body"] = json!("What this pull request does");
    click_last(h, "Refresh");
    quiet(h);
    if f.host.lock().unwrap().prs[0]["user"]["login"] != "me" {
        click(h, "Other open (1)");
    }
    click(h, "Show details");
    wait_for_text(h, "What this pull request does");
    quiet(h);
}

#[test]
fn a_pull_requests_details_show_its_files_checks_and_conversation() {
    let f = Fx::new();
    f.host.lock().unwrap().prs[0]["user"]["login"] = json!("someone-else");
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    open_details(&f, &mut h);
    no_errors(&h);
    wait_for_label(&mut h, "Files (1)");
    click(&mut h, "Files (1)");
    wait_for_text(&mut h, "+first added line");
    click(&mut h, "Checks (1)");
    wait_for_label(&mut h, "build-job");
    wait_for_text(&mut h, "It broke");
    click(&mut h, "Conversation (1)");
    wait_for_text(&mut h, "A thought about this");
}

#[test]
fn a_review_with_a_verdict_is_posted_and_the_words_stay_if_it_fails() {
    let f = Fx::new();
    f.host.lock().unwrap().prs[0]["user"]["login"] = json!("someone-else");
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    open_details(&f, &mut h);
    f.host.lock().unwrap().fail.push(("/api/v1/repos/o/r/pulls/7/reviews".to_string(), 500, json!({"message":"Host is down"}).to_string()));
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("Needs a test");
    h.step();
    h.step();
    click(&mut h, "Request changes");
    pump(&mut h, |a| !a.errors_showing().is_empty(), "the error");
    assert!(h.state().errors_showing()[0].contains("Host is down"));
    assert_eq!(h.state().repos[0].open_tab_unsaved("pr:7").as_deref(), Some("A review you're writing."), "what was written is kept");
    f.host.lock().unwrap().fail.clear();
    h.state_mut().errors_clear_for_test();
    click(&mut h, "Request changes");
    wait_for_text(&mut h, "Requested changes on #7");
    quiet(&mut h);
    let posted = f.requests("POST", "/api/v1/repos/o/r/pulls/7/reviews");
    let body: Value = serde_json::from_str(&posted.last().unwrap().body).unwrap();
    assert_eq!(body["event"], "REQUEST_CHANGES");
    assert_eq!(body["body"], "Needs a test");
    assert_eq!(h.state().repos[0].open_tab_unsaved("pr:7"), None, "and it's cleared once the host has it");
}

#[test]
fn a_comment_on_a_line_goes_to_that_line() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    open_details(&f, &mut h);
    click(&mut h, "Files (1)");
    wait_for_text(&mut h, "+second added line");
    click(&mut h, "    2");
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("On this line");
    h.step();
    h.step();
    click(&mut h, "Post comment");
    wait_for_text(&mut h, "Commented on line 2 of topic.txt");
    quiet(&mut h);
    let posted = f.requests("POST", "/api/v1/repos/o/r/pulls/7/reviews");
    let body: Value = serde_json::from_str(&posted[0].body).unwrap();
    assert_eq!(body["comments"][0], json!({"path":"topic.txt","body":"On this line","new_position":2}));
    assert_eq!(body["commit_id"], "headsha");
}

#[test]
fn a_merge_from_the_details_can_squash() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    open_details(&f, &mut h);
    click(&mut h, "Merge…");
    wait_for_label(&mut h, "How");
    h.state_mut().form_set_for_test("style", "Squash into one commit");
    click(&mut h, "Merge");
    wait_for_text(&mut h, "Merged #7");
    quiet(&mut h);
    let merged = f.requests("POST", "/api/v1/repos/o/r/pulls/7/merge");
    assert_eq!(serde_json::from_str::<Value>(&merged[0].body).unwrap()["Do"], "squash");
}

#[test]
fn creating_a_draft_sends_its_title_and_goes_back_to_the_list() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    let mut v = LaunchpadView::create_pr_for("topic", "main");
    v.write_for_test("My title", "My body");
    h.state_mut().repos[0].open_tab(Tab::Launchpad(Box::new(v)));
    wait_for_label(&mut h, "Open in browser instead");
    quiet(&mut h);
    click(&mut h, "Draft");
    click(&mut h, "Create");
    wait_for_text(&mut h, "Created http://127.0.0.1:9/created/1");
    quiet(&mut h);
    no_errors(&h);
    let sent = f.requests("POST", "/api/v1/repos/o/r/pulls");
    let body: Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!((body["title"].as_str(), body["body"].as_str(), body["head"].as_str(), body["base"].as_str()), (Some("WIP: My title"), Some("My body"), Some("topic"), Some("main")));
    assert!(h.query_by_label("Open in browser instead").is_none(), "back on the list");
}

/// SN-16.
#[test]
fn a_forks_pull_request_on_a_branch_of_the_same_name_is_not_the_current_branch() {
    let f = Fx::new();
    let mut fork = gitea_pr(9, "main", "From a fork of it");
    fork["head"]["repo"]["full_name"] = json!("someone/r");
    f.host.lock().unwrap().prs = vec![fork];
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    wait_for_text(&mut h, "From a fork of it");
    wait_for_label(&mut h, "From a fork");
    assert!(h.query_by_label("Current branch").is_none(), "this repository's main isn't that");
    assert!(h.query_by_label("Check out").is_some());
    assert_eq!(h.state().repos[0].data.branch_pr, None, "and the status bar doesn't claim it");
    {
        use egui_kittest::kittest::NodeT;
        assert!(h.get_by_label("Compare").accesskit_node().is_disabled(), "there is no such branch on this remote");
    }
}

/// SN-37, with the mock being GitHub.
#[test]
fn a_host_that_returns_part_of_the_list_gets_a_warning_beside_it() {
    let f = Fx::new();
    f.host.lock().unwrap().graphql = Some(json!({"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"title":"Partly here","headRefName":"f","baseRefName":"main","author":{"login":"me"}}]}}},"errors":[{"message":"Resource not accessible by personal access token"}]}));
    let mut h = f.harness(ForgeKind::GitHub, |_| {});
    open_launchpad(&mut h);
    wait_for_text(&mut h, "Partly here");
    wait_for_text(&mut h, "not accessible by personal access token");
    no_errors(&h);
}

// ---- Which reloads ask the host (Phase 7's remainder of VS-22)

#[test]
fn a_finished_text_tab_does_not_ask_the_host_again_but_f5_does() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    let lists = || f.requests("GET", "/api/v1/repos/o/r/pulls").len();
    let before = lists();
    assert!(before > 0);
    h.state_mut().repos[0].open_tab(Tab::Text(Box::new(gitem::views::textview::TextView::new("A text job", Box::new(|| Ok("Some answer".to_string()))))));
    wait_for_text(&mut h, "Some answer");
    quiet(&mut h);
    // Launchpad draws again, where a stale generation would show.
    h.state_mut().repos[0].open_tab(Tab::Launchpad(Box::default()));
    wait_for_label(&mut h, "Pull requests");
    quiet(&mut h);
    assert_eq!(lists(), before, "a finished Text tab reloaded the host's lists");

    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::Refresh);
    h.step();
    quiet(&mut h);
    assert!(lists() > before, "F5 is meant to");
}

// ---- Several sources (VS-23)

fn with_jira(f: &Fx) -> impl FnOnce(&mut Settings) + use<> {
    let base = format!("{}/jira", f.base);
    move |s: &mut Settings| {
        s.issue_trackers.jira_base_url = base;
        s.issue_trackers.jira_email = "e@example.com".to_string();
    }
}

fn issues_page(h: &mut Harness<GitemApp>) {
    click(h, "Issues");
    quiet(h);
}

#[test]
fn one_failed_source_shows_above_what_the_others_found() {
    let f = Fx::new();
    gitem_core::cred::store("gitem:forge:jira", "e", SECRET).unwrap();
    f.host.lock().unwrap().issues = vec![json!({"number":3,"title":"From the host","html_url":"http://127.0.0.1:9/3","state":"open","pull_request":null})];
    let tweak = with_jira(&f);
    f.host.lock().unwrap().fail.push(("/jira/".to_string(), 500, "Jira's down".to_string()));
    let mut h = f.harness(ForgeKind::Gitea, tweak);
    open_launchpad(&mut h);
    issues_page(&mut h);
    wait_for_text(&mut h, "From the host");
    assert!(error_boxes(&h).iter().any(|b| b.contains("Jira's down")), "{:?}", error_boxes(&h));

    // The other way round, with the fault gone from Jira.
    {
        let mut host = f.host.lock().unwrap();
        host.fail.clear();
        host.fail.push(("/api/v1/repos/o/r/issues".to_string(), 500, json!({"message":"The host is down"}).to_string()));
    }
    click_last(&mut h, "Refresh");
    quiet(&mut h);
    wait_for_text(&mut h, "From Jira");
    assert!(error_boxes(&h).iter().any(|b| b.contains("The host is down")), "{:?}", error_boxes(&h));
    assert!(h.query_by_label_contains("From the host").is_none());
}

#[test]
fn start_work_makes_a_branch_from_an_issue() {
    let f = Fx::new();
    f.host.lock().unwrap().issues = vec![json!({"number":3,"title":"Fix the thing","html_url":"http://127.0.0.1:9/3","state":"open","pull_request":null})];
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_launchpad(&mut h);
    issues_page(&mut h);
    click(&mut h, "Start work…");
    click(&mut h, "Start");
    quiet(&mut h);
    no_errors(&h);
    assert_eq!(f.in_user(&["branch", "--show-current"]), "3-fix-the-thing");
}

// ---- Agents, reading the scratch home its transcripts are in there

fn transcript(root: &str, id: &str) {
    let dir = gitem_core::settings::config_dir().join("agent-home/.claude/projects").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let (cwd, file) = (root.to_string(), format!("{root}/a.txt"));
    let lines = [
        json!({"type":"user","cwd":cwd,"gitBranch":"main","sessionId":id,"message":{"content":[{"type":"text","text":"hi"}]}}),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"Editing the file"},{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":file}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}),
    ];
    std::fs::write(dir.join(format!("{id}.jsonl")), lines.map(|l| l.to_string()).join("\n")).unwrap();
}

#[test]
fn agent_sessions_read_only_the_scratch_folder() {
    let roots = gitem_core::agents::default_roots();
    assert!(roots.iter().all(|(_, p)| p.starts_with(gitem_core::settings::config_dir())), "{roots:?}");
    assert!(gitem_core::agents::opencode_db().starts_with(gitem_core::settings::config_dir()));
}

#[test]
fn an_agent_session_can_be_archived_brought_back_and_looked_into() {
    let f = Fx::new();
    transcript(&f.in_user(&["rev-parse", "--show-toplevel"]), "session-archive-test");
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    h.state_mut().repos[0].open_tab(Tab::Agents(Box::default()));
    wait_for_label(&mut h, "Activity");
    wait_for_text(&mut h, "Editing the file");

    click(&mut h, "Activity");
    wait_for_text(&mut h, "Files this Claude Code session touched recently");
    click(&mut h, "Close activity");
    assert!(h.query_by_label_contains("touched recently").is_none());

    click(&mut h, "Archive");
    wait_for_text(&mut h, "Show archived (1)");
    assert!(h.query_by_label("Activity").is_none(), "hidden");
    assert!(h.state().settings.archived_sessions.iter().any(|s| s == "session-archive-test"));

    click(&mut h, "Show archived (1)");
    wait_for_label(&mut h, "Unarchive");
    click(&mut h, "Unarchive");
    wait_for_label(&mut h, "Archive");
    assert!(h.state().settings.archived_sessions.is_empty());
    assert!(h.query_by_label("Unarchive").is_none());
    assert!(f.root.join("a.txt").exists(), "nothing was touched");
}

// ---- Credentials (TQ-3 item 8, SN-34)

fn open_credentials(h: &mut Harness<GitemApp>) {
    h.state_mut().repos[0].open_tab(Tab::Credentials(Box::default()));
    wait_for_label(h, "Stored secrets");
    quiet(h);
}

/// Real `ssh-keygen`, writing to the scratch folder. The agent is never touched in this run.
#[test]
fn a_key_generated_from_the_form_lands_in_the_scratch_folder() {
    if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
        eprintln!("ssh-keygen isn't installed: skipping");
        return;
    }
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_credentials(&mut h);
    click(&mut h, "SSH keys");
    wait_for_text(&mut h, "The SSH agent isn't used in this run");
    click(&mut h, "Generate key…");
    wait_for_label(&mut h, "Generate");
    let dir = gitem_core::cred::ssh_dir();
    assert!(dir.starts_with(gitem_core::settings::config_dir()));
    click(&mut h, "Generate");
    wait_for_text(&mut h, "Created ");
    no_errors(&h);
    assert!(dir.join("id_ed25519").exists() && dir.join("id_ed25519.pub").exists());
    click_last(&mut h, "Refresh");
    wait_for_label(&mut h, "id_ed25519");
    let _ = std::fs::remove_file(dir.join("id_ed25519"));
    let _ = std::fs::remove_file(dir.join("id_ed25519.pub"));
}

#[test]
fn a_git_credential_with_a_path_is_saved_for_its_host() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |_| {});
    open_credentials(&mut h);
    click(&mut h, "Add git credential…");
    wait_for_label(&mut h, "Save");
    quiet(&mut h);
    let input = egui::accesskit::Role::TextInput;
    let url = h.query_all_by_role(input).next().unwrap();
    url.focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.query_all_by_role(input).next().unwrap().type_text("https://sn34.invalid/org/repo.git");
    h.step();
    h.step();
    click(&mut h, "Save");
    wait_for_text(&mut h, "Credential saved for https://sn34.invalid");
    let saved = format!("{}https://sn34.invalid", gitem_core::cred::PREFIX_GIT);
    assert!(gitem_core::cred::get(&saved).unwrap().is_some(), "{:?}", gitem_core::cred::list(gitem_core::cred::PREFIX_GIT));
    gitem_core::cred::delete(&saved).unwrap();
}

// ---- Settings that say something about the network (SN-10, SN-17)

fn open_settings_page(h: &mut Harness<GitemApp>, page: &str) {
    h.state_mut().repos[0].open_tab(Tab::Settings(Box::default()));
    wait_for_label_any(h, page);
    quiet(h);
    click_last(h, page);
    quiet(h);
}

#[test]
fn settings_says_when_openais_default_is_going_until_a_model_is_named() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |s| s.ai.provider = gitem_core::ai::AiProvider::OpenAi);
    open_settings_page(&mut h, "AI");
    let note = gitem_core::ai::AiProvider::OpenAi.default_model_note(gitem_core::util::now_ts()).unwrap();
    wait_for_text(&mut h, &note);
    h.state_mut().settings.ai.model = "some-current-model".into();
    h.step();
    h.step();
    assert!(h.query_by_label_contains(&note).is_none());
}

#[test]
fn a_ca_file_that_cannot_be_used_says_so_on_the_page() {
    let f = Fx::new();
    let mut h = f.harness(ForgeKind::Gitea, |s| s.ca_file = "Z:/no/such/authorities.pem".into());
    open_settings_page(&mut h, "Hosting");
    wait_for_text(&mut h, "Using the system's store: Can't read Z:/no/such/authorities.pem");
    h.state_mut().settings.ca_file = String::new();
    let t = Instant::now();
    while gitem_core::forge::ca_file_problem().is_some() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "it stayed");
        std::thread::sleep(Duration::from_millis(10));
    }
}
