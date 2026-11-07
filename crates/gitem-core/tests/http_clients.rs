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
    assert!(c.approve_pr(4).unwrap_err().to_string().contains("Open on Remote"));
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
