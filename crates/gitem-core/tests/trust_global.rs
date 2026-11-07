//! What the user set up for themselves stays in force, with no question asked. This has its
//! own test binary because it needs a global git config, which is per binary.
gitem_testenv::isolate!();

use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use gitem_core::trust::TrustState;
use gitem_core::{GitExe, Repo};

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    g.helper = None;
    g
}

fn q(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git runs");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stat_dirty(p: &Path) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10)).unwrap();
}

/// One global filter that notes when it runs, set once, and a marker for all to share.
fn global_setup() -> (MutexGuard<'static, ()>, std::path::PathBuf) {
    static TURN: Mutex<()> = Mutex::new(());
    static MARKER: OnceLock<std::path::PathBuf> = OnceLock::new();
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let marker = MARKER.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("gitem-trust-global-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = dir.join("marker.txt");
        let prog = format!("sh -c 'echo mine >> \"{}\"; cat' -", m.to_string_lossy().replace('\\', "/"));
        q(&std::env::temp_dir(), &["config", "--global", "filter.mine.clean", &prog]);
        q(&std::env::temp_dir(), &["config", "--global", "filter.shared.clean", &prog]);
        m
    });
    let _ = std::fs::remove_file(marker);
    (turn, marker.clone())
}

fn repo_with_attr(attr: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    q(d.path(), &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("core.autocrlf", "false")] {
        q(d.path(), &["config", k, v]);
    }
    std::fs::write(d.path().join(".gitattributes"), attr).unwrap();
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    q(d.path(), &["add", "-A"]);
    q(d.path(), &["commit", "-q", "-m", "base"]);
    d
}

fn fired(marker: &Path) -> String {
    std::fs::read_to_string(marker).unwrap_or_default()
}

#[test]
fn a_filter_in_global_config_runs_as_usual_with_nothing_asked() {
    let (_turn, marker) = global_setup();
    let d = repo_with_attr("*.txt filter=mine\n");
    let _ = std::fs::remove_file(&marker);
    stat_dirty(&d.path().join("a.txt"));
    let repo = Repo::open(git(), d.path()).unwrap();
    assert_eq!(repo.trust.state, TrustState::Clean, "{:?}", repo.trust.risky);
    assert!(repo.git.neutral.config.is_empty());
    let st = repo.status().unwrap();
    assert!(fired(&marker).contains("mine"), "the user's own filter ran");
    assert!(st.entries.is_empty(), "and the file is clean");
}

#[test]
fn a_repository_overriding_a_global_filter_gets_the_users_filter_instead() {
    let (_turn, marker) = global_setup();
    let d = repo_with_attr("*.txt filter=shared\n");
    let evil = d.path().join("evil-ran.txt");
    q(d.path(), &["config", "filter.shared.clean", &format!("sh -c 'echo x >> \"{}\"; cat' -", evil.to_string_lossy().replace('\\', "/"))]);
    let _ = std::fs::remove_file(&marker);
    stat_dirty(&d.path().join("a.txt"));
    let repo = Repo::open(git(), d.path()).unwrap();
    assert_eq!(repo.trust.state, TrustState::Undecided);
    let st = repo.status().unwrap();
    assert!(!evil.exists(), "the repository's own didn't run");
    assert!(fired(&marker).contains("mine"), "the user's did");
    assert!(st.entries.iter().all(|e| e.path != "a.txt"), "so the file is clean");
}

#[test]
fn the_same_value_locally_as_globally_is_not_a_question() {
    let (_turn, _marker) = global_setup();
    let d = repo_with_attr("*.txt filter=mine\n");
    let value = q(d.path(), &["config", "--global", "filter.mine.clean"]);
    q(d.path(), &["config", "filter.mine.clean", value.trim()]);
    assert_eq!(Repo::open(git(), d.path()).unwrap().trust.state, TrustState::Clean);
}

/// With git-lfs installed the usual way: nothing to ask, and no false "modified".
#[test]
fn global_lfs_stays_functional_and_unprompted() {
    let (_turn, _marker) = global_setup();
    if !Command::new("git").args(["lfs", "version"]).output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: git-lfs isn't installed");
        return;
    }
    q(&std::env::temp_dir(), &["lfs", "install", "--skip-repo"]);
    let d = tempfile::tempdir().unwrap();
    q(d.path(), &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("core.autocrlf", "false")] {
        q(d.path(), &["config", k, v]);
    }
    q(d.path(), &["lfs", "track", "*.bin"]);
    std::fs::write(d.path().join("big.bin"), "pretend this is big\n").unwrap();
    q(d.path(), &["add", "-A"]);
    q(d.path(), &["commit", "-q", "-m", "add"]);
    assert!(q(d.path(), &["cat-file", "-p", "HEAD:big.bin"]).contains("git-lfs"), "it really went through LFS");
    stat_dirty(&d.path().join("big.bin"));

    let repo = Repo::open(git(), d.path()).unwrap();
    assert_eq!(repo.trust.state, TrustState::Clean, "{:?}", repo.trust.risky);
    assert!(repo.git.neutral.config.is_empty());
    let st = repo.status().unwrap();
    assert!(st.entries.is_empty(), "no false M: {:?}", st.entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>());

    // Installing it locally as well adds nothing new, so that's no question either.
    q(d.path(), &["lfs", "install", "--local"]);
    assert_eq!(Repo::open(git(), d.path()).unwrap().trust.state, TrustState::Clean);
}
