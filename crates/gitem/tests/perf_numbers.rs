//! Numbers, not checks: real `GitemApp` frames and real git on large throwaway repositories.
//! Every test is ignored because they take minutes and mean nothing outside a release build.
//!
//!   cargo build --release -p gitem --example slowgit
//!   cargo test --release -p gitem --test perf_numbers -- --ignored --nocapture --test-threads=1
//!
//! The bounds that fail a normal test run are in `ui_flows.rs`, and in `gitem-core/tests/perf.rs`.
//! The harness doesn't paint, so GPU and tessellation cost aren't in these frame times.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem::views::Tab;
use gitem_core::model::*;
use gitem_core::repo::DiffSource;
use gitem_core::{GitExe, LogQuery, Repo, ops};

const WAIT: Duration = Duration::from_secs(600);

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: GitExe,
}

fn real_git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git");
    g.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
    g
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = real_git();
        let f = Self { _dir: dir, root, git };
        f.git(&["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Test User"), ("user.email", "test@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
            f.git(&["config", k, v]);
        }
        f
    }
    fn git(&self, args: &[&str]) -> String {
        self.git.run(Some(&self.root), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
    }
    fn write(&self, p: &str, s: &str) {
        let path = self.root.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, s).unwrap();
    }
    fn commit(&self, m: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", m]);
    }
    fn import(&self, script: String) {
        let opts = gitem_core::runner::RunOpts { stdin: Some(script.into_bytes()), is_write: true, ..Default::default() };
        let o = self.git.run_opts(Some(&self.root), &["fast-import".to_string(), "--quiet".to_string()], &opts, &mut |_| {}).unwrap();
        assert!(o.ok(), "{}", o.stderr);
    }
    fn commits(&self, n: usize) {
        let mut s = String::new();
        for i in 0..n {
            let parent = if i > 0 { format!("from :{i}\n") } else { String::new() };
            let msg = format!("commit number {i}");
            s.push_str(&format!("commit refs/heads/main\nmark :{}\ncommitter T <t@x> {} +0000\ndata {}\n{msg}\n{parent}M 100644 inline f{}.txt\ndata {}\n{i}\n\n", i + 1, 1_600_000_000 + i, msg.len(), i % 50, i.to_string().len() + 1));
        }
        self.import(s);
        self.git(&["reset", "-q", "--hard", "main"]);
    }
}

fn harness_with<'a>(f: &Fx, git: GitExe, settings: gitem_core::settings::Settings) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, settings);
    app.git = Ok(git);
    let root = f.root.clone();
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

fn harness<'a>(f: &Fx) -> Harness<'a, GitemApp> {
    harness_with(f, f.git.clone(), Default::default())
}

fn quiet(h: &Harness<GitemApp>) -> bool {
    gitem::task::running(&h.ctx) == 0 && h.state().ops_idle()
}

