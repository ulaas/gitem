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
    mock_with_headers(move |r| {
        let (status, body) = handler(r);
        (status, Vec::new(), body)
    })
}

/// The same, when a test cares about the reply's headers too.
fn mock_with_headers(handler: impl Fn(&Req) -> (u16, Vec<(&'static str, String)>, String) + Send + 'static) -> Mock {
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
            let (status, extra, body) = handler(&req);
            l2.lock().unwrap().push(req);
            let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    Mock { base, log }
}

fn client(kind: ForgeKind, path: &str, api: &str, user: &str) -> ForgeClient {
    ForgeClient::new(RemoteRepo { kind, host: "host.test".into(), path: path.into(), api_base: Some(api.to_string()), templates: Default::default(), origin: None }, "TOKEN".into(), user.into())
}

#[test]
fn github_flow() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/user") => (200, json!({"login":"ann"}).to_string()),
        ("POST", "/graphql") if r.body.contains("issues(") => (200, json!({"data":{"repository":{"issues":{"nodes":[{"number":1,"title":"Bug","url":"i","state":"OPEN","author":{"login":"b"},"labels":{"nodes":[{"name":"x"}]},"assignees":{"nodes":[{"login":"c"}]}}]}}}}).to_string()),
        ("POST", "/graphql") => (200, json!({"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"title":"T","url":"u","author":{"login":"ann"},"headRefName":"f","baseRefName":"main","reviewDecision":"APPROVED"}]}}}}).to_string()),
        ("POST", "/repos/o/r/pulls") => (201, json!({"html_url":"http://pr/9"}).to_string()),
        ("PUT", "/repos/o/r/pulls/5/merge") => (200, "{}".to_string()),
        ("POST", "/repos/o/r/issues/5/comments") => (201, "{}".to_string()),
        ("POST", "/repos/o/r/pulls/5/reviews") => (200, "{}".to_string()),
        ("POST", "/gists") => (201, json!({"html_url":"http://gist/1"}).to_string()),
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
    assert_eq!((issues.len(), issues[0].state.as_str(), issues[0].labels[0].as_str(), issues[0].assignees[0].as_str()), (1, "open", "x", "c"));
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
    assert!(!log.iter().any(|r| r.path.contains("/issues?")), "the REST list has pull requests in it, which can crowd out issues");
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

/// Each page goes back from the lowest id of the one before, up to the limit all hosts have.
#[test]
fn trello_cards_come_a_page_at_a_time_and_stop() {
    let cards = |from: usize, n: usize| json!((from..from + n).map(|i| json!({"id":format!("{:08}", 1000 - i),"idShort":i,"name":format!("Card {i}")})).collect::<Vec<_>>());
    let m = mock(move |r| {
        let before = r.path.split("before=").nth(1).map(|b| b.split('&').next().unwrap().parse::<usize>().unwrap());
        assert!(r.path.contains("limit=50"), "{}", r.path);
        match before {
            None => (200, cards(0, 50).to_string()),
            Some(b) if b == 1000 - 49 => (200, cards(50, 10).to_string()),
            other => panic!("asked from {other:?}"),
        }
    });
    let all = forge::trello_issues(&m.base, "key:tok").unwrap();
    assert_eq!(all.len(), 60);
    assert_eq!(all[59].title, "Card 59");
}

/// A host that ignores `before` gives the same page, which must not go round for ever or repeat.
#[test]
fn trello_paging_stops_when_a_page_holds_nothing_new() {
    let page = json!((0..50).map(|i| json!({"id":format!("{i:08}"),"idShort":i,"name":"Card"})).collect::<Vec<_>>());
    let m = mock(move |_| (200, page.to_string()));
    assert_eq!(forge::trello_issues(&m.base, "key:tok").unwrap().len(), 50);
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

// ---- Phase 9: what a host's replies can do. All of it against this mock, whose replies are
// written from the providers' docs or from memory, so it says nothing about the live services.

/// SN-8. Gitea and Forgejo send the key on every issue, as null. Seen on their live API in the
/// review, and repeated here by hand.
#[test]
fn gitea_issues_with_a_null_pull_request_are_issues() {
    let m = mock(|_| (200, json!([{"number":1,"title":"Real","html_url":"i","state":"open","pull_request":null},{"number":2,"title":"A PR","pull_request":{"merged":false}}]).to_string()));
    let list = client(ForgeKind::Gitea, "o/r", &m.base, "").list_issues().unwrap();
    assert_eq!(list.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["Real"]);
}

/// SN-12. These turned into a GET, which answered 200, so nothing was done and it said it was.
#[test]
fn a_write_that_is_redirected_fails_and_nothing_follows_it() {
    let target = mock(|_| (200, "{}".to_string()));
    for status in [301, 302, 303, 307, 308] {
        let base = redirector(status, target.base.clone());
        let c = client(ForgeKind::GitHub, "o/r", &base, "");
        for (what, r) in [("merge", c.merge_pr(5)), ("comment", c.comment_pr(5, "hi")), ("approve", c.approve_pr(5)), ("create", c.create_pr("t", "b", "f", "main", false).map(|_| ()))] {
            let e = r.expect_err(what).to_string();
            assert!(e.contains(&status.to_string()) && e.contains("Nothing was changed"), "{what}: {e}");
        }
        let a = client(ForgeKind::AzureDevOps, "org/proj/repo", &base, "me");
        assert!(a.comment_pr(5, "hi").is_err());
    }
    assert!(target.log.lock().unwrap().is_empty(), "something followed a redirect: {:?}", target.log.lock().unwrap());
}

/// A plain read can still follow a host that moved.
#[test]
fn a_read_follows_a_redirect() {
    let target = mock(|_| (200, json!({"login":"ann"}).to_string()));
    let base = redirector(302, target.base.clone());
    assert_eq!(client(ForgeKind::GitHub, "o/r", &base, "").whoami().unwrap(), "ann");
}

/// Answers once with more promised than is sent, and hangs up.
fn cut_off_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
            }
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\nConnection: close\r\n\r\n[{\"iid\":1");
        }
    });
    base
}

/// SN-11. Each of these used to be an empty list, which reads as there being nothing.
#[test]
fn a_failed_or_partial_reply_is_not_an_empty_list() {
    let base = cut_off_server();
    assert!(client(ForgeKind::GitLab, "g/r", &base, "").list_prs().is_err(), "connection dropped");
    assert!(client(ForgeKind::GitLab, "g/r", &base, "").list_issues().is_err());
    assert!(forge::jira_issues(&base, "e", "t", "", false).is_err());
    assert!(forge::trello_issues(&base, "k:t").is_err());
    assert!(forge::linear_issues_at(&format!("{base}/graphql"), "t").is_err());

    let huge = format!("[{}]", "{\"iid\":1},".repeat(1_500_000));
    let m = mock(move |_| (200, huge.clone()));
    assert!(client(ForgeKind::GitLab, "g/r", &m.base, "").list_prs().is_err(), "over the size limit");

    for reply in ["", "{}", "{\"message\":\"odd\"}"] {
        let m = mock(move |_| (200, reply.to_string()));
        assert!(client(ForgeKind::GitLab, "g/r", &m.base, "").list_prs().is_err(), "{reply:?}");
        assert!(client(ForgeKind::Gitea, "o/r", &m.base, "").list_issues().is_err(), "{reply:?}");
        assert!(client(ForgeKind::Bitbucket, "o/r", &m.base, "").list_prs().is_err(), "{reply:?}");
        assert!(client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "").list_prs().is_err(), "{reply:?}");
    }

    let m = mock(|_| (200, json!({"errors":[{"message":"Authentication required"}]}).to_string()));
    assert!(forge::linear_issues_at(&format!("{}/graphql", m.base), "t").unwrap_err().to_string().contains("Authentication required"));
    let m = mock(|_| (200, json!({"errorMessages":["The query is not valid"]}).to_string()));
    assert!(forge::jira_issues(&m.base, "e", "t", "", false).unwrap_err().to_string().contains("not valid"));
}

fn numbered(from: usize, to: usize, make: impl Fn(usize) -> Value) -> Vec<Value> {
    (from..to).map(make).collect()
}

fn query(path: &str, key: &str) -> usize {
    path.split(['?', '&']).find_map(|kv| kv.strip_prefix(&format!("{key}="))).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// SN-15, for each host's way of doing pages. 120 are there, in pages of 50.
#[test]
fn lists_read_every_page() {
    let page_of = |page: usize| ((page - 1) * 50, (page * 50).min(120));
    let m = mock(move |r| {
        let (a, b) = page_of(query(&r.path, "page").max(1));
        (200, serde_json::to_string(&numbered(a, b, |i| json!({"iid":i,"number":i}))).unwrap())
    });
    assert_eq!(client(ForgeKind::GitLab, "g/r", &m.base, "").list_prs().unwrap().len(), 120);
    assert_eq!(client(ForgeKind::GitLab, "g/r", &m.base, "").list_issues().unwrap().len(), 120);
    assert_eq!(client(ForgeKind::Gitea, "o/r", &m.base, "").list_prs().unwrap().len(), 120);
    assert_eq!(client(ForgeKind::Gitea, "o/r", &m.base, "").list_issues().unwrap().len(), 120);

    // Another host named in `next` must never see the token.
    let elsewhere = mock(|_| (200, "{}".to_string()));
    let next = format!("{}/stolen", elsewhere.base);
    let m = mock(move |r| {
        let page = query(&r.path, "page").max(1);
        let (a, b) = page_of(page);
        let mut v = json!({"values":numbered(a, b, |i| json!({"id":i}))});
        if b < 120 {
            v["next"] = json!(next);
        }
        (200, v.to_string())
    });
    assert_eq!(client(ForgeKind::Bitbucket, "o/r", &m.base, "me").list_prs().unwrap().len(), 120);
    assert_eq!(client(ForgeKind::Bitbucket, "o/r", &m.base, "me").list_issues().unwrap().len(), 120);
    assert!(elsewhere.log.lock().unwrap().is_empty(), "it followed an address from the reply");

    let m = mock(|r| {
        let a = query(&r.path, "start");
        let b = (a + 50).min(120);
        (200, json!({"values":numbered(a, b, |i| json!({"id":i})),"isLastPage":b == 120}).to_string())
    });
    assert_eq!(client(ForgeKind::BitbucketServer, "scm/P/r", &m.base, "").list_prs().unwrap().len(), 120);

    let m = mock(|r| {
        let a = query(&r.path, "$skip");
        let b = (a + 50).min(120);
        (200, json!({"value":numbered(a, b, |i| json!({"pullRequestId":i}))}).to_string())
    });
    assert_eq!(client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "me").list_prs().unwrap().len(), 120);

    let m = mock(|r| {
        let cursor = r.json()["variables"]["c"].as_str().map_or(0, |c| c.parse().unwrap());
        let b = (cursor + 50).min(120);
        let conn = json!({"pageInfo":{"hasNextPage":b < 120,"endCursor":b.to_string()},"nodes":numbered(cursor, b, |i| json!({"number":i}))});
        let list = if r.body.contains("issues(") { "issues" } else { "pullRequests" };
        (200, json!({"data":{"repository":{list:conn}}}).to_string())
    });
    assert_eq!(client(ForgeKind::GitHub, "o/r", &m.base, "").list_prs().unwrap().len(), 120);
    assert_eq!(client(ForgeKind::GitHub, "o/r", &m.base, "").list_issues().unwrap().len(), 120);

    let m = mock(|r| {
        let cursor = r.json()["variables"]["c"].as_str().map_or(0, |c| c.parse().unwrap());
        let b = (cursor + 50).min(120);
        (200, json!({"data":{"viewer":{"assignedIssues":{"pageInfo":{"hasNextPage":b < 120,"endCursor":b.to_string()},"nodes":numbered(cursor, b, |i| json!({"identifier":format!("L-{i}")}))}}}}).to_string())
    });
    assert_eq!(forge::linear_issues_at(&format!("{}/graphql", m.base), "t").unwrap().len(), 120);

    let m = mock(|r| {
        let a = query(&r.path, "nextPageToken");
        let b = (a + 50).min(120);
        let mut v = json!({"issues":numbered(a, b, |i| json!({"key":format!("J-{i}")}))});
        if b < 120 {
            v["nextPageToken"] = json!(b.to_string());
        }
        (200, v.to_string())
    });
    assert_eq!(forge::jira_issues(&m.base, "e", "t", "", false).unwrap().len(), 120);
    let m = mock(|r| {
        let a = query(&r.path, "startAt");
        let b = (a + 50).min(120);
        (200, json!({"total":120,"issues":numbered(a, b, |i| json!({"key":format!("J-{i}")}))}).to_string())
    });
    assert_eq!(forge::jira_issues(&m.base, "", "t", "", true).unwrap().len(), 120);
}

#[test]
fn a_very_long_list_stops_at_the_limit() {
    let m = mock(|_| (200, serde_json::to_string(&numbered(0, 50, |i| json!({"number":i}))).unwrap()));
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    assert_eq!(c.list_prs().unwrap().len(), forge::LIST_LIMIT);
    assert_eq!(m.log.lock().unwrap().len(), forge::LIST_LIMIT / 50, "and stops asking");
}

/// SN-16. A fork's `main` isn't this repository's `main`.
#[test]
fn each_host_says_when_a_pull_request_is_from_a_fork() {
    let m = mock(|_| (200, json!({"data":{"repository":{"pullRequests":{"nodes":[{"number":1,"headRefName":"main","isCrossRepository":true},{"number":2,"headRefName":"main","isCrossRepository":false}]}}}}).to_string()));
    let forks = |c: ForgeClient| c.list_prs().unwrap().iter().map(|p| p.from_fork).collect::<Vec<_>>();
    assert_eq!(forks(client(ForgeKind::GitHub, "o/r", &m.base, "")), [true, false]);
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    assert_eq!(c.pr_for_branch("main").unwrap().unwrap().number, 2);

    let m = mock(|_| (200, json!([{"iid":1,"source_project_id":7,"target_project_id":8},{"iid":2,"source_project_id":8,"target_project_id":8}]).to_string()));
    assert_eq!(forks(client(ForgeKind::GitLab, "g/r", &m.base, "")), [true, false]);
    let m = mock(|_| (200, json!([{"number":1,"head":{"ref":"main","repo":{"full_name":"you/r"}},"base":{"repo":{"full_name":"o/r"}}},{"number":2,"head":{"ref":"x","repo":{"full_name":"o/r"}},"base":{"repo":{"full_name":"o/r"}}}]).to_string()));
    assert_eq!(forks(client(ForgeKind::Gitea, "o/r", &m.base, "")), [true, false]);
    let m = mock(|_| (200, json!({"values":[{"id":1,"source":{"repository":{"full_name":"you/r"}},"destination":{"repository":{"full_name":"o/r"}}},{"id":2,"source":{"repository":{"full_name":"o/r"}},"destination":{"repository":{"full_name":"o/r"}}}]}).to_string()));
    assert_eq!(forks(client(ForgeKind::Bitbucket, "o/r", &m.base, "me")), [true, false]);
    let side = |slug: &str| json!({"repository":{"slug":slug,"project":{"key":"P"}}});
    let (a, b) = (side("yours"), side("r"));
    let m = mock(move |_| (200, json!({"isLastPage":true,"values":[{"id":1,"fromRef":a,"toRef":b},{"id":2,"fromRef":b,"toRef":b}]}).to_string()));
    assert_eq!(forks(client(ForgeKind::BitbucketServer, "scm/P/r", &m.base, "")), [true, false]);
    let m = mock(|_| (200, json!({"value":[{"pullRequestId":1,"forkSource":{"name":"refs/heads/x"}},{"pullRequestId":2}]}).to_string()));
    assert_eq!(forks(client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "me")), [true, false]);
}

