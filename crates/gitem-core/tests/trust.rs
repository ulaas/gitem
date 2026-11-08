//! Repositories that try to run something or write somewhere. Every program here only
//! appends a word to a marker file in a temp folder, and every file a hostile argument
//! names is in one too. Each test first shows plain git really does run the thing or write
//! the file, so a test can't pass just by the attack having stopped working.
gitem_testenv::isolate!();

use std::path::{Path, PathBuf};
use std::process::Command;

use gitem_core::model::LogQuery;
use gitem_core::ops::{self, GitOp};
use gitem_core::repo::{BlameOpts, DiffOpts, DiffSource, rev_arg};
use gitem_core::runner::RunOpts;
use gitem_core::trust::TrustState;
use gitem_core::{GitExe, Repo, mcp};
use serde_json::json;

fn git() -> GitExe {
    let mut g = GitExe::discover(None).expect("git available");
    g.helper = None;
    g
}

/// Plain git, as a terminal would run it.
fn plain(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git").arg("-C").arg(dir).args(args).output().expect("git runs")
}

fn q(dir: &Path, args: &[&str]) -> String {
    let o = plain(dir, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

struct Fx {
    _dir: tempfile::TempDir,
    parent: PathBuf,
    root: PathBuf,
    marker: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Not canonicalized: on Windows that gives a `\\?\` path, which git reads as a host.
        let parent = dir.path().to_path_buf();
        let root = parent.join("repo");
        std::fs::create_dir(&root).unwrap();
        init(&root);
        Self { _dir: dir, marker: parent.join("marker.txt"), parent, root }
    }

    /// A one-line shell command that notes its word and passes its input along.
    fn prog(&self, word: &str) -> String {
        format!("sh -c 'echo {word} >> \"{}\"; cat' -", self.marker.to_string_lossy().replace('\\', "/"))
    }

    fn set(&self, key: &str, value: &str) {
        q(&self.root, &["config", key, value]);
    }

    fn write(&self, name: &str, text: &str) {
        let p = self.root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn commit(&self, msg: &str) {
        q(&self.root, &["add", "-A"]);
        q(&self.root, &["commit", "-q", "-m", msg]);
    }

    fn fired(&self) -> String {
        std::fs::read_to_string(&self.marker).unwrap_or_default()
    }

    fn clear(&self) {
        let _ = std::fs::remove_file(&self.marker);
    }

    fn open(&self) -> Repo {
        Repo::open(git(), &self.root).expect("opens")
    }

    /// A file git has to look at again: what any zip or tar round trip leaves.
    fn stat_dirty(&self, name: &str) {
        stat_dirty(&self.root.join(name));
    }
}

fn init(root: &Path) {
    q(root, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.com"), ("commit.gpgsign", "false"), ("core.autocrlf", "false"), ("protocol.file.allow", "always")] {
        q(root, &["config", k, v]);
    }
}

fn stat_dirty(p: &Path) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10)).unwrap();
}

/// A repository whose `a.txt` goes through a filter from its own config, as delivered.
fn filtered() -> Fx {
    let f = Fx::new();
    f.write(".gitattributes", "*.txt filter=evil\n");
    f.write("a.txt", "one\n");
    f.commit("base");
    f.set("filter.evil.clean", &f.prog("clean"));
    f.set("filter.evil.smudge", &f.prog("smudge"));
    f.set("filter.evil.required", "true");
    f.stat_dirty("a.txt");
    f
}

#[test]
fn a_delivered_folder_runs_nothing_when_opened_or_read() {
    let f = filtered();
    plain(&f.root, &["status", "--porcelain"]);
    assert!(f.fired().contains("clean"), "plain git runs the filter, or this test proves nothing");
    f.clear();
    f.stat_dirty("a.txt");

    let repo = f.open();
    assert_eq!(f.fired(), "", "opening ran it");
    assert_eq!(repo.trust.state, TrustState::Undecided);
    let keys: Vec<&str> = repo.trust.risky.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["filter.evil.clean", "filter.evil.smudge"]);
    repo.status().expect("status works with the filter off");
    assert_eq!(f.fired(), "", "status ran it");
    repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    repo.diff_text(&DiffSource::Unstaged).unwrap();
    repo.file_at("HEAD", "a.txt").unwrap();
    assert_eq!(f.fired(), "");
}

#[test]
fn trusting_turns_the_programs_on_and_a_new_key_asks_again() {
    let f = filtered();
    let mut repo = f.open();
    repo.set_trusted(true).unwrap();
    repo.status().unwrap();
    assert!(f.fired().contains("clean"), "a trusted repository works as in plain git");

    let again = f.open();
    assert_eq!(again.trust.state, TrustState::Trusted, "remembered");
    assert!(again.git.neutral.config.is_empty());

    f.set("diff.other.textconv", &f.prog("textconv"));
    let changed = f.open();
    assert_eq!(changed.trust.state, TrustState::Undecided, "the list grew");
    assert!(!changed.git.neutral.config.is_empty(), "and everything is off again until there's an answer");
}

#[test]
fn keeping_them_off_is_remembered_and_stays_off() {
    let f = filtered();
    let mut repo = f.open();
    repo.set_trusted(false).unwrap();
    assert_eq!(repo.trust.state, TrustState::Declined);
    repo.status().unwrap();
    assert_eq!(f.fired(), "");
    let again = f.open();
    assert_eq!(again.trust.state, TrustState::Declined);
    again.status().unwrap();
    assert_eq!(f.fired(), "");
}

#[test]
fn a_moved_folder_is_a_new_folder() {
    let f = filtered();
    f.open().set_trusted(true).unwrap();
    let moved = f.parent.join("moved");
    std::fs::rename(&f.root, &moved).unwrap();
    assert_eq!(Repo::open(git(), &moved).unwrap().trust.state, TrustState::Undecided);
}

#[test]
fn finding_repositories_runs_nothing() {
    let f = filtered();
    let found = gitem_core::repo::find_repos(&f.parent, 2);
    assert_eq!(found, vec![f.root.clone()]);
    assert_eq!(f.fired(), "");
}

/// A name that can't be said again can't be overridden, so that's a refusal.
#[test]
fn a_driver_name_that_is_not_text_stops_the_repository_opening() {
    let f = Fx::new();
    f.write(".gitattributes", "*.txt filter=x\n");
    f.write("a.txt", "one\n");
    f.commit("base");
    let config = f.root.join(".git/config");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.extend_from_slice(b"[filter \"\xff\"]\n\tclean = evil\n");
    std::fs::write(&config, bytes).unwrap();
    let e = Repo::open(git(), &f.root).err().expect("refused");
    assert!(e.to_string().contains("UTF-8"), "{e}");
}

/// One bad byte in a comment was enough for the quick look at a submodule to give up.
#[test]
fn a_submodule_is_looked_at_properly_whatever_its_config_holds() {
    let f = Fx::new();
    let sub = f.parent.join("sub");
    std::fs::create_dir(&sub).unwrap();
    init(&sub);
    std::fs::write(sub.join("a.txt"), "a\n").unwrap();
    q(&sub, &["add", "-A"]);
    q(&sub, &["commit", "-q", "-m", "one"]);
    q(&f.root, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sub"]);
    q(&f.root, &["commit", "-q", "-m", "with sub"]);
    assert_eq!(f.open().trust.state, TrustState::Clean);

    let inner = f.root.join("sub");
    let config = PathBuf::from(q(&inner, &["rev-parse", "--absolute-git-dir"]).trim()).join("config");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.extend_from_slice(b"# \xff\n[include]\n\tpath = extra.cfg\n");
    std::fs::write(&config, &bytes).unwrap();
    std::fs::write(config.with_file_name("extra.cfg"), format!("[filter \"evil\"]\n\tclean = \"{}\"\n", f.prog("c").replace('"', "\\\""))).unwrap();
    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided, "{:?}", repo.trust.risky);
    assert_eq!(repo.trust.risky[0].key, "filter.evil.clean");
}

/// More than it will check isn't a way past it: it says so, and doesn't go into any of them.
#[test]
fn too_many_submodules_count_as_unknown() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let head = q(&f.root, &["rev-parse", "HEAD"]).trim().to_string();
    for i in 0..70 {
        let path = format!("s{i}");
        std::fs::create_dir(f.root.join(&path)).unwrap();
        init(&f.root.join(&path));
        q(&f.root, &["update-index", "--add", "--cacheinfo", &format!("160000,{head},{path}")]);
    }
    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided, "{:?}", repo.trust.risky);
    assert_eq!(repo.trust.risky[0].key, "(submodules)");
    assert!(repo.git.neutral.config.contains(&("diff.ignoreSubmodules".to_string(), "all".to_string())));
    repo.status().unwrap();
}

#[test]
fn a_repository_with_no_programs_is_never_asked_about() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    // None of these runs anything.
    f.set("core.fsmonitor", "true");
    f.set("core.hooksPath", ".husky");
    f.set("remote.origin.url", "https://example.invalid/x.git");
    f.set("url.https://example.invalid/.insteadOf", "ex:");
    f.set("filter.nothing.clean", "");
    f.set("submodule.s.update", "rebase");
    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Clean, "{:?}", repo.trust.risky);
    assert!(repo.git.neutral.config.is_empty());
    assert_eq!(repo.trust.hooks_path.as_deref(), Some(".husky"), "kept for the question, if there is one");
}

#[test]
fn include_path_is_seen_too() {
    let f = filtered();
    q(&f.root, &["config", "--unset", "filter.evil.clean"]);
    q(&f.root, &["config", "--unset", "filter.evil.smudge"]);
    let extra = f.root.join(".git/extra.cfg");
    std::fs::write(&extra, format!("[filter \"evil\"]\n\tclean = \"{}\"\n", f.prog("included").replace('"', "\\\""))).unwrap();
    f.set("include.path", "extra.cfg");
    f.stat_dirty("a.txt");
    plain(&f.root, &["status", "--porcelain"]);
    assert!(f.fired().contains("included"));
    f.clear();
    f.stat_dirty("a.txt");
    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided);
    repo.status().unwrap();
    assert_eq!(f.fired(), "");
}

#[test]
fn a_driver_whose_name_holds_an_equals_sign_is_still_stopped() {
    let f = Fx::new();
    f.write(".gitattributes", "*.txt filter=a=b\n");
    f.write("a.txt", "one\n");
    f.commit("base");
    f.set("filter.a=b.clean", &f.prog("clean"));
    f.stat_dirty("a.txt");
    plain(&f.root, &["status", "--porcelain"]);
    assert!(f.fired().contains("clean"));
    f.clear();
    f.stat_dirty("a.txt");
    f.open().status().unwrap();
    assert_eq!(f.fired(), "");
}

#[test]
fn a_required_filter_with_a_failing_program_does_not_stop_reads() {
    let f = filtered();
    f.set("filter.evil.clean", "exit 1");
    f.stat_dirty("a.txt");
    assert!(f.open().status().is_ok());
}

/// The parent's config is clean. Git goes into the submodule anyway.
#[test]
fn a_submodule_config_counts_as_the_repository_config() {
    let f = Fx::new();
    let sub = f.parent.join("sub");
    std::fs::create_dir(&sub).unwrap();
    init(&sub);
    std::fs::write(sub.join(".gitattributes"), "* filter=evil\n").unwrap();
    std::fs::write(sub.join("a.txt"), "a\n").unwrap();
    q(&sub, &["add", "-A"]);
    q(&sub, &["commit", "-q", "-m", "one"]);
    q(&f.root, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &sub.to_string_lossy(), "sub"]);
    q(&f.root, &["commit", "-q", "-m", "with sub"]);
    let inner = f.root.join("sub");
    q(&inner, &["config", "filter.evil.clean", &f.prog("subclean")]);
    q(&inner, &["config", "filter.evil.required", "true"]);
    stat_dirty(&inner.join("a.txt"));
    plain(&f.root, &["status", "--porcelain"]);
    assert!(f.fired().contains("subclean"), "plain git runs it from the parent");
    f.clear();
    stat_dirty(&inner.join("a.txt"));

    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided);
    assert_eq!(repo.trust.risky[0].submodule, "sub");
    assert!(repo.trust.risky[0].describe().starts_with("sub: filter.evil.clean = "), "{}", repo.trust.risky[0].describe());
    repo.status().unwrap();
    assert_eq!(f.fired(), "");
}

/// A plain clone can't carry a `.git`, but it can carry a folder shaped like one.
#[test]
fn a_tracked_gitdir_shaped_folder_is_refused() {
    let f = Fx::new();
    let evil = f.root.join("evil");
    for d in ["objects", "refs/heads"] {
        std::fs::create_dir_all(evil.join(d)).unwrap();
    }
    std::fs::write(evil.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let config = format!("[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tworktree = .\n[filter \"evil\"]\n\tclean = \"{}\"\n", f.prog("embedded").replace('"', "\\\""));
    std::fs::write(evil.join("config"), config).unwrap();
    std::fs::write(evil.join("refs/heads/.keep"), "").unwrap();
    std::fs::write(evil.join("objects/.keep"), "").unwrap();
    std::fs::write(evil.join(".gitattributes"), "* filter=evil\n").unwrap();
    std::fs::write(evil.join("a.txt"), "a\n").unwrap();
    f.commit("looks innocent");
    let clone = f.parent.join("clone");
    q(&f.parent, &["clone", "-q", &f.root.to_string_lossy(), "clone"]);

    // Its index and files have to be there for a filter to fire. Plain git shows the risk.
    let inner = clone.join("evil");
    plain(&inner, &["add", "-A"]);
    assert!(f.fired().contains("embedded"), "plain git treats it as a repository");
    f.clear();
    stat_dirty(&inner.join("a.txt"));

    assert!(Repo::open(git(), &clone).is_ok(), "the clone itself is fine");
    let e = Repo::open(git(), &inner).err().expect("refused");
    assert!(e.to_string().contains("bare"), "{e}");
    assert_eq!(f.fired(), "");
}

#[test]
fn each_way_a_read_could_start_a_program_is_closed() {
    let f = Fx::new();
    f.write(".gitattributes", "*.txt filter=evil diff=evil merge=evil\n");
    f.write("a.txt", "one\n");
    f.commit("base");
    q(&f.root, &["switch", "-q", "-c", "side"]);
    f.write("a.txt", "side\n");
    f.commit("side");
    q(&f.root, &["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("main");
    f.write("a.txt", "dirty\n");
    let head = q(&f.root, &["rev-parse", "HEAD"]).trim().to_string();
    let signed = {
        let tree = q(&f.root, &["rev-parse", "HEAD^{tree}"]).trim().to_string();
        let raw = format!("tree {tree}\nparent {head}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n xx\n -----END PGP SIGNATURE-----\n\nsigned\n");
        let mut c = Command::new("git").arg("-C").arg(&f.root).args(["hash-object", "-t", "commit", "-w", "--stdin"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(c.stdin.as_mut().unwrap(), raw.as_bytes()).unwrap();
        String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap().trim().to_string()
    };
    for (k, word) in [("filter.evil.clean", "clean"), ("filter.evil.smudge", "smudge"), ("diff.evil.textconv", "textconv"), ("diff.evil.command", "diffcommand"), ("diff.external", "external"), ("merge.evil.driver", "mergedriver")] {
        f.set(k, &f.prog(word));
    }
    let gpg = f.parent.join("gpg.sh");
    write_script(&gpg, &format!("#!/bin/sh\necho gpg >> \"{}\"\n", f.marker.to_string_lossy().replace('\\', "/")));
    f.set("gpg.program", &gpg.to_string_lossy().replace('\\', "/"));
    let hook = f.root.join(".git/hooks/post-index-change");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, &format!("#!/bin/sh\necho hook >> \"{}\"\n", f.marker.to_string_lossy().replace('\\', "/")));
    f.stat_dirty("a.txt");

    // Each of these starts its program in plain git. Together they prove the fixture works.
    plain(&f.root, &["log", "-1", "--format=%G?", &signed]);
    assert!(f.fired().contains("gpg"), "gpg: {}", f.fired());
    f.clear();
    plain(&f.root, &["show", "HEAD", "--format="]);
    assert!(f.fired().contains("textconv"), "textconv: {}", f.fired());
    f.clear();
    plain(&f.root, &["diff", "HEAD~1", "HEAD"]);
    assert!(f.fired().contains("external") || f.fired().contains("diffcommand"), "external diff: {}", f.fired());
    f.clear();
    plain(&f.root, &["merge-tree", "--write-tree", "main", "side"]);
    assert!(f.fired().contains("mergedriver"), "merge driver: {}", f.fired());
    f.clear();

    let repo = f.open();
    let quiet = |what: &str, f: &Fx| {
        assert_eq!(f.fired(), "", "{what} started a program");
    };
    repo.status().unwrap();
    quiet("status", &f);
    repo.signature(&signed).unwrap();
    quiet("signature", &f);
    repo.diff(&DiffSource::Commit(head.clone()), None, &DiffOpts::default()).unwrap();
    quiet("a commit's diff", &f);
    repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    quiet("the working diff", &f);
    repo.diff(&DiffSource::WorkingVsHead, Some("a.txt"), &DiffOpts::default()).unwrap();
    quiet("the diff against HEAD", &f);
    for src in [DiffSource::Unstaged, DiffSource::Staged, DiffSource::WorkingVsHead, DiffSource::Commit(head.clone()), DiffSource::Range("main".into(), "side".into()), DiffSource::MergeBase("main".into(), "side".into()), DiffSource::WorkingVs("side".into())] {
        repo.diff_text(&src).unwrap();
        quiet(&format!("diff_text of {src:?}"), &f);
        repo.patch_bytes(&src).unwrap();
        quiet(&format!("patch_bytes of {src:?}"), &f);
    }
    repo.blame("a.txt", None, BlameOpts::default()).unwrap();
    repo.blame("a.txt", Some("side"), BlameOpts::default()).unwrap();
    quiet("blame", &f);
    repo.file_history_points("a.txt", 10).unwrap();
    quiet("file history", &f);
    repo.log(&LogQuery { pickaxe_s: Some("side".into()), all: true, ..Default::default() }).unwrap();
    quiet("a pickaxe search", &f);
    repo.predict_conflicts("main", "side").unwrap();
    quiet("predicting conflicts", &f);
    repo.compare_files("main", "side", false).unwrap();
    quiet("comparing branches", &f);
    assert!(!repo.trust.risky.is_empty());
}

/// The hook is gone from every read, even in a repository nobody has any doubts about.
#[test]
fn reads_never_run_hooks_even_in_a_trusted_repository() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let hook = f.root.join(".git/hooks/post-index-change");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, &format!("#!/bin/sh\necho hook >> \"{}\"\n", f.marker.to_string_lossy().replace('\\', "/")));
    f.stat_dirty("a.txt");
    plain(&f.root, &["diff"]);
    assert!(f.fired().contains("hook"), "plain git fires it");
    f.clear();
    f.stat_dirty("a.txt");
    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Clean);
    repo.diff(&DiffSource::Unstaged, None, &DiffOpts::default()).unwrap();
    repo.status().unwrap();
    assert_eq!(f.fired(), "");
    assert!(repo.hooks().iter().any(|h| h.name == "pre-commit"), "the Hooks page still looks in the right folder");
    assert!(repo.hooks()[0].path.replace('\\', "/").contains(".git/hooks"), "{}", repo.hooks()[0].path);
}

/// `git config` is left out of the read flags, so these are the repository's own values.
#[test]
fn reading_config_sees_the_real_values() {
    let f = filtered();
    f.set("core.fsmonitor", "true");
    f.set("core.hooksPath", ".husky");
    let repo = f.open();
    assert_eq!(repo.config_get("core.fsmonitor").as_deref(), Some("true"));
    assert_eq!(repo.config_get("core.hooksPath").as_deref(), Some(".husky"));
    assert_eq!(repo.config_get("filter.evil.required").as_deref(), Some("true"), "a restricted repository doesn't change that");
    assert!(repo.health().unwrap().items.iter().any(|i| i.label == "File system monitor" && i.enabled));
}

/// User-started writes in a restricted repository skip its programs too, and still run its
/// hooks, as in git.
#[test]
fn writes_skip_the_programs_but_keep_the_hooks() {
    let f = filtered();
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, &format!("#!/bin/sh\necho precommit >> \"{}\"\n", f.marker.to_string_lossy().replace('\\', "/")));
    f.write("a.txt", "two\n");
    let repo = f.open();
    ops::stage_all().run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(f.fired(), "");
    ops::commit(&ops::CommitOpts { message: "m".into(), ..Default::default() }).run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(f.fired().trim(), "precommit");
}

#[test]
fn a_write_nobody_started_runs_no_hooks() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let hook = f.root.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, &format!("#!/bin/sh\necho hook >> \"{}\"\n", f.marker.to_string_lossy().replace('\\', "/")));
    let repo = f.open();
    let op = GitOp::new("t").step(&["update-ref", "refs/x/y", "HEAD"]);
    op.clone().background().run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(f.fired(), "");
    op.run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert!(f.fired().contains("hook"), "the same write by hand does");
}

/// Everything that talks to a remote: run by a user on a restricted repository, none of
/// these may start the repository's program. Only localhost addresses that refuse.
#[test]
fn network_programs_are_neutralised() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let bare = f.parent.join("r.git");
    q(&f.parent, &["init", "-q", "--bare", &bare.to_string_lossy()]);
    let script = |name: &str, word: &str| {
        let p = f.parent.join(name);
        write_script(&p, &format!("#!/bin/sh\necho {word} >> \"{}\"\nexit 1\n", f.marker.to_string_lossy().replace('\\', "/")));
        p.to_string_lossy().replace('\\', "/")
    };
    f.set("core.sshCommand", &script("ssh.sh", "sshcommand"));
    f.set("core.gitProxy", &script("proxy.sh", "gitproxy"));
    f.set("credential.helper", &format!("!{}", script("cred.sh", "credhelper")));
    f.set("remote.up.url", &bare.to_string_lossy());
    f.set("remote.up.uploadpack", &script("up.sh", "uploadpack"));
    f.set("remote.ex.url", &format!("ext::sh {}", script("ext.sh", "exttransport")));
    f.set("protocol.ext.allow", "always");

    plain(&f.root, &["ls-remote", "ssh://127.0.0.1:1/x.git"]);
    assert!(f.fired().contains("sshcommand"), "{}", f.fired());
    f.clear();
    plain(&f.root, &["ls-remote", "git://127.0.0.1:1/x.git"]);
    assert!(f.fired().contains("gitproxy"), "{}", f.fired());
    f.clear();
    plain(&f.root, &["ls-remote", "up"]);
    assert!(f.fired().contains("uploadpack"), "{}", f.fired());
    f.clear();
    plain(&f.root, &["ls-remote", "ex"]);
    assert!(f.fired().contains("exttransport"), "{}", f.fired());
    f.clear();

    let repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided);
    let keys: Vec<&str> = repo.trust.risky.iter().map(|r| r.key.as_str()).collect();
    for k in ["core.sshcommand", "core.gitproxy", "credential.helper", "remote.up.uploadpack", "protocol.ext.allow"] {
        assert!(keys.contains(&k), "{k} isn't in {keys:?}");
    }
    let run = |args: &[&str]| GitOp::new("t").step(args).run(&repo.git, Some(&repo.root), None, &mut |_| {});
    let _ = run(&["ls-remote", "ssh://127.0.0.1:1/x.git"]);
    assert_eq!(f.fired(), "", "ssh");
    let _ = run(&["ls-remote", "git://127.0.0.1:1/x.git"]);
    assert_eq!(f.fired(), "", "proxy");
    let up = run(&["ls-remote", "up"]);
    assert!(up.err().is_some_and(|e| e.to_string().contains("not allowed")), "git takes the first upload-pack, so config can't replace it: the transport is off");
    assert_eq!(f.fired(), "", "upload-pack");
    let _ = run(&["ls-remote", "ex"]);
    assert_eq!(f.fired(), "", "ext");
    let opts = RunOpts { stdin: Some(b"url=https://example.invalid/\n\n".to_vec()), env: vec![("GIT_TERMINAL_PROMPT".into(), "0".into()), ("GIT_ASKPASS".into(), "".into())], ..Default::default() };
    let args = ["credential", "fill"].map(String::from);
    let _ = repo.git.run_opts(Some(&repo.root), &args, &opts, &mut |_| {});
    assert_eq!(f.fired(), "", "credential helper");
}

/// Each tool, with every field an agent controls set to an option that writes a file.
#[test]
fn no_mcp_tool_can_write_a_file() {
    let f = filtered();
    let victim = f.parent.join("victim.txt");
    let v = victim.to_string_lossy().replace('\\', "/");
    let inject = format!("--output={v}");

    q(&f.root, &["log", "-1", &inject]);
    assert!(victim.exists(), "plain git writes the file");
    std::fs::remove_file(&victim).unwrap();
    let repo = f.open();

    let call = |name: &str, args: serde_json::Value| {
        let reply = mcp::handle(&repo, &json!({"id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
        reply.pointer("/result/content/0/text").and_then(|t| t.as_str()).unwrap_or_default().to_string()
    };
    let cases = [
        ("git_diff", json!({"commit": inject})),
        ("git_diff", json!({"commit": format!("--output={v}"), "staged": true})),
        ("git_log", json!({"query": format!("ref:{inject}")})),
        ("git_log", json!({"query": format!("commit:{inject}")})),
        ("git_log", json!({"query": format!("#:{inject}")})),
        ("git_log", json!({"query": format!("\"ref:{inject}\"")})),
        ("git_log", json!({"path": inject})),
        ("git_log", json!({"query": inject})),
        ("git_log", json!({"query": format!("author:{inject}")})),
        ("git_log", json!({"query": format!("file:{inject}")})),
        ("git_log", json!({"query": format!("change:{inject}")})),
        ("git_log", json!({"query": format!("after:{inject}")})),
        ("git_blame", json!({"path": inject})),
        ("git_status", json!({"x": inject})),
        ("git_branches", json!({"x": inject})),
    ];
    for (name, args) in cases {
        let reply = call(name, args.clone());
        assert!(!victim.exists(), "{name} {args} wrote the file: {reply}");
    }
    assert_eq!(f.fired(), "", "and ran nothing");
    let e = call("git_diff", json!({"commit": inject}));
    assert!(e.contains("starts with a dash"), "{e}");
}

#[test]
fn every_revision_sink_refuses_an_option() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let victim = f.parent.join("victim.txt");
    let bad = format!("--output={}", victim.to_string_lossy().replace('\\', "/"));
    let repo = f.open();
    let b = bad.as_str();
    let h = "HEAD";
    assert!(rev_arg(b).is_err() && rev_arg("-x").is_err() && rev_arg("HEAD").is_ok() && rev_arg("a-b").is_ok() && rev_arg("main~1").is_ok());
    assert!(repo.commit(b).is_err());
    assert!(repo.log(&LogQuery { revs: vec![b.to_string()], ..Default::default() }).is_err());
    assert!(repo.messages_since(b).is_err());
    assert!(repo.signature(b).is_err());
    assert!(repo.containing_refs(b).is_err());
    assert!(repo.commit_note(b).is_none());
    assert!(repo.describe(b).is_none());
    assert!(repo.resolve(b).is_err());
    assert!(repo.merge_base(b, h).is_err() && repo.merge_base(h, b).is_err());
    assert!(repo.ahead_behind(b, h).is_err() && repo.ahead_behind(h, b).is_err());
    assert!(repo.reflog(b, 5).is_err());
    assert!(repo.file_at(b, "a.txt").is_err());
    assert!(repo.tracked_files(Some(b)).is_err());
    assert!(repo.commit_files(b).is_err());
    assert!(repo.compare_files(b, h, false).is_err() && repo.compare_files(h, b, true).is_err());
    for src in [DiffSource::Commit(b.into()), DiffSource::WorkingVs(b.into()), DiffSource::Range(b.into(), h.into()), DiffSource::Range(h.into(), b.into()), DiffSource::MergeBase(b.into(), h.into()), DiffSource::MergeBase(h.into(), b.into())] {
        assert!(repo.diff(&src, None, &DiffOpts::default()).is_err(), "{src:?}");
        assert!(repo.diff_text(&src).is_err(), "{src:?}");
        assert!(repo.patch_bytes(&src).is_err(), "{src:?}");
    }
    assert!(repo.blame("a.txt", Some(b), BlameOpts::default()).is_err());
    assert!(repo.history_points(None, b, 5).is_err());
    assert!(repo.commit_stats(&[b.to_string()], false, 5).is_err());
    assert!(repo.predict_conflicts(b, h).is_err() && repo.predict_conflicts(h, b).is_err());
    assert!(repo.file_sizes(b).is_err());
    assert!(repo.file_activity(b, 5).is_err());
    assert!(repo.contributors(b, false).is_err());
    assert!(repo.commit_times(b, None, "2000-01-01").is_err());
    assert!(repo.next_revision("a.txt", b).is_none());
    assert!(repo.prev_revision("a.txt", b).is_none());
    assert!(!repo.is_ancestor(b, h) && !repo.is_ancestor(h, b));
    assert!(!victim.exists(), "something wrote the file");
    assert!(repo.check_ignore(b).is_none());
    assert!(!victim.exists());
    repo.log(&LogQuery { revs: vec!["HEAD".to_string()], ..Default::default() }).expect("real revisions still work");
    repo.blame("a.txt", Some("HEAD"), BlameOpts::default()).expect("including blame's");
}

#[test]
fn ops_refuse_a_name_that_git_would_read_as_an_option() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    f.write("a.txt", "uncommitted\n");
    let repo = f.open();
    let bad = "--hard";
    let forced = ops::reset(bad, ops::ResetMode::Mixed);
    assert_eq!(forced.steps[0].args, ["reset", "--mixed", "--hard", "--"], "this is what it would have run");
    let cases: Vec<GitOp> = vec![
        forced,
        ops::checkout(bad),
        ops::checkout_detached(bad),
        ops::merge(bad, ops::MergeMode::Default),
        ops::rebase(&ops::RebaseOpts { onto: bad.into(), ..Default::default() }),
        ops::cherry_pick(&[bad.to_string()], false, false),
        ops::revert(bad, false, None),
        ops::branch_create("ok", Some(bad), false),
        ops::branch_create(bad, None, false),
        ops::branch_force_move("ok", bad),
        ops::tag_create("ok", Some(bad), None, false),
        ops::tag_create(bad, None, None, false),
        ops::tag_delete(bad),
        ops::stash_apply(bad, false, false),
        ops::bisect_start(bad, "HEAD"),
        ops::archive(bad, "out.tar", "tar"),
        ops::format_patch(bad, "out"),
        ops::fixup_commit(bad),
        ops::restore_file_from(bad, "a.txt"),
    ];
    for op in cases {
        let e = op.run(&repo.git, Some(&repo.root), None, &mut |_| {}).err().unwrap_or_else(|| panic!("{} ran", op.title));
        assert!(e.to_string().contains("starts with a dash"), "{}: {e}", op.title);
    }
    assert_eq!(std::fs::read_to_string(f.root.join("a.txt")).unwrap(), "uncommitted\n");
}

/// What a clone from a hostile server looks like.
#[test]
fn a_dash_leading_tag_from_a_clone_is_shown_by_its_full_name_and_can_be_removed() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    q(&f.root, &["update-ref", "refs/tags/--output=pwned.txt", "HEAD"]);
    q(&f.root, &["update-ref", "refs/tags/--hard", "HEAD"]);
    q(&f.root, &["update-ref", "refs/heads/--bad", "HEAD"]);
    q(&f.parent, &["clone", "-q", &f.root.to_string_lossy(), "clone"]);
    let clone = f.parent.join("clone");
    let pwned = clone.join("pwned.txt");
    q(&clone, &["log", "-1", "--output=pwned.txt"]);
    assert!(pwned.exists(), "as a plain option, it writes the file");
    std::fs::remove_file(&pwned).unwrap();
    q(&clone, &["config", "user.name", "T"]);
    q(&clone, &["config", "user.email", "t@example.com"]);

    let repo = Repo::open(git(), &clone).unwrap();
    let tags = repo.tags().unwrap();
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"refs/tags/--output=pwned.txt"), "{names:?}");
    assert!(names.contains(&"refs/tags/--hard"), "{names:?}");
    // Every way a name is used.
    let commits = repo.log(&LogQuery { revs: vec!["refs/tags/--output=pwned.txt".into()], limit: 1, ..Default::default() }).unwrap();
    assert_eq!(commits.len(), 1, "it works as a revision");
    let labels: Vec<String> = commits[0].refs.iter().map(|r| r.name.clone()).collect();
    assert!(labels.iter().any(|l| l == "refs/tags/--output=pwned.txt"), "commit labels too: {labels:?}");
    assert!(labels.iter().all(|l| !l.starts_with('-')), "{labels:?}");
    assert!(repo.containing_refs("HEAD").unwrap().1.iter().all(|t| !t.starts_with('-')));
    assert!(repo.diff(&DiffSource::Range("refs/tags/--output=pwned.txt".into(), "HEAD".into()), None, &DiffOpts::default()).is_ok());
    assert!(!pwned.exists());

    // The palette's "Reset to which ref?" lists these names.
    f_write(&clone, "a.txt", "uncommitted\n");
    ops::reset("refs/tags/--hard", ops::ResetMode::Mixed).run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read_to_string(clone.join("a.txt")).unwrap(), "uncommitted\n", "still a mixed reset");

    ops::tag_delete("refs/tags/--output=pwned.txt").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    let names: Vec<String> = repo.tags().unwrap().into_iter().map(|t| t.name).collect();
    assert!(!names.iter().any(|n| n.contains("pwned")), "{names:?}");
    assert!(!pwned.exists());
}

fn f_write(root: &Path, name: &str, text: &str) {
    std::fs::write(root.join(name), text).unwrap();
}

#[test]
fn a_dash_leading_default_branch_from_a_clone_is_handled() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    q(&f.root, &["update-ref", "refs/heads/--output=pwned.txt", "HEAD"]);
    q(&f.root, &["symbolic-ref", "HEAD", "refs/heads/--output=pwned.txt"]);
    q(&f.parent, &["clone", "-q", &f.root.to_string_lossy(), "clone"]);
    let clone = f.parent.join("clone");
    let repo = Repo::open(git(), &clone).unwrap();
    let st = repo.status().unwrap();
    assert_eq!(st.branch.as_deref(), Some("refs/heads/--output=pwned.txt"));
    let branches = repo.branches().unwrap();
    let local = branches.iter().find(|b| !b.is_remote).unwrap();
    assert_eq!(local.name, "refs/heads/--output=pwned.txt");
    assert!(repo.log(&LogQuery { revs: vec![local.name.clone()], limit: 1, ..Default::default() }).is_ok());
    assert!(!clone.join("pwned.txt").exists());
    // On that branch it's in use, so try one that isn't.
    q(&clone, &["switch", "-q", "--detach"]);
    ops::branch_delete(&local.name, true).run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert!(repo.branches().unwrap().iter().all(|b| b.is_remote));
}

/// A commit-msg hook shouldn't find its environment changed by gitem.
#[test]
fn hooks_see_the_users_locale_and_no_literal_path_mode() {
    let f = Fx::new();
    f.write("[id].txt", "one\n");
    f.write("id.txt", "other\n");
    f.commit("base");
    let log = f.parent.join("hookenv.txt");
    let hook = f.root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    write_script(&hook, &format!("#!/bin/sh\necho \"lc=[$LC_ALL] literal=[$GIT_LITERAL_PATHSPECS]\" >> \"{}\"\n", log.to_string_lossy().replace('\\', "/")));
    f.write("[id].txt", "two\n");
    f.write("id.txt", "changed too\n");
    let repo = f.open();
    let plan = vec![ops::ComposedCommit { message: "just the bracket file".to_string(), files: vec!["[id].txt".to_string()], ..Default::default() }];
    ops::compose_commits(&plan).run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    // The user's own locale, whatever it is: some machines, as macOS CI runners, set LC_ALL.
    let own = std::env::var("LC_ALL").unwrap_or_default();
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), format!("lc=[{own}] literal=[]"));
    let committed = q(&f.root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(committed.trim(), "[id].txt", "the name was still taken literally, not as a glob");
    assert!(q(&f.root, &["status", "--porcelain"]).contains("id.txt"), "the other file stays out of it");
}

#[test]
fn a_failed_write_still_reports_in_english() {
    let f = Fx::new();
    let repo = f.open();
    let e = GitOp::new("t").step(&["switch", "no-such-branch"]).run(&repo.git, Some(&repo.root), None, &mut |_| {}).err().unwrap();
    assert!(e.to_string().contains("invalid reference"), "{e}");
}

#[test]
fn config_set_copes_with_several_values_and_a_leading_dash() {
    let f = Fx::new();
    let repo = f.open();
    q(&f.root, &["config", "--add", "x.multi", "1"]);
    q(&f.root, &["config", "--add", "x.multi", "2"]);
    assert!(!plain(&f.root, &["config", "--local", "x.multi", "3"]).status.success(), "plain set refuses this");
    ops::config_set("x.multi", "3", "local").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(q(&f.root, &["config", "--get-all", "x.multi"]).trim(), "3");
    ops::config_set("x.dash", "--output=x", "local").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert_eq!(q(&f.root, &["config", "--get", "x.dash"]).trim(), "--output=x");
    ops::config_unset("x.multi", "local").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert!(!plain(&f.root, &["config", "--get", "x.multi"]).status.success());
}

#[test]
fn an_edit_made_in_gitem_is_not_asked_about_but_one_from_outside_is() {
    let f = Fx::new();
    f.write("a.txt", "one\n");
    f.commit("base");
    let mut repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Clean);
    assert!(!repo.recheck_trust(false).unwrap(), "nothing changed");

    ops::config_set("core.sshCommand", "my-ssh", "local").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    assert!(repo.recheck_trust(true).unwrap());
    assert_eq!(repo.trust.state, TrustState::Trusted, "the user typed it in here");

    f.set("filter.evil.clean", &f.prog("clean"));
    assert!(repo.recheck_trust(false).unwrap());
    assert_eq!(repo.trust.state, TrustState::Undecided);
    assert!(!repo.git.neutral.config.is_empty(), "off from now on");

    // And an edit in gitem doesn't get a repository out of being asked.
    ops::config_set("x.y", "z", "local").run(&repo.git, Some(&repo.root), None, &mut |_| {}).unwrap();
    f.set("diff.d.textconv", &f.prog("tc"));
    repo.recheck_trust(true).unwrap();
    assert_eq!(repo.trust.state, TrustState::Undecided);
}

#[test]
fn a_clone_is_trusted_as_it_stands_but_init_of_an_existing_folder_is_not_adopted() {
    let f = filtered();
    let clone = f.parent.join("clone");
    q(&f.parent, &["clone", "-q", &f.root.to_string_lossy(), "clone"]);
    gitem_core::trust::adopt_new_repo(&git(), &clone).unwrap();
    let cloned = Repo::open(git(), &clone).unwrap();
    assert_eq!(cloned.trust.state, TrustState::Clean, "a plain clone has no such keys");
    q(&clone, &["config", "core.sshCommand", "x"]);
    assert_eq!(Repo::open(git(), &clone).unwrap().trust.state, TrustState::Undecided, "later additions ask");

    // Its own template gave it this key, say.
    gitem_core::trust::adopt_new_repo(&git(), &clone).unwrap();
    assert_eq!(Repo::open(git(), &clone).unwrap().trust.state, TrustState::Trusted);
}

/// The Signing page reads and overwrites this file. A repository doesn't get to choose it.
#[test]
fn the_allowed_signers_path_comes_only_from_global_config() {
    let f = Fx::new();
    f.set("gpg.ssh.allowedSignersFile", "C:/somewhere/precious.txt");
    let repo = f.open();
    assert_eq!(repo.config_get("gpg.ssh.allowedSignersFile").as_deref(), Some("C:/somewhere/precious.txt"), "git itself would use it");
    assert_eq!(repo.global_config_path("gpg.ssh.allowedSignersFile"), None);
}

#[test]
fn mcp_blame_keeps_to_the_ignore_rules() {
    let f = Fx::new();
    f.write(".aiignore", "*.pem
");
    f.write("key.PEM", "SECRET
");
    f.write("ok.txt", "fine
");
    f.commit("base");
    let repo = f.open();
    let blame = |path: &str| mcp::handle(&repo, &json!({"id": 1, "method": "tools/call", "params": {"name": "git_blame", "arguments": {"path": path}}})).unwrap().to_string();
    assert!(!blame("key.PEM").contains("SECRET"));
    assert!(blame("ok.txt").contains("fine"));
}

#[test]
fn a_damaged_record_means_asking_again() {
    let f = filtered();
    f.open().set_trusted(true).unwrap();
    let path = gitem_core::settings::config_dir().join("trusted-repos.json");
    assert_eq!(f.open().trust.state, TrustState::Trusted);
    assert!(std::fs::read(&path).is_ok());
    // Other tests run beside this one and share the file. Rewriting it by hand lost the
    // records they added in between, so this one entry is damaged under the store's own lock.
    gitem_core::trust::remember(&f.open().root, "something else", true).unwrap();
    assert_eq!(f.open().trust.state, TrustState::Undecided);
}

/// Real LFS, if it's here. `lfs install --local` writes local keys, so this asks once.
#[test]
fn a_local_lfs_install_is_asked_about_once_and_then_works() {
    if !plain(Path::new("."), &["lfs", "version"]).status.success() {
        eprintln!("skipped: git-lfs isn't installed");
        return;
    }
    let f = Fx::new();
    q(&f.root, &["lfs", "install", "--local"]);
    q(&f.root, &["lfs", "track", "*.bin"]);
    f.write("big.bin", "pretend this is big\n");
    f.commit("add");
    f.stat_dirty("big.bin");

    let mut repo = f.open();
    assert_eq!(repo.trust.state, TrustState::Undecided);
    assert!(repo.trust.risky.iter().any(|r| r.key == "filter.lfs.clean"), "{:?}", repo.trust.risky);
    let st = repo.status().unwrap();
    assert!(st.entries.iter().any(|e| e.path == "big.bin"), "with the filter off it looks changed, as the banner says");

    repo.set_trusted(true).unwrap();
    let st = repo.status().unwrap();
    assert!(st.entries.is_empty(), "trusted, it's clean: {:?}", st.entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>());
    assert_eq!(f.open().trust.state, TrustState::Trusted, "and it isn't asked again");
}

/// A script git or the shell will run: written, and on Unix marked executable, which they need.
fn write_script(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
