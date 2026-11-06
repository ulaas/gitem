//! Parsers for git's machine-readable output formats. Pure functions over text,
//! so they can be tested without spawning git.
use crate::model::*;

/// Field separator inside a record and record terminator for `git log` formats.
pub const LOG_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%D%x00%s%x00%b%x1e";

pub fn parse_log(out: &str) -> Vec<Commit> {
    let mut v = Vec::new();
    for rec in out.split('\x1e') {
        let rec = rec.trim_start_matches(['\n', '\r']);
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.split('\0').collect();
        if f.len() < 11 {
            continue;
        }
        v.push(Commit {
            sha: f[0].to_string(),
            parents: f[1].split_whitespace().map(String::from).collect(),
            author_name: f[2].to_string(),
            author_email: f[3].to_string(),
            author_time: f[4].parse().unwrap_or(0),
            committer_name: f[5].to_string(),
            committer_email: f[6].to_string(),
            commit_time: f[7].parse().unwrap_or(0),
            refs: parse_decoration(f[8]),
            subject: f[9].to_string(),
            body: f[10].trim_end().to_string(),
        });
    }
    v
}

/// Parse `%D`: `HEAD -> main, origin/main, tag: v1, refs/stash`.
pub fn parse_decoration(d: &str) -> Vec<RefLabel> {
    let mut v = Vec::new();
    for part in d.split(", ").filter(|p| !p.is_empty()) {
        if let Some(b) = part.strip_prefix("HEAD -> ") {
            v.push(RefLabel { kind: RefKind::Branch, name: b.to_string(), current: true });
        } else if part == "HEAD" {
            v.push(RefLabel { kind: RefKind::Head, name: "HEAD".into(), current: true });
        } else if let Some(t) = part.strip_prefix("tag: ") {
            v.push(RefLabel { kind: RefKind::Tag, name: t.to_string(), current: false });
        } else if part == "refs/stash" {
            v.push(RefLabel { kind: RefKind::Stash, name: "stash".into(), current: false });
        } else if part.contains('/') && !part.starts_with("refs/heads/") {
            // Slash-named local branches are disambiguated by `mark_remote_refs`.
            v.push(RefLabel { kind: RefKind::Remote, name: part.to_string(), current: false });
        } else {
            v.push(RefLabel { kind: RefKind::Branch, name: part.to_string(), current: false });
        }
    }
    v
}

/// `%D` cannot distinguish `feature/x` from `origin/x`. Fix labels using the
/// known remote names: anything not under `<remote>/` is a local branch.
pub fn mark_remote_refs(commits: &mut [Commit], remotes: &[String]) {
    for c in commits {
        for r in &mut c.refs {
            if r.kind == RefKind::Remote && !remotes.iter().any(|m| r.name.starts_with(&format!("{m}/"))) {
                r.kind = RefKind::Branch;
            }
        }
    }
}

pub const BRANCH_FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track)%00%(committerdate:unix)%00%(subject)%00%(authorname)%00%(worktreepath)";

pub fn parse_branches(out: &str) -> Vec<Branch> {
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 9 {
            continue;
        }
        let (name, is_remote) = if let Some(n) = f[0].strip_prefix("refs/heads/") {
            (n, false)
        } else if let Some(n) = f[0].strip_prefix("refs/remotes/") {
            (n, true)
        } else {
            continue;
        };
        if is_remote && name.ends_with("/HEAD") {
            continue;
        }
        let (ahead, behind, gone) = parse_track(f[4]);
        v.push(Branch {
            name: name.to_string(),
            sha: f[1].to_string(),
            is_current: f[2] == "*",
            is_remote,
            upstream: (!f[3].is_empty()).then(|| f[3].to_string()),
            upstream_gone: gone,
            ahead,
            behind,
            date: f[5].parse().unwrap_or(0),
            subject: f[6].to_string(),
            author: f[7].to_string(),
            worktree: (!f[8].is_empty()).then(|| f[8].to_string()),
        });
    }
    v
}