/// SN-22. What's sent follows the review's memory of these APIs, not a check against them.
#[test]
fn bitbucket_cloud_groups_by_account_and_asks_for_what_it_needs() {
    let m = mock(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/user") => (200, json!({"account_id":"acct-me","display_name":"Me Myself"}).to_string()),
        ("GET", p) if p.contains("/pullrequests") => (200, json!({"values":[
            {"id":1,"author":{"display_name":"Me Myself","account_id":"acct-me"},"reviewers":[]},
            {"id":2,"author":{"display_name":"Other","account_id":"acct-o"},"reviewers":[{"account_id":"acct-me"}]}
        ]}).to_string()),
        ("GET", p) if p.contains("/issues") => (200, json!({"values":[]}).to_string()),
        ("POST", _) => (201, json!({"links":{"html":{"href":"http://b/1"}}}).to_string()),
        _ => (404, "{}".to_string()),
    });
    let c = client(ForgeKind::Bitbucket, "o/r", &m.base, "e@x");
    let me = c.whoami().unwrap();
    assert_eq!(me, "acct-me");
    let prs = c.list_prs().unwrap();
    assert_ne!(forge::launchpad_group(&prs[0], &me), forge::LaunchpadGroup::Other, "mine");
    assert_eq!(forge::launchpad_group(&prs[1], &me), forge::LaunchpadGroup::NeedsYourReview);
    c.list_issues().unwrap();
    c.create_pr("t", "b", "f", "main", true).unwrap();
    let log = m.log.lock().unwrap();
    assert!(log.iter().any(|r| r.path.contains("/pullrequests?") && r.path.contains("fields=%2Bvalues.reviewers")));
    assert!(log.iter().any(|r| r.path.contains("/issues?") && r.path.contains("state%3D%22new%22%20OR%20state%3D%22open%22")), "closed issues stay out");
    assert_eq!(log.iter().find(|r| r.method == "POST").unwrap().json()["draft"], true);
}

