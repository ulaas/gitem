//! What the Pull Request view reads and posts beyond a list's card: changed files with their
//! diffs, each check, the conversation and inline comments, a merge by a chosen method, and a
//! review verdict. The field names come from each host's documentation as remembered, and are
//! only tested against hand-written replies: see `tests/http_clients.rs`.
use serde_json::{Value, json};

use super::*;

/// How a pull request's commits land on its base branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStyle {
    Merge,
    Squash,
    Rebase,
}

impl MergeStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merge commit",
            Self::Squash => "Squash into one commit",
            Self::Rebase => "Rebase onto the base branch",
        }
    }
}

impl ForgeKind {
    /// What can be asked for by name. Anything else is whatever the host is set up to do.
    pub fn merge_styles(self) -> &'static [MergeStyle] {
        match self {
            Self::GitHub | Self::Gitea | Self::AzureDevOps => &[MergeStyle::Merge, MergeStyle::Squash, MergeStyle::Rebase],
            Self::GitLab | Self::Bitbucket => &[MergeStyle::Merge, MergeStyle::Squash],
            Self::BitbucketServer | Self::Gerrit | Self::GoogleSource => &[],
        }
    }

    /// Gitea's API, as far as is known here, takes a new comment on a line but no reply to one.
    pub fn can_reply_in_threads(self) -> bool {
        matches!(self, Self::GitHub | Self::GitLab | Self::Bitbucket | Self::BitbucketServer | Self::AzureDevOps)
    }

    /// GitLab has approvals and comments, but no verdict that blocks a merge request by name.
    pub fn can_request_changes(self) -> bool {
        matches!(self, Self::GitHub | Self::Gitea | Self::Bitbucket | Self::BitbucketServer | Self::AzureDevOps)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrFile {
    pub path: String,
    /// Where it was before, for a rename.
    pub old_path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    /// Its hunks, where the host gives them. Empty for a binary file or a very large change.
    pub patch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    Running,
    /// Skipped, neutral, or a word this doesn't know.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    /// The host's own word for it, or a line about it.
    pub detail: String,
    /// Where its log is. Logs stay on the host: every host keeps them differently.
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrComment {
    pub author: String,
    pub body: String,
    pub when: i64,
    /// Where it's pinned, for an inline comment.
    pub path: Option<String>,
    pub line: Option<u32>,
    /// `APPROVED` and the like, for a review.
    pub verdict: String,
    /// The host's name for this comment, where something can be done with it.
    pub id: String,
    /// Which thread it's part of. Empty for one that stands alone and can't be answered, such as
    /// a comment on a GitHub pull request as a whole.
    pub thread: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrDetails {
    pub head_sha: String,
    pub base_sha: String,
    /// Where the diff starts, which an inline comment on GitLab has to name.
    pub start_sha: String,
    pub files: Vec<PrFile>,
    pub checks: Vec<PrCheck>,
    pub comments: Vec<PrComment>,
    /// What couldn't be read, with what could shown beside it.
    pub problems: Vec<String>,
}

impl PrDetails {
    /// The conversation as it reads: each comment on its own, or a thread with its first comment
    /// where that started and its replies after it, oldest first. Assumes `comments` is in order.
    pub fn threads(&self) -> Vec<Vec<&PrComment>> {
        let mut out: Vec<Vec<&PrComment>> = Vec::new();
        for c in &self.comments {
            match out.iter_mut().find(|g| !c.thread.is_empty() && g[0].thread == c.thread) {
                Some(g) => g.push(c),
                None => out.push(vec![c]),
            }
        }
        out
    }
}

/// One line of a file's hunks, numbered as the two sides have it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchLineKind {
    Hunk,
    Context,
    Added,
    Removed,
}

/// Numbers from each `@@ -a,b +c,d @@`. Lines before the first hunk are a file's header, or
/// nothing, and are left out.
pub fn patch_lines(patch: &str) -> Vec<PatchLine> {
    let (mut out, mut old, mut new, mut started) = (Vec::new(), 0u32, 0u32, false);
    for l in patch.lines() {
        if let Some(rest) = l.strip_prefix("@@ -") {
            let start = |s: &str| s.split(',').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(1);
            let mut parts = rest.split(' ');
            old = start(parts.next().unwrap_or(""));
            new = start(parts.next().unwrap_or("").trim_start_matches('+'));
            started = true;
            out.push(PatchLine { kind: PatchLineKind::Hunk, old: None, new: None, text: l.to_string() });
        } else if !started || l.starts_with('\\') {
            continue;
        } else if let Some(t) = l.strip_prefix('+') {
            out.push(PatchLine { kind: PatchLineKind::Added, old: None, new: Some(new), text: t.to_string() });
            new += 1;
        } else if let Some(t) = l.strip_prefix('-') {
            out.push(PatchLine { kind: PatchLineKind::Removed, old: Some(old), new: None, text: t.to_string() });
            old += 1;
        } else {
            out.push(PatchLine { kind: PatchLineKind::Context, old: Some(old), new: Some(new), text: l.strip_prefix(' ').unwrap_or(l).to_string() });
            old += 1;
            new += 1;
        }
    }
    out
}

/// Each file's part of a whole diff, by the name after `+++` or else `diff --git`.
pub fn split_diff(diff: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for chunk in diff.split("diff --git ").skip(1) {
        let first = chunk.lines().next().unwrap_or("");
        let plus = chunk.lines().find_map(|l| l.strip_prefix("+++ ")).filter(|n| *n != "/dev/null").and_then(|n| n.strip_prefix("b/").or(Some(n)));
        let minus = chunk.lines().find_map(|l| l.strip_prefix("--- ")).filter(|n| *n != "/dev/null").and_then(|n| n.strip_prefix("a/").or(Some(n)));
        // `a/x b/x`, for when neither of those is there: an empty file or a pure rename.
        let named = first.rsplit_once(" b/").map(|x| x.1);
        let Some(path) = plus.or(minus).or(named) else { continue };
        out.push((path.trim().to_string(), chunk.split_once('\n').map_or("", |x| x.1).to_string()));
    }
    out
}

fn state_of(word: &str) -> CheckState {
    match word.to_ascii_lowercase().as_str() {
        "success" | "successful" | "passed" | "succeeded" | "completed_success" => CheckState::Passed,
        "failure" | "failed" | "error" | "errored" | "timed_out" | "cancelled" | "canceled" | "action_required" | "stopped" => CheckState::Failed,
        "pending" | "running" | "in_progress" | "queued" | "waiting" | "created" | "inprogress" | "notstarted" | "preparing" | "scheduled" => CheckState::Running,
        _ => CheckState::Other,
    }
}

fn num(v: Option<&Value>) -> u32 {
    v.and_then(Value::as_u64).unwrap_or(0) as u32
}

/// A host's name for something, which may be a number or a string.
fn id_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(x)) => x.clone(),
        _ => String::new(),
    }
}