/// `[ahead 2, behind 1]`, `[gone]`, or empty.
pub fn parse_track(t: &str) -> (u32, u32, bool) {
    if t.contains("gone") {
        return (0, 0, true);
    }
    let num = |key: &str| -> u32 {
        t.find(key)
            .and_then(|i| t[i + key.len()..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
            .unwrap_or(0)
    };
    (num("ahead"), num("behind"), false)
}

pub const TAG_FORMAT: &str = "--format=%(refname:short)%00%(objectname)%00%(objecttype)%00%(creatordate:unix)%00%(subject)%00%(creator)%00%(*objectname)";

pub fn parse_tags(out: &str) -> Vec<Tag> {
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 7 {
            continue;
        }
        let annotated = f[2] == "tag";
        v.push(Tag {
            name: f[0].to_string(),
            sha: if annotated && !f[6].is_empty() { f[6].to_string() } else { f[1].to_string() },
            annotated,
            date: f[3].parse().unwrap_or(0),
            subject: f[4].to_string(),
            tagger: f[5].split(" <").next().unwrap_or("").to_string(),
        });
    }
    v
}

pub fn parse_remotes(out: &str) -> Vec<Remote> {
    let mut v: Vec<Remote> = Vec::new();
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(url), Some(kind)) = (it.next(), it.next(), it.next()) else { continue };
        let idx = match v.iter().position(|r| r.name == name) {
            Some(i) => i,
            None => {
                v.push(Remote { name: name.to_string(), ..Default::default() });
                v.len() - 1
            }
        };
        if kind.contains("fetch") {
            v[idx].fetch_url = url.to_string();
        } else {
            v[idx].push_url = url.to_string();
        }
    }
    v
}

pub const STASH_FORMAT: &str = "--format=%gd%x00%H%x00%gs%x00%ct";

pub fn parse_stashes(out: &str) -> Vec<Stash> {
    let mut v = Vec::new();
    for (i, line) in out.lines().enumerate() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 4 {
            continue;
        }
        // "On main: message" or "WIP on main: abc123 subject"
        let msg = f[2];
        let (branch, message) = match msg.split_once(": ") {
            Some((head, rest)) => {
                let b = head.strip_prefix("WIP on ").or_else(|| head.strip_prefix("On ")).unwrap_or(head);
                (b.to_string(), rest.to_string())
            }
            None => (String::new(), msg.to_string()),
        };
        v.push(Stash { index: i, selector: f[0].to_string(), sha: f[1].to_string(), message, branch, date: f[3].parse().unwrap_or(0) });
    }
    v
}

pub fn parse_worktrees(out: &str, current_root: &str) -> Vec<Worktree> {
    let mut v = Vec::new();
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let cur = norm(current_root);
    for (i, block) in out.split("\n\n").enumerate() {
        let mut w = Worktree { is_main: i == 0, ..Default::default() };
        for line in block.lines() {
            let (k, val) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "worktree" => w.path = val.to_string(),
                "HEAD" => w.head = val.to_string(),
                "branch" => w.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string()),
                "detached" => w.detached = true,
                "bare" => w.bare = true,
                "locked" => w.locked = Some(val.to_string()),
                "prunable" => w.prunable = Some(val.to_string()),
                _ => {}
            }
        }
        if !w.path.is_empty() {
            w.is_current = norm(&w.path) == cur;
            v.push(w);
        }
    }
    v
}

pub fn parse_submodules(status: &str) -> Vec<Submodule> {
    let mut v = Vec::new();
    for line in status.lines() {
        if line.len() < 2 {
            continue;
        }
        let state = line.chars().next().unwrap_or(' ');
        let rest = line[state.len_utf8()..].trim_start();
        let mut it = rest.splitn(2, ' ');
        let sha = it.next().unwrap_or("").to_string();
        let tail = it.next().unwrap_or("");
        let (path, describe) = match tail.rfind(" (") {
            Some(i) if tail.ends_with(')') => (&tail[..i], &tail[i + 2..tail.len() - 1]),
            _ => (tail, ""),
        };
        v.push(Submodule { path: path.to_string(), sha, describe: describe.to_string(), state, url: String::new() });
    }
    v
}