#[test]
fn a_gitea_draft_is_marked_in_its_title_and_read_back_from_it() {
    let m = mock(|r| match r.method.as_str() {
        "POST" => (201, json!({"html_url":"http://g/1"}).to_string()),
        _ => (200, json!([{"number":1,"title":"WIP: thing"},{"number":2,"title":"Done","draft":false}]).to_string()),
    });
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    c.create_pr("thing", "b", "f", "main", true).unwrap();
    c.create_pr("other", "b", "f", "main", false).unwrap();
    let log = m.log.lock().unwrap().clone();
    let titles: Vec<String> = log.iter().map(|r| r.json()["title"].as_str().unwrap().to_string()).collect();
    assert_eq!(titles, ["WIP: thing", "other"]);
    assert_eq!(c.list_prs().unwrap().iter().map(|p| p.draft).collect::<Vec<_>>(), [true, false]);
}

/// SN-33.
#[test]
fn sign_in_pages_and_rate_limits_are_explained() {
    let m = mock(|_| (203, "<!DOCTYPE html><html>Sign in</html>".to_string()));
    let e = client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "me").list_prs().unwrap_err().to_string();
    assert!(e.contains("sign-in") && e.contains("Connect"), "{e}");

    let m = mock_with_headers(|_| (429, vec![("Retry-After", "42".to_string())], json!({"message":"Slow down"}).to_string()));
    let e = client(ForgeKind::GitLab, "g/r", &m.base, "").list_prs().unwrap_err().to_string();
    assert!(e.contains("429") && e.contains("Slow down") && e.contains("42 s"), "{e}");

    let reset = (gitem_core::util::now_ts() + 600).to_string();
    let m = mock_with_headers(move |_| (403, vec![("X-RateLimit-Remaining", "0".to_string()), ("X-RateLimit-Reset", reset.clone())], json!({"message":"API rate limit exceeded"}).to_string()));
    let e = client(ForgeKind::GitHub, "o/r", &m.base, "").whoami().unwrap_err().to_string();
    assert!(e.contains("rate limit resets in"), "{e}");

    let m = mock(|_| (401, json!({"message":"Bad credentials"}).to_string()));
    let e = client(ForgeKind::GitHub, "o/r", &m.base, "").whoami().unwrap_err().to_string();
    assert!(e.contains("Bad credentials") && e.contains("Connect"), "{e}");
}

/// SN-37.
#[test]
fn a_partial_graphql_error_keeps_what_did_load() {
    let m = mock(|_| (200, json!({"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"headRefName":"f"},null]}}},"errors":[{"message":"Resource not accessible by personal access token"}]}).to_string()));
    let (prs, warning) = client(ForgeKind::GitHub, "o/r", &m.base, "").list_prs_noting_problems().unwrap();
    assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), [5]);
    assert!(warning.unwrap().contains("not accessible"));
    let m = mock(|_| (200, json!({"data":{"repository":null},"errors":[{"message":"Could not resolve to a Repository"}]}).to_string()));
    assert!(client(ForgeKind::GitHub, "o/r", &m.base, "").list_prs().unwrap_err().to_string().contains("Could not resolve"));
}

/// SN-13, all the way from a remote's address to the requests it causes.
#[test]
fn a_remote_on_a_port_is_where_requests_go() {
    let m = mock(|_| (200, json!({"login":"ann"}).to_string()));
    let url = format!("{}/o/r.git", m.base);
    let custom = vec![gitem_core::settings::CustomHost { host: "127.0.0.1".into(), kind: ForgeKind::Gitea, api_base: String::new(), templates: Default::default() }];
    let repo = forge::identify(&url, &custom).unwrap();
    assert_eq!(repo.web_url(), format!("{}/o/r", m.base));
    assert_eq!(repo.token_host(), m.base, "with http:// in it, so it can't be a token saved for https");
    assert_eq!(ForgeClient::new(repo, "TOKEN".into(), String::new()).whoami().unwrap(), "ann");
    assert_eq!(m.log.lock().unwrap()[0].path, "/api/v1/user");
}

/// Merge, comment and approve on the hosts the flow tests above leave out.
#[test]
fn comments_and_approvals_reach_each_hosts_address() {
    let m = mock(|_| (200, "{}".to_string()));
    let last = |m: &Mock| {
        let log = m.log.lock().unwrap();
        let r = log.last().unwrap();
        (r.method.clone(), r.path.clone(), r.json())
    };
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    c.comment_pr(4, "hi").unwrap();
    assert_eq!(last(&m), ("POST".to_string(), "/repos/o/r/issues/4/comments".to_string(), json!({"body":"hi"})));
    c.approve_pr(4).unwrap();
    assert_eq!(last(&m).2["event"], "APPROVED");
    let c = client(ForgeKind::Bitbucket, "o/r", &m.base, "me");
    c.comment_pr(4, "hi").unwrap();
    assert_eq!(last(&m), ("POST".to_string(), "/repositories/o/r/pullrequests/4/comments".to_string(), json!({"content":{"raw":"hi"}})));
    c.approve_pr(4).unwrap();
    assert_eq!(last(&m).1, "/repositories/o/r/pullrequests/4/approve");
    let c = client(ForgeKind::BitbucketServer, "scm/P/r", &m.base, "");
    c.comment_pr(4, "hi").unwrap();
    assert_eq!(last(&m), ("POST".to_string(), "/projects/P/repos/r/pull-requests/4/comments".to_string(), json!({"text":"hi"})));
    let c = client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "me");
    c.comment_pr(4, "hi").unwrap();
    let (method, path, body) = last(&m);
    assert_eq!((method.as_str(), body["comments"][0]["content"].as_str()), ("POST", Some("hi")));
    assert!(path.starts_with("/o/p/_apis/git/repositories/r/pullrequests/4/threads"), "{path}");
    // Approving there is a vote, which goes under your id. This mock doesn't know who you are.
    assert!(c.approve_pr(4).unwrap_err().to_string().contains("didn't say who you are"));
}

// ---- AI replies (SN-10, SN-26), against the same mock

#[test]
fn default_models_are_not_the_retired_names() {
    assert_eq!(AiProvider::Gemini.default_model(), "gemini-3.6-flash");
    assert_eq!(AiProvider::OpenRouter.default_model(), "anthropic/claude-opus-5.5");
    let m = mock(|_| (200, json!({"candidates":[{"content":{"parts":[{"text":"ok"}]}}]}).to_string()));
    ai::complete(&ai_cfg(AiProvider::Gemini, &m.base), Some("K"), "s", "u").unwrap();
    assert!(m.log.lock().unwrap()[0].path.contains("gemini-3.6-flash"), "a blank model setting sends the default");
}

#[test]
fn a_reply_cut_off_at_the_length_limit_is_not_passed_off_as_whole() {
    let cases = [
        (AiProvider::Anthropic, json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"partial"}]})),
        (AiProvider::OpenAi, json!({"choices":[{"finish_reason":"length","message":{"content":"partial"}}]})),
        (AiProvider::Gemini, json!({"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[{"text":"partial"}]}}]})),
    ];
    for (provider, reply) in cases {
        let body = reply.to_string();
        let m = mock(move |_| (200, body.clone()));
        let e = ai::complete(&ai_cfg(provider, &m.base), Some("K"), "s", "u").unwrap_err().to_string();
        assert!(e.contains("cut off"), "{provider:?}: {e}");
    }
    let m = mock(|_| (200, json!({"choices":[{"finish_reason":"content_filter","message":{"content":""}}]}).to_string()));
    assert!(ai::complete(&ai_cfg(AiProvider::OpenAi, &m.base), Some("K"), "s", "u").unwrap_err().to_string().contains("declined"));
    let m = mock(|_| (200, json!({"promptFeedback":{"blockReason":"SAFETY"}}).to_string()));
    assert!(ai::complete(&ai_cfg(AiProvider::Gemini, &m.base), Some("K"), "s", "u").unwrap_err().to_string().contains("declined"));
}

#[test]
fn a_gemini_reply_in_several_parts_is_read_whole() {
    let m = mock(|_| (200, json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"one "},{"text":"two"}]}}]}).to_string()));
    assert_eq!(ai::complete(&ai_cfg(AiProvider::Gemini, &m.base), Some("K"), "s", "u").unwrap(), "one two");
}