fn idle(h: &mut Harness<GitemApp>, ms: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_quiet(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    let mut n = 0;
    while n < 5 {
        h.step();
        n = if quiet(h) { n + 1 } else { 0 };
        assert!(t.elapsed() < WAIT, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(h: &mut Harness<GitemApp>) {
    let t = Instant::now();
    while h.state().repos.first().is_none_or(|r| r.data.status.get().is_none() || r.data.refs.get().is_none()) {
        h.step();
        assert!(t.elapsed() < WAIT);
        std::thread::sleep(Duration::from_millis(5));
    }
    idle(h, 700);
    wait_quiet(h);
}

/// The middle of `n` frames, in milliseconds.
fn frame_ms(h: &mut Harness<GitemApp>, n: usize) -> f64 {
    let mut times: Vec<f64> = (0..n)
        .map(|_| {
            let t = Instant::now();
            h.step();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[n / 2]
}

/// The middle of a few goes at something that runs git.
fn best_ms(mut run: impl FnMut()) -> f64 {
    let mut times: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            run();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[2]
}

fn commands(g: &GitExe) -> Vec<String> {
    g.log.lock().unwrap().iter().map(|e| e.command.clone()).collect()
}

/// Commands logged since `from`, grouped by subcommand, most first.
fn summary(g: &GitExe, from: usize) -> String {
    let all = commands(g);
    let mut by: std::collections::BTreeMap<String, usize> = Default::default();
    for c in all.iter().skip(from) {
        *by.entry(c.split_whitespace().nth(1).unwrap_or("?").to_string()).or_default() += 1;
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    let detail: Vec<String> = v.iter().map(|(k, n)| format!("{k} {n}")).collect();
    format!("{} commands: {}", all.len().saturating_sub(from), detail.join(", "))
}

fn untracked(n: usize) -> RepoStatus {
    let entries = (0..n).map(|i| StatusEntry { path: format!("many/dir{}/file{i}.txt", i % 40), orig_path: None, index: FileStatus::Unmodified, worktree: FileStatus::Untracked, conflict: None, submodule: false }).collect();
    RepoStatus { branch: Some("main".into()), entries, ..Default::default() }
}

fn changes_frames(tree: bool) {
    for n in [1_000, 6_000, 20_000, 100_000] {
        let f = Fx::new();
        f.write("a.txt", "a\n");
        f.commit("base");
        let mut h = harness_with(&f, f.git.clone(), gitem_core::settings::Settings { changes_tree: tree, ..Default::default() });
        settle(&mut h);
        h.state_mut().repos[0].open_tab(Tab::changes());
        h.state_mut().repos[0].data.status.value = Some(untracked(n).into());
        h.state_mut().repos[0].data.mark_changed();
        h.step();
        h.step();
        println!("PF-5 Changes {} {n} entries: {:.2} ms per frame", if tree { "tree" } else { "flat" }, frame_ms(&mut h, 30));
    }
}

#[test]
#[ignore = "measurement"]
fn pf5_changes_flat() {
    changes_frames(false);
}

#[test]
#[ignore = "measurement"]
fn pf5_changes_tree() {
    changes_frames(true);
}

fn many_refs(f: &Fx, n: usize) {
    f.commits(200);
    let shas = f.git(&["rev-list", "HEAD"]);
    let shas: Vec<&str> = shas.lines().collect();
    let refs: String = (0..n).map(|i| {
        let sha = shas[i % shas.len()];
        format!("create refs/heads/topic/branch-{i} {sha}\ncreate refs/tags/t{i} {sha}\n")
    }).collect();
    let op = ops::GitOp::new("refs").step(&["update-ref", "--stdin"]).stdin(refs.into_bytes());
    op.run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    f.git(&["pack-refs", "--all"]);
}

#[test]
#[ignore = "measurement"]
fn pf36_sidebar_refs() {
    let f = Fx::new();
    many_refs(&f, 5_000);
    let mut h = harness(&f);
    settle(&mut h);
    println!("PF-36 5,000 branches and 5,000 tags, tags closed: {:.2} ms per frame", frame_ms(&mut h, 60));
    use egui_kittest::kittest::Queryable;
    h.get_by_label("Tags section").click();
    h.step();
    idle(&mut h, 500);
    println!("PF-36 5,000 branches and 5,000 tags, tags open: {:.2} ms per frame", frame_ms(&mut h, 60));
}

fn minified(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 64);
    let mut i = 0;
    while s.len() < bytes {
        s += &format!("function f{i}(a,b){{var x=a+{i};if(x>b)return\"s{i}\";return x*2}};");
        i += 1;
    }
    s.truncate(bytes);
    s
}

#[test]
#[ignore = "measurement"]
fn pf2_long_lines() {
    for kb in [5, 50, 200, 1_000, 5_000] {
        let f = Fx::new();
        f.write("min.js", "x\n");
        f.commit("base");
        f.write("min.js", &format!("{}\n", minified(kb * 1000)));
        let mut h = harness(&f);
        settle(&mut h);
        let t = Instant::now();
        h.state_mut().repos[0].open_tab(Tab::file("min.js", None, None));
        wait_quiet(&mut h);
        let loaded = t.elapsed().as_secs_f64();
        println!("PF-2 File view, {kb} KB line: {:.2} ms per frame, loaded in {loaded:.2} s", frame_ms(&mut h, 30));
        let t = Instant::now();
        h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("min.js".into())));
        wait_quiet(&mut h);
        let loaded = t.elapsed().as_secs_f64();
        println!("PF-2 Diff view, {kb} KB line: {:.2} ms per frame, loaded in {loaded:.2} s", frame_ms(&mut h, 30));
    }
}

/// A screen of long lines, which is what per-line caching is for.
#[test]
#[ignore = "measurement"]
fn pf2_many_long_lines() {
    let f = Fx::new();
    let line = minified(50_000);
    let text: String = (0..200).map(|i| format!("{i}{line}\n")).collect();
    f.write("min.js", "x\n");
    f.commit("base");
    f.write("min.js", &text);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::file("min.js", None, None));
    wait_quiet(&mut h);
    println!("PF-2 File view, a screen of 50 KB lines: {:.2} ms per frame", frame_ms(&mut h, 30));
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Unstaged, Some("min.js".into())));
    wait_quiet(&mut h);
    println!("PF-2 Diff view, a screen of 50 KB lines: {:.2} ms per frame", frame_ms(&mut h, 30));
}

#[test]
#[ignore = "measurement"]
fn pf10_big_commit() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    let mut s = String::from("commit refs/heads/big\ncommitter T <t@x> 1600000000 +0000\ndata 3\nbig\nfrom refs/heads/main^0\n");
    for i in 0..20_000 {
        s.push_str(&format!("M 100644 inline d{}/f{i}.txt\ndata 2\nx\n", i % 200));
    }
    s.push('\n');
    f.import(s);
    let sha = f.git(&["rev-parse", "big"]).trim().to_string();
    let mut h = harness(&f);
    settle(&mut h);
    let t = Instant::now();
    h.state_mut().repos[0].inspect.set(&sha);
    wait_quiet(&mut h);
    println!("PF-10 Inspect, 20,000-file commit: loaded in {:.2} s, {:.2} ms per frame", t.elapsed().as_secs_f64(), frame_ms(&mut h, 30));
    let t = Instant::now();
    h.state_mut().repos[0].open_tab(Tab::diff(DiffSource::Commit(sha.clone()), None));
    wait_quiet(&mut h);
    println!("PF-10 Diff view and Inspect, 20,000-file commit: loaded in {:.2} s, {:.2} ms per frame", t.elapsed().as_secs_f64(), frame_ms(&mut h, 30));
    let repo = Repo::open(f.git.clone(), &f.root).unwrap();
    println!("PF-10 commit_files alone: {:.0} ms", best_ms(|| drop(repo.commit_files(&sha).unwrap())));
}

