//! Coding-agent sessions on this machine, read from the transcript files the agent
//! CLIs write. Read-only. State comes from file activity and the last recorded
//! step, so it's an estimate: gitem has no live connection to a running agent.
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// Transcripts can be very large, so only both ends are read: Codex and Copilot
/// record the working folder once, at the start.
const TAIL_BYTES: u64 = 512 * 1024;
const HEAD_BYTES: u64 = 256 * 1024;
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
            "Copilot" => format!("copilot --resume {}", self.id),
            "OpenCode" => format!("opencode --session {}", self.id),
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
        if kind.contains('.') {
            copilot_event(kind, &v, &mut t, &mut counts, &mut open);
            continue;
        }
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

/// Copilot CLI: `{"type":"session.start","data":{...}}`. The format is undocumented;
/// the assistant text and file argument names are best guesses, read when present.
fn copilot_event(kind: &str, v: &Value, t: &mut Tail, counts: &mut HashMap<String, u64>, open: &mut Vec<String>) {
    let Some(data) = v.get("data") else { return };
    let text = |p: &str| data.pointer(p).and_then(Value::as_str);
    match kind {
        "session.start" => {
            t.id = text("/sessionId").unwrap_or(&t.id).to_string();
            t.cwd = text("/context/cwd").unwrap_or(&t.cwd).to_string();
            t.branch = text("/context/branch").unwrap_or(&t.branch).to_string();
        }
        "assistant.message" => {
            if let Some(x) = text("/content").filter(|x| !x.is_empty()) {
                t.last_text = x.chars().take(240).collect();
            }
        }
        "tool.execution_start" => {
            if let Some(id) = text("/toolCallId") {
                open.push(id.to_string());
            }
            if let Some(f) = ["/arguments/path", "/arguments/file_path", "/arguments/filePath"].iter().find_map(|p| text(p)) {
                *counts.entry(f.replace('\\', "/")).or_default() += 1;
            }
        }
        "tool.execution_complete" => {
            if let Some(id) = text("/toolCallId") {
                open.retain(|x| x != id);
            }
        }
        _ => {}
    }
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
    let mut b = Vec::new();
    if len > HEAD_BYTES + TAIL_BYTES {
        f.by_ref().take(HEAD_BYTES).read_to_end(&mut b).ok()?;
        b.push(b'\n');
        f.seek(SeekFrom::Start(len - TAIL_BYTES)).ok()?;
    }
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
    let copilot = std::env::var_os("COPILOT_HOME").map_or_else(|| home.join(".copilot"), PathBuf::from);
    vec![("Claude Code", home.join(".claude/projects")), ("Codex", home.join(".codex/sessions")), ("Copilot", copilot.join("session-state"))]
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
            // Copilot keeps one `<id>/events.jsonl` per session.
            let named = if path.file_stem().is_some_and(|s| s == "events") { path.parent().and_then(Path::file_name) } else { path.file_stem() };
            let id = if t.id.is_empty() { named.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { t.id };
            out.push(AgentSession { agent, id, path, cwd: t.cwd, branch: t.branch, modified, state: state_of(now - modified, t.pending_tool), last_text: t.last_text, files: t.files });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

/// Where OpenCode keeps its session database.
pub fn opencode_db() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default());
    std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from).join("opencode/opencode.db")
}