/// Parse `git status --porcelain=v2 --branch -z`.
pub fn parse_status(out: &str) -> RepoStatus {
    let mut st = RepoStatus::default();
    let mut it = out.split('\0');
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        let code = rec.as_bytes()[0];
        match code {
            b'#' => {
                let mut p = rec.split_whitespace();
                p.next();
                match p.next() {
                    Some("branch.oid") => st.head_sha = p.next().unwrap_or("").to_string(),
                    Some("branch.head") => {
                        let h = p.next().unwrap_or("");
                        if h != "(detached)" {
                            st.branch = Some(h.to_string());
                        }
                    }
                    Some("branch.upstream") => st.upstream = p.next().map(String::from),
                    Some("branch.ab") => {
                        st.ahead = p.next().and_then(|x| x.trim_start_matches('+').parse().ok()).unwrap_or(0);
                        st.behind = p.next().and_then(|x| x.trim_start_matches('-').parse().ok()).unwrap_or(0);
                    }
                    _ => {}
                }
            }
            b'1' => {
                // 1 XY sub mH mI mW hH hI path
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    let mut xy = f[1].chars();
                    st.entries.push(StatusEntry {
                        path: f[8].to_string(),
                        orig_path: None,
                        index: FileStatus::from_code(xy.next().unwrap_or('.')),
                        worktree: FileStatus::from_code(xy.next().unwrap_or('.')),
                        conflict: None,
                        submodule: f[2].starts_with('S'),
                    });
                }
            }
            b'2' => {
                // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                if f.len() == 10 {
                    let mut xy = f[1].chars();
                    let orig = it.next().map(String::from);
                    st.entries.push(StatusEntry {
                        path: f[9].to_string(),
                        orig_path: orig,
                        index: FileStatus::from_code(xy.next().unwrap_or('.')),
                        worktree: FileStatus::from_code(xy.next().unwrap_or('.')),
                        conflict: None,
                        submodule: f[2].starts_with('S'),
                    });
                }
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    st.entries.push(StatusEntry {
                        path: f[10].to_string(),
                        orig_path: None,
                        index: FileStatus::Unmerged,
                        worktree: FileStatus::Unmerged,
                        conflict: Some(f[1].to_string()),
                        submodule: f[2].starts_with('S'),
                    });
                }
            }
            b'?' => st.entries.push(StatusEntry {
                path: rec.get(2..).unwrap_or("").to_string(),
                orig_path: None,
                index: FileStatus::Unmodified,
                worktree: FileStatus::Untracked,
                conflict: None,
                submodule: false,
            }),
            _ => {}
        }
    }
    st
}

/// Parse `git blame --line-porcelain`.
pub fn parse_blame(out: &str) -> Vec<BlameLine> {
    let mut v = Vec::new();
    let mut cur = BlameLine::default();
    let mut expecting_header = true;
    for line in out.split('\n') {
        if expecting_header {
            if line.is_empty() {
                continue;
            }
            let mut p = line.split(' ');
            cur = BlameLine {
                sha: p.next().unwrap_or("").trim_start_matches('^').to_string(),
                orig_line: p.next().and_then(|x| x.parse().ok()).unwrap_or(0),
                final_line: p.next().and_then(|x| x.parse().ok()).unwrap_or(0),
                ..Default::default()
            };
            expecting_header = false;
        } else if let Some(t) = line.strip_prefix('\t') {
            cur.text = t.strip_suffix('\r').unwrap_or(t).to_string();
            v.push(std::mem::take(&mut cur));
            expecting_header = true;
        } else if let Some(x) = line.strip_prefix("author ") {
            cur.author = x.to_string();
        } else if let Some(x) = line.strip_prefix("author-mail ") {
            cur.author_email = x.trim_matches(['<', '>']).to_string();
        } else if let Some(x) = line.strip_prefix("author-time ") {
            cur.author_time = x.parse().unwrap_or(0);
        } else if let Some(x) = line.strip_prefix("summary ") {
            cur.summary = x.to_string();
        } else if let Some(x) = line.strip_prefix("filename ") {
            cur.orig_path = Some(x.to_string());
        } else if line == "boundary" {
            cur.boundary = true;
        }
    }
    v
}

