//! What the app does while git is slow: a real git behind a proxy that takes longer for the
//! commands named, and logs when each starts and ends. These run one at a time, since the
//! proxy is told what to do through this process's environment.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::GitExe;
use gitem_core::repo::DiffSource;
use gitem_core::settings::Settings;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

const PATIENCE: Duration = Duration::from_secs(180);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    log: PathBuf,
    real: GitExe,
    _turn: MutexGuard<'static, ()>,
}

fn run(g: &GitExe, root: &Path, args: &[&str]) -> String {
    g.run(Some(root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
}

fn proxy_path() -> PathBuf {
    let me = std::env::current_exe().unwrap();
    me.parent().unwrap().parent().unwrap().join("examples").join(if cfg!(windows) { "slowgit.exe" } else { "slowgit" })
}

impl Fx {
    /// A repository with a few commits, and the proxy set to delay `names` by `ms`.
    fn new(names: &str, ms: u64) -> Self {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo").canonicalize().or_else(|_| {
            std::fs::create_dir(dir.path().join("repo"))?;
            dir.path().join("repo").canonicalize()
        }).unwrap();
        let real = GitExe::discover(None).unwrap();
        run(&real, &root, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            run(&real, &root, &["config", k, v]);
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        run(&real, &root, &["add", "-A"]);
        run(&real, &root, &["commit", "-q", "-m", "first"]);
        for i in 0..20 {
            std::fs::write(root.join("a.txt"), format!("{i}\n")).unwrap();
            run(&real, &root, &["commit", "-qam", &format!("commit {i}")]);
        }
        let log = dir.path().join("slow.log");
        assert!(proxy_path().exists(), "cargo test builds examples, but {} isn't there", proxy_path().display());
        // Safe here: one test at a time, and only the proxy reads these.
        unsafe {
            std::env::set_var("SLOWGIT_REAL", &real.path);
            std::env::set_var("SLOWGIT_MATCH", names);
            std::env::set_var("SLOWGIT_MS", ms.to_string());
            std::env::set_var("SLOWGIT_LOG", &log);
        }
        Self { _dir: dir, root, log, real, _turn: turn }
    }

    fn slow(&self) -> GitExe {
        let mut g = self.real.clone();
        g.path = proxy_path();
        g
    }

    /// How many started, and the most running at once.
    fn report(&self) -> (usize, i32) {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let mut ev: Vec<(u128, i32)> = text
            .lines()
            .filter_map(|l| {
                let mut p = l.split(' ');
                let kind = p.next()?;
                Some((p.next()?.parse().ok()?, if kind == "S" { 1 } else { -1 }))
            })
            .collect();
        ev.sort();
        let (mut now, mut peak) = (0, 0);
        for (_, d) in &ev {
            now += d;
            peak = peak.max(now);
        }
        (ev.iter().filter(|e| e.1 == 1).count(), peak)
    }

    fn harness(&self, mut settings: Settings) -> Harness<'static, GitemApp> {
        // What these count is what each view starts, not what opening the graph does.
        settings.details_on_open = false;
        let ctx = egui::Context::default();
        let mut app = GitemApp::with_settings(&ctx, settings);
        app.git = Ok(self.slow());
        let root = self.root.clone();
        let mut opened = false;
        let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5);
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
}

fn pump(h: &mut Harness<GitemApp>, ms: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn quiet(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut n = 0;
    while n < 5 {
        h.step();
        n = if gitem::task::running(&h.ctx) == 0 && h.state().ops_idle() { n + 1 } else { 0 };
        assert!(t.elapsed() < PATIENCE, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A burst of changes during a slow load: one load at a time, and one more when it lands.
fn no_pile_up(f: &Fx, mut h: Harness<GitemApp>, what: &str) {
    pump(&mut h, 300);
    quiet(&mut h);
    let _ = std::fs::remove_file(&f.log);
    for i in 0..10 {
        h.state_mut().repos[0].data.mark_rewritten();
        std::fs::write(f.root.join("b.txt"), format!("{i}\n")).unwrap();
        pump(&mut h, 100);
    }
    quiet(&mut h);
    let (started, peak) = f.report();
    assert_eq!(peak, 1, "{what}: more than one at once, {started} started");
    assert!((2..=5).contains(&started), "{what}: {started} started for 10 changes in about a second, with each taking 400 ms");
}

#[test]
fn a_slow_file_list_under_steady_changes_runs_one_at_a_time() {
    let f = Fx::new("ls-files", 400);
    let h = f.harness(Settings { sidebar_files: true, ..Default::default() });
    no_pile_up(&f, h, "file list");
}

#[test]
fn a_slow_blame_under_steady_changes_runs_one_at_a_time() {
    let f = Fx::new("blame", 400);
    let mut h = f.harness(Settings::default());
    pump(&mut h, 100);
    h.state_mut().repos[0].open_tab(Tab::file("a.txt", None, None));
    no_pile_up(&f, h, "blame");
}

#[test]
fn a_slow_diff_under_steady_changes_runs_one_at_a_time() {
    let f = Fx::new("diff", 400);
    std::fs::write(f.root.join("a.txt"), "changed\n").unwrap();
    let mut h = f.harness(Settings::default());
    pump(&mut h, 100);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("a.txt".into())));
    no_pile_up(&f, h, "diff");
}

/// The load running when the last change happens may have missed it, so another must follow.
#[test]
fn the_change_during_a_slow_diff_is_shown_when_it_finishes() {
    let f = Fx::new("diff", 500);
    std::fs::write(f.root.join("a.txt"), "first edit\n").unwrap();
    let mut h = f.harness(Settings::default());
    pump(&mut h, 100);
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("a.txt".into())));
    pump(&mut h, 100);
    std::fs::write(f.root.join("a.txt"), "second edit\n").unwrap();
    h.state_mut().repos[0].data.mark_rewritten();
    quiet(&mut h);
    let diff = h.state().repos[0].dock.iter_all_tabs().find_map(|(_, t)| if let Tab::Diff(d) = t { d.loaded_changes() } else { None }).expect("the diff is open");
    assert!(diff.unwrap().contains("second edit"));
}