#[test]
fn a_busy_provider_is_tried_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for status in [429, 500, 503, 529] {
        let tries = AtomicUsize::new(0);
        let m = mock_with_headers(move |_| {
            if tries.fetch_add(1, Ordering::SeqCst) < 2 {
                (status, vec![("Retry-After", "0".to_string())], json!({"error":{"message":"busy"}}).to_string())
            } else {
                (200, Vec::new(), json!({"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}).to_string())
            }
        });
        assert_eq!(ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap(), "done", "{status}");
        assert_eq!(m.log.lock().unwrap().len(), 3);
    }
}

#[test]
fn retries_stop_and_some_failures_get_none() {
    let m = mock_with_headers(|_| (529, vec![("Retry-After", "0".to_string())], json!({"error":{"message":"Overloaded"}}).to_string()));
    let e = ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap_err().to_string();
    assert!(e.contains("529") && e.contains("Overloaded"), "{e}");
    assert_eq!(m.log.lock().unwrap().len(), 3, "the first try and two more");

    let m = mock(|_| (400, json!({"error":{"message":"Bad request"}}).to_string()));
    assert!(ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").is_err());
    assert_eq!(m.log.lock().unwrap().len(), 1, "asking again won't help");

    // Sitting an hour on a background thread helps nobody. It says how long, and stops.
    let start = std::time::Instant::now();
    let m = mock_with_headers(|_| (429, vec![("Retry-After", "3600".to_string())], json!({"error":{"message":"Slow down"}}).to_string()));
    let e = ai::complete(&ai_cfg(AiProvider::Anthropic, &m.base), Some("K"), "s", "u").unwrap_err().to_string();
    assert!(e.contains("3600 s"), "{e}");
    assert!(start.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(m.log.lock().unwrap().len(), 1);
}

// ---- TLS (SN-17)

/// Checking against the system's store instead of a bundled list mustn't check less. A
/// certificate nobody vouches for is refused before a request is sent. A company authority in
/// the Windows store being trusted wasn't tested: that would mean writing to the real store.
#[test]
fn a_certificate_nobody_vouches_for_is_refused_before_anything_is_sent() {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = certified.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().unwrap().with_no_client_auth().with_single_cert(vec![cert], key).unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<usize>> = Arc::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut s);
            let mut buf = [0u8; 256];
            if matches!(tls.read(&mut buf), Ok(n) if n > 0) {
                *seen.lock().unwrap() += 1;
            }
        }
    });
    let base = format!("https://localhost:{port}");
    let e = client(ForgeKind::GitHub, "o/r", &base, "").whoami().unwrap_err().to_string();
    assert!(e.to_lowercase().contains("certificate"), "{e}");
    let e = ai::complete(&ai_cfg(AiProvider::Anthropic, &base), Some("FAKE_KEY"), "s", "u").unwrap_err().to_string();
    assert!(e.to_lowercase().contains("certificate"), "{e}");
    assert_eq!(*requests.lock().unwrap(), 0, "a request reached the server");
}

// ---- one pull request in full. Every reply here is hand-written from memory of each host's
// documentation, so this shows what gitem sends and reads, not what a host really does.

#[test]
fn github_pull_request_details_and_review() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repos/o/r/pulls/5") => (200, json!({"head":{"sha":"h1"},"base":{"sha":"b1"}}).to_string()),
        ("GET", "/repos/o/r/pulls/5/files") => (200, json!([{"filename":"a.rs","status":"modified","additions":2,"deletions":1,"patch":"@@ -1 +1,2 @@\n-a\n+b\n+c"},{"filename":"new.rs","previous_filename":"old.rs","status":"renamed"}]).to_string()),
        ("GET", "/repos/o/r/commits/h1/check-runs") => (200, json!({"check_runs":[{"name":"build","status":"completed","conclusion":"failure","html_url":"http://ci/1"},{"name":"lint","status":"in_progress","conclusion":null,"html_url":"http://ci/2"}]}).to_string()),
        ("GET", "/repos/o/r/commits/h1/status") => (200, json!({"statuses":[{"context":"legacy","state":"success","description":"fine","target_url":"http://ci/3"}]}).to_string()),
        ("GET", "/repos/o/r/issues/5/comments") => (200, json!([{"user":{"login":"bea"},"body":"Looks fine","created_at":"2026-01-02T00:00:00Z"}]).to_string()),
        ("GET", "/repos/o/r/pulls/5/comments") => (200, json!([{"user":{"login":"cy"},"body":"Why?","path":"a.rs","line":2,"created_at":"2026-01-03T00:00:00Z"}]).to_string()),
        ("GET", "/repos/o/r/pulls/5/reviews") => (200, json!([{"user":{"login":"bea"},"state":"CHANGES_REQUESTED","body":"Fix it","submitted_at":"2026-01-01T00:00:00Z"}]).to_string()),
        ("POST", "/repos/o/r/pulls/5/reviews") | ("POST", "/repos/o/r/pulls/5/comments") | ("POST", "/repos/o/r/issues/5/comments") => (201, "{}".to_string()),
        ("PUT", "/repos/o/r/pulls/5/merge") => (200, "{}".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let d = c.pr_details(5).unwrap();
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert_eq!((d.head_sha.as_str(), d.base_sha.as_str()), ("h1", "b1"));
    assert_eq!(d.files[0].additions, 2);
    assert_eq!(d.files[1].old_path, "old.rs");
    use gitem_core::forge::details::{CheckState, MergeStyle, Verdict};
    let states: Vec<(&str, CheckState)> = d.checks.iter().map(|x| (x.name.as_str(), x.state)).collect();
    assert_eq!(states, vec![("build", CheckState::Failed), ("lint", CheckState::Running), ("legacy", CheckState::Passed)]);
    let order: Vec<&str> = d.comments.iter().map(|x| x.author.as_str()).collect();
    assert_eq!(order, vec!["bea", "bea", "cy"], "oldest first");
    assert_eq!(d.comments[0].verdict, "CHANGES_REQUESTED");
    assert_eq!(d.comments[2].path.as_deref(), Some("a.rs"));
    assert_eq!(d.comments[2].line, Some(2));

    c.review_pr(5, Verdict::RequestChanges, "No").unwrap();
    c.review_pr(5, Verdict::Approve, "Fine").unwrap();
    c.review_pr(5, Verdict::Comment, "Hm").unwrap();
    c.comment_line(5, &d, &d.files[0], 2, "Here").unwrap();
    c.merge_pr_as(5, Some(MergeStyle::Squash)).unwrap();
    let log = m.log.lock().unwrap();
    let posts: Vec<Value> = log.iter().filter(|r| r.method != "GET").map(Req::json).collect();
    assert_eq!(posts[0], json!({"event":"REQUEST_CHANGES","body":"No"}));
    assert_eq!(posts[1], json!({"event":"APPROVE","body":"Fine"}));
    assert_eq!(posts[2], json!({"body":"Hm"}));
    assert_eq!(posts[3], json!({"body":"Here","commit_id":"h1","path":"a.rs","line":2,"side":"RIGHT"}));
    assert_eq!(posts[4], json!({"merge_method":"squash"}));
}

