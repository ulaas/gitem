//! Whether a repository's own config may run programs.
//!
//! A folder can arrive with its `.git` (a zip, a shared drive, a "repro" attachment), and
//! that config can name a program for git to run as a filter, a diff driver, a signature
//! checker and so on. Reading such a repository would run it, and a GUI reads as soon as
//! a folder is opened. So [`crate::Repo::open`] looks first. Every way into a repository
//! goes through it: the session, the command line, links, helper windows and MCP.
//!
//! Only config in the repository itself counts. What's in the user's global config, like
//! Git LFS, is theirs and is left alone, so normal repositories are never asked about.
//! Git hooks are a separate matter: reads never run them, and writes the user starts do,
//! as in git. `core.hooksPath` is shown when asking, for information.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::runner::{GitExe, Neutral, RunOpts};
use crate::{Error, Result};

/// One config entry that can start a program.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RiskyKey {
    /// The submodule it's in, or empty for the repository itself.
    pub submodule: String,
    pub key: String,
    pub value: String,
}

impl RiskyKey {
    /// For the question to the user. It can't say more than it shows, so line breaks and
    /// text direction controls are made visible, and very long values are cut.
    pub fn describe(&self) -> String {
        let value = crate::util::mask_config_value(&self.key, &self.value);
        let line = if self.submodule.is_empty() { format!("{} = {value}", self.key) } else { format!("{}: {} = {value}", self.submodule, self.key) };
        let shown: String = line.chars().take(400).map(|c| if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { '\u{fffd}' } else { c }).collect();
        if line.chars().count() > 400 { format!("{shown}…") } else { shown }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrustState {
    /// Nothing in its config runs a program.
    #[default]
    Clean,
    /// The user has accepted exactly this list.
    Trusted,
    /// Not asked yet, or the list has changed since. Its programs stay off.
    Undecided,
    /// The user said to keep them off. Asked again if the list changes.
    Declined,
}

/// What opening a repository found. See the module comment.
#[derive(Debug, Clone, Default)]
pub struct RepoTrust {
    pub state: TrustState,
    pub risky: Vec<RiskyKey>,
    pub hooks_path: Option<String>,
    pub hash: String,
    /// The overrides to use while it isn't trusted.
    pub neutral: Arc<Neutral>,
    /// The repository's own config as last read, to tell cheaply whether it has changed.
    fingerprint: String,
}

impl RepoTrust {
    /// Whether its programs are being kept from running.
    pub fn restricted(&self) -> bool {
        matches!(self.state, TrustState::Undecided | TrustState::Declined)
    }
}

/// Every key that can start a program. `git config` reports the section and the variable
/// in lower case and leaves the subsection as written. `remote.*.vcs` isn't here: it can
/// only name a `git-remote-*` program that's already on PATH.
const KEYS: &str = concat!(
    r"^(",
    r"filter\..*\.(clean|smudge|process)",
    r"|diff\..*\.(textconv|command)|diff\.external",
    r"|merge\..*\.driver",
    r"|gpg\.program|gpg\..*\.program|gpg\.ssh\.defaultkeycommand",
    r"|core\.(sshcommand|gitproxy|fsmonitor|alternaterefscommand|hookspath)",
    r"|credential\.helper|credential\..*\.helper",
    r"|alias\.lfs",
    r"|remote\..*\.(uploadpack|receivepack)",
    r"|protocol\.allow|protocol\.ext\.allow",
    r"|submodule\..*\.update",
    r"|lfs\.extension\..*\.(clean|smudge)|lfs\.customtransfer\..*\.path",
    r"|difftool\..*\.(cmd|path)|mergetool\..*\.(cmd|path)",
    r"|tar\..*\.command",
    r")$",
);

/// Words that must be in a submodule's config for [`KEYS`] to match, directly or by way of
/// another file. This spares a git process for each one on large repositories.
const KEY_WORDS: [&str; 18] = ["filter", "diff", "merge", "gpg", "fsmonitor", "sshcommand", "gitproxy", "alternaterefs", "credential", "alias", "protocol", "pack", "update", "lfs", "tool", "tar", "include", "worktreeconfig"];

/// How many git processes to spend on submodules, each about 30 ms, on the UI thread. A
/// repository with more than this isn't fully checked, so it counts as unknown.
const MAX_SUBMODULE_PROBES: usize = 64;

/// Whether this entry can start a program. Some only can with some values:
/// `core.fsmonitor=true` is git's own daemon, which the Health page offers, and a remote's
/// `ext::` URL runs its command only where a `protocol` setting allows that transport.
fn runs_a_program(key: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let var = key.rsplit('.').next().unwrap_or("");
    let section = key.split('.').next().unwrap_or("");
    match (section, var) {
        ("core", "fsmonitor") => !matches!(value.to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"),
        ("core", "hookspath") => false,
        ("protocol", "allow") => !value.eq_ignore_ascii_case("never"),
        ("submodule", "update") => value.starts_with('!'),
        _ => true,
    }
}

struct Entry {
    scope: String,
    key: String,
    value: String,
}

/// `git config --show-scope -z`: `scope NUL key LF value NUL`. A key with no value has no LF.
fn parse_entries(out: &str) -> Vec<Entry> {
    let mut it = out.split('\0');
    let mut v = Vec::new();
    while let (Some(scope), Some(rest)) = (it.next(), it.next()) {
        let (key, value) = rest.split_once('\n').unwrap_or((rest, ""));
        v.push(Entry { scope: scope.to_string(), key: key.to_string(), value: value.to_string() });
    }
    v
}

fn read_config(git: &GitExe, dir: &Path) -> Result<String> {
    let o = git.run_raw(Some(dir), &["config", "--show-scope", "-z", "--get-regexp", KEYS])?;
    match o.code {
        // A name that isn't valid text can't be named again to override it, so its driver
        // would stay on. Refusing is all that's left. Real configs don't have these.
        0 => String::from_utf8(o.stdout).map_err(|_| Error::Other(format!("The git config of {} has settings that aren't valid UTF-8, so gitem can't check what it would run, and won't open it.", dir.display()))),
        // No key matched.
        1 => Ok(String::new()),
        _ => Err(Error::Git(o.stderr.trim().to_string())),
    }
}

fn own_scope(scope: &str) -> bool {
    matches!(scope, "local" | "worktree")
}

/// Gitem's own "Use gitem for this repository" button writes this, so it isn't a surprise.
fn is_gitem_helper(git: &GitExe, key: &str, value: &str) -> bool {
    key == "credential.helper" && git.helper.as_deref().is_some_and(|h| crate::cred::helper_config_value(h) == value)
}

/// What the key would be if the repository hadn't set it, keeping the user's own config in
/// force: a global LFS filter, say, that the repository has overridden.
type Outer = HashMap<String, Vec<String>>;

fn outer_values(entries: &[Entry]) -> Outer {
    let mut m = Outer::new();
    for e in entries.iter().filter(|e| !own_scope(&e.scope)) {
        m.entry(e.key.clone()).or_default().push(e.value.clone());
    }
    m
}

fn risky_in(git: &GitExe, dir: &Path, submodule: &str, out: &mut Scan) -> Result<()> {
    let text = read_config(git, dir)?;
    let entries = parse_entries(&text);
    if submodule.is_empty() {
        out.fingerprint = text;
    }
    let outer = outer_values(&entries);
    for e in entries.iter().filter(|e| own_scope(&e.scope)) {
        if submodule.is_empty() && e.key == "core.hookspath" && !e.value.is_empty() {
            out.hooks_path = Some(e.value.clone());
        }
        // The same thing is in force anyway, whatever this repository says.
        let already_mine = outer.get(&e.key).is_some_and(|v| v.contains(&e.value));
        if runs_a_program(&e.key, &e.value) && !already_mine && !is_gitem_helper(git, &e.key, &e.value) {
            out.risky.push(RiskyKey { submodule: submodule.to_string(), key: e.key.clone(), value: e.value.clone() });
        }
    }
    out.outer.extend(outer);
    Ok(())
}

#[derive(Default)]
struct Scan {
    risky: Vec<RiskyKey>,
    hooks_path: Option<String>,
    outer: Outer,
    too_many_submodules: bool,
    fingerprint: String,
}

/// `git status` goes into each initialised submodule and runs that one's filters, from
/// config the scan of the parent can't see. So their config counts too.
fn scan_submodules(git: &GitExe, dir: &Path, prefix: &str, probes: &mut usize, out: &mut Scan) -> Result<()> {
    if *probes >= MAX_SUBMODULE_PROBES {
        out.too_many_submodules = true;
        return Ok(());
    }
    *probes += 1;
    let o = git.run_raw(Some(dir), &["ls-files", "--stage", "-z"])?;
    if !o.ok() {
        return Ok(());
    }
    let text = o.text();
    for rec in text.split('\0') {
        let Some(path) = rec.strip_prefix("160000 ").and_then(|r| r.split_once('\t')).map(|x| x.1) else { continue };
        let sub = dir.join(path);
        let Some(config) = submodule_config(&sub) else { continue };
        let name = format!("{prefix}{path}");
        // Bytes, not text: one odd byte in a comment mustn't be a way past this. A file that
        // can't be read gets asked about properly.
        let worth_asking = std::fs::read(&config).map_or(true, |bytes| {
            let low = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            KEY_WORDS.iter().any(|w| low.contains(w))
        });
        if worth_asking {
            if *probes >= MAX_SUBMODULE_PROBES {
                out.too_many_submodules = true;
                return Ok(());
            }
            *probes += 1;
            risky_in(git, &sub, &name, out)?;
        }
        scan_submodules(git, &sub, &format!("{name}/"), probes, out)?;
    }
    Ok(())
}

/// The config file of a checked-out submodule. Its `.git` is a folder, or a file naming one.
fn submodule_config(sub: &Path) -> Option<PathBuf> {
    let dot_git = sub.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.join("config"));
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    Some(sub.join(target).join("config"))
}

/// The overrides for this list. Each driver is emptied, or put back to what the user's own
/// config says. Nothing can be removed from the command line, only overridden, so each
/// name has to be listed.
fn neutral_for(risky: &[RiskyKey], outer: &Outer, too_many_submodules: bool) -> Neutral {
    let mut n = Neutral::default();
    let mut set = BTreeMap::<String, Vec<String>>::new();
    let last = |k: &str, none: &str| outer.get(k).and_then(|v| v.last().cloned()).unwrap_or_else(|| none.to_string());
    for r in risky {
        let k = r.key.as_str();
        let var = k.rsplit('.').next().unwrap_or("");
        let section = k.split('.').next().unwrap_or("");
        match (section, var) {
            ("filter", _) => {
                let driver = &k[..k.len() - var.len()];
                for field in ["clean", "smudge"] {
                    let name = format!("{driver}{field}");
                    let value = last(&name, "");
                    set.insert(name, vec![value]);
                }
                // Git ignores `clean` and `smudge` once a driver has a `process` at all, even
                // an empty one. So it's only emptied if there's nothing of the user's to lose
                // by that, or the repository set one that has to be stopped.
                let process = format!("{driver}process");
                let mine = |field: &str| outer.contains_key(&format!("{driver}{field}"));
                let repo_set_process = risky.iter().any(|r| r.key == process);
                if outer.contains_key(&process) {
                    let value = last(&process, "");
                    set.insert(process, vec![value]);
                } else if repo_set_process || !(mine("clean") || mine("smudge")) {
                    set.insert(process, vec![String::new()]);
                }
                let required = format!("{driver}required");
                let value = last(&required, "false");
                set.insert(required, vec![value]);
            }
            ("diff", "textconv") => n.no_textconv = true,
            // Git takes the first of these and the override comes last, so it can't be
            // overridden by config. An empty variable is what stops it.
            ("core", "gitproxy") => n.env.push(("GIT_PROXY_COMMAND".to_string(), String::new())),
            ("core", "sshcommand") => {
                set.insert(k.to_string(), vec![last(k, "ssh")]);
            }
            ("core", "fsmonitor") => {
                set.insert(k.to_string(), vec!["false".to_string()]);
            }
            // Git takes the first of these too, and only runs them for local, ssh and git://
            // remotes. So those are the transports that are off. Fetching over https works.
            ("remote", _) => n.env.push(("GIT_ALLOW_PROTOCOL".to_string(), "https:http".to_string())),
            ("protocol", _) => {
                set.insert("protocol.ext.allow".to_string(), vec!["never".to_string()]);
            }
            ("submodule", _) => {
                set.insert(k.to_string(), vec!["checkout".to_string()]);
            }
            // These collect: an empty value clears the list, then the user's own go back.
            ("credential", _) => {
                let mut values = vec![String::new()];
                values.extend(outer.get(k).cloned().unwrap_or_default());
                set.insert(k.to_string(), values);
            }
            _ => {
                set.insert(k.to_string(), vec![last(k, "")]);
            }
        }
    }
    if too_many_submodules {
        set.insert("diff.ignoreSubmodules".to_string(), vec!["all".to_string()]);
    }
    for (k, values) in set {
        n.config.extend(values.into_iter().map(|v| (k.clone(), v)));
    }
    n
}

const TOO_MANY_SUBMODULES: &str = "(submodules)";

/// Identifies the list: a change to it means asking again. The empty list has no hash.
fn list_hash(git: &GitExe, risky: &[RiskyKey]) -> Result<String> {
    if risky.is_empty() {
        return Ok(String::new());
    }
    let mut text = String::new();
    for r in risky {
        text.push_str(&format!("{}\0{}\0{}\n", r.submodule, r.key, r.value));
    }
    let opts = RunOpts { stdin: Some(text.into_bytes()), ..Default::default() };
    let args = ["hash-object", "--stdin"].map(String::from);
    Ok(git.run_opts(None, &args, &opts, &mut |_| {})?.into_result()?.text().trim().to_string())
}

/// Reads the repository's config and decides. Runs nothing from the repository: `git
/// config`, `ls-files` and `hash-object --stdin` start no filter or hook.
pub fn check(git: &GitExe, root: &Path) -> Result<RepoTrust> {
    let mut scan = Scan::default();
    risky_in(git, root, "", &mut scan)?;
    let mut probes = 0;
    scan_submodules(git, root, "", &mut probes, &mut scan)?;
    let neutral = neutral_for(&scan.risky, &scan.outer, scan.too_many_submodules);
    let mut risky = scan.risky;
    if scan.too_many_submodules {
        risky.push(RiskyKey { submodule: String::new(), key: TOO_MANY_SUBMODULES.to_string(), value: format!("more than {MAX_SUBMODULE_PROBES} to check, so their changes aren't shown until you decide") });
    }
    risky.sort();
    risky.dedup();
    let hash = list_hash(git, &risky)?;
    let state = if risky.is_empty() {
        TrustState::Clean
    } else {
        match load().get(&record_key(root)) {
            Some(r) if r.hash == hash => {
                if r.trusted {
                    TrustState::Trusted
                } else {
                    TrustState::Declined
                }
            }
            _ => TrustState::Undecided,
        }
    };
    Ok(RepoTrust { state, risky, hooks_path: scan.hooks_path, hash, neutral: Arc::new(neutral), fingerprint: scan.fingerprint })
}

/// Whether the repository's own config has changed since this was read. One quick `git
/// config`, so it can be asked after every write.
pub fn config_changed(git: &GitExe, root: &Path, since: &RepoTrust) -> Result<bool> {
    Ok(read_config(git, root)? != since.fingerprint)
}

// ---- the record of what was decided ------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub hash: String,
    pub trusted: bool,
}

fn store_path() -> PathBuf {
    crate::settings::config_dir().join("trusted-repos.json")
}

/// A folder's identity. A moved folder is a new one, and that's asked about again.
fn record_key(root: &Path) -> String {
    let s = root.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// A damaged file means asking again, which is the safe way to be wrong.
fn load() -> BTreeMap<String, Record> {
    std::fs::read(store_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Written beside the real file and moved over it, so a crash can't leave half a file.
pub fn remember(root: &Path, hash: &str, trusted: bool) -> Result<()> {
    // Read, change and write as one step. Another copy of gitem doing the same in the same
    // moment could lose this answer, and that only means asking again.
    static ONE_WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load();
    all.insert(record_key(root), Record { hash: hash.to_string(), trusted });
    let path = store_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all).map_err(|e| Error::Other(e.to_string()))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Gitem has just cloned or created this, so whatever config it has came from the user's
/// own template, not from a stranger. Later additions are asked about as usual.
pub fn adopt_new_repo(git: &GitExe, path: &Path) -> Result<()> {
    let repo = crate::Repo::open(git.clone(), path)?;
    if repo.trust.state != TrustState::Clean {
        remember(&repo.root, &repo.trust.hash, true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_values_that_run_something_count() {
        assert!(runs_a_program("filter.x.clean", "evil"));
        assert!(!runs_a_program("filter.x.clean", ""), "an empty driver does nothing");
        assert!(runs_a_program("core.fsmonitor", "./evil.sh"));
        for off in ["true", "FALSE", "yes", "0"] {
            assert!(!runs_a_program("core.fsmonitor", off), "{off} is git's own daemon, or none");
        }
        assert!(!runs_a_program("core.hookspath", ".husky"), "information only");
        assert!(runs_a_program("protocol.ext.allow", "always"));
        assert!(runs_a_program("protocol.allow", "user"));
        assert!(!runs_a_program("protocol.ext.allow", "NEVER"));
        assert!(!runs_a_program("submodule.a.update", "rebase"));
        assert!(runs_a_program("submodule.a.update", "!evil"));
    }

    fn key(k: &str, v: &str) -> RiskyKey {
        RiskyKey { submodule: String::new(), key: k.to_string(), value: v.to_string() }
    }

    fn overrides(n: &Neutral) -> Vec<(&str, &str)> {
        n.config.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    #[test]
    fn a_filter_is_emptied_completely_and_can_no_longer_be_required() {
        let n = neutral_for(&[key("filter.a=b.smudge", "evil")], &Outer::new(), false);
        assert_eq!(overrides(&n), vec![("filter.a=b.clean", ""), ("filter.a=b.process", ""), ("filter.a=b.required", "false"), ("filter.a=b.smudge", "")]);
    }

    #[test]
    fn the_users_own_driver_comes_back_when_the_repository_overrides_it() {
        let mut outer = Outer::new();
        outer.insert("filter.lfs.clean".into(), vec!["git-lfs clean -- %f".into()]);
        outer.insert("filter.lfs.required".into(), vec!["true".into()]);
        let n = neutral_for(&[key("filter.lfs.clean", "evil")], &outer, false);
        let o = overrides(&n);
        assert!(o.contains(&("filter.lfs.clean", "git-lfs clean -- %f")), "{o:?}");
        assert!(o.contains(&("filter.lfs.required", "true")), "{o:?}");
    }

    #[test]
    fn credential_helpers_are_cleared_then_the_users_own_restored() {
        let mut outer = Outer::new();
        outer.insert("credential.helper".into(), vec!["manager".into()]);
        let n = neutral_for(&[key("credential.helper", "!evil")], &outer, false);
        assert_eq!(overrides(&n), vec![("credential.helper", ""), ("credential.helper", "manager")]);
    }

    #[test]
    fn each_kind_of_key_has_a_harmless_value() {
        let n = neutral_for(&[key("core.sshcommand", "evil"), key("core.fsmonitor", "evil"), key("gpg.program", "evil"), key("gpg.ssh.program", "evil"), key("merge.m.driver", "evil"), key("diff.external", "evil"), key("diff.d.command", "evil"), key("protocol.ext.allow", "always"), key("submodule.s.update", "!evil"), key("alias.lfs", "!evil")], &Outer::new(), false);
        let o = overrides(&n);
        for want in [("core.sshcommand", "ssh"), ("core.fsmonitor", "false"), ("gpg.program", ""), ("gpg.ssh.program", ""), ("merge.m.driver", ""), ("diff.external", ""), ("diff.d.command", ""), ("protocol.ext.allow", "never"), ("submodule.s.update", "checkout"), ("alias.lfs", "")] {
            assert!(o.contains(&want), "{want:?} missing from {o:?}");
        }
        assert!(!n.no_textconv);
        assert!(neutral_for(&[key("diff.d.textconv", "evil")], &Outer::new(), false).no_textconv);
        assert_eq!(neutral_for(&[key("core.gitproxy", "evil")], &Outer::new(), false).env, vec![("GIT_PROXY_COMMAND".to_string(), String::new())]);
        assert_eq!(neutral_for(&[key("remote.o.receivepack", "evil")], &Outer::new(), false).env, vec![("GIT_ALLOW_PROTOCOL".to_string(), "https:http".to_string())]);
    }

    #[test]
    fn entries_parse_with_newlines_in_values_and_no_value_at_all() {
        let e = parse_entries("local\0filter.a=b.clean\nx y\nz\0global\0diff.d.command\0");
        assert_eq!((e[0].scope.as_str(), e[0].key.as_str(), e[0].value.as_str()), ("local", "filter.a=b.clean", "x y\nz"));
        assert_eq!((e[1].scope.as_str(), e[1].key.as_str(), e[1].value.as_str()), ("global", "diff.d.command", ""));
    }

    #[test]
    fn the_question_cannot_be_made_to_say_something_else() {
        let d = key("filter.x.clean", "good\nreally \u{202e}live.exe").describe();
        assert!(!d.contains('\n') && !d.contains('\u{202e}'), "{d:?}");
        let long = key("filter.x.clean", &"x".repeat(5000)).describe();
        assert!(long.chars().count() < 500 && long.ends_with('…'), "{}", long.len());
    }

    #[test]
    fn a_describe_line_hides_secrets_in_urls() {
        assert_eq!(key("remote.o.url", "ext::x https://me:SECRET@h/").describe(), "remote.o.url = ext::x https://***@h/");
    }
}
