//! The one test here that a real host answers: the hosting client against a real Gitea, which
//! a mock written by the same hand can't stand in for. It's opt-in, as it needs a server.
//!
//! ```text
//! docker run -d --name gitem-gitea -p 3000:3000 -e GITEA__security__INSTALL_LOCK=true gitea/gitea:1
//! docker exec -u git gitem-gitea gitea admin user create --username gitem --password gitem-test-pw --email gitem@example.invalid --must-change-password=false
//! docker exec -u git gitem-gitea gitea admin user generate-access-token --username gitem --scopes all --raw
//! GITEM_GITEA_URL=http://127.0.0.1:3000 GITEM_GITEA_TOKEN=<that token> cargo test -p gitem-core --test gitea_contract -- --ignored
//! docker rm -f gitem-gitea
//! ```
//!
//! It only talks to localhost, so a token for a real account can't be used by accident. It makes
//! a repository named after this process, and deletes it after.
//!
//! Isolation clears variables starting with GITEM_ before tests start, so it reads its settings
//! from the raw environment block captured here, before that happens.
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gitem_core::forge::{ForgeClient, ForgeKind, identify};
use gitem_core::settings::CustomHost;
use gitem_core::{GitExe, ops};
use serde_json::{Value, json};

static SETTINGS: OnceLock<Option<(String, String)>> = OnceLock::new();

/// Runs before `isolate!`'s setup: the linker orders these sections by name.
#[used]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCT"))]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
#[cfg_attr(not(any(windows, target_os = "macos")), unsafe(link_section = ".init_array.00001"))]
static READ_SETTINGS: extern "C" fn() = {
    extern "C" fn read() {
        let _ = SETTINGS.set(std::env::var("GITEM_GITEA_URL").ok().zip(std::env::var("GITEM_GITEA_TOKEN").ok()));
    }
    read
};

gitem_testenv::isolate!();

fn raw(method: &str, url: &str, token: &str, body: Option<Value>) -> u16 {
    let auth = format!("token {token}");
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let reply = match (method, body) {
        ("POST", Some(b)) => agent.post(url).header("authorization", &auth).header("content-type", "application/json").send(b.to_string()),
        ("PATCH", Some(b)) => agent.patch(url).header("authorization", &auth).header("content-type", "application/json").send(b.to_string()),
        ("DELETE", _) => agent.delete(url).header("authorization", &auth).call(),
        _ => agent.get(url).header("authorization", &auth).call(),
    };
    reply.unwrap_or_else(|e| panic!("{method} {url}: {e}")).status().as_u16()
}

fn run(git: &GitExe, dir: &std::path::Path, args: &[&str]) -> String {
    git.run(Some(dir), args).unwrap_or_else(|e| panic!("git {args:?}: {e}")).text()
}

