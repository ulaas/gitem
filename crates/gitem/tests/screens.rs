//! Pictures of what `--shot` can't reach: dialogs, open menus, keyboard focus, and views filled
//! with fake data from a mock host on localhost. Opt-in, as it only writes files for someone to
//! look at and asserts nothing about them:
//!
//! ```text
//! SCREENS_OUT=some/scratch/folder cargo test -p gitem --test screens -- --ignored --test-threads=1
//! SCREENS_ONLY=launchpad,more ... to draw just those
//! ```
//!
//! Each picture goes through kittest's wgpu renderer, in this binary's scratch settings and
//! secret store, so what's drawn is egui's output for that frame. Clicks are kittest's synthetic
//! input, and hover is only what the last click left behind.
gitem_testenv::isolate!();

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::forge::{ForgeKind, identify};
use gitem_core::repo::DiffSource;
use gitem_core::settings::{CustomHost, Settings, ThemeChoice};
use serde_json::{Value, json};

const PATIENCE: Duration = Duration::from_secs(40);

fn pr(number: u64, head: &str, title: &str, draft: bool) -> Value {
    json!({"number":number,"title":title,"html_url":format!("http://127.0.0.1:9/pulls/{number}"),"user":{"login":if number % 2 == 0 {"me"} else {"someone-else"}},"state":"open","created_at":"2026-10-01T10:00:00Z","updated_at":"2026-11-01T10:00:00Z","draft":draft,"mergeable":number != 12,
        "head":{"ref":head,"repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}}})
}

/// A host on localhost with made-up pull requests. It only answers what Launchpad reads.
fn fake_host() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    return;
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut b = vec![0u8; len];
                let _ = r.read_exact(&mut b);
                let page_one = !path.contains("page=2");
                let route = path.split('?').next().unwrap_or("");
                let body = match route {
                    "/api/v1/user" => json!({"login":"me"}),
                    "/api/v1/repos/o/r/pulls" if page_one => json!([pr(7, "topic", "Teach the parser about wide characters", false), pr(9, "fix/long-branch-name-that-keeps-on-going", "Fix the thing that broke when a very long title like this one has to fit in a card", false), pr(12, "draft-work", "Sketch a new layout", true)]),
                    "/api/v1/repos/o/r/issues" if page_one => json!([{"number":3,"title":"Crash when opening a file","state":"open","html_url":"http://127.0.0.1:9/issues/3","user":{"login":"someone-else"},"labels":[{"name":"bug","color":"ff0000"}]}]),
                    _ => json!([]),
                };
                let body = body.to_string();
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    base
}

struct Fx {
    _dir: tempfile::TempDir,
    top: PathBuf,
    root: PathBuf,
    git: GitExe,
}

fn run(git: &GitExe, dir: &Path, args: &[&str]) -> String {
    git.run(Some(dir), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
}

fn make_repo(git: &GitExe, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    run(git, root, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.invalid"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
        run(git, root, &["config", k, v]);
    }
    std::fs::create_dir_all(root.join("src/deep/er")).unwrap();
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn main() {\n    println!(\"hi\");\n}\n".repeat(30)).unwrap();
    std::fs::write(root.join("src/deep/er/data.json"), "{\"k\": 1}\n".repeat(400)).unwrap();
    run(git, root, &["add", "-A"]);
    run(git, root, &["commit", "-q", "-m", "First commit\n\nWith a body that says why."]);
    for i in 0..6 {
        std::fs::write(root.join("a.txt"), format!("one\ntwo {i}\nthree\n")).unwrap();
        run(git, root, &["add", "-A"]);
        run(git, root, &["commit", "-q", "-m", &format!("Change a.txt, step {i}")]);
    }
    run(git, root, &["tag", "v1.0"]);
    run(git, root, &["branch", "topic"]);
    run(git, root, &["branch", "team/alpha/one"]);
    run(git, root, &["branch", "team/alpha/two"]);
    run(git, root, &["branch", "team/beta/three"]);
    std::fs::write(root.join("a.txt"), "one\ntwo changed\nthree\nand four, with a line long enough that it has to wrap around in any pane this window can give it, and then it goes on a little more so there is no doubt at all\n中文宽字符测试的一行文字,它足够长以至于在任何窗格中都必须换行显示,这样才能看到每个字符是否都被完整地画出来而没有被下一行遮住或者裁掉。日本語の全角文字も同じように折り返されるはずです。\n").unwrap();
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        let git = GitExe::discover(None).unwrap();
        let root = top.join("repo");
        make_repo(&git, &root);
        Self { _dir: dir, top, root, git }
    }

    fn harness(&self, tweak: impl FnOnce(&mut Settings), extra: Vec<PathBuf>) -> Harness<'static, GitemApp> {
        let mut settings = Settings { details_on_open: false, ..Default::default() };
        tweak(&mut settings);
        let ctx = egui::Context::default();
        let mut app = GitemApp::with_settings(&ctx, settings);
        app.git = Ok(self.git.clone());
        let root = self.root.clone();
        let mut opened = false;
        let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(5);
        let mut h = builder.build_ui_state(
            move |ui, app: &mut GitemApp| {
                if !opened {
                    opened = true;
                    let c = ui.ctx().clone();
                    // `with_settings` gave them to the other context, which isn't drawn.
                    gitem::theme::install_fonts(&c);
                    for p in &extra {
                        app.open_repo(&c, p, false);
                    }
                    app.open_repo(&c, &root, false);
                }
                app.frame(ui);
            },
            app,
        );
        let t = Instant::now();
        while !h.state().repos.first().is_some_and(|r| r.data.status.get().is_some() && r.data.refs.get().is_some()) {
            h.step();
            assert!(t.elapsed() < PATIENCE, "the repository never loaded");
            std::thread::sleep(Duration::from_millis(10));
        }
        quiet(&mut h);
        h
    }
}

fn quiet(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut frames = 0;
    while frames < 6 {
        h.step();
        frames = if gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() { frames + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for(h: &mut Harness<GitemApp>, label: &str) {
    let t = Instant::now();
    while h.query_all_by_label_contains(label).next().is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "nothing labelled {label:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A dialog takes no clicks for a moment after it appears.
fn settle_a_moment(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(260) {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn click(h: &mut Harness<GitemApp>, label: &str) {
    wait_for(h, label);
    settle_a_moment(h);
    h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no exact label {label:?}")).click();
    h.step();
    h.step();
    h.step();
}

fn open(h: &mut Harness<GitemApp>, tab: Tab) {
    h.state_mut().repos[0].open_tab(tab);
    quiet(h);
    settle_a_moment(h);
}

struct Out {
    dir: PathBuf,
    theme: &'static str,
}

impl Out {
    fn save(&self, h: &mut Harness<GitemApp>, name: &str) {
        h.step();
        let img = h.render().expect("wgpu render");
        let path = self.dir.join(format!("{name}-{}.png", self.theme));
        img.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

fn wanted(name: &str) -> bool {
    std::env::var("SCREENS_ONLY").map_or(true, |only| only.split(',').any(|o| o == name))
}

fn scene_set(out: &Out, theme: ThemeChoice) {
    let f = Fx::new();
    let set = move |s: &mut Settings| {
        s.theme = theme;
    };
    if wanted("form") {
        let mut h = f.harness(set, vec![]);
        h.state_mut().repos[0].open_tab(Tab::graph());
        quiet(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, gitem::action::Action::Form(gitem::forms::reset_form("HEAD~1".into())));
        settle_a_moment(&mut h);
        out.save(&mut h, "form");
    }
    if wanted("confirm") {
        let mut h = f.harness(set, vec![]);
        // The close dialog's sibling: a danger form opened by name.
        h.state_mut().repos[0].open_tab(Tab::Worktrees(Box::default()));
        quiet(&mut h);
        let c = gitem::action::Action::Confirm { title: "Discard all changes".into(), body: "This deletes the changes in 1 file. They can't be brought back.".into(), label: "Discard".into(), op: gitem_core::ops::discard_paths(&["a.txt".to_string()]) };
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, c);
        settle_a_moment(&mut h);
        out.save(&mut h, "confirm");
    }
    if wanted("more") {
        let mut h = f.harness(set, vec![]);
        click(&mut h, "More");
        settle_a_moment(&mut h);
        out.save(&mut h, "more");
    }
    if wanted("filters") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::graph());
        let label = format!("Filters {}", gitem::chrome::icon::CARET_DOWN);
        click(&mut h, &label);
        settle_a_moment(&mut h);
        out.save(&mut h, "filters");
    }
    if wanted("help") {
        let mut h = f.harness(set, vec![]);
        h.key_press(egui::Key::F1);
        settle_a_moment(&mut h);
        out.save(&mut h, "help");
    }
    if wanted("focus") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::graph());
        for presses in [4, 12] {
            for _ in 0..presses {
                h.key_press(egui::Key::Tab);
            }
            out.save(&mut h, &format!("focus{presses}"));
        }
    }
    if wanted("wrap") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::diff(DiffSource::Unstaged, Some("a.txt".into())));
        click(&mut h, "Wrap");
        quiet(&mut h);
        out.save(&mut h, "wrap-split");
        click(&mut h, "Unified");
        quiet(&mut h);
        out.save(&mut h, "wrap-unified");
    }
    if wanted("overflow") {
        let extras: Vec<PathBuf> = (0..12)
            .map(|i| {
                let p = f.top.join(format!("a-repository-with-a-long-name-{i:02}"));
                if !p.exists() {
                    make_repo(&f.git, &p);
                }
                p
            })
            .collect();
        let mut h = f.harness(set, extras);
        quiet(&mut h);
        out.save(&mut h, "tabs");
        let hidden: Vec<String> = h.query_all_by_label_contains(gitem::chrome::icon::DOTS_THREE).map(|n| format!("{n:?}")).collect();
        eprintln!("overflow candidates: {}", hidden.len());
        h.query_all_by_label_contains(gitem::chrome::icon::DOTS_THREE).next().expect("the overflow button").click();
        h.step();
        h.step();
        settle_a_moment(&mut h);
        out.save(&mut h, "overflow");
    }
    if wanted("treemap") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::Treemap(Box::default()));
        out.save(&mut h, "treemap");
    }
    if wanted("visual") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::visual("a.txt"));
        out.save(&mut h, "visual");
    }
    if wanted("launchpad") {
        let base = fake_host();
        let root = f.root.clone();
        run(&f.git, &root, &["remote", "add", "origin", &format!("{base}/o/r.git")]);
        let custom = CustomHost { host: "127.0.0.1".into(), kind: ForgeKind::Gitea, api_base: String::new(), templates: Default::default() };
        let repo = identify(&format!("{base}/o/r.git"), std::slice::from_ref(&custom)).unwrap();
        gitem_core::cred::set_forge_token(&repo.token_host(), "", "FAKE_SECRET_VALUE").unwrap();
        let c2 = custom.clone();
        let mut h = f.harness(
            move |s| {
                s.theme = theme;
                s.custom_hosts = vec![c2];
            },
            vec![],
        );
        open(&mut h, Tab::Launchpad(Box::default()));
        quiet(&mut h);
        click(&mut h, "Other open (2)");
        wait_for(&mut h, "Teach the parser");
        quiet(&mut h);
        out.save(&mut h, "launchpad");
    }
    if wanted("credentials") {
        gitem_core::cred::set_forge_token("git.example.invalid", "someone", "FAKE_SECRET_VALUE").unwrap();
        gitem_core::cred::set_ai_key("anthropic", "FAKE_SECRET_VALUE").unwrap();
        gitem_core::cred::store("gitem/git/https://code.example.invalid", "another.person", "FAKE_SECRET_VALUE").unwrap();
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::Credentials(Box::default()));
        wait_for(&mut h, "Stored secrets");
        quiet(&mut h);
        out.save(&mut h, "credentials");
    }
}

/// Draws each scene in Dark and in Light.
#[test]
#[ignore = "writes pictures for someone to look at: set SCREENS_OUT"]
fn draw_screens() {
    let dir = PathBuf::from(std::env::var("SCREENS_OUT").expect("SCREENS_OUT"));
    std::fs::create_dir_all(&dir).unwrap();
    scene_set(&Out { dir: dir.clone(), theme: "dark" }, ThemeChoice::Dark);
    scene_set(&Out { dir, theme: "light" }, ThemeChoice::Light);
}