#[test]
#[ignore = "measurement"]
fn pf13_pf20_log() {
    let f = Fx::new();
    f.commits(60_000);
    let repo = Repo::open(f.git.clone(), &f.root).unwrap();
    println!("PF-13 log limit 1, 60,000 commits: {:.0} ms", best_ms(|| drop(repo.commit("HEAD").unwrap())));
    let q = LogQuery { revs: vec!["HEAD".into()], limit: 100, ..Default::default() };
    println!("PF-13 log limit 100: {:.0} ms", best_ms(|| drop(repo.log(&q).unwrap())));
    let before = commands(&f.git).len();
    repo.log(&q).unwrap();
    println!("PF-20 one log: {}", summary(&f.git, before));
    println!("PF-32 contributors, 60,000 commits: {:.0} ms", best_ms(|| drop(repo.contributors("HEAD", false).unwrap())));
    println!("PF-32 people, as the sidebar now reads them: {:.0} ms", best_ms(|| drop(repo.people("HEAD").unwrap())));
}

/// A checked-out tree with many tracked files in many folders, and no untracked ones.
fn big_tree(f: &Fx, n: usize) {
    let mut s = String::from("commit refs/heads/main\ncommitter T <t@x> 1600000000 +0000\ndata 3\nbig\n");
    for i in 0..n {
        s.push_str(&format!("M 100644 inline a{}/b{}/f{i}.txt\ndata 2\nx\n", i % 100, i % 1000));
    }
    s.push('\n');
    f.import(s);
    f.git(&["reset", "-q", "--hard", "main"]);
}

