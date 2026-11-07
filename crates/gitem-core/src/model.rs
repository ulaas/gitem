//! Plain data types describing repository state.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub commit_time: i64,
    pub subject: String,
    pub body: String,
    pub refs: Vec<RefLabel>,
}

impl Commit {
    pub fn short(&self) -> &str {
        crate::util::prefix(&self.sha, 8)
    }
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
    pub fn message(&self) -> String {
        if self.body.trim().is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body.trim_end())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Head,
    Branch,
    Remote,
    Tag,
    Stash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    pub kind: RefKind,
    pub name: String,
    /// True for the branch `HEAD` points at.
    pub current: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub upstream_gone: bool,
    pub ahead: u32,
    pub behind: u32,
    pub date: i64,
    pub subject: String,
    pub author: String,
    /// Path of the worktree holding this branch, if any.
    pub worktree: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Tag {
    pub name: String,
    pub sha: String,
    pub annotated: bool,
    pub date: i64,
    pub subject: String,
    pub tagger: String,
}

#[derive(Debug, Clone, Default)]
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct Stash {
    pub index: usize,
    pub selector: String,
    pub sha: String,
    pub message: String,
    pub branch: String,
    pub date: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Worktree {
    pub path: String,
    pub head: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
    pub is_main: bool,
    pub is_current: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Submodule {
    pub path: String,
    pub sha: String,
    pub describe: String,
    /// ' ' clean, '-' uninitialised, '+' different commit, 'U' conflict.
    pub state: char,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatus {
    Unmodified,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Ignored,
}

impl FileStatus {
    pub fn from_code(c: char) -> Self {
        match c {
            'A' => Self::Added,
            'M' => Self::Modified,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            'U' => Self::Unmerged,
            '?' => Self::Untracked,
            '!' => Self::Ignored,
            _ => Self::Unmodified,
        }
    }
    pub fn letter(self) -> char {
        match self {
            Self::Unmodified => ' ',
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::TypeChanged => 'T',
            Self::Unmerged => 'U',
            Self::Untracked => '?',
            Self::Ignored => '!',
        }
    }
}

/// One entry of `git status --porcelain=v2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub index: FileStatus,
    pub worktree: FileStatus,
    /// Two-letter conflict code such as `UU`, `AA`, `DU`.
    pub conflict: Option<String>,
    pub submodule: bool,
}

impl StatusEntry {
    pub fn is_staged(&self) -> bool {
        self.conflict.is_none() && !matches!(self.index, FileStatus::Unmodified | FileStatus::Untracked | FileStatus::Ignored)
    }
    pub fn is_unstaged(&self) -> bool {
        self.conflict.is_none() && !matches!(self.worktree, FileStatus::Unmodified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoOp {
    #[default]
    None,
    Merge,
    Rebase,
    RebaseInteractive,
    CherryPick,
    Revert,
    Bisect,
    Am,
}

impl RepoOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Merge => "Merging",
            Self::Rebase => "Rebasing",
            Self::RebaseInteractive => "Interactive rebase",
            Self::CherryPick => "Cherry-picking",
            Self::Revert => "Reverting",
            Self::Bisect => "Bisecting",
            Self::Am => "Applying mailbox",
        }
    }
    /// The git sub-command used for `--continue` / `--abort` / `--skip`.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Self::Merge => Some("merge"),
            Self::Rebase | Self::RebaseInteractive => Some("rebase"),
            Self::CherryPick => Some("cherry-pick"),
            Self::Revert => Some("revert"),
            Self::Am => Some("am"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepoStatus {
    pub head_sha: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<StatusEntry>,
    pub op: RepoOp,
    /// Progress such as (3, 7) during a rebase.
    pub op_progress: Option<(u32, u32)>,
    /// What is being merged/picked, for display.
    pub op_target: Option<String>,
    /// The operation is waiting on the user and ordinary commits are part of it, as at a
    /// rebase edit step. Conflicts still come first.
    pub op_allows_commits: bool,
}

impl RepoStatus {
    pub fn conflicts(&self) -> impl Iterator<Item = &StatusEntry> {
        self.entries.iter().filter(|e| e.conflict.is_some())
    }
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One line of `git blame`.
#[derive(Debug, Clone, Default)]
pub struct BlameLine {
    pub sha: String,
    pub orig_line: u32,
    pub final_line: u32,
    pub author: String,
    pub author_email: String,
    pub author_time: i64,
    pub summary: String,
    pub orig_path: Option<String>,
    pub text: String,
    pub boundary: bool,
}

impl BlameLine {
    /// All-zero sha is how git marks lines not yet committed.
    pub fn is_uncommitted(&self) -> bool {
        self.sha.bytes().all(|b| b == b'0')
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: FileStatus,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ReflogEntry {
    pub selector: String,
    pub sha: String,
    pub action: String,
    pub message: String,
    pub date: i64,
    pub author: String,
}

#[derive(Debug, Clone, Default)]
pub struct Contributor {
    pub name: String,
    pub email: String,
    pub commits: u32,
    pub additions: u64,
    pub deletions: u64,
    pub first: i64,
    pub last: i64,
    pub is_you: bool,
}

/// A point on the visual file history chart.
#[derive(Debug, Clone, Default)]
pub struct FileHistoryPoint {
    pub sha: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    pub additions: u32,
    pub deletions: u32,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConfigEntry {
    pub scope: String,
    pub origin: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct HookInfo {
    pub name: String,
    pub path: String,
    pub enabled: bool,
    pub exists: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BisectState {
    pub active: bool,
    pub good: Vec<String>,
    pub bad: Option<String>,
    pub log: String,
}

/// One tuning option of Repository Health.
#[derive(Debug, Clone, Default)]
pub struct HealthItem {
    pub label: String,
    pub detail: String,
    pub config: Vec<(String, String)>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RepoHealth {
    pub tracked_files: usize,
    pub commits: u64,
    pub loose_objects: String,
    pub pack_size: String,
    pub items: Vec<HealthItem>,
}

#[derive(Debug, Clone, Default)]
pub struct LfsFile {
    pub oid: String,
    pub path: String,
    pub downloaded: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureInfo {
    /// `G` good, `B` bad, `U` untrusted, `N` none, ...
    pub status: String,
    pub signer: String,
    pub key: String,
}

/// Filters for `git log`. Every field maps to a git flag.
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    pub revs: Vec<String>,
    pub all: bool,
    pub limit: usize,
    pub skip: usize,
    pub author: Option<String>,
    pub grep: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub paths: Vec<String>,
    pub follow: bool,
    pub first_parent: bool,
    pub no_merges: bool,
    pub pickaxe_s: Option<String>,
    pub pickaxe_g: Option<String>,
    pub regex_ignore_case: bool,
    pub line_range: Option<(u32, u32, String)>,
    /// Exclude refs/stash from `--all` (the graph adds stashes explicitly).
    pub exclude_stash: bool,
    pub committer: Option<String>,
    pub merges_only: bool,
    /// Commits whose message does NOT match `grep`.
    pub invert_grep: bool,
    /// Every message term must match, not just one.
    pub all_match: bool,
    /// Treat message terms as plain text rather than regular expressions.
    pub fixed_strings: bool,
    /// `paths` are exact file names, not search patterns.
    pub literal_paths: bool,
    /// Ref globs passed as `--exclude=<glob>` before `--all`.
    pub excludes: Vec<String>,
    /// No parent before all its children, which the graph's lanes rely on. It makes git walk
    /// the whole history first, whatever the limit, so only the graph asks for it.
    pub date_order: bool,
}
