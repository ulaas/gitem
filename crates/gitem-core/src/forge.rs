//! Remote hosting integrations: URL building for every provider plus REST and
//! GraphQL clients for pull requests, issues, and gists; Jira and Linear issues.
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::settings::{Autolink, CustomHost};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ForgeKind {
    GitHub,
    GitLab,
    Bitbucket,
    AzureDevOps,
    Gitea,
    /// Self-hosted Bitbucket Server / Data Center, with its own URLs and API.
    BitbucketServer,
    /// Links only: Gerrit review has no pull-request model to list here.
    Gerrit,
    /// Links only.
    GoogleSource,
}

impl ForgeKind {
    pub const ALL: [ForgeKind; 8] = [Self::GitHub, Self::GitLab, Self::Bitbucket, Self::BitbucketServer, Self::AzureDevOps, Self::Gitea, Self::Gerrit, Self::GoogleSource];

    /// Whether pull requests and issues can be used through this provider's API.
    pub fn has_api(self) -> bool {
        !matches!(self, Self::Gerrit | Self::GoogleSource)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Bitbucket => "Bitbucket",
            Self::AzureDevOps => "Azure DevOps",
            Self::Gitea => "Gitea / Forgejo",
            Self::BitbucketServer => "Bitbucket Server / Data Center",
            Self::Gerrit => "Gerrit",
            Self::GoogleSource => "Google Source",
        }
    }
    pub fn pr_word(self) -> &'static str {
        if self == Self::GitLab { "merge request" } else { "pull request" }
    }
}

/// A repository on a hosting provider, identified from a remote URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteRepo {
    pub kind: ForgeKind,
    pub host: String,
    /// `owner/repo` path (GitLab subgroups included). For Azure: `org/project/repo`.
    pub path: String,
    pub api_base: Option<String>,
    pub templates: UrlTemplates,
}

/// Links for a host with its own URL scheme. Tokens: `${repo}`, `${sha}`, `${file}`,
/// `${line}`, `${branch}`, `${base}`, `${head}`. A blank template uses the
/// provider type's normal link.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct UrlTemplates {
    pub commit: String,
    pub file: String,
    pub branch: String,
    pub compare: String,
}

impl RemoteRepo {
    fn templated(&self, t: &str, pairs: &[(&str, &str)]) -> Option<String> {
        if t.is_empty() {
            return None;
        }
        let mut out = t.replace("${repo}", &self.path);
        for (k, v) in pairs {
            out = out.replace(&format!("${{{k}}}"), v);
        }
        Some(out)
    }
}

/// Normalise any remote URL form to (host, path).
pub fn parse_remote_url(url: &str) -> Option<(String, String)> {
    let u = url.trim();
    let strip = |p: &str| p.trim_matches('/').trim_end_matches(".git").to_string();
    if let Some((scheme, rest)) = u.split_once("://") {
        // A one-letter scheme is a Windows drive, as in `C://folder`.
        if scheme == "file" || scheme.len() <= 1 {
            return None;
        }
        let rest = rest.rsplit_once('@').map(|x| x.1).filter(|_| rest.split('/').next().is_some_and(|a| a.contains('@'))).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host).to_lowercase();
        if host.is_empty() {
            return None;
        }
        return Some((host, strip(path)));
    }
    // scp-like: [user@]host:path. A Windows drive letter has a one-letter "host".
    let (left, path) = u.split_once(':')?;
    if left.len() <= 1 || left.contains('/') || left.contains('\\') {
        return None;
    }
    let host = left.rsplit('@').next().unwrap_or(left).to_lowercase();
    Some((host, strip(path)))
}

pub fn identify(url: &str, custom: &[CustomHost]) -> Option<RemoteRepo> {
    let (host, path) = parse_remote_url(url)?;
    if let Some(c) = custom.iter().find(|c| c.host.eq_ignore_ascii_case(&host)) {
        let api = (!c.api_base.is_empty()).then(|| c.api_base.trim_end_matches('/').to_string());
        return Some(RemoteRepo { kind: c.kind, host, path, api_base: api, templates: c.templates.clone() });
    }
    let kind = if host == "github.com" || host.contains("github") {
        ForgeKind::GitHub
    } else if host.contains("gitlab") {
        ForgeKind::GitLab
    } else if host.contains("bitbucket") {
        ForgeKind::Bitbucket
    } else if host.contains("dev.azure.com") || host.ends_with("visualstudio.com") {
        ForgeKind::AzureDevOps
    } else if host.contains("gitea") || host.contains("forgejo") || host == "codeberg.org" {
        ForgeKind::Gitea
    } else if host.ends_with("googlesource.com") {
        ForgeKind::GoogleSource
    } else if host.contains("gerrit") {
        ForgeKind::Gerrit
    } else {
        return None;
    };
    let mut path = path;
    if kind == ForgeKind::AzureDevOps {
        // https: org/project/_git/repo; ssh: v3/org/project/repo
        path = path.trim_start_matches("v3/").replace("/_git/", "/");
    }
    Some(RemoteRepo { kind, host: if kind == ForgeKind::AzureDevOps { "dev.azure.com".into() } else { host.trim_start_matches("ssh.").to_string() }, path, api_base: None, templates: UrlTemplates::default() })
}

fn enc_path(p: &str) -> String {
    p.split('/').map(url_encode).collect::<Vec<_>>().join("/")
}

pub fn url_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

impl RemoteRepo {
    pub fn web_url(&self) -> String {
        match self.kind {
            ForgeKind::AzureDevOps => {
                let p: Vec<&str> = self.path.splitn(3, '/').collect();
                if p.len() == 3 { format!("https://dev.azure.com/{}/{}/_git/{}", p[0], p[1], p[2]) } else { format!("https://dev.azure.com/{}", self.path) }
            }
            _ => format!("https://{}/{}", self.host, self.path),
        }
    }
    /// `PROJECT/repo` from Bitbucket Server's `scm/PROJECT/repo` clone path.
    fn server_parts(&self) -> (String, String) {
        let p = self.path.trim_start_matches("scm/");
        let (a, b) = p.split_once('/').unwrap_or((p, ""));
        (a.to_string(), b.to_string())
    }

    fn server_web(&self) -> String {
        let (p, r) = self.server_parts();
        format!("https://{}/projects/{p}/repos/{r}", self.host)
    }

