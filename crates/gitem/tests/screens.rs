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
use egui_kittest::kittest::{NodeT, Queryable};
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
    json!({"number":number,"title":title,"html_url":format!("http://127.0.0.1:9/pulls/{number}"),"user":{"login":if number.is_multiple_of(2) {"me"} else {"someone-else"}},"state":"open","created_at":"2026-10-01T10:00:00Z","updated_at":"2026-11-01T10:00:00Z","draft":draft,"mergeable":number != 12,
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
                if route == "/api/v1/repos/o/r/pulls/7.diff" {
                    let diff = "diff --git a/src/parse.rs b/src/parse.rs\n--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -10,4 +10,6 @@ fn width(c: char) -> usize {\n     match c {\n-        _ => 1,\n+        '\\u{1100}'..='\\u{115F}' => 2,\n+        '\\u{3000}'..='\\u{9FFF}' => 2,\n+        _ => 1,\n     }\n }\n";
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{diff}", diff.len());
                    return;
                }
                let body = match route {
                    "/api/v1/user" => json!({"login":"me"}),
                    "/api/v1/repos/o/r/pulls/7" => json!({"head":{"sha":"headsha"},"base":{"sha":"basesha"}}),
                    "/api/v1/repos/o/r/pulls/7/files" => json!([{"filename":"src/parse.rs","status":"changed","additions":3,"deletions":1},{"filename":"README.md","status":"changed","additions":1,"deletions":0}]),
                    "/api/v1/repos/o/r/commits/headsha/statuses" => json!([{"context":"build / windows","status":"success","description":"Passed in 4m","target_url":"http://127.0.0.1:9/ci/1"},{"context":"test / linux","status":"failure","description":"2 tests failed","target_url":"http://127.0.0.1:9/ci/2"},{"context":"lint","status":"pending","description":"Waiting","target_url":""}]),
                    "/api/v1/repos/o/r/issues/7/comments" => json!([{"user":{"login":"bea"},"body":"Could this handle combining marks as well? They come up in the Vietnamese test file.","created_at":"2026-11-02T10:00:00Z"}]),
                    "/api/v1/repos/o/r/pulls/7/reviews" => json!([{"id":1,"user":{"login":"cy"},"state":"REQUEST_CHANGES","body":"The width table needs a test.","comments_count":1,"submitted_at":"2026-11-03T10:00:00Z"}]),
                    "/api/v1/repos/o/r/pulls/7/reviews/1/comments" => json!([{"id":61,"user":{"login":"cy"},"body":"What about zero-width joiners?","path":"src/parse.rs","position":12,"created_at":"2026-11-03T10:00:01Z"}]),
                    "/api/v3/user" => json!({"login":"me"}),
                    "/api/v3/repos/o/r/pulls/7" => json!({"head":{"sha":"headsha"},"base":{"sha":"basesha"}}),
                    "/api/v3/repos/o/r/pulls/7/files" => json!([{"filename":"src/parse.rs","status":"modified","additions":3,"deletions":1,"patch":"@@ -10,4 +10,6 @@\n     match c {\n-        _ => 1,\n+        _ => 2,"}]),
                    "/api/v3/repos/o/r/issues/7/comments" => json!([{"id":1,"user":{"login":"bea"},"body":"Could this handle combining marks as well?","created_at":"2026-11-02T10:00:00Z"}]),
                    "/api/v3/repos/o/r/pulls/7/comments" => json!([
                        {"id":31,"user":{"login":"cy"},"body":"Why 2 here and not the table's width?","path":"src/parse.rs","line":11,"created_at":"2026-11-03T10:00:00Z"},
                        {"id":32,"in_reply_to_id":31,"user":{"login":"me"},"body":"The table only covers Hangul so far.","path":"src/parse.rs","line":11,"created_at":"2026-11-03T11:00:00Z"},
                        {"id":33,"in_reply_to_id":31,"user":{"login":"cy"},"body":"Fair. A test for it, then?","path":"src/parse.rs","line":11,"created_at":"2026-11-03T12:00:00Z"},
                        {"id":40,"user":{"login":"bea"},"body":"This could use a comment saying where the ranges come from.","path":"src/parse.rs","line":12,"created_at":"2026-11-04T09:00:00Z"},
                    ]),
                    "/api/v3/repos/o/r/commits/headsha/check-runs" => json!({"check_runs":[]}),
                    "/api/v3/repos/o/r/commits/headsha/status" => json!({"statuses":[]}),
                    "/api/v4/merge_requests" => json!([{"iid":5,"title":"Move the settings file","web_url":"http://127.0.0.1:9/team/app/-/merge_requests/5","author":{"username":"me"},"references":{"full":"team/app!5"},"source_branch":"settings","target_branch":"main","updated_at":"2026-11-01T10:00:00Z"}]),
                    "/api/v1/repos/issues/search" => json!([{"number":41,"title":"Add the new tokenizer","html_url":"http://127.0.0.1:9/o/other/41","user":{"login":"me"},"state":"open","repository":{"full_name":"o/other"}},{"number":3,"title":"Support long paths on Windows","html_url":"http://127.0.0.1:9/team/app/3","user":{"login":"bea"},"state":"open","repository":{"full_name":"team/app"},"pull_request":{"draft":true}}]),
                    "/api/v1/repos/o/r/pulls" if page_one => {
                        let mut fork = pr(15, "from-a-fork", "Someone else's fork", false);
                        fork["head"]["repo"]["full_name"] = json!("them/r");
                        json!([pr(7, "topic", "Teach the parser about wide characters", false), pr(9, "fix/long-branch-name-that-keeps-on-going", "Fix the thing that broke when a very long title like this one has to fit in a card", false), pr(12, "draft-work", "Sketch a new layout", true), fork])
                    },
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
    if wanted("norepo") {
        // Nothing open: the welcome page, its palette, and its two pages.
        let mut settings = Settings { details_on_open: false, theme, recent_repos: vec![f.root.to_string_lossy().replace('\\', "/")], ..Default::default() };
        settings.custom_hosts = vec![];
        let ctx = egui::Context::default();
        let mut app = GitemApp::with_settings(&ctx, settings);
        app.git = Ok(f.git.clone());
        let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1280.0, 800.0)).with_max_steps(5);
        let mut fonts = false;
        let mut h = builder.build_ui_state(
            move |ui, app: &mut GitemApp| {
                if !std::mem::replace(&mut fonts, true) {
                    gitem::theme::install_fonts(ui.ctx());
                }
                app.frame(ui);
            },
            app,
        );
        settle_a_moment(&mut h);
        out.save(&mut h, "norepo-welcome");
        h.state_mut().open_palette_with("");
        settle_a_moment(&mut h);
        out.save(&mut h, "norepo-palette");
        h.state_mut().open_palette_with("/x");
        settle_a_moment(&mut h);
        out.save(&mut h, "norepo-palette-files");
        h.state_mut().open_palette_with("");
        click(&mut h, "Pull Requests I'm In");
        settle_a_moment(&mut h);
        out.save(&mut h, "norepo-everything");
    }
    if wanted("10d") {
        // Hunks to pick, a drawing, its diff, branches picked, and the model's settings and form.
        let lines: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        std::fs::write(f.root.join("h.txt"), lines.join("\n") + "\n").unwrap();
        std::fs::write(f.root.join("drawing.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="90"><rect width="160" height="90" rx="8" fill="#2563eb"/><circle cx="80" cy="45" r="30" fill="#fbbf24"/><text x="10" y="20">A label</text></svg>"##).unwrap();
        run(&f.git, &f.root, &["add", "h.txt", "drawing.svg"]);
        run(&f.git, &f.root, &["commit", "-q", "-m", "drawing and lines"]);
        let mut changed = lines.clone();
        changed[3] = "early change".to_string();
        changed[30] = "late change".to_string();
        std::fs::write(f.root.join("h.txt"), changed.join("\n") + "\n").unwrap();
        std::fs::write(f.root.join("drawing.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="90"><rect width="160" height="90" rx="8" fill="#16a34a"/><circle cx="80" cy="45" r="30" fill="#fbbf24"/><text x="10" y="20">A label</text></svg>"##).unwrap();
        let mut h = f.harness(move |s: &mut Settings| {
            s.theme = theme;
            s.branch_model.enabled = true;
        }, vec![]);
        open(&mut h, Tab::StashHunks(Box::default()));
        wait_for(&mut h, "h.txt: lines");
        settle_a_moment(&mut h);
        let first = h.query_all_by_label_contains("h.txt: lines").next().unwrap().accesskit_node().label().unwrap().to_string();
        click(&mut h, &first);
        settle_a_moment(&mut h);
        out.save(&mut h, "stashhunks");
        open(&mut h, Tab::file("drawing.svg", None, None));
        wait_for(&mut h, "Picture");
        quiet(&mut h);
        settle_a_moment(&mut h);
        out.save(&mut h, "svgfile");
        open(&mut h, Tab::diff(DiffSource::Unstaged, Some("drawing.svg".to_string())));
        wait_for(&mut h, "Pictures");
        click(&mut h, "Pictures");
        quiet(&mut h);
        settle_a_moment(&mut h);
        out.save(&mut h, "svgdiff");
        open(&mut h, Tab::home());
        settle_a_moment(&mut h);
        out.save(&mut h, "modelhome");
        h.query_all_by_label("one").next().unwrap().click_modifiers(egui::Modifiers::COMMAND);
        h.step();
        h.query_all_by_label("two").next().unwrap().click_modifiers(egui::Modifiers::COMMAND);
        h.step();
        settle_a_moment(&mut h);
        out.save(&mut h, "pickedbranches");
        h.state_mut().open_palette_with("Finish");
        settle_a_moment(&mut h);
        out.save(&mut h, "modelpalette");
        let ctx = h.ctx.clone();
        let model = gitem_core::branchmodel::BranchModel { enabled: true, ..Default::default() };
        h.state_mut().open_palette_with("");
        h.key_press(egui::Key::Escape);
        h.step();
        let form = gitem::forms::start_branch(&model, gitem_core::branchmodel::Kind::Feature, &h.state().repos[0].data);
        h.state_mut().apply_action(&ctx, gitem::action::Action::Form(form));
        settle_a_moment(&mut h);
        out.save(&mut h, "modelstart");
        run(&f.git, &f.root, &["checkout", "--", "h.txt", "drawing.svg"]);
    }
    if wanted("rail") {
        let mut h = f.harness(move |s: &mut Settings| {
            s.theme = theme;
            s.rail_labels = true;
        }, vec![]);
        settle_a_moment(&mut h);
        out.save(&mut h, "rail-names");
        let mut h = f.harness(move |s: &mut Settings| {
            s.theme = theme;
            s.rail_labels = true;
            s.rail_full = true;
        }, vec![]);
        settle_a_moment(&mut h);
        out.save(&mut h, "rail-names-all");
    }
    if wanted("form") {
        let mut h = f.harness(set, vec![]);
        h.state_mut().repos[0].open_tab(Tab::graph());
        quiet(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, gitem::action::Action::Form(gitem::forms::reset_form("HEAD~1".into())));
        settle_a_moment(&mut h);
        out.save(&mut h, "form");
    }
    if wanted("switch") {
        let mut h = f.harness(set, vec![]);
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, gitem::action::Action::Form(gitem::forms::switch_with_changes("topic".into(), vec!["a.txt".into(), "src/lib.rs".into()])));
        settle_a_moment(&mut h);
        out.save(&mut h, "switch");
    }
    if wanted("scan") {
        let mut h = f.harness(set, vec![]);
        let top = f.top.join("scan");
        for n in ["one", "deep/er/two"] {
            if !top.join(n).exists() {
                make_repo(&f.git, &top.join(n));
            }
        }
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, gitem::action::Action::ScanRepos(top));
        wait_for(&mut h, "deep/er/two");
        settle_a_moment(&mut h);
        out.save(&mut h, "scan");
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
    if wanted("refs") {
        for i in 0..30 {
            let name = format!("ref{i:02}");
            if f.git.run(Some(&f.root), &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")]).is_err() {
                run(&f.git, &f.root, &["branch", &name]);
            }
        }
        let mut h = f.harness(set, vec![]);
        h.set_size(vec2(1000.0, 500.0));
        open(&mut h, Tab::graph());
        h.query_all_by_label("Change a.txt, step 5").next().unwrap().click_secondary();
        settle_a_moment(&mut h);
        out.save(&mut h, "refs-menu");
        h.query_by_label_contains("refs here").unwrap().click();
        settle_a_moment(&mut h);
        out.save(&mut h, "refs-list");
    }
    if wanted("stale") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::graph());
        let head = f.root.join(".git/HEAD");
        let was = std::fs::read(&head).unwrap();
        std::fs::write(&head, "this is not a ref
").unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().apply_action(&ctx, gitem::action::Action::Refresh);
        quiet(&mut h);
        settle_a_moment(&mut h);
        out.save(&mut h, "stale");
        std::fs::write(&head, was).unwrap();
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
        click(&mut h, "Other open (3)");
        wait_for(&mut h, "Teach the parser");
        quiet(&mut h);
        out.save(&mut h, "launchpad");
    }
    if wanted("pr") || wanted("prfiles") || wanted("prchecks") || wanted("prtalk") || wanted("prthread") || wanted("everywhere") || wanted("hosts") || wanted("fork") || wanted("hosting") {
        let base = fake_host();
        let root = f.root.clone();
        let _ = f.git.run(Some(&root), &["remote", "remove", "origin"]);
        run(&f.git, &root, &["remote", "add", "origin", &format!("{base}/o/r.git")]);
        let custom = CustomHost { host: "127.0.0.1".into(), kind: ForgeKind::Gitea, api_base: String::new(), templates: Default::default() };
        let repo = identify(&format!("{base}/o/r.git"), std::slice::from_ref(&custom)).unwrap();
        gitem_core::cred::set_forge_token(&repo.token_host(), "", "FAKE_SECRET_VALUE").unwrap();
        let settings_with = |c: CustomHost| {
            move |s: &mut Settings| {
                s.theme = theme;
                s.custom_hosts = vec![c];
                s.recent_repos = vec!["C:/work/other".to_string()];
                s.ca_file = "C:/not/a/real/ca.pem".to_string();
            }
        };
        let pr = gitem_core::forge::PullRequest { number: 7, title: "Teach the parser about wide characters".into(), author: "someone-else".into(), head: "topic".into(), base: "main".into(), url: "http://127.0.0.1:9/pulls/7".into(), body: "East Asian text was measured a column short, so rows clipped.\n\nThis gives the parser a width table, and tests it against what egui lays out.".into(), ..Default::default() };
        let mut h = f.harness(settings_with(custom.clone()), vec![]);
        h.state_mut().repos[0].open_tab(Tab::PullRequest(Box::new(gitem::views::pullrequest::PullRequestView::new(pr.clone(), "me"))));
        wait_for(&mut h, "Files (2)");
        quiet(&mut h);
        if wanted("pr") {
            out.save(&mut h, "pr");
        }
        if wanted("prfiles") {
            click(&mut h, "Files (2)");
            quiet(&mut h);
            click(&mut h, "   11");
            quiet(&mut h);
            out.save(&mut h, "prfiles");
        }
        if wanted("prchecks") {
            click(&mut h, "Checks (3)");
            quiet(&mut h);
            out.save(&mut h, "prchecks");
        }
        if wanted("prtalk") {
            click(&mut h, "Conversation (3)");
            quiet(&mut h);
            out.save(&mut h, "prtalk");
        }
        if wanted("prthread") {
            let github = CustomHost { host: "127.0.0.1".into(), kind: ForgeKind::GitHub, api_base: String::new(), templates: Default::default() };
            let mut h = f.harness(settings_with(github), vec![]);
            h.state_mut().repos[0].open_tab(Tab::PullRequest(Box::new(gitem::views::pullrequest::PullRequestView::new(pr.clone(), "me"))));
            wait_for(&mut h, "Conversation (5)");
            click(&mut h, "Conversation (5)");
            wait_for(&mut h, "Reply to cy's thread");
            quiet(&mut h);
            out.save(&mut h, "prthread");
            click(&mut h, "Reply to bea");
            wait_for(&mut h, "Post reply");
            settle_a_moment(&mut h);
            out.save(&mut h, "prreply");
        }
        if wanted("everywhere") {
            open(&mut h, Tab::Launchpad(Box::default()));
            click(&mut h, "Everything I'm in");
            wait_for(&mut h, "o/other");
            quiet(&mut h);
            out.save(&mut h, "everywhere");
        }
        if wanted("hosts") {
            // A second host that answers, one that can't be reached, and one with nothing saved.
            let lab = fake_host();
            let (dead, unsaved) = ("http://127.0.0.1:9", fake_host());
            let customs: Vec<CustomHost> = [(lab.as_str(), ForgeKind::GitLab), (dead, ForgeKind::Gitea), (unsaved.as_str(), ForgeKind::Gitea)].iter().map(|(b, kind)| CustomHost { host: b.trim_start_matches("http://").to_string(), kind: *kind, api_base: String::new(), templates: Default::default() }).collect();
            let mut recent = Vec::new();
            for (i, (b, c)) in [lab.as_str(), dead, unsaved.as_str()].iter().zip(&customs).enumerate() {
                let dir = f.top.join(format!("recent{i}"));
                make_repo(&f.git, &dir);
                run(&f.git, &dir, &["remote", "add", "origin", &format!("{b}/team/app.git")]);
                recent.push(gitem_core::Repo::open(f.git.clone(), &dir).unwrap().root.to_string_lossy().replace('\\', "/"));
                if i < 2 {
                    let id = identify(&format!("{b}/team/app.git"), std::slice::from_ref(c)).unwrap();
                    gitem_core::cred::set_forge_token(&id.token_host(), "", "FAKE_SECRET_VALUE").unwrap();
                }
            }
            let mut all = customs.clone();
            all.push(custom.clone());
            let mut h = f.harness(
                move |s: &mut Settings| {
                    s.theme = theme;
                    s.custom_hosts = all;
                    s.recent_repos = recent;
                },
                vec![],
            );
            open(&mut h, Tab::Launchpad(Box::default()));
            click(&mut h, "Everything I'm in");
            wait_for(&mut h, "o/other");
            wait_for(&mut h, "team/app");
            quiet(&mut h);
            out.save(&mut h, "hosts");
        }
        if wanted("fork") {
            let fork = gitem_core::forge::PullRequest { from_fork: true, head_repo: "them/r".into(), head: "from-a-fork".into(), ..pr.clone() };
            let (remotes, ctx) = (h.state().repos[0].data.remotes().to_vec(), h.ctx.clone());
            let form = gitem::views::launchpad::push_to_fork_form(&fork, "pr/7", Some(&repo), &remotes);
            h.state_mut().apply_action(&ctx, gitem::action::Action::Form(form));
            settle_a_moment(&mut h);
            out.save(&mut h, "fork");
            h.state_mut().apply_action(&ctx, gitem::action::Action::Refresh);
        }
        if wanted("hosting") {
            let mut h = f.harness(settings_with(custom), vec![]);
            open(&mut h, Tab::Settings(Box::new(gitem::views::settings::SettingsView::on_page("hosting"))));
            settle_a_moment(&mut h);
            out.save(&mut h, "hosting");
        }
    }
    if wanted("prune") {
        for b in ["finished-one", "finished-two"] {
            let _ = f.git.run(Some(&f.root), &["branch", b]);
        }
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::Prune(Box::default()));
        settle_a_moment(&mut h);
        out.save(&mut h, "prune");
    }
    if wanted("undo") {
        let _ = f.git.run(Some(&f.root), &["branch", "going"]);
        let mut h = f.harness(set, vec![]);
        h.state_mut().repos[0].queue_for_test(gitem_core::ops::branch_delete("going", true));
        wait_for(&mut h, "Undo delete of branch going");
        quiet(&mut h);
        out.save(&mut h, "undo");
        h.get_by_label("Undo delete of branch going").hover();
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(900) {
            h.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        out.save(&mut h, "tooltip");
    }
    if wanted("grep") {
        let mut h = f.harness(set, vec![]);
        h.state_mut().repos[0].open_tab(Tab::Grep(Box::new(gitem::views::grep::GrepView::looking_for("println", ""))));
        quiet(&mut h);
        h.query_all_by_label("Search").last().unwrap().click();
        wait_for(&mut h, "src/lib.rs:2");
        quiet(&mut h);
        out.save(&mut h, "grep");
    }
    if wanted("keysbelow") {
        let mut h = f.harness(move |s| {
            s.theme = theme;
            s.key_bindings.insert("fetch".to_string(), "Ctrl+Shift+L".to_string());
        }, vec![]);
        open(&mut h, Tab::Settings(Box::new(gitem::views::settings::SettingsView::on_page("workflow"))));
        settle_a_moment(&mut h);
        h.set_size(vec2(1280.0, 2400.0));
        settle_a_moment(&mut h);
        out.save(&mut h, "keysbelow");
    }
    if wanted("helpkeys") {
        let mut h = f.harness(move |s| {
            s.theme = theme;
            s.key_bindings.insert("fetch".to_string(), "Ctrl+Shift+L".to_string());
        }, vec![]);
        h.key_press(egui::Key::F1);
        settle_a_moment(&mut h);
        out.save(&mut h, "helpkeys");
    }
    if wanted("across") {
        let other = f.top.join("other-repo");
        make_repo(&f.git, &other);
        std::fs::write(other.join("new.txt"), "n\n").unwrap();
        let mut h = f.harness(set, vec![other]);
        wait_for(&mut h, " open");
        quiet(&mut h);
        out.save(&mut h, "across");
        let summary = h.query_all_by_label_contains(" open").next().unwrap().accesskit_node().label().unwrap().to_string();
        click(&mut h, &summary);
        settle_a_moment(&mut h);
        out.save(&mut h, "acrossopen");
    }
    if wanted("restored") {
        let mut h = f.harness(set, vec![]);
        open(&mut h, Tab::from_spec("search:change:parser after:2024-01-01").unwrap());
        settle_a_moment(&mut h);
        out.save(&mut h, "restored");
    }
    if wanted("keys") {
        let mut h = f.harness(move |s| {
            s.theme = theme;
            s.key_bindings.insert("fetch".to_string(), "Ctrl+Shift+L".to_string());
        }, vec![]);
        open(&mut h, Tab::Settings(Box::new(gitem::views::settings::SettingsView::on_page("workflow"))));
        settle_a_moment(&mut h);
        out.save(&mut h, "keys");
    }
    if wanted("drop") {
        let mut h = f.harness(set, vec![]);
        h.input_mut().hovered_files.push(egui::HoveredFile { path: Some(f.root.clone()), mime: String::new() });
        settle_a_moment(&mut h);
        out.save(&mut h, "drop");
    }
    if wanted("credentials") {
        gitem_core::cred::set_forge_token("git.example.invalid", "someone", "FAKE_SECRET_VALUE").unwrap();
        gitem_core::cred::set_ai_key("anthropic", "FAKE_SECRET_VALUE").unwrap();
        gitem_core::cred::store(&format!("{}https://code.example.invalid", gitem_core::cred::PREFIX_GIT), "another.person", "FAKE_SECRET_VALUE").unwrap();
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