#[test]
#[ignore = "measurement"]
fn pf9_file_list() {
    let f = Fx::new();
    big_tree(&f, 100_000);
    let repo = Repo::open(f.git.clone(), &f.root).unwrap();
    println!("PF-9 file list, 100,000 tracked files: {:.0} ms", best_ms(|| drop(repo.tracked_files(None).unwrap())));
    println!("PF-9 status, for comparison: {:.0} ms", best_ms(|| drop(repo.status().unwrap())));
}

#[test]
#[ignore = "measurement"]
fn pf12_refresh_counts() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base");
    let t = Instant::now();
    let mut h = harness(&f);
    settle(&mut h);
    println!("PF-19 open a small repository: {} in {:.2} s", summary(&f.git, 0), t.elapsed().as_secs_f64());
    let from = commands(&f.git).len();
    f.git(&["update-ref", "refs/heads/other", "HEAD"]);
    let from2 = commands(&f.git).len();
    idle(&mut h, 1500);
    wait_quiet(&mut h);
    println!("PF-12 one ref made outside: {} (one of them the update itself: {})", summary(&f.git, from2), from2 - from);
    let from = commands(&f.git).len();
    f.write("b.txt", "b2\n");
    f.git(&["add", "b.txt"]);
    let from2 = commands(&f.git).len();
    idle(&mut h, 1500);
    wait_quiet(&mut h);
    println!("PF-12 a file staged outside: {} ({} for the stage itself)", summary(&f.git, from2), from2 - from);
    let from = commands(&f.git).len();
    h.state_mut().apply_action(&egui::Context::default(), gitem::action::Action::Run(ops::unstage_paths(&["b.txt".to_string()], true)));
    idle(&mut h, 1500);
    wait_quiet(&mut h);
    println!("PF-12 a file unstaged in gitem: {}", summary(&f.git, from));
}

#[test]
#[ignore = "measurement"]
fn pf14_branches_behind() {
    let f = Fx::new();
    f.commits(20);
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("r.git");
    f.git(&["clone", "-q", "--bare", ".", bare_path.to_str().unwrap()]);
    f.git(&["remote", "add", "origin", bare_path.to_str().unwrap()]);
    f.git(&["fetch", "-q", "origin"]);
    for i in 0..60 {
        let name = format!("b{i}");
        f.git(&["branch", &name, "HEAD~1"]);
        f.git(&["update-ref", &format!("refs/remotes/origin/{name}"), "HEAD"]);
        f.git(&["config", &format!("branch.{name}.remote"), "origin"]);
        f.git(&["config", &format!("branch.{name}.merge"), &format!("refs/heads/{name}")]);
    }
    let start = commands(&f.git).len();
    let mut h = harness(&f);
    settle(&mut h);
    println!("PF-14 open with 60 branches behind: {}", summary(&f.git, start));
    let from = commands(&f.git).len();
    f.git(&["update-ref", "refs/heads/unrelated", "HEAD"]);
    idle(&mut h, 1500);
    wait_quiet(&mut h);
    println!("PF-14 then one ref changes: {}", summary(&f.git, from));
}

