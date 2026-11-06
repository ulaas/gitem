//! Runs the hosting, issue-tracker, and AI clients over real HTTP against a mock
//! server on localhost: real requests, sockets, and parsing. No external service
//! is contacted, so this checks our side of the protocol, not the providers'.
gitem_testenv::isolate!();

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use gitem_core::ai::{self, AiConfig, AiProvider};
use gitem_core::forge::{self, ForgeClient, ForgeKind, RemoteRepo};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|x| x.1.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

struct Mock {
    base: String,
    log: Arc<Mutex<Vec<Req>>>,
}

/// Serves `(status, body)` from the handler, on its own thread.
fn mock(handler: impl Fn(&Req) -> (u16, String) + Send + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log: Arc<Mutex<Vec<Req>>> = Arc::default();
    let l2 = log.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            if r.read_line(&mut first).is_err() || first.is_empty() {
                continue;
            }
            let mut parts = first.split_whitespace();
            let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
            let mut headers = Vec::new();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let t = h.trim();
                if t.is_empty() {
                    break;
                }
                if let Some((k, v)) = t.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let mut b = vec![0u8; len];
            r.read_exact(&mut b).unwrap();
            let req = Req { method, path, headers, body: String::from_utf8_lossy(&b).into_owned() };
            let (status, body) = handler(&req);
            l2.lock().unwrap().push(req);
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    Mock { base, log }
}

fn client(kind: ForgeKind, path: &str, api: &str, user: &str) -> ForgeClient {
    ForgeClient::new(RemoteRepo { kind, host: "host.test".into(), path: path.into(), api_base: Some(api.to_string()), templates: Default::default() }, "TOKEN".into(), user.into())
}

