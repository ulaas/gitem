//! A small Model Context Protocol server over stdio, so coding agents can query a
//! repository through gitem. `gitem --mcp [path]`. Every tool is read-only.
use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::model::LogQuery;
use crate::repo::{BlameOpts, DiffSource};
use crate::{GitExe, Repo};

const DEFAULT_PROTOCOL: &str = "2025-06-18";

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": props, "required": required}})
}

fn tools() -> Value {
    json!([
        tool("git_status", "Current branch, upstream distance, and changed files.", json!({}), &[]),
        tool("git_log", "Recent commits, optionally for one path. Accepts search syntax such as @:author or =:text.", json!({"limit": {"type":"integer"}, "path": {"type":"string"}, "query": {"type":"string"}}), &[]),
        tool("git_diff", "A unified diff: working changes by default, or `staged`, or one `commit`.", json!({"staged": {"type":"boolean"}, "commit": {"type":"string"}}), &[]),
        tool("git_blame", "Who last changed each line of a file.", json!({"path": {"type":"string"}}), &["path"]),
        tool("git_branches", "Local and remote branches with ahead and behind counts.", json!({}), &[]),
    ])
}

fn call(repo: &Repo, name: &str, a: &Value) -> crate::Result<String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str);
    match name {
        "git_status" => {
            let st = repo.status()?;
            let mut out = format!("branch: {}\nupstream: {}\nahead: {} behind: {}\n", st.branch.as_deref().unwrap_or("(detached)"), st.upstream.as_deref().unwrap_or("(none)"), st.ahead, st.behind);
            for e in &st.entries {
                out.push_str(&format!("{}{} {}\n", e.index.letter(), e.worktree.letter(), e.path));
            }
            Ok(out)
        }
        "git_log" => {
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(30).min(1000) as usize;
            let mut q = match s("query") {
                Some(t) => crate::repo::parse_search(t),
                None => LogQuery { revs: vec!["HEAD".to_string()], ..Default::default() },
            };
            q.limit = limit;
            if let Some(p) = s("path") {
                q.paths = vec![p.to_string()];
                q.literal_paths = true;
            }
            Ok(repo.log(&q)?.iter().map(|c| format!("{} {} ({}) {}\n", c.short(), crate::util::format_day(c.commit_time), c.author_name, c.subject)).collect())
        }
        "git_diff" => {
            let src = if let Some(c) = s("commit") {
                DiffSource::Commit(c.to_string())
            } else if a.get("staged").and_then(Value::as_bool).unwrap_or(false) {
                DiffSource::Staged
            } else {
                DiffSource::Unstaged
            };
            repo.diff_for_ai(&src)
        }
        "git_blame" => {
            let p = s("path").ok_or_else(|| crate::Error::Other("path is required".into()))?;
            Ok(repo.blame(p, None, BlameOpts::default())?.iter().map(|b| format!("{} {}: {}\n", crate::util::prefix(&b.sha, 8), b.author, b.text)).collect())
        }
        "git_branches" => Ok(repo.branches()?.iter().map(|b| format!("{}{} +{} -{} {}\n", if b.is_current { "* " } else { "" }, b.name, b.ahead, b.behind, b.subject)).collect()),
        other => Err(crate::Error::Other(format!("unknown tool {other}"))),
    }
}

/// One JSON-RPC message in, at most one out (notifications get no reply).
pub fn handle(repo: &Repo, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => {
            let v = msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or(DEFAULT_PROTOCOL);
            json!({"protocolVersion": v, "capabilities": {"tools": {}}, "serverInfo": {"name": "gitem", "version": env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or("");
            let empty = json!({});
            let args = msg.pointer("/params/arguments").unwrap_or(&empty);
            // Tool failures are results the agent can read, not protocol errors.
            match call(repo, name, args) {
                Ok(t) => json!({"content": [{"type": "text", "text": t}]}),
                Err(e) => json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true}),
            }
        }
        _ => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Serve until stdin closes.
pub fn serve(path: &Path) -> i32 {
    let opened = GitExe::discover(None).and_then(|g| Repo::open(g, path));
    let repo = match opened {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gitem --mcp: {e}");
            return 1;
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(reply) = handle(&repo, &msg)
            && writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
                break;
            }
    }
    0
}