/// Parse `git diff/show --name-status -z`, combined with `--numstat -z` data.
pub fn parse_name_status(out: &str) -> Vec<FileChange> {
    let mut v = Vec::new();
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let c = code.chars().next().unwrap_or(' ');
        let status = FileStatus::from_code(c);
        if matches!(c, 'R' | 'C') {
            let orig = it.next().unwrap_or("").to_string();
            let path = it.next().unwrap_or("").to_string();
            v.push(FileChange { path, orig_path: Some(orig), status, additions: None, deletions: None });
        } else {
            let path = it.next().unwrap_or("").to_string();
            v.push(FileChange { path, orig_path: None, status, additions: None, deletions: None });
        }
    }
    v
}

/// Parse `--numstat -z` into (path, additions, deletions); binary files give `None`.
pub fn parse_numstat(out: &str) -> Vec<(String, Option<u32>, Option<u32>)> {
    let mut v = Vec::new();
    let mut it = out.split('\0');
    while let Some(rec) = it.next() {
        let rec = rec.trim_start_matches('\n');
        if rec.is_empty() {
            continue;
        }
        let mut p = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) else { continue };
        let path = if path.is_empty() {
            // Rename: "add\tdel\t\0old\0new\0"
            let _old = it.next();
            it.next().unwrap_or("").to_string()
        } else {
            path.to_string()
        };
        v.push((path, a.parse().ok(), d.parse().ok()));
    }
    v
}

pub const REFLOG_FORMAT: &str = "--format=%gd%x00%H%x00%gs%x00%ct%x00%gn";

pub fn parse_reflog(out: &str) -> Vec<ReflogEntry> {
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 5 {
            continue;
        }
        let (action, message) = f[2].split_once(": ").unwrap_or((f[2], ""));
        v.push(ReflogEntry {
            selector: f[0].to_string(),
            sha: f[1].to_string(),
            action: action.to_string(),
            message: message.to_string(),
            date: f[3].parse().unwrap_or(0),
            author: f[4].to_string(),
        });
    }
    v
}

/// `git config --list --show-scope --show-origin -z`: `scope\0origin\0key\nvalue\0`.
pub fn parse_config(out: &str) -> Vec<ConfigEntry> {
    let mut v = Vec::new();
    let mut it = out.split('\0');
    while let Some(scope) = it.next() {
        if scope.is_empty() {
            continue;
        }
        let (Some(origin), Some(kv)) = (it.next(), it.next()) else { break };
        let (key, value) = kv.split_once('\n').unwrap_or((kv, ""));
        v.push(ConfigEntry { scope: scope.to_string(), origin: origin.to_string(), key: key.to_string(), value: value.to_string() });
    }
    v
}

/// Commit stats stream for file history and contributors:
/// `git log --format=<sep>%H%x00... --numstat`.
pub const STAT_LOG_FORMAT: &str = "--format=\x01%H%x00%an%x00%ae%x00%at%x00%s";

pub struct StatCommit {
    pub sha: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    pub additions: u32,
    pub deletions: u32,
    pub path: Option<String>,
}

