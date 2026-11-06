//! Coding-agent sessions on this machine, read from the transcript files the agent
//! CLIs write. Read-only. State comes from file activity and the last recorded
//! step, so it's an estimate: gitem has no live connection to a running agent.
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// Only the end of each transcript is read: they can be very large.
const TAIL_BYTES: u64 = 512 * 1024;
const MAX_FILES: usize = 300;
/// A file written this recently means the agent is active.
const WORKING_SECS: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Working,
    /// Stopped partway through a tool call, usually waiting for permission.
    NeedsInput,
    Idle,
}

impl AgentState {
    pub const ALL: [AgentState; 3] = [Self::NeedsInput, Self::Working, Self::Idle];
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsInput => "Probably needs input",
            Self::Idle => "Idle",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentSession {
    pub agent: &'static str,
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    pub branch: String,
    pub modified: i64,
    pub state: AgentState,
    pub last_text: String,
    /// (file, times read or edited) for the activity treemap.
    pub files: Vec<(String, u64)>,
}

impl AgentSession {
    /// Command that reopens this session in a terminal.
    pub fn resume_command(&self) -> String {
        match self.agent {
            "Codex" => format!("codex resume {}", self.id),
            _ => format!("claude --resume {}", self.id),
        }
    }
}

fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

#[derive(Default, Debug, PartialEq)]
pub struct Tail {
    pub cwd: String,
    pub branch: String,
    pub id: String,
    pub last_text: String,
    pub pending_tool: bool,
    pub files: Vec<(String, u64)>,
}

/// Summarise JSON-lines transcript text. The first line may be cut off by the
/// tail read; anything unparseable is skipped.
pub fn parse_tail(text: &str) -> Tail {
    let mut t = Tail::default();
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut open: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
        if !get("cwd").is_empty() {
            t.cwd = get("cwd").to_string();
        }
        if !get("gitBranch").is_empty() {
            t.branch = get("gitBranch").to_string();
        }
        if !get("sessionId").is_empty() {
            t.id = get("sessionId").to_string();
        }
        // Codex: {"type":"session_meta","payload":{"id","cwd"}}
        if get("type") == "session_meta"
            && let Some(p) = v.get("payload") {
                t.cwd = p.get("cwd").and_then(Value::as_str).unwrap_or(&t.cwd).to_string();
                t.id = p.get("id").and_then(Value::as_str).unwrap_or(&t.id).to_string();
            }
        let kind = get("type");
        if kind != "assistant" && kind != "user" {
            continue;
        }
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { continue };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") if kind == "assistant" => {
                    if let Some(x) = b.get("text").and_then(Value::as_str) {
                        t.last_text = x.chars().take(240).collect();
                    }
                }
                Some("tool_use") => {
                    if let Some(id) = b.get("id").and_then(Value::as_str) {
                        open.push(id.to_string());
                    }
                    if let Some(f) = b.pointer("/input/file_path").and_then(Value::as_str) {
                        *counts.entry(f.replace('\\', "/")).or_default() += 1;
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                        open.retain(|x| x != id);
                    }
                }
                _ => {}
            }
        }
    }
    t.pending_tool = !open.is_empty();
    t.files = counts.into_iter().collect();
    t.files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    t
}

pub fn state_of(age_secs: i64, pending_tool: bool) -> AgentState {
    if age_secs < WORKING_SECS {
        AgentState::Working
    } else if pending_tool {
        AgentState::NeedsInput
    } else {
        AgentState::Idle
    }
}

fn read_tail(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    if len > TAIL_BYTES {
        f.seek(SeekFrom::Start(len - TAIL_BYTES)).ok()?;
    }
    let mut b = Vec::new();
    f.read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

fn collect_jsonl(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, i64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                collect_jsonl(&p, depth - 1, out);
            }
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            let m = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            out.push((p, m));
        }
    }
}

/// `(agent name, transcript folder)` for each supported CLI.
pub fn default_roots() -> Vec<(&'static str, PathBuf)> {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default());
    vec![("Claude Code", home.join(".claude/projects")), ("Codex", home.join(".codex/sessions"))]
}

/// Sessions whose working folder is one of `worktrees`, newest first.
pub fn scan(roots: &[(&'static str, PathBuf)], worktrees: &[String], now: i64) -> Vec<AgentSession> {
    let wanted: Vec<String> = worktrees.iter().map(|w| norm(w)).collect();
    let mut out = Vec::new();
    for (agent, root) in roots {
        let mut files = Vec::new();
        collect_jsonl(root, 4, &mut files);
        files.sort_by_key(|x| std::cmp::Reverse(x.1));
        files.truncate(MAX_FILES);
        for (path, modified) in files {
            let Some(text) = read_tail(&path) else { continue };
            let t = parse_tail(&text);
            if !wanted.contains(&norm(&t.cwd)) {
                continue;
            }
            let id = if t.id.is_empty() { path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { t.id };
            out.push(AgentSession { agent, id, path, cwd: t.cwd, branch: t.branch, modified, state: state_of(now - modified, t.pending_tool), last_text: t.last_text, files: t.files });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"cut off first line
{"type":"user","cwd":"C:\\repo","gitBranch":"main","sessionId":"s1","message":{"content":[{"type":"text","text":"hi"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Editing now"},{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"C:\\repo\\a.rs"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}},{"type":"tool_use","id":"t3","name":"Read","input":{"file_path":"C:\\repo\\a.rs"}}]}}
"#;

    #[test]
    fn transcript_summary() {
        let t = parse_tail(SAMPLE);
        assert_eq!((t.cwd.as_str(), t.branch.as_str(), t.id.as_str()), ("C:\\repo", "main", "s1"));
        assert_eq!(t.last_text, "Editing now");
        assert!(t.pending_tool, "t2 and t3 have no results yet");
        assert_eq!(t.files, vec![("C:/repo/a.rs".to_string(), 2)]);
    }

    #[test]
    fn states() {
        assert_eq!(state_of(5, true), AgentState::Working);
        assert_eq!(state_of(60, true), AgentState::NeedsInput);
        assert_eq!(state_of(60, false), AgentState::Idle);
    }

    #[test]
    fn scan_filters_by_worktree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projects/x");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.jsonl"), SAMPLE).unwrap();
        std::fs::write(root.join("b.jsonl"), r#"{"type":"user","cwd":"D:\\other","sessionId":"s2"}"#).unwrap();
        let roots = vec![("Claude Code", d.path().join("projects"))];
        let now = crate::util::now_ts() + 100;
        let found = scan(&roots, &["c:/REPO/".to_string()], now);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "s1");
        assert_eq!(found[0].state, AgentState::NeedsInput);
        assert_eq!(found[0].resume_command(), "claude --resume s1");
    }

    #[test]
    fn codex_sessions() {
        let t = parse_tail(r#"{"type":"session_meta","payload":{"id":"cx","cwd":"/w"}}"#);
        assert_eq!((t.id.as_str(), t.cwd.as_str()), ("cx", "/w"));
    }
}