/// Bitbucket Server hangs a comment's replies off the comment, to any depth. All go in one thread,
/// on the first comment's line.
fn server_comments(c: &Value, thread: &str, path: Option<&str>, line: Option<u32>, into: &mut Vec<PrComment>) {
    let id = id_of(c.get("id"));
    let thread = if thread.is_empty() { id.as_str() } else { thread };
    into.push(PrComment { author: s(c.pointer("/author/name")), body: s(c.get("text")), when: c.get("createdDate").and_then(Value::as_i64).unwrap_or(0) / 1000, path: path.map(String::from), line, verdict: String::new(), id: id.clone(), thread: thread.to_string() });
    for reply in c.get("comments").and_then(Value::as_array).into_iter().flatten() {
        server_comments(reply, thread, path, line, into);
    }
}

fn line_of(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64).map(|n| n as u32)
}

/// Keeps what's left readable when one part of the reply can't be had.
struct Gather {
    problems: Vec<String>,
}

impl Gather {
    fn take<T: Default>(&mut self, what: &str, r: Result<T>) -> T {
        r.unwrap_or_else(|e| {
            self.problems.push(format!("{what}: {e}"));
            T::default()
        })
    }
}

impl ForgeClient {
    /// A pull request's diff in full, for the hosts whose lists of files don't have it. Not JSON,
    /// which is the one reason this isn't `request`.
    fn get_text(&self, url: &str) -> Result<String> {
        let auth = match self.repo.kind {
            ForgeKind::Gitea => format!("token {}", self.token),
            ForgeKind::Bitbucket | ForgeKind::AzureDevOps => format!("Basic {}", base64(&format!("{}:{}", self.user, self.token))),
            _ => format!("Bearer {}", self.token),
        };
        let mut resp = agent().get(url).header("authorization", &auth).call().map_err(|e| Error::Other(format!("{url}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| Error::Other(format!("reading the reply from {url}: {e}")))?;
        if status >= 400 {
            return Err(Error::Other(format!("HTTP {status}: {}", text.chars().take(300).collect::<String>())));
        }
        Ok(text)
    }

    /// Everything the view shows about one pull request. Only a failure to read the pull request
    /// itself is an error: a host that won't list its checks still has files to show.
    pub fn pr_details(&self, n: u64) -> Result<PrDetails> {
        self.needs_api()?;
        let mut g = Gather { problems: Vec::new() };
        let mut d = match self.repo.kind {
            ForgeKind::GitHub => self.github_details(n, &mut g)?,
            ForgeKind::Gitea => self.gitea_details(n, &mut g)?,
            ForgeKind::GitLab => self.gitlab_details(n, &mut g)?,
            ForgeKind::Bitbucket => self.bitbucket_details(n, &mut g)?,
            ForgeKind::BitbucketServer => self.server_details(n, &mut g)?,
            ForgeKind::AzureDevOps => self.azure_details(n, &mut g)?,
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
        };
        d.problems = g.problems;
        d.comments.sort_by_key(|c| c.when);
        Ok(d)
    }

    fn paged_get(&self, url: impl Fn(usize) -> String, pointer: &str, what: &str) -> Result<Vec<Value>> {
        paged(|page| {
            let v = self.request(Method::Get, &url(page))?;
            let items = list_in(&v, pointer, what)?;
            let more = items.len() == PER_PAGE;
            Ok((items, more))
        })
    }

    fn github_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (base, path) = (self.api_base(), self.repo.path.clone());
        let pr = self.get(&format!("/repos/{path}/pulls/{n}"))?;
        let (head_sha, base_sha) = (s(pr.pointer("/head/sha")), s(pr.pointer("/base/sha")));
        let files = self.paged_get(|p| format!("{base}/repos/{path}/pulls/{n}/files?per_page={PER_PAGE}&page={p}"), "", "Files");
        let files = g.take("Files", files).iter().map(|f| PrFile { path: s(f.get("filename")), old_path: s(f.get("previous_filename")), status: s(f.get("status")), additions: num(f.get("additions")), deletions: num(f.get("deletions")), patch: s(f.get("patch")) }).collect();
        let mut checks = Vec::new();
        let runs = self.get(&format!("/repos/{path}/commits/{head_sha}/check-runs?per_page=100")).and_then(|v| list_in(&v, "/check_runs", "Checks"));
        for c in g.take("Checks", runs) {
            let (status, conclusion) = (s(c.get("status")), s(c.get("conclusion")));
            let word = if conclusion.is_empty() { status } else { conclusion };
            checks.push(PrCheck { name: s(c.get("name")), state: state_of(&word), detail: word, url: s(c.get("html_url")) });
        }
        let statuses = self.get(&format!("/repos/{path}/commits/{head_sha}/status")).and_then(|v| list_in(&v, "/statuses", "Commit statuses"));
        for c in g.take("Commit statuses", statuses) {
            checks.push(PrCheck { name: s(c.get("context")), state: state_of(&s(c.get("state"))), detail: s(c.get("description")), url: s(c.get("target_url")) });
        }
        let mut comments = Vec::new();
        let said = self.paged_get(|p| format!("{base}/repos/{path}/issues/{n}/comments?per_page={PER_PAGE}&page={p}"), "", "Comments");
        for c in g.take("Comments", said) {
            comments.push(PrComment { author: s(c.pointer("/user/login")), body: s(c.get("body")), when: ts(c.get("created_at")), ..Default::default() });
        }
        let inline = self.paged_get(|p| format!("{base}/repos/{path}/pulls/{n}/comments?per_page={PER_PAGE}&page={p}"), "", "Inline comments");
        for c in g.take("Inline comments", inline) {
            let id = id_of(c.get("id"));
            // A reply says which comment started it, and GitHub only takes replies to that one.
            let thread = Some(id_of(c.get("in_reply_to_id"))).filter(|t| !t.is_empty()).unwrap_or_else(|| id.clone());
            comments.push(PrComment { author: s(c.pointer("/user/login")), body: s(c.get("body")), when: ts(c.get("created_at")), path: Some(s(c.get("path"))), line: line_of(c.get("line")).or(line_of(c.get("original_line"))), verdict: String::new(), id, thread });
        }
        let reviews = self.paged_get(|p| format!("{base}/repos/{path}/pulls/{n}/reviews?per_page={PER_PAGE}&page={p}"), "", "Reviews");
        for c in g.take("Reviews", reviews) {
            comments.push(PrComment { author: s(c.pointer("/user/login")), body: s(c.get("body")), when: ts(c.get("submitted_at")), verdict: s(c.get("state")), ..Default::default() });
        }
        Ok(PrDetails { head_sha, base_sha, files, checks, comments, ..Default::default() })
    }

    fn gitea_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (base, path) = (self.api_base(), self.repo.path.clone());
        let pr = self.get(&format!("/repos/{path}/pulls/{n}"))?;
        let (head_sha, base_sha) = (s(pr.pointer("/head/sha")), s(pr.pointer("/base/sha")));
        let diff = g.take("The diff", self.get_text(&format!("{base}/repos/{path}/pulls/{n}.diff")));
        let parts = split_diff(&diff);
        let listed = self.paged_get(|p| format!("{base}/repos/{path}/pulls/{n}/files?limit={PER_PAGE}&page={p}"), "", "Files");
        let files = g
            .take("Files", listed)
            .iter()
            .map(|f| {
                let path = s(f.get("filename"));
                let patch = parts.iter().find(|(p, _)| *p == path).map(|x| x.1.clone()).unwrap_or_default();
                PrFile { path, old_path: s(f.get("previous_filename")), status: s(f.get("status")), additions: num(f.get("additions")), deletions: num(f.get("deletions")), patch }
            })
            .collect();
        let statuses = self.get(&format!("/repos/{path}/commits/{head_sha}/statuses")).and_then(|v| list_in(&v, "", "Checks"));
        let checks = g.take("Checks", statuses).iter().map(|c| PrCheck { name: s(c.get("context")), state: state_of(&s(c.get("status"))), detail: s(c.get("description")), url: s(c.get("target_url")) }).collect();
        let mut comments = Vec::new();
        let said = self.paged_get(|p| format!("{base}/repos/{path}/issues/{n}/comments?limit={PER_PAGE}&page={p}"), "", "Comments");
        for c in g.take("Comments", said) {
            comments.push(PrComment { author: s(c.pointer("/user/login")), body: s(c.get("body")), when: ts(c.get("created_at")), ..Default::default() });
        }
        let reviews = self.paged_get(|p| format!("{base}/repos/{path}/pulls/{n}/reviews?limit={PER_PAGE}&page={p}"), "", "Reviews");
        for r in g.take("Reviews", reviews) {
            comments.push(PrComment { author: s(r.pointer("/user/login")), body: s(r.get("body")), when: ts(r.get("submitted_at")), verdict: s(r.get("state")), ..Default::default() });
            let id = r.get("id").and_then(Value::as_u64).unwrap_or(0);
            if r.get("comments_count").and_then(Value::as_u64).unwrap_or(0) > 0 {
                let inner = self.get(&format!("/repos/{path}/pulls/{n}/reviews/{id}/comments")).and_then(|v| list_in(&v, "", "Inline comments"));
                for c in g.take("Inline comments", inner) {
                    let id = id_of(c.get("id"));
                    comments.push(PrComment { author: s(c.pointer("/user/login")), body: s(c.get("body")), when: ts(c.get("created_at")), path: Some(s(c.get("path"))), line: line_of(c.get("position")), verdict: String::new(), thread: id.clone(), id });
                }
            }
        }
        Ok(PrDetails { head_sha, base_sha, files, checks, comments, ..Default::default() })
    }

    fn gitlab_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (base, id) = (self.api_base(), url_encode(&self.repo.path));
        let mr = self.get(&format!("/projects/{id}/merge_requests/{n}/changes"))?;
        let files = list_in(&mr, "/changes", "Files");
        let files = g
            .take("Files", files)
            .iter()
            .map(|f| {
                let status = if f.get("new_file").and_then(Value::as_bool) == Some(true) {
                    "added"
                } else if f.get("deleted_file").and_then(Value::as_bool) == Some(true) {
                    "removed"
                } else if f.get("renamed_file").and_then(Value::as_bool) == Some(true) {
                    "renamed"
                } else {
                    "modified"
                };
                let patch = s(f.get("diff"));
                let lines = patch_lines(&patch);
                PrFile { path: s(f.get("new_path")), old_path: s(f.get("old_path")), status: status.to_string(), additions: lines.iter().filter(|l| l.kind == PatchLineKind::Added).count() as u32, deletions: lines.iter().filter(|l| l.kind == PatchLineKind::Removed).count() as u32, patch }
            })
            .collect();
        let mut checks = Vec::new();
        let pipelines = self.get(&format!("/projects/{id}/merge_requests/{n}/pipelines")).and_then(|v| list_in(&v, "", "Pipelines"));
        // Newest first, and only that one says what's true now.
        if let Some(latest) = g.take("Pipelines", pipelines).first() {
            let pid = latest.get("id").and_then(Value::as_u64).unwrap_or(0);
            let jobs = self.get(&format!("/projects/{id}/pipelines/{pid}/jobs?per_page=100")).and_then(|v| list_in(&v, "", "Jobs"));
            for j in g.take("Jobs", jobs) {
                let word = s(j.get("status"));
                checks.push(PrCheck { name: s(j.get("name")), state: state_of(&word), detail: word, url: s(j.get("web_url")) });
            }
        }
        // Discussions rather than the plain list of notes, which doesn't say which thread each is in.
        let talks = self.paged_get(|p| format!("{base}/projects/{id}/merge_requests/{n}/discussions?per_page={PER_PAGE}&page={p}"), "", "Comments");
        let mut comments = Vec::new();
        for t in g.take("Comments", talks) {
            let thread = if t.get("individual_note").and_then(Value::as_bool) == Some(true) { String::new() } else { s(t.get("id")) };
            for c in t.get("notes").and_then(Value::as_array).into_iter().flatten().filter(|c| c.get("system").and_then(Value::as_bool) != Some(true)) {
                comments.push(PrComment { author: s(c.pointer("/author/username")), body: s(c.get("body")), when: ts(c.get("created_at")), path: c.pointer("/position/new_path").and_then(Value::as_str).map(String::from), line: line_of(c.pointer("/position/new_line")), verdict: String::new(), id: id_of(c.get("id")), thread: thread.clone() });
            }
        }
        Ok(PrDetails { head_sha: s(mr.pointer("/diff_refs/head_sha")), base_sha: s(mr.pointer("/diff_refs/base_sha")), start_sha: s(mr.pointer("/diff_refs/start_sha")), files, checks, comments, problems: Vec::new() })
    }