    pub fn commit_url(&self, sha: &str) -> String {
        if let Some(u) = self.templated(&self.templates.commit, &[("sha", sha)]) {
            return u;
        }
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/commits/{sha}", self.server_web()),
            ForgeKind::Gerrit => return format!("{}/+/{sha}", self.web_url()),
            ForgeKind::GoogleSource => return format!("{}/+/{sha}", self.web_url()),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub | ForgeKind::Gitea => format!("{}/commit/{sha}", self.web_url()),
            ForgeKind::GitLab => format!("{}/-/commit/{sha}", self.web_url()),
            ForgeKind::Bitbucket => format!("{}/commits/{sha}", self.web_url()),
            ForgeKind::AzureDevOps => format!("{}/commit/{sha}", self.web_url()),
        }
    }
    pub fn file_url(&self, rev: &str, path: &str, line: Option<(u32, u32)>) -> String {
        let p = enc_path(path);
        let n = line.map(|l| l.0.to_string()).unwrap_or_default();
        if let Some(u) = self.templated(&self.templates.file, &[("sha", rev), ("file", &p), ("line", &n)]) {
            return u;
        }
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/browse/{p}?at={rev}{}", self.server_web(), line.map(|(a, b)| if a == b { format!("#{a}") } else { format!("#{a}-{b}") }).unwrap_or_default()),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return format!("{base}/+/{rev}/{p}{}", line.map(|(a, _)| format!("#{a}")).unwrap_or_default()),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub => format!(
                "{base}/blob/{rev}/{p}{}",
                line.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-L{b}") }).unwrap_or_default()
            ),
            ForgeKind::GitLab => format!("{base}/-/blob/{rev}/{p}{}", line.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-{b}") }).unwrap_or_default()),
            ForgeKind::Bitbucket => format!("{base}/src/{rev}/{p}{}", line.map(|(a, b)| if a == b { format!("#lines-{a}") } else { format!("#lines-{a}:{b}") }).unwrap_or_default()),
            ForgeKind::Gitea => format!("{base}/src/commit/{rev}/{p}{}", line.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-L{b}") }).unwrap_or_default()),
            ForgeKind::AzureDevOps => format!(
                "{base}?path=/{p}&version=GC{rev}{}",
                line.map(|(a, b)| format!("&line={a}&lineEnd={}&lineStartColumn=1&lineEndColumn=1", b + 1)).unwrap_or_default()
            ),
        }
    }
    pub fn branch_url(&self, branch: &str) -> String {
        let b = enc_path(branch);
        if let Some(u) = self.templated(&self.templates.branch, &[("branch", &b)]) {
            return u;
        }
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/browse?at=refs/heads/{b}", self.server_web()),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return format!("{base}/+/refs/heads/{b}"),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub => format!("{base}/tree/{b}"),
            ForgeKind::GitLab => format!("{base}/-/tree/{b}"),
            ForgeKind::Bitbucket => format!("{base}/branch/{b}"),
            ForgeKind::Gitea => format!("{base}/src/branch/{b}"),
            ForgeKind::AzureDevOps => format!("{base}?version=GB{}", url_encode(branch)),
        }
    }
    pub fn tag_url(&self, tag: &str) -> String {
        let t = enc_path(tag);
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/browse?at=refs/tags/{t}", self.server_web()),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return format!("{base}/+/refs/tags/{t}"),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub => format!("{base}/releases/tag/{t}"),
            ForgeKind::GitLab => format!("{base}/-/tags/{t}"),
            ForgeKind::Bitbucket => format!("{base}/src/{t}"),
            ForgeKind::Gitea => format!("{base}/releases/tag/{t}"),
            ForgeKind::AzureDevOps => format!("{base}?version=GT{}", url_encode(tag)),
        }
    }
    pub fn compare_url(&self, base_ref: &str, head: &str) -> String {
        if let Some(u) = self.templated(&self.templates.compare, &[("base", base_ref), ("head", head)]) {
            return u;
        }
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/compare/commits?sourceBranch=refs/heads/{}&targetBranch=refs/heads/{}", self.server_web(), url_encode(head), url_encode(base_ref)),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return format!("{base}/+/{}..{}", enc_path(base_ref), enc_path(head)),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub | ForgeKind::Gitea => format!("{base}/compare/{}...{}", enc_path(base_ref), enc_path(head)),
            ForgeKind::GitLab => format!("{base}/-/compare/{}...{}", enc_path(base_ref), enc_path(head)),
            ForgeKind::Bitbucket => format!("{base}/branches/compare/{}%0D{}", enc_path(head), enc_path(base_ref)),
            ForgeKind::AzureDevOps => format!("{base}/branchCompare?baseVersion=GB{}&targetVersion=GB{}", url_encode(base_ref), url_encode(head)),
        }
    }
    pub fn new_pr_url(&self, base_ref: &str, head: &str) -> String {
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/pull-requests?create&sourceBranch=refs/heads/{}&targetBranch=refs/heads/{}", self.server_web(), url_encode(head), url_encode(base_ref)),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return base,
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub | ForgeKind::Gitea => format!("{base}/compare/{}...{}?expand=1", enc_path(base_ref), enc_path(head)),
            ForgeKind::GitLab => format!("{base}/-/merge_requests/new?merge_request[source_branch]={}&merge_request[target_branch]={}", url_encode(head), url_encode(base_ref)),
            ForgeKind::Bitbucket => format!("{base}/pull-requests/new?source={}&dest={}", url_encode(head), url_encode(base_ref)),
            ForgeKind::AzureDevOps => format!("{base}/pullrequestcreate?sourceRef={}&targetRef={}", url_encode(head), url_encode(base_ref)),
        }
    }
    pub fn pr_url(&self, n: u64) -> String {
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer => return format!("{}/pull-requests/{n}/overview", self.server_web()),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => return format!("https://{}/c/{}/+/{n}", self.host, self.path),
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitHub => format!("{base}/pull/{n}"),
            ForgeKind::GitLab => format!("{base}/-/merge_requests/{n}"),
            ForgeKind::Bitbucket => format!("{base}/pull-requests/{n}"),
            ForgeKind::Gitea => format!("{base}/pulls/{n}"),
            ForgeKind::AzureDevOps => format!("{base}/pullrequest/{n}"),
        }
    }
    pub fn issue_url(&self, n: u64) -> String {
        let base = self.web_url();
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => return base,
            _ => {}
        }
        match self.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("handled above"),
            ForgeKind::GitLab => format!("{base}/-/issues/{n}"),
            ForgeKind::AzureDevOps => {
                let p: Vec<&str> = self.path.splitn(3, '/').collect();
                if p.len() >= 2 { format!("https://dev.azure.com/{}/{}/_workitems/edit/{n}", p[0], p[1]) } else { base }
            }
            _ => format!("{base}/issues/{n}"),
        }
    }
    /// Ref that holds a PR head, for checking it out locally.
    pub fn pr_ref(&self, n: u64) -> String {
        match self.kind {
            ForgeKind::BitbucketServer => format!("refs/pull-requests/{n}/from"),
            ForgeKind::GitLab => format!("refs/merge-requests/{n}/head"),
            ForgeKind::AzureDevOps => format!("refs/pull/{n}/head"),
            ForgeKind::Bitbucket => format!("refs/pull-requests/{n}/from"),
            _ => format!("refs/pull/{n}/head"),
        }
    }
    pub fn token_page(&self) -> String {
        match self.kind {
            ForgeKind::BitbucketServer => format!("https://{}/plugins/servlet/access-tokens/manage", self.host),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => format!("https://{}", self.host),
            ForgeKind::GitHub => format!("https://{}/settings/tokens", self.host),
            ForgeKind::GitLab => format!("https://{}/-/user_settings/personal_access_tokens", self.host),
            ForgeKind::Bitbucket => "https://bitbucket.org/account/settings/app-passwords/".to_string(),
            ForgeKind::AzureDevOps => "https://dev.azure.com/_usersSettings/tokens".to_string(),
            ForgeKind::Gitea => format!("https://{}/user/settings/applications", self.host),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub base: String,
    pub updated: i64,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty.
    pub review_decision: String,
    /// `SUCCESS`, `FAILURE`, `PENDING`, or empty.
    pub checks: String,
    /// `MERGEABLE`, `CONFLICTING`, `UNKNOWN`, or empty.
    pub mergeable: String,
    pub additions: u32,
    pub deletions: u32,
    pub requested_reviewers: Vec<String>,
    pub labels: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issue {
    /// Display id: `#12`, `PROJ-34`.
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub state: String,
    pub updated: i64,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub source: String,
}

impl Issue {
    /// Branch name from a template with `${id}` and `${title}`.
    pub fn branch_name_as(&self, format: &str) -> String {
        if format.trim().is_empty() {
            return self.branch_name();
        }
        let full = self.branch_name();
        let id = self.id.trim_start_matches('#').to_lowercase();
        let title = full.strip_prefix(&format!("{id}-")).unwrap_or("");
        format.replace("${id}", &id).replace("${title}", title)
    }

    /// Suggested branch name for "start work".
    pub fn branch_name(&self) -> String {
        let id = self.id.trim_start_matches('#').to_lowercase();
        let mut slug = String::new();
        for c in self.title.to_lowercase().chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c);
            } else if !slug.ends_with('-') && !slug.is_empty() {
                slug.push('-');
            }
        }
        let slug: String = slug.trim_end_matches('-').chars().take(50).collect();
        format!("{id}-{}", slug.trim_end_matches('-'))
    }
}

/// Launchpad action groups, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LaunchpadGroup {
    Mergeable,
    Blocked,
    NeedsYourReview,
    /// Yours, and a reviewer asked for changes: "requires follow-up".
    ChangesRequested,
    WaitingForReview,
    Draft,
    Other,
}

impl LaunchpadGroup {
    pub const ALL: [LaunchpadGroup; 7] = [Self::Mergeable, Self::Blocked, Self::NeedsYourReview, Self::ChangesRequested, Self::WaitingForReview, Self::Draft, Self::Other];
    pub fn label(self) -> &'static str {
        match self {
            Self::Mergeable => "Ready to merge",
            Self::Blocked => "Blocked",
            Self::NeedsYourReview => "Needs your review",
            Self::ChangesRequested => "Requires follow-up",
            Self::WaitingForReview => "Waiting for review",
            Self::Draft => "Draft",
            Self::Other => "Other open",
        }
    }
}

pub fn launchpad_group(pr: &PullRequest, me: &str) -> LaunchpadGroup {
    let mine = !me.is_empty() && pr.author.eq_ignore_ascii_case(me);
    if pr.requested_reviewers.iter().any(|r| r.eq_ignore_ascii_case(me)) && !me.is_empty() {
        return LaunchpadGroup::NeedsYourReview;
    }
    if pr.draft {
        return LaunchpadGroup::Draft;
    }
    if mine {
        if pr.mergeable == "CONFLICTING" || pr.checks == "FAILURE" {
            return LaunchpadGroup::Blocked;
        }
        if pr.review_decision == "CHANGES_REQUESTED" {
            return LaunchpadGroup::ChangesRequested;
        }
        if pr.review_decision == "APPROVED" {
            return LaunchpadGroup::Mergeable;
        }
        return LaunchpadGroup::WaitingForReview;
    }
    LaunchpadGroup::Other
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(60))).user_agent("gitem").build().into()
}

