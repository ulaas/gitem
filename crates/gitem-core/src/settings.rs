//! Persistent app settings (JSON in the user config dir). Secrets never live
//! here; they go through [`crate::cred`].
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::AiConfig;
use crate::forge::ForgeKind;

/// The folder named by `GITEM_CONFIG_DIR`. When it's set, everything gitem keeps
/// stays in there: settings, window state and secrets.
pub fn config_dir_override() -> Option<PathBuf> {
    std::env::var_os("GITEM_CONFIG_DIR").map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(p) = config_dir_override() {
        return p;
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
    /// Opening the graph shows the current commit's details.
    pub details_on_open: bool,
    /// Every view on the rail, rather than the usual few and a More list.
    pub rail_full: bool,
    /// No moving spinners or animated transitions.
    pub reduce_motion: bool,
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
    /// The Changes list groups files by folder rather than listing them flat.
    pub changes_tree: bool,
    /// Open tabs per repository, restored next time.
    pub session_tabs: std::collections::HashMap<String, Vec<String>>,
    /// Tab arrangement per repository, including splits.
    pub session_layouts: std::collections::HashMap<String, String>,
    /// Fields this version doesn't know, kept so a save doesn't drop what a newer one wrote.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

/// What reading the settings file found.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub settings: Settings,
    /// The settings as read, so a later save can tell what changed here from what another
    /// copy of the app changed. Pass it to [`Settings::save_merged`].
    pub baseline: Value,
    /// Something the user should be told.
    pub problem: Option<String>,
    /// The file is there but couldn't be read, so writing would destroy it unseen.
    pub save_blocked: bool,
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

impl Settings {
    /// Every preference back to its default, keeping where the user was and what they keep
    /// coming back to, none of which they'd think of as a preference.
    pub fn with_default_preferences(&self) -> Self {
        let c = self.clone();
        Self {
            recent_repos: c.recent_repos,
            open_repos: c.open_repos,
            workspaces: c.workspaces,
            favorite_branches: c.favorite_branches,
            pinned_prs: c.pinned_prs,
            snoozed_prs: c.snoozed_prs,
            archived_sessions: c.archived_sessions,
            palette_recent: c.palette_recent,
            session_tabs: c.session_tabs,
            session_layouts: c.session_layouts,
            unknown: c.unknown,
            ..Self::default()
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            git_path: String::new(),
            theme: ThemeChoice::Dark,
            details_on_open: true,
            rail_full: false,
            reduce_motion: false,
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
            changes_tree: false,
            sidebar_files: false,
            session_tabs: std::collections::HashMap::new(),
            session_layouts: std::collections::HashMap::new(),
            unknown: serde_json::Map::new(),
        }
    }
}

/// Reads settings text, keeping everything that makes sense. Returns what it had to drop;
/// `Err` when the text as a whole isn't a JSON object. A byte-order mark, as some editors
/// add, doesn't count against it.
fn parse_tolerant(text: &str) -> Result<(Settings, Vec<String>), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Value::Object(fields) = &value else { return Err("it isn't a JSON object".to_string()) };
    if let Ok(s) = serde_json::from_value::<Settings>(value.clone()) {
        return Ok((s, Vec::new()));
    }
    // One bad value, like a theme from a newer version, shouldn't cost all the others.
    let (mut good, mut dropped) = (serde_json::Map::new(), Vec::new());
    for (k, v) in fields {
        let alone = Value::Object(std::iter::once((k.clone(), v.clone())).collect());
        if serde_json::from_value::<Settings>(alone).is_ok() {
            good.insert(k.clone(), v.clone());
        } else {
            dropped.push(k.clone());
        }
    }
    serde_json::from_value(Value::Object(good)).map(|s| (s, dropped)).map_err(|e| e.to_string())
}

/// Keeps a copy of a settings file that can't be used in full, beside it.
fn back_up(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let stamp = crate::util::now_ts();
    for n in 0..1000 {
        let name = format!("{}.bad-{stamp}{}", path.file_name().unwrap_or_default().to_string_lossy(), if n == 0 { String::new() } else { format!("-{n}") });
        let copy = path.with_file_name(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&copy) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                return Ok(copy);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many backups of the settings file"))
}

/// Three-way merge of JSON, by key. A value changed on only one side takes that change.
/// Changed on both, this copy's wins, except that objects merge key by key.
fn merge_json(base: Option<&Value>, mine: Option<&Value>, disk: Option<&Value>) -> Option<Value> {
    if mine == base {
        return disk.cloned();
    }
    if disk == base {
        return mine.cloned();
    }
    let (Some(Value::Object(m)), Some(Value::Object(d))) = (mine, disk) else { return mine.cloned() };
    let empty = serde_json::Map::new();
    let b = if let Some(Value::Object(b)) = base { b } else { &empty };
    let mut out = serde_json::Map::new();
    for k in m.keys().chain(d.keys()).chain(b.keys()) {
        if !out.contains_key(k)
            && let Some(v) = merge_json(b.get(k), m.get(k), d.get(k))
        {
            out.insert(k.clone(), v);
        }
    }
    Some(Value::Object(out))
}

impl Settings {
    pub fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    /// For the small helper windows, which only want the look: a problem with the file
    /// is the main app's to report, so nothing is written here.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| parse_tolerant(&s).ok()).map(|(s, _)| s).unwrap_or_default()
    }

    /// Reads the file and says what went wrong. A file it can't use in full is kept as a
    /// backup first, because the next save replaces it.
    pub fn load_checked() -> LoadedSettings {
        let path = Self::path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadedSettings::new(Settings::default(), None, false),
            Err(e) => {
                let why = format!("gitem couldn't read its settings ({e}), so it's using defaults and won't save any this time, which keeps {} as it is.", path.display());
                return LoadedSettings::new(Settings::default(), Some(why), true);
            }
        };
        let parsed = std::str::from_utf8(&bytes).map_err(|e| e.to_string()).and_then(parse_tolerant);
        if String::from_utf8_lossy(&bytes).trim().is_empty() {
            return LoadedSettings::new(Settings::default(), Some(format!("{} was empty, so gitem started with default settings.", path.display())), false);
        }
        let unreadable = parsed.is_err();
        let (settings, what) = match parsed {
            Ok((s, dropped)) if dropped.is_empty() => return LoadedSettings::new(s, None, false),
            Ok((s, dropped)) => (s, format!("These settings had values gitem couldn't read and were reset: {}.", dropped.join(", "))),
            Err(e) => (Settings::default(), format!("The settings file couldn't be read ({e}), so gitem started with defaults.")),
        };
        match back_up(&path, &bytes) {
            Ok(copy) => {
                // Moved aside rather than left, or the next save would report it all over again.
                if unreadable {
                    let _ = std::fs::remove_file(&path);
                }
                LoadedSettings::new(settings, Some(format!("{what}\n\nThe original is kept at {}.", copy.display())), false)
            }
            Err(e) => {
                let why = format!("{what}\n\nA backup couldn't be made ({e}), so no settings will be saved this time, which keeps {} as it is.", path.display());
                LoadedSettings::new(settings, Some(why), true)
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        write_settings(&Self::path(), &serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)
    }

    /// Saves over whatever is in the file now, keeping what another copy of the app has
    /// changed there since `baseline`, the settings this copy last read or wrote. Afterwards
    /// `self` holds the merged result. Returns something to tell the user, if any.
    pub fn save_merged(&mut self, baseline: &mut Value) -> std::io::Result<Option<String>> {
        let path = Self::path();
        let mine = serde_json::to_value(&*self).map_err(std::io::Error::other)?;
        let mut note = None;
        let disk = match std::fs::read(&path) {
            Ok(bytes) => match std::str::from_utf8(&bytes).ok().map(|t| t.strip_prefix('\u{feff}').unwrap_or(t)).and_then(|t| serde_json::from_str::<Value>(t).ok()).filter(Value::is_object) {
                Some(v) => Some(v),
                // Something broke it while this copy ran. It's not this copy's to lose.
                None if !bytes.is_empty() => {
                    let copy = back_up(&path, &bytes)?;
                    note = Some(format!("The settings file had become unreadable, so it was replaced. The old one is kept at {}.", copy.display()));
                    None
                }
                None => None,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let merged = merge_json(Some(baseline), Some(&mine), disk.as_ref()).unwrap_or(mine);
        write_settings(&path, &serde_json::to_vec_pretty(&merged).map_err(std::io::Error::other)?)?;
        if let Ok((s, _)) = parse_tolerant(&merged.to_string()) {
            *baseline = serde_json::to_value(&s).unwrap_or(merged);
            *self = s;
        }
        Ok(note)
    }

    pub fn touch_recent(&mut self, path: &str) {
        self.recent_repos.retain(|r| r != path);
        self.recent_repos.insert(0, path.to_string());
        self.recent_repos.truncate(30);
    }
}

impl LoadedSettings {
    fn new(settings: Settings, problem: Option<String>, save_blocked: bool) -> Self {
        let baseline = serde_json::to_value(&settings).unwrap_or(Value::Null);
        Self { settings, baseline, problem, save_blocked }
    }
}

fn write_settings(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::util::write_atomic(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resetting_preferences_keeps_where_the_user_was() {
        let mut s = Settings { theme: ThemeChoice::Paper, ui_scale: 1.5, ..Default::default() };
        s.ai.custom_instructions = "be brief".into();
        s.recent_repos.push("a".into());
        s.open_repos.push("b".into());
        s.workspaces.push(Workspace { name: "w".into(), repos: Vec::new() });
        s.favorite_branches.push("f".into());
        s.unknown.insert("from_a_newer_version".into(), Value::Bool(true));
        let r = s.with_default_preferences();
        assert_eq!((r.theme, r.ui_scale, r.ai.custom_instructions.as_str()), (ThemeChoice::Dark, 1.0, ""));
        assert_eq!((r.recent_repos, r.open_repos, r.workspaces.len(), r.favorite_branches), (s.recent_repos, s.open_repos, 1, s.favorite_branches));
        assert!(r.unknown.contains_key("from_a_newer_version"));
    }
    use serde_json::json;

    fn merged(base: Value, mine: Value, disk: Value) -> Value {
        merge_json(Some(&base), Some(&mine), Some(&disk)).unwrap()
    }

    #[test]
    fn a_change_on_either_side_is_kept() {
        let base = json!({"a": 1, "b": 1});
        assert_eq!(merged(base.clone(), json!({"a": 2, "b": 1}), json!({"a": 1, "b": 3})), json!({"a": 2, "b": 3}));
    }

    #[test]
    fn a_value_changed_on_both_sides_is_this_copys() {
        assert_eq!(merged(json!({"a": 1}), json!({"a": 2}), json!({"a": 3})), json!({"a": 2}));
    }

    #[test]
    fn objects_merge_key_by_key_and_removals_count() {
        let base = json!({"tabs": {"x": 1, "y": 1, "gone": 1}});
        let mine = json!({"tabs": {"x": 2, "y": 1}});
        let disk = json!({"tabs": {"x": 1, "y": 1, "gone": 1, "z": 1}});
        assert_eq!(merged(base, mine, disk), json!({"tabs": {"x": 2, "y": 1, "z": 1}}));
    }

    #[test]
    fn one_bad_value_costs_only_itself() {
        let (s, dropped) = parse_tolerant(r#"{"theme":"Neon","language":"tr"}"#).unwrap();
        assert_eq!(dropped, vec!["theme"]);
        assert_eq!(s.language, "tr");
        assert_eq!(s.theme, ThemeChoice::Dark);
    }

    #[test]
    fn a_byte_order_mark_is_not_a_problem() {
        let (s, dropped) = parse_tolerant("\u{feff}{\"language\":\"tr\"}").unwrap();
        assert!(dropped.is_empty());
        assert_eq!(s.language, "tr");
    }

    #[test]
    fn unknown_fields_go_back_out_on_saving() {
        let (s, _) = parse_tolerant(r#"{"from_the_future":{"x":1}}"#).unwrap();
        assert!(serde_json::to_string(&s).unwrap().contains("from_the_future"));
    }

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