#[test]
fn holding_an_arrow_key_loads_only_the_commit_it_stops_on() {
    let f = Fx::new("show", 100);
    let shas = f.real.run(Some(&f.root), &["rev-list", "HEAD"]).unwrap().text();
    let shas: Vec<&str> = shas.lines().collect();
    let mut h = f.harness(Settings::default());
    pump(&mut h, 300);
    quiet(&mut h);
    let _ = std::fs::remove_file(&f.log);
    for sha in shas.iter().take(15) {
        h.state_mut().repos[0].inspect.set(sha);
        h.step();
        // Whatever is shown belongs to this commit: never the one before.
        assert!(h.state().repos[0].inspect.showing().is_none_or(|s| s == *sha), "another commit's details are showing");
        pump(&mut h, 30);
        assert!(h.state().repos[0].inspect.showing().is_none_or(|s| s == *sha), "another commit's details are showing");
    }
    quiet(&mut h);
    pump(&mut h, 300);
    quiet(&mut h);
    let (started, peak) = f.report();
    assert!(started <= 2, "{started} show commands for 15 selections");
    assert!(peak <= 1, "{peak} at once");
    let t = Instant::now();
    while h.state().repos[0].inspect.showing() != Some(shas[14]) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "the one it stopped on never showed");
        std::thread::sleep(Duration::from_millis(10));
    }
    quiet(&mut h);
}

#[test]
fn a_single_click_after_a_pause_loads_at_once() {
    let f = Fx::new("", 0);
    let shas = f.real.run(Some(&f.root), &["rev-list", "HEAD"]).unwrap().text();
    let shas: Vec<&str> = shas.lines().collect();
    let mut h = f.harness(Settings::default());
    pump(&mut h, 300);
    quiet(&mut h);
    h.state_mut().repos[0].inspect.set(shas[3]);
    h.step();
    assert!(gitem::task::running(&h.ctx) > 0, "it started on the first frame");
    quiet(&mut h);
}

/// A session repository on a share that isn't there took 21 seconds per repository.
#[test]
fn a_session_repository_that_is_slow_to_answer_does_not_hold_up_the_window() {
    let f = Fx::new("rev-parse", 4000);
    let path = Settings::path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let s = Settings { git_path: proxy_path().to_string_lossy().into_owned(), open_repos: vec![f.root.to_string_lossy().replace('\\', "/")], ..Default::default() };
    s.save().unwrap();
    let ctx = egui::Context::default();
    let start = Instant::now();
    let app = GitemApp::new(&ctx, &[]);
    let took = start.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(took < Duration::from_millis(3500), "{took:?} before the window could start");
    assert!(app.repos.is_empty(), "it hasn't answered yet");
    let mut h: Harness<GitemApp> = Harness::builder().with_size(vec2(1500.0, 940.0)).with_max_steps(5).build_ui_state(|ui, app: &mut GitemApp| app.frame(ui), app);
    h.step();
    assert!(h.query_by_label_contains("Opening repo").is_some(), "it says what it's waiting for");
    let t = Instant::now();
    while h.state().repos.is_empty() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "it never arrived");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.state().errors_showing().is_empty());
}


/// AS-12: a read that hangs left the last answer on screen, with nothing to say so.
#[test]
fn a_status_that_never_answers_says_so_and_can_be_restarted() {
    let f = Fx::new("status", 40_000);
    let mut h = f.harness(Settings::default());
    pump(&mut h, 200);
    let t = Instant::now();
    while h.query_by_label("Stop and try again").is_none() {
        h.step();
        assert!(t.elapsed() < PATIENCE, "it never said anything");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(t.elapsed() >= Duration::from_secs(15), "not before it's been a while");
    assert!(h.query_by_label_contains("Still waiting: git status").is_some());
    h.get_by_label("Stop and try again").click();
    pump(&mut h, 2000);
    assert_eq!(f.report().0, 2, "the first was stopped, and another started");
    assert!(h.state().errors_showing().is_empty());
}