#[test]
fn details_keep_what_could_be_read_when_part_of_it_fails() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repos/o/r/pulls/5") => (200, json!({"head":{"sha":"h1"},"base":{"sha":"b1"}}).to_string()),
        ("GET", "/repos/o/r/pulls/5/files") => (200, json!([{"filename":"a.rs","status":"modified"}]).to_string()),
        (_, p) if p.contains("check-runs") => (403, json!({"message":"token can't read checks"}).to_string()),
        _ => (200, "[]".to_string()),
    });
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let d = c.pr_details(5).unwrap();
    assert_eq!(d.files.len(), 1);
    assert!(d.problems.iter().any(|p| p.contains("Checks") && p.contains("can't read checks")), "{:?}", d.problems);
}

#[test]
fn a_pull_request_that_cannot_be_read_at_all_is_an_error() {
    let m = mock(|_| (404, json!({"message":"gone"}).to_string()));
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    assert!(c.pr_details(5).unwrap_err().to_string().contains("gone"));
}

#[test]
fn gitea_pull_request_details_and_review() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repos/o/r/pulls/3") => (200, json!({"head":{"sha":"h"},"base":{"sha":"b"}}).to_string()),
        ("GET", "/repos/o/r/pulls/3.diff") => (200, "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n+b\n".to_string()),
        ("GET", "/repos/o/r/pulls/3/files") => (200, json!([{"filename":"a.txt","status":"changed","additions":1,"deletions":1}]).to_string()),
        ("GET", "/repos/o/r/commits/h/statuses") => (200, json!([{"context":"ci","status":"pending","target_url":"http://ci"}]).to_string()),
        ("GET", "/repos/o/r/issues/3/comments") => (200, json!([{"user":{"login":"bea"},"body":"Hi","created_at":"2026-01-02T00:00:00Z"}]).to_string()),
        ("GET", "/repos/o/r/pulls/3/reviews") => (200, json!([{"id":9,"user":{"login":"cy"},"state":"APPROVED","body":"","comments_count":1,"submitted_at":"2026-01-03T00:00:00Z"}]).to_string()),
        ("GET", "/repos/o/r/pulls/3/reviews/9/comments") => (200, json!([{"user":{"login":"cy"},"body":"Nit","path":"a.txt","position":1,"created_at":"2026-01-03T00:00:01Z"}]).to_string()),
        ("POST", _) => (201, "{}".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    let d = c.pr_details(3).unwrap();
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert!(d.files[0].patch.contains("+b"));
    assert_eq!(d.checks[0].name, "ci");
    assert_eq!(d.comments.iter().map(|x| x.body.as_str()).collect::<Vec<_>>(), vec!["Hi", "", "Nit"]);
    use gitem_core::forge::details::{MergeStyle, Verdict};
    c.review_pr(3, Verdict::RequestChanges, "No").unwrap();
    c.comment_line(3, &d, &d.files[0], 1, "Here").unwrap();
    c.merge_pr_as(3, Some(MergeStyle::Rebase)).unwrap();
    let log = m.log.lock().unwrap();
    let posts: Vec<Value> = log.iter().filter(|r| r.method != "GET").map(Req::json).collect();
    assert_eq!(posts[0], json!({"event":"REQUEST_CHANGES","body":"No"}));
    assert_eq!(posts[1]["comments"][0], json!({"path":"a.txt","body":"Here","new_position":1}));
    assert_eq!(posts[2], json!({"Do":"rebase"}));
}

#[test]
fn gitlab_merge_request_details_and_review() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/projects/o%2Fr/merge_requests/4/changes") => (200, json!({"diff_refs":{"base_sha":"b","head_sha":"h","start_sha":"s"},"changes":[{"old_path":"a","new_path":"a","diff":"@@ -1 +1 @@\n-x\n+y\n"},{"old_path":"n","new_path":"n","new_file":true,"diff":""}]}).to_string()),
        ("GET", "/projects/o%2Fr/merge_requests/4/pipelines") => (200, json!([{"id":77},{"id":76}]).to_string()),
        ("GET", "/projects/o%2Fr/pipelines/77/jobs") => (200, json!([{"name":"test","status":"failed","web_url":"http://j"}]).to_string()),
        ("GET", "/projects/o%2Fr/merge_requests/4/discussions") => (200, json!([{"id":"sys","individual_note":true,"notes":[{"body":"pushed","system":true,"author":{"username":"bot"}}]},{"id":"d1","individual_note":false,"notes":[{"id":11,"body":"Nit","author":{"username":"bea"},"created_at":"2026-01-01T00:00:00Z","position":{"new_path":"a","new_line":1}}]}]).to_string()),
        ("POST", _) | ("PUT", _) => (201, "{}".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::GitLab, "o/r", &m.base, "");
    let d = c.pr_details(4).unwrap();
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert_eq!((d.files[0].additions, d.files[0].deletions), (1, 1));
    assert_eq!(d.files[1].status, "added");
    assert_eq!(d.checks[0].name, "test");
    assert_eq!(d.comments.len(), 1, "the host's own notes are left out");
    use gitem_core::forge::details::{MergeStyle, Verdict};
    assert!(c.review_pr(4, Verdict::RequestChanges, "No").unwrap_err().to_string().contains("no way to request changes"));
    c.review_pr(4, Verdict::Approve, "Fine").unwrap();
    c.comment_line(4, &d, &d.files[0], 1, "Here").unwrap();
    c.merge_pr_as(4, Some(MergeStyle::Squash)).unwrap();
    let log = m.log.lock().unwrap();
    let posts: Vec<(&str, Value)> = log.iter().filter(|r| r.method != "GET").map(|r| (r.path.as_str(), r.json())).collect();
    assert!(posts[0].0.ends_with("/approve"));
    assert_eq!(posts[1].1, json!({"body":"Fine"}), "words go separately");
    assert_eq!(posts[2].1["position"]["start_sha"], "s");
    assert_eq!(posts[2].1["position"]["new_line"], 1);
    assert_eq!(posts[3].1, json!({"squash":true}));
}

#[test]
fn bitbucket_pull_request_details_and_review() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repositories/o/r/pullrequests/2") => (200, json!({"source":{"commit":{"hash":"h"}},"destination":{"commit":{"hash":"b"}}}).to_string()),
        ("GET", "/repositories/o/r/pullrequests/2/diff") => (200, "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n".to_string()),
        ("GET", "/repositories/o/r/pullrequests/2/diffstat") => (200, json!({"values":[{"status":"modified","lines_added":1,"lines_removed":1,"new":{"path":"a"},"old":{"path":"a"}}]}).to_string()),
        ("GET", "/repositories/o/r/pullrequests/2/statuses") => (200, json!({"values":[{"name":"ci","state":"SUCCESSFUL","url":"http://ci"}]}).to_string()),
        ("GET", "/repositories/o/r/pullrequests/2/comments") => (200, json!({"values":[{"user":{"display_name":"Bea"},"content":{"raw":"Nit"},"created_on":"2026-01-01T00:00:00Z","inline":{"path":"a","to":1}}]}).to_string()),
        ("POST", _) => (200, "{}".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::Bitbucket, "o/r", &m.base, "u");
    let d = c.pr_details(2).unwrap();
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert!(d.files[0].patch.contains("+y"));
    assert_eq!(d.checks[0].state, gitem_core::forge::details::CheckState::Passed);
    assert_eq!(d.comments[0].line, Some(1));
    use gitem_core::forge::details::{MergeStyle, Verdict};
    c.review_pr(2, Verdict::RequestChanges, "").unwrap();
    c.comment_line(2, &d, &d.files[0], 1, "Here").unwrap();
    c.merge_pr_as(2, Some(MergeStyle::Squash)).unwrap();
    let log = m.log.lock().unwrap();
    let posts: Vec<(&str, Value)> = log.iter().filter(|r| r.method != "GET").map(|r| (r.path.as_str(), r.json())).collect();
    assert!(posts[0].0.ends_with("/request-changes"));
    assert_eq!(posts[1].1["inline"], json!({"path":"a","to":1}));
    assert_eq!(posts[2].1, json!({"merge_strategy":"squash"}));
}

