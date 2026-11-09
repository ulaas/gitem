//! How fast find in files is on a folder, beside `git grep` on the same.
//!
//! `cargo run --release -p gitem-core --example search_bench -- FOLDER QUERY [--regex] [--runs N] [--rev REV]`
//!
//! With `--rev`, the files as that revision has them, beside `git grep` at the same.
//!
//! Run it twice: the first run reads from disk, the rest from the system's file cache.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gitem_core::GitExe;
use gitem_core::textsearch::{self, Event, Query, Request, Scope};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(folder), Some(text)) = (args.first(), args.get(1)) else {
        eprintln!("usage: search_bench FOLDER QUERY [--regex] [--runs N]");
        std::process::exit(2);
    };
    let regex = args.iter().any(|a| a == "--regex");
    let rev = args.iter().position(|a| a == "--rev").and_then(|i| args.get(i + 1)).cloned();
    let runs: usize = args.iter().position(|a| a == "--runs").and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok()).unwrap_or(3);
    let root = PathBuf::from(folder).canonicalize().expect("folder");
    let git = GitExe::discover(None).ok();
    for run in 1..=runs {
        let query = Query { text: text.clone(), regex, max_matches: 0, ..Default::default() };
        let scope = match &rev {
            Some(r) => Scope::Revision(r.clone()),
            None => Scope::WorkingTree { folder: None },
        };
        let req = Request { root: root.clone(), query, scope, overlay: HashMap::new(), git: git.clone() };
        let t = Instant::now();
        let search = textsearch::start(req, || {}).expect("query");
        let mut first: Option<Duration> = None;
        let summary = loop {
            match search.next_blocking() {
                Some(Event::Found(_)) => {
                    first.get_or_insert(t.elapsed());
                }
                Some(Event::Done(s)) => break s,
                None => panic!("ended without a summary"),
            }
        };
        let total = t.elapsed();
        let grep = git.as_ref().map(|g| {
            let t = Instant::now();
            let flavour = if regex { "-E" } else { "-F" };
            let mut a = vec!["grep", "-n", "-I", "-i", flavour, "-e", text.as_str()];
            match &rev {
                Some(r) => a.push(r.as_str()),
                None => a.push("--untracked"),
            }
            a.push("--");
            let _ = g.run_raw(Some(&root), &a);
            t.elapsed()
        });
        println!(
            "run {run}: {} matches in {} of {} files; first result {:?}, all {:?}; git grep {:?}",
            summary.matches,
            summary.files_matched,
            summary.files_searched,
            first.unwrap_or_default(),
            total,
            grep.unwrap_or_default()
        );
    }
}