#[test]
#[ignore = "needs a Gitea server: see the top of this file"]
fn the_client_works_against_a_real_gitea() {
    let Some((url, token)) = SETTINGS.get().cloned().flatten() else {
        panic!("Set GITEM_GITEA_URL and GITEM_GITEA_TOKEN: see the top of this file.");
    };
    let url = url.trim_end_matches('/').to_string();
    assert!(url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost"), "{url}: this test only talks to localhost");

    let name = format!("gitem-contract-{}", std::process::id());
    assert_eq!(raw("POST", &format!("{url}/api/v1/user/repos"), &token, Some(json!({"name":name,"auto_init":true,"default_branch":"main"}))), 201, "creating the repository");
    let result = std::panic::catch_unwind(|| exercise(&url, &token, &name));
    let _ = raw("DELETE", &format!("{url}/api/v1/repos/gitem/{name}"), &token, None);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

fn exercise(url: &str, token: &str, name: &str) {
    let custom = vec![CustomHost { host: url.split("://").nth(1).unwrap().split(':').next().unwrap().to_string(), kind: ForgeKind::Gitea, api_base: String::new(), templates: Default::default() }];
    let remote = format!("{url}/gitem/{name}.git");
    let repo = identify(&remote, &custom).expect("a custom host");
    let c = ForgeClient::new(repo.clone(), token.to_string(), String::new());
    assert_eq!(c.whoami().unwrap(), "gitem");

    // Real git, with the token in the address so no helper is involved.
    let dir = tempfile::tempdir().unwrap();
    let git = GitExe::discover(None).unwrap();
    let with_token = remote.replacen("://", &format!("://gitem:{token}@"), 1);
    run(&git, dir.path(), &["clone", "-q", &with_token, "work"]);
    let work = dir.path().join("work");
    for (k, v) in [("user.name", "T"), ("user.email", "t@example.invalid"), ("commit.gpgsign", "false")] {
        run(&git, &work, &["config", k, v]);
    }
    run(&git, &work, &["switch", "-q", "-c", "topic"]);
    std::fs::write(work.join("topic.txt"), "one\n").unwrap();
    run(&git, &work, &["add", "-A"]);
    run(&git, &work, &["commit", "-q", "-m", "topic"]);
    run(&git, &work, &["push", "-q", "origin", "topic"]);

    // SN-22: a draft by its title. SN-8: an issue Gitea sends with `pull_request: null`.
    let created = c.create_pr("A change", "Its body", "topic", "main", true).unwrap();
    assert!(created.contains(&format!("{name}/pulls/1")), "{created}");
    let prs = c.list_prs().unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!((prs[0].head.as_str(), prs[0].base.as_str(), prs[0].draft, prs[0].from_fork), ("topic", "main", true, false));
    assert_eq!(prs[0].url, created);

    assert_eq!(raw("POST", &format!("{url}/api/v1/repos/gitem/{name}/issues"), token, Some(json!({"title":"An issue"}))), 201);
    let issues = c.list_issues().unwrap();
    assert_eq!(issues.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["An issue"], "and the pull request isn't one");

    c.comment_pr(prs[0].number, "A comment").unwrap();

    // VS-4: the ref Check out fetches really is there, twice, with a rewrite in between.
    let fetch_ref = repo.pr_ref(&prs[0]).unwrap();
    assert_eq!(fetch_ref, "refs/pull/1/head");
    let run_op = |op: ops::GitOp| {
        for step in &op.steps {
            let args: Vec<&str> = step.args.iter().map(String::as_str).collect();
            run(&git, &work, &args);
        }
    };
    run(&git, &work, &["switch", "-q", "main"]);
    run_op(ops::fetch_pr("origin", &fetch_ref, "pr/1"));
    let first = run(&git, &work, &["rev-parse", "pr/1"]);
    run(&git, &work, &["switch", "-q", "topic"]);
    run(&git, &work, &["commit", "-q", "--amend", "-m", "topic rewritten"]);
    run(&git, &work, &["push", "-q", "--force", "origin", "topic"]);
    run(&git, &work, &["switch", "-q", "main"]);
    let t = Instant::now();
    loop {
        run_op(ops::fetch_pr("origin", &fetch_ref, "pr/1"));
        if run(&git, &work, &["rev-parse", "pr/1"]) != first {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the pull request ref never moved");
        std::thread::sleep(Duration::from_millis(500));
    }

    // Gitea won't merge a draft, and gitem doesn't offer to: it's marked ready on the host first,
    // which on Gitea means the title loses its WIP: prefix.
    assert!(prs[0].merge_refusal().is_some_and(|r| r.contains("draft")), "{:?}", prs[0].merge_refusal());
    let pulls = format!("{url}/api/v1/repos/gitem/{name}/pulls/{}", prs[0].number);
    assert_eq!(raw("PATCH", &pulls, token, Some(json!({"title":"A change"}))), 201, "marking it ready");
    let prs = c.list_prs().unwrap();
    assert_eq!((prs[0].title.as_str(), prs[0].draft), ("A change", false), "ready");

    // Gitea works out whether it can merge in the background, so this may take a moment.
    let t = Instant::now();
    loop {
        match c.merge_pr(prs[0].number) {
            Ok(()) => break,
            Err(e) => {
                assert!(t.elapsed() < Duration::from_secs(60), "{e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    assert!(c.list_prs().unwrap().is_empty(), "merged, so not open");
}