#[test]
fn bitbucket_server_pull_request_details_and_review() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/rest/projects/P/repos/r/pull-requests/2") => (200, json!({"fromRef":{"latestCommit":"h"},"toRef":{"latestCommit":"b"}}).to_string()),
        ("GET", "/rest/projects/P/repos/r/pull-requests/2.diff") => (200, "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n".to_string()),
        ("GET", "/rest/projects/P/repos/r/pull-requests/2/changes") => (200, json!({"values":[{"type":"MODIFY","path":{"toString":"a"}}]}).to_string()),
        ("GET", "/rest/projects/P/repos/r/pull-requests/2/activities") => (200, json!({"values":[{"action":"COMMENTED","comment":{"text":"Nit","author":{"name":"bea"},"createdDate":1000},"commentAnchor":{"path":"a","line":1}},{"action":"APPROVED","user":{"name":"cy"},"createdDate":2000},{"action":"OPENED"}]}).to_string()),
        ("POST", _) | ("PUT", _) => (200, "{}".to_string()),
        _ => (200, json!({"values":[]}).to_string()),
    });
    let c = client(ForgeKind::BitbucketServer, "scm/P/r", &format!("{}/rest", m.base), "ann");
    let d = c.pr_details(2).unwrap();
    assert!(d.problems.iter().all(|p| !p.contains("Files")), "{:?}", d.problems);
    assert_eq!(d.files[0].status, "modify");
    assert_eq!(d.files[0].additions, 1);
    assert_eq!(d.comments.len(), 2);
    assert_eq!(d.comments[1].verdict, "APPROVED");
    use gitem_core::forge::details::Verdict;
    c.review_pr(2, Verdict::RequestChanges, "").unwrap();
    c.comment_line(2, &d, &d.files[0], 1, "Here").unwrap();
    let log = m.log.lock().unwrap();
    let posts: Vec<(&str, Value)> = log.iter().filter(|r| r.method != "GET").map(|r| (r.path.as_str(), r.json())).collect();
    assert!(posts[0].0.ends_with("/participants/ann"));
    assert_eq!(posts[0].1, json!({"status":"NEEDS_WORK"}));
    assert_eq!(posts[1].1["anchor"]["path"], "a");
}

#[test]
fn azure_pull_request_details_and_a_vote() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/org/_apis/connectionData") => (200, json!({"authenticatedUser":{"id":"me-id","providerDisplayName":"Me"}}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8") => (200, json!({"lastMergeSourceCommit":{"commitId":"h"},"lastMergeTargetCommit":{"commitId":"b"}}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8/iterations") => (200, json!({"value":[{"id":1},{"id":2}]}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8/iterations/2/changes") => (200, json!({"changeEntries":[{"changeType":"edit","item":{"path":"/a.txt"}}]}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8/statuses") => (200, json!({"value":[{"context":{"name":"ci"},"state":"failed","description":"Boom","targetUrl":"http://ci"}]}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8/threads") => (200, json!({"value":[{"threadContext":{"filePath":"/a.txt","rightFileStart":{"line":3}},"comments":[{"author":{"displayName":"Bea"},"content":"Nit","publishedDate":"2026-01-01T00:00:00Z","commentType":"text"},{"author":{"displayName":"System"},"content":"voted","commentType":"system"}]}]}).to_string()),
        ("PUT", _) | ("POST", _) | ("PATCH", _) => (200, "{}".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::AzureDevOps, "org/p/r", &m.base, "u");
    let d = c.pr_details(8).unwrap();
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    assert_eq!(d.files[0].path, "a.txt");
    assert_eq!(d.files[0].patch, "", "it lists names only");
    assert_eq!(d.comments.len(), 1, "the system's own notes are left out");
    assert_eq!(d.comments[0].line, Some(3));
    use gitem_core::forge::details::{MergeStyle, Verdict};
    c.approve_pr(8).unwrap();
    c.review_pr(8, Verdict::RequestChanges, "No").unwrap();
    c.comment_line(8, &d, &d.files[0], 3, "Here").unwrap();
    c.merge_pr_as(8, Some(MergeStyle::Squash)).unwrap();
    let log = m.log.lock().unwrap();
    let writes: Vec<(&str, Value)> = log.iter().filter(|r| r.method != "GET").map(|r| (r.path.as_str(), r.json())).collect();
    assert!(writes[0].0.contains("/reviewers/me-id"));
    assert_eq!(writes[0].1, json!({"vote":10}));
    assert_eq!(writes[1].1, json!({"vote":-10}));
    assert!(writes[2].0.contains("/threads"), "the words for that vote");
    assert_eq!(writes[3].1["threadContext"]["filePath"], "/a.txt");
    assert_eq!(writes[4].1["completionOptions"]["mergeStrategy"], "squash");
}

#[test]
fn the_pull_requests_youre_involved_in_come_from_all_over_the_host() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("POST", "/graphql") if r.body.contains("involves:@me") => (200, json!({"data":{"search":{"nodes":[{"number":1,"title":"One","url":"http://h/o/a/1","headRefName":"t","baseRefName":"main","repository":{"nameWithOwner":"o/a"},"author":{"login":"me"}},null,{"number":2,"title":"Two","url":"http://h/o/b/2","headRefName":"u","baseRefName":"main","repository":{"nameWithOwner":"o/b"}}]}}}).to_string()),
        _ => (404, json!({"message":"no route"}).to_string()),
    });
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let got = c.involved_prs().unwrap();
    assert_eq!(got.iter().map(|x| (x.repo.as_str(), x.pr.number)).collect::<Vec<_>>(), vec![("o/a", 1), ("o/b", 2)]);

    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repos/issues/search") if r.path.contains("created=true") => (200, json!([{"number":3,"title":"Mine","html_url":"http://h/o/a/3","repository":{"full_name":"o/a"}}]).to_string()),
        ("GET", "/repos/issues/search") if r.path.contains("assigned=true") => (200, json!([{"number":3,"title":"Mine","html_url":"http://h/o/a/3","repository":{"full_name":"o/a"}},{"number":4,"title":"Given","html_url":"http://h/o/b/4","repository":{"full_name":"o/b"}}]).to_string()),
        ("GET", "/repos/issues/search") => (200, "[]".to_string()),
        _ => (404, json!({"message":"no route"}).to_string()),
    });
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    let got = c.involved_prs().unwrap();
    assert_eq!(got.iter().map(|x| x.pr.number).collect::<Vec<_>>(), vec![3, 4], "one found twice shows once");

    let m = mock(|r| match r.path.split('?').next().unwrap() {
        "/merge_requests" => (200, json!([{"iid":5,"title":"T","web_url":"http://h/o/a/-/merge_requests/5","references":{"full":"o/a!5"},"source_branch":"s","target_branch":"main"}]).to_string()),
        _ => (404, json!({"message":"no route"}).to_string()),
    });
    let got = client(ForgeKind::GitLab, "o/r", &m.base, "").involved_prs().unwrap();
    assert_eq!(got[0].repo, "o/a");

    let m = mock(|r| match r.path.split('?').next().unwrap() {
        "/org/_apis/connectionData" => (200, json!({"authenticatedUser":{"id":"me"}}).to_string()),
        "/org/_apis/git/pullrequests" => (200, json!({"value":[{"pullRequestId":6,"title":"A","repository":{"name":"a","project":{"name":"p"}}}]}).to_string()),
        _ => (404, json!({"message":"no route"}).to_string()),
    });
    let got = client(ForgeKind::AzureDevOps, "org/x/y", &m.base, "u").involved_prs().unwrap();
    assert_eq!(got[0].repo, "p/a");
    assert!(got[0].pr.url.ends_with("/org/p/_git/a/pullrequest/6"), "{}", got[0].pr.url);
}

