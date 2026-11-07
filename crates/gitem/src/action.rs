//! The UI's message vocabulary. Views never mutate shared state directly: they
//! push `Action`s through `Cx`, and the app applies them between frames.
use std::path::PathBuf;
use std::sync::Arc;

use gitem_core::Repo;
use gitem_core::forge::RemoteRepo;
use gitem_core::model::*;
use gitem_core::ops::GitOp;
use gitem_core::repo::DiffSource;
use gitem_core::settings::Settings;

use crate::data::RepoData;
use crate::forms::FormDialog;
use crate::theme::Palette;
use crate::views::Tab;

pub enum Action {
    /// Show the About box.
    About,
    /// Open settings, with or without a repository.
    OpenSettings,
    /// Run a write op on this repo's queue.
    Run(GitOp),
    /// Take back the last thing done in this repository, if that's still possible.
    UndoLast,
    /// Ask first, then run.
    Confirm { title: String, body: String, label: String, op: GitOp },
    Form(FormDialog),
    /// A write in this repository stopped at its index lock: offer a way out, with why.
    OfferLockRemoval(PathBuf, String),
    /// A tab asked to close with work in it. `key` names the tab, `lost` what would go.
    AskCloseTab { key: String, lost: String },
    OpenTab(Tab),
    /// Show a commit in the Inspect panel.
    Inspect(String),
    /// The same, unless one's showing already.
    InspectFirst(String),
    OpenUrl(String),
    /// Read the repository again, but not what's on its host.
    ReloadRepository,
    Copy(String),
    Toast(String),
    Error(String),
    OpenRepo(PathBuf),
    /// Run on this repo's queue, then open a repository if it succeeded.
    RunThenOpen(GitOp, PathBuf),
    /// Network or other slow work off the UI thread. Its message becomes a toast
    /// and the repository refreshes afterwards.
    Background(Box<dyn FnOnce() -> Result<String, String> + Send>),
    /// Slow work off the UI thread that ends in something to do, by the repository that was
    /// active when it was asked for.
    Later(Box<dyn FnOnce() -> Result<Later, String> + Send>),
    /// Run outside the queue: it ends when something else does, such as a tool the user has open.
    RunDetached(GitOp),
    /// Give up on any read that's stuck, and read the repository again.
    Retry,
    /// Open a working-tree file in the configured editor, or reveal it.
    OpenFile(PathBuf),
    /// Run an op outside any repo (clone, init); then open the path on success.
    RunGlobal { op: GitOp, open_after: Option<PathBuf> },
    Refresh,
    /// Set the text in the status-bar blame slot.
    StatusBlame(String),
    Settings(Box<dyn FnOnce(&mut Settings) + Send>),
    OpenPalette,
    /// Open a `gitem://` link.
    DeepLink(gitem_core::forge::DeepLink),
    /// Show again the question about what this repository's config may run.
    ReviewTrust,
    /// Show only this ref in the commit graph.
    GraphFocus(String),
    /// Hide this ref from the commit graph.
    GraphHide(String),
    /// Hide everything but the view, or bring it all back.
    ToggleZen,
    /// Open a terminal in a folder, optionally running a command there.
    OpenTerminal(PathBuf, Option<String>),
    /// Close this repository's tab, asking first if that would lose something.
    CloseRepo(PathBuf),
    /// Its folder has gone. Ask where to, and open it from there.
    LocateRepo(PathBuf),
    /// A view from the rail's More list, which takes over from the last one.
    OpenFromMore(Tab),
    /// The open pull request for a branch, for the status bar.
    BranchPr(Option<(String, u64, String)>),
    /// Something changed on the hosting provider, so its lists are out of date.
    ForgeChanged(String),
    /// Look for repositories in a folder, and offer what's found.
    ScanRepos(PathBuf),
    /// These, in order.
    Many(Vec<Action>),
    /// Apply an action to a specific repository, whichever one is showing now.
    ForRepo(PathBuf, Box<Action>),
}