#[test]
fn github_flow() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/user") => (200, json!({"login":"ann"}).to_string()),
        ("POST", "/graphql") => (200, json!({"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"title":"T","url":"u","author":{"login":"ann"},"headRefName":"f","baseRefName":"main","reviewDecision":"APPROVED"}]}}}}).to_string()),
        ("POST", "/repos/o/r/pulls") => (201, json!({"html_url":"http://pr/9"}).to_string()),
        ("PUT", "/repos/o/r/pulls/5/merge") => (200, "{}".to_string()),
        ("POST", "/repos/o/r/issues/5/comments") => (201, "{}".to_string()),
        ("POST", "/repos/o/r/pulls/5/reviews") => (200, "{}".to_string()),
        ("POST", "/gists") => (201, json!({"html_url":"http://gist/1"}).to_string()),
        ("GET", p) if p.starts_with("/repos/o/r/issues") => (200, json!([{"number":1,"title":"Bug","html_url":"i","state":"open","user":{"login":"b"}},{"number":2,"title":"is a PR","pull_request":{}}]).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::GitHub, "o/r", &format!("{}/v3", m.base), "");
    // api_base is used as given for REST and with /v3 removed for GraphQL.
    let c = ForgeClient::new(RemoteRepo { api_base: Some(m.base.clone()), ..c.repo }, c.token, c.user);
    assert_eq!(c.whoami().unwrap(), "ann");
    let prs = c.list_prs().unwrap();
    assert_eq!(prs[0].number, 5);
    assert_eq!(forge::launchpad_group(&prs[0], "ann"), forge::LaunchpadGroup::Mergeable);
    let issues = c.list_issues().unwrap();
    assert_eq!(issues.len(), 1, "pull requests are filtered out of issues");
    assert_eq!(c.create_pr("t", "b", "f", "main", true).unwrap(), "http://pr/9");
    c.merge_pr(5).unwrap();
    c.comment_pr(5, "hi").unwrap();
    c.approve_pr(5).unwrap();
    assert_eq!(c.create_gist("d", "x.patch", "body", false).unwrap(), "http://gist/1");
    assert_eq!(c.pr_for_branch("f").unwrap().unwrap().number, 5);

    let log = m.log.lock().unwrap();
    assert!(log.iter().all(|r| r.header("authorization") == Some("Bearer TOKEN")));
    let create = log.iter().find(|r| r.path == "/repos/o/r/pulls").unwrap().json();
    assert_eq!((create["head"].as_str(), create["base"].as_str(), create["draft"].as_bool()), (Some("f"), Some("main"), Some(true)));
    let gist = log.iter().find(|r| r.path == "/gists").unwrap().json();
    assert_eq!(gist["public"], false, "patches are shared as secret gists");
    assert_eq!(gist["files"]["x.patch"]["content"], "body");
    let q = log.iter().find(|r| r.path == "/graphql").unwrap().json();
    assert_eq!((q["variables"]["o"].as_str(), q["variables"]["n"].as_str()), (Some("o"), Some("r")));
}

#[test]
fn api_errors_are_reported_with_their_message() {
    let m = mock(|_| (403, json!({"message":"Bad credentials"}).to_string()));
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let e = c.whoami().unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("Bad credentials"), "{e}");
    let m = mock(|_| (200, json!({"errors":[{"message":"Could not resolve"}]}).to_string()));
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    assert!(c.list_prs().unwrap_err().to_string().contains("Could not resolve"));
}

#[test]
fn gitlab_flow() {
    let m = mock(|r| {
        let p = r.path.as_str();
        match r.method.as_str() {
            "GET" if p.starts_with("/projects/g%2Fs%2Fr/merge_requests") => (200, json!([{"iid":3,"title":"M","web_url":"u","author":{"username":"a"},"source_branch":"f","target_branch":"main","state":"opened"}]).to_string()),
            "GET" if p.starts_with("/projects/g%2Fs%2Fr/issues") => (200, json!([{"iid":8,"title":"I","web_url":"u","state":"opened","labels":["x"]}]).to_string()),
            "POST" if p == "/projects/g%2Fs%2Fr/merge_requests" => (201, json!({"web_url":"http://mr/1"}).to_string()),
            "PUT" if p == "/projects/g%2Fs%2Fr/merge_requests/3/merge" => (200, "{}".to_string()),
            "POST" if p == "/projects/g%2Fs%2Fr/merge_requests/3/notes" => (201, "{}".to_string()),
            "POST" if p == "/projects/g%2Fs%2Fr/merge_requests/3/approve" => (201, "{}".to_string()),
            _ => (404, format!("{{\"message\":\"{p}\"}}")),
        }
    });
    let c = client(ForgeKind::GitLab, "g/s/r", &m.base, "");
    assert_eq!(c.list_prs().unwrap()[0].number, 3);
    assert_eq!(c.list_issues().unwrap()[0].labels, vec!["x"]);
    assert_eq!(c.create_pr("t", "b", "f", "main", true).unwrap(), "http://mr/1");
    c.merge_pr(3).unwrap();
    c.comment_pr(3, "x").unwrap();
    c.approve_pr(3).unwrap();
    let log = m.log.lock().unwrap();
    let created = log.iter().find(|r| r.method == "POST" && r.path.ends_with("/merge_requests")).unwrap().json();
    assert_eq!(created["title"], "Draft: t", "GitLab marks drafts in the title");
}

#[test]
fn bitbucket_cloud_flow_uses_basic_auth() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", p) if p.starts_with("/repositories/o/r/pullrequests") => (200, json!({"values":[{"id":2,"title":"B","links":{"html":{"href":"u"}},"source":{"branch":{"name":"f"}},"destination":{"branch":{"name":"main"}}}]}).to_string()),
        ("POST", "/repositories/o/r/pullrequests") => (201, json!({"links":{"html":{"href":"http://b/1"}}}).to_string()),
        ("POST", "/repositories/o/r/pullrequests/2/merge") => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    });
    let c = client(ForgeKind::Bitbucket, "o/r", &m.base, "me");
    assert_eq!(c.list_prs().unwrap()[0].head, "f");
    assert_eq!(c.create_pr("t", "b", "f", "main", false).unwrap(), "http://b/1");
    c.merge_pr(2).unwrap();
    assert_eq!(m.log.lock().unwrap()[0].header("authorization"), Some("Basic bWU6VE9LRU4="));
}