#[test]
fn a_commit_says_which_pull_requests_it_came_in_by() {
    let m = mock(|r| match r.path.as_str() {
        "/repos/o/r/commits/abc/pulls" => (200, json!([{"number":7,"title":"T","html_url":"http://h/7","state":"closed"}]).to_string()),
        "/repos/o/r/commits/abc/pull" => (200, json!({"number":8,"title":"U","html_url":"http://h/8","state":"open"}).to_string()),
        "/repos/o/r/commits/none/pull" => (404, json!({"message":"not found"}).to_string()),
        "/projects/o%2Fr/repository/commits/abc/merge_requests" => (200, json!([{"iid":9,"title":"V","web_url":"http://h/9","state":"merged"}]).to_string()),
        "/repositories/o/r/commit/abc/pullrequests" => (200, json!({"values":[{"id":10,"title":"W","links":{"html":{"href":"http://h/10"}},"state":"OPEN"}]}).to_string()),
        _ => (404, json!({"message":"no route"}).to_string()),
    });
    assert_eq!(client(ForgeKind::GitHub, "o/r", &m.base, "").prs_for_commit("abc").unwrap()[0].number, 7);
    assert_eq!(client(ForgeKind::Gitea, "o/r", &m.base, "").prs_for_commit("abc").unwrap()[0].number, 8);
    assert!(client(ForgeKind::Gitea, "o/r", &m.base, "").prs_for_commit("none").unwrap().is_empty(), "none isn't an error");
    assert_eq!(client(ForgeKind::GitLab, "o/r", &m.base, "").prs_for_commit("abc").unwrap()[0].number, 9);
    let b = client(ForgeKind::Bitbucket, "o/r", &m.base, "").prs_for_commit("abc").unwrap();
    assert_eq!((b[0].number, b[0].url.as_str()), (10, "http://h/10"));
    assert!(client(ForgeKind::AzureDevOps, "o/p/r", &m.base, "").prs_for_commit("abc").is_err());
}

// ---- replies within a thread

use gitem_core::forge::details::{self, PrComment};

fn threads_of(d: &details::PrDetails) -> Vec<Vec<&str>> {
    d.threads().iter().map(|g| g.iter().map(|c| c.body.as_str()).collect()).collect()
}

#[test]
fn github_threads_group_replies_and_a_reply_goes_to_the_comment_that_started_it() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repos/o/r/pulls/5") => (200, json!({"head":{"sha":"h1"},"base":{"sha":"b1"}}).to_string()),
        ("GET", "/repos/o/r/pulls/5/comments") => (200, json!([
            {"id":31,"user":{"login":"bea"},"body":"Why?","path":"a.rs","line":2,"created_at":"2026-01-03T00:00:00Z"},
            {"id":40,"user":{"login":"cy"},"body":"Elsewhere","path":"b.rs","line":1,"created_at":"2026-01-04T00:00:00Z"},
            {"id":32,"in_reply_to_id":31,"user":{"login":"ann"},"body":"Because","path":"a.rs","line":2,"created_at":"2026-01-05T00:00:00Z"},
        ]).to_string()),
        ("GET", "/repos/o/r/issues/5/comments") => (200, json!([{"id":9,"user":{"login":"bea"},"body":"Overall fine","created_at":"2026-01-02T00:00:00Z"}]).to_string()),
        ("POST", "/repos/o/r/pulls/5/comments/31/replies") => (201, "{}".to_string()),
        ("GET", _) => (200, "[]".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let d = c.pr_details(5).unwrap();
    assert_eq!(threads_of(&d), vec![vec!["Overall fine"], vec!["Why?", "Because"], vec!["Elsewhere"]]);
    assert_eq!(d.threads()[0][0].thread, "", "a comment on the whole pull request has no thread to answer in");
    c.reply_in_thread(5, d.threads()[1][0], "Thanks").unwrap();
    let log = m.log.lock().unwrap();
    let post = log.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(post.path, "/repos/o/r/pulls/5/comments/31/replies");
    assert_eq!(post.json(), json!({"body":"Thanks"}));
}

#[test]
fn gitlab_threads_come_from_discussions_and_a_reply_goes_to_the_discussion() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/projects/o%2Fr/merge_requests/4/changes") => (200, json!({"diff_refs":{"base_sha":"b","head_sha":"h","start_sha":"s"},"changes":[]}).to_string()),
        ("GET", "/projects/o%2Fr/merge_requests/4/discussions") => (200, json!([
            {"id":"lone","individual_note":true,"notes":[{"id":1,"body":"On the whole","author":{"username":"bea"},"created_at":"2026-01-01T00:00:00Z"}]},
            {"id":"abc123","individual_note":false,"notes":[
                {"id":2,"body":"Nit","author":{"username":"bea"},"created_at":"2026-01-02T00:00:00Z","position":{"new_path":"a","new_line":1}},
                {"id":3,"body":"Done","author":{"username":"ann"},"created_at":"2026-01-03T00:00:00Z","position":{"new_path":"a","new_line":1}},
                {"id":4,"body":"resolved","system":true,"author":{"username":"bot"},"created_at":"2026-01-04T00:00:00Z"}]},
        ]).to_string()),
        ("POST", "/projects/o%2Fr/merge_requests/4/discussions/abc123/notes") => (201, "{}".to_string()),
        ("GET", _) => (200, "[]".to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::GitLab, "o/r", &m.base, "");
    let d = c.pr_details(4).unwrap();
    assert_eq!(threads_of(&d), vec![vec!["On the whole"], vec!["Nit", "Done"]]);
    c.reply_in_thread(4, d.threads()[1][0], "Thanks").unwrap();
    assert_eq!(m.log.lock().unwrap().iter().find(|r| r.method == "POST").unwrap().json(), json!({"body":"Thanks"}));
}

#[test]
fn bitbucket_threads_follow_parents_to_any_depth_and_a_reply_names_its_parent() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/repositories/o/r/pullrequests/2") => (200, json!({"source":{"commit":{"hash":"h"}},"destination":{"commit":{"hash":"b"}}}).to_string()),
        ("GET", "/repositories/o/r/pullrequests/2/comments") => (200, json!({"values":[
            {"id":1,"user":{"display_name":"Bea"},"content":{"raw":"Nit"},"created_on":"2026-01-01T00:00:00Z","inline":{"path":"a","to":1}},
            {"id":2,"parent":{"id":1},"user":{"display_name":"Ann"},"content":{"raw":"Sure"},"created_on":"2026-01-02T00:00:00Z"},
            {"id":3,"parent":{"id":2},"user":{"display_name":"Bea"},"content":{"raw":"Thanks"},"created_on":"2026-01-03T00:00:00Z"},
        ]}).to_string()),
        ("POST", "/repositories/o/r/pullrequests/2/comments") => (201, "{}".to_string()),
        ("GET", p) if p.ends_with("/diff") => (200, String::new()),
        ("GET", _) => (200, json!({"values":[]}).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::Bitbucket, "o/r", &m.base, "u");
    let d = c.pr_details(2).unwrap();
    assert_eq!(threads_of(&d), vec![vec!["Nit", "Sure", "Thanks"]]);
    c.reply_in_thread(2, d.threads()[0][0], "Ok").unwrap();
    assert_eq!(m.log.lock().unwrap().iter().find(|r| r.method == "POST").unwrap().json(), json!({"content":{"raw":"Ok"},"parent":{"id":1}}));
}

#[test]
fn bitbucket_server_shows_replies_it_used_to_leave_out_and_a_reply_names_its_parent() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/rest/projects/P/repos/r/pull-requests/2") => (200, json!({"fromRef":{"latestCommit":"h"},"toRef":{"latestCommit":"b"}}).to_string()),
        ("GET", "/rest/projects/P/repos/r/pull-requests/2/activities") => (200, json!({"values":[{"action":"COMMENTED","commentAnchor":{"path":"a","line":1},"comment":{"id":5,"text":"Nit","author":{"name":"bea"},"createdDate":1000000,"comments":[
            {"id":6,"text":"Sure","author":{"name":"ann"},"createdDate":2000000,"comments":[{"id":7,"text":"Thanks","author":{"name":"bea"},"createdDate":3000000,"comments":[]}]}]}}]}).to_string()),
        ("POST", "/rest/projects/P/repos/r/pull-requests/2/comments") => (201, "{}".to_string()),
        ("GET", p) if p.ends_with(".diff") => (200, String::new()),
        ("GET", _) => (200, json!({"values":[]}).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::BitbucketServer, "scm/P/r", &format!("{}/rest", m.base), "ann");
    let d = c.pr_details(2).unwrap();
    assert_eq!(threads_of(&d), vec![vec!["Nit", "Sure", "Thanks"]]);
    assert_eq!(d.comments[2].path.as_deref(), Some("a"), "on the first comment's line");
    c.reply_in_thread(2, d.threads()[0][0], "Ok").unwrap();
    assert_eq!(m.log.lock().unwrap().iter().find(|r| r.method == "POST").unwrap().json(), json!({"text":"Ok","parent":{"id":5}}));
}