    fn bitbucket_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (base, path) = (self.api_base(), self.repo.path.clone());
        let pr = self.get(&format!("/repositories/{path}/pullrequests/{n}"))?;
        let diff = g.take("The diff", self.get_text(&format!("{base}/repositories/{path}/pullrequests/{n}/diff")));
        let parts = split_diff(&diff);
        let page_of = |what: &'static str, tail: String| {
            paged(|page| {
                let v = self.get(&format!("/repositories/{path}/pullrequests/{n}/{tail}?pagelen={PER_PAGE}&page={page}"))?;
                let more = v.get("next").is_some_and(|x| !x.is_null());
                Ok((list_in(&v, "/values", what)?, more))
            })
        };
        let stat = page_of("Files", "diffstat".to_string());
        let files = g
            .take("Files", stat)
            .iter()
            .map(|f| {
                let path = s(f.pointer("/new/path")).trim().to_string();
                let path = if path.is_empty() { s(f.pointer("/old/path")) } else { path };
                let patch = parts.iter().find(|(p, _)| *p == path).map(|x| x.1.clone()).unwrap_or_default();
                PrFile { path, old_path: s(f.pointer("/old/path")), status: s(f.get("status")), additions: num(f.get("lines_added")), deletions: num(f.get("lines_removed")), patch }
            })
            .collect();
        let statuses = page_of("Checks", "statuses".to_string());
        let checks = g.take("Checks", statuses).iter().map(|c| PrCheck { name: s(c.get("name")), state: state_of(&s(c.get("state"))), detail: s(c.get("description")), url: s(c.get("url")) }).collect();
        let said = page_of("Comments", "comments".to_string());
        let said = g.take("Comments", said);
        // Each says only which it answers, so a thread is named after the one at the top.
        let parent_of: Vec<(String, String)> = said.iter().map(|c| (id_of(c.get("id")), id_of(c.pointer("/parent/id")))).collect();
        let top_of = |id: String| {
            let mut at = id;
            for _ in 0..parent_of.len() {
                match parent_of.iter().find(|(i, up)| *i == at && !up.is_empty()) {
                    Some((_, up)) => at = up.clone(),
                    None => break,
                }
            }
            at
        };
        let comments = said
            .iter()
            .map(|c| {
                let id = id_of(c.get("id"));
                PrComment { author: s(c.pointer("/user/display_name")), body: s(c.pointer("/content/raw")), when: ts(c.get("created_on")), path: c.pointer("/inline/path").and_then(Value::as_str).map(String::from), line: line_of(c.pointer("/inline/to")), verdict: String::new(), thread: top_of(id.clone()), id }
            })
            .collect();
        Ok(PrDetails { head_sha: s(pr.pointer("/source/commit/hash")), base_sha: s(pr.pointer("/destination/commit/hash")), files, checks, comments, ..Default::default() })
    }

    fn server_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (b, p, r) = self.server_api();
        let at = format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}");
        let pr = self.request(Method::Get, &at)?;
        let head_sha = s(pr.pointer("/fromRef/latestCommit"));
        let diff = g.take("The diff", self.get_text(&format!("{at}.diff")));
        let parts = split_diff(&diff);
        let changes = self.request(Method::Get, &format!("{at}/changes?limit=1000")).and_then(|v| list_in(&v, "/values", "Files"));
        let files = g
            .take("Files", changes)
            .iter()
            .map(|f| {
                let path = s(f.pointer("/path/toString"));
                let patch = parts.iter().find(|(x, _)| *x == path).map(|x| x.1.clone()).unwrap_or_default();
                let lines = patch_lines(&patch);
                PrFile { path, old_path: s(f.pointer("/srcPath/toString")), status: s(f.get("type")).to_lowercase(), additions: lines.iter().filter(|l| l.kind == PatchLineKind::Added).count() as u32, deletions: lines.iter().filter(|l| l.kind == PatchLineKind::Removed).count() as u32, patch }
            })
            .collect();
        let root = self.repo.root();
        let builds = self.request(Method::Get, &format!("{root}/rest/build-status/1.0/commits/{head_sha}")).and_then(|v| list_in(&v, "/values", "Checks"));
        let checks = g.take("Checks", builds).iter().map(|c| PrCheck { name: s(c.get("name")), state: state_of(&s(c.get("state"))), detail: s(c.get("description")), url: s(c.get("url")) }).collect();
        let acts = self.request(Method::Get, &format!("{at}/activities?limit=200")).and_then(|v| list_in(&v, "/values", "Comments"));
        let mut comments = Vec::new();
        for a in g.take("Comments", acts) {
            match s(a.get("action")).as_str() {
                "COMMENTED" => {
                    if let Some(c) = a.get("comment") {
                        server_comments(c, "", a.pointer("/commentAnchor/path").and_then(Value::as_str), line_of(a.pointer("/commentAnchor/line")), &mut comments);
                    }
                }
                verdict @ ("APPROVED" | "REVIEWED") => comments.push(PrComment { author: s(a.pointer("/user/name")), when: a.get("createdDate").and_then(Value::as_i64).unwrap_or(0) / 1000, verdict: verdict.to_string(), ..Default::default() }),
                _ => {}
            }
        }
        Ok(PrDetails { head_sha, base_sha: s(pr.pointer("/toRef/latestCommit")), files, checks, comments, ..Default::default() })
    }

    fn azure_details(&self, n: u64, g: &mut Gather) -> Result<PrDetails> {
        let (org, proj, repo) = self.azure_parts()?;
        let at = format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}", self.azure_root());
        const V: &str = "api-version=7.0";
        let pr = self.request(Method::Get, &format!("{at}?{V}"))?;
        let mut files = Vec::new();
        let iterations = self.request(Method::Get, &format!("{at}/iterations?{V}")).and_then(|v| list_in(&v, "/value", "Files"));
        // The latest is all that matters. Azure's lists hold names only, so no diff comes with them.
        if let Some(last) = g.take("Files", iterations).last().and_then(|i| i.get("id")).and_then(Value::as_u64) {
            let changes = self.request(Method::Get, &format!("{at}/iterations/{last}/changes?{V}")).and_then(|v| list_in(&v, "/changeEntries", "Files"));
            files = g.take("Files", changes).iter().map(|f| PrFile { path: s(f.pointer("/item/path")).trim_start_matches('/').to_string(), old_path: s(f.get("originalPath")).trim_start_matches('/').to_string(), status: s(f.get("changeType")), ..Default::default() }).collect();
        }
        let statuses = self.request(Method::Get, &format!("{at}/statuses?{V}")).and_then(|v| list_in(&v, "/value", "Checks"));
        let checks = g.take("Checks", statuses).iter().map(|c| PrCheck { name: s(c.pointer("/context/name")), state: state_of(&s(c.get("state"))), detail: s(c.get("description")), url: s(c.get("targetUrl")) }).collect();
        let threads = self.request(Method::Get, &format!("{at}/threads?{V}")).and_then(|v| list_in(&v, "/value", "Comments"));
        let mut comments = Vec::new();
        for t in g.take("Comments", threads) {
            for c in t.get("comments").and_then(Value::as_array).into_iter().flatten() {
                // The system's own notes about pushes and votes.
                if s(c.get("commentType")).eq_ignore_ascii_case("system") {
                    continue;
                }
                comments.push(PrComment { author: s(c.pointer("/author/displayName")), body: s(c.get("content")), when: ts(c.get("publishedDate")), path: t.pointer("/threadContext/filePath").and_then(Value::as_str).map(|x| x.trim_start_matches('/').to_string()), line: line_of(t.pointer("/threadContext/rightFileStart/line")), verdict: String::new(), id: id_of(c.get("id")), thread: id_of(t.get("id")) });
            }
        }
        Ok(PrDetails { head_sha: s(pr.pointer("/lastMergeSourceCommit/commitId")), base_sha: s(pr.pointer("/lastMergeTargetCommit/commitId")), files, checks, comments, ..Default::default() })
    }

    /// Merge by a chosen method, or `None` for whatever the host does by default.
    pub fn merge_pr_as(&self, n: u64, style: Option<MergeStyle>) -> Result<()> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                let pr = self.request(Method::Get, &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}"))?;
                let version = pr.get("version").and_then(Value::as_u64).unwrap_or(0);
                self.request(Method::Post(json!({"version":version})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/merge")).map(|_| ())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
            ForgeKind::GitHub => {
                let body = match style {
                    None => json!({}),
                    Some(MergeStyle::Merge) => json!({"merge_method":"merge"}),
                    Some(MergeStyle::Squash) => json!({"merge_method":"squash"}),
                    Some(MergeStyle::Rebase) => json!({"merge_method":"rebase"}),
                };
                self.request(Method::Put(body), &format!("{}/repos/{}/pulls/{n}/merge", self.api_base(), self.repo.path)).map(|_| ())
            }
            ForgeKind::Gitea => {
                let how = match style {
                    None | Some(MergeStyle::Merge) => "merge",
                    Some(MergeStyle::Squash) => "squash",
                    Some(MergeStyle::Rebase) => "rebase",
                };
                self.request(Method::Post(json!({"Do":how})), &format!("{}/repos/{}/pulls/{n}/merge", self.api_base(), self.repo.path)).map(|_| ())
            }
            ForgeKind::GitLab => {
                let body = if style == Some(MergeStyle::Squash) { json!({"squash":true}) } else { json!({}) };
                self.request(Method::Put(body), &format!("{}/projects/{}/merge_requests/{n}/merge", self.api_base(), url_encode(&self.repo.path))).map(|_| ())
            }
            ForgeKind::Bitbucket => {
                let body = match style {
                    None => json!({}),
                    Some(MergeStyle::Squash) => json!({"merge_strategy":"squash"}),
                    Some(_) => json!({"merge_strategy":"merge_commit"}),
                };
                self.request(Method::Post(body), &format!("{}/repositories/{}/pullrequests/{n}/merge", self.api_base(), self.repo.path)).map(|_| ())
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                let base = format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}?api-version=7.0", self.azure_root());
                let pr = self.request(Method::Get, &base)?;
                let last = pr.get("lastMergeSourceCommit").cloned().unwrap_or(Value::Null);
                let mut body = json!({"status":"completed","lastMergeSourceCommit":last});
                if let Some(st) = style {
                    let name = match st {
                        MergeStyle::Merge => "noFastForward",
                        MergeStyle::Squash => "squash",
                        MergeStyle::Rebase => "rebase",
                    };
                    body["completionOptions"] = json!({"mergeStrategy":name});
                }
                self.request(Method::Patch(body), &base).map(|_| ())
            }
        }
    }

    /// Your vote on Azure DevOps: 10 approves, -10 is "rejected".
    fn azure_vote(&self, n: u64, vote: i32) -> Result<()> {
        let (org, proj, repo) = self.azure_parts()?;
        let who = self.request(Method::Get, &format!("{}/{org}/_apis/connectionData", self.azure_root()))?;
        let id = s(who.pointer("/authenticatedUser/id"));
        if id.is_empty() {
            return Err(Error::Other("The host didn't say who you are, so no vote was sent.".into()));
        }
        self.request(Method::Put(json!({"vote":vote})), &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}/reviewers/{id}?api-version=7.0", self.azure_root())).map(|_| ())
    }

    /// Post a verdict, with what you had to say about it.
    pub fn review_pr(&self, n: u64, verdict: Verdict, body: &str) -> Result<()> {
        self.needs_api()?;
        if verdict == Verdict::RequestChanges && !self.repo.kind.can_request_changes() {
            return Err(Error::Other(format!("{} has no way to request changes by name. Leave a comment instead.", self.repo.kind.label())));
        }
        if verdict == Verdict::Comment {
            return self.comment_pr(n, body);
        }
        match self.repo.kind {
            ForgeKind::GitHub | ForgeKind::Gitea => {
                let event = match (self.repo.kind, verdict) {
                    (_, Verdict::RequestChanges) => "REQUEST_CHANGES",
                    (ForgeKind::GitHub, _) => "APPROVE",
                    _ => "APPROVED",
                };
                return self.request(Method::Post(json!({"event":event,"body":body})), &format!("{}/repos/{}/pulls/{n}/reviews", self.api_base(), self.repo.path)).map(|_| ());
            }
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                let status = if verdict == Verdict::Approve { "APPROVED" } else { "NEEDS_WORK" };
                if self.user.is_empty() {
                    return Err(Error::Other("Give a username with Connect: this host needs it to record your verdict.".into()));
                }
                self.request(Method::Put(json!({"status":status})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/participants/{}", url_encode(&self.user)))?;
            }
            ForgeKind::GitLab => {
                self.request(Method::Post(json!({})), &format!("{}/projects/{}/merge_requests/{n}/approve", self.api_base(), url_encode(&self.repo.path)))?;
            }
            ForgeKind::Bitbucket => {
                let what = if verdict == Verdict::Approve { "approve" } else { "request-changes" };
                self.request(Method::Post(json!({})), &format!("{}/repositories/{}/pullrequests/{n}/{what}", self.api_base(), self.repo.path))?;
            }
            ForgeKind::AzureDevOps => self.azure_vote(n, if verdict == Verdict::Approve { 10 } else { -10 })?,
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
        }
        // Those hosts take the verdict and any words separately.
        if body.trim().is_empty() { Ok(()) } else { self.comment_pr(n, body) }
    }

    /// A reply within the thread that `first` started: the first comment [`PrDetails::threads`] gives.
    pub fn reply_in_thread(&self, n: u64, first: &PrComment, body: &str) -> Result<()> {
        self.needs_api()?;
        if !self.repo.kind.can_reply_in_threads() {
            return Err(Error::Other(format!("{} can't take a reply within a thread from here. Comment on the line again in Files.", self.repo.kind.label())));
        }
        let number = || first.id.parse::<u64>().map_err(|_| Error::Other("The host didn't say which comment this is, so no reply was sent.".into()));
        match self.repo.kind {
            ForgeKind::GitHub => self.request(Method::Post(json!({"body":body})), &format!("{}/repos/{}/pulls/{n}/comments/{}/replies", self.api_base(), self.repo.path, number()?)).map(|_| ()),
            ForgeKind::GitLab => self.request(Method::Post(json!({"body":body})), &format!("{}/projects/{}/merge_requests/{n}/discussions/{}/notes", self.api_base(), url_encode(&self.repo.path), url_encode(&first.thread))).map(|_| ()),
            ForgeKind::Bitbucket => self.request(Method::Post(json!({"content":{"raw":body},"parent":{"id":number()?}})), &format!("{}/repositories/{}/pullrequests/{n}/comments", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                self.request(Method::Post(json!({"text":body,"parent":{"id":number()?}})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/comments")).map(|_| ())
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                self.request(Method::Post(json!({"content":body,"parentCommentId":number()?,"commentType":1})), &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}/threads/{}/comments?api-version=7.0", self.azure_root(), url_encode(&first.thread))).map(|_| ())
            }
            ForgeKind::Gitea | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected above"),
        }
    }

    /// A comment on one line of the new side of a file.
    pub fn comment_line(&self, n: u64, d: &PrDetails, file: &PrFile, line: u32, body: &str) -> Result<()> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::GitHub => self.request(Method::Post(json!({"body":body,"commit_id":d.head_sha,"path":file.path,"line":line,"side":"RIGHT"})), &format!("{}/repos/{}/pulls/{n}/comments", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::Gitea => self.request(Method::Post(json!({"event":"COMMENT","body":"","commit_id":d.head_sha,"comments":[{"path":file.path,"body":body,"new_position":line}]})), &format!("{}/repos/{}/pulls/{n}/reviews", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::GitLab => {
                let old = if file.old_path.is_empty() { &file.path } else { &file.old_path };
                let position = json!({"position_type":"text","base_sha":d.base_sha,"start_sha":d.start_sha,"head_sha":d.head_sha,"old_path":old,"new_path":file.path,"new_line":line});
                self.request(Method::Post(json!({"body":body,"position":position})), &format!("{}/projects/{}/merge_requests/{n}/discussions", self.api_base(), url_encode(&self.repo.path))).map(|_| ())
            }
            ForgeKind::Bitbucket => self.request(Method::Post(json!({"content":{"raw":body},"inline":{"path":file.path,"to":line}})), &format!("{}/repositories/{}/pullrequests/{n}/comments", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                self.request(Method::Post(json!({"text":body,"anchor":{"line":line,"lineType":"ADDED","fileType":"TO","path":file.path}})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/comments")).map(|_| ())
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                let context = json!({"filePath":format!("/{}", file.path),"rightFileStart":{"line":line,"offset":1},"rightFileEnd":{"line":line,"offset":1}});
                self.request(Method::Post(json!({"comments":[{"content":body,"commentType":1}],"status":1,"threadContext":context})), &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}/threads?api-version=7.0", self.azure_root())).map(|_| ())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
        }
    }
}

/// A pull request from anywhere on the host, with the repository it's in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvolvedPr {
    pub repo: String,
    pub pr: PullRequest,
}

/// What one host said when asked for everything you're in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPrs {
    /// The name its token is saved under.
    pub host: String,
    pub kind: ForgeKind,
    pub found: std::result::Result<Vec<InvolvedPr>, String>,
}

/// One repository per host to ask through, first seen first. Azure DevOps asks per organisation,
/// so its organisations count as hosts here. Hosts with no API to ask are left out.
pub fn hosts_to_ask(urls: &[String], custom: &[crate::settings::CustomHost]) -> Vec<RemoteRepo> {
    let mut out: Vec<RemoteRepo> = Vec::new();
    for repo in urls.iter().filter_map(|u| identify(u, custom)).filter(|r| r.kind.has_api()) {
        if !out.iter().any(|o| same_host(o, &repo)) {
            out.push(repo);
        }
    }
    out
}

/// Whether one query answers for both.
pub fn same_host(a: &RemoteRepo, b: &RemoteRepo) -> bool {
    let org = |r: &RemoteRepo| if r.kind == ForgeKind::AzureDevOps { r.path.split('/').next().unwrap_or("").to_string() } else { String::new() };
    a.kind == b.kind && a.token_host() == b.token_host() && org(a) == org(b)
}

/// Asks each host at once, so a slow one holds up only its own answer. Each host's failure stays
/// its own, beside what the rest found.
pub fn involved_on_hosts(clients: Vec<ForgeClient>) -> Vec<HostPrs> {
    std::thread::scope(|scope| {
        let asking: Vec<_> = clients.iter().map(|c| (c, scope.spawn(move || c.involved_prs()))).collect();
        asking
            .into_iter()
            .map(|(c, job)| HostPrs {
                host: c.repo.token_host(),
                kind: c.repo.kind,
                found: job.join().unwrap_or_else(|_| Err(Error::Other("Asking this host stopped unexpectedly.".into()))).map_err(|e| e.to_string()),
            })
            .collect()
    })
}

/// A pull request a commit came in by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
}

impl ForgeClient {
    /// Every open pull request you're involved in on this host, in any repository. What each host
    /// counts as involved differs: on GitHub it's its own `involves:`, elsewhere what you opened,
    /// were assigned or were asked to review, as far as its API says.
    pub fn involved_prs(&self) -> Result<Vec<InvolvedPr>> {
        self.needs_api()?;
        let mut all: Vec<InvolvedPr> = Vec::new();
        let mut add = |found: Vec<InvolvedPr>| {
            for f in found {
                if !all.iter().any(|x| x.pr.url == f.pr.url) {
                    all.push(f);
                }
            }
        };
        match self.repo.kind {
            ForgeKind::GitHub => {
                let url = if self.repo.host == "github.com" && self.repo.api_base.is_none() { "https://api.github.com/graphql".to_string() } else { format!("{}/graphql", self.api_base().trim_end_matches("/v3")) };
                let q = "query($c:String){search(query:\"is:open is:pr involves:@me archived:false\",type:ISSUE,first:50,after:$c){pageInfo{hasNextPage endCursor} nodes{... on PullRequest{number title url body isDraft state author{login} isCrossRepository headRepository{nameWithOwner} headRefName baseRefName mergeable reviewDecision additions deletions updatedAt repository{nameWithOwner} labels(first:10){nodes{name}} reviewRequests(first:15){nodes{requestedReviewer{... on User{login}}}} commits(last:1){nodes{commit{statusCheckRollup{state}}}}}}}}";
                let mut cursor = Value::Null;
                let mut found = Vec::new();
                loop {
                    let v = self.request(Method::Post(json!({"query":q,"variables":{"c":cursor}})), &url)?;
                    let Some(conn) = v.pointer("/data/search").filter(|c| !c.is_null()) else {
                        return Err(Error::Other(v.pointer("/errors/0/message").and_then(Value::as_str).unwrap_or("The host's reply had no list in it.").to_string()));
                    };
                    found.extend(conn.pointer("/nodes").and_then(Value::as_array).into_iter().flatten().filter(|n| !n.is_null()).map(|n| InvolvedPr { repo: s(n.pointer("/repository/nameWithOwner")), pr: parse_github_pr(n) }));
                    let more = conn.pointer("/pageInfo/hasNextPage").and_then(Value::as_bool).unwrap_or(false);
                    cursor = conn.pointer("/pageInfo/endCursor").cloned().unwrap_or(Value::Null);
                    if !more || cursor.is_null() || found.len() >= LIST_LIMIT {
                        break;
                    }
                }
                add(found);
            }
            ForgeKind::Gitea => {
                let base = self.api_base();
                for who in ["created=true", "assigned=true", "review_requested=true"] {
                    let items = self.paged_get(|p| format!("{base}/repos/issues/search?type=pulls&state=open&{who}&limit={PER_PAGE}&page={p}"), "", "Pull requests")?;
                    add(items.iter().map(|n| InvolvedPr { repo: s(n.pointer("/repository/full_name")), pr: PullRequest { number: n.get("number").and_then(Value::as_u64).unwrap_or(0), title: s(n.get("title")), url: s(n.get("html_url")), author: s(n.pointer("/user/login")), state: s(n.get("state")), updated: ts(n.get("updated_at")), draft: n.pointer("/pull_request/draft").and_then(Value::as_bool).unwrap_or(false), ..Default::default() } }).collect());
                }
            }
            ForgeKind::GitLab => {
                let base = self.api_base();
                for who in ["created_by_me", "assigned_to_me"] {
                    let items = self.paged_get(|p| format!("{base}/merge_requests?state=opened&scope={who}&per_page={PER_PAGE}&page={p}"), "", "Merge requests")?;
                    add(items.iter().map(|n| InvolvedPr { repo: s(n.pointer("/references/full")).split('!').next().unwrap_or("").to_string(), pr: parse_gitlab_mr(n) }).collect());
                }
            }
            ForgeKind::Bitbucket => {
                let me = self.whoami()?;
                let items = paged(|page| {
                    let v = self.get(&format!("/pullrequests/{}?state=OPEN&pagelen={PER_PAGE}&page={page}", url_encode(&me)))?;
                    let more = v.get("next").is_some_and(|x| !x.is_null());
                    Ok((list_in(&v, "/values", "Pull requests")?, more))
                })?;
                add(items.iter().map(|n| InvolvedPr { repo: s(n.pointer("/destination/repository/full_name")), pr: parse_bitbucket_pr(n) }).collect());
            }
            ForgeKind::BitbucketServer => {
                let (b, _, _) = self.server_api();
                let items = paged(|page| {
                    let v = self.request(Method::Get, &format!("{b}/dashboard/pull-requests?state=OPEN&limit={PER_PAGE}&start={}", (page - 1) * PER_PAGE))?;
                    let more = v.get("isLastPage").and_then(Value::as_bool) == Some(false);
                    Ok((list_in(&v, "/values", "Pull requests")?, more))
                })?;
                add(items
                    .iter()
                    .map(|n| {
                        let (key, slug) = (s(n.pointer("/toRef/repository/project/key")), s(n.pointer("/toRef/repository/slug")));
                        let mut pr = parse_bitbucket_server_pr(n, &self.repo);
                        // A pull request elsewhere isn't at this repository's address.
                        pr.url = n.pointer("/links/self/0/href").and_then(Value::as_str).map(String::from).unwrap_or_default();
                        InvolvedPr { repo: format!("{key}/{slug}"), pr }
                    })
                    .collect());
            }
            ForgeKind::AzureDevOps => {
                let (org, _, _) = self.azure_parts()?;
                let who = self.request(Method::Get, &format!("{}/{org}/_apis/connectionData", self.azure_root()))?;
                let id = s(who.pointer("/authenticatedUser/id"));
                for filter in ["creatorId", "reviewerId"] {
                    let root = self.azure_root();
                    let items = self.paged_get(|p| format!("{root}/{org}/_apis/git/pullrequests?searchCriteria.status=active&searchCriteria.{filter}={id}&$top={PER_PAGE}&$skip={}&api-version=7.0", (p - 1) * PER_PAGE), "/value", "Pull requests")?;
                    add(items
                        .iter()
                        .map(|n| {
                            let (project, name) = (s(n.pointer("/repository/project/name")), s(n.pointer("/repository/name")));
                            let mut pr = parse_azure_pr(n, &self.repo);
                            pr.url = format!("{root}/{org}/{project}/_git/{name}/pullrequest/{}", pr.number);
                            InvolvedPr { repo: format!("{project}/{name}"), pr }
                        })
                        .collect());
                }
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
        }
        Ok(all)
    }

    /// The pull requests a commit came in by, on the hosts that can say.
    pub fn prs_for_commit(&self, sha: &str) -> Result<Vec<CommitPr>> {
        self.needs_api()?;
        let from = |n: &Value, number: &str, url: &str| CommitPr { number: n.get(number).and_then(Value::as_u64).unwrap_or(0), title: s(n.get("title")), url: s(n.pointer(url)), state: s(n.get("state")) };
        Ok(match self.repo.kind {
            ForgeKind::GitHub => list_in(&self.get(&format!("/repos/{}/commits/{sha}/pulls", self.repo.path))?, "", "Pull requests")?.iter().map(|n| from(n, "number", "/html_url")).collect(),
            // One at most. A 404 means there's none, not that something's wrong.
            ForgeKind::Gitea => match self.get(&format!("/repos/{}/commits/{sha}/pull", self.repo.path)) {
                Ok(n) => vec![from(&n, "number", "/html_url")],
                Err(e) if e.to_string().contains("HTTP 404") => Vec::new(),
                Err(e) => return Err(e),
            },
            ForgeKind::GitLab => list_in(&self.get(&format!("/projects/{}/repository/commits/{sha}/merge_requests", url_encode(&self.repo.path)))?, "", "Merge requests")?.iter().map(|n| from(n, "iid", "/web_url")).collect(),
            ForgeKind::Bitbucket => list_in(&self.get(&format!("/repositories/{}/commit/{sha}/pullrequests", self.repo.path))?, "/values", "Pull requests")?.iter().map(|n| from(n, "id", "/links/html/href")).collect(),
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                list_in(&self.request(Method::Get, &format!("{b}/projects/{p}/repos/{r}/commits/{sha}/pull-requests"))?, "/values", "Pull requests")?.iter().map(|n| from(n, "id", "/links/self/0/href")).collect()
            }
            ForgeKind::AzureDevOps => return Err(Error::Other("Azure DevOps has no simple way to ask this. Search for the commit on the host.".into())),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_number_each_side_on_their_own() {
        let lines = patch_lines("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -3,2 +10,3 @@ fn x()\n same\n-old\n+new\n+newer\n\\ No newline at end of file\n@@ -40 +50 @@\n-a\n+b\n");
        let got: Vec<(PatchLineKind, Option<u32>, Option<u32>)> = lines.iter().map(|l| (l.kind, l.old, l.new)).collect();
        use PatchLineKind::*;
        assert_eq!(got, vec![(Hunk, None, None), (Context, Some(3), Some(10)), (Removed, Some(4), None), (Added, None, Some(11)), (Added, None, Some(12)), (Hunk, None, None), (Removed, Some(40), None), (Added, None, Some(50))]);
        assert_eq!(lines[3].text, "new");
    }

    #[test]
    fn a_whole_diff_splits_by_file_including_a_deleted_one_and_a_bare_rename() {
        let d = "diff --git a/x y.txt b/x y.txt\n--- a/x y.txt\n+++ b/x y.txt\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\ndiff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\n";
        let parts = split_diff(d);
        let names: Vec<&str> = parts.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, vec!["x y.txt", "gone", "new"]);
        assert!(parts[0].1.contains("+b"));
        assert_eq!(patch_lines(&parts[1].1).len(), 2);
    }

    #[test]
    fn every_word_a_host_uses_lands_somewhere_sensible() {
        for (w, st) in [("SUCCESS", CheckState::Passed), ("failed", CheckState::Failed), ("in_progress", CheckState::Running), ("skipped", CheckState::Other), ("", CheckState::Other)] {
            assert_eq!(state_of(w), st, "{w}");
        }
    }
}