#[test]
fn bitbucket_server_flow() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", p) if p.starts_with("/projects/PROJ/repos/repo/pull-requests?") => (200, json!({"values":[{"id":6,"title":"S","state":"OPEN","author":{"user":{"name":"a"}},"fromRef":{"id":"refs/heads/f"},"toRef":{"id":"refs/heads/main"},"updatedDate":1700000000000i64}]}).to_string()),
        ("GET", "/projects/PROJ/repos/repo/pull-requests/6") => (200, json!({"version":4}).to_string()),
        ("POST", "/projects/PROJ/repos/repo/pull-requests/6/merge") => (200, "{}".to_string()),
        ("POST", "/projects/PROJ/repos/repo/pull-requests") => (201, json!({"id":7}).to_string()),
        _ => (404, "{}".to_string()),
    });
    let c = client(ForgeKind::BitbucketServer, "scm/PROJ/repo", &m.base, "");
    let prs = c.list_prs().unwrap();
    assert_eq!((prs[0].head.as_str(), prs[0].base.as_str(), prs[0].updated), ("f", "main", 1_700_000_000));
    assert!(c.create_pr("t", "b", "f", "main", false).unwrap().ends_with("/pull-requests/7/overview"));
    c.merge_pr(6).unwrap();
    let log = m.log.lock().unwrap();
    assert_eq!(log.iter().find(|r| r.path.ends_with("/merge")).unwrap().json()["version"], 4, "merge sends the PR version");
}

#[test]
fn gitea_flow() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", p) if p.starts_with("/repos/o/r/pulls") => (200, json!([{"number":4,"title":"G","html_url":"u","user":{"login":"a"},"head":{"ref":"f"},"base":{"ref":"main"},"mergeable":false}]).to_string()),
        ("POST", "/repos/o/r/pulls/4/merge") => (200, "".to_string()),
        _ => (404, "{}".to_string()),
    });
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    assert_eq!(c.list_prs().unwrap()[0].mergeable, "CONFLICTING");
    c.merge_pr(4).unwrap();
    assert_eq!(m.log.lock().unwrap()[0].header("authorization"), Some("token TOKEN"));
}

#[test]
fn azure_devops_flow_against_a_custom_server_root() {
    let m = mock(|r| {
        let p = r.path.as_str();
        match r.method.as_str() {
            "GET" if p.starts_with("/org/proj/_apis/git/repositories/repo/pullrequests?") => (200, json!({"value":[{"pullRequestId":11,"title":"A","sourceRefName":"refs/heads/f","targetRefName":"refs/heads/main","mergeStatus":"conflicts","isDraft":false}]}).to_string()),
            "GET" if p.starts_with("/org/proj/_apis/git/repositories/repo/pullrequests/11") => (200, json!({"lastMergeSourceCommit":{"commitId":"abc"}}).to_string()),
            "PATCH" if p.starts_with("/org/proj/_apis/git/repositories/repo/pullrequests/11") => (200, "{}".to_string()),
            "POST" if p.starts_with("/org/proj/_apis/wit/wiql") => (200, json!({"workItems":[{"id":21}]}).to_string()),
            "GET" if p.starts_with("/org/_apis/wit/workitems") => (200, json!({"value":[{"id":21,"fields":{"System.Title":"W","System.State":"New"}}]}).to_string()),
            _ => (404, format!("{{\"message\":\"{p}\"}}")),
        }
    });
    let c = client(ForgeKind::AzureDevOps, "org/proj/repo", &m.base, "me");
    assert_eq!(c.list_prs().unwrap()[0].mergeable, "CONFLICTING");
    assert_eq!(c.list_issues().unwrap()[0].title, "W");
    c.merge_pr(11).unwrap();
    let log = m.log.lock().unwrap();
    let patch = log.iter().find(|r| r.method == "PATCH").unwrap().json();
    assert_eq!(patch["status"], "completed");
    assert_eq!(patch["lastMergeSourceCommit"]["commitId"], "abc");
}

