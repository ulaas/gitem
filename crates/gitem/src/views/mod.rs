//! All dockable views. A `Tab` wraps one view; `Tab::key` de-duplicates opening.
use egui::Ui;
use gitem_core::repo::DiffSource;

use crate::action::Cx;

pub mod agents;
pub mod changes;
pub mod changes_tree;
pub mod composer;
pub mod console;
pub mod contributors;
pub mod credentials;
pub mod diffview;
pub mod fileview;
pub mod filetree;
pub mod graph;
pub mod graphcols;
pub mod health;
pub mod history;
pub mod home;
pub mod inspect;
pub mod launchpad;
pub mod mergeview;
pub mod patches;
pub mod rebase;
pub mod reflog;
pub mod review;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod textview;
pub mod tools;
pub mod tree;
pub mod treemap;
pub mod visual;
pub mod worktrees;

macro_rules! tabs {
    ($($var:ident($ty:ty)),* $(,)?) => {
        pub enum Tab { $($var(Box<$ty>)),* }
        impl Tab {
            pub fn key(&self) -> String { match self { $(Tab::$var(v) => v.key()),* } }
            pub fn title(&self) -> String { match self { $(Tab::$var(v) => v.title()),* } }
            pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) { match self { $(Tab::$var(v) => v.ui(ui, cx)),* } }
        }
    };
}

tabs! {
    Home(home::HomeView),
    Graph(graph::GraphView),
    Changes(changes::ChangesView),
    Diff(diffview::DiffView),
    Merge(mergeview::MergeView),
    File(fileview::FileView),
    History(history::HistoryView),
    Visual(visual::VisualView),
    Search(search::SearchView),
    Rebase(rebase::RebaseView),
    Launchpad(launchpad::LaunchpadView),
    Reflog(reflog::ReflogView),
    Bisect(reflog::BisectView),
    Contributors(contributors::ContributorsView),
    Worktrees(worktrees::WorktreesView),
    Tools(tools::ToolsView),
    Config(tools::ConfigView),
    Credentials(credentials::CredentialsView),
    Settings(settings::SettingsView),
    Console(console::ConsoleView),
    Patches(patches::PatchesView),
    Composer(composer::ComposerView),
    Tree(tree::TreeView),
    Health(health::HealthView),
    Treemap(treemap::TreemapView),
    Text(textview::TextView),
    Agents(agents::AgentsView),
    Review(review::ReviewView),
}

impl Tab {
    /// What closing this tab would lose, in a line for the user. `None` if nothing.
    pub fn unsaved(&self) -> Option<String> {
        match self {
            Tab::Merge(v) => v.unsaved(),
            Tab::Changes(v) => v.unsaved(),
            Tab::Composer(v) => v.unsaved(),
            _ => None,
        }
    }

    /// A short description of this tab for restoring the session. Tabs holding
    /// one-off state (diffs, merges, AI results) aren't restored.
    pub fn spec(&self) -> Option<String> {
        Some(match self {
            Tab::Home(_) => "home".into(),
            Tab::Graph(_) => "graph".into(),
            Tab::Changes(_) => "changes".into(),
            Tab::Launchpad(_) => "launchpad".into(),
            Tab::Worktrees(_) => "worktrees".into(),
            Tab::Contributors(_) => "contributors".into(),
            Tab::Reflog(_) => "reflog".into(),
            Tab::Bisect(_) => "bisect".into(),
            Tab::Patches(_) => "patches".into(),
            Tab::Composer(_) => "composer".into(),
            Tab::Tools(_) => "tools".into(),
            Tab::Config(_) => "config".into(),
            Tab::Credentials(_) => "credentials".into(),
            Tab::Console(_) => "console".into(),
            Tab::Settings(_) => "settings".into(),
            Tab::Treemap(_) => "treemap".into(),
            Tab::Health(_) => "health".into(),
            Tab::Agents(_) => "agents".into(),
            Tab::Review(_) => "review".into(),
            Tab::File(v) if v.rev.is_none() => format!("file:{}", v.path),
            _ => return None,
        })
    }

    pub fn from_spec(s: &str) -> Option<Self> {
        Some(match s {
            "home" => Tab::home(),
            "graph" => Tab::graph(),
            "changes" => Tab::changes(),
            "launchpad" => Tab::Launchpad(Box::default()),
            "worktrees" => Tab::Worktrees(Box::default()),
            "contributors" => Tab::Contributors(Box::default()),
            "reflog" => Tab::Reflog(Box::default()),
            "bisect" => Tab::Bisect(Box::default()),
            "patches" => Tab::Patches(Box::default()),
            "composer" => Tab::Composer(Box::default()),
            "tools" => Tab::Tools(Box::default()),
            "config" => Tab::Config(Box::default()),
            "credentials" => Tab::Credentials(Box::default()),
            "console" => Tab::Console(Box::default()),
            "settings" => Tab::Settings(Box::default()),
            "treemap" => Tab::Treemap(Box::default()),
            "health" => Tab::Health(Box::default()),
            "agents" => Tab::Agents(Box::default()),
            "review" => Tab::Review(Box::default()),
            other => Tab::file(other.strip_prefix("file:")?, None, None),
        })
    }

    pub fn home() -> Self {
        Tab::Home(Box::default())
    }
    pub fn graph() -> Self {
        Tab::Graph(Box::default())
    }
    pub fn changes() -> Self {
        Tab::Changes(Box::default())
    }
    pub fn diff(src: DiffSource, path: Option<String>) -> Self {
        Tab::Diff(Box::new(diffview::DiffView::new(src, path)))
    }
    pub fn file(path: &str, rev: Option<String>, line: Option<u32>) -> Self {
        Tab::File(Box::new(fileview::FileView::new(path, rev, line)))
    }
    pub fn file_history(path: &str) -> Self {
        Tab::History(Box::new(history::HistoryView::for_file(path)))
    }
    pub fn line_history(path: &str, a: u32, b: u32) -> Self {
        Tab::History(Box::new(history::HistoryView::for_lines(path, a, b)))
    }
    pub fn visual(path: &str) -> Self {
        Tab::Visual(Box::new(visual::VisualView::new(path)))
    }
    pub fn merge(path: &str) -> Self {
        Tab::Merge(Box::new(mergeview::MergeView::for_repo_path(path)))
    }
    pub fn compare(a: &str, b: &str) -> Self {
        Tab::Search(Box::new(search::SearchView::compare(a, b)))
    }
    pub fn search(q: &str) -> Self {
        Tab::Search(Box::new(search::SearchView::search(q)))
    }
    pub fn tree(rev: &str) -> Self {
        Tab::Tree(Box::new(tree::TreeView::new(rev)))
    }
    pub fn rebase(base: Option<String>) -> Self {
        Tab::Rebase(Box::new(rebase::RebaseView::new(base)))
    }
}