#[test]
#[ignore = "measurement"]
fn pf11_ignored_churn() {
    let f = Fx::new();
    f.write(".gitignore", "target/\n");
    f.write("a.txt", "a\n");
    f.commit("base");
    std::fs::create_dir_all(f.root.join("target")).unwrap();
    let mut h = harness(&f);
    settle(&mut h);
    let from = commands(&f.git).len();
    let generation = h.state().repos[0].data.status_gen;
    let root = f.root.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..60 {
            let _ = std::fs::write(root.join("target/out.txt"), format!("{i}\n"));
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    idle(&mut h, 6000);
    writer.join().unwrap();
    println!("PF-11 6 s of writes to an ignored file: {}, status_gen +{}", summary(&f.git, from), h.state().repos[0].data.status_gen - generation);
    let from = commands(&f.git).len();
    let generation = h.state().repos[0].data.status_gen;
    let root = f.root.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..60 {
            let _ = std::fs::write(root.join("a.txt"), format!("{i}\n"));
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    idle(&mut h, 6000);
    writer.join().unwrap();
    println!("PF-11 6 s of writes to a tracked file: {}, status_gen +{}", summary(&f.git, from), h.state().repos[0].data.status_gen - generation);
}

/// A git that takes `ms` longer for the named subcommands, and logs when each ran.
fn slow_git(names: &str, ms: u64, log: &Path) -> GitExe {
    let me = std::env::current_exe().unwrap();
    let exe = me.parent().unwrap().parent().unwrap().join("examples").join(if cfg!(windows) { "slowgit.exe" } else { "slowgit" });
    assert!(exe.exists(), "build the slowgit example first: {}", exe.display());
    let real = real_git();
    // These run with --test-threads=1, and nothing else reads them until a child starts.
    unsafe {
        std::env::set_var("SLOWGIT_REAL", &real.path);
        std::env::set_var("SLOWGIT_MATCH", names);
        std::env::set_var("SLOWGIT_MS", ms.to_string());
        std::env::set_var("SLOWGIT_LOG", log);
    }
    let _ = std::fs::remove_file(log);
    let mut g = real;
    g.path = exe;
    g
}

/// Started, and the most running at once.
fn slow_report(log: &Path) -> (usize, i32) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut ev: Vec<(u128, i32)> = text.lines().filter_map(|l| {
        let mut p = l.split(' ');
        let k = p.next()?;
        Some((p.next()?.parse().ok()?, if k == "S" { 1 } else { -1 }))
    }).collect();
    ev.sort();
    let (mut cur, mut peak) = (0, 0);
    for (_, d) in &ev {
        cur += d;
        peak = peak.max(cur);
    }
    (ev.iter().filter(|e| e.1 == 1).count(), peak)
}

fn pf3_run(what: &str, names: &str, settings: gitem_core::settings::Settings, tab: Option<Tab>) {
    let f = Fx::new();
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    f.write("big.txt", &body);
    f.write("churn.txt", "0\n");
    f.commit("first");
    f.write("big.txt", &format!("{body}x\n"));
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("slow.log");
    let g = slow_git(names, 3000, &log);
    let mut h = harness_with(&f, g, settings);
    settle(&mut h);
    if let Some(t) = tab {
        h.state_mut().repos[0].open_tab(t);
    }
    idle(&mut h, 500);
    let root = f.root.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..50 {
            let _ = std::fs::write(root.join("churn.txt"), format!("{i}\n"));
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    idle(&mut h, 10_000);
    writer.join().unwrap();
    let (started, peak) = slow_report(&log);
    println!("PF-3 {what}, each takes 3 s, 10 s of writes: {started} started, peak {peak} at once");
    idle(&mut h, 7000);
    wait_quiet(&mut h);
}

#[test]
#[ignore = "measurement"]
fn pf3_pileup() {
    pf3_run("Files sidebar, ls-files", "ls-files", gitem_core::settings::Settings { sidebar_files: true, ..Default::default() }, None);
    pf3_run("File view, blame", "blame", Default::default(), Some(Tab::file("big.txt", None, None)));
    pf3_run("Diff view, diff", "diff", Default::default(), Some(Tab::diff(DiffSource::Unstaged, Some("big.txt".into()))));
}

#[test]
#[ignore = "measurement"]
fn pf4_selection() {
    let f = Fx::new();
    f.commits(200);
    f.git(&["tag", "v1", "main~5"]);
    let shas = f.git(&["rev-list", "HEAD"]);
    let shas: Vec<String> = shas.lines().map(String::from).collect();
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("slow.log");
    let g = slow_git("log,show,for-each-ref,notes,describe,remote", 250, &log);
    let mut h = harness_with(&f, g.clone(), Default::default());
    settle(&mut h);
    idle(&mut h, 3000);
    wait_quiet(&mut h);
    let from = commands(&g).len();
    h.state_mut().repos[0].inspect.set(&shas[10]);
    idle(&mut h, 500);
    wait_quiet(&mut h);
    println!("PF-4 one selection: {}", summary(&g, from));
    let _ = std::fs::remove_file(&log);
    let from = commands(&g).len();
    let t = Instant::now();
    let mut presses = 0;
    while t.elapsed() < Duration::from_secs(5) {
        h.state_mut().repos[0].inspect.set(&shas[20 + presses % 150]);
        h.step();
        presses += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    let (started, peak) = slow_report(&log);
    println!("PF-4 storm, each command takes 250 ms: {presses} selections in 5 s, {started} started, peak {peak} at once");
    idle(&mut h, 3000);
    wait_quiet(&mut h);
    println!("PF-4 storm when it has drained: {}", summary(&g, from));
}

/// AS-11, with a git that takes 4 seconds to answer the first question, standing in for an
/// unreachable share: nothing here may touch a network.
#[test]
#[ignore = "measurement"]
fn as11_slow_session_repo() {
    let f = Fx::new();
    f.write("a.txt", "a\n");
    f.commit("base");
    let d = tempfile::tempdir().unwrap();
    let g = slow_git("rev-parse", 4000, &d.path().join("slow.log"));
    let s = gitem_core::settings::Settings { git_path: g.path.to_string_lossy().into_owned(), open_repos: vec![f.root.to_string_lossy().replace('\\', "/")], ..Default::default() };
    s.save().unwrap();
    let ctx = egui::Context::default();
    let t = Instant::now();
    let app = GitemApp::new(&ctx, &[]);
    println!("AS-11 start with a session repository that takes 4 s to answer: window after {:.2} s", t.elapsed().as_secs_f64());
    drop(app);
    let _ = std::fs::remove_file(gitem_core::settings::Settings::path());
}

#[test]
#[ignore = "measurement"]
fn pf22_pf23_lists() {
    let f = Fx::new();
    many_refs(&f, 3_000);
    let mut h = harness(&f);
    settle(&mut h);
    {
        let mut log = f.git.log.lock().unwrap();
        for i in 0..2_000 {
            log.push_back(gitem_core::runner::LogEntry { command: format!("git status {i}"), cwd: "C:/r".into(), code: 0, millis: 1, output: "x".repeat(8000), is_write: false });
        }
    }
    h.state_mut().repos[0].open_tab(Tab::Console(Box::default()));
    idle(&mut h, 200);
    println!("PF-22 Console, 2,000 full entries: {:.2} ms per frame", frame_ms(&mut h, 30));
    h.state_mut().open_palette_with("branch");
    idle(&mut h, 500);
    wait_quiet(&mut h);
    println!("PF-23 palette, 3,000 branches: {:.2} ms per frame", frame_ms(&mut h, 30));
}

#[test]
#[ignore = "measurement"]
fn pf16_files_sidebar() {
    let f = Fx::new();
    big_tree(&f, 20_000);
    let mut h = harness_with(&f, f.git.clone(), gitem_core::settings::Settings { sidebar_files: true, ..Default::default() });
    settle(&mut h);
    println!("PF-16 Files sidebar, 20,000 files, folders closed: {:.2} ms per frame", frame_ms(&mut h, 30));
    use egui_kittest::kittest::Queryable;
    for name in ["a0", "a0/b0", "a1"] {
        if let Some(row) = h.query_by_label(name) {
            row.click();
        }
        h.step();
        h.step();
    }
    println!("PF-16 Files sidebar, 20,000 files, a few folders open: {:.2} ms per frame", frame_ms(&mut h, 30));
}

#[test]
#[ignore = "measurement"]
fn pf15_big_file() {
    let f = Fx::new();
    let body: String = (0..1_000_000).map(|i| format!("line number {i} of a big file\n")).collect();
    f.write("big.txt", &body);
    f.commit("base");
    let mut h = harness(&f);
    settle(&mut h);
    let t = Instant::now();
    h.state_mut().repos[0].open_tab(Tab::file("big.txt", None, None));
    wait_quiet(&mut h);
    println!("PF-15 File view, {} MB file: loaded in {:.1} s, {:.2} ms per frame", body.len() / 1_000_000, t.elapsed().as_secs_f64(), frame_ms(&mut h, 30));
}

/// Status slowed by a tree whose files were all touched, then the same after a refresh.
#[test]
#[ignore = "measurement"]
fn pf24_touched_files() {
    let f = Fx::new();
    big_tree(&f, 20_000);
    let repo = Repo::open(f.git.clone(), &f.root).unwrap();
    // Earlier, not later: git never trusts a timestamp newer than the index it's reading.
    let later = std::time::SystemTime::now() - Duration::from_secs(600);
    for i in 0..20_000 {
        let p = f.root.join(format!("a{}/b{}/f{i}.txt", i % 100, i % 1000));
        let file = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        file.set_modified(later).unwrap();
    }
    println!("PF-24 status, 20,000 files touched: {:.0} ms", best_ms(|| drop(repo.status().unwrap())));
    ops::refresh_index().background().run(&f.git, Some(&f.root), None, &mut |_| {}).unwrap();
    println!("PF-24 status, after refreshing the index: {:.0} ms", best_ms(|| drop(repo.status().unwrap())));
}

#[test]
#[ignore = "measurement"]
fn pf30_pf31_views() {
    let f = Fx::new();
    f.commits(5_000);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::visual(""));
    wait_quiet(&mut h);
    println!("PF-30 Visual, 5,000 commits: {:.2} ms per frame", frame_ms(&mut h, 30));
    let ppp = h.ctx.pixels_per_point();
    let t = Instant::now();
    let out = h.output().clone();
    let prims = h.ctx.tessellate(out.shapes, ppp);
    println!("PF-30 Visual, 5,000 commits, tessellate: {:.2} ms, {} primitives", t.elapsed().as_secs_f64() * 1000.0, prims.len());

    let f = Fx::new();
    let body: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
    f.write("m.txt", &format!("{body}base\n"));
    f.commit("base");
    f.git(&["switch", "-q", "-c", "side"]);
    f.write("m.txt", &format!("{body}side\n"));
    f.commit("side");
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", &format!("{body}main\n"));
    f.commit("main");
    let _ = f.git.run_raw(Some(&f.root), &["merge", "side"]);
    let mut h = harness(&f);
    settle(&mut h);
    h.state_mut().repos[0].open_tab(Tab::merge("m.txt"));
    wait_quiet(&mut h);
    println!("PF-31 merge view, 20,000 stable lines closed: {:.2} ms per frame", frame_ms(&mut h, 30));
    use egui_kittest::kittest::Queryable;
    if let Some(open) = h.query_by_label_contains("unchanged") {
        open.click();
        h.step();
        h.step();
        println!("PF-31 merge view, 20,000 stable lines open: {:.2} ms per frame", frame_ms(&mut h, 30));
    }
}