#[test]
fn link_only_providers_say_so() {
    let c = client(ForgeKind::Gerrit, "p/r", "http://127.0.0.1:9", "");
    assert!(c.list_prs().unwrap_err().to_string().contains("no pull request API"));
    assert!(c.merge_pr(1).is_err());
    assert!(c.list_issues().unwrap().is_empty());
}

#[test]
fn jira_cloud_server_linear_and_trello() {
    let m = mock(|r| {
        let p = r.path.as_str();
        if p.starts_with("/rest/api/3/search/jql") || p.starts_with("/rest/api/2/search") {
            (200, json!({"issues":[{"key":"AB-1","fields":{"summary":"S","status":{"name":"Open"}}}]}).to_string())
        } else if p == "/graphql" {
            (200, json!({"data":{"viewer":{"assignedIssues":{"nodes":[{"identifier":"LIN-2","title":"L","url":"u","state":{"name":"Todo"}}]}}}}).to_string())
        } else if p.starts_with("/1/members/me/cards") {
            (200, json!([{"idShort":3,"name":"Card","shortUrl":"u","dateLastActivity":"2024-01-02T03:04:05Z"}]).to_string())
        } else {
            (404, "{}".to_string())
        }
    });
    assert_eq!(forge::jira_issues(&m.base, "e@x", "tok", "", false).unwrap()[0].id, "AB-1");
    assert_eq!(forge::jira_issues(&m.base, "", "tok", "", true).unwrap()[0].url, format!("{}/browse/AB-1", m.base));
    assert_eq!(forge::linear_issues_at(&format!("{}/graphql", m.base), "lin").unwrap()[0].id, "LIN-2");
    let t = forge::trello_issues(&m.base, "key:tok").unwrap();
    assert_eq!((t[0].title.as_str(), t[0].source.as_str()), ("Card", "Trello"));
    assert!(forge::trello_issues(&m.base, "malformed").is_err());
    let log = m.log.lock().unwrap();
    assert!(log[0].header("authorization").unwrap().starts_with("Basic "), "Jira Cloud: email and token");
    assert_eq!(log[1].header("authorization"), Some("Bearer tok"), "Jira Server: personal access token");
    assert!(log[1].path.contains("jql=assignee"));
    assert_eq!(log[2].header("authorization"), Some("lin"));
    assert!(log[3].path.contains("key=key&token=tok"));
}

fn ai_cfg(provider: AiProvider, base: &str) -> AiConfig {
    AiConfig { provider, base_url: base.to_string(), ..Default::default() }
}

#[test]
fn anthropic_request_on_the_wire() {
    let m = mock(|_| (200, json!({"stop_reason":"end_turn","content":[{"type":"text","text":"Fix the bug"}]}).to_string()));
    let out = ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("KEY"), "system text", "user text").unwrap();
    assert_eq!(out, "Fix the bug");
    let log = m.log.lock().unwrap();
    let r = &log[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/v1/messages"));
    assert_eq!(r.header("x-api-key"), Some("KEY"));
    assert_eq!(r.header("anthropic-version"), Some("2026-10-21"));
    assert_eq!(r.header("anthropic-beta"), Some("server-side-fallback-2026-07-01"));
    let b = r.json();
    assert_eq!(b["model"], "claude-opus-5-5");
    assert_eq!(b["system"], "system text");
    assert_eq!(b["messages"][0]["content"], "user text");
    assert_eq!(b["fallbacks"], "default");
}

#[test]
fn ai_errors_and_refusals_reach_the_user() {
    let m = mock(|_| (429, json!({"error":{"type":"rate_limit_error","message":"Slow down"}}).to_string()));
    let e = ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap_err().to_string();
    assert!(e.contains("429") && e.contains("Slow down"), "{e}");
    let m = mock(|_| (200, json!({"stop_reason":"refusal","content":[]}).to_string()));
    assert!(ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap_err().to_string().contains("declined"));
    let m = mock(|_| (502, "<html>gateway</html>".to_string()));
    assert!(ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap_err().to_string().contains("502"));
}