pub fn parse_stat_log(out: &str) -> Vec<StatCommit> {
    let mut v: Vec<StatCommit> = Vec::new();
    for line in out.lines() {
        if let Some(rec) = line.strip_prefix('\x01') {
            let f: Vec<&str> = rec.split('\0').collect();
            if f.len() >= 5 {
                v.push(StatCommit {
                    sha: f[0].to_string(),
                    author: f[1].to_string(),
                    email: f[2].to_string(),
                    time: f[3].parse().unwrap_or(0),
                    subject: f[4].to_string(),
                    additions: 0,
                    deletions: 0,
                    path: None,
                });
            }
        } else if let Some(c) = v.last_mut() {
            let mut p = line.splitn(3, '\t');
            if let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) {
                c.additions += a.parse().unwrap_or(0);
                c.deletions += d.parse().unwrap_or(0);
                if c.path.is_none() {
                    c.path = Some(path.to_string());
                }
            }
        }
    }
    v
}

/// `git log --format=<0x01>%H --numstat`: sha -> (added, removed).
pub fn parse_commit_stats(out: &str) -> std::collections::HashMap<String, (u32, u32)> {
    let mut m = std::collections::HashMap::new();
    let mut cur: Option<String> = None;
    for line in out.lines() {
        if let Some(sha) = line.strip_prefix('\x01') {
            cur = Some(sha.to_string());
            m.entry(sha.to_string()).or_insert((0, 0));
        } else if let Some(c) = &cur {
            let mut p = line.splitn(3, '\t');
            if let (Some(a), Some(d), Some(_)) = (p.next(), p.next(), p.next()) {
                let e = m.entry(c.clone()).or_insert((0, 0));
                e.0 += a.parse().unwrap_or(0);
                e.1 += d.parse().unwrap_or(0);
            }
        }
    }
    m
}