fn ts(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_str).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp()).unwrap_or(0)
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

fn strs(v: Option<&Value>, key: &str) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.get(key).and_then(Value::as_str).map(String::from)).collect()).unwrap_or_default()
}

pub struct ForgeClient {
    pub repo: RemoteRepo,
    pub token: String,
    pub user: String,
}

enum Method {
    Get,
    Post(Value),
    Put(Value),
    Patch(Value),
}

impl ForgeClient {
    pub fn new(repo: RemoteRepo, token: String, user: String) -> Self {
        Self { repo, token, user }
    }

    fn api_base(&self) -> String {
        if let Some(a) = &self.repo.api_base {
            return a.clone();
        }
        let h = &self.repo.host;
        match self.repo.kind {
            ForgeKind::GitHub => {
                if h == "github.com" {
                    "https://api.github.com".to_string()
                } else {
                    format!("https://{h}/api/v3")
                }
            }
            ForgeKind::GitLab => format!("https://{h}/api/v4"),
            ForgeKind::Bitbucket => "https://api.bitbucket.org/2.0".to_string(),
            ForgeKind::BitbucketServer => format!("https://{h}/rest/api/1.0"),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => format!("https://{h}"),
            ForgeKind::Gitea => format!("https://{h}/api/v1"),
            ForgeKind::AzureDevOps => "https://dev.azure.com".to_string(),
        }
    }