#[test]
fn azure_threads_keep_their_ids_and_a_reply_goes_into_the_thread() {
    let m = mock(|r| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8") => (200, json!({"lastMergeSourceCommit":{"commitId":"h"},"lastMergeTargetCommit":{"commitId":"b"}}).to_string()),
        ("GET", "/org/p/_apis/git/repositories/r/pullrequests/8/threads") => (200, json!({"value":[{"id":70,"threadContext":{"filePath":"/a.txt","rightFileStart":{"line":3}},"comments":[
            {"id":1,"author":{"displayName":"Bea"},"content":"Nit","publishedDate":"2026-01-01T00:00:00Z","commentType":"text"},
            {"id":2,"parentCommentId":1,"author":{"displayName":"Ann"},"content":"Sure","publishedDate":"2026-01-02T00:00:00Z","commentType":"text"},
            {"id":3,"author":{"displayName":"System"},"content":"voted","commentType":"system"}]}]}).to_string()),
        ("POST", "/org/p/_apis/git/repositories/r/pullrequests/8/threads/70/comments") => (200, "{}".to_string()),
        ("GET", _) => (200, json!({"value":[]}).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let c = client(ForgeKind::AzureDevOps, "org/p/r", &m.base, "u");
    let d = c.pr_details(8).unwrap();
    assert_eq!(threads_of(&d), vec![vec!["Nit", "Sure"]]);
    c.reply_in_thread(8, d.threads()[0][0], "Ok").unwrap();
    assert_eq!(m.log.lock().unwrap().iter().find(|r| r.method == "POST").unwrap().json(), json!({"content":"Ok","parentCommentId":1,"commentType":1}));
}

#[test]
fn a_host_with_no_replies_says_so_and_sends_nothing() {
    let m = mock(|_| (500, "{}".to_string()));
    let c = client(ForgeKind::Gitea, "o/r", &m.base, "");
    assert!(!ForgeKind::Gitea.can_reply_in_threads());
    let first = PrComment { id: "1".into(), thread: "1".into(), ..Default::default() };
    let e = c.reply_in_thread(3, &first, "Ok").unwrap_err().to_string();
    assert!(e.contains("can't take a reply"), "{e}");
    assert!(m.log.lock().unwrap().is_empty());
}

#[test]
fn a_comment_with_no_id_sends_no_reply_rather_than_a_wrong_one() {
    let m = mock(|_| (201, "{}".to_string()));
    let c = client(ForgeKind::GitHub, "o/r", &m.base, "");
    let e = c.reply_in_thread(3, &PrComment { thread: "x".into(), ..Default::default() }, "Ok").unwrap_err().to_string();
    assert!(e.contains("no reply was sent"), "{e}");
    assert!(m.log.lock().unwrap().is_empty());
}

// ---- everything you're in, on the two hosts 10b only compiled, and across hosts

#[test]
fn bitbucket_lists_what_youre_in_under_your_own_name() {
    let m = mock(|r| match r.path.split('?').next().unwrap() {
        "/user" => (200, json!({"username":"ann","account_id":"id-1","nickname":"ann"}).to_string()),
        p if p.starts_with("/pullrequests/") => (200, json!({"values":[{"id":6,"title":"In the other repo","state":"OPEN","author":{"display_name":"Ann","nickname":"ann"},"source":{"branch":{"name":"t"},"repository":{"full_name":"o/a"}},"destination":{"branch":{"name":"main"},"repository":{"full_name":"o/a"}},"links":{"html":{"href":"http://h/o/a/6"}}}]}).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let got = client(ForgeKind::Bitbucket, "o/r", &m.base, "u").involved_prs().unwrap();
    assert_eq!(got.iter().map(|x| (x.repo.as_str(), x.pr.number, x.pr.title.as_str())).collect::<Vec<_>>(), vec![("o/a", 6, "In the other repo")]);
    assert_eq!(got[0].pr.url, "http://h/o/a/6");
    let log = m.log.lock().unwrap();
    assert!(log.iter().any(|r| r.path.starts_with("/pullrequests/") && r.path.contains("state=OPEN")), "{:?}", log.iter().map(|r| r.path.clone()).collect::<Vec<_>>());
}

#[test]
fn bitbucket_server_lists_what_youre_in_from_its_dashboard_and_follows_pages() {
    let m = mock(|r| match (r.path.split('?').next().unwrap(), r.path.contains("start=0")) {
        ("/rest/dashboard/pull-requests", true) => (200, json!({"isLastPage":false,"values":[{"id":6,"title":"First","state":"OPEN","fromRef":{"displayId":"t"},"toRef":{"displayId":"main","repository":{"slug":"a","project":{"key":"P"}}},"links":{"self":[{"href":"http://h/projects/P/repos/a/pull-requests/6"}]}}]}).to_string()),
        ("/rest/dashboard/pull-requests", false) => (200, json!({"isLastPage":true,"values":[{"id":7,"title":"Second","state":"OPEN","fromRef":{"displayId":"u"},"toRef":{"displayId":"main","repository":{"slug":"b","project":{"key":"Q"}}},"links":{"self":[{"href":"http://h/projects/Q/repos/b/pull-requests/7"}]}}]}).to_string()),
        _ => (404, json!({"message":format!("no route {}", r.path)}).to_string()),
    });
    let got = client(ForgeKind::BitbucketServer, "scm/P/r", &format!("{}/rest", m.base), "ann").involved_prs().unwrap();
    assert_eq!(got.iter().map(|x| (x.repo.as_str(), x.pr.number)).collect::<Vec<_>>(), vec![("P/a", 6), ("Q/b", 7)]);
    assert_eq!(got[1].pr.url, "http://h/projects/Q/repos/b/pull-requests/7", "its own address, not this repository's");
}

#[test]
fn hosts_to_ask_are_each_host_once_and_azure_per_organisation() {
    let urls: Vec<String> = ["https://gitlab.example/a/b.git", "git@gitlab.example:c/d.git", "https://gitea.example/e/f.git", "https://dev.azure.com/one/p/_git/r", "https://dev.azure.com/two/p/_git/r", "https://dev.azure.com/one/q/_git/s", "https://gerrit.example/x", "https://unknown.example/y.git"].map(String::from).to_vec();
    let got = details::hosts_to_ask(&urls, &[]);
    let names: Vec<(ForgeKind, String)> = got.iter().map(|r| (r.kind, r.path.clone())).collect();
    assert_eq!(names, vec![(ForgeKind::GitLab, "a/b".to_string()), (ForgeKind::Gitea, "e/f".to_string()), (ForgeKind::AzureDevOps, "one/p/r".to_string()), (ForgeKind::AzureDevOps, "two/p/r".to_string())], "gerrit has no API to ask, and the last isn't a host at all");
}

#[test]
fn one_hosts_failure_stays_beside_what_the_others_found() {
    let good = mock(|r| match r.path.split('?').next().unwrap() {
        "/merge_requests" => (200, json!([{"iid":5,"title":"T","web_url":"http://h/o/a/-/merge_requests/5","references":{"full":"o/a!5"},"source_branch":"s","target_branch":"main"}]).to_string()),
        _ => (404, "{}".to_string()),
    });
    let bad = mock(|_| (401, json!({"message":"bad token"}).to_string()));
    let got = details::involved_on_hosts(vec![client(ForgeKind::Gitea, "o/r", &bad.base, ""), client(ForgeKind::GitLab, "o/r", &good.base, "")]);
    assert_eq!(got.len(), 2);
    assert!(got[0].found.as_ref().unwrap_err().contains("bad token"), "{:?}", got[0].found);
    assert_eq!(got[1].found.as_ref().unwrap()[0].pr.number, 5);
    assert_eq!(got[1].kind, ForgeKind::GitLab);
}