/// `git ls-tree -r -l -z`: `<mode> <type> <oid> <size>\t<path>`.
pub fn parse_ls_tree_sizes(out: &str) -> Vec<(String, u64)> {
    out.split('\0')
        .filter_map(|rec| {
            let (meta, path) = rec.split_once('\t')?;
            let size = meta.split_whitespace().nth(3)?.parse().ok()?;
            Some((path.to_string(), size))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_record_cut_short_is_not_a_panic() {
        assert_eq!(parse_status("?\0").entries[0].path, "");
        assert_eq!(parse_status("? \u{e9}\0").entries[0].path, "\u{e9}");
    }

    #[test]
    fn commit_stats_and_tree_sizes() {
        let s = parse_commit_stats("\x01abc\n\n1\t2\ta\n3\t4\tb\n\x01def\n-\t-\tbin\n");
        assert_eq!(s["abc"], (4, 6));
        assert_eq!(s["def"], (0, 0));
        let t = parse_ls_tree_sizes("100644 blob aaa     12\tsrc/a b.rs\0160000 commit bbb       -\tsub\0");
        assert_eq!(t, vec![("src/a b.rs".to_string(), 12)]);
    }

    #[test]
    fn log_roundtrip() {
        let s = "abc\0p1 p2\0Ann\0a@x\0100\0Bob\0b@x\0200\0HEAD -> main, tag: v1, origin/main\0subj\0body line\n\x1e\ndef\0\0A\0a\01\0B\0b\02\0\0s2\0\x1e";
        let c = parse_log(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].parents, vec!["p1", "p2"]);
        assert_eq!(c[0].refs.len(), 3);
        assert!(c[0].refs[0].current);
        assert_eq!(c[0].refs[1].kind, RefKind::Tag);
        assert_eq!(c[0].refs[2].kind, RefKind::Remote);
        assert_eq!(c[0].body, "body line");
        assert!(c[1].parents.is_empty());
    }

    #[test]
    fn slash_branch_not_remote() {
        let mut c = vec![Commit { refs: parse_decoration("feature/x, origin/main"), ..Default::default() }];
        mark_remote_refs(&mut c, &["origin".to_string()]);
        assert_eq!(c[0].refs[0].kind, RefKind::Branch);
        assert_eq!(c[0].refs[1].kind, RefKind::Remote);
    }

    #[test]
    fn track() {
        assert_eq!(parse_track("[ahead 2, behind 1]"), (2, 1, false));
        assert_eq!(parse_track("[behind 7]"), (0, 7, false));
        assert_eq!(parse_track("[gone]"), (0, 0, true));
        assert_eq!(parse_track(""), (0, 0, false));
    }

    #[test]
    fn status_v2() {
        let s = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 a a sp ace.txt\0\
2 R. N... 100644 100644 100644 a a R100 new.txt\0old.txt\0\
u UU N... 100644 100644 100644 100644 a b c conflict.txt\0\
? untracked.txt\0";
        let st = parse_status(s);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!((st.ahead, st.behind), (2, 1));
        assert_eq!(st.entries.len(), 4);
        assert_eq!(st.entries[0].path, "sp ace.txt");
        assert_eq!(st.entries[0].worktree, FileStatus::Modified);
        assert_eq!(st.entries[1].orig_path.as_deref(), Some("old.txt"));
        assert_eq!(st.entries[1].index, FileStatus::Renamed);
        assert_eq!(st.entries[2].conflict.as_deref(), Some("UU"));
        assert_eq!(st.entries[3].worktree, FileStatus::Untracked);
    }

    #[test]
    fn blame_porcelain() {
        let s = "aaa 1 1 2\nauthor Ann\nauthor-mail <a@x>\nauthor-time 100\nsummary first\nfilename f.txt\n\tline one\r\naaa 2 2\nauthor Ann\nauthor-mail <a@x>\nauthor-time 100\nsummary first\nfilename f.txt\n\tline two\n";
        let b = parse_blame(s);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].text, "line one");
        assert_eq!(b[1].final_line, 2);
        assert_eq!(b[1].author_email, "a@x");
    }

    #[test]
    fn name_status_rename() {
        let v = parse_name_status("M\0a.txt\0R100\0old\0new\0");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].path, "new");
        assert_eq!(v[1].orig_path.as_deref(), Some("old"));
    }

    #[test]
    fn numstat_rename_and_binary() {
        let v = parse_numstat("1\t2\ta.txt\0-\t-\tb.bin\0\n3\t4\t\0old\0new\0");
        assert_eq!(v[0], ("a.txt".to_string(), Some(1), Some(2)));
        assert_eq!(v[1].1, None);
        assert_eq!(v[2].0, "new");
    }

    #[test]
    fn worktrees() {
        let s = "worktree C:/r\nHEAD abc\nbranch refs/heads/main\n\nworktree C:/w\nHEAD def\ndetached\nlocked why\n\n";
        let w = parse_worktrees(s, "c:/R/");
        assert_eq!(w.len(), 2);
        assert!(w[0].is_main && w[0].is_current);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[1].detached);
        assert_eq!(w[1].locked.as_deref(), Some("why"));
    }

    #[test]
    fn stash_msg() {
        let s = parse_stashes("stash@{0}\0abc\0WIP on main: 123 subj\0100\nstash@{1}\0def\0On dev: mine\0200\n");
        assert_eq!(s[0].branch, "main");
        assert_eq!(s[1].message, "mine");
        assert_eq!(s[1].index, 1);
    }

    #[test]
    fn config_z() {
        let c = parse_config("global\0file:C:/x\0user.name\nAnn\0local\0file:.git/config\0core.bare\nfalse\0");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].value, "Ann");
        assert_eq!(c[1].scope, "local");
    }

    #[test]
    fn submodule_status() {
        let s = parse_submodules(" abc lib/a (v1.0)\n-def lib/b\n+123 lib c (heads/main)\n");
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].path, "lib/a");
        assert_eq!(s[1].state, '-');
        assert_eq!(s[2].path, "lib c");
        assert_eq!(s[2].describe, "heads/main");
    }
}