    fn request(&self, m: Method, url: &str) -> Result<Value> {
        let a = agent();
        let auth = match self.repo.kind {
            ForgeKind::GitHub => format!("Bearer {}", self.token),
            ForgeKind::GitLab | ForgeKind::BitbucketServer => format!("Bearer {}", self.token),
            ForgeKind::Gerrit | ForgeKind::GoogleSource => format!("Basic {}", base64(&format!("{}:{}", self.user, self.token))),
            ForgeKind::Gitea => format!("token {}", self.token),
            ForgeKind::Bitbucket | ForgeKind::AzureDevOps => format!("Basic {}", base64(&format!("{}:{}", self.user, self.token))),
        };
        let accept = if self.repo.kind == ForgeKind::GitHub { "application/vnd.github+json" } else { "application/json" };
        let send = |b: ureq::RequestBuilder<ureq::typestate::WithBody>, v: Value| {
            b.header("authorization", &auth).header("accept", accept).header("content-type", "application/json").send(v.to_string())
        };
        let resp = match m {
            Method::Get => a.get(url).header("authorization", &auth).header("accept", accept).call(),
            Method::Post(v) => send(a.post(url), v),
            Method::Put(v) => send(a.put(url), v),
            Method::Patch(v) => send(a.patch(url), v),
        };
        let mut resp = resp.map_err(|e| Error::Other(format!("{url}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from).or_else(|| v.pointer("/error/message").and_then(Value::as_str).map(String::from)))
                .unwrap_or_else(|| text.chars().take(300).collect());
            return Err(Error::Other(format!("HTTP {status}: {msg}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| Error::Other(format!("bad response from {url}: {e}")))
    }

    /// Azure DevOps Services, or an on-premises Azure DevOps Server collection.
    fn azure_root(&self) -> String {
        self.repo.api_base.clone().unwrap_or_else(|| "https://dev.azure.com".to_string())
    }

    fn needs_api(&self) -> Result<()> {
        if self.repo.kind.has_api() {
            Ok(())
        } else {
            Err(Error::Other(format!("{} links work, but it has no pull request API to use here.", self.repo.kind.label())))
        }
    }

    fn server_api(&self) -> (String, String, String) {
        let (p, r) = self.repo.server_parts();
        let base = self.repo.api_base.clone().unwrap_or_else(|| format!("https://{}/rest/api/1.0", self.repo.host));
        (base, p, r)
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::Get, &format!("{}{}", self.api_base(), path))
    }

    fn azure_parts(&self) -> Result<(String, String, String)> {
        let p: Vec<&str> = self.repo.path.splitn(3, '/').collect();
        if p.len() == 3 {
            Ok((p[0].to_string(), p[1].to_string(), p[2].to_string()))
        } else {
            Err(Error::Other(format!("Unrecognised Azure DevOps path: {}", self.repo.path)))
        }
    }

    /// Login of the authenticated user.
    pub fn whoami(&self) -> Result<String> {
        self.needs_api()?;
        Ok(match self.repo.kind {
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => self.user.clone(),
            ForgeKind::GitHub | ForgeKind::Gitea => s(self.get("/user")?.get("login")),
            ForgeKind::GitLab => s(self.get("/user")?.get("username")),
            ForgeKind::Bitbucket => s(self.get("/user")?.get("username")),
            ForgeKind::AzureDevOps => {
                let (org, _, _) = self.azure_parts()?;
                let v = self.request(Method::Get, &format!("{}/{org}/_apis/connectionData", self.azure_root()))?;
                s(v.pointer("/authenticatedUser/providerDisplayName"))
            }
        })
    }

    pub fn list_prs(&self) -> Result<Vec<PullRequest>> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (base, p, r) = self.server_api();
                let v = self.request(Method::Get, &format!("{base}/projects/{p}/repos/{r}/pull-requests?state=OPEN&limit=50"))?;
                Ok(v.get("values").and_then(Value::as_array).map(|a| a.iter().map(|x| parse_bitbucket_server_pr(x, &self.repo)).collect()).unwrap_or_default())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => Ok(Vec::new()),
            ForgeKind::GitHub => {
                let (owner, name) = self.repo.path.split_once('/').ok_or_else(|| Error::Other("bad repo path".into()))?;
                let q = "query($o:String!,$n:String!){repository(owner:$o,name:$n){pullRequests(first:50,states:OPEN,orderBy:{field:UPDATED_AT,direction:DESC}){nodes{number title url body isDraft state author{login} headRefName baseRefName mergeable reviewDecision additions deletions updatedAt labels(first:10){nodes{name}} reviewRequests(first:15){nodes{requestedReviewer{... on User{login}}}} commits(last:1){nodes{commit{statusCheckRollup{state}}}}}}}}";
                let url = if self.repo.host == "github.com" && self.repo.api_base.is_none() {
                    "https://api.github.com/graphql".to_string()
                } else {
                    format!("{}/graphql", self.api_base().trim_end_matches("/v3"))
                };
                let v = self.request(Method::Post(json!({"query":q,"variables":{"o":owner,"n":name}})), &url)?;
                if let Some(e) = v.pointer("/errors/0/message").and_then(Value::as_str) {
                    return Err(Error::Other(e.to_string()));
                }
                Ok(parse_github_prs(&v))
            }
            ForgeKind::GitLab => {
                let v = self.get(&format!("/projects/{}/merge_requests?state=opened&per_page=50&order_by=updated_at", url_encode(&self.repo.path)))?;
                Ok(parse_gitlab_mrs(&v))
            }
            ForgeKind::Bitbucket => {
                let v = self.get(&format!("/repositories/{}/pullrequests?state=OPEN&pagelen=50", self.repo.path))?;
                Ok(v.get("values").and_then(Value::as_array).map(|a| a.iter().map(parse_bitbucket_pr).collect()).unwrap_or_default())
            }
            ForgeKind::Gitea => {
                let v = self.get(&format!("/repos/{}/pulls?state=open&limit=50", self.repo.path))?;
                Ok(v.as_array().map(|a| a.iter().map(parse_gitea_pr).collect()).unwrap_or_default())
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                let v = self.request(Method::Get, &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests?searchCriteria.status=active&api-version=7.0", self.azure_root()))?;
                let web = self.repo.clone();
                Ok(v.get("value").and_then(Value::as_array).map(|a| a.iter().map(|p| parse_azure_pr(p, &web)).collect()).unwrap_or_default())
            }
        }
    }

    pub fn list_issues(&self) -> Result<Vec<Issue>> {
        let label = self.repo.kind.label().to_string();
        Ok(match self.repo.kind {
            // These hosts don't offer issue tracking.
            ForgeKind::BitbucketServer | ForgeKind::Gerrit | ForgeKind::GoogleSource => Vec::new(),
            ForgeKind::GitHub | ForgeKind::Gitea => {
                let path = if self.repo.kind == ForgeKind::GitHub {
                    format!("/repos/{}/issues?state=open&per_page=50&sort=updated", self.repo.path)
                } else {
                    format!("/repos/{}/issues?state=open&type=issues&limit=50", self.repo.path)
                };
                let v = self.get(&path)?;
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|i| i.get("pull_request").is_none())
                            .map(|i| {
                                let n = i.get("number").and_then(Value::as_u64).unwrap_or(0);
                                Issue {
                                    id: format!("#{n}"),
                                    number: n,
                                    title: s(i.get("title")),
                                    url: s(i.get("html_url")),
                                    author: s(i.pointer("/user/login")),
                                    state: s(i.get("state")),
                                    updated: ts(i.get("updated_at")),
                                    labels: strs(i.get("labels"), "name"),
                                    assignees: strs(i.get("assignees"), "login"),
                                    source: label.clone(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            ForgeKind::GitLab => {
                let v = self.get(&format!("/projects/{}/issues?state=opened&per_page=50&order_by=updated_at", url_encode(&self.repo.path)))?;
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                let n = i.get("iid").and_then(Value::as_u64).unwrap_or(0);
                                Issue {
                                    id: format!("#{n}"),
                                    number: n,
                                    title: s(i.get("title")),
                                    url: s(i.get("web_url")),
                                    author: s(i.pointer("/author/username")),
                                    state: s(i.get("state")),
                                    updated: ts(i.get("updated_at")),
                                    labels: i.get("labels").and_then(Value::as_array).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                                    assignees: strs(i.get("assignees"), "username"),
                                    source: label.clone(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            ForgeKind::Bitbucket => {
                let v = self.get(&format!("/repositories/{}/issues?pagelen=50", self.repo.path))?;
                v.get("values")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                let n = i.get("id").and_then(Value::as_u64).unwrap_or(0);
                                Issue {
                                    id: format!("#{n}"),
                                    number: n,
                                    title: s(i.get("title")),
                                    url: s(i.pointer("/links/html/href")),
                                    author: s(i.pointer("/reporter/display_name")),
                                    state: s(i.get("state")),
                                    updated: ts(i.get("updated_on")),
                                    source: label.clone(),
                                    ..Default::default()
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, _) = self.azure_parts()?;
                let wiql = json!({"query":"Select [System.Id] From WorkItems Where [System.TeamProject] = @project And [System.State] <> 'Closed' And [System.State] <> 'Done' Order By [System.ChangedDate] Desc"});
                let v = self.request(Method::Post(wiql), &format!("{}/{org}/{proj}/_apis/wit/wiql?$top=50&api-version=7.0", self.azure_root()))?;
                let ids: Vec<String> = v.get("workItems").and_then(Value::as_array).map(|a| a.iter().filter_map(|w| w.get("id").and_then(Value::as_u64)).map(|n| n.to_string()).collect()).unwrap_or_default();
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let d = self.request(Method::Get, &format!("{}/{org}/_apis/wit/workitems?ids={}&api-version=7.0", self.azure_root(), ids.join(",")))?;
                d.get("value")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                let n = i.get("id").and_then(Value::as_u64).unwrap_or(0);
                                Issue {
                                    id: format!("#{n}"),
                                    number: n,
                                    title: s(i.pointer("/fields/System.Title")),
                                    url: self.repo.issue_url(n),
                                    author: s(i.pointer("/fields/System.CreatedBy/displayName")),
                                    state: s(i.pointer("/fields/System.State")),
                                    updated: ts(i.pointer("/fields/System.ChangedDate")),
                                    source: label.clone(),
                                    ..Default::default()
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        })
    }

    pub fn create_pr(&self, title: &str, body: &str, head: &str, base: &str, draft: bool) -> Result<String> {
        self.needs_api()?;
        let url_of = |v: &Value, ptr: &str| s(v.pointer(ptr));
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                let side = |name: &str| json!({"id": format!("refs/heads/{name}"), "repository": {"slug": r, "project": {"key": p}}});
                let v = self.request(Method::Post(json!({"title":title,"description":body,"draft":draft,"fromRef":side(head),"toRef":side(base)})), &format!("{b}/projects/{p}/repos/{r}/pull-requests"))?;
                Ok(self.repo.pr_url(v.get("id").and_then(Value::as_u64).unwrap_or(0)))
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
            ForgeKind::GitHub => {
                let v = self.request(Method::Post(json!({"title":title,"body":body,"head":head,"base":base,"draft":draft})), &format!("{}/repos/{}/pulls", self.api_base(), self.repo.path))?;
                Ok(url_of(&v, "/html_url"))
            }
            ForgeKind::Gitea => {
                let v = self.request(Method::Post(json!({"title":title,"body":body,"head":head,"base":base})), &format!("{}/repos/{}/pulls", self.api_base(), self.repo.path))?;
                Ok(url_of(&v, "/html_url"))
            }
            ForgeKind::GitLab => {
                let t = if draft { format!("Draft: {title}") } else { title.to_string() };
                let v = self.request(
                    Method::Post(json!({"title":t,"description":body,"source_branch":head,"target_branch":base})),
                    &format!("{}/projects/{}/merge_requests", self.api_base(), url_encode(&self.repo.path)),
                )?;
                Ok(url_of(&v, "/web_url"))
            }
            ForgeKind::Bitbucket => {
                let v = self.request(
                    Method::Post(json!({"title":title,"description":body,"source":{"branch":{"name":head}},"destination":{"branch":{"name":base}}})),
                    &format!("{}/repositories/{}/pullrequests", self.api_base(), self.repo.path),
                )?;
                Ok(url_of(&v, "/links/html/href"))
            }
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                let v = self.request(
                    Method::Post(json!({"title":title,"description":body,"sourceRefName":format!("refs/heads/{head}"),"targetRefName":format!("refs/heads/{base}"),"isDraft":draft})),
                    &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests?api-version=7.0", self.azure_root()),
                )?;
                Ok(self.repo.pr_url(v.get("pullRequestId").and_then(Value::as_u64).unwrap_or(0)))
            }
        }
    }

    /// Merge a pull request with the host's default strategy.
    pub fn merge_pr(&self, n: u64) -> Result<()> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                let pr = self.request(Method::Get, &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}"))?;
                let version = pr.get("version").and_then(Value::as_u64).unwrap_or(0);
                self.request(Method::Post(json!({"version":version})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/merge")).map(|_| ())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
            ForgeKind::GitHub => self.request(Method::Put(json!({})), &format!("{}/repos/{}/pulls/{n}/merge", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::Gitea => self.request(Method::Post(json!({"Do":"merge"})), &format!("{}/repos/{}/pulls/{n}/merge", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::GitLab => self.request(Method::Put(json!({})), &format!("{}/projects/{}/merge_requests/{n}/merge", self.api_base(), url_encode(&self.repo.path))).map(|_| ()),
            ForgeKind::Bitbucket => self.request(Method::Post(json!({})), &format!("{}/repositories/{}/pullrequests/{n}/merge", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                let base = format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}?api-version=7.0", self.azure_root());
                let pr = self.request(Method::Get, &base)?;
                let last = pr.get("lastMergeSourceCommit").cloned().unwrap_or(Value::Null);
                self.request(Method::Patch(json!({"status":"completed","lastMergeSourceCommit":last})), &base).map(|_| ())
            }
        }
    }

    pub fn comment_pr(&self, n: u64, body: &str) -> Result<()> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                self.request(Method::Post(json!({"text":body})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/comments")).map(|_| ())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
            ForgeKind::GitHub | ForgeKind::Gitea => self.request(Method::Post(json!({"body":body})), &format!("{}/repos/{}/issues/{n}/comments", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::GitLab => self.request(Method::Post(json!({"body":body})), &format!("{}/projects/{}/merge_requests/{n}/notes", self.api_base(), url_encode(&self.repo.path))).map(|_| ()),
            ForgeKind::Bitbucket => self.request(Method::Post(json!({"content":{"raw":body}})), &format!("{}/repositories/{}/pullrequests/{n}/comments", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::AzureDevOps => {
                let (org, proj, repo) = self.azure_parts()?;
                self.request(
                    Method::Post(json!({"comments":[{"content":body,"commentType":1}],"status":1})),
                    &format!("{}/{org}/{proj}/_apis/git/repositories/{repo}/pullrequests/{n}/threads?api-version=7.0", self.azure_root()),
                )
                .map(|_| ())
            }
        }
    }

    /// Approve a pull request as the authenticated user.
    pub fn approve_pr(&self, n: u64) -> Result<()> {
        self.needs_api()?;
        match self.repo.kind {
            ForgeKind::BitbucketServer => {
                let (b, p, r) = self.server_api();
                self.request(Method::Post(json!({})), &format!("{b}/projects/{p}/repos/{r}/pull-requests/{n}/approve")).map(|_| ())
            }
            ForgeKind::Gerrit | ForgeKind::GoogleSource => unreachable!("rejected by needs_api"),
            ForgeKind::GitHub => self.request(Method::Post(json!({"event":"APPROVE"})), &format!("{}/repos/{}/pulls/{n}/reviews", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::Gitea => self.request(Method::Post(json!({"event":"APPROVED"})), &format!("{}/repos/{}/pulls/{n}/reviews", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::GitLab => self.request(Method::Post(json!({})), &format!("{}/projects/{}/merge_requests/{n}/approve", self.api_base(), url_encode(&self.repo.path))).map(|_| ()),
            ForgeKind::Bitbucket => self.request(Method::Post(json!({})), &format!("{}/repositories/{}/pullrequests/{n}/approve", self.api_base(), self.repo.path)).map(|_| ()),
            ForgeKind::AzureDevOps => Err(Error::Other("Approving from gitem is not available on Azure DevOps. Use Open on Remote.".into())),
        }
    }

    /// Open pull request for a branch, if any.
    pub fn pr_for_branch(&self, branch: &str) -> Result<Option<PullRequest>> {
        Ok(self.list_prs()?.into_iter().find(|p| p.head == branch))
    }

    /// Post local edits as suggested changes on a GitHub pull request's lines. Run
    /// with the pull request checked out, so line numbers match its head.
    pub fn suggest_changes(&self, n: u64, suggestions: &[Suggestion]) -> Result<()> {
        if self.repo.kind != ForgeKind::GitHub {
            return Err(Error::Other("Suggested changes need a GitHub pull request.".into()));
        }
        let comments: Vec<Value> = suggestions
            .iter()
            .map(|x| {
                let body = format!("```suggestion\n{}\n```", x.lines.join("\n"));
                if x.start == x.end {
                    json!({"path":x.path,"line":x.end,"side":"RIGHT","body":body})
                } else {
                    json!({"path":x.path,"start_line":x.start,"line":x.end,"start_side":"RIGHT","side":"RIGHT","body":body})
                }
            })
            .collect();
        self.request(Method::Post(json!({"event":"COMMENT","comments":comments})), &format!("{}/repos/{}/pulls/{n}/reviews", self.api_base(), self.repo.path)).map(|_| ())
    }

    /// Share text as a gist; used for the patch library.
    pub fn create_gist(&self, description: &str, filename: &str, content: &str, public: bool) -> Result<String> {
        if self.repo.kind != ForgeKind::GitHub {
            return Err(Error::Other("Sharing patches requires a GitHub token.".into()));
        }
        let v = self.request(
            Method::Post(json!({"description":description,"public":public,"files":{filename:{"content":content}}})),
            &format!("{}/gists", self.api_base()),
        )?;
        Ok(s(v.get("html_url")))
    }
}

/// A change proposed against lines of the committed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub lines: Vec<String>,
}

/// Turn a working-tree diff into suggestions: each run of changes replaces the
/// committed lines it touches. A pure insertion also restates the line above it,
/// since a suggestion must replace at least one existing line.
pub fn suggestions_from_diff(files: &[crate::diff::FileDiff]) -> Vec<Suggestion> {
    use crate::diff::LineKind;
    let mut out = Vec::new();
    for f in files.iter().filter(|f| !f.is_new && !f.is_deleted && !f.is_binary) {
        for h in &f.hunks {
            let mut i = 0;
            let mut before: Option<&crate::diff::DiffLine> = None;
            while i < h.lines.len() {
                let l = &h.lines[i];
                if l.kind == LineKind::Context {
                    before = Some(l);
                    i += 1;
                    continue;
                }
                if l.kind == LineKind::NoNewline {
                    i += 1;
                    continue;
                }
                let run_start = i;
                while i < h.lines.len() && matches!(h.lines[i].kind, LineKind::Add | LineKind::Del | LineKind::NoNewline) {
                    i += 1;
                }
                let run = &h.lines[run_start..i];
                let dels: Vec<u32> = run.iter().filter(|x| x.kind == LineKind::Del).filter_map(|x| x.old_no).collect();
                let adds: Vec<String> = run.iter().filter(|x| x.kind == LineKind::Add).map(|x| x.text.clone()).collect();
                if let (Some(a), Some(b)) = (dels.first(), dels.last()) {
                    out.push(Suggestion { path: f.display_path().to_string(), start: *a, end: *b, lines: adds });
                } else if let Some(ctx) = before.and_then(|c| c.old_no.map(|n| (n, c.text.clone()))) {
                    let mut lines = vec![ctx.1];
                    lines.extend(adds);
                    out.push(Suggestion { path: f.display_path().to_string(), start: ctx.0, end: ctx.0, lines });
                }
            }
        }
    }
    out
}

/// A `gitem://` link to a repository, commit, or file line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeepLink {
    pub repo: String,
    pub commit: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl DeepLink {
    pub fn to_url(&self) -> String {
        let mut u = format!("gitem://open?repo={}", url_encode(&self.repo));
        if let Some(c) = &self.commit {
            u.push_str(&format!("&commit={}", url_encode(c)));
        }
        if let Some(f) = &self.file {
            u.push_str(&format!("&file={}", url_encode(f)));
        }
        if let Some(l) = self.line {
            u.push_str(&format!("&line={l}"));
        }
        u
    }

    pub fn parse(url: &str) -> Option<Self> {
        let q = url.strip_prefix("gitem://open?")?;
        let mut d = DeepLink::default();
        for pair in q.split('&') {
            let (k, v) = pair.split_once('=')?;
            let v = percent_decode(v);
            match k {
                "repo" => d.repo = v,
                // A link is anyone's to write, and this goes on to git's command line.
                "commit" if is_commit_id(&v) => d.commit = Some(v),
                "commit" => return None,
                "file" if is_repo_relative(&v) => d.file = Some(v),
                "file" => return None,
                "line" => d.line = v.parse().ok(),
                _ => {}
            }
        }
        (!d.repo.is_empty()).then_some(d)
    }
}

/// A whole or abbreviated commit id. It's what gitem's own links contain, and nothing else
/// is needed to link to a commit.
fn is_commit_id(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A path inside a repository, not one that climbs out of it or starts somewhere else.
fn is_repo_relative(s: &str) -> bool {
    let parts: Vec<&str> = s.split(['/', '\\']).collect();
    !s.is_empty() && !parts[0].is_empty() && !parts[0].contains(':') && !parts.contains(&"..")
}

/// A path on another computer: `\\\\host\\share`, `//host/share`, or one of Windows's
/// `\\\\?\\` forms. Just touching one can send the user's password hash to that computer.
pub fn is_network_path(p: &str) -> bool {
    let mut c = p.trim_start().chars();
    matches!((c.next(), c.next()), (Some('/' | '\\'), Some('/' | '\\')))
}

pub fn parse_github_prs(v: &Value) -> Vec<PullRequest> {
    let Some(nodes) = v.pointer("/data/repository/pullRequests/nodes").and_then(Value::as_array) else { return Vec::new() };
    nodes
        .iter()
        .map(|n| PullRequest {
            number: n.get("number").and_then(Value::as_u64).unwrap_or(0),
            title: s(n.get("title")),
            url: s(n.get("url")),
            author: s(n.pointer("/author/login")),
            state: s(n.get("state")),
            draft: n.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
            head: s(n.get("headRefName")),
            base: s(n.get("baseRefName")),
            updated: ts(n.get("updatedAt")),
            review_decision: s(n.get("reviewDecision")),
            checks: s(n.pointer("/commits/nodes/0/commit/statusCheckRollup/state")),
            mergeable: s(n.get("mergeable")),
            additions: n.get("additions").and_then(Value::as_u64).unwrap_or(0) as u32,
            deletions: n.get("deletions").and_then(Value::as_u64).unwrap_or(0) as u32,
            requested_reviewers: n
                .pointer("/reviewRequests/nodes")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|r| r.pointer("/requestedReviewer/login").and_then(Value::as_str).map(String::from)).collect())
                .unwrap_or_default(),
            labels: strs(n.pointer("/labels/nodes"), "name"),
            body: s(n.get("body")),
        })
        .collect()
}

pub fn parse_gitlab_mrs(v: &Value) -> Vec<PullRequest> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|n| {
                    let status = s(n.get("detailed_merge_status"));
                    PullRequest {
                        number: n.get("iid").and_then(Value::as_u64).unwrap_or(0),
                        title: s(n.get("title")),
                        url: s(n.get("web_url")),
                        author: s(n.pointer("/author/username")),
                        state: s(n.get("state")),
                        draft: n.get("draft").and_then(Value::as_bool).unwrap_or(false),
                        head: s(n.get("source_branch")),
                        base: s(n.get("target_branch")),
                        updated: ts(n.get("updated_at")),
                        mergeable: if n.get("has_conflicts").and_then(Value::as_bool).unwrap_or(false) {
                            "CONFLICTING".into()
                        } else if status == "mergeable" {
                            "MERGEABLE".into()
                        } else {
                            String::new()
                        },
                        review_decision: if status == "mergeable" { "APPROVED".into() } else { String::new() },
                        requested_reviewers: strs(n.get("reviewers"), "username"),
                        labels: n.get("labels").and_then(Value::as_array).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                        body: s(n.get("description")),
                        ..Default::default()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_bitbucket_pr(n: &Value) -> PullRequest {
    PullRequest {
        number: n.get("id").and_then(Value::as_u64).unwrap_or(0),
        title: s(n.get("title")),
        url: s(n.pointer("/links/html/href")),
        author: s(n.pointer("/author/display_name")),
        state: s(n.get("state")),
        draft: n.get("draft").and_then(Value::as_bool).unwrap_or(false),
        head: s(n.pointer("/source/branch/name")),
        base: s(n.pointer("/destination/branch/name")),
        updated: ts(n.get("updated_on")),
        body: s(n.get("description")),
        ..Default::default()
    }
}

fn parse_gitea_pr(n: &Value) -> PullRequest {
    PullRequest {
        number: n.get("number").and_then(Value::as_u64).unwrap_or(0),
        title: s(n.get("title")),
        url: s(n.get("html_url")),
        author: s(n.pointer("/user/login")),
        state: s(n.get("state")),
        draft: n.get("draft").and_then(Value::as_bool).unwrap_or(false),
        head: s(n.pointer("/head/ref")),
        base: s(n.pointer("/base/ref")),
        updated: ts(n.get("updated_at")),
        mergeable: match n.get("mergeable").and_then(Value::as_bool) {
            Some(true) => "MERGEABLE".into(),
            Some(false) => "CONFLICTING".into(),
            None => String::new(),
        },
        requested_reviewers: strs(n.get("requested_reviewers"), "login"),
        labels: strs(n.get("labels"), "name"),
        body: s(n.get("body")),
        ..Default::default()
    }
}

fn parse_bitbucket_server_pr(n: &Value, repo: &RemoteRepo) -> PullRequest {
    let num = n.get("id").and_then(Value::as_u64).unwrap_or(0);
    let short = |v: Option<&Value>| s(v).trim_start_matches("refs/heads/").to_string();
    PullRequest {
        number: num,
        title: s(n.get("title")),
        url: repo.pr_url(num),
        author: s(n.pointer("/author/user/name")),
        state: s(n.get("state")),
        draft: n.get("draft").and_then(Value::as_bool).unwrap_or(false),
        head: short(n.pointer("/fromRef/id")),
        base: short(n.pointer("/toRef/id")),
        updated: n.get("updatedDate").and_then(Value::as_i64).unwrap_or(0) / 1000,
        requested_reviewers: n.get("reviewers").and_then(Value::as_array).map(|a| a.iter().filter_map(|r| r.pointer("/user/name").and_then(Value::as_str).map(String::from)).collect()).unwrap_or_default(),
        body: s(n.get("description")),
        ..Default::default()
    }
}

fn parse_azure_pr(n: &Value, repo: &RemoteRepo) -> PullRequest {
    let num = n.get("pullRequestId").and_then(Value::as_u64).unwrap_or(0);
    let short = |v: Option<&Value>| s(v).trim_start_matches("refs/heads/").to_string();
    PullRequest {
        number: num,
        title: s(n.get("title")),
        url: repo.pr_url(num),
        author: s(n.pointer("/createdBy/displayName")),
        state: s(n.get("status")),
        draft: n.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: short(n.get("sourceRefName")),
        base: short(n.get("targetRefName")),
        updated: ts(n.get("creationDate")),
        mergeable: match s(n.get("mergeStatus")).as_str() {
            "succeeded" => "MERGEABLE".into(),
            "conflicts" => "CONFLICTING".into(),
            _ => String::new(),
        },
        requested_reviewers: strs(n.get("reviewers"), "displayName"),
        body: s(n.get("description")),
        ..Default::default()
    }
}

// ---- other issue trackers -------------------------------------------------------

/// Trello cards assigned to you. The credential is `apikey:token`.
pub fn trello_issues(base_url: &str, credential: &str) -> Result<Vec<Issue>> {
    let (key, token) = credential.split_once(':').ok_or_else(|| Error::Other("A Trello credential is written as apikey:token.".into()))?;
    let base = if base_url.is_empty() { "https://api.trello.com" } else { base_url.trim_end_matches('/') };
    let url = format!("{base}/1/members/me/cards?filter=visible&fields=name,shortUrl,idShort,dateLastActivity,closed&key={}&token={}", url_encode(key), url_encode(token));
    let mut resp = agent().get(&url).call().map_err(|e| Error::Other(format!("Trello: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(Error::Other(format!("Trello HTTP {status}: {}", text.chars().take(300).collect::<String>())));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| Error::Other(format!("Trello: {e}")))?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|c| Issue {
                    id: format!("#{}", c.get("idShort").and_then(Value::as_u64).unwrap_or(0)),
                    number: c.get("idShort").and_then(Value::as_u64).unwrap_or(0),
                    title: s(c.get("name")),
                    url: s(c.get("shortUrl")),
                    state: "open".into(),
                    updated: ts(c.get("dateLastActivity")),
                    source: "Trello".into(),
                    ..Default::default()
                })
                .collect()
        })
        .unwrap_or_default())
}

/// `server` selects the older API of self-hosted Jira Data Center and Server,
/// where the token is a bearer personal access token.
pub fn jira_issues(base_url: &str, email: &str, token: &str, jql: &str, server: bool) -> Result<Vec<Issue>> {
    let base = base_url.trim_end_matches('/');
    let jql = if jql.trim().is_empty() { "assignee = currentUser() AND resolution = Unresolved ORDER BY updated DESC" } else { jql };
    let path = if server { "rest/api/2/search?" } else { "rest/api/3/search/jql?" };
    let url = format!("{base}/{path}maxResults=50&fields=summary,status,updated,reporter,assignee,labels&jql={}", url_encode(jql));
    let auth = if server { format!("Bearer {token}") } else { format!("Basic {}", base64(&format!("{email}:{token}"))) };
    let mut resp = agent()
        .get(&url)
        .header("authorization", &auth)
        .header("accept", "application/json")
        .call()
        .map_err(|e| Error::Other(format!("Jira: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(Error::Other(format!("Jira HTTP {status}: {}", text.chars().take(300).collect::<String>())));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| Error::Other(format!("Jira: {e}")))?;
    Ok(parse_jira(&v, base))
}

pub fn parse_jira(v: &Value, base: &str) -> Vec<Issue> {
    v.get("issues")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| {
                    let key = s(i.get("key"));
                    Issue {
                        url: format!("{base}/browse/{key}"),
                        id: key,
                        title: s(i.pointer("/fields/summary")),
                        author: s(i.pointer("/fields/reporter/displayName")),
                        state: s(i.pointer("/fields/status/name")),
                        updated: i.pointer("/fields/updated").and_then(Value::as_str).and_then(|d| chrono::DateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S%.3f%z").ok()).map(|d| d.timestamp()).unwrap_or(0),
                        labels: i.pointer("/fields/labels").and_then(Value::as_array).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                        assignees: i.pointer("/fields/assignee/displayName").and_then(Value::as_str).map(|x| vec![x.to_string()]).unwrap_or_default(),
                        source: "Jira".into(),
                        ..Default::default()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn linear_issues(token: &str) -> Result<Vec<Issue>> {
    linear_issues_at("https://api.linear.app/graphql", token)
}

pub fn linear_issues_at(url: &str, token: &str) -> Result<Vec<Issue>> {
    let q = "query{viewer{assignedIssues(first:50,orderBy:updatedAt,filter:{completedAt:{null:true},canceledAt:{null:true}}){nodes{identifier title url updatedAt state{name} creator{name} assignee{name} labels{nodes{name}}}}}}";
    let mut resp = agent()
        .post(url)
        .header("authorization", token)
        .header("content-type", "application/json")
        .send(json!({"query":q}).to_string())
        .map_err(|e| Error::Other(format!("Linear: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status >= 400 {
        return Err(Error::Other(format!("Linear HTTP {status}: {}", text.chars().take(300).collect::<String>())));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| Error::Other(format!("Linear: {e}")))?;
    Ok(v.pointer("/data/viewer/assignedIssues/nodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| Issue {
                    id: s(i.get("identifier")),
                    title: s(i.get("title")),
                    url: s(i.get("url")),
                    author: s(i.pointer("/creator/name")),
                    state: s(i.pointer("/state/name")),
                    updated: ts(i.get("updatedAt")),
                    labels: strs(i.pointer("/labels/nodes"), "name"),
                    assignees: i.pointer("/assignee/name").and_then(Value::as_str).map(|x| vec![x.to_string()]).unwrap_or_default(),
                    source: "Linear".into(),
                    ..Default::default()
                })
                .collect()
        })
        .unwrap_or_default())
}

// ---- autolinks ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

/// Find issue references in text: configured autolinks, `#123` for the repo's
/// host, and bare URLs. Spans are byte ranges, sorted and non-overlapping.
pub fn find_links(text: &str, autolinks: &[Autolink], repo: Option<&RemoteRepo>) -> Vec<LinkSpan> {
    let mut spans: Vec<LinkSpan> = Vec::new();
    let b = text.as_bytes();
    let boundary_before = |i: usize| i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');

    for (i, _) in text.match_indices("http") {
        if !boundary_before(i) || !(text[i..].starts_with("http://") || text[i..].starts_with("https://")) {
            continue;
        }
        let rest = &text[i..];
        let end = rest.find(|c: char| c.is_whitespace() || matches!(c, ')' | '>' | '"' | '\'' | ']')).unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':']);
        spans.push(LinkSpan { start: i, end: i + url.len(), url: url.to_string() });
    }
    for a in autolinks.iter().filter(|a| !a.prefix.is_empty()) {
        for (i, _) in text.match_indices(a.prefix.as_str()) {
            if !boundary_before(i) {
                continue;
            }
            let start = i + a.prefix.len();
            let rest = &text[start..];
            let n = if a.alphanumeric {
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(rest.len())
            } else {
                rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len())
            };
            if n > 0 {
                spans.push(LinkSpan { start: i, end: start + n, url: a.url.replace("<num>", &rest[..n]) });
            }
        }
    }
    if let Some(r) = repo {
        for (i, _) in text.match_indices('#') {
            if !boundary_before(i) {
                continue;
            }
            let rest = &text[i + 1..];
            let n = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if n > 0
                && let Ok(num) = rest[..n].parse::<u64>() {
                    spans.push(LinkSpan { start: i, end: i + 1 + n, url: r.issue_url(num) });
                }
        }
    }
    spans.sort_by_key(|s| s.start);
    let mut out: Vec<LinkSpan> = Vec::new();
    for s in spans {
        if out.last().is_none_or(|l| s.start >= l.end) {
            out.push(s);
        }
    }
    out
}

/// Fetch bytes; `None` on any failure or non-success status (e.g. no gravatar).
pub fn http_get_bytes(url: &str) -> Option<Vec<u8>> {
    let mut r = agent().get(url).call().ok()?;
    if r.status().as_u16() != 200 {
        return None;
    }
    r.body_mut().read_to_vec().ok()
}

/// A web link to a file on any provider, back to (revision, path, line).
pub fn parse_file_url(url: &str) -> Option<(String, String, Option<u32>)> {
    let (no_frag, frag) = url.split_once('#').unwrap_or((url, ""));
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    let after = ["/-/blob/", "/blob/", "/src/commit/", "/src/branch/", "/src/", "/+/"].iter().find_map(|m| no_query.split_once(m).map(|x| x.1))?;
    let (rev, path) = after.split_once('/')?;
    let line = frag.trim_start_matches("lines-").trim_start_matches('L').split(['-', ':']).next().and_then(|n| n.parse().ok());
    let decoded = percent_decode(path);
    Some((rev.to_string(), decoded, line))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        // Bytes, not a slice of `s`: what follows the `%` may be part of a longer character.
        if b[i] == b'%' && i + 2 < b.len()
            && let (Some(hi), Some(lo)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn gravatar_url(email: &str, size: u32) -> String {
    let d = md5::compute(email.trim().to_lowercase().as_bytes());
    format!("https://www.gravatar.com/avatar/{d:x}?s={size}&d=404")
}

fn base64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_percent_escape_is_kept_as_text() {
        assert_eq!(percent_decode("a%20b%C3%A9"), "a b\u{e9}");
        for raw in ["%a\u{e9}", "%\u{e9}a", "%\u{20ac}", "%", "%a", "%zz", "%+1"] {
            assert_eq!(percent_decode(raw), raw);
        }
        assert_eq!(DeepLink::parse("gitem://open?repo=%a\u{e9}").map(|d| d.repo).as_deref(), Some("%a\u{e9}"));
        assert_eq!(parse_file_url("https://h.invalid/o/r/blob/main/%a\u{e9}.txt").map(|x| x.1).as_deref(), Some("%a\u{e9}.txt"));
    }

    #[test]
    fn remote_url_forms() {
        for (u, h, p) in [
            ("git@github.com:o/r.git", "github.com", "o/r"),
            ("https://github.com/o/r", "github.com", "o/r"),
            ("https://user:tok@github.com/o/r.git", "github.com", "o/r"),
            ("ssh://git@gitlab.example.com:2222/g/s/r.git", "gitlab.example.com", "g/s/r"),
        ] {
            assert_eq!(parse_remote_url(u), Some((h.to_string(), p.to_string())), "{u}");
        }
        assert_eq!(parse_remote_url("C:/repos/x"), None);
        assert_eq!(parse_remote_url("file:///c/repos/x"), None);
    }

    #[test]
    fn identify_providers() {
        assert_eq!(identify("git@github.com:o/r.git", &[]).unwrap().kind, ForgeKind::GitHub);
        assert_eq!(identify("https://gitlab.com/g/s/r", &[]).unwrap().path, "g/s/r");
        assert_eq!(identify("git@bitbucket.org:o/r.git", &[]).unwrap().kind, ForgeKind::Bitbucket);
        assert_eq!(identify("https://codeberg.org/o/r", &[]).unwrap().kind, ForgeKind::Gitea);
        let az = identify("https://dev.azure.com/org/proj/_git/repo", &[]).unwrap();
        assert_eq!(az.kind, ForgeKind::AzureDevOps);
        assert_eq!(az.path, "org/proj/repo");
        assert_eq!(az.web_url(), "https://dev.azure.com/org/proj/_git/repo");
        let az2 = identify("git@ssh.dev.azure.com:v3/org/proj/repo", &[]).unwrap();
        assert_eq!(az2.path, "org/proj/repo");
        assert!(identify("https://example.com/o/r", &[]).is_none());
        let c = vec![CustomHost { host: "git.corp".into(), kind: ForgeKind::GitLab, api_base: "https://git.corp/api/v4/".into(), templates: UrlTemplates::default() }];
        let r = identify("git@git.corp:a/b.git", &c).unwrap();
        assert_eq!(r.kind, ForgeKind::GitLab);
        assert_eq!(r.api_base.as_deref(), Some("https://git.corp/api/v4"));
    }

    #[test]
    fn custom_url_templates() {
        let templates = UrlTemplates { commit: "https://git.corp/${repo}/c/${sha}".into(), file: "https://git.corp/f/${file}?r=${sha}#${line}".into(), ..Default::default() };
        let c = vec![CustomHost { host: "git.corp".into(), kind: ForgeKind::Gerrit, api_base: String::new(), templates }];
        let r = identify("git@git.corp:a/b.git", &c).unwrap();
        assert_eq!(r.commit_url("abc"), "https://git.corp/a/b/c/abc");
        assert_eq!(r.file_url("abc", "x y", Some((3, 3))), "https://git.corp/f/x%20y?r=abc#3");
        assert_eq!(r.branch_url("m"), "https://git.corp/a/b/+/refs/heads/m", "blank template uses the provider's link");
    }

    #[test]
    fn suggestions_cover_replacements_and_insertions() {
        let d = crate::diff::parse_unified("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n one\n-two\n+TWO\n three\n+added\n four\n");
        let s = suggestions_from_diff(&d);
        assert_eq!(s[0], Suggestion { path: "f".into(), start: 2, end: 2, lines: vec!["TWO".into()] });
        assert_eq!(s[1], Suggestion { path: "f".into(), start: 3, end: 3, lines: vec!["three".into(), "added".into()] });
    }

    #[test]
    fn deep_links_round_trip() {
        let d = DeepLink { repo: "C:/my repo".into(), commit: Some("abcd".into()), file: Some("a b.rs".into()), line: Some(4) };
        assert_eq!(DeepLink::parse(&d.to_url()), Some(d));
        assert_eq!(DeepLink::parse("https://x"), None);
        assert_eq!(DeepLink::parse("gitem://open?commit=abc"), None, "a repo is required");
        for bad in ["--output=C:/x", "HEAD", "abc", "abcd --all", "abcg", "main", "%2D%2Doutput%3Dx"] {
            assert_eq!(DeepLink::parse(&format!("gitem://open?repo=r&commit={bad}")), None, "commit {bad}");
        }
        for good in ["abcd", "ABCD1234", "0123456789012345678901234567890123456789"] {
            assert_eq!(DeepLink::parse(&format!("gitem://open?repo=r&commit={good}")).and_then(|d| d.commit).as_deref(), Some(good));
        }
        for bad in ["../secret", "a/../../b", "/etc/passwd", "C:/x", "%5Cx", "a%5C..%5C..%5Cb"] {
            assert_eq!(DeepLink::parse(&format!("gitem://open?repo=r&file={bad}")), None, "file {bad}");
        }
        assert_eq!(DeepLink::parse("gitem://open?repo=r&file=src/a.rs").and_then(|d| d.file).as_deref(), Some("src/a.rs"));
    }

    #[test]
    fn network_paths_are_told_from_local_ones() {
        for p in ["\\\\evil\\share\\repo", "//evil/share/repo", "\\/evil/x", "/\\evil", "\\\\?\\UNC\\evil\\x", " \\\\evil\\x"] {
            assert!(is_network_path(p), "{p}");
        }
        for p in ["C:/repo", "C:\\repo", "/home/me/repo", "repo", "./repo", "\\repo"] {
            assert!(!is_network_path(p), "{p}");
        }
    }

    #[test]
    fn more_providers() {
        let g = identify("https://android.googlesource.com/platform/art", &[]).unwrap();
        assert_eq!(g.kind, ForgeKind::GoogleSource);
        assert_eq!(g.commit_url("abc"), "https://android.googlesource.com/platform/art/+/abc");
        assert!(!g.kind.has_api());
        let c = vec![CustomHost { host: "bb.corp".into(), kind: ForgeKind::BitbucketServer, api_base: String::new(), templates: UrlTemplates::default() }];
        let b = identify("https://bb.corp/scm/PROJ/repo.git", &c).unwrap();
        assert_eq!(b.commit_url("abc"), "https://bb.corp/projects/PROJ/repos/repo/commits/abc");
        assert_eq!(b.pr_ref(4), "refs/pull-requests/4/from");
        assert_eq!(b.file_url("abc", "a b", Some((2, 2))), "https://bb.corp/projects/PROJ/repos/repo/browse/a%20b?at=abc#2");
    }

    #[test]
    fn links_parse_back_to_files() {
        assert_eq!(parse_file_url("https://github.com/o/r/blob/abc/src/a%20b.rs#L12-L14"), Some(("abc".into(), "src/a b.rs".into(), Some(12))));
        assert_eq!(parse_file_url("https://gitlab.com/g/r/-/blob/main/x.rs#L3"), Some(("main".into(), "x.rs".into(), Some(3))));
        assert_eq!(parse_file_url("https://bitbucket.org/o/r/src/abc/x.rs#lines-5"), Some(("abc".into(), "x.rs".into(), Some(5))));
        assert_eq!(parse_file_url("https://example.com/nothing"), None);
    }

    #[test]
    fn branch_name_template() {
        let i = Issue { id: "ABC-1".into(), title: "Update Login".into(), ..Default::default() };
        assert_eq!(i.branch_name_as("feature/${id}/${title}"), "feature/abc-1/update-login");
        assert_eq!(i.branch_name_as(""), "abc-1-update-login");
    }

    #[test]
    fn urls() {
        let g = identify("git@github.com:o/r.git", &[]).unwrap();
        assert_eq!(g.commit_url("abc"), "https://github.com/o/r/commit/abc");
        assert_eq!(g.file_url("abc", "src/a b.rs", Some((3, 5))), "https://github.com/o/r/blob/abc/src/a%20b.rs#L3-L5");
        assert_eq!(g.new_pr_url("main", "f/x"), "https://github.com/o/r/compare/main...f/x?expand=1");
        assert_eq!(g.pr_ref(7), "refs/pull/7/head");
        let l = identify("https://gitlab.com/g/r", &[]).unwrap();
        assert_eq!(l.commit_url("abc"), "https://gitlab.com/g/r/-/commit/abc");
        assert_eq!(l.pr_ref(7), "refs/merge-requests/7/head");
        let b = identify("git@bitbucket.org:o/r.git", &[]).unwrap();
        assert_eq!(b.file_url("abc", "a", Some((1, 1))), "https://bitbucket.org/o/r/src/abc/a#lines-1");
    }

    #[test]
    fn github_pr_parsing() {
        let v = json!({"data":{"repository":{"pullRequests":{"nodes":[{
            "number":5,"title":"T","url":"U","isDraft":false,"state":"OPEN","author":{"login":"ann"},
            "headRefName":"f","baseRefName":"main","mergeable":"MERGEABLE","reviewDecision":"APPROVED",
            "additions":3,"deletions":1,"updatedAt":"2024-01-02T03:04:05Z",
            "labels":{"nodes":[{"name":"bug"}]},
            "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"bob"}}]},
            "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"SUCCESS"}}}]}
        }]}}}});
        let p = parse_github_prs(&v);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].author, "ann");
        assert_eq!(p[0].checks, "SUCCESS");
        assert_eq!(p[0].requested_reviewers, vec!["bob"]);
        assert_eq!(p[0].labels, vec!["bug"]);
        assert_eq!(launchpad_group(&p[0], "ann"), LaunchpadGroup::Mergeable);
        assert_eq!(launchpad_group(&p[0], "bob"), LaunchpadGroup::NeedsYourReview);
        assert_eq!(launchpad_group(&p[0], "eve"), LaunchpadGroup::Other);
    }

    #[test]
    fn launchpad_blocked_and_draft() {
        let mut p = PullRequest { author: "me".into(), checks: "FAILURE".into(), ..Default::default() };
        assert_eq!(launchpad_group(&p, "me"), LaunchpadGroup::Blocked);
        p.draft = true;
        assert_eq!(launchpad_group(&p, "me"), LaunchpadGroup::Draft);
    }

    #[test]
    fn gitlab_mr_parsing() {
        let v = json!([{"iid":9,"title":"T","web_url":"U","author":{"username":"a"},"source_branch":"f","target_branch":"m","has_conflicts":true,"state":"opened"}]);
        let p = parse_gitlab_mrs(&v);
        assert_eq!(p[0].number, 9);
        assert_eq!(p[0].mergeable, "CONFLICTING");
    }

    #[test]
    fn jira_parsing() {
        let v = json!({"issues":[{"key":"PROJ-1","fields":{"summary":"S","status":{"name":"Open"},"labels":["x"],"updated":"2024-01-02T03:04:05.000+0000"}}]});
        let i = parse_jira(&v, "https://j");
        assert_eq!(i[0].id, "PROJ-1");
        assert_eq!(i[0].url, "https://j/browse/PROJ-1");
        assert!(i[0].updated > 0);
    }

    #[test]
    fn autolinks() {
        let g = identify("git@github.com:o/r.git", &[]).unwrap();
        let al = vec![Autolink { prefix: "JIRA-".into(), url: "https://j/<num>".into(), alphanumeric: false }];
        let t = "Fix JIRA-12 and #34, see https://x.y/z. Not a#5";
        let l = find_links(t, &al, Some(&g));
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!(l[0].url, "https://j/12");
        assert_eq!(l[1].url, "https://github.com/o/r/issues/34");
        assert_eq!(l[2].url, "https://x.y/z");
        assert_eq!(&t[l[0].start..l[0].end], "JIRA-12");
    }

    #[test]
    fn branch_names() {
        let i = Issue { id: "#12".into(), title: "Fix: the  Big Bug!".into(), ..Default::default() };
        assert_eq!(i.branch_name(), "12-fix-the-big-bug");
        let j = Issue { id: "PROJ-3".into(), title: "x".into(), ..Default::default() };
        assert_eq!(j.branch_name(), "proj-3-x");
    }

    #[test]
    fn b64() {
        assert_eq!(base64("user:pw"), "dXNlcjpwdw==");
        assert_eq!(base64("abc"), "YWJj");
        assert_eq!(base64("ab"), "YWI=");
    }

    #[test]
    fn gravatar() {
        assert_eq!(gravatar_url(" MyEmailAddress@example.com ", 80), "https://www.gravatar.com/avatar/0bc83cb571cd1c50ba6f3e8a78ef1346?s=80&d=404");
    }
}