#[test]
fn openai_style_and_gemini_requests_on_the_wire() {
    for p in [AiProvider::OpenAi, AiProvider::Mistral, AiProvider::OpenRouter, AiProvider::OpenAiCompatible] {
        let m = mock(|_| (200, json!({"choices":[{"message":{"content":"ok"}}]}).to_string()));
        assert_eq!(ai::complete(&ai_cfg(p, &m.base), Some("K"), "s", "u").unwrap(), "ok");
        let log = m.log.lock().unwrap();
        assert_eq!(log[0].path, "/chat/completions");
        assert_eq!(log[0].header("authorization"), Some("Bearer K"));
        assert_eq!(log[0].json()["messages"][1]["content"], "u");
    }
    // A local server needs no key.
    let m = mock(|_| (200, json!({"choices":[{"message":{"content":"ok"}}]}).to_string()));
    assert_eq!(ai::complete(&ai_cfg(AiProvider::OpenAiCompatible, &m.base), None, "s", "u").unwrap(), "ok");
    assert!(ai::complete(&ai_cfg(AiProvider::Mistral, &m.base), None, "s", "u").is_err(), "hosted providers do");

    let m = mock(|_| (200, json!({"candidates":[{"content":{"parts":[{"text":"gem"}]}}]}).to_string()));
    assert_eq!(ai::complete(&ai_cfg(AiProvider::Gemini, &m.base), Some("K"), "s", "u").unwrap(), "gem");
    let log = m.log.lock().unwrap();
    assert!(log[0].path.ends_with(":generateContent"));
    assert_eq!(log[0].header("x-goog-api-key"), Some("K"));
}

#[test]
fn code_suggestions_post_a_review_with_suggestion_blocks() {
    let m = mock(|_| (200, "{}".to_string()));
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let s = vec![
        forge::Suggestion { path: "a.rs".into(), start: 3, end: 3, lines: vec!["new".into()] },
        forge::Suggestion { path: "b.rs".into(), start: 5, end: 7, lines: vec!["x".into(), "y".into()] },
    ];
    c.suggest_changes(9, &s).unwrap();
    let log = m.log.lock().unwrap();
    assert_eq!(log[0].path, "/repos/o/r/pulls/9/reviews");
    let b = log[0].json();
    assert_eq!(b["event"], "COMMENT");
    assert_eq!(b["comments"][0]["line"], 3);
    assert!(b["comments"][0].get("start_line").is_none());
    assert_eq!(b["comments"][1]["start_line"], 5);
    assert_eq!(b["comments"][1]["body"], "```suggestion\nx\ny\n```");
    let g = client(ForgeKind::GitLab, "o/r", &m.base, "");
    assert!(g.suggest_changes(9, &s).is_err());
}

/// Answers every request with a redirect to `to`, as a misconfigured gateway might.
fn redirector(status: u16, to: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
            }
            let mut b = vec![0u8; len];
            let _ = r.read_exact(&mut b);
            let _ = write!(s, "HTTP/1.1 {status} Moved\r\nLocation: {to}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    base
}

#[test]
fn ai_keys_are_never_sent_to_a_redirect_target() {
    let target = mock(|_| (200, "{}".to_string()));
    // 127.0.0.1 and localhost are different hosts as far as a client can tell.
    let other_host = target.base.replace("127.0.0.1", "localhost");
    for provider in [AiProvider::Anthropic, AiProvider::Gemini, AiProvider::OpenAi] {
        for status in [301, 302, 303, 307, 308] {
            let base = redirector(status, other_host.clone());
            let e = ai::complete(&ai_cfg(provider, &base), Some("FAKE_KEY"), "s", "u").unwrap_err().to_string();
            assert!(e.contains(&status.to_string()), "the error says what happened: {e}");
        }
    }
    let seen = target.log.lock().unwrap();
    assert!(seen.is_empty(), "the other host was contacted: {seen:?}");
}
