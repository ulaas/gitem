//! Persistent app settings (JSON in the user config dir). Secrets never live
//! here; they go through [`crate::cred`].
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ai::AiConfig;
use crate::forge::ForgeKind;

pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("GITEM_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("dev", "gitem", "gitem").map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".gitem"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Workspace {
    pub name: String,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Autolink {
    /// Plain prefix such as `JIRA-` or `#`; the number that follows is the id.
    pub prefix: String,
    /// URL template with `<num>` as a placeholder.
    pub url: String,
    #[serde(default)]
    pub alphanumeric: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomHost {
    pub host: String,
    pub kind: ForgeKind,
    /// API base override, e.g. `https://ghe.corp/api/v3`.
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub templates: crate::forge::UrlTemplates,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct IssueTrackerConfig {
    #[serde(default)]
    pub jira_base_url: String,
    #[serde(default)]
    pub jira_email: String,
    #[serde(default)]
    pub jira_jql: String,
    #[serde(default)]
    pub linear_enabled: bool,
    /// Self-hosted Jira Data Center or Server rather than Jira Cloud.
    #[serde(default)]
    pub jira_server: bool,
    #[serde(default)]
    pub trello_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
    Midnight,
    Black,
    Slate,
    Forest,
    Plum,
    Mocha,
    Paper,
    Mist,
    Sage,
    Blush,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 12] = [Self::Dark, Self::Midnight, Self::Black, Self::Slate, Self::Forest, Self::Plum, Self::Mocha, Self::Light, Self::Paper, Self::Mist, Self::Sage, Self::Blush];

    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Midnight => "Midnight",
            Self::Black => "Black",
            Self::Slate => "Slate",
            Self::Forest => "Forest",
            Self::Plum => "Plum",
            Self::Mocha => "Mocha",
            Self::Paper => "Paper",
            Self::Mist => "Mist",
            Self::Sage => "Sage",
            Self::Blush => "Blush",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub git_path: String,
    pub theme: ThemeChoice,
    /// A named accent, "#rrggbb", or empty for the theme's own.
    pub accent: String,
    pub ui_scale: f32,
    pub editor_font_size: f32,
    pub recent_repos: Vec<String>,
    pub open_repos: Vec<String>,
    pub workspaces: Vec<Workspace>,
    pub autolinks: Vec<Autolink>,
    pub custom_hosts: Vec<CustomHost>,
    pub ai: AiConfig,
    pub issue_trackers: IssueTrackerConfig,
    // Blame presentation
    pub inline_blame: bool,
    pub inline_blame_format: String,
    pub code_lens: bool,
    pub blame_ignore_ws: bool,
    pub blame_detect_moves: bool,
    pub heatmap_age_days: u32,
    // Avatars
    pub gravatar: bool,
    // Behaviour
    pub auto_fetch_minutes: u32,
    pub confirm_destructive: bool,
    pub commit_sign: bool,
    pub commit_signoff: bool,
    pub prune_on_fetch: bool,
    pub pull_mode: String,
    pub graph_page_size: usize,
    pub default_external_editor: String,
    pub date_relative: bool,
    pub favorite_branches: Vec<String>,
    /// Launchpad: "owner/repo#12" keys.
    pub pinned_prs: Vec<String>,
    /// Launchpad: key and the time it comes back.
    pub snoozed_prs: Vec<(String, i64)>,
    pub start_work_format: String,
    /// Run in a new terminal to start an agent session in a worktree.
    pub agent_command: String,
    /// Run in a new terminal by "Run task on worktree".
    pub worktree_task: String,
    /// Run git's safe maintenance when a repository is opened.
    pub auto_maintenance: bool,
    pub graph_changes_column: bool,
    pub graph_dim_merges: bool,
    pub archived_sessions: Vec<String>,
    pub language: String,
    pub graph_layout: Option<GraphLayout>,
    /// Commands last used from the palette, newest first.
    pub palette_recent: Vec<String>,
    /// The sidebar shows files rather than branches and the like.
    pub sidebar_files: bool,
    /// Open tabs per repository, restored next time.
    pub session_tabs: std::collections::HashMap<String, Vec<String>>,
    /// Tab arrangement per repository, including splits.
    pub session_layouts: std::collections::HashMap<String, String>,
}

/// Commit graph columns: a saved arrangement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphLayout {
    pub columns: Vec<String>,
    pub hidden: Vec<String>,
    /// Widths the user has set, by column.
    #[serde(default)]
    pub widths: std::collections::HashMap<String, f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            git_path: String::new(),
            theme: ThemeChoice::Dark,
            accent: String::new(),
            ui_scale: 1.0,
            editor_font_size: 13.0,
            recent_repos: Vec::new(),
            open_repos: Vec::new(),
            workspaces: Vec::new(),
            autolinks: Vec::new(),
            custom_hosts: Vec::new(),
            ai: AiConfig::default(),
            issue_trackers: IssueTrackerConfig::default(),
            inline_blame: true,
            inline_blame_format: "{author}, {ago} · {subject}".to_string(),
            code_lens: true,
            blame_ignore_ws: false,
            blame_detect_moves: false,
            heatmap_age_days: 90,
            gravatar: false,
            auto_fetch_minutes: 0,
            confirm_destructive: true,
            commit_sign: false,
            commit_signoff: false,
            prune_on_fetch: true,
            pull_mode: "default".to_string(),
            graph_page_size: 2000,
            default_external_editor: String::new(),
            date_relative: true,
            favorite_branches: Vec::new(),
            pinned_prs: Vec::new(),
            snoozed_prs: Vec::new(),
            start_work_format: String::new(),
            agent_command: "claude".to_string(),
            worktree_task: String::new(),
            auto_maintenance: false,
            graph_changes_column: false,
            graph_dim_merges: false,
            archived_sessions: Vec::new(),
            language: "en".to_string(),
            graph_layout: None,
            palette_recent: Vec::new(),
            sidebar_files: false,
            session_tabs: std::collections::HashMap::new(),
            session_layouts: std::collections::HashMap::new(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, p)
    }

    pub fn touch_recent(&mut self, path: &str) {
        self.recent_repos.retain(|r| r != path);
        self.recent_repos.insert(0, path.to_string());
        self.recent_repos.truncate(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_fields_tolerated() {
        let s: Settings = serde_json::from_str(r#"{"theme":"Light","no_such_field":1}"#).unwrap();
        assert_eq!(s.theme, ThemeChoice::Light);
        assert!(s.inline_blame);
    }

    #[test]
    fn recent_dedups() {
        let mut s = Settings::default();
        s.touch_recent("a");
        s.touch_recent("b");
        s.touch_recent("a");
        assert_eq!(s.recent_repos, vec!["a", "b"]);
    }
}