/// `(last assistant text, tool still running, file counts)` from an OpenCode
/// session's `(part JSON, message JSON)` rows, newest first. Tool parts are read
/// from OpenCode's documented tool shape; no real one was available to check.
fn opencode_summary(rows: &[(String, String)]) -> (String, bool, Vec<(String, u64)>) {
    let mut last_text = String::new();
    let mut pending = false;
    let mut counts: HashMap<String, u64> = HashMap::new();
    for (part, message) in rows {
        let Ok(p) = serde_json::from_str::<Value>(part) else { continue };
        match p.get("type").and_then(Value::as_str) {
            Some("text") if last_text.is_empty() => {
                let role = serde_json::from_str::<Value>(message).ok().and_then(|m| m.get("role").and_then(Value::as_str).map(String::from));
                if role.as_deref() == Some("assistant") {
                    last_text = p.get("text").and_then(Value::as_str).unwrap_or("").chars().take(240).collect();
                }
            }
            Some("tool") => {
                pending |= matches!(p.pointer("/state/status").and_then(Value::as_str), Some("pending" | "running"));
                if let Some(f) = ["/state/input/filePath", "/state/input/path"].iter().find_map(|k| p.pointer(k).and_then(Value::as_str)) {
                    *counts.entry(f.replace('\\', "/")).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    let mut files: Vec<_> = counts.into_iter().collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    (last_text, pending, files)
}

/// OpenCode sessions in `worktrees`. A missing or unfamiliar database gives none.
pub fn scan_opencode(db: &Path, worktrees: &[String], now: i64) -> Vec<AgentSession> {
    read_opencode(db, worktrees, now).unwrap_or_default()
}

fn read_opencode(db: &Path, worktrees: &[String], now: i64) -> rusqlite::Result<Vec<AgentSession>> {
    use rusqlite::{Connection, OpenFlags};
    if !db.exists() {
        return Ok(Vec::new());
    }
    let wanted: Vec<String> = worktrees.iter().map(|w| norm(w)).collect();
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(std::time::Duration::from_millis(200))?;
    let mut sessions = c.prepare("select id, directory, time_updated from session where parent_id is null and time_archived is null order by time_updated desc limit 300")?;
    let rows: Vec<(String, String, i64)> = sessions.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut parts = c.prepare("select p.data, m.data from part p join message m on m.id = p.message_id where p.session_id = ?1 order by p.time_created desc, p.id desc limit 500")?;
    let mut out = Vec::new();
    for (id, cwd, updated_ms) in rows {
        if !wanted.contains(&norm(&cwd)) {
            continue;
        }
        let items: Vec<(String, String)> = parts.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let (last_text, pending, files) = opencode_summary(&items);
        let modified = updated_ms / 1000;
        out.push(AgentSession { agent: "OpenCode", id, path: db.to_path_buf(), cwd, branch: String::new(), modified, state: state_of(now - modified, pending), last_text, files });
    }
    Ok(out)
}

/// Every supported agent's sessions in `worktrees`, newest first.
pub fn scan_all(worktrees: &[String], now: i64) -> Vec<AgentSession> {
    let mut out = scan(&default_roots(), worktrees, now);
    out.extend(scan_opencode(&opencode_db(), worktrees, now));
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
    fn copilot_sessions() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("state/abc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("events.jsonl"),
            r#"{"type":"session.start","data":{"sessionId":"abc","context":{"cwd":"C:\\repo","branch":"dev"}},"id":"1"}
{"type":"assistant.message","data":{"content":"Looking"}}
{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"view","arguments":{"path":"C:\\repo\\a.rs"}}}
{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":true}}
{"type":"tool.execution_start","data":{"toolCallId":"t2","toolName":"shell","arguments":{}}}
"#,
        )
        .unwrap();
        let roots = vec![("Copilot", d.path().join("state"))];
        let found = scan(&roots, &["C:/repo".to_string()], crate::util::now_ts() + 100);
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!((s.id.as_str(), s.branch.as_str(), s.last_text.as_str()), ("abc", "dev", "Looking"));
        assert_eq!(s.state, AgentState::NeedsInput, "t2 is unfinished");
        assert_eq!(s.files, vec![("C:/repo/a.rs".to_string(), 1)]);
        assert_eq!(s.resume_command(), "copilot --resume abc");
    }

    #[test]
    fn long_sessions_keep_the_folder_recorded_on_the_first_line() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let mut text = String::from("{\"type\":\"session_meta\",\"payload\":{\"id\":\"cx\",\"cwd\":\"C:/repo\"}}\n");
        let filler = format!("{{\"type\":\"response_item\",\"x\":\"{}\"}}\n", "y".repeat(1000));
        while text.len() < 2 * 1024 * 1024 {
            text.push_str(&filler);
        }
        std::fs::write(root.join("long.jsonl"), text).unwrap();
        let found = scan(&[("Codex", root)], &["C:/repo".to_string()], crate::util::now_ts() + 100);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "cx");
    }

    #[test]
    fn opencode_sessions_come_from_its_database() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("opencode.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            r#"create table session (id text primary key, directory text, parent_id text, time_archived integer, time_updated integer);
            create table message (id text primary key, session_id text, data text);
            create table part (id text primary key, message_id text, session_id text, time_created integer, data text);
            insert into session values ('ses1', 'C:\repo', null, null, 1000000), ('child', 'C:\repo', 'ses1', null, 1000000), ('old', 'C:\repo', null, 5, 1000000), ('else', 'D:\other', null, null, 1000000);
            insert into message values ('m1', 'ses1', '{"role":"user"}'), ('m2', 'ses1', '{"role":"assistant"}');
            insert into part values
              ('p1', 'm1', 'ses1', 1, '{"type":"text","text":"please fix"}'),
              ('p2', 'm2', 'ses1', 2, '{"type":"text","text":"Fixing now"}'),
              ('p3', 'm2', 'ses1', 3, '{"type":"tool","tool":"edit","state":{"status":"completed","input":{"filePath":"C:\\repo\\a.rs"}}}'),
              ('p4', 'm2', 'ses1', 4, '{"type":"tool","tool":"bash","state":{"status":"running","input":{}}}');"#,
        )
        .unwrap();
        drop(c);
        let found = scan_opencode(&db, &["c:/REPO".to_string()], 2000);
        assert_eq!(found.len(), 1, "only top-level, unarchived sessions in this repository");
        let s = &found[0];
        assert_eq!((s.id.as_str(), s.modified, s.last_text.as_str()), ("ses1", 1000, "Fixing now"));
        assert_eq!(s.state, AgentState::NeedsInput);
        assert_eq!(s.files, vec![("C:/repo/a.rs".to_string(), 1)]);
        assert_eq!(s.resume_command(), "opencode --session ses1");
    }

    #[test]
    fn unreadable_opencode_data_gives_no_sessions() {
        let d = tempfile::tempdir().unwrap();
        assert!(scan_opencode(&d.path().join("none.db"), &["x".to_string()], 0).is_empty());
        let junk = d.path().join("junk.db");
        std::fs::write(&junk, "not a database").unwrap();
        assert!(scan_opencode(&junk, &["x".to_string()], 0).is_empty());
    }

    #[test]
    fn codex_sessions() {
        let t = parse_tail(r#"{"type":"session_meta","payload":{"id":"cx","cwd":"/w"}}"#);
        assert_eq!((t.id.as_str(), t.cwd.as_str()), ("cx", "/w"));
    }
}