/// What slow work comes back with. Plain data, since it's made on another thread.
pub enum Later {
    Copy(String),
    Toast(String),
    Run(GitOp),
    RunGlobal(GitOp),
    Diff(DiffSource, Option<String>),
    /// Built back on the UI thread, from what the work found.
    Form(Box<dyn FnOnce() -> FormDialog + Send>),
    Tab(Box<dyn FnOnce() -> Tab + Send>),
    Confirm { title: String, body: String, label: String, op: GitOp },
    /// A toast, and the hosting provider's lists are out of date.
    ForgeChanged(String),
    Settings(Box<dyn FnOnce(&mut Settings) + Send>),
    /// These, in order.
    Many(Vec<Later>),
}

impl Later {
    pub fn into_action(self) -> Action {
        match self {
            Later::Copy(s) => Action::Copy(s),
            Later::Toast(s) => Action::Toast(s),
            Later::Run(op) => Action::Run(op),
            Later::RunGlobal(op) => Action::RunGlobal { op, open_after: None },
            Later::Diff(src, path) => Action::OpenTab(Tab::diff(src, path)),
            Later::Form(build) => Action::Form(build()),
            Later::Tab(build) => Action::OpenTab(build()),
            Later::Confirm { title, body, label, op } => Action::Confirm { title, body, label, op },
            Later::ForgeChanged(s) => Action::ForgeChanged(s),
            Later::Settings(f) => Action::Settings(f),
            Later::Many(all) => Action::Many(all.into_iter().map(Later::into_action).collect()),
        }
    }
}

/// Everything a view needs for one frame.
pub struct Cx<'a> {
    pub repo: &'a Repo,
    pub data: &'a RepoData,
    pub settings: &'a Settings,
    pub pal: &'a Palette,
    pub out: &'a mut Vec<Action>,
    /// A dialog is showing, drawn after the views and with its own keyboard shortcuts.
    pub dialog_open: bool,
    /// This is the view the user is working in, when several panes are showing. A key press
    /// outside a text field belongs to one of them.
    pub in_front: bool,
}

impl Cx<'_> {
    pub fn run(&mut self, op: GitOp) {
        self.out.push(Action::Run(op));
    }

    /// Destructive op: asks first, unless confirmations are turned off.
    pub fn confirm(&mut self, title: &str, body: &str, label: &str, op: GitOp) {
        if self.settings.confirm_destructive {
            self.out.push(Action::Confirm { title: title.into(), body: body.into(), label: label.into(), op });
        } else {
            self.out.push(Action::Run(op));
        }
    }

    pub fn form(&mut self, f: FormDialog) {
        self.out.push(Action::Form(f));
    }

    pub fn open(&mut self, t: Tab) {
        self.out.push(Action::OpenTab(t));
    }

    pub fn inspect(&mut self, sha: &str) {
        self.out.push(Action::Inspect(sha.to_string()));
    }

    pub fn copy(&mut self, s: impl Into<String>) {
        self.out.push(Action::Copy(s.into()));
    }

    pub fn url(&mut self, s: impl Into<String>) {
        self.out.push(Action::OpenUrl(s.into()));
    }

    pub fn toast(&mut self, s: impl Into<String>) {
        self.out.push(Action::Toast(s.into()));
    }

    pub fn error(&mut self, s: impl Into<String>) {
        self.out.push(Action::Error(s.into()));
    }

    pub fn diff(&mut self, src: DiffSource, path: Option<&str>) {
        self.open(Tab::diff(src, path.map(String::from)));
    }

    /// The hosted repo for `remote`, falling back to the preferred remote.
    pub fn forge(&self) -> Option<&RemoteRepo> {
        self.data.forge()
    }

    pub fn status(&self) -> Option<&RepoStatus> {
        self.data.status.get().map(|s| &**s)
    }

    pub fn current_branch(&self) -> Option<String> {
        self.status().and_then(|s| s.branch.clone())
    }

    pub fn repo_arc(&self) -> Arc<Repo> {
        Arc::new(self.repo.clone())
    }
}
